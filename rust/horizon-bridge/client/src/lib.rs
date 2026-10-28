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
struct Client {
    attempted: bool,
    welcome: Option<Welcome>,
    snapshots: Option<SnapshotAssembler>,
    effects: Option<EffectGate>,
    sequence: u64,
    acknowledged: Option<u64>,
    tick: u64,
    weapon: Option<WeaponState>,
    firing: bool,
    aiming: bool,
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
            }
            ServerPacket::Snapshot(page) => {
                let assembler = self.snapshots.as_mut().ok_or("snapshot before welcome")?;
                if let Some(snapshot) = assembler.push(page).map_err(|e| format!("{e:?}"))? {
                    self.acknowledged = snapshot.acknowledged_input;
                    let next = snapshot.weapons.into_iter().next();
                    if let (Some(before), Some(after)) = (&self.weapon, &next)
                        && before.target == after.target
                        && after.magazine < before.magazine
                    {
                        render::shake(0.08, 0.10);
                    }
                    if self.weapon.as_ref().map(|w| &w.target) != next.as_ref().map(|w| &w.target) {
                        self.firing = false;
                        self.aiming = false;
                    }
                    self.weapon = next;
                    let models: Vec<_> = snapshot
                        .entities
                        .into_iter()
                        .filter_map(|e| {
                            if e.entity.state_schema != 1 || e.key.index > u64::from(u32::MAX) {
                                return None;
                            }
                            Some(render::VehicleModel {
                                entity: e.key.index as u32,
                                profile: String::from_utf8(e.entity.state).ok()?,
                            })
                        })
                        .collect();
                    render::set_vehicle_models(&models);
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
            self.firing = false;
            self.aiming = false;
        }
        render::set_zoom(render::Zoom {
            factor: if self.aiming { 1.7 } else { 1. },
            smooth: true,
            turn: if self.aiming { 0.65 } else { 1. },
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
                weapon: None,
                firing: false,
                aiming: false,
            },
            Registration::new()
                .command("sbw_status")
                .command("sbw_probe")
                .command("sbw_loadout")
                .on_tick(),
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
horizon_mod_sdk::export_mod!(Client);
