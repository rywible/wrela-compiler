//! The checked-in fixtures are current: each `game.wasm` is its `game.wat` compiled, and each
//! manifest is valid and in canonical form. `WRELA_BLESS=1` rewrites the `.wasm` files.

use std::path::{Path, PathBuf};
use wrela_abi::Manifest;

fn fixtures() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("runtime/fixtures")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.join("manifest.json").is_file())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty(), "no fixtures in {}", root.display());
    dirs
}

#[test]
fn wasm_is_the_compiled_wat() {
    let bless = std::env::var_os("WRELA_BLESS").is_some();
    for dir in fixtures() {
        let wasm = wat::parse_file(dir.join("game.wat")).expect("game.wat compiles");
        let path = dir.join("game.wasm");
        if bless {
            std::fs::write(&path, &wasm).expect("write game.wasm");
        }
        let checked_in = std::fs::read(&path).unwrap_or_default();
        assert!(checked_in == wasm, "{} is stale; rerun with WRELA_BLESS=1", path.display());
    }
}

#[test]
fn manifests_are_valid_and_canonical() {
    for dir in fixtures() {
        let json = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest.json");
        let manifest = Manifest::parse(&json).expect("valid manifest");
        assert_eq!(manifest.to_json(), json, "{} isn't in canonical form", dir.display());
        for p in &manifest.pipelines {
            assert!(
                dir.join(&p.shader).is_file(),
                "{} names a missing shader {}",
                dir.display(),
                p.shader
            );
        }
    }
}
