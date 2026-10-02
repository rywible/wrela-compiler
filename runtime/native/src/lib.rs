//! # The native host
//!
//! Runs a built wrela program without a browser: the WASM under wasmtime, the command stream
//! under wgpu, headless (the screen is an offscreen texture). It serves tests, agents and
//! performance comparisons, and must agree with the browser runtime (`runtime/browser`): the
//! same state hash for the same frame times, and the same pixels up to GPU rounding.
//!
//! The contract both hosts implement is `wrela-abi` (runtime/abi). This crate never re-implements
//! the format: batches go through [`wrela_abi::stream::decode`] and
//! [`wrela_abi::stream::Sequencer`], manifests through [`wrela_abi::Manifest::parse`].
//!
//! ```no_run
//! let mut host = wrela_host::Host::load("runtime/fixtures/first-light")?;
//! let times: Vec<f32> = (0..60).map(|i| wrela_host::frame_time(i, 60.0)).collect();
//! let run = host.run_frames(&times, 640, 360)?;
//! println!("{}", run.hash_hex());
//! run.write_png("frame.png")?;
//! # Ok::<(), wrela_host::Error>(())
//! ```
//!
//! A [`Host`] holds the GPU lock ([`lock`]) for its lifetime. A [`CpuHost`] runs only the WASM:
//! it checks what the program submits but executes none of it, so it needs neither a GPU nor
//! the lock, for tests that call exports.

mod check;
mod error;
mod gpu;
pub mod image;
pub mod lock;
mod program;

pub use error::{Error, Result};
pub use gpu::{GpuTiming, SCREEN_FORMAT, map_read, open_device};
pub use program::Value;

use gpu::Gpu;
use program::Program;
use std::path::Path;
use wrela_abi::Manifest;

/// How to run a program.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Time every dispatch and screen pass with GPU timestamp queries ([`RunResult::timings`]).
    /// The host then waits for the GPU at every submission, so use it for durations, not
    /// throughput. Fails to load if the adapter can't time.
    pub timestamps: bool,
    /// Keep a copy of every batch the program submits ([`Host::take_batches`]): for tests
    /// that look at what a program recorded, such as a dispatch's uniform bytes.
    pub record: bool,
}

/// The outcome of [`Host::run_frames`].
#[derive(Clone, Debug)]
pub struct RunResult {
    /// FNV-1a 64 over every byte the program has submitted since it loaded.
    pub hash: u64,
    pub width: u32,
    pub height: u32,
    /// The screen after the last frame: RGBA8, rows top to bottom, no padding.
    pub frame: Vec<u8>,
    /// GPU durations, with [`Options::timestamps`]; empty otherwise.
    pub timings: Vec<GpuTiming>,
}

impl RunResult {
    /// The hash as both hosts print it: sixteen lowercase hex digits.
    pub fn hash_hex(&self) -> String {
        format!("{:016x}", self.hash)
    }

    pub fn write_png(&self, path: impl AsRef<Path>) -> Result<()> {
        image::write_png(path.as_ref(), self.width, self.height, &self.frame)
    }
}

/// The time of frame `i` at `fps` frames a second, as both hosts compute it: `i / fps` in f64,
/// rounded to f32.
pub fn frame_time(i: u32, fps: f64) -> f32 {
    (f64::from(i) / fps) as f32
}

/// Reads a build's manifest, WASM and WGSL from `dir`.
fn read_build(dir: &Path) -> Result<(Manifest, Vec<u8>, Vec<String>)> {
    let read = |name: &str| {
        let path = dir.join(name);
        std::fs::read(&path).map_err(|e| Error::io(path, e))
    };
    let manifest_path = dir.join("manifest.json");
    let manifest =
        std::fs::read_to_string(&manifest_path).map_err(|e| Error::io(&manifest_path, e))?;
    let manifest = Manifest::parse(&manifest)?;
    let wasm = read(&manifest.wasm)?;
    let shaders = manifest
        .pipelines
        .iter()
        .map(|p| {
            let path = dir.join(&p.shader);
            std::fs::read_to_string(&path).map_err(|e| Error::io(path, e))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((manifest, wasm, shaders))
}

/// A loaded program, ready to run.
pub struct Host {
    program: Program<Gpu>,
    // Declared last: dropped after the GPU.
    _lock: lock::GpuLock,
}

impl Host {
    /// Loads the build in `dir` (manifest.json, the WASM, the WGSL), opens the GPU and builds
    /// every pipeline.
    pub fn load(dir: impl AsRef<Path>) -> Result<Host> {
        Host::load_with(dir, &Options::default())
    }

    pub fn load_with(dir: impl AsRef<Path>, options: &Options) -> Result<Host> {
        let dir = dir.as_ref();
        let (manifest, wasm, shaders) = read_build(dir)?;
        // Everything that can be checked without the GPU is, before taking it.
        let compiled = program::compile(&wasm)?;
        let lock = lock::GpuLock::acquire(&format!("wrela-host {}", dir.display()))?;
        let gpu = Gpu::new(&manifest, &shaders, options.timestamps)?;
        let checker = check::Checker::new(&manifest, check::Limits::webgpu_defaults());
        let mut program = Program::instantiate(&compiled, checker, gpu)?;
        if options.record {
            program.record_batches();
        }
        Ok(Host { program, _lock: lock })
    }

    /// Calls `frame(times[i], width, height)` for each time in order, then reads the screen
    /// back.
    pub fn run_frames(&mut self, times: &[f32], width: u32, height: u32) -> Result<RunResult> {
        self.program.executor().set_screen(width, height)?;
        for (i, &time) in times.iter().enumerate() {
            self.program.executor().set_frame(i);
            self.program.frame(time, width, height)?;
        }
        let gpu = self.program.executor();
        let frame = gpu.read_screen()?;
        let timings = gpu.take_timings();
        Ok(RunResult { hash: self.program.hash().value(), width, height, frame, timings })
    }

    /// Calls one of the program's other exports with numeric arguments: CPU evaluation for
    /// tests. Anything it submits runs (and counts toward the hash).
    pub fn call_export(&mut self, name: &str, args: &[Value]) -> Result<Vec<Value>> {
        self.program.call(name, args)
    }

    /// Reads a GPU buffer back after a run. For tests: it waits for the GPU.
    pub fn read_buffer(&mut self, handle: u32) -> Result<Vec<u8>> {
        self.program.executor().read_buffer(handle)
    }

    /// The batches submitted since the last call (or load), with [`Options::record`]; decode
    /// them with [`wrela_abi::stream::decode`].
    pub fn take_batches(&mut self) -> Vec<Vec<u8>> {
        self.program.take_batches()
    }

    /// The GPU durations recorded since the last call (or load), with [`Options::timestamps`]:
    /// for timing work an export submits. It waits for the GPU.
    pub fn take_timings(&mut self) -> Result<Vec<GpuTiming>> {
        let gpu = self.program.executor();
        gpu.flush()?;
        Ok(gpu.take_timings())
    }
}

/// Executes nothing: for [`CpuHost`], whose commands are only checked.
struct NoGpu;

impl program::Executor for NoGpu {
    fn execute(&mut self, _cmd: &wrela_abi::stream::Command<'_>) -> Result<()> {
        Ok(())
    }

    fn end_frame(&mut self) -> Result<()> {
        Ok(())
    }
}

/// A program's WASM alone: exports run under wasmtime, and what they submit is decoded,
/// sequenced and checked like a [`Host`]'s, then dropped. No GPU, no lock.
pub struct CpuHost {
    program: Program<NoGpu>,
}

impl CpuHost {
    /// Loads the build in `dir`; its shaders must exist but aren't compiled.
    pub fn load(dir: impl AsRef<Path>) -> Result<CpuHost> {
        let (manifest, wasm, _) = read_build(dir.as_ref())?;
        let compiled = program::compile(&wasm)?;
        let checker = check::Checker::new(&manifest, check::Limits::webgpu_defaults());
        let mut program = Program::instantiate(&compiled, checker, NoGpu)?;
        program.record_batches();
        Ok(CpuHost { program })
    }

    /// Calls an export with numeric arguments.
    pub fn call_export(&mut self, name: &str, args: &[Value]) -> Result<Vec<Value>> {
        self.program.call(name, args)
    }

    /// Calls `frame(time, width, height)` and ends the frame.
    pub fn frame(&mut self, time: f32, width: u32, height: u32) -> Result<()> {
        self.program.frame(time, width, height)
    }

    /// The batches submitted since the last call (or load).
    pub fn take_batches(&mut self) -> Vec<Vec<u8>> {
        self.program.take_batches()
    }

    /// FNV-1a 64 of every byte submitted since the program loaded.
    pub fn hash(&self) -> u64 {
        self.program.hash().value()
    }

    /// The names and WASM types of the module's exported functions: `(params, results)`.
    pub fn exports(&mut self) -> Vec<(String, Vec<&'static str>, Vec<&'static str>)> {
        self.program.exports()
    }
}
