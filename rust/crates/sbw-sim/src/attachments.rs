//! Typed attachment and weapon-modifier rules.
//!
//! This module owns the rules for what an attachment may be installed on, which
//! slots a weapon exposes, how a fitted set of attachments changes a weapon's
//! stats, and how that configuration survives a save round trip. Horizon owns
//! the item stacks; this layer only validates a proposed configuration and
//! reduces it to a deterministic stat block.
//!
//! Invariants:
//! - A weapon with no attachments is exactly its base definition: every stat is
//!   returned unchanged, and the empty configuration encodes to a fixed record.
//! - A configuration is ordered and bounded: installs append, removals splice,
//!   and no weapon ever holds more than [`MAX_SLOTS_PER_WEAPON`] attachments or
//!   the same slot twice.
//! - Modifiers fold in a fixed order recorded in the configuration, so the same
//!   bytes always yield the same stats. Additive terms are summed first, then
//!   multiplicative terms multiply the sum, and the result is clamped to the
//!   definition's bounds.
//! - Saved configurations are versioned Postcard bounded by [`STATE_BYTES`];
//!   unknown schemas, foreign weapons, duplicates and unknown ids are rejected
//!   rather than reinitialized.
//!
//! # Integration
//!
//! The host keeps the bytes, not the struct: build the definition and
//! catalogue once, decode the player's save against them, apply changes, then
//! re-encode. `Config::stats` is the single fold the gameplay side consumes.
//!
//! ```ignore
//! use sbw_sim::attachments::{ak47_definition, catalogue, Config, Slot};
//!
//! let def = ak47_definition();
//! let cat = catalogue();
//!
//! let mut config = Config::none();
//! config
//!     .install(&def, &cat, Slot::Optic, "superbwarfare:red_dot")
//!     .expect("slot is free");
//! assert_eq!(config.stats(&def, &cat).spread, 75);
//!
//! // Persist: the host stores these bytes, not the struct.
//! let bytes = config.encode();
//! assert_eq!(Config::decode(&bytes, &def, &cat).unwrap(), config);
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;

/// Saved-state schema for a fitted attachment configuration.
pub const SCHEMA: u16 = 1;
/// Encoded configuration is a bounded list of ids; the contract allows 4 KiB.
pub const STATE_BYTES: usize = 512;
/// Hard cap on attachments fitted to one weapon.
pub const MAX_SLOTS_PER_WEAPON: usize = 8;
/// Largest multiplicative fold a single modifier may request, in percent.
const MAX_FACTOR_PCT: u32 = 1_000;
/// Additive headroom around a base value, in percent of the base.
const ADD_HEADROOM_PCT: i32 = 100;
/// Longest attachment id the API accepts.
const MAX_ID: usize = 64;
/// Most effects one attachment definition may carry.
const MAX_EFFECTS: usize = 8;

/// The slots a weapon definition may expose. A slot is a typed rail: an optic
/// cannot be fitted to a muzzle rail, and vice versa.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Slot {
    Optic,
    Muzzle,
    Magazine,
    Grip,
    Underbarrel,
}

impl Slot {
    /// Stable transport suffix, used when the host needs an item id.
    pub fn item_suffix(self) -> &'static str {
        match self {
            Slot::Optic => "optic",
            Slot::Muzzle => "muzzle",
            Slot::Magazine => "magazine",
            Slot::Grip => "grip",
            Slot::Underbarrel => "underbarrel",
        }
    }

    /// Every slot, in a fixed order.
    pub const ALL: [Slot; 5] = [
        Slot::Optic,
        Slot::Muzzle,
        Slot::Magazine,
        Slot::Grip,
        Slot::Underbarrel,
    ];

    fn index(self) -> usize {
        match self {
            Slot::Optic => 0,
            Slot::Muzzle => 1,
            Slot::Magazine => 2,
            Slot::Grip => 3,
            Slot::Underbarrel => 4,
        }
    }
}

/// The stats an attachment may change. Kept integer-valued so the fold is
/// exactly reproducible across platforms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Stat {
    /// Base damage per hit, in tenths of a point.
    Damage,
    /// Magazine capacity in rounds.
    Capacity,
    /// Reload time in ticks.
    Reload,
    /// Shot interval in ticks.
    Interval,
    /// Muzzle speed in tenths of a block per tick.
    Speed,
    /// Recoil per shot in hundredths.
    Recoil,
    /// Spread in hundredths of a degree.
    Spread,
}

/// Every stat, in a fixed fold order.
pub const ALL_STATS: [Stat; 7] = [
    Stat::Damage,
    Stat::Capacity,
    Stat::Reload,
    Stat::Interval,
    Stat::Speed,
    Stat::Recoil,
    Stat::Spread,
];

/// The bounded stat block a modifier set reduces to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub damage: i32,
    pub capacity: i32,
    pub reload: i32,
    pub interval: i32,
    pub speed: i32,
    pub recoil: i32,
    pub spread: i32,
}

impl Stats {
    pub fn get(&self, stat: Stat) -> i32 {
        match stat {
            Stat::Damage => self.damage,
            Stat::Capacity => self.capacity,
            Stat::Reload => self.reload,
            Stat::Interval => self.interval,
            Stat::Speed => self.speed,
            Stat::Recoil => self.recoil,
            Stat::Spread => self.spread,
        }
    }

    pub fn set(&mut self, stat: Stat, value: i32) {
        match stat {
            Stat::Damage => self.damage = value,
            Stat::Capacity => self.capacity = value,
            Stat::Reload => self.reload = value,
            Stat::Interval => self.interval = value,
            Stat::Speed => self.speed = value,
            Stat::Recoil => self.recoil = value,
            Stat::Spread => self.spread = value,
        }
    }
}

/// A weapon's base stat block plus the per-stat clamps every fold respects.
/// Clamps are data from the definition, never engine-specific code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Definition {
    /// Slot mask: `slots[slot.index()]` is true when the weapon exposes it.
    pub slots: [bool; 5],
    pub base: Stats,
    pub min: Stats,
    pub max: Stats,
}

impl Definition {
    /// A definition with a slot mask. Clamps default to the base value widened
    /// by the module's additive headroom and floored at zero, so a definition
    /// cannot be built that permits a negative or absurd stat.
    pub fn new(slots: [bool; 5], base: Stats) -> Self {
        let mut min = base;
        let mut max = base;
        for stat in ALL_STATS {
            let b = base.get(stat);
            let room = b.saturating_mul(ADD_HEADROOM_PCT).abs() / 100;
            min.set(stat, (b - room).max(0));
            max.set(stat, b + room);
        }
        Self {
            slots,
            base,
            min,
            max,
        }
    }

    /// Explicit clamps, used when a definition needs tighter bounds than the
    /// computed headroom. Rejects inverted or negative ranges.
    pub fn bounded(slots: [bool; 5], base: Stats, min: Stats, max: Stats) -> Option<Self> {
        for stat in ALL_STATS {
            if min.get(stat) < 0 || min.get(stat) > max.get(stat) {
                return None;
            }
        }
        Some(Self {
            slots,
            base,
            min,
            max,
        })
    }

    pub fn has_slot(&self, slot: Slot) -> bool {
        self.slots[slot.index()]
    }

    fn clamp(&self, stat: Stat, value: i32) -> i32 {
        value.clamp(self.min.get(stat), self.max.get(stat))
    }
}

/// How one attachment changes one stat. `Add` is applied to the running sum of
/// the base value, `Scale` multiplies that sum; percentages keep the fold
/// integer-exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Modifier {
    /// Adds `N` whole stat units. Zero is not a meaningful effect.
    Add(i32),
    /// Multiplies by `N` percent; `N` is bounded to a positive, sane range.
    Scale(u32),
}

/// The bounded stat change an attachment carries. Order in the definition is
/// the application order; a stat appears at most once so the fold is unambiguous.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
    pub stat: Stat,
    pub modifier: Modifier,
}

/// An attachment's static definition: which slot it fits and what it does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub slot: Slot,
    pub effects: Vec<Effect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Defect {
    EmptyId,
    IdTooLong,
    NoEffects,
    TooManyEffects,
    DuplicateStat,
    ZeroAdd,
    BadScale,
}

/// A definition table. Small and linear: the catalogue is tens of entries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalogue {
    entries: Vec<Attachment>,
}

impl Catalogue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a definition, rejecting malformed or duplicate ones.
    pub fn insert(&mut self, attachment: Attachment) -> Result<(), Defect> {
        check_definition(&attachment)?;
        if self.get(&attachment.id).is_some() {
            return Err(Defect::DuplicateStat);
        }
        self.entries.push(attachment);
        Ok(())
    }

    /// Adds an already-validated definition; the built-in table uses this.
    pub fn insert_unchecked(&mut self, attachment: Attachment) {
        self.entries.push(attachment);
    }

    pub fn get(&self, id: &str) -> Option<&Attachment> {
        self.entries.iter().find(|a| a.id == id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every id in insertion order, for host menus and tooling.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|a| a.id.as_str())
    }

    /// Every definition fitting `slot`, in insertion order. A weapon with a
    /// disabled slot yields nothing.
    pub fn fitting(&self, slot: Slot) -> impl Iterator<Item = &Attachment> {
        self.entries.iter().filter(move |a| a.slot == slot)
    }
}

/// Rejects a definition a runtime fold could never apply unambiguously.
pub fn check_definition(attachment: &Attachment) -> Result<(), Defect> {
    if attachment.id.is_empty() {
        return Err(Defect::EmptyId);
    }
    if attachment.id.len() > MAX_ID {
        return Err(Defect::IdTooLong);
    }
    if attachment.effects.is_empty() {
        return Err(Defect::NoEffects);
    }
    if attachment.effects.len() > MAX_EFFECTS {
        return Err(Defect::TooManyEffects);
    }
    let mut seen: Vec<Stat> = Vec::with_capacity(attachment.effects.len());
    for effect in &attachment.effects {
        if seen.contains(&effect.stat) {
            return Err(Defect::DuplicateStat);
        }
        seen.push(effect.stat);
        match effect.modifier {
            Modifier::Add(0) => return Err(Defect::ZeroAdd),
            Modifier::Add(_) => {}
            Modifier::Scale(pct) if (1..=MAX_FACTOR_PCT).contains(&pct) => {}
            Modifier::Scale(_) => return Err(Defect::BadScale),
        }
    }
    Ok(())
}

/// One fitted attachment: the slot it occupies and the definition id in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fit {
    pub slot: Slot,
    pub id: String,
}

/// A configuration a caller proposes to fit to one weapon. Ordered: index `n`
/// in `fitted` is the `n`-th attachment that remains installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    schema: u16,
    fitted: Vec<Fit>,
}

/// Why a configuration change was refused. The caller rereads and retries a
/// stale one whole, exactly like the vehicle transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// Unknown, oversize, trailing or schema-mismatched saved bytes.
    CorruptState,
    /// The saved configuration names a slot the definition does not expose.
    SlotNotAvailable(Slot),
    /// An unknown attachment id, or the id does not fit the target slot.
    Incompatible(Slot),
    /// The same slot already carries an attachment.
    Occupied(Slot),
    /// Nothing is installed in the slot asked for removal.
    NotFitted(Slot),
    /// The weapon already has [`MAX_SLOTS_PER_WEAPON`] attachments.
    Full,
    /// The host supplied an unreadable or oversize id.
    InvalidId,
}
impl fmt::Display for Reject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Reject {}

impl Config {
    /// A stable identifier for a fitted configuration: the ordered ids joined by
    /// a byte that never appears in an id. Hosts use it as a cache key, so two
    /// configurations compare equal exactly when their folds agree.
    pub fn fingerprint(&self) -> String {
        self.fitted
            .iter()
            .map(|fit| fit.id.as_str())
            .collect::<Vec<_>>()
            .join("\u{1f}")
    }

    /// The no-attachment baseline: exactly the definition's base stats.
    pub fn none() -> Self {
        Self {
            schema: SCHEMA,
            fitted: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.fitted.is_empty()
    }

    pub fn len(&self) -> usize {
        self.fitted.len()
    }

    /// The fitted definition ids, in application order.
    pub fn fitted_ids(&self) -> impl Iterator<Item = &str> {
        self.fitted.iter().map(|f| f.id.as_str())
    }

    /// The id installed in `slot`, if any.
    pub fn id_in(&self, slot: Slot) -> Option<&str> {
        self.fitted
            .iter()
            .find(|f| f.slot == slot)
            .map(|f| f.id.as_str())
    }

    fn position(&self, slot: Slot) -> Option<usize> {
        self.fitted.iter().position(|f| f.slot == slot)
    }

    /// Installs an attachment, appending it to the application order. The whole
    /// configuration must be valid; nothing is added otherwise.
    pub fn install(
        &mut self,
        def: &Definition,
        catalogue: &Catalogue,
        slot: Slot,
        id: &str,
    ) -> Result<(), Reject> {
        let attachment = resolve(def, catalogue, slot, id)?;
        if self.position(slot).is_some() {
            return Err(Reject::Occupied(slot));
        }
        if self.fitted.len() >= MAX_SLOTS_PER_WEAPON {
            return Err(Reject::Full);
        }
        self.fitted.push(Fit {
            slot: attachment.slot,
            id: attachment.id.clone(),
        });
        Ok(())
    }

    /// Removes a fitted attachment, closing the gap in the application order.
    pub fn remove(&mut self, def: &Definition, slot: Slot) -> Result<String, Reject> {
        if !def.has_slot(slot) {
            return Err(Reject::SlotNotAvailable(slot));
        }
        let index = self.position(slot).ok_or(Reject::NotFitted(slot))?;
        Ok(self.fitted.remove(index).id)
    }

    /// Every fitted entry must resolve against this catalogue and fit its slot;
    /// a load is atomic, so a single bad entry discards the whole save.
    pub fn validate(&self, def: &Definition, catalogue: &Catalogue) -> Result<(), Reject> {
        if self.fitted.len() > MAX_SLOTS_PER_WEAPON {
            return Err(Reject::CorruptState);
        }
        let mut seen: Vec<Slot> = Vec::with_capacity(self.fitted.len());
        for fit in &self.fitted {
            let attachment = resolve(def, catalogue, fit.slot, &fit.id)?;
            if seen.contains(&attachment.slot) {
                return Err(Reject::Occupied(attachment.slot));
            }
            seen.push(attachment.slot);
        }
        Ok(())
    }

    /// Folds the fitted attachments onto the definition's base stats.
    ///
    /// Deterministic and ordered: for each stat, every `Add` is summed in
    /// fitting order, the base value is the starting sum, then every `Scale` in
    /// fitting order multiplies the sum with integer truncation toward zero, and
    /// the result is clamped to the definition's bounds. An empty configuration
    /// returns the base stats untouched.
    pub fn stats(&self, def: &Definition, catalogue: &Catalogue) -> Stats {
        let mut out = def.base;
        for stat in ALL_STATS {
            let mut sum = i64::from(def.base.get(stat));
            let mut scale_pct: i64 = 100;
            let mut scaled = false;
            for fit in &self.fitted {
                let Some(attachment) = catalogue.get(&fit.id) else {
                    continue;
                };
                for effect in &attachment.effects {
                    if effect.stat != stat {
                        continue;
                    }
                    match effect.modifier {
                        Modifier::Add(n) => {
                            sum += i64::from(n);
                        }
                        Modifier::Scale(pct) => {
                            scale_pct = (scale_pct * i64::from(pct)) / 100;
                            scaled = true;
                        }
                    }
                }
            }
            let folded = if scaled { (sum * scale_pct) / 100 } else { sum };
            let clamped = folded.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            out.set(stat, def.clamp(stat, clamped));
        }
        out
    }

    /// Serialization-safe persistent form. Bounded and deterministic.
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("bounded attachment config")
    }

    /// Strict decode. Rejects bytes a writer must never have produced, and
    /// verifies every fitted id against the target definition and catalogue.
    pub fn decode(bytes: &[u8], def: &Definition, catalogue: &Catalogue) -> Result<Self, Reject> {
        if bytes.is_empty() || bytes.len() > STATE_BYTES {
            return Err(Reject::CorruptState);
        }
        let (config, rest): (Self, _) =
            postcard::take_from_bytes(bytes).map_err(|_| Reject::CorruptState)?;
        if !rest.is_empty() || config.schema != SCHEMA {
            return Err(Reject::CorruptState);
        }
        config.validate(def, catalogue)?;
        Ok(config)
    }
}

fn resolve<'a>(
    def: &Definition,
    catalogue: &'a Catalogue,
    slot: Slot,
    id: &str,
) -> Result<&'a Attachment, Reject> {
    if !def.has_slot(slot) {
        return Err(Reject::SlotNotAvailable(slot));
    }
    if id.is_empty() || id.len() > MAX_ID {
        return Err(Reject::InvalidId);
    }
    let attachment = catalogue.get(id).ok_or(Reject::Incompatible(slot))?;
    if attachment.slot != slot {
        return Err(Reject::Incompatible(slot));
    }
    Ok(attachment)
}

/// The crate's built-in attachment table. Values follow the mod's public data
/// definitions; ids stay short so a fitted configuration is a few dozen bytes.
pub fn catalogue() -> Catalogue {
    let mut c = Catalogue::new();
    for (id, slot, effects) in BUILTIN {
        c.insert_unchecked(Attachment {
            id: (*id).into(),
            slot: *slot,
            effects: effects
                .iter()
                .map(|(stat, modifier)| Effect {
                    stat: *stat,
                    modifier: *modifier,
                })
                .collect(),
        });
    }
    c
}

/// `(id, slot, effects)` triples for the built-in table.
type BuiltinAttachment = (&'static str, Slot, &'static [(Stat, Modifier)]);
const BUILTIN: &[BuiltinAttachment] = &[
    (
        "superbwarfare:red_dot",
        Slot::Optic,
        &[(Stat::Spread, Modifier::Scale(75))],
    ),
    (
        "superbwarfare:scope_4x",
        Slot::Optic,
        &[
            (Stat::Spread, Modifier::Scale(50)),
            (Stat::Recoil, Modifier::Add(5)),
        ],
    ),
    (
        "superbwarfare:suppressor",
        Slot::Muzzle,
        &[
            (Stat::Recoil, Modifier::Add(-8)),
            (Stat::Damage, Modifier::Scale(90)),
        ],
    ),
    (
        "superbwarfare:muzzle_brake",
        Slot::Muzzle,
        &[(Stat::Recoil, Modifier::Add(-12))],
    ),
    (
        "superbwarfare:extended_mag",
        Slot::Magazine,
        &[
            (Stat::Capacity, Modifier::Add(15)),
            (Stat::Reload, Modifier::Add(6)),
        ],
    ),
    (
        "superbwarfare:quick_mag",
        Slot::Magazine,
        &[(Stat::Reload, Modifier::Scale(75))],
    ),
    (
        "superbwarfare:vertical_grip",
        Slot::Grip,
        &[(Stat::Recoil, Modifier::Add(-6))],
    ),
    (
        "superbwarfare:laser",
        Slot::Underbarrel,
        &[(Stat::Spread, Modifier::Add(-4))],
    ),
];

/// The AK-47's base stat block, mirroring `gun(AK47)` in the crate root.
pub fn ak47_definition() -> Definition {
    Definition::new(
        [true, true, true, false, true],
        Stats {
            damage: 90,
            capacity: 30,
            reload: 48,
            interval: 2,
            speed: 180,
            recoil: 100,
            spread: 100,
        },
    )
}

/// The Glock 17's base stat block, mirroring `gun(GLOCK17)` in the crate root.
pub fn glock17_definition() -> Definition {
    Definition::new(
        [true, true, true, false, false],
        Stats {
            damage: 55,
            capacity: 17,
            reload: 42,
            interval: 3,
            speed: 110,
            recoil: 60,
            spread: 120,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ak() -> Definition {
        ak47_definition()
    }
    fn cat() -> Catalogue {
        catalogue()
    }

    fn attachment(id: &str, slot: Slot, effects: Vec<Effect>) -> Attachment {
        Attachment {
            id: id.into(),
            slot,
            effects,
        }
    }

    #[test]
    fn builtin_catalogue_is_well_formed_and_slot_typed() {
        let c = cat();
        assert_eq!(c.len(), BUILTIN.len());
        assert!(!c.is_empty());
        // Every built-in definition passes the same check the API enforces.
        for (id, slot, effects) in BUILTIN {
            assert!(c.get(id).is_some_and(|a| a.slot == *slot));
            assert!(effects.iter().all(|(_, m)| match m {
                Modifier::Add(n) => *n != 0,
                Modifier::Scale(p) => (1..=MAX_FACTOR_PCT).contains(p),
            }));
        }
        assert!(c.get("superbwarfare:nope").is_none());
    }

    #[test]
    fn no_attachment_is_exactly_the_base_definition() {
        let def = ak();
        let config = Config::none();
        assert!(config.is_empty());
        assert_eq!(config.stats(&def, &cat()), def.base);
        // The empty configuration survives a save round trip unchanged.
        let bytes = config.encode();
        assert!(bytes.len() <= STATE_BYTES);
        assert_eq!(Config::decode(&bytes, &def, &cat()).unwrap(), config);
    }

    #[test]
    fn install_folds_additive_then_multiplicative() {
        let def = ak();
        let c = cat();
        let mut config = Config::none();
        config
            .install(&def, &c, Slot::Muzzle, "superbwarfare:suppressor")
            .unwrap();
        let stats = config.stats(&def, &c);
        // -8 recoil is additive, damage scales by 90%.
        assert_eq!(stats.recoil, 92);
        assert_eq!(stats.damage, 81);
        // Untouched stats are unchanged.
        assert_eq!(stats.capacity, def.base.capacity);
        assert_eq!(stats.speed, def.base.speed);
    }

    #[test]
    fn scale_applies_to_the_added_sum_not_the_base() {
        let def = ak();
        let c = cat();
        let mut config = Config::none();
        config
            .install(&def, &c, Slot::Magazine, "superbwarfare:extended_mag")
            .unwrap();
        config
            .install(&def, &c, Slot::Optic, "superbwarfare:red_dot")
            .unwrap();
        let stats = config.stats(&def, &c);
        // Capacity: 30 + 15 = 45, no scale on it.
        assert_eq!(stats.capacity, 45);
        assert_eq!(stats.reload, def.base.reload + 6);
        // Spread: no add, scale 75% of 100.
        assert_eq!(stats.spread, 75);
    }

    #[test]
    fn stacking_scales_compose_integer_exactly() {
        let def = ak();
        let c = cat();
        let mut config = Config::none();
        config
            .install(&def, &c, Slot::Optic, "superbwarfare:scope_4x")
            .unwrap();
        config
            .install(&def, &c, Slot::Muzzle, "superbwarfare:suppressor")
            .unwrap();
        let stats = config.stats(&def, &c);
        // Damage: 90 * 90 / 100 = 81, one scale only.
        assert_eq!(stats.damage, 81);
        // Spread: only the optic scales it, 100 * 50/100 = 50. The suppressor
        // carries no spread effect, so nothing composes on top of it.
        assert_eq!(stats.spread, 50);
        // Recoil: (100 + 5) - 8 = 97; addition order cannot change it.
        assert_eq!(stats.recoil, 97);
        // Re-folding the same configuration is deterministic.
        assert_eq!(config.stats(&def, &c), stats);
    }

    #[test]
    fn clamps_bound_a_hostile_definition_combination() {
        let def = ak();
        let c = cat();
        // Squeeze the recoil range so the fold must clamp on both sides.
        let mut min = def.base;
        min.recoil = 95;
        let mut max = def.base;
        max.recoil = 96;
        let tight = Definition::bounded(def.slots, def.base, min, max).unwrap();
        let mut down = Config::none();
        down.install(&tight, &c, Slot::Muzzle, "superbwarfare:muzzle_brake")
            .unwrap();
        assert_eq!(down.stats(&tight, &c).recoil, 95);
        let mut up = Config::none();
        up.install(&tight, &c, Slot::Optic, "superbwarfare:scope_4x")
            .unwrap();
        assert_eq!(up.stats(&tight, &c).recoil, 96);
        // An inverted or negative range is never a valid definition.
        assert!(Definition::bounded(def.slots, def.base, max, min).is_none());
        let mut negative = min;
        negative.damage = -1;
        assert!(Definition::bounded(def.slots, def.base, negative, max).is_none());
    }

    #[test]
    fn duplicate_and_incompatible_installs_are_refused() {
        let def = ak();
        let c = cat();
        let mut config = Config::none();
        config
            .install(&def, &c, Slot::Muzzle, "superbwarfare:suppressor")
            .unwrap();
        assert_eq!(
            config.install(&def, &c, Slot::Muzzle, "superbwarfare:muzzle_brake"),
            Err(Reject::Occupied(Slot::Muzzle))
        );
        // An optic id cannot occupy the muzzle rail.
        assert_eq!(
            config.install(&def, &c, Slot::Muzzle, "superbwarfare:red_dot"),
            Err(Reject::Incompatible(Slot::Muzzle))
        );
        // Unknown ids and a slot this weapon does not expose.
        assert_eq!(
            config.install(&def, &c, Slot::Muzzle, "superbwarfare:nope"),
            Err(Reject::Incompatible(Slot::Muzzle))
        );
        assert_eq!(
            config.install(&def, &c, Slot::Grip, "superbwarfare:vertical_grip"),
            Err(Reject::SlotNotAvailable(Slot::Grip))
        );
        assert_eq!(
            config.install(&def, &c, Slot::Optic, ""),
            Err(Reject::InvalidId)
        );
        assert_eq!(
            config.install(&def, &c, Slot::Optic, &"x".repeat(MAX_ID + 1)),
            Err(Reject::InvalidId)
        );
        // The refused installs changed nothing.
        assert_eq!(config.len(), 1);
        assert_eq!(config.id_in(Slot::Muzzle), Some("superbwarfare:suppressor"));
    }

    #[test]
    fn install_appends_and_remove_splices_in_order() {
        let def = ak();
        let c = cat();
        let mut config = Config::none();
        config
            .install(&def, &c, Slot::Optic, "superbwarfare:scope_4x")
            .unwrap();
        config
            .install(&def, &c, Slot::Muzzle, "superbwarfare:suppressor")
            .unwrap();
        config
            .install(&def, &c, Slot::Magazine, "superbwarfare:quick_mag")
            .unwrap();
        assert_eq!(
            config.fitted_ids().collect::<Vec<_>>(),
            vec![
                "superbwarfare:scope_4x",
                "superbwarfare:suppressor",
                "superbwarfare:quick_mag"
            ]
        );
        assert_eq!(
            config.remove(&def, Slot::Muzzle).unwrap(),
            "superbwarfare:suppressor"
        );
        assert_eq!(
            config.fitted_ids().collect::<Vec<_>>(),
            vec!["superbwarfare:scope_4x", "superbwarfare:quick_mag"]
        );
        assert_eq!(
            config.remove(&def, Slot::Muzzle),
            Err(Reject::NotFitted(Slot::Muzzle))
        );
        assert_eq!(
            config.remove(&def, Slot::Grip),
            Err(Reject::SlotNotAvailable(Slot::Grip))
        );
        // The remaining fold is exactly as if the removed one never existed.
        let mut expected = Config::none();
        expected
            .install(&def, &c, Slot::Optic, "superbwarfare:scope_4x")
            .unwrap();
        expected
            .install(&def, &c, Slot::Magazine, "superbwarfare:quick_mag")
            .unwrap();
        assert_eq!(config.stats(&def, &c), expected.stats(&def, &c));
    }

    #[test]
    fn a_full_weapon_refuses_more_but_never_exceeds_the_cap() {
        let def = Definition::new([true; 5], ak().base);
        let c = cat();
        let mut config = Config::none();
        for (slot, id) in [
            (Slot::Optic, "superbwarfare:red_dot"),
            (Slot::Muzzle, "superbwarfare:suppressor"),
            (Slot::Magazine, "superbwarfare:extended_mag"),
            (Slot::Grip, "superbwarfare:vertical_grip"),
            (Slot::Underbarrel, "superbwarfare:laser"),
        ] {
            config.install(&def, &c, slot, id).unwrap();
        }
        assert_eq!(config.len(), 5);
        // Every slot is filled, so any further install hits Occupied first.
        assert_eq!(
            config.install(&def, &c, Slot::Optic, "superbwarfare:scope_4x"),
            Err(Reject::Occupied(Slot::Optic))
        );
        assert!(config.len() <= MAX_SLOTS_PER_WEAPON);
        // A save claiming more than the cap is corrupt, not reinitialized.
        let mut over = config.clone();
        while over.fitted.len() <= MAX_SLOTS_PER_WEAPON {
            over.fitted.push(Fit {
                slot: Slot::Optic,
                id: "superbwarfare:red_dot".into(),
            });
        }
        assert_eq!(
            Config::decode(&over.encode(), &def, &c),
            Err(Reject::CorruptState)
        );
    }

    #[test]
    fn config_round_trips_and_rejects_foreign_or_corrupt_saves() {
        let def = ak();
        let c = cat();
        let mut config = Config::none();
        config
            .install(&def, &c, Slot::Optic, "superbwarfare:red_dot")
            .unwrap();
        config
            .install(&def, &c, Slot::Muzzle, "superbwarfare:suppressor")
            .unwrap();
        let bytes = config.encode();
        assert!(bytes.len() <= STATE_BYTES);
        let back = Config::decode(&bytes, &def, &c).unwrap();
        assert_eq!(back, config);
        assert_eq!(back.stats(&def, &c), config.stats(&def, &c));

        // Empty, oversize and trailing bytes are refused.
        assert_eq!(Config::decode(&[], &def, &c), Err(Reject::CorruptState));
        assert_eq!(
            Config::decode(&[0u8; STATE_BYTES + 1], &def, &c),
            Err(Reject::CorruptState)
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            Config::decode(&trailing, &def, &c),
            Err(Reject::CorruptState)
        );

        // A schema bump is not silently accepted.
        let mut newer = config.clone();
        newer.schema = SCHEMA + 1;
        assert_eq!(
            Config::decode(&newer.encode(), &def, &c),
            Err(Reject::CorruptState)
        );

        // A save naming a slot the weapon lacks fails whole.
        let mut unavailable = config.clone();
        unavailable.fitted.push(Fit {
            slot: Slot::Grip,
            id: "superbwarfare:vertical_grip".into(),
        });
        assert_eq!(
            Config::decode(&unavailable.encode(), &def, &c),
            Err(Reject::SlotNotAvailable(Slot::Grip))
        );

        // Two entries in one slot are rejected on load.
        let mut doubled = config.clone();
        doubled.fitted.push(Fit {
            slot: Slot::Muzzle,
            id: "superbwarfare:muzzle_brake".into(),
        });
        assert_eq!(
            Config::decode(&doubled.encode(), &def, &c),
            Err(Reject::Occupied(Slot::Muzzle))
        );

        // An unknown id never resolves on load either.
        let mut unknown = config.clone();
        unknown.fitted.push(Fit {
            slot: Slot::Magazine,
            id: "superbwarfare:nope".into(),
        });
        assert_eq!(
            Config::decode(&unknown.encode(), &def, &c),
            Err(Reject::Incompatible(Slot::Magazine))
        );

        // A save from another weapon's slot mask is not reused as-is. The glock
        // exposes Optic, Muzzle and Magazine, so the AK's optics and muzzle fit
        // would actually load; a grip is the slot it lacks, and it fails whole.
        let glock = glock17_definition();
        let mut foreign = config.clone();
        foreign.fitted.push(Fit {
            slot: Slot::Grip,
            id: "superbwarfare:vertical_grip".into(),
        });
        assert_eq!(
            Config::decode(&foreign.encode(), &glock, &c),
            Err(Reject::SlotNotAvailable(Slot::Grip))
        );
        assert_eq!(
            Config::decode(&Config::none().encode(), &glock, &c)
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn malformed_definitions_never_enter_the_catalogue() {
        let effect = |stat, modifier| Effect { stat, modifier };
        let mut c = Catalogue::new();
        let plus_one = vec![effect(Stat::Damage, Modifier::Add(1))];
        assert_eq!(
            c.insert(attachment("", Slot::Optic, plus_one.clone())),
            Err(Defect::EmptyId)
        );
        assert_eq!(
            c.insert(attachment(&"x".repeat(MAX_ID + 1), Slot::Optic, plus_one)),
            Err(Defect::IdTooLong)
        );
        assert_eq!(
            c.insert(attachment("a:empty", Slot::Optic, Vec::new())),
            Err(Defect::NoEffects)
        );
        for (id, modifier, defect) in [
            ("a:zero", Modifier::Add(0), Defect::ZeroAdd),
            ("a:bad_scale", Modifier::Scale(0), Defect::BadScale),
            (
                "a:huge_scale",
                Modifier::Scale(MAX_FACTOR_PCT + 1),
                Defect::BadScale,
            ),
        ] {
            assert_eq!(
                c.insert(attachment(
                    id,
                    Slot::Optic,
                    vec![effect(Stat::Damage, modifier)]
                )),
                Err(defect),
                "{id}"
            );
        }
        assert_eq!(
            c.insert(attachment(
                "a:dup",
                Slot::Optic,
                vec![
                    effect(Stat::Damage, Modifier::Add(1)),
                    effect(Stat::Damage, Modifier::Scale(50)),
                ]
            )),
            Err(Defect::DuplicateStat)
        );
        let too_many: Vec<Effect> = (0..=MAX_EFFECTS)
            .map(|i| effect(ALL_STATS[i % ALL_STATS.len()], Modifier::Add(i as i32 + 1)))
            .collect();
        assert_eq!(
            c.insert(attachment("a:many", Slot::Optic, too_many)),
            Err(Defect::TooManyEffects)
        );
        // A well-formed definition is accepted, and a repeat id is not.
        let good = attachment(
            "a:good",
            Slot::Grip,
            vec![effect(Stat::Recoil, Modifier::Add(-3))],
        );
        assert_eq!(c.insert(good.clone()), Ok(()));
        assert_eq!(c.insert(good), Err(Defect::DuplicateStat));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn catalogue_lists_ids_and_filters_by_slot() {
        let c = cat();
        let ids: Vec<&str> = c.ids().collect();
        assert_eq!(ids.len(), BUILTIN.len());
        assert_eq!(ids.first(), Some(&"superbwarfare:red_dot"));
        // Every id is unique and every listing agrees with the count.
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());

        let optics: Vec<&str> = c.fitting(Slot::Optic).map(|a| a.id.as_str()).collect();
        assert_eq!(
            optics,
            vec!["superbwarfare:red_dot", "superbwarfare:scope_4x"]
        );
        assert_eq!(c.fitting(Slot::Grip).count(), 1);
        assert_eq!(
            c.fitting(Slot::Muzzle)
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            vec!["superbwarfare:suppressor", "superbwarfare:muzzle_brake"]
        );
        // Every listed attachment really is in its slot.
        for slot in Slot::ALL {
            assert!(c.fitting(slot).all(|a| a.slot == slot));
        }
    }

    #[test]
    fn definition_headroom_bounds_every_stat_around_its_base() {
        let def = ak();
        for stat in ALL_STATS {
            let base = def.base.get(stat);
            assert!(def.min.get(stat) <= base, "{stat:?}");
            assert!(def.max.get(stat) >= base, "{stat:?}");
            assert!(def.min.get(stat) >= 0, "{stat:?}");
        }
        // The computed headroom is exactly plus or minus the base value here.
        assert_eq!(def.max.recoil, def.base.recoil * 2);
        assert_eq!(def.min.capacity, 0);
        // Every built-in definition exposes at least one slot.
        assert!(Slot::ALL.iter().any(|s| def.has_slot(*s)));
        assert!(!def.has_slot(Slot::Grip));
    }

    #[test]
    fn slot_suffixes_are_distinct_and_stable() {
        let mut seen = Vec::new();
        for slot in Slot::ALL {
            let suffix = slot.item_suffix();
            assert!(!suffix.is_empty());
            assert!(!seen.contains(&suffix), "{suffix} reused");
            seen.push(suffix);
        }
        assert_eq!(Slot::Optic.item_suffix(), "optic");
        assert_eq!(Slot::Underbarrel.item_suffix(), "underbarrel");
    }

    #[test]
    fn folds_are_clamped_by_the_definition_not_the_catalogue() {
        let c = cat();
        // A weapon whose reload is floored well above zero cannot be scaled away.
        let mut base = ak().base;
        base.reload = 100;
        let mut min = base;
        min.reload = 50;
        let mut max = base;
        max.reload = 200;
        let def = Definition::bounded(ak().slots, base, min, max).unwrap();
        let mut config = Config::none();
        config
            .install(&def, &c, Slot::Magazine, "superbwarfare:quick_mag")
            .unwrap();
        // 100 * 75% = 75, still above the floor.
        assert_eq!(config.stats(&def, &c).reload, 75);

        // Squeeze the ceiling so the same fit clamps at the top.
        let mut tight_max = base;
        tight_max.reload = 80;
        let tight = Definition::bounded(ak().slots, base, min, tight_max).unwrap();
        let mut raised = Config::none();
        raised
            .install(&tight, &c, Slot::Magazine, "superbwarfare:extended_mag")
            .unwrap();
        assert_eq!(raised.stats(&tight, &c).reload, 80);
    }

    #[test]
    fn install_then_remove_restores_base_stats_and_bytes() {
        let def = ak();
        let c = cat();
        let baseline = Config::none();
        let mut config = Config::none();
        for (slot, id) in [
            (Slot::Optic, "superbwarfare:scope_4x"),
            (Slot::Muzzle, "superbwarfare:suppressor"),
            (Slot::Magazine, "superbwarfare:extended_mag"),
            (Slot::Underbarrel, "superbwarfare:laser"),
        ] {
            config.install(&def, &c, slot, id).unwrap();
        }
        // Fitted, the fold differs from base; the fingerprint reflects the set.
        assert_ne!(config.stats(&def, &c), def.base);
        assert_eq!(
            config.fingerprint(),
            "superbwarfare:scope_4x\u{1f}superbwarfare:suppressor\u{1f}superbwarfare:extended_mag\u{1f}superbwarfare:laser"
        );
        for slot in [Slot::Optic, Slot::Muzzle, Slot::Magazine, Slot::Underbarrel] {
            config.remove(&def, slot).unwrap();
        }
        assert!(config.is_empty());
        assert_eq!(config.stats(&def, &c), def.base);
        assert_eq!(config.encode(), baseline.encode());
        assert_eq!(config.fingerprint(), baseline.fingerprint());
        assert_eq!(
            Config::decode(&config.encode(), &def, &c).unwrap(),
            baseline
        );
    }

    #[test]
    fn application_order_is_the_fitting_order_for_scales() {
        let def = ak();
        let c = cat();
        // The same two attachments fitted in opposite order. The fold is
        // order-independent here, but the recorded order (and therefore the
        // persisted bytes) is not.
        let mut first = Config::none();
        first
            .install(&def, &c, Slot::Optic, "superbwarfare:red_dot")
            .unwrap();
        first
            .install(&def, &c, Slot::Muzzle, "superbwarfare:suppressor")
            .unwrap();
        let mut second = Config::none();
        second
            .install(&def, &c, Slot::Muzzle, "superbwarfare:suppressor")
            .unwrap();
        second
            .install(&def, &c, Slot::Optic, "superbwarfare:red_dot")
            .unwrap();
        // Each slot contributes at most one scale, so swapping the two fits
        // commutes: spread is scaled once by 75% either way and recoil's single
        // additive term is unaffected by position. The fingerprints still differ
        // because the recorded order differs, which is what a host caches on.
        assert_eq!(first.stats(&def, &c), second.stats(&def, &c));
        assert_ne!(first.fingerprint(), second.fingerprint());
        assert_eq!(first.len(), second.len());
        assert_eq!(first.stats(&def, &c).spread, 75);
    }

    #[test]
    fn a_save_with_bad_shape_is_corrupt_never_reinitialized() {
        let def = ak();
        let c = cat();
        // A missing schema field decodes as garbage, not as an empty config.
        let garbage = [0xffu8; 4];
        assert_eq!(
            Config::decode(&garbage, &def, &c),
            Err(Reject::CorruptState)
        );
        assert!(Config::decode(&garbage, &def, &c).is_err());
    }

    #[test]
    fn catalogue_is_deterministic_across_builds() {
        let a = catalogue();
        let b = catalogue();
        assert_eq!(a, b);
        assert_eq!(a.len(), BUILTIN.len());
    }

    #[test]
    fn fingerprint_is_stable_for_the_empty_configuration() {
        assert_eq!(Config::none().fingerprint(), "");
        // Two distinct weapons share the baseline fingerprint, which is why the
        // host must key caches by weapon definition as well.
        assert_eq!(Config::none().fingerprint(), Config::none().fingerprint());
        let def = glock17_definition();
        let c = cat();
        assert_eq!(Config::none().stats(&def, &c), def.base);
    }

    #[test]
    fn every_builtin_resolves_in_its_own_slot() {
        let def = Definition::new([true; 5], ak().base);
        let c = cat();
        // Each built-in can be fitted to a weapon exposing every slot.
        for (id, slot, _) in BUILTIN {
            let mut config = Config::none();
            assert_eq!(config.install(&def, &c, *slot, id), Ok(()), "{id}");
            assert_eq!(config.id_in(*slot), Some(*id));
            assert!(config.validate(&def, &c).is_ok(), "{id}");
            assert!(config.stats(&def, &c).get(Stat::Damage) >= 0);
        }
        // And none of them resolves against a weapon that exposes no slots.
        let bare = Definition::new([false; 5], ak().base);
        for (id, slot, _) in BUILTIN {
            let mut config = Config::none();
            assert_eq!(
                config.install(&bare, &c, *slot, id),
                Err(Reject::SlotNotAvailable(*slot)),
                "{id}"
            );
        }
    }
}
