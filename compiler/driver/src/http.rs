//! The little HTTP the toolchain's servers speak (the studio's, `serve.rs`, and `wrela run`'s,
//! `live.rs`): one request per connection, on 127.0.0.1 only. A request must name this machine
//! as its host (so a page elsewhere can't reach a server through DNS rebinding), and a post must
//! come from the server's own pages (their `Origin`), so another site open in the browser can't
//! change files. A put (test mode's results) isn't checked for its origin: a browser asks a
//! server before another site's page may put, and these servers answer no such question.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The largest request body a server takes (a test's frame, an edit's JSON).
pub(crate) const MAX_BODY: usize = 16 << 20;

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

/// Listens on 127.0.0.1:`port` (0: a free port): the listener, and its port.
pub(crate) fn listen(port: u16) -> Result<(TcpListener, u16), String> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("can't listen on 127.0.0.1:{port}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    Ok((listener, port))
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
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("/"));
    let mut length = 0usize;
    let (mut host, mut origin) = (String::new(), None::<String>);
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let (k, v) = (k.trim(), v.trim());
            if k.eq_ignore_ascii_case("content-length") {
                length = v.parse().unwrap_or(0);
            } else if k.eq_ignore_ascii_case("host") {
                host = v.to_ascii_lowercase();
            } else if k.eq_ignore_ascii_case("origin") {
                origin = Some(v.to_ascii_lowercase());
            }
        }
    }
    let mut out = stream.try_clone()?;
    let local = ["127.0.0.1", "localhost", "[::1]"];
    if !local.iter().any(|h| host == *h || host == format!("{h}:{port}")) {
        respond(
            &mut out,
            403,
            "text/plain",
            b"this server answers only requests for this machine",
        )?;
        return Ok(None);
    }
    // A post only from this server's own pages.
    if method == "POST" && origin.as_deref() != Some(format!("http://{host}").as_str()) {
        respond(&mut out, 403, "text/plain", b"posts are taken only from this server's pages")?;
        return Ok(None);
    }
    // A body past the cap is refused, not cut short (and then written as if whole).
    if length > MAX_BODY {
        let why = format!("a request's body is at most {MAX_BODY} bytes");
        respond(&mut out, 413, "text/plain", why.as_bytes())?;
        return Ok(None);
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Ok(Some(Request { method, path: path.to_string(), query: query.to_string(), body }))
}

/// Writes a test-mode result (`PUT /results/<name>`) under `root`: a plain file name only.
pub(crate) fn put_result(
    out: &mut TcpStream,
    root: &Path,
    name: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let plain = !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !name.starts_with('.');
    if !plain {
        return respond(out, 403, "text/plain", b"a result is a plain file name");
    }
    let results = root.join("results");
    if crate::ensure_no_symlink_path(root, &results.join(name)).is_err() {
        return respond(out, 403, "text/plain", b"a result path cannot contain a symbolic link");
    }
    std::fs::create_dir_all(&results)?;
    crate::write_atomic(&results.join(name), body)?;
    respond(out, 201, "text/plain", b"")
}

/// Answers a GET of the file at `rel` in `dir`, an `index.html` as `page` makes it of the
/// file's text: not found if there's none, or `rel` isn't a plain path in `dir`.
pub(crate) fn get_file(
    out: &mut TcpStream,
    dir: &Path,
    rel: &str,
    page: impl FnOnce(&str) -> String,
) -> std::io::Result<()> {
    // Request paths still contain URL escapes. Decode once before checking or joining a path:
    // checking the encoded spelling would let `%2e%2e` escape the build directory.
    let Some(rel) = decode_path(rel) else {
        return respond(out, 404, "text/plain", b"not found");
    };
    if wrela_abi::check::path_problem(&rel).is_some()
        || crate::ensure_no_symlink_path(dir, &dir.join(&rel)).is_err()
    {
        return respond(out, 404, "text/plain", b"not found");
    }
    match std::fs::read(dir.join(&rel)) {
        Ok(mut bytes) => {
            if rel == "index.html" {
                bytes = page(&String::from_utf8_lossy(&bytes)).into_bytes();
            }
            respond(out, 200, mime(&rel), &bytes)
        }
        Err(_) => respond(out, 404, "text/plain", b"not found"),
    }
}

fn decode_path(path: &str) -> Option<String> {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let mut decoded = Vec::with_capacity(path.len());
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let value = hex(bytes.next()?)? * 16 + hex(bytes.next()?)?;
            // A URL escape cannot introduce a new path part after routing has selected a file.
            if value == b'/' || value == b'\\' {
                return None;
            }
            decoded.push(value);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).ok()
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("wgsl") => "text/plain; charset=utf-8",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

/// An HTTP response, with the headers that let the page's workers share memory (COOP and
/// COEP, which `SharedArrayBuffer` needs) and keep nothing cached.
pub(crate) fn respond(
    out: &mut TcpStream,
    status: u16,
    kind: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Content Too Large",
        _ => "Method Not Allowed",
    };
    write!(
        out,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nCross-Origin-Opener-Policy: same-origin\r\nCross-Origin-Embedder-Policy: require-corp\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    out.write_all(body)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

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
