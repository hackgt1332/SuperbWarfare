//! Negotiation and session guards, independent of transport and simulation.

use crate::{
    PROTOCOL, TICK_RATE, limits,
    types::*,
    validate::{self, Validate, ValidationError},
};

/// Shared by client and server; input capability declarations are not permissions.
#[derive(Debug)]
pub struct ServerPolicy {
    content_sha256: [u8; 32],
    required_features: Vec<Feature>,
}
impl ServerPolicy {
    pub fn new(
        content_sha256: [u8; 32],
        required_features: Vec<Feature>,
    ) -> Result<Self, ValidationError> {
        validate::features(&required_features)?;
        Ok(Self {
            content_sha256,
            required_features,
        })
    }

    /// Call only after decoding a Hello from the authenticated connection.
    /// The host supplies a fresh token; constructing a Welcome does not start gameplay.
    pub fn negotiate(
        &self,
        hello: &Hello,
        session: Uuid,
        server_tick: u64,
    ) -> Result<Welcome, RejectReason> {
        hello.validate().map_err(|_| RejectReason::InvalidInput)?;
        session.validate().map_err(|_| RejectReason::InvalidInput)?;
        // Pre-1.0 minor changes can break the ABI; exact matching is intentional.
        if hello.protocol != PROTOCOL {
            return Err(RejectReason::IncompatibleVersion);
        }
        if hello.content_sha256 != self.content_sha256 {
            return Err(RejectReason::ContentMismatch);
        }
        if !self
            .required_features
            .iter()
            .all(|f| hello.features.contains(f))
        {
            return Err(RejectReason::MissingFeature);
        }
        Ok(Welcome {
            protocol: PROTOCOL,
            session,
            server_tick,
            tick_rate: TICK_RATE,
            content_sha256: self.content_sha256.to_vec(),
            required_features: self.required_features.clone(),
        })
    }
}

/// Client verifies the Welcome against its own Hello before accepting any state.
pub fn verify_welcome(hello: &Hello, welcome: &Welcome) -> Result<(), RejectReason> {
    hello.validate().map_err(|_| RejectReason::InvalidInput)?;
    welcome.validate().map_err(|_| RejectReason::InvalidInput)?;
    if hello.protocol != PROTOCOL || welcome.protocol != PROTOCOL {
        return Err(RejectReason::IncompatibleVersion);
    }
    if hello.content_sha256 != welcome.content_sha256 {
        return Err(RejectReason::ContentMismatch);
    }
    if !welcome
        .required_features
        .iter()
        .all(|f| hello.features.contains(f))
    {
        return Err(RejectReason::MissingFeature);
    }
    Ok(())
}

/// One per authenticated player connection, created ONLY after negotiation.
/// Run before gameplay checks; even a gameplay-rejected intent consumes its
/// sequence. Never infer the player's identity from this token alone.
#[derive(Debug)]
pub struct InputGate {
    session: Uuid,
    last_sequence: Option<u64>,
    tick: Option<u64>,
    count: usize,
}
impl InputGate {
    pub fn new(session: Uuid) -> Result<Self, ValidationError> {
        session.validate()?;
        Ok(Self {
            session,
            last_sequence: None,
            tick: None,
            count: 0,
        })
    }
    pub fn last_sequence(&self) -> Option<u64> {
        self.last_sequence
    }

    /// Uses authoritative server time. Errors never change gate state.
    pub fn accept(&mut self, input: &Input, server_tick: u64) -> Result<(), RejectReason> {
        input.validate().map_err(|_| RejectReason::InvalidInput)?;
        if input.session != self.session {
            return Err(RejectReason::WrongSession);
        }
        if self
            .last_sequence
            .is_some_and(|last| input.sequence <= last)
        {
            return Err(RejectReason::ReplayedInput);
        }
        if self.tick.is_some_and(|last| server_tick < last) {
            return Err(RejectReason::InvalidInput);
        }
        let count = if self.tick == Some(server_tick) {
            self.count
        } else {
            0
        };
        if count >= limits::INPUTS_PER_TICK {
            return Err(RejectReason::RateLimited);
        }
        self.last_sequence = Some(input.sequence);
        self.tick = Some(server_tick);
        self.count = count + 1;
        Ok(())
    }
}

/// Guards full replacement snapshots; allocate a new guard on every Welcome.
#[derive(Debug)]
pub struct SnapshotGate {
    session: Uuid,
    tick: Option<u64>,
    acknowledged: Option<u64>,
}
impl SnapshotGate {
    pub fn new(session: Uuid) -> Result<Self, ValidationError> {
        session.validate()?;
        Ok(Self {
            session,
            tick: None,
            acknowledged: None,
        })
    }
    pub fn accept(&mut self, snapshot: &Snapshot) -> Result<(), RejectReason> {
        snapshot
            .validate()
            .map_err(|_| RejectReason::InvalidInput)?;
        if snapshot.session != self.session {
            return Err(RejectReason::WrongSession);
        }
        if self.tick.is_some_and(|last| snapshot.server_tick <= last) {
            return Err(RejectReason::ReplayedInput);
        }
        if self
            .acknowledged
            .is_some_and(|last| snapshot.acknowledged_input.is_none_or(|v| v < last))
        {
            return Err(RejectReason::InvalidInput);
        }
        self.tick = Some(snapshot.server_tick);
        self.acknowledged = snapshot.acknowledged_input;
        Ok(())
    }
}

#[derive(Debug)]
struct PendingSnapshot {
    tick: u64,
    acknowledged: Option<u64>,
    pages: Vec<Option<SnapshotPage>>,
    bytes: usize,
    weapons: usize,
    entities: usize,
}

/// Bounded assembly: at most one pending tick and 32 pages. Returns a complete
/// snapshot only once; no caller-visible mutation is needed for partial pages.
#[derive(Debug)]
pub struct SnapshotAssembler {
    gate: SnapshotGate,
    pending: Option<PendingSnapshot>,
}
impl SnapshotAssembler {
    pub fn new(session: Uuid) -> Result<Self, ValidationError> {
        Ok(Self {
            gate: SnapshotGate::new(session)?,
            pending: None,
        })
    }
    pub fn push(&mut self, page: SnapshotPage) -> Result<Option<Snapshot>, RejectReason> {
        page.validate().map_err(|_| RejectReason::InvalidInput)?;
        if page.session != self.gate.session {
            return Err(RejectReason::WrongSession);
        }
        if self.gate.tick.is_some_and(|tick| page.server_tick <= tick)
            || self
                .pending
                .as_ref()
                .is_some_and(|v| page.server_tick < v.tick)
        {
            return Err(RejectReason::ReplayedInput);
        }
        if self
            .gate
            .acknowledged
            .is_some_and(|last| page.acknowledged_input.is_none_or(|v| v < last))
        {
            return Err(RejectReason::InvalidInput);
        }
        let bytes = crate::wire::encode_server(&ServerPacket::Snapshot(page.clone()))
            .map_err(|_| RejectReason::InvalidInput)?
            .len();
        if self
            .pending
            .as_ref()
            .is_none_or(|v| page.server_tick > v.tick)
        {
            self.pending = Some(PendingSnapshot {
                tick: page.server_tick,
                acknowledged: page.acknowledged_input,
                pages: vec![None; usize::from(page.pages)],
                bytes: 0,
                weapons: 0,
                entities: 0,
            });
        }
        let pending = self
            .pending
            .as_mut()
            .expect("pending snapshot initialized above");
        if pending.pages.len() != usize::from(page.pages)
            || pending.acknowledged != page.acknowledged_input
        {
            return Err(RejectReason::InvalidInput);
        }
        let index = usize::from(page.page);
        if pending.pages[index].is_some() {
            return Err(RejectReason::ReplayedInput);
        }
        if pending.bytes + bytes > limits::SNAPSHOT_BYTES
            || pending.weapons + page.weapons.len() > limits::WEAPONS
            || pending.entities + page.entities.len() > limits::TRACKED_ENTITIES
        {
            return Err(RejectReason::InvalidInput);
        }
        pending.bytes += bytes;
        pending.weapons += page.weapons.len();
        pending.entities += page.entities.len();
        pending.pages[index] = Some(page);
        if pending.pages.iter().any(Option::is_none) {
            return Ok(None);
        }
        let pending = self
            .pending
            .take()
            .expect("pending snapshot initialized above");
        let mut snapshot = Snapshot {
            session: self.gate.session,
            server_tick: pending.tick,
            acknowledged_input: pending.acknowledged,
            weapons: Vec::new(),
            entities: Vec::new(),
        };
        for part in pending.pages.into_iter().flatten() {
            snapshot.weapons.extend(part.weapons);
            snapshot.entities.extend(part.entities);
        }
        self.gate.accept(&snapshot)?;
        Ok(Some(snapshot))
    }
}

/// Presentation events are monotonic per recipient. A rejected batch does not
/// consume IDs; a new session needs a new gate, just as snapshots do.
#[derive(Debug)]
pub struct EffectGate {
    session: Uuid,
    last: u64,
}
impl EffectGate {
    pub fn new(session: Uuid) -> Result<Self, ValidationError> {
        session.validate()?;
        Ok(Self { session, last: 0 })
    }
    pub fn accept(&mut self, batch: &Effects) -> Result<(), RejectReason> {
        batch.validate().map_err(|_| RejectReason::InvalidInput)?;
        if batch.session != self.session {
            return Err(RejectReason::WrongSession);
        }
        if batch
            .events
            .first()
            .is_some_and(|event| event.id <= self.last)
        {
            return Err(RejectReason::ReplayedInput);
        }
        if let Some(last) = batch.events.last() {
            self.last = last.id;
        }
        Ok(())
    }
}
