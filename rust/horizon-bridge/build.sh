#!/bin/sh
# Run from any directory. Player packages contain Rust/WASM and mod-owned art.
set -eu
bridge=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
output="$bridge/../target/horizon-bridge"
cargo=${CARGO:-cargo}
"$cargo" build --manifest-path "$bridge/Cargo.toml" --locked --target wasm32-unknown-unknown --target-dir "$output" -p sbw-horizon-client -p sbw-horizon-server
for side in client server; do
    "$cargo" run --manifest-path "$bridge/Cargo.toml" --locked -p sbw-horizon-tools -- "$bridge/$side/horizon-mod.toml" "$output/wasm32-unknown-unknown/debug/sbw_horizon_$side.wasm" "$output/superbwarfare-$side.hmod"
done
