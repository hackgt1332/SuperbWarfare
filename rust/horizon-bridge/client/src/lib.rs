//! Player input and presentation. The server owns all ammunition and damage.
use horizon_mod_sdk::{Mod, Registration, command, hud, input, net, render};
use sbw_contract::{
    PROTOCOL,
    session::{EffectGate, SnapshotAssembler, verify_welcome},
    types::*,
    wire,
};
#[path = "../../shared.rs"]
mod shared;

/// Largest frame delta the presentation blends are allowed to advance by.
const MAX_FRAME_DT: f32 = 0.1;
/// Exponential approach rates, in 1/s, so they are frame-rate independent:
/// aiming settles fastest, then the reload lower, then whole-weapon recoil.
const AIM_RATE: f32 = 16.;
const RELOAD_RATE: f32 = 9.;
const RECOIL_RATE: f32 = 15.;

/// Whole-weapon pose offsets for one held gun.
struct GunOffsets {
    /// Aimed (iron-sight) shift.
    aim_x: f32,
    aim_y: f32,
    aim_z: f32,
    /// Vertical drop while reloading.
    lower: f32,
    /// Roll in degrees while reloading.
    roll: f32,
    /// Recoil pitch in degrees and its forward push.
    pitch: f32,
    push: f32,
}
impl GunOffsets {
    fn of(id: &str) -> Self {
        Self {
            aim_x: if id == sbw_sim::AK47 { -0.335 } else { -0.279 },
            aim_y: 0.2544,
            aim_z: 0.08,
            lower: 0.12,
            roll: 12.,
            pitch: 1.4,
            push: 0.045,
        }
    }
}

/// Player-visible smoothed state for the held gun: the ADS and reload blends,
/// the recoil impulse and the equip carry-over.
///
/// Everything here is disposable render state. It advances only from `frame`,
/// is snapped by `clear` when the authoritative render state goes away, and
/// never feeds back into the authoritative fire/reload path: the server still
/// owns ammunition, damage and the reload deadline, and the player's input is
/// unchanged. `step` and `offsets` are pure enough to exercise without a mod
/// host, which is how the transitions below are covered by tests.
struct Presentation {
    /// Eases 0..1 between the hip pose and the iron-sight pose.
    aim: Blend,
    /// Eases 0..1 for the reload lower, so the drop and the roll that rides it
    /// blend in and unwind instead of stepping.
    reload: Blend,
    /// Whole-weapon recoil impulse; decays every frame.
    recoil: f32,
    /// The gun the blends above were last eased for. When the held definition
    /// changes, the eased values are rebased onto the new gun so the pose it
    /// starts from is continuous instead of snapping.
    equipped: Option<String>,
}

/// The pose offsets the presentation produces, in the same units as
/// [`GunOffsets`]. Kept separate so an equip carry-over can be checked without
/// needing a `render::HeldPose`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct PoseOffsets {
    x: f32,
    y: f32,
    z: f32,
    pitch: f32,
    roll: f32,
}
impl Default for Presentation {
    fn default() -> Self {
        Self {
            aim: Blend::new(AIM_RATE),
            reload: Blend::new(RELOAD_RATE),
            recoil: 0.,
            equipped: None,
        }
    }
}
impl Presentation {
    /// Drop all temporary render state at once. Called wherever the held pose is
    /// cleared, so a disconnect, rejection or unequip cannot leave a lowered,
    /// rolled or recoiling gun behind.
    fn clear(&mut self) {
        self.aim.clear();
        self.reload.clear();
        self.recoil = 0.;
        self.equipped = None;
    }
    /// Forget frame continuity without disturbing the current pose. Called on a
    /// fresh session and on an equip change so the next frame settles from the
    /// pose the gun is already showing instead of replaying a transition on a
    /// stale interval. `recoil` and `equipped` are untouched; `step` decays the
    /// impulse by `dt` as usual.
    fn reanchor(&mut self) {
        self.aim.last_dt = 0.;
        self.reload.last_dt = 0.;
    }
    /// Pose offsets for `definition` with the given eased aim/reload values.
    /// Pure, so the blending can be checked without a renderer.
    fn offsets(definition: &str, aim: f32, reload: f32, recoil: f32) -> PoseOffsets {
        let o = GunOffsets::of(definition);
        PoseOffsets {
            x: aim * o.aim_x,
            y: aim * o.aim_y - reload * o.lower,
            z: aim * o.aim_z + recoil * o.push,
            pitch: recoil * o.pitch,
            // The roll rides the same blend as the lower: it is applied while
            // the gun is down and unwound as it comes back up.
            roll: -reload * o.roll,
        }
    }
    /// Advance the blends and produce the held pose for this frame.
    fn held_pose(
        &mut self,
        definition: &str,
        reloading: bool,
        aiming: bool,
        dt: f32,
    ) -> render::HeldPose {
        let pose = self.step(definition, reloading, aiming, dt);
        render::HeldPose {
            item: definition.to_owned(),
            x: pose.x,
            y: pose.y,
            z: pose.z,
            pitch: pose.pitch,
            yaw: 0.,
            roll: pose.roll,
        }
    }
    /// Advance the blends by `dt` and return this frame's pose offsets. Split
    /// out from [`Self::held_pose`] so the eased state, including the equip
    /// carry-over, can be exercised without creating render objects.
    fn step(&mut self, definition: &str, reloading: bool, aiming: bool, dt: f32) -> PoseOffsets {
        let dt = dt.clamp(0., MAX_FRAME_DT);
        // Aiming is suppressed while reloading; releasing it here too means a
        // reload started mid-ADS blends back out instead of snapping.
        self.aim
            .step(if aiming && !reloading { 1. } else { 0. }, dt);
        self.reload.step(if reloading { 1. } else { 0. }, dt);
        self.recoil *= (-dt * RECOIL_RATE).exp();
        // A different gun is an equip change: remember the pose it is leaving
        // behind so the new gun starts there and eases toward its own offsets.
        // The blends themselves stay live, so ADS and a reload that is still
        // running on the new gun simply continue from the carried pose.
        if self.equipped.as_deref() != Some(definition)
            && let Some(previous) = self.equipped.replace(definition.to_owned())
        {
            // Carry the pose the previous gun was presenting into the new
            // gun's eased values, in both directions. ADS, a running reload
            // and recoil keep resolving from there instead of snapping.
            let before = GunOffsets::of(&previous);
            let after = GunOffsets::of(definition);
            // `self.recoil` was decayed above; this frame's impulse is
            // simply re-expressed in the new gun's push units, so the swap
            // does not add a second decay.
            self.aim.rebase(match (before.aim_x, after.aim_x) {
                (from, to) if to != 0. => self.aim.value * from / to,
                _ => 0.,
            });
            self.reload.rebase(match (before.roll, after.roll) {
                (from, to) if to != 0. => self.reload.value * from / to,
                _ => 0.,
            });
            // Recoil is an impulse rather than a steady offset: hold it and
            // let the new gun settle from it, scaled into its own push.
            self.recoil = if after.push != 0. {
                self.recoil * before.push / after.push
            } else {
                0.
            };
        }
        // The first gun of a session starts from rest.
        Self::offsets(definition, self.aim.value, self.reload.value, self.recoil)
    }
}

/// A value eased toward a binary target with an exponential approach.
///
/// Each instance carries the rate it eases at, so aiming, the reload lower and
/// any future transition can settle at their own pace without duplicating this.
#[derive(Clone, Copy, Default)]
struct Blend {
    value: f32,
    /// Approach rate in 1/s.
    rate: f32,
    /// Previous frame interval, in seconds; `<= 0` means no previous frame.
    last_dt: f32,
}
impl Blend {
    fn new(rate: f32) -> Self {
        Self {
            value: 0.,
            rate,
            last_dt: 0.,
        }
    }
    /// Start the next step from `value` without easing, and forget frame
    /// continuity. Used when a pose carries over from another gun, so the new
    /// gun eases from that pose at once instead of snapping to the target.
    fn rebase(&mut self, value: f32) {
        self.value = value;
        self.last_dt = 0.;
    }
    /// Move `value` toward `target` over `dt` seconds.
    ///
    /// The first frame, a zero-length frame and a frame far longer than the
    /// previous one all settle immediately. That keeps the very first equipped
    /// frame and a resumed frame after a stall from replaying a transition for
    /// a change that has already happened on the server, and stops a hitch from
    /// extrapolating an exponential that would overshoot.
    fn step(&mut self, target: f32, dt: f32) {
        if dt <= 0. || self.last_dt <= 0. || dt > self.last_dt * 2. {
            self.value = target;
        } else {
            self.value += (target - self.value) * (1. - (-dt * self.rate).exp());
        }
        self.last_dt = dt;
    }
    /// Reset to the neutral pose and forget continuity, so the next eased
    /// change starts from a clean frame interval.
    fn clear(&mut self) {
        self.value = 0.;
        self.last_dt = 0.;
    }
}

struct Client {
    attempted: bool,
    welcome: Option<Welcome>,
    snapshots: Option<SnapshotAssembler>,
    effects: Option<EffectGate>,
    sequence: u64,
    acknowledged: Option<u64>,
    tick: u64,
    server_tick: u64,
    snapshot_tick: u64,
    weapon: Option<WeaponState>,
    firing: bool,
    aiming: bool,
    pose: Presentation,
}
impl Client {
    fn hello() -> Hello {
        Hello {
            protocol: PROTOCOL,
            content_sha256: shared::CONTENT_SHA256.to_vec(),
            features: sbw_sim::FEATURES.to_vec(),
        }
    }
    fn reset(&mut self) {
        self.attempted = false;
        self.welcome = None;
        self.snapshots = None;
        self.effects = None;
        self.sequence = 0;
        self.acknowledged = None;
        self.weapon = None;
        self.firing = false;
        self.aiming = false;
        self.pose.clear();
        render::set_held_pose(None);
        hud::clear();
        render::set_zoom(render::Zoom {
            factor: 1.,
            smooth: true,
            turn: 1.,
        });
        render::set_vehicle_models(&[]);
    }
    fn intent(&mut self, intent: Intent) -> Result<(), String> {
        let welcome = self
            .welcome
            .as_ref()
            .ok_or("Not connected to a compatible server")?;
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or("Reconnect to reset input sequence")?;
        let packet = ClientPacket::Input(Input {
            session: welcome.session,
            sequence,
            client_tick: self.tick,
            intent,
        });
        net::send(
            None,
            &wire::encode_client(&packet).map_err(|e| e.to_string())?,
        )?;
        self.sequence = sequence;
        Ok(())
    }
    fn action(&mut self, action: WeaponAction) {
        let Some(w) = &self.weapon else {
            return;
        };
        if input::held().is_some_and(|h| h.id == w.definition && h.slot == u32::from(w.target.slot))
        {
            let _ = self.intent(Intent::Weapon(WeaponInput {
                target: w.target,
                action,
            }));
        }
    }
    fn receive(&mut self, bytes: &[u8]) -> Result<(), String> {
        match wire::decode_server(bytes).map_err(|e| e.to_string())? {
            ServerPacket::Welcome(welcome) => {
                if self.welcome.is_some() {
                    return Err("duplicate welcome".into());
                }
                verify_welcome(&Self::hello(), &welcome).map_err(|e| format!("{e:?}"))?;
                self.snapshots =
                    Some(SnapshotAssembler::new(welcome.session).map_err(|e| e.to_string())?);
                self.effects = Some(EffectGate::new(welcome.session).map_err(|e| e.to_string())?);
                self.welcome = Some(welcome);
                // A new session restarts the frame clock; drop continuity so the
                // first frame eases instead of snapping on a stale interval.
                self.pose.reanchor();
            }
            ServerPacket::Snapshot(page) => {
                let assembler = self.snapshots.as_mut().ok_or("snapshot before welcome")?;
                if let Some(snapshot) = assembler.push(page).map_err(|e| format!("{e:?}"))? {
                    self.acknowledged = snapshot.acknowledged_input;
                    self.server_tick = snapshot.server_tick;
                    self.snapshot_tick = self.tick;
                    let next = snapshot.weapons.into_iter().next();
                    let swap_targets = match &self.weapon {
                        // Equipping joins the assemble below, but a gun that
                        // leaves the render state without a replacement only
                        // clears the pose in `frame`; there is nothing to carry.
                        Some(before) => next
                            .as_ref()
                            .is_some_and(|after| before.target != after.target),
                        None => false,
                    };
                    if let (Some(before), Some(after)) = (&self.weapon, &next)
                        && before.target == after.target
                        && after.magazine < before.magazine
                    {
                        self.pose.recoil = (self.pose.recoil + 1.).min(2.5);
                        render::shake(if self.aiming { 0.025 } else { 0.055 }, 0.09);
                    }
                    // A different target is a fresh equip. Drop the local input
                    // latch; the pose blends stay live and settle toward the new
                    // gun on their own next frame, so the transition stays smooth.
                    let same_equip =
                        self.weapon.as_ref().map(|w| &w.target) == next.as_ref().map(|w| &w.target);
                    if !same_equip {
                        self.firing = false;
                        // Equipping another gun moves the held pose to the new
                        // definition while the ADS/reload blends keep running.
                        // Re-anchor the frame clock so the first frame of the
                        // swap eases from the carried pose, not a stale one.
                        if swap_targets {
                            self.pose.reanchor();
                        }
                    }
                    self.weapon = next;
                }
            }
            ServerPacket::Effects(batch) => self
                .effects
                .as_mut()
                .ok_or("effects before welcome")?
                .accept(&batch)
                .map_err(|e| format!("{e:?}"))?,
            ServerPacket::Rejected(r) => {
                if r.sequence.is_none() {
                    self.reset();
                    self.attempted = true;
                }
                horizon_mod_sdk::info(&format!(
                    "{}: request rejected: {:?}",
                    shared::MOD_ID,
                    r.reason
                ));
            }
        }
        Ok(())
    }
    fn input(&mut self) {
        for event in input::poll_attack() {
            match event {
                input::AttackEvent::Start(_) => {
                    self.firing = true;
                    self.action(WeaponAction::TriggerPress);
                }
                input::AttackEvent::Stop(_) => {
                    self.firing = false;
                    self.action(WeaponAction::TriggerRelease);
                }
            }
        }
        for event in input::poll() {
            match event {
                input::Event::UseStart(u) if sbw_sim::VEHICLES.contains(&u.item.id.as_str()) => {
                    let _ = self.intent(Intent::DeployVehicle(u.item.slot as u8));
                }
                input::Event::UseStart(_) => {
                    self.aiming = true;
                    self.action(WeaponAction::AimStart);
                }
                input::Event::UseStop(_) => {
                    self.aiming = false;
                    self.action(WeaponAction::AimStop);
                }
                input::Event::KeyDown(k) if k == "reload" => self.action(WeaponAction::Reload),
                input::Event::KeyDown(k) if k == "mode" => self.action(WeaponAction::CycleFireMode),
                _ => {}
            }
        }
        if self.firing && self.tick.is_multiple_of(5) {
            self.action(WeaponAction::TriggerPress);
        }
        if self.aiming && self.tick.is_multiple_of(5) {
            self.action(WeaponAction::AimStart);
        }
        let held = input::held();
        if let Some(w) = self.weapon.as_ref().filter(|w| {
            held.as_ref()
                .is_some_and(|h| h.id == w.definition && h.slot == u32::from(w.target.slot))
        }) {
            let reloading = !matches!(w.reload, ReloadPhase::Idle);
            hud::set_panel(
                hud::Corner::BottomRight,
                &[
                    hud::Line {
                        text: format!(
                            "{}  {} / {}",
                            w.definition.trim_start_matches("superbwarfare:"),
                            w.magazine,
                            w.reserve
                        ),
                        color: 0xffffff,
                    },
                    hud::Line {
                        text: if reloading {
                            "Reloading...".into()
                        } else {
                            format!(
                                "{}  R reload | V fire mode",
                                if w.fire_mode == 0 && w.definition == sbw_sim::AK47 {
                                    "AUTO"
                                } else {
                                    "SEMI"
                                }
                            )
                        },
                        color: 0xbbccaa,
                    },
                ],
            );
        } else {
            hud::clear();
            if !held
                .as_ref()
                .is_some_and(|h| sbw_sim::GUNS.contains(&h.id.as_str()))
            {
                self.firing = false;
                self.aiming = false;
                // The zoom is frame-owned: nothing else clears it when the gun
                // leaves the hand without going through `reset` or `frame`.
                render::set_zoom(render::Zoom {
                    factor: 1.,
                    smooth: true,
                    turn: 1.,
                });
            }
        }
        let zoomed = self.aiming
            && self
                .weapon
                .as_ref()
                .is_some_and(|w| matches!(w.reload, ReloadPhase::Idle));
        render::set_zoom(render::Zoom {
            factor: if zoomed { 1.7 } else { 1. },
            smooth: true,
            turn: if zoomed { 0.65 } else { 1. },
        });
    }
}
impl Mod for Client {
    fn init() -> Result<(Self, Registration), String> {
        let mut claims: Vec<_> = sbw_sim::GUNS.iter().map(|s| (*s).to_string()).collect();
        input::claim_attack(&claims);
        claims.extend(sbw_sim::VEHICLES.iter().map(|id| (*id).to_string()));
        input::claim_items(&claims);
        input::register_key("reload", "Reload weapon", "key.keyboard.r")?;
        input::register_key("mode", "Cycle fire mode", "key.keyboard.v")?;
        Ok((
            Self {
                attempted: false,
                welcome: None,
                snapshots: None,
                effects: None,
                sequence: 0,
                acknowledged: None,
                tick: 0,
                server_tick: 0,
                snapshot_tick: 0,
                weapon: None,
                firing: false,
                aiming: false,
                pose: Presentation::default(),
            },
            Registration::new()
                .command("sbw_status")
                .command("sbw_probe")
                .command("sbw_loadout")
                .on_tick()
                .on_frame(),
        ))
    }
    fn tick(&mut self, tick: u64) {
        self.tick = tick;
        for event in net::poll() {
            match event {
                net::Event::Disconnected => self.reset(),
                net::Event::Received(m) => {
                    if let Err(e) = self.receive(&m.data) {
                        horizon_mod_sdk::warn(&format!("SuperbWarfare: {e}"));
                    }
                }
            }
        }
        if net::connected()
            && !self.attempted
            && let Ok(packet) = wire::encode_client(&ClientPacket::Hello(Self::hello()))
            && net::send(None, &packet).is_ok()
        {
            self.attempted = true;
        }
        self.input();
    }
    fn frame(&mut self, seconds: f32) {
        let dt = seconds.clamp(0., MAX_FRAME_DT);
        let held = input::held();
        let weapon = self.weapon.as_ref().filter(|w| {
            held.as_ref()
                .is_some_and(|h| h.id == w.definition && h.slot == u32::from(w.target.slot))
        });
        let Some(w) = weapon else {
            // No gun is being presented: clear every piece of temporary pose
            // state, not just the pose itself.
            self.pose.clear();
            render::set_held_pose(None);
            render::set_hud(&[]);
            render::set_zoom(render::Zoom {
                factor: 1.,
                smooth: true,
                turn: 1.,
            });
            return;
        };
        let reloading = !matches!(w.reload, ReloadPhase::Idle);
        let pose = self
            .pose
            .held_pose(&w.definition, reloading, self.aiming, dt);
        render::set_held_pose(Some(&pose));
        let (width, height) = render::gui_size();
        let mut shapes = Vec::new();
        if reloading {
            let total = match sbw_sim::gun(&w.definition) {
                // The client cannot tell a finished reload from an interrupted
                // one before the render state clears, so progress uses the
                // definition's upper bound rather than `phase_end_tick`, which
                // would complete early and sit full while still reloading.
                Some(g) if w.magazine > 0 => f32::from(g.reload),
                Some(g) => f32::from(g.empty_reload),
                None => 0.,
            };
            let estimated_tick = self
                .server_tick
                .saturating_add(self.tick.saturating_sub(self.snapshot_tick));
            let remaining = w.phase_end_tick.saturating_sub(estimated_tick) as f32;
            // Re-anchor the bar to the authoritative countdown each frame, so a
            // dropped or late snapshot cannot leave it stuck off-target.
            let progress = if total > 0. {
                (1. - remaining / total).clamp(0., 1.)
            } else {
                0.
            };
            shapes.push(render::Shape::Fill(render::FillShape {
                rect: render::Rect {
                    x: width / 2. - 30.,
                    y: height / 2. + 24.,
                    w: 60.,
                    h: 3.,
                },
                color: 0x99222222,
            }));
            shapes.push(render::Shape::Fill(render::FillShape {
                rect: render::Rect {
                    x: width / 2. - 30.,
                    y: height / 2. + 24.,
                    w: 60. * progress,
                    h: 3.,
                },
                color: 0xffe8d9ad,
            }));
        }
        render::set_hud(&shapes);
    }
    fn command(&mut self, name: &str, _: &str) -> Result<Option<String>, String> {
        if name == "sbw_status" {
            return Ok(Some(if self.welcome.is_some() {
                format!("Connected; acknowledged input: {:?}", self.acknowledged)
            } else if self.attempted {
                "Waiting for a compatible server".into()
            } else {
                "Disconnected".into()
            }));
        }
        if name == "sbw_loadout" {
            for item in [
                "ak_47 1",
                "glock_17 1",
                "rifle_ammo 64",
                "handgun_ammo 64",
                "sodayo_pick_up 1",
                "truck 1",
            ] {
                command::run(&format!("give @s superbwarfare:{item}"));
            }
            return Ok(Some("Requested test loadout (requires cheats/operator permissions). Select a gun and press R; use a vehicle item on open ground.".into()));
        }
        self.intent(Intent::Weapon(WeaponInput {
            target: WeaponRef {
                instance: Uuid { high: 0, low: 1 },
                slot: 0,
                equip_revision: 0,
            },
            action: WeaponAction::TriggerRelease,
        }))?;
        Ok(Some("Probe sent".into()))
    }
}
#[cfg(test)]
mod tests {
    //! The blends are pure math, so the presentation transitions are covered
    //! here without a mod host. These tests only exercise code in this file.
    use super::*;

    /// One 60 Hz frame. Long enough to be "not the first frame", short enough
    /// that `dt > last_dt * 2.` stays false and the blend eases.
    const FRAME: f32 = 1. / 60.;

    /// Ease `blend` toward `target` for `frames` settled 60 Hz frames.
    fn ease(mut blend: Blend, target: f32, frames: u32) -> f32 {
        blend.step(blend.value, FRAME); // establish frame continuity first
        for _ in 0..frames {
            blend.step(target, FRAME);
        }
        blend.value
    }

    #[test]
    fn blend_settles_immediately_on_the_first_frame() {
        // No previous interval: the first eased change must not replay.
        let mut blend = Blend::new(RELOAD_RATE);
        blend.step(1., FRAME);
        assert_eq!(blend.value, 1.);
    }

    #[test]
    fn blend_eases_and_approaches_without_overshooting() {
        let one_frame = ease(Blend::new(RELOAD_RATE), 1., 1);
        assert!(one_frame > 0. && one_frame < 1., "eased, not stepped");
        // Approaching an asymptote from below must never pass it.
        let settled = ease(Blend::new(RELOAD_RATE), 1., 600);
        assert!(settled <= 1., "never overshoots the target");
        assert!(settled > 0.99, "converges toward the target");
    }

    #[test]
    fn blend_settles_after_a_stall_instead_of_replaying() {
        let mut blend = Blend::new(RELOAD_RATE);
        blend.step(0., FRAME);
        // A frame far longer than the previous one settles at once, so a hitch
        // cannot leave the gun mid-transition for a state the server has passed.
        blend.step(1., MAX_FRAME_DT);
        assert_eq!(blend.value, 1.);
    }

    #[test]
    fn reload_lower_and_roll_blend_in_and_out_together() {
        // The roll rides the reload blend, so it must appear and clear with it.
        let lowered = Presentation::offsets(sbw_sim::AK47, 0., 1., 0.);
        let raised = Presentation::offsets(sbw_sim::AK47, 0., 0., 0.);
        assert!(lowered.y < raised.y, "lowering drops the gun");
        assert!(lowered.roll < 0., "the roll follows the lower");
        assert_eq!(raised.roll, 0., "the roll unwinds when the reload ends");

        // Mid-blend is strictly between the two poses on every reload axis.
        let half = Presentation::offsets(sbw_sim::AK47, 0., 0.5, 0.);
        assert!(half.y > lowered.y && half.y < raised.y);
        assert!(half.roll > lowered.roll && half.roll < raised.roll);
    }

    #[test]
    fn aiming_raises_the_gun_forward_of_the_hip_pose() {
        let hip = Presentation::offsets(sbw_sim::AK47, 0., 0., 0.);
        let ads = Presentation::offsets(sbw_sim::AK47, 1., 0., 0.);
        assert!(ads.y > hip.y, "ADS raises the sight line");
        assert!(ads.x < hip.x, "ADS pulls the gun across");
        assert!(ads.z > hip.z, "ADS pushes it forward");
    }

    #[test]
    fn reload_started_while_aiming_blends_ads_out() {
        let mut pose = Presentation::default();
        // Aim to full over a settled frame first.
        pose.step(sbw_sim::AK47, false, true, 0.);
        let aimed = pose.step(sbw_sim::AK47, false, true, FRAME);
        assert!(aimed.x < 0., "aiming with no reload reaches the ADS pose");
        // A reload is authoritative and must not be suppressed, but the ADS
        // pose blends back out over the next frames rather than snapping.
        pose.step(sbw_sim::AK47, true, true, FRAME);
        let during = pose.step(sbw_sim::AK47, true, true, FRAME);
        assert!(during.x > aimed.x, "ADS eased back toward the hip pose");
        assert!(during.y < aimed.y, "the running reload lowers the gun");
    }

    #[test]
    fn equip_change_carries_the_pose_instead_of_snapping() {
        let mut pose = Presentation::default();
        // Present the AK far into a reload so it is visibly lowered and rolled.
        for _ in 0..30 {
            pose.step(sbw_sim::AK47, true, false, FRAME);
        }
        let eased = pose.reload.value;
        assert!(eased > 0.5, "the AK is well into the lower");
        let ak_roll = -eased * GunOffsets::of(sbw_sim::AK47).roll;
        // The very frame the other gun appears must not step: the carry-over
        // rebases the eased state onto the new definition in both directions.
        let glock = pose.step(sbw_sim::GLOCK17, true, false, FRAME);
        // The reload stays at its carried value, so the lower is unchanged.
        assert!(
            (glock.y - (-eased * GunOffsets::of(sbw_sim::GLOCK17).lower)).abs() < 1e-5,
            "lower is continuous across the equip"
        );
        // And the roll the AK was showing carries too, up to one frame of easing
        // toward the Glock's own (smaller) roll.
        assert!(glock.roll < 0., "the roll carries to the new gun");
        assert!(
            (ak_roll - glock.roll).abs() < 0.5,
            "roll is continuous across the equip"
        );
    }

    #[test]
    fn first_equip_starts_from_rest() {
        let mut pose = Presentation::default();
        let first = pose.step(sbw_sim::AK47, false, false, 0.);
        assert_eq!(first, PoseOffsets::default());
    }

    #[test]
    fn clear_drops_every_temporary_render_state() {
        let mut pose = Presentation::default();
        pose.step(sbw_sim::AK47, true, true, 0.);
        for _ in 0..10 {
            pose.step(sbw_sim::AK47, true, true, FRAME);
        }
        pose.recoil = 1.;
        pose.clear();
        assert_eq!(pose.reload.value, 0.);
        assert_eq!(pose.aim.value, 0.);
        assert_eq!(pose.recoil, 0.);
        assert!(pose.equipped.is_none(), "the equip memory is dropped");
        // The next frame is a fresh gun again, so nothing replays from before.
        let after = pose.step(sbw_sim::AK47, false, false, FRAME);
        assert_eq!(after, PoseOffsets::default());
    }

    #[test]
    fn reanchor_settles_the_next_frame_on_the_current_pose() {
        let mut pose = Presentation::default();
        pose.step(sbw_sim::AK47, true, false, 0.);
        pose.step(sbw_sim::AK47, true, false, FRAME);
        let lowered = pose.reload.value;
        assert!(lowered > 0., "well into the lower");
        // A new session must not replay the lower: settle on the state at once.
        pose.reanchor();
        pose.step(sbw_sim::AK47, false, false, FRAME);
        assert_eq!(pose.reload.value, 0., "re-anchored frame snaps to target");
    }

    #[test]
    fn recoil_decays_below_the_impulse_ceiling() {
        let mut pose = Presentation::default();
        pose.step(sbw_sim::AK47, false, false, 0.);
        pose.recoil = 1.;
        for _ in 0..60 {
            pose.step(sbw_sim::AK47, false, false, FRAME);
        }
        assert!(pose.recoil < 0.05, "the impulse fades over about a second");
    }

    #[test]
    fn unknown_gun_keeps_the_ak_defaults_and_never_divides_by_zero() {
        // The carry-over divides by the new gun's offsets; the fallback must
        // stay finite even for an id the sim does not know.
        let mut pose = Presentation::default();
        pose.step(sbw_sim::AK47, false, false, 0.);
        let other = pose.step("superbwarfare:unknown", false, false, FRAME);
        assert!(other.x.is_finite() && other.y.is_finite() && other.roll.is_finite());
    }

    #[test]
    fn negative_dt_is_clamped_and_settles_the_blend() {
        let mut pose = Presentation::default();
        pose.step(sbw_sim::AK47, false, false, 0.);
        pose.step(sbw_sim::AK47, true, false, FRAME);
        // A negative interval clamps to zero, which settles the blend at once.
        // It must not advance past the target or yield a non-finite pose.
        let after = pose.step(sbw_sim::AK47, true, false, -1.);
        assert!(after.y.is_finite());
        assert_eq!(pose.reload.value, 1., "settled at the reload target");
        assert_eq!(after.y, -GunOffsets::of(sbw_sim::AK47).lower);
    }
}
horizon_mod_sdk::export_mod!(Client);
