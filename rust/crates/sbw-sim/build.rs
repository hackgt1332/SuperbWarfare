use sha2::{Digest, Sha256};
fn main() {
    let mut hash = Sha256::new();
    for path in [
        "src/lib.rs",
        "src/pickup.json",
        "src/truck.json",
        "../sbw-contract/wit/contract.wit",
    ] {
        println!("cargo:rerun-if-changed={path}");
        let bytes = std::fs::read(path).expect("authoritative content source");
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    let bytes: [u8; 32] = hash.finalize().into();
    std::fs::write(
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("content_hash.rs"),
        format!("pub const CONTENT_SHA256: [u8;32] = {bytes:?};"),
    )
    .unwrap();
}
