//! Deterministic, server-owned vehicle weapon and turret rules.
//!
//! # Scope and honest limits
//!
//! This module is the *rules* half of mounted weapons. It decides, for one
//! authoritative tick, whether a seat occupant may traverse a turret and whether
//! a fire intent is accepted; accepted fire produces one [`ProjectileRequest`]
//! for the host to spawn. It never applies damage, never writes a pose, and never
//! trusts a client-supplied aim vector, muzzle position, spread or cooldown.
//!
//! Deliberately **not** implemented yet:
//!
//! * **Multi-seat native mounting is not wired.** In the shipped mod, turrets are
//!   still resolved host-side from a single hard-coded gunner seat. Native
//!   multi-seat attachment (a driver plus independent gunners bound to distinct
//!   turret indices) is exercised only by this module's tests. A root integration
//!   step must map each [`SeatId`] to a real native seat before the rules below
//!   mean anything in game.
//! * Ammunition is an abstract round count. It does not consume inventory stacks;
//!   the host must reconcile each accepted shot with the item stack it drained.
//! * Ballistics, penetration and ricochet live in `ballistics`. The
//!   [`ProjectileRequest`] here is the input to that module, not a substitute.
//!
//! # Conventions
//!
//! * One [`VehicleWeapons::tick`] is exactly one server tick at
//!   [`sbw_contract::TICK_RATE`]. There is no wall clock: cadence is counted in
//!   ticks and a tick advances by exactly one.
//! * Yaw is stored wrapped to `(-pi, pi]`; pitch is stored clamped to the
//!   mount's `[pitch_min, pitch_max]`. Traverse is capped per tick by the mount's
//!   rate, so a hostile caller cannot snap a turret instantly.
//! * Every external number is sanitized to a finite value before use. Non-finite
//!   aim or axis input is rejected as [`FireError::NonFinite`] rather than
//!   poisoning state with NaN.

use sbw_contract::types::{EntityKey, Vec3};
use std::f64::consts::PI;

/// Tolerance for the half-open wrap boundary and float comparisons.
pub const WRAP_EPSILON: f64 = 1.0e-9;
/// Upper bound on stored rounds in a magazine or reserve bin.
pub const MAX_ROUNDS: u32 = 4096;
/// Upper bound on ticks a reload or cooldown may last.
pub const MAX_TICKS: u32 = 20 * 60 * 60;

/// Identifies which occupant of a vehicle a request is attributed to.
///
/// Seat zero is the driver seat, which may own a driver-operated weapon. Seats
/// one and up are gunner seats. The host maps these to native seats; see the
/// module note on multi-seat mounting.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SeatId(pub u16);

impl SeatId {
    /// The vehicle's driver position.
    pub const DRIVER: SeatId = SeatId(0);
    /// Whether this seat may operate a turret at all (any seat other than driver
    /// may be a gunner once the host binds it).
    pub fn is_gunner(self) -> bool {
        self.0 != Self::DRIVER.0
    }
}

/// Signature of an occupant's authorization for one seat, minted by the host.
///
/// The sim never guesses seat ownership: it compares the [`SeatId`] an occupant
/// presents with the seat this grant names. A grant for seat 2 cannot fire seat
/// 0's driver weapon.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OccupantGrant {
    /// The player instance the host authenticated for this seat.
    pub occupant: sbw_contract::types::Uuid,
    pub seat: SeatId,
}

impl OccupantGrant {
    pub fn new(occupant: sbw_contract::types::Uuid, seat: SeatId) -> Self {
        Self { occupant, seat }
    }
}

/// Static, data-driven description of one mounted weapon.
///
/// Values are data, not engine code. A hostile profile cannot widen traverse,
/// raise cadence or enlarge a magazine past the constants above.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TurretProfile {
    /// Traverse rate in radians per tick about the vehicle's vertical axis.
    pub yaw_rate_per_tick: f64,
    /// Elevation rate in radians per tick.
    pub pitch_rate_per_tick: f64,
    /// Lowest elevation the mount can reach, radians. Negative points down.
    pub pitch_min: f64,
    /// Highest elevation the mount can reach, radians.
    pub pitch_max: f64,
    /// Smallest gap between accepted shots, in ticks. Zero is refused.
    pub cooldown_ticks: u32,
    /// Ticks a magazine swap occupies. Zero is refused.
    pub reload_ticks: u32,
    /// Rounds a full magazine holds. Zero is refused.
    pub magazine: u32,
    /// Rounds carried outside the magazine.
    pub reserve: u32,
    /// Whether this mount may be held and fired continuously.
    pub automatic: bool,
}

/// Why building or mutating a turret failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileError {
    /// A rate or limit was NaN, infinite, negative, or above a hard cap.
    NonFinite(&'static str),
    /// A range was inverted or a required count was zero.
    OutOfRange(&'static str),
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileError::NonFinite(name) => write!(f, "non-finite or over-limit {name}"),
            ProfileError::OutOfRange(name) => write!(f, "out-of-range {name}"),
        }
    }
}
impl std::error::Error for ProfileError {}

impl TurretProfile {
    /// Validate and normalize a profile supplied by a data file or caller.
    pub fn sanitize(self) -> Result<Self, ProfileError> {
        let rates = [
            (self.yaw_rate_per_tick, "yaw rate"),
            (self.pitch_rate_per_tick, "pitch rate"),
            (self.pitch_min, "pitch minimum"),
            (self.pitch_max, "pitch maximum"),
        ];
        for (value, name) in rates {
            if !value.is_finite() {
                return Err(ProfileError::NonFinite(name));
            }
        }
        if !(0.0..=PI).contains(&self.yaw_rate_per_tick) {
            return Err(ProfileError::OutOfRange("yaw rate"));
        }
        if !(0.0..=PI).contains(&self.pitch_rate_per_tick) {
            return Err(ProfileError::OutOfRange("pitch rate"));
        }
        if self.pitch_min < -PI || self.pitch_max > PI || self.pitch_min > self.pitch_max {
            return Err(ProfileError::OutOfRange("pitch range"));
        }
        if self.cooldown_ticks == 0 {
            return Err(ProfileError::OutOfRange("cooldown"));
        }
        if self.reload_ticks == 0 {
            return Err(ProfileError::OutOfRange("reload ticks"));
        }
        if self.magazine == 0 || self.magazine > MAX_ROUNDS {
            return Err(ProfileError::OutOfRange("magazine"));
        }
        if self.reserve > MAX_ROUNDS {
            return Err(ProfileError::OutOfRange("reserve"));
        }
        if self.cooldown_ticks > MAX_TICKS || self.reload_ticks > MAX_TICKS {
            return Err(ProfileError::OutOfRange("duration"));
        }
        // Rounds and rates are already inside their bands; nothing is silently
        // widened here, so a caller that reads back a sanitized profile sees
        // exactly the constraints it will be held to.
        Ok(self)
    }

    /// Whether an aim elevation is reachable by this mount.
    pub fn pitch_in_range(&self, pitch: f64) -> bool {
        pitch.is_finite() && pitch >= self.pitch_min && pitch <= self.pitch_max
    }
}

/// A fire intent as the host relays it. Nothing here is trusted; the aim is
/// clamped to this mount's reach and its own traverse rate before it is applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FireIntent {
    /// Seat the request is attributed to. Checked against the occupant grant.
    pub seat: SeatId,
    /// Desired absolute yaw, radians. Wrapped to `(-pi, pi]`.
    pub aim_yaw: f64,
    /// Desired absolute elevation, radians. Clamped to the mount's range.
    pub aim_pitch: f64,
}

/// Why a fire intent was not turned into a projectile request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FireError {
    /// The granted occupant does not hold the seat the intent names.
    UnauthorizedSeat,
    /// The mount is reloading; it cannot fire until the swap ends.
    Reloading,
    /// Fewer ticks than `cooldown_ticks` have passed since the last shot.
    CoolingDown,
    /// The magazine is empty and the reserve is empty too.
    Empty,
    /// An aim component was NaN or infinite.
    NonFinite,
}

impl std::fmt::Display for FireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            FireError::UnauthorizedSeat => "occupant does not hold that seat",
            FireError::Reloading => "weapon is reloading",
            FireError::CoolingDown => "weapon is cooling down",
            FireError::Empty => "no rounds available",
            FireError::NonFinite => "aim is not finite",
        };
        f.write_str(text)
    }
}
impl std::error::Error for FireError {}

/// What the host is asked to spawn. Positional data is deliberately absent: the
/// host owns pose, so it places the projectile from the accepted muzzle, not
/// from anything the client sent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectileRequest {
    /// Vehicle the shot came from.
    pub vehicle: EntityKey,
    /// Seat that fired.
    pub seat: SeatId,
    /// Absolute yaw the turret actually reached, radians.
    pub yaw: f64,
    /// Absolute elevation the turret actually reached, radians.
    pub pitch: f64,
    /// Authoritative tick the shot is attributed to.
    pub tick: u64,
}

/// One mounted weapon's authoritative state.
#[derive(Clone, Debug)]
pub struct Turret {
    profile: TurretProfile,
    yaw: f64,
    pitch: f64,
    rounds: u32,
    reserve: u32,
    last_shot_tick: Option<u64>,
    /// Absolute tick at which an in-progress reload completes.
    reload_end: Option<u64>,
}

impl Turret {
    /// Start a turret pointed along `yaw` and resting at the mount's lowest
    /// elevation. Initial aim is clamped, never trusted.
    pub fn new(profile: TurretProfile, yaw: f64) -> Result<Self, ProfileError> {
        let profile = profile.sanitize()?;
        let yaw = if yaw.is_finite() { wrap_pi(yaw) } else { 0.0 };
        Ok(Self {
            pitch: profile.pitch_min,
            rounds: profile.magazine,
            reserve: profile.reserve,
            yaw,
            profile,
            last_shot_tick: None,
            reload_end: None,
        })
    }

    pub fn profile(&self) -> &TurretProfile {
        &self.profile
    }
    pub fn yaw(&self) -> f64 {
        self.yaw
    }
    pub fn pitch(&self) -> f64 {
        self.pitch
    }
    pub fn rounds(&self) -> u32 {
        self.rounds
    }
    pub fn reserve(&self) -> u32 {
        self.reserve
    }
    pub fn is_reloading(&self, tick: u64) -> bool {
        self.reload_end.is_some_and(|end| tick < end)
    }

    /// Traverse toward an aim by at most one tick's rotation, clamping elevation.
    /// Returns nothing; inspect [`Turret::yaw`] and [`Turret::pitch`]. Non-finite
    /// axes are ignored so a caller cannot corrupt the mount's stored aim.
    pub fn aim(&mut self, aim_yaw: f64, aim_pitch: f64) {
        if !aim_yaw.is_finite() || !aim_pitch.is_finite() {
            return;
        }
        let desired_yaw = wrap_pi(aim_yaw);
        let desired_pitch = aim_pitch.clamp(self.profile.pitch_min, self.profile.pitch_max);
        self.yaw = self.step_angle(self.yaw, desired_yaw, self.profile.yaw_rate_per_tick);
        self.pitch = self.step_angle(self.pitch, desired_pitch, self.profile.pitch_rate_per_tick);
    }

    /// One-angle move along the shorter arc, capped at `rate` radians.
    fn step_angle(&self, from: f64, to: f64, rate: f64) -> f64 {
        let delta = wrap_pi(to - from);
        let limited = delta.clamp(-rate, rate);
        wrap_pi(from + limited)
    }

    /// Begin a magazine swap if one is not already running. Returns whether a
    /// swap started. Refused when the magazine is full or nothing is in reserve.
    pub fn begin_reload(&mut self, tick: u64) -> bool {
        if self.is_reloading(tick) || self.reserve == 0 || self.rounds == self.profile.magazine {
            return false;
        }
        self.reload_end = Some(tick.saturating_add(u64::from(self.profile.reload_ticks)));
        true
    }

    /// Complete any reload whose end tick has arrived, moving reserve into the
    /// magazine. Idempotent; call every tick or once before firing.
    pub fn complete_reload(&mut self, tick: u64) {
        let Some(end) = self.reload_end else { return };
        if tick < end {
            return;
        }
        let want = self.profile.magazine.saturating_sub(self.rounds);
        let moved = want.min(self.reserve);
        self.rounds += moved;
        self.reserve -= moved;
        self.reload_end = None;
    }

    /// Attempt to turn an intent into an accepted shot, traversing the turret
    /// first. On success the round is decremented and the cooldown armed.
    pub fn try_fire(
        &mut self,
        vehicle: EntityKey,
        grant: OccupantGrant,
        intent: FireIntent,
        tick: u64,
    ) -> Result<ProjectileRequest, FireError> {
        if grant.seat != intent.seat {
            return Err(FireError::UnauthorizedSeat);
        }
        if !intent.aim_yaw.is_finite() || !intent.aim_pitch.is_finite() {
            return Err(FireError::NonFinite);
        }
        self.aim(intent.aim_yaw, intent.aim_pitch);
        self.complete_reload(tick);
        if self.is_reloading(tick) {
            return Err(FireError::Reloading);
        }
        if let Some(last) = self.last_shot_tick {
            let ready = last.saturating_add(u64::from(self.profile.cooldown_ticks));
            if tick < ready {
                return Err(FireError::CoolingDown);
            }
        }
        if self.rounds == 0 {
            return Err(FireError::Empty);
        }
        self.rounds -= 1;
        self.last_shot_tick = Some(tick);
        Ok(ProjectileRequest {
            vehicle,
            seat: intent.seat,
            yaw: self.yaw,
            pitch: self.pitch,
            tick,
        })
    }
}

/// Wrap an angle to the half-open interval `(-pi, pi]`.
pub fn wrap_pi(radians: f64) -> f64 {
    if !radians.is_finite() {
        return 0.0;
    }
    let mut wrapped = (radians + PI).rem_euclid(2.0 * PI) - PI;
    if wrapped <= -PI + WRAP_EPSILON {
        wrapped = PI;
    }
    wrapped
}

/// All mounted weapons on every tracked vehicle, advanced one authoritative tick.
///
/// The collection is server-owned and bounded by
/// [`sbw_contract::limits::TRACKED_ENTITIES`].
#[derive(Default, Debug)]
pub struct VehicleWeapons {
    mounts: Vec<Mount>,
    tick: u64,
}

#[derive(Debug)]
struct Mount {
    vehicle: EntityKey,
    turret: Turret,
}

impl VehicleWeapons {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last tick this collection advanced to.
    pub fn current_tick(&self) -> u64 {
        self.tick
    }

    /// Mount one turret on a vehicle. Refused when the vehicle already carries
    /// the maximum mounts, so a caller cannot grow state without bound.
    pub fn mount(
        &mut self,
        vehicle: EntityKey,
        profile: TurretProfile,
        yaw: f64,
    ) -> Result<(), ProfileError> {
        if self.mounts.len() >= sbw_contract::limits::TRACKED_ENTITIES {
            return Err(ProfileError::OutOfRange("mount count"));
        }
        let turret = Turret::new(profile, yaw)?;
        self.mounts.push(Mount { vehicle, turret });
        Ok(())
    }

    pub fn turret(&self, vehicle: EntityKey) -> Option<&Turret> {
        self.mounts
            .iter()
            .find(|m| m.vehicle == vehicle)
            .map(|m| &m.turret)
    }

    pub fn turret_mut(&mut self, vehicle: EntityKey) -> Option<&mut Turret> {
        self.mounts
            .iter_mut()
            .find(|m| m.vehicle == vehicle)
            .map(|m| &mut m.turret)
    }

    /// Advance every mount by one tick, finishing reloads that came due. Returns
    /// the new authoritative tick. There is no wall clock; a tick is one step.
    pub fn tick(&mut self) -> u64 {
        self.tick = self.tick.saturating_add(1);
        for mount in &mut self.mounts {
            mount.turret.complete_reload(self.tick);
        }
        self.tick
    }

    /// Traverse a mount without firing. Returns whether the mount exists.
    pub fn aim(
        &mut self,
        vehicle: EntityKey,
        intent: FireIntent,
        grant: OccupantGrant,
    ) -> Result<(), FireError> {
        let mount = self
            .mounts
            .iter_mut()
            .find(|m| m.vehicle == vehicle)
            .ok_or(FireError::UnauthorizedSeat)?;
        if grant.seat != intent.seat {
            return Err(FireError::UnauthorizedSeat);
        }
        if !intent.aim_yaw.is_finite() || !intent.aim_pitch.is_finite() {
            return Err(FireError::NonFinite);
        }
        mount.turret.aim(intent.aim_yaw, intent.aim_pitch);
        Ok(())
    }

    /// Submit a fire intent and advance the mount by one tick of traversal.
    pub fn fire(
        &mut self,
        vehicle: EntityKey,
        grant: OccupantGrant,
        intent: FireIntent,
    ) -> Result<ProjectileRequest, FireError> {
        let tick = self.tick;
        let mount = self
            .mounts
            .iter_mut()
            .find(|m| m.vehicle == vehicle)
            .ok_or(FireError::UnauthorizedSeat)?;
        mount.turret.try_fire(vehicle, grant, intent, tick)
    }
}

/// Convenience: a unit direction from an accepted request, for the host's
/// projectile spawn. Kept here so the host never derives it from client aim.
pub fn direction(yaw: f64, pitch: f64) -> Vec3 {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    Vec3 {
        x: cy * cp,
        y: sp,
        z: sy * cp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(index: u64) -> EntityKey {
        EntityKey {
            index,
            generation: 1,
        }
    }

    fn uuid(low: u64) -> sbw_contract::types::Uuid {
        sbw_contract::types::Uuid { high: 1, low }
    }

    fn profile() -> TurretProfile {
        TurretProfile {
            yaw_rate_per_tick: 0.05,
            pitch_rate_per_tick: 0.05,
            pitch_min: -0.2,
            pitch_max: 1.0,
            cooldown_ticks: 4,
            reload_ticks: 40,
            magazine: 3,
            reserve: 6,
            automatic: true,
        }
    }

    fn grant(seat: u16) -> OccupantGrant {
        OccupantGrant::new(uuid(7), SeatId(seat))
    }

    fn intent(seat: u16, yaw: f64, pitch: f64) -> FireIntent {
        FireIntent {
            seat: SeatId(seat),
            aim_yaw: yaw,
            aim_pitch: pitch,
        }
    }

    #[test]
    fn unauthorized_seat_is_refused() {
        let mut weapons = VehicleWeapons::new();
        weapons.mount(key(1), profile(), 0.0).unwrap();
        // Gunner holds seat 1 but the intent names seat 0.
        let err = weapons
            .fire(key(1), grant(1), intent(0, 0.0, 0.0))
            .unwrap_err();
        assert_eq!(err, FireError::UnauthorizedSeat);
        assert_eq!(weapons.turret(key(1)).unwrap().rounds(), 3);
    }

    #[test]
    fn unknown_vehicle_is_refused() {
        let mut weapons = VehicleWeapons::new();
        let err = weapons
            .fire(key(9), grant(0), intent(0, 0.0, 0.0))
            .unwrap_err();
        assert_eq!(err, FireError::UnauthorizedSeat);
    }

    #[test]
    fn traverse_is_rate_limited_and_pitch_clamped() {
        let mut turret = Turret::new(profile(), 0.0).unwrap();
        // Ask for a huge yaw and unreachable pitch in one tick. Elevation is
        // rate-limited like yaw: one tick moves at most `pitch_rate_per_tick`
        // from the resting `pitch_min` (-0.2), so it reaches -0.15, not the
        // unreachable 5.0 it was asked for.
        turret.aim(3.0, 5.0);
        assert!((turret.yaw() - 0.05).abs() < 1e-12, "yaw {}", turret.yaw());
        assert!(
            (turret.pitch() - (-0.15)).abs() < 1e-12,
            "pitch {}",
            turret.pitch()
        );
        // Slows instead of the full rate when the target is nearer than one step.
        turret.aim(0.02, 1.0);
        assert!((turret.yaw() - 0.02).abs() < 1e-12, "yaw {}", turret.yaw());
        // Asked to stay put, it does not drift past the target.
        turret.aim(0.02, 1.0);
        assert!((turret.yaw() - 0.02).abs() < 1e-12, "yaw {}", turret.yaw());
        // Enough ticks eventually reach the elevation ceiling, where the mount
        // clamps: it never overshoots `pitch_max`, however hard it is pushed.
        for _ in 0..40 {
            turret.aim(0.0, 9.0);
        }
        assert!(
            (turret.pitch() - 1.0).abs() < 1e-12,
            "pitch {}",
            turret.pitch()
        );
        assert!((turret.yaw() - 0.0).abs() < 1e-12, "yaw {}", turret.yaw());
    }

    #[test]
    fn non_finite_aim_is_ignored_not_stored() {
        let mut turret = Turret::new(profile(), 0.1).unwrap();
        turret.aim(f64::NAN, f64::INFINITY);
        // `wrap_pi` is exact only up to float rounding, so compare with a
        // tolerance rather than for bit equality.
        assert!((turret.yaw() - 0.1).abs() < 1e-12, "yaw {}", turret.yaw());
        assert_eq!(turret.pitch(), -0.2);
    }

    #[test]
    fn yaw_wraps_to_half_open_interval() {
        assert!((wrap_pi(0.0) - 0.0).abs() < 1e-12);
        assert!((wrap_pi(PI) - PI).abs() < 1e-12);
        assert!((wrap_pi(-PI) - PI).abs() < 1e-12);
        assert!((wrap_pi(3.0 * PI) - PI).abs() < 1e-12);
        assert!((wrap_pi(-3.0 * PI) - PI).abs() < 1e-12);
        assert!(wrap_pi(PI + 0.5) < 0.0);
        assert_eq!(wrap_pi(f64::NAN), 0.0);
    }

    #[test]
    fn reload_moves_reserve_and_blocks_fire() {
        let mut turret = Turret::new(profile(), 0.0).unwrap();
        let vehicle = key(1);
        // Empty the magazine of 3 rounds across four-tick gaps.
        for tick in [0u64, 4, 8] {
            turret
                .try_fire(vehicle, grant(0), intent(0, 0.0, 0.0), tick)
                .unwrap();
        }
        assert_eq!(turret.rounds(), 0);
        assert_eq!(
            turret
                .try_fire(vehicle, grant(0), intent(0, 0.0, 0.0), 12)
                .unwrap_err(),
            FireError::Empty
        );
        assert!(turret.begin_reload(12));
        assert!(turret.is_reloading(20));
        // Firing mid-reload is refused, not merely empty.
        assert_eq!(
            turret
                .try_fire(vehicle, grant(0), intent(0, 0.0, 0.0), 20)
                .unwrap_err(),
            FireError::Reloading
        );
        turret.complete_reload(52);
        assert!(!turret.is_reloading(52));
        assert_eq!(turret.rounds(), 3);
        assert_eq!(turret.reserve(), 3);
        // Idempotent: a second completion does not conjure rounds.
        turret.complete_reload(60);
        assert_eq!(turret.rounds(), 3);
    }

    #[test]
    fn reload_refused_when_full_or_empty_reserve() {
        let mut turret = Turret::new(profile(), 0.0).unwrap();
        assert!(!turret.begin_reload(0), "full magazine");
        let mut empty = Turret::new(
            TurretProfile {
                reserve: 0,
                ..profile()
            },
            0.0,
        )
        .unwrap();
        assert!(!empty.begin_reload(0), "nothing in reserve");
    }

    #[test]
    fn cadence_enforces_cooldown_ticks() {
        let mut turret = Turret::new(profile(), 0.0).unwrap();
        let vehicle = key(1);
        turret
            .try_fire(vehicle, grant(0), intent(0, 0.0, 0.0), 10)
            .unwrap();
        assert_eq!(
            turret
                .try_fire(vehicle, grant(0), intent(0, 0.0, 0.0), 13)
                .unwrap_err(),
            FireError::CoolingDown
        );
        // Exactly cooldown_ticks later is accepted.
        turret
            .try_fire(vehicle, grant(0), intent(0, 0.0, 0.0), 14)
            .unwrap();
    }

    #[test]
    fn accepted_fire_reports_traversed_aim_not_client_aim() {
        let mut weapons = VehicleWeapons::new();
        weapons.mount(key(1), profile(), 0.0).unwrap();
        let shot = weapons.fire(key(1), grant(0), intent(0, 3.0, 9.0)).unwrap();
        assert!((shot.yaw - 0.05).abs() < 1e-12, "yaw {}", shot.yaw);
        // The reported elevation is the traversed one (one rate step above the
        // resting pitch_min), never the client's unreachable 9.0.
        assert!((shot.pitch - (-0.15)).abs() < 1e-12, "pitch {}", shot.pitch);
        assert_eq!(shot.tick, 0);
        assert_eq!(shot.seat, SeatId::DRIVER);
    }

    #[test]
    fn collection_ticks_advance_and_complete_reloads() {
        let mut weapons = VehicleWeapons::new();
        weapons.mount(key(1), profile(), 0.0).unwrap();
        // Empty the magazine. Only `VehicleWeapons::tick` advances the
        // authoritative clock, so the four-tick cooldown between shots has to
        // be spent explicitly between fires.
        for _ in 0..3 {
            weapons.fire(key(1), grant(0), intent(0, 0.0, 0.0)).unwrap();
            for _ in 0..profile().cooldown_ticks {
                weapons.tick();
            }
        }
        assert_eq!(weapons.turret(key(1)).unwrap().rounds(), 0);
        assert_eq!(
            weapons.current_tick(),
            3 * u64::from(profile().cooldown_ticks)
        );
        let started = weapons.current_tick();
        assert!(weapons.turret_mut(key(1)).unwrap().begin_reload(started));
        // A tick short of the swap keeps the mount reloading.
        weapons.tick();
        assert!(
            weapons
                .turret(key(1))
                .unwrap()
                .is_reloading(weapons.current_tick())
        );
        for _ in 0..profile().reload_ticks {
            weapons.tick();
        }
        assert!(
            !weapons
                .turret(key(1))
                .unwrap()
                .is_reloading(weapons.current_tick())
        );
        assert_eq!(weapons.turret(key(1)).unwrap().rounds(), 3);
        assert_eq!(weapons.turret(key(1)).unwrap().reserve(), 3);
    }

    #[test]
    fn profile_sanitize_rejects_bad_data() {
        assert_eq!(
            TurretProfile {
                yaw_rate_per_tick: f64::NAN,
                ..profile()
            }
            .sanitize()
            .unwrap_err(),
            ProfileError::NonFinite("yaw rate")
        );
        assert!(matches!(
            TurretProfile {
                pitch_min: 1.0,
                pitch_max: -1.0,
                ..profile()
            }
            .sanitize(),
            Err(ProfileError::OutOfRange("pitch range"))
        ));
        assert!(matches!(
            TurretProfile {
                magazine: 0,
                ..profile()
            }
            .sanitize(),
            Err(ProfileError::OutOfRange("magazine"))
        ));
        assert!(matches!(
            TurretProfile {
                cooldown_ticks: 0,
                ..profile()
            }
            .sanitize(),
            Err(ProfileError::OutOfRange("cooldown"))
        ));
        // An over-large magazine is refused outright, never silently widened.
        assert!(matches!(
            TurretProfile {
                magazine: 99_999,
                ..profile()
            }
            .sanitize(),
            Err(ProfileError::OutOfRange("magazine"))
        ));
        // A negative traverse rate is not a valid "fixed mount".
        assert!(matches!(
            TurretProfile {
                yaw_rate_per_tick: -0.1,
                ..profile()
            }
            .sanitize(),
            Err(ProfileError::OutOfRange("yaw rate"))
        ));
    }

    #[test]
    fn non_finite_intent_is_refused_by_fire() {
        let mut weapons = VehicleWeapons::new();
        weapons.mount(key(1), profile(), 0.0).unwrap();
        let err = weapons
            .fire(key(1), grant(0), intent(0, f64::NAN, 0.0))
            .unwrap_err();
        assert_eq!(err, FireError::NonFinite);
        // Nothing was consumed and no cooldown was armed.
        assert_eq!(weapons.turret(key(1)).unwrap().rounds(), 3);
    }

    #[test]
    fn direction_is_unit_length_for_cardinal_axes() {
        let up = direction(0.0, PI / 2.0);
        assert!((up.y - 1.0).abs() < 1e-12);
        let forward = direction(0.0, 0.0);
        let len = (forward.x * forward.x + forward.y * forward.y + forward.z * forward.z).sqrt();
        assert!((len - 1.0).abs() < 1e-9);
    }
}
