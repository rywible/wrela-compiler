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

mod cache;
mod error;
#[cfg(feature = "gpu")]
mod gpu;
#[cfg(feature = "gpu")]
pub mod image;
pub mod lock;
mod program;
mod shared;

pub use cache::compiled_code_prefix;
pub use error::{Error, Result};
#[cfg(feature = "gpu")]
pub use gpu::{
    GpuTiming, Timing, frame_spans, map_read, open_device, read_timestamp_spans, read_timestamps,
};
pub use program::{Failure, PostHandler, Ticked, Value};
pub use wrela_abi::input::{Event, Scripted, parse_script};
pub use wrela_abi::ticks::{TickLog, frame_time, lockstep_ticks};

#[cfg(feature = "gpu")]
use gpu::Gpu;
use program::Program;
use std::path::Path;
use wrela_abi::Manifest;

/// How to run a program.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Time every dispatch and pass with GPU timestamp queries ([`RunResult::timings`]): each
    /// one's start and end, beside the others, so a frame's span is its first start to its
    /// last end ([`frame_spans`]). The host then waits for the GPU at every submission, so use
    /// it for durations, not throughput. Fails to load if the adapter can't time.
    pub timestamps: bool,
    /// With `timestamps`, run each pass and dispatch alone: submitted and waited for before the
    /// next is recorded, so each one's time is its own (on Apple GPUs passes overlap, and one
    /// pass's start to end holds others' work).
    pub serial: bool,
    /// Keep a copy of every batch the program submits ([`Host::take_batches`]): for tests
    /// that look at what a program recorded, such as a dispatch's uniform bytes.
    pub record: bool,
    /// Where the program's storage requests read and write; by default, `storage/` in the
    /// build's directory.
    pub storage: Option<std::path::PathBuf>,
    /// How many threads run a parallel job's chunks, the program's own included (at most
    /// `wrela_abi::memory::MAX_WORKERS + 1`); 0 for [`default_workers`].
    pub workers: u32,
    /// What answers the program's posts (`std::io::post`); without it, they fail.
    pub post: Option<PostHandler>,
    /// Don't show the program's printed lines on stderr (`wrela: ...`): for a caller that
    /// shows them itself ([`Host::take_logs`]), such as `wrela studio`'s headless runner.
    pub quiet: bool,
    /// Don't run the program's `init` while loading: the caller runs it ([`Host::init`]),
    /// after asking for the ticker's first hash, say.
    pub defer_init: bool,
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

/// Where a build in `dir` reads and writes, with `storage` overriding its storage directory,
/// and what answers its posts.
fn io(dir: &Path, storage: Option<&Path>, post: Option<PostHandler>, quiet: bool) -> program::Io {
    program::Io {
        build: Some(dir.to_path_buf()),
        storage: Some(storage.map_or_else(|| dir.join("storage"), Path::to_path_buf)),
        post,
        quiet,
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

/// Why a tick log didn't replay.
#[derive(Debug)]
pub enum ReplayError {
    /// The log is for another build: its WASM hash isn't this build's.
    OtherBuild { log: u64, build: u64 },
    /// The program has no ticker after `init`.
    NoTicker,
    /// The log's rate isn't the ticker's.
    OtherRate { log: u32, ticker: u32 },
    /// The first world's hash, after `init`, isn't the log's.
    FirstWorld { log: u64, replayed: u64 },
    /// Tick `tick`'s hash isn't the log's: the first that differs.
    Tick { tick: u32, log: u64, replayed: u64 },
    /// The program failed, in tick `tick` if it was in one: it trapped, or the host couldn't
    /// run it.
    Failed { tick: Option<u32>, error: Error },
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hex = wrela_abi::hash::hex;
        match self {
            ReplayError::OtherBuild { log, build } => write!(
                f,
                "the log is for another build: its WASM hash is {}, this build's {}",
                hex(*log),
                hex(*build)
            ),
            ReplayError::NoTicker => write!(f, "the program starts no ticker in `init`"),
            ReplayError::OtherRate { log, ticker } => {
                write!(f, "the log's ticks are at {log} Hz, the ticker's at {ticker} Hz")
            }
            ReplayError::FirstWorld { log, replayed } => write!(
                f,
                "the first world differs: the log has {}, the replay {}",
                hex(*log),
                hex(*replayed)
            ),
            ReplayError::Tick { tick, log, replayed } => write!(
                f,
                "tick {tick} differs: the log has {}, the replay {}",
                hex(*log),
                hex(*replayed)
            ),
            ReplayError::Failed { error, .. } => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ReplayError {}

/// [`ReplayError::OtherBuild`] unless `log` is for the build whose WASM hash is `build`.
fn same_build(log: &TickLog, build: u64) -> Result<(), ReplayError> {
    if log.wasm_hash == build {
        Ok(())
    } else {
        Err(ReplayError::OtherBuild { log: log.wasm_hash, build })
    }
}

/// Compiles the build in `dir` and keeps its compiled code beside it, so the loads that follow
/// don't compile it again (`cache`).
pub fn precompile(dir: impl AsRef<Path>) -> Result<()> {
    let dir = dir.as_ref();
    let (_, wasm, _) = read_build(dir)?;
    let hash = wrela_abi::ticks::wasm_hash(&wasm);
    program::compile_cached(&wasm, hash, Some(dir), None).map(|_| ())
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
    wasm_hash: u64,
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
        let wasm_hash = wrela_abi::ticks::wasm_hash(&wasm);
        // Everything that can be checked without the GPU is, before taking it.
        let compiled = program::compile_cached(&wasm, wasm_hash, Some(dir), None)?;
        let lock = lock::GpuLock::acquire(&format!("wrela-host {}", dir.display()))?;
        let timing = match (options.timestamps, options.serial) {
            (false, _) => Timing::Off,
            (true, false) => Timing::Span,
            (true, true) => Timing::Serial,
        };
        let gpu = Gpu::new(&manifest, &shaders, timing)?;
        let io = io(dir, options.storage.as_deref(), options.post.clone(), options.quiet);
        let n = workers(options.workers);
        let program = Program::instantiate_with(
            &compiled,
            &manifest,
            gpu,
            io,
            options.record,
            n,
            !options.defer_init,
        )?;
        Ok(Host { program, wasm_hash, _lock: lock })
    }

    /// Calls `frame(times[i], width, height)` for each time in order, then reads the screen
    /// back.
    pub fn run_frames(&mut self, times: &[f32], width: u32, height: u32) -> Result<RunResult> {
        self.run_frames_with(times, width, height, &[])
    }

    /// Runs `frames` frames at `fps` in lockstep with the program's ticker, if it has one
    /// (test mode, #43 §2.3; [`CpuHost::lockstep_frame`]), then reads the screen back. With
    /// `log`, each tick's records and state hash go into a tick log, which it gives too (an
    /// error if the program has no ticker): ask for the first world's hash with
    /// [`Host::want_hashes`] before the program's `init`, which a [`Options::defer_init`] load
    /// leaves to [`Host::init`].
    pub fn run_lockstep(
        &mut self,
        frames: u32,
        fps: f64,
        width: u32,
        height: u32,
        script: &[Scripted],
        log: bool,
    ) -> Result<(RunResult, Option<TickLog>)> {
        self.program.executor().set_screen(width, height)?;
        let mut log = if log {
            self.program.want_hashes(true);
            Some(self.program.tick_log(self.wasm_hash)?)
        } else {
            None
        };
        for i in 0..frames {
            self.program.executor().set_frame(i as usize);
            self.program.lockstep_frame(i, fps, width, height, script, log.as_mut())?;
        }
        Ok((self.result(width, height)?, log))
    }

    /// Frame `i` of a lockstep run ([`Host::run_lockstep`]), alone: for a test that looks at the
    /// program between frames (its exports, the screen). Frames go in order from 0.
    pub fn lockstep_frame(
        &mut self,
        i: u32,
        fps: f64,
        width: u32,
        height: u32,
        script: &[Scripted],
    ) -> Result<()> {
        self.program.executor().set_screen(width, height)?;
        self.program.executor().set_frame(i as usize);
        self.program.lockstep_frame(i, fps, width, height, script, None)
    }

    /// [`CpuHost::ticks_beside_frames`], with the GPU: the frames draw.
    pub fn ticks_beside_frames(
        &mut self,
        ticks: u32,
        frames: u32,
        fps: f64,
        width: u32,
        height: u32,
    ) -> Result<Vec<(Ticked, f64)>> {
        self.program.executor().set_screen(width, height)?;
        self.program.ticks_beside_frames(ticks, frames, fps, width, height)
    }

    /// Runs the program's `init`, for a host loaded with [`Options::defer_init`].
    pub fn init(&mut self) -> Result<()> {
        self.program.run_init()
    }

    /// Whether the ticker reports its state's hash ([`CpuHost::want_hashes`]).
    pub fn want_hashes(&mut self, on: bool) {
        self.program.want_hashes(on);
    }

    /// [`Host::run_frames`], with `script`'s events queued before the frames they're for
    /// (wrela_abi `input`).
    pub fn run_frames_with(
        &mut self,
        times: &[f32],
        width: u32,
        height: u32,
        script: &[Scripted],
    ) -> Result<RunResult> {
        self.program.executor().set_screen(width, height)?;
        for (i, &time) in times.iter().enumerate() {
            for e in wrela_abi::input::events_at(script, i as u32) {
                self.program.push_input(e);
            }
            self.program.executor().set_frame(i);
            self.program.frame(time, width, height)?;
        }
        self.result(width, height)
    }

    /// The outcome of a run: the screen, read back, and the GPU durations recorded.
    fn result(&mut self, width: u32, height: u32) -> Result<RunResult> {
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

    /// Queues an input event, which the program reads from its next call on.
    pub fn push_input(&mut self, e: Event) {
        self.program.push_input(e);
    }

    /// Calls `frame(time, width, height)` once, and ends the frame: for a caller that drives
    /// the program a frame at a time (a studio session), and reads the screen when it wants.
    pub fn frame(&mut self, time: f32, width: u32, height: u32) -> Result<()> {
        self.program.executor().set_screen(width, height)?;
        self.program.frame(time, width, height)
    }

    /// The screen after the last frame: RGBA8, rows top to bottom.
    pub fn read_screen(&mut self) -> Result<Vec<u8>> {
        self.program.executor().read_screen()
    }

    /// The lines the program printed (`std::io::print`) since the last call.
    pub fn take_logs(&mut self) -> Vec<String> {
        self.program.take_logs()
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
    /// FNV-1a 64 of the WASM: which build a tick log is for.
    wasm_hash: u64,
    /// Whether its hosts keep a copy of every batch ([`CpuHost::take_batches`]).
    record: bool,
}

impl CpuBuild {
    /// Loads the build in `dir`; its shaders must exist but aren't compiled. Its compiled code
    /// is kept beside it, as a [`Host`]'s is.
    pub fn load(dir: impl AsRef<Path>) -> Result<CpuBuild> {
        CpuBuild::load_compiled(dir.as_ref(), None)
    }

    /// [`CpuBuild::load`], with each call (`init`, a frame, a tick, an export) given `fuel`
    /// units of work (about one per WASM instruction): a call that does more traps, after the
    /// same work on every machine. For tests that run a program's frames, whose build is made
    /// for the run: its compiled code isn't kept beside it, only in the directory every build
    /// shares, if there is one (`WRELA_NATIVE_CACHE`).
    pub fn load_metered(dir: impl AsRef<Path>, fuel: u64) -> Result<CpuBuild> {
        CpuBuild::load_compiled(dir.as_ref(), Some(fuel))
    }

    fn load_compiled(dir: &Path, fuel: Option<u64>) -> Result<CpuBuild> {
        let (manifest, wasm, _) = read_build(dir)?;
        let wasm_hash = wrela_abi::ticks::wasm_hash(&wasm);
        let beside = fuel.is_none().then_some(dir);
        let compiled = program::compile_cached(&wasm, wasm_hash, beside, fuel)?;
        Ok(CpuBuild { compiled, manifest, dir: dir.to_path_buf(), wasm_hash, record: false })
    }

    /// Its hosts keep a copy of every batch the program submits ([`CpuHost::take_batches`]):
    /// for tests that look at what a program recorded. A long run keeps none.
    pub fn recording(mut self) -> CpuBuild {
        self.record = true;
        self
    }

    /// FNV-1a 64 of its WASM ([`wrela_abi::ticks::wasm_hash`]).
    pub fn wasm_hash(&self) -> u64 {
        self.wasm_hash
    }

    /// Runs the program's ticks with no frames (`wrela-host --no-gpu`): `init`, then `ticks`
    /// ticks with `script`'s records (its tick-keyed events), each tick's state hash asked
    /// for: the tick log they make. Parallel work runs on `workers` threads (0 for the default).
    pub fn record_ticks(&self, ticks: u32, script: &[Scripted], workers: u32) -> Result<TickLog> {
        let mut host = self.hashing_host(workers)?;
        let mut log = host.program.tick_log(self.wasm_hash)?;
        for _ in 0..ticks {
            host.program.scripted_tick(script, Some(&mut log))?;
        }
        Ok(log)
    }

    /// Replays a tick log (`wrela-host --replay`): `init`, then each of the log's ticks
    /// ([`CpuHost::replay_ticks`]). How many ticks replayed. A log for another build fails
    /// before the program starts.
    pub fn replay(&self, log: &TickLog, workers: u32) -> Result<u32, ReplayError> {
        same_build(log, self.wasm_hash)?;
        let mut host = self
            .hashing_host(workers)
            .map_err(|error| ReplayError::Failed { tick: None, error })?;
        let n = log.ticks.len() as u32;
        host.replay_ticks(log, n)?;
        Ok(n)
    }

    /// A new instance after `init`, its ticker asked for each state's hash, the first world's
    /// too.
    fn hashing_host(&self, workers: u32) -> Result<CpuHost> {
        let mut host = self.instantiate_in(workers, None)?;
        host.want_hashes(true);
        host.init()?;
        Ok(host)
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
        let io = io(&self.dir, storage, None, false);
        let n = self::workers(workers);
        let (compiled, manifest) = (&self.compiled, &self.manifest);
        let program =
            Program::instantiate_with(compiled, manifest, NoGpu, io, self.record, n, false)?;
        Ok(CpuHost { program, wasm_hash: self.wasm_hash })
    }
}

/// A program's WASM alone: exports run under wasmtime, and what they submit is decoded,
/// sequenced and checked like a [`Host`]'s, then dropped. No GPU, no lock.
pub struct CpuHost {
    program: Program<NoGpu>,
    /// Its build's ([`CpuBuild::wasm_hash`]).
    wasm_hash: u64,
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

    /// Queues an input event, which the program reads from its next call on.
    pub fn push_input(&mut self, e: Event) {
        self.program.push_input(e);
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

    /// The ticker's rate, once the program has started one (`std::tick::start`).
    pub fn ticker_hz(&self) -> Option<u32> {
        self.program.ticker_hz()
    }

    /// Whether the ticker reports its state's hash: from `start` on (the first world's) if
    /// asked before [`CpuHost::init`].
    pub fn want_hashes(&mut self, on: bool) {
        self.program.want_hashes(on);
    }

    /// Queues input events as the ticker's records: the next ticks take them, at most
    /// `wrela_abi::memory::MAX_TICK_RECORDS` each.
    pub fn push_records(&mut self, events: impl IntoIterator<Item = Event>) {
        self.program.push_records(events);
    }

    /// Runs the ticker's next tick with the queued records: what it was given, and its state's
    /// hash if [`CpuHost::want_hashes`] asked for it. An error if there's no ticker, or the tick
    /// trapped (the program's panic).
    pub fn tick(&mut self) -> Result<Ticked> {
        self.program.tick()
    }

    /// Replays a tick log's first `ticks` ticks (all it has, if fewer), after `init`: checks the
    /// build, the ticker's rate and the first world's hash against the log's, then runs each
    /// tick with its records and checks its state hash. Stops at the first that differs, and
    /// names it. Ask for hashes before `init` ([`CpuHost::want_hashes`]), or the first world's
    /// has none.
    pub fn replay_ticks(&mut self, log: &TickLog, ticks: u32) -> Result<(), ReplayError> {
        same_build(log, self.wasm_hash)?;
        let hz = self.ticker_hz().ok_or(ReplayError::NoTicker)?;
        if hz != log.hz {
            return Err(ReplayError::OtherRate { log: log.hz, ticker: hz });
        }
        let first = self.program.reported_hash();
        if first != log.first {
            return Err(ReplayError::FirstWorld { log: log.first, replayed: first });
        }
        self.want_hashes(true);
        for (k, t) in (0..ticks).zip(&log.ticks) {
            self.program.push_raw_records(&t.records);
            let got = self.tick().map_err(|error| ReplayError::Failed { tick: Some(k), error })?;
            let replayed = got.hash.expect("hashes asked for");
            if replayed != t.hash {
                return Err(ReplayError::Tick { tick: k, log: t.hash, replayed });
            }
        }
        Ok(())
    }

    /// Runs frame `i` of a lockstep schedule at `fps` (test mode, #43 §2.3): the ticks that
    /// come before it, then the frame, with `script`'s events of frame `i` queued for the frame
    /// and the ticker, and its tick-keyed events as their ticks' records.
    pub fn lockstep_frame(
        &mut self,
        i: u32,
        fps: f64,
        width: u32,
        height: u32,
        script: &[Scripted],
    ) -> Result<()> {
        self.program.lockstep_frame(i, fps, width, height, script, None)
    }

    /// Makes each helper hold back a long job's result for `micros` microseconds once it's
    /// ready: tests slow jobs down, so the threads that take them wait.
    pub fn hold_jobs(&self, micros: u32) {
        self.program.hold_jobs(micros);
    }

    /// Runs `ticks` ticks (with no records, each asked for its hash) on an OS thread of their
    /// own while this one runs `frames` frames at `fps`, neither waiting for the other (test
    /// mode's paced schedule, #43 §2.3): each tick, and its time in milliseconds.
    pub fn ticks_beside_frames(
        &mut self,
        ticks: u32,
        frames: u32,
        fps: f64,
        width: u32,
        height: u32,
    ) -> Result<Vec<(Ticked, f64)>> {
        self.program.ticks_beside_frames(ticks, frames, fps, width, height)
    }
}
