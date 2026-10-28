//! Package an already built bridge core module as a Horizon .hmod.
use std::{error::Error, path::PathBuf};
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: sbw-horizon-tools MANIFEST CORE.wasm OUTPUT.hmod".into());
    }
    let manifest = std::fs::read_to_string(&args[0])?;
    let component = horizon_mods::manifest::componentize(&std::fs::read(&args[1])?)?;
    fn collect(
        root: &std::path::Path,
        dir: &std::path::Path,
        files: &mut Vec<(String, Vec<u8>)>,
    ) -> std::io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                collect(root, &path, files)?;
            } else if entry.file_type()?.is_file() {
                files.push((
                    path.strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace('\\', "/"),
                    std::fs::read(&path)?,
                ));
            }
        }
        Ok(())
    }
    let root = std::path::Path::new(&args[0]).parent().unwrap();
    let mut files = Vec::new();
    for dir in ["assets", "data"] {
        collect(root, &root.join(dir), &mut files)?;
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let refs: Vec<_> = files
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    let package = horizon_mods::Package::write_files(&manifest, Some(&component), &refs)?;
    let output = PathBuf::from(&args[2]);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output, package)?;
    println!("{}", output.display());
    Ok(())
}
