//! Writes runtime/browser/src/abi.gen.ts from the ABI's definitions, and
//! runtime/abi/vectors.json, the test vectors both hosts are checked against.

fn main() -> std::io::Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (path, text) in [
        (wrela_abi::typescript::TS_PATH, wrela_abi::typescript()),
        (wrela_abi::vectors::VECTORS_PATH, wrela_abi::vectors::vectors()),
    ] {
        let path = root.join(path);
        std::fs::write(&path, text)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}
