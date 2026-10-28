//! Authoritative fuel, health and repair state for drivable vehicles.
//!
//! Horizon owns physics, collision, damage application and atomic inventory
//! commits. This module owns the rules: how much fuel a validated throttle
//! burns per authoritative tick, what a refuel or repair kit transaction may
//! change, and when a vehicle is destroyed or may be resurrected.
//!
//! Invariants:
//! - Fuel and health only change from a simulation tick or an accepted
//!   transaction; there is no other source of either.
//! - A refuel credits exactly the fuel units its plan removes from the
//!   inventory, and a repair heals exactly the kit units its plan consumes, so
//!   a caller that commits every write atomically cannot gain fuel or health
//!   for free. A stale commit fails whole; the caller rereads and retries.
//! - Fuel item state must be empty. A filled container cannot be drained into
//!   vehicle fuel, and containers are never handed back.
//! - A destroyed vehicle burns no fuel and cannot be fueled; it is restored
//!   only by field repair, to a bounded fraction of maximum health.
//! - Revisions never wrap. Saved state is versioned Postcard and bounded by
//!   [`STATE_BYTES`]; unknown schemas and out-of-range fields are rejected.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Saved-state schema handed to the host as `entity-record.state-schema`.
pub const SCHEMA: u16 = 1;
/// Encoded state is a handful of fields; the contract allows up to 4 KiB.
pub const STATE_BYTES: usize = 128;
/// Hard cap on proposed inventory writes per transaction.
pub const MAX_WRITES: usize = 16;
/// Health is stored in hundredths of a point.
const HEALTH_SCALE: u32 = 100;

/// Stable transport name used by item stacks in this crate for a vehicle.
pub const VEHICLE_ITEM: &str = "superbwarfare:vehicle";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Uuid {
    pub high: u64,
    pub low: u64,
}
impl Uuid {
    pub fn is_valid(self) -> bool {
        self.high != 0 || self.low != 0
    }
}

/// Static per-definition data. Values are data, not engine-specific code.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VehicleProfile {
    pub max_health: f32,
    pub fuel_capacity: u32,
    /// Fuel units burned per tick at full throttle.
    pub fuel_per_tick: u32,
    /// Idle burn as a fraction of moving burn, in basis points (0..=10_000).
    pub idle_basis_points: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileError {
    NonFiniteHealth,
    NonPositiveHealth,
    ZeroCapacity,
    ZeroBurn,
    BadIdleFraction,
}
impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ProfileError {}

impl VehicleProfile {
    pub fn new(
        max_health: f32,
        fuel_capacity: u32,
        fuel_per_tick: u32,
        idle_basis_points: u16,
    ) -> Result<Self, ProfileError> {
        if !max_health.is_finite() {
            return Err(ProfileError::NonFiniteHealth);
        }
        if max_health <= 0.0 {
            return Err(ProfileError::NonPositiveHealth);
        }
        if fuel_capacity == 0 {
            return Err(ProfileError::ZeroCapacity);
        }
        if fuel_per_tick == 0 {
            return Err(ProfileError::ZeroBurn);
        }
        if idle_basis_points > 10_000 {
            return Err(ProfileError::BadIdleFraction);
        }
        Ok(Self {
            max_health,
            fuel_capacity,
            fuel_per_tick,
            idle_basis_points,
        })
    }

    /// Health ceiling in hundredths, rounded up so any positive point value
    /// is representable and the maximum survives the round trip.
    pub fn max_health_centi(&self) -> u32 {
        // The profile constructor guarantees a finite positive value.
        let scaled = f64::from(self.max_health) * f64::from(HEALTH_SCALE);
        scaled.ceil().clamp(1.0, f64::from(u32::MAX)) as u32
    }
}

/// The condition this crate accepts as engine activity for one tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Engine {
    Off,
    Running,
}
impl Engine {
    /// Running only if the engine switch is on, a driver is seated and the
    /// throttle the server accepted is a finite axis inside [-1, 1].
    pub fn validate(self, occupied: bool, throttle: f32) -> Result<Engine, Reject> {
        match self {
            Engine::Off => Ok(Engine::Off),
            Engine::Running
                if occupied && throttle.is_finite() && (-1.0..=1.0).contains(&throttle) =>
            {
                Ok(Engine::Running)
            }
            Engine::Running => Err(Reject::InvalidThrottle),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKit {
    Fuel,
    Repair,
}
impl FieldKit {
    pub fn item(self) -> &'static str {
        match self {
            FieldKit::Fuel => "superbwarfare:fuel_canister",
            FieldKit::Repair => "superbwarfare:repair_kit",
        }
    }
}

/// A stack bound to one slot of a server-issued inventory snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stack {
    pub instance: Uuid,
    pub item: String,
    pub count: u32,
    pub state_schema: u16,
    pub state: Vec<u8>,
}

/// One slot after a transaction; `None` clears it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotWrite {
    pub index: u16,
    pub value: Option<Stack>,
}

/// The bounded edit the caller passes to `commit-inventory`, together with
/// the `expected_revision` it read. The whole edit commits or nothing does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryPlan {
    pub writes: Vec<SlotWrite>,
}

// `InvalidStack` carries a static reason, so `Reject` is `Clone` but not
// `Copy`; every call site moves or clones it rather than relying on a copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reject {
    /// Empty, oversize, trailing, wrong schema, or out-of-range saved bytes.
    CorruptState,
    WrongVehicle,
    InvalidThrottle,
    /// The tick is not strictly after the state's last processed tick.
    Stale,
    /// Fuel or repair requested for a destroyed vehicle.
    Destroyed,
    /// Field repair requested for an intact vehicle.
    NotDestroyed,
    Full,
    Undamaged,
    ZeroAmount,
    TooManyWrites,
    InvalidSlot,
    DuplicateSlot,
    InvalidStack(String),
    RevisionOverflow,
}
impl fmt::Display for Reject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Reject {}

/// Persistent, versioned vehicle state. Field order is frozen for schema 1.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VehicleResources {
    schema: u16,
    /// Vehicle identity; a save belongs to exactly one vehicle.
    high: u64,
    low: u64,
    pub revision: u64,
    /// Last authoritative tick that changed this state; zero before any tick.
    pub tick: u64,
    pub fuel: u32,
    health_centi: u32,
    pub destroyed: bool,
}

/// One deterministic tick of fuel consumption.
#[derive(Clone, Debug, PartialEq)]
pub struct Burn {
    pub next: VehicleResources,
    pub spent: u32,
    /// The vehicle ran out of fuel on this tick and the engine is now off.
    pub dry: bool,
}

/// An accepted fuel or repair transaction.
///
/// The two halves are one atomic unit: `next` carries the new vehicle state and
/// `inventory` carries the slot edits that paid for it. A caller commits them
/// together against the inventory `revision` it read (`state.revision` when the
/// proposal was made); if the commit is refused because the inventory moved,
/// nothing changed and the caller rereads, re-proposes and retries. The host is
/// responsible for making that pair atomic — writing `next` without
/// `inventory` would hand the vehicle free fuel or health, and vice versa.
#[derive(Clone, Debug, PartialEq)]
pub struct Transaction {
    pub next: VehicleResources,
    pub inventory: InventoryPlan,
}

impl VehicleResources {
    /// Spawned vehicle: full fuel and health, never ticked.
    pub fn new(instance: Uuid, profile: &VehicleProfile) -> Result<Self, Reject> {
        if !instance.is_valid() {
            return Err(Reject::WrongVehicle);
        }
        Ok(Self {
            schema: SCHEMA,
            high: instance.high,
            low: instance.low,
            revision: 1,
            tick: 0,
            fuel: profile.fuel_capacity,
            health_centi: profile.max_health_centi(),
            destroyed: false,
        })
    }

    pub fn instance(&self) -> Uuid {
        Uuid {
            high: self.high,
            low: self.low,
        }
    }

    /// Strict decode: rejects any bytes a writer must never have produced.
    pub fn decode(bytes: &[u8], instance: Uuid, profile: &VehicleProfile) -> Result<Self, Reject> {
        if bytes.is_empty() || bytes.len() > STATE_BYTES {
            return Err(Reject::CorruptState);
        }
        let (s, rest): (Self, _) =
            postcard::take_from_bytes(bytes).map_err(|_| Reject::CorruptState)?;
        if !rest.is_empty()
            || s.schema != SCHEMA
            || s.revision == 0
            || (s.high, s.low) != (instance.high, instance.low)
            || s.fuel > profile.fuel_capacity
            || s.health_centi > profile.max_health_centi()
            || s.destroyed != (s.health_centi == 0)
        {
            return Err(Reject::CorruptState);
        }
        Ok(s)
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("bounded vehicle state")
    }

    pub fn health(&self) -> f32 {
        self.health_centi as f32 / HEALTH_SCALE as f32
    }

    pub fn is_full(&self, profile: &VehicleProfile) -> bool {
        self.fuel == profile.fuel_capacity
    }

    fn commit(&self, mut next: Self) -> Result<Self, Reject> {
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or(Reject::RevisionOverflow)?;
        // This is the single place revisions advance; callers only stage
        // already-validated field changes in `next`.
        Ok(next)
    }
}

/// Deterministic fuel burn for one authoritative tick.
///
/// At full throttle a running engine spends `fuel_per_tick`; otherwise it
/// scales linearly with `|throttle|`, with an idle floor of
/// `fuel_per_tick * idle_basis_points / 10_000` (at least one unit once the
/// running engine has any nonzero budget). The burn is capped at the fuel left
/// so fuel never goes negative. A destroyed or off engine returns `None`.
pub fn tick_fuel(
    state: &VehicleResources,
    profile: &VehicleProfile,
    tick: u64,
    engine: Engine,
    occupied: bool,
    throttle: f32,
) -> Result<Option<Burn>, Reject> {
    let engine = engine.validate(occupied, throttle)?;
    if tick < state.tick {
        return Err(Reject::Stale);
    }
    if state.destroyed || engine == Engine::Off || state.fuel == 0 {
        return Ok(None);
    }
    let per_tick = u64::from(profile.fuel_per_tick);
    let idle = per_tick * u64::from(profile.idle_basis_points) / 10_000;
    let moving = (f64::from(throttle.abs()) * per_tick as f64).round() as u64;
    let rate = idle.max(moving).max(1);
    let spent = u32::try_from(rate).unwrap_or(u32::MAX).min(state.fuel);
    if spent == 0 {
        return Ok(None);
    }
    let mut next = state.clone();
    next.fuel -= spent;
    next.tick = tick;
    Ok(Some(Burn {
        next: state.commit(next)?,
        spent,
        dry: state.fuel == spent,
    }))
}

/// Host-owned damage result, already validated by the host's damage rules.
#[derive(Clone, Debug, PartialEq)]
pub struct Damage {
    pub next: VehicleResources,
    /// True on the tick the vehicle transitions to destroyed.
    pub destroyed_now: bool,
}

/// Applies a positive, finite amount of damage. At most one tick of damage is
/// applied per atomic call; overflow clamps to destruction.
pub fn apply_damage(
    state: &VehicleResources,
    amount: f32,
    tick: u64,
) -> Result<Option<Damage>, Reject> {
    if !amount.is_finite() || amount <= 0.0 {
        return Err(Reject::InvalidStack("damage amount".into()));
    }
    if tick < state.tick {
        return Err(Reject::Stale);
    }
    if state.destroyed {
        return Ok(None);
    }
    let scaled = (f64::from(amount) * f64::from(HEALTH_SCALE)).round();
    let scaled = scaled.clamp(1.0, f64::from(u32::MAX)) as u32;
    let loss = scaled.min(state.health_centi);
    let health_centi = state.health_centi - loss;
    if loss == 0 {
        return Ok(None);
    }
    let mut next = state.clone();
    next.health_centi = health_centi;
    // A destroyed vehicle is a wreck: it does not burn fuel while waiting for
    // field repair, which also ends any running engine in the host.
    next.destroyed = health_centi == 0;
    next.tick = tick;
    let next = state.commit(next)?;
    Ok(Some(Damage {
        destroyed_now: next.destroyed,
        next,
    }))
}

/// Refuel proposal. Fuel units are credited only for canisters the plan
/// consumes (`count` units each). Slots are untouched if any check fails.
pub fn propose_refuel(
    state: &VehicleResources,
    profile: &VehicleProfile,
    canisters: &[(u16, Stack)],
) -> Result<Transaction, Reject> {
    if state.destroyed {
        return Err(Reject::Destroyed);
    }
    // `fuel` is public state a caller could have pushed past the tank. A full
    // (or overfull) tank simply has no room; subtracting must never wrap, which
    // would otherwise hand out fuel for free or panic on overflow.
    let room = profile.fuel_capacity.saturating_sub(state.fuel);
    if room == 0 {
        return Err(Reject::Full);
    }
    plan_consume(state, FieldKit::Fuel, canisters, room, |next, units| {
        next.fuel += units;
    })
}

/// Repair kit proposal. No kit is consumed beyond what is needed: fit kits
/// are used up to the missing health and the last partial kit is retained
/// with a reduced count, matching the item's per-use repair amount.
pub fn propose_repair(
    state: &VehicleResources,
    profile: &VehicleProfile,
    kits: &[(u16, Stack, u32)],
) -> Result<Transaction, Reject> {
    if state.destroyed {
        return Err(Reject::Destroyed);
    }
    let missing = profile.max_health_centi() - state.health_centi;
    if missing == 0 {
        return Err(Reject::Undamaged);
    }
    let mut writes = Vec::new();
    let mut seen = Vec::new();
    let mut healed = 0u32;
    for (index, stack, amount_centi) in kits {
        check_slot(&mut seen, *index)?;
        if *amount_centi == 0 {
            return Err(Reject::ZeroAmount);
        }
        check_stack(stack, FieldKit::Repair)?;
        let need = missing - healed;
        if need == 0 {
            break;
        }
        // One kit is one repair use, worth `amount_centi`, never more than the
        // damage still outstanding.
        let per_use = (*amount_centi).min(need);
        healed += per_use;
        writes.push(SlotWrite {
            index: *index,
            value: if stack.count > 1 {
                let mut left = stack.clone();
                left.count -= 1;
                Some(left)
            } else {
                None
            },
        });
    }
    if healed == 0 {
        return Err(Reject::Undamaged);
    }
    let mut next = state.clone();
    next.health_centi += healed;
    next.destroyed = false;
    finish(state, next, writes)
}

/// Restores a wreck to a bounded fraction of maximum health. It consumes one
/// repair kit and no fuel; the wreck keeps whatever fuel it had.
pub fn field_repair(
    state: &VehicleResources,
    profile: &VehicleProfile,
    kit: (u16, Stack),
    tick: u64,
) -> Result<Transaction, Reject> {
    if !state.destroyed {
        return Err(Reject::NotDestroyed);
    }
    if tick <= state.tick {
        return Err(Reject::Stale);
    }
    let (index, stack) = kit;
    let mut seen = Vec::new();
    check_slot(&mut seen, index)?;
    check_stack(&stack, FieldKit::Repair)?;
    let restored = (profile.max_health_centi() / 3).max(1);
    let mut next = state.clone();
    next.health_centi = restored;
    next.destroyed = false;
    next.tick = tick;
    let write = SlotWrite {
        index,
        value: if stack.count > 1 {
            let mut left = stack;
            left.count -= 1;
            Some(left)
        } else {
            None
        },
    };
    finish(state, next, vec![write])
}

fn finish(
    state: &VehicleResources,
    next: VehicleResources,
    writes: Vec<SlotWrite>,
) -> Result<Transaction, Reject> {
    if writes.len() > MAX_WRITES {
        return Err(Reject::TooManyWrites);
    }
    Ok(Transaction {
        next: state.commit(next)?,
        inventory: InventoryPlan { writes },
    })
}

fn check_slot(seen: &mut Vec<u16>, index: u16) -> Result<(), Reject> {
    if index >= 256 {
        return Err(Reject::InvalidSlot);
    }
    if seen.contains(&index) {
        return Err(Reject::DuplicateSlot);
    }
    seen.push(index);
    if seen.len() > MAX_WRITES {
        return Err(Reject::TooManyWrites);
    }
    Ok(())
}

fn check_stack(stack: &Stack, kit: FieldKit) -> Result<(), Reject> {
    if !stack.instance.is_valid() {
        return Err(Reject::InvalidStack("stack UUID".into()));
    }
    if stack.item != kit.item() {
        return Err(Reject::InvalidStack("item".into()));
    }
    if stack.count == 0 {
        return Err(Reject::InvalidStack("count".into()));
    }
    if !stack.state.is_empty() || stack.state_schema != 0 {
        return Err(Reject::InvalidStack("dirty state".into()));
    }
    Ok(())
}

fn plan_consume(
    state: &VehicleResources,
    kit: FieldKit,
    items: &[(u16, Stack)],
    room: u32,
    credit: impl Fn(&mut VehicleResources, u32),
) -> Result<Transaction, Reject> {
    let mut writes = Vec::new();
    let mut seen = Vec::new();
    let mut credited = 0u32;
    for (index, stack) in items {
        check_slot(&mut seen, *index)?;
        check_stack(stack, kit)?;
        let need = room - credited;
        if need == 0 {
            break;
        }
        let units = stack.count.min(need);
        credited += units;
        writes.push(SlotWrite {
            index: *index,
            value: if stack.count > units {
                let mut left = stack.clone();
                left.count -= units;
                Some(left)
            } else {
                None
            },
        });
    }
    if credited == 0 {
        return Err(Reject::ZeroAmount);
    }
    let mut next = state.clone();
    credit(&mut next, credited);
    finish(state, next, writes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> VehicleProfile {
        VehicleProfile::new(100.0, 1000, 10, 2_000).unwrap()
    }
    fn uuid(n: u64) -> Uuid {
        Uuid { high: n, low: 1 }
    }
    fn vehicle() -> VehicleResources {
        VehicleResources::new(uuid(7), &profile()).unwrap()
    }
    fn canister(n: u64, count: u32) -> Stack {
        Stack {
            instance: uuid(n),
            item: FieldKit::Fuel.item().into(),
            count,
            state_schema: 0,
            state: Vec::new(),
        }
    }
    fn kit(n: u64, count: u32) -> Stack {
        Stack {
            instance: uuid(n),
            item: FieldKit::Repair.item().into(),
            count,
            state_schema: 0,
            state: Vec::new(),
        }
    }
    fn drained() -> VehicleResources {
        let mut v = vehicle();
        for t in 1..=100 {
            v = tick_fuel(&v, &profile(), t, Engine::Running, true, 1.0)
                .unwrap()
                .unwrap()
                .next;
        }
        assert_eq!(v.fuel, 0);
        v
    }

    #[test]
    fn burn_scales_with_throttle_and_is_deterministic() {
        let p = profile();
        let v = vehicle();
        let half = tick_fuel(&v, &p, 5, Engine::Running, true, 0.5)
            .unwrap()
            .unwrap();
        assert_eq!(half.spent, 5);
        let full = tick_fuel(&v, &p, 5, Engine::Running, true, -1.0)
            .unwrap()
            .unwrap();
        assert_eq!(full.spent, 10);
        let idle = tick_fuel(&v, &p, 5, Engine::Running, true, 0.0)
            .unwrap()
            .unwrap();
        assert_eq!(idle.spent, 2);
        assert_eq!(idle.next.revision, v.revision + 1);
        // Replaying the same tick on the same state gives the same result.
        let again = tick_fuel(&v, &p, 5, Engine::Running, true, 0.5)
            .unwrap()
            .unwrap();
        assert_eq!(half, again);
    }

    #[test]
    fn engine_requires_occupant_and_valid_throttle() {
        let p = profile();
        let v = vehicle();
        assert!(tick_fuel(&v, &p, 1, Engine::Running, false, 1.0).is_err());
        assert!(tick_fuel(&v, &p, 1, Engine::Running, true, f32::NAN).is_err());
        assert!(tick_fuel(&v, &p, 1, Engine::Running, true, 1.5).is_err());
        assert_eq!(tick_fuel(&v, &p, 1, Engine::Off, false, 5.0).unwrap(), None);
    }

    #[test]
    fn run_dry_and_no_free_fuel() {
        let p = profile();
        let v = drained();
        assert_eq!(
            tick_fuel(&v, &p, 101, Engine::Running, true, 1.0).unwrap(),
            None
        );
        let last = drained();
        assert_eq!(last.fuel, 0);
        // Refueling with no canisters is refused.
        assert_eq!(propose_refuel(&v, &p, &[]), Err(Reject::ZeroAmount));
        // A full tank takes no fuel and therefore consumes nothing.
        assert_eq!(
            propose_refuel(&vehicle(), &p, &[(0, canister(1, 4))]),
            Err(Reject::Full)
        );
    }

    #[test]
    fn dry_on_partial_tick_reports_engine_stop() {
        let p = profile();
        let mut v = vehicle();
        v.fuel = 3;
        let burn = tick_fuel(&v, &p, 1, Engine::Running, true, 1.0)
            .unwrap()
            .unwrap();
        assert_eq!(burn.spent, 3);
        assert!(burn.dry);
        assert_eq!(burn.next.fuel, 0);
    }

    #[test]
    fn refuel_credits_exactly_what_the_plan_consumes() {
        let p = profile();
        let mut v = vehicle();
        v.fuel = 970;
        let t = propose_refuel(&v, &p, &[(4, canister(1, 30)), (9, canister(2, 10))]).unwrap();
        assert_eq!(t.next.fuel, 1000);
        assert_eq!(t.next.revision, v.revision + 1);
        // Fuel units are borrowed one-for-one from stack counts: exactly 30
        // units fill the tank, so the first can is drained and the second,
        // which was never needed, is left whole and unwritten.
        assert_eq!(
            t.inventory.writes,
            vec![SlotWrite {
                index: 4,
                value: None
            }]
        );
        let encoded = t.next.encode();
        assert_eq!(
            VehicleResources::decode(&encoded, uuid(7), &p).unwrap(),
            t.next
        );
    }

    #[test]
    fn refuel_partial_stack_returns_reduced_count() {
        let p = profile();
        let mut v = vehicle();
        // Three units of room against a five-unit stack: three are taken and the
        // slot keeps the remaining two.
        v.fuel = 997;
        let t = propose_refuel(&v, &p, &[(2, canister(3, 5))]).unwrap();
        assert_eq!(t.next.fuel, 1000);
        assert_eq!(t.inventory.writes.len(), 1);
        let w = &t.inventory.writes[0];
        assert_eq!(w.index, 2);
        assert_eq!(w.value.as_ref().unwrap().count, 2);
    }

    #[test]
    fn filled_or_foreign_stacks_are_never_accepted() {
        let p = profile();
        let mut v = vehicle();
        v.fuel = 0;
        let mut dirty = canister(1, 4);
        dirty.state = vec![1];
        dirty.state_schema = 1;
        assert!(propose_refuel(&v, &p, &[(0, dirty)]).is_err());
        assert!(propose_refuel(&v, &p, &[(0, kit(1, 4))]).is_err());
        let mut zero = canister(2, 0);
        zero.count = 0;
        assert!(propose_refuel(&v, &p, &[(0, zero)]).is_err());
        let mut noid = canister(0, 4);
        noid.instance = Uuid { high: 0, low: 0 };
        assert!(propose_refuel(&v, &p, &[(0, noid)]).is_err());
        assert_eq!(
            propose_refuel(&v, &p, &[(0, canister(1, 1)), (0, canister(2, 1))]),
            Err(Reject::DuplicateSlot)
        );
        assert_eq!(
            propose_refuel(&v, &p, &[(256, canister(1, 1))]),
            Err(Reject::InvalidSlot)
        );
        let too_many: Vec<_> = (0..=MAX_WRITES as u16)
            .map(|i| (i, canister(u64::from(i) + 1, 1)))
            .collect();
        assert_eq!(
            propose_refuel(&v, &p, &too_many),
            Err(Reject::TooManyWrites)
        );
    }

    #[test]
    fn repair_heals_only_its_kit_units_and_is_bounded() {
        let p = profile();
        let mut v = vehicle();
        let d = apply_damage(&v, 40.0, 1).unwrap().unwrap();
        v = d.next;
        assert_eq!(v.health(), 60.0);
        let t = propose_repair(&v, &p, &[(1, kit(1, 2), 1500)]).unwrap();
        // Health is in hundredths: one kit use restores 1500 centi (15 points),
        // so 6000 becomes 7500, and the second kit stays in the slot.
        assert_eq!(t.next.health_centi, 7500);
        assert_eq!(t.next.health(), 75.0);
        assert_eq!(t.inventory.writes[0].value.as_ref().unwrap().count, 1);
        assert_eq!(
            propose_repair(&vehicle(), &p, &[(1, kit(1, 1), 1000)]),
            Err(Reject::Undamaged)
        );
        assert!(propose_repair(&v, &p, &[(1, canister(1, 1), 1000)]).is_err());
        assert_eq!(
            propose_repair(&v, &p, &[(1, kit(1, 1), 0)]),
            Err(Reject::ZeroAmount)
        );
    }

    #[test]
    fn damage_destroys_at_zero_and_blocks_fuel_and_repair() {
        let p = profile();
        let v = vehicle();
        let d = apply_damage(&v, 1e9, 3).unwrap().unwrap();
        assert!(d.next.destroyed);
        assert!(d.destroyed_now);
        assert_eq!(d.next.health(), 0.0);
        assert_eq!(
            tick_fuel(&d.next, &p, 4, Engine::Running, true, 1.0).unwrap(),
            None
        );
        assert_eq!(
            propose_refuel(&d.next, &p, &[(0, canister(1, 1))]),
            Err(Reject::Destroyed)
        );
        assert_eq!(
            propose_repair(&d.next, &p, &[(0, kit(1, 1), 9999)]),
            Err(Reject::Destroyed)
        );
        assert_eq!(apply_damage(&d.next, 5.0, 4).unwrap(), None);
        assert!(apply_damage(&v, f32::INFINITY, 1).is_err());
        assert!(apply_damage(&v, -1.0, 1).is_err());
        // A wreck loses no fuel over time.
        let mut wreck = d.next.clone();
        wreck.fuel = 10;
        assert_eq!(
            tick_fuel(&wreck, &p, 9, Engine::Running, true, 1.0).unwrap(),
            None
        );
    }

    #[test]
    fn field_repair_needs_a_kit_and_restores_a_bounded_fraction() {
        let p = profile();
        let v = vehicle();
        let wreck = apply_damage(&v, 100.0, 1).unwrap().unwrap().next;
        // The wreck's last tick is the damage tick (1); resurrection must be
        // strictly later, so replaying tick 1 is stale.
        assert_eq!(
            field_repair(&wreck, &p, (0, kit(1, 2)), 1).unwrap_err(),
            Reject::Stale
        );
        assert!(field_repair(&v, &p, (0, kit(1, 2)), 2).is_err());
        assert!(field_repair(&wreck, &p, (0, canister(1, 1)), 2).is_err());
        let t = field_repair(&wreck, &p, (0, kit(1, 2)), 2).unwrap();
        assert!(!t.next.destroyed);
        // A third of maximum health, floored to the centi grid, never more.
        assert_eq!(t.next.health_centi, p.max_health_centi() / 3);
        assert!(t.next.health() < p.max_health);
        assert_eq!(t.next.revision, wreck.revision + 1);
        assert_eq!(t.inventory.writes[0].value.as_ref().unwrap().count, 1);
        // Restored wreck can burn fuel again.
        assert!(
            tick_fuel(&t.next, &p, 3, Engine::Running, true, 1.0)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn stale_ticks_and_revision_overflow_are_rejected() {
        let p = profile();
        let v = tick_fuel(&vehicle(), &p, 50, Engine::Running, true, 1.0)
            .unwrap()
            .unwrap()
            .next;
        assert_eq!(
            tick_fuel(&v, &p, 49, Engine::Running, true, 1.0),
            Err(Reject::Stale)
        );
        let mut maxed = v.clone();
        maxed.revision = u64::MAX;
        assert_eq!(
            tick_fuel(&maxed, &p, 51, Engine::Running, true, 1.0),
            Err(Reject::RevisionOverflow)
        );
    }

    #[test]
    fn corrupt_or_foreign_saves_are_not_reinitialized() {
        let p = profile();
        let v = vehicle();
        let encoded = v.encode();
        assert!(encoded.len() <= STATE_BYTES);
        assert!(VehicleResources::decode(&encoded, uuid(8), &p).is_err());
        assert!(VehicleResources::decode(&[], uuid(7), &p).is_err());
        assert!(VehicleResources::decode(&[255; STATE_BYTES + 1], uuid(7), &p).is_err());
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(VehicleResources::decode(&trailing, uuid(7), &p).is_err());
        let mut over = v.clone();
        over.fuel = 1001;
        assert_eq!(
            VehicleResources::decode(&over.encode(), uuid(7), &p),
            Err(Reject::CorruptState)
        );
        let mut wreck = v.clone();
        wreck.health_centi = 0;
        wreck.destroyed = false;
        assert_eq!(
            VehicleResources::decode(&wreck.encode(), uuid(7), &p),
            Err(Reject::CorruptState)
        );
        let mut wrong_schema = v;
        wrong_schema.schema = 2;
        assert_eq!(
            VehicleResources::decode(&wrong_schema.encode(), uuid(7), &p),
            Err(Reject::CorruptState)
        );
        // A saved state from a smaller-tanked vehicle is not clamped.
        assert!(
            VehicleResources::decode(
                &encoded,
                uuid(7),
                &VehicleProfile::new(100.0, 50, 1, 0).unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn profiles_reject_meaningless_values() {
        assert!(VehicleProfile::new(f32::NAN, 1, 1, 0).is_err());
        assert!(VehicleProfile::new(0.0, 1, 1, 0).is_err());
        assert!(VehicleProfile::new(1.0, 0, 1, 0).is_err());
        assert!(VehicleProfile::new(1.0, 1, 0, 0).is_err());
        assert!(VehicleProfile::new(1.0, 1, 1, 10_001).is_err());
        assert!(VehicleResources::new(Uuid { high: 0, low: 0 }, &profile()).is_err());
    }

    /// Sums the item units a plan removes from the inventory. `before` maps a
    /// slot index to the count it held before the transaction; `None` clears
    /// the whole stack.
    fn removed(plan: &InventoryPlan, before: &[(u16, u32)]) -> u32 {
        plan.writes
            .iter()
            .map(|w| {
                let start = before.iter().find(|(i, _)| *i == w.index).unwrap().1;
                match &w.value {
                    Some(left) => start - left.count,
                    None => start,
                }
            })
            .sum()
    }

    #[test]
    fn refuel_and_repair_are_conservative() {
        let p = profile();
        let mut v = vehicle();
        v.fuel = 0;
        v.health_centi = 0;
        v.destroyed = true;
        // A wreck refuses both ordinary transactions outright.
        assert_eq!(
            propose_refuel(&v, &p, &[(0, canister(1, 10))]),
            Err(Reject::Destroyed)
        );
        assert_eq!(
            propose_repair(&v, &p, &[(0, kit(1, 1), 500)]),
            Err(Reject::Destroyed)
        );

        // Refueling credits exactly the canister units the plan drains, and
        // stops consuming once the tank is full.
        let mut v = vehicle();
        v.fuel = 900;
        let cans = [(3, canister(1, 60)), (5, canister(2, 60))];
        let t = propose_refuel(&v, &p, &cans).unwrap();
        let credited = t.next.fuel - v.fuel;
        assert_eq!(credited, removed(&t.inventory, &[(3, 60), (5, 60)]));
        // The first canister's 60 units fill part of the 100 room; the second
        // gives the remaining 40 and keeps 20 in its slot.
        assert_eq!(credited, 100);
        assert_eq!(t.inventory.writes.len(), 2);
        assert_eq!(t.inventory.writes[0].index, 3);
        assert_eq!(t.inventory.writes[0].value, None);
        assert_eq!(t.inventory.writes[1].index, 5);
        assert_eq!(t.inventory.writes[1].value.as_ref().unwrap().count, 20);

        // Repair heals exactly the kit units the plan consumes, at the per-kit
        // amount the item declares (health is in hundredths of a point, so the
        // kit count and the centi healed are different units).
        let mut v = vehicle();
        v.health_centi = 9000;
        let per_kit_centi = 100u32;
        let kits = [(2, kit(1, 3), per_kit_centi)];
        let t = propose_repair(&v, &p, &kits).unwrap();
        let healed = t.next.health_centi - v.health_centi;
        assert_eq!(healed, removed(&t.inventory, &[(2, 3)]) * per_kit_centi);
        assert_eq!(healed, per_kit_centi);
    }

    #[test]
    fn an_overfull_tank_has_no_room_and_never_wraps() {
        let p = profile();
        let mut v = vehicle();
        // `fuel` is public state a caller could have pushed past the tank. The
        // room calculation must saturate, not underflow into free fuel.
        v.fuel = p.fuel_capacity + 5;
        assert_eq!(
            propose_refuel(&v, &p, &[(0, canister(1, 4))]),
            Err(Reject::Full)
        );
    }

    #[test]
    fn transaction_revision_is_the_commit_cas_token() {
        let p = profile();
        let mut v = vehicle();
        v.fuel = 990;
        let t = propose_refuel(&v, &p, &[(0, canister(1, 4))]).unwrap();
        // The proposal is derived from exactly one input revision ...
        assert_eq!(t.next.revision, v.revision + 1);
        // ... and a replay against the unchanged input converges identically,
        // which is what a caller relies on when it retries a refused commit.
        let retry = propose_refuel(&v, &p, &[(0, canister(1, 4))]).unwrap();
        assert_eq!(t, retry);
        // Encoding the accepted state survives the strict decode unchanged.
        assert_eq!(
            VehicleResources::decode(&t.next.encode(), uuid(7), &p).unwrap(),
            t.next
        );
    }
}
