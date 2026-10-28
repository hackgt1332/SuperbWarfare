//! Package an already built bridge core module as a Horizon .hmod.
use std::{error::Error, path::PathBuf};
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: sbw-horizon-tools MANIFEST CORE.wasm OUTPUT.hmod".into());
    }
    let manifest = std::fs::read_to_string(&args[0])?;
    let component = horizon_mods::manifest::componentize(&std::fs::read(&args[1])?)?;
    let package = horizon_mods::Package::write(&manifest, &component)?;
    let output = PathBuf::from(&args[2]);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output, package)?;
    println!("{}", output.display());
    Ok(())
}
