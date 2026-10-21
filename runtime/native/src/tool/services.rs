//! The tool services (`std::tool`'s modules, language.md §15.5): each request's service and
//! bytes, read as std writes them, and the work that answers it (`http` and `run` are modules
//! of their own; `testing` is a tool test's fakes, §13.2). std owns both sides of every
//! layout; this is the host's half. Every answer starts with a byte, 0 for success then what the
//! service gives, or 1 then why it failed, in UTF-8. A service the host doesn't have fails the
//! request instead, as any host's does.

use super::blobs::Blobs;
use super::caps::{Caps, Entry, MOST_BYTES};
use super::http::{Fake, Faked};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// What a request is answered with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// The answer's bytes, starting with whether the service succeeded.
    Bytes(Vec<u8>),
    /// The host has no such service: the request fails (`REQUEST_FAILED`).
    Failed,
}

/// A success, giving `payload`.
fn ok(payload: &[u8]) -> Answer {
    let mut bytes = Vec::with_capacity(payload.len() + 1);
    bytes.push(0);
    bytes.extend_from_slice(payload);
    Answer::Bytes(bytes)
}

/// A failure, for `why`.
fn err(why: &str) -> Answer {
    let mut bytes = vec![1];
    bytes.extend_from_slice(why.as_bytes());
    Answer::Bytes(bytes)
}

pub(crate) fn answer(result: Result<Vec<u8>, String>) -> Answer {
    match result {
        Ok(payload) => ok(&payload),
        Err(why) => err(&why),
    }
}

/// What every service shares: the capability context, the blobs, a tool test's fakes, the
/// directory the host owns for the run's programs, and whether the tool has ended.
pub(crate) struct Services {
    pub caps: Caps,
    pub blobs: Mutex<Blobs>,
    /// How long a blob's write to a file waits before it writes: a test's slow disk.
    pub io_delay: Duration,
    /// Set once the tool has ended: a worker waiting on a program or a response stops.
    pub closing: Arc<AtomicBool>,
    /// Whether this is a tool test's run, which has fakes (`std::tool::testing`).
    pub testing: bool,
    /// A tool test's fakes for its fake sites' URLs.
    pub fakes: Mutex<HashMap<String, Fake>>,
    /// The run's own directory, made when a program or a browser first runs: its `HOME`, its
    /// input and output files, and its browsers' profiles; those of a host that was killed are
    /// removed when the next makes its own. `work_at` names it, for a test; else it's a new one in the system's
    /// temporary directory.
    pub work: OnceLock<Result<PathBuf, String>>,
    pub work_at: Option<PathBuf>,
    /// Where each program's process group is written as it starts, for a supervisor that must
    /// kill them if this host can't (a tool test's, §13.2).
    pub groups: Option<PathBuf>,
    /// A test's moment between a program's start and its record.
    pub spawn_pause: Duration,
    /// A test's name lookup that takes this long, which nothing cancels.
    pub resolve_stall: Duration,
    /// How long a cancelled HTTP client has to return before the run is the host's failure.
    pub grace: Duration,
    /// Set by a worker whose work can't be stopped: the host ends the run as its failure.
    pub stuck: Arc<Stuck>,
    /// The run's budget as a whole (`--timeout`), if it has one: the toolchain's requests may
    /// ask for a deadline as long as it.
    pub budget: Option<Duration>,
    /// The compiler the toolchain service runs, if the host was given one, and its cache.
    pub toolchain: Option<super::toolchain::Service>,
    /// The builds the drive service holds.
    pub drives: Mutex<super::drive::Drives>,
    /// The run's compiled code of its driven builds.
    pub codes: super::drive::Codes,
    /// A test's wait before the drive service reads each blob's bytes ([`super::Testing`]).
    pub drive_hold: Duration,
    /// A test's wait before the drive service compiles a build's code ([`super::Testing`]).
    pub compile_hold: Duration,
    /// The servers the serve service holds.
    pub servers: Mutex<super::serve::Servers>,
    /// The browsers the browser service holds.
    pub browsers: Mutex<super::browser::Browsers>,
    /// Where a service that answers a request itself, later, gives the answer
    /// ([`Work::Later`]): the run's shared state, until it ends.
    pub answers: Mutex<Option<Arc<super::hub::Hub>>>,
    /// A test's held output write, and its output write that fails part way ([`super::Testing`]).
    pub hold: Option<(String, Duration)>,
    pub write_fail: Option<String>,
    /// A test's start of a server that fails once the server listens.
    pub fail_server_start: bool,
    /// A test's image work that holds this long first, seeing no cancel.
    pub image_stall: Duration,
    /// What tells a test the host has reached a moment it waits for.
    pub signal: Option<super::Signal>,
    /// The threads of work given up on (it didn't stop within the grace after it was
    /// cancelled): the run's end waits for them with the rest, and counts those still running.
    pub strays: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

/// Work that didn't stop within its grace of being cancelled: why, and when it was found. The
/// run is the host's failure, but the tool first has one more grace to read the answer of the
/// request that failed and end on its own, so what the tool says about its failure is shown
/// before what the host says about its workers.
pub(crate) struct Stuck {
    why: OnceLock<String>,
    at: OnceLock<std::time::Instant>,
    grace: Duration,
}

impl Stuck {
    /// Nothing stuck yet; the run ends `grace` after something is.
    pub fn new(grace: Duration) -> Stuck {
        Stuck { why: OnceLock::new(), at: OnceLock::new(), grace }
    }

    /// Records why work is stuck: the first reason given counts.
    pub fn set(&self, why: String) -> Result<(), String> {
        self.why.set(why)?;
        let _ = self.at.set(std::time::Instant::now());
        Ok(())
    }

    /// Why work is stuck, if it is.
    pub fn get(&self) -> Option<&String> {
        self.why.get()
    }

    /// Why the run must end now: work is stuck, and the tool's grace since has passed.
    pub fn due(&self) -> Option<&String> {
        let at = self.at.get()?;
        if at.elapsed() >= self.grace { self.why.get() } else { None }
    }
}

impl Services {
    pub(crate) fn blobs(&self) -> std::sync::MutexGuard<'_, Blobs> {
        // A worker that panicked while it held them leaves them as they were.
        self.blobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The driven builds.
    pub(crate) fn drives(&self) -> std::sync::MutexGuard<'_, super::drive::Drives> {
        self.drives.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The servers.
    pub(crate) fn servers(&self) -> std::sync::MutexGuard<'_, super::serve::Servers> {
        self.servers.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The browsers.
    pub(crate) fn browsers(&self) -> std::sync::MutexGuard<'_, super::browser::Browsers> {
        self.browsers.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Gives `answer` to `request`, which a service answers itself ([`Work::Later`]): nothing,
    /// once the run has ended.
    pub(crate) fn send(&self, request: u32, answer: Answer) {
        let hub = self.answers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(hub) = hub {
            hub.deliver(request, answer);
        }
    }

    /// Tells a test the host has reached `moment` ([`super::Signal`]).
    pub(crate) fn signal(&self, moment: &'static str) {
        if let Some(signal) = &self.signal {
            (signal.0)(moment);
        }
    }

    /// The fakes a tool test set, by URL (none outside a test).
    pub(crate) fn fakes(&self) -> std::sync::MutexGuard<'_, HashMap<String, Fake>> {
        self.fakes.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The run's own directory, with `home/` in it, made the first time it's needed.
    pub(crate) fn work_dir(&self) -> Result<PathBuf, String> {
        self.work
            .get_or_init(|| {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.subsec_nanos());
                let dir = self.work_at.clone().unwrap_or_else(|| {
                    // What a killed host left (its programs' files, its browsers' profiles)
                    // goes first.
                    wrela_chrome::sweep(&std::env::temp_dir(), "wrela-tool-");
                    std::env::temp_dir()
                        .join(format!("wrela-tool-{}-{n}-{nanos}", std::process::id()))
                });
                std::fs::create_dir_all(dir.join("home"))
                    .map(|_| dir)
                    .map_err(|e| format!("can't make the run's directory: {e}"))
            })
            .clone()
    }

    /// Reads `from` to its end into `out` as std's `blob::read_data` reads it: 0 and the bytes
    /// (a count, then each) if they fit an answer, else 1 and a new blob of them, made for
    /// `request`. The bytes are charged to the blob budget as they're read, a chunk at a time,
    /// so a body past the budget, or past `most` bytes, is refused as soon as it gets there,
    /// not once it's all read; the blob is released then.
    pub(crate) fn put_read(
        &self,
        request: u32,
        out: &mut Vec<u8>,
        from: &mut dyn std::io::Read,
        most: u64,
        what: &str,
    ) -> Result<(), String> {
        let mut sink = self.sink(request, most);
        let mut chunk = vec![0; 1 << 20];
        loop {
            let n = from.read(&mut chunk).map_err(|e| format!("{what}: {e}"))?;
            if n == 0 {
                break;
            }
            sink.push(&chunk[..n], what)?;
        }
        sink.finish(out);
        Ok(())
    }

    /// A sink for at most `most` bytes of `request`'s answer.
    pub(crate) fn sink(&self, request: u32, most: u64) -> Sink<'_> {
        Sink { s: self, request, inline: Vec::new(), blob: None, total: 0, most }
    }
}

/// Bytes being given to a request's answer: kept here while they fit one, then in a blob of
/// the request's, which is released if they never reach the answer.
pub(crate) struct Sink<'a> {
    s: &'a Services,
    request: u32,
    inline: Vec<u8>,
    blob: Option<u32>,
    total: u64,
    most: u64,
}

impl Sink<'_> {
    /// Adds `bytes`, charged to the budget now: an error past it or past `most`.
    pub(crate) fn push(&mut self, bytes: &[u8], what: &str) -> Result<(), String> {
        self.total += bytes.len() as u64;
        if self.total > self.most {
            return Err(format!("{what} is more than {} bytes", self.most));
        }
        if self.blob.is_none() && self.inline.len() + bytes.len() <= MOST_BYTES {
            self.inline.extend_from_slice(bytes);
            return Ok(());
        }
        let mut blobs = self.s.blobs();
        let id = match self.blob {
            Some(id) => id,
            None => {
                let id = blobs.create(self.request)?;
                self.blob = Some(id);
                let kept = std::mem::take(&mut self.inline);
                blobs.append(id, &kept)?;
                id
            }
        };
        blobs.append(id, bytes)
    }

    /// The bytes, or the blob, written to `out`: the blob is the answer's now.
    pub(crate) fn finish(mut self, out: &mut Vec<u8>) {
        match self.blob.take() {
            None => {
                out.push(0);
                out.extend_from_slice(&(self.inline.len() as u32).to_le_bytes());
                out.extend_from_slice(&self.inline);
            }
            Some(id) => {
                out.push(1);
                out.extend_from_slice(&id.to_le_bytes());
            }
        }
    }
}

impl Drop for Sink<'_> {
    /// Bytes that never reached an answer give their blob back.
    fn drop(&mut self) {
        if let Some(id) = self.blob.take() {
            let _ = self.s.blobs().release(id);
        }
    }
}

/// Bytes a request carries, or a blob of them.
pub(crate) enum Data {
    Bytes(Vec<u8>),
    Blob(u32),
}

/// Work on a worker thread: given the services and the request's number.
pub(crate) type Task = Box<dyn FnOnce(&Arc<Services>, u32) -> Answer + Send>;

/// A request registered with the service that answers it, as the tool makes it: its answer now,
/// or `None` if the service sends it later ([`Services::send`]).
pub(crate) type Register = Box<dyn FnOnce(&Arc<Services>, u32) -> Option<Answer> + Send>;

/// How a request is carried out.
pub(crate) enum Work {
    /// Answered now, as the tool makes it.
    Now(Answer),
    /// Answered, with no bytes, once this time has passed (on the run's clock: a test's is
    /// fake).
    Timer(Duration),
    /// Answered by a worker of the pool for files and blobs.
    Worker(Task),
    /// Answered by a worker of the pool for long waits (HTTP, programs), so that a slow
    /// network or program never holds up files.
    Long(Task),
    /// Answered by a worker of the pool for the compiler and images, whose work takes seconds
    /// to minutes: so it holds up neither files nor the network.
    Heavy(Task),
    /// Answered by a worker of the pool for driven builds, so a tool that drives several
    /// instances at once runs them at once, and a build's calls never wait for the compiler.
    Drive(Task),
    /// Answered by a worker of the pool for long waits, but given to the tool only once this
    /// much of a test's fake time has passed since the request: a fake site's delay.
    Held(Duration, Task),
    /// Registered at once, as the tool makes it, with the service that answers it when
    /// something happens (a served post arrives), not by a worker that would wait for it.
    Later(Register),
    /// Answered with the date now, on the run's clock.
    Utc,
    /// A test sets its fake clock's date: seconds and nanoseconds since 1970.
    SetUtc(u64, u32),
}

/// A request's bytes, read in order as std's `Writer` wrote them.
pub(crate) struct Body<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Body<'a> {
    /// A reader at the start of `bytes`.
    pub(crate) fn new(bytes: &'a [u8]) -> Body<'a> {
        Body { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len());
        let Some(end) = end else { return Err("the request's bytes end too soon".into()) };
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("four bytes")))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("eight bytes")))
    }

    pub(crate) fn str(&mut self) -> Result<String, String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "text isn't UTF-8".to_string())
    }

    /// Bytes or a blob, as std's `blob::write_data` writes them: 0, a count and the bytes, or
    /// 1 and a blob's handle.
    pub(crate) fn data(&mut self) -> Result<Data, String> {
        match self.u8()? {
            0 => {
                let n = self.u32()? as usize;
                Ok(Data::Bytes(self.take(n)?.to_vec()))
            }
            1 => Ok(Data::Blob(self.u32()?)),
            k => Err(format!("{k} isn't bytes (0) or a blob (1)")),
        }
    }

    /// The bytes left, all of them.
    pub(crate) fn rest(&mut self) -> &'a [u8] {
        let out = &self.bytes[self.at..];
        self.at = self.bytes.len();
        out
    }

    pub(crate) fn end(&self) -> Result<(), String> {
        if self.at == self.bytes.len() { Ok(()) } else { Err("the request has bytes left".into()) }
    }
}

/// How request `service`, with `body`, is carried out, with what the services hold.
pub(crate) fn route(s: &Services, service: &str, body: &[u8]) -> Work {
    let Some(name) = service.strip_prefix("tool/") else { return Work::Now(Answer::Failed) };
    let mut b = Body::new(body);
    match name {
        "time/after" => match b.u64().and_then(|bits| b.end().map(|_| f64::from_bits(bits))) {
            Ok(s) if (0.0..1e9).contains(&s) => Work::Timer(Duration::from_secs_f64(s)),
            Ok(s) => Work::Now(err(&format!("can't wait {s} seconds"))),
            Err(why) => Work::Now(err(&why)),
        },
        "time/utc" => match b.end() {
            Ok(()) => Work::Utc,
            Err(why) => Work::Now(err(&why)),
        },
        "http/fetch" => {
            let f = match super::http::parse(&mut b, std::time::Instant::now()) {
                Ok(f) => f,
                Err(why) => return Work::Now(err(&why)),
            };
            // A fake site's delay is the test's fake time, read as the request is made. A delay
            // past the request's deadline fails it at the deadline, as a real site would.
            let delay = match super::http::target(&f.url) {
                Ok((host, _)) if s.caps.host(host) == Some(true) => {
                    s.fakes().get(super::http::fake_key(&f.url)).map_or(0.0, |f| f.delay)
                }
                _ => 0.0,
            };
            let delay = Duration::from_secs_f64(delay);
            if delay > f.deadline {
                let why = super::http::expired(&f);
                return Work::Held(f.deadline, Box::new(move |_, _| err(&why)));
            }
            let task: Task = Box::new(move |s, request| answer(super::http::fetch(s, request, f)));
            if delay > Duration::ZERO { Work::Held(delay, task) } else { Work::Long(task) }
        }
        _ if name.starts_with("toolchain/") => {
            super::toolchain::route(s, &name["toolchain/".len()..], &mut b)
        }
        _ if name.starts_with("drive/") => super::drive::route(&name["drive/".len()..], &mut b),
        _ if name.starts_with("image/") => super::image::route(&name["image/".len()..], &mut b),
        _ if name.starts_with("serve/") => super::serve::route(&name["serve/".len()..], &mut b),
        _ if name.starts_with("browser/") => {
            super::browser::route(s, &name["browser/".len()..], &mut b)
        }
        "run/run" => match super::run::parse(&mut b, std::time::Instant::now()) {
            Ok(r) => Work::Long(Box::new(move |s, request| answer(super::run::run(s, request, r)))),
            Err(why) => Work::Now(err(&why)),
        },
        "testing/fake" | "testing/utc" if !s.testing => Work::Now(err(
            "only a tool test's host has fakes: `std::tool::testing` is for `@test(tool)`",
        )),
        "testing/fake" => match testing_fake(&mut b) {
            Ok((url, change)) => {
                let mut fakes = s.fakes();
                let set = fakes.entry(super::http::fake_key(&url).to_string()).or_default();
                change(set);
                Work::Now(ok(&[]))
            }
            Err(why) => Work::Now(err(&why)),
        },
        "testing/utc" => {
            match (|| Ok::<_, String>((b.u64()?, b.u32()?)))().and_then(|v| b.end().map(|_| v)) {
                Ok((seconds, nanos)) if nanos < 1_000_000_000 => Work::SetUtc(seconds, nanos),
                Ok(_) => Work::Now(err("nanoseconds are fewer than 1000000000")),
                Err(why) => Work::Now(err(&why)),
            }
        }
        "files/read" | "files/list" | "files/stat" | "files/remove" | "files/remove_all"
        | "files/make_dirs" | "files/read_blob" => {
            let path = match b.str().and_then(|p| b.end().map(|_| p)) {
                Ok(p) => p,
                Err(why) => return Work::Now(err(&why)),
            };
            let name = name.to_string();
            Work::Worker(Box::new(move |s, request| files(s, request, &name, &path)))
        }
        "files/write" => {
            let path = match b.str() {
                Ok(p) => p,
                Err(why) => return Work::Now(err(&why)),
            };
            let bytes = b.rest().to_vec();
            if bytes.len() > MOST_BYTES {
                return Work::Now(err("more than 512 KiB at once: write a blob"));
            }
            Work::Worker(Box::new(move |s, _| answer(s.caps.write(&path, &bytes).map(|_| vec![]))))
        }
        "files/write_blob" => {
            let parsed = b.str().and_then(|p| Ok((p, b.u32()?))).and_then(|v| b.end().map(|_| v));
            let (path, id) = match parsed {
                Ok(v) => v,
                Err(why) => return Work::Now(err(&why)),
            };
            Work::Worker(Box::new(move |s, _| {
                // The bytes are shared, not copied, and written once the table's lock is let go.
                let bytes = match s.blobs().get(id) {
                    Ok(b) => b,
                    Err(why) => return err(&why),
                };
                answer(
                    s.caps
                        .write_with(&path, |f| {
                            s.signal("write");
                            std::thread::sleep(s.io_delay);
                            f.write_all(&bytes)
                        })
                        .map(|_| vec![]),
                )
            }))
        }
        "blob/new" => match b.end() {
            Ok(()) => Work::Worker(Box::new(|s, request| {
                answer(s.blobs().create(request).map(|id| id.to_le_bytes().to_vec()))
            })),
            Err(why) => Work::Now(err(&why)),
        },
        "blob/append" => {
            let id = match b.u32() {
                Ok(id) => id,
                Err(why) => return Work::Now(err(&why)),
            };
            let bytes = b.rest().to_vec();
            if bytes.len() > MOST_BYTES {
                return Work::Now(err("more than 512 KiB at once"));
            }
            Work::Worker(Box::new(move |s, _| answer(s.blobs().append(id, &bytes).map(|_| vec![]))))
        }
        "blob/read" => {
            let parsed = (|| Ok::<_, String>((b.u32()?, b.u64()?, b.u32()?)))();
            match parsed.and_then(|v| b.end().map(|_| v)) {
                Ok((_, _, len)) if len as usize > MOST_BYTES => {
                    Work::Now(err("more than 512 KiB at once"))
                }
                Ok((id, offset, len)) => Work::Worker(Box::new(move |s, _| {
                    answer(s.blobs().read(id, offset, len as usize))
                })),
                Err(why) => Work::Now(err(&why)),
            }
        }
        "blob/len" | "blob/release" => {
            let id = match b.u32().and_then(|id| b.end().map(|_| id)) {
                Ok(id) => id,
                Err(why) => return Work::Now(err(&why)),
            };
            if name == "blob/len" {
                Work::Worker(Box::new(move |s, _| {
                    answer(s.blobs().get(id).map(|b| (b.len() as u64).to_le_bytes().to_vec()))
                }))
            } else {
                Work::Worker(Box::new(move |s, _| answer(s.blobs().release(id).map(|_| vec![]))))
            }
        }
        _ => Work::Now(Answer::Failed),
    }
}

/// A change to a fake site's URL, as std's `testing::fake` writes it: the URL, then what to
/// change: 0 a status (a `u32`), 1 missing, 2 bytes (the rest), each in place of the one set
/// before; 3 a delay (an `f64`'s bits); 4 nothing faked; 5 how many of the next requests fail
/// in transport (a `u32`).
#[allow(clippy::type_complexity)]
fn testing_fake(b: &mut Body) -> Result<(String, Box<dyn FnOnce(&mut Fake)>), String> {
    let url = b.str()?;
    super::http::target(&url)?;
    let change: Box<dyn FnOnce(&mut Fake)> = match b.u8()? {
        0 => {
            let status = b.u32()?;
            if !(100..=999).contains(&status) {
                return Err(format!("{status} isn't a status: 100 to 999"));
            }
            Box::new(move |f| f.answer = Some(Faked::Status(status as u16)))
        }
        1 => Box::new(|f| f.answer = Some(Faked::Missing)),
        2 => {
            let bytes = b.rest().to_vec();
            if bytes.len() > MOST_BYTES {
                return Err("a fake's bytes are at most 512 KiB".into());
            }
            Box::new(move |f| f.answer = Some(Faked::Bytes(bytes)))
        }
        3 => {
            let delay = f64::from_bits(b.u64()?);
            if !(0.0..=super::http::LONGEST).contains(&delay) {
                return Err(format!("a delay is 0 to {} seconds", super::http::LONGEST));
            }
            Box::new(move |f| f.delay = delay)
        }
        4 => Box::new(|f| *f = Fake::default()),
        5 => {
            let fails = b.u32()?;
            Box::new(move |f| f.fails = fails)
        }
        k => return Err(format!("{k} isn't a fake")),
    };
    b.end()?;
    Ok((url, change))
}

/// An entry as `files::read_list` and `read_entry` read it: (the name), the kind, the size.
fn write_entry(out: &mut Vec<u8>, e: &Entry, named: bool) {
    if named {
        out.extend_from_slice(&(e.name.len() as u32).to_le_bytes());
        out.extend_from_slice(e.name.as_bytes());
    }
    out.push(e.kind as u8);
    out.extend_from_slice(&e.size.to_le_bytes());
}

/// The file services that take a path alone.
fn files(s: &Services, request: u32, name: &str, path: &str) -> Answer {
    let c = &s.caps;
    answer(match name {
        "files/read" => c.read(path),
        "files/list" => c.list(path).map(|entries| {
            let mut out = (entries.len() as u32).to_le_bytes().to_vec();
            for e in &entries {
                write_entry(&mut out, e, true);
            }
            out
        }),
        "files/stat" => c.stat(path).map(|e| {
            let mut out = Vec::new();
            write_entry(&mut out, &e, false);
            out
        }),
        "files/remove" => c.remove(path).map(|_| vec![]),
        "files/remove_all" => c.remove_all(path).map(|_| vec![]),
        "files/make_dirs" => c.make_dirs(path).map(|_| vec![]),
        "files/read_blob" => read_blob(s, request, path),
        _ => unreachable!("routed only the file services"),
    })
}

/// The file at `path` as a new blob, made for `request`: its handle. A file that doesn't fit the
/// budget is refused before it's read; one that fails part way releases its blob.
fn read_blob(s: &Services, request: u32, path: &str) -> Result<Vec<u8>, String> {
    let id = s.blobs().create(request)?;
    let read =
        s.caps.read_chunks(path, |size| s.blobs().fits(size), |chunk| s.blobs().append(id, chunk));
    match read {
        Ok(()) => Ok(id.to_le_bytes().to_vec()),
        Err(why) => {
            let _ = s.blobs().release(id);
            Err(why)
        }
    }
}

/// Services with no grants and `budget`, for a test of one service.
#[cfg(test)]
pub(crate) fn for_test(budget: super::BlobBudget) -> Services {
    Services {
        caps: Caps::open(&super::caps::Grants::default(), |_| None).expect("no grants"),
        blobs: Mutex::new(Blobs::new(budget)),
        io_delay: Duration::ZERO,
        closing: Arc::new(AtomicBool::new(false)),
        testing: false,
        fakes: Mutex::new(HashMap::new()),
        work: OnceLock::new(),
        work_at: None,
        groups: None,
        spawn_pause: Duration::ZERO,
        resolve_stall: Duration::ZERO,
        grace: Duration::ZERO,
        stuck: Arc::new(Stuck::new(Duration::ZERO)),
        budget: None,
        toolchain: None,
        drives: Mutex::default(),
        codes: super::drive::Codes::default(),
        drive_hold: Duration::ZERO,
        compile_hold: Duration::ZERO,
        servers: Mutex::default(),
        browsers: Mutex::default(),
        answers: Mutex::new(None),
        hold: None,
        write_fail: None,
        fail_server_start: false,
        image_stall: Duration::ZERO,
        signal: None,
        strays: Mutex::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::BlobBudget;

    /// A reader of `left` zeros that counts what it gave.
    struct Counted {
        left: usize,
        given: usize,
    }

    impl std::io::Read for Counted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.left);
            buf[..n].fill(0);
            self.left -= n;
            self.given += n;
            Ok(n)
        }
    }

    #[test]
    fn a_body_past_the_budget_is_refused_as_it_arrives_and_its_blob_given_back() {
        let services = for_test(BlobBudget { bytes: 1 << 20, handles: 4 });
        let mut from = Counted { left: 64 << 20, given: 0 };
        let mut out = Vec::new();
        let read = services.put_read(1, &mut out, &mut from, u64::MAX, "the body");
        assert!(read.unwrap_err().contains("budget"));
        assert!(from.given <= 2 << 20, "refused within a chunk of the budget: {}", from.given);
        assert_eq!(services.blobs().release_all(), (0, 0), "its blob is given back");
        // Within the budget, past what an answer holds: a blob.
        let mut from = Counted { left: 600 << 10, given: 0 };
        services.put_read(2, &mut out, &mut from, u64::MAX, "the body").expect("it fits");
        assert_eq!(out[0], 1, "a blob");
        assert_eq!(services.blobs().release_all(), (1, 600 << 10));
    }
}
