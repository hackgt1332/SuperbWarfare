//! Contract limits apply at both the wire and component boundary.
pub const PACKET_BYTES: usize = 16 * 1024;
pub const RESOURCE_ID_BYTES: usize = 128;
pub const STATE_BYTES: usize = 4096;
pub const FEATURES: usize = 9;
pub const INPUTS_PER_TICK: usize = 32;
pub const WEAPONS: usize = 9;
pub const TRACKED_ENTITIES: usize = 64;
pub const SNAPSHOT_PAGES: usize = 32;
pub const SNAPSHOT_BYTES: usize = SNAPSHOT_PAGES * PACKET_BYTES;
pub const EFFECTS: usize = 64;
pub const CLAIMED_ITEMS: usize = 256;
pub const INVENTORY_SLOTS: usize = 256;
pub const SWEEP_IGNORED: usize = 64;
pub const SWEEP_HITS: usize = 64;
/// Held trigger/vehicle controls must be refreshed; expiry releases them.
pub const INPUT_LEASE_TICKS: u64 = 10;
pub const INPUT_HEARTBEAT_TICKS: u64 = 5;
