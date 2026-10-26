//! The little HTTP the toolchain's local servers speak: `wrela run`'s (the driver's `live.rs`),
//! and a tool's (`std::tool::serve`, language.md §15.5). One request per connection, on
//! 127.0.0.1, or on every interface when it's asked to serve the LAN (`--lan`, `lan: true`). A
//! request must name this machine as its host: a loopback name, or on the LAN one of this
//! machine's own addresses, never a name (so a page elsewhere can't reach a server through DNS
//! rebinding). A post must come from the server's own pages (their `Origin`), so another site
//! open in the browser can't change files. A put (test mode's results) isn't checked for its
//! origin: a browser asks a server before another site's page may put, and these servers answer
//! no such question; on the LAN any device can put, which is what serving the LAN is for (a
//! phone's bench run reporting back).
//!
//! Every answer is cross-origin isolated (COOP and COEP, which `SharedArrayBuffer` needs, and
//! CORP), and keeps nothing cached. Files are reached through a
//! directory handle one part at a time, following no symbolic link (`confined`).

use cap_std::fs::Dir;
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// The largest request body a server takes (a test's frame, an edit's JSON), but a put's.
pub const MAX_BODY: usize = 16 << 20;

/// The largest page's result a server takes (`PUT /results/<name>`: a screen, a clip), which
/// goes to its file as it arrives.
pub const MAX_RESULT: usize = 256 << 20;

/// The most bytes a request's head (its line and headers) may have.
const MAX_HEAD: u64 = 64 << 10;

/// A connection whose reads and writes stop at `due`, even when its peer keeps making progress,
/// and whose each wait for the peer stops after `idle`.
pub struct DeadlineIo {
    pub stream: TcpStream,
    pub due: Instant,
    pub idle: Duration,
}

impl DeadlineIo {
    fn left(&self) -> std::io::Result<Duration> {
        let left = self.due.saturating_duration_since(Instant::now());
        if left.is_zero() {
            Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "request deadline elapsed"))
        } else {
            Ok(left.min(self.idle))
        }
    }
}

impl Read for DeadlineIo {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.stream.set_read_timeout(Some(self.left()?))?;
        self.stream.read(bytes)
    }
}

impl Write for DeadlineIo {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.stream.set_write_timeout(Some(self.left()?))?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

/// A request's head: its method, its path and query, its body's length (read after it), and
/// the browser session a tool's browser's proxy named it with ([`wrela_chrome::proxy::SESSION_HEADER`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub path: String,
    pub query: String,
    pub length: usize,
    pub session: Option<String>,
    /// The request's `If-None-Match`: the tag of the copy the browser holds.
    pub if_none_match: Option<String>,
    /// Whether the request's `Accept-Encoding` lists `gzip`.
    pub gzip: bool,
}

impl Head {
    /// The value of `key` in the query (`?key=value&...`).
    pub fn param(&self, key: &str) -> Option<&str> {
        self.query.split('&').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
    }
}

/// Listens on 127.0.0.1:`port` (0: a free port), or with `lan` on every interface: the
/// listener, its port, and the hosts a request may name besides the loopback names (with `lan`,
/// this machine's addresses on its networks; else none).
pub fn listen(port: u16, lan: bool) -> Result<(TcpListener, u16, Vec<String>), String> {
    let at = if lan { "0.0.0.0" } else { "127.0.0.1" };
    let listener =
        TcpListener::bind((at, port)).map_err(|e| format!("can't listen on {at}:{port}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let hosts = if lan {
        crate::interfaces::lan_addresses().iter().map(|a| a.to_string()).collect()
    } else {
        Vec::new()
    };
    Ok((listener, port, hosts))
}

/// The URLs a LAN device opens a server on `port` at: one an address in `hosts`.
pub fn lan_urls(hosts: &[String], port: u16) -> Vec<String> {
    hosts.iter().map(|h| format!("http://{h}:{port}/")).collect()
}

/// Reads a request's head from `reader`, for a server on `port` that also answers for `hosts`
/// (its LAN addresses, from [`listen`]): `None` if it was refused (the refusal is answered on
/// `out`): one for another host, a post from another origin, a body past [`MAX_BODY`]
/// ([`MAX_RESULT`] for a put), a body framed other than by its length, or a head past 64 KiB.
pub fn read_head(
    reader: &mut impl BufRead,
    out: &mut impl Write,
    port: u16,
    hosts: &[String],
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
    let mut if_none_match = None;
    let mut gzip = false;
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
            } else if k.eq_ignore_ascii_case("if-none-match") {
                if_none_match = Some(v.to_string());
            } else if k.eq_ignore_ascii_case("accept-encoding") {
                gzip = accepts_gzip(v);
            }
        }
    }
    let local = ["127.0.0.1", "localhost", "[::1]"];
    let named = |h: &str| host == h || host == format!("{h}:{port}");
    if !local.iter().any(|h| named(h)) && !hosts.iter().any(|h| named(h)) {
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
    let most = if method == "PUT" { MAX_RESULT } else { MAX_BODY };
    if length > most {
        let why = format!("a request's body is at most {most} bytes");
        respond(out, 413, "text/plain", why.as_bytes())?;
        return Ok(None);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Ok(Some(Head {
        method,
        path: path.to_string(),
        query: query.to_string(),
        length,
        session,
        if_none_match,
        gzip,
    }))
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

/// Whether a browser may keep a file it was sent.
#[derive(Clone, Copy, Debug)]
pub enum Caching<'a> {
    /// Never: each request reads the file again (a hot-reloading server's, whose files change
    /// under the same names).
    NoStore,
    /// It may keep the file and must ask again before it uses it (`no-cache`), naming the copy
    /// it has (`if_none_match`, the request's), which is answered with 304 while it's current.
    /// A kept copy is what lets the browser keep a WASM module's compiled code between opens.
    Revalidate { if_none_match: Option<&'a str> },
}

/// Writes a response's head: its status, its body's kind and length, and the headers every
/// answer has: `Cache-Control: no-store`, unless `tag` names the file's version, which the
/// browser may keep (`no-cache`, with the tag).
fn head_with(
    out: &mut dyn Write,
    status: u16,
    kind: &str,
    len: u64,
    tag: Option<&str>,
    gzipped: bool,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        304 => "Not Modified",
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
    let mut cache = match tag {
        Some(tag) => format!("Cache-Control: no-cache\r\nETag: {tag}"),
        None => "Cache-Control: no-store".to_string(),
    };
    if gzipped {
        cache.push_str("\r\nContent-Encoding: gzip\r\nVary: Accept-Encoding");
    }
    write!(
        out,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {len}\r\n{cache}\r\nCross-Origin-Opener-Policy: same-origin\r\nCross-Origin-Embedder-Policy: require-corp\r\nCross-Origin-Resource-Policy: same-origin\r\nConnection: close\r\n\r\n",
    )
}

fn head(out: &mut dyn Write, status: u16, kind: &str, len: u64) -> std::io::Result<()> {
    head_with(out, status, kind, len, None, false)
}

/// Whether an `Accept-Encoding` value lists gzip (and doesn't refuse it with `q=0`).
fn accepts_gzip(value: &str) -> bool {
    value.split(',').any(|coding| {
        let mut parts = coding.split(';').map(str::trim);
        let named = parts.next().is_some_and(|c| c.eq_ignore_ascii_case("gzip") || c == "*");
        let refused = parts.any(|p| {
            p.strip_prefix("q=").is_some_and(|q| q.parse::<f32>().is_ok_and(|q| q == 0.0))
        });
        named && !refused
    })
}

/// Whether a response of this media type is worth compressing: the build's code, shaders and
/// data (WASM, text, JSON, scripts), not images, which are compressed already.
fn compressible(kind: &str) -> bool {
    kind == "application/wasm" || kind == "application/json" || kind.starts_with("text/")
}

/// The most bytes of compressed files [`gzipped`] keeps, across every server in the process.
const MAX_COMPRESSED: usize = 256 << 20;

/// A file's identity and version: its device, its inode, its length and when it changed.
type Version = (u64, u64, u64, u128);

/// The compressed files kept, by the file's identity and version, oldest first; their bytes.
#[derive(Default)]
struct Compressed {
    files: std::collections::VecDeque<(Version, std::sync::Arc<Vec<u8>>)>,
    bytes: usize,
}

static COMPRESSED: std::sync::Mutex<Compressed> =
    std::sync::Mutex::new(Compressed { files: std::collections::VecDeque::new(), bytes: 0 });

/// How many times a server has compressed a file (a test's counter).
static COMPRESSIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times the process's servers have compressed a file.
pub fn compressions() -> u64 {
    COMPRESSIONS.load(std::sync::atomic::Ordering::Relaxed)
}

/// `bytes` compressed with gzip at level 6.
fn gzip(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    COMPRESSIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut encoder = flate2::write::GzEncoder::new(
        Vec::with_capacity(bytes.len() / 3),
        flate2::Compression::new(6),
    );
    encoder.write_all(bytes)?;
    encoder.finish()
}

/// The largest file a server gzips; a larger one is sent as it is.
const MOST_GZIPPED: u64 = 64 << 20;

/// The most bytes the process's servers compress at once (the plain bytes read for it); past
/// it, a file is sent as it is rather than wait.
const MOST_COMPRESSING: usize = 256 << 20;

/// The plain bytes being compressed now.
static COMPRESSING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `n` bytes of [`MOST_COMPRESSING`], taken until it's dropped; none if they aren't free.
struct Compressing(usize);

impl Compressing {
    fn take(n: usize) -> Option<Compressing> {
        use std::sync::atomic::Ordering;
        COMPRESSING
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                held.checked_add(n).filter(|&total| total <= MOST_COMPRESSING)
            })
            .ok()
            .map(|_| Compressing(n))
    }
}

impl Drop for Compressing {
    fn drop(&mut self) {
        COMPRESSING.fetch_sub(self.0, std::sync::atomic::Ordering::AcqRel);
    }
}

/// The file `file` (whose metadata is `meta`, at most `len` bytes of it read), gzipped: from
/// the process's cache when the same file at the same version was compressed before, so a
/// second load costs no compression. Nothing is written to disk. `None`, with nothing read,
/// when the file is over [`MOST_GZIPPED`] or compressing it now would pass
/// [`MOST_COMPRESSING`]: a LAN server's peer can't make it hold more than that.
fn gzipped(
    file: &mut dyn Read,
    meta: &std::fs::Metadata,
    len: u64,
) -> std::io::Result<Option<std::sync::Arc<Vec<u8>>>> {
    use std::os::unix::fs::MetadataExt;
    let key = (meta.dev(), meta.ino(), len, written(meta));
    let lock = || COMPRESSED.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((_, bytes)) = lock().files.iter().find(|(k, _)| *k == key) {
        return Ok(Some(bytes.clone()));
    }
    if len > MOST_GZIPPED {
        return Ok(None);
    }
    let Some(_held) = Compressing::take(len as usize) else { return Ok(None) };
    let mut plain = Vec::with_capacity(len as usize);
    file.take(len).read_to_end(&mut plain)?;
    if (plain.len() as u64) < len {
        return Err(std::io::Error::other("the file was cut short as it was read"));
    }
    let bytes = std::sync::Arc::new(gzip(&plain)?);
    let mut kept = lock();
    if bytes.len() <= MAX_COMPRESSED {
        kept.bytes += bytes.len();
        kept.files.push_back((key, bytes.clone()));
        while kept.bytes > MAX_COMPRESSED {
            let Some((_, old)) = kept.files.pop_front() else { break };
            kept.bytes -= old.len();
        }
    }
    Ok(Some(bytes))
}

/// A file's version, as an entity tag: which file it is (its device and inode), its length and
/// when it was last written, as the gzip cache keys it: a file replaced by another of the same
/// length and time is another file.
fn tag_of(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("\"{:x}-{:x}-{:x}-{:x}\"", meta.dev(), meta.ino(), meta.len(), written(meta))
}

/// When a file was last written, in nanoseconds since 1970 (0 if the system can't say): its
/// version, with its length, for the gzip cache and the entity tag alike.
fn written(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
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
    caching: Caching,
    gzip_ok: bool,
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
    let Ok(mut file) = file else { return not_found(out) };
    if let (Some(page), "index.html") = (page, rel.as_str()) {
        let mut bytes = Vec::new();
        file.take(MAX_BODY as u64).read_to_end(&mut bytes)?;
        let body = page(&String::from_utf8_lossy(&bytes)).into_bytes();
        if gzip_ok {
            return send_gzipped(out, mime(&rel), &gzip(&body)?, None);
        }
        return respond(out, 200, mime(&rel), &body);
    }
    let meta = file.metadata()?;
    let len = meta.len();
    let tag = match caching {
        Caching::NoStore => None,
        Caching::Revalidate { if_none_match } => {
            let tag = tag_of(&meta);
            if if_none_match.is_some_and(|held| held.split(',').any(|t| t.trim() == tag)) {
                head_with(out, 304, mime(&rel), 0, Some(&tag), false)?;
                return out.flush();
            }
            Some(tag)
        }
    };
    // Compressed as it's served, never as a file: the release's edge compresses the same types.
    if gzip_ok
        && compressible(mime(&rel))
        && let Some(body) = gzipped(&mut file, &meta, len)?
    {
        return send_gzipped(out, mime(&rel), &body, tag.as_deref());
    }
    head_with(out, 200, mime(&rel), len, tag.as_deref(), false)?;
    // At most the length the head gave, though the file grows meanwhile.
    let sent = std::io::copy(&mut file.take(len), out)?;
    if sent < len {
        return Err(std::io::Error::other("the file was cut short as it was sent"));
    }
    out.flush()
}

/// A 200 of `body`, gzipped, of type `kind`, with its entity tag if it has one.
fn send_gzipped(
    out: &mut dyn Write,
    kind: &str,
    body: &[u8],
    tag: Option<&str>,
) -> std::io::Result<()> {
    head_with(out, 200, kind, body.len() as u64, tag, true)?;
    out.write_all(body)?;
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
    if length > MAX_RESULT as u64 {
        let why = format!("a result is at most {MAX_RESULT} bytes");
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
    let tmp = crate::confined::temp_name(name);
    let written = (|| {
        let mut file = crate::confined::open_file(&results, &tmp, true)?;
        file.set_len(0)?;
        let copied = std::io::copy(&mut body.take(length), &mut file)?;
        if copied < length {
            return Err(std::io::Error::other("the result's body ended short of its length"));
        }
        crate::confined::rename(&results, &tmp, name)
    })();
    if let Err(e) = written {
        undo(Some(&results), &tmp);
        let _ = respond(out, 400, "text/plain", b"the result wasn't written");
        return Err(e);
    }
    respond(out, 201, "text/plain", b"").map(|()| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head_of(text: &str) -> (Option<Head>, String) {
        let mut out = Vec::new();
        let head = read_head(&mut text.as_bytes(), &mut out, 8000, &[]).expect("read");
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

    #[test]
    fn a_put_may_carry_a_result_larger_than_another_body() {
        let sized = |method: &str, length: usize| {
            head_of(&format!(
                "{method} /results/clip.webm HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost\r\nContent-Length: {length}\r\n\r\n"
            ))
        };
        assert_eq!(sized("PUT", MAX_BODY + 1).0.map(|h| h.length), Some(MAX_BODY + 1));
        assert!(sized("PUT", MAX_RESULT + 1).1.starts_with("HTTP/1.1 413"));
        assert!(sized("POST", MAX_BODY + 1).1.starts_with("HTTP/1.1 413"));
    }

    /// A served file in `dir` named `name` holding `bytes`, and the directory's handle.
    pub(crate) fn served(tag: &str, name: &str, bytes: &[u8]) -> (std::path::PathBuf, Dir) {
        let root =
            std::env::temp_dir().join(format!("wrela-loopback-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("directory");
        std::fs::write(root.join(name), bytes).expect("file");
        let dir = Dir::open_ambient_dir(&root, cap_std::ambient_authority()).expect("open");
        (root, dir)
    }

    fn get(dir: &Dir, rel: &str, caching: Caching) -> String {
        String::from_utf8_lossy(&get_bytes(dir, rel, caching, false)).into_owned()
    }

    fn get_bytes(dir: &Dir, rel: &str, caching: Caching, gzip: bool) -> Vec<u8> {
        let mut out = Vec::new();
        get_file(&mut out, dir, rel, None, caching, gzip).expect("answered");
        out
    }

    /// A response's head and body.
    fn split(response: &[u8]) -> (String, &[u8]) {
        let at = response.windows(4).position(|w| w == b"\r\n\r\n").expect("a head") + 4;
        (String::from_utf8_lossy(&response[..at]).into_owned(), &response[at..])
    }

    /// A request that accepts gzip gets the file gzipped, with its inner type, `Vary` and the
    /// compressed length; one that doesn't gets the plain bytes; an image is never compressed;
    /// and a second load of the same file at the same version compresses nothing.
    #[test]
    fn files_are_gzipped_at_serve_time_where_the_request_accepts_it() {
        use std::io::Read;
        let wasm: Vec<u8> = (0..200_000u32).flat_map(|i| (i % 251).to_le_bytes()).collect();
        let (root, dir) = served("gzip", "game.wasm", &wasm);
        std::fs::write(root.join("a.png"), b"not really a png").expect("png");
        let before = compressions();
        let caching = Caching::Revalidate { if_none_match: None };
        let response = get_bytes(&dir, "game.wasm", caching, true);
        let (head, body) = split(&response);
        assert!(head.contains("Content-Encoding: gzip\r\n"), "{head}");
        assert!(head.contains("Vary: Accept-Encoding\r\n"), "{head}");
        assert!(head.contains("Content-Type: application/wasm\r\n"), "{head}");
        assert!(head.contains(&format!("Content-Length: {}\r\n", body.len())), "{head}");
        assert!(body.len() < wasm.len() / 3, "{} of {} bytes", body.len(), wasm.len());
        let mut plain = Vec::new();
        flate2::read::GzDecoder::new(body).read_to_end(&mut plain).expect("gunzips");
        assert_eq!(plain, wasm);
        let again = get_bytes(&dir, "game.wasm", caching, true);
        assert_eq!(split(&again).1, body);
        assert_eq!(compressions() - before, 1, "the second load is compressed from the cache");
        let unasked = get_bytes(&dir, "game.wasm", caching, false);
        let (head, body) = split(&unasked);
        assert!(!head.contains("Content-Encoding"), "{head}");
        assert_eq!(body, wasm);
        let image = get_bytes(&dir, "a.png", caching, true);
        assert!(!split(&image).0.contains("Content-Encoding"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn accept_encoding_is_read_with_its_weights() {
        assert!(accepts_gzip("gzip, deflate, br, zstd"));
        assert!(accepts_gzip("br;q=1.0, gzip;q=0.8"));
        assert!(accepts_gzip("*"));
        assert!(!accepts_gzip("gzip;q=0, br"));
        assert!(!accepts_gzip("br, identity"));
    }

    /// A file the browser may keep is sent with its tag, and asked for again with that tag it's
    /// answered 304 with no body; a hot-reloading server's never is.
    #[test]
    fn a_kept_file_is_revalidated_by_its_tag() {
        let (root, dir) = served("tag", "game.wasm", b"\0asm");
        let first = get(&dir, "game.wasm", Caching::Revalidate { if_none_match: None });
        assert!(
            first.starts_with("HTTP/1.1 200") && first.contains("Cache-Control: no-cache"),
            "{first}"
        );
        let tag = first.lines().find_map(|l| l.strip_prefix("ETag: ")).expect("a tag").to_string();
        let again = get(&dir, "game.wasm", Caching::Revalidate { if_none_match: Some(&tag) });
        assert!(again.starts_with("HTTP/1.1 304") && again.ends_with("\r\n\r\n"), "{again}");
        let stale = get(&dir, "game.wasm", Caching::Revalidate { if_none_match: Some("\"0-0\"") });
        assert!(stale.starts_with("HTTP/1.1 200") && stale.ends_with("\0asm"), "{stale}");
        let live = get(&dir, "game.wasm", Caching::NoStore);
        assert!(live.contains("Cache-Control: no-store") && !live.contains("ETag"), "{live}");
        let _ = std::fs::remove_dir_all(root);
    }
}
