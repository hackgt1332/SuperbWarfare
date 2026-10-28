//! Typed, data-driven weapon and ammunition catalog.
//!
//! The original SuperbWarfare mod defines its whole arsenal as JSON under
//! `data/<ns>/sbw/guns/*.json`. Only two of those definitions (AK-47, Glock 17)
//! currently have ported gameplay. This module imports the *metadata* of the
//! complete arsenal into typed, validated records so tooling and later work can
//! reason about the wider set without guessing at string keys.
//!
//! # What this is and is not
//!
//! * It is a **catalog**: deterministic, validated facts read from the original
//!   JSON — ids, ammo family, magazine sizes, damage, fire modes, reload times,
//!   special semantics.
//! * It is **not** a claim of playability. Importing a record, or marking it
//!   [`Status::Ported`], never by itself makes a gun shootable in Horizon. Only
//!   [`crate::gun`] + the bridge decide that, and today it answers for exactly
//!   two ids. Tests assert the ported set is a subset of the catalog and that
//!   ported stat rows equal the live gameplay values.
//! * Unsupported ammo families — energy (`FE`), infinite/free (`Infinity`), and
//!   disposable/special items — are represented explicitly as
//!   [`AmmoClass::Unsupported`] with the reason, rather than being silently
//!   coerced into a bullet.
//!
//! Loading is pure: it takes bytes and returns records. No filesystem, no clock,
//! no RNG, so a converted catalog is byte-identical for identical input.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

/// Prefix for every id this catalog emits.
pub const NAMESPACE: &str = "superbwarfare";

/// Working form of a gun definition id, e.g. `superbwarfare:ak_47`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GunId(pub String);

impl GunId {
    /// Build a namespaced id from a bare file stem, e.g. `ak_47`.
    pub fn from_name(name: &str) -> Self {
        if name.contains(':') {
            Self(name.to_owned())
        } else {
            Self(format!("{NAMESPACE}:{name}"))
        }
    }
    /// Bare name without the namespace, e.g. `ak_47`.
    pub fn name(&self) -> &str {
        self.0.split_once(':').map(|(_, n)| n).unwrap_or(&self.0)
    }
}

impl fmt::Display for GunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How the shooter feeds a weapon. Derived from the original `AmmoType`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AmmoClass {
    /// A normal consumable bullet family, e.g. `@RifleAmmo`.
    Bullet(AmmoFamily),
    /// Charges an internal energy buffer, e.g. `"FE"` (forge energy) or the
    /// ability to shoot with no item (`Infinity`). No bullet item required.
    Energy,
    /// Does not consume ammo items at all — melee/tool/utility weapons.
    Infinite,
    /// A bespoke item id round, e.g. `superbwarfare:rpg_rocket_tbg`.
    Item(String),
    /// Not modelled yet; carries the reason for the explicit gap.
    Unsupported(UnsupportedReason),
}

/// A recognised bullet ammo family. Feastable gameplay only currently wires
/// the first two, but the family list is complete for the original set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AmmoFamily {
    Rifle,
    Handgun,
    Shotgun,
    Sniper,
    Heavy,
}

impl AmmoFamily {
    /// Consumable item id backing this family.
    pub fn item(self) -> &'static str {
        match self {
            Self::Rifle => "superbwarfare:rifle_ammo",
            Self::Handgun => "superbwarfare:handgun_ammo",
            Self::Shotgun => "superbwarfare:shotgun_ammo",
            Self::Sniper => "superbwarfare:sniper_ammo",
            Self::Heavy => "superbwarfare:heavy_ammo",
        }
    }
    /// Parse the `@FamilyAmmo` shorthand used in the source JSON.
    pub fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "@RifleAmmo" => Self::Rifle,
            "@HandgunAmmo" => Self::Handgun,
            "@ShotgunAmmo" => Self::Shotgun,
            "@SniperAmmo" => Self::Sniper,
            "@HeavyAmmo" => Self::Heavy,
            _ => return None,
        })
    }
}

/// Why a definition could not be fully modelled. Kept explicit so an importer
/// never has to pretend a value exists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnsupportedReason {
    /// Energy/fuel charges (`FE`), e.g. the Devotion, Taser, repair tool.
    EnergyAmmo,
    /// `Infinity`: the weapon does not draw from any ammo pool.
    FreeAmmo,
    /// A non-`@` item id we have not catalogued an item for.
    UnknownItem(String),
    /// A `/`-separated or otherwise structured ammo string we do not parse.
    ComplexAmmo(String),
    /// `AmmoType` absent entirely.
    MissingAmmo,
    /// Multiple ammo types including at least one we cannot model.
    MixedAmmo,
    /// A fire-mode token outside the modelled `Semi`/`Auto`/`Burst` set,
    /// e.g. `Charge`. Recorded rather than coerced to a default.
    UnknownFireMode(String),
}

impl fmt::Display for UnsupportedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EnergyAmmo => f.write_str("energy/fuel ammo (FE) is not simulated"),
            Self::FreeAmmo => f.write_str("infinite ammo weapons are not simulated"),
            Self::UnknownItem(id) => write!(f, "unknown ammo item {id}"),
            Self::ComplexAmmo(s) => write!(f, "unparsed ammo expression {s:?}"),
            Self::MissingAmmo => f.write_str("no AmmoType declared"),
            Self::MixedAmmo => f.write_str("mixed ammo types not modelled"),
            Self::UnknownFireMode(s) => write!(f, "fire mode {s:?} is not modelled"),
        }
    }
}

/// Fire selectors available on a weapon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FireMode {
    Semi,
    Auto,
    Burst,
}

impl FireMode {
    pub fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "Semi" => Self::Semi,
            "Auto" => Self::Auto,
            "Burst" => Self::Burst,
            _ => return None,
        })
    }
}

/// Broad weapon category from `GunType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Category {
    Rifle,
    Handgun,
    Shotgun,
    Smg,
    Sniper,
    MachineGun,
    Launcher,
    Special,
    Other,
}

impl Category {
    pub fn parse(token: &str) -> Self {
        match token {
            "Rifle" => Self::Rifle,
            "Handgun" => Self::Handgun,
            "Shotgun" => Self::Shotgun,
            "Smg" => Self::Smg,
            "Sniper" => Self::Sniper,
            "MachineGun" => Self::MachineGun,
            "DirectLauncher" | "CurvedLauncher" | "Launcher" => Self::Launcher,
            "Special" => Self::Special,
            _ => Self::Other,
        }
    }
}

/// Whether a definition has live gameplay in this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// Values are wired into [`crate::gun`] and shootable through the bridge.
    Ported,
    /// Metadata imported and validated, but not yet playable here.
    Cataloged,
}

/// Validated metadata for one original weapon definition.
///
/// Every field is optional where the source may omit it; parsers fill defaults
/// documented on the accessor. Nothing here grants gameplay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeaponMeta {
    pub id: GunId,
    pub category: Category,
    pub status: Status,
    /// Ammo family or explicit unsupported reason.
    pub ammo: AmmoClass,
    /// Base (first) magazine size, in rounds. `None` for melee/no-magazine.
    pub magazine: Option<u32>,
    /// All magazine upgrade levels, ascending; empty when unstated.
    pub magazine_levels: Vec<u32>,
    /// Damage per projectile, if declared.
    pub damage: Option<f32>,
    /// Muzzle velocity in blocks/tick, if declared.
    pub velocity: Option<f32>,
    /// Rounds per minute, if declared.
    pub rpm: Option<u32>,
    /// Fire selector on first equip.
    pub default_fire_mode: FireMode,
    /// Selectors the weapon can cycle.
    pub fire_modes: Vec<FireMode>,
    /// Full-magazine reload time in ticks, if declared.
    pub normal_reload: Option<u16>,
    /// Empty-magazine reload time in ticks, if declared.
    pub empty_reload: Option<u16>,
    /// Explicit gameplay semantics this port does not implement yet.
    pub unsupported: Vec<UnsupportedReason>,
}

impl WeaponMeta {
    /// Ticks between shots implied by `rpm` (600 RPM -> 2 ticks at 20 Hz).
    /// Rounds up and never returns zero for a positive RPM.
    pub fn interval_ticks(&self) -> Option<u16> {
        let rpm = self.rpm?;
        if rpm == 0 {
            return None;
        }
        Some(1200_u32.div_ceil(rpm).max(1) as u16)
    }
    /// Whether any field or nested expression was left unmodelled.
    pub fn has_unsupported(&self) -> bool {
        !self.unsupported.is_empty()
    }
    /// True only if metadata says the weapon is shootable through [`crate::gun`].
    pub fn is_playable(&self) -> bool {
        self.status == Status::Ported
    }
    /// The consumable bullet family, when the weapon uses one.
    pub fn ammo_family(&self) -> Option<AmmoFamily> {
        match &self.ammo {
            AmmoClass::Bullet(family) => Some(*family),
            _ => None,
        }
    }
}

/// Deterministic, ordered catalog. Ordering is by id so output is stable.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    weapons: Vec<WeaponMeta>,
    warnings: Vec<Warning>,
}

impl Catalog {
    /// Import one raw definition plus its bare `name` (file stem). Always
    /// returns a record; characteristics that cannot be modelled are recorded
    /// in [`WeaponMeta::unsupported`] and [`Catalog::warnings`].
    pub fn import_value(name: &str, value: &Value) -> (WeaponMeta, Vec<Warning>) {
        parse_weapon(name, value)
    }

    /// Import raw definition bytes (`serde_json`). Returns an error only for
    /// malformed JSON, never for unknown fields.
    pub fn import_bytes(
        name: &str,
        bytes: &[u8],
    ) -> Result<(WeaponMeta, Vec<Warning>), ImportError> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|e| ImportError::Json(e.to_string()))?;
        Ok(parse_weapon(name, &value))
    }

    /// Build a catalog from `(name, bytes)` pairs. Input order does not affect
    /// output; the catalog is sorted by id and de-duplicated, last write winning
    /// for a repeated id.
    pub fn from_entries<I>(entries: I) -> Result<Self, ImportError>
    where
        I: IntoIterator<Item = (String, Vec<u8>)>,
    {
        let mut pairs: Vec<(String, Vec<u8>)> = entries.into_iter().collect();
        // Normalise input order so the outcome is independent of how the caller
        // enumerated the tree. Equal names break on the raw bytes, so even two
        // definitions sharing a name resolve deterministically.
        pairs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut warnings = Vec::new();
        // A BTreeMap both sorts by id and gives last-write-wins for a repeated
        // id; `Vec::dedup_by` would instead keep the *first*, silently reversing
        // the documented precedence.
        let mut by_id = std::collections::BTreeMap::new();
        for (name, bytes) in pairs {
            let (meta, mut warns) = Self::import_bytes(&name, &bytes)?;
            warnings.append(&mut warns);
            by_id.insert(meta.id.0.clone(), meta);
        }
        Ok(Self {
            weapons: by_id.into_values().collect(),
            warnings,
        })
    }

    pub fn weapons(&self) -> &[WeaponMeta] {
        &self.weapons
    }
    pub fn len(&self) -> usize {
        self.weapons.len()
    }
    pub fn is_empty(&self) -> bool {
        self.weapons.is_empty()
    }
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }
    pub fn get(&self, id: &str) -> Option<&WeaponMeta> {
        self.weapons.iter().find(|w| w.id.0 == id)
    }
    /// Every definition whose metadata was imported but is not playable here.
    pub fn unsupported(&self) -> impl Iterator<Item = &WeaponMeta> {
        self.weapons.iter().filter(|w| w.has_unsupported())
    }
    /// The ids this catalog expects [`crate::gun`] to answer for. A catalog with
    /// an entry marked `Ported` that `gun()` rejects is inconsistent; callers
    /// should surface that rather than assume playability.
    pub fn ported_ids(&self) -> Vec<&str> {
        self.weapons
            .iter()
            .filter(|w| w.status == Status::Ported)
            .map(|w| w.id.0.as_str())
            .collect()
    }
}

/// A structured, non-fatal import note.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    pub id: String,
    pub field: &'static str,
    pub reason: UnsupportedReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    Json(String),
}
impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "invalid gun JSON: {e}"),
        }
    }
}
impl std::error::Error for ImportError {}

/// Ids that currently have live gameplay. Kept in sync with [`crate::gun`] and
/// asserted by tests; changing gameplay coverage means changing this list too.
pub const PORTED: [&str; 2] = [crate::AK47, crate::GLOCK17];

fn parse_weapon(name: &str, value: &Value) -> (WeaponMeta, Vec<Warning>) {
    let id = GunId::from_name(name);
    let mut warnings = Vec::new();
    let mut unsupported = Vec::new();

    let magazine_levels = magazine_levels(value);
    let magazine = magazine_levels.first().copied();
    let ammo = parse_ammo(value.get("AmmoType"), &id, &mut warnings);
    let (default_fire_mode, fire_modes) = parse_fire_modes(value, &id, &mut warnings);
    let category = value
        .get("GunType")
        .and_then(Value::as_str)
        .map(Category::parse)
        .unwrap_or(Category::Other);

    // A launcher or energy weapon carries extra semantics we do not simulate.
    // Each gap is recorded both on the record and as a warning so an importer
    // can act on it without re-parsing the source.
    let mut note = |field: &'static str, reason: UnsupportedReason| {
        unsupported.push(reason.clone());
        warnings.push(Warning {
            id: id.0.clone(),
            field,
            reason,
        });
    };
    if let Some(v) = value.get("Projectile") {
        if !v.is_null() {
            note(
                "Projectile",
                UnsupportedReason::ComplexAmmo("custom projectile".into()),
            );
        }
    }
    if value
        .get("ExplosionDamage")
        .and_then(Value::as_f64)
        .is_some_and(|n| n != 0.0)
    {
        note(
            "ExplosionDamage",
            UnsupportedReason::ComplexAmmo("explosive damage".into()),
        );
    }
    if value.get("ChargeAction").is_some() {
        note("ChargeAction", UnsupportedReason::EnergyAmmo);
    }
    // Carry the ammo-class gap onto the record's unsupported list as well, so
    // callers reading only `WeaponMeta` still see energy/free/mixed ammo.
    match &ammo {
        AmmoClass::Energy => note("AmmoType", UnsupportedReason::EnergyAmmo),
        AmmoClass::Infinite => note("AmmoType", UnsupportedReason::FreeAmmo),
        AmmoClass::Unsupported(r) => note("AmmoType", r.clone()),
        _ => {}
    }
    // Lift any fire-mode gaps recorded during parsing onto the record too, so a
    // caller reading only `WeaponMeta` still sees them. Done after `note` is
    // dropped so the shared read of `warnings` does not overlap its mutable
    // borrow.
    for w in &warnings {
        if let UnsupportedReason::UnknownFireMode(_) = &w.reason {
            if matches!(w.field, "AvailableFireModes" | "DefaultFireMode") {
                unsupported.push(w.reason.clone());
            }
        }
    }
    unsupported.sort_by_key(|r| format!("{r:?}"));
    unsupported.dedup();
    warnings.dedup();

    let status = if PORTED.contains(&id.0.as_str()) {
        Status::Ported
    } else {
        Status::Cataloged
    };

    let meta = WeaponMeta {
        id,
        category,
        status,
        ammo,
        magazine,
        magazine_levels,
        damage: f32_of(value, "Damage"),
        velocity: f32_of(value, "Velocity"),
        rpm: value.get("RPM").and_then(Value::as_u64).map(|n| n as u32),
        default_fire_mode,
        fire_modes,
        normal_reload: u16_of(value, "NormalReloadTime"),
        empty_reload: u16_of(value, "EmptyReloadTime"),
        unsupported,
    };
    (meta, warnings)
}

/// All magazine levels ascending. Accepts `"Magazine": [30,40,70]` or a plain
/// integer. Zero is filtered out because it names budget/backpack ammo.
fn magazine_levels(value: &Value) -> Vec<u32> {
    let mut levels: Vec<u32> = match value.get("Magazine") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_u64)
            .map(|n| n as u32)
            .collect(),
        Some(Value::Number(n)) => n.as_u64().map(|n| vec![n as u32]).unwrap_or_default(),
        _ => Vec::new(),
    };
    levels.retain(|n| *n > 0);
    levels.sort_unstable();
    levels.dedup();
    levels
}

fn parse_ammo(value: Option<&Value>, id: &GunId, warnings: &mut Vec<Warning>) -> AmmoClass {
    let token = match value {
        None | Some(Value::Null) => {
            return unsupported(id, "AmmoType", UnsupportedReason::MissingAmmo, warnings);
        }
        // A list: take the first entry, note the mix if any entry is unmodelled.
        Some(Value::Array(items)) => match items.first() {
            Some(first) => {
                if items.len() > 1 {
                    warnings.push(Warning {
                        id: id.0.clone(),
                        field: "AmmoType",
                        reason: UnsupportedReason::MixedAmmo,
                    });
                }
                first
            }
            None => return unsupported(id, "AmmoType", UnsupportedReason::MissingAmmo, warnings),
        },
        Some(other) => other,
    };

    // Object ammo consumer: {"Ammo": <token> | [token, ...]}.
    let token = match token {
        Value::Object(map) => match map.get("Ammo") {
            Some(Value::Array(list)) => match list.first() {
                Some(t) => t,
                None => {
                    return unsupported(id, "AmmoType", UnsupportedReason::MissingAmmo, warnings);
                }
            },
            Some(t) => t,
            None => return unsupported(id, "AmmoType", UnsupportedReason::MissingAmmo, warnings),
        },
        t => t,
    };

    match token {
        Value::String(s) => classify_token(s.trim(), id, warnings),
        Value::Array(list) => match list.first() {
            Some(Value::String(s)) => classify_token(s.trim(), id, warnings),
            _ => unsupported(id, "AmmoType", UnsupportedReason::MissingAmmo, warnings),
        },
        _ => unsupported(id, "AmmoType", UnsupportedReason::MissingAmmo, warnings),
    }
}

/// Classify one bare ammo token string.
fn classify_token(token: &str, id: &GunId, warnings: &mut Vec<Warning>) -> AmmoClass {
    if let Some(family) = AmmoFamily::parse(token) {
        return AmmoClass::Bullet(family);
    }
    match token {
        "FE" => AmmoClass::Energy,
        "Infinity" => AmmoClass::Infinite,
        // A concrete item id we can name, even if we do not simulate it.
        s if s.contains(':') => AmmoClass::Item(s.to_owned()),
        // An unknown `@` shorthand.
        s if s.starts_with('@') => unsupported(
            id,
            "AmmoType",
            UnsupportedReason::ComplexAmmo(s.to_owned()),
            warnings,
        ),
        s => unsupported(
            id,
            "AmmoType",
            UnsupportedReason::UnknownItem(s.to_owned()),
            warnings,
        ),
    }
}

/// Record a non-fatal gap: build the descriptor and log a structured warning.
fn unsupported(
    id: &GunId,
    field: &'static str,
    reason: UnsupportedReason,
    warnings: &mut Vec<Warning>,
) -> AmmoClass {
    warnings.push(Warning {
        id: id.0.clone(),
        field,
        reason: reason.clone(),
    });
    AmmoClass::Unsupported(reason)
}

/// Parse `AvailableFireModes` / `DefaultFireMode`. Tokens outside the modelled
/// `Semi`/`Auto`/`Burst` set (e.g. `Charge`) are **not** silently coerced: each
/// is reported to `warnings` and the unaffected modelled modes are kept. The
/// returned default always names a mode in the returned list.
fn parse_fire_modes(
    value: &Value,
    id: &GunId,
    warnings: &mut Vec<Warning>,
) -> (FireMode, Vec<FireMode>) {
    let mut modes: Vec<FireMode> = Vec::new();
    if let Some(items) = value.get("AvailableFireModes").and_then(Value::as_array) {
        for item in items {
            let Some(token) = item.as_str() else { continue };
            match FireMode::parse(token) {
                Some(m) => modes.push(m),
                None => warnings.push(Warning {
                    id: id.0.clone(),
                    field: "AvailableFireModes",
                    reason: UnsupportedReason::UnknownFireMode(token.to_owned()),
                }),
            }
        }
    }
    modes.sort();
    modes.dedup();
    let declared_default = value.get("DefaultFireMode").and_then(Value::as_str);
    let default = match declared_default.and_then(FireMode::parse) {
        Some(m) => {
            if !modes.contains(&m) {
                modes.push(m);
                modes.sort();
            }
            m
        }
        None => {
            if let Some(token) = declared_default {
                warnings.push(Warning {
                    id: id.0.clone(),
                    field: "DefaultFireMode",
                    reason: UnsupportedReason::UnknownFireMode(token.to_owned()),
                });
            }
            modes.iter().copied().next().unwrap_or(FireMode::Semi)
        }
    };
    if modes.is_empty() {
        modes.push(default);
    }
    (default, modes)
}

fn f32_of(value: &Value, key: &str) -> Option<f32> {
    value.get(key).and_then(Value::as_f64).map(|n| n as f32)
}

/// A tick count that may be either a bare number or a per-magazine-level array
/// (`"NormalReloadTime": [48, 48, 56]`, one entry per `Magazine` level). We take
/// the first entry — the base level the live gameplay uses — and never silently
/// drop a declared value. Non-finite, negative, and non-numeric entries yield
/// `None` so an absent value is distinguishable from a zero one.
fn u16_of(value: &Value, key: &str) -> Option<u16> {
    let n = match value.get(key) {
        Some(Value::Array(items)) => items.first().and_then(Value::as_f64),
        Some(other) => other.as_f64(),
        None => None,
    }?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    Some(n.min(f64::from(u16::MAX)) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn import(name: &str, v: Value) -> WeaponMeta {
        let (meta, _) = Catalog::import_value(name, &v);
        meta
    }

    /// Bytes of a real definition from the original source tree.
    fn source(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../../src/main/resources/data/superbwarfare/sbw/guns/{name}.json"
        ));
        std::fs::read(path).unwrap()
    }

    #[test]
    fn ak_47_metadata_matches_live_gameplay_values() {
        let (meta, _) = Catalog::import_bytes("ak_47", &source("ak_47")).unwrap();
        let live = crate::gun(crate::AK47).unwrap();
        assert_eq!(meta.id.0, live.id);
        assert_eq!(meta.status, Status::Ported);
        assert!(meta.is_playable());
        assert_eq!(meta.magazine, Some(live.capacity));
        assert_eq!(meta.damage, Some(live.damage));
        assert_eq!(meta.velocity, Some(live.speed));
        assert_eq!(meta.normal_reload, Some(live.reload));
        assert_eq!(meta.empty_reload, Some(live.empty_reload));
        assert_eq!(meta.default_fire_mode, FireMode::Auto);
        assert_eq!(meta.ammo, AmmoClass::Bullet(AmmoFamily::Rifle));
        assert_eq!(meta.ammo_family(), Some(AmmoFamily::Rifle));
        // A ported gun is fully modelled: no unsupported gaps were needed.
        assert!(!meta.has_unsupported(), "{:?}", meta.unsupported);
        assert_eq!(meta.fire_modes, vec![FireMode::Semi, FireMode::Auto]);
    }

    #[test]
    fn real_charged_fire_mode_is_flagged_in_the_tree() {
        // `reforging` is the live example of an unmodelled `Charge` fire mode.
        let (meta, _) = Catalog::import_bytes("reforging", &source("reforging")).unwrap();
        assert!(
            meta.unsupported
                .iter()
                .any(|r| matches!(r, UnsupportedReason::UnknownFireMode(m) if m == "Charge"))
        );
        assert!(!meta.is_playable());
    }

    #[test]
    fn glock_17_metadata_matches_live_gameplay_values() {
        let (meta, _) = Catalog::import_bytes("glock_17", &source("glock_17")).unwrap();
        let live = crate::gun(crate::GLOCK17).unwrap();
        assert_eq!(meta.magazine, Some(live.capacity));
        assert_eq!(meta.damage, Some(live.damage));
        assert_eq!(meta.velocity, Some(live.speed));
        assert_eq!(meta.default_fire_mode, FireMode::Semi);
        assert_eq!(meta.ammo, AmmoClass::Bullet(AmmoFamily::Handgun));
        assert_eq!(meta.interval_ticks(), Some(live.interval));
        // Reload times arrive as per-level arrays in the source; the base entry
        // must match the live value, not silently vanish to `None`.
        assert_eq!(meta.normal_reload, Some(live.reload));
        assert_eq!(meta.empty_reload, Some(live.empty_reload));
        assert!(!meta.has_unsupported());
    }

    #[test]
    fn per_level_array_reload_times_take_the_base_entry() {
        // The real AK-47 / Glock sources use `[48, 48, 56]` style arrays.
        let meta = import(
            "m_4",
            json!({"NormalReloadTime":[52,52,61],"EmptyReloadTime":[56,56,65]}),
        );
        assert_eq!(meta.normal_reload, Some(52));
        assert_eq!(meta.empty_reload, Some(56));
        // A bare number still works, and an absent value stays absent.
        assert_eq!(
            import("x", json!({"NormalReloadTime":30})).normal_reload,
            Some(30)
        );
        assert_eq!(import("x", json!({})).normal_reload, None);
        // A nonsensical entry does not fabricate a value.
        assert_eq!(
            import("x", json!({"NormalReloadTime":-4})).normal_reload,
            None
        );
        assert_eq!(
            import("x", json!({"NormalReloadTime":[]})).normal_reload,
            None
        );
    }

    #[test]
    fn unknown_fire_modes_are_reported_not_coerced() {
        // `reforging` declares `"DefaultFireMode": "Charge"`, which this port
        // does not model. It must surface as a gap, not silently become Semi.
        let (meta, warnings) = Catalog::import_value(
            "reforging",
            &json!({"DefaultFireMode":"Charge","AvailableFireModes":["Charge"]}),
        );
        assert!(
            meta.unsupported
                .iter()
                .any(|r| matches!(r, UnsupportedReason::UnknownFireMode(m) if m == "Charge"))
        );
        assert!(
            warnings.iter().any(
                |w| matches!(&w.reason, UnsupportedReason::UnknownFireMode(m) if m == "Charge")
            )
        );
        // With no modelled mode available the default falls back to Semi so the
        // record stays well-formed, but the gap is still recorded.
        assert_eq!(meta.default_fire_mode, FireMode::Semi);
        assert_eq!(meta.fire_modes, vec![FireMode::Semi]);
    }

    #[test]
    fn known_fire_modes_survive_an_unknown_sibling() {
        let meta = import(
            "mixed",
            json!({"DefaultFireMode":"Auto","AvailableFireModes":["Semi","Charge","Auto"]}),
        );
        assert_eq!(meta.default_fire_mode, FireMode::Auto);
        assert_eq!(meta.fire_modes, vec![FireMode::Semi, FireMode::Auto]);
        assert!(meta.has_unsupported());
    }

    #[test]
    fn only_ported_guns_are_playable_and_port_set_matches_gun() {
        for name in PORTED {
            let bare = GunId(name.to_owned());
            let (meta, _) = Catalog::import_bytes(bare.name(), &source(bare.name())).unwrap();
            assert_eq!(meta.status, Status::Ported);
            assert!(crate::gun(name).is_some());
            assert_eq!(meta.id.0, name);
        }
        // A catalog-only weapon is explicitly not playable.
        let meta = import("m_4", json!({"GunType":"Rifle","AmmoType":"@RifleAmmo"}));
        assert_eq!(meta.status, Status::Cataloged);
        assert!(!meta.is_playable());
    }

    #[test]
    fn energy_ammo_is_explicitly_unsupported_not_coerced() {
        let meta = import(
            "devotion",
            json!({"GunType":"MachineGun","AmmoType":"FE","Magazine":[40,60,80],"MaxEnergy":100000}),
        );
        assert_eq!(meta.ammo, AmmoClass::Energy);
        assert!(meta.unsupported.contains(&UnsupportedReason::EnergyAmmo));
        assert!(meta.has_unsupported());
        assert_eq!(meta.ammo_family(), None);
    }

    #[test]
    fn infinite_and_item_ammo_are_classified_distinctly() {
        let melee = import("knife", json!({"GunType":"Special","AmmoType":"Infinity"}));
        assert_eq!(melee.ammo, AmmoClass::Infinite);
        assert!(melee.unsupported.contains(&UnsupportedReason::FreeAmmo));
        let rocket = import(
            "rpg",
            json!({"GunType":"DirectLauncher","AmmoType":["superbwarfare:rpg_rocket_tbg"]}),
        );
        assert_eq!(
            rocket.ammo,
            AmmoClass::Item("superbwarfare:rpg_rocket_tbg".into())
        );
    }

    #[test]
    fn missing_ammo_is_reported_not_defaulted() {
        let meta = import("mystery", json!({"GunType":"Rifle","Damage":4}));
        assert_eq!(
            meta.ammo,
            AmmoClass::Unsupported(UnsupportedReason::MissingAmmo)
        );
        assert!(meta.unsupported.contains(&UnsupportedReason::MissingAmmo));
    }

    #[test]
    fn rpm_converts_to_bounded_server_cadence() {
        let meta = import("hk_416", json!({"RPM":600}));
        assert_eq!(meta.interval_ticks(), Some(2));
        let slow = import("rpg", json!({"RPM":120}));
        assert_eq!(slow.interval_ticks(), Some(10));
        let none = import("none", json!({}));
        assert_eq!(none.interval_ticks(), None);
    }

    #[test]
    fn magazine_levels_are_sorted_unique_and_drop_zero() {
        let meta = import("m_60", json!({"Magazine":0}));
        assert_eq!(meta.magazine, None);
        assert!(meta.magazine_levels.is_empty());
        let multi = import("ak_47", json!({"Magazine":[30,40,70]}));
        assert_eq!(multi.magazine, Some(30));
        assert_eq!(multi.magazine_levels, vec![30, 40, 70]);
    }

    #[test]
    fn catalog_is_order_independent_and_deduplicated() {
        let a = (
            String::from("m_4"),
            br#"{"GunType":"Rifle","AmmoType":"@RifleAmmo"}"#.to_vec(),
        );
        let b = (
            String::from("ak_47"),
            br#"{"GunType":"Rifle","AmmoType":"@RifleAmmo"}"#.to_vec(),
        );
        let c = (
            String::from("m_4"),
            br#"{"GunType":"Smg","AmmoType":"@HandgunAmmo"}"#.to_vec(),
        );
        let first = Catalog::from_entries(vec![a.clone(), b.clone()]).unwrap();
        let second = Catalog::from_entries(vec![b, a, c]).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 2);
        // Last write wins for the duplicate id.
        assert_eq!(
            second.get("superbwarfare:m_4").unwrap().category,
            Category::Smg
        );
        assert!(first.get("superbwarfare:ak_47").is_some());
        assert!(first.get("superbwarfare:m_4").is_some());
    }

    #[test]
    fn catalog_output_is_sorted_by_id_and_entry_order_never_flips_it() {
        let a = (
            String::from("m_4"),
            br#"{"GunType":"Rifle","AmmoType":"@RifleAmmo"}"#.to_vec(),
        );
        let b = (
            String::from("ak_47"),
            br#"{"GunType":"Rifle","AmmoType":"@RifleAmmo"}"#.to_vec(),
        );
        let c = (
            String::from("m_4"),
            br#"{"GunType":"Smg","AmmoType":"@HandgunAmmo"}"#.to_vec(),
        );
        let forward = Catalog::from_entries(vec![a.clone(), b.clone(), c.clone()]).unwrap();
        let reverse = Catalog::from_entries(vec![c, b, a]).unwrap();
        // Same tree -> same catalog regardless of enumeration order.
        assert_eq!(forward.weapons(), reverse.weapons());
        // Sorted ascending by id, and the duplicate resolved to the last write.
        let ids: Vec<&str> = forward.weapons().iter().map(|w| w.id.0.as_str()).collect();
        assert_eq!(ids, vec!["superbwarfare:ak_47", "superbwarfare:m_4"]);
        assert_eq!(
            forward.get("superbwarfare:m_4").unwrap().category,
            Category::Smg
        );
    }

    #[test]
    fn malformed_json_is_an_error_not_a_silent_record() {
        assert!(Catalog::import_bytes("bad", b"{not json").is_err());
        assert!(Catalog::import_bytes("bad", b"").is_err());
    }

    #[test]
    fn catalog_reads_the_real_original_gun_tree() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../src/main/resources/data/superbwarfare/sbw/guns");
        let mut entries = Vec::new();
        for dir in std::fs::read_dir(&root).unwrap() {
            let path = dir.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
            entries.push((name, std::fs::read(&path).unwrap()));
        }
        let catalog = Catalog::from_entries(entries).unwrap();
        // The real tree is large; a regression that stops importing most of it
        // must fail loudly.
        assert!(catalog.len() >= 60, "imported only {}", catalog.len());
        // Ported entries stay playable; everything else is metadata only.
        assert_eq!(catalog.ported_ids(), vec![crate::AK47, crate::GLOCK17]);
        assert!(catalog.get(crate::AK47).unwrap().is_playable());
        assert!(!catalog.get("superbwarfare:rpg").unwrap().is_playable());
        assert!(catalog.get("superbwarfare:rpg").unwrap().has_unsupported());
        // At least one energy weapon and one infinite-ammo weapon are flagged.
        assert!(catalog.unsupported().any(|w| w.ammo == AmmoClass::Energy));
        assert!(catalog.unsupported().any(|w| w.ammo == AmmoClass::Infinite));
        // Every cataloged entry carries a namespaced id.
        assert!(catalog.weapons().iter().all(|w| w.id.0.contains(':')));
        // The real AK-47 source stores reload times as a per-level array; the
        // base entry must survive import rather than silently become `None`.
        assert_eq!(catalog.get(crate::AK47).unwrap().normal_reload, Some(48));
        assert_eq!(catalog.get(crate::AK47).unwrap().empty_reload, Some(64));
        // The real tree contains at least one unmodelled fire mode (`Charge`).
        assert!(catalog.weapons().iter().any(|w| {
            w.unsupported
                .iter()
                .any(|r| matches!(r, UnsupportedReason::UnknownFireMode(_)))
        }));
    }

    #[test]
    fn warnings_capture_unsupported_reasons_by_field() {
        let (_, warnings) =
            Catalog::import_value("devotion", &json!({"GunType":"MachineGun","AmmoType":"FE"}));
        assert!(warnings.iter().all(|w| !w.field.is_empty()));
    }
}
