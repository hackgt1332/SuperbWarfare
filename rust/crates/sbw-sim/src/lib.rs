//! Authoritative first playable weapon slice. This crate owns gameplay rules;
//! Horizon owns inventory CAS, projectile collision, damage, and vehicle physics.
use sbw_contract::types::*;
use serde::{Deserialize, Serialize};

pub mod attachments;
pub mod ballistics;
pub mod catalog;
pub mod vehicle_resources;
pub mod vehicle_weapons;

include!(concat!(env!("OUT_DIR"), "/content_hash.rs"));

pub const AK47: &str = "superbwarfare:ak_47";
pub const GLOCK17: &str = "superbwarfare:glock_17";
pub const PICKUP: &str = "superbwarfare:sodayo_pick_up";
pub const TRUCK: &str = "superbwarfare:truck";
pub const VEHICLES: [&str; 2] = [PICKUP, TRUCK];
pub const GUNS: [&str; 2] = [AK47, GLOCK17];
pub const FEATURES: &[Feature] = &[
    Feature::ItemState,
    Feature::AttackInput,
    Feature::ModChannel,
    Feature::Entities,
    Feature::Collision,
    Feature::Damage,
    Feature::WorldModels,
    Feature::WorldPersistence,
    Feature::Vehicles,
];
#[derive(Clone, Debug)]
pub struct Gun {
    pub id: &'static str,
    pub ammo: &'static str,
    pub capacity: u32,
    pub damage: f32,
    pub speed: f32,
    pub interval: u16,
    pub reload: u16,
    pub empty_reload: u16,
    pub automatic: bool,
}
pub fn gun(id: &str) -> Option<Gun> {
    // Values follow the mod's public data definitions (base magazine only).
    Some(match id {
        AK47 => Gun {
            id: AK47,
            ammo: "superbwarfare:rifle_ammo",
            capacity: 30,
            damage: 9.,
            speed: 18.,
            interval: 2,
            reload: 48,
            empty_reload: 64,
            automatic: true,
        },
        GLOCK17 => Gun {
            id: GLOCK17,
            ammo: "superbwarfare:handgun_ammo",
            capacity: 17,
            damage: 5.5,
            speed: 11.,
            interval: 3,
            reload: 42,
            empty_reload: 45,
            automatic: false,
        },
        _ => return None,
    })
}
/// Persistent, versioned item state. No wall-clock or client timestamps.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Weapon {
    schema: u8,
    pub high: u64,
    pub low: u64,
    pub revision: u64,
    pub magazine: u32,
    pub loading: u32,
    pub reload_left: u16,
    pub cooldown: u16,
    pub mode: u16,
}
impl Weapon {
    pub fn new(instance: Uuid) -> Self {
        Self {
            schema: 1,
            high: instance.high,
            low: instance.low,
            revision: 1,
            magazine: 0,
            loading: 0,
            reload_left: 0,
            cooldown: 0,
            mode: 0,
        }
    }
    pub fn decode(bytes: &[u8], gun: &Gun) -> Option<Self> {
        let (s, rest): (Self, _) = postcard::take_from_bytes(bytes).ok()?;
        (rest.is_empty()
            && s.schema == 1
            && (s.high != 0 || s.low != 0)
            && s.revision > 0
            && s.magazine <= gun.capacity
            && s.loading <= gun.capacity - s.magazine
            && s.reload_left <= gun.empty_reload.max(gun.reload)
            && s.cooldown <= gun.interval
            && s.mode <= u16::from(gun.automatic)
            && (s.loading == 0) == (s.reload_left == 0))
            .then_some(s)
    }
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("bounded weapon state")
    }
    pub fn target(&self, slot: u8, equip_revision: u64) -> WeaponRef {
        WeaponRef {
            instance: Uuid {
                high: self.high,
                low: self.low,
            },
            slot,
            equip_revision,
        }
    }
    pub fn snapshot(
        &self,
        slot: u8,
        equip_revision: u64,
        gun: &Gun,
        reserve: u32,
        input: &Controls,
        tick: u64,
    ) -> WeaponState {
        WeaponState {
            target: self.target(slot, equip_revision),
            revision: self.revision,
            definition: gun.id.into(),
            magazine: self.magazine,
            chambered: false,
            reserve,
            heat: 0.,
            fire_mode: self.mode,
            reload: if self.reload_left > 0 {
                ReloadPhase::Loading
            } else {
                ReloadPhase::Idle
            },
            phase_end_tick: if self.reload_left > 0 {
                tick + u64::from(self.reload_left)
            } else {
                0
            },
            trigger_held: input.held,
            aiming: input.aiming,
        }
    }
}
#[derive(Default, Clone, Debug)]
pub struct Controls {
    held: bool,
    latched: bool,
    pressed: bool,
    aiming: bool,
    aim_lease: u64,
    reload: bool,
    cycle: bool,
    lease: u64,
}
impl Controls {
    pub fn action(&mut self, action: WeaponAction, tick: u64) {
        match action {
            WeaponAction::TriggerPress => {
                self.pressed |= !self.latched;
                self.latched = true;
                self.held = true;
                self.lease = tick + 10;
            }
            WeaponAction::TriggerRelease => {
                self.held = false;
                self.latched = false;
                self.pressed = false;
            }
            WeaponAction::AimStart => {
                self.aiming = true;
                self.aim_lease = tick + 10;
            }
            WeaponAction::AimStop => self.aiming = false,
            WeaponAction::Reload => self.reload = true,
            WeaponAction::CycleFireMode => self.cycle = true,
            // A reload reserves its rounds atomically; it runs to completion.
            WeaponAction::CancelReload => {}
        }
    }
    pub fn aiming(&self) -> bool {
        self.aiming
    }
    pub fn stop(&mut self) {
        *self = Self::default();
    }
}
pub struct Step {
    pub next: Weapon,
    pub consume: u32,
    pub shot: bool,
}
pub fn step(
    gun: &Gun,
    state: &Weapon,
    input: &mut Controls,
    reserve: u32,
    tick: u64,
) -> Option<Step> {
    if tick >= input.lease {
        input.held = false;
        input.pressed = false;
    }
    if tick >= input.aim_lease {
        input.aiming = false;
    }
    let mut next = state.clone();
    let mut consume = 0;
    let mut shot = false;
    if next.cooldown > 0 {
        next.cooldown -= 1;
    }
    if next.reload_left > 0 {
        next.reload_left -= 1;
        if next.reload_left == 0 {
            next.magazine += next.loading;
            next.loading = 0;
        }
    }
    if input.cycle {
        if gun.automatic {
            next.mode = 1 - next.mode;
        }
        input.cycle = false;
    }
    if input.reload {
        input.reload = false;
        if next.reload_left == 0 && next.magazine < gun.capacity && reserve > 0 {
            consume = (gun.capacity - next.magazine).min(reserve);
            next.loading = consume;
            next.reload_left = if next.magazine == 0 {
                gun.empty_reload
            } else {
                gun.reload
            };
            input.pressed = false;
        }
    }
    let automatic = gun.automatic && next.mode == 0;
    if next.reload_left == 0
        && next.cooldown == 0
        && next.magazine > 0
        && input.held
        && (automatic || input.pressed)
    {
        next.magazine -= 1;
        next.cooldown = gun.interval;
        shot = true;
        input.pressed = false;
    }
    // A semi-auto click during cooldown cannot become a delayed shot.
    if !automatic {
        input.pressed = false;
    }
    if next == *state {
        None
    } else {
        next.revision = next.revision.checked_add(1)?;
        Some(Step {
            next,
            consume,
            shot,
        })
    }
}
/// Generic engine profile, expressed as data rather than engine-specific code.
pub fn vehicle_profile(id: &str) -> Option<&'static str> {
    match id {
        PICKUP => Some(pickup_profile()),
        TRUCK => Some(include_str!("truck.json")),
        _ => None,
    }
}
pub fn pickup_profile() -> &'static str {
    include_str!("pickup.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn loaded(g: &Gun) -> Weapon {
        let mut s = Weapon::new(Uuid { high: 1, low: 2 });
        s.magazine = g.capacity;
        s
    }
    #[test]
    fn semi_heartbeat_after_expiry_is_not_another_physical_click() {
        let gun = gun(GLOCK17).unwrap();
        let mut state = loaded(&gun);
        let mut controls = Controls::default();
        controls.action(WeaponAction::TriggerPress, 0);
        for tick in 0..=10 {
            if let Some(r) = step(&gun, &state, &mut controls, 0, tick) {
                state = r.next;
            }
        }
        controls.action(WeaponAction::TriggerPress, 11);
        assert!(step(&gun, &state, &mut controls, 0, 11).is_none());
        assert_eq!(state.magazine, 16);
        controls.action(WeaponAction::TriggerRelease, 12);
        controls.action(WeaponAction::TriggerPress, 13);
        assert!(step(&gun, &state, &mut controls, 0, 13).unwrap().shot);
    }
    #[test]
    fn aim_requires_a_live_server_tick_lease() {
        let gun = gun(AK47).unwrap();
        let state = loaded(&gun);
        let mut input = Controls::default();
        input.action(WeaponAction::AimStart, 1);
        assert!(input.aiming());
        step(&gun, &state, &mut input, 0, 10);
        assert!(input.aiming());
        step(&gun, &state, &mut input, 0, 11);
        assert!(!input.aiming());
    }
    #[test]
    fn reload_reserves_once_and_restores_after_serialization() {
        let g = gun(AK47).unwrap();
        let mut c = Controls::default();
        let s = Weapon::new(Uuid { high: 1, low: 2 });
        c.action(WeaponAction::Reload, 0);
        let r = step(&g, &s, &mut c, 19, 0).unwrap();
        assert_eq!(r.consume, 19);
        let mut s = Weapon::decode(&r.next.encode(), &g).unwrap();
        for tick in 1..=64 {
            let r = step(&g, &s, &mut c, 0, tick).unwrap();
            assert_eq!(r.consume, 0);
            assert!(!r.shot);
            s = r.next;
        }
        assert_eq!(s.magazine, 19);
        assert_eq!(s.loading, 0);
    }
    #[test]
    fn auto_has_server_cadence_and_expires_without_heartbeat() {
        let g = gun(AK47).unwrap();
        let mut s = loaded(&g);
        let mut c = Controls::default();
        c.action(WeaponAction::TriggerPress, 0);
        let mut shots = 0;
        for tick in 0..20 {
            if let Some(r) = step(&g, &s, &mut c, 0, tick) {
                shots += u32::from(r.shot);
                s = r.next;
            }
        }
        assert_eq!(shots, 5);
        assert_eq!(s.magazine, 25);
    }
    #[test]
    fn semi_heartbeat_does_not_refire_and_switch_releases() {
        let g = gun(GLOCK17).unwrap();
        let mut s = loaded(&g);
        let mut c = Controls::default();
        for tick in 0..20 {
            c.action(WeaponAction::TriggerPress, tick);
            if let Some(r) = step(&g, &s, &mut c, 0, tick) {
                s = r.next;
            }
        }
        assert_eq!(s.magazine, 16);
        c.stop();
        c.action(WeaponAction::TriggerPress, 20);
        assert!(step(&g, &s, &mut c, 0, 20).unwrap().shot);
    }
    #[test]
    fn untrusted_or_newer_saved_state_is_not_reinitialized() {
        let g = gun(AK47).unwrap();
        let mut s = loaded(&g);
        s.magazine = 100;
        assert!(Weapon::decode(&s.encode(), &g).is_none());
        assert!(Weapon::decode(&[255], &g).is_none());
    }
    #[test]
    fn no_ammo_means_no_reload_or_shot() {
        let g = gun(AK47).unwrap();
        let s = Weapon::new(Uuid { high: 1, low: 2 });
        let mut c = Controls::default();
        c.action(WeaponAction::Reload, 1);
        c.action(WeaponAction::TriggerPress, 1);
        assert!(step(&g, &s, &mut c, 0, 1).is_none());
    }
}
