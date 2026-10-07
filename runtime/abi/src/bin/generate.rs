//! Writes runtime/browser/src/abi.gen.ts and compiler/std/abi.wrela from the ABI's definitions,
//! and runtime/abi/vectors.json, the test vectors both hosts are checked against.

fn main() -> std::io::Result<()> {
    for (path, text) in wrela_abi::generated_files() {
        std::fs::write(&path, text)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}
