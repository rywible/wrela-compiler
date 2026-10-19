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
//! A [`CpuHost`] runs only the WASM: it checks what the program submits but executes none
//! of it, so export tests need no GPU.

mod cache;
mod confined;
mod error;
#[cfg(feature = "gpu")]
mod gpu;
#[cfg(feature = "gpu")]
mod handover;
#[cfg(feature = "gpu")]
pub mod image;
pub mod loopback;
pub mod pages;
mod program;
pub mod rooms;
pub mod server;
mod shared;
pub mod tool;
mod trace;

pub use cache::compiled_code_prefix;
pub use error::{Error, Result};
#[cfg(feature = "gpu")]
pub use gpu::{GpuTiming, frame_spans, map_read, open_device, read_timestamps, submission_spans};
pub use trace::{Phase, Trace, Traced};
/// Chrome over the DevTools protocol, which the browser service and the page runs drive.
pub use wrela_chrome as chrome;
/// Build-time code's shared memory (constants, language.md §13), read and written by the
/// build's thread while its helpers wait or have stopped.
pub mod build_memory {
    pub use crate::shared::{slice, write};
}
pub use program::{
    BuildHelpers, Failure, HelperRun, HostHandler, PostHandler, Stop, Ticked, Value,
    metered_engine, refuse_host_functions,
};
pub use wrela_abi::input::{Event, Scripted, parse_script};
pub use wrela_abi::ticks::{TickLog, frame_time, lockstep_ticks};

#[cfg(feature = "gpu")]
use gpu::Gpu;
use program::Program;
use std::path::Path;
#[cfg(feature = "gpu")]
use std::path::PathBuf;
use wrela_abi::Manifest;

/// Whether passes and dispatches are timed with GPU timestamp queries, and how
/// ([`RunResult::timings`]). Timed, the host waits for the GPU at every submission, so use it
/// for durations, not throughput. A load fails if the adapter can't time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Timing {
    #[default]
    Off,
    /// Each one's start and end, beside the others, so a frame's span is its first start to its
    /// last end (`frame_spans`).
    Span,
    /// Each pass and dispatch alone: submitted and waited for before the next is recorded, so
    /// each one's time is its own (on Apple GPUs passes overlap, and one pass's start to end
    /// holds others' work).
    Serial,
}

/// How to run a program.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Whether every dispatch and pass is timed on the GPU, and how.
    pub timing: Timing,
    /// Keep a copy of every batch the program submits ([`Host::take_batches`]): for tests
    /// that look at what a program recorded, such as a dispatch's uniform bytes.
    pub record: bool,
    /// Where the program's storage requests read and write; by default, `storage/` in the
    /// directory of the build the host loaded (or restarted), which every build after it in
    /// the host, by a hot reload or a hand-over, shares.
    pub storage: Option<std::path::PathBuf>,
    /// How many threads run a parallel job's chunks, the program's own included (at most
    /// `wrela_abi::memory::MAX_WORKERS + 1`); 0 for [`default_workers`].
    pub workers: u32,
    /// What answers the program's posts (`std::io::post`); without it, they fail.
    pub post: Option<PostHandler>,
    /// Named application-host capabilities (`std::host::request`); unsupported without this.
    pub host: Option<HostHandler>,
    /// Don't show the program's printed lines on stderr (`wrela: ...`): for a caller that
    /// shows them itself ([`Host::take_logs`]), such as `wrela studio`'s headless runner.
    pub quiet: bool,
    /// Don't run the program's `init` while loading: the caller runs it ([`Host::init`]),
    /// after asking for the ticker's first hash, say.
    pub defer_init: bool,
    /// The host may hot reload ([`Host::reload`]): it keeps each tick's records for the new
    /// build to run again. (Without it, nothing grows tick by tick.)
    pub reloadable: bool,
}

impl Options {
    /// Where a build in `dir` loaded this way reads and writes ([`io`]).
    #[cfg(feature = "gpu")]
    fn io(&self, dir: &Path) -> program::Io {
        io(dir, self.storage.as_deref(), self.post.clone(), self.host.clone(), self.quiet)
    }

    /// These options with their storage in the build in `dir`'s, unless they name it: the
    /// storage of the builds a host runs after it.
    #[cfg(feature = "gpu")]
    fn homed(&self, dir: &Path) -> Options {
        let mut options = self.clone();
        options.storage.get_or_insert_with(|| dir.join("storage"));
        options
    }
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
fn io(
    dir: &Path,
    storage: Option<&Path>,
    post: Option<PostHandler>,
    host: Option<HostHandler>,
    quiet: bool,
) -> program::Io {
    program::Io {
        build: Some(dir.to_path_buf()),
        build_dir: None,
        storage: Some(storage.map_or_else(|| dir.join("storage"), Path::to_path_buf)),
        post,
        host,
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
    /// GPU durations, with [`Options::timing`]; empty otherwise.
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
    /// Tick `tick` has more records than one ticker call can consume.
    TooManyRecords { tick: u32, count: usize },
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
            ReplayError::TooManyRecords { tick, count } => write!(
                f,
                "tick {tick} has {count} records; a tick takes at most {}",
                wrela_abi::memory::MAX_TICK_RECORDS
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
    compile_build(dir.as_ref(), None).map(|_| ())
}

/// A build read from its directory, its WASM compiled.
struct Build {
    manifest: Manifest,
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    shaders: Vec<String>,
    /// FNV-1a 64 of the WASM: which build a tick log is for.
    wasm_hash: u64,
    compiled: program::Compiled,
}

/// Reads the build in `dir` and compiles its WASM, each call given `fuel` if it's metered. An
/// unmetered build's compiled code is kept beside it (`cache`): the loads that follow read it
/// back.
fn compile_build(dir: &Path, fuel: Option<u64>) -> Result<Build> {
    compile_build_with_shaders(dir, fuel, true)
}

/// CPU frame tests emit only the manifest and WASM; other builds still check every file.
fn compile_build_with_shaders(dir: &Path, fuel: Option<u64>, shaders: bool) -> Result<Build> {
    let (manifest, wasm, shaders) = read_build(dir, shaders)?;
    let wasm_hash = wrela_abi::ticks::wasm_hash(&wasm);
    let beside = fuel.is_none().then_some(dir);
    // An optimized build keeps its trap locations beside its WASM; without them, a trap gives
    // its code offset.
    let sidecar = Path::new(&manifest.wasm).with_extension(wrela_abi::lines::SIDECAR);
    let sidecar = std::fs::read(dir.join(sidecar)).ok();
    let compiled = program::compile_cached(&wasm, wasm_hash, beside, fuel, sidecar.as_deref())?;
    Ok(Build { manifest, shaders, wasm_hash, compiled })
}

/// A build's directory, opened: a driven build reads its files, and its program fetches, only
/// through it.
pub use cap_std::fs::Dir as BuildDir;

/// The directory at `path`, opened as a [`BuildDir`].
pub fn open_build_dir(path: &Path) -> std::io::Result<BuildDir> {
    BuildDir::open_ambient_dir(path, cap_std::ambient_authority())
}

/// The most bytes a driven build's file may have.
const MOST_DRIVEN_FILE: u64 = 512 << 20;

/// A build read through its directory's handle `build`, compiled to be driven (the tool host's
/// `drive`, language.md §15.5): on the interruptible engine, its calls ended by `stop`, and with
/// no compiled code read from or kept anywhere. `shaders` reads its WGSL too. No file is
/// reached through a link.
fn driven_build(build: &BuildDir, stop: Stop, shaders: bool) -> Result<Build> {
    let read = |name: &str| confined::read(build, name, MOST_DRIVEN_FILE);
    let file = |name: &str| read(name).map_err(|why| Error::Program(format!("{name}: {why}")));
    let text = |bytes: Vec<u8>, name: &str| {
        String::from_utf8(bytes).map_err(|_| Error::Program(format!("{name} isn't UTF-8")))
    };
    let manifest = Manifest::parse(&text(file("manifest.json")?, "manifest.json")?)?;
    let wasm = file(&manifest.wasm)?;
    let shaders = if shaders {
        let mut out = Vec::new();
        for p in &manifest.pipelines {
            out.push(text(file(&p.shader)?, &p.shader)?);
        }
        out
    } else {
        Vec::new()
    };
    let wasm_hash = wrela_abi::ticks::wasm_hash(&wasm);
    let sidecar = Path::new(&manifest.wasm).with_extension(wrela_abi::lines::SIDECAR);
    let sidecar = read(&sidecar.to_string_lossy()).ok();
    let compiled = program::compile_interruptible(&wasm, sidecar.as_deref(), stop)?;
    Ok(Build { manifest, shaders, wasm_hash, compiled })
}

/// Reads a build's manifest, WASM and WGSL from `dir`.
fn read_build(dir: &Path, shaders: bool) -> Result<(Manifest, Vec<u8>, Vec<String>)> {
    let text = |name: &str| read_file(dir, name, |path| std::fs::read_to_string(path));
    let manifest = Manifest::parse(&text("manifest.json")?)?;
    let wasm = read_file(dir, &manifest.wasm, |path| std::fs::read(path))?;
    let shaders = if shaders {
        manifest.pipelines.iter().map(|p| text(&p.shader)).collect::<Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
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
    /// Always a program, but while a hot reload swaps it ([`Host::reload`]).
    program: Option<Program<Gpu>>,
    /// The running build, and its directory and how it was loaded: what a hot reload loads the
    /// next build with, and what starts again when a hand-over fails.
    build: Build,
    dir: PathBuf,
    options: Options,
    /// The builds the program got ready to hand over to (`std::reload::prepare`).
    prepared: handover::PreparedBuilds,
    /// Preparations let go before they finished: dropped only after their threads end.
    cancelled_preparations: Vec<handover::Preparation>,
}

#[cfg(feature = "gpu")]
impl Drop for Host {
    fn drop(&mut self) {
        self.clear_prepared();
    }
}

#[cfg(feature = "gpu")]
impl Host {
    fn p(&mut self) -> &mut Program<Gpu> {
        self.program.as_mut().expect("the host has a program")
    }

    /// `build`, in `dir`, started in the place of the program `carried` comes from, on its
    /// device (`build`'s pipelines taken from the spares where they can be): given what it
    /// kept, its unread input and, if it has any, its ticks to run again. `run_init` runs its
    /// `init` too.
    fn start_carrying(
        &self,
        build: &Build,
        dir: &Path,
        mut carried: program::Retired<Gpu>,
        run_init: bool,
    ) -> std::result::Result<Program<Gpu>, (Error, Box<program::Retired<Gpu>>)> {
        if let Err(error) =
            program::Executor::reload(&mut carried.executor, &build.manifest, &build.shaders)
        {
            return Err((error, Box::new(carried)));
        }
        let mut program = Program::instantiate_carrying_recover(
            &build.compiled,
            &build.manifest,
            carried,
            self.options.io(dir),
            workers(self.options.workers),
            false,
        )?;
        program.hand_over_here();
        if run_init { program.init_recover() } else { Ok(program) }
    }

    /// Starts the old build again after a candidate failed, preserving the host for another call.
    fn install_program(&mut self, program: Program<Gpu>) {
        // Pending prepare request IDs belong to the retired Wasm instance. A new instance
        // starts numbering them again; only the prepared builds themselves can carry over.
        for prepared in self.prepared.values_mut() {
            if let handover::Prepared::Going(_, waiting) = prepared {
                waiting.clear();
            }
        }
        self.program = Some(program);
    }

    fn restore_old(&mut self, carried: program::Retired<Gpu>, run_init: bool) -> Result<()> {
        let checkpoint = carried.checkpoint();
        match self.start_carrying(&self.build, &self.dir, carried, run_init) {
            Ok(program) => {
                self.install_program(program);
                Ok(())
            }
            Err((error, failed)) if run_init => {
                // `init` may have become fallible after the old build first started (for
                // example, its storage changed). Keep the host callable so init can be retried.
                let program = self
                    .start_carrying(&self.build, &self.dir, checkpoint.restore(*failed), false)
                    .map_err(|(retry, _)| {
                        Error::Program(format!(
                            "the old build could not restart ({error}); starting it without init also failed ({retry})"
                        ))
                    })?;
                self.install_program(program);
                Err(error)
            }
            Err((error, _)) => Err(error),
        }
    }

    fn recover_failed(
        &mut self,
        carried: program::Retired<Gpu>,
        run_init: bool,
        error: Error,
    ) -> Error {
        match self.restore_old(carried, run_init) {
            Ok(()) => error,
            Err(restore) if self.program.is_some() => Error::Program(format!(
                "the replacement failed ({error}); the old build's init failed ({restore}); the host resumed with init deferred"
            )),
            Err(restore) => Error::Program(format!(
                "the replacement failed ({error}); the old build could not restart ({restore})"
            )),
        }
    }

    fn clear_prepared(&mut self) {
        for prepared in std::mem::take(&mut self.prepared).into_values() {
            if let handover::Prepared::Going(job, _) = prepared {
                let _ = job.join();
            }
        }
        for job in self.cancelled_preparations.drain(..) {
            let _ = job.join();
        }
    }

    /// Hot reload: replaces the running program with the build in `dir` (by default the one
    /// it was loaded from, rebuilt), in this process, on the same device. The new build starts
    /// with `init`; what the old one kept (`std::reload::keep`) is what its `kept()` gives;
    /// its ticker, if it starts one, runs the old one's ticks again first, each with the
    /// records it had, so the sim is where the old one was (as the new code runs it); input
    /// the old one hadn't read is the new one's. A pipeline whose WGSL and manifest entry
    /// are unchanged is kept. Panics unless the host was loaded [`Options::reloadable`].
    pub fn reload(&mut self, dir: Option<&Path>) -> Result<()> {
        assert!(self.options.reloadable, "a hot reload needs a host loaded `reloadable`");
        let dir = dir.map_or_else(|| self.dir.clone(), Path::to_path_buf);
        let build = compile_build(&dir, None)?;
        let old = self.program.take().expect("the host has a program");
        let old_init_done = old.init_done();
        let carried = old.retire();
        let checkpoint = carried.checkpoint();
        let program = match self.start_carrying(&build, &dir, carried, true) {
            Ok(program) => program,
            Err((error, carried)) => {
                return Err(self.recover_failed(
                    checkpoint.restore(*carried),
                    old_init_done,
                    error,
                ));
            }
        };
        self.install_program(program);
        self.build = build;
        self.dir = dir;
        self.settle_handing()
    }

    /// The same build again from its start, on the same device, as `options` says (its
    /// storage, its workers, whether `init` waits): a new program, with nothing of the last
    /// one's (its buffers and textures released; nothing kept, no ticks carried). The pipelines
    /// are kept. For tests that each start fresh on one host (`wrela test`'s GPU frame tests).
    pub fn restart_with(&mut self, options: &Options) -> Result<()> {
        self.restart_in(self.dir.clone(), options)
    }

    /// Starts the build in `dir` afresh, keeping the device and unchanged pipelines. Nothing
    /// of the old program is carried: its resources, screen, requests and recorded state go;
    /// the new program reads files and, by default, storage from `dir`. Changing the timing
    /// option opens a device with that capability.
    pub fn restart_in(&mut self, dir: impl AsRef<Path>, options: &Options) -> Result<()> {
        let dir = dir.as_ref();
        let options = &options.homed(dir);
        let build = compile_build(dir, None)?;
        let same_device = options.timing == self.options.timing;
        // A preparation belongs to the retiring program. On the same device, finish its GPU
        // work before resetting resources. With a new device, keep it until the replacement
        // starts, so a failed attempt leaves its pending requests intact.
        if same_device {
            self.clear_prepared();
        }
        let mut program = if same_device {
            let old = self.program.take().expect("the host has a program");
            let old_init_done = old.init_done();
            let (mut fresh, saved) = old.retire().fresh_for_restart(options.record);
            if let Err(error) = fresh.executor.restart_build(&build.manifest, &build.shaders) {
                return Err(self.recover_failed(saved.restore(fresh), old_init_done, error));
            }
            let started = Program::instantiate_carrying_recover(
                &build.compiled,
                &build.manifest,
                fresh,
                options.io(dir),
                workers(options.workers),
                false,
            )
            .and_then(|mut program| {
                program.hand_over_here();
                if options.defer_init { Ok(program) } else { program.init_recover() }
            });
            match started {
                Ok(program) => program,
                Err((error, failed)) => {
                    return Err(self.recover_failed(saved.restore(*failed), old_init_done, error));
                }
            }
        } else {
            let executor = Gpu::new(&build.manifest, &build.shaders, options.timing)?;
            let mut program = Program::instantiate_with(
                &build.compiled,
                &build.manifest,
                executor,
                options.io(dir),
                options.record,
                workers(options.workers),
                false,
            )?;
            program.hand_over_here();
            if options.defer_init {
                program
            } else {
                program.init_recover().map_err(|(error, _)| error)?
            }
        };
        if !same_device {
            self.clear_prepared();
        }
        if options.reloadable {
            program.keep_ticks();
        }
        program.hand_over_here();
        self.install_program(program);
        self.build = build;
        self.dir = dir.to_path_buf();
        self.options = options.clone();
        self.settle_handing()
    }

    /// Hot reload of a literal: in a lifted build (`wrela build --lift`), literal `i` takes
    /// `value`, as `std::lift::set` gives it, from the program's next call on (CPU and GPU).
    pub fn set_literal(&mut self, i: u32, value: f32) -> Result<()> {
        let args = [Value::I32(i as i32), Value::F32(value)];
        self.p().call(wrela_abi::EXPORT_LIFT_SET, &args).map(|_| ())
    }

    /// Loads the build in `dir` (manifest.json, the WASM, the WGSL), opens the GPU and builds
    /// every pipeline.
    pub fn load(dir: impl AsRef<Path>) -> Result<Host> {
        Host::load_with(dir, &Options::default())
    }

    pub fn load_with(dir: impl AsRef<Path>, options: &Options) -> Result<Host> {
        let dir = dir.as_ref();
        let options = &options.homed(dir);
        let build = compile_build(dir, None)?;
        Host::start(build, dir, options, true, None)
    }

    /// The build in the directory `build`, opened at `dir`, to be driven (the tool host's
    /// `drive`, language.md §15.5): its calls end when `stop` is due, on each of its threads,
    /// and it keeps and reads no compiled code. Its files, and its program's fetches, are read
    /// through `build` alone, so a link put in place of `dir` later leads nowhere. It can't hand
    /// over to another build or hot reload (a request to fails). Its storage is
    /// `options.storage`, which a driven build must name.
    pub fn load_driven(build: BuildDir, dir: &Path, stop: Stop, options: &Options) -> Result<Host> {
        if options.storage.is_none() || options.reloadable {
            return Err(Error::Program(
                "a driven build names its storage, and isn't reloadable".into(),
            ));
        }
        let compiled = driven_build(&build, stop, true)?;
        Host::start(compiled, dir, options, false, Some(std::sync::Arc::new(build)))
    }

    /// Starts `build`, in `dir`, on a new device; `hands_over` lets it hand over to other builds,
    /// and its fetches read through `handle` if it's given.
    fn start(
        build: Build,
        dir: &Path,
        options: &Options,
        hands_over: bool,
        handle: Option<std::sync::Arc<BuildDir>>,
    ) -> Result<Host> {
        let gpu = Gpu::new(&build.manifest, &build.shaders, options.timing)?;
        let n = workers(options.workers);
        let mut io = options.io(dir);
        io.build_dir = handle;
        let mut program = Program::instantiate_with(
            &build.compiled,
            &build.manifest,
            gpu,
            io,
            options.record,
            n,
            false,
        )?;
        if options.reloadable {
            program.keep_ticks();
        }
        if hands_over {
            program.hand_over_here();
        }
        let mut host = Host {
            program: Some(program),
            build,
            dir: dir.to_path_buf(),
            options: options.clone(),
            prepared: handover::PreparedBuilds::new(),
            cancelled_preparations: Vec::new(),
        };
        if !options.defer_init {
            host.init()?;
        }
        Ok(host)
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
        self.run_lockstep_seen(frames, fps, width, height, script, log, 0, |_, _| Ok(()))
    }

    /// [`Host::run_lockstep`], with every `every`th frame's screen (0: none) read back as it's
    /// drawn and given to `seen` with its frame's number: a clip's frames.
    #[allow(clippy::too_many_arguments)]
    pub fn run_lockstep_seen(
        &mut self,
        frames: u32,
        fps: f64,
        width: u32,
        height: u32,
        script: &[Scripted],
        log: bool,
        every: u32,
        seen: impl FnMut(u32, &[u8]) -> Result<()>,
    ) -> Result<(RunResult, Option<TickLog>)> {
        self.run_lockstep_seen_from(frames, fps, width, height, script, log, every, 0, seen)
    }

    /// [`Host::run_lockstep_seen`], capturing only frames at or after `first`. Earlier frames
    /// still run in full, so simulation and temporal rendering reach the same state, without
    /// reading back screens the caller would discard. The `every` schedule stays based at 0.
    #[allow(clippy::too_many_arguments)]
    pub fn run_lockstep_seen_from(
        &mut self,
        frames: u32,
        fps: f64,
        width: u32,
        height: u32,
        script: &[Scripted],
        log: bool,
        every: u32,
        first: u32,
        mut seen: impl FnMut(u32, &[u8]) -> Result<()>,
    ) -> Result<(RunResult, Option<TickLog>)> {
        program::valid_fps(fps)?;
        self.p().executor().set_screen(width, height)?;
        let mut log = if log {
            self.p().want_hashes(true);
            let hash = self.build.wasm_hash;
            Some(self.p().tick_log(hash)?)
        } else {
            None
        };
        for i in 0..frames {
            self.p().executor().set_frame(i as usize);
            self.p().lockstep_frame(i, fps, width, height, script, log.as_mut())?;
            self.settle_handing()?;
            if every > 0 && i >= first && i % every == 0 {
                let screen = self.p().executor().read_screen()?;
                seen(i, &screen)?;
            }
        }
        Ok((self.result(width, height)?, log))
    }

    /// Runs in lockstep until the program prints a line holding `until` (`std::io::print`), at
    /// most `frames` frames: that frame's screen (a capture's still), or none if it never did.
    pub fn run_lockstep_until(
        &mut self,
        frames: u32,
        fps: f64,
        width: u32,
        height: u32,
        script: &[Scripted],
        until: &str,
    ) -> Result<Option<RunResult>> {
        program::valid_fps(fps)?;
        self.p().executor().set_screen(width, height)?;
        self.p().take_logs();
        for i in 0..frames {
            self.p().executor().set_frame(i as usize);
            self.p().lockstep_frame(i, fps, width, height, script, None)?;
            self.settle_handing()?;
            if self.p().take_logs().iter().any(|line| line.contains(until)) {
                return Ok(Some(self.result(width, height)?));
            }
        }
        Ok(None)
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
        self.p().executor().set_screen(width, height)?;
        self.p().executor().set_frame(i as usize);
        self.p().lockstep_frame(i, fps, width, height, script, None)?;
        self.settle_handing()
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
        self.p().executor().set_screen(width, height)?;
        let ticked = self.p().ticks_beside_frames(ticks, frames, fps, width, height)?;
        self.settle_handing()?;
        Ok(ticked)
    }

    /// Runs the program's `init`, for a host loaded with [`Options::defer_init`].
    pub fn init(&mut self) -> Result<()> {
        self.p().run_init()?;
        self.settle_handing()
    }

    /// The last trap or panic, where it happened: [`CpuHost::last_failure`], on the GPU.
    pub fn last_failure(&self) -> Option<&Failure> {
        self.program.as_ref().and_then(|p| p.last_failure())
    }

    /// Whether the ticker reports its state's hash ([`CpuHost::want_hashes`]).
    pub fn want_hashes(&mut self, on: bool) {
        self.p().want_hashes(on);
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
        self.p().executor().set_screen(width, height)?;
        for (i, &time) in times.iter().enumerate() {
            for s in wrela_abi::input::events_at(script, i as u32) {
                self.p().push_scripted(s);
            }
            self.p().executor().set_frame(i);
            self.p().frame(time, width, height)?;
            self.settle_handing()?;
        }
        self.result(width, height)
    }

    /// The outcome of a run: the screen, read back, and the GPU durations recorded.
    fn result(&mut self, width: u32, height: u32) -> Result<RunResult> {
        let gpu = self.p().executor();
        let frame = gpu.read_screen()?;
        let timings = gpu.take_timings()?;
        let hash = self.p().hash().value();
        Ok(RunResult { hash, width, height, frame, timings })
    }

    /// Calls one of the program's other exports with numeric arguments: CPU evaluation for
    /// tests. Anything it submits runs (and counts toward the hash).
    pub fn call_export(&mut self, name: &str, args: &[Value]) -> Result<Vec<Value>> {
        let values = self.p().call(name, args)?;
        self.settle_handing()?;
        Ok(values)
    }

    /// Renders `quanta` quanta of the program's voice offline (as [`CpuHost::render_audio`]).
    pub fn render_audio(&mut self, quanta: u32) -> Result<Vec<f32>> {
        self.p().render_audio(quanta)
    }

    /// Queues an input event, which the program reads from its next call on.
    pub fn push_input(&mut self, e: Event) {
        self.p().push_input(e);
    }

    /// Queues a script's event (a message with its bytes), which the program reads from its
    /// next call on.
    pub fn push_scripted(&mut self, s: &Scripted) {
        self.p().push_scripted(s);
    }

    /// Queues a message of `bytes` from connection `conn` (language.md §17.2).
    pub fn push_message(&mut self, conn: u32, bytes: Vec<u8>) {
        self.p().push_message(conn, bytes);
    }

    /// Calls `frame(time, width, height)` once, and ends the frame: for a caller that drives
    /// the program a frame at a time (a studio session), and reads the screen when it wants.
    pub fn frame(&mut self, time: f32, width: u32, height: u32) -> Result<()> {
        self.p().executor().set_screen(width, height)?;
        self.p().frame(time, width, height)?;
        self.settle_handing()
    }

    /// The directory of the build that's running: another after a hand-over
    /// (`std::reload::hand_over`).
    pub fn build_dir(&self) -> &Path {
        &self.dir
    }

    /// How many pipelines the current device has built: what a hand-over, reload or restart
    /// built, by the difference. Restarts that change GPU capabilities open a new device.
    pub fn pipelines_built(&mut self) -> usize {
        self.p().executor().pipelines_built()
    }

    /// The screen after the last frame: RGBA8, rows top to bottom.
    pub fn read_screen(&mut self) -> Result<Vec<u8>> {
        self.p().executor().read_screen()
    }

    /// The screen's pixels from (`x`, `y`), `width` × `height` of them, after the last frame:
    /// RGBA8, rows top to bottom. An error unless all are on the screen.
    pub fn read_screen_region(
        &mut self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>> {
        self.p().executor().read_screen_region([x, y], [width, height])
    }

    /// The lines the program printed (`std::io::print`) since the last call.
    pub fn take_logs(&mut self) -> Vec<String> {
        self.p().take_logs()
    }

    /// What the program printed and timed (`std::io::print`, `std::time::phase`), which its
    /// threads share.
    pub fn trace(&mut self) -> Trace {
        self.p().trace()
    }

    /// The handles of the GPU buffers the program holds, oldest first: for tests, to find a
    /// buffer to read back. A buffer the program dropped is gone.
    pub fn buffers(&mut self) -> Vec<u32> {
        self.p().executor().buffers()
    }

    /// Reads a GPU buffer back after a run. For tests: it waits for the GPU.
    pub fn read_buffer(&mut self, handle: u32) -> Result<Vec<u8>> {
        self.p().executor().read_buffer(handle)
    }

    /// The batches submitted since the last call (or load), with [`Options::record`]; decode
    /// them with [`wrela_abi::stream::decode`].
    pub fn take_batches(&mut self) -> Vec<Vec<u8>> {
        self.p().take_batches()
    }

    /// The GPU durations recorded since the last call (or load), with [`Options::timing`]:
    /// for timing work an export submits. It waits for the GPU.
    pub fn take_timings(&mut self) -> Result<Vec<GpuTiming>> {
        let gpu = self.p().executor();
        gpu.take_timings()
    }

    /// The GPU this host runs on: its adapter's name and driver.
    pub fn adapter(&mut self) -> String {
        self.p().executor().adapter()
    }

    /// FNV-1a 64 over every byte the program has submitted since it loaded.
    pub fn hash(&mut self) -> u64 {
        self.p().hash().value()
    }

    /// Makes each helper hold back a task's result for `micros` microseconds once it's ready
    /// ([`CpuHost::hold_jobs`]).
    pub fn hold_jobs(&mut self, micros: u32) {
        self.p().hold_jobs(micros);
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
    /// Whether the caller shows the program's printed lines itself.
    quiet: bool,
    /// A driven build's directory, opened: its programs fetch through it.
    handle: Option<std::sync::Arc<BuildDir>>,
}

impl CpuBuild {
    /// Loads the build in `dir`; its shaders must exist but aren't compiled. Its compiled code
    /// is kept beside it, as a [`Host`]'s is.
    pub fn load(dir: impl AsRef<Path>) -> Result<CpuBuild> {
        CpuBuild::load_compiled(dir.as_ref(), None)
    }

    /// A partial build for CPU tools: manifest and WASM only. No shader files are needed;
    /// submitted GPU commands are checked against the manifest and then discarded.
    pub fn load_cpu(dir: impl AsRef<Path>) -> Result<CpuBuild> {
        let dir = dir.as_ref();
        let build = compile_build_with_shaders(dir, None, false)?;
        Ok(Self::from_build(dir, build))
    }

    /// [`CpuBuild::load`], with each call (`init`, a frame, a tick, an export) given `fuel`
    /// units of work (about one per WASM instruction): a call that does more traps, after the
    /// same work on every machine. For tests that run a program's frames, whose build is made
    /// for the run: its compiled code isn't kept beside it, only in the directory every build
    /// shares, if there is one (`WRELA_NATIVE_CACHE`).
    pub fn load_metered(dir: impl AsRef<Path>, fuel: u64) -> Result<CpuBuild> {
        CpuBuild::load_compiled(dir.as_ref(), Some(fuel))
    }

    /// A CPU frame test's partial build: only manifest and WASM, with no emitted shader
    /// files. Commands still undergo the manifest's resource and sequencing checks, but
    /// no shader runs. The compiler checks shader source before emitting this build.
    pub fn load_metered_cpu(dir: impl AsRef<Path>, fuel: u64) -> Result<CpuBuild> {
        let dir = dir.as_ref();
        let build = compile_build_with_shaders(dir, Some(fuel), false)?;
        Ok(Self::from_build(dir, build))
    }

    fn load_compiled(dir: &Path, fuel: Option<u64>) -> Result<CpuBuild> {
        Ok(Self::from_build(dir, compile_build(dir, fuel)?))
    }

    /// The build in the directory `build`, opened at `dir`, to be driven (the tool host's
    /// `drive`, language.md §15.5): its hosts' calls end when `stop` is due, on each of their
    /// threads, and it keeps and reads no compiled code. Its shaders aren't read. Its files, and
    /// its programs' fetches, are read through `build` alone.
    pub fn load_driven(build: BuildDir, dir: &Path, stop: Stop) -> Result<CpuBuild> {
        let mut out = Self::from_build(dir, driven_build(&build, stop, false)?).quiet();
        out.handle = Some(std::sync::Arc::new(build));
        Ok(out)
    }

    fn from_build(dir: &Path, build: Build) -> CpuBuild {
        let Build { manifest, wasm_hash, compiled, .. } = build;
        CpuBuild {
            compiled,
            manifest,
            dir: dir.to_path_buf(),
            wasm_hash,
            record: false,
            quiet: false,
            handle: None,
        }
    }

    /// Its hosts keep a copy of every batch the program submits ([`CpuHost::take_batches`]):
    /// for tests that look at what a program recorded. A long run keeps none.
    pub fn recording(mut self) -> CpuBuild {
        self.record = true;
        self
    }

    /// Keeps printed lines for [`CpuHost::take_logs`] without also showing them on stderr.
    pub fn quiet(mut self) -> CpuBuild {
        self.quiet = true;
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
        let mut io = io(&self.dir, storage, None, None, self.quiet);
        io.build_dir = self.handle.clone();
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

    /// Queues a script's event (a message with its bytes).
    pub fn push_scripted(&mut self, s: &Scripted) {
        self.program.push_scripted(s);
    }

    /// Queues a message of `bytes` from connection `conn` (language.md §17.2).
    pub fn push_message(&mut self, conn: u32, bytes: Vec<u8>) {
        self.program.push_message(conn, bytes);
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

    /// What the program printed and timed (`std::io::print`, `std::time::phase`), which its
    /// threads share.
    pub fn trace(&self) -> Trace {
        self.program.trace()
    }

    /// How many chunks of parallel jobs the workers (not the program's own thread) have run.
    pub fn worker_chunks(&self) -> u32 {
        self.program.helped()
    }

    /// Renders `quanta` quanta of the program's voice offline: `wrela_abi::AUDIO_QUANTUM`
    /// samples each, at `wrela_abi::AUDIO_SAMPLE_RATE`, the two channels interleaved (left,
    /// right, left, ...). Each call goes on from the last.
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

    /// Runs the next tick with `script`'s records of it (its tick-keyed events) queued after the
    /// rest, as `wrela-host --no-gpu` does.
    pub fn scripted_tick(&mut self, script: &[Scripted]) -> Result<()> {
        self.program.scripted_tick(script, None)
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
            if t.records.len() > wrela_abi::memory::MAX_TICK_RECORDS as usize {
                return Err(ReplayError::TooManyRecords { tick: k, count: t.records.len() });
            }
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

    /// Makes each helper hold back a task's result for `micros` microseconds once it's
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
