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

/// Writes a test-mode result (`PUT /results/<name>`) into `results`: a plain file name only.
pub(crate) fn put_result(
    out: &mut TcpStream,
    results: &std::path::Path,
    name: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let plain = !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !name.starts_with('.');
    if !plain {
        return respond(out, 403, "text/plain", b"a result is a plain file name");
    }
    std::fs::create_dir_all(results)?;
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
    if wrela_abi::check::path_problem(rel).is_some() {
        return respond(out, 404, "text/plain", b"not found");
    }
    match std::fs::read(dir.join(rel)) {
        Ok(mut bytes) => {
            if rel == "index.html" {
                bytes = page(&String::from_utf8_lossy(&bytes)).into_bytes();
            }
            respond(out, 200, mime(rel), &bytes)
        }
        Err(_) => respond(out, 404, "text/plain", b"not found"),
    }
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
