# SuperbWarfare for Horizon: guns and ground vehicles

This Rust/WASM port implements **AK-47**, **Glock 17**, ammunition, and a drivable
**Sodayo pickup and cargo truck prototypes**. It requires the bundled Horizon engine patch.

Guns have persistent per-item identity/ammunition, timed reloads, server-enforced
cadence, automatic/semi-automatic modes, aimed spread, input expiry, HUD ammo,
zoom and recoil feedback. Reloads atomically reserve ammunition; shots atomically
update the held gun and create a native projectile. Clients send button intents,
never damage, hit reports, magazine counts or aim positions. Item swaps, stale
state, replayed packets, mismatched content and reconnects are checked.

Guns use converted, textured 3D models with a smooth iron-sight transition,
whole-weapon recoil and a reload progress bar. Both vehicles use converted original
textured geometry with animated wheels, one driver seat, acceleration, braking
before reverse, steering, gravity, block/vehicle collision, health, projectile
damage, native passengers and chunk saves. Deployment checks solid upward ground,
support across the footprint and chassis clearance before consuming the item. The server moves it from authenticated movement
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

The engine changes are maintained locally on `sbw/mod-network`.
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

Use your launcher's prepared Minecraft 26.3 resources if available. Otherwise,
install/play your own Minecraft 26.3 client once and obtain the matching server
jar. With JDK 25 available, prepare them from the Horizon checkout:

```sh
target/debug/horizon-prepare --root "$PWD" --prepare --server-jar "/path/to/26.3-server.jar"
```

For a nonstandard launcher directory, append `--minecraft-dir "/path/to/minecraft"`.
The tool reads your own installation and writes `.local/horizon-client/26.3`.
Then launch an isolated test world from the Horizon directory:

```sh
scratch=$(mktemp -d)
engine="$PWD"
mkdir -p "$scratch/game"
(
cd "$scratch/game"
HORIZON_HOME="$scratch/game" HORIZON_LAUNCHER_HOME="$scratch/launcher" \
XDG_DATA_HOME="$scratch/data" XDG_CONFIG_HOME="$scratch/config" \
HORIZON_LAUNCHER_NO_SELF_UPDATE=1 HORIZON_NO_AUDIO=1 \
"$engine/target/debug/horizon-client" --world SuperbWarfareTest --creative \
  --resources "$engine/.local/horizon-client/26.3" \
  --dev-mod "$engine/../SuperbWarfare/rust/target/horizon-bridge/superbwarfare-client.hmod" \
  --dev-mod "$engine/../SuperbWarfare/rust/target/horizon-bridge/superbwarfare-server.hmod"
)
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
complete arsenal. The models are static conversions with cosmetic wheel steering/rolling
and whole-weapon pose animation; original bone reload/fire animations, hands and
attachments are not yet ported. Native arrow ballistics/rendering and damage rules stand
in for custom bullets. Headshot multipliers, custom penetration, gun animations,
attachments, sounds, fuel, vehicle weapons, multiple seats, aircraft, machines,
and the remaining guns/vehicles are still outstanding. Reload cancellation is
not implemented. Vehicle model snapshots currently cover up to eight tracked
vehicles within a 10 KiB state budget per recipient dimension.

Transport is bounded: 16 KiB packets, eight sends per hook, 64 queued requests,
32 accepted inputs per player/server tick, connection-bound tokens and full
replacement snapshots. Triggers and aim expire after ten ticks without a heartbeat. Re-equipping a gun
changes its input generation, so a delayed click cannot affect the new equip.
The content digest covers all simulation modules, vehicle tuning and the WIT schema.
The engine's APIs are experimental level 10, not an upstream compatibility claim.

The standalone simulation crate also contains tested attachment modifiers, ballistic
penetration/ricochet rules, turret authorization/cadence, and fuel/repair transaction
planning. These rule modules are foundations for later host integration; their
presence does not enable those features in a running world. The catalog importer
reads the wider original arsenal as metadata, while only AK-47 and Glock 17 are
playable. See [CATALOG.md](tools/CATALOG.md).

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
round trips, vehicle profile validation, ground placement, steering/reverse,
client correction, foreign texture atlases and bounded held poses. Isolated live
Metal rendering checks exercised the 3D guns, aiming, reloading, automatic fire,
pickup deployment, mounting and driving on the default server-thread path.
Multiplayer/LAN feel and original animation parity remain to be checked.

Code is GPL-3.0-only. The reused gun/vehicle model and texture art remains under the original
SuperbWarfare asset license; it is packaged only with this mod, never in Horizon.

Central validation covers the contract/simulation workspace, real-WASM bridge and
converter tests, and native client library/binary tests. The engine-wide self-check
still reports pre-existing Clippy warnings in unrelated code; targeted checks
record those separately from changed-code findings.
