//! Helpers shared by the integration tests (each test file uses some of them).
#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `*.wrela` file under `compiler/` (the conformance suite in compiler/tests, and the
/// standard library), sorted, skipping build output.
pub fn wrela_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            if p.is_dir() {
                if name != "target" && !name.starts_with('.') {
                    walk(&p, out);
                }
            } else if name.ends_with(".wrela") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(&repo_root().join("compiler"), &mut out);
    out.sort();
    out
}

/// The `SAMPLE` program of compiler/syntax/tests/roundtrip.rs (a raw string literal there).
pub fn roundtrip_sample() -> String {
    let path = repo_root().join("compiler/syntax/tests/roundtrip.rs");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let start = text.find("const SAMPLE: &str = r#\"").expect("roundtrip.rs has no SAMPLE") + 24;
    let len = text[start..].find("\"#;").expect("SAMPLE isn't closed");
    text[start..start + len].to_string()
}

/// Runs `f` on a thread with a big stack: the oracle's derivation counting recurses once per
/// nested rule and repetition, which can be deep for whole files.
pub fn with_big_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn_scoped(s, f)
            .expect("spawning a test thread")
            .join()
            .unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}
