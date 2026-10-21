//! # The wrela runtime ABI
//!
//! This crate is the one definition of how a compiled wrela program talks to a host (the browser
//! runtime in `runtime/browser`, or the native host in `runtime/native`). It's an executable
//! spec: the format's version, opcodes and layouts are defined here, documented here, and
//! pinned by golden byte tests. The browser runtime's constants are generated from it
//! ([`typescript`]), and so are std's ([`wrela`]); a test fails if a checked-in copy drifts.
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
//!   anything else: a program with state makes it there (language.md §15);
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
//! A tool build (language.md §15.5) exports `tool` ([`EXPORT_TOOL`]) in place of `frame`, and
//! `__tool_worker` in place of `__worker`; it imports [`TOOL_FUNCTIONS`], which no other build
//! does, and makes its requests through them, not through `submit`.
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
pub mod wrela;

pub use input::{IMPORT_INPUT, IMPORT_PAYLOAD};
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
/// calling [`EXPORT_AUDIO`] with the same two numbers (language.md §16.4). Once per program.
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
/// [`memory::AUDIO_OUT`] (its left channel, then its right), on the audio thread
/// ([`memory::THREAD_AUDIO`]), on its own instance of the module, with the same memory.
pub const EXPORT_AUDIO: &str = "__audio";
/// `__tick(thread, task, context, tick)`: runs tick `tick` of the ticker `wrela.tick` started,
/// with the records at [`memory::TICK_RECORDS`], on the ticker's thread
/// ([`memory::THREAD_TICK`]), on its own instance of the module, with the same memory.
pub const EXPORT_TICK: &str = "__tick";
/// `__lift_set(literal, value)`: in a lifted build (language.md §23), changes a literal's value,
/// as `std::lift::set` does: code reads the new one from its next call, on the CPU and the GPU.
/// A host calls it on the program's thread, between calls (a tool's edit, hot reload).
pub const EXPORT_LIFT_SET: &str = "__lift_set";
/// `keep(address, length)`: the host keeps those bytes of the program's memory for the build
/// that replaces this one while it runs (hot reload, `std::reload`), in place of what it kept
/// before. A host that starts a program anew keeps nothing.
pub const IMPORT_KEEP: &str = "keep";
/// `kept(address, cap) -> length`: copies up to `cap` bytes of what the replaced build last
/// kept to the address, and gives how many it kept, all of them (0 if nothing). The tool host
/// gives a tool its start this way instead: its arguments and granted secrets (`std::tool`).
pub const IMPORT_KEPT: &str = "kept";
/// `clock(address)`: writes the seconds since the host started the program, an `f64` that never
/// goes back, to the address (`std::time::now`). Reading it is `nondet` (language.md §8).
pub const IMPORT_CLOCK: &str = "clock";
/// `print(address, length)`: shows that UTF-8 line on the host's console now, from any thread
/// (`std::io::print`). The program never reads it back (the `trace` effect, language.md §8).
pub const IMPORT_PRINT: &str = "print";
/// `phase(address, length)`: this thread's time from now to its next `phase`, or to the end of
/// the call the host made (a frame, a tick, a constant, a test), counts under that UTF-8 name;
/// an empty name counts it under none (`std::time::phase`). The host reports it, and the
/// program never reads it back (the `trace` effect).
pub const IMPORT_PHASE: &str = "phase";
/// The audio thread's sample rate, in hertz, and how many samples one `__audio` call renders
/// (a Web Audio render quantum). Both hosts render at this rate; the browser resamples to the
/// device's.
pub const AUDIO_SAMPLE_RATE: u32 = 48000;
pub const AUDIO_QUANTUM: u32 = 128;
/// The voice's channels: left, then right.
pub const AUDIO_CHANNELS: u32 = 2;
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
pub const HOST_FUNCTIONS: [HostFunction; 13] = [
    HostFunction { name: IMPORT_SUBMIT, params: 2, results: 0 },
    HostFunction { name: IMPORT_REQUEST_STATUS, params: 1, results: 1 },
    HostFunction { name: IMPORT_REQUEST_TAKE, params: 2, results: 0 },
    HostFunction { name: IMPORT_LIMIT, params: 1, results: 1 },
    HostFunction { name: IMPORT_AUDIO, params: 2, results: 0 },
    HostFunction { name: IMPORT_INPUT, params: 2, results: 1 },
    HostFunction { name: IMPORT_PAYLOAD, params: 4, results: 1 },
    HostFunction { name: IMPORT_TICK, params: 3, results: 0 },
    HostFunction { name: IMPORT_KEEP, params: 2, results: 0 },
    HostFunction { name: IMPORT_KEPT, params: 2, results: 1 },
    HostFunction { name: IMPORT_CLOCK, params: 1, results: 0 },
    HostFunction { name: IMPORT_PRINT, params: 2, results: 0 },
    HostFunction { name: IMPORT_PHASE, params: 2, results: 0 },
];

/// The export the host calls each frame.
pub const EXPORT_FRAME: &str = "frame";
/// The export a program with state has (language.md §15): the host calls it once, after
/// instantiating the program and before any other export.
pub const EXPORT_INIT: &str = "init";
/// A server build's exports (language.md §15): the server host calls `serve_init` once, when it
/// starts the program, and `serve` whenever something happens (an event, a timer).
pub const EXPORT_SERVE_INIT: &str = "serve_init";
pub const EXPORT_SERVE: &str = "serve";
/// A tool build's export (language.md §15.3, §15.5): `tool() -> u32`, which the tool host calls
/// once, on the tool's thread. It runs the whole tool, waiting on its requests in the host
/// ([`TOOL_FUNCTIONS`]), and its result is the tool's exit status (0 to 255).
pub const EXPORT_TOOL: &str = "tool";
/// `__tool_worker(thread)`: a tool build's helpers' thread entry, in place of
/// [`EXPORT_WORKER`]: the same loop, which tells the host when the helper waits for work and
/// when it wakes ([`IMPORT_TOOL_IDLE`], [`IMPORT_TOOL_ACTIVE`]).
pub const EXPORT_TOOL_WORKER: &str = "__tool_worker";
/// `tool_request(service_at, service_len, body_at, body_len) -> request`: makes a request of
/// the tool service named by that UTF-8 (`tool/files/read`), with those bytes, at once, and
/// gives the number the host chose for it. Any thread of a tool build may call it. The tool
/// host carries the request out beside the tool (language.md §15.5).
pub const IMPORT_TOOL_REQUEST: &str = "tool_request";
/// `tool_wait(request) -> length`: blocks the calling thread until the request is answered,
/// and gives the answer's length in bytes, or [`REQUEST_FAILED`] if the host has no such
/// service. If the run must stop while it waits (its timeout, an interrupt, a helper's trap,
/// stuck work), it traps the calling thread instead, with that reason.
pub const IMPORT_TOOL_WAIT: &str = "tool_wait";
/// `tool_any(at, count) -> index`: blocks until one of the `count` requests whose numbers
/// (`u32`s) are at `at` is answered, and gives its index there; it reads none of them. It
/// traps as [`IMPORT_TOOL_WAIT`] does.
pub const IMPORT_TOOL_ANY: &str = "tool_any";
/// `tool_take(request, at)`: copies an answered request's answer to `at` and forgets the
/// request. What the request made (blobs, driven builds, servers, browsers) is the tool's from
/// here on.
pub const IMPORT_TOOL_TAKE: &str = "tool_take";
/// `tool_idle()`: a helper is about to wait for work, holding none ([`EXPORT_TOOL_WORKER`]).
/// A tool test's fake clock reads it (language.md §13.2).
pub const IMPORT_TOOL_IDLE: &str = "tool_idle";
/// `tool_active()`: a helper woke, before it looks for work.
pub const IMPORT_TOOL_ACTIVE: &str = "tool_active";
/// `tool_join(slot)`: blocks the calling thread while the task in slot `slot` (its address) is
/// [`memory::SLOT_STARTED`] on another thread, as a tool task's join (`std::tool::Task`). It
/// traps as [`IMPORT_TOOL_WAIT`] does.
pub const IMPORT_TOOL_JOIN: &str = "tool_join";
/// `tool_done()`: a helper finished a task and wrote its slot: a thread in `tool_join` may go
/// on.
pub const IMPORT_TOOL_DONE: &str = "tool_done";

/// The functions only a tool build imports, besides [`HOST_FUNCTIONS`]: a tool's requests, and
/// what its threads tell the host. A tool build makes no request through `submit` (the tool
/// host refuses `submit`, `request_status` and `request_take`), and no other build imports
/// these.
pub const TOOL_FUNCTIONS: [HostFunction; 8] = [
    HostFunction { name: IMPORT_TOOL_REQUEST, params: 4, results: 1 },
    HostFunction { name: IMPORT_TOOL_WAIT, params: 1, results: 1 },
    HostFunction { name: IMPORT_TOOL_ANY, params: 2, results: 1 },
    HostFunction { name: IMPORT_TOOL_TAKE, params: 2, results: 0 },
    HostFunction { name: IMPORT_TOOL_IDLE, params: 0, results: 0 },
    HostFunction { name: IMPORT_TOOL_ACTIVE, params: 0, results: 0 },
    HostFunction { name: IMPORT_TOOL_JOIN, params: 1, results: 0 },
    HostFunction { name: IMPORT_TOOL_DONE, params: 0, results: 0 },
];
/// The custom section both of a program's builds carry (language.md §15.3): eight bytes,
/// little-endian, the [`pair_id`] of its client and server modules, which a client and a server
/// compare to know they're one program's.
pub const PAIR_SECTION: &str = "wrela.pair";

/// The pair id of a program's modules, each without its [`PAIR_SECTION`]: FNV-1a 64 over each
/// one's length (eight bytes, little-endian, 0 for a role the program hasn't) and bytes, the
/// client's first.
pub fn pair_id(client: Option<&[u8]>, server: Option<&[u8]>) -> u64 {
    let mut hash = crate::hash::StateHash::new();
    for wasm in [client, server] {
        let bytes = wasm.unwrap_or(&[]);
        hash.update(&(bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    hash.value()
}

/// The pair id in a module's [`PAIR_SECTION`], if it has one: the module's sections are read
/// in order, and anything malformed gives `None`.
pub fn read_pair(wasm: &[u8]) -> Option<u64> {
    fn leb(b: &[u8], at: &mut usize) -> Option<usize> {
        let mut x = 0u32;
        for shift in (0..35).step_by(7) {
            let byte = *b.get(*at)?;
            *at += 1;
            if shift == 28 && byte > 15 {
                return None;
            }
            x |= u32::from(byte & 127) << shift;
            if byte < 128 {
                return Some(x as usize);
            }
        }
        None
    }
    let mut at = 8;
    if wasm.get(..8)? != b"\0asm\x01\0\0\0" {
        return None;
    }
    let mut pair = None;
    while at < wasm.len() {
        let id = wasm[at];
        at += 1;
        let size = leb(wasm, &mut at)?;
        let body = wasm.get(at..at.checked_add(size)?)?;
        at += size;
        if id != 0 {
            continue;
        }
        let mut k = 0;
        let len = leb(body, &mut k)?;
        if body.get(k..k.checked_add(len)?)? == PAIR_SECTION.as_bytes() {
            let id: [u8; 8] = body.get(k + len..)?.try_into().ok()?;
            pair.get_or_insert(u64::from_le_bytes(id));
        }
    }
    pair
}
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
/// runtime's constants ([`typescript`]), std's ([`wrela`]) and the test vectors ([`vectors`]).
/// `cargo run -p wrela-abi --bin generate` writes them.
pub fn generated_files() -> [(PathBuf, String); 3] {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    [
        (root.join(typescript::TS_PATH), typescript()),
        (root.join(wrela::WRELA_PATH), wrela::wrela()),
        (root.join(vectors::VECTORS_PATH), vectors::vectors()),
    ]
}

#[cfg(test)]
mod tests {
    /// A module's pair section reads back; one without, a cut one and not a module give none.
    #[test]
    fn a_pair_section_reads_back() {
        let header = b"\0asm\x01\0\0\0".to_vec();
        let id = super::pair_id(Some(b"client"), None);
        let mut wasm = header.clone();
        // A custom section: id 0, its size, the name's length and bytes, the eight bytes.
        let name = super::PAIR_SECTION.as_bytes();
        wasm.push(0);
        wasm.push((1 + name.len() + 8) as u8);
        wasm.push(name.len() as u8);
        wasm.extend_from_slice(name);
        wasm.extend_from_slice(&id.to_le_bytes());
        assert_eq!(super::read_pair(&wasm), Some(id));
        assert_eq!(super::read_pair(&header), None);
        assert_eq!(super::read_pair(&wasm[..wasm.len() - 1]), None);
        assert_eq!(super::read_pair(b"not a module"), None);
        // The ids of the two roles' modules differ by which is which.
        assert_ne!(super::pair_id(Some(b"a"), None), super::pair_id(None, Some(b"a")));
    }

    #[test]
    fn a_pair_section_rejects_malformed_modules() {
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        let name = super::PAIR_SECTION.as_bytes();
        wasm.extend([0, (1 + name.len() + 8) as u8, name.len() as u8]);
        wasm.extend(name);
        wasm.extend(7u64.to_le_bytes());
        assert_eq!(super::read_pair(&wasm), Some(7));

        let mut section_after_pair = wasm.clone();
        section_after_pair.extend([0, 1, 0]);
        assert_eq!(super::read_pair(&section_after_pair), Some(7));

        let mut bad_version = wasm.clone();
        bad_version[4] = 2;
        assert_eq!(super::read_pair(&bad_version), None);

        let mut truncated_after_pair = wasm.clone();
        truncated_after_pair.extend([0, 10]);
        assert_eq!(super::read_pair(&truncated_after_pair), None);

        // A section's length is u32 LEB128, including a padded representation.
        let mut oversized_length = wasm[..8].to_vec();
        oversized_length.extend([0, wasm[9] | 0x80, 0x80, 0x80, 0x80, 0x10]);
        oversized_length.extend_from_slice(&wasm[10..]);
        assert_eq!(super::read_pair(&oversized_length), None);
    }

    /// The checked-in generated files are this crate's output (the browser runtime and std
    /// read them).
    #[test]
    fn checked_in_copies_are_current() {
        for (path, text) in super::generated_files() {
            let actual = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                actual == text,
                "{} is stale; run `cargo run -p wrela-abi --bin generate`",
                path.display()
            );
        }
    }
}
