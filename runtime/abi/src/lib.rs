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
//! - may export `init()`, which the host calls once, after instantiating the module and before
//!   anything else: a program with state makes it there (language.md §12);
//! - imports `wrela.submit(ptr: i32, len: i32)`, which hands the host a batch of commands: `len`
//!   bytes at `ptr` in its memory (see [`stream`]); and may import
//!   `wrela.request_status(request: i32) -> i32` and `wrela.request_take(request: i32, ptr: i32)`,
//!   which answer the requests its batches make, `wrela.limit(index: i32) -> i32`, which
//!   gives one of the GPU's limits ([`Limits`]), `wrela.audio(task: i32, context: i32)`,
//!   which starts the program's voice ([`IMPORT_AUDIO`]), `wrela.tick(task: i32, context: i32,
//!   hz: i32)`, which starts its ticker ([`IMPORT_TICK`]), and `wrela.input(ptr: i32, cap: i32)
//!   -> i32`, which gives it the pointer and keyboard events that have arrived ([`input`]);
//! - imports its memory, shared, as `wrela.memory` ([`memory`]);
//! - may export thread entries (`__worker`, `__audio`, `__tick`), which a host calls on the
//!   program's other threads ([`memory`]'s threads);
//! - may have a start function, which runs as each instance starts and may not call the host:
//!   the compiler's copies the program's constants into the memory, once however many instances
//!   share it ([`memory::DATA_READY`]).
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
pub mod input;
pub mod lines;
pub mod manifest;
pub mod memory;
pub mod stream;
pub mod ticks;
pub mod typescript;
pub mod vectors;

pub use input::IMPORT_INPUT;
pub use manifest::Manifest;
pub use typescript::typescript;

use std::path::{Path, PathBuf};

/// The program's imports: their module, and each one's name. `submit` hands the host a batch;
/// `request_status` and `request_take` answer requests ([`stream`]).
pub const IMPORT_MODULE: &str = "wrela";
pub const IMPORT_SUBMIT: &str = "submit";
pub const IMPORT_REQUEST_STATUS: &str = "request_status";
pub const IMPORT_REQUEST_TAKE: &str = "request_take";
pub const IMPORT_LIMIT: &str = "limit";
/// `wrela.audio(task, context)`: starts the program's voice, which the audio thread renders by
/// calling [`EXPORT_AUDIO`] with the same two numbers (language.md §6.13). Once per program.
pub const IMPORT_AUDIO: &str = "audio";
/// `wrela.tick(task, context, hz)`: starts the program's ticker, which the ticker's thread
/// steps `hz` times a second by calling [`EXPORT_TICK`] with the two numbers and the tick's
/// (language.md's `std::tick`). Once per program: a second is a panic.
pub const IMPORT_TICK: &str = "tick";
/// The memory, which the module imports, shared (`memory`), and also exports.
pub const IMPORT_MEMORY: &str = "memory";
/// `__worker(thread)`: a helper's thread entry (`memory`'s threads and helpers).
pub const EXPORT_WORKER: &str = "__worker";
/// `__audio(thread, task, context)`: renders one quantum of the voice `wrela.audio` started, at
/// [`memory::AUDIO_OUT`], on the audio thread ([`memory::THREAD_AUDIO`]), on its own instance
/// of the module, with the same memory.
pub const EXPORT_AUDIO: &str = "__audio";
/// `__tick(thread, task, context, tick)`: runs tick `tick` of the ticker `wrela.tick` started,
/// with the records at [`memory::TICK_RECORDS`], on the ticker's thread
/// ([`memory::THREAD_TICK`]), on its own instance of the module, with the same memory.
pub const EXPORT_TICK: &str = "__tick";
/// The audio thread's sample rate, in hertz, and how many samples one `__audio` call renders
/// (a Web Audio render quantum). Both hosts render at this rate; the browser resamples to the
/// device's.
pub const AUDIO_SAMPLE_RATE: u32 = 48000;
pub const AUDIO_QUANTUM: u32 = 128;
/// What `request_status` says of a request that isn't answered yet, and of one that failed.
pub const REQUEST_PENDING: i32 = -1;
pub const REQUEST_FAILED: i32 = -2;

/// A function the host gives a program, in [`IMPORT_MODULE`]: its name, and how many `i32`s it
/// takes and gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostFunction {
    pub name: &'static str,
    pub params: usize,
    pub results: usize,
}

impl HostFunction {
    /// Its type, as both hosts word it: `(i32, i32) -> ()`.
    pub fn signature(&self) -> String {
        let list = |n| vec!["i32"; n].join(", ");
        format!("({}) -> ({})", list(self.params), list(self.results))
    }
}

/// Every function a program may import besides its memory ([`IMPORT_MEMORY`]).
pub const HOST_FUNCTIONS: [HostFunction; 7] = [
    HostFunction { name: IMPORT_SUBMIT, params: 2, results: 0 },
    HostFunction { name: IMPORT_REQUEST_STATUS, params: 1, results: 1 },
    HostFunction { name: IMPORT_REQUEST_TAKE, params: 2, results: 0 },
    HostFunction { name: IMPORT_LIMIT, params: 1, results: 1 },
    HostFunction { name: IMPORT_AUDIO, params: 2, results: 0 },
    HostFunction { name: IMPORT_INPUT, params: 2, results: 1 },
    HostFunction { name: IMPORT_TICK, params: 3, results: 0 },
];

/// The export the host calls each frame.
pub const EXPORT_FRAME: &str = "frame";
/// The export a program with state has (language.md §12): the host calls it once, after
/// instantiating the program and before any other export.
pub const EXPORT_INIT: &str = "init";
pub const EXPORT_MEMORY: &str = "memory";

/// The GPU limits a program can ask for (`wrela.limit(index)`, by index in this order) and the
/// hosts check commands against. Each host opens its device with the adapter's own limits, so
/// these are the GPU's real ones, at least WebGPU's defaults ([`Limits::DEFAULT`]). A value
/// over `u32::MAX` is given as `u32::MAX`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// `maxTextureDimension2D`.
    pub max_texture_size: u32,
    /// The lesser of `maxBufferSize` and `maxStorageBufferBindingSize`: every buffer can be bound
    /// as storage.
    pub max_buffer_size: u32,
    /// `maxStorageBuffersPerShaderStage`.
    pub max_storage_buffers_per_stage: u32,
    /// `maxUniformBufferBindingSize`.
    pub max_uniform_buffer_binding_size: u32,
    /// `maxComputeWorkgroupStorageSize`.
    pub max_workgroup_storage_size: u32,
    /// `maxComputeInvocationsPerWorkgroup`.
    pub max_workgroup_invocations: u32,
    /// `maxComputeWorkgroupSizeX`, `Y` and `Z`.
    pub max_workgroup_size: [u32; 3],
    /// `maxComputeWorkgroupsPerDimension`.
    pub max_workgroups_per_dimension: u32,
}

impl Limits {
    /// WebGPU's defaults: what a pipeline is compiled for, and what a host without a GPU
    /// reports.
    pub const DEFAULT: Limits = Limits {
        max_texture_size: 8192,
        max_buffer_size: 134_217_728,
        max_storage_buffers_per_stage: 8,
        max_uniform_buffer_binding_size: 65_536,
        max_workgroup_storage_size: 16_384,
        max_workgroup_invocations: 256,
        max_workgroup_size: [256, 256, 64],
        max_workgroups_per_dimension: 65_535,
    };

    /// How many there are: `wrela.limit` gives 0 for an index past the last.
    pub const COUNT: u32 = 10;

    /// The limit at `index`, as `wrela.limit` gives it.
    pub fn get(&self, index: u32) -> u32 {
        match index {
            0 => self.max_texture_size,
            1 => self.max_buffer_size,
            2 => self.max_storage_buffers_per_stage,
            3 => self.max_uniform_buffer_binding_size,
            4 => self.max_workgroup_storage_size,
            5 => self.max_workgroup_invocations,
            6..=8 => self.max_workgroup_size[index as usize - 6],
            9 => self.max_workgroups_per_dimension,
            _ => 0,
        }
    }
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

    /// std's copies of the ABI's numbers (compiler/std, which can't import this crate) are this
    /// crate's.
    #[test]
    fn std_copies_are_current() {
        use crate::memory::*;
        let std = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compiler/std");
        for (file, name, value) in [
            ("alloc.wrela", "STATE", ALLOC_STATE),
            ("alloc.wrela", "BUMP", ALLOC_STATE),
            ("alloc.wrela", "ALLOCATIONS", ALLOCATIONS),
            ("mem.wrela", "THREAD_BLOCKS", THREAD_BLOCKS),
            ("mem.wrela", "THREAD_BLOCK_SIZE", THREAD_BLOCK_SIZE),
            ("mem.wrela", "PANIC", PANIC),
            ("par.wrela", "WAKE", PAR_WAKE),
            ("par.wrela", "SHUTDOWN", PAR_SHUTDOWN),
            ("par.wrela", "HELPED", PAR_HELPED),
            ("par.wrela", "HOLD", PAR_HOLD),
            ("par.wrela", "THREADS", THREADS),
            ("par.wrela", "GENERATION", JOB_GENERATION),
            ("par.wrela", "TICKET", JOB_TICKET),
            ("par.wrela", "TASK", JOB_TASK),
            ("par.wrela", "CONTEXT", JOB_CONTEXT),
            ("par.wrela", "CHUNKS", JOB_CHUNKS),
            ("par.wrela", "DONE", JOB_DONE),
            ("par.wrela", "FAILED", JOB_FAILED),
            ("par.wrela", "DONE_FAILED", JOB_DONE_FAILED),
            ("par.wrela", "DEPTH", DEPTH),
            ("par.wrela", "RUNNING", RUNNING),
            ("par.wrela", "JOIN_WAITS", JOIN_WAITS),
            ("par.wrela", "SLOTS", JOB_SLOTS),
            ("par.wrela", "SLOT_COUNT", JOB_SLOT_COUNT),
            ("par.wrela", "SLOT_SIZE", JOB_SLOT_SIZE),
            ("par.wrela", "SLOT_THREAD", SLOT_THREAD),
            ("par.wrela", "FAILED_JOB", SLOT_FAILED),
            ("audio.wrela", "OUT", AUDIO_OUT),
            ("audio.wrela", "SAMPLE_RATE", crate::AUDIO_SAMPLE_RATE),
            ("audio.wrela", "QUANTUM", crate::AUDIO_QUANTUM),
            ("tick.wrela", "WANT_HASH", TICK_WANT_HASH),
            ("tick.wrela", "HASH", TICK_HASH),
            ("tick.wrela", "ORIGIN", TICK_ORIGIN),
            ("tick.wrela", "RECORDS", TICK_RECORDS),
            ("tick.wrela", "MAX_RECORDS", MAX_TICK_RECORDS),
        ] {
            let text = std::fs::read_to_string(std.join(file)).unwrap();
            let decl = format!("const {name}: u32 = ");
            let found = text.lines().find_map(|l| {
                let l = l.strip_prefix("pub ").unwrap_or(l);
                l.strip_prefix(&decl)
            });
            assert_eq!(found, Some(value.to_string().as_str()), "std's `{name}` in {file}");
        }
    }
}
