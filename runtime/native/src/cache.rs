//! Compiled code for a loaded build. The build's files are untrusted, so executable Wasmtime
//! artifacts are kept only in the host's private code cache (`crate::code`). If that cache can't
//! be used, compiling the WASM directly is safe and still lets the build run.

use crate::error::{Error, Result};
use wasmtime::{Engine, Module};

/// Compiles `wasm`, using the host's private cache when available. A build-local `.native`
/// directory is never read: a build may contain arbitrary files beside its WASM.
pub(crate) fn module(engine: &Engine, wasm: &[u8]) -> Result<Module> {
    let cache =
        crate::code::cache_dir().and_then(|dir| dir.map(crate::code::Cache::open).transpose());
    if let Ok(Some(cache)) = cache {
        return cache.module(engine, wasm).map_err(Error::Program);
    }
    Module::new(engine, wasm).map_err(|e| Error::Program(format!("{e:#}")))
}
