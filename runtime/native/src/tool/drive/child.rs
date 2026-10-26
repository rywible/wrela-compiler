//! A driven build's process (language.md §15.5's `drive`): `wrela tool --drive <parent>`, which
//! the tool host starts for each build it loads. It holds the native host's own `Host` (with a
//! GPU) or `CpuHost`, on the driven engine (`crate::program::driven_engine`), whose epoch nothing
//! moves: no call is interrupted, and the tool host stops the build by killing its process group.
//!
//! **What it gets.** File descriptors the tool host opened: 3, the tool host's commands; 4, its
//! answers; 5, the build's directory; 6, the root the build is in (the builds it hands over to
//! are opened through it, never leaving it); and 7, the build's storage. It opens nothing by a
//! path, and gets each build's code from the tool host, which compiles each build once in a run.
//!
//! **It ends with the tool host.** A thread of its own reads the commands: at their end (the
//! tool host has ended, however it ended) or at an error, it ends the process at once, whatever
//! the build is running (WASM in a loop, a join, a large answer half written). On Linux the
//! process also asks to be killed when its parent ends (`PR_SET_PDEATHSIG`).
//!
//! **One command at a time**, on the process's main thread, which owns the host: the load,
//! then steps, posts and answers to posts. While it runs steps, it asks the tool host to reserve
//! each blob's bytes before it reads them back or renders them, and keeps the answer's inline
//! part within [`wire::MOST_INLINE`] bytes as it collects it.
#![allow(unsafe_code)]

use super::wire::{self, Load, Step};
use crate::locked;
use crate::tool::caps::MOST_BYTES;
use crate::tool::services::Body;
use crate::tool::toolchain::{Out, bounded_to};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

/// The file descriptors the tool host gives the process.
pub(crate) const FDS: [i32; 5] = [3, 4, 5, 6, 7];

/// The most bytes of lines a build keeps during a request (as a program's output is bounded,
/// `std::tool::run`): the lines past it are dropped as they're printed, and counted.
const MOST_KEPT_LINE_BYTES: usize = 256 << 20;

/// The most commands one `Commands` step gives: the rest wait for the next.
const MOST_COMMANDS: usize = 65536;

/// The most buffers a `Buffers` step names: past it, the request fails.
#[cfg(feature = "gpu")]
const MOST_BUFFERS: usize = 65536;

/// The room an answer keeps for its counts and its trap: what its steps may fill is
/// [`wire::MOST_INLINE`] less this.
const KEPT_ROOM: usize = 64 << 10;

/// The most bytes of each of a trap's texts.
const MOST_TRAP_TEXT: usize = 16 << 10;

/// The bytes of one quantum of a voice's samples: its channels' samples, `f32`s.
const QUANTUM_BYTES: u64 = wrela_abi::AUDIO_QUANTUM as u64 * wrela_abi::AUDIO_CHANNELS as u64 * 4;

/// Ends the process now, running nothing more: no destructor, no handler, no flush.
fn exit_now() -> ! {
    // SAFETY: `_exit` ends the process at once; nothing runs after it.
    unsafe { libc::_exit(0) }
}

/// The open file descriptor `fd`, which the tool host gave the process, now the process's to
/// own, and closed at an `exec` from here on; or the process ends if it isn't open.
fn adopt(fd: i32) -> std::os::fd::OwnedFd {
    use std::os::fd::FromRawFd;
    // SAFETY: `fcntl` only reads the descriptor's flags; it's checked open before it's owned,
    // and nothing else in the process owns it: the tool host gave it to this process alone,
    // and no other code here names it.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            exit_now();
        }
        std::os::fd::OwnedFd::from_raw_fd(fd)
    }
}

/// Asks to be killed when the parent `parent` ends (Linux), and ends now if it already has.
fn die_with(parent: u32) {
    #[cfg(target_os = "linux")]
    // SAFETY: `prctl(PR_SET_PDEATHSIG, SIGKILL)` changes only this process's death signal.
    unsafe {
        let _ = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
    }
    if std::os::unix::process::parent_id() != parent {
        exit_now();
    }
}

/// A reply to an ask: its status and bytes.
type Reply = (u8, Vec<u8>);

/// The process's way to the tool host: its answers' writer, and the asks waiting for a reply,
/// by number.
struct Link {
    out: Mutex<File>,
    waiting: Mutex<HashMap<u32, mpsc::Sender<Reply>>>,
    next: AtomicU32,
}

impl Link {
    /// Sends a frame; a tool host that can't be written to has ended, and so does this.
    fn send(&self, kind: u8, parts: &[&[u8]]) {
        let mut out = locked(&self.out);
        if wire::write_frame(&mut *out, kind, parts).is_err() {
            exit_now();
        }
    }

    /// Asks the tool host `what`, with `parts`, and waits for its reply: its status and bytes.
    fn ask(&self, what: u8, parts: &[&[u8]]) -> Reply {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (sent, reply) = mpsc::channel();
        locked(&self.waiting).insert(id, sent);
        let id_bytes = id.to_le_bytes();
        let mut all: Vec<&[u8]> = vec![&id_bytes, std::slice::from_ref(&what)];
        all.extend_from_slice(parts);
        self.send(wire::ASK, &all);
        // The reader ends the process before it lets go of the asks.
        reply.recv().unwrap_or_else(|_| exit_now())
    }

    /// A blob of `bytes` bytes reserved in the tool's budget: its handle.
    fn reserve(&self, bytes: u64) -> Result<u32, String> {
        match self.ask(wire::ASK_RESERVE, &[&bytes.to_le_bytes()]) {
            (wire::GIVEN, id) if id.len() == 4 => {
                Ok(u32::from_le_bytes(id.try_into().expect("four bytes")))
            }
            (_, why) => Err(String::from_utf8_lossy(&why).into_owned()),
        }
    }

    /// The module of `wasm` for the driven engine, from the tool host: by its SHA-256, and if
    /// the tool host hasn't its code, by its WASM.
    fn code(&self, wasm: &[u8]) -> Result<wasmtime::Module, String> {
        let sha = crate::code::sha256(wasm);
        let mut reply = self.ask(wire::ASK_CODE, &[&sha]);
        if reply.0 == wire::SEND_WASM {
            reply = self.ask(wire::ASK_WASM, &[wasm]);
        }
        match reply {
            (wire::GIVEN, bytes) => crate::code::load(crate::program::driven_engine(), &bytes),
            (_, why) => Err(String::from_utf8_lossy(&why).into_owned()),
        }
    }

    /// A command's end.
    fn done(&self, result: Result<Vec<u8>, String>) {
        match result {
            Ok(bytes) => self.send(wire::DONE, &[&[0], &bytes]),
            Err(why) => self.send(wire::DONE, &[&[1], why.as_bytes()]),
        }
    }
}

/// `wrela tool --drive <parent>`: runs a driven build for the tool host `parent` until the
/// tool host closes it or ends.
pub fn run(parent: u32) -> ! {
    die_with(parent);
    let [commands, answers, build, root, storage] = FDS.map(adopt);
    let link = Arc::new(Link {
        out: Mutex::new(File::from(answers)),
        waiting: Mutex::new(HashMap::new()),
        next: AtomicU32::new(0),
    });
    let dir = |fd: std::os::fd::OwnedFd| cap_std::fs::Dir::from_std_file(File::from(fd));
    let (build, root, storage) = (dir(build), dir(root), dir(storage));
    let (given, received) = mpsc::channel::<(u8, Vec<u8>)>();
    let reader = link.clone();
    let spawned =
        std::thread::Builder::new().name("wrela-drive-commands".into()).spawn(move || {
            let mut from = File::from(commands);
            loop {
                // The tool host has ended, or what it sent can't be read: so does this, now.
                let Ok(Some((kind, len))) = wire::read_head(&mut from) else { exit_now() };
                let Ok(bytes) = wire::read_bytes(&mut from, len) else { exit_now() };
                if kind == wire::REPLY {
                    if bytes.len() < 5 {
                        exit_now();
                    }
                    let id = u32::from_le_bytes(bytes[..4].try_into().expect("four bytes"));
                    let waiting = locked(&reader.waiting).remove(&id);
                    let Some(waiting) = waiting else { exit_now() };
                    let _ = waiting.send((bytes[4], bytes[5..].to_vec()));
                } else if given.send((kind, bytes)).is_err() {
                    exit_now();
                }
            }
        });
    if spawned.is_err() {
        exit_now();
    }
    let Ok((wire::LOAD, bytes)) = received.recv() else { exit_now() };
    let since = Instant::now();
    let loaded = (|| {
        let load = Load::parse(&mut Body::new(&bytes))?;
        let storage = Arc::new(storage);
        let (host, pipelines) = start(&load, build, root, storage, &link)?;
        Ok::<_, String>(Actor {
            host,
            load,
            origin: Instant::now(),
            inited: false,
            next_frame: 0,
            trap: None,
            posts: VecDeque::new(),
            recorded: VecDeque::new(),
            pipelines,
        })
    })();
    let mut actor = match loaded {
        Ok(a) => a,
        Err(why) => {
            link.done(Err(why));
            exit_now()
        }
    };
    let mut out = Out::default();
    out.f64(since.elapsed().as_secs_f64());
    link.done(Ok(out.0));
    while let Ok((kind, bytes)) = received.recv() {
        let run = std::panic::AssertUnwindSafe(|| actor.carry_out(kind, &bytes, &link));
        let result = std::panic::catch_unwind(run)
            .unwrap_or_else(|_| Err("the native host failed (it panicked)".into()));
        link.done(result);
    }
    exit_now()
}

/// The host for `load`, its build read through `build`, the builds it hands over to opened
/// through `root`, its storage in `storage`, and every build's code from the tool host.
fn start(
    load: &Load,
    build: cap_std::fs::Dir,
    root: cap_std::fs::Dir,
    storage: Arc<cap_std::fs::Dir>,
    link: &Arc<Link>,
) -> Result<(Hosted, Vec<String>), String> {
    let asks = link.clone();
    let code: crate::CodeFor = Arc::new(move |wasm: &[u8]| asks.code(wasm));
    let name = std::path::Path::new(&load.path);
    if load.gpu {
        #[cfg(feature = "gpu")]
        {
            let options = crate::Options {
                timing: load.timing,
                storage_dir: Some(storage),
                workers: load.workers,
                quiet: true,
                defer_init: true,
                record: load.record,
                ..crate::Options::default()
            };
            // It hands over to the builds it names beside it, each opened through the root's
            // handle by its root path, as its own directory was opened by the tool host.
            let root_name = load.path.split('/').next().unwrap_or_default().to_string();
            let beside = crate::Beside {
                at: load.path.clone(),
                open: Arc::new(move |path: &str| {
                    let rel = match path.split_once('/') {
                        Some((first, rel)) if first == root_name => rel,
                        None if path == root_name => "",
                        _ => return Err(format!("`{path}` isn't in the root `{root_name}`")),
                    };
                    let parts =
                        if rel.is_empty() { Vec::new() } else { crate::confined::parts(rel)? };
                    let dir = crate::confined::open_dir(&root, &parts, false)
                        .map_err(|e| format!("`{path}`: {e}"))?;
                    Ok((std::path::PathBuf::from(path), dir))
                }),
                code: code.clone(),
            };
            let mut host = crate::Host::load_driven(build, name, &code, &options, Some(beside))
                .map_err(|e| e.to_string())?;
            // Only if asked: the word is the jobs' protocol's, which a program without jobs may
            // keep other bytes in.
            if load.hold > 0 {
                host.hold_jobs(load.hold);
            }
            let names = host.pipeline_names();
            return Ok((Hosted::Gpu(Box::new(host)), names));
        }
        #[cfg(not(feature = "gpu"))]
        return Err("this tool host was built without the GPU".into());
    }
    drop(root);
    let mut build = crate::CpuBuild::load_driven(build, name, &code).map_err(|e| e.to_string())?;
    if load.record {
        build = build.recording();
    }
    let names = build.pipeline_names();
    let host = build.instantiate_stored(load.workers, storage).map_err(|e| e.to_string())?;
    if load.hold > 0 {
        host.hold_jobs(load.hold);
    }
    Ok((Hosted::Cpu(host), names))
}

/// A driven build's host.
enum Hosted {
    Cpu(crate::CpuHost),
    #[cfg(feature = "gpu")]
    Gpu(Box<crate::Host>),
}

/// `$call` on `$hosted`'s host, named `$h`, whichever host it is: what both hosts do alike.
macro_rules! on_host {
    ($hosted:expr, $h:ident => $call:expr) => {
        match $hosted {
            Hosted::Cpu($h) => $call,
            #[cfg(feature = "gpu")]
            Hosted::Gpu($h) => $call,
        }
    };
}

/// A trap: the host's message, the panic's, where in the source, what it happened in (`init`
/// 0, a frame 1, a tick 2, a call 3, the voice 4) with the frame's or tick's number, and the
/// step it ended.
#[derive(Clone)]
struct Trap {
    message: String,
    panic: String,
    at: String,
    stage: u8,
    number: u32,
    step: u32,
}

/// The build's state, on the process's main thread.
struct Actor {
    host: Hosted,
    load: Load,
    origin: Instant,
    inited: bool,
    next_frame: u32,
    /// Its first trap: it runs no code after it.
    trap: Option<Trap>,
    /// The posts the program made that the tool hasn't been given yet.
    posts: VecDeque<crate::QueuedPost>,
    /// The commands it recorded that the tool hasn't been given yet.
    recorded: VecDeque<Recorded>,
    /// Its pipelines' names, by index (the manifest's).
    pipelines: Vec<String>,
}

/// A command a build recorded, as `commands` gives it: its opcode's name; a label's name; the
/// name of the pipeline a dispatch or draw uses; a dispatch's workgroups; the buffers it binds
/// (each one's handle, offset and size; 0 for the whole); what a host-facing command names (a
/// storage path, a fetch's or a post's URL, a host request's service, a build's path); and the
/// bytes a storage write, a post or a host request carries.
#[derive(Default)]
struct Recorded {
    kind: &'static str,
    label: String,
    pipeline: String,
    groups: [u32; 3],
    bindings: Vec<[u32; 3]>,
    target: String,
    bytes: Vec<u8>,
}

impl Recorded {
    /// `c`, its pipeline named from `pipelines`.
    fn of(c: &wrela_abi::stream::Command, pipelines: &[String]) -> Recorded {
        use wrela_abi::stream::Command as C;
        let named = |p: u32| pipelines.get(p as usize).cloned().unwrap_or_default();
        let bound = |b: &[wrela_abi::stream::Binding]| -> Vec<[u32; 3]> {
            b.iter().map(|b| [b.handle, b.offset, b.size]).collect()
        };
        let mut r = Recorded { kind: c.opcode().name(), ..Recorded::default() };
        match c {
            C::Label { name } => r.label = bounded(name),
            C::Dispatch { pipeline, groups, bindings, .. } => {
                (r.pipeline, r.groups, r.bindings) = (named(*pipeline), *groups, bound(bindings));
            }
            C::StorageRead { path, .. }
            | C::Fetch { url: path, .. }
            | C::Prepare { path, .. }
            | C::Unprepare { path }
            | C::HandOver { path, .. } => r.target = bounded(path),
            C::StorageWrite { path, data: bytes, .. }
            | C::Post { url: path, body: bytes, .. }
            | C::HostRequest { service: path, body: bytes, .. } => {
                (r.target, r.bytes) = (bounded(path), bytes.to_vec());
            }
            C::Draw { pipeline, bindings, .. }
            | C::DrawIndexed { pipeline, bindings, .. }
            | C::DispatchIndirect { pipeline, bindings, .. }
            | C::DrawIndirect { pipeline, bindings, .. }
            | C::DrawIndexedIndirect { pipeline, bindings, .. }
            | C::MultiDrawIndirect { pipeline, bindings, .. } => {
                (r.pipeline, r.bindings) = (named(*pipeline), bound(bindings));
            }
            _ => {}
        }
        r
    }

    /// Its bytes in an answer.
    fn size(&self) -> usize {
        self.kind.len()
            + self.label.len()
            + self.pipeline.len()
            + self.target.len()
            + self.bytes.len()
            + 36
            + 12 * self.bindings.len()
    }

    fn write(&self, out: &mut Out) {
        out.str(self.kind).str(&self.label).str(&self.pipeline);
        out.u32(self.groups[0]).u32(self.groups[1]).u32(self.groups[2]);
        out.u32(self.bindings.len() as u32);
        for b in &self.bindings {
            out.u32(b[0]).u32(b[1]).u32(b[2]);
        }
        out.str(&self.target).u32(self.bytes.len() as u32);
        out.0.extend_from_slice(&self.bytes);
    }
}

/// What a request's steps saw, collected in step order: each kind's count and bytes, as std's
/// `drive::Seen` reads them, and the bytes they hold in all.
#[derive(Default)]
struct Seen {
    ran: (u32, Out),
    captures: (u32, Out),
    hashes: (u32, Out),
    phases: (u32, Out),
    audio: (u32, Out),
    buffers: (u32, Out),
    bytes: (u32, Out),
    commands: (u32, Out),
    running: (u32, Out),
    used: usize,
}

impl Seen {
    /// Counts `n` more bytes: an error if the answer's steps would fill more than they may.
    fn room(&mut self, n: usize) -> Result<(), String> {
        if self.used + n > wire::MOST_INLINE - KEPT_ROOM {
            return Err(format!(
                "what the steps saw is more than an answer holds ({} bytes, blobs aside)",
                wire::MOST_INLINE
            ));
        }
        self.used += n;
        Ok(())
    }

    /// What's left for the lines.
    fn left(&self) -> usize {
        wire::MOST_INLINE - KEPT_ROOM - self.used
    }

    /// The answer: each kind in order, the lines and how many were dropped, then the trap.
    fn finish(
        self,
        lines: &[String],
        dropped: u64,
        log: Option<u32>,
        trap: Option<&Trap>,
    ) -> Vec<u8> {
        let mut out = Out::default();
        for (n, bytes) in [
            self.ran,
            self.captures,
            self.hashes,
            self.phases,
            self.audio,
            self.buffers,
            self.bytes,
            self.commands,
            self.running,
        ] {
            out.u32(n);
            out.0.extend_from_slice(&bytes.0);
        }
        out.strs(lines).u32(u32::try_from(dropped).unwrap_or(u32::MAX));
        match log {
            None => {
                out.u8(0);
            }
            Some(id) => {
                out.u8(1).u8(1).u32(id);
            }
        }
        match trap {
            None => {
                out.u8(0);
            }
            Some(t) => {
                out.u8(1).str(&t.message).str(&t.panic).str(&t.at);
                out.u8(t.stage).u32(t.number).u32(t.step);
            }
        }
        out.0
    }
}

/// What running one step that runs code gave: when it started, how long it took, each frame's
/// or tick's start and end, and an export's results.
#[derive(Default)]
struct Ran {
    started: f64,
    elapsed: f64,
    spans: Vec<(f64, f64)>,
    results: Vec<crate::Value>,
}

impl Ran {
    /// Its bytes with `spans` and `results` frames and results.
    fn size(spans: usize, results: usize) -> usize {
        24 + 16 * spans + 9 * results
    }

    fn write(&self, out: &mut Out) {
        out.f64(self.started).f64(self.elapsed).u32(self.spans.len() as u32);
        for (start, end) in &self.spans {
            out.f64(*start).f64(*end);
        }
        out.u32(self.results.len() as u32);
        for v in &self.results {
            let (kind, bits) = wire::scalar(*v);
            out.u8(kind).u64(bits);
        }
    }
}

impl Actor {
    fn failure(&self) -> Option<crate::Failure> {
        on_host!(&self.host, h => h.last_failure().cloned())
    }

    /// Its first trap, `e`, in `stage` number `number` (or in the tick the host says it
    /// happened in: a lockstep frame runs ticks too, and the ticker numbers them), at step
    /// `step`.
    fn trapped(&mut self, e: &crate::Error, stage: u8, number: u32, step: usize) {
        let failure = self.failure();
        let (stage, number) = match failure.as_ref().and_then(|f| f.tick) {
            Some(k) if stage != 4 => (2, k),
            _ => (stage, number),
        };
        let text = |s: String| bounded_to(&s, MOST_TRAP_TEXT);
        self.trap.get_or_insert(Trap {
            message: text(e.to_string()),
            panic: text(failure.as_ref().and_then(|f| f.panic.clone()).unwrap_or_default()),
            at: text(failure.and_then(|f| f.at).unwrap_or_default()),
            stage,
            number,
            step: step as u32,
        });
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

    fn trace(&mut self) -> crate::Trace {
        on_host!(&mut self.host, host => host.trace())
    }

    /// Carries out the tool host's command `kind`, with `bytes`: its answer.
    fn carry_out(&mut self, kind: u8, bytes: &[u8], link: &Link) -> Result<Vec<u8>, String> {
        let mut b = Body::new(bytes);
        match kind {
            wire::STEPS => {
                let steps = wire::parse_steps(&mut b)?;
                b.end()?;
                self.steps(steps, link)
            }
            wire::POSTS => Ok(self.posts()),
            wire::ANSWER => {
                let post = b.u32()?;
                let answer = match b.u8()? {
                    0 => Ok(b.rest().to_vec()),
                    _ => Err(String::from_utf8_lossy(b.rest()).into_owned()),
                };
                on_host!(&mut self.host, host => host.answer_post(post, answer))?;
                Ok(Vec::new())
            }
            k => Err(format!("{k} isn't a command")),
        }
    }

    /// Runs `steps` in order: what they saw. After a trap, a step that runs code is skipped.
    /// The lines and the quota are the request's alone: what an earlier request left (one
    /// whose host panicked) is dropped first, and a failed request's lines go with its failure,
    /// if they fit an answer.
    fn steps(&mut self, steps: Vec<Step>, link: &Link) -> Result<Vec<u8>, String> {
        let trace = self.trace();
        trace.keep_at_most(usize::MAX, MOST_KEPT_LINE_BYTES);
        let _ = (trace.take_lines(), trace.take_dropped());
        let mut seen = Seen::default();
        for (i, step) in steps.into_iter().enumerate() {
            let name = step.name();
            if let Err(why) = self.step(i, step, &mut seen, link) {
                let mut why = format!("step {i} ({name}): {why}");
                let (printed, _) = (trace.take_lines(), trace.take_dropped());
                let size: usize = printed.iter().map(|l| l.len() + 1).sum();
                if !printed.is_empty() && why.len() + size + 64 <= wire::MOST_INLINE {
                    why.push_str(&format!("; the build printed {} lines:", printed.len()));
                    for l in &printed {
                        why.push('\n');
                        why.push_str(l);
                    }
                }
                return Err(why);
            }
        }
        let printed = trace.take_lines();
        let dropped = trace.take_dropped();
        // Every line, in the answer if they all fit it (each cut at half an answer), else as a
        // blob, newline after newline, reserved before its bytes cross.
        let inline: usize = printed.iter().map(|l| l.len().min(MOST_BYTES / 2) + 4).sum();
        let (lines, log) = if inline <= seen.left() {
            (printed.iter().map(|l| bounded(l)).collect(), None)
        } else {
            let bytes: usize = printed.iter().map(|l| l.len() + 1).sum();
            let id = link.reserve(bytes as u64).map_err(|why| {
                format!("the {} lines it printed, {bytes} bytes: {why}", printed.len())
            })?;
            let mut text = Vec::with_capacity(bytes);
            for l in &printed {
                text.extend_from_slice(l.as_bytes());
                text.push(b'\n');
            }
            link.send(wire::BLOB, &[&id.to_le_bytes(), &text]);
            (Vec::new(), Some(id))
        };
        let trap = self.trap.clone();
        Ok(seen.finish(&lines, dropped, log, trap.as_ref()))
    }

    fn step(&mut self, i: usize, step: Step, seen: &mut Seen, link: &Link) -> Result<(), String> {
        if step.runs_code() {
            if self.trap.is_some() {
                return Ok(());
            }
            let first = matches!(step, Step::Init);
            if first == self.inited {
                return Err(if first {
                    "`init` has run already".into()
                } else {
                    format!("`{}` runs after `init`", step.name())
                });
            }
        }
        let mut ran = Ran { started: self.since(), ..Ran::default() };
        let (w, h, fps) = (self.load.width, self.load.height, self.load.fps);
        match step {
            Step::Init => {
                seen.room(Ran::size(0, 0))?;
                let done = on_host!(&mut self.host, host => host.init());
                self.inited = true;
                if let Err(e) = done {
                    self.trapped(&e, 0, 0, i);
                }
            }
            Step::Frames(n, script) => {
                let script = wire::script(&script)?;
                seen.room(Ran::size(n as usize, 0))?;
                for _ in 0..n {
                    let k = self.next_frame;
                    let start = self.since();
                    let done = on_host!(&mut self.host, host => host.lockstep_frame(k, fps, w, h, &script));
                    self.next_frame += 1;
                    if let Err(e) = done {
                        self.trapped(&e, 1, k, i);
                        break;
                    }
                    ran.spans.push((start, self.since()));
                }
            }
            Step::FrameAt(time) => {
                seen.room(Ran::size(1, 0))?;
                let k = self.next_frame;
                let start = self.since();
                let done = on_host!(&mut self.host, host => host.frame(time, w, h));
                self.next_frame += 1;
                match done {
                    Ok(()) => ran.spans.push((start, self.since())),
                    Err(e) => self.trapped(&e, 1, k, i),
                }
            }
            Step::Ticks(n, script) => {
                let script = wire::script(&script)?;
                if self.cpu().is_none() {
                    return Err("ticks alone run on a build loaded without the GPU".into());
                }
                seen.room(Ran::size(n as usize, 0))?;
                for _ in 0..n {
                    let start = self.since();
                    let done = self.cpu().map_or(Ok(()), |host| host.scripted_tick(&script));
                    match done {
                        Ok(()) => {}
                        // No ticker, say: not a trap.
                        Err(crate::Error::Program(why)) => return Err(why),
                        Err(e) => {
                            // A tick's trap: the host numbers it.
                            self.trapped(&e, 2, 0, i);
                            break;
                        }
                    }
                    ran.spans.push((start, self.since()));
                }
            }
            Step::Call(export, args) => {
                seen.room(Ran::size(0, 0))?;
                let done = on_host!(&mut self.host, host => host.call_export(&export, &args));
                match done {
                    Ok(values) => {
                        seen.room(9 * values.len())?;
                        ran.results = values;
                    }
                    // An export that isn't there, or arguments of the wrong types, isn't a trap.
                    Err(crate::Error::Program(why)) => return Err(why),
                    Err(e) => self.trapped(&e, 3, 0, i),
                }
            }
            Step::Audio(quanta, timed) => return self.audio(i, quanta, timed, seen, link),
            Step::Capture => {
                seen.room(13)?;
                #[cfg(feature = "gpu")]
                if let Hosted::Gpu(host) = &mut self.host {
                    // Its bytes and blob are reserved before it's read back: a capture that
                    // doesn't fit isn't read.
                    let bytes = u64::from(w) * u64::from(h) * 4;
                    let id = link
                        .reserve(bytes)
                        .map_err(|why| format!("the screen's {bytes} bytes: {why}"))?;
                    let rgba = host.read_screen().map_err(|e| e.to_string())?;
                    link.send(wire::BLOB, &[&id.to_le_bytes(), &rgba]);
                    let (n, out) = &mut seen.captures;
                    *n += 1;
                    out.u32(w).u32(h).u8(1).u32(id);
                    return Ok(());
                }
                let _ = link;
                return Err("a build loaded without the GPU has no screen".into());
            }
            Step::Buffers => {
                #[cfg(feature = "gpu")]
                if let Hosted::Gpu(host) = &mut self.host {
                    let held = host.buffers();
                    if held.len() > MOST_BUFFERS {
                        return Err(format!(
                            "the build holds {} buffers, more than an answer names ({MOST_BUFFERS})",
                            held.len()
                        ));
                    }
                    seen.room(4 + 4 * held.len())?;
                    let (n, out) = &mut seen.buffers;
                    *n += 1;
                    out.u32(held.len() as u32);
                    for h in held {
                        out.u32(h);
                    }
                    return Ok(());
                }
                return Err("a build loaded without the GPU holds no GPU buffers".into());
            }
            Step::ReadBuffer(handle) => {
                seen.room(5)?;
                #[cfg(feature = "gpu")]
                if let Hosted::Gpu(host) = &mut self.host {
                    // Its size first, then the blob reserved, then its bytes read back.
                    let size = host.buffer_size(handle).map_err(|e| e.to_string())?;
                    let id = link
                        .reserve(size)
                        .map_err(|why| format!("buffer {handle}'s {size} bytes: {why}"))?;
                    let bytes = host.read_buffer(handle).map_err(|e| e.to_string())?;
                    link.send(wire::BLOB, &[&id.to_le_bytes(), &bytes]);
                    let (n, out) = &mut seen.bytes;
                    *n += 1;
                    out.u8(1).u32(id);
                    return Ok(());
                }
                let _ = handle;
                return Err("a build loaded without the GPU holds no GPU buffers".into());
            }
            Step::Running => {
                let at = match &self.host {
                    Hosted::Cpu(_) => self.load.path.clone(),
                    #[cfg(feature = "gpu")]
                    Hosted::Gpu(host) => host.driven_at().unwrap_or(&self.load.path).to_string(),
                };
                seen.room(4 + at.len())?;
                let (n, out) = &mut seen.running;
                *n += 1;
                out.str(&at);
                return Ok(());
            }
            Step::Hash => {
                seen.room(8)?;
                let hash = on_host!(&mut self.host, host => host.hash());
                let (n, out) = &mut seen.hashes;
                *n += 1;
                out.u64(hash);
                return Ok(());
            }
            Step::Phases => {
                let phases = self.trace().phases();
                let size: usize = phases.iter().map(|p| 20 + p.name.len()).sum();
                seen.room(4 + size)?;
                let (n, out) = &mut seen.phases;
                *n += 1;
                out.u32(phases.len() as u32);
                for p in &phases {
                    out.str(&p.name).f64(p.seconds).u32(p.times);
                }
                return Ok(());
            }
            Step::Commands => return self.commands(seen),
            Step::Hold(path) => {
                on_host!(&mut self.host, host => host.hold(&path));
                return Ok(());
            }
            Step::Release(path) => {
                on_host!(&mut self.host, host => host.release(&path));
                return Ok(());
            }
        }
        ran.elapsed = self.since() - ran.started;
        let (n, out) = &mut seen.ran;
        *n += 1;
        ran.write(out);
        Ok(())
    }

    /// The commands it recorded since the last `Commands`, decoded: as many as the answer has
    /// room for, the oldest first; the rest wait for the next.
    fn commands(&mut self, seen: &mut Seen) -> Result<(), String> {
        if !self.load.record {
            return Err("the build wasn't loaded to record its commands (`record`)".into());
        }
        let batches = match &mut self.host {
            Hosted::Cpu(host) => host.take_batches(),
            #[cfg(feature = "gpu")]
            Hosted::Gpu(host) => {
                // The build that runs names the pipelines: after a hand-over, another.
                self.pipelines = host.pipeline_names();
                host.take_batches()
            }
        };
        for batch in &batches {
            let commands = wrela_abi::stream::decode(batch)
                .map_err(|e| format!("a recorded batch doesn't decode: {e}"))?;
            // A batch begins with a mark of its own: a label counts to its batch's end.
            self.recorded.push_back(Recorded { kind: "Batch", ..Recorded::default() });
            for c in &commands {
                self.recorded.push_back(Recorded::of(c, &self.pipelines));
            }
        }
        // A command no answer holds, even one with nothing else in it, is dropped, and the
        // request fails, saying so: an observation is whole or isn't given.
        if let Some(r) = self.recorded.front()
            && r.size() > Seen::default().left() - 8
        {
            let why = format!(
                "a recorded `{}` of {} bytes is more than an answer holds, so it's dropped",
                r.kind,
                r.size()
            );
            self.recorded.pop_front();
            return Err(why);
        }
        seen.room(8)?;
        let mut given = Out::default();
        let mut count = 0u32;
        while let Some(r) = self.recorded.front() {
            if count as usize == MOST_COMMANDS || seen.room(r.size()).is_err() {
                break;
            }
            r.write(&mut given);
            count += 1;
            self.recorded.pop_front();
        }
        let (n, out) = &mut seen.commands;
        *n += 1;
        out.u32(count);
        out.0.extend_from_slice(&given.0);
        out.u32(self.recorded.len() as u32);
        Ok(())
    }

    /// The voice's next `quanta` quanta, rendered offline as `CpuHost::render_audio` renders
    /// them: all at once, or one at a time and each timed. Their blobs are reserved first, so
    /// a render that doesn't fit runs nothing. A trap is the build's, as a call's is.
    fn audio(
        &mut self,
        i: usize,
        quanta: u32,
        timed: bool,
        seen: &mut Seen,
        link: &Link,
    ) -> Result<(), String> {
        seen.room(11)?;
        let bytes = u64::from(quanta) * QUANTUM_BYTES;
        let samples_blob =
            link.reserve(bytes).map_err(|why| format!("the voice's {bytes} bytes: {why}"))?;
        let times_blob = if timed {
            let bytes = u64::from(quanta) * 8;
            Some(
                link.reserve(bytes)
                    .map_err(|why| format!("the quanta's times, {bytes} bytes: {why}"))?,
            )
        } else {
            None
        };
        let mut samples = Vec::with_capacity(bytes as usize);
        let mut times = Vec::new();
        let mut render = |n: u32| on_host!(&mut self.host, host => host.render_audio(n));
        let mut done = Ok(());
        if timed {
            times.reserve(8 * quanta as usize);
            for _ in 0..quanta {
                let started = Instant::now();
                match render(1) {
                    Ok(s) => {
                        times.extend_from_slice(&started.elapsed().as_secs_f64().to_le_bytes());
                        samples.extend(s.iter().flat_map(|x| x.to_le_bytes()));
                    }
                    Err(e) => {
                        done = Err(e);
                        break;
                    }
                }
            }
        } else {
            match render(quanta) {
                Ok(s) => samples.extend(s.iter().flat_map(|x| x.to_le_bytes())),
                Err(e) => done = Err(e),
            }
        }
        match done {
            Ok(()) => {
                link.send(wire::BLOB, &[&samples_blob.to_le_bytes(), &samples]);
                let (n, out) = &mut seen.audio;
                *n += 1;
                out.u8(1).u32(samples_blob);
                match times_blob {
                    Some(t) => {
                        link.send(wire::BLOB, &[&t.to_le_bytes(), &times]);
                        out.u8(1).u8(1).u32(t);
                    }
                    None => {
                        out.u8(0);
                    }
                }
                Ok(())
            }
            // No voice, say: not a trap.
            Err(crate::Error::Program(why)) => Err(why),
            Err(e) => {
                self.trapped(&e, 4, 0, i);
                Ok(())
            }
        }
    }

    /// The posts the program made since the tool was last given them: as many as an answer
    /// holds, the oldest first; the rest at the next.
    fn posts(&mut self) -> Vec<u8> {
        let made = on_host!(&mut self.host, host => host.take_posts());
        self.posts.extend(made);
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
        let mut out = Out::default();
        out.u32(given.len() as u32);
        for p in &given {
            out.u32(p.request).str(&p.url).u8(0).u32(p.body.len() as u32);
            out.0.extend_from_slice(&p.body);
        }
        out.0
    }
}

/// `line`, at most half an answer's bytes of it (cut at a character), with a note if it was
/// cut.
fn bounded(line: &str) -> String {
    bounded_to(line, MOST_BYTES / 2)
}

#[cfg(test)]
mod tests {
    use super::Recorded;
    use wrela_abi::stream::{Binding, Command};

    /// Every draw names its pipeline, indexed and multi-draws too: the pass viewer lists them.
    #[test]
    fn every_draw_names_its_pipeline() {
        let names = vec![String::from("a"), String::from("b")];
        let bound = vec![Binding { handle: 7, offset: 0, size: 0 }];
        let indexed = Command::DrawIndexed {
            pipeline: 1,
            indices: 3,
            index_offset: 0,
            index_size: 0,
            index_count: 6,
            instances: 1,
            first_index: 0,
            base_vertex: 0,
            first_instance: 0,
            bindings: bound.clone(),
            uniforms: &[],
        };
        let multi = Command::MultiDrawIndirect {
            pipeline: 1,
            indices: Some([3, 0, 0]),
            arguments: 4,
            offset: 0,
            count: 8,
            count_buffer: 5,
            count_offset: 0,
            bindings: bound,
            uniforms: &[],
        };
        for c in [indexed, multi] {
            let r = Recorded::of(&c, &names);
            assert_eq!((r.pipeline.as_str(), r.bindings.as_slice()), ("b", &[[7, 0, 0]][..]));
        }
    }
}
