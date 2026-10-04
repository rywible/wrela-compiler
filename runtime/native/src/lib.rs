//! # The native host
//!
//! Runs a built wrela program without a browser: the WASM under wasmtime, the command stream
//! under wgpu, headless (the screen is an offscreen texture). It serves tests, agents and
//! performance comparisons, and must agree with the browser runtime (`runtime/browser`): the
//! same state hash for the same frame times, and the same pixels up to GPU rounding.
//!
//! The contract both hosts implement is `wrela-abi` (runtime/abi). This crate never re-implements
//! the format: batches go through [`wrela_abi::stream::decode`],
//! [`wrela_abi::stream::Sequencer`] and [`wrela_abi::check::Checker`], manifests through
//! [`wrela_abi::Manifest::parse`].
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

mod error;
#[cfg(feature = "gpu")]
mod gpu;
#[cfg(feature = "gpu")]
pub mod image;
pub mod lock;
mod program;
mod shared;

pub use error::{Error, Result};
#[cfg(feature = "gpu")]
pub use gpu::{GpuTiming, map_read, open_device, read_timestamps};
pub use program::{Failure, Value};

#[cfg(feature = "gpu")]
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
    /// Where the program's storage requests read and write; by default, `storage/` in the
    /// build's directory.
    pub storage: Option<std::path::PathBuf>,
    /// How many threads run a parallel job's chunks, the program's own included (at most
    /// `wrela_abi::memory::MAX_WORKERS + 1`); 0 for [`default_workers`].
    pub workers: u32,
}

/// The threads a program's parallel jobs run on by default: one per core, at most
/// `wrela_abi::memory::MAX_WORKERS + 1`.
pub fn default_workers() -> u32 {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    cores.min(wrela_abi::memory::MAX_WORKERS + 1)
}

fn workers(n: u32) -> u32 {
    if n == 0 { default_workers() } else { n }
}

/// Where a build in `dir` reads and writes, with `storage` overriding its storage directory.
fn io(dir: &Path, storage: Option<&Path>) -> program::Io {
    program::Io {
        build: Some(dir.to_path_buf()),
        storage: Some(storage.map_or_else(|| dir.join("storage"), Path::to_path_buf)),
    }
}

/// The outcome of [`Host::run_frames`].
#[cfg(feature = "gpu")]
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

#[cfg(feature = "gpu")]
impl RunResult {
    /// The hash as both hosts print it: sixteen lowercase hex digits.
    pub fn hash_hex(&self) -> String {
        wrela_abi::hash::hex(self.hash)
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
    let text = |name: &str| read_file(dir, name, |path| std::fs::read_to_string(path));
    let manifest = Manifest::parse(&text("manifest.json")?)?;
    let wasm = read_file(dir, &manifest.wasm, |path| std::fs::read(path))?;
    let shaders = manifest.pipelines.iter().map(|p| text(&p.shader)).collect::<Result<Vec<_>>>()?;
    Ok((manifest, wasm, shaders))
}

/// Reads the file `name` in `dir` with `read` (`std::fs::read` or `std::fs::read_to_string`).
fn read_file<T>(
    dir: &Path,
    name: &str,
    read: impl FnOnce(&Path) -> std::io::Result<T>,
) -> Result<T> {
    let path = dir.join(name);
    read(&path).map_err(|e| Error::io(path, e))
}

/// A loaded program, ready to run.
#[cfg(feature = "gpu")]
pub struct Host {
    program: Program<Gpu>,
    // Declared last: dropped after the GPU.
    _lock: lock::GpuLock,
}

#[cfg(feature = "gpu")]
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
        let compiled = program::compile_with(&wasm, None)?;
        let lock = lock::GpuLock::acquire(&format!("wrela-host {}", dir.display()))?;
        let gpu = Gpu::new(&manifest, &shaders, options.timestamps)?;
        let io = io(dir, options.storage.as_deref());
        let n = workers(options.workers);
        let program = Program::instantiate(&compiled, &manifest, gpu, io, options.record, n)?;
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

    /// Renders `quanta` quanta of the program's voice offline (as [`CpuHost::render_audio`]).
    pub fn render_audio(&mut self, quanta: u32) -> Result<Vec<f32>> {
        self.program.render_audio(quanta)
    }

    /// The handles of the GPU buffers the program holds, oldest first: for tests, to find a
    /// buffer to read back. A buffer the program dropped is gone.
    pub fn buffers(&mut self) -> Vec<u32> {
        self.program.executor().buffers()
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

    fn read_back(&mut self, _handle: u32, _offset: u32, _size: u32) -> Result<Vec<u8>> {
        Err(Error::Gpu("a CPU host has no GPU to read back from".into()))
    }
}

/// A build's WASM, compiled and checked once: each [`CpuBuild::start`] gives a new [`CpuHost`]
/// without compiling it again (for tests that run a program on many threads).
pub struct CpuBuild {
    manifest: Manifest,
    compiled: program::Compiled,
    dir: std::path::PathBuf,
}

impl CpuBuild {
    /// Loads the build in `dir`; its shaders must exist but aren't compiled.
    pub fn load(dir: impl AsRef<Path>) -> Result<CpuBuild> {
        CpuBuild::load_compiled(dir.as_ref(), None)
    }

    /// [`CpuBuild::load`], with each call (`init`, a frame, an export) given `fuel` units of
    /// work (about one per WASM instruction): a call that does more traps, after the same work
    /// on every machine. For tests that run a program's frames.
    pub fn load_metered(dir: impl AsRef<Path>, fuel: u64) -> Result<CpuBuild> {
        CpuBuild::load_compiled(dir.as_ref(), Some(fuel))
    }

    fn load_compiled(dir: &Path, fuel: Option<u64>) -> Result<CpuBuild> {
        let (manifest, wasm, _) = read_build(dir)?;
        let compiled = program::compile_with(&wasm, fuel)?;
        Ok(CpuBuild { compiled, manifest, dir: dir.to_path_buf() })
    }

    /// A new instance of the program: its own memory, batches and hash, and its parallel jobs
    /// on [`default_workers`] threads.
    pub fn start(&self) -> Result<CpuHost> {
        self.start_with(0)
    }

    /// [`CpuBuild::start`], with parallel jobs on `workers` threads (the caller's included; 0
    /// for the default).
    pub fn start_with(&self, workers: u32) -> Result<CpuHost> {
        let mut host = self.instantiate_in(workers, None)?;
        host.init()?;
        Ok(host)
    }

    /// [`CpuBuild::start_with`], with the program's storage requests reading and writing
    /// `storage` (by default, `storage/` in the build's directory), and without running
    /// `init`: call [`CpuHost::init`] next, which says why it failed if it does.
    pub fn instantiate_in(&self, workers: u32, storage: Option<&Path>) -> Result<CpuHost> {
        let io = io(&self.dir, storage);
        let n = self::workers(workers);
        let (compiled, manifest) = (&self.compiled, &self.manifest);
        let program = Program::instantiate_with(compiled, manifest, NoGpu, io, true, n, false)?;
        Ok(CpuHost { program })
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
        CpuBuild::load(dir)?.start()
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

    /// Why the last call failed, if it trapped: where, by WASM offset, and how.
    pub fn last_failure(&self) -> Option<&Failure> {
        self.program.last_failure()
    }

    /// Runs the program's `init`, for a host [`CpuBuild::instantiate_in`] made.
    pub fn init(&mut self) -> Result<()> {
        self.program.run_init()
    }

    /// The lines the program printed (`std::io::print`) since the last call.
    pub fn take_logs(&mut self) -> Vec<String> {
        self.program.take_logs()
    }

    /// How many chunks of parallel jobs the workers (not the program's own thread) have run.
    pub fn worker_chunks(&self) -> u32 {
        self.program.helped()
    }

    /// Renders `quanta` quanta of the program's voice offline: `wrela_abi::AUDIO_QUANTUM`
    /// samples each, mono, at `wrela_abi::AUDIO_SAMPLE_RATE`. Each call goes on from the last.
    pub fn render_audio(&mut self, quanta: u32) -> Result<Vec<f32>> {
        self.program.render_audio(quanta)
    }

    /// The names and WASM types of the module's exported functions: `(params, results)`.
    pub fn exports(&self) -> Vec<(String, Vec<&'static str>, Vec<&'static str>)> {
        self.program.exports()
    }
}
