//! Verify and wrap a linkage fixture built for wasm32-unknown-unknown.
use std::{error::Error, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("expected path to a core .wasm module")?,
    );
    let module = std::fs::read(&path)?;
    let component = wit_component::ComponentEncoder::default()
        .module(&module)?
        .validate(true)
        .encode()?;
    let output = path.with_extension("component.wasm");
    std::fs::write(&output, &component)?;
    println!(
        "Validated component: {} ({} bytes)",
        output.display(),
        component.len()
    );
    Ok(())
}
