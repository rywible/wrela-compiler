//! The toolchain's local servers' HTTP (the studio's, `serve.rs`, and `wrela run`'s, `live.rs`):
//! the protocol is `wrela_host::loopback`'s, which a tool's server (`std::tool::serve`) speaks
//! too: 127.0.0.1 only, a host that names this machine, posts only from the server's own pages,
//! and files reached through a directory handle following no link. This is how these servers
//! listen, read a whole request, and serve a directory by its path.

use std::io::{BufReader, Read};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wrela_host::loopback;

pub(crate) use wrela_host::loopback::{listen, respond};

/// A request: its method, its path and query, and its body.
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub body: Vec<u8>,
}

impl Request {
    /// The value of `key` in the query (`?key=value&...`).
    pub fn param(&self, key: &str) -> Option<&str> {
        self.query.split('&').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
    }
}

/// Answers `listener`'s connections from a thread of its own, each with `handle` on a thread of
/// its own, until `stopping` is set ([`stop`] sets it).
pub(crate) fn accept(
    listener: TcpListener,
    stopping: Arc<AtomicBool>,
    handle: impl Fn(TcpStream) + Clone + Send + 'static,
) {
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if stopping.load(Ordering::SeqCst) {
                break;
            }
            let handle = handle.clone();
            std::thread::spawn(move || handle(stream));
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
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream.try_clone()?;
    let Some(head) = loopback::read_head(&mut reader, &mut out, port)? else { return Ok(None) };
    let mut body = vec![0u8; head.length];
    reader.read_exact(&mut body)?;
    Ok(Some(Request { method: head.method, path: head.path, query: head.query, body }))
}

/// The directory `root`, opened, if it isn't a symbolic link itself.
fn open(root: &Path) -> Option<cap_std::fs::Dir> {
    if std::fs::symlink_metadata(root).ok()?.file_type().is_symlink() {
        return None;
    }
    cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority()).ok()
}

/// Writes a test-mode result (`PUT /results/<name>`) under `root`: a plain file name only.
pub(crate) fn put_result(
    out: &mut TcpStream,
    root: &Path,
    name: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let _ = std::fs::create_dir_all(root);
    let Some(dir) = open(root) else {
        return respond(out, 403, "text/plain", b"a result path cannot contain a symbolic link");
    };
    loopback::put_result(out, &dir, name, &mut &body[..], body.len() as u64).map(|_| ())
}

/// Answers a GET of the file at `rel` in `dir`, an `index.html` as `page` makes it of the
/// file's text: not found if there's none, or `rel` isn't a plain path in `dir`.
pub(crate) fn get_file(
    out: &mut TcpStream,
    dir: &Path,
    rel: &str,
    page: impl Fn(&str) -> String,
) -> std::io::Result<()> {
    let Some(at) = open(dir) else { return respond(out, 404, "text/plain", b"not found") };
    loopback::get_file(out, &at, rel, Some(&page))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
            get_file(&mut stream, &build, request.path.trim_start_matches('/'), str::to_string)
                .expect("answer GET");
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
            get_file(stream, &dir, "visible.txt", str::to_string).expect("answer GET");
        });
        assert!(served.starts_with("HTTP/1.1 404"), "{served}");
        assert!(!served.contains("private"));
        let written = answer(&|stream| {
            put_result(stream, &dir, "new.txt", b"bad").expect("answer PUT");
        });
        assert!(written.starts_with("HTTP/1.1 403"), "{written}");
        assert!(!outside.join("new.txt").exists());
        symlink(&outside, dir.join("linked-page")).expect("linked page");
        let written = answer(&|stream| {
            put_result(stream, &dir.join("linked-page"), "new.txt", b"bad")
                .expect("answer PUT through linked page");
        });
        assert!(written.starts_with("HTTP/1.1 403"), "{written}");
        assert!(!outside.join("new.txt").exists());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(outside);
    }
}
