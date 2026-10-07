//! Compiled code, kept beside a build: compiling a program's WASM is most of the time it takes
//! to load (the lens on the wolf: 1.5 s of 2.2), and an agent's actions load the same build
//! over and over. The first load writes the module's compiled code to
//! `<build>/.native/game-<hash>-<arch>.cwasm` (the hash is the WASM's, the arch the machine's);
//! later loads of the same WASM read it back. `wrela studio` writes it right after building the
//! lens (`precompile`). With `WRELA_NATIVE_CACHE` naming a directory, every build keeps its code
//! there instead (the tests' builds, made afresh each run with mostly the same WASM), and code
//! written more than a day before is removed as new code is written. A build made for one run
//! keeps none beside it.
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

/// The start of the names of `wasm`'s compiled code in a build's `.native` directory: each
/// machine's code, and code that counts fuel, is a file of its own.
pub fn compiled_code_prefix(wasm: &[u8]) -> String {
    stem(wrela_abi::ticks::wasm_hash(wasm))
}

/// The start of the names of the compiled code of the WASM whose hash is `hash`: `game-` and
/// the hash.
fn stem(hash: u64) -> String {
    format!("game-{}", wrela_abi::hash::hex(hash))
}

/// Where the compiled code of the WASM whose hash is `hash` goes for the build in `dir` (`None`:
/// a build made for one run, which keeps it only in the shared directory): code for another
/// machine (an x86-64 host under Rosetta, beside an arm64 one), or code that counts fuel (for
/// tests), is another file.
fn path(dir: Option<&Path>, hash: u64, fuel: bool) -> Option<PathBuf> {
    let arch = std::env::consts::ARCH;
    let fuel = if fuel { "-fuel" } else { "" };
    let name = format!("{}-{arch}{fuel}.cwasm", stem(hash));
    Some(shared().or_else(|| dir.map(|d| d.join(".native")))?.join(name))
}

/// A directory of compiled code that every build shares, if `WRELA_NATIVE_CACHE` names one:
/// the tests', whose builds are made afresh on each run while their WASM mostly stays the same.
fn shared() -> Option<PathBuf> {
    std::env::var_os("WRELA_NATIVE_CACHE").filter(|d| !d.is_empty()).map(PathBuf::from)
}

/// Removes the compiled code of every other WASM from the directory `keep` is in: a build has
/// one WASM (whose names start with `stem`), and the code of the ones it had before is never
/// read again.
fn remove_others(keep: &Path, stem: &str) {
    let Some(dir) = keep.parent() else { return };
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(stem) && name.starts_with("game-") && name.ends_with(".cwasm") {
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

/// `wasm`, whose hash is `hash`, compiled for `engine`: read back from the cache of the build in
/// `dir` (or the shared one) when an earlier load left it there, else compiled, and written
/// there (best effort: a build in a read-only place still loads).
pub(crate) fn module(
    engine: &Engine,
    wasm: &[u8],
    hash: u64,
    dir: Option<&Path>,
) -> Result<Module> {
    let compile = || Module::new(engine, wasm).map_err(|e| Error::Program(format!("{e:#}")));
    let Some(file) = path(dir, hash, engine.get_consume_fuel()) else {
        return compile();
    };
    if file.is_file() {
        // SAFETY: the file is what `Module::serialize` wrote for this WASM (see the module's
        // documentation); wasmtime refuses code from another version or configuration, and then
        // the WASM is compiled again.
        if let Ok(m) = unsafe { Module::deserialize_file(engine, &file) } {
            return Ok(m);
        }
    }
    let module = compile()?;
    if let Ok(bytes) = module.serialize() {
        // A temporary file of this write's own: threads of one process may load a build at once.
        static WRITES: AtomicU64 = AtomicU64::new(0);
        let n = WRITES.fetch_add(1, Ordering::Relaxed);
        let tmp = file.with_extension(format!("{}-{n}.tmp", std::process::id()));
        let written = file.parent().is_some_and(|d| std::fs::create_dir_all(d).is_ok())
            && std::fs::write(&tmp, bytes).is_ok()
            && std::fs::rename(&tmp, &file).is_ok();
        if written && shared().is_none() {
            remove_others(&file, &stem(hash));
        } else if written {
            remove_stale(&file);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    Ok(module)
}
