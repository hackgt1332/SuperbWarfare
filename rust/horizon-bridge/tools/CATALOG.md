# Weapon & Ammo Catalog

Typed, validated metadata for the **whole** original SuperbWarfare arsenal, plus a
deterministic importer that renders it to a stable JSON report.

This document describes what the catalog *is*, what it is deliberately *not*, how
to regenerate the report, and which original semantics remain unmodelled.

## What it is, and what it is not

* **It is a catalog.** Every gun definition under
  `src/main/resources/data/superbwarfare/sbw/guns/*.json` is read into a
  `sbw_sim::catalog::WeaponMeta` record: id, category, ammo family, magazine
  sizes, damage, velocity, RPM, fire modes, reload times, and an explicit list of
  semantics this port does not implement.
* **It is not a claim of playability.** Exactly two ids currently have live
  gameplay in Horizon — `superbwarfare:ak_47` and `superbwarfare:glock_17` —
  and those are the only ids for which `sbw_sim::gun` returns a `Gun`. A record
  whose `status` is `Cataloged` has had its metadata imported and validated; it
  does **not** shoot, reload, or exist in any inventory. Do not translate
  "present in the catalog" into "playable", and do not translate "models/items
  exist" into "playable" either. Only `sbw_sim::gun` answers that question, and
  today it answers for two ids.
* **Unsupported semantics are explicit, not coerced.** Energy (`FE`),
  infinite/no-ammo (`Infinity`), bespoke item rounds, and unparsed `@`-shorthands
  are surfaced as `AmmoClass::Energy` / `Infinite` / `Item(_)` / `Unsupported(_)`
  and mirrored into `WeaponMeta::unsupported` and `Catalog::warnings`. Nothing is
  silently downgraded to "a bullet".

## Library surface (`sbw_sim::catalog`)

Loading is **pure** — it takes bytes and returns records. No filesystem, no clock,
no RNG, so identical input yields byte-identical output.

| Item | Purpose |
| --- | --- |
| `GunId` | Namespaced id (`superbwarfare:ak_47`); `from_name` adds the namespace, `name` strips it. |
| `AmmoClass` | `Bullet(AmmoFamily)` \| `Energy` \| `Infinite` \| `Item(String)` \| `Unsupported(UnsupportedReason)`. |
| `AmmoFamily` | `Rifle`, `Handgun`, `Shotgun`, `Sniper`, `Heavy`; maps to the consumable item id via `item()`. |
| `UnsupportedReason` | Why a value could not be modelled (see below). |
| `FireMode` / `Category` | Parsed `AvailableFireModes` / `GunType`. |
| `Status` | `Ported` (live in `sbw_sim::gun`) or `Cataloged` (metadata only). |
| `WeaponMeta` | One validated record. `interval_ticks()` converts RPM to a 20 Hz server cadence; `has_unsupported()` answers whether any gap was recorded. |
| `Catalog` | Ordered collection. `from_entries` sorts and de-duplicates by id (last wins), so input order never affects output. |
| `Warning` | `{ id, field, reason }` — a structured, non-fatal import note. |
| `ImportError` | Malformed JSON only; unknown fields are never an error. |

The catalog is carried into the crate via `pub mod catalog;` in
`rust/crates/sbw-sim/src/lib.rs`. `catalog::PORTED` is kept in lockstep with
`crate::gun`, and tests assert the ported set is exactly the ids `gun()` answers
for.

## Unsupported semantics

These are recorded explicitly so an importer never has to pretend a value exists:

| Reason | Source | Example |
| --- | --- | --- |
| `EnergyAmmo` | `"AmmoType": "FE"`, or a `ChargeAction` | `devotion` (fuel-fed machine gun) |
| `FreeAmmo` | `"AmmoType": "Infinity"` | `knife`, `repair_tool`, hammers, batons |
| `UnknownItem` | a non-`@` ammo id with no catalogued item | — |
| `ComplexAmmo` | an unparsed `@`-shorthand, custom `Projectile`, or `ExplosionDamage` | `rpg` (`superbwarfare:rpg_rocket_*`, explosion) |
| `MissingAmmo` | `AmmoType` absent entirely | — |
| `MixedAmmo` | an array/multi ammo list including at least one unmodelled entry | `rpg` (TBG + standard rockets) |
| `UnknownFireMode` | a `DefaultFireMode`/`AvailableFireModes` token outside `Semi`/`Auto`/`Burst` | `reforging` (`Charge`) |

A weapon that declares `Projectile` (non-null) or non-zero `ExplosionDamage`
also carries a `ComplexAmmo` note, because projectiles and explosives are not
simulated by this port even when the metadata imports cleanly.

### How the common shapes are read

* **Reload times** may be a bare number or a per-magazine-level array
  (`"NormalReloadTime": [48, 48, 56]`). The catalog takes the **first** entry —
  the base level the live gameplay uses. This is what keeps AK-47/Glock 17
  metadata equal to the live `sbw_sim::gun` values; a regression here is caught
  by the ported-value tests.
* **Magazine** may be a bare number or an ascending array; levels are sorted,
  de-duplicated, and zero (budget/backpack ammo) is dropped.
* **Ammo** may be a bare string, an array (first entry used, a mix flagged), or
  an object `{ "Ammo": <token> | [token, ...] }` as the RPG/GP-25 use.
* **Fire modes** are read from the top level only. Unmodelled tokens are reported
  as `UnknownFireMode` and the modelled siblings are kept; when no modelled mode
  exists the default falls back to `Semi` so the record stays well-formed, with
  the gap still recorded.

## Importer tool (`convert-catalog`)

`rust/horizon-bridge/tools/src/bin/convert-catalog.rs` is a **development-only**
binary. The writing game never loads it. It:

1. Walks `data/superbwarfare/sbw/guns/*.json` in the repository (path overridable
   by a positional root argument),
2. imports every definition through `sbw_sim::catalog::Catalog::from_entries`,
3. renders a stable JSON report (sorted object keys, fixed key order), and
4. writes it, or verifies it with `--check`.

```
# regenerate the report next to the tool crate
cargo run -p sbw-horizon-tools --bin convert-catalog

# verify an existing report has not drifted (fails on any difference)
cargo run -p sbw-horizon-tools --bin convert-catalog -- --check

# point at a different checkout / write elsewhere
cargo run -p sbw-horizon-tools --bin convert-catalog -- /path/to/repo -o out.json
```

Output is metadata only. A `"status": "ported"` entry means `sbw_sim::gun`
answers for that id; it is not a statement that the weapon is playable in
Horizon.

## Determinism

The tree is read in sorted name order, `Catalog::from_entries` sorts by id and
de-duplicates, and all JSON is emitted with a fixed key order and a trailing
newline. The same source tree therefore produces byte-identical output, which is
what makes `--check` a meaningful CI gate.

## Tests

All tests below are written but **NOT RUN** in this coding pass — the shared
checkout runs one validation after the wave. They live in the modules they cover.

`rust/crates/sbw-sim/src/catalog.rs`:

* `ak_47_metadata_matches_live_gameplay_values` / `glock_17_metadata_matches_live_gameplay_values`
  — imported stats equal the live `sbw_sim::gun` values (capacity, damage,
  velocity, reload, fire mode, ammo family, cadence). This is the guard that the
  catalog did not drift from gameplay.
* `only_ported_guns_are_playable_and_port_set_matches_gun` — every `Ported` id is
  answered by `gun()`; a catalog-only weapon is explicitly not playable.
* `energy_ammo_is_explicitly_unsupported_not_coerced`,
  `infinite_and_item_ammo_are_classified_distinctly`,
  `missing_ammo_is_reported_not_defaulted` — unsupported families are named, not
  coerced.
* `rpm_converts_to_bounded_server_cadence`,
  `magazine_levels_are_sorted_unique_and_drop_zero`,
  `catalog_is_order_independent_and_deduplicated`,
  `catalog_output_is_sorted_by_id_and_entry_order_never_flips_it`,
  `malformed_json_is_an_error_not_a_silent_record`.
* `per_level_array_reload_times_take_the_base_entry` — array/`number`/absent/`[]`
  and negative inputs.
* `unknown_fire_modes_are_reported_not_coerced` and
  `known_fire_modes_survive_an_unknown_sibling`.
* `real_charged_fire_mode_is_flagged_in_the_tree` — `reforging` carries the real
  `Charge` gap.
* `catalog_reads_the_real_original_gun_tree` — imports the real tree (asserts
  `>= 60` definitions), checks the ported set, that `rpg` is present but not
  playable and flagged, that energy + infinite families both appear, that AK-47's
  array reload times import, and that some unmodelled fire mode exists.
* `warnings_capture_unsupported_reasons_by_field`.

`rust/horizon-bridge/tools/src/bin/convert-catalog.rs`:

* `real_tree_imports_and_renders_deterministically` — two runs over the same tree
  render identically.
* `render_is_newline_terminated_and_pretty`.
* `ported_entries_render_as_ported_and_report_is_metadata_only`.
* `report_carries_array_reload_times_and_fire_mode_gaps`.
* `missing_tree_is_an_error`.

## Limits

* Only two weapons are playable; the rest are metadata.
* Projectiles, explosions, energy draw, spread/recoil, attachments, perks, and
  melee damage are **not** modelled — they appear only as `unsupported` notes.
* Magazine/damage/velocity/reload values are read from the base definition;
  attachment- and perk-driven overrides are not applied.
* Nested per-ammo `Override` blocks (e.g. the RPG's standard rocket) are not
  expanded into separate records; the first ammo entry names the weapon's base
  round and the mix is flagged.
* Only the **top-level** `AvailableFireModes`/`DefaultFireMode` are read. Nested
  attachment fire-mode overrides (e.g. `reforging`'s object-valued
  `AvailableFireModes`) are not descended into; where they express semantics the
  top level does not, the top-level `DefaultFireMode` still surfaces the gap (as
  it does for `reforging`'s `Charge`).
