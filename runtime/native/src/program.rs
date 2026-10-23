//! The CPU side: a program's WASM under wasmtime, its one import, and the batches it submits.
//!
//! [`Program`] is generic over the [`Executor`] that carries out decoded commands, so the ABI
//! rules (imports, exports, bounds, decoding, sequencing, the state hash) are tested here
//! without a GPU; [`crate::gpu::Gpu`] is the real executor.

use crate::error::{Error, Result};
use crate::shared;
use crate::trace::Trace;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use wasmtime::SharedMemory;
use wasmtime::{
    Engine, ExternType, Func, FuncType, Instance, Linker, Module, Store, TypedFunc, Val, ValType,
};
use wrela_abi::check::{Checker, CommandError};
use wrela_abi::hash::StateHash;
use wrela_abi::input::{EVENT_SIZE, Event, Queue, Scripted};
use wrela_abi::lines::{self, Lines};
use wrela_abi::memory::{self, MAX_WORKERS};
use wrela_abi::stream::{self, Command, Sequencer};
use wrela_abi::ticks::{Tick, TickLog, frame_time, lockstep_ticks};
use wrela_abi::{
    AUDIO_QUANTUM, EXPORT_AUDIO, EXPORT_FRAME, EXPORT_INIT, EXPORT_MEMORY, EXPORT_TICK,
    EXPORT_WORKER, HOST_FUNCTIONS, IMPORT_AUDIO, IMPORT_CLOCK, IMPORT_INPUT, IMPORT_KEEP,
    IMPORT_KEPT, IMPORT_LIMIT, IMPORT_MEMORY, IMPORT_MODULE, IMPORT_PAYLOAD, IMPORT_PHASE,
    IMPORT_PRINT, IMPORT_REQUEST_STATUS, IMPORT_REQUEST_TAKE, IMPORT_SUBMIT, IMPORT_TICK, Manifest,
    REQUEST_FAILED, REQUEST_MISSING, REQUEST_PENDING,
};

/// The process's one wasmtime engine: every program compiles and runs under it, as wasmtime
/// intends (an engine is costly to make, and cheap to share).
static ENGINE: LazyLock<Engine> = LazyLock::new(|| engine(false));

/// [`ENGINE`], counting fuel: for running a program's frames in a test, where a call that
/// does too much work fails rather than hangs, after the same work on every machine.
static METERED: LazyLock<Engine> = LazyLock::new(|| engine(true));

/// [`METERED`], with epoch interruption too: for a build's constants that the tool host's
/// `toolchain` service computes (language.md §15.5), whose calls a [`Stop`] ends when the
/// request is cancelled, while the fuel still bounds their work.
static METERED_STOPPABLE: LazyLock<Engine> = LazyLock::new(|| engine_with(true, true));

/// The engine that counts fuel ([`METERED`]): the compiler runs a build's constants and tests
/// under it too, so a process that builds and runs programs makes one.
pub fn metered_engine() -> &'static Engine {
    &METERED
}

/// [`ENGINE`] with epoch interruption, for a program the tool host drives (language.md §15.5's
/// `drive`): the tool host compiles a driven build's code for it, and the build's own process
/// runs that code on it. Nothing moves its epoch and each store's deadline is far past reach
/// ([`never_due`]), so no call is interrupted: the tool host stops a driven build by killing its
/// process. Its code is the old in-process driven engine's, which measured faster on parallel
/// vector work than [`ENGINE`]'s (Cranelift lays out the loops otherwise).
static DRIVEN: LazyLock<Engine> = LazyLock::new(|| engine_with(false, true));

/// The engine a driven build runs on ([`DRIVEN`]).
pub(crate) fn driven_engine() -> &'static Engine {
    &DRIVEN
}

/// Gives `store`, if it's of [`DRIVEN`], a deadline that never comes: nothing moves that
/// engine's epoch.
fn never_due<T>(store: &mut Store<T>) {
    if Engine::same(store.engine(), &DRIVEN) {
        store.set_epoch_deadline(u64::MAX / 2);
    }
}

/// The engine that counts fuel and whose calls a [`Stop`] ends ([`METERED_STOPPABLE`]): a
/// store of its must have a stop installed ([`Stop::install`]), or its first call traps.
pub fn stoppable_metered_engine() -> &'static Engine {
    &METERED_STOPPABLE
}

fn engine(fuel: bool) -> Engine {
    engine_with(fuel, false)
}

fn engine_with(fuel: bool, interruptible: bool) -> Engine {
    let mut config = wasmtime::Config::new();
    // A program's memory is shared with its workers (wrela_abi::memory). Wasmtime's threads are
    // a tier 2 feature: no security updates for old releases.
    config.wasm_threads(true).shared_memory(true).consume_fuel(fuel);
    config.epoch_interruption(interruptible);
    // No native unwind info: wasmtime's backtraces and its handling of a panic in a host
    // function don't need it, and macOS registers it for every function under one lock per
    // process, so a test binary's threads queued on it as they loaded programs.
    config.native_unwind_info(false);
    Engine::new(&config).expect("wasmtime's configuration is valid")
}

/// How often the stoppable engine's epoch moves on: how soon a [`Stop`] ends a call.
const EPOCH: std::time::Duration = std::time::Duration::from_millis(10);

/// The thread that moves [`METERED_STOPPABLE`]'s epoch on, while a [`Stop`] is alive: how many
/// are, and the thread with its flag to end.
type Ticking = Option<(usize, Arc<AtomicBool>, std::thread::JoinHandle<()>)>;
static TICKING: std::sync::Mutex<Ticking> = std::sync::Mutex::new(None);

/// What ends the calls of a build's constants that the tool host's `toolchain` service computes
/// (language.md §15.5), on the stoppable engine ([`stoppable_metered_engine`]): once it's
/// stopped, past its deadline, or once what it was made with says so, every call on any of its
/// threads traps with an interrupt within about 10 ms, as the engine's epoch moves. The first
/// thread that sees it due stops it for good, so every other thread of that call stops too,
/// whatever deadline is set after. A wait in a call (`atomic.wait`) isn't interrupted, but a
/// helper's trap wakes what waits for it. While a `Stop` is alive, a thread of the host's moves
/// the epoch on; the last one's drop ends and joins it.
#[derive(Clone)]
pub struct Stop(Arc<StopState>);

struct StopState {
    stopped: AtomicBool,
    /// Nanoseconds after `base` from which calls stop; `u64::MAX`: none.
    deadline: AtomicU64,
    base: std::time::Instant,
    /// Another reason to stop, asked on each epoch ([`Stop::when_set`]).
    also: Option<Box<dyn Fn() -> bool + Send + Sync>>,
}

impl Stop {
    /// A stop that is due once `flag` is set: a cancelled compile's (the tool host's toolchain
    /// requests, language.md §15.5), so its constants stop within about 10 ms.
    pub fn when_set(flag: Arc<AtomicBool>) -> Result<Stop> {
        Stop::ticking(Some(Box::new(move || flag.load(Ordering::Acquire))))
    }

    /// A stop whose epochs [`TICKING`]'s thread moves on, also due once `also` says so.
    fn ticking(also: Option<Box<dyn Fn() -> bool + Send + Sync>>) -> Result<Stop> {
        let mut ticking = crate::locked(&TICKING);
        match ticking.as_mut() {
            Some((count, _, _)) => *count += 1,
            None => {
                let running = Arc::new(AtomicBool::new(true));
                let r = running.clone();
                let thread = std::thread::Builder::new()
                    .name("wrela-stop-epochs".into())
                    .spawn(move || {
                        while r.load(Ordering::Acquire) {
                            std::thread::sleep(EPOCH);
                            METERED_STOPPABLE.increment_epoch();
                        }
                    })
                    .map_err(|e| {
                        Error::Program(format!("can't start the thread that stops calls: {e}"))
                    })?;
                *ticking = Some((1, running, thread));
            }
        }
        Ok(Stop(Arc::new(StopState {
            stopped: AtomicBool::new(false),
            deadline: AtomicU64::new(u64::MAX),
            base: std::time::Instant::now(),
            also,
        })))
    }

    /// Stops every call from now on.
    pub fn stop(&self) {
        self.0.stopped.store(true, Ordering::Release);
    }

    /// Calls stop from `at` on (`None`: no deadline).
    pub fn set_deadline(&self, at: Option<std::time::Instant>) {
        let nanos = at.map_or(u64::MAX, |at| {
            u64::try_from(at.saturating_duration_since(self.0.base).as_nanos()).unwrap_or(u64::MAX)
        });
        self.0.deadline.store(nanos, Ordering::Release);
    }

    /// Whether calls stop now: it's stopped, or past its deadline.
    pub fn is_due(&self) -> bool {
        let s = &self.0;
        s.stopped.load(Ordering::Acquire)
            || s.base.elapsed().as_nanos() >= u128::from(s.deadline.load(Ordering::Acquire))
            || s.also.as_ref().is_some_and(|also| also())
    }

    /// Whether it was stopped ([`Stop::stop`]), not only past a deadline.
    pub fn is_stopped(&self) -> bool {
        self.0.stopped.load(Ordering::Acquire)
    }

    /// Makes `store`'s calls end when this is due.
    pub fn install<T>(&self, store: &mut Store<T>) {
        let stop = self.clone();
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(move |_| {
            Ok(if stop.is_due() {
                // Latched before this thread stops: a deadline cleared once this call has
                // answered doesn't let another of its threads (a helper held in a wait) go on.
                stop.stop();
                wasmtime::UpdateDeadline::Interrupt
            } else {
                wasmtime::UpdateDeadline::Continue(1)
            })
        });
    }
}

impl Drop for StopState {
    fn drop(&mut self) {
        let mut ticking = crate::locked(&TICKING);
        if let Some((count, _, _)) = ticking.as_mut() {
            *count -= 1;
            if *count == 0
                && let Some((_, running, thread)) = ticking.take()
            {
                running.store(false, Ordering::Release);
                let _ = thread.join();
            }
        }
    }
}

/// Why the program's last call failed, for a caller that reports it against the source: the
/// WASM offset of each frame, innermost first; the trap, if it was one; and the message a
/// panic left.
#[derive(Clone, Debug)]
pub struct Failure {
    pub frames: Vec<u32>,
    pub trap: Option<wasmtime::Trap>,
    pub panic: Option<String>,
    /// Where in the source it happened, if the build says (its trap locations), else the
    /// innermost frame's code offset.
    pub at: Option<String>,
    /// The ticker's tick it happened in, if it happened in one (alone, or in a lockstep frame).
    pub tick: Option<u32>,
}

/// Carries out commands that have already been decoded, sequenced and checked.
pub(crate) trait Executor: 'static {
    fn execute(&mut self, cmd: &Command<'_>) -> Result<()>;
    /// Begins every frame, including frames driven one at a time by a caller.
    fn begin_frame(&mut self) {}
    /// Called after each call of `frame` returns: finish (submit) the frame's work.
    fn end_frame(&mut self) -> Result<()>;
    /// A buffer's bytes `offset..offset + size`, for a `ReadBuffer` request: the GPU's work
    /// recorded so far, done.
    fn read_back(&mut self, handle: u32, offset: u32, size: u32) -> Result<Vec<u8>>;
    /// The device's limits, which commands are checked against and `wrela.limit` gives.
    fn limits(&self) -> wrela_abi::Limits {
        wrela_abi::Limits::DEFAULT
    }
    /// Hot reload: forget the old build's resources, and take the new build's pipelines.
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    fn reload(&mut self, _manifest: &Manifest, _shaders: &[String]) -> Result<()> {
        Ok(())
    }
}

/// A numeric WASM value, for calling a program's other exports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl Value {
    fn to_val(self) -> Val {
        match self {
            Value::I32(v) => Val::I32(v),
            Value::I64(v) => Val::I64(v),
            Value::F32(v) => Val::F32(v.to_bits()),
            Value::F64(v) => Val::F64(v.to_bits()),
        }
    }

    fn from_val(v: &Val) -> Option<Value> {
        Some(match *v {
            Val::I32(v) => Value::I32(v),
            Val::I64(v) => Value::I64(v),
            Val::F32(bits) => Value::F32(f32::from_bits(bits)),
            Val::F64(bits) => Value::F64(f64::from_bits(bits)),
            _ => return None,
        })
    }

    fn ty(self) -> ValType {
        match self {
            Value::I32(_) => ValType::I32,
            Value::I64(_) => ValType::I64,
            Value::F32(_) => ValType::F32,
            Value::F64(_) => ValType::F64,
        }
    }
}

/// A WASM type's name, as in a signature: `i32`, `f64`, ... (`other` for a non-numeric type).
fn type_name(ty: ValType) -> &'static str {
    match ty {
        ValType::I32 => "i32",
        ValType::I64 => "i64",
        ValType::F32 => "f32",
        ValType::F64 => "f64",
        _ => "other",
    }
}

/// The store's data: what `wrela.submit` needs.
struct State<E> {
    /// The program's memory, which its workers share.
    memory: SharedMemory,
    checker: Checker,
    executor: E,
    hash: StateHash,
    sequencer: Sequencer,
    /// Checked batches wait until the export returns successfully. The program may reuse its
    /// command memory between submits, so each batch has its own bytes.
    pending: Vec<Vec<u8>>,
    /// A trapped export restores the sequence state alongside the checker's resources.
    before_call: Option<Sequencer>,
    /// The typed error behind the most recent trap raised by `submit`, so the caller gets it
    /// rather than wasmtime's flattened message.
    failure: Option<Error>,
    /// Copies of the submitted batches, when recording.
    batches: Option<Vec<Vec<u8>>>,
    /// The first error the executor gave: what it holds isn't known after it, so every later
    /// command fails. (A command the sequencer or the checker rejects leaves no trace.)
    failed: Option<String>,
    requests: Requests,
    /// Whether the instance has started: its start function, which runs as it's
    /// instantiated, may not call the host.
    started: bool,
    /// The voice `wrela.audio` started: its task and context.
    voice: Option<(u32, u32)>,
    /// Whether the module has the audio thread entry that starting a voice requires.
    has_audio_entry: bool,
    /// The ticker `wrela.tick` started: its task, context and rate.
    ticker: Option<(u32, u32, u32)>,
    /// Whether the module has the ticker thread entry that starting a ticker requires.
    has_tick_entry: bool,
    /// What the program printed and timed, on any of its threads (`std::io::print`,
    /// `std::time::phase`): each line also shown on stderr unless quiet.
    trace: Trace,
    /// Input events not yet read (`wrela.input`).
    input: Queue,
    /// What the build this one replaced kept (`wrela.kept`), and what this one keeps
    /// (`wrela.keep`): hot reload.
    kept: Vec<u8>,
    keeping: Vec<u8>,
    /// What the program asked of other builds during the current call (`std::reload`), for the
    /// host to carry out after it; and whether its host does ([`Program::hand_over_here`]).
    /// Where it doesn't, each such request fails.
    handing: Vec<Handing>,
    hands_over: bool,
}

/// What a program asks of another build (`std::reload`): the path is as the program wrote it,
/// relative to its build.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(feature = "gpu"), allow(dead_code))]
pub(crate) enum Handing {
    Prepare { request: u32, path: String },
    Unprepare { path: String },
    HandOver { request: u32, path: String },
}

/// What a start function that calls the host gets.
const CALLED_WHILE_STARTING: &str =
    "its start function called the host; a wrela program calls the host only from an export";

/// Where a program's requests read and write (language.md §17.1): its build, for `Fetch`, its
/// storage directory, for `StorageRead` and `StorageWrite`, and what answers its `Post`s.
#[derive(Clone, Debug, Default)]
pub(crate) struct Io {
    pub build: Option<PathBuf>,
    /// A driven build's directory, opened: when it's there, `Fetch` reads through it alone, one
    /// part at a time, following no link (`crate::confined`), and never by `build`'s path.
    pub build_dir: Option<Arc<cap_std::fs::Dir>>,
    pub storage: Option<PathBuf>,
    /// A driven build's storage directory, opened: when it's there, `StorageRead` and
    /// `StorageWrite` go through it alone, one part at a time, following no link, and never by
    /// `storage`'s path.
    pub storage_dir: Option<Arc<cap_std::fs::Dir>>,
    pub post: Option<PostHandler>,
    pub host: Option<HostHandler>,
    /// Don't show printed lines on stderr: the caller shows them ([`crate::Options::quiet`]).
    pub quiet: bool,
    /// Its posts wait for the host's user, who takes them ([`Program::take_posts`]) and
    /// answers each ([`Program::answer`]): a driven build's go to its tool. `post` answers none.
    pub queue_posts: bool,
}

/// The most posts a program's host's user holds unanswered: past them, a post fails at once.
pub(crate) const MOST_QUEUED_POSTS: usize = 64;

/// The most bytes a queued post's body has: past them, it fails at once.
pub(crate) const MOST_QUEUED_POST: usize = 512 << 10;

/// A post waiting for the host's user to answer it: its request, its URL and its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedPost {
    pub request: u32,
    pub url: String,
    pub body: Vec<u8>,
}

/// A post's answer, from the URL and the body.
type Answer = dyn Fn(&str, &[u8]) -> std::result::Result<Vec<u8>, String> + Send + Sync;

/// What answers a program's posts (`std::io::post`): given the URL (relative to the build) and
/// the body, the reply's bytes, or why there's none. The host that serves the build answers
/// them; a native host has none unless its user gives one.
#[derive(Clone)]
pub struct PostHandler(pub Arc<Answer>);

impl PostHandler {
    pub fn new(
        f: impl Fn(&str, &[u8]) -> std::result::Result<Vec<u8>, String> + Send + Sync + 'static,
    ) -> PostHandler {
        PostHandler(Arc::new(f))
    }
}

/// Application-host capabilities: named services receive opaque bytes and answer requests.
#[derive(Clone)]
pub struct HostHandler(pub Arc<Answer>);

impl HostHandler {
    pub fn new(
        f: impl Fn(&str, &[u8]) -> std::result::Result<Vec<u8>, String> + Send + Sync + 'static,
    ) -> HostHandler {
        HostHandler(Arc::new(f))
    }
}

impl std::fmt::Debug for HostHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostHandler")
    }
}

impl std::fmt::Debug for PostHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PostHandler")
    }
}

/// The program's requests: answered ones it hasn't taken, and ones answered during the current
/// call, which it sees from the next call on.
#[derive(Default)]
struct Requests {
    io: Io,
    answered: HashMap<u32, std::result::Result<Vec<u8>, String>>,
    arriving: Vec<(u32, std::result::Result<Vec<u8>, String>)>,
    /// The storage reads among them that failed because their path holds nothing.
    missing: HashSet<u32>,
    /// Requests the host answers after a call ([`Program::answer`]): another build's.
    waiting: Vec<u32>,
    /// IDs in this call's checked batches, before their requests are carried out.
    reserved: HashSet<u32>,
    /// Posts that wait for the host's user ([`Io::queue_posts`]), not yet taken.
    posts: Vec<QueuedPost>,
    /// The requests of posts taken and not yet answered.
    taken: HashSet<u32>,
}

impl Requests {
    /// A request's number, checked to be free.
    fn start(&self, request: u32, op: stream::Opcode) -> Result<()> {
        if self.answered.contains_key(&request)
            || self.arriving.iter().any(|a| a.0 == request)
            || self.waiting.contains(&request)
            || self.reserved.contains(&request)
        {
            return Err(Error::Command(CommandError {
                opcode: op,
                why: format!("request {request} is already in use"),
            }));
        }
        Ok(())
    }

    /// The file a storage path or a URL names, under `dir`.
    fn file(dir: &Option<PathBuf>, path: &str, what: &str) -> std::result::Result<PathBuf, String> {
        match dir {
            Some(d) => {
                let file = d.join(path);
                let mut prefix = d.clone();
                for part in path.split('/') {
                    prefix.push(part);
                    match std::fs::symlink_metadata(&prefix) {
                        Ok(metadata) if metadata.file_type().is_symlink() => {
                            return Err(format!("the path `{path}` crosses a symbolic link"));
                        }
                        Ok(_) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                        Err(e) => return Err(e.to_string()),
                    }
                }
                Ok(file)
            }
            None => Err(format!("this host has no {what}")),
        }
    }

    /// The bytes of the file `path` names: through `handle` if it's opened, else under `dir`;
    /// `None` if nothing is there.
    fn read(
        handle: &Option<Arc<cap_std::fs::Dir>>,
        dir: &Option<PathBuf>,
        path: &str,
        what: &str,
    ) -> std::result::Result<Option<Vec<u8>>, String> {
        match handle {
            Some(handle) => crate::confined::read_if_there(handle, path, u64::MAX),
            None => Self::file(dir, path, what).and_then(|f| match std::fs::read(f) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                read => read.map(Some).map_err(|e| e.to_string()),
            }),
        }
    }

    /// Carries out an IO request now; its answer arrives at the end of the call.
    fn io(&mut self, cmd: &Command<'_>) -> Result<()> {
        let (request, answer) = match cmd {
            Command::StorageRead { request, path } => {
                self.start(*request, cmd.opcode())?;
                let read = Self::read(&self.io.storage_dir, &self.io.storage, path, "storage");
                if let Ok(None) = read {
                    self.missing.insert(*request);
                }
                (
                    *request,
                    read.and_then(|r| r.ok_or_else(|| format!("nothing is stored at `{path}`"))),
                )
            }
            Command::StorageWrite { request, path, data } => {
                self.start(*request, cmd.opcode())?;
                let a = match &self.io.storage_dir {
                    Some(dir) => crate::confined::write(dir, path, data).map(|()| Vec::new()),
                    None => Self::file(&self.io.storage, path, "storage").and_then(|f| {
                        if let Some(parent) = f.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                        }
                        std::fs::write(f, data).map(|()| Vec::new()).map_err(|e| e.to_string())
                    }),
                };
                (*request, a)
            }
            Command::Fetch { request, url } => {
                self.start(*request, cmd.opcode())?;
                let what = "build to fetch from";
                let read = Self::read(&self.io.build_dir, &self.io.build, url, what);
                (*request, read.and_then(|r| r.ok_or_else(|| format!("`{url}`: nothing is there"))))
            }
            Command::HostRequest { request, service, body } => {
                self.start(*request, cmd.opcode())?;
                let a = match &self.io.host {
                    Some(h) => (h.0)(service, body),
                    None => Err("this host has no capability handler".to_string()),
                };
                (*request, a)
            }
            Command::Post { request, url, body } if self.io.queue_posts => {
                self.start(*request, cmd.opcode())?;
                let held = self.posts.len() + self.taken.len();
                let refused = if held >= MOST_QUEUED_POSTS {
                    Some(format!("{MOST_QUEUED_POSTS} posts already wait for an answer"))
                } else if body.len() > MOST_QUEUED_POST {
                    Some(format!("a post's body is at most {MOST_QUEUED_POST} bytes here"))
                } else {
                    None
                };
                if let Some(why) = refused {
                    (*request, Err(why))
                } else {
                    self.waiting.push(*request);
                    let post =
                        QueuedPost { request: *request, url: url.to_string(), body: body.to_vec() };
                    self.posts.push(post);
                    return Ok(());
                }
            }
            Command::Post { request, url, body } => {
                self.start(*request, cmd.opcode())?;
                let a = match &self.io.post {
                    Some(h) => (h.0)(url, body),
                    None => Err("this host has nothing that answers posts".to_string()),
                };
                (*request, a)
            }
            _ => return Ok(()),
        };
        self.arriving.push((request, answer));
        Ok(())
    }

    /// The end of a call: what arrived during it is answered now.
    fn settle(&mut self) {
        for (r, a) in self.arriving.drain(..) {
            self.answered.insert(r, a);
        }
    }
}

impl<E: Executor> State<E> {
    fn begin_call(&mut self) {
        self.checker.begin_call();
        self.before_call = Some(self.sequencer.clone());
    }

    fn discard_call(&mut self) {
        self.pending.clear();
        self.requests.reserved.clear();
        self.checker.end_call(false);
        if let Some(before) = self.before_call.take() {
            self.sequencer = before;
        }
    }

    /// A successful export must close every pass before any of its commands take effect.
    fn commit_call(&mut self) -> Result<()> {
        if let Err(error) = self.sequencer.end_frame() {
            self.discard_call();
            return Err(error.into());
        }
        self.checker.end_call(true);
        self.before_call = None;
        self.requests.reserved.clear();
        for batch in std::mem::take(&mut self.pending) {
            for command in stream::decode(&batch)? {
                self.execute(&command)?;
            }
        }
        Ok(())
    }

    fn submit(&mut self, batch: &[u8]) -> Result<()> {
        // The state hash covers every submitted byte, valid or not.
        self.hash.update(batch);
        if let Some(b) = &mut self.batches {
            b.push(batch.to_vec());
        }
        for cmd in stream::decode(batch)? {
            if let Some(first) = &self.failed {
                return Err(Error::Command(CommandError {
                    opcode: cmd.opcode(),
                    why: format!("the host failed to carry out an earlier command ({first})"),
                }));
            }
            let before = self.sequencer.clone();
            self.sequencer.step(&cmd)?;
            if let Err(e) = self.checker.check(&cmd) {
                self.sequencer = before;
                return Err(e.into());
            }
            match &cmd {
                Command::ReadBuffer { request, .. }
                | Command::StorageRead { request, .. }
                | Command::StorageWrite { request, .. }
                | Command::Fetch { request, .. }
                | Command::Post { request, .. }
                | Command::HostRequest { request, .. }
                | Command::Prepare { request, .. }
                | Command::HandOver { request, .. } => {
                    self.requests.start(*request, cmd.opcode())?;
                    self.requests.reserved.insert(*request);
                }
                _ => {}
            }
        }
        self.pending.push(batch.to_vec());
        Ok(())
    }

    /// Carries out a command already checked during the successful export.
    fn execute(&mut self, cmd: &Command<'_>) -> Result<()> {
        if let Err(e) = self.executor.execute(cmd) {
            self.failed = Some(e.to_string());
            return Err(e);
        }
        match cmd {
            Command::ReadBuffer { request, handle, offset, size } => {
                self.requests.start(*request, cmd.opcode())?;
                let answer =
                    self.executor.read_back(*handle, *offset, *size).map_err(|e| e.to_string());
                self.requests.arriving.push((*request, answer));
            }
            Command::StorageRead { .. }
            | Command::StorageWrite { .. }
            | Command::Fetch { .. }
            | Command::Post { .. }
            | Command::HostRequest { .. } => self.requests.io(cmd)?,
            Command::Prepare { request, path } | Command::HandOver { request, path } => {
                self.requests.start(*request, cmd.opcode())?;
                let path = path.to_string();
                if !self.hands_over {
                    let why = "this host has no other build to hand over to".to_string();
                    self.requests.arriving.push((*request, Err(why)));
                } else if let Command::Prepare { .. } = cmd {
                    self.requests.waiting.push(*request);
                    self.handing.push(Handing::Prepare { request: *request, path });
                } else {
                    self.requests.waiting.push(*request);
                    self.handing.push(Handing::HandOver { request: *request, path });
                }
            }
            Command::Unprepare { path } if self.hands_over => {
                self.handing.push(Handing::Unprepare { path: path.to_string() });
            }
            _ => {}
        }
        Ok(())
    }
}

/// The audio thread's instance of the program, and its `__audio(thread, task, context)`.
type AudioThread = (Store<bool>, TypedFunc<(u32, u32, u32), ()>);

/// The ticker's thread's instance of the program, its `__tick(thread, task, context, tick)`,
/// and the next tick's number.
struct TickThread {
    store: Store<bool>,
    tick: TypedFunc<(u32, u32, u32, u32), ()>,
    next: u32,
    memory: SharedMemory,
    task: u32,
    context: u32,
    fuel: Option<u64>,
    trace: Trace,
}

impl TickThread {
    /// Runs the next tick with `records`, on whichever OS thread holds this (a tick can run
    /// beside the program's frames): its state's hash too, if the host asked for it
    /// ([`Program::want_hashes`]).
    fn run(
        &mut self,
        records: Vec<[u8; EVENT_SIZE as usize]>,
    ) -> std::result::Result<Ticked, (u32, wasmtime::Error)> {
        let k = self.next;
        let at = memory::TICK_RECORDS as usize;
        shared::write(&self.memory, at, &(records.len() as u32).to_le_bytes());
        shared::write(&self.memory, at + 4, records.as_flattened());
        if let Some(fuel) = self.fuel {
            self.store.set_fuel(fuel).map_err(|e| (k, e))?;
        }
        // The ticker's counts (its block's words), before and after: what the tick did.
        let block = memory::thread_block(memory::THREAD_TICK);
        let words =
            [memory::JOIN_WAITS, memory::ALLOCATIONS, memory::LOCK_WAITS, memory::LOCK_SPINS];
        let counts = || words.map(|w| shared::load_u32(&self.memory, block + w));
        let before = counts();
        let result =
            self.tick.call(&mut self.store, (memory::THREAD_TICK, self.task, self.context, k));
        // The tick's phase ends with it.
        self.trace.end_phase();
        self.next += 1;
        result.map_err(|e| (k, e))?;
        let after = counts();
        let [waited, allocations, lock_waits, lock_spins] =
            [0, 1, 2, 3].map(|i| after[i].wrapping_sub(before[i]));
        let hashed = shared::load_u32(&self.memory, memory::TICK_WANT_HASH) != 0;
        Ok(Ticked {
            tick: k,
            records,
            hash: hashed.then(|| shared::load_u64(&self.memory, memory::TICK_HASH)),
            waited,
            allocations,
            lock_waits,
            lock_spins,
        })
    }
}

/// What a tick was given and gave: its number, its records, and the state's hash after its
/// step when the host asked for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticked {
    pub tick: u32,
    pub records: Vec<[u8; EVENT_SIZE as usize]>,
    pub hash: Option<u64>,
    /// How many times the tick waited at a job's join for a helper still running it: the sim
    /// waiting for an answer due (wrela_abi `JOIN_WAITS`). The host logs it.
    pub waited: u32,
    /// How many blocks the tick allocated (`ALLOCATIONS`).
    pub allocations: u32,
    /// How many times it found the allocator's lock taken, and the tries it spun on it
    /// (`LOCK_WAITS`, `LOCK_SPINS`).
    pub lock_waits: u32,
    pub lock_spins: u32,
}

pub(crate) struct Program<E: 'static> {
    store: Store<State<E>>,
    /// The worker threads, which return when the program is dropped (or retired).
    _helpers: Helpers,
    memory: SharedMemory,
    module: Module,
    /// The audio thread's instance, once [`Program::render_audio`] has made it.
    audio: Option<AudioThread>,
    /// The ticker's thread's instance, once [`Program::tick`] has made it.
    ticker: Option<TickThread>,
    /// The ticker's records not yet stamped into a tick, as bytes: a tick takes at most
    /// `wrela_abi::memory::MAX_TICK_RECORDS`, and the rest wait for the next.
    records: std::collections::VecDeque<[u8; EVENT_SIZE as usize]>,
    instance: Instance,
    frame: TypedFunc<(f32, u32, u32), ()>,
    /// Where the code came from, for trap messages (`wrela_abi::lines`).
    lines: Option<Arc<Lines>>,
    /// The fuel each call gets, on a metered engine ([`METERED`]).
    fuel: Option<u64>,
    /// Why the last call failed, if it did.
    last_failure: Option<Failure>,
    /// Whether `init` completed (or the module has no `init`).
    init_done: bool,
    /// The records of each tick run that had any, by tick: what a build that replaces this one
    /// replays (hot reload). Kept only for a program that may be replaced
    /// ([`Program::keep_ticks`]).
    ran: Option<Vec<(u32, Vec<[u8; EVENT_SIZE as usize]>)>>,
    /// Hot reload: the ticks the replaced build ran, which this one runs again first, each with
    /// the records it had.
    replay: Option<Replay>,
    /// The first frame of its ticks in lockstep: the frame a program handed over to started
    /// at, whose ticks count from there (a hot reload's go on from the replaced build's).
    tick_from: Option<u32>,
}

/// The ticks a replaced build ran, for the build that replaces it to run again (hot reload).
struct Replay {
    /// How many: ticks 0 to `upto - 1`.
    upto: u32,
    /// The records of those that had any, by tick.
    records: std::collections::HashMap<u32, Vec<[u8; EVENT_SIZE as usize]>>,
}

/// What a program hands the build that replaces it while it runs (hot reload): its executor
/// (the device, which keeps running), what it kept (`std::reload`), the ticks it ran with their
/// records, and its input not yet read.
pub(crate) struct Retired<E> {
    pub executor: E,
    kept: Vec<u8>,
    ticks: u32,
    /// None for a program that kept none (it wasn't to be replaced).
    ran: Option<Vec<(u32, Vec<[u8; EVENT_SIZE as usize]>)>>,
    input: Queue,
    records: std::collections::VecDeque<[u8; EVENT_SIZE as usize]>,
    trace: Option<Trace>,
    batches: Option<Vec<Vec<u8>>>,
    tick_from: Option<u32>,
}

/// The carried data kept aside while a restart starts a fresh program on the old device.
#[cfg(feature = "gpu")]
pub(crate) struct RestartState {
    kept: Vec<u8>,
    ticks: u32,
    ran: Option<Vec<(u32, Vec<[u8; EVENT_SIZE as usize]>)>>,
    input: Queue,
    records: std::collections::VecDeque<[u8; EVENT_SIZE as usize]>,
    trace: Option<Trace>,
    batches: Option<Vec<Vec<u8>>>,
    tick_from: Option<u32>,
}

#[cfg(feature = "gpu")]
impl RestartState {
    /// Reattaches the old program's data to the executor returned by a failed restart.
    pub(crate) fn restore<E>(self, failed: Retired<E>) -> Retired<E> {
        Retired {
            executor: failed.executor,
            kept: self.kept,
            ticks: self.ticks,
            ran: self.ran,
            input: self.input,
            records: self.records,
            trace: self.trace,
            batches: self.batches,
            tick_from: self.tick_from,
        }
    }
}

/// What a failed replacement's `init` must not consume from the retiring program.
#[cfg(feature = "gpu")]
pub(crate) struct CarryCheckpoint {
    kept: Vec<u8>,
    ticks: u32,
    ran: Option<Vec<(u32, Vec<[u8; EVENT_SIZE as usize]>)>>,
    input: Queue,
    records: std::collections::VecDeque<[u8; EVENT_SIZE as usize]>,
    batches_len: Option<usize>,
    tick_from: Option<u32>,
}

#[cfg(feature = "gpu")]
impl CarryCheckpoint {
    pub(crate) fn restore<E>(self, mut failed: Retired<E>) -> Retired<E> {
        failed.kept = self.kept;
        failed.ticks = self.ticks;
        failed.ran = self.ran;
        failed.input = self.input;
        failed.records = self.records;
        if let Some(len) = self.batches_len
            && let Some(batches) = &mut failed.batches
        {
            batches.truncate(len);
        }
        failed.tick_from = self.tick_from;
        failed
    }
}

impl<E> Retired<E> {
    /// What a program that replaces none starts with: `executor` alone, and with `record`, a
    /// place for its batches.
    fn new(executor: E, record: bool) -> Retired<E> {
        Retired {
            executor,
            kept: Vec::new(),
            ticks: 0,
            ran: None,
            input: Queue::default(),
            records: Default::default(),
            trace: None,
            batches: record.then(Vec::new),
            tick_from: None,
        }
    }

    /// Saves the bytes, unread input and recorded batches a replacement's `init` can change.
    #[cfg(feature = "gpu")]
    pub(crate) fn checkpoint(&self) -> CarryCheckpoint {
        CarryCheckpoint {
            kept: self.kept.clone(),
            ticks: self.ticks,
            ran: self.ran.clone(),
            input: self.input.clone(),
            records: self.records.clone(),
            batches_len: self.batches.as_ref().map(Vec::len),
            tick_from: self.tick_from,
        }
    }

    /// Starts a fresh program on this executor while keeping the old program's data for
    /// rollback if the restart fails.
    #[cfg(feature = "gpu")]
    pub(crate) fn fresh_for_restart(self, record: bool) -> (Retired<E>, RestartState) {
        let Retired { executor, kept, ticks, ran, input, records, trace, batches, tick_from } =
            self;
        let saved = RestartState { kept, ticks, ran, input, records, trace, batches, tick_from };
        (Retired::new(executor, record), saved)
    }

    /// What a build handed over to gets (`std::reload::hand_over`): the executor, what was
    /// kept, the input not yet read, the trace and batches; no ticks to replay, since the new
    /// build's sim is its own.
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub(crate) fn handed_over(self) -> Retired<E> {
        Retired { ticks: 0, ran: None, records: Default::default(), tick_from: None, ..self }
    }

    /// Adds a line to what the program printed, as the host's own: why a hand-over failed.
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub(crate) fn log(&mut self, line: String) {
        match &self.trace {
            Some(t) => t.print(line),
            None => eprintln!("wrela: {line}"),
        }
    }

    /// The same, with `kept` in place of what was kept: a build started again after a
    /// hand-over failed starts with what it had kept before it.
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub(crate) fn keeping(self, kept: Vec<u8>) -> Retired<E> {
        Retired { kept, ..self.handed_over() }
    }

    /// Restores the bytes a program kept before a replacement's `init` failed, without
    /// discarding ticks that still need to be replayed.
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub(crate) fn with_kept(self, kept: Vec<u8>) -> Retired<E> {
        Retired { kept, ..self }
    }
}

/// A program's helper threads (a client build's: language.md §16.1): dropping it shuts them down
/// (they see the flag when they wake) and waits for them.
pub(crate) struct Helpers {
    memory: SharedMemory,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Helpers {
    /// `count` helpers (at most [`MAX_WORKERS`]) for `module`'s instances on `memory`, each on
    /// a thread of its own from [`memory::THREAD_HELPER0`], printing and timing into `trace`,
    /// their calls ending when `stop` is due; none if the module exports no `__worker` (it runs
    /// no parallel jobs). The memory's [`memory::PAR_HELPERS`] says how many. Why they can't
    /// start, the helpers already started shut down.
    pub(crate) fn start(
        module: &Module,
        memory: &SharedMemory,
        count: u32,
        trace: &Trace,
        stop: Option<&Stop>,
    ) -> std::result::Result<Helpers, String> {
        let mut helpers = Helpers { memory: memory.clone(), threads: Vec::new() };
        if module.get_export(EXPORT_WORKER).is_none() {
            return Ok(helpers);
        }
        let controls = memory::PAR_HELPERS as usize + 4;
        if memory.data().len() < controls {
            return Err(format!(
                "it exports `{EXPORT_WORKER}` but its memory is {} bytes; the helpers need at least {controls}",
                memory.data().len()
            ));
        }
        let n = count.min(MAX_WORKERS);
        for i in 0..n {
            let thread = memory::THREAD_HELPER0 + i;
            let (module, memory, trace) = (module.clone(), memory.clone(), trace.clone());
            let spawned = spawn_worker(module, memory, thread, trace, stop.cloned())
                .map_err(|e| format!("can't start helper {i}: {e}"))?;
            helpers.threads.push(spawned);
        }
        shared::store_u32(memory, memory::PAR_HELPERS, n);
        Ok(helpers)
    }
}

impl Drop for Helpers {
    /// Tells the helpers to return (each sees it once it waits for work) and waits for them.
    fn drop(&mut self) {
        if self.threads.is_empty() {
            return;
        }
        shut_down_helpers(&self.memory);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Tells the helpers on `memory` to return: each sees it once it waits for work
/// ([`memory::PAR_SHUTDOWN`]).
pub(crate) fn shut_down_helpers(memory: &SharedMemory) {
    shared::store_u32(memory, memory::PAR_SHUTDOWN, 1);
    shared::add_u32(memory, memory::PAR_WAKE, 1);
    let _ = memory.atomic_notify(u64::from(memory::PAR_WAKE), u32::MAX);
}

/// Checks a module against the program ABI before instantiating it: it imports its memory,
/// shared, as `wrela.memory`, may import only the host's functions besides, and must export
/// `memory` and `frame(f32, i32, i32)`. The memory's type.
pub(crate) fn check_abi(module: &Module) -> Result<wasmtime::MemoryType> {
    let mut memory = None;
    for import in module.imports() {
        let (m, n) = (import.module(), import.name());
        if (m, n) == (IMPORT_MODULE, IMPORT_MEMORY) {
            match import.ty() {
                ExternType::Memory(t) if t.is_shared() => memory = Some(t),
                ty => {
                    return Err(Error::Program(format!(
                        "its import `{m}.{n}` must be a shared memory, not {}",
                        describe(&ty)
                    )));
                }
            }
            continue;
        }
        let want = HOST_FUNCTIONS.iter().find(|f| m == IMPORT_MODULE && f.name == n);
        let Some(want) = want else {
            let all: Vec<String> = std::iter::once(IMPORT_MEMORY)
                .chain(HOST_FUNCTIONS.iter().map(|f| f.name))
                .map(|name| format!("`{IMPORT_MODULE}.{name}`"))
                .collect();
            return Err(Error::Program(format!(
                "it imports `{m}.{n}`, which isn't one of the host's: {}",
                all.join(", ")
            )));
        };
        // Its type as both hosts word it.
        let shown = want.signature();
        if !matches!(import.ty(), ExternType::Func(ref f) if signature(f) == shown) {
            return Err(Error::Program(format!(
                "its import `{m}.{n}` must be a function {shown}, not {}",
                describe(&import.ty())
            )));
        }
    }
    let Some(memory) = memory else {
        return Err(Error::Program(format!(
            "it doesn't import its memory, shared, as `{IMPORT_MODULE}.{IMPORT_MEMORY}`"
        )));
    };
    if !matches!(module.get_export(EXPORT_MEMORY), Some(ExternType::Memory(_))) {
        return Err(Error::Program(format!("it doesn't export its memory as `{EXPORT_MEMORY}`")));
    }
    if !module.get_export(EXPORT_FRAME).is_some_and(|t| is_func(&t, &["f32", "i32", "i32"])) {
        return Err(Error::Program(format!(
            "it must export `{EXPORT_FRAME}(time: f32, width: i32, height: i32)` with no results"
        )));
    }
    if module.get_export(EXPORT_INIT).is_some_and(|t| !is_func(&t, &[])) {
        return Err(Error::Program(format!(
            "its `{EXPORT_INIT}` export must be a function with no parameters and no results"
        )));
    }
    for (name, params) in [
        (EXPORT_WORKER, &["i32"][..]),
        (EXPORT_AUDIO, &["i32", "i32", "i32"][..]),
        (EXPORT_TICK, &["i32", "i32", "i32", "i32"][..]),
    ] {
        if module.get_export(name).is_some_and(|t| !is_func(&t, params)) {
            return Err(Error::Program(format!(
                "its `{name}` export must be a function ({}) -> ()",
                params.join(", ")
            )));
        }
    }
    Ok(memory)
}

/// Whether `ty` is a function with these parameter types (by [`type_name`]) and no results.
fn is_func(ty: &ExternType, params: &[&str]) -> bool {
    let ExternType::Func(f) = ty else { return false };
    f.params().map(type_name).eq(params.iter().copied()) && f.results().len() == 0
}

/// A function's type, as both hosts word it: `(i32, i32) -> ()`.
fn signature(f: &FuncType) -> String {
    let list = |types: &mut dyn Iterator<Item = ValType>| {
        types.map(|t| t.to_string()).collect::<Vec<_>>().join(", ")
    };
    format!("({}) -> ({})", list(&mut f.params()), list(&mut f.results()))
}

/// What an import is, as both hosts word it: `a function (i32) -> ()`, `a memory`, ...
fn describe(ty: &ExternType) -> String {
    match ty {
        ExternType::Func(f) => format!("a function {}", signature(f)),
        ExternType::Global(_) => "a global".into(),
        ExternType::Table(_) => "a table".into(),
        ExternType::Memory(_) => "a memory".into(),
        ExternType::Tag(_) => "a tag".into(),
    }
}

/// A module that satisfies the program ABI, ready to instantiate (any number of times).
pub(crate) struct Compiled {
    module: Module,
    /// The fuel each call gets, when compiled for [`METERED`].
    fuel: Option<u64>,
    /// Its memory's type: shared.
    memory: wasmtime::MemoryType,
    lines: Option<Arc<Lines>>,
    /// Its pair id (`wrela_abi::PAIR_SECTION`), if it has one.
    pair: Option<u64>,
}

/// The engine a program compiles for: [`METERED`] with `fuel`, else [`ENGINE`].
fn engine_for(fuel: Option<u64>) -> &'static Engine {
    if fuel.is_some() { &METERED } else { &ENGINE }
}

/// Compiles a program and checks it against the ABI ([`check_abi`]); with `fuel`, for the
/// metered engine, where each call gets that much fuel.
#[cfg(test)]
pub(crate) fn compile_with(wasm: &[u8], fuel: Option<u64>) -> Result<Compiled> {
    let module =
        Module::new(engine_for(fuel), wasm).map_err(|e| Error::Program(format!("{e:#}")))?;
    compiled(module, wasm, fuel, None)
}

/// [`compile_with`], the compiled code kept beside the build in `dir`, if given
/// (`crate::cache`); `hash` is the WASM's ([`wrela_abi::ticks::wasm_hash`]). `sidecar` is an
/// optimized build's trap locations, read from beside its WASM ([`lines::SIDECAR`]).
pub(crate) fn compile_cached(
    wasm: &[u8],
    hash: u64,
    dir: Option<&std::path::Path>,
    fuel: Option<u64>,
    sidecar: Option<&[u8]>,
) -> Result<Compiled> {
    compiled(crate::cache::module(engine_for(fuel), wasm, hash, dir)?, wasm, fuel, sidecar)
}

/// A program the tool host drives, whose code `module` is, compiled for [`driven_engine`] from
/// `wasm` (the tool host's, which compiled it once for every load of the build). `sidecar` is as
/// [`compile_cached`]'s.
pub(crate) fn compile_driven(
    module: Module,
    wasm: &[u8],
    sidecar: Option<&[u8]>,
) -> Result<Compiled> {
    if !Engine::same(module.engine(), &DRIVEN) {
        return Err(Error::Program("a driven build's code is for another engine".into()));
    }
    compiled(module, wasm, None, sidecar)
}

fn compiled(
    module: Module,
    wasm: &[u8],
    fuel: Option<u64>,
    sidecar: Option<&[u8]>,
) -> Result<Compiled> {
    let memory = check_abi(&module)?;
    // A build's own section, else an optimized build's file beside it.
    let (payload, from) = match lines::custom_section(wasm, lines::SECTION) {
        Some(payload) => (Some(payload), format!("`{}` section", lines::SECTION)),
        None => (sidecar, format!("`.{}` file", lines::SIDECAR)),
    };
    let lines = match payload {
        Some(payload) => Some(Arc::new(
            Lines::decode(payload)
                .map_err(|e| Error::Program(format!("its {from} is malformed: {e}")))?,
        )),
        None => None,
    };
    Ok(Compiled { module, fuel, memory, lines, pair: wrela_abi::read_pair(wasm) })
}

impl<E: Executor> Program<E> {
    /// Instantiates a compiled program whose commands are checked against `manifest` and
    /// carried out by `executor`.
    /// `workers` is how many threads run a parallel job's chunks, this one included: 1 runs
    /// them all here.
    #[cfg(test)]
    pub(crate) fn instantiate(
        compiled: &Compiled,
        manifest: &Manifest,
        executor: E,
        io: Io,
        record: bool,
        workers: u32,
    ) -> Result<Program<E>> {
        Program::instantiate_with(compiled, manifest, executor, io, record, workers, true)
    }

    /// [`Program::instantiate`], running `init` only with `run_init`: otherwise the caller
    /// runs it ([`Program::run_init`]), and learns why it failed if it does.
    pub(crate) fn instantiate_with(
        compiled: &Compiled,
        manifest: &Manifest,
        executor: E,
        io: Io,
        record: bool,
        workers: u32,
        run_init: bool,
    ) -> Result<Program<E>> {
        let carried = Retired::new(executor, record);
        Program::instantiate_carrying(compiled, manifest, carried, io, workers, run_init)
    }

    /// Hot reload: a new build's program in place of the one `carried` comes from, on its
    /// executor, with what it kept, its input not yet read, and its ticks to run again (each
    /// with the records it had) before any other.
    pub(crate) fn instantiate_carrying(
        compiled: &Compiled,
        manifest: &Manifest,
        carried: Retired<E>,
        io: Io,
        workers: u32,
        run_init: bool,
    ) -> Result<Program<E>> {
        Self::instantiate_carrying_recover(compiled, manifest, carried, io, workers, run_init)
            .map_err(|(error, _)| error)
    }

    /// Like [`Program::instantiate_carrying`], but gives the carried state back when the
    /// replacement cannot start, so its host can start the previous build again.
    pub(crate) fn instantiate_carrying_recover(
        compiled: &Compiled,
        manifest: &Manifest,
        carried: Retired<E>,
        io: Io,
        workers: u32,
        run_init: bool,
    ) -> std::result::Result<Program<E>, (Error, Box<Retired<E>>)> {
        let prepared_trace = carried
            .trace
            .clone()
            .unwrap_or_else(|| Trace::new((!io.quiet).then(|| "wrela: ".to_string())));
        let engine = compiled.module.engine();
        let mut linker = Linker::new(engine);
        let wrap = |e: wasmtime::Error| Error::Program(format!("{e:#}"));
        let memory = match SharedMemory::new(engine, compiled.memory.clone()) {
            Ok(memory) => memory,
            Err(e) => return Err((wrap(e), Box::new(carried))),
        };
        macro_rules! link {
            ($call:expr) => {
                if let Err(e) = $call {
                    return Err((wrap(e), Box::new(carried)));
                }
            };
        }
        link!(linker.func_wrap(IMPORT_MODULE, IMPORT_SUBMIT, submit::<E>));
        link!(linker.func_wrap(IMPORT_MODULE, IMPORT_REQUEST_STATUS, request_status::<E>));
        link!(linker.func_wrap(IMPORT_MODULE, IMPORT_REQUEST_TAKE, request_take::<E>));
        link!(linker.func_wrap(IMPORT_MODULE, IMPORT_LIMIT, limit::<E>));
        link!(linker.func_wrap(IMPORT_MODULE, IMPORT_AUDIO, audio::<E>));
        link!(linker.func_wrap(IMPORT_MODULE, IMPORT_INPUT, self::input::<E>));
        link!(linker.func_wrap(IMPORT_MODULE, IMPORT_PAYLOAD, payload::<E>));
        link!(linker.func_wrap(IMPORT_MODULE, IMPORT_TICK, tick::<E>));
        link!(linker.func_wrap(IMPORT_MODULE, IMPORT_KEEP, keep::<E>));
        link!(linker.func_wrap(IMPORT_MODULE, IMPORT_KEPT, self::kept::<E>));
        link!(linker.func_wrap(IMPORT_MODULE, IMPORT_CLOCK, clock::<E>));
        link!(prepared_trace.link_checked(&mut linker, Some(&memory), check_started::<E>));

        let Retired { executor, kept, ticks, ran, mut input, records, trace, batches, tick_from } =
            carried;
        // A reload to another program's build drops the network's events (language.md §17.2).
        input.for_build(compiled.pair);
        let trace = trace.unwrap_or(prepared_trace);
        let state = State {
            memory: memory.clone(),
            checker: Checker::with_limits(manifest, executor.limits()),
            executor,
            hash: StateHash::new(),
            sequencer: Sequencer::new(),
            pending: Vec::new(),
            before_call: None,
            failure: None,
            // From the start, so `init`'s batches are kept too.
            batches,
            failed: None,
            requests: Requests { io, ..Requests::default() },
            started: false,
            voice: None,
            has_audio_entry: compiled
                .module
                .get_export(EXPORT_AUDIO)
                .is_some_and(|t| is_func(&t, &["i32", "i32", "i32"])),
            ticker: None,
            has_tick_entry: compiled
                .module
                .get_export(EXPORT_TICK)
                .is_some_and(|t| is_func(&t, &["i32", "i32", "i32", "i32"])),
            trace: trace.clone(),
            input,
            kept,
            keeping: Vec::new(),
            handing: Vec::new(),
            hands_over: false,
        };
        let mut store = Store::new(engine, state);
        never_due(&mut store);
        macro_rules! fail_after_store {
            ($error:expr) => {{
                let error = $error;
                let state = store.into_data();
                let carried = Retired {
                    executor: state.executor,
                    kept: state.kept,
                    ticks,
                    ran,
                    input: state.input,
                    records,
                    trace: Some(state.trace),
                    batches: state.batches,
                    tick_from,
                };
                return Err((error, Box::new(carried)));
            }};
        }
        if let Some(fuel) = compiled.fuel
            && let Err(e) = store.set_fuel(fuel)
        {
            fail_after_store!(wrap(e));
        }
        if let Err(e) = linker.define(&store, IMPORT_MODULE, IMPORT_MEMORY, memory.clone()) {
            fail_after_store!(wrap(e));
        }
        let instance = match linker.instantiate(&mut store, &compiled.module) {
            Ok(instance) => instance,
            Err(e) => {
                let error = store
                    .data_mut()
                    .failure
                    .take()
                    .unwrap_or_else(|| Error::Program(format!("{e:#}")));
                fail_after_store!(error);
            }
        };
        let exports_imported_memory = instance
            .get_shared_memory(&mut store, EXPORT_MEMORY)
            .is_some_and(|exported| std::ptr::eq(exported.data().as_ptr(), memory.data().as_ptr()));
        if !exports_imported_memory {
            fail_after_store!(Error::Program(format!(
                "its `{EXPORT_MEMORY}` export isn't its imported shared memory"
            )));
        }
        store.data_mut().started = true;
        let frame = match instance.get_typed_func::<(f32, u32, u32), ()>(&mut store, EXPORT_FRAME) {
            Ok(frame) => frame,
            Err(e) => fail_after_store!(Error::Program(format!("{e:#}"))),
        };
        // The helpers: each its own instance on its own thread, sharing the memory.
        let n = workers.clamp(1, MAX_WORKERS + 1) - 1;
        let helpers = match Helpers::start(&compiled.module, &memory, n, &trace, None) {
            Ok(h) => h,
            Err(why) => fail_after_store!(Error::Program(why)),
        };
        let program = Program {
            store,
            instance,
            frame,
            lines: compiled.lines.clone(),
            _helpers: helpers,
            memory,
            module: compiled.module.clone(),
            audio: None,
            ticker: None,
            records,
            fuel: compiled.fuel,
            last_failure: None,
            init_done: false,
            // A program that replaces one may be replaced in turn.
            ran: ran.as_ref().map(|_| Vec::new()),
            replay: (ticks > 0)
                .then(|| Replay { upto: ticks, records: ran.into_iter().flatten().collect() }),
            tick_from,
        };
        if run_init { program.init_recover() } else { Ok(program) }
    }

    /// Runs init after its host has registered its capabilities. A failed replacement gives
    /// its carried state back, with the original kept bytes, as instantiation does.
    pub(crate) fn init_recover(
        mut self,
    ) -> std::result::Result<Program<E>, (Error, Box<Retired<E>>)> {
        let kept = self.store.data().kept.clone();
        match self.run_init() {
            Ok(()) => Ok(self),
            Err(error) => Err((error, Box::new(self.retire().with_kept(kept)))),
        }
    }

    /// Keeps each tick's records from now on (before any tick runs): what a build that
    /// replaces this one replays (hot reload, [`Program::retire`]).
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub(crate) fn keep_ticks(&mut self) {
        self.ran.get_or_insert_with(Vec::new);
    }

    /// Ends this program for a build that replaces it while it runs (hot reload): its helpers
    /// stop, and what carries over to the new one is handed back
    /// ([`Program::instantiate_carrying`]). Ticks it was still to replay are replayed by the
    /// next build too.
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub(crate) fn retire(self) -> Retired<E> {
        let Program { store, _helpers: helpers, ticker, ran, replay, records, tick_from, .. } =
            self;
        drop(helpers);
        let state = store.into_data();
        // The ticks it hadn't replayed yet, what the build before it ran, carry over too.
        let pending: Option<(u32, Vec<_>)> =
            replay.map(|r| (r.upto, r.records.into_iter().collect()));
        let replay = pending.as_ref().map(|(upto, records)| (*upto, records.as_slice()));
        let next = ticker.map_or(0, |t| t.next);
        let (ticks, ran) = match ran {
            Some(ran) => {
                let (ticks, ran) = wrela_abi::ticks::carried(&ran, replay, next);
                (ticks, Some(ran))
            }
            // One that kept none wasn't to be replaced: only how many ticks there were.
            None => (next.max(replay.map_or(0, |r| r.0)), None),
        };
        Retired {
            executor: state.executor,
            kept: state.keeping,
            ticks,
            ran,
            input: state.input,
            records,
            trace: Some(state.trace),
            batches: state.batches,
            tick_from,
        }
    }

    /// Its host carries out what it asks of other builds (`std::reload`): it takes them with
    /// [`Program::take_handing`] after each call.
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub(crate) fn hand_over_here(&mut self) {
        self.store.data_mut().hands_over = true;
    }

    /// What the program asked of other builds since the last time (`std::reload`): the host
    /// carries it out, and answers each request with [`Program::answer`].
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub(crate) fn take_handing(&mut self) -> Vec<Handing> {
        std::mem::take(&mut self.store.data_mut().handing)
    }

    /// Answers request `request`, one of those [`Program::take_handing`] gave: the program sees
    /// it from its next call on.
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub(crate) fn answer(&mut self, request: u32, answer: std::result::Result<Vec<u8>, String>) {
        let requests = &mut self.store.data_mut().requests;
        requests.waiting.retain(|&r| r != request);
        requests.answered.insert(request, answer);
    }

    /// The posts that wait for the host's user since the last time ([`Io::queue_posts`]): each
    /// waits until [`Program::answer_post`] answers it.
    pub(crate) fn take_posts(&mut self) -> Vec<QueuedPost> {
        let requests = &mut self.store.data_mut().requests;
        let posts = std::mem::take(&mut requests.posts);
        requests.taken.extend(posts.iter().map(|p| p.request));
        posts
    }

    /// Answers post `request`, one [`Program::take_posts`] gave: the program sees the answer
    /// from its next call on. An error if no such post waits.
    pub(crate) fn answer_post(
        &mut self,
        request: u32,
        answer: std::result::Result<Vec<u8>, String>,
    ) -> std::result::Result<(), String> {
        let requests = &mut self.store.data_mut().requests;
        if !requests.taken.remove(&request) {
            return Err(format!("no post {request} waits for an answer"));
        }
        requests.waiting.retain(|&r| r != request);
        requests.answered.insert(request, answer);
        Ok(())
    }

    /// What it keeps for the build that replaces it (`std::reload::keep`), so far.
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub(crate) fn keeping(&self) -> &[u8] {
        &self.store.data().keeping
    }

    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub(crate) fn init_done(&self) -> bool {
        self.init_done
    }

    /// Runs `init`, for a program [`Program::instantiate_with`] made without running it: a
    /// program with state makes it once, before anything else runs (language.md §15).
    pub(crate) fn run_init(&mut self) -> Result<()> {
        let Ok(init) = self.instance.get_typed_func::<(), ()>(&mut self.store, EXPORT_INIT) else {
            self.init_done = true;
            return Ok(());
        };
        self.refuel()?;
        let result = init.call(&mut self.store, ());
        self.finish_call(result)?;
        self.init_done = true;
        Ok(())
    }

    /// A metered program's fuel, filled for the next call.
    fn refuel(&mut self) -> Result<()> {
        match self.fuel {
            Some(fuel) => self.store.set_fuel(fuel).map_err(|e| Error::Program(format!("{e:#}"))),
            None => Ok(()),
        }?;
        self.store.data_mut().begin_call();
        Ok(())
    }

    /// Why the last call failed, if it did: for reporting it against the source.
    pub(crate) fn last_failure(&self) -> Option<&Failure> {
        self.last_failure.as_ref()
    }

    /// Calls `frame(time, width, height)`, then ends the frame.
    pub(crate) fn frame(&mut self, time: f32, width: u32, height: u32) -> Result<()> {
        self.store.data_mut().executor.begin_frame();
        self.store.data_mut().checker.set_screen_size(width, height);
        self.refuel()?;
        let result = self.frame.call(&mut self.store, (time, width, height));
        self.finish_call(result)
    }

    /// Calls another export with numeric arguments.
    pub(crate) fn call(&mut self, name: &str, args: &[Value]) -> Result<Vec<Value>> {
        let func: Func = self
            .instance
            .get_func(&mut self.store, name)
            .ok_or_else(|| Error::Program(format!("it has no exported function `{name}`")))?;
        let ty = func.ty(&self.store);
        let arg_types = || args.iter().map(|a| type_name(a.ty()));
        if !ty.params().map(type_name).eq(arg_types()) {
            let given: Vec<&str> = arg_types().collect();
            return Err(Error::Program(format!(
                "`{name}` has type {ty}; it can't take ({})",
                given.join(", ")
            )));
        }
        let args: Vec<Val> = args.iter().map(|a| a.to_val()).collect();
        let mut results: Vec<Val> = ty.results().map(|_| Val::I32(0)).collect();
        self.refuel()?;
        let result = func.call(&mut self.store, &args, &mut results);
        self.finish_call(result)?;
        results
            .iter()
            .map(|v| {
                Value::from_val(v)
                    .ok_or_else(|| Error::Program(format!("`{name}` returns a non-numeric value")))
            })
            .collect()
    }

    /// The exported functions' names and WASM signatures.
    pub(crate) fn exports(&self) -> Vec<(String, Vec<&'static str>, Vec<&'static str>)> {
        self.instance
            .module(&self.store)
            .exports()
            .filter_map(|e| match e.ty() {
                ExternType::Func(f) => Some((
                    e.name().to_string(),
                    f.params().map(type_name).collect(),
                    f.results().map(type_name).collect(),
                )),
                _ => None,
            })
            .collect()
    }

    /// Turns a call's outcome into ours, preferring the typed error `submit` recorded.
    fn finish_call(&mut self, result: wasmtime::Result<()>) -> Result<()> {
        self.settle(result)?;
        let state = self.store.data_mut();
        let result = state.executor.end_frame();
        if let Err(error) = &result {
            state.failed = Some(error.to_string());
        }
        result
    }

    /// Settles requests and traces, preserving a Wasm trap before checking command completion.
    fn settle(&mut self, result: wasmtime::Result<()>) -> Result<()> {
        self.last_failure = None;
        let failure = self.store.data_mut().failure.take();
        let result = match (result, failure) {
            (Ok(()), _) => self.store.data_mut().commit_call(),
            (Err(_), Some(failure)) => {
                self.store.data_mut().discard_call();
                Err(failure)
            }
            (Err(trap), None) => {
                self.store.data_mut().discard_call();
                Err(self.trapped(&trap, memory::THREAD_MAIN))
            }
        };
        // What the call's requests got, the program sees from its next call on. The call's
        // phase ends with it.
        self.store.data_mut().requests.settle();
        // The messages it was given: their bytes go with the call.
        self.store.data_mut().input.end_call();
        self.store.data().trace.end_phase();
        result
    }

    /// A trap on thread `thread` as the host reports it, with its panic message and where it
    /// was; kept as [`Program::last_failure`].
    fn trapped(&mut self, trap: &wasmtime::Error, thread: u32) -> Error {
        let (what, at) = self.describe_trap(trap);
        let panic = self.take_panic_message(thread);
        let frames = trap
            .downcast_ref::<wasmtime::WasmBacktrace>()
            .map(|bt| {
                bt.frames().iter().filter_map(|f| f.module_offset()).map(|o| o as u32).collect()
            })
            .unwrap_or_default();
        let kind = trap.downcast_ref::<wasmtime::Trap>().copied();
        self.last_failure =
            Some(Failure { frames, trap: kind, panic: panic.clone(), at, tick: None });
        Error::Trap(match panic {
            Some(msg) => format!("panic: {msg}: {what}"),
            None => what,
        })
    }

    /// The message a panic on thread `thread` left before it trapped (its block's
    /// `wrela_abi::memory::PANIC`), cleared so a later trap that isn't a panic doesn't repeat it.
    fn take_panic_message(&mut self, thread: u32) -> Option<String> {
        use wrela_abi::memory::{PANIC_CAP, panic_at, panic_message};
        let memory = &self.store.data().memory;
        let at = panic_at(thread) as usize;
        let message = panic_message(&shared::read(memory, at, 4 + PANIC_CAP as usize)?)?;
        shared::write(memory, at, &0u32.to_le_bytes());
        Some(message)
    }

    /// What trapped, and where in the source if the program says: the location of the
    /// innermost frame that has one. A build without its trap locations (an optimized build
    /// whose `game.lines` is missing) gives the innermost frame's module offset instead.
    fn describe_trap(&self, trap: &wasmtime::Error) -> (String, Option<String>) {
        let what = match trap.downcast_ref::<wasmtime::Trap>() {
            Some(t) => t.to_string(),
            None => format!("{trap:#}"),
        };
        let frames = trap.downcast_ref::<wasmtime::WasmBacktrace>().map(|bt| bt.frames());
        let offsets = frames.into_iter().flatten().filter_map(|f| f.module_offset());
        let at = match &self.lines {
            Some(lines) => offsets.into_iter().find_map(|o| lines.at(o as u32)).map(String::from),
            None => offsets.into_iter().next().map(|o| format!("code offset {o:#x}")),
        };
        match at {
            Some(loc) => (format!("{what} at {loc}"), Some(loc)),
            None => (what, None),
        }
    }

    /// Renders `quanta` quanta of the program's voice ([`wrela_abi::AUDIO_QUANTUM`] samples
    /// each, at [`wrela_abi::AUDIO_SAMPLE_RATE`]) on an instance of its own, as the audio
    /// thread does, here and now: an offline run. The samples are the two channels
    /// interleaved: left, right, left, and so on. The voice goes on where the last call
    /// stopped. Errors if the program hasn't started one.
    pub(crate) fn render_audio(&mut self, quanta: u32) -> Result<Vec<f32>> {
        let Some((task, context)) = self.store.data().voice else {
            return Err(Error::Program("it hasn't started a voice (`std::audio::play`)".into()));
        };
        let wrap = |e: wasmtime::Error| Error::Program(format!("{e:#}"));
        if self.audio.is_none() {
            let (mut store, instance) =
                other_instance(&self.module, &self.memory, "the audio thread", &self.trace(), None)
                    .map_err(wrap)?;
            let render = instance
                .get_typed_func::<(u32, u32, u32), ()>(&mut store, EXPORT_AUDIO)
                .map_err(wrap)?;
            self.audio = Some((store, render));
        }
        let trace = self.trace();
        let n = AUDIO_QUANTUM as usize;
        let channels = wrela_abi::AUDIO_CHANNELS as usize;
        let mut out = Vec::with_capacity(channels * n * quanta as usize);
        self.last_failure = None;
        for _ in 0..quanta {
            let result = {
                let (store, render) = self.audio.as_mut().expect("made above");
                trace.end_phase_after(|| {
                    render.call(&mut *store, (memory::THREAD_AUDIO, task, context))
                })
            };
            if let Err(trap) = result {
                return Err(match self.trapped(&trap, memory::THREAD_AUDIO) {
                    Error::Trap(why) => Error::Trap(format!("the voice trapped: {why}")),
                    error => error,
                });
            }
            // The quantum finished writing these samples before returning. Convert them
            // straight from shared memory, before the next quantum can overwrite them.
            let bytes = shared::slice(&self.memory, memory::AUDIO_OUT as usize, channels * n * 4)
                .ok_or_else(|| {
                Error::Program("the samples are past the memory's end".into())
            })?;
            let sample = |i: usize| {
                let b = &bytes[4 * i..4 * i + 4];
                f32::from_le_bytes([b[0], b[1], b[2], b[3]])
            };
            for i in 0..n {
                out.push(sample(i));
                out.push(sample(n + i));
            }
        }
        Ok(out)
    }

    /// How many chunks of parallel jobs the helpers have run (`wrela_abi::memory::PAR_HELPED`).
    pub(crate) fn helped(&self) -> u32 {
        if self.memory.data().len() < memory::PAR_HELPED as usize + 4 {
            0
        } else {
            shared::load_u32(&self.memory, memory::PAR_HELPED)
        }
    }

    /// Holds back each task's result for `micros` microseconds once it's ready, whichever
    /// thread ran it (`wrela_abi::memory::PAR_HOLD`): for tests, which slow jobs down so the
    /// threads that take them wait.
    pub(crate) fn hold_jobs(&self, micros: u32) {
        if self.memory.data().len() >= memory::PAR_HOLD as usize + 4 {
            shared::store_u32(&self.memory, memory::PAR_HOLD, micros);
        }
    }

    /// The ticker's rate, once the program has started one (`std::tick::start`).
    pub(crate) fn ticker_hz(&self) -> Option<u32> {
        self.store.data().ticker.map(|t| t.2)
    }

    /// Whether each tick reports its state's hash (`wrela_abi::memory::TICK_WANT_HASH`), and so
    /// does the ticker's `start`, if it hasn't run yet.
    pub(crate) fn want_hashes(&self, on: bool) {
        if self.memory.data().len() >= memory::TICK_WANT_HASH as usize + 4 {
            shared::store_u32(&self.memory, memory::TICK_WANT_HASH, u32::from(on));
        }
    }

    /// The state hash the ticker reported last: after `start`, the first state's.
    pub(crate) fn reported_hash(&self) -> u64 {
        shared::load_u64(&self.memory, memory::TICK_HASH)
    }

    /// A tick log of no ticks yet, for the program's ticker and the build whose WASM hash is
    /// `wasm_hash`: an error if the program has no ticker.
    pub(crate) fn tick_log(&self, wasm_hash: u64) -> Result<TickLog> {
        let Some(hz) = self.ticker_hz() else {
            return Err(Error::Program(
                "it starts no ticker in `init` (`std::tick::start`)".into(),
            ));
        };
        Ok(TickLog::new(wasm_hash, hz, self.reported_hash()))
    }

    /// Queues input events for the ticker: the next ticks take them as records, at most
    /// `wrela_abi::memory::MAX_TICK_RECORDS` each, oldest first.
    pub(crate) fn push_records(&mut self, events: impl IntoIterator<Item = Event>) {
        self.records.extend(events.into_iter().map(|e| e.bytes()));
    }

    /// Queues records exactly as a tick log has them.
    pub(crate) fn push_raw_records(&mut self, records: &[[u8; EVENT_SIZE as usize]]) {
        self.records.extend(records.iter().copied());
    }

    /// The next tick's number.
    fn next_tick(&self) -> u32 {
        self.ticker.as_ref().map_or(0, |t| t.next)
    }

    /// Runs the ticker's next tick on its own instance, with the queued records (at most
    /// `MAX_TICK_RECORDS`; the rest wait): its number, and the state's hash if
    /// [`Program::want_hashes`] asked for it. An error if the program has no ticker, or the
    /// tick trapped (the program's panic).
    pub(crate) fn tick(&mut self) -> Result<Ticked> {
        let k = self.next_tick();
        // Replaying the replaced build's ticks: each with the records it had, while the
        // queued records wait.
        let records: Vec<[u8; EVENT_SIZE as usize]> = match &mut self.replay {
            Some(r) if k < r.upto => r.records.remove(&k).unwrap_or_default(),
            _ => {
                self.replay = None;
                let n = self.records.len().min(memory::MAX_TICK_RECORDS as usize);
                self.records.drain(..n).collect()
            }
        };
        if let Some(ran) = &mut self.ran
            && !records.is_empty()
        {
            ran.push((k, records.clone()));
        }
        let mut thread = self.take_tick_thread()?;
        let result = thread.run(records);
        self.ticker = Some(thread);
        let t = result.map_err(|(k, trap)| self.tick_trapped(k, &trap))?;
        if t.waited > 0 {
            let line = format!("tick {}: the sim waited for a job due ({}×)", t.tick, t.waited);
            self.store.data().trace.keep(line);
        }
        Ok(t)
    }

    /// Runs the next tick with `script`'s records of it (its tick-keyed events) queued after the
    /// rest. With `log`, its records and state hash go into it (hashes asked for).
    pub(crate) fn scripted_tick(
        &mut self,
        script: &[Scripted],
        log: Option<&mut TickLog>,
    ) -> Result<()> {
        self.push_records(wrela_abi::input::records_at(script, self.next_tick()));
        let t = self.tick()?;
        if let Some(log) = log {
            let hash = t.hash.expect("hashes asked for");
            log.ticks.push(Tick { records: t.records, hash });
        }
        Ok(())
    }

    /// Frame `i` of a lockstep schedule at `fps` (#43 §2.3): `script`'s events of frame `i`
    /// queued for the frame and the ticker, then the ticks that come before the frame (each with
    /// its tick-keyed records too), then the frame. With `log`, each tick's records and state
    /// hash go into it (hashes asked for).
    pub(crate) fn lockstep_frame(
        &mut self,
        i: u32,
        fps: f64,
        width: u32,
        height: u32,
        script: &[Scripted],
        mut log: Option<&mut TickLog>,
    ) -> Result<()> {
        valid_fps(fps)?;
        let arriving: Vec<&Scripted> = wrela_abi::input::events_at(script, i).collect();
        for s in &arriving {
            self.push_scripted(s);
        }
        // A tick takes no network events.
        let events: Vec<Event> =
            arriving.iter().map(|s| s.event).filter(|e| !e.kind.is_network()).collect();
        if let Some(hz) = self.ticker_hz() {
            let from = *self.tick_from.get_or_insert(i);
            // A build handed over to starts its own ticks at this frame's time. A hot
            // reload keeps the old base, so its replayed ticks stay on that same clock.
            shared::write(
                &self.memory,
                memory::TICK_ORIGIN as usize,
                &(f64::from(from) / fps).to_le_bytes(),
            );
            self.push_records(events);
            while self.next_tick() < lockstep_ticks(i - from, hz, fps) {
                self.scripted_tick(script, log.as_deref_mut())?;
            }
        }
        self.frame(frame_time(i, fps), width, height)
    }

    /// The ticker's thread's instance, made the first time, to run ticks on any OS thread
    /// ([`TickThread::run`]); give it back by putting it in `self.ticker`. An error if the
    /// program hasn't started a ticker.
    fn take_tick_thread(&mut self) -> Result<TickThread> {
        let Some((task, context, _)) = self.store.data().ticker else {
            return Err(Error::Program("it hasn't started a ticker (`std::tick::start`)".into()));
        };
        if let Some(t) = self.ticker.take() {
            return Ok(t);
        }
        let wrap = |e: wasmtime::Error| Error::Program(format!("{e:#}"));
        let (mut store, instance) =
            other_instance(&self.module, &self.memory, "the ticker", &self.trace(), None)
                .map_err(wrap)?;
        let tick = instance
            .get_typed_func::<(u32, u32, u32, u32), ()>(&mut store, EXPORT_TICK)
            .map_err(wrap)?;
        let memory = self.memory.clone();
        let trace = self.trace();
        Ok(TickThread { store, tick, next: 0, memory, task, context, fuel: self.fuel, trace })
    }

    /// Runs `ticks` ticks (with no records, each asked for its hash) on an OS thread of their
    /// own while this one runs `frames` frames at `fps`, neither waiting for the other (test
    /// mode's paced schedule, #43 §2.3): each tick, and its time in milliseconds.
    pub(crate) fn ticks_beside_frames(
        &mut self,
        ticks: u32,
        frames: u32,
        fps: f64,
        width: u32,
        height: u32,
    ) -> Result<Vec<(Ticked, f64)>> {
        valid_fps(fps)?;
        self.want_hashes(true);
        let mut thread = self.take_tick_thread()?;
        let (ticked, framed) = std::thread::scope(|s| {
            let ticking = s.spawn(move || {
                let mut out = Vec::new();
                for _ in 0..ticks {
                    let start = std::time::Instant::now();
                    match thread.run(Vec::new()) {
                        Ok(t) => out.push((t, start.elapsed().as_secs_f64() * 1000.0)),
                        Err(e) => return (thread, out, Some(e)),
                    }
                }
                (thread, out, None)
            });
            let framed =
                (0..frames).try_for_each(|i| self.frame(frame_time(i, fps), width, height));
            (ticking.join().expect("the ticker's thread"), framed)
        });
        let (thread, out, failed) = ticked;
        self.ticker = Some(thread);
        if let Some((k, trap)) = failed {
            return Err(self.tick_trapped(k, &trap));
        }
        framed?;
        Ok(out)
    }

    /// Tick `k` trapped: the program's panic, as the host reports it.
    fn tick_trapped(&mut self, k: u32, trap: &wasmtime::Error) -> Error {
        let error = self.trapped(trap, memory::THREAD_TICK);
        if let Some(f) = &mut self.last_failure {
            f.tick = Some(k);
        }
        match error {
            Error::Trap(why) => Error::Trap(format!("in tick {k}: {why}")),
            e => e,
        }
    }

    /// FNV-1a 64 of every byte submitted since the program loaded.
    pub(crate) fn hash(&self) -> StateHash {
        self.store.data().hash
    }

    /// Queues an input event: the program reads it (`wrela.input`) from its next call on.
    pub(crate) fn push_input(&mut self, e: Event) {
        self.store.data_mut().input.push(e);
    }

    /// Queues a script's event (a message with its bytes).
    pub(crate) fn push_scripted(&mut self, s: &Scripted) {
        self.store.data_mut().input.push_scripted(s);
    }

    /// Queues a message of `bytes` from connection `conn`.
    pub(crate) fn push_message(&mut self, conn: u32, bytes: Vec<u8>) {
        self.store.data_mut().input.push_message(conn, bytes);
    }

    /// The lines printed since the last call.
    pub(crate) fn take_logs(&mut self) -> Vec<String> {
        self.store.data().trace.take_lines()
    }

    /// What the program printed and timed, which its threads share.
    pub(crate) fn trace(&self) -> Trace {
        self.store.data().trace.clone()
    }

    pub(crate) fn take_batches(&mut self) -> Vec<Vec<u8>> {
        self.store.data_mut().batches.as_mut().map(std::mem::take).unwrap_or_default()
    }

    #[cfg_attr(
        not(any(feature = "gpu", test)),
        expect(dead_code, reason = "only `Host` reaches its GPU")
    )]
    pub(crate) fn executor(&mut self) -> &mut E {
        &mut self.store.data_mut().executor
    }
}

pub(crate) fn valid_fps(fps: f64) -> Result<()> {
    if fps.is_finite() && fps > 0.0 {
        Ok(())
    } else {
        Err(Error::Program(format!("fps must be finite and greater than zero, not {fps:?}")))
    }
}

/// Another thread's instance of the program, on the shared memory: a helper's, the audio
/// thread's or the ticker's. Its code records no GPU work and makes no requests (its effects
/// are checked), so the host's functions refuse to run, as `who`.
fn other_instance(
    module: &Module,
    memory: &SharedMemory,
    who: &'static str,
    trace: &Trace,
    stop: Option<&Stop>,
) -> wasmtime::Result<(Store<bool>, Instance)> {
    let engine = module.engine();
    let mut store = Store::new(engine, false);
    never_due(&mut store);
    if let Some(stop) = stop {
        stop.install(&mut store);
    }
    // On a metered engine, another thread's instance isn't what a call's fuel bounds.
    if engine.get_consume_fuel() {
        store.set_fuel(u64::MAX)?;
    }
    let mut linker = Linker::new(engine);
    linker.define(&store, IMPORT_MODULE, IMPORT_MEMORY, memory.clone())?;
    // The clock answers on every thread (an `@audio` voice may read it), and so do a line
    // printed and a phase (a tick or a parallel job's chunk may print).
    let m = memory.clone();
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_CLOCK,
        move |mut caller: wasmtime::Caller<'_, bool>, ptr: u32| {
            check_thread_started(caller.data_mut())?;
            write_clock(&m, ptr)
        },
    )?;
    trace.link_checked(&mut linker, Some(memory), check_thread_started)?;
    refuse(&mut linker, engine, [IMPORT_CLOCK, IMPORT_PRINT, IMPORT_PHASE], |_| {
        format!("{who} can't call the host")
    })?;
    let instance = linker.instantiate(&mut store, module)?;
    *store.data_mut() = true;
    Ok((store, instance))
}

/// Helper threads for code the build runs (constants, language.md §13): each an instance of a
/// module built with shared memory, on a thread of its own, running `__worker` until
/// [`BuildHelpers::finish`]. Each has `fuel` (as the call it helps does), and what each used is
/// counted, so the build bounds a constant's work across its threads.
pub struct BuildHelpers {
    memory: SharedMemory,
    threads: Vec<std::thread::JoinHandle<HelperRun>>,
}

/// What a build's helper did: the fuel it used, and whether it ran out.
#[derive(Clone, Copy, Debug, Default)]
pub struct HelperRun {
    pub used: u64,
    pub out_of_fuel: bool,
}

impl BuildHelpers {
    /// `count` helpers for `module`'s instances on `memory` (none if it has no `__worker`: it
    /// runs no parallel jobs), each with `fuel`, printing and timing into `trace`, and ended by
    /// `stop` (which a module of a stoppable engine needs: [`stoppable_metered_engine`]).
    pub fn start(
        module: &Module,
        memory: &SharedMemory,
        count: u32,
        fuel: u64,
        trace: &Trace,
        stop: Option<&Stop>,
    ) -> BuildHelpers {
        let n = if module.get_export(EXPORT_WORKER).is_some() {
            let n = count.min(MAX_WORKERS);
            shared::store_u32(memory, memory::PAR_HELPERS, n);
            n
        } else {
            0
        };
        let threads = (0..n)
            .map(|i| {
                let (module, memory, trace) = (module.clone(), memory.clone(), trace.clone());
                let stop = stop.cloned();
                let thread = memory::THREAD_HELPER0 + i;
                std::thread::spawn(move || {
                    let mut used = HelperRun::default();
                    let run = |used: &mut HelperRun| -> wasmtime::Result<()> {
                        let (mut store, instance) = other_instance(
                            &module,
                            &memory,
                            "build-time code",
                            &trace,
                            stop.as_ref(),
                        )?;
                        let metered = store.engine().get_consume_fuel();
                        if metered {
                            store.set_fuel(fuel)?;
                        }
                        let worker =
                            instance.get_typed_func::<u32, ()>(&mut store, EXPORT_WORKER)?;
                        let result = worker.call(&mut store, thread);
                        if metered {
                            used.used = fuel - store.get_fuel().unwrap_or(0);
                        }
                        if let Err(e) = &result {
                            used.out_of_fuel = e.downcast_ref::<wasmtime::Trap>()
                                == Some(&wasmtime::Trap::OutOfFuel);
                        }
                        result
                    };
                    if trace.end_phase_after(|| run(&mut used)).is_err() {
                        helper_trapped(&memory, thread);
                    }
                    used
                })
            })
            .collect();
        BuildHelpers { memory: memory.clone(), threads }
    }

    /// Stops the helpers and waits for them: what each did.
    pub fn finish(mut self) -> Vec<HelperRun> {
        self.stop()
    }

    fn stop(&mut self) -> Vec<HelperRun> {
        if self.threads.is_empty() {
            return Vec::new();
        }
        shut_down_helpers(&self.memory);
        self.threads.drain(..).map(|t| t.join().unwrap_or_default()).collect()
    }
}

impl Drop for BuildHelpers {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The host functions build-time code imports, each refusing as `why` says: constants and
/// tests record no GPU work and make no requests (their effects are checked, §8). What they
/// print and time, `trace` answers.
pub fn refuse_host_functions<T: 'static>(
    linker: &mut Linker<T>,
    engine: &Engine,
    why: fn(results: u32) -> &'static str,
    trace: &Trace,
    memory: Option<&SharedMemory>,
) -> wasmtime::Result<()> {
    trace.link(linker, memory)?;
    refuse(linker, engine, [IMPORT_AUDIO, IMPORT_PRINT, IMPORT_PHASE], why)
}

/// Defines each host function but those `answered` as one that traps with `why`, given how many
/// results it has.
fn refuse<T: 'static, W: std::fmt::Display + Send + Sync + 'static>(
    linker: &mut Linker<T>,
    engine: &Engine,
    answered: [&str; 3],
    why: impl Fn(u32) -> W,
) -> wasmtime::Result<()> {
    for f in HOST_FUNCTIONS.iter().filter(|f| !answered.contains(&f.name)) {
        let why = why(f.results as u32);
        let i32s = |n| std::iter::repeat_n(ValType::I32, n);
        let ty = FuncType::new(engine, i32s(f.params), i32s(f.results));
        linker.func_new(IMPORT_MODULE, f.name, ty, move |_, _, _| {
            Err(wasmtime::format_err!("{why}"))
        })?;
    }
    Ok(())
}

/// A helper: an instance of the program on a thread of its own, number `thread`, running
/// `__worker(thread)` until the program shuts it down. If it traps, it tells the thread that
/// waits for what it ran (wrela_abi `memory`'s helpers), and runs nothing more.
fn spawn_worker(
    module: Module,
    memory: SharedMemory,
    thread: u32,
    trace: Trace,
    stop: Option<Stop>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new().name(format!("wrela-helper-{thread}")).spawn(move || {
        let run = || -> wasmtime::Result<()> {
            let (mut store, instance) =
                other_instance(&module, &memory, "a helper", &trace, stop.as_ref())?;
            let worker = instance.get_typed_func::<u32, ()>(&mut store, EXPORT_WORKER)?;
            worker.call(&mut store, thread)
        };
        if trace.end_phase_after(run).is_err() {
            helper_trapped(&memory, thread);
        }
    })
}

/// A helper, `thread`, trapped: the thread waiting for what it ran learns it, and traps with
/// its panic message (wrela_abi `memory`'s helpers).
pub(crate) fn helper_trapped(m: &SharedMemory, thread: u32) {
    let at = memory::thread_block(thread) + memory::RUNNING;
    let len = m.data().len();
    if at as usize + 4 > len {
        return;
    }
    let running = shared::load_u32(m, at);
    let blocks = memory::THREAD_BLOCKS..memory::THREAD_BLOCKS_END;
    let slots = memory::TASK_SLOTS..memory::TASK_SLOTS_END;
    if blocks.contains(&running)
        && (running - memory::THREAD_BLOCKS).is_multiple_of(memory::THREAD_BLOCK_SIZE)
        && (running + memory::JOB_FAILED + 4) as usize <= len
    {
        // A thread's parallel job: its starting thread waits on DONE. The first helper to trap
        // is the one whose message the program reports.
        shared::cas_u32(m, running + memory::JOB_FAILED, 0, thread + 1);
        shared::or_u32(m, running + memory::JOB_DONE, memory::JOB_DONE_FAILED);
        let _ = m.atomic_notify(u64::from(running + memory::JOB_DONE), u32::MAX);
    } else if slots.contains(&running)
        && (running - memory::TASK_SLOTS).is_multiple_of(memory::TASK_SLOT_SIZE)
        && (running + memory::SLOT_THREAD + 4) as usize <= len
    {
        // A task: whoever takes it waits on its state.
        shared::store_u32(m, running + memory::SLOT_THREAD, thread);
        shared::store_u32(m, running + memory::SLOT_STATE, memory::SLOT_FAILED);
        let _ = m.atomic_notify(u64::from(running + memory::SLOT_STATE), u32::MAX);
    }
}

/// Fails the host function that's running: `e` is kept for the caller ([`State::failure`]),
/// and wasmtime traps with its message.
fn fail<E>(state: &mut State<E>, e: Error) -> wasmtime::Error {
    let message = e.to_string();
    state.failure = Some(e);
    wasmtime::format_err!("{message}")
}

/// Fails a host function the instance's start function called: it may not call the host.
fn check_started<E>(state: &mut State<E>) -> wasmtime::Result<()> {
    if state.started {
        return Ok(());
    }
    Err(fail(state, Error::Program(CALLED_WHILE_STARTING.into())))
}

/// [`check_started`] for another thread's instance, whose state only records whether its start
/// function has finished.
fn check_thread_started(started: &mut bool) -> wasmtime::Result<()> {
    if *started {
        return Ok(());
    }
    Err(wasmtime::format_err!("{CALLED_WHILE_STARTING}"))
}

/// `wrela.submit(ptr, len)`: check and keep one batch until its export returns successfully.
fn submit<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    ptr: u32,
    len: u32,
) -> wasmtime::Result<()> {
    let state = caller.data_mut();
    check_started(state)?;
    // A handle of its own, so the batch is read in place while `state` changes.
    let memory = state.memory.clone();
    let result = match shared::slice(&memory, ptr as usize, len as usize) {
        Some(batch) => state.submit(batch),
        None => Err(Error::Trap(format!(
            "submit({ptr}, {len}) reaches past the end of the program's memory ({} bytes)",
            state.memory.data().len()
        ))),
    };
    result.map_err(|e| fail(state, e))
}

/// `wrela.audio(task, context)`: starts the program's voice. The audio thread renders it
/// ([`Program::render_audio`]). A program has one.
fn audio<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    task: u32,
    context: u32,
) -> wasmtime::Result<()> {
    let state = caller.data_mut();
    check_started(state)?;
    if state.voice.is_some() {
        let e = Error::Trap("a program starts one voice, and this one started a second".into());
        return Err(fail(state, e));
    }
    if !state.has_audio_entry {
        let e = Error::Trap(format!(
            "it starts a voice but doesn't export `{EXPORT_AUDIO}(thread, task, context)`"
        ));
        return Err(fail(state, e));
    }
    let need = memory::AUDIO_OUT as usize + AUDIO_QUANTUM as usize * 4;
    if state.memory.data().len() < need {
        let got = state.memory.data().len();
        let e = Error::Trap(format!(
            "it starts a voice but its memory is {got} bytes; the audio thread needs at least {need}"
        ));
        return Err(fail(state, e));
    }
    state.voice = Some((task, context));
    Ok(())
}

/// `wrela.tick(task, context, hz)`: starts the program's ticker, whose ticks the host runs
/// ([`Program::tick`]). A program has one: a second is its panic.
fn tick<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    task: u32,
    context: u32,
    hz: u32,
) -> wasmtime::Result<()> {
    let state = caller.data_mut();
    check_started(state)?;
    if state.ticker.is_some() {
        let e =
            Error::Trap("panic: a program starts one ticker, and this one started a second".into());
        return Err(fail(state, e));
    }
    if hz == 0 {
        let e =
            Error::Trap("it starts a ticker at 0 Hz; a ticker ticks at least once a second".into());
        return Err(fail(state, e));
    }
    if !state.has_tick_entry {
        let e = Error::Trap(format!(
            "it starts a ticker but doesn't export `{EXPORT_TICK}(thread, task, context, tick)`"
        ));
        return Err(fail(state, e));
    }
    let need =
        memory::TICK_RECORDS as usize + 4 + memory::MAX_TICK_RECORDS as usize * EVENT_SIZE as usize;
    if state.memory.data().len() < need {
        let got = state.memory.data().len();
        let e = Error::Trap(format!(
            "it starts a ticker but its memory is {got} bytes; the ticker needs at least {need}"
        ));
        return Err(fail(state, e));
    }
    state.ticker = Some((task, context, hz));
    Ok(())
}

/// `wrela.input(ptr, cap) -> count`: copies up to `cap` queued input events to `ptr`, oldest
/// first, and returns how many (wrela_abi `input`).
fn input<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    ptr: u32,
    cap: u32,
) -> wasmtime::Result<i32> {
    let state = caller.data_mut();
    check_started(state)?;
    let n = (cap as usize).min(state.input.len());
    let fits = (ptr as usize)
        .checked_add(n * EVENT_SIZE as usize)
        .is_some_and(|end| end <= state.memory.data().len());
    if !fits {
        return Err(wasmtime::format_err!(
            "input({ptr}, {cap}) reaches past the end of the program's memory"
        ));
    }
    // Fewer than `n` when the call's messages reach their bytes' limit.
    let bytes = state.input.take(cap);
    shared::write(&state.memory, ptr as usize, &bytes);
    Ok((bytes.len() / EVENT_SIZE as usize) as i32)
}

/// `wrela.payload(serial_lo, serial_hi, ptr, cap) -> length`: copies the bytes of a message the
/// current call was given, up to `cap` of them, to `ptr`; -1 for another serial.
fn payload<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    serial_lo: u32,
    serial_hi: u32,
    ptr: u32,
    cap: u32,
) -> wasmtime::Result<i32> {
    let state = caller.data_mut();
    check_started(state)?;
    let serial = u64::from(serial_lo) | (u64::from(serial_hi) << 32);
    let Some(bytes) = state.input.payload(serial) else { return Ok(-1) };
    let n = bytes.len().min(cap as usize);
    if !shared::write(&state.memory, ptr as usize, &bytes[..n]) {
        return Err(wasmtime::format_err!(
            "payload(.., {ptr}, {cap}) reaches past the end of the program's memory"
        ));
    }
    Ok(bytes.len() as i32)
}

/// `wrela.keep(ptr, len)`: keeps those bytes for the build that replaces this one (hot reload).
fn keep<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    ptr: u32,
    len: u32,
) -> wasmtime::Result<()> {
    let state = caller.data_mut();
    check_started(state)?;
    let Some(bytes) = shared::slice(&state.memory, ptr as usize, len as usize) else {
        return Err(wasmtime::format_err!(
            "keep({ptr}, {len}) reaches past the end of the program's memory"
        ));
    };
    // A program may keep its state every frame: the buffer is reused.
    state.keeping.clear();
    state.keeping.extend_from_slice(bytes);
    Ok(())
}

/// `wrela.kept(ptr, cap) -> i32`: copies up to `cap` bytes of what the replaced build kept to
/// `ptr`; how many it kept.
fn kept<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    ptr: u32,
    cap: u32,
) -> wasmtime::Result<i32> {
    let state = caller.data_mut();
    check_started(state)?;
    let n = (cap as usize).min(state.kept.len());
    if !shared::write(&state.memory, ptr as usize, &state.kept[..n]) {
        return Err(wasmtime::format_err!(
            "kept({ptr}, {cap}) reaches past the end of the program's memory"
        ));
    }
    Ok(state.kept.len() as i32)
}

/// When the host started its first program: `wrela.clock`'s zero, the same for every thread
/// and every build a hot reload swaps in, so the clock never goes back.
static CLOCK_ZERO: LazyLock<std::time::Instant> = LazyLock::new(std::time::Instant::now);

/// The clock (`wrela.clock`): the seconds since [`CLOCK_ZERO`], the same on every thread.
pub(crate) fn clock_seconds() -> f64 {
    CLOCK_ZERO.elapsed().as_secs_f64()
}

/// Writes the clock (`wrela.clock`): the seconds since [`CLOCK_ZERO`], an `f64`, at `ptr`.
fn write_clock(memory: &SharedMemory, ptr: u32) -> wasmtime::Result<()> {
    let seconds = clock_seconds();
    if !shared::write(memory, ptr as usize, &seconds.to_le_bytes()) {
        return Err(wasmtime::format_err!(
            "clock({ptr}) reaches past the end of the program's memory"
        ));
    }
    Ok(())
}

/// `wrela.clock(ptr)`: the seconds since the host started its first program, at `ptr`.
fn clock<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    ptr: u32,
) -> wasmtime::Result<()> {
    let state = caller.data_mut();
    check_started(state)?;
    write_clock(&state.memory, ptr)
}

/// `wrela.limit(index) -> i32`: one of the device's limits ([`wrela_abi::Limits`]), as a `u32`'s
/// bits; 0 past the last.
fn limit<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    index: u32,
) -> wasmtime::Result<i32> {
    let state = caller.data_mut();
    check_started(state)?;
    Ok(state.executor.limits().get(index) as i32)
}

/// `wrela.request_status(request) -> i32`: -1 while the request is pending, -2 if it failed,
/// -3 if it was a storage read of a path that holds nothing, else its answer's length in bytes.
fn request_status<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    request: u32,
) -> wasmtime::Result<i32> {
    let state = caller.data_mut();
    check_started(state)?;
    Ok(match state.requests.answered.get(&request) {
        None => REQUEST_PENDING,
        Some(Err(_)) if state.requests.missing.contains(&request) => REQUEST_MISSING,
        Some(Err(_)) => REQUEST_FAILED,
        Some(Ok(bytes)) => bytes.len() as i32,
    })
}

/// `wrela.request_take(request, ptr)`: copies an answered request's bytes to `ptr` in the
/// program's memory, and forgets the request (a failed one too).
fn request_take<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    request: u32,
    ptr: u32,
) -> wasmtime::Result<()> {
    let state = caller.data_mut();
    check_started(state)?;
    let Some(answer) = state.requests.answered.get(&request) else {
        return Err(wasmtime::format_err!("request_take({request}): the request isn't answered"));
    };
    let bytes = answer.as_ref().map_or(&[][..], Vec::as_slice);
    if !shared::write(&state.memory, ptr as usize, bytes) {
        return Err(wasmtime::format_err!(
            "request_take({request}, {ptr}) reaches past the end of the program's memory"
        ));
    }
    state.requests.answered.remove(&request);
    state.requests.missing.remove(&request);
    Ok(())
}

#[cfg(test)]
mod tests;
