//! The tool host: the native host's tool profile (language.md §15.5), which runs a tool build,
//! `tool/tool.wasm`. It calls `tool` once, on the tool's thread; its result is the exit status.
//! The tool waits for its requests in the host: `tool_request` makes one at once (the host
//! numbers it and hands it to its service's work), `tool_wait` and `tool_any` block the calling
//! thread until an answer comes, and `tool_take` copies it ([`hub`]).
//!
//! A tool has no GPU, no ticker and no voice; its memory may grow to 4 GiB, and no call has a
//! deadline of its own (a tool may compute for minutes between requests). A run as a whole may
//! have one (`--timeout`), and an interrupt (Ctrl-C) stops it, inside a call too: a thread that
//! computes is stopped by the engine's epoch, and one that waits in the host wakes at least every
//! 10 ms to look ([`hub::Hub::block`]).
//!
//! **A tool has helpers** for `std::par`'s work and its own tasks (`std::tool::spawn`,
//! [`Options::workers`], language.md §16.1, §16.2): its memory is shared, and each helper is an
//! instance of the module on it, on a thread of its own, with the tool's host functions, so a
//! task's waits block only its own thread. They are the run's: the interrupt, the timeout and the
//! end of the run stop their calls and waits as they stop the tool's own (the first thread that
//! sees a reason latches the stop for every other), a helper's trap ends the run with its message
//! (status 70), and the run's end shuts them down within the grace, else the run is the host's
//! failure.
//!
//! **Services run beside the tool.** The requests are carried out on the host's worker threads
//! (files, blobs, HTTP, programs, the toolchain, driven builds, images, servers) or by the host
//! itself (timers, the date, a served post's wait, which the server answers when a post comes),
//! and each answer wakes the threads that wait. Every service is given one capability context
//! ([`Caps`]), made from the command line's grants, and reaches files, secrets, hosts and programs
//! through it alone.
//!
//! **A tool test's clock is fake** ([`Options::fake_clock`], language.md §13.2). It moves only
//! when every outstanding request is one the clock answers (a timer, or a fake site's answer
//! held for its delay), or one that waits on an event no tool test makes (a served post), and
//! every thread of the tool waits ([`hub`]'s rule): then it jumps to the soonest. While a worker
//! has a request, it waits.
//!
//! **Printing never waits.** A line the tool prints goes on a bounded queue, and a thread of the
//! host's own hands it to the [`Printer`]; a full queue drops the line and counts it. So a
//! console nobody reads can't hold a call where the clock thread can't stop it.
//!
//! **Every end is clean.** However the tool ends (its exit status, a trap, `--timeout`, an
//! interrupt), the host stops its workers (work not started is dropped, work in progress gets a
//! bounded grace), stops the clock thread that interrupts calls, and releases every blob, those
//! of requests the tool never read first ([`Ended`] says what was left). A worker running a
//! program kills its process group as it sees the end, one waiting for an HTTP response shuts
//! its connection down and joins the client's thread, one waiting for the toolchain or an image
//! cancels its work, and every driven build's process group is killed and every browser's
//! connection closed, then each build (its process reaped), each server and each browser (its
//! process group and its profile) is closed, with the same grace (a server's listener and connections are shut and its threads
//! joined, so its port is closed when the run returns); then the directory the run's programs
//! had is removed. No file work holds the blob table's lock, so a stuck worker can't hold up the
//! release. A worker, a driven build or a server's thread still running after the grace makes
//! the end the host's failure (status 71), whatever the tool's own end was, and `wrela tool`
//! then ends the process, so no worker outlives it. Work found stuck while the tool runs gives
//! the tool one grace to read the request that failed and end on its own ([`services::Stuck`]),
//! so what it says comes before the host's message.
//!
//! **Compiled code is kept only in the code cache** ([`Options::code_cache`], `crate::code`): the
//! tool's own build's, and each driven build's, which the drive service compiles once in a run.
//! `wrela tool` refuses a grant whose directory is at or above the cache, so nothing it loads
//! comes from a directory a tool could write.

mod blobs;
mod browser;
mod caps;
mod console;
mod drive;
mod http;
mod hub;
mod image;
mod run;
mod serve;
mod services;
pub mod toolchain;

pub use blobs::BlobBudget;
pub use caps::{Caps, Grants, Opened, Program, ProgramGrant};
pub use drive::run_child as drive_child;
pub use run::{GROUPS_ENV, launch};
pub use toolchain::Toolchain;

use crate::locked;
use console::Console;
use hub::{Hub, Thread};
use services::{Answer, Services, Task, Work};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use wasmtime::{Caller, Config, Engine, Linker, Module, SharedMemory, Store, UpdateDeadline};
use wrela_abi::memory::{MAX_WORKERS, THREAD_HELPER0, THREAD_MAIN, TOOL_MAX_PAGES};
use wrela_abi::stream;
use wrela_abi::{
    EXPORT_MEMORY, EXPORT_TOOL, EXPORT_TOOL_WORKER, IMPORT_CLOCK, IMPORT_KEEP, IMPORT_KEPT,
    IMPORT_LIMIT, IMPORT_MEMORY, IMPORT_MODULE, IMPORT_PHASE, IMPORT_PRINT, IMPORT_REQUEST_STATUS,
    IMPORT_REQUEST_TAKE, IMPORT_SUBMIT, IMPORT_TOOL_ACTIVE, IMPORT_TOOL_ANY, IMPORT_TOOL_DONE,
    IMPORT_TOOL_IDLE, IMPORT_TOOL_JOIN, IMPORT_TOOL_REQUEST, IMPORT_TOOL_TAKE, IMPORT_TOOL_WAIT,
};

/// How often the run's clock thread ticks: how soon an interrupt or `--timeout` stops a call.
const EPOCH: Duration = Duration::from_millis(10);

/// The worker threads that carry out files and blobs, those that wait on the network and on
/// programs, those that wait on the compiler and images, and those that run driven builds' work:
/// four pools, so that a long wait never holds up a file, a build never holds up the network,
/// and a driven build's calls never wait for the compiler. Four compile at once at most; driven
/// builds run as many at once as the machine has cores, from 4 to `MOST_DRIVE_WORKERS`
/// (`drive_workers`), so a tool that drives several instances of a build uses the machine.
const WORKERS: usize = 4;
const LONG_WORKERS: usize = 4;
const HEAVY_WORKERS: usize = 4;
const MOST_DRIVE_WORKERS: usize = 16;

/// The drive pool's workers: one per core the machine has, from 4 to `MOST_DRIVE_WORKERS`.
fn drive_workers() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().clamp(4, MOST_DRIVE_WORKERS))
}

/// How long, once the tool has ended, the host waits for work in progress to finish.
const GRACE: Duration = Duration::from_secs(5);

/// Joins `t` once it has finished, looking every `poll` until `until`: whether it finished (if
/// not, it's left to run).
fn join_by(t: std::thread::JoinHandle<()>, until: Instant, poll: Duration) -> bool {
    while !t.is_finished() && Instant::now() < until {
        std::thread::sleep(poll);
    }
    let finished = t.is_finished();
    if finished {
        let _ = t.join();
    }
    finished
}

/// How many helpers a tool has unless it says ([`Options::workers`]): one fewer than the
/// machine's cores (the tool's own thread runs on one), at most `wrela_abi`'s `MAX_WORKERS`.
pub fn default_workers() -> u32 {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    cores.saturating_sub(1).min(MAX_WORKERS)
}

/// Where a tool's lines go: each as the tool prints it.
pub type Printer = Arc<dyn Fn(&str) + Send + Sync>;

/// How to run a tool.
pub struct Options {
    /// What follows `--` on the command line (`std::tool::args`).
    pub args: Vec<String>,
    /// What the grants opened.
    pub caps: Caps,
    /// The run's budget as a whole, if it has one.
    pub timeout: Option<Duration>,
    /// How many helpers run `std::par`'s work beside the tool's own thread: 0 to `wrela_abi`'s
    /// `MAX_WORKERS` (more is that many). [`default_workers`] unless the run says.
    pub workers: u32,
    pub blob_budget: BlobBudget,
    /// Set (by a Ctrl-C handler) to stop the tool.
    pub interrupt: Option<Arc<AtomicBool>>,
    /// Where the tool's `print` lines go, from a thread of the host's own: the tool never waits
    /// on it.
    pub print: Printer,
    /// The export the host calls: `tool`, or a tool test's (§13.2).
    pub entry: String,
    /// A tool test's fake clock, from this date (seconds and nanoseconds since 1970): timers
    /// fire, in the order of their deadlines, as soon as the tool waits on nothing else, and
    /// `std::tool::testing` sets fakes. `None`: the system's clock, and no fakes.
    pub fake_clock: Option<(u64, u32)>,
    /// A tool test's run on the system's clock, its fakes kept: one with a live browser, whose
    /// page the clock mustn't jump past (`browser: true`, language.md §13.2).
    pub real_clock: bool,
    /// A file the host appends each program's process group to as it starts, for a supervisor
    /// that must kill them if this host can't (a tool test's, §13.2).
    pub groups: Option<std::path::PathBuf>,
    /// The compiler the toolchain service runs (`std::tool::toolchain`): `None`, and its
    /// requests fail.
    pub toolchain: Option<Arc<dyn Toolchain>>,
    /// The directory, the host's own, where the toolchain keeps its constants' cache: never one
    /// a tool is granted. `None`: one of the run's own, removed with it.
    pub cache: Option<std::path::PathBuf>,
    /// The code cache's directory (`crate::code::cache_dir`), which no root the run grants may
    /// hold (`Caps::refuse_code_cache`): the tool's build's code, and its driven builds', are
    /// kept there from one run to the next. `None`: none is kept.
    pub code_cache: Option<crate::code::Cache>,
    /// What only tests change.
    #[doc(hidden)]
    pub testing: Testing,
}

/// A run's settings that only tests change: the workers' grace, a slow disk under each blob
/// written to a file, a clock thread that fails to start, where the run's own directory is, a
/// pause between a program's start and its record, a name lookup that stalls, and, for the
/// toolchain's outputs, one write held a while (a served page's result too, by `results/<name>`)
/// and one that fails part way (each chosen by its path's end, or `#n` for a request's `n`th
/// write), a server's start that fails once it
/// listens, image work that holds a while, seeing no cancel, a [`Signal`] of the moments a
/// test waits for, helpers slow to start, a helper that can't start, and a pause between
/// `any`'s check of its requests and its wait.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct Testing {
    pub grace: Duration,
    pub io_delay: Duration,
    pub fail_clock_thread: bool,
    pub work_dir: Option<std::path::PathBuf>,
    pub spawn_pause: Duration,
    pub resolve_stall: Duration,
    pub hold: Option<(String, Duration)>,
    pub write_fail: Option<String>,
    pub fail_server_start: bool,
    pub image_stall: Duration,
    pub signal: Option<Signal>,
    /// How long each helper waits before it starts running work.
    pub helper_delay: Duration,
    /// The helper, by its index, whose thread fails to start.
    pub fail_helper: Option<u32>,
    /// How long `tool_any` waits between checking its requests and blocking.
    pub any_pause: Duration,
    /// How long the drive service waits before it reads each blob's bytes from a driven
    /// build's process.
    pub drive_hold: Duration,
    /// How long the drive service waits before it compiles a driven build's code.
    pub compile_hold: Duration,
}

/// What a test is told as the host reaches a moment it waits for, so it acts then rather than
/// after a guessed time: `"write"` (a blob's write to a file starts), `"resolve"` (an HTTP
/// request's name lookup starts), `"image"` (an image's work starts), `"listening"` (a server
/// listens), `"connection"` (a server takes a connection), `"post"` (a server queues a post),
/// `"browser"` (a browser has opened its page), `"run"` (a browser's script starts),
/// `"closing"` (the host takes a browser's close) and `"close"` (a worker starts it).
#[doc(hidden)]
#[derive(Clone)]
pub struct Signal(pub Arc<dyn Fn(&'static str) + Send + Sync>);

impl std::fmt::Debug for Signal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Signal")
    }
}

impl Default for Testing {
    fn default() -> Testing {
        Testing {
            grace: GRACE,
            io_delay: Duration::ZERO,
            fail_clock_thread: false,
            work_dir: None,
            spawn_pause: Duration::ZERO,
            resolve_stall: Duration::ZERO,
            hold: None,
            write_fail: None,
            fail_server_start: false,
            image_stall: Duration::ZERO,
            signal: None,
            helper_delay: Duration::ZERO,
            fail_helper: None,
            any_pause: Duration::ZERO,
            drive_hold: Duration::ZERO,
            compile_hold: Duration::ZERO,
        }
    }
}

impl Options {
    /// A run with `caps`, no arguments, no timeout, the default blob budget, and its lines on
    /// standard output.
    pub fn new(caps: Caps) -> Options {
        Options {
            args: Vec::new(),
            caps,
            timeout: None,
            workers: default_workers(),
            blob_budget: BlobBudget::default(),
            interrupt: None,
            print: Arc::new(|line| println!("{line}")),
            entry: EXPORT_TOOL.to_string(),
            fake_clock: None,
            real_clock: false,
            groups: None,
            toolchain: None,
            cache: None,
            code_cache: None,
            testing: Testing::default(),
        }
    }
}

/// How a tool ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// `tool` returned this exit status.
    Exit(u8),
    /// The tool waited on nothing: on no request, or on one it had made no wait for (already
    /// taken, or never made).
    WaitsOnNothing,
    /// `tool` returned a number that isn't an exit status.
    BadStatus(u32),
    /// The tool trapped, with its panic's message if it panicked.
    Trap(String),
    /// The run went past its `--timeout`.
    Timeout,
    /// An interrupt stopped it.
    Interrupted,
    /// The host couldn't run it, or couldn't do what it asked.
    Host(String),
}

impl End {
    /// The process's exit status for this end: the tool's own, 70 for the tool's failure (a
    /// trap, a helper's among them, waiting on nothing, a status out of range), 71 for the
    /// host's, 124 past the timeout, 130 interrupted.
    pub fn status(&self) -> u8 {
        match self {
            End::Exit(n) => *n,
            End::WaitsOnNothing | End::BadStatus(_) | End::Trap(_) => 70,
            End::Host(_) => 71,
            End::Timeout => 124,
            End::Interrupted => 130,
        }
    }
}

impl std::fmt::Display for End {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            End::Exit(n) => write!(f, "the tool exited with status {n}"),
            End::WaitsOnNothing => f.write_str(
                "the tool waits on nothing: on no request, or on one that isn't outstanding",
            ),
            End::BadStatus(n) => {
                write!(f, "`tool` returned {n}, which isn't an exit status (0 to 255)")
            }
            End::Trap(why) => write!(f, "the tool trapped: {why}"),
            End::Timeout => f.write_str("the tool ran past its timeout"),
            End::Interrupted => f.write_str("the tool was interrupted"),
            End::Host(why) => write!(f, "the tool host: {why}"),
        }
    }
}

/// What a run left, once it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    pub end: End,
    /// The blobs the tool still held when it ended, which the host released, and their bytes.
    pub blobs_released: (usize, u64),
    /// Worker threads and helpers still running past the grace (0 unless a service or a helper
    /// was stuck, and then the end is [`End::Host`]).
    pub workers_left: usize,
    /// The lines the tool printed that the console didn't take in time, and the host dropped.
    pub lines_dropped: u64,
    /// How many chunks of its parallel jobs the tool's helpers ran (`std::par`): 0 without
    /// helpers.
    pub helped: u32,
}

/// A tool build: its module's bytes, compiled for each run.
#[derive(Clone)]
pub struct ToolBuild {
    wasm: Vec<u8>,
}

impl ToolBuild {
    /// The tool build in the build directory `dir`: its `tool/tool.wasm`.
    pub fn load(dir: &Path) -> Result<ToolBuild, String> {
        let path = dir.join("tool").join("tool.wasm");
        let wasm = std::fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        Ok(ToolBuild { wasm })
    }

    /// A tool build from its module's bytes.
    pub fn from_wasm(wasm: Vec<u8>) -> ToolBuild {
        ToolBuild { wasm }
    }
}

/// What `std::tool::args` and `secret` read: a count and each argument, then a count and each
/// secret's name and value (text as a `u32` byte count, then UTF-8).
fn start_bytes(args: &[String], secrets: &[(String, String)]) -> Vec<u8> {
    let mut out = toolchain::Out::default();
    out.strs(args).u32(secrets.len() as u32);
    for (name, value) in secrets {
        out.str(name).str(value);
    }
    out.0
}

/// The worker threads, which carry out services.
struct Pool {
    jobs: Mutex<Option<mpsc::Sender<(u32, Task)>>>,
    handles: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// How many workers are running, which each lowers as it returns.
    alive: Arc<(Mutex<usize>, Condvar)>,
    /// Set once the tool has ended: work not started yet is dropped.
    closing: Arc<AtomicBool>,
}

impl Pool {
    fn start(services: &Arc<Services>, hub: &Arc<Hub>, name: &str, workers: usize) -> Pool {
        let (jobs, queue) = mpsc::channel::<(u32, Task)>();
        let queue = Arc::new(Mutex::new(queue));
        let alive = Arc::new((Mutex::new(workers), Condvar::new()));
        // The services' own: a worker waiting on a program or a response sees it too.
        let closing = services.closing.clone();
        let mut handles = Vec::new();
        for i in 0..workers {
            let (queue, running, closing) = (queue.clone(), alive.clone(), closing.clone());
            let (services, hub) = (services.clone(), hub.clone());
            let spawned = std::thread::Builder::new().name(format!("wrela-tool-{name}-{i}")).spawn(
                move || {
                    loop {
                        let job = queue.lock().map(|q| q.recv());
                        let Ok(Ok((request, task))) = job else { break };
                        if closing.load(Ordering::Acquire) {
                            continue;
                        }
                        let run = std::panic::AssertUnwindSafe(|| task(&services, request));
                        let answer = std::panic::catch_unwind(run)
                            .unwrap_or_else(|_| services::err("the host's service failed"));
                        // A failed request's answer names no blob or build: those made for it go
                        // back.
                        if matches!(&answer, Answer::Bytes(b) if b.first() == Some(&1)) {
                            rollback(&services, request);
                        }
                        hub.deliver(request, answer);
                    }
                    let (count, wake) = &*running;
                    *locked(count) -= 1;
                    wake.notify_all();
                },
            );
            match spawned {
                Ok(h) => handles.push(h),
                Err(_) => {
                    let (count, _) = &*alive;
                    *locked(count) -= 1;
                }
            }
        }
        Pool { jobs: Mutex::new(Some(jobs)), handles: Mutex::new(handles), alive, closing }
    }

    fn send(&self, request: u32, task: Task) {
        let jobs = locked(&self.jobs);
        if let Some(jobs) = jobs.as_ref() {
            // A pool with no workers left drops the task; the request stays outstanding until
            // the run ends.
            let _ = jobs.send((request, task));
        }
    }

    /// Tells the workers to stop: work not started is dropped, and an idle worker returns.
    fn close(&self) {
        self.closing.store(true, Ordering::Release);
        locked(&self.jobs).take();
    }

    /// Stops the workers: work not started is dropped, and work in progress has until `until`
    /// to finish. How many are still running then (they're left to finish alone, and the run
    /// doesn't end cleanly).
    fn stop(&self, until: Instant) -> usize {
        self.close();
        let grace = until.saturating_duration_since(Instant::now());
        let (count, wake) = &*self.alive;
        let left = locked(count);
        let (left, _) = wake
            .wait_timeout_while(left, grace, |n| *n > 0)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let left = *left;
        if left == 0 {
            for h in std::mem::take(&mut *locked(&self.handles)) {
                let _ = h.join();
            }
        }
        left
    }
}

/// What a failed request made goes back: its blobs, and the driven builds, servers and
/// browsers made for it, each closed.
fn rollback(services: &Services, request: u32) {
    services.blobs().rollback(request);
    let made = services.drives().rollback(request);
    for d in made {
        if !d.close(services.grace, &services.strays) {
            let _ = services.stuck.set("a failed request's driven build didn't close".into());
        }
    }
    let made = services.servers().rollback(request);
    for server in made {
        if !server.close(services, services.grace) {
            let _ = services.stuck.set("a failed request's server didn't close".into());
        }
    }
    let made = services.browsers().rollback(request);
    for browser in made {
        if !browser.close() {
            let _ = services.stuck.set("a failed request's browser didn't end cleanly".into());
        }
    }
}

/// The four pools of workers: files and blobs, long waits (HTTP, programs), heavy work (the
/// compiler, images) and driven builds.
struct Pools {
    files: Pool,
    long: Pool,
    heavy: Pool,
    drive: Pool,
}

impl Pools {
    /// Stops all four, their work in progress sharing one `grace`: how many workers are left.
    fn stop(&self, grace: Duration) -> usize {
        let until = Instant::now() + grace;
        // All are told first, so none's work waits on another's grace.
        for p in [&self.files, &self.long, &self.heavy, &self.drive] {
            p.close();
        }
        self.drive.stop(until)
            + self.heavy.stop(until)
            + self.long.stop(until)
            + self.files.stop(until)
    }
}

/// The run's clock thread: it advances the engine's epoch, so a call can be stopped.
struct Ticker {
    running: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Ticker {
    /// Starts the thread, or says why it couldn't: without it, no call could be stopped.
    fn start(engine: &Engine, fail: bool) -> Result<Ticker, String> {
        let running = Arc::new(AtomicBool::new(true));
        let (r, engine) = (running.clone(), engine.clone());
        let spawned = if fail {
            Err(std::io::Error::other("a test's injected failure"))
        } else {
            std::thread::Builder::new().name("wrela-tool-epochs".into()).spawn(move || {
                while r.load(Ordering::Acquire) {
                    std::thread::sleep(EPOCH);
                    engine.increment_epoch();
                }
            })
        };
        let handle =
            spawned.map_err(|e| format!("can't start the thread that stops calls: {e}"))?;
        Ok(Ticker { running, handle: Some(handle) })
    }

    fn stop(mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// What stops a run: work that can't be stopped, a helper's trap, an interrupt and its
/// timeout; and the latch the first thread that sees one sets, so every other stops too.
#[derive(Clone)]
pub(crate) struct Reasons {
    interrupt: Option<Arc<AtomicBool>>,
    pub(crate) deadline: Option<Instant>,
    stuck: Arc<services::Stuck>,
    /// The first trap of a helper's: the tool's failure.
    trapped: Arc<OnceLock<End>>,
    /// Set by the first thread that sees a reason, and once the tool's own thread has
    /// returned.
    stop: Arc<AtomicBool>,
}

impl Reasons {
    /// Why the run must stop now, if it must: a worker whose work can't be stopped (the
    /// host's failure), a helper's trap, an interrupt, or the timeout.
    pub(crate) fn now(&self) -> Option<End> {
        if let Some(why) = self.stuck.due() {
            return Some(End::Host(why.clone()));
        }
        if let Some(trap) = self.trapped.get() {
            return Some(trap.clone());
        }
        if self.interrupt.as_ref().is_some_and(|i| i.load(Ordering::Acquire)) {
            return Some(End::Interrupted);
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Some(End::Timeout);
        }
        None
    }

    /// Every thread's call stops from now on.
    pub(crate) fn latch(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub(crate) fn latched(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// Runs the tool to its end, and cleans up after it.
pub fn run(build: &ToolBuild, options: Options) -> Ended {
    let failed = |why: String| Ended {
        end: End::Host(why),
        blobs_released: (0, 0),
        workers_left: 0,
        lines_dropped: 0,
        helped: 0,
    };
    let deadline = match options.timeout.map(|t| Instant::now().checked_add(t)) {
        None => None,
        Some(Some(d)) => Some(d),
        Some(None) => return failed("the timeout is further off than the clock reaches".into()),
    };
    let mut config = Config::new();
    config.wasm_threads(true).shared_memory(true).epoch_interruption(true);
    let engine = match Engine::new(&config) {
        Ok(e) => e,
        Err(e) => return failed(format!("the tool engine: {e:#}")),
    };
    let start = start_bytes(&options.args, options.caps.secrets());
    let budget = options.timeout;
    let Options {
        caps,
        blob_budget,
        interrupt,
        print,
        testing,
        entry,
        fake_clock,
        real_clock,
        groups,
        toolchain,
        cache,
        code_cache,
        workers,
        ..
    } = options;
    let blobs = Mutex::new(blobs::Blobs::new(blob_budget));
    let services = Arc::new(Services {
        caps,
        blobs,
        io_delay: testing.io_delay,
        closing: Arc::new(AtomicBool::new(false)),
        testing: fake_clock.is_some(),
        fakes: Mutex::new(HashMap::new()),
        work: std::sync::OnceLock::new(),
        work_at: testing.work_dir.clone(),
        groups,
        spawn_pause: testing.spawn_pause,
        resolve_stall: testing.resolve_stall,
        grace: testing.grace,
        stuck: Arc::new(services::Stuck::new(testing.grace)),
        budget,
        toolchain: toolchain.map(|toolchain| toolchain::Service { toolchain, cache }),
        hold: testing.hold.clone(),
        write_fail: testing.write_fail.clone(),
        fail_server_start: testing.fail_server_start,
        image_stall: testing.image_stall,
        signal: testing.signal.clone(),
        strays: Mutex::default(),
        drives: Mutex::default(),
        codes: drive::Codes::new(code_cache.clone()),
        drive_hold: testing.drive_hold,
        compile_hold: testing.compile_hold,
        servers: Mutex::default(),
        browsers: Mutex::default(),
        answers: Mutex::new(None),
    });
    let hub = Arc::new(Hub::new(fake_clock.filter(|_| !real_clock)));
    *locked(&services.answers) = Some(hub.clone());
    let pools = Arc::new(Pools {
        files: Pool::start(&services, &hub, "files", WORKERS),
        long: Pool::start(&services, &hub, "long", LONG_WORKERS),
        heavy: Pool::start(&services, &hub, "heavy", HEAVY_WORKERS),
        drive: Pool::start(&services, &hub, "drive", drive_workers()),
    });
    let (console, writer) = match Console::start(print) {
        Ok(c) => c,
        Err(why) => {
            let workers_left = pools.stop(testing.grace);
            return Ended { workers_left, ..failed(why) };
        }
    };
    let ticker = match Ticker::start(&engine, testing.fail_clock_thread) {
        Ok(t) => t,
        Err(why) => {
            // Before any Wasm runs: the workers already started stop with it.
            let workers_left = pools.stop(testing.grace);
            drop(console);
            let lines_dropped = writer.stop(testing.grace);
            return Ended { workers_left, lines_dropped, ..failed(why) };
        }
    };
    let reasons = Reasons {
        interrupt,
        deadline,
        stuck: services.stuck.clone(),
        trapped: Arc::new(OnceLock::new()),
        stop: Arc::new(AtomicBool::new(false)),
    };
    let tool = Tool {
        hub,
        services: services.clone(),
        pools: pools.clone(),
        reasons,
        start,
        console,
        workers,
        helper_delay: testing.helper_delay,
        fail_helper: testing.fail_helper,
        any_pause: testing.any_pause,
        grace: testing.grace,
    };
    // The stores, and the console's sending ends in them, go when `drive` returns, and its
    // helpers have stopped by then (or are counted as left).
    let (mut end, helpers_left, run_helped) = tool.drive(&engine, build, &entry, code_cache);
    ticker.stop();
    // Driven builds' processes are killed first, and every browser's connection closes, so the
    // workers waiting on them return.
    services.drives().stop_all();
    services.browsers().disconnect_all();
    let mut workers_left = pools.stop(testing.grace) + helpers_left;
    // Then each driven build closes, its process reaped: one that isn't within the grace is
    // left, as a stuck worker is.
    let driven = services.drives().take_all();
    let until = Instant::now() + testing.grace;
    for d in driven {
        if !d.close(until.saturating_duration_since(Instant::now()), &services.strays) {
            workers_left += 1;
        }
    }
    // And each server: its listener and connections shut, its threads joined, so its port is
    // closed when this returns. Nothing answers the run's requests after this.
    let servers = services.servers().take_all();
    for server in servers {
        if !server.close(&services, until.saturating_duration_since(Instant::now())) {
            workers_left += 1;
        }
    }
    // And each browser: its process group killed and its profile removed.
    let browsers = services.browsers().take_all();
    for b in browsers {
        if !b.close() {
            workers_left += 1;
        }
    }
    locked(&services.answers).take();
    // Work given up on as it ran past its grace: each thread still running now is left.
    let strays = std::mem::take(&mut *locked(&services.strays));
    for t in strays {
        if !join_by(t, until, EPOCH) {
            workers_left += 1;
        }
    }
    let lines_dropped = writer.stop(testing.grace);
    // Its programs' files and `HOME`: no program runs now (each worker kills its own as the
    // tool ends), unless a worker is still past its grace.
    if let Some(Ok(dir)) = services.work.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
    // A worker past its grace may still be changing files: that isn't a clean end.
    if workers_left > 0 {
        let stuck = services.stuck.get().map_or(String::new(), |why| format!(": {why}"));
        end = End::Host(format!(
            "{workers_left} of the host's workers were still running {:?} after the tool ended ({end}){stuck}",
            testing.grace
        ));
    } else if let Some(why) = services.stuck.get()
        && !matches!(end, End::Host(_))
    {
        // Work that didn't stop, seen only as the run was cleaned up.
        end = End::Host(format!("{why} ({end})"));
    }
    // No file work holds the table's lock, so a stuck worker can't keep this waiting.
    let blobs_released = locked(&services.blobs).release_all();
    Ended { end, blobs_released, workers_left, lines_dropped, helped: run_helped }
}

/// What every thread of the tool reaches through its store: the run's shared state, the
/// services and their workers, what stops the run, the tool's start and its memory.
pub(crate) struct Shared {
    hub: Arc<Hub>,
    services: Arc<Services>,
    pools: Arc<Pools>,
    reasons: Reasons,
    /// What `kept` gives: the arguments and the granted secrets.
    start: Vec<u8>,
    /// The tool's memory, which its helpers share.
    memory: SharedMemory,
    /// A test's pause in `tool_any` ([`Testing::any_pause`]).
    any_pause: Duration,
}

/// A run about to start: what [`run`] made for it.
struct Tool {
    hub: Arc<Hub>,
    services: Arc<Services>,
    pools: Arc<Pools>,
    reasons: Reasons,
    start: Vec<u8>,
    console: Console,
    /// How many helpers the tool has, how long each waits before it starts (a test's), and how
    /// long they have to stop once it ends.
    workers: u32,
    helper_delay: Duration,
    fail_helper: Option<u32>,
    any_pause: Duration,
    grace: Duration,
}

/// A thread's part of the host, in its store: the tool's own thread's, or a helper's.
struct ToolState {
    /// The thread's number (wrela_abi `memory`'s threads).
    thread: u32,
    shared: Arc<Shared>,
    /// What the host refused to do, or the tool's own misuse of it, which fails the call.
    refused: Option<End>,
    /// Why the run stopped the call, if it did.
    stopped: Option<End>,
    console: Console,
}

impl Tool {
    /// Compiles the tool (or reads its code back from `cache`), calls its entry on its thread
    /// with its helpers beside it, and stops them: how it ended, the helpers left past the
    /// grace, and the chunks they ran.
    fn drive(
        self,
        engine: &Engine,
        build: &ToolBuild,
        entry: &str,
        cache: Option<crate::code::Cache>,
    ) -> (End, usize, u32) {
        let module = match &cache {
            Some(cache) => cache.module(engine, &build.wasm),
            None => Module::new(engine, &build.wasm).map_err(|e| format!("{e:#}")),
        };
        let module = match module {
            Ok(m) => m,
            Err(e) => return (End::Host(format!("compiling the tool build: {e}")), 0, 0),
        };
        let memory = match check_tool(&module, engine, entry) {
            Ok(m) => m,
            Err(why) => return (End::Host(why), 0, 0),
        };
        let shared = Arc::new(Shared {
            hub: self.hub.clone(),
            services: self.services.clone(),
            pools: self.pools.clone(),
            reasons: self.reasons.clone(),
            start: self.start,
            memory: memory.clone(),
            any_pause: self.any_pause,
        });
        let linker = match linker(engine) {
            Ok(l) => l,
            Err(e) => return (End::Host(format!("{e:#}")), 0, 0),
        };
        let mut store = new_store(engine, &shared, THREAD_MAIN, &self.console);
        let instance = match instantiate(&linker, &mut store, &module) {
            Ok(i) => i,
            Err(e) => return (classify(&mut store, &e), 0, 0),
        };
        let exported = instance.get_shared_memory(&mut store, EXPORT_MEMORY);
        if !exported.is_some_and(|m| std::ptr::eq(m.data().as_ptr(), memory.data().as_ptr())) {
            let why = format!(
                "this isn't a tool build: its `{EXPORT_MEMORY}` export isn't its imported memory"
            );
            return (End::Host(why), 0, 0);
        }
        let tool = match instance.get_typed_func::<(), u32>(&mut store, entry) {
            Ok(f) => f,
            Err(e) => return (End::Host(format!("`{entry}`: {e:#}")), 0, 0),
        };
        self.hub.set(THREAD_MAIN, Thread::Running);
        let (mut helpers, started) = Helpers::start(
            &module,
            &linker,
            &shared,
            self.workers,
            &self.console,
            (self.helper_delay, self.fail_helper),
        );
        drop(self.console);
        // A reason to stop that came first wins: before the tool runs (a budget spent while it
        // compiled, an interrupt already given), and over a status it returns.
        let end = match started {
            Err(why) => End::Host(format!("the tool's helpers: {why}")),
            Ok(()) => match self.reasons.now() {
                Some(why) => why,
                None => {
                    store.data_mut().refused = None;
                    let ended = match tool.call(&mut store, ()) {
                        Ok(status) => match u8::try_from(status) {
                            Ok(n) => End::Exit(n),
                            Err(_) => End::BadStatus(status),
                        },
                        Err(e) => classify(&mut store, &e),
                    };
                    match (&ended, self.reasons.now()) {
                        (End::Exit(_) | End::BadStatus(_), Some(why)) => why,
                        _ => ended,
                    }
                }
            },
        };
        self.hub.gone(THREAD_MAIN);
        // However it ended, every helper's call stops now, and the helpers return.
        self.reasons.latch();
        self.hub.notify();
        let left = helpers.shut_down(&memory, &self.hub, self.grace);
        let helped = crate::shared::load_u32(&memory, wrela_abi::memory::PAR_HELPED);
        (end, left, helped)
    }
}

/// Checks `module` is a tool build with the export `entry`: its memory, shared, capped at
/// 4 GiB, which it makes.
fn check_tool(module: &Module, engine: &Engine, entry: &str) -> Result<SharedMemory, String> {
    let memory_type = module.imports().find_map(|i| match i.ty() {
        wasmtime::ExternType::Memory(m)
            if (i.module(), i.name()) == (IMPORT_MODULE, IMPORT_MEMORY) && m.is_shared() =>
        {
            Some(m)
        }
        _ => None,
    });
    let Some(memory_type) = memory_type.filter(|_| module.get_export(EXPORT_MEMORY).is_some())
    else {
        return Err(format!(
            "this isn't a tool build: it doesn't import its memory, shared, as `{IMPORT_MODULE}.{IMPORT_MEMORY}`"
        ));
    };
    if memory_type.maximum().is_none_or(|m| m > u64::from(TOOL_MAX_PAGES)) {
        return Err(format!(
            "this isn't a tool build: its memory may grow past {TOOL_MAX_PAGES} pages"
        ));
    }
    if module.get_export(entry).is_none() {
        return Err(format!("this isn't a tool build: it doesn't export `{entry}`"));
    }
    SharedMemory::new(engine, memory_type).map_err(|e| format!("the tool's memory: {e:#}"))
}

/// A store for thread `thread` of the tool: its calls stop within about 10 ms of a reason to
/// stop the run, or of another thread's stop.
fn new_store(
    engine: &Engine,
    shared: &Arc<Shared>,
    thread: u32,
    console: &Console,
) -> Store<ToolState> {
    let state = ToolState {
        thread,
        shared: shared.clone(),
        refused: None,
        stopped: None,
        console: console.clone(),
    };
    let mut store = Store::new(engine, state);
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(move |mut cx| {
        let reasons = &cx.data().shared.reasons;
        let why = reasons.now().or_else(|| reasons.latched().then_some(End::Interrupted));
        if let Some(why) = why {
            reasons.latch();
            cx.data_mut().stopped = Some(why);
            return Ok(UpdateDeadline::Interrupt);
        }
        Ok(UpdateDeadline::Continue(1))
    });
    store
}

/// The tool's helpers: each an instance of the module on the tool's memory, on a thread of its
/// own, running `__tool_worker` with the tool's host functions until the tool ends.
struct Helpers {
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Helpers {
    /// `count` helpers (at most `MAX_WORKERS`), each waiting `delay` before it starts (a test's;
    /// and a test's helper that fails to start, by its index); none if the module exports no
    /// `__tool_worker` (it runs no parallel work). The memory's `PAR_HELPERS` says how many.
    /// Each is running from the start, as the fake clock reads it, until it first waits for
    /// work. If one can't start, those started are kept, to be shut down as the run ends, and
    /// why is given beside them.
    fn start(
        module: &Module,
        linker: &Linker<ToolState>,
        shared: &Arc<Shared>,
        count: u32,
        console: &Console,
        (delay, fail): (Duration, Option<u32>),
    ) -> (Helpers, Result<(), String>) {
        let mut helpers = Helpers { threads: Vec::new() };
        if module.get_export(EXPORT_TOOL_WORKER).is_none() {
            return (helpers, Ok(()));
        }
        for i in 0..count.min(MAX_WORKERS) {
            let thread = THREAD_HELPER0 + i;
            shared.hub.set(thread, Thread::Running);
            let (module, linker, mine) = (module.clone(), linker.clone(), shared.clone());
            let console = console.clone();
            let spawned = if fail == Some(i) {
                Err(std::io::Error::other("a test's injected failure"))
            } else {
                std::thread::Builder::new().name(format!("wrela-tool-helper-{thread}")).spawn(
                    move || {
                        if !delay.is_zero() {
                            std::thread::sleep(delay);
                        }
                        let engine = module.engine().clone();
                        let mut store = new_store(&engine, &mine, thread, &console);
                        let ran = instantiate(&linker, &mut store, &module).and_then(|instance| {
                            let worker = instance
                                .get_typed_func::<u32, ()>(&mut store, EXPORT_TOOL_WORKER)?;
                            worker.call(&mut store, thread)
                        });
                        if let Err(e) = ran {
                            helper_failed(&mut store, &e);
                        }
                        mine.hub.gone(thread);
                    },
                )
            };
            match spawned {
                Ok(h) => helpers.threads.push(h),
                Err(e) => {
                    shared.hub.gone(thread);
                    return (helpers, Err(format!("can't start helper {i}: {e}")));
                }
            }
        }
        crate::shared::store_u32(
            &shared.memory,
            wrela_abi::memory::PAR_HELPERS,
            helpers.threads.len() as u32,
        );
        (helpers, Ok(()))
    }

    /// Tells the helpers to return (each sees it once it waits for work, or once a wait in the
    /// host sees the run's latch) and waits for them, at most `grace`: how many it left.
    fn shut_down(&mut self, memory: &SharedMemory, hub: &Hub, grace: Duration) -> usize {
        if self.threads.is_empty() {
            return 0;
        }
        crate::program::shut_down_helpers(memory);
        hub.notify();
        let until = Instant::now() + grace;
        self.threads
            .drain(..)
            .map(|t| join_by(t, until, Duration::from_millis(1)))
            .filter(|joined| !joined)
            .count()
    }
}

/// A helper's call failed with `e`. The thread that waits for what it ran learns it, as a
/// `std::par` wait in WASM isn't interrupted (wrela_abi `memory`'s helpers). Unless the run
/// stopped it, its trap (or what the host refused it) is the tool's failure, which ends the run.
fn helper_failed(store: &mut Store<ToolState>, e: &wasmtime::Error) {
    let (thread, shared) = (store.data().thread, store.data().shared.clone());
    if store.data().stopped.is_none() {
        // Before the waiting thread wakes, so it ends with this reason.
        let end = classify(store, e);
        let _ = shared.reasons.trapped.set(end);
    }
    crate::program::helper_trapped(&shared.memory, thread);
    shared.hub.notify();
}

/// Why `e`, a call's error, happened: the run's stop first (also one another thread saw), then
/// the host's refusals, then the thread's own trap, with its panic's message if it panicked (a
/// helper's, if it was a helper's: std copies it into the waiting thread's block).
fn classify(store: &mut Store<ToolState>, e: &wasmtime::Error) -> End {
    let s = store.data_mut();
    if let Some(why) = s.stopped.take() {
        return why;
    }
    if let Some(why) = s.refused.take() {
        return why;
    }
    if let Some(why) = s.shared.reasons.now() {
        return why;
    }
    match e.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::Interrupt) => End::Interrupted,
        Some(wasmtime::Trap::StackOverflow) => {
            End::Trap("the call went deeper than the stack allows".into())
        }
        Some(t) => {
            use wrela_abi::memory::{PANIC_CAP, panic_at, panic_message};
            let at = panic_at(s.thread) as usize;
            let message = crate::shared::read(&s.shared.memory, at, 4 + PANIC_CAP as usize)
                .and_then(|bytes| panic_message(&bytes));
            End::Trap(message.unwrap_or_else(|| t.to_string()))
        }
        None => End::Host(format!("{e:#}")),
    }
}

/// Bytes `[at, at + len)` of the tool's memory.
fn read(c: &mut Caller<'_, ToolState>, at: u32, len: u32) -> wasmtime::Result<Vec<u8>> {
    crate::shared::read(&c.data().shared.memory, at as usize, len as usize).ok_or_else(|| {
        wasmtime::format_err!("({at}, {len}) reaches past the end of the tool's memory")
    })
}

/// Writes `bytes` at `at` in the tool's memory.
fn write(c: &mut Caller<'_, ToolState>, at: u32, bytes: &[u8], what: &str) -> wasmtime::Result<()> {
    if crate::shared::write(&c.data().shared.memory, at as usize, bytes) {
        return Ok(());
    }
    Err(wasmtime::format_err!("{what}({at}) reaches past the end of the tool's memory"))
}

/// Fails the host function that's running with `end`, which the call ends with.
fn refuse(c: &mut Caller<'_, ToolState>, end: End) -> wasmtime::Error {
    let message = end.to_string();
    c.data_mut().refused = Some(end);
    wasmtime::format_err!("{message}")
}

/// Blocks the calling thread in `state` until `ready` gives something, or traps it with the
/// run's stop reason.
fn block<R>(
    c: &mut Caller<'_, ToolState>,
    state: Thread,
    ready: impl FnMut(&hub::State) -> Option<R>,
) -> wasmtime::Result<R> {
    let (thread, shared) = (c.data().thread, c.data().shared.clone());
    match shared.hub.block(thread, state, &shared.memory, &shared.reasons, ready) {
        Ok(r) => Ok(r),
        // The tool's own failure (a helper's too), not the run's stop.
        Err(End::WaitsOnNothing) => Err(refuse(c, End::WaitsOnNothing)),
        Err(why) => {
            let message = why.to_string();
            c.data_mut().stopped = Some(why);
            Err(wasmtime::format_err!("{message}"))
        }
    }
}

/// Makes request `service` with `body`: numbers it, and hands it to the service's work, at
/// once (language.md §15.5's deadlines count from here).
fn make_request(shared: &Shared, service: &str, body: &[u8]) -> u32 {
    let request = shared.hub.make();
    let hub = &shared.hub;
    match services::route(&shared.services, service, body) {
        Work::Now(answer) => hub.deliver(request, answer),
        Work::Timer(after) => {
            hub.lock().timer(request, after);
            hub.notify();
        }
        Work::Worker(task) => shared.pools.files.send(request, task),
        Work::Long(task) => shared.pools.long.send(request, task),
        Work::Heavy(task) => shared.pools.heavy.send(request, task),
        Work::Drive(task) => shared.pools.drive.send(request, task),
        Work::Held(after, task) => {
            hub.lock().hold(request, after);
            shared.pools.long.send(request, task);
        }
        Work::Later(register) => {
            hub.lock().wait_later(request);
            if let Some(answer) = register(&shared.services, request) {
                hub.deliver(request, answer);
            }
        }
        Work::Utc => {
            let answer = hub.lock().utc();
            hub.deliver(request, answer);
        }
        Work::SetUtc(seconds, nanos) => {
            let answer = match &mut hub.lock().fake {
                Some(f) => {
                    let set = Duration::new(seconds, nanos);
                    // The fake time since the start stays; the date it reads moves.
                    f.utc = set.saturating_sub(f.now);
                    services::ok(&[])
                }
                None => services::err("only a tool test's fake clock is set: not the system's"),
            };
            hub.deliver(request, answer);
        }
    }
    request
}

/// The host functions a tool build may import; any other import refuses to link.
fn linker(engine: &Engine) -> wasmtime::Result<Linker<ToolState>> {
    let mut linker = Linker::new(engine);
    // A tool's requests go through `tool_request`: what the command stream carries is a
    // client's (storage, fetches, posts) or GPU work, which a tool has none of.
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_SUBMIT,
        |mut c: Caller<'_, ToolState>, at: u32, len: u32| {
            let batch = read(&mut c, at, len)?;
            let commands = match stream::decode(&batch) {
                Ok(commands) => commands,
                Err(e) => return Err(wasmtime::format_err!("a malformed command batch: {e}")),
            };
            match commands.first() {
                None => Ok(()),
                Some(cmd) => {
                    let why = format!(
                        "a tool host can't carry out {:?}: a tool's files are `std::tool::files`",
                        cmd.opcode()
                    );
                    Err(refuse(&mut c, End::Host(why)))
                }
            }
        },
    )?;
    let no = |name: &str| {
        End::Host(format!(
            "a tool host has no `{name}`: a tool waits on its requests (`std::tool::Call::wait`)"
        ))
    };
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_REQUEST_STATUS,
        move |mut c: Caller<'_, ToolState>, _: u32| {
            Err::<i32, _>(refuse(&mut c, no(IMPORT_REQUEST_STATUS)))
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_REQUEST_TAKE,
        move |mut c: Caller<'_, ToolState>, _: u32, _: u32| {
            Err::<(), _>(refuse(&mut c, no(IMPORT_REQUEST_TAKE)))
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_TOOL_REQUEST,
        |mut c: Caller<'_, ToolState>, sat: u32, slen: u32, bat: u32, blen: u32| {
            let service = read(&mut c, sat, slen)?;
            let Ok(service) = String::from_utf8(service) else {
                return Err(wasmtime::format_err!("a tool request's service isn't UTF-8"));
            };
            let body = read(&mut c, bat, blen)?;
            let shared = c.data().shared.clone();
            Ok(make_request(&shared, &service, &body))
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_TOOL_WAIT,
        |mut c: Caller<'_, ToolState>, request: u32| -> wasmtime::Result<i32> {
            if !Hub::known(&c.data().shared.hub.lock(), request) {
                return Err(refuse(&mut c, End::WaitsOnNothing));
            }
            block(&mut c, Thread::Waiting(vec![request]), |s| Hub::status(s, request))
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_TOOL_ANY,
        |mut c: Caller<'_, ToolState>, at: u32, count: u32| -> wasmtime::Result<u32> {
            let bytes = read(&mut c, at, count.saturating_mul(4))?;
            let requests: Vec<u32> = bytes
                .chunks_exact(4)
                .map(|w| u32::from_le_bytes(w.try_into().expect("four bytes")))
                .collect();
            let known = {
                let s = c.data().shared.hub.lock();
                requests.iter().all(|&r| Hub::known(&s, r))
            };
            if requests.is_empty() || !known {
                return Err(refuse(&mut c, End::WaitsOnNothing));
            }
            let pause = c.data().shared.any_pause;
            if !pause.is_zero() {
                std::thread::sleep(pause);
            }
            let set = requests.clone();
            block(&mut c, Thread::Waiting(set), move |s| Hub::first(s, &requests))
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_TOOL_TAKE,
        |mut c: Caller<'_, ToolState>, request: u32, at: u32| {
            let shared = c.data().shared.clone();
            let Some(answer) = shared.hub.take(request) else {
                return Err(wasmtime::format_err!(
                    "tool_take({request}): the request isn't answered"
                ));
            };
            let bytes = match &answer {
                Answer::Bytes(b) => b.clone(),
                Answer::Failed => Vec::new(),
            };
            if let Err(e) = write(&mut c, at, &bytes, "tool_take") {
                shared.hub.untake(request, answer);
                return Err(e);
            }
            // What the request made is the tool's now.
            let services = &shared.services;
            services.blobs().publish(request);
            services.drives().publish(request);
            services.servers().publish(request);
            services.browsers().publish(request);
            Ok(())
        },
    )?;
    linker.func_wrap(IMPORT_MODULE, IMPORT_TOOL_IDLE, |c: Caller<'_, ToolState>| {
        c.data().shared.hub.set(c.data().thread, Thread::Idle);
    })?;
    linker.func_wrap(IMPORT_MODULE, IMPORT_TOOL_ACTIVE, |c: Caller<'_, ToolState>| {
        c.data().shared.hub.set(c.data().thread, Thread::Running);
    })?;
    linker.func_wrap(IMPORT_MODULE, IMPORT_TOOL_DONE, |c: Caller<'_, ToolState>| {
        // Under the lock: a thread about to block in `tool_join` sees the slot, or this wakes it.
        drop(c.data().shared.hub.lock());
        c.data().shared.hub.notify();
    })?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_TOOL_JOIN,
        |mut c: Caller<'_, ToolState>, slot: u32| -> wasmtime::Result<()> {
            use wrela_abi::memory::{TASK_SLOT_SIZE, TASK_SLOTS, TASK_SLOTS_END};
            if !(TASK_SLOTS..TASK_SLOTS_END).contains(&slot)
                || !(slot - TASK_SLOTS).is_multiple_of(TASK_SLOT_SIZE)
            {
                return Err(wasmtime::format_err!("tool_join({slot}): that isn't a task's slot"));
            }
            let memory = c.data().shared.memory.clone();
            block(&mut c, Thread::Joining(slot), move |_| Hub::joined(&memory, slot).then_some(()))
        },
    )?;
    // A tool has no GPU: every limit is 0.
    linker
        .func_wrap(IMPORT_MODULE, IMPORT_LIMIT, |_: Caller<'_, ToolState>, _: u32| -> i32 { 0 })?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_PRINT,
        |mut c: Caller<'_, ToolState>, at: u32, len: u32| {
            let line = String::from_utf8_lossy(&read(&mut c, at, len)?).into_owned();
            // Queued, never waited on: a console that doesn't take lines can't hold the tool.
            c.data().console.print(line);
            Ok(())
        },
    )?;
    // Every thread reads the same clock.
    linker.func_wrap(IMPORT_MODULE, IMPORT_CLOCK, |mut c: Caller<'_, ToolState>, at: u32| {
        let bytes = crate::program::clock_seconds().to_le_bytes();
        write(&mut c, at, &bytes, "clock")
    })?;
    // Phases time frames; a tool's are ignored.
    linker.func_wrap(IMPORT_MODULE, IMPORT_PHASE, |_: Caller<'_, ToolState>, _: u32, _: u32| {})?;
    // No build replaces a tool's, so what it keeps goes nowhere.
    linker.func_wrap(IMPORT_MODULE, IMPORT_KEEP, |_: Caller<'_, ToolState>, _: u32, _: u32| {})?;
    // A tool's start: its arguments and secrets (`std::tool::args`, `secret`).
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_KEPT,
        |mut c: Caller<'_, ToolState>, at: u32, cap: u32| -> wasmtime::Result<i32> {
            let shared = c.data().shared.clone();
            let n = shared.start.len().min(cap as usize);
            write(&mut c, at, &shared.start[..n], "kept")?;
            Ok(shared.start.len() as i32)
        },
    )?;
    Ok(linker)
}

/// An instance of `module` in `store`, with the tool's host functions and its memory.
fn instantiate(
    linker: &Linker<ToolState>,
    store: &mut Store<ToolState>,
    module: &Module,
) -> wasmtime::Result<wasmtime::Instance> {
    let mut linker = linker.clone();
    let memory = store.data().shared.memory.clone();
    linker.define(&*store, IMPORT_MODULE, IMPORT_MEMORY, memory)?;
    linker.instantiate(store, module)
}

#[cfg(test)]
mod pool_tests {
    /// The compiler's pool stays at 4, whatever the machine; driven builds get a worker per
    /// core, 4 to 16.
    #[test]
    fn compiles_are_bounded_and_drives_follow_the_cores() {
        assert_eq!(super::HEAVY_WORKERS, 4);
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        let drive = super::drive_workers();
        assert_eq!(drive, cores.clamp(4, super::MOST_DRIVE_WORKERS));
        assert!((4..=16).contains(&drive));
    }
}
