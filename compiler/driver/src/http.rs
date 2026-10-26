//! The HTTP of `wrela run`'s local server (`live.rs`):
//! the protocol is `wrela_host::loopback`'s, which a tool's server (`std::tool::serve`) speaks
//! too: 127.0.0.1 only, a host that names this machine, posts only from the server's own pages,
//! and files reached through a directory handle following no link. This is how the server
//! listens, reads requests, and serves files through its held output directory.

use std::io::{self, BufReader, Read};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use wrela_host::loopback::{self, DeadlineIo};

pub(crate) use wrela_host::loopback::{listen, respond};

/// A slow peer may occupy one request thread for at most this long, including its body.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

/// A local client cannot make the live server spawn an unbounded number of threads.
const MAX_ACTIVE: usize = 64;

/// A request: its method, its path and query, and its body, not yet read: the reader (with
/// what it read past the head) and the body's length.
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    body: Option<(BufReader<DeadlineIo>, usize)>,
}

impl Request {
    /// The value of `key` in the query (`?key=value&...`).
    pub fn param(&self, key: &str) -> Option<&str> {
        self.query.split('&').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
    }

    /// The body, read whole: `None` if it's longer than `max` bytes (none of it is read), or
    /// was taken already.
    pub fn read_body(&mut self, max: usize) -> io::Result<Option<Vec<u8>>> {
        match self.body.take() {
            Some((mut reader, length)) if length <= max => {
                let mut body = vec![0; length];
                reader.read_exact(&mut body)?;
                Ok(Some(body))
            }
            _ => Ok(None),
        }
    }
}

/// Answers `listener`'s connections from a thread of its own, each with `handle` on a thread of
/// its own, until `stopping` is set ([`stop`] sets it).
pub(crate) fn accept(
    listener: TcpListener,
    stopping: Arc<AtomicBool>,
    handle: impl Fn(TcpStream) + Clone + Send + 'static,
) {
    accept_limited(listener, stopping, handle, MAX_ACTIVE);
}

struct Active(Arc<AtomicUsize>);

impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn accept_limited(
    listener: TcpListener,
    stopping: Arc<AtomicBool>,
    handle: impl Fn(TcpStream) + Clone + Send + 'static,
    limit: usize,
) {
    let active = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if stopping.load(Ordering::SeqCst) {
                break;
            }
            if active
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < limit).then_some(n + 1))
                .is_err()
            {
                let mut out = stream;
                let _ = out.set_write_timeout(Some(REQUEST_DEADLINE));
                let _ = respond(&mut out, 503, "text/plain", b"too many open requests");
                continue;
            }
            if stream.set_write_timeout(Some(REQUEST_DEADLINE)).is_err() {
                active.fetch_sub(1, Ordering::AcqRel);
                continue;
            }
            let handle = handle.clone();
            let slot = Active(Arc::clone(&active));
            // If spawning fails, the moved slot is dropped and its count released.
            let _ = std::thread::Builder::new().spawn(move || {
                let _slot = slot;
                handle(stream);
            });
        }
    });
}

/// Stops the server on `port` whose connections [`accept`] answers until `stopping`: false if
/// it was stopped already.
pub(crate) fn stop(port: u16, stopping: &AtomicBool) -> bool {
    if stopping.swap(true, Ordering::SeqCst) {
        return false;
    }
    // Wakes the accepting thread, which then sees it's stopping.
    let _ = TcpStream::connect(("127.0.0.1", port));
    true
}

/// Reads a request from `stream`, for a server on `port`: `None` if it was refused (the
/// refusal is answered).
pub(crate) fn read(stream: &TcpStream, port: u16) -> std::io::Result<Option<Request>> {
    read_with_timeout(stream, port, REQUEST_DEADLINE)
}

fn read_with_timeout(
    stream: &TcpStream,
    port: u16,
    timeout: Duration,
) -> io::Result<Option<Request>> {
    let input = DeadlineIo {
        stream: stream.try_clone()?,
        due: Instant::now() + timeout,
        idle: Duration::MAX,
    };
    let mut reader = BufReader::new(input);
    let mut out = stream.try_clone()?;
    let Some(head) = loopback::read_head(&mut reader, &mut out, port)? else { return Ok(None) };
    // The body is read, or refused, by what answers the request.
    let body = Some((reader, head.length));
    Ok(Some(Request { method: head.method, path: head.path, query: head.query, body }))
}

/// Writes a test-mode result (`PUT /results/<name>`) under `root`: a plain file name only.
/// Results may be 256 MiB, so the body is copied from the request straight into the result's
/// temporary file.
pub(crate) fn put_result(
    out: &mut TcpStream,
    root: &cap_std::fs::Dir,
    name: &str,
    request: &mut Request,
) -> std::io::Result<()> {
    let Some((reader, length)) = request.body.as_mut() else {
        return respond(out, 400, "text/plain", b"a result needs a PUT body");
    };
    put_result_from(out, root, name, reader, *length as u64)
}

fn put_result_from(
    out: &mut TcpStream,
    root: &cap_std::fs::Dir,
    name: &str,
    body: &mut dyn Read,
    length: u64,
) -> std::io::Result<()> {
    loopback::put_result(out, root, name, body, length).map(|_| ())
}

/// Answers a GET of the file at `rel` in `dir`, an `index.html` as `page` makes it of the
/// file's text: not found if there's none, or `rel` isn't a plain path in `dir`.
pub(crate) fn get_file(
    out: &mut TcpStream,
    dir: &cap_std::fs::Dir,
    rel: &str,
    page: impl Fn(&str) -> String,
) -> std::io::Result<()> {
    loopback::get_file(out, dir, rel, Some(&page), loopback::Caching::NoStore)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    fn open(path: &Path) -> cap_std::fs::Dir {
        cap_std::fs::Dir::open_ambient_dir(path, cap_std::ambient_authority())
            .expect("open test directory")
    }

    #[test]
    fn put_results_stream_from_the_request_into_the_file() {
        let root =
            std::env::temp_dir().join(format!("wrela-http-stream-put-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("result root");
        let root_dir = open(&root);
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listen");
        let port = listener.local_addr().expect("port").port();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
            write!(
                stream,
                "PUT /results/shoot.txt HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 5\r\n\r\nhello"
            )
            .expect("request");
            let mut response = String::new();
            stream.read_to_string(&mut response).expect("response");
            response
        });
        let (mut stream, _) = listener.accept().expect("accept");
        let mut request = read(&stream, port).expect("request").expect("allowed");
        put_result(&mut stream, &root_dir, "shoot.txt", &mut request).expect("write result");
        drop(request);
        drop(stream);
        assert!(client.join().expect("response").starts_with("HTTP/1.1 201"));
        assert_eq!(std::fs::read(root.join("results/shoot.txt")).expect("result"), b"hello");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_large_put_header_does_not_allocate_its_body() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listen");
        let port = listener.local_addr().expect("port").port();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
            write!(
                stream,
                "PUT /results/shoot.txt HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n",
                loopback::MAX_RESULT
            )
            .expect("request head");
        });
        let (stream, _) = listener.accept().expect("accept");
        let mut request = read(&stream, port).expect("request").expect("allowed");
        assert_eq!(request.body.as_ref().map(|(_, n)| *n), Some(loopback::MAX_RESULT));
        assert!(
            request.read_body(loopback::MAX_RESULT - 1).expect("refused").is_none(),
            "a body over its limit isn't read"
        );
        client.join().expect("client");
    }

    #[test]
    fn too_many_open_connections_are_refused() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listen");
        let port = listener.local_addr().expect("port").port();
        let stopping = Arc::new(AtomicBool::new(false));
        let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let held = Arc::clone(&gate);
        accept_limited(
            listener,
            Arc::clone(&stopping),
            move |_stream| {
                started_tx.send(()).expect("started");
                let (lock, changed) = &*held;
                let open = lock.lock().expect("gate");
                drop(changed.wait_while(open, |open| !*open).expect("gate"));
            },
            2,
        );
        let first = TcpStream::connect(("127.0.0.1", port)).expect("first");
        let second = TcpStream::connect(("127.0.0.1", port)).expect("second");
        for _ in 0..2 {
            started_rx.recv_timeout(Duration::from_secs(3)).expect("admitted connection");
        }
        let mut third = TcpStream::connect(("127.0.0.1", port)).expect("third");
        third.set_read_timeout(Some(Duration::from_secs(3))).expect("read timeout");
        let mut response = String::new();
        third.read_to_string(&mut response).expect("refusal");
        assert!(response.starts_with("HTTP/1.1 503"), "{response}");
        assert!(started_rx.try_recv().is_err(), "the extra request was not handled");
        let (lock, changed) = &*gate;
        *lock.lock().expect("gate") = true;
        changed.notify_all();
        drop(first);
        drop(second);
        stop(port, &stopping);
    }

    #[test]
    fn a_trickling_request_expires_at_its_deadline() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listen");
        let port = listener.local_addr().expect("port").port();
        let stop_sending = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let send_stop = Arc::clone(&stop_sending);
        let send_finished = Arc::clone(&finished);
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
            let until = Instant::now() + Duration::from_secs(2);
            while Instant::now() < until && !send_stop.load(Ordering::Acquire) {
                if stream.write_all(b"G").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            send_finished.store(true, Ordering::Release);
        });
        let (stream, _) = listener.accept().expect("accept");
        let result = read_with_timeout(&stream, port, Duration::from_millis(100));
        assert!(result.is_err(), "an incomplete request cannot succeed");
        assert!(!finished.load(Ordering::Acquire), "the peer was still sending at the deadline");
        stop_sending.store(true, Ordering::Release);
        client.join().expect("client");
    }

    #[test]
    fn percent_encoded_get_paths_are_decoded_once_and_checked() {
        let root = std::env::temp_dir().join(format!("wrela-http-escapes-{}", std::process::id()));
        let build = root.join("build");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&build).expect("build");
        std::fs::write(build.join("game%logic.js"), "literal percent").expect("script");
        std::fs::write(build.join("%2e%2e.txt"), "decoded once").expect("escaped name");
        std::fs::write(build.join("..txt"), "decoded twice").expect("other name");
        std::fs::write(root.join("secret.txt"), "private").expect("outside file");
        let build_dir = open(&build);

        let answer = |target: &str| {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listen");
            let port = listener.local_addr().expect("port").port();
            let target = target.to_string();
            let client = std::thread::spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
                write!(stream, "GET /{target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n")
                    .expect("request");
                let mut response = String::new();
                stream.read_to_string(&mut response).expect("response");
                response
            });
            let (mut stream, _) = listener.accept().expect("accept");
            let request = read(&stream, port).expect("read request").expect("allowed request");
            get_file(&mut stream, &build_dir, request.path.trim_start_matches('/'), str::to_string)
                .expect("answer GET");
            // The request's unread body holds the connection open too.
            drop(request);
            drop(stream);
            client.join().expect("response")
        };

        let script = answer("game%25logic.js");
        assert!(script.starts_with("HTTP/1.1 200"), "{script}");
        assert!(script.ends_with("literal percent"), "{script}");
        let once = answer("%252e%252e.txt");
        assert!(once.starts_with("HTTP/1.1 200"), "{once}");
        assert!(once.ends_with("decoded once"), "{once}");
        for target in ["%2e%2e/secret.txt", "%2e%2e%2fsecret.txt", "bad%escape.js"] {
            let response = answer(target);
            assert!(response.starts_with("HTTP/1.1 404"), "{target}: {response}");
            assert!(!response.contains("private"), "{target}: {response}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn linked_files_are_not_served_or_written() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!("wrela-http-links-{}", std::process::id()));
        let outside =
            std::env::temp_dir().join(format!("wrela-http-outside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&dir).expect("page");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(outside.join("secret.txt"), "private").expect("secret");
        symlink(outside.join("secret.txt"), dir.join("visible.txt")).expect("linked file");
        symlink(&outside, dir.join("results")).expect("linked results");
        let held = open(&dir);

        let answer = |serve: &dyn Fn(&mut TcpStream)| {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listen");
            let port = listener.local_addr().expect("port").port();
            let client = std::thread::spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
                let mut response = String::new();
                stream.read_to_string(&mut response).expect("read response");
                response
            });
            let (mut stream, _) = listener.accept().expect("accept");
            serve(&mut stream);
            drop(stream);
            client.join().expect("response")
        };
        let served = answer(&|stream| {
            get_file(stream, &held, "visible.txt", str::to_string).expect("answer GET");
        });
        assert!(served.starts_with("HTTP/1.1 404"), "{served}");
        assert!(!served.contains("private"));
        let written = answer(&|stream| {
            put_result_from(stream, &held, "new.txt", &mut &b"bad"[..], 3).expect("answer PUT");
        });
        assert!(written.starts_with("HTTP/1.1 403"), "{written}");
        assert!(!outside.join("new.txt").exists());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_output_path_cannot_redirect_served_files_or_results() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("wrela-http-held-{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!("wrela-http-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&root).expect("output");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(root.join("index.html"), "original").expect("original page");
        std::fs::write(outside.join("index.html"), "private").expect("other page");
        let held = open(&root);
        let moved = root.with_extension("moved");
        let _ = std::fs::remove_dir_all(&moved);
        std::fs::rename(&root, &moved).expect("move output");
        symlink(&outside, &root).expect("replace output path");

        let answer = |serve: &dyn Fn(&mut TcpStream)| {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listen");
            let port = listener.local_addr().expect("port").port();
            let client = std::thread::spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
                let mut response = String::new();
                stream.read_to_string(&mut response).expect("response");
                response
            });
            let (mut stream, _) = listener.accept().expect("accept");
            serve(&mut stream);
            drop(stream);
            client.join().expect("client")
        };
        let served =
            answer(&|out| get_file(out, &held, "index.html", str::to_string).expect("GET"));
        assert!(served.ends_with("original"), "{served}");
        assert!(!served.contains("private"));
        let written = answer(&|out| {
            put_result_from(out, &held, "proof.txt", &mut &b"safe"[..], 4).expect("PUT");
        });
        assert!(written.starts_with("HTTP/1.1 201"), "{written}");
        assert_eq!(std::fs::read(moved.join("results/proof.txt")).expect("result"), b"safe");
        assert!(!outside.join("results/proof.txt").exists());
        std::fs::remove_file(&root).expect("remove link");
        let _ = std::fs::remove_dir_all(moved);
        let _ = std::fs::remove_dir_all(outside);
    }
}
