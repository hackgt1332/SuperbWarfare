# SuperbWarfare Rust contract

This workspace starts the Horizon port with a buildable contract, before gameplay.
The component ABI is `superbwarfare:contract@0.1.0` in
[`contract.wit`](crates/sbw-contract/wit/contract.wit). Rust data types, client imports,
server imports and guest export traits are generated from that file. There is no
second handwritten representation of the same messages.

**Status:** proposed adapter API, tested independently. Unmodified Horizon at
`56f968e11c7627536cb6a83420caddce7e3acb8f` cannot link these interfaces. This is not a
playable `.hmod` and does not change either game's behavior. The reference mod is
SuperbWarfare `d7b608fa9692d4373d5f9875c276eb871130e37d` (Forge 1.20.1); the target
engine is Horizon's Minecraft 26.3 implementation.

## Build and verify

From this directory, with Rust and Cargo on PATH:

```sh
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
cargo run -p sbw-contract --example handshake --locked
```

Default builds expose shared native types, validation, codecs and session guards.
`client-bindings` and `server-bindings` additionally expose the corresponding guest
traits/imports and `export_client!` / `export_server!`. The crate has no dependency
on Forge, Minecraft jars, Horizon internals, a JVM, graphics or an OS transport.
The lockfile is checked in. Generated ABI glue is the only unsafe-code exception.

To verify real WASM exports as well, install the `wasm32-unknown-unknown` Rust
target, build each linkage fixture with its own side enabled, and componentize it:

```sh
rustup target add wasm32-unknown-unknown
cargo build -p sbw-contract --example client_guest --features client-bindings --target wasm32-unknown-unknown --locked
cargo build -p sbw-contract --example server_guest --features server-bindings --target wasm32-unknown-unknown --locked
cargo run -p sbw-contract --example componentize --locked -- target/wasm32-unknown-unknown/debug/examples/client_guest.wasm
cargo run -p sbw-contract --example componentize --locked -- target/wasm32-unknown-unknown/debug/examples/server_guest.wasm
```

The fixtures export all guest callbacks but perform no gameplay. Componentization
validates their actual ABI. Native tooling can enable both binding features;
each real guest enables just its own side.

## Component boundary

| World | Host imports | Guest exports |
|---|---|---|
| `schema` | shared types only | none |
| `client` | item-scoped attack/use claims, send intent, held model, presentation effects | init, local input, receive, tick, disconnected |
| `server` | player/inventory reads, atomic inventory commits, send state, swept collision, entity lifecycle, damage | init, authenticated receive, leave, tick, entity load/unload |

This package specifies additional capabilities needed for the port. Existing
Horizon logging, HUD, settings and resource systems remain separate. A future
adapter must validate capability imports and link only the appropriate world.
Do not register a server capability in a client store. The schema world is for
shared native tooling and is not a runnable guest.

The initial scope is a networked firearm plus custom entity lifecycle and vehicle
control primitives. Machines, menus, energy interoperability, radar, arbitrary
world edits, chunk tickets and full vehicle simulation need later contracts.

The host must enforce these semantics:

- **Identity:** `receive(player, packet)` gets the authenticated player from the
  connection, never from payload contents. Session tokens are correlation and
  replay boundaries, not authentication. UUIDs persist across saves; entity keys
  do not. Runtime indices and generations start at 1 and are never reused with
  the same generation. Clear runtime state on world switch.
- **Input:** claim attack and use before vanilla acts, only for claimed held
  items. Use the host's stack UUID, selected slot and equip revision. `claim-items`
  replaces the previous claim list. Release held actions on item switch, screen
  open, focus loss, disconnect or mod failure. Reset claims on mod unload.
  The server player view supplies the selected slot, equip revision and runtime
  entity handle. Increase equip revision whenever selected slot or stack instance
  changes, including switching away and back; ammunition-only edits do not change
  it. Replicate these server-issued references before emitting matching local
  input. Client inventory prediction cannot invent a new accepted reference.
- **Authority:** client packets contain intent, not claimed hits, damage, shot
  count, charge power or vehicle position. Server time determines cadence,
  reloading and charge. The server checks spectator status, held stack identity,
  equip revision, ammunition, cooldowns, seat ownership and loaded world before
  applying an action. Target references are not permission grants.
- **Held actions:** trigger press/release is idempotent state, not one shot per
  packet. While held, refresh trigger/aim/vehicle state every 5 ticks; the server
  expires it after 10 ticks without refresh. An outdated release must clear the
  old instance's held state; it must never activate or modify a newly held weapon.
  A new equip revision invalidates old held state immediately.
- **Inventory:** read and compare-and-exchange a whole inventory revision. Validate
  every write, destination slot, stack identity, registered item and stack-size
  limit first; commit gun state and consumed ammo together or change nothing.
  Revisions never wrap. A failed commit returns `stale-revision` and the caller
  must reread; no spawn/damage may be committed speculatively before ammo commits.
- **Entities:** allocate handles on spawn/load, reject duplicate persistent UUIDs,
  and restrict writes to this mod's entities. `update-entity` replaces its complete
  transform and opaque state, increments its revision, and persists both. Unload
  saves state and removes the runtime handle; despawn removes the saved entity.
  Restore all records before the first simulation tick. Invalid handles or
  generations have no effects. Entity persistence errors return `unavailable`.
- **Collision/damage:** sweep loaded chunks only, return nearest hits first, and
  report unavailable queries instead of treating unloaded space as empty. Damage
  uses a registered damage type and host-owned vanilla rules. Verify source
  attribution and target visibility; a client cannot call either operation.
- **Presentation:** the host resolves registered model/animation/effect IDs and
  owns GPU/audio resources. No file paths, raw shader source or Java callbacks
  cross the ABI. A held model persists until replaced/cleared; effects are
  one-shot and dimension-filtered. Reset presentation on disconnect or failure.
- **Failure:** every mutation validates all arguments before effects. Every API
  call is bounded, synchronous and non-reentrant; event delivery is queued until
  the current guest call returns. On trap/unload, clear held actions and reject
  new gameplay input while retaining committed saves. No locks are held through
  guest calls.

## Negotiation and delivery

The transport is one reliable, ordered, connection-scoped mod channel in each
direction. The component ABI passes typed packets; `wire` defines the bytes used
by a native/network adapter. The adapter must run the same validation and size
limits even for integrated single-player calls that bypass a socket.

1. Start pending. Only one `Hello` is accepted before negotiation; do not dispatch
   gameplay or create an `InputGate` yet. Server sends no snapshots/effects yet.
2. Validate exact protocol version, gameplay-content SHA-256 and required
   features with `ServerPolicy::negotiate`. The host supplies a fresh UUID token
   for each authenticated player connection. Send `Welcome` or a rejection.
3. The client calls `verify_welcome` against its own Hello. A successful Welcome
   resets prediction, input numbering, snapshot assembly and effect IDs. The
   server may queue initial state immediately behind it on the ordered channel.
4. For each input, validate/decode, look up the authenticated connection's gate,
   then call `InputGate::accept` with the server tick. Only then run gameplay
   checks. Accepted-for-processing sequences are consumed even if gameplay later
   rejects a stale target or denies an action. Return a rejection with that input
   sequence, and include the last processed sequence in the next snapshot.
5. On leave/disconnect, destroy the gate and all held actions. Old session input
   cannot reactivate a new connection. Do not renegotiate an active connection.

Feature declarations report adapter support, not trust or operator permissions.
Before starting a world, check the host's implemented capabilities; the current
Horizon SDK alone cannot satisfy them. A normal gun requires `item-state`,
`attack-input`, `mod-channel`, `entities`, `collision`, `damage`, `world-models`,
and `world-persistence`; vehicle controls additionally require `vehicles`.

The gameplay-content digest is a 32-byte value supplied by the future content
builder. Both sides must hash the same effective authoritative definitions,
gameplay scripts and balance-affecting configuration, including override order.
Cosmetic-only settings need not match. The contract accepts the digest but does
not yet scan files or define a content bundle format; adapters must not invent
different digest rules. A change to effective gameplay content requires a new
session; hot-reloading it mid-session is outside version 0.1.

## State replication

Snapshots are **full replacements of this recipient's tracked set**, scoped to
one session and tick. They are not deltas or a list of every entity in the world.
Use `wire::snapshot_pages` to construct bounded pages and `SnapshotAssembler` to
apply them. The assembler returns no snapshot until every page is present and
the combined identities, sizes, acknowledgement and tick pass validation. Missing
entities in a completed snapshot are removed locally. Empty completed snapshots
are valid. A newer tick replaces an incomplete older tick, never the visible set.

All pages share session/tick/ack/page count. Duplicate pages, inconsistent metadata,
regressing ticks/acks, duplicate handles and duplicate persistent UUIDs are refused.
On a malformed snapshot, keep the last applied state and request no speculative
world mutation; the adapter should log and close a persistently invalid peer.
The host chooses interest sets. Over-limit sets return an error; silently dropping
entities from a replacement snapshot would falsely despawn them.

Entity-record bytes are opaque, explicitly schema-versioned mod state. The server
must send only the public replication representation to a recipient; private
inventory/AI/save data must not leak through these bytes. Persisted and replicated
schemas may be different nonzero IDs. Their codecs belong to future `sbw-sim`.
Weapon snapshots carry authoritative ammunition/reload/heat state separately.

Presentation effects have recipient-session-scoped monotonically increasing IDs.
Use `EffectGate` before presenting a batch. An acknowledged input is not a shot
confirmation; only authoritative state/effects establish what happened.

## Versions, units and bounds

The WIT package version (`0.1.0`), gameplay protocol (`0.1`), wire format (`1`),
opaque-state schema and future Horizon manifest API are independent. Do not claim
that a `0.3` Horizon manifest can load this contract. Pre-1.0 protocol versions
must match exactly. A wire shape, field order or enum order change needs a new
protocol version and updated golden fixtures; an incompatible frame codec change
also increments the format byte. Update the WIT package version with ABI changes.

Wire format 1 is `53 42 57 00`, format byte `01`, direction (`01` client / `02`
server), then one Postcard value. Reject trailing bytes, invalid discriminants,
truncation, wrong direction and oversized frames. Postcard serializes WIT-generated
Serde types in declaration order. Frozen fixtures guard this ordering.

Coordinates use blocks, velocities use blocks per tick, and simulation runs at
20 Hz. Axes follow Minecraft: +X east, +Y up, +Z south. Player yaw 0 faces south,
yaw +90 faces west, and positive pitch looks down. Quaternions rotate local
vectors into world coordinates with `q * v * inverse(q)`; model-local +Y is up
and +Z is forward. Vehicle axes are normalized intent: positive throttle forward,
strafe right, lift up, yaw right, pitch nose up, roll right wing down. Player
look uses degrees; quaternions use x/y/z/w and must be unit length
within squared-norm tolerance 0.001. Other floats must be finite. Host world and
gameplay bounds apply in addition to structural validation.

| Limit | Value |
|---|---:|
| Encoded packet, including header | 16 KiB |
| One opaque state blob | 4 KiB |
| Pending snapshot pages / encoded bytes | 32 / 512 KiB |
| Entities / weapons in a complete tracked snapshot | 64 / 9 |
| Effects per batch; collision hits / ignored entities | 64 each |
| Claimed items / inventory slots | 256 each |
| Resource identifier | 128 ASCII bytes |
| Accepted input per player per authoritative tick | 32 |
| Held input heartbeat / lease expiry | 5 / 10 ticks |

These are initial contract limits, not measured performance claims. Native adapters
reject oversize bytes before deserialization; component hosts must also enforce
guest memory/fuel and list limits at the ABI boundary. Validators provide shape
checks, not gameplay authorization. Opaque state size checks do not validate its
future schema. Unsupported schemas are rejected before loading/mutating state.

## Next integration step

Implement a Horizon host adapter for these two worlds, authenticated channel
routing, item claims and inventory transactions. Then wire a single AK-47 through
intent -> authoritative simulation -> paged snapshot/effects. Preserve the
standalone tests and add real integrated-server/LAN/dedicated-server checks when
the adapter exists. No Java compatibility bridge or asset redistribution is needed
to build and exercise this contract.
