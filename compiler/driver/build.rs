//! Embeds the explanations `wrela explain` prints (explain/<code>.wrela), one per diagnostic
//! code, as a table: `src/explain.rs` reads them. Also embeds the conformance suite's cases, for
//! `wrela primer`, and the packages `wrela studio` writes beside the lens's program.

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
    // The studio's packages (studio/ and ui/), so the command works from any directory: each
    // package's modules, manifest and fonts, not its tests or build output, by path from it.
    let mut packages = String::new();
    for (name, dir) in [("STUDIO", "studio"), ("UI", "ui")] {
        let skip =
            |n: &str| ["tests", "build", "results", "target"].contains(&n) || n.starts_with('.');
        let keep = |n: &str| n.ends_with(".wrela") || n == "wrela.toml" || n.ends_with(".wfont");
        let files = walk(&root.join(dir), &|n| !skip(n), &keep);
        packages.push_str(&table(name, "&[u8]", "include_bytes", &files));
    }
    out("packages.rs", &packages);
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
