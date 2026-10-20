//! `std::tool::drive` (language.md §15.5): a build loaded on the native host, beside the tool,
//! and run as the tool says: `init`, frames and ticks with a `std::input::Script`, the screen
//! captured, the state's hash, what it printed and timed, its other exports called. It's the
//! native host's own `Host` (with a GPU) or `CpuHost`, not another.
//!
//! **A driven build is a resource of its request.** `load` makes it, and it's registered under
//! the request before the answer that names it reaches the tool: until the tool reads that
//! answer it's the request's, and if the request fails, or the tool ends first, the host closes
//! it. The host closes every driven build when the tool ends.
//!
//! **Each driven build has a thread of its own**, which owns its host and carries out its
//! operations in order, one at a time (another while one runs fails: the build is busy). The
//! worker that waits for an operation looks for its deadline (from when the host took the
//! request) and the tool's end every few milliseconds.
//!
//! **Driven WASM can be stopped.** The build is compiled for an engine with epoch interruption,
//! and every call into it, on any of its threads, has the operation's deadline: past it, or when
//! the tool ends, the call traps within about 10 ms. A build whose call was stopped, or trapped,
//! runs nothing more (its state is a call's middle); its screen, hash, lines and phases can
//! still be read. A call that waits and so isn't stopped (a job's join on a job that never
//! ends) makes the run the host's failure once the grace after the deadline has passed, as an
//! HTTP client that won't return does.
//!
//! **No compiled code is kept or read**: each `load` compiles the build's WASM, which the host
//! reads through the grants, as it reads the build's other files. The build's storage is a
//! directory of the host's own, removed when the build closes.

use super::caps::MOST_BYTES;
use super::services::{Answer, Body, Data, Services, Task, Work};
use super::toolchain::Out;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use wrela_abi::input::Scripted;

/// An operation's deadline unless the request asks for another.
const DEADLINE: Duration = Duration::from_secs(60);

/// How often the worker looks at the operation it waits for.
const POLL: Duration = Duration::from_millis(10);

/// The most lines `logs` gives at once, and the most bytes of them: the rest wait for the next
/// `logs`. A line longer than an answer holds is cut, with a note.
const MOST_LINES: usize = 4096;

/// The most frames or ticks one request runs.
const MOST_STEPS: u32 = 1 << 20;

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

    /// Stops every running call of every build: the tool has ended.
    pub(crate) fn stop_all(&self) {
        for d in self.held.values() {
            d.stop.stop();
        }
    }
}

/// What a driven build's thread is sent: an operation, and where its reply goes.
type Sent = (Command, mpsc::Sender<Result<Reply, String>>);

/// A driven build: what sends its thread operations, what stops its calls, whether an operation
/// is running, its thread, and its screen's size.
pub(crate) struct Driven {
    commands: Mutex<Option<mpsc::Sender<Sent>>>,
    stop: crate::Stop,
    busy: AtomicBool,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    size: (u32, u32),
}

impl Driven {
    /// Closes it: its thread is told to end, and its host goes with it. Whether the thread
    /// ended within `grace` (if not, it's left, and the run isn't clean).
    pub(crate) fn close(&self, grace: Duration) -> bool {
        self.stop.stop();
        self.commands.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let Some(thread) =
            self.thread.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
        else {
            return true;
        };
        let since = Instant::now();
        while !thread.is_finished() {
            if since.elapsed() >= grace {
                return false;
            }
            std::thread::sleep(POLL);
        }
        let _ = thread.join();
        true
    }
}

/// How a build is loaded: with a GPU or on the CPU alone, its screen's size, its frames' rate,
/// how many threads run its parallel jobs (0: one per core), how its GPU work is timed, and how
/// long each helper holds a job's result back (microseconds: a test's slow jobs).
#[derive(Clone, Debug)]
#[cfg_attr(not(feature = "gpu"), allow(dead_code))]
struct Load {
    path: String,
    gpu: bool,
    width: u32,
    height: u32,
    fps: f64,
    workers: u32,
    timing: crate::Timing,
    hold: u32,
}

/// An operation of a driven build's.
enum Command {
    Init,
    Frames(u32, Vec<Scripted>),
    Ticks(u32, Vec<Scripted>),
    Capture,
    Hash,
    Logs,
    Phases,
    Call(String, Vec<crate::Value>),
    Posts,
    Answer(u32, Result<Data, String>),
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::Init => "init",
            Command::Frames(..) => "frames",
            Command::Ticks(..) => "ticks",
            Command::Capture => "capture",
            Command::Hash => "hash",
            Command::Logs => "logs",
            Command::Phases => "phases",
            Command::Call(..) => "call",
            Command::Posts => "posts",
            Command::Answer(..) => "answer",
        }
    }
}

/// What an operation gives.
#[cfg_attr(not(feature = "gpu"), allow(dead_code))]
enum Reply {
    Loaded(f64),
    Ran(Ran),
    Screen(u32, u32, Vec<u8>),
    Hash(u64),
    Logs(Vec<String>, usize),
    Phases(Vec<crate::Phase>),
    Posts(Vec<crate::QueuedPost>),
    Answered,
}

/// What running gave: when it started (seconds since the build loaded) and how long it took,
/// each frame's or tick's start and end, an export's results, and its trap, if it trapped.
#[derive(Default)]
struct Ran {
    started: f64,
    elapsed: f64,
    steps: Vec<(f64, f64)>,
    results: Vec<crate::Value>,
    trap: Option<Trap>,
}

/// A trap: the host's message, the panic's, where in the source, and in what: `init` (0), a
/// frame (1), a tick (2) or a call (3), with the frame's or tick's number.
struct Trap {
    message: String,
    panic: String,
    at: String,
    step: u8,
    number: u32,
}

/// A driven build's host, on its thread.
enum Hosted {
    Cpu(crate::CpuHost),
    #[cfg(feature = "gpu")]
    Gpu(Box<crate::Host>),
}

/// A driven build's thread's state.
struct Actor {
    host: Hosted,
    load: Load,
    origin: Instant,
    inited: bool,
    next_frame: u32,
    /// Why it runs nothing more: it trapped, or a call was stopped.
    ended: Option<String>,
    /// The posts the program made that the tool hasn't been given yet.
    posts: std::collections::VecDeque<crate::QueuedPost>,
    /// The lines the program printed that the tool hasn't been given yet.
    lines: std::collections::VecDeque<String>,
}

impl Actor {
    fn failure(&self) -> Option<crate::Failure> {
        match &self.host {
            Hosted::Cpu(h) => h.last_failure().cloned(),
            #[cfg(feature = "gpu")]
            Hosted::Gpu(h) => h.last_failure().cloned(),
        }
    }

    /// A trap of `e` in `step` number `number`, or in the tick the host says it happened in:
    /// a lockstep frame runs ticks too, and the ticker numbers them, not this.
    fn trap(&mut self, e: &crate::Error, step: u8, number: u32) -> Trap {
        let failure = self.failure();
        let message = e.to_string();
        self.ended = Some(format!("it trapped: {message}"));
        let (step, number) = match failure.as_ref().and_then(|f| f.tick) {
            Some(k) => (2, k),
            None => (step, number),
        };
        Trap {
            message,
            panic: failure.as_ref().and_then(|f| f.panic.clone()).unwrap_or_default(),
            at: failure.and_then(|f| f.at).unwrap_or_default(),
            step,
            number,
        }
    }

    /// Its host, if it's a CPU host.
    fn cpu(&mut self) -> Option<&mut crate::CpuHost> {
        match &mut self.host {
            Hosted::Cpu(host) => Some(host),
            #[cfg(feature = "gpu")]
            Hosted::Gpu(_) => None,
        }
    }

    fn since(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }

    fn carry_out(&mut self, command: Command) -> Result<Reply, String> {
        let running = matches!(
            command,
            Command::Init | Command::Frames(..) | Command::Ticks(..) | Command::Call(..)
        );
        if running {
            if let Some(why) = &self.ended {
                return Err(format!("the build runs nothing more: {why}"));
            }
            let first = matches!(command, Command::Init);
            if first == self.inited {
                return Err(if first {
                    "`init` has run already".into()
                } else {
                    format!("`{}` runs after `init`", command.name())
                });
            }
        }
        let mut ran = Ran { started: self.since(), ..Ran::default() };
        let (w, h, fps) = (self.load.width, self.load.height, self.load.fps);
        match command {
            Command::Init => {
                let done = match &mut self.host {
                    Hosted::Cpu(host) => host.init(),
                    #[cfg(feature = "gpu")]
                    Hosted::Gpu(host) => host.init(),
                };
                self.inited = true;
                if let Err(e) = done {
                    ran.trap = Some(self.trap(&e, 0, 0));
                }
            }
            Command::Frames(n, script) => {
                for _ in 0..n {
                    let i = self.next_frame;
                    let start = self.since();
                    let done = match &mut self.host {
                        Hosted::Cpu(host) => host.lockstep_frame(i, fps, w, h, &script),
                        #[cfg(feature = "gpu")]
                        Hosted::Gpu(host) => host.lockstep_frame(i, fps, w, h, &script),
                    };
                    self.next_frame += 1;
                    if let Err(e) = done {
                        ran.trap = Some(self.trap(&e, 1, i));
                        break;
                    }
                    ran.steps.push((start, self.since()));
                }
            }
            Command::Ticks(n, script) => {
                if self.cpu().is_none() {
                    return Err("ticks alone run on a build loaded without the GPU".into());
                }
                for _ in 0..n {
                    let start = self.since();
                    let done = self.cpu().map_or(Ok(()), |host| host.scripted_tick(&script));
                    match done {
                        Ok(()) => {}
                        // No ticker, say: not a trap.
                        Err(crate::Error::Program(why)) => return Err(why),
                        Err(e) => {
                            // A tick's trap: the host numbers it.
                            ran.trap = Some(self.trap(&e, 2, 0));
                            break;
                        }
                    }
                    ran.steps.push((start, self.since()));
                }
            }
            Command::Call(export, args) => {
                let done = match &mut self.host {
                    Hosted::Cpu(host) => host.call_export(&export, &args),
                    #[cfg(feature = "gpu")]
                    Hosted::Gpu(host) => host.call_export(&export, &args),
                };
                match done {
                    Ok(values) => ran.results = values,
                    // An export that isn't there, or arguments of the wrong types, isn't a trap.
                    Err(crate::Error::Program(why)) => return Err(why),
                    Err(e) => ran.trap = Some(self.trap(&e, 3, 0)),
                }
            }
            Command::Capture => {
                return match &mut self.host {
                    Hosted::Cpu(_) => Err("a build loaded without the GPU has no screen".into()),
                    #[cfg(feature = "gpu")]
                    Hosted::Gpu(host) => {
                        let rgba = host.read_screen().map_err(|e| e.to_string())?;
                        Ok(Reply::Screen(w, h, rgba))
                    }
                };
            }
            Command::Hash => {
                return Ok(Reply::Hash(match &mut self.host {
                    Hosted::Cpu(host) => host.hash(),
                    #[cfg(feature = "gpu")]
                    Hosted::Gpu(host) => host.hash(),
                }));
            }
            Command::Logs => {
                let printed = match &mut self.host {
                    Hosted::Cpu(host) => host.take_logs(),
                    #[cfg(feature = "gpu")]
                    Hosted::Gpu(host) => host.take_logs(),
                };
                self.lines.extend(printed);
                // As many as an answer holds, the oldest first; the rest wait for the next.
                let mut given = Vec::new();
                let mut bytes = 0;
                while let Some(l) = self.lines.front() {
                    let size = l.len().min(MOST_BYTES / 2) + 4;
                    if given.len() == MOST_LINES || (!given.is_empty() && bytes + size > MOST_BYTES)
                    {
                        break;
                    }
                    bytes += size;
                    given.extend(self.lines.pop_front());
                }
                return Ok(Reply::Logs(given, self.lines.len()));
            }
            Command::Posts => {
                let made = match &mut self.host {
                    Hosted::Cpu(host) => host.take_posts(),
                    #[cfg(feature = "gpu")]
                    Hosted::Gpu(host) => host.take_posts(),
                };
                self.posts.extend(made);
                // As many as an answer holds, the oldest first; the rest wait for the next.
                let mut given = Vec::new();
                let mut bytes = 0;
                while let Some(p) = self.posts.front() {
                    let size = p.body.len() + p.url.len() + 16;
                    if !given.is_empty() && bytes + size > MOST_BYTES {
                        break;
                    }
                    bytes += size;
                    given.extend(self.posts.pop_front());
                }
                return Ok(Reply::Posts(given));
            }
            Command::Answer(request, data) => {
                let answer = match data {
                    Ok(Data::Bytes(bytes)) => Ok(bytes),
                    Ok(Data::Blob(_)) => {
                        return Err("an answer's blob is read before it's sent".into());
                    }
                    Err(why) => Err(why),
                };
                match &mut self.host {
                    Hosted::Cpu(host) => host.answer_post(request, answer),
                    #[cfg(feature = "gpu")]
                    Hosted::Gpu(host) => host.answer_post(request, answer),
                }?;
                return Ok(Reply::Answered);
            }
            Command::Phases => {
                let trace = match &mut self.host {
                    Hosted::Cpu(host) => host.trace(),
                    #[cfg(feature = "gpu")]
                    Hosted::Gpu(host) => host.trace(),
                };
                return Ok(Reply::Phases(trace.phases()));
            }
        }
        ran.elapsed = self.since() - ran.started;
        Ok(Reply::Ran(ran))
    }
}

/// The host for `load`, made on the build's own thread: its directory opened through the grants
/// once, now, and its files and its program's fetches read through that handle alone.
fn start(s: &Services, load: &Load, stop: crate::Stop, storage: PathBuf) -> Result<Hosted, String> {
    let (dir, handle) = s.caps.open_dir(&load.path)?;
    if load.gpu {
        #[cfg(feature = "gpu")]
        {
            let options = crate::Options {
                timing: load.timing,
                storage: Some(storage),
                workers: load.workers,
                quiet: true,
                defer_init: true,
                ..crate::Options::default()
            };
            let mut host = crate::Host::load_driven(handle, &dir, stop, &options)
                .map_err(|e| e.to_string())?;
            // Only if asked: the word is the jobs' protocol's, which a program without jobs may
            // keep other bytes in.
            if load.hold > 0 {
                host.hold_jobs(load.hold);
            }
            return Ok(Hosted::Gpu(Box::new(host)));
        }
        #[cfg(not(feature = "gpu"))]
        return Err("this tool host was built without the GPU".into());
    }
    let build = crate::CpuBuild::load_driven(handle, &dir, stop).map_err(|e| e.to_string())?;
    let host = build.instantiate_in(load.workers, Some(&storage)).map_err(|e| e.to_string())?;
    if load.hold > 0 {
        host.hold_jobs(load.hold);
    }
    Ok(Hosted::Cpu(host))
}

/// A request's deadline: the `f64` of seconds at its end (0: [`DEADLINE`]).
fn deadline(b: &mut Body) -> Result<Duration, String> {
    let d = super::http::deadline(f64::from_bits(b.u64()?), DEADLINE)?;
    b.end()?;
    Ok(d)
}

fn failed(why: String) -> Work {
    Work::Now(Answer::Bytes([&[1u8][..], why.as_bytes()].concat()))
}

/// A script as `std::input::Script::as_json` writes it, or none for empty text.
fn script(text: &str) -> Result<Vec<Scripted>, String> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    crate::parse_script(text).map_err(|e| format!("the script: {e}"))
}

/// How request `name` of the drive service, with `b`, is carried out.
pub(crate) fn route(name: &str, b: &mut Body) -> Work {
    let entered = Instant::now();
    if name == "load" {
        let parsed = (|| {
            let path = b.str()?;
            let gpu = b.u8()? != 0;
            let (width, height) = (b.u32()?, b.u32()?);
            let fps = f64::from_bits(b.u64()?);
            let workers = b.u32()?;
            let timing = match b.u8()? {
                0 => crate::Timing::Off,
                1 => crate::Timing::Span,
                2 => crate::Timing::Serial,
                k => return Err(format!("{k} isn't a timing mode")),
            };
            let hold = b.u32()?;
            if !(fps.is_finite() && fps > 0.0) {
                return Err(format!("frames' rate is more than 0, not {fps}"));
            }
            if width == 0 || height == 0 || width > 16384 || height > 16384 {
                return Err(format!("a screen of {width}×{height} isn't 1 to 16384 each way"));
            }
            if workers > wrela_abi::memory::MAX_WORKERS + 1 {
                return Err(format!("at most {} workers", wrela_abi::memory::MAX_WORKERS + 1));
            }
            let load = Load { path, gpu, width, height, fps, workers, timing, hold };
            Ok((load, deadline(b)?))
        })();
        return match parsed {
            Ok((load, deadline)) => {
                let task: Task =
                    Box::new(move |s, request| loaded(s, request, load, entered + deadline));
                Work::Drive(task)
            }
            Err(why) => failed(why),
        };
    }
    let parsed = (|| {
        let handle = b.u32()?;
        let command = match name {
            "init" => Command::Init,
            "frames" | "ticks" => {
                let n = b.u32()?;
                if n > MOST_STEPS {
                    return Err(format!("at most {MOST_STEPS} at once"));
                }
                let s = script(&b.str()?)?;
                if name == "frames" { Command::Frames(n, s) } else { Command::Ticks(n, s) }
            }
            "capture" => Command::Capture,
            "hash" => Command::Hash,
            "logs" => Command::Logs,
            "phases" => Command::Phases,
            "posts" => Command::Posts,
            "answer" => {
                let post = b.u32()?;
                Command::Answer(post, Ok(b.data()?))
            }
            "fail" => {
                let post = b.u32()?;
                Command::Answer(post, Err(b.str()?))
            }
            "call" => {
                let export = b.str()?;
                let n = b.u32()?;
                if n > 64 {
                    return Err("an export takes at most 64 arguments".into());
                }
                let mut args = Vec::new();
                for _ in 0..n {
                    let (kind, bits) = (b.u8()?, b.u64()?);
                    args.push(match kind {
                        0 => crate::Value::I32(bits as u32 as i32),
                        1 => crate::Value::I64(bits as i64),
                        2 => crate::Value::F32(f32::from_bits(bits as u32)),
                        3 => crate::Value::F64(f64::from_bits(bits)),
                        k => return Err(format!("{k} isn't a scalar's kind")),
                    });
                }
                Command::Call(export, args)
            }
            "close" => {
                b.end()?;
                return Ok((handle, None, Duration::ZERO));
            }
            _ => return Err(format!("the drive service has no `{name}`")),
        };
        Ok((handle, Some(command), deadline(b)?))
    })();
    match parsed {
        Ok((handle, None, _)) => {
            let task: Task = Box::new(move |s, _| closed(s, handle));
            Work::Drive(task)
        }
        Ok((handle, Some(command), deadline)) => {
            let task: Task = Box::new(move |s, request| {
                operate(s, request, handle, command, entered + deadline, deadline)
            });
            Work::Drive(task)
        }
        Err(why) => failed(why),
    }
}

/// The handle `handle`'s build.
fn find(s: &Services, handle: u32) -> Result<Arc<Driven>, String> {
    s.drives()
        .held
        .get(&handle)
        .cloned()
        .ok_or_else(|| format!("the tool holds no driven build {handle}"))
}

/// Waits for `reply` until `due`: what it gave, or why it didn't come. Past `due`, or when the
/// tool ends, `stop` stops the build's calls, and the reply gets the grace; if it hasn't come
/// then, the run is the host's failure.
fn wait(
    s: &Services,
    reply: &mpsc::Receiver<Result<Reply, String>>,
    stop: &crate::Stop,
    due: Instant,
    what: &str,
    deadline: Duration,
) -> Result<Reply, String> {
    let (why, grace) = loop {
        match reply.recv_timeout(POLL) {
            Ok(r) => return r,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!("the build's thread ended during `{what}`"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if s.closing.load(Ordering::Acquire) {
            // Half the grace: the pool waits for this worker the whole grace.
            break (format!("the tool ended during `{what}`"), s.grace / 2);
        }
        if Instant::now() >= due {
            let why = format!(
                "`{what}` ran past its deadline of {} s, and the host stopped the build",
                deadline.as_secs_f64()
            );
            break (why, s.grace);
        }
    };
    stop.stop();
    if reply.recv_timeout(grace).is_ok() {
        return Err(why);
    }
    let _ = s.stuck.set(format!("a driven build didn't stop within {grace:?} of `{what}`'s end"));
    Err(why)
}

/// Loads `load` for `request` by `due`: its handle, and how long loading took.
fn loaded(s: &Arc<Services>, request: u32, load: Load, due: Instant) -> Answer {
    let what = format!("load {}", load.path);
    let deadline = due.saturating_duration_since(Instant::now());
    if deadline.is_zero() {
        return super::services::answer(Err(format!("`{what}` didn't start within its deadline")));
    }
    let stop = match crate::Stop::new() {
        Ok(s) => s,
        Err(e) => return super::services::answer(Err(e.to_string())),
    };
    let handle = {
        let mut d = s.drives();
        d.next += 1;
        d.next
    };
    let storage = match s.work_dir() {
        Ok(dir) => dir.join(format!("drive-{handle}")),
        Err(why) => return super::services::answer(Err(why)),
    };
    let (commands, received) = mpsc::channel::<Sent>();
    let (sent, reply) = mpsc::channel();
    let size = (load.width, load.height);
    let (services, load2, stop2, dir) = (s.clone(), load, stop.clone(), storage);
    let spawned =
        std::thread::Builder::new().name(format!("wrela-drive-{handle}")).spawn(move || {
            let since = Instant::now();
            let _ = std::fs::create_dir_all(&dir);
            let host = start(&services, &load2, stop2, dir.clone());
            drop(services);
            let host = match host {
                Ok(h) => h,
                Err(why) => {
                    let _ = sent.send(Err(why));
                    let _ = std::fs::remove_dir_all(&dir);
                    return;
                }
            };
            let mut actor = Actor {
                host,
                load: load2,
                origin: Instant::now(),
                inited: false,
                next_frame: 0,
                ended: None,
                posts: std::collections::VecDeque::new(),
                lines: std::collections::VecDeque::new(),
            };
            if sent.send(Ok(Reply::Loaded(since.elapsed().as_secs_f64()))).is_err() {
                drop(actor);
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
            while let Ok((command, answer)) = received.recv() {
                let run = std::panic::AssertUnwindSafe(|| actor.carry_out(command));
                let result = std::panic::catch_unwind(run)
                    .unwrap_or_else(|_| Err("the native host failed (it panicked)".into()));
                let _ = answer.send(result);
            }
            drop(actor);
            let _ = std::fs::remove_dir_all(&dir);
        });
    let thread = match spawned {
        Ok(t) => t,
        Err(e) => {
            return super::services::answer(Err(format!("can't start the build's thread: {e}")));
        }
    };
    let driven = Arc::new(Driven {
        commands: Mutex::new(Some(commands)),
        stop: stop.clone(),
        busy: AtomicBool::new(true),
        thread: Mutex::new(Some(thread)),
        size,
    });
    // Registered under its request before any answer names it.
    {
        let mut d = s.drives();
        d.held.insert(handle, driven.clone());
        d.unpublished.entry(request).or_default().push(handle);
    }
    // Loading runs no WASM: past its deadline, it's the compile that's waited for.
    stop.set_deadline(Some(due));
    let result = wait(s, &reply, &stop, due, &what, deadline);
    stop.set_deadline(None);
    driven.busy.store(false, Ordering::Release);
    let answer = match result {
        Ok(Reply::Loaded(elapsed)) => {
            let mut out = Out::default();
            out.u32(handle).f64(elapsed);
            Ok(out.0)
        }
        Ok(_) => Err("the build's thread answered something else".into()),
        Err(why) => Err(why),
    };
    // A failed load's build is closed by the worker, which rolls the request back.
    super::services::answer(answer)
}

/// Carries out `command` on `handle`'s build for `request` by `due`.
fn operate(
    s: &Arc<Services>,
    request: u32,
    handle: u32,
    command: Command,
    due: Instant,
    deadline: Duration,
) -> Answer {
    let what = command.name();
    super::services::answer((|| {
        if Instant::now() >= due {
            return Err(format!("`{what}` didn't start within its deadline"));
        }
        let driven = find(s, handle)?;
        if driven.busy.swap(true, Ordering::AcqRel) {
            return Err(format!("the driven build {handle} is busy with another operation"));
        }
        struct Idle<'a>(&'a Driven);
        impl Drop for Idle<'_> {
            fn drop(&mut self) {
                self.0.stop.set_deadline(None);
                self.0.busy.store(false, Ordering::Release);
            }
        }
        let _idle = Idle(&driven);
        // An answer's blob is read now: the build's thread has no blobs.
        let command = match command {
            Command::Answer(post, Ok(Data::Blob(id))) => {
                Command::Answer(post, Ok(Data::Bytes(s.blobs().get(id)?.as_ref().clone())))
            }
            c => c,
        };
        // The screen's bytes and its blob are reserved before it's read back: a capture that
        // doesn't fit isn't read. The blob is the request's, so a failure gives them back.
        let mut screen = None;
        if matches!(command, Command::Capture) {
            let (w, h) = driven.size;
            let bytes = u64::from(w) * u64::from(h) * 4;
            let id = s
                .blobs()
                .create_reserved(request, bytes)
                .map_err(|why| format!("the screen's {bytes} bytes: {why}"))?;
            screen = Some(id);
        }
        let (sent, reply) = mpsc::channel();
        {
            let commands =
                driven.commands.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(commands) = commands.as_ref() else {
                return Err(format!("the driven build {handle} is closed"));
            };
            driven.stop.set_deadline(Some(due));
            commands
                .send((command, sent))
                .map_err(|_| format!("the driven build {handle}'s thread has ended"))?;
        }
        let reply = wait(s, &reply, &driven.stop, due, what, deadline)?;
        let mut out = Out::default();
        match reply {
            Reply::Ran(ran) => write_ran(&mut out, &ran, &driven.stop, due, what, deadline)?,
            Reply::Screen(w, h, rgba) => {
                let id = screen.ok_or("a screen nobody asked for")?;
                s.blobs().fill(id, rgba)?;
                out.u32(w).u32(h).u8(1).u32(id);
            }
            Reply::Hash(hash) => {
                out.u64(hash);
            }
            Reply::Logs(lines, waiting) => {
                out.u32(lines.len() as u32);
                for l in &lines {
                    out.str(&bounded(l));
                }
                out.u32(waiting as u32);
            }
            Reply::Phases(phases) => {
                out.u32(phases.len() as u32);
                for p in &phases {
                    out.str(&p.name).f64(p.seconds).u32(p.times);
                }
            }
            Reply::Posts(posts) => {
                out.u32(posts.len() as u32);
                for p in &posts {
                    out.u32(p.request).str(&p.url).u8(0).u32(p.body.len() as u32);
                    out.0.extend_from_slice(&p.body);
                }
            }
            Reply::Answered => {}
            Reply::Loaded(_) => return Err("the build's thread answered something else".into()),
        }
        Ok(out.0)
    })())
}

/// Writes `ran` as `drive::read_ran` reads it. A call that the deadline stopped (the trap is
/// an interrupt, and the deadline has passed) is the request's failure, not a trap.
fn write_ran(
    out: &mut Out,
    ran: &Ran,
    stop: &crate::Stop,
    due: Instant,
    what: &str,
    deadline: Duration,
) -> Result<(), String> {
    if ran.trap.as_ref().is_some_and(|t| t.message.contains("interrupt"))
        && (stop.is_stopped() || Instant::now() >= due)
    {
        return Err(format!(
            "`{what}` ran past its deadline of {} s, and the host stopped the build",
            deadline.as_secs_f64()
        ));
    }
    out.f64(ran.started).f64(ran.elapsed).u32(ran.steps.len() as u32);
    for (start, end) in &ran.steps {
        out.f64(*start).f64(*end);
    }
    out.u32(ran.results.len() as u32);
    for v in &ran.results {
        let (kind, bits) = match *v {
            crate::Value::I32(x) => (0, x as u32 as u64),
            crate::Value::I64(x) => (1, x as u64),
            crate::Value::F32(x) => (2, u64::from(x.to_bits())),
            crate::Value::F64(x) => (3, x.to_bits()),
        };
        out.u8(kind).u64(bits);
    }
    match &ran.trap {
        None => {
            out.u8(0);
        }
        Some(t) => {
            out.u8(1).str(&t.message).str(&t.panic).str(&t.at).u8(t.step).u32(t.number);
        }
    }
    Ok(())
}

/// Closes `handle`'s build.
fn closed(s: &Services, handle: u32) -> Answer {
    let driven = s.drives().held.remove(&handle);
    let Some(driven) = driven else {
        return super::services::answer(Err(format!("the tool holds no driven build {handle}")));
    };
    if !driven.close(s.grace) {
        let _ = s.stuck.set(format!("the driven build {handle} didn't close within {:?}", s.grace));
        return super::services::answer(Err(format!("the driven build {handle} didn't close")));
    }
    super::services::answer(Ok(Vec::new()))
}

/// `line`, at most half an answer's bytes of it (cut at a character), with a note if it was
/// cut.
fn bounded(line: &str) -> String {
    let most = MOST_BYTES / 2;
    if line.len() <= most {
        return line.to_string();
    }
    let mut end = most;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} more bytes)", &line[..end], line.len() - end)
}
