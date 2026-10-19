//! Embeds the explanations `wrela explain` prints (explain/<code>.wrela), one per diagnostic
//! code, as a table: `src/explain.rs` reads them. Also embeds the conformance suite's cases, for
//! `wrela primer`, and the identity of the code that computes a constant, for the constants
//! cache's keys.

use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    // The explanations: each by its code, the file's name.
    let explain = manifest.join("explain");
    let files: Vec<(String, PathBuf)> = walk(&explain, &|_| true, &|n| n.ends_with(".wrela"))
        .into_iter()
        .map(|(rel, path)| (rel.trim_end_matches(".wrela").to_string(), path))
        .collect();
    out("explanations.rs", &table("FILES", "&str", "include_str", &files));
    // The conformance suite's cases (compiler/tests/conformance: each a file, or a directory of
    // a package's files), by path from the suite.
    let cases =
        walk(&root.join("compiler/tests/conformance"), &|_| true, &|n| n.ends_with(".wrela"));
    out("conformance.rs", &table("CASES", "&str", "include_str", &cases));
    // What a computed constant's value depends on beside its WASM, the fuel and the format
    // (const_cache.rs's key): the code that runs it and reads it back, and the host it runs
    // in. Their sources' hash is in each key, so a cache shared by checkouts of other commits
    // never serves a value they computed differently.
    let mut sources: Vec<PathBuf> = [
        "compiler/driver/src/consts.rs",
        "compiler/driver/src/const_cache.rs",
        "compiler/lower/src/eval.rs",
        "runtime/native/src/program.rs",
        "runtime/native/src/trace.rs",
    ]
    .iter()
    .map(|p| root.join(p))
    .collect();
    sources.extend(
        walk(&root.join("runtime/abi/src"), &|_| true, &|n| n.ends_with(".rs"))
            .into_iter()
            .map(|(_, p)| p),
    );
    let mut h = Sha256::new();
    for path in &sources {
        println!("cargo:rerun-if-changed={}", path.display());
        let text = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        h.update((text.len() as u64).to_le_bytes());
        h.update(&text);
    }
    // And the WASM engine's version.
    let lock = root.join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());
    let lock = std::fs::read_to_string(&lock).expect("Cargo.lock");
    let wasmtime = lock
        .split("[[package]]")
        .find(|p| p.contains("\nname = \"wasmtime\"\n"))
        .expect("wasmtime in Cargo.lock");
    h.update(wasmtime.as_bytes());
    let id: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    println!("cargo:rustc-env=WRELA_CONST_IDENTITY={id}");
}

/// The files under `base` whose names `keep` takes, in the directories `enter` takes (at any
/// depth), each by its path from `base` with `/` between its parts, sorted by that path. Cargo
/// runs this again when one of them, or a directory's list, changes.
fn walk(
    base: &Path,
    enter: &dyn Fn(&str) -> bool,
    keep: &dyn Fn(&str) -> bool,
) -> Vec<(String, PathBuf)> {
    fn visit(
        base: &Path,
        dir: &Path,
        enter: &dyn Fn(&str) -> bool,
        keep: &dyn Fn(&str) -> bool,
        out: &mut Vec<(String, PathBuf)>,
    ) {
        println!("cargo:rerun-if-changed={}", dir.display());
        let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if enter(&name) {
                    visit(base, &path, enter, keep, out);
                }
            } else if keep(&name) {
                println!("cargo:rerun-if-changed={}", path.display());
                let rel = path.strip_prefix(base).expect("under base");
                out.push((rel.to_string_lossy().replace('\\', "/"), path));
            }
        }
    }
    let mut out = Vec::new();
    visit(base, base, enter, keep, &mut out);
    out.sort();
    out
}

/// The Rust table `name` of `files`: each one's key, and its contents by `include`
/// (`include_str` or `include_bytes`, which give a `ty`).
fn table(name: &str, ty: &str, include: &str, files: &[(String, PathBuf)]) -> String {
    let mut out = format!("pub(crate) const {name}: &[(&str, {ty})] = &[\n");
    for (key, path) in files {
        writeln!(out, "    ({key:?}, {include}!({:?})),", path.display().to_string())
            .expect("write to a string");
    }
    out.push_str("];\n");
    out
}

/// Writes `text` to the file `name` in cargo's `OUT_DIR`.
fn out(name: &str, text: &str) {
    let dest = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join(name);
    std::fs::write(&dest, text).unwrap_or_else(|e| panic!("{}: {e}", dest.display()));
}
