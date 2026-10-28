# Horizon transport adapter

This is the first executable step after the contract. Two Rust/WASM components
load in Horizon's existing client and server sandboxes and exchange the
`sbw-contract` packets over an experimental, connection-bound `net` capability.
The channel runs through Horizon's real custom-payload handlers; in-frame single
player uses the same inboxes as loopback. There is no Java runtime or command-based
damage simulation.

**Implemented:** content/version negotiation, authenticated player routing,
host-generated per-connection tokens, replay rejection, bounded messages,
reconnect cleanup, and authoritative acknowledgements. Multiple requests in one
server tick produce one replacement snapshot per recipient. Queued replies and
deferred requests retain their connection identity across reconnects.

**Not implemented:** weapon simulation, item claims/state transactions, custom
entities, collision/damage, models, machines or asset conversion. The bridge
advertises only `mod-channel`. Every gameplay intent is explicitly rejected as
`not-ready`; acknowledging one means it was processed, never that a shot happened.
The content digest identifies this transport probe, not the future arsenal.

## Development layout

The target checkout must be a sibling named `horizon`, as provided for this task:

```text
code/
  horizon/
  SuperbWarfare/
```

This is a separate Cargo workspace because it depends on that patched checkout.
The root contract workspace continues to build on its own. The accompanying
`horizon-mod-network.patch` records the required engine changes against Horizon
`56f968e11c7627536cb6a83420caddce7e3acb8f`. Apply it only to a clean, unpatched target:

```sh
git -C ../horizon apply ../SuperbWarfare/rust/horizon-bridge/horizon-mod-network.patch
```

The native engine runtime enforces mod id, side, packet and queue limits. Server
sender identity comes from its authenticated connection. Session tokens are native
random values, not guest-generated timestamps. A token is a replay boundary, not
a replacement for authentication. Native queue overflow is refused; this probe
also bounds its deferred queue and logs overflow. It is not a throughput-tested
gameplay transport yet.

## Build packages

Run from the SuperbWarfare repository root with Rust/Cargo on PATH and the
`wasm32-unknown-unknown` target installed:

```sh
cargo build --manifest-path rust/horizon-bridge/Cargo.toml --locked --target wasm32-unknown-unknown --target-dir rust/target/horizon-bridge -p sbw-horizon-client -p sbw-horizon-server
cargo run --manifest-path rust/horizon-bridge/Cargo.toml --locked --target-dir rust/target/horizon-bridge -p sbw-horizon-tools -- rust/horizon-bridge/client/horizon-mod.toml rust/target/horizon-bridge/wasm32-unknown-unknown/debug/sbw_horizon_client.wasm rust/target/horizon-bridge/superbwarfare-client.hmod
cargo run --manifest-path rust/horizon-bridge/Cargo.toml --locked --target-dir rust/target/horizon-bridge -p sbw-horizon-tools -- rust/horizon-bridge/server/horizon-mod.toml rust/target/horizon-bridge/wasm32-unknown-unknown/debug/sbw_horizon_server.wasm rust/target/horizon-bridge/superbwarfare-server.hmod
```

Build Horizon with `--features mods`. Its `--dev-mod PATH` option can load the
client and server packages; repeat it for both in integrated single player. A
dedicated server needs its server package, and joining clients need the client
package. These have the same mod id and intentionally different execution sides.
Use prepared resources from your own Minecraft installation to run the game.

The client negotiates when its server advertises channel support. `/sbw_status`
reports readiness and the latest acknowledged sequence. `/sbw_probe` sends a
diagnostic trigger-release intent with a synthetic weapon reference; the server
rejects it as not-ready and acknowledges processing it. It does not select,
spawn, consume ammunition or fire a weapon.

## Verification

```sh
cargo test --manifest-path rust/horizon-bridge/Cargo.toml --locked --target-dir rust/target/horizon-bridge -p sbw-horizon-tools --test bridge
cargo clippy --manifest-path rust/horizon-bridge/Cargo.toml --locked --target-dir rust/target/horizon-bridge --workspace --all-targets -- -D warnings
```

The integration tests compile both real WASM guests, package them as `.hmod`s,
and load them in Horizon's real runtime. They exercise negotiation, acknowledgements,
replay/version/content rejection, burst coalescing and same-player reconnects.
They require no Minecraft resources. They do not launch the game or replace
integrated-server/LAN/dedicated-server play testing.

Validation against the pinned engine base: 2,344 Horizon workspace tests, 21
contract tests and 5 real-WASM bridge tests passed. Formatting and targeted
Clippy checks passed. Horizon's full self-check remains blocked by 12 existing
Clippy warnings in untouched `horizon-world` files. Vanilla parity and live play
tests require prepared Minecraft resources, which were unavailable here.

The next implementation step is item-specific attack/use ownership and atomic
inventory transactions, followed by one server-authoritative firearm.
