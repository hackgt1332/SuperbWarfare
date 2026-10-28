//! Shape validation shared by wire codecs and typed component adapters.
//! Ownership, permissions, registry membership and revisions require host state.

use crate::{TICK_RATE, limits, types::*};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError(pub &'static str);

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid or over-limit {}", self.0)
    }
}
impl std::error::Error for ValidationError {}

pub trait Validate {
    fn validate(&self) -> Result<(), ValidationError>;
}

fn require(valid: bool, field: &'static str) -> Result<(), ValidationError> {
    if valid {
        Ok(())
    } else {
        Err(ValidationError(field))
    }
}

fn unique<T: PartialEq>(values: &[T]) -> bool {
    values
        .iter()
        .enumerate()
        .all(|(i, v)| !values[..i].contains(v))
}

/// Minecraft-style identifiers, with no empty, `.` or `..` path segments.
pub fn resource_id(value: &str) -> Result<(), ValidationError> {
    let valid = value.len() <= limits::RESOURCE_ID_BYTES
        && value.split_once(':').is_some_and(|(ns, path)| {
            !ns.is_empty()
                && ns
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
                && path.split('/').all(|part| {
                    !part.is_empty()
                        && part != "."
                        && part != ".."
                        && part.bytes().all(|b| {
                            b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b)
                        })
                })
        });
    require(valid, "resource identifier")
}

pub fn features(values: &[Feature]) -> Result<(), ValidationError> {
    require(
        values.len() <= limits::FEATURES && unique(values),
        "features",
    )
}

impl Validate for Uuid {
    fn validate(&self) -> Result<(), ValidationError> {
        require(self.high != 0 || self.low != 0, "UUID")
    }
}
impl Validate for EntityKey {
    fn validate(&self) -> Result<(), ValidationError> {
        require(self.index != 0 && self.generation != 0, "entity key")
    }
}
impl Validate for Vec3 {
    fn validate(&self) -> Result<(), ValidationError> {
        require(
            self.x.is_finite() && self.y.is_finite() && self.z.is_finite(),
            "vector",
        )
    }
}
impl Validate for Rotation {
    fn validate(&self) -> Result<(), ValidationError> {
        let norm: f64 = [self.x, self.y, self.z, self.w]
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum();
        require(
            norm.is_finite() && (norm - 1.0).abs() <= 0.001,
            "unit quaternion",
        )
    }
}
impl Validate for Transform {
    fn validate(&self) -> Result<(), ValidationError> {
        self.position.validate()?;
        self.orientation.validate()?;
        self.velocity.validate()
    }
}
impl Validate for Hello {
    fn validate(&self) -> Result<(), ValidationError> {
        require(self.content_sha256.len() == 32, "content SHA-256")?;
        features(&self.features)
    }
}
impl Validate for Welcome {
    fn validate(&self) -> Result<(), ValidationError> {
        self.session.validate()?;
        require(self.tick_rate == TICK_RATE, "tick rate")?;
        require(self.content_sha256.len() == 32, "content SHA-256")?;
        features(&self.required_features)
    }
}
impl Validate for WeaponRef {
    fn validate(&self) -> Result<(), ValidationError> {
        self.instance.validate()?;
        require(usize::from(self.slot) < limits::WEAPONS, "weapon slot")
    }
}
impl Validate for VehicleControls {
    fn validate(&self) -> Result<(), ValidationError> {
        require(
            [
                self.throttle,
                self.strafe,
                self.lift,
                self.yaw,
                self.pitch,
                self.roll,
            ]
            .iter()
            .all(|v| v.is_finite() && (-1.0..=1.0).contains(v)),
            "control axis",
        )
    }
}
impl Validate for Input {
    fn validate(&self) -> Result<(), ValidationError> {
        self.session.validate()?;
        require(self.sequence > 0, "input sequence")?;
        match &self.intent {
            Intent::Weapon(v) => v.target.validate(),
            Intent::Vehicle(v) => {
                v.target.validate()?;
                if let VehicleAction::Controls(axes) = &v.action {
                    axes.validate()?;
                }
                Ok(())
            }
        }
    }
}
impl Validate for ClientPacket {
    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Hello(v) => v.validate(),
            Self::Input(v) => v.validate(),
        }
    }
}
impl Validate for WeaponState {
    fn validate(&self) -> Result<(), ValidationError> {
        self.target.validate()?;
        resource_id(&self.definition)?;
        require(self.heat.is_finite() && self.heat >= 0.0, "weapon heat")
    }
}

fn state(schema: u16, bytes: &[u8]) -> Result<(), ValidationError> {
    require(
        schema != 0 && bytes.len() <= limits::STATE_BYTES,
        "versioned state",
    )
}
impl Validate for EntityRecord {
    fn validate(&self) -> Result<(), ValidationError> {
        self.instance.validate()?;
        resource_id(&self.definition)?;
        resource_id(&self.dimension)?;
        self.pose.validate()?;
        state(self.state_schema, &self.state)
    }
}
impl Validate for EntityState {
    fn validate(&self) -> Result<(), ValidationError> {
        self.key.validate()?;
        self.entity.validate()
    }
}
impl Validate for Snapshot {
    fn validate(&self) -> Result<(), ValidationError> {
        snapshot_fields(
            &self.session,
            self.acknowledged_input,
            &self.weapons,
            &self.entities,
        )
    }
}
fn snapshot_fields(
    session: &Uuid,
    acknowledged: Option<u64>,
    weapons: &[WeaponState],
    entities: &[EntityState],
) -> Result<(), ValidationError> {
    session.validate()?;
    require(acknowledged != Some(0), "acknowledged sequence")?;
    require(weapons.len() <= limits::WEAPONS, "weapons")?;
    require(
        entities.len() <= limits::TRACKED_ENTITIES,
        "tracked entities",
    )?;
    for (i, weapon) in weapons.iter().enumerate() {
        weapon.validate()?;
        require(
            !weapons[..i].iter().any(|v| {
                v.target.slot == weapon.target.slot || v.target.instance == weapon.target.instance
            }),
            "duplicate weapon",
        )?;
    }
    for (i, entity) in entities.iter().enumerate() {
        entity.validate()?;
        require(
            !entities[..i]
                .iter()
                .any(|v| v.key == entity.key || v.entity.instance == entity.entity.instance),
            "duplicate entity",
        )?;
    }
    Ok(())
}
impl Validate for SnapshotPage {
    fn validate(&self) -> Result<(), ValidationError> {
        require(
            self.pages > 0
                && usize::from(self.pages) <= limits::SNAPSHOT_PAGES
                && self.page < self.pages,
            "snapshot page",
        )?;
        snapshot_fields(
            &self.session,
            self.acknowledged_input,
            &self.weapons,
            &self.entities,
        )
    }
}
impl Validate for Effect {
    fn validate(&self) -> Result<(), ValidationError> {
        require(self.id > 0, "effect ID")?;
        resource_id(&self.resource_id)?;
        resource_id(&self.dimension)?;
        self.position.validate()?;
        self.direction.validate()?;
        if let Some(key) = &self.source {
            key.validate()?;
        }
        Ok(())
    }
}
pub fn effects(events: &[Effect]) -> Result<(), ValidationError> {
    require(events.len() <= limits::EFFECTS, "effects")?;
    let mut last = 0;
    for event in events {
        event.validate()?;
        require(event.id > last, "effect order")?;
        last = event.id;
    }
    Ok(())
}
impl Validate for Effects {
    fn validate(&self) -> Result<(), ValidationError> {
        self.session.validate()?;
        effects(&self.events)
    }
}
impl Validate for ServerPacket {
    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Welcome(v) => v.validate(),
            Self::Rejected(v) => require(v.sequence != Some(0), "rejected sequence"),
            Self::Snapshot(v) => v.validate(),
            Self::Effects(v) => v.validate(),
        }
    }
}
impl Validate for Stack {
    fn validate(&self) -> Result<(), ValidationError> {
        self.instance.validate()?;
        resource_id(&self.item)?;
        require(self.count > 0, "stack count")?;
        state(self.state_schema, &self.state)
    }
}
fn slots(values: &[Slot]) -> Result<(), ValidationError> {
    require(values.len() <= limits::INVENTORY_SLOTS, "inventory slots")?;
    for (i, slot) in values.iter().enumerate() {
        require(
            usize::from(slot.index) < limits::INVENTORY_SLOTS
                && !values[..i].iter().any(|v| v.index == slot.index),
            "slot index",
        )?;
        if let Some(stack) = &slot.value {
            stack.validate()?;
            require(
                !values[..i]
                    .iter()
                    .filter_map(|v| v.value.as_ref())
                    .any(|v| v.instance == stack.instance),
                "duplicate stack UUID",
            )?;
        }
    }
    Ok(())
}
impl Validate for Inventory {
    fn validate(&self) -> Result<(), ValidationError> {
        slots(&self.slots)
    }
}
impl Validate for InventoryEdit {
    fn validate(&self) -> Result<(), ValidationError> {
        require(!self.writes.is_empty(), "empty inventory edit")?;
        slots(&self.writes)
    }
}
impl Validate for Player {
    fn validate(&self) -> Result<(), ValidationError> {
        self.id.validate()?;
        self.entity.validate()?;
        require(
            usize::from(self.selected_slot) < limits::WEAPONS,
            "selected slot",
        )?;
        resource_id(&self.dimension)?;
        self.position.validate()?;
        require(
            self.yaw.is_finite() && self.pitch.is_finite() && (-90.0..=90.0).contains(&self.pitch),
            "player look",
        )
    }
}
impl Validate for Sweep {
    fn validate(&self) -> Result<(), ValidationError> {
        resource_id(&self.dimension)?;
        self.start.validate()?;
        self.end.validate()?;
        require(
            self.radius.is_finite() && self.radius >= 0.0,
            "sweep radius",
        )?;
        require(
            self.ignored.len() <= limits::SWEEP_IGNORED && unique(&self.ignored),
            "ignored entities",
        )?;
        for key in &self.ignored {
            key.validate()?;
        }
        Ok(())
    }
}
impl Validate for Hit {
    fn validate(&self) -> Result<(), ValidationError> {
        if let HitTarget::Entity(key) = &self.target {
            key.validate()?;
        }
        self.position.validate()?;
        self.normal.validate()?;
        require(
            self.fraction.is_finite() && (0.0..=1.0).contains(&self.fraction),
            "hit fraction",
        )
    }
}
pub fn hits(values: &[Hit]) -> Result<(), ValidationError> {
    require(values.len() <= limits::SWEEP_HITS, "sweep hits")?;
    let mut last = 0.0;
    for hit in values {
        hit.validate()?;
        require(hit.fraction >= last, "hit order")?;
        last = hit.fraction;
    }
    Ok(())
}
impl Validate for DamageRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        self.target.validate()?;
        self.source.validate()?;
        resource_id(&self.damage_type)?;
        require(
            self.amount.is_finite() && self.amount > 0.0,
            "damage amount",
        )
    }
}
impl Validate for LocalInput {
    fn validate(&self) -> Result<(), ValidationError> {
        self.target.validate()
    }
}
impl Validate for HeldModel {
    fn validate(&self) -> Result<(), ValidationError> {
        self.instance.validate()?;
        resource_id(&self.model)?;
        resource_id(&self.animation)?;
        require(
            self.animation_seconds.is_finite()
                && self.animation_seconds >= 0.0
                && self.aim_fraction.is_finite()
                && (0.0..=1.0).contains(&self.aim_fraction),
            "model animation",
        )
    }
}
pub fn claimed_items(items: &[String]) -> Result<(), ValidationError> {
    require(
        items.len() <= limits::CLAIMED_ITEMS && unique(items),
        "claimed items",
    )?;
    for item in items {
        resource_id(item)?;
    }
    Ok(())
}
