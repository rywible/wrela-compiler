//! Compiled code, kept beside a build: compiling a program's WASM is most of the time it takes
//! to load (the lens on the wolf: 1.5 s of 2.2), and an agent's actions load the same build
//! over and over. The first load writes the module's compiled code to
//! `<build>/.native/game-<hash>.cwasm` (the hash is the WASM's); later loads of the same WASM
//! read it back. `wrela studio` writes it right after building the lens (`precompile`). With
//! `WRELA_NATIVE_CACHE` naming a directory, every build keeps its code there instead (the
//! tests' builds, made afresh each run with mostly the same WASM), and code written more than a
//! day before is removed as new code is written.
//!
//! Reading compiled code back is `unsafe` in wasmtime: the bytes become executable code, so they
//! must be what `Module::serialize` wrote. Wasmtime checks that they were written by its own
//! version with this engine's settings, and refuses them if not; this module only reads files it
//! wrote, in the build's own directory, named by the WASM's hash. Someone who can write that
//! directory can already replace the WASM itself.
#![allow(unsafe_code)]

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use wasmtime::{Engine, Module};

/// The name of the compiled code of `wasm` in a build's `.native` directory.
pub fn compiled_code_name(wasm: &[u8]) -> String {
    format!("{}.cwasm", stem(wasm))
}

/// The start of the names of `wasm`'s compiled code: `game-` and the WASM's hash.
fn stem(wasm: &[u8]) -> String {
    let mut hash = wrela_abi::hash::StateHash::new();
    hash.update(wasm);
    format!("game-{}", hash.hex())
}

/// Where the compiled code of `wasm` goes for the build in `dir`: code for another machine (an
/// x86-64 host under Rosetta, beside an arm64 one), or code that counts fuel (for tests), is
/// another file.
fn path(dir: &Path, wasm: &[u8], fuel: bool) -> PathBuf {
    let arch = std::env::consts::ARCH;
    let name = match (fuel, arch) {
        (false, "aarch64") => compiled_code_name(wasm),
        (false, _) => format!("{}-{arch}.cwasm", stem(wasm)),
        (true, _) => format!("{}-{arch}-fuel.cwasm", stem(wasm)),
    };
    shared().unwrap_or_else(|| dir.join(".native")).join(name)
}

/// A directory of compiled code that every build shares, if `WRELA_NATIVE_CACHE` names one:
/// the tests', whose builds are made afresh on each run while their WASM mostly stays the same.
fn shared() -> Option<PathBuf> {
    std::env::var_os("WRELA_NATIVE_CACHE").filter(|d| !d.is_empty()).map(PathBuf::from)
}

/// Removes the compiled code of every other WASM from the directory `keep` is in: a build has
/// one WASM, and the code of the ones it had before is never read again.
fn remove_others(keep: &Path, wasm: &[u8]) {
    let Some(dir) = keep.parent() else { return };
    let stem = stem(wasm);
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(&stem) && name.starts_with("game-") && name.ends_with(".cwasm") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Removes the code in the shared directory `keep` is in that was written more than a day ago.
fn remove_stale(keep: &Path) {
    let Some(dir) = keep.parent() else { return };
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|a| a > day));
        if old && e.file_name().to_string_lossy().ends_with(".cwasm") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// `wasm` compiled for `engine`: read back from the build's cache in `dir` when an earlier load
/// left it there, else compiled, and written there (best effort: a build in a read-only place
/// still loads).
pub(crate) fn module(engine: &Engine, wasm: &[u8], dir: &Path) -> Result<Module> {
    let file = path(dir, wasm, engine.get_consume_fuel());
    if file.is_file() {
        // SAFETY: the file is what `Module::serialize` wrote for this WASM (see the module's
        // documentation); wasmtime refuses code from another version or configuration, and then
        // the WASM is compiled again.
        if let Ok(m) = unsafe { Module::deserialize_file(engine, &file) } {
            return Ok(m);
        }
    }
    let module = Module::new(engine, wasm).map_err(|e| Error::Program(format!("{e:#}")))?;
    if let Ok(bytes) = module.serialize() {
        // A temporary file of this write's own: threads of one process may load a build at once.
        static WRITES: AtomicU64 = AtomicU64::new(0);
        let n = WRITES.fetch_add(1, Ordering::Relaxed);
        let tmp = file.with_extension(format!("{}-{n}.tmp", std::process::id()));
        let written = std::fs::create_dir_all(file.parent().unwrap_or(dir)).is_ok()
            && std::fs::write(&tmp, bytes).is_ok()
            && std::fs::rename(&tmp, &file).is_ok();
        if written && shared().is_none() {
            remove_others(&file, wasm);
        } else if written {
            remove_stale(&file);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    Ok(module)
}
