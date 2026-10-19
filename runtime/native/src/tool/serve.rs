//! `std::tool::serve` (language.md §15.5): a local static server over a directory in a root, on
//! 127.0.0.1, with the toolchain's servers' rules (`crate::loopback`): a request must name this
//! machine as its host, and a post must come from the server's own pages. A GET is answered
//! with the file, through the directory's handle, following no link. A PUT of
//! `/results/<name>` writes the page's result, if the root is a write root, and is refused in a
//! read root. A POST goes to the tool: `next_post` gives it, and `answer` answers it.
//!
//! **A server is a resource of its request.** `start` registers it under the request before the
//! answer that names it reaches the tool: until the tool reads that answer it's the request's,
//! and if the request fails, or the tool ends first, the host closes it. The host closes every
//! server when the tool ends: the listener and every connection are shut, and every thread of
//! the server's is joined within the grace (if one isn't, the run is the host's failure), so the
//! port is closed once the run returns.
//!
//! **Posts wait on the tool, not on a worker.** A post's body is charged to the tool's blob
//! budget as soon as its length is known, before it's read; it's queued, and given to the
//! oldest waiting `next_post` (inline if at most 512 KiB, else as a blob of that request's).
//! A post nobody answers fails at its deadline (from when the server took it) with 504, and a
//! `next_post` that no post comes for fails at its own. Both are on the system's clock, also in
//! a tool test.

use super::services::{Answer, Body, Data, Services, Work};
use super::toolchain::Out;
use cap_std::fs::Dir;
use std::collections::{HashMap, VecDeque};
use std::io::BufReader;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A post's deadline unless `start` gives another.
const POST_DEADLINE: Duration = Duration::from_secs(30);

/// A `next_post`'s deadline unless it asks for another.
const NEXT_DEADLINE: Duration = Duration::from_secs(60);

/// The most connections a server has open at once: past them, a new one is closed unread.
const MOST_CONNECTIONS: usize = 64;

/// The most posts a server holds at once, queued or given to the tool.
const MOST_POSTS: usize = 64;

/// How long a connection's read or write may wait.
const IDLE: Duration = Duration::from_secs(10);

/// How often the server's thread looks for a connection, its end and its waiters' deadlines.
const POLL: Duration = Duration::from_millis(10);

/// The servers of one run: each by its handle, and each request's that the tool hasn't been
/// told of.
#[derive(Default)]
pub(crate) struct Servers {
    held: HashMap<u32, Arc<Server>>,
    next: u32,
    unpublished: HashMap<u32, Vec<u32>>,
}

impl Servers {
    /// The tool has read request `request`'s answer: its server is the tool's now.
    pub(crate) fn publish(&mut self, request: u32) {
        self.unpublished.remove(&request);
    }

    /// Request `request` failed: the servers made for it, which no answer names, to close.
    pub(crate) fn rollback(&mut self, request: u32) -> Vec<Arc<Server>> {
        let ids = self.unpublished.remove(&request).unwrap_or_default();
        ids.iter().filter_map(|id| self.held.remove(id)).collect()
    }

    /// The ports of the servers running: what a browser of the tool's may reach.
    pub(crate) fn ports(&self) -> Vec<u16> {
        self.held.values().map(|s| s.port).collect()
    }

    /// The server on `port`, if one runs there.
    pub(crate) fn on_port(&self, port: u16) -> Option<Arc<Server>> {
        self.held.values().find(|s| s.port == port).cloned()
    }

    /// Takes every server, to close: the tool has ended.
    pub(crate) fn take_all(&mut self) -> Vec<Arc<Server>> {
        self.unpublished.clear();
        self.held.drain().map(|(_, s)| s).collect()
    }
}

/// A post the server holds: its number, what it was sent to, its body, the bytes charged for
/// it, where its answer goes, and its deadline.
struct Post {
    id: u32,
    path: String,
    body: Vec<u8>,
    charged: u64,
    reply: mpsc::Sender<Arc<Vec<u8>>>,
    due: Instant,
}

/// Where a post's answer goes, and by when.
type Reply = (mpsc::Sender<Arc<Vec<u8>>>, Instant);

/// What a server's threads share and change.
#[derive(Default)]
struct State {
    next_post: u32,
    /// Posts not given to the tool yet, oldest first.
    queued: VecDeque<Post>,
    /// Posts given to the tool, waiting for its answer, by number.
    given: HashMap<u32, Reply>,
    /// `next_post` requests waiting for a post, oldest first, with their deadlines.
    waiting: VecDeque<(u32, Instant, Duration)>,
    closed: bool,
}

impl State {
    /// The oldest waiting `next_post` and the oldest queued post, if both are there and neither
    /// is past its deadline at `now`; and the bytes charged for the posts passed over, past
    /// theirs, which are dropped (their connections answer 504), to give back. A waiter past its
    /// deadline is left for the server's thread to fail.
    fn pair(&mut self, now: Instant) -> (Option<(u32, Post)>, u64) {
        let mut refund = 0;
        while self.queued.front().is_some_and(|p| p.due <= now) {
            refund += self.queued.pop_front().expect("a post").charged;
        }
        let ready = self.waiting.front().is_some_and(|(_, due, _)| *due > now);
        if !ready || self.queued.is_empty() {
            return (None, refund);
        }
        let (request, _, _) = self.waiting.pop_front().expect("a waiter");
        (Some((request, self.queued.pop_front().expect("a post"))), refund)
    }

    /// Where post `id`'s answer goes, if it waits for one at `now`: not past its deadline.
    fn claim(&mut self, id: u32, now: Instant) -> Result<mpsc::Sender<Arc<Vec<u8>>>, String> {
        match self.given.remove(&id) {
            Some((reply, due)) if due > now => Ok(reply),
            Some(_) => Err(format!("post {id}'s deadline has passed: its page was answered 504")),
            None => Err(format!(
                "post {id} isn't waiting for an answer: it was answered, or its deadline passed, or its connection closed"
            )),
        }
    }
}

/// A running server.
pub(crate) struct Server {
    port: u16,
    /// The served directory's handle, and whether its root is a write root.
    dir: Dir,
    writable: bool,
    post_deadline: Duration,
    state: Mutex<State>,
    stopping: AtomicBool,
    /// The open connections, to shut when it closes.
    conns: Mutex<HashMap<u64, TcpStream>>,
    /// Its threads: the one that accepts, and each connection's.
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// The `results/DONE` it has kept for each browser session (the session its browser's proxy
    /// named the put with), or why it can't be read whole: a browser's page has put its result
    /// once its session's is here.
    done: Mutex<HashMap<String, Result<String, String>>>,
}

impl Server {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The `results/DONE` it has kept for browser session `session`, if it has.
    pub(crate) fn done(&self, session: &str) -> Option<Result<String, String>> {
        self.done.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(session).cloned()
    }

    /// Closes it: its waiters fail, its posts' bytes are given back, its listener and
    /// connections are shut, and its threads are joined. Whether they ended within `grace`.
    pub(crate) fn close(&self, s: &Services, grace: Duration) -> bool {
        self.stopping.store(true, Ordering::SeqCst);
        {
            let mut st = self.state();
            st.closed = true;
            for (request, _, _) in st.waiting.drain(..) {
                s.send(request, failure("the server closed"));
            }
            let charged: u64 = st.queued.drain(..).map(|p| p.charged).sum();
            s.blobs().refund(charged);
            st.given.clear();
        }
        for c in self.conns.lock().unwrap_or_else(std::sync::PoisonError::into_inner).values() {
            let _ = c.shutdown(Shutdown::Both);
        }
        let until = Instant::now() + grace;
        loop {
            let threads = std::mem::take(
                &mut *self.threads.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            if threads.is_empty() {
                return true;
            }
            for t in threads {
                while !t.is_finished() {
                    if Instant::now() >= until {
                        return false;
                    }
                    std::thread::sleep(POLL);
                }
                let _ = t.join();
            }
        }
    }

    /// Gives queued posts to waiting `next_post`s, oldest to oldest, for as long as both are
    /// there.
    fn deliver(&self, s: &Services, st: &mut State) {
        loop {
            let (pair, refund) = st.pair(Instant::now());
            s.blobs().refund(refund);
            let Some((request, post)) = pair else { return };
            let mut out = Out::default();
            out.u32(post.id).str(&post.path);
            if post.body.len() <= super::caps::MOST_BYTES {
                out.u8(0).u32(post.body.len() as u32);
                out.0.extend_from_slice(&post.body);
                s.blobs().refund(post.charged);
            } else {
                match s.blobs().adopt(request, post.body) {
                    Ok(id) => {
                        out.u8(1).u32(id);
                    }
                    Err((why, body)) => {
                        // The post stays first, its bytes still charged, for the next waiter.
                        st.queued.push_front(Post { body, ..post });
                        s.send(request, failure(&format!("the post's body: {why}")));
                        continue;
                    }
                }
            }
            st.given.insert(post.id, (post.reply, post.due));
            s.send(request, super::services::answer(Ok(out.0)));
        }
    }
}

/// The most of a `results/DONE` a browser's result is read as: one longer is written, and its
/// browser's result is an error that says so, never the part that fits.
const MOST_DONE: usize = 1 << 20;

/// A reader that keeps what it gives, up to [`MOST_DONE`], and counts all of it: a result's
/// body as it's written.
struct Tee<'a> {
    from: &'a mut dyn std::io::Read,
    kept: Vec<u8>,
    total: u64,
}

impl std::io::Read for Tee<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.from.read(buf)?;
        let room = MOST_DONE.saturating_sub(self.kept.len());
        self.kept.extend_from_slice(&buf[..n.min(room)]);
        self.total += n as u64;
        Ok(n)
    }
}

fn failure(why: &str) -> Answer {
    super::services::answer(Err(why.to_string()))
}

fn failed(why: String) -> Work {
    Work::Now(failure(&why))
}

/// How request `name` of the serve service, with `b`, is carried out.
pub(crate) fn route(name: &str, b: &mut Body) -> Work {
    let entered = Instant::now();
    let parsed = (|| -> Result<Work, String> {
        Ok(match name {
            "start" => {
                let root = b.str()?;
                let port = b.u32()?;
                let port = u16::try_from(port).map_err(|_| format!("{port} isn't a port"))?;
                let posts = super::http::deadline(f64::from_bits(b.u64()?), POST_DEADLINE)?;
                b.end()?;
                Work::Long(Box::new(move |s, request| {
                    super::services::answer(start(s, request, &root, port, posts))
                }))
            }
            "next_post" => {
                let handle = b.u32()?;
                let deadline = super::http::deadline(f64::from_bits(b.u64()?), NEXT_DEADLINE)?;
                b.end()?;
                Work::Later(Box::new(move |s, request| {
                    let server = match find(s, handle) {
                        Ok(server) => server,
                        Err(why) => return Some(failure(&why)),
                    };
                    let mut st = server.state();
                    if st.closed {
                        return Some(failure("the server closed"));
                    }
                    st.waiting.push_back((request, entered + deadline, deadline));
                    server.deliver(s, &mut st);
                    None
                }))
            }
            "answer" => {
                let handle = b.u32()?;
                let post = b.u32()?;
                let data = b.data()?;
                b.end()?;
                Work::Worker(Box::new(move |s, _| {
                    super::services::answer(answer(s, handle, post, data).map(|()| Vec::new()))
                }))
            }
            "close" => {
                let handle = b.u32()?;
                b.end()?;
                Work::Long(Box::new(move |s, _| {
                    let server = s.servers().held.remove(&handle);
                    let Some(server) = server else {
                        return failure(&format!("the tool holds no server {handle}"));
                    };
                    if !server.close(s, s.grace) {
                        let _ = s.stuck.set(format!("the server {handle} didn't close"));
                        return failure(&format!("the server {handle} didn't close"));
                    }
                    super::services::answer(Ok(Vec::new()))
                }))
            }
            _ => return Err(format!("the serve service has no `{name}`")),
        })
    })();
    parsed.unwrap_or_else(failed)
}

/// The handle `handle`'s server.
fn find(s: &Services, handle: u32) -> Result<Arc<Server>, String> {
    s.servers()
        .held
        .get(&handle)
        .cloned()
        .ok_or_else(|| format!("the tool holds no server {handle}"))
}

/// Answers post `post` of `handle`'s server with `data`'s bytes.
fn answer(s: &Services, handle: u32, post: u32, data: Data) -> Result<(), String> {
    let server = find(s, handle)?;
    let bytes = match data {
        Data::Bytes(b) => Arc::new(b),
        Data::Blob(id) => s.blobs().get(id)?,
    };
    let reply = server.state().claim(post, Instant::now())?;
    reply.send(bytes).map_err(|_| format!("post {post}'s connection closed before the answer"))
}

/// Starts a server over the directory at `root` on 127.0.0.1:`port` (0: a free one) for
/// `request`: its handle, port and origin.
fn start(
    s: &Arc<Services>,
    request: u32,
    root: &str,
    port: u16,
    posts: Duration,
) -> Result<Vec<u8>, String> {
    let (_, dir) = s.caps.open_dir(root)?;
    let writable = s.caps.writable(root).is_ok();
    let (listener, port) = crate::loopback::listen(port)?;
    listener.set_nonblocking(true).map_err(|e| format!("the server's listener: {e}"))?;
    let server = Arc::new(Server {
        port,
        dir,
        writable,
        post_deadline: posts,
        state: Mutex::new(State::default()),
        stopping: AtomicBool::new(false),
        conns: Mutex::new(HashMap::new()),
        threads: Mutex::new(Vec::new()),
        done: Mutex::new(HashMap::new()),
    });
    // Registered under its request before anything runs, and before any answer names it.
    let handle = {
        let mut servers = s.servers();
        servers.next += 1;
        let handle = servers.next;
        servers.held.insert(handle, server.clone());
        servers.unpublished.entry(request).or_default().push(handle);
        handle
    };
    let (services, accepting) = (s.clone(), server.clone());
    let spawned = std::thread::Builder::new()
        .name(format!("wrela-serve-{handle}"))
        .spawn(move || accept(&services, &accepting, listener));
    // A failed start's server is closed by the worker, which rolls the request back.
    let thread = spawned.map_err(|e| format!("can't start the server's thread: {e}"))?;
    server.threads.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(thread);
    s.signal("listening");
    if s.fail_server_start {
        return Err("a test's failure, once the server listens".into());
    }
    let mut out = Out::default();
    out.u32(handle).u32(u32::from(port)).str(&format!("http://127.0.0.1:{port}"));
    Ok(out.0)
}

/// The server's own thread: it takes connections until the server closes, and fails each
/// `next_post` whose deadline passes.
fn accept(s: &Arc<Services>, server: &Arc<Server>, listener: TcpListener) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    while !server.stopping.load(Ordering::SeqCst) {
        expire(s, server);
        match listener.accept() {
            Ok((stream, _)) => {
                // A post's deadline counts from here.
                let taken = Instant::now();
                let mut conns =
                    server.conns.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                // Under the lock `close` shuts the connections in: one taken after it is
                // dropped.
                if server.stopping.load(Ordering::SeqCst) || conns.len() >= MOST_CONNECTIONS {
                    continue;
                }
                let Ok(kept) = stream.try_clone() else { continue };
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                conns.insert(id, kept);
                drop(conns);
                s.signal("connection");
                let (services, serving) = (s.clone(), server.clone());
                let spawned =
                    std::thread::Builder::new().name("wrela-serve-conn".into()).spawn(move || {
                        let _ = connection(&services, &serving, stream, taken);
                        serving
                            .conns
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&id);
                    });
                let mut threads =
                    server.threads.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                threads.retain(|t| !t.is_finished());
                match spawned {
                    Ok(t) => threads.push(t),
                    Err(_) => {
                        server
                            .conns
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&id);
                    }
                }
            }
            // None waiting (`WouldBlock`), or one that went before it was taken.
            Err(_) => std::thread::sleep(POLL),
        }
    }
    // The listener goes with this thread: the port is closed.
}

/// Fails each `next_post` of `server` whose deadline has passed.
fn expire(s: &Services, server: &Server) {
    let now = Instant::now();
    let mut st = server.state();
    if st.waiting.iter().all(|(_, due, _)| *due > now) {
        return;
    }
    let (late, kept): (VecDeque<_>, VecDeque<_>) =
        st.waiting.drain(..).partition(|(_, due, _)| *due <= now);
    st.waiting = kept;
    drop(st);
    for (request, _, deadline) in late {
        let why = format!("no post came within the deadline of {} s", deadline.as_secs_f64());
        s.send(request, failure(&why));
    }
}

/// Answers one connection's request.
fn connection(
    s: &Services,
    server: &Server,
    stream: TcpStream,
    taken: Instant,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IDLE))?;
    stream.set_write_timeout(Some(IDLE))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    let Some(head) = crate::loopback::read_head(&mut reader, &mut out, server.port)? else {
        return Ok(());
    };
    let respond = crate::loopback::respond;
    match head.method.as_str() {
        "GET" => {
            let path = head.path.trim_start_matches('/');
            let rel = if path.is_empty() || path.ends_with('/') {
                format!("{path}index.html")
            } else {
                path.to_string()
            };
            crate::loopback::get_file(&mut out, &server.dir, &rel, None)
        }
        "PUT" => match head.path.strip_prefix("/results/") {
            Some(_) if !server.writable => respond(
                &mut out,
                403,
                "text/plain",
                b"this server's root is granted for reading only: it takes no results",
            ),
            Some(name) => {
                // A test's held write of a result.
                if let Some((which, held)) = &s.hold
                    && format!("results/{name}").ends_with(which.as_str())
                {
                    std::thread::sleep(*held);
                }
                // The body as it's written, so the result kept is this put's own.
                let mut body = Tee { from: &mut reader, kept: Vec::new(), total: 0 };
                let put = crate::loopback::put_result(
                    &mut out,
                    &server.dir,
                    name,
                    &mut body,
                    head.length as u64,
                );
                // Kept: the browser that put it has its result now, not when it asked to.
                if matches!(put, Ok(true))
                    && name == "DONE"
                    && let Some(session) = &head.session
                {
                    let text = if body.total > MOST_DONE as u64 {
                        Err(format!(
                            "the page's results/DONE is {} bytes, more than the {MOST_DONE} a result is read as",
                            body.total
                        ))
                    } else {
                        Ok(String::from_utf8_lossy(&body.kept).trim().to_string())
                    };
                    server
                        .done
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(session.clone(), text);
                }
                put.map(|_| ())
            }
            None => respond(&mut out, 403, "text/plain", b"a page puts only its results/"),
        },
        "POST" => post(s, server, head, &mut reader, &mut out, taken + server.post_deadline),
        _ => respond(&mut out, 405, "text/plain", b"not allowed"),
    }
}

/// A post: its body charged to the budget, then read, queued for the tool, and its answer
/// waited for, all by `due`, its deadline from when the server took its connection: past it, a
/// body still being read, a post still queued, and one whose answer hasn't come are each
/// answered 504, and their bytes given back.
fn post(
    s: &Services,
    server: &Server,
    head: crate::loopback::Head,
    reader: &mut BufReader<TcpStream>,
    out: &mut TcpStream,
    due: Instant,
) -> std::io::Result<()> {
    let respond = crate::loopback::respond;
    let late = |out: &mut TcpStream| {
        let why = format!(
            "the tool didn't answer the post within its deadline of {} s",
            server.post_deadline.as_secs_f64()
        );
        respond(out, 504, "text/plain", why.as_bytes())
    };
    let charged = head.length as u64;
    if let Err(why) = s.blobs().charge(charged) {
        return respond(
            out,
            413,
            "text/plain",
            format!("past the tool's blob budget: {why}").as_bytes(),
        );
    }
    // Given back unless the post's bytes become its queue's, then a blob's.
    struct Charge<'a>(&'a Services, u64);
    impl Drop for Charge<'_> {
        fn drop(&mut self) {
            self.0.blobs().refund(self.1);
        }
    }
    let mut charge = Charge(s, charged);
    let mut body = vec![0; head.length];
    let mut got = 0;
    while got < body.len() {
        let left = due.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return late(out);
        }
        reader.get_ref().set_read_timeout(Some(left.min(IDLE)))?;
        match std::io::Read::read(reader, &mut body[got..]) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if left <= IDLE {
                    continue;
                }
                return Err(e);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let (reply, answered) = mpsc::channel();
    let id = {
        let mut st = server.state();
        if st.closed {
            drop(st);
            return respond(out, 503, "text/plain", b"the server is closing");
        }
        if st.queued.len() + st.given.len() >= MOST_POSTS {
            drop(st);
            return respond(out, 503, "text/plain", b"the tool has as many posts as it holds");
        }
        if Instant::now() >= due {
            drop(st);
            return late(out);
        }
        st.next_post += 1;
        let id = st.next_post;
        let path = if head.query.is_empty() {
            head.path.clone()
        } else {
            format!("{}?{}", head.path, head.query)
        };
        st.queued.push_back(Post { id, path, body, charged, reply, due });
        charge.1 = 0;
        s.signal("post");
        server.deliver(s, &mut st);
        id
    };
    loop {
        match answered.recv_timeout(POLL) {
            Ok(bytes) => return respond(out, 200, "application/octet-stream", &bytes),
            // Dropped past its deadline (`State::pair`), or the server closed.
            Err(mpsc::RecvTimeoutError::Disconnected) if Instant::now() >= due => return late(out),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return respond(out, 503, "text/plain", b"the server closed");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if server.stopping.load(Ordering::SeqCst) {
            return respond(out, 503, "text/plain", b"the server closed");
        }
        if Instant::now() >= due {
            let mut st = server.state();
            // Still queued: its bytes are given back. Given: the tool's answer comes too late.
            if let Some(at) = st.queued.iter().position(|p| p.id == id) {
                let p = st.queued.remove(at).expect("queued");
                s.blobs().refund(p.charged);
            }
            st.given.remove(&id);
            drop(st);
            // An answer the tool gave just before its deadline.
            if let Ok(bytes) = answered.try_recv() {
                return respond(out, 200, "application/octet-stream", &bytes);
            }
            return late(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(id: u32, due: Instant) -> (Post, mpsc::Receiver<Arc<Vec<u8>>>) {
        let (reply, answered) = mpsc::channel();
        let p = Post { id, path: "/p".into(), body: vec![1; 3], charged: 3, reply, due };
        (p, answered)
    }

    #[test]
    fn a_post_past_its_deadline_is_neither_delivered_nor_answered() {
        let now = Instant::now();
        let (soon, later) = (now + Duration::from_secs(1), now + Duration::from_secs(9));
        let mut st = State::default();
        let (expired, dropped) = post(1, soon);
        let (fresh, _kept) = post(2, later);
        st.queued.push_back(expired);
        st.queued.push_back(fresh);
        st.waiting.push_back((7, later, Duration::from_secs(9)));
        // At 2 s: the first post's deadline has passed; it's dropped, its bytes given back, and
        // the waiter gets the second.
        let at = now + Duration::from_secs(2);
        let (pair, refund) = st.pair(at);
        assert_eq!(refund, 3);
        let Some((request, p)) = pair else { panic!("the fresh post is delivered") };
        assert_eq!((request, p.id), (7, 2));
        assert!(dropped.recv().is_err(), "the expired post's connection hears it's gone");
        // An answer before its deadline is taken; one after it isn't.
        st.given.insert(2, (p.reply, later));
        assert!(st.claim(2, at).is_ok());
        let (p3, _r3) = post(3, soon);
        st.given.insert(3, (p3.reply, soon));
        let late = st.claim(3, at).expect_err("past its deadline");
        assert!(late.contains("deadline"), "{late}");
        assert!(st.claim(3, at).is_err(), "and it's gone");
        // A waiter past its own deadline isn't given a post.
        let (p4, _r4) = post(4, later);
        st.queued.push_back(p4);
        st.waiting.push_back((8, soon, Duration::from_secs(1)));
        assert!(st.pair(at).0.is_none());
    }
}
