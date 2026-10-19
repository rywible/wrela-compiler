//! The little HTTP the toolchain's local servers speak: the studio's and `wrela run`'s (the
//! driver's `serve.rs` and `live.rs`), and a tool's (`std::tool::serve`, language.md §15.5). One
//! request per connection, on 127.0.0.1 only. A request must name this machine as its host (so a
//! page elsewhere can't reach a server through DNS rebinding), and a post must come from the
//! server's own pages (their `Origin`), so another site open in the browser can't change files. A
//! put (test mode's results) isn't checked for its origin: a browser asks a server before another
//! site's page may put, and these servers answer no such question.
//!
//! Every answer is cross-origin isolated (COOP and COEP, which `SharedArrayBuffer` needs, and
//! CORP, as tools/serve.py sends), and keeps nothing cached. Files are reached through a
//! directory handle one part at a time, following no symbolic link (`confined`).

use cap_std::fs::Dir;
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;

/// The largest request body a server takes (a test's frame, an edit's JSON, a page's result).
pub const MAX_BODY: usize = 16 << 20;

/// The most bytes a request's head (its line and headers) may have.
const MAX_HEAD: u64 = 64 << 10;

/// A request's head: its method, its path and query, its body's length (read after it), and
/// the browser session a tool's browser's proxy named it with ([`wrela_chrome::proxy::SESSION_HEADER`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub path: String,
    pub query: String,
    pub length: usize,
    pub session: Option<String>,
}

impl Head {
    /// The value of `key` in the query (`?key=value&...`).
    pub fn param(&self, key: &str) -> Option<&str> {
        self.query.split('&').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
    }
}

/// Listens on 127.0.0.1:`port` (0: a free port): the listener, and its port.
pub fn listen(port: u16) -> Result<(TcpListener, u16), String> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("can't listen on 127.0.0.1:{port}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    Ok((listener, port))
}

/// Reads a request's head from `reader`, for a server on `port`: `None` if it was refused (the
/// refusal is answered on `out`): one for another host, a post from another origin, a body past
/// [`MAX_BODY`], a body framed other than by its length, or a head past 64 KiB.
pub fn read_head(
    reader: &mut impl BufRead,
    out: &mut impl Write,
    port: u16,
) -> std::io::Result<Option<Head>> {
    let mut left = MAX_HEAD;
    let mut line = |reader: &mut dyn BufRead| -> std::io::Result<Option<String>> {
        let mut bytes = Vec::new();
        let n = (&mut *reader).take(left).read_until(b'\n', &mut bytes)?;
        left -= n as u64;
        if left == 0 && !bytes.ends_with(b"\n") {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    };
    let too_long = |out: &mut dyn Write| {
        let why = format!("a request's head is at most {MAX_HEAD} bytes");
        respond(out, 431, "text/plain", why.as_bytes())
    };
    let Some(first) = line(reader)? else {
        too_long(out)?;
        return Ok(None);
    };
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("/"));
    let mut length = Some(0usize);
    let (mut host, mut origin, mut framed) = (String::new(), None::<String>, false);
    let mut session = None;
    loop {
        let Some(h) = line(reader)? else {
            too_long(out)?;
            return Ok(None);
        };
        if h.is_empty() || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let (k, v) = (k.trim(), v.trim());
            if k.eq_ignore_ascii_case("content-length") {
                length = v.parse().ok();
            } else if k.eq_ignore_ascii_case("host") {
                host = v.to_ascii_lowercase();
            } else if k.eq_ignore_ascii_case("origin") {
                origin = Some(v.to_ascii_lowercase());
            } else if k.eq_ignore_ascii_case("transfer-encoding") {
                framed = true;
            } else if k.eq_ignore_ascii_case(wrela_chrome::proxy::SESSION_HEADER) {
                session = Some(v.to_string());
            }
        }
    }
    let local = ["127.0.0.1", "localhost", "[::1]"];
    if !local.iter().any(|h| host == *h || host == format!("{h}:{port}")) {
        respond(out, 403, "text/plain", b"this server answers only requests for this machine")?;
        return Ok(None);
    }
    // A post only from this server's own pages.
    if method == "POST" && origin.as_deref() != Some(format!("http://{host}").as_str()) {
        respond(out, 403, "text/plain", b"posts are taken only from this server's pages")?;
        return Ok(None);
    }
    let Some(length) = length.filter(|_| !framed) else {
        respond(out, 411, "text/plain", b"a request's body is framed by its Content-Length")?;
        return Ok(None);
    };
    // A body past the cap is refused, not cut short (and then written as if whole).
    if length > MAX_BODY {
        let why = format!("a request's body is at most {MAX_BODY} bytes");
        respond(out, 413, "text/plain", why.as_bytes())?;
        return Ok(None);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Ok(Some(Head { method, path: path.to_string(), query: query.to_string(), length, session }))
}

/// `path` with its URL escapes decoded, once: `None` if an escape is malformed, names a `/` or a
/// `\` (so an escape can't add a part to the path), or the bytes aren't UTF-8.
pub fn decode_path(path: &str) -> Option<String> {
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

/// The media type of the file at `path`, by its extension.
pub fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("wgsl" | "txt") => "text/plain; charset=utf-8",
        Some("css") => "text/css",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// Writes a response's head: its status, its body's kind and length, and the headers every
/// answer has.
fn head(out: &mut dyn Write, status: u16, kind: &str, len: u64) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    };
    write!(
        out,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {len}\r\nCache-Control: no-store\r\nCross-Origin-Opener-Policy: same-origin\r\nCross-Origin-Embedder-Policy: require-corp\r\nCross-Origin-Resource-Policy: same-origin\r\nConnection: close\r\n\r\n",
    )
}

/// An HTTP response of `body`.
pub fn respond(out: &mut dyn Write, status: u16, kind: &str, body: &[u8]) -> std::io::Result<()> {
    head(out, status, kind, body.len() as u64)?;
    out.write_all(body)?;
    out.flush()
}

/// Answers a GET of the file at `rel` (as the request names it, with its escapes) in `dir`,
/// reached through the handle one part at a time, following no link: not found if there's no
/// regular file there, or `rel` isn't a plain path. The file is sent as it's read, except an
/// `index.html` that `page` makes a page of: that's read whole (at most [`MAX_BODY`]), and `page`
/// is given its text.
pub fn get_file(
    out: &mut dyn Write,
    dir: &Dir,
    rel: &str,
    page: Option<&dyn Fn(&str) -> String>,
) -> std::io::Result<()> {
    let not_found = |out: &mut dyn Write| respond(out, 404, "text/plain", b"not found");
    // Decoded once before it's checked or opened: checking the encoded spelling would let
    // `%2e%2e` leave the directory.
    let Some(rel) = decode_path(rel) else { return not_found(out) };
    if wrela_abi::check::path_problem(&rel).is_some() {
        return not_found(out);
    }
    let Ok(parts) = crate::confined::parts(&rel) else { return not_found(out) };
    let (name, above) = parts.split_last().expect("a path has a part");
    let file = crate::confined::open_dir(dir, above, false)
        .and_then(|at| crate::confined::open_file(&at, name, false));
    let Ok(file) = file else { return not_found(out) };
    if let (Some(page), "index.html") = (page, rel.as_str()) {
        let mut bytes = Vec::new();
        file.take(MAX_BODY as u64).read_to_end(&mut bytes)?;
        return respond(out, 200, mime(&rel), page(&String::from_utf8_lossy(&bytes)).as_bytes());
    }
    let len = file.metadata()?.len();
    head(out, 200, mime(&rel), len)?;
    // At most the length the head gave, though the file grows meanwhile.
    let sent = std::io::copy(&mut file.take(len), out)?;
    if sent < len {
        return Err(std::io::Error::other("the file was cut short as it was sent"));
    }
    out.flush()
}

/// Writes a page's result (`PUT /results/<name>`) under `dir`, from `length` bytes of `body`:
/// `name` is a plain file name, and `results/` and the file are reached through the handle
/// following no link. The bytes go to a new file of their own beside it, which then takes the
/// result's place, so a reader never sees half of one. If the body ends short, or a write fails,
/// the new file is removed, and `results/` too if this put made it. Whether the result was
/// written (a refused put is answered, and isn't).
pub fn put_result(
    out: &mut dyn Write,
    dir: &Dir,
    name: &str,
    body: &mut dyn Read,
    length: u64,
) -> std::io::Result<bool> {
    let plain = !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !name.starts_with('.');
    if !plain {
        return respond(out, 403, "text/plain", b"a result is a plain file name").map(|()| false);
    }
    if length > MAX_BODY as u64 {
        let why = format!("a result is at most {MAX_BODY} bytes");
        return respond(out, 413, "text/plain", why.as_bytes()).map(|()| false);
    }
    let refused = |out: &mut dyn Write| {
        respond(out, 403, "text/plain", b"a result path cannot contain a symbolic link")
    };
    let Ok(made) = crate::confined::make_dir(dir, "results") else {
        return refused(out).map(|()| false);
    };
    let undo = |results: Option<&Dir>, tmp: &str| {
        if let Some(r) = results {
            let _ = crate::confined::remove(r, tmp, false);
        }
        if made {
            let _ = crate::confined::remove(dir, "results", true);
        }
    };
    let Ok(results) = crate::confined::open_dir(dir, &["results"], false) else {
        undo(None, "");
        return refused(out).map(|()| false);
    };
    // A file of this put's own: several pages may put one result at once.
    static PUTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = PUTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = format!(".{name}.tmp-{}-{n}", std::process::id());
    let written = (|| {
        let mut file = crate::confined::open_file(&results, &tmp, true)?;
        file.set_len(0)?;
        let copied = std::io::copy(&mut body.take(length), &mut file)?;
        if copied < length {
            return Err(std::io::Error::other("the result's body ended short of its length"));
        }
        rename(&results, &tmp, name)
    })();
    if let Err(e) = written {
        undo(Some(&results), &tmp);
        let _ = respond(out, 400, "text/plain", b"the result wasn't written");
        return Err(e);
    }
    respond(out, 201, "text/plain", b"").map(|()| true)
}

/// Renames `from` in `dir` to `to` in it: a link at `to` is replaced, not followed.
#[cfg(unix)]
fn rename(dir: &Dir, from: &str, to: &str) -> std::io::Result<()> {
    rustix::fs::renameat(dir, from, dir, to).map_err(Into::into)
}

#[cfg(not(unix))]
fn rename(_: &Dir, _: &str, _: &str) -> std::io::Result<()> {
    Err(std::io::Error::other("a confined path needs a Unix"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head_of(text: &str) -> (Option<Head>, String) {
        let mut out = Vec::new();
        let head = read_head(&mut text.as_bytes(), &mut out, 8000).expect("read");
        (head, String::from_utf8_lossy(&out).into_owned())
    }

    #[test]
    fn a_head_names_this_machine_and_frames_its_body_by_length() {
        let (ok, _) = head_of("GET /a?x=1 HTTP/1.1\r\nHost: 127.0.0.1:8000\r\n\r\n");
        let ok = ok.expect("allowed");
        assert_eq!((ok.path.as_str(), ok.param("x"), ok.length), ("/a", Some("1"), 0));
        let (other, said) = head_of("GET / HTTP/1.1\r\nHost: evil.example\r\n\r\n");
        assert!(other.is_none() && said.starts_with("HTTP/1.1 403"), "{said}");
        let post = "POST / HTTP/1.1\r\nHost: localhost:8000\r\nOrigin: http://evil.example\r\n\r\n";
        assert!(head_of(post).1.starts_with("HTTP/1.1 403"));
        let chunked =
            "PUT /results/a HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert!(head_of(chunked).1.starts_with("HTTP/1.1 411"));
        let long = format!("GET / HTTP/1.1\r\nHost: localhost\r\nX: {}\r\n\r\n", "a".repeat(70000));
        assert!(head_of(&long).1.starts_with("HTTP/1.1 431"));
        let isolated = head_of("GET / HTTP/1.1\r\nHost: x\r\n\r\n").1;
        assert!(isolated.contains("Cross-Origin-Embedder-Policy: require-corp"));
        assert!(isolated.contains("Cross-Origin-Resource-Policy: same-origin"));
    }
}
