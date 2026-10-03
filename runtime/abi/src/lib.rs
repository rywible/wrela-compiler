//! # The wrela runtime ABI
//!
//! This crate is the one definition of how a compiled wrela program talks to a host (the browser
//! runtime in `runtime/browser`, or the native host in `runtime/native`). It's an executable
//! spec: the format's version, opcodes and layouts are defined here, documented here, and
//! pinned by golden byte tests. The browser runtime's constants are generated from it
//! ([`typescript`]), and a test fails if the checked-in copy drifts.
//!
//! ## What a build ships (D-099)
//!
//! - `game.wasm`: the program's CPU code.
//! - One WGSL module per pipeline.
//! - `manifest.json`: a [`Manifest`]. It names the WASM and lists every pipeline.
//! - The standard runtime, pinned to the version the program was built with (D-100).
//!
//! ## The program ABI
//!
//! The WASM module
//! - exports its linear memory as `memory`;
//! - exports `frame(time: f32, width: u32, height: u32)`, which the host calls once per frame
//!   with the seconds since the program started and the canvas size in pixels;
//! - imports exactly one function, `wrela.submit(ptr: i32, len: i32)`, which hands the host a
//!   batch of commands: `len` bytes at `ptr` in its memory (see [`stream`]);
//! - has no start function ([`has_start_function`]): it would run while the module is
//!   instantiated, before the host can read the program's memory.
//!
//! Nothing else is imported, so nothing host-dependent can reach the program's arithmetic
//! (D-015). Other exports are the program's own `pub fn`s, which tests may call.
//!
//! ## The state hash
//!
//! [`hash::StateHash`] is FNV-1a 64 over every byte of every batch the program submits, in
//! order. Two hosts running the same program at the same frame times must compute the same
//! hash (AC7).

pub mod check;
pub mod hash;
pub mod lines;
pub mod manifest;
pub mod stream;
pub mod typescript;
pub mod vectors;

pub use manifest::Manifest;
pub use typescript::typescript;

use std::path::{Path, PathBuf};

/// The program's one import: module and name.
pub const IMPORT_MODULE: &str = "wrela";
pub const IMPORT_SUBMIT: &str = "submit";
/// The export the host calls each frame.
pub const EXPORT_FRAME: &str = "frame";
pub const EXPORT_MEMORY: &str = "memory";

/// Whether a WASM module has a start function, which the program ABI forbids.
pub fn has_start_function(module: &[u8]) -> bool {
    lines::sections(module).any(|(id, _)| id == 8)
}

/// The checked-in files generated from this crate, each as (path, contents): the browser
/// runtime's constants ([`typescript`]) and the test vectors ([`vectors`]).
/// `cargo run -p wrela-abi --bin gen-ts` writes them.
pub fn generated_files() -> [(PathBuf, String); 2] {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    [
        (root.join(typescript::TS_PATH), typescript()),
        (root.join(vectors::VECTORS_PATH), vectors::vectors()),
    ]
}

#[cfg(test)]
mod tests {
    /// The checked-in generated files are this crate's output (the browser runtime reads them).
    #[test]
    fn checked_in_copies_are_current() {
        for (path, text) in super::generated_files() {
            let actual = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                actual == text,
                "{} is stale; run `cargo run -p wrela-abi --bin gen-ts`",
                path.display()
            );
        }
    }
}
