# Gun model conversion

`convert-guns` deterministically converts the repository's original AK-47 and
Glock 17 Bedrock rest-pose geometry into Horizon's native 26.3 cuboid item models.
The generated JSON and original, byte-identical texture copies live under
`client/assets/superbwarfare/`. Keep the original asset license and credits:
this conversion changes their representation, not their ownership or license.

```sh
cargo run --manifest-path rust/horizon-bridge/Cargo.toml -p sbw-horizon-tools --bin convert-guns
cargo run --manifest-path rust/horizon-bridge/Cargo.toml -p sbw-horizon-tools --bin convert-guns -- --check
cargo test --manifest-path rust/horizon-bridge/Cargo.toml -p sbw-horizon-tools --bin convert-guns
```

The native packaging tool remains the package's default executable; the model
converter always has an explicit `--bin convert-guns` argument. `--check` fails
when source geometry or textures no longer reproduce the checked-in artifacts.

## Representation

- Source: `models/bedrock/gun/{ak_47,glock_17}.geo.json` and
  `textures/bedrock/gun/{ak_47,glock_17}.png` in the original mod resources.
- Standard configuration only: OEM standard stock/handguard and base magazine.
  Alternative stock/magazine/handguard subtrees, chamber bullets and the original
  arm placeholders are omitted. Other receiver/barrel/sight/rail geometry remains.
- AK-47: 372 cuboids. Glock 17: 195 cuboids. Per-face UVs and omitted faces are
  preserved. Source atlas dimensions are used even though both PNGs are 256².
- Cubes are flattened through their complete bone hierarchy. Bone and cube
  pivots are in the original model coordinates; rotations apply X, then Y, then Z.
  The source assets use the opposite signed rotation from native cuboid elements;
  their X/Y/Z angles are negated before composing. A source Glock frame detail
  regression and a textured static preview check this convention.
  Each cube is emitted with its original extents and a rotation about its
  transformed center. Cubes retain their source bone name for inspection.
- The grip origin becomes model `[8,8,8]`, +Y is up, and the muzzle points -Z.
  AK model coordinates are scaled by 0.5 to stay inside the native element range;
  Glock coordinates are retained at 1.0. Display-context scale is applied after
  that conversion. First-person rotations are neutral so runtime aiming/recoil
  can move the entire model without undoing a baked tool tilt.
- Explicit first/third person transforms exist for both hands, plus GUI, ground,
  and item frame transforms. The side-lit GUI shows the actual three-dimensional
  model. Glock's illuminated sight elements keep native light emission.

The geometric conventions follow the official [Bedrock geometry schema](https://learn.microsoft.com/en-us/minecraft/creator/reference/content/visualreference/geometry.v1.14.0).
The converter tests compare composed rotations with Horizon's real element
matrix, load and bake the generated models with its real model library, check
finite textured quads and held transforms, and verify texture byte equality.

## Limits

These are static standard-configuration meshes. The Forge bone animations,
attachment switching, shell geometry, custom arm poses and emissive texture pass
are not recreated here. Whole-model aiming/recoil is supplied by the runtime;
animated magazines, slide and bolt need a later pose/model API. Display transforms
are initial camera-space tuning and should be checked in the running game for
both handedness settings and supported field-of-view values.
