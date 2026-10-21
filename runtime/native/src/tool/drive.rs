//! `std::tool::drive` (language.md §15.5): a build loaded on the native host, beside the tool,
//! and run as the tool says, in steps: `init`, frames and ticks with a `std::input::Script`, its
//! exports called and its voice rendered, the screen captured, the state's hash, its phases, its
//! GPU buffers, the commands it recorded, and which build runs. It's the native host's own
//! `Host` (with a GPU) or `CpuHost`, not another.
//!
//! **Each driven build has a process of its own** ([`child`]): this `wrela`, run again as
//! `wrela tool --drive`. Its calls are never interrupted from inside: a deadline is the host's kill
//! of the process's group, which stops every thread of the build at once, a join or a large
//! answer half written too, which an interrupted call can't. The host opens the build's directory, its root and its
//! storage through the grants, as before, and gives the process those directories, open; the
//! process opens nothing by a path. Commands and answers cross a pair of pipes, in frames
//! ([`wire`]). The process leads a process group of its own, which a supervisor's file of groups
//! has before the process gets its first command; it ends at once when its commands end, so a
//! host that dies, however it dies, leaves none behind.
//!
//! **A driven build is a resource of its request.** `load` makes it, and it's registered under
//! the request before the answer that names it reaches the tool: until the tool reads that
//! answer it's the request's, and if the request fails, or the tool ends first, the host closes
//! it. The host closes every driven build when the tool ends. `run` loads a build, runs its
//! steps and closes it, in one request.
//!
//! **One request at a time** runs on a build: another fails while one runs. The worker that
//! waits for its answer looks for its deadline (from when the host took the request) and the
//! tool's end every few milliseconds; past either, the host kills the build's process group, and
//! the build can't be observed again. A build whose code trapped stays alive: what doesn't run
//! code can still be observed.
//!
//! **Blobs are reserved before they're read.** Before the build's process reads a screen or a
//! buffer back, or renders a voice, it asks the host to reserve the blob's bytes in the tool's
//! budget, under the request: one that doesn't fit isn't read. The bytes then come in a frame
//! the size of which the host checks against what it reserved before it reads them.
//!
//! **Compiled code is shared.** The host compiles each build's WASM once in a run, for the
//! engine the build's process runs on, by its SHA-256 ([`Codes`]), and keeps the code: every
//! load of the build, and every hand-over to it, is given that code. Between runs, the code is
//! kept in the host's code cache (`crate::code`), which no grant reaches.

mod child;
mod wire;

pub use child::run as run_child;

use super::services::{Answer, Body, Data, Services, Task, Work};
use super::toolchain::Out;
use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::time::{Duration, Instant};
use wire::{Load, Step};

/// A request's deadline unless it asks for another.
const DEADLINE: Duration = Duration::from_secs(60);

/// How often the worker looks at the request it waits for.
const POLL: Duration = Duration::from_millis(10);

/// The most bytes of a build's WASM its process sends for the host to compile.
const MOST_WASM: usize = 512 << 20;

/// Where a build's code came from, as `drive::Code` numbers it: compiled for this load, read
/// from the code cache, or compiled earlier in the run.
const COMPILED: u8 = 0;
const CACHED: u8 = 1;
const REUSED: u8 = 2;

/// The driven builds of one run: each by its handle, and each request's that the tool hasn't
/// been told of.
#[derive(Default)]
pub(crate) struct Drives {
    held: HashMap<u32, Arc<Driven>>,
    next: u32,
    unpublished: HashMap<u32, Vec<u32>>,
}

impl Drives {
    /// The tool has read request `request`'s answer: its driven build is the tool's now.
    pub(crate) fn publish(&mut self, request: u32) {
        self.unpublished.remove(&request);
    }

    /// Request `request` failed: the builds made for it, which no answer names, are closed.
    pub(crate) fn rollback(&mut self, request: u32) -> Vec<Arc<Driven>> {
        let ids = self.unpublished.remove(&request).unwrap_or_default();
        ids.iter().filter_map(|id| self.held.remove(id)).collect()
    }

    /// Takes every build, to close: the tool has ended.
    pub(crate) fn take_all(&mut self) -> Vec<Arc<Driven>> {
        self.unpublished.clear();
        self.held.drain().map(|(_, d)| d).collect()
    }

    /// Kills every build's process group: the tool has ended.
    pub(crate) fn stop_all(&self) {
        for d in self.held.values() {
            d.stop("the tool ended");
        }
    }
}

/// The request a build's process works for: its number, and the blobs reserved for it, each
/// with its bytes reserved and whether they've come.
struct Current {
    request: u32,
    reserved: HashMap<u32, (u64, bool)>,
}

/// A driven build: its process, the process group it leads, the pipe its commands go down, the
/// answers its reader thread passes on, the request it works for, whether a request runs,
/// why it can't be observed (it was stopped, or its process ended), where its code came from,
/// and the storage directory the host made for it (removed with it).
pub(crate) struct Driven {
    process: Lifecycle<Spawned>,
    to: Mutex<File>,
    answers: Mutex<mpsc::Receiver<Result<Vec<u8>, String>>>,
    reader: Mutex<Option<std::thread::JoinHandle<()>>>,
    current: Mutex<Option<Current>>,
    busy: AtomicBool,
    stopped: Mutex<Option<String>>,
    code: Mutex<Option<u8>>,
    storage: Option<PathBuf>,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Driven {
    /// Kills its process group, saying `why` it can't be observed now (the first reason
    /// stays): unless it's reaped already.
    fn stop(&self, why: &str) {
        locked(&self.stopped).get_or_insert_with(|| why.to_string());
        self.process.signal();
    }

    /// Kills its process group, saying `why`, and reaps its process: whether it was reaped
    /// within `grace`.
    fn kill(&self, why: &str, grace: Duration) -> bool {
        locked(&self.stopped).get_or_insert_with(|| why.to_string());
        self.process.reap(grace)
    }

    /// Closes it: its process group is killed and its process reaped, and its storage goes if
    /// the host made it. Its reader, which ends with the process's pipe, is joined if it has
    /// ended; one still at work (compiling the build's code, say) is put with `strays`, which
    /// the run's end waits for within its grace. Whether its process was reaped within `grace`
    /// (if not, it's left, and the run isn't clean).
    pub(crate) fn close(
        &self,
        grace: Duration,
        strays: &Mutex<Vec<std::thread::JoinHandle<()>>>,
    ) -> bool {
        let reaped = self.kill("it was closed", grace);
        if let Some(reader) = locked(&self.reader).take() {
            if reader.is_finished() {
                let _ = reader.join();
            } else {
                locked(strays).push(reader);
            }
        }
        if let Some(dir) = &self.storage {
            let _ = std::fs::remove_dir_all(dir);
        }
        reaped
    }

    /// Why it can't be observed, if it can't.
    fn unobservable(&self, handle: u32) -> Result<(), String> {
        match locked(&self.stopped).as_ref() {
            Some(why) => Err(format!("the driven build {handle} can't be observed: {why}")),
            None => Ok(()),
        }
    }

    /// A reply to its process's ask `id`.
    fn reply(&self, id: u32, status: u8, bytes: &[u8]) -> std::io::Result<()> {
        wire::write_frame(
            &mut *locked(&self.to),
            wire::REPLY,
            &[&id.to_le_bytes(), &[status], bytes],
        )
    }
}

/// What the host does to a driven build's process: kill its process group, a signal alone
/// (nothing is reaped), and reap it.
trait Process: Send {
    fn signal(&mut self);
    /// Whether it has ended, reaped now if it has.
    fn reaped(&mut self) -> bool;
}

/// A driven build's process, which leads its process group.
struct Spawned {
    child: std::process::Child,
    group: i32,
}

impl Process for Spawned {
    fn signal(&mut self) {
        if let Some(pid) = rustix::process::Pid::from_raw(self.group) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }

    fn reaped(&mut self) -> bool {
        // An error is a process reaped elsewhere: gone too.
        !matches!(self.child.try_wait(), Ok(None))
    }
}

/// A process the host signals and reaps under one lock, reaped only through what owns it: once
/// it's reaped, its group's number may be another's, so it's never signalled after.
struct Lifecycle<P> {
    process: Mutex<Option<P>>,
}

impl<P: Process> Lifecycle<P> {
    fn new(p: P) -> Lifecycle<P> {
        Lifecycle { process: Mutex::new(Some(p)) }
    }

    /// Kills its group, unless it's reaped.
    fn signal(&self) {
        if let Some(p) = locked(&self.process).as_mut() {
            p.signal();
        }
    }

    /// Kills its group and reaps it: whether it was reaped within `grace`.
    fn reap(&self, grace: Duration) -> bool {
        let since = Instant::now();
        let mut signalled = false;
        loop {
            {
                let mut held = locked(&self.process);
                let Some(p) = held.as_mut() else { return true };
                if !signalled {
                    p.signal();
                    signalled = true;
                }
                if p.reaped() {
                    *held = None;
                    return true;
                }
            }
            if since.elapsed() >= grace {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }
}

/// A build's code, once it's compiled (or why it wasn't): what a load waits for while another
/// load of the build compiles it.
type Compiling = OnceLock<Result<Arc<Vec<u8>>, String>>;

/// What the run's builds' code is: each build's, by its WASM's SHA-256, compiled at most once,
/// and the code cache, if the run has one.
#[derive(Default)]
pub(crate) struct Codes {
    kept: Mutex<HashMap<[u8; 32], Arc<Compiling>>>,
    cache: Option<crate::code::Cache>,
}

impl Codes {
    /// The run's code, kept in `cache` from one run to the next if it's given.
    pub(crate) fn new(cache: Option<crate::code::Cache>) -> Codes {
        Codes { kept: Mutex::default(), cache }
    }

    /// The code of the WASM whose SHA-256 is `sha`, if the run has it or the cache does, and
    /// where it came from.
    fn known(&self, sha: &[u8; 32]) -> Option<(Arc<Vec<u8>>, u8)> {
        let cell = locked(&self.kept).get(sha).cloned();
        if let Some(Ok(code)) = cell.as_ref().and_then(|c| c.get()) {
            return Some((code.clone(), REUSED));
        }
        if cell.is_some() {
            // Being compiled, or failed: the WASM comes, and waits for that compile.
            return None;
        }
        let engine = crate::program::driven_engine();
        let bytes = Arc::new(self.cache.as_ref()?.get(engine, sha)?);
        let cell = locked(&self.kept).entry(*sha).or_default().clone();
        let _ = cell.set(Ok(bytes.clone()));
        Some((bytes, CACHED))
    }

    /// The code of `wasm`, compiled now unless the run has compiled it (or another load is
    /// compiling it, which this waits for), and where it came from.
    fn compile(&self, wasm: &[u8]) -> Result<(Arc<Vec<u8>>, u8), String> {
        let sha = crate::code::sha256(wasm);
        let cell = locked(&self.kept).entry(sha).or_default().clone();
        let mut from = REUSED;
        let code = cell.get_or_init(|| {
            from = COMPILED;
            let engine = crate::program::driven_engine();
            let bytes = crate::code::compile(engine, wasm)?;
            if let Some(cache) = &self.cache {
                cache.put(engine, &sha, &bytes);
            }
            Ok(Arc::new(bytes))
        });
        code.clone().map(|c| (c, from))
    }
}

/// A request's deadline: the `f64` of seconds next in `b` (0: [`DEADLINE`]).
fn deadline(b: &mut Body) -> Result<Duration, String> {
    super::http::deadline(f64::from_bits(b.u64()?), DEADLINE)
}

fn failed(why: String) -> Work {
    Work::Now(Answer::Bytes([&[1u8][..], why.as_bytes()].concat()))
}

/// A load's request, read: how to load it, its storage, its deadline.
fn load_request(b: &mut Body) -> Result<(Load, String, Duration), String> {
    let load = Load::parse(b)?;
    let storage = b.str()?;
    Ok((load, storage, deadline(b)?))
}

/// How request `name` of the drive service, with `b`, is carried out.
pub(crate) fn route(name: &str, b: &mut Body) -> Work {
    match task(name, b, Instant::now()) {
        Ok(task) => Work::Drive(task),
        Err(why) => failed(why),
    }
}

/// The work of request `name`, with `b`, which the host took at `entered`.
fn task(name: &str, b: &mut Body, entered: Instant) -> Result<Task, String> {
    let task: Task = match name {
        "load" => {
            let (load, storage, deadline) = load_request(b)?;
            b.end()?;
            Box::new(move |s, request| {
                super::services::answer(loaded(s, request, load, storage, entered + deadline))
            })
        }
        "run" => {
            let (load, storage, load_deadline) = load_request(b)?;
            let steps = wire::parse_steps(b)?;
            let deadline = deadline(b)?;
            b.end()?;
            Box::new(move |s, request| {
                let due = entered + load_deadline;
                super::services::answer(run(s, request, load, storage, due, steps, deadline))
            })
        }
        "close" => {
            let handle = b.u32()?;
            b.end()?;
            Box::new(move |s, _| closed(s, handle))
        }
        "answer" | "fail" => {
            let (handle, post) = (b.u32()?, b.u32()?);
            let answer = if name == "answer" { Ok(b.data()?) } else { Err(b.str()?) };
            let deadline = deadline(b)?;
            b.end()?;
            Box::new(move |s, request| {
                let due = entered + deadline;
                super::services::answer(answered(s, request, handle, post, answer, due, deadline))
            })
        }
        "steps" | "posts" => {
            let handle = b.u32()?;
            let (kind, bytes) = if name == "steps" {
                let mut out = Out::default();
                wire::write_steps(&mut out, &wire::parse_steps(b)?);
                (wire::STEPS, out.0)
            } else {
                (wire::POSTS, Vec::new())
            };
            let deadline = deadline(b)?;
            b.end()?;
            let what = if name == "steps" { "steps" } else { "posts" };
            Box::new(move |s, request| {
                let due = entered + deadline;
                let done = operate(s, request, handle, kind, bytes, what, due, deadline);
                super::services::answer(done)
            })
        }
        _ => return Err(format!("the drive service has no `{name}`")),
    };
    Ok(task)
}

/// The handle `handle`'s build.
fn find(s: &Services, handle: u32) -> Result<Arc<Driven>, String> {
    s.drives()
        .held
        .get(&handle)
        .cloned()
        .ok_or_else(|| format!("the tool holds no driven build {handle}"))
}

/// Why a request that ran past its deadline failed.
fn past_deadline(what: &str, deadline: Duration) -> String {
    format!(
        "`{what}` ran past its deadline of {} s, and the host stopped the build",
        deadline.as_secs_f64()
    )
}

/// Sends `driven`'s process command `kind` with `bytes`, for `request`, and waits for its
/// answer until `due`: the answer, or why it didn't come. Past `due`, or when the tool ends,
/// the build's process group is killed, and the build can't be observed again. Blobs reserved
/// for the request that the answer doesn't name are released.
#[allow(clippy::too_many_arguments)]
fn exchange(
    s: &Services,
    driven: &Driven,
    handle: u32,
    request: u32,
    kind: u8,
    bytes: &[u8],
    due: Instant,
    what: &str,
    deadline: Duration,
) -> Result<Vec<u8>, String> {
    driven.unobservable(handle)?;
    let answers = locked(&driven.answers);
    *locked(&driven.current) = Some(Current { request, reserved: HashMap::new() });
    let sent = wire::write_frame(&mut *locked(&driven.to), kind, &[bytes]);
    let got = match sent {
        Err(_) => Err(None),
        Ok(()) => loop {
            match answers.recv_timeout(POLL) {
                Ok(answer) => break Ok(answer),
                Err(mpsc::RecvTimeoutError::Disconnected) => break Err(None),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if s.closing.load(Ordering::Acquire) {
                killed(s, driven, handle, "the tool ended");
                break Err(Some(format!("the tool ended during `{what}`")));
            }
            if Instant::now() >= due {
                let why = format!("`{what}` ran past its deadline, and the host stopped it");
                killed(s, driven, handle, &why);
                break Err(Some(past_deadline(what, deadline)));
            }
        },
    };
    let current = locked(&driven.current).take();
    match got {
        Ok(Ok(answer)) => {
            // What was reserved and never filled isn't the answer's: it goes back now.
            let unfilled = current.into_iter().flat_map(|c| c.reserved).filter(|(_, (_, f))| !f);
            let mut blobs = s.blobs();
            for (id, _) in unfilled {
                let _ = blobs.release(id);
            }
            Ok(answer)
        }
        Ok(Err(why)) => Err(why),
        Err(Some(why)) => Err(why),
        Err(None) => {
            // The process ended, or was killed, on its own or by `close`.
            let why = locked(&driven.stopped).clone();
            killed(s, driven, handle, "its process ended");
            Err(match why {
                Some(why) => format!("the driven build {handle} ended during `{what}`: {why}"),
                None => format!("the driven build {handle}'s process ended during `{what}`"),
            })
        }
    }
}

/// Kills `driven`'s process group, saying `why`, and reaps its process: one that isn't reaped
/// within the grace makes the run the host's failure.
fn killed(s: &Services, driven: &Driven, handle: u32, why: &str) {
    if !driven.kill(why, s.grace) {
        let _ = s.stuck.set(format!("the driven build {handle}'s process didn't end when killed"));
    }
}

/// Reads `driven`'s process's frames until they end: its asks answered (blobs reserved under
/// the request it works for, code given), each blob's bytes put in the blob reserved for them,
/// and each command's answer passed on through `answers`. A frame that breaks the protocol kills
/// the process.
fn read_answers(
    services: Weak<Services>,
    driven: Weak<Driven>,
    mut from: File,
    answers: mpsc::Sender<Result<Vec<u8>, String>>,
) {
    let broken = |d: &Driven, why: &str| d.stop(&format!("its process broke the protocol: {why}"));
    loop {
        let Ok(Some((kind, len))) = wire::read_head(&mut from) else { return };
        let (Some(s), Some(d)) = (services.upgrade(), driven.upgrade()) else { return };
        match kind {
            wire::ASK => {
                if !(5..=MOST_WASM + 5).contains(&len) {
                    return broken(&d, "an ask's size");
                }
                let Ok(bytes) = wire::read_bytes(&mut from, len) else { return };
                let id = u32::from_le_bytes(bytes[..4].try_into().expect("four bytes"));
                let (status, reply) = ask(&s, &d, bytes[4], &bytes[5..]);
                if d.reply(id, status, &reply).is_err() {
                    return;
                }
            }
            wire::BLOB => {
                let mut id = [0u8; 4];
                if len < 4 || std::io::Read::read_exact(&mut from, &mut id).is_err() {
                    return broken(&d, "a blob's handle");
                }
                let id = u32::from_le_bytes(id);
                let size = (len - 4) as u64;
                // Known, and no larger than reserved, before a byte of it is read.
                let room = locked(&d.current)
                    .as_ref()
                    .and_then(|c| c.reserved.get(&id).filter(|(_, filled)| !filled).map(|r| r.0));
                if room.is_none_or(|room| size > room) {
                    return broken(&d, "a blob that wasn't reserved, or past what was");
                }
                std::thread::sleep(s.drive_hold);
                let Ok(bytes) = wire::read_bytes(&mut from, len - 4) else { return };
                if let Err(why) = s.blobs().fill(id, bytes) {
                    return broken(&d, &why);
                }
                if let Some(c) = locked(&d.current).as_mut()
                    && let Some(r) = c.reserved.get_mut(&id)
                {
                    r.1 = true;
                }
            }
            wire::DONE => {
                if len == 0 || len > wire::MOST_FRAME {
                    return broken(&d, "an answer's size");
                }
                let Ok(bytes) = wire::read_bytes(&mut from, len) else { return };
                let answer = match bytes[0] {
                    0 => Ok(bytes[1..].to_vec()),
                    _ => Err(String::from_utf8_lossy(&bytes[1..]).into_owned()),
                };
                if answers.send(answer).is_err() {
                    return;
                }
            }
            _ => return broken(&d, "a frame of no kind it sends"),
        }
    }
}

/// The host's answer to its build's process's ask `what`, with `bytes`: a status and bytes.
fn ask(s: &Services, d: &Driven, what: u8, bytes: &[u8]) -> (u8, Arc<Vec<u8>>) {
    let refused = |why: String| (wire::REFUSED, Arc::new(why.into_bytes()));
    match what {
        wire::ASK_RESERVE => {
            let Ok(size) = bytes.try_into().map(u64::from_le_bytes) else {
                return refused("a reservation's size".into());
            };
            let mut current = locked(&d.current);
            let Some(c) = current.as_mut() else {
                return refused("no request is running".into());
            };
            match s.blobs().create_reserved(c.request, size) {
                Ok(id) => {
                    c.reserved.insert(id, (size, false));
                    (wire::GIVEN, Arc::new(id.to_le_bytes().to_vec()))
                }
                Err(why) => refused(why),
            }
        }
        wire::ASK_CODE => {
            let Ok(sha) = <[u8; 32]>::try_from(bytes) else {
                return refused("a code's SHA-256".into());
            };
            match s.codes.known(&sha) {
                Some((code, from)) => {
                    locked(&d.code).get_or_insert(from);
                    (wire::GIVEN, code)
                }
                None => (wire::SEND_WASM, Arc::default()),
            }
        }
        wire::ASK_WASM => {
            // A test's slow compile.
            std::thread::sleep(s.compile_hold);
            match s.codes.compile(bytes) {
                Ok((code, from)) => {
                    locked(&d.code).get_or_insert(from);
                    (wire::GIVEN, code)
                }
                Err(why) => refused(why),
            }
        }
        k => refused(format!("{k} isn't an ask")),
    }
}

/// Hands the pipes' and directories' ends `fds` to the process as its fds 3 up, in order, as
/// [`child`] takes them.
#[cfg(unix)]
#[allow(unsafe_code)]
fn hand_fds(c: &mut std::process::Command, fds: [std::os::fd::RawFd; 5]) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure runs in the child between `fork` and `exec`, and calls only `fcntl`
    // and `dup2`, which are async-signal-safe; it allocates nothing and touches no lock. Each
    // end is first copied above 10 (closed at `exec`), so no `dup2` can overwrite another's
    // source; `dup2` gives fds 3 to 7 without close-on-exec.
    unsafe {
        c.pre_exec(move || {
            let mut high = [0; 5];
            for (h, fd) in high.iter_mut().zip(fds) {
                *h = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10);
                if *h < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            for (h, to) in high.into_iter().zip(child::FDS) {
                if libc::dup2(h, to) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

/// Loads `load` for `request` by `due`: its handle, and its answer's bytes as std's `Loaded`
/// reads them.
fn loaded(
    s: &Arc<Services>,
    request: u32,
    load: Load,
    storage: String,
    due: Instant,
) -> Result<Vec<u8>, String> {
    let started = Instant::now();
    let (handle, driven) = start(s, request, load, storage, due)?;
    let mut out = Out::default();
    out.u32(handle).f64(started.elapsed().as_secs_f64());
    out.u8(locked(&driven.code).unwrap_or(COMPILED));
    Ok(out.0)
}

/// Starts `load`'s process for `request`, registered under the request, and loads the build in
/// it by `due`: its handle and the build.
fn start(
    s: &Arc<Services>,
    request: u32,
    load: Load,
    storage: String,
    due: Instant,
) -> Result<(u32, Arc<Driven>), String> {
    let what = format!("load {}", load.path);
    let deadline = due.saturating_duration_since(Instant::now());
    if deadline.is_zero() {
        return Err(format!("`{what}` didn't start within its deadline"));
    }
    let Some(runner) = s.caps.runner() else {
        return Err("a driven build runs in a process of its own, which `wrela tool` starts, and nothing here runs one".into());
    };
    // Opened through the grants, once, now: the process gets these directories alone.
    let (_, build) = s.caps.open_dir(&load.path)?;
    let root = s.caps.root_handle(&load.path)?;
    let handle = {
        let mut d = s.drives();
        d.next += 1;
        d.next
    };
    // The build's storage: a write root's directory the tool names, which it keeps, or a
    // directory of the host's, removed with the build.
    let (owned, storage) = if storage.is_empty() {
        let dir = s.work_dir()?.join(format!("drive-{handle}"));
        std::fs::create_dir_all(&dir).map_err(|e| format!("the build's storage: {e}"))?;
        let opened = cap_std::fs::Dir::open_ambient_dir(&dir, cap_std::ambient_authority())
            .map_err(|e| format!("the build's storage: {e}"))?;
        (Some(dir), opened)
    } else {
        let (_, dir) = s
            .caps
            .open_dir_in(&storage, true)
            .map_err(|why| format!("the build's storage: {why}"))?;
        (None, dir)
    };
    // The pipes are made, and the process started, while no other process starts
    // (`wrela_chrome::spawning`): only the build's process holds their other ends, so its
    // commands end when this host does.
    let spawning = wrela_chrome::spawning();
    let (child_reads, to) = std::io::pipe().map_err(|e| format!("a pipe: {e}"))?;
    let (from, child_writes) = std::io::pipe().map_err(|e| format!("a pipe: {e}"))?;
    let mut command = std::process::Command::new(runner);
    command.arg("tool").arg("--drive").arg(std::process::id().to_string());
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        hand_fds(
            &mut command,
            [
                child_reads.as_raw_fd(),
                child_writes.as_raw_fd(),
                build.as_raw_fd(),
                root.as_raw_fd(),
                storage.as_raw_fd(),
            ],
        );
    }
    let spawned = command.spawn();
    drop(spawning);
    // The process's ends and directories are its own now: ours close, so its end ends the pipe.
    drop((child_reads, child_writes, build, root, storage));
    let child = match spawned {
        Ok(c) => c,
        Err(e) => {
            if let Some(dir) = &owned {
                let _ = std::fs::remove_dir_all(dir);
            }
            return Err(format!("can't start the build's process: {e}"));
        }
    };
    let group = child.id() as i32;
    // Recorded for a supervisor before the process gets its first command: until then it only
    // waits, and it ends with this host.
    if let Err(why) = super::run::record(s, group) {
        Lifecycle::new(Spawned { child, group }).reap(s.grace);
        if let Some(dir) = &owned {
            let _ = std::fs::remove_dir_all(dir);
        }
        return Err(why);
    }
    let (sent, answers) = mpsc::channel();
    let driven = Arc::new(Driven {
        process: Lifecycle::new(Spawned { child, group }),
        to: Mutex::new(File::from(std::os::fd::OwnedFd::from(to))),
        answers: Mutex::new(answers),
        reader: Mutex::new(None),
        current: Mutex::new(None),
        busy: AtomicBool::new(true),
        stopped: Mutex::new(None),
        code: Mutex::new(None),
        storage: owned,
    });
    // Registered under its request before any answer names it.
    {
        let mut d = s.drives();
        d.held.insert(handle, driven.clone());
        d.unpublished.entry(request).or_default().push(handle);
    }
    let (services, weak) = (Arc::downgrade(s), Arc::downgrade(&driven));
    let from = File::from(std::os::fd::OwnedFd::from(from));
    let reader = std::thread::Builder::new()
        .name(format!("wrela-drive-{handle}"))
        .spawn(move || read_answers(services, weak, from, sent))
        .map_err(|e| format!("can't start the thread that reads the build's process: {e}"))?;
    *locked(&driven.reader) = Some(reader);
    let mut out = Out::default();
    load.write(&mut out);
    let done = exchange(s, &driven, handle, request, wire::LOAD, &out.0, due, &what, deadline);
    driven.busy.store(false, Ordering::Release);
    // A failed load's build is closed by the worker, which rolls the request back.
    done.map(|_| (handle, driven))
}

/// Carries out command `kind` with `bytes` (`what`, as a failure names it) on `handle`'s build
/// for `request` by `due`: the process's answer.
#[allow(clippy::too_many_arguments)]
fn operate(
    s: &Arc<Services>,
    request: u32,
    handle: u32,
    kind: u8,
    bytes: Vec<u8>,
    what: &str,
    due: Instant,
    deadline: Duration,
) -> Result<Vec<u8>, String> {
    if Instant::now() >= due {
        return Err(format!("`{what}` didn't start within its deadline"));
    }
    let driven = find(s, handle)?;
    if driven.busy.swap(true, Ordering::AcqRel) {
        return Err(format!("the driven build {handle} is busy with another request"));
    }
    let done = exchange(s, &driven, handle, request, kind, &bytes, due, what, deadline);
    driven.busy.store(false, Ordering::Release);
    done
}

/// Answers post `post` of `handle`'s build with `answer` (a blob's bytes read now: the build's
/// process has no blobs), for `request` by `due`.
fn answered(
    s: &Arc<Services>,
    request: u32,
    handle: u32,
    post: u32,
    answer: Result<Data, String>,
    due: Instant,
    deadline: Duration,
) -> Result<Vec<u8>, String> {
    let mut out = Out::default();
    out.u32(post);
    match answer {
        Ok(Data::Bytes(bytes)) => {
            out.u8(0).0.extend_from_slice(&bytes);
        }
        Ok(Data::Blob(id)) => {
            let bytes = s.blobs().get(id)?;
            out.u8(0).0.extend_from_slice(&bytes);
        }
        Err(why) => {
            out.u8(1).0.extend_from_slice(why.as_bytes());
        }
    }
    operate(s, request, handle, wire::ANSWER, out.0, "answer", due, deadline)
}

/// `run`: loads `load` by `load_due`, runs `steps` within `deadline` of the load's end, and
/// closes the build: what the steps saw.
fn run(
    s: &Arc<Services>,
    request: u32,
    load: Load,
    storage: String,
    load_due: Instant,
    steps: Vec<Step>,
    deadline: Duration,
) -> Result<Vec<u8>, String> {
    let (handle, driven) = start(s, request, load, storage, load_due)?;
    let mut out = Out::default();
    wire::write_steps(&mut out, &steps);
    let due = Instant::now() + deadline;
    let seen = exchange(s, &driven, handle, request, wire::STEPS, &out.0, due, "steps", deadline);
    // No answer names the build: it closes now, whatever the steps gave.
    let rolled = s.drives().rollback(request);
    for d in rolled {
        if !d.close(s.grace, &s.strays) {
            let _ =
                s.stuck.set(format!("the driven build {handle} didn't close within {:?}", s.grace));
        }
    }
    seen
}

/// Closes `handle`'s build.
fn closed(s: &Services, handle: u32) -> Answer {
    let driven = s.drives().held.remove(&handle);
    let Some(driven) = driven else {
        return super::services::answer(Err(format!("the tool holds no driven build {handle}")));
    };
    if !driven.close(s.grace, &s.strays) {
        let _ = s.stuck.set(format!("the driven build {handle} didn't close within {:?}", s.grace));
        return super::services::answer(Err(format!("the driven build {handle} didn't close")));
    }
    super::services::answer(Ok(Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process whose signals and reaping are recorded, reaped at its `after`th look.
    struct Recorded {
        log: Arc<Mutex<Vec<&'static str>>>,
        looks: u32,
        after: u32,
    }

    impl Process for Recorded {
        fn signal(&mut self) {
            locked(&self.log).push("signal");
        }

        fn reaped(&mut self) -> bool {
            self.looks += 1;
            let done = self.looks >= self.after;
            if done {
                locked(&self.log).push("reaped");
            }
            done
        }
    }

    /// A real process signalled through `Spawned` is still its owner's to reap: the owner's wait
    /// reports the kill, not a process reaped elsewhere (ECHILD). This guards the Linux reap: a
    /// group kill that also reaps (`run::kill_group`, through `waitpgid`) would leave the owner's
    /// pid free for another process, which a later signal could reach. It runs on macOS too.
    #[cfg(unix)]
    #[test]
    fn a_signalled_process_is_its_owners_to_reap() {
        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let child = command.spawn().expect("sleep");
        let group = child.id() as i32;
        let mut spawned = Spawned { child, group };
        spawned.signal();
        let status = spawned.child.wait().expect("the owner reaps it, not something else");
        assert_eq!(std::os::unix::process::ExitStatusExt::signal(&status), Some(9), "{status}");
    }

    /// Closes and the tool's end at once: the process is signalled, then reaped once, and never
    /// signalled after it's reaped, whoever asks.
    #[test]
    fn a_reaped_process_is_never_signalled() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let life = Arc::new(Lifecycle::new(Recorded { log: log.clone(), looks: 0, after: 3 }));
        let signals = {
            let life = life.clone();
            std::thread::spawn(move || {
                for _ in 0..2000 {
                    life.signal();
                }
            })
        };
        let closes: Vec<_> = (0..2)
            .map(|_| {
                let life = life.clone();
                std::thread::spawn(move || life.reap(Duration::from_secs(5)))
            })
            .collect();
        for c in closes {
            assert!(c.join().expect("a close"), "reaped");
        }
        signals.join().expect("the signals");
        life.signal();
        let log = locked(&log).clone();
        let reaped: Vec<usize> =
            log.iter().enumerate().filter(|(_, e)| **e == "reaped").map(|(i, _)| i).collect();
        assert_eq!(reaped.len(), 1, "reaped once: {log:?}");
        assert!(log[reaped[0]..].iter().all(|e| *e != "signal"), "a signal after the reap");
        assert_eq!(log.first(), Some(&"signal"), "signalled before it's reaped");
    }
}
