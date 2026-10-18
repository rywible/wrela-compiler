//! The tool host: the native host's tool profile (language.md §15.5), which runs a tool build,
//! `tool/tool.wasm`. It calls `tool_init` once, then `tool`, then `tool` again each time at least
//! one of the tool's outstanding requests is answered, until `tool` returns an exit status
//! instead of `std::tool::RUNNING`. A tool that returns `RUNNING` with nothing outstanding waits
//! on nothing, and the host stops it (exit status 70) rather than spin.
//!
//! A tool has one thread, no GPU, no ticker and no voice; its memory may grow to 4 GiB, and no
//! call has a deadline of its own (a tool may compute for minutes between requests). A run as a
//! whole may have one (`--timeout`), and an interrupt (Ctrl-C) stops it, inside a call too.
//!
//! **Services run beside the tool.** A call never waits on one: the requests a call makes are
//! carried out after it returns, on the host's worker threads (files, blobs, HTTP, programs) or
//! by the host's own loop (timers, the date), and each answer reaches the tool no sooner than
//! its next call. Every service is given one capability context ([`Caps`]), made from the
//! command line's grants, and reaches files, secrets, hosts and programs through it alone.
//!
//! **A tool test's clock is fake** ([`Options::fake_clock`], language.md §13.2). It moves only
//! when every outstanding request is one the clock answers (a timer, or a fake site's answer
//! held for its delay): then it jumps to the soonest. While a worker has a request, it waits.
//!
//! **Printing never waits.** A line the tool prints goes on a bounded queue, and a thread of the
//! host's own hands it to the [`Printer`]; a full queue drops the line and counts it. So a
//! console nobody reads can't hold a call where the clock thread can't stop it.
//!
//! **Every end is clean.** However the tool ends (its exit status, a trap, `--timeout`, an
//! interrupt, waiting on nothing), the host stops its workers (work not started is dropped,
//! work in progress gets a bounded grace), stops the clock thread that interrupts calls, and
//! releases every blob, those of requests the tool never read first ([`Ended`] says what was
//! left). A worker running a program kills its process group as it sees the end, and one
//! waiting for an HTTP response shuts its connection down and joins the client's thread;
//! then the directory the run's programs had is removed. No file work holds the blob table's
//! lock, so a stuck worker can't hold up the release.
//! A worker still running after the grace makes the end the host's failure (status 71), whatever
//! the tool's own end was, and `wrela tool` then ends the process, so no worker outlives it. The host keeps no compiled code: it compiles the tool's WASM for each run, so nothing
//! it loads comes from a directory a tool could write.

mod blobs;
mod caps;
mod console;
mod http;
mod run;
mod services;

pub use blobs::BlobBudget;
pub use caps::{Caps, Grants, Program, ProgramGrant};
pub use run::{GROUPS_ENV, launch};

use console::Console;
use services::{Answer, Services, Task, Work};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use wasmtime::{Caller, Config, Engine, Linker, Module, Store, UpdateDeadline};
use wrela_abi::stream::{self, Command};
use wrela_abi::{
    EXPORT_MEMORY, EXPORT_TOOL, EXPORT_TOOL_INIT, IMPORT_CLOCK, IMPORT_KEEP, IMPORT_KEPT,
    IMPORT_LIMIT, IMPORT_MODULE, IMPORT_PHASE, IMPORT_PRINT, IMPORT_REQUEST_STATUS,
    IMPORT_REQUEST_TAKE, IMPORT_SUBMIT, TOOL_RUNNING,
};

/// How often the run's clock thread ticks: how soon an interrupt or `--timeout` stops a call.
const EPOCH: Duration = Duration::from_millis(10);

/// How often the host's loop looks for an interrupt while it waits for answers.
const WAKE: Duration = Duration::from_millis(20);

/// The worker threads that carry out files and blobs, and those that wait on the network and
/// on programs: two pools, so that a long wait never holds up a file.
const WORKERS: usize = 4;
const LONG_WORKERS: usize = 4;

/// How long, once the tool has ended, the host waits for work in progress to finish.
const GRACE: Duration = Duration::from_secs(5);

/// A tool's memory, at most: what a 32-bit address reaches.
const MEMORY_CAP: usize = 4 << 30;

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
    pub blob_budget: BlobBudget,
    /// Set (by a Ctrl-C handler) to stop the tool.
    pub interrupt: Option<Arc<AtomicBool>>,
    /// Where the tool's `print` lines go, from a thread of the host's own: the tool never waits
    /// on it.
    pub print: Printer,
    /// The exports the host calls: `tool_init` and `tool`, or a tool test's (§13.2).
    pub entries: (String, String),
    /// A tool test's fake clock, from this date (seconds and nanoseconds since 1970): timers
    /// fire, in the order of their deadlines, as soon as the tool waits on nothing else, and
    /// `std::tool::testing` sets fakes. `None`: the system's clock, and no fakes.
    pub fake_clock: Option<(u64, u32)>,
    /// A file the host appends each program's process group to as it starts, for a supervisor
    /// that must kill them if this host can't (a tool test's, §13.2).
    pub groups: Option<std::path::PathBuf>,
    /// What only tests change.
    #[doc(hidden)]
    pub testing: Testing,
}

/// A run's settings that only tests change: the workers' grace, a slow disk under each blob
/// written to a file, a clock thread that fails to start, where the run's own directory is, a
/// pause between a program's start and its record, and a name lookup that stalls.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct Testing {
    pub grace: Duration,
    pub io_delay: Duration,
    pub fail_clock_thread: bool,
    pub work_dir: Option<std::path::PathBuf>,
    pub spawn_pause: Duration,
    pub resolve_stall: Duration,
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
            blob_budget: BlobBudget::default(),
            interrupt: None,
            print: Arc::new(|line| println!("{line}")),
            entries: (EXPORT_TOOL_INIT.to_string(), EXPORT_TOOL.to_string()),
            fake_clock: None,
            groups: None,
            testing: Testing::default(),
        }
    }
}

/// How a tool ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// `tool` returned this exit status.
    Exit(u8),
    /// `tool` returned `RUNNING` with no request outstanding.
    WaitsOnNothing,
    /// `tool` returned a number that's neither `RUNNING` nor an exit status.
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
    /// trap, waiting on nothing, a status out of range), 71 for the host's, 124 past the
    /// timeout, 130 interrupted.
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
                "the tool waits on nothing: `tool` returned RUNNING with no request outstanding",
            ),
            End::BadStatus(n) => write!(
                f,
                "`tool` returned {n}, which is neither RUNNING nor an exit status (0 to 255)"
            ),
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
    /// Worker threads still running past the grace (0 unless a service was stuck, and then the
    /// end is [`End::Host`]).
    pub workers_left: usize,
    /// The lines the tool printed that the console didn't take in time, and the host dropped.
    pub lines_dropped: u64,
}

/// A tool build: its module's bytes, compiled for each run.
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

/// The tool's part of the host, in its store.
struct ToolState {
    /// What `kept` gives: the arguments and the granted secrets.
    start: Vec<u8>,
    /// Whether the cap refused memory, in the current call.
    denied: bool,
    /// What the host couldn't do, which fails the call.
    refused: Option<String>,
    /// Why the clock thread stopped the call, if it did.
    stopped: Option<End>,
    /// The requests the current call made: number, service and bytes.
    made: Vec<(u32, String, Vec<u8>)>,
    /// Answers the tool may read.
    answered: HashMap<u32, Answer>,
    /// The requests whose answers the current call read.
    taken: Vec<u32>,
    console: Console,
    clock: Instant,
}

impl wasmtime::ResourceLimiter for ToolState {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > MEMORY_CAP {
            self.denied = true;
            return Err(wasmtime::format_err!("the tool's memory cap"));
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(true)
    }
}

/// What `std::tool::args` and `secret` read: a count and each argument, then a count and each
/// secret's name and value (text as a `u32` byte count, then UTF-8).
fn start_bytes(args: &[String], secrets: &[(String, String)]) -> Vec<u8> {
    let mut out = Vec::new();
    let text = |out: &mut Vec<u8>, s: &str| {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    };
    out.extend_from_slice(&(args.len() as u32).to_le_bytes());
    for a in args {
        text(&mut out, a);
    }
    out.extend_from_slice(&(secrets.len() as u32).to_le_bytes());
    for (name, value) in secrets {
        text(&mut out, name);
        text(&mut out, value);
    }
    out
}

/// The worker threads, which carry out services.
struct Pool {
    jobs: Option<mpsc::Sender<(u32, Task)>>,
    handles: Vec<std::thread::JoinHandle<()>>,
    /// How many workers are running, which each lowers as it returns.
    alive: Arc<(Mutex<usize>, Condvar)>,
    /// Set once the tool has ended: work not started yet is dropped.
    closing: Arc<AtomicBool>,
}

impl Pool {
    fn start(
        services: &Arc<Services>,
        done: &mpsc::Sender<(u32, Answer)>,
        name: &str,
        workers: usize,
    ) -> Pool {
        let (jobs, queue) = mpsc::channel::<(u32, Task)>();
        let queue = Arc::new(Mutex::new(queue));
        let alive = Arc::new((Mutex::new(workers), Condvar::new()));
        // The services' own: a worker waiting on a program or a response sees it too.
        let closing = services.closing.clone();
        let mut handles = Vec::new();
        for i in 0..workers {
            let (queue, running, closing) = (queue.clone(), alive.clone(), closing.clone());
            let (services, done) = (services.clone(), done.clone());
            let spawned = std::thread::Builder::new().name(format!("wrela-tool-{name}-{i}")).spawn(
                move || {
                    loop {
                        let job = queue.lock().map(|q| q.recv());
                        let Ok(Ok((request, task))) = job else { break };
                        if closing.load(Ordering::Acquire) {
                            continue;
                        }
                        let run = std::panic::AssertUnwindSafe(|| task(&services, request));
                        let answer = std::panic::catch_unwind(run).unwrap_or_else(|_| {
                            Answer::Bytes(b"\x01the host's service failed".to_vec())
                        });
                        // A failed request's answer names no blob: those made for it go back.
                        if matches!(&answer, Answer::Bytes(b) if b.first() == Some(&1)) {
                            services.blobs().rollback(request);
                        }
                        if done.send((request, answer)).is_err() {
                            break;
                        }
                    }
                    let (count, wake) = &*running;
                    *count.lock().unwrap_or_else(std::sync::PoisonError::into_inner) -= 1;
                    wake.notify_all();
                },
            );
            match spawned {
                Ok(h) => handles.push(h),
                Err(_) => {
                    let (count, _) = &*alive;
                    *count.lock().unwrap_or_else(std::sync::PoisonError::into_inner) -= 1;
                }
            }
        }
        Pool { jobs: Some(jobs), handles, alive, closing }
    }

    fn send(&self, request: u32, task: Task) {
        if let Some(jobs) = &self.jobs {
            // A pool with no workers left drops the task; the request stays outstanding until
            // the run ends.
            let _ = jobs.send((request, task));
        }
    }

    /// Tells the workers to stop: work not started is dropped, and an idle worker returns.
    fn close(&mut self) {
        self.closing.store(true, Ordering::Release);
        self.jobs = None;
    }

    /// Stops the workers: work not started is dropped, and work in progress has until `until`
    /// to finish. How many are still running then (they're left to finish alone, and the run
    /// doesn't end cleanly).
    fn stop(mut self, until: Instant) -> usize {
        self.close();
        let grace = until.saturating_duration_since(Instant::now());
        let (count, wake) = &*self.alive;
        let left = count.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (left, _) = wake
            .wait_timeout_while(left, grace, |n| *n > 0)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let left = *left;
        if left == 0 {
            for h in self.handles.drain(..) {
                let _ = h.join();
            }
        }
        left
    }
}

/// The two pools of workers: files and blobs, and long waits (HTTP, programs).
struct Pools {
    files: Pool,
    long: Pool,
}

impl Pools {
    /// Stops both, their work in progress sharing one `grace`: how many workers are left.
    fn stop(self, grace: Duration) -> usize {
        let until = Instant::now() + grace;
        let Pools { mut files, mut long } = self;
        // Both are told first, so neither's work waits on the other's grace.
        files.close();
        long.close();
        long.stop(until) + files.stop(until)
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

/// Why the run must stop now, if it must: a worker whose work can't be stopped (the host's
/// failure), an interrupt, or the timeout.
fn stop_reason(
    interrupt: &Option<Arc<AtomicBool>>,
    deadline: Option<Instant>,
    stuck: &std::sync::OnceLock<String>,
) -> Option<End> {
    if let Some(why) = stuck.get() {
        return Some(End::Host(why.clone()));
    }
    if interrupt.as_ref().is_some_and(|i| i.load(Ordering::Acquire)) {
        return Some(End::Interrupted);
    }
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return Some(End::Timeout);
    }
    None
}

/// Runs the tool to its end, and cleans up after it.
pub fn run(build: &ToolBuild, options: Options) -> Ended {
    let failed = |why: String| Ended {
        end: End::Host(why),
        blobs_released: (0, 0),
        workers_left: 0,
        lines_dropped: 0,
    };
    let deadline = match options.timeout.map(|t| Instant::now().checked_add(t)) {
        None => None,
        Some(Some(d)) => Some(d),
        Some(None) => return failed("the timeout is further off than the clock reaches".into()),
    };
    let mut config = Config::new();
    config.wasm_threads(true).epoch_interruption(true);
    let engine = match Engine::new(&config) {
        Ok(e) => e,
        Err(e) => return failed(format!("the tool engine: {e:#}")),
    };
    let start = start_bytes(&options.args, options.caps.secrets());
    let Options {
        caps, blob_budget, interrupt, print, testing, entries, fake_clock, groups, ..
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
        stuck: Arc::new(std::sync::OnceLock::new()),
    });
    let (done, answers) = mpsc::channel();
    let pools = Pools {
        files: Pool::start(&services, &done, "files", WORKERS),
        long: Pool::start(&services, &done, "long", LONG_WORKERS),
    };
    drop(done);
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
    let mut run = Run {
        pools: &pools,
        services: &services,
        answers: &answers,
        interrupt: &interrupt,
        deadline,
        outstanding: HashSet::new(),
        origin: Instant::now(),
        fake: fake_clock.map(|(s, n)| Fake { now: Duration::ZERO, utc: Duration::new(s, n) }),
        timed: BinaryHeap::new(),
        timed_answers: HashMap::new(),
        due: HashMap::new(),
        arrived: Vec::new(),
    };
    // The store, and the console's sending end in it, go when `drive` returns.
    let mut end = run.drive(&engine, build, &entries, start, console);
    ticker.stop();
    let workers_left = pools.stop(testing.grace);
    let lines_dropped = writer.stop(testing.grace);
    // Its programs' files and `HOME`: no program runs now (each worker kills its own as the
    // tool ends), unless a worker is still past its grace.
    if let Some(Ok(dir)) = services.work.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
    // A worker past its grace may still be changing files: that isn't a clean end.
    if workers_left > 0 {
        end = End::Host(format!(
            "{workers_left} of the host's workers were still running {:?} after the tool ended ({end})",
            testing.grace
        ));
    }
    // No file work holds the table's lock, so a stuck worker can't keep this waiting.
    let blobs_released =
        services.blobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).release_all();
    Ended { end, blobs_released, workers_left, lines_dropped }
}

/// A tool test's fake clock: the time since the run started, which moves only when the tool
/// waits on nothing but the clock, and the date at the start.
struct Fake {
    now: Duration,
    utc: Duration,
}

/// A run in progress: what's outstanding, and where answers come from.
struct Run<'a> {
    pools: &'a Pools,
    services: &'a Arc<Services>,
    answers: &'a mpsc::Receiver<(u32, Answer)>,
    interrupt: &'a Option<Arc<AtomicBool>>,
    deadline: Option<Instant>,
    /// Requests made and not answered yet.
    outstanding: HashSet<u32>,
    /// When the run started: the system clock's times are from it.
    origin: Instant,
    /// A tool test's fake clock, if this is one.
    fake: Option<Fake>,
    /// Answers given to the tool at a time on the run's clock, soonest first: timers', and a
    /// fake site's held for its delay.
    timed: BinaryHeap<Reverse<(Duration, u32)>>,
    timed_answers: HashMap<u32, Answer>,
    /// When requests a worker carries out may be answered, if not at once: a fake site's delay.
    due: HashMap<u32, Duration>,
    /// Answers that have arrived since the last call.
    arrived: Vec<(u32, Answer)>,
}

impl Run<'_> {
    /// The time on the run's clock: since its start, real or fake.
    fn now(&self) -> Duration {
        match &self.fake {
            Some(f) => f.now,
            None => self.origin.elapsed(),
        }
    }

    /// The date now, as `time/utc` answers it: the system's, or the fake clock's.
    fn utc(&self) -> Answer {
        let now = match &self.fake {
            Some(f) => f.utc + f.now,
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default(),
        };
        let mut payload = vec![0];
        payload.extend_from_slice(&now.as_secs().to_le_bytes());
        payload.extend_from_slice(&now.subsec_nanos().to_le_bytes());
        Answer::Bytes(payload)
    }

    /// `answer` to `request`, given to the tool now, or at the time it's due.
    fn answered(&mut self, request: u32, answer: Answer) {
        match self.due.remove(&request) {
            Some(at) if at > self.now() => {
                self.timed.push(Reverse((at, request)));
                self.timed_answers.insert(request, answer);
            }
            _ => self.arrived.push((request, answer)),
        }
    }

    fn drive(
        &mut self,
        engine: &Engine,
        build: &ToolBuild,
        (init_export, tool_export): &(String, String),
        start: Vec<u8>,
        console: Console,
    ) -> End {
        let module = match Module::new(engine, &build.wasm) {
            Ok(m) => m,
            Err(e) => return End::Host(format!("compiling the tool build: {e:#}")),
        };
        let shared = module
            .imports()
            .any(|i| matches!(i.ty(), wasmtime::ExternType::Memory(m) if m.is_shared()));
        if shared || module.get_export(EXPORT_MEMORY).is_none() {
            return End::Host("this isn't a tool build: its memory isn't its own".into());
        }
        if module.get_export(tool_export).is_none() {
            return End::Host(format!(
                "this isn't a tool build: it doesn't export `{tool_export}`"
            ));
        }
        let state = ToolState {
            start,
            denied: false,
            refused: None,
            stopped: None,
            made: Vec::new(),
            answered: HashMap::new(),
            taken: Vec::new(),
            console,
            clock: Instant::now(),
        };
        let mut store = Store::new(engine, state);
        store.limiter(|s| s);
        store.set_epoch_deadline(1);
        let (interrupt, deadline) = (self.interrupt.clone(), self.deadline);
        let stuck = self.services.stuck.clone();
        store.epoch_deadline_callback(move |mut cx| {
            if let Some(why) = stop_reason(&interrupt, deadline, &stuck) {
                cx.data_mut().stopped = Some(why);
                return Ok(UpdateDeadline::Interrupt);
            }
            Ok(UpdateDeadline::Continue(1))
        });
        let linker = match linker(engine) {
            Ok(l) => l,
            Err(e) => return End::Host(format!("{e:#}")),
        };
        let instance = match linker.instantiate(&mut store, &module) {
            Ok(i) => i,
            Err(e) => return classify(&mut store, None, &e),
        };
        let memory = instance.get_memory(&mut store, EXPORT_MEMORY);
        let tool = match instance.get_typed_func::<(), u32>(&mut store, tool_export) {
            Ok(f) => f,
            Err(e) => return End::Host(format!("`{tool_export}`: {e:#}")),
        };
        if instance.get_export(&mut store, init_export).is_some() {
            let init = match instance.get_typed_func::<(), ()>(&mut store, init_export) {
                Ok(f) => f,
                Err(e) => return End::Host(format!("`{init_export}`: {e:#}")),
            };
            if let Err(end) = call(&mut store, memory, |s| init.call(s, ())) {
                return end;
            }
            self.after_call(&mut store);
        }
        loop {
            if let Some(why) = stop_reason(self.interrupt, self.deadline, &self.services.stuck) {
                return why;
            }
            let status = match call(&mut store, memory, |s| tool.call(s, ())) {
                Ok(status) => status,
                Err(end) => return end,
            };
            self.after_call(&mut store);
            if status != TOOL_RUNNING {
                return match u8::try_from(status) {
                    Ok(n) => End::Exit(n),
                    Err(_) => End::BadStatus(status),
                };
            }
            if let Err(end) = self.wait() {
                return end;
            }
            let s = store.data_mut();
            for (request, answer) in self.arrived.drain(..) {
                self.outstanding.remove(&request);
                s.answered.insert(request, answer);
            }
        }
    }

    /// Carries out the requests the call made, and gives the blobs of the answers it read to
    /// the tool.
    fn after_call(&mut self, store: &mut Store<ToolState>) {
        let s = store.data_mut();
        if !s.taken.is_empty() {
            let mut blobs =
                self.services.blobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            for request in s.taken.drain(..) {
                blobs.publish(request);
            }
        }
        for (request, service, body) in std::mem::take(&mut s.made) {
            self.outstanding.insert(request);
            match services::route(self.services, &service, &body) {
                Work::Now(answer) => self.arrived.push((request, answer)),
                Work::Timer(after) => {
                    let at = self.now() + after;
                    self.timed.push(Reverse((at, request)));
                    self.timed_answers.insert(request, Answer::Bytes(vec![0]));
                }
                Work::Worker(task) => self.pools.files.send(request, task),
                Work::Long(task) => self.pools.long.send(request, task),
                Work::Held(after, task) => {
                    self.due.insert(request, self.now() + after);
                    self.pools.long.send(request, task);
                }
                Work::Utc => self.arrived.push((request, self.utc())),
                Work::SetUtc(seconds, nanos) => {
                    let answer = match &mut self.fake {
                        Some(f) => {
                            let set = Duration::new(seconds, nanos);
                            // The fake time since the start stays; the date it reads moves.
                            f.utc = set.saturating_sub(f.now);
                            Answer::Bytes(vec![0])
                        }
                        None => Answer::Bytes(b"\x01only a tool test's clock is set".to_vec()),
                    };
                    self.arrived.push((request, answer));
                }
            }
        }
    }

    /// Waits until at least one answer has arrived: an error to end the run if nothing is
    /// outstanding, the timeout passes or an interrupt comes first. On a fake clock, once every
    /// outstanding request waits on the clock alone, the clock jumps to the soonest.
    fn wait(&mut self) -> Result<(), End> {
        loop {
            while let Ok((request, answer)) = self.answers.try_recv() {
                self.answered(request, answer);
            }
            let now = self.now();
            while let Some(&Reverse((at, request))) = self.timed.peek()
                && at <= now
            {
                self.timed.pop();
                let answer = self.timed_answers.remove(&request).unwrap_or(Answer::Failed);
                self.arrived.push((request, answer));
            }
            if !self.arrived.is_empty() {
                return Ok(());
            }
            if self.outstanding.is_empty() {
                return Err(End::WaitsOnNothing);
            }
            if let Some(why) = stop_reason(self.interrupt, self.deadline, &self.services.stuck) {
                return Err(why);
            }
            // Quiescent: nothing outstanding but what the clock answers.
            if let Some(fake) = &mut self.fake
                && self.outstanding.len() == self.timed_answers.len()
                && let Some(&Reverse((at, _))) = self.timed.peek()
            {
                fake.now = fake.now.max(at);
                continue;
            }
            let mut wait = WAKE;
            if self.fake.is_none()
                && let Some(&Reverse((at, _))) = self.timed.peek()
            {
                wait = wait.min(at.saturating_sub(now));
            }
            if let Some(d) = self.deadline {
                wait = wait.min(d.saturating_duration_since(Instant::now()));
            }
            match self.answers.recv_timeout(wait) {
                Ok((request, answer)) => self.answered(request, answer),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // No worker is left to answer: only the clock can.
                    if self.timed.is_empty() {
                        return Err(End::Host("the host's workers stopped".into()));
                    }
                    std::thread::sleep(wait);
                }
            }
        }
    }
}

/// A call into the tool: its result, or how the run ends.
fn call<R>(
    store: &mut Store<ToolState>,
    memory: Option<wasmtime::Memory>,
    f: impl FnOnce(&mut Store<ToolState>) -> wasmtime::Result<R>,
) -> Result<R, End> {
    let s = store.data_mut();
    s.denied = false;
    s.refused = None;
    f(store).map_err(|e| classify(store, memory, &e))
}

/// Why `e`, a call's error, happened: the clock thread's stop first, then the host's limits,
/// then the tool's own trap, with its panic's message from `memory` if it panicked.
fn classify(
    store: &mut Store<ToolState>,
    memory: Option<wasmtime::Memory>,
    e: &wasmtime::Error,
) -> End {
    let s = store.data_mut();
    if let Some(why) = s.stopped.take() {
        return why;
    }
    if s.denied {
        return End::Host("the tool asked for more memory than 4 GiB".into());
    }
    if let Some(why) = s.refused.take() {
        return End::Host(why);
    }
    match e.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::Interrupt) => End::Interrupted,
        Some(wasmtime::Trap::StackOverflow) => {
            End::Trap("the call went deeper than the stack allows".into())
        }
        Some(t) => {
            use wrela_abi::memory::{PANIC_CAP, THREAD_MAIN, panic_at, panic_message};
            let at = panic_at(THREAD_MAIN) as usize;
            let message = memory.and_then(|m| {
                let bytes = m.data(&*store).get(at..at + 4 + PANIC_CAP as usize)?.to_vec();
                panic_message(&bytes)
            });
            End::Trap(message.unwrap_or_else(|| t.to_string()))
        }
        None => End::Host(format!("{e:#}")),
    }
}

/// The program's memory, from a host function's caller.
fn memory(c: &mut Caller<'_, ToolState>) -> wasmtime::Result<wasmtime::Memory> {
    c.get_export(EXPORT_MEMORY)
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmtime::format_err!("the tool has no memory export"))
}

/// Bytes `[at, at + len)` of the tool's memory.
fn read(c: &mut Caller<'_, ToolState>, at: u32, len: u32) -> wasmtime::Result<Vec<u8>> {
    let m = memory(c)?;
    let data = m.data(&*c);
    let end = (at as usize).checked_add(len as usize).filter(|&e| e <= data.len());
    let Some(end) = end else {
        return Err(wasmtime::format_err!(
            "({at}, {len}) reaches past the end of the tool's memory"
        ));
    };
    Ok(data[at as usize..end].to_vec())
}

/// Writes `bytes` at `at` in the tool's memory.
fn write(c: &mut Caller<'_, ToolState>, at: u32, bytes: &[u8], what: &str) -> wasmtime::Result<()> {
    let m = memory(c)?;
    m.write(&mut *c, at as usize, bytes).map_err(|_| {
        wasmtime::format_err!("{what}({at}) reaches past the end of the tool's memory")
    })
}

/// The host functions a tool build may import; any other import refuses to link.
fn linker(engine: &Engine) -> wasmtime::Result<Linker<ToolState>> {
    let mut linker = Linker::new(engine);
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_SUBMIT,
        |mut c: Caller<'_, ToolState>, at: u32, len: u32| {
            let batch = read(&mut c, at, len)?;
            let commands = match stream::decode(&batch) {
                Ok(commands) => commands,
                Err(e) => return Err(wasmtime::format_err!("a malformed command batch: {e}")),
            };
            for cmd in commands {
                let s = c.data_mut();
                match cmd {
                    Command::HostRequest { request, service, body } => {
                        s.made.push((request, service.to_string(), body.to_vec()));
                    }
                    other => {
                        // A tool's requests are its services': storage, fetches and posts are a
                        // client's, and a tool records no GPU work.
                        let why = format!(
                            "a tool host can't carry out {:?}: a tool's files are `std::tool::files`",
                            other.opcode()
                        );
                        s.refused = Some(why.clone());
                        return Err(wasmtime::format_err!(why));
                    }
                }
            }
            Ok(())
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_REQUEST_STATUS,
        |c: Caller<'_, ToolState>, request: u32| -> i32 {
            match c.data().answered.get(&request) {
                None => wrela_abi::REQUEST_PENDING,
                Some(Answer::Failed) => wrela_abi::REQUEST_FAILED,
                Some(Answer::Bytes(b)) => b.len() as i32,
            }
        },
    )?;
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_REQUEST_TAKE,
        |mut c: Caller<'_, ToolState>, request: u32, at: u32| {
            let Some(answer) = c.data_mut().answered.remove(&request) else {
                return Err(wasmtime::format_err!(
                    "request_take({request}): the request isn't answered"
                ));
            };
            let bytes = match &answer {
                Answer::Bytes(b) => b.clone(),
                Answer::Failed => Vec::new(),
            };
            if let Err(e) = write(&mut c, at, &bytes, "request_take") {
                c.data_mut().answered.insert(request, answer);
                return Err(e);
            }
            c.data_mut().taken.push(request);
            Ok(())
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
    linker.func_wrap(IMPORT_MODULE, IMPORT_CLOCK, |mut c: Caller<'_, ToolState>, at: u32| {
        let bytes = c.data().clock.elapsed().as_secs_f64().to_le_bytes();
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
            let start = c.data().start.clone();
            let n = start.len().min(cap as usize);
            write(&mut c, at, &start[..n], "kept")?;
            Ok(start.len() as i32)
        },
    )?;
    Ok(linker)
}
