//! Development-only, deterministic importer for the original gun definitions.
//!
//! The writing game never loads this binary. It walks `data/<ns>/sbw/guns/*.json`
//! in the SuperbWarfare source tree, imports every definition through
//! [`sbw_sim::catalog`], and emits a stable JSON report plus a human summary.
//!
//! Determinism is the point: directory order is normalised, the catalog sorts by
//! id, and all JSON is emitted with sorted object keys and a fixed key order, so
//! the same tree always produces byte-identical output. `--check` fails if the
//! on-disk report has drifted from what the tree would generate.
//!
//! The report is metadata only. A `ported` state here means `sbw_sim::gun`
//! answers for that id; it is not, and must not be read as, a claim that the
//! weapon is playable in Horizon.

use serde_json::{Map, Value, json};
use std::error::Error;
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

use sbw_sim::catalog::{AmmoClass, Catalog, Status, WeaponMeta};

/// Relative location of the source gun tree inside the repository root.
const GUNS_DIR: &str = "src/main/resources/data/superbwarfare/sbw/guns";
/// Default report location, inside this tool's crate.
const DEFAULT_REPORT: &str = "catalog-report.json";

fn main() -> Result<()> {
    let mut check = false;
    let mut root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut out: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
            "-o" | "--out" => {
                let value = args.next().ok_or("--out needs a path")?;
                out = Some(PathBuf::from(value));
            }
            other => root = PathBuf::from(other),
        }
    }
    let report_path =
        out.unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_REPORT));

    let entries = read_tree(&root.join(GUNS_DIR))?;
    let catalog = Catalog::from_entries(entries)?;
    let report = render(&catalog)?;
    let bytes = {
        let mut b = serde_json::to_vec_pretty(&report)?;
        b.push(b'\n');
        b
    };

    if check {
        let current = std::fs::read(&report_path).unwrap_or_default();
        if current != bytes {
            return Err(format!(
                "{} is stale; re-run convert-catalog without --check",
                report_path.display()
            )
            .into());
        }
    } else {
        std::fs::write(&report_path, &bytes)?;
    }

    let ported = catalog.ported_ids();
    let unsupported = catalog.unsupported().count();
    println!(
        "{}: {} definitions, {} ported, {} with unsupported semantics, {} warnings -> {}",
        root.join(GUNS_DIR).display(),
        catalog.len(),
        ported.len(),
        unsupported,
        catalog.warnings().len(),
        report_path.display()
    );
    if check {
        println!("check passed: report is up to date");
    }
    Ok(())
}

/// Read `(name, bytes)` for every `.json` gun definition, sorted by name.
fn read_tree(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("non-UTF-8 gun filename")?
            .to_owned();
        entries.push((name, std::fs::read(&path)?));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    if entries.is_empty() {
        return Err(format!("no gun definitions under {}", dir.display()).into());
    }
    Ok(entries)
}

/// Build the stable JSON report from a catalog.
fn render(catalog: &Catalog) -> Result<Value> {
    let weapons: Vec<Value> = catalog.weapons().iter().map(render_weapon).collect();
    let warnings: Vec<Value> = catalog
        .warnings()
        .iter()
        .map(|w| {
            let mut o = Map::new();
            o.insert("id".into(), json!(w.id));
            o.insert("field".into(), json!(w.field));
            o.insert("reason".into(), json!(w.reason.to_string()));
            Value::Object(o)
        })
        .collect();
    Ok(json!({
        "note": "Metadata imported from the original SuperbWarfare gun definitions. \
                 A ported entry means sbw_sim::gun answers for that id, not that the \
                 weapon is playable in Horizon.",
        "namespace": sbw_sim::catalog::NAMESPACE,
        "count": catalog.len(),
        "ported": catalog.ported_ids(),
        "weapons": weapons,
        "warnings": warnings,
    }))
}

/// Emit one weapon with an explicit, stable key order.
fn render_weapon(w: &WeaponMeta) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), json!(w.id.0));
    o.insert("category".into(), json!(format!("{:?}", w.category)));
    o.insert(
        "status".into(),
        json!(match w.status {
            Status::Ported => "ported",
            Status::Cataloged => "cataloged",
        }),
    );
    o.insert("ammo".into(), json!(ammo_label(&w.ammo)));
    o.insert(
        "ammo_family".into(),
        json!(w.ammo_family().map(|f| format!("{f:?}"))),
    );
    o.insert("magazine".into(), json!(w.magazine));
    o.insert("magazine_levels".into(), json!(w.magazine_levels));
    o.insert("damage".into(), json!(w.damage));
    o.insert("velocity".into(), json!(w.velocity));
    o.insert("rpm".into(), json!(w.rpm));
    o.insert("interval_ticks".into(), json!(w.interval_ticks()));
    o.insert(
        "default_fire_mode".into(),
        json!(format!("{:?}", w.default_fire_mode)),
    );
    o.insert(
        "fire_modes".into(),
        json!(
            w.fire_modes
                .iter()
                .map(|m| format!("{m:?}"))
                .collect::<Vec<_>>()
        ),
    );
    o.insert("normal_reload".into(), json!(w.normal_reload));
    o.insert("empty_reload".into(), json!(w.empty_reload));
    o.insert(
        "unsupported".into(),
        json!(
            w.unsupported
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        ),
    );
    Value::Object(o)
}

fn ammo_label(ammo: &AmmoClass) -> String {
    match ammo {
        AmmoClass::Bullet(f) => format!("bullet:{f:?}"),
        AmmoClass::Energy => "energy".into(),
        AmmoClass::Infinite => "infinite".into(),
        AmmoClass::Item(id) => format!("item:{id}"),
        AmmoClass::Unsupported(r) => format!("unsupported:{r}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    #[test]
    fn real_tree_imports_and_renders_deterministically() {
        let entries = read_tree(&root().join(GUNS_DIR)).unwrap();
        let a = Catalog::from_entries(entries.clone()).unwrap();
        let b = Catalog::from_entries(entries).unwrap();
        assert_eq!(render(&a).unwrap(), render(&b).unwrap());
        assert!(a.len() >= 60, "imported only {}", a.len());
    }

    #[test]
    fn render_is_newline_terminated_and_pretty() {
        let entries = read_tree(&root().join(GUNS_DIR)).unwrap();
        let catalog = Catalog::from_entries(entries).unwrap();
        let mut bytes = serde_json::to_vec_pretty(&render(&catalog).unwrap()).unwrap();
        bytes.push(b'\n');
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert!(bytes.starts_with(b"{"));
    }

    #[test]
    fn ported_entries_render_as_ported_and_report_is_metadata_only() {
        let entries = read_tree(&root().join(GUNS_DIR)).unwrap();
        let catalog = Catalog::from_entries(entries).unwrap();
        let report = render(&catalog).unwrap();
        assert_eq!(report["count"], catalog.len());
        let weapons = report["weapons"].as_array().unwrap();
        let ak = weapons
            .iter()
            .find(|w| w["id"] == "superbwarfare:ak_47")
            .unwrap();
        assert_eq!(ak["status"], "ported");
        assert_eq!(ak["ammo_family"], "Rifle");
        assert_eq!(ak["magazine"], 30);
        // A cataloged launcher is present but not playable.
        let rpg = weapons
            .iter()
            .find(|w| w["id"] == "superbwarfare:rpg")
            .unwrap();
        assert_eq!(rpg["status"], "cataloged");
    }

    #[test]
    fn missing_tree_is_an_error() {
        assert!(read_tree(&root().join("does/not/exist")).is_err());
    }

    #[test]
    fn report_carries_array_reload_times_and_fire_mode_gaps() {
        let entries = read_tree(&root().join(GUNS_DIR)).unwrap();
        let catalog = Catalog::from_entries(entries).unwrap();
        let report = render(&catalog).unwrap();
        let weapons = report["weapons"].as_array().unwrap();
        // AK-47's reload times are per-level arrays in the source; the base
        // entry must appear in the report rather than null.
        let ak = weapons
            .iter()
            .find(|w| w["id"] == "superbwarfare:ak_47")
            .unwrap();
        assert_eq!(ak["normal_reload"], 48);
        assert_eq!(ak["empty_reload"], 64);
        // `reforging` declares an unmodelled `Charge` fire mode; the gap is
        // recorded as an unsupported reason, and the entry stays cataloged.
        let reforging = weapons
            .iter()
            .find(|w| w["id"] == "superbwarfare:reforging")
            .unwrap();
        assert_eq!(reforging["status"], "cataloged");
        let reasons = reforging["unsupported"].as_array().unwrap();
        assert!(
            reasons
                .iter()
                .any(|r| r.as_str().is_some_and(|s| s.contains("Charge"))),
            "expected a Charge fire-mode gap, got {reasons:?}"
        );
    }
}
