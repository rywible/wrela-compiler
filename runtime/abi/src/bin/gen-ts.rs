//! Writes runtime/browser/src/abi.gen.ts from the ABI's definitions.

fn main() -> std::io::Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = root.join(wrela_abi::typescript::TS_PATH);
    std::fs::write(&path, wrela_abi::typescript())?;
    println!("wrote {}", path.display());
    Ok(())
}
