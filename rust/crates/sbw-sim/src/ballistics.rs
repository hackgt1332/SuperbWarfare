//! Deterministic ballistic rules: bounded projectile integration, ordered hits,
//! material penetration/ricochet, and armor plus hit-region damage.
//!
//! # Scope
//!
//! This is a rules module only. It does not move native arrows, allocate host
//! entities, or mutate health. The current native arrow adapter keeps running as
//! it does today; a later root integration step wraps this module around it:
//!
//! 1. Build a [`Shot`] from the authenticated shooter and its gun data.
//! 2. Reserve a [`Pool`] slot and call [`launch`] to give the projectile this
//!    shot's muzzle velocity.
//! 3. Each server tick, implement [`World`] over the host's swept query and call
//!    [`step`], then hand [`Step::hits`] to the host's damage calls.
//! 4. Feed [`Step::shot`] back in next tick, so budgets deplete across the flight.
//!
//! Nothing here changes native bullets by itself.
//!
//! # Conventions
//!
//! * Positions are blocks, velocities blocks per tick, at
//!   [`sbw_contract::TICK_RATE`] (20 Hz). One [`step`] is exactly one server tick;
//!   there is no wall clock, RNG, or float accumulation beyond the projectile's
//!   own stored state.
//! * [`Shot`] is server-owned: profile, cross-tick budgets, and the spread seed
//!   all travel with the shot. A caller cannot reset a shot's penetration or
//!   ricochet budget by asking for another tick.
//! * Every external number is sanitized to a finite, bounded value before use, so a
//!   hostile or corrupt caller cannot put NaN into simulation state.
//! * Spawns that would exceed [`limits::SWEEP_HITS`] are refused; see [`Pool`].
//!
use sbw_contract::limits;
use sbw_contract::types::{EntityKey, Vec3};

/// Longest sub-segment traced in one query, in blocks. A projectile crossing more
/// than this in one tick is traced as several queries, so a fast round cannot
/// skip a thin obstacle.
pub const MAX_SEGMENT: f64 = 4.0;
/// Speed below which a projectile counts as stopped.
pub const MIN_SPEED: f64 = 1.0e-4;
/// Travel below which a tick is not worth tracing, in blocks.
const MIN_SEGMENT: f64 = 1.0e-3;
/// Upper bounds on values accepted from a caller.
pub const MAX_SPEED: f64 = 64.0;
pub const MAX_GRAVITY: f64 = 1.0;
pub const MAX_DAMAGE: f32 = 10_000.0;
pub const MAX_BUDGET: u8 = 8;
/// Longest flight, in ticks, for each kind before a projectile expires.
pub const MAX_AGE_TICKS: u32 = 600;
/// Tolerance for degenerate directions and float comparisons.
const EPSILON: f64 = 1.0e-9;

/// A projectile's role, for the damage and penetration rule lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    /// Player or mob rifle/handgun round.
    Bullet,
    /// Larger cannon shell; overmatches light armor.
    Shell,
    /// Arrow or thrown object; stopped by any armor.
    Arrow,
}

/// Armor class a hit region exposes to penetration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ArmorClass {
    None,
    Light,
    Heavy,
}

/// Body region a hit resolved to, before armor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Region {
    Head,
    Body,
    Limbs,
}

/// Obstacle material. Values live here, not in the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Material {
    Foliage,
    Glass,
    Thin,
    Wood,
    Sand,
    Stone,
    Metal,
}

impl Material {
    /// Fraction of damage retained per block passed through. Zero stops the round.
    pub fn transmittance(self) -> f64 {
        match self {
            Self::Foliage => 0.92,
            Self::Glass => 0.85,
            Self::Thin => 0.55,
            Self::Wood => 0.45,
            Self::Sand => 0.35,
            Self::Stone => 0.25,
            Self::Metal => 0.0,
        }
    }
    /// Smallest `|cos(incidence)|` at which the surface is struck squarely
    /// enough to be penetrated or stopped. Below this the round skates off.
    ///
    /// A hard, smooth face (metal) deflects unless the hit is fairly square; a
    /// soft one only deflects at a very shallow grazing angle. `None` for
    /// surfaces that never ricochet and are always treated squarely.
    pub fn ricochet(self) -> Option<f64> {
        match self {
            Self::Metal => Some(0.30),
            Self::Stone => Some(0.25),
            Self::Sand | Self::Wood | Self::Thin => Some(0.10),
            Self::Foliage | Self::Glass => None,
        }
    }
}

/// Rule set for one projectile kind, populated from gun data.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    /// Muzzle speed in blocks per tick.
    pub speed: f64,
    /// Downward acceleration in blocks per tick squared. Zero is flat.
    pub gravity: f64,
    /// Velocity retained per tick in `[0, 1]`. One is frictionless.
    pub drag: f64,
    /// Base damage before region scaling and armor.
    pub damage: f32,
    /// Hits this projectile may pass through and keep flying for, over its life.
    pub penetrations: u8,
    /// Hits this projectile may bounce off and keep flying for, over its life.
    pub ricochets: u8,
    pub kind: Kind,
}

impl Profile {
    /// Clamp an untrusted profile into finite, bounded values.
    ///
    /// Non-finite inputs are not silently flattened to the *lower* bound. Gravity
    /// is signed (negative is an upward arc), so collapsing a hostile
    /// `NEG_INFINITY`/`NaN` to the minimum would pick an arbitrary, far-downward
    /// value; magnitude is what must be bounded, so a non-finite gravity becomes
    /// the positive [`MAX_GRAVITY`] ceiling. `speed` and `damage` keep the
    /// ordinary lower-bound rule, since zero is their safe end.
    pub fn sanitized(self) -> Self {
        Self {
            speed: bounded(self.speed, 0.0, MAX_SPEED),
            gravity: bounded_magnitude(self.gravity, MAX_GRAVITY),
            drag: bounded(self.drag, 0.0, 1.0),
            damage: bounded_f32(self.damage, 0.0, MAX_DAMAGE),
            penetrations: self.penetrations.min(MAX_BUDGET),
            ricochets: self.ricochets.min(MAX_BUDGET),
            kind: self.kind,
        }
    }
    /// Speed after `ticks` ticks of drag, floored at zero.
    pub fn speed_after(&self, ticks: u32) -> f64 {
        let drag = self.drag.clamp(0.0, 1.0);
        let mut speed = self.speed.max(0.0);
        for _ in 0..ticks {
            speed *= drag;
            if speed < MIN_SPEED {
                return 0.0;
            }
        }
        speed
    }
    /// Whether this kind passes through `material` without losing budget.
    pub fn overmatches(self, material: Material) -> bool {
        self.kind == Kind::Shell && material.transmittance() > 0.0
    }
}

/// Simulation-owned projectile state. `Copy`, so callers pass it by value across
/// ticks instead of aliasing mutable state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projectile {
    pub kind: Kind,
    /// World position in blocks; finite.
    pub position: Vec3,
    /// Velocity in blocks per tick; finite.
    pub velocity: Vec3,
    /// Ticks this projectile has existed, used to expire it.
    pub age: u32,
}

impl Projectile {
    /// A projectile at rest at `position`, about to be given a velocity.
    pub fn new(kind: Kind, position: Vec3) -> Self {
        Self {
            kind,
            position: finite_vec(position),
            velocity: zero(),
            age: 0,
        }
    }
    /// Whether this projectile has outlived [`MAX_AGE_TICKS`].
    pub fn expired(&self) -> bool {
        self.age >= MAX_AGE_TICKS
    }
}

/// Server-owned facts about one shot. Never built from a client hit report.
///
/// The `penetrations` and `ricochets` fields are the shot's remaining life-wide
/// budget, decremented by [`step`] and written back into the returned
/// [`Step::shot`]. A caller that keeps stepping a projectile carries these
/// forward, so extra ticks cannot mint new budget.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shot {
    /// The damage source: the shooter's entity.
    pub owner: EntityKey,
    /// Precise creature that fired, kept for attribution and reporting.
    pub shooter: EntityKey,
    pub profile: Profile,
    /// Tick the shot was accepted on.
    pub tick: u64,
    /// Spread in blocks per tick added to the aim direction. Sanitized to
    /// `[0, profile.speed]`, so it is a bounded perturbation of speed, not a
    /// teleport. Zero is a perfect shot.
    pub spread: f64,
    /// Deterministic pattern seed. Identical shots must use identical seeds.
    pub seed: u64,
    /// Aiming shrinks spread to a quarter.
    pub aiming: bool,
    /// Remaining penetration budget for this shot's whole flight.
    pub penetrations: u8,
    /// Remaining ricochet budget for this shot's whole flight.
    pub ricochets: u8,
}

impl Shot {
    /// Build a shot from an authenticated owner with full budgets from `profile`.
    pub fn new(owner: EntityKey, shooter: EntityKey, profile: Profile, tick: u64) -> Self {
        let profile = profile.sanitized();
        Self {
            owner,
            shooter,
            profile,
            tick,
            spread: 0.0,
            seed: 0,
            aiming: false,
            penetrations: profile.penetrations,
            ricochets: profile.ricochets,
        }
    }
    /// Initial velocity for this shot, in blocks per tick.
    pub fn muzzle_velocity(&self) -> Vec3 {
        let speed = self.profile.sanitized().speed;
        scale(self.direction(), speed)
    }
    /// Unit aim direction: forward, rotated by a deterministic spread offset.
    pub fn direction(&self) -> Vec3 {
        let offset = self.spread_offset();
        unit(
            Vec3 {
                x: offset.x,
                y: offset.y,
                z: 1.0 + offset.z,
            },
            Vec3 {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
        )
    }
    fn spread_offset(&self) -> Vec3 {
        let speed = self.profile.sanitized().speed;
        let cap = bounded(self.spread, 0.0, speed);
        let cap = if self.aiming { cap * 0.25 } else { cap };
        if cap <= 0.0 || speed <= 0.0 {
            return zero();
        }
        let unit_offset = unit(
            Vec3 {
                x: hash_unit(self.seed),
                y: hash_unit(self.seed ^ 0x9e37_79b9_7f4a_7c15),
                z: 0.0,
            },
            Vec3 {
                x: 1.0,
                y: 0.0,
                z: 0.0,
            },
        );
        scale(unit_offset, cap / speed)
    }
}

/// Taxonomy of a swept-query candidate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    World(Material),
    Entity(EntityKey),
}

/// One obstacle candidate returned by the host's swept query.
///
/// `t` is the fractional parameter along the queried segment, in `[0, 1]`;
/// `normal` points back toward the incoming side of the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub t: f64,
    pub normal: Vec3,
    pub target: Target,
}

/// What happened to the projectile at a hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Damage applied; projectile stopped.
    Impact,
    /// Damage applied (or reduced); projectile continued.
    Penetrated,
    /// No damage; projectile continued with a reflected velocity.
    Ricocheted,
}

/// One resolved hit, in strictly increasing `t` order within a [`Step`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    /// Fraction along the tick's travel, `[0, 1]`.
    pub t: f64,
    pub target: Target,
    /// World-space point of contact.
    pub position: Vec3,
    /// Surface normal, unit length, facing the incoming round.
    pub normal: Vec3,
    /// Damage actually applied, after region scaling, armor and attenuation.
    pub damage: f32,
    /// Region for entity hits; `None` for world hits.
    pub region: Option<Region>,
    pub outcome: Outcome,
}

/// Lookup of obstacles between two points, implemented by the host (or a test).
///
/// Returned candidates must be deterministic for the same arguments. The
/// simulation sorts them by `t`, ignores NaN, and never trusts the order.
pub trait World {
    /// Obstacles intersecting `start -> end`, unsorted and possibly empty.
    fn segment(&self, start: Vec3, end: Vec3) -> Vec<Segment>;
    /// Body region for an entity hit at `point`, or `None` if it has no regions.
    fn region(&self, entity: EntityKey, point: Vec3) -> Option<Region>;
    /// Armor class for an entity hit region.
    fn armor(&self, entity: EntityKey, region: Region) -> ArmorClass;
}

/// Result of advancing one projectile for exactly one tick.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// Position after the tick's travel and any reflections.
    pub position: Vec3,
    /// Velocity after drag, gravity and any reflections.
    pub velocity: Vec3,
    /// `true` when the projectile should be kept for the next tick.
    pub alive: bool,
    /// Shot carried forward with depleted budgets. `None` once the projectile is
    /// no longer alive.
    pub shot: Option<Shot>,
    /// Hits this tick, ordered by increasing `t`.
    pub hits: Vec<Hit>,
}

impl Step {
    fn dead(position: Vec3, velocity: Vec3, hits: Vec<Hit>) -> Self {
        Self {
            position: finite_vec(position),
            velocity: finite_vec(velocity),
            alive: false,
            shot: None,
            hits,
        }
    }
}

/// Adopt a projectile into a shot: place it at `position` with this shot's muzzle
/// velocity. The muzzle speed is read from the *sanitized* profile, and the aim
/// direction already includes any spread offset, so the projectile is never
/// faster than [`MAX_SPEED`] and never flies along a NaN direction.
pub fn launch(mut projectile: Projectile, shot: &Shot) -> Projectile {
    let profile = shot.profile.sanitized();
    projectile.kind = profile.kind;
    projectile.position = finite_vec(projectile.position);
    projectile.velocity = scale(shot.direction(), profile.speed);
    projectile.age = 0;
    projectile
}

/// Advance `projectile` for exactly one tick under `shot`.
///
/// The tick is integrated in sub-steps of at most [`MAX_SEGMENT`] blocks so a
/// fast round cannot tunnel a thin obstacle. At each hit the outcome is chosen in
/// a fixed order — a surface that ricochets, if the incidence is glancing and the
/// shot still has ricochet budget; otherwise penetration if the shot still has
/// penetration budget or the round overmatches the material; otherwise a stop.
///
/// `world` supplies the swept query; candidates may arrive in any order and are
/// sorted here. Returned hits are ordered by increasing `t`.
pub fn step<W: World + ?Sized>(
    projectile: Projectile,
    shot: &Shot,
    world: &W,
    hits: &mut Vec<Hit>,
) -> Step {
    hits.clear();
    if projectile.expired() || !is_finite(projectile.position) {
        return Step::dead(projectile.position, zero(), Vec::new());
    }
    let profile = shot.profile.sanitized();
    let mut position = projectile.position;
    // Gravity and drag are applied exactly once per tick, before the sweep, and
    // are *not* re-applied after a reflection within the same tick: `travel` is
    // the start-of-tick velocity, so the profile's acceleration is integrated one
    // time and every sub-step of this tick shares that single result.
    let drag = profile.drag.clamp(0.0, 1.0);
    let gravity = bounded(profile.gravity, -MAX_GRAVITY, MAX_GRAVITY);
    let travel = finite_vec(Vec3 {
        x: projectile.velocity.x * drag,
        y: projectile.velocity.y * drag - gravity,
        z: projectile.velocity.z * drag,
    });
    // A projectile whose start-of-tick speed is already at rest carries zero
    // velocity forward: multiplying a stopped round by drag must never revive it.
    if speed(projectile.velocity) <= MIN_SPEED {
        return Step::dead(position, zero(), Vec::new());
    }
    let mut velocity = travel;
    let mut penetration_budget = shot.penetrations.min(MAX_BUDGET);
    let mut ricochet_budget = shot.ricochets.min(MAX_BUDGET);
    let mut alive = true;

    if speed(velocity) <= MIN_SPEED {
        // Drag consumed the tick's last motion; a round can stop in flight.
        return Step::dead(position, zero(), Vec::new());
    }
    let remaining = length(travel);
    if remaining <= MIN_SEGMENT {
        // Slower than a millimetre per tick: nothing meaningful to trace.
        return Step {
            position: finite_vec(position),
            velocity,
            alive,
            shot: Some(carry(shot, penetration_budget, ricochet_budget)),
            hits: Vec::new(),
        };
    }
    let step_len = MAX_SEGMENT.min(remaining);
    let mut done = 0.0f64;
    while done < remaining {
        let len = step_len.min(remaining - done);
        let start = position;
        let end = add(start, scale(travel, len / remaining));
        let mut candidates = world.segment(start, end);
        // Host candidates are untrusted: drop non-finite and out-of-range ones,
        // then sort by `t` so hit order is a property of the world, not the host.
        candidates.retain(|c| c.t.is_finite() && c.t >= -EPSILON && c.t <= 1.0 + EPSILON);
        candidates.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
        candidates.truncate(limits::SWEEP_HITS);
        for candidate in candidates {
            let t = candidate.t.clamp(0.0, 1.0);
            let point = add(start, scale(sub(end, start), t));
            let normal = unit(candidate.normal, scale(velocity, -1.0));
            // A normal facing the incoming round points backwards along travel;
            // facing it forward means the round left the surface, so ignore it.
            if dot(normal, velocity) > 0.0 {
                continue;
            }
            let mut hit = Hit {
                t,
                target: candidate.target,
                position: point,
                normal,
                damage: 0.0,
                region: None,
                outcome: Outcome::Impact,
            };
            match candidate.target {
                Target::Entity(entity) => {
                    // Never damage the shooter's own entity; pass through it.
                    if entity == shot.owner {
                        continue;
                    }
                    let region = world.region(entity, point).unwrap_or(Region::Body);
                    let armor = world.armor(entity, region);
                    // Armor plate deflects at the metal threshold; bare flesh
                    // (no armor) never ricochets.
                    let glancing =
                        armor != ArmorClass::None && is_glancing(Material::Metal, normal, velocity);
                    hit.region = Some(region);
                    if glancing && ricochet_budget > 0 {
                        ricochet_budget -= 1;
                        hit.outcome = Outcome::Ricocheted;
                        hit.damage = 0.0;
                        velocity = reflect(velocity, normal);
                    } else {
                        let damage = damage_for(&profile, region, armor, 1.0);
                        hit.damage = damage;
                        hit.outcome = Outcome::Impact;
                        alive = false;
                    }
                }
                Target::World(material) => {
                    let transmission = material.transmittance();
                    let glancing = is_glancing(material, normal, velocity);
                    if glancing && ricochet_budget > 0 {
                        ricochet_budget -= 1;
                        hit.outcome = Outcome::Ricocheted;
                        velocity = reflect(velocity, normal);
                    } else if transmission <= 0.0 {
                        hit.outcome = Outcome::Impact;
                        alive = false;
                    } else if penetration_budget > 0 || profile.overmatches(material) {
                        if !profile.overmatches(material) {
                            penetration_budget -= 1;
                        }
                        // Damage is only attributed to entities; world hits that
                        // penetrate report their budget spend through `outcome`.
                        hit.damage = 0.0;
                        hit.outcome = Outcome::Penetrated;
                        velocity = scale(velocity, transmission);
                    } else {
                        hit.outcome = Outcome::Impact;
                        alive = false;
                    }
                }
            }
            hits.push(hit);
            if !alive {
                // Stopped at this point; remaining travel this tick is dropped.
                position = point;
                return Step {
                    position: finite_vec(position),
                    velocity,
                    alive: false,
                    shot: None,
                    hits: std::mem::take(hits),
                };
            }
        }
        position = end;
        done += len;
        if speed(velocity) <= MIN_SPEED {
            alive = false;
            break;
        }
    }
    if alive {
        Step {
            position: finite_vec(position),
            velocity,
            alive,
            shot: Some(carry(shot, penetration_budget, ricochet_budget)),
            hits: std::mem::take(hits),
        }
    } else {
        Step::dead(position, velocity, std::mem::take(hits))
    }
}

/// Shot carried forward with the budgets the tick actually spent.
fn carry(shot: &Shot, penetrations: u8, ricochets: u8) -> Shot {
    let mut next = *shot;
    next.penetrations = penetrations;
    next.ricochets = ricochets;
    next
}

/// `|cos(incidence)|` between `normal` and `velocity`, or `1.0` when the round
/// is too slow to define a direction.
fn incidence(normal: Vec3, velocity: Vec3) -> f64 {
    let speed = speed(velocity);
    if speed <= MIN_SPEED {
        return 1.0;
    }
    (dot(normal, velocity) / speed).abs().clamp(0.0, 1.0)
}

/// `true` when `material` deflects the round at this incidence.
///
/// The round skates off when it strikes more shallowly than the surface's
/// square-hit threshold. A surface that never ricochets ([`Material::ricochet`]
/// is `None`) always answers `false`, so the caller falls through to penetration
/// or a stop.
fn is_glancing(material: Material, normal: Vec3, velocity: Vec3) -> bool {
    match material.ricochet() {
        Some(square) => incidence(normal, velocity) < square,
        None => false,
    }
}

/// Damage for a hit, after region scaling and armor class.
///
/// * [`Region::Head`] doubles damage; [`Region::Limbs`] scales it down.
/// * [`ArmorClass::Heavy`] blocks a bullet outright, halves a shell.
/// * [`ArmorClass::Light`] reduces a bullet, blocks an arrow.
///
/// `transmission` is the material attenuation carried in from penetration;
/// `1.0` for a direct hit.
pub fn damage_for(profile: &Profile, region: Region, armor: ArmorClass, transmission: f64) -> f32 {
    let profile = profile.sanitized();
    let region_scale = match region {
        Region::Head => 2.0,
        Region::Body => 1.0,
        Region::Limbs => 0.6,
    };
    let armor_scale = match (profile.kind, armor) {
        (_, ArmorClass::None) => 1.0,
        (Kind::Bullet, ArmorClass::Light) => 0.6,
        (Kind::Bullet, ArmorClass::Heavy) => 0.0,
        (Kind::Shell, ArmorClass::Light) => 1.0,
        (Kind::Shell, ArmorClass::Heavy) => 0.5,
        (Kind::Arrow, ArmorClass::Light) => 0.0,
        (Kind::Arrow, ArmorClass::Heavy) => 0.0,
    };
    let scale = region_scale * armor_scale * bounded(transmission, 0.0, 1.0);
    let damage = f64::from(profile.damage) * scale;
    if damage.is_finite() {
        damage.clamp(0.0, f64::from(MAX_DAMAGE)) as f32
    } else {
        0.0
    }
}

// --- Small vector helpers -------------------------------------------------
//
// Deliberately local and finite-only: no NaN, no infinity, and no dependence on
// the host's vector type beyond the plain `Vec3` fields.

fn zero() -> Vec3 {
    Vec3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    }
}

fn bounded(value: f64, low: f64, high: f64) -> f64 {
    if value.is_finite() {
        value.clamp(low, high)
    } else {
        low
    }
}

/// Bound a signed value's *magnitude*, collapsing a non-finite input to the
/// ceiling `limit` rather than the lower bound. Used for gravity, where zero is
/// not the safe end of the range.
fn bounded_magnitude(value: f64, limit: f64) -> f64 {
    if value.is_finite() {
        value.clamp(-limit, limit)
    } else {
        limit
    }
}

fn bounded_f32(value: f32, low: f32, high: f32) -> f32 {
    if value.is_finite() {
        value.clamp(low, high)
    } else {
        low
    }
}

fn finite_vec(v: Vec3) -> Vec3 {
    Vec3 {
        x: bounded(v.x, -MAX_COORD, MAX_COORD),
        y: bounded(v.y, -MAX_COORD, MAX_COORD),
        z: bounded(v.z, -MAX_COORD, MAX_COORD),
    }
}

/// Largest coordinate magnitude accepted; beyond this a position is not usable.
const MAX_COORD: f64 = 3.0e7;

fn add(a: Vec3, b: Vec3) -> Vec3 {
    finite_vec(Vec3 {
        x: a.x + b.x,
        y: a.y + b.y,
        z: a.z + b.z,
    })
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    finite_vec(Vec3 {
        x: a.x - b.x,
        y: a.y - b.y,
        z: a.z - b.z,
    })
}

fn scale(v: Vec3, s: f64) -> Vec3 {
    let s = bounded(s, -MAX_COORD, MAX_COORD);
    finite_vec(Vec3 {
        x: v.x * s,
        y: v.y * s,
        z: v.z * s,
    })
}

fn dot(a: Vec3, b: Vec3) -> f64 {
    let value = a.x * b.x + a.y * b.y + a.z * b.z;
    if value.is_finite() { value } else { 0.0 }
}

fn length(v: Vec3) -> f64 {
    let squared = dot(v, v);
    if squared.is_finite() {
        squared.max(0.0).sqrt()
    } else {
        0.0
    }
}

fn speed(v: Vec3) -> f64 {
    length(v)
}

/// Unit vector, falling back to `fallback` when `v` is too short to normalize.
fn unit(v: Vec3, fallback: Vec3) -> Vec3 {
    let len = length(v);
    if len <= EPSILON {
        let fb = length(fallback);
        if fb <= EPSILON {
            zero()
        } else {
            scale(fallback, 1.0 / fb)
        }
    } else {
        scale(v, 1.0 / len)
    }
}

/// Mirror `velocity` about the plane whose unit normal is `normal`.
fn reflect(velocity: Vec3, normal: Vec3) -> Vec3 {
    let n = unit(normal, velocity);
    let factor = 2.0 * dot(velocity, n);
    finite_vec(Vec3 {
        x: velocity.x - n.x * factor,
        y: velocity.y - n.y * factor,
        z: velocity.z - n.z * factor,
    })
}

/// A deterministic unit value in `[-1, 1]` from `seed`. Same seed, same value.
fn hash_unit(seed: u64) -> f64 {
    // SplitMix64-style avalanche, then map to [-1, 1].
    let mut x = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    let unit = (x >> 11) as f64 / (1u64 << 53) as f64;
    unit * 2.0 - 1.0
}

/// Whether every component of `v` is finite.
fn is_finite(v: Vec3) -> bool {
    v.x.is_finite() && v.y.is_finite() && v.z.is_finite()
}

/// Bounded per-shot pool, so a misbehaving caller cannot spawn without limit.
///
/// The pool only counts live shots; it does not integrate them. Integration
/// stays with [`step`], which is a pure function of its inputs.
///
/// # Ownership of tickets
///
/// A [`Slot`] is only meaningful to the pool that issued it. Rather than trust a
/// caller-supplied "held" flag — which, because [`Slot`] is `Copy`, would let the
/// *same* ticket release more than once and drive the live count below zero — the
/// pool keeps the authoritative set of outstanding tickets itself:
///
/// * each spawn mints a fresh [`SlotId`], so tickets are never interchangeable
///   even when copied;
/// * releasing a ticket the pool no longer holds (a double release, a stale copy,
///   or a forged one) is a no-op;
/// * `live` is always exactly the number of outstanding tickets, so it can never
///   underflow even under hostile use.
///
/// Ids are unique *within* one pool, not across pools, so a ticket is not
/// interchangeable between pools; [`Pool::release`] simply does nothing when the
/// id is not currently held here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pool {
    /// Identity of the next ticket to mint. Monotonic, so a released id is never
    /// reissued and a stale copy can never stand in for a live slot.
    next: u64,
    /// Outstanding ticket ids, in mint order. Bounded by [`limits::SWEEP_HITS`].
    held: Vec<SlotId>,
}

impl Pool {
    /// An empty pool.
    pub const fn new() -> Self {
        Self {
            next: 0,
            held: Vec::new(),
        }
    }
    /// Reserve a slot, or refuse when [`limits::SWEEP_HITS`] shots are already
    /// live. Refusal is a normal result, not an error.
    pub fn spawn(&mut self) -> Option<Slot> {
        if self.held.len() >= limits::SWEEP_HITS {
            return None;
        }
        let id = SlotId { id: self.next };
        self.next = self.next.wrapping_add(1);
        self.held.push(id);
        Some(Slot { id })
    }
    /// Live shots currently tracked.
    pub fn live(&self) -> usize {
        self.held.len()
    }
    /// Release a slot. Releasing the same ticket twice, or one this pool never
    /// issued, is a no-op; the live count never underflows.
    pub fn release(&mut self, slot: Slot) {
        if let Some(index) = self.held.iter().position(|held| *held == slot.id) {
            self.held.swap_remove(index);
        }
    }
}

/// Identity of a reserved slot, unique within one [`Pool`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotId {
    id: u64,
}

/// Proof of a reserved pool slot. Dropping it does not release the slot; call
/// [`Pool::release`] so the pool's live count stays honest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    id: SlotId,
}

impl Slot {
    /// Whether this slot still occupies its pool.
    ///
    /// Because a [`Pool`] tracks the tickets it issued, this reports whether the
    /// ticket is still outstanding *in its own pool*.
    pub fn is_held(&self, pool: &Pool) -> bool {
        pool.held.contains(&self.id)
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

    fn at(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3 { x, y, z }
    }

    /// Flat bullet: no gravity, no drag, one block of travel per tick.
    fn bullet(damage: f32) -> Profile {
        Profile {
            speed: 1.0,
            gravity: 0.0,
            drag: 1.0,
            damage,
            penetrations: 1,
            ricochets: 1,
            kind: Kind::Bullet,
        }
    }

    /// A world built from explicit obstacles and fixed entity regions.
    struct TestWorld {
        segments: Vec<Segment>,
        entities: Vec<(EntityKey, Region, ArmorClass)>,
        calls: std::cell::Cell<usize>,
    }

    impl TestWorld {
        fn empty() -> Self {
            Self {
                segments: Vec::new(),
                entities: Vec::new(),
                calls: std::cell::Cell::new(0),
            }
        }
        fn with(mut self, t: f64, target: Target, normal: Vec3) -> Self {
            self.segments.push(Segment { t, normal, target });
            self
        }
        fn entity(mut self, entity: EntityKey, region: Region, armor: ArmorClass) -> Self {
            self.entities.push((entity, region, armor));
            self
        }
    }

    impl World for TestWorld {
        fn segment(&self, _start: Vec3, _end: Vec3) -> Vec<Segment> {
            self.calls.set(self.calls.get() + 1);
            self.segments.clone()
        }
        fn region(&self, entity: EntityKey, _point: Vec3) -> Option<Region> {
            self.entities
                .iter()
                .find(|(e, _, _)| *e == entity)
                .map(|(_, r, _)| *r)
        }
        fn armor(&self, entity: EntityKey, region: Region) -> ArmorClass {
            self.entities
                .iter()
                .find(|(e, r, _)| *e == entity && *r == region)
                .map(|(_, _, a)| *a)
                .unwrap_or(ArmorClass::None)
        }
    }

    fn shot(owner: EntityKey, profile: Profile) -> Shot {
        Shot::new(owner, owner, profile, 0)
    }

    #[test]
    fn flat_bullet_advances_exactly_one_block_per_tick() {
        let world = TestWorld::empty();
        let s = shot(key(1), bullet(9.0));
        let mut projectile = launch(Projectile::new(Kind::Bullet, at(0.0, 0.0, 0.0)), &s);
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &world, &mut hits);
        assert!(step.alive);
        assert_eq!(step.position, at(0.0, 0.0, 1.0));
        assert!(step.hits.is_empty());
        projectile = Projectile {
            position: step.position,
            velocity: step.velocity,
            ..projectile
        };
        let s = step.shot.expect("alive shots carry forward");
        let step = super::step(projectile, &s, &world, &mut hits);
        assert_eq!(step.position, at(0.0, 0.0, 2.0));
    }

    #[test]
    fn gravity_and_drag_are_applied_once_per_tick() {
        let world = TestWorld::empty();
        let profile = Profile {
            speed: 1.0,
            gravity: 0.1,
            drag: 0.5,
            ..bullet(9.0)
        };
        let s = shot(key(1), profile);
        let mut projectile = launch(Projectile::new(Kind::Bullet, zero()), &s);
        // Muzzle velocity is +1 block/tick; the first tick halves it and drops it.
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &world, &mut hits);
        assert!((step.velocity.x - 0.0).abs() < 1e-12);
        assert!((step.velocity.y + 0.1).abs() < 1e-12);
        assert!((step.velocity.z - 0.5).abs() < 1e-12);
        projectile = Projectile {
            position: step.position,
            velocity: step.velocity,
            ..projectile
        };
        let s = step.shot.unwrap();
        let step = super::step(projectile, &s, &world, &mut hits);
        assert!((step.velocity.z - 0.25).abs() < 1e-12);
        // Gravity is an *acceleration* (see `Profile::gravity` and `step`): each
        // tick adds `-gravity` to the vertical velocity that drag retained from
        // the previous tick, so the fall accelerates rather than settling at a
        // constant rate. Feeding y = -0.1 back through `-0.1 * 0.5 - 0.1` gives
        // -0.15; the vertical component then converges on gravity/(1 - drag) = -0.2.
        assert!((step.velocity.y + 0.15).abs() < 1e-12);
    }

    #[test]
    fn head_and_limb_hits_scale_damage_around_the_body() {
        let profile = bullet(10.0);
        assert_eq!(
            damage_for(&profile, Region::Body, ArmorClass::None, 1.0),
            10.0
        );
        assert_eq!(
            damage_for(&profile, Region::Head, ArmorClass::None, 1.0),
            20.0
        );
        assert_eq!(
            damage_for(&profile, Region::Limbs, ArmorClass::None, 1.0),
            6.0
        );
    }

    #[test]
    fn armor_class_changes_damage_by_projectile_kind() {
        let b = bullet(10.0);
        assert_eq!(damage_for(&b, Region::Body, ArmorClass::Light, 1.0), 6.0);
        assert_eq!(damage_for(&b, Region::Body, ArmorClass::Heavy, 1.0), 0.0);
        let shell = Profile {
            kind: Kind::Shell,
            ..bullet(10.0)
        };
        assert_eq!(
            damage_for(&shell, Region::Body, ArmorClass::Heavy, 1.0),
            5.0
        );
        let arrow = Profile {
            kind: Kind::Arrow,
            ..bullet(10.0)
        };
        assert_eq!(
            damage_for(&arrow, Region::Body, ArmorClass::Light, 1.0),
            0.0
        );
    }

    #[test]
    fn a_stopping_material_ends_the_round_at_the_contact_point() {
        // Metal transmits nothing, so the round stops where it meets the wall.
        let world =
            TestWorld::empty().with(0.5, Target::World(Material::Metal), at(0.0, 0.0, -1.0));
        let s = shot(key(1), bullet(9.0));
        let projectile = launch(Projectile::new(Kind::Bullet, zero()), &s);
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &world, &mut hits);
        assert!(!step.alive);
        assert!(step.shot.is_none());
        assert_eq!(step.hits.len(), 1);
        assert_eq!(step.hits[0].outcome, Outcome::Impact);
        assert_eq!(step.position, at(0.0, 0.0, 0.5));
    }

    #[test]
    fn penetration_spends_budget_once_and_attenuates_velocity() {
        // Wood transmits 0.45 and the profile has exactly one penetration.
        let world = TestWorld::empty().with(0.5, Target::World(Material::Wood), at(0.0, 0.0, -1.0));
        let s = shot(key(1), bullet(9.0));
        let projectile = launch(Projectile::new(Kind::Bullet, zero()), &s);
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &world, &mut hits);
        assert!(step.alive);
        assert_eq!(step.hits[0].outcome, Outcome::Penetrated);
        assert!((step.velocity.z - 0.45).abs() < 1e-12);
        let carried = step.shot.unwrap();
        assert_eq!(carried.penetrations, 0);
        // With no budget left, a second wooden wall stops the round.
        let projectile = Projectile {
            position: step.position,
            velocity: step.velocity,
            ..projectile
        };
        let step = super::step(projectile, &carried, &world, &mut hits);
        assert!(!step.alive);
        assert_eq!(step.hits[0].outcome, Outcome::Impact);
    }

    #[test]
    fn a_shell_overmatches_wood_without_spending_budget() {
        let world = TestWorld::empty().with(0.5, Target::World(Material::Wood), at(0.0, 0.0, -1.0));
        let profile = Profile {
            kind: Kind::Shell,
            ..bullet(10.0)
        };
        let s = shot(key(1), profile);
        let projectile = launch(Projectile::new(Kind::Shell, zero()), &s);
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &world, &mut hits);
        assert!(step.alive);
        assert_eq!(step.hits[0].outcome, Outcome::Penetrated);
        assert_eq!(step.shot.unwrap().penetrations, s.penetrations);
    }

    #[test]
    fn armor_plate_can_ricochet_a_round_that_bare_flesh_would_take() {
        let target = key(2);
        // Travel is +z; a normal mostly perpendicular to it is a shallow graze.
        let normal = unit(at(1.0, 0.0, -0.1), at(0.0, 0.0, -1.0));
        let armored = TestWorld::empty()
            .with(0.5, Target::Entity(target), normal)
            .entity(target, Region::Body, ArmorClass::Heavy);
        let s = shot(key(1), bullet(9.0));
        let projectile = launch(Projectile::new(Kind::Bullet, zero()), &s);
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &armored, &mut hits);
        assert_eq!(step.hits[0].outcome, Outcome::Ricocheted);
        assert_eq!(step.hits[0].damage, 0.0);
        assert_eq!(step.shot.unwrap().ricochets, 0);

        // The same geometry on an unarmored region takes damage instead.
        let bare = TestWorld::empty()
            .with(0.5, Target::Entity(target), normal)
            .entity(target, Region::Body, ArmorClass::None);
        let step = super::step(projectile, &s, &bare, &mut hits);
        assert_eq!(step.hits[0].outcome, Outcome::Impact);
        assert_eq!(step.hits[0].damage, 9.0);
    }

    #[test]
    fn a_shallow_hit_on_metal_ricochets_and_spends_its_budget() {
        // Travel is +z; a normal mostly perpendicular to it is a shallow graze,
        // cos ~ 0.10, well below metal's square-hit threshold of 0.30.
        let normal = unit(at(1.0, 0.0, -0.1), at(0.0, 0.0, -1.0));
        let world = TestWorld::empty().with(0.5, Target::World(Material::Metal), normal);
        let s = shot(key(1), bullet(9.0));
        let mut projectile = launch(Projectile::new(Kind::Bullet, zero()), &s);
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &world, &mut hits);
        assert_eq!(step.hits[0].outcome, Outcome::Ricocheted);
        assert_eq!(step.hits[0].damage, 0.0);
        let carried = step.shot.unwrap();
        assert_eq!(carried.ricochets, 0);
        // Budget spent: the next metal surface stops the round even head-on.
        projectile = Projectile {
            position: step.position,
            velocity: step.velocity,
            ..projectile
        };
        let square =
            TestWorld::empty().with(0.5, Target::World(Material::Metal), at(0.0, 0.0, -1.0));
        let step = super::step(projectile, &carried, &square, &mut hits);
        assert!(!step.alive);
        assert_eq!(step.hits[0].outcome, Outcome::Impact);
    }

    #[test]
    fn a_square_hit_on_metal_stops_without_spending_ricochet_budget() {
        let world =
            TestWorld::empty().with(0.5, Target::World(Material::Metal), at(0.0, 0.0, -1.0));
        let s = shot(key(1), bullet(9.0));
        let projectile = launch(Projectile::new(Kind::Bullet, zero()), &s);
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &world, &mut hits);
        assert!(!step.alive);
        assert_eq!(step.hits[0].outcome, Outcome::Impact);
        // A dead round carries no shot forward at all.
        assert!(step.shot.is_none());
    }

    #[test]
    fn foliage_never_ricochets_and_is_transparent_to_a_bullet() {
        let world =
            TestWorld::empty().with(0.5, Target::World(Material::Foliage), at(0.0, 0.0, -1.0));
        let s = shot(key(1), bullet(9.0));
        let projectile = launch(Projectile::new(Kind::Bullet, zero()), &s);
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &world, &mut hits);
        // Foliage has no ricochet threshold, so it transmits rather than deflects.
        assert_eq!(step.hits[0].outcome, Outcome::Penetrated);
        assert!(step.alive);
    }

    #[test]
    fn an_entity_hit_reports_region_and_scaled_damage() {
        let target = key(2);
        let world = TestWorld::empty()
            .with(0.5, Target::Entity(target), at(0.0, 0.0, -1.0))
            .entity(target, Region::Head, ArmorClass::None);
        let s = shot(key(1), bullet(9.0));
        let projectile = launch(Projectile::new(Kind::Bullet, zero()), &s);
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &world, &mut hits);
        assert!(!step.alive);
        assert_eq!(step.hits[0].region, Some(Region::Head));
        assert_eq!(step.hits[0].damage, 18.0);
    }

    #[test]
    fn the_shooter_cannot_hit_its_own_entity() {
        let world = TestWorld::empty()
            .with(0.5, Target::Entity(key(1)), at(0.0, 0.0, -1.0))
            .entity(key(1), Region::Body, ArmorClass::None);
        let s = shot(key(1), bullet(9.0));
        let projectile = launch(Projectile::new(Kind::Bullet, zero()), &s);
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &world, &mut hits);
        assert!(step.alive);
        assert!(step.hits.is_empty());
    }

    #[test]
    fn hits_are_ordered_by_increasing_t_regardless_of_host_order() {
        // Deliberately out of order, as a hostile host could return them. Both
        // are wood, so with two penetrations the round passes through both.
        let world = TestWorld::empty()
            .with(0.9, Target::World(Material::Wood), at(0.0, 0.0, -1.0))
            .with(0.1, Target::World(Material::Glass), at(0.0, 0.0, -1.0));
        let profile = Profile {
            penetrations: 2,
            ..bullet(9.0)
        };
        let s = shot(key(1), profile);
        let projectile = launch(Projectile::new(Kind::Bullet, zero()), &s);
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &world, &mut hits);
        assert_eq!(step.hits.len(), 2);
        assert!(step.hits[0].t < step.hits[1].t);
        assert_eq!(step.hits[0].target, Target::World(Material::Glass));
        assert_eq!(step.hits[1].target, Target::World(Material::Wood));
        // Both penetrated, so the round is still flying with no budget left.
        assert!(step.alive);
        assert_eq!(step.shot.unwrap().penetrations, 0);
    }

    #[test]
    fn a_fast_round_is_traced_in_bounded_sub_segments() {
        let world = TestWorld::empty();
        let profile = Profile {
            speed: 64.0,
            ..bullet(9.0)
        };
        let s = shot(key(1), profile);
        let projectile = launch(Projectile::new(Kind::Bullet, zero()), &s);
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &world, &mut hits);
        assert_eq!(step.position, at(0.0, 0.0, 64.0));
        // 64 blocks / 4 blocks per query == 16 queries, never one giant sweep.
        assert_eq!(world.calls.get(), 16);
    }

    #[test]
    fn spread_is_deterministic_and_bounded_by_speed() {
        let mut a = shot(key(1), bullet(9.0));
        a.spread = 1.0;
        a.seed = 7;
        let mut b = a;
        b.seed = 7;
        assert_eq!(a.muzzle_velocity(), b.muzzle_velocity());
        assert!(length(a.muzzle_velocity()) <= 1.0 + 1e-9);
        // Aiming shrinks the offset; a perfect shot is unaffected.
        let mut c = a;
        c.aiming = true;
        assert!(length(c.spread_offset()) < length(a.spread_offset()));
        let mut perfect = a;
        perfect.spread = 0.0;
        assert_eq!(perfect.direction(), at(0.0, 0.0, 1.0));
    }

    #[test]
    fn a_hostile_profile_is_clamped_to_finite_bounds() {
        let hostile = Profile {
            speed: f64::NAN,
            gravity: f64::INFINITY,
            drag: 42.0,
            damage: f32::NAN,
            penetrations: 255,
            ricochets: 255,
            kind: Kind::Bullet,
        }
        .sanitized();
        assert_eq!(hostile.speed, 0.0);
        assert_eq!(hostile.gravity, MAX_GRAVITY);
        assert_eq!(hostile.drag, 1.0);
        assert_eq!(hostile.damage, 0.0);
        assert_eq!(hostile.penetrations, MAX_BUDGET);
        assert_eq!(hostile.ricochets, MAX_BUDGET);
        assert!(hostile.speed.is_finite() && hostile.damage.is_finite());
    }

    #[test]
    fn extra_ticks_cannot_mint_new_budget() {
        let world = TestWorld::empty().with(0.5, Target::World(Material::Wood), at(0.0, 0.0, -1.0));
        let s = shot(key(1), bullet(9.0));
        let mut carried = s;
        let mut projectile = launch(Projectile::new(Kind::Bullet, zero()), &s);
        let mut hits = Vec::new();
        // Penetrate the first wall, then keep stepping: budget must stay at zero.
        for _ in 0..3 {
            let step = super::step(projectile, &carried, &world, &mut hits);
            if let Some(next) = step.shot {
                carried = next;
                projectile = Projectile {
                    position: step.position,
                    velocity: step.velocity,
                    age: projectile.age + 1,
                    ..projectile
                };
            } else {
                break;
            }
            assert!(carried.penetrations <= s.penetrations);
        }
        assert_eq!(carried.penetrations, 0);
    }

    #[test]
    fn expired_projectiles_do_not_step() {
        let world = TestWorld::empty();
        let s = shot(key(1), bullet(9.0));
        let mut projectile = launch(Projectile::new(Kind::Bullet, zero()), &s);
        projectile.age = MAX_AGE_TICKS;
        let mut hits = Vec::new();
        let step = super::step(projectile, &s, &world, &mut hits);
        assert!(!step.alive);
        assert!(step.hits.is_empty());
    }

    #[test]
    fn the_pool_refuses_spawns_past_its_limit_and_never_underflows() {
        let mut pool = Pool::new();
        let mut slots = Vec::new();
        for _ in 0..limits::SWEEP_HITS {
            slots.push(pool.spawn().expect("within limit"));
        }
        assert_eq!(pool.live(), limits::SWEEP_HITS);
        assert!(pool.spawn().is_none());
        let first = slots[0];
        pool.release(first);
        assert_eq!(pool.live(), limits::SWEEP_HITS - 1);
        // Releasing the same slot twice is a no-op, not an underflow.
        pool.release(first);
        assert_eq!(pool.live(), limits::SWEEP_HITS - 1);
        assert!(pool.spawn().is_some());
    }

    #[test]
    fn a_stale_or_foreign_ticket_cannot_release_a_live_slot() {
        let mut pool = Pool::new();
        let a = pool.spawn().expect("within limit");
        let b = pool.spawn().expect("within limit");
        // The ticket keeps its identity across `Copy`, which is why the pool,
        // not the ticket, is the authority on whether a slot is live.
        pool.release(a);
        pool.release(a);
        assert_eq!(pool.live(), 1);
        // A ticket from a different pool, even one with matching history, is a
        // stranger here and must not touch this pool's count.
        let mut other = Pool::new();
        let foreign = other.spawn().expect("within limit");
        pool.release(foreign);
        other.release(b);
        assert_eq!(pool.live(), 1);
        assert_eq!(other.live(), 1);
        assert!(b.is_held(&pool));
        assert!(!a.is_held(&pool));
    }

    #[test]
    fn a_shot_serializes_without_loss() {
        let s = shot(key(1), bullet(9.0));
        let bytes = postcard::to_allocvec(&s.profile).expect("bounded profile");
        let back: Profile = postcard::from_bytes(&bytes).expect("round trip");
        assert_eq!(back, s.profile.sanitized());
    }
}
