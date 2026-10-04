//! Embeds the explanations `wrela explain` prints (explain/<code>.wrela), one per diagnostic
//! code, as a table: `src/explain.rs` reads them.

use std::fmt::Write;

fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("explain");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("compiler/driver/explain")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "wrela"))
        .collect();
    files.sort();
    let mut out = String::from("pub(crate) const FILES: &[(&str, &str)] = &[\n");
    for f in files {
        println!("cargo:rerun-if-changed={}", f.display());
        let code = f.file_stem().expect("a name").to_string_lossy().into_owned();
        writeln!(out, "    ({code:?}, include_str!({:?})),", f.display().to_string())
            .expect("write to a string");
    }
    out.push_str("];\n");
    let dest =
        std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("explanations.rs");
    std::fs::write(dest, out).expect("write the table");
}
