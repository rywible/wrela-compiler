#![allow(dead_code)] // each test binary uses part of this

pub mod wat;

use std::path::{Path, PathBuf};

pub fn first_light() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/first-light")
}

/// A fresh, empty temporary directory for one test.
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wrela-host-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A copy of first-light's build with its `game.wasm` replaced by `wasm`.
pub fn first_light_with(name: &str, wasm: &[u8]) -> PathBuf {
    let dir = temp_dir(name);
    for entry in std::fs::read_dir(first_light()).expect("fixture") {
        let path = entry.expect("entry").path();
        if path.is_file() {
            std::fs::copy(&path, dir.join(path.file_name().expect("name"))).expect("copy");
        }
    }
    std::fs::write(dir.join("game.wasm"), wasm).expect("write wasm");
    dir
}

/// A build directory with the given manifest, WGSL files and WASM.
pub fn build(name: &str, manifest: &str, shaders: &[(&str, &str)], wasm: &[u8]) -> PathBuf {
    let dir = temp_dir(name);
    std::fs::write(dir.join("manifest.json"), manifest).expect("write manifest");
    for (file, source) in shaders {
        std::fs::write(dir.join(file), source).expect("write shader");
    }
    std::fs::write(dir.join("game.wasm"), wasm).expect("write wasm");
    dir
}
