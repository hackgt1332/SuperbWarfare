# SuperbWarfare for Horizon: first gameplay slice

This Rust/WASM port implements **AK-47**, **Glock 17**, ammunition, and a drivable
**Sodayo pickup and cargo truck prototypes**. It requires the bundled Horizon engine patch.

Guns have persistent per-item identity/ammunition, timed reloads, server-enforced
cadence, automatic/semi-automatic modes, aimed spread, input expiry, HUD ammo,
zoom and recoil feedback. Reloads atomically reserve ammunition; shots atomically
update the held gun and create a native projectile. Clients send button intents,
never damage, hit reports, magazine counts or aim positions. Item swaps, stale
state, replayed packets, mismatched content and reconnects are checked.

Both vehicles have a shaded cuboid prototype model, one driver seat, acceleration,
steering, gravity, block/vehicle collision, health, projectile damage, native
passengers and chunk saves. The server moves it from authenticated movement
inputs and ignores client vehicle position packets. Item state, vehicle profile,
and vehicle damage survive native saves. Native tracking uses the boat wire type;
the matching client component supplies the vehicle appearance.

## Build

Keep this repository and the Horizon checkout next to each other. Rust 1.95+
and `wasm32-unknown-unknown` are required. From the SuperbWarfare root:

```sh
rustup target add wasm32-unknown-unknown
# On a clean Horizon checkout at 56f968e11c7627536cb6a83420caddce7e3acb8f:
git -C ../horizon apply --check ../SuperbWarfare/rust/horizon-bridge/horizon-mod-network.patch
git -C ../horizon apply ../SuperbWarfare/rust/horizon-bridge/horizon-mod-network.patch
cargo build --manifest-path ../horizon/Cargo.toml --locked -p horizon-client --features mods --bin horizon-client
./rust/horizon-bridge/build.sh
```

The engine changes are committed locally at `d93cd7d74` on `sbw/mod-network`.
The local `/Users/dan/code/horizon` checkout already contains the patch; skip both
`git apply` commands there. The filename covers both the networking foundation
and the new gameplay support. Build outputs:

- `rust/target/horizon-bridge/superbwarfare-client.hmod` (code, items, art, recipes)
- `rust/target/horizon-bridge/superbwarfare-server.hmod` (server rules)

Both packages belong on the world host so its content tables match the client.
Joining clients need the client package. `--dev-mod` can be repeated; the components
share a mod id and have different execution sides. Enable the mods feature in the
engine build. Minecraft 26.3 resources must be prepared from your own installation;
no Mojang files are included here.

## Play

The prepared resources are currently missing from this checkout. Install/play your
own Minecraft 26.3 client once and obtain the matching server jar. With JDK 25
available, prepare them from `/Users/dan/code/horizon`:

```sh
target/debug/horizon-prepare --root "$PWD" --prepare --server-jar "/path/to/26.3-server.jar"
```

For a nonstandard launcher directory, append `--minecraft-dir "/path/to/minecraft"`.
The tool reads your own installation and writes `.local/horizon-client/26.3`.
Then launch an isolated test world from the Horizon directory:

```sh
HORIZON_HOME="$PWD/.local/sbw-test" target/debug/horizon-client --world SuperbWarfareTest --creative \
  --resources .local/horizon-client/26.3 \
  --dev-mod ../SuperbWarfare/rust/target/horizon-bridge/superbwarfare-client.hmod \
  --dev-mod ../SuperbWarfare/rust/target/horizon-bridge/superbwarfare-server.hmod
```

Create a separate test world. In chat, use `/sbw_status` to check negotiation,
then `/sbw_loadout` for the two guns, 64 rounds of each ammo type, and pickup and truck items.
The loadout uses ordinary `/give` commands and requires cheats/operator rights.
Guns start empty: select one and press **R** before firing.

| Action | Control |
| --- | --- |
| Fire | Left mouse; hold for AK automatic fire |
| Aim | Hold right mouse |
| Reload | R |
| AK auto/semi mode | V |
| Place a vehicle | Hold its item and right-click open, level ground |
| Enter a vehicle | Right-click the vehicle with an empty hand |
| Drive / steer | W/S and A/D |
| Exit | Shift |

The reload/mode keys can be rebound in Horizon's mod controls. A reload reserves
rounds once, pauses while the gun is put away, and resumes when reselected.
`/sbw_probe` remains a networking diagnostic; it cannot fire a gun.

Survival recipes are provisional: rifle ammo uses an iron nugget, copper ingot
and gunpowder (16 rounds); handgun ammo uses an iron nugget and gunpowder (16).
The guns and vehicles also have crafting recipes in the client package. These
recipes provide access while the original machines/materials have not been ported.

## Current limits

This is an initial implementation of two guns and two ground vehicles, not the
complete arsenal. Weapon display uses the mod's item icons; the vehicles use a
simple new cuboid model. Native arrow ballistics/rendering and damage rules stand
in for custom bullets. Headshot multipliers, custom penetration, gun animations,
attachments, sounds, fuel, vehicle weapons, multiple seats, aircraft, machines,
and the remaining guns/vehicles are still outstanding. Reload cancellation is
not implemented. Vehicle model snapshots currently cover up to eight tracked
vehicles within a 10 KiB state budget per recipient dimension.

Transport is bounded: 16 KiB packets, eight sends per hook, 64 queued requests,
32 accepted inputs per player/server tick, connection-bound tokens and full
replacement snapshots. Triggers and aim expire after ten ticks without a heartbeat. Re-equipping a gun
changes its input generation, so a delayed click cannot affect the new equip.
The content digest covers simulation code, vehicle tuning and the WIT schema.
The engine's APIs are experimental level 9, not an upstream compatibility claim.

## Verify

```sh
cargo test --manifest-path rust/Cargo.toml --workspace --all-features --locked
cargo test --manifest-path rust/horizon-bridge/Cargo.toml --locked -p sbw-horizon-tools --test bridge
cargo clippy --manifest-path rust/horizon-bridge/Cargo.toml --workspace --all-targets --locked -- -D warnings
```

The bridge tests compile and execute the real client/server WASM components in
Horizon's runtime. They exercise negotiation, content/version rejection, replay,
reconnects, burst snapshots, timed reload reservation, shot edits and selected-slot
vehicle deployment. Native engine tests cover inventory CAS, saved component
round trips and vehicle profile validation. These do not replace live play tests:
this workspace lacks the prepared Minecraft 26.3 resources required to launch a
world, so rendering, movement feel and LAN gameplay remain unverified in game.

Code is GPL-3.0-only. The reused item/vehicle icon art remains under the original
SuperbWarfare asset license; it is packaged only with this mod, never in Horizon.

Final validation: the full engine workspace passed 2,349 tests before the review
fixes; the affected crates and rebuilt executable were checked again afterward.
The mod workspace and ten real-WASM bridge tests pass, with bridge Clippy clean.
The engine's full self-check still reports pre-existing Clippy warnings in
untouched world/mob/dragon code; the changed crates also pass targeted Clippy with
those existing warning categories allowed. Three parallel reviews covered gameplay,
engine integration, and launch prerequisites. No live game was launched.
