//! `std::tool::http` (language.md §15.5): a request over HTTPS to a host the grants name, and
//! its response, as std's `http` writes and reads them. Only `https://` URLs, to a host granted
//! exactly (`--net HOST`, or `--net HOST:PORT` for that port; no user), and no redirect is
//! followed: the tool sees it. The tool's headers can't name the host: `Host` and any header
//! that sets where the request goes are refused, so the grant is the host the request reaches.
//! A body larger than one answer carries goes through a blob, charged to the blob budget as it
//! arrives, and a response's body is at most [`MOST_RESPONSE`] bytes. Every request has a
//! deadline, 30 s unless it asks for another up to [`LONGEST`], from when the host took it.
//!
//! **The connection is the host's to close.** The client runs on a thread of the worker's own,
//! scoped to it. Its connection is made here ([`Connect`]), and the worker keeps a handle to
//! the socket: past the deadline, or once the tool ends, it shuts the socket down, so whatever
//! the client waits on returns, and the worker joins the thread before it answers. Names are
//! resolved on that thread too, so no thread is left behind. A thread that still hasn't
//! returned the grace after it was cancelled (a name lookup, which no socket cuts) ends the
//! run at once as the host's failure (status 71), with or without a timeout; its worker is
//! left past the grace, and `wrela tool` ends the process.
//!
//! A tool test's hosts are fake sites (`Caps::host` says which): the host answers a `GET` (or a
//! `HEAD`) with the file at `scratch/sites/<host>/<path>`, through the capability context like
//! any file, or 404 if there's none, unless the test set a fake for that URL
//! (`std::tool::testing::fake`): a status, a missing page, other bytes, or a delay in the
//! test's fake time. A fake's answers obey a real one's limits. No tool test reaches the
//! network.

use super::services::{Body, Data, Services};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, RustlsConnector, Transport,
};

/// A request's deadline unless it asks for another.
pub(crate) const DEADLINE: Duration = Duration::from_secs(30);

/// The longest deadline a request may ask for.
pub(crate) const LONGEST: f64 = 600.0;

/// The most bytes a response's body may have.
pub(crate) const MOST_RESPONSE: u64 = 256 << 20;

/// How often a worker waiting for a response looks at its deadline and the tool's end.
const POLL: Duration = Duration::from_millis(10);

/// A request, as std's `http::fetch` writes it, and when its deadline is.
pub(crate) struct Fetch {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Data,
    /// How long it may take, and the moment it must be done by: from when the host took it.
    pub deadline: Duration,
    pub due: Instant,
}

/// A request's deadline: `seconds`, or `default` for 0.
pub(crate) fn deadline(seconds: f64, default: Duration) -> Result<Duration, String> {
    if seconds == 0.0 {
        return Ok(default);
    }
    if !(seconds > 0.0 && seconds <= LONGEST) {
        return Err(format!("a deadline is more than 0 seconds and at most {LONGEST}"));
    }
    Ok(Duration::from_secs_f64(seconds))
}

/// Whether a request may carry the header `name`: not one that names where it goes (`Host`,
/// HTTP/2's pseudo-headers), or one the client sets itself from the body.
fn header_problem(name: &str, value: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if name.is_empty()
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    {
        return Some(format!("`{name}` isn't a header's name"));
    }
    if matches!(lower.as_str(), "host" | "content-length" | "transfer-encoding" | "connection") {
        return Some(format!(
            "a tool can't set the `{name}` header: the host sets it from the granted URL and the body"
        ));
    }
    if value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
        return Some(format!("the `{name}` header's value has a line break"));
    }
    None
}

/// Reads a request, which the host took at `entered`: its method, URL, headers (a count, then
/// each name and value), deadline (an `f64`'s bits) and body.
pub(crate) fn parse(b: &mut Body, entered: Instant) -> Result<Fetch, String> {
    let method = b.str()?;
    let url = b.str()?;
    let n = b.u32()?;
    let mut headers = Vec::new();
    for _ in 0..n {
        let (name, value) = (b.str()?, b.str()?);
        if let Some(why) = header_problem(&name, &value) {
            return Err(why);
        }
        headers.push((name, value));
    }
    let deadline = deadline(f64::from_bits(b.u64()?), DEADLINE)?;
    let body = b.data()?;
    b.end()?;
    if method.is_empty() || !method.bytes().all(|c| c.is_ascii_uppercase()) {
        return Err(format!("`{method}` isn't a method: `GET`, `PUT`, ..."));
    }
    Ok(Fetch { method, url, headers, body, deadline, due: entered + deadline })
}

/// The host (with its port, if the URL names one) and the path (from its `/`, without the
/// query) of an `https://` URL, which names no user: what a grant must match, or why it can't.
pub(crate) fn target(url: &str) -> Result<(&str, &str), String> {
    let Some(rest) = url.strip_prefix("https://") else {
        return Err(format!("`{url}` isn't an `https://` URL: a tool reaches hosts over HTTPS"));
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (host, rest) = rest.split_at(end);
    if host.contains('@') {
        return Err(format!("`{url}` names a user: a host is granted as `--net HOST`, alone"));
    }
    let path = rest.split(['?', '#']).next().unwrap_or("");
    Ok((host, if path.is_empty() { "/" } else { path }))
}

/// What a test set for a fake site's URL (`std::tool::testing::fake`): what it answers in
/// place of the file, if anything, and after how long.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Fake {
    pub answer: Option<Faked>,
    /// Seconds of the test's fake time before the answer.
    pub delay: f64,
}

/// What a fake site answers for a URL in place of its file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Faked {
    /// The file (if there is one) with this status.
    Status(u16),
    /// 404 with no body, though the file is there.
    Missing,
    /// 200 with these bytes, whatever the method.
    Bytes(Vec<u8>),
}

/// The URL a fake is kept under: the URL without its fragment.
pub(crate) fn fake_key(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

/// A response's status and headers, as std's `http::read_response` reads them; its body
/// follows (`Sink::finish`).
fn head(status: u16, headers: &[(String, String)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&u32::from(status).to_le_bytes());
    out.extend_from_slice(&(headers.len() as u32).to_le_bytes());
    for (name, value) in headers {
        for t in [name, value] {
            out.extend_from_slice(&(t.len() as u32).to_le_bytes());
            out.extend_from_slice(t.as_bytes());
        }
    }
    out
}

/// A whole response of `body`, for `request`.
fn respond(
    s: &Services,
    request: u32,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<Vec<u8>, String> {
    let mut out = head(status, headers);
    let mut sink = s.sink(request, MOST_RESPONSE);
    sink.push(body, "the response's body")?;
    sink.finish(&mut out);
    Ok(out)
}

/// Carries out `f` for `request`: from a fake site if its host is one, else over the network.
/// A request still queued at its deadline isn't started.
pub(crate) fn fetch(s: &Services, request: u32, f: Fetch) -> Result<Vec<u8>, String> {
    let (host, path) = target(&f.url)?;
    let granted = s.caps.host(host);
    if granted == Some(false) && Instant::now() >= f.due {
        return Err(expired(&f));
    }
    match granted {
        None => Err(format!("the host `{host}` isn't granted: `--net {host}` grants it, exactly")),
        Some(true) => fake(s, request, &f, host, path),
        Some(false) => real(s, request, &f),
    }
}

/// Why `f` failed at its deadline.
pub(crate) fn expired(f: &Fetch) -> String {
    format!("{}: no response within its deadline of {} s", f.url, f.deadline.as_secs_f64())
}

/// A fake site's answer: the test's fake for the URL, else the file under
/// `scratch/sites/<host>/` (a path ending in `/` is its `index.html`), for `GET` and `HEAD`,
/// within a real response's limits.
fn fake(s: &Services, request: u32, f: &Fetch, host: &str, path: &str) -> Result<Vec<u8>, String> {
    let set = s.fakes().get(fake_key(&f.url)).cloned().unwrap_or_default();
    let length = |n: u64| vec![("content-length".to_string(), n.to_string())];
    let status = match set.answer {
        Some(Faked::Missing) => return respond(s, request, 404, &length(0), &[]),
        Some(Faked::Bytes(bytes)) => {
            return respond(s, request, 200, &length(bytes.len() as u64), &bytes);
        }
        Some(Faked::Status(n)) => Some(n),
        None => None,
    };
    if f.method != "GET" && f.method != "HEAD" {
        return respond(s, request, status.unwrap_or(405), &length(0), &[]);
    }
    let mut file = path.trim_start_matches('/').to_string();
    if file.is_empty() || file.ends_with('/') {
        file.push_str("index.html");
    }
    let path = format!("scratch/sites/{host}/{file}");
    let Ok(entry) = s.caps.stat(&path) else {
        return respond(s, request, status.unwrap_or(404), &length(0), &[]);
    };
    if f.method == "HEAD" {
        return respond(s, request, status.unwrap_or(200), &length(entry.size), &[]);
    }
    let mut sink = s.sink(request, MOST_RESPONSE);
    let what = "the response's body";
    // Refused before a byte is read if it's past a response's most, then as it's read.
    let read = s.caps.read_chunks(
        &path,
        |size| {
            if size > MOST_RESPONSE {
                Err(format!("{what} is more than {MOST_RESPONSE} bytes"))
            } else {
                Ok(())
            }
        },
        |chunk| sink.push(chunk, what),
    );
    match read {
        Ok(()) => {
            let mut out = head(status.unwrap_or(200), &length(entry.size));
            sink.finish(&mut out);
            Ok(out)
        }
        Err(why) => Err(why),
    }
}

/// A socket the worker can shut down while the client waits on it.
#[derive(Clone, Debug, Default)]
struct Socket {
    stream: Arc<Mutex<Option<TcpStream>>>,
    cancelled: Arc<AtomicBool>,
}

impl Socket {
    /// Shuts the connection down, and any made from now on: what the client waits on returns.
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(s) =
            self.stream.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref()
        {
            let _ = s.shutdown(Shutdown::Both);
        }
    }

    fn keep(&self, stream: &TcpStream) -> std::io::Result<()> {
        let kept = stream.try_clone()?;
        *self.stream.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(kept);
        if self.cancelled.load(Ordering::Acquire) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        Ok(())
    }
}

/// The connector of the client: a TCP connection the worker holds a handle to.
#[derive(Debug)]
struct Connect(Socket);

impl<In: Transport> Connector<In> for Connect {
    type Out = Tcp;

    fn connect(
        &self,
        details: &ConnectionDetails,
        _: Option<In>,
    ) -> Result<Option<Tcp>, ureq::Error> {
        let mut last = None;
        for addr in details.addrs.iter() {
            if self.0.cancelled.load(Ordering::Acquire) {
                break;
            }
            let attempt = match details.timeout.not_zero() {
                Some(t) => TcpStream::connect_timeout(addr, *t),
                None => TcpStream::connect(addr),
            };
            match attempt {
                Ok(stream) => {
                    self.0.keep(&stream)?;
                    let _ = stream.set_nodelay(true);
                    let config = details.config;
                    let buffers =
                        LazyBuffers::new(config.input_buffer_size(), config.output_buffer_size());
                    return Ok(Some(Tcp { stream, buffers }));
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last
            .unwrap_or_else(|| std::io::Error::other("the request was stopped before it connected"))
            .into())
    }
}

/// A connection the client reads and writes, with its timeouts.
struct Tcp {
    stream: TcpStream,
    buffers: LazyBuffers,
}

impl std::fmt::Debug for Tcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tcp").field("peer", &self.stream.peer_addr().ok()).finish()
    }
}

/// `e` as the client's error: a timeout is one.
fn io_error(e: std::io::Error, timeout: &NextTimeout) -> ureq::Error {
    if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) {
        ureq::Error::Timeout(timeout.reason)
    } else {
        e.into()
    }
}

impl Transport for Tcp {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.stream.set_write_timeout(timeout.not_zero().map(|t| *t))?;
        let output = &self.buffers.output()[..amount];
        self.stream.write_all(output).map_err(|e| io_error(e, &timeout))
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        self.stream.set_read_timeout(timeout.not_zero().map(|t| *t))?;
        let input = self.buffers.input_append_buf();
        let n = self.stream.read(input).map_err(|e| io_error(e, &timeout))?;
        self.buffers.input_appended(n);
        Ok(n > 0)
    }

    fn is_open(&mut self) -> bool {
        // Each request has an agent and a connection of its own: none is reused.
        false
    }
}

/// Names resolved on the client's own thread, with no thread of their own; `stall` is a
/// test's lookup that takes that long.
#[derive(Debug)]
struct Resolve {
    stall: Duration,
    signal: Option<super::Signal>,
}

impl Resolver for Resolve {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _: &ureq::config::Config,
        _: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        if let Some(signal) = &self.signal {
            (signal.0)("resolve");
        }
        std::thread::sleep(self.stall);
        let host = uri.host().ok_or(ureq::Error::HostNotFound)?;
        let port = uri.port_u16().unwrap_or(443);
        let mut out = self.empty();
        for addr in (host.trim_matches(['[', ']']), port).to_socket_addrs()?.take(16) {
            out.push(addr);
        }
        if out.is_empty() { Err(ureq::Error::HostNotFound) } else { Ok(out) }
    }
}

/// `f` over the network: the client on a thread scoped to this worker, which shuts the
/// connection down past the deadline or once the tool ends, then joins the thread.
fn real(s: &Services, request: u32, f: &Fetch) -> Result<Vec<u8>, String> {
    let body: Arc<Vec<u8>> = match &f.body {
        Data::Bytes(b) => Arc::new(b.clone()),
        Data::Blob(id) => s.blobs().get(*id)?,
    };
    let socket = Socket::default();
    let stopped = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let client = std::thread::Builder::new()
            .name("wrela-tool-http".into())
            .spawn_scoped(scope, || send(s, request, f, &body, &socket));
        let client = client.map_err(|e| format!("can't start the request's thread: {e}"))?;
        let mut cancelled: Option<Instant> = None;
        while !client.is_finished() {
            let ended = s.closing.load(Ordering::Acquire);
            if (ended || Instant::now() >= f.due) && !stopped.swap(true, Ordering::AcqRel) {
                socket.cancel();
                cancelled = Some(Instant::now());
            }
            // Cancelled and still not back (a name lookup, which no socket cuts): the run can't
            // end cleanly, so the host ends it now as its failure, whether or not it has a
            // timeout, and this worker is left past the grace.
            if cancelled.is_some_and(|at| at.elapsed() >= s.grace) {
                let _ = s.stuck.set(format!(
                    "a request to {} didn't stop within {:?} of being cancelled",
                    f.url, s.grace
                ));
            }
            std::thread::sleep(POLL);
        }
        let got = client.join().unwrap_or_else(|_| Err("the request's thread failed".into()));
        if stopped.load(Ordering::Acquire) {
            return Err(if s.closing.load(Ordering::Acquire) {
                format!("{}: the tool ended before the response", f.url)
            } else {
                expired(f)
            });
        }
        got
    })
}

/// Sends `f` with `body` and reads the response, within its deadline (from when the host took
/// it): no proxy, no redirect followed, any status a response (not an error). The body goes
/// to the answer as it arrives.
fn send(
    s: &Services,
    request: u32,
    f: &Fetch,
    body: &[u8],
    socket: &Socket,
) -> Result<Vec<u8>, String> {
    let failed = |e: &dyn std::fmt::Display| format!("{}: {e}", f.url);
    let left = f.due.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(expired(f));
    }
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .proxy(None)
        .timeout_global(Some(left))
        .build();
    let connector = Connect(socket.clone()).chain(RustlsConnector::default());
    let agent = ureq::Agent::with_parts(
        config,
        connector,
        Resolve { stall: s.resolve_stall, signal: s.signal.clone() },
    );
    let mut builder = ureq::http::Request::builder().method(f.method.as_str()).uri(&f.url);
    for (name, value) in &f.headers {
        builder = builder.header(name, value);
    }
    let response = if body.is_empty() {
        let r = builder.body(ureq::SendBody::none()).map_err(|e| failed(&e))?;
        agent.run(r)
    } else {
        let r = builder.body(body).map_err(|e| failed(&e))?;
        agent.run(r)
    };
    let mut response = response.map_err(|e| failed(&e))?;
    let status = response.status().as_u16();
    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(n, v)| (n.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
        .collect();
    let mut out = head(status, &headers);
    let mut reader = response.body_mut().with_config().limit(MOST_RESPONSE + 1).reader();
    s.put_read(request, &mut out, &mut reader, MOST_RESPONSE, "the response's body")
        .map_err(|e| failed(&e))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_urls_without_a_user_are_targets() {
        assert_eq!(target("https://game.example/a/b?x=1"), Ok(("game.example", "/a/b")));
        assert_eq!(target("https://game.example"), Ok(("game.example", "/")));
        assert_eq!(target("https://game.example:8443/x"), Ok(("game.example:8443", "/x")));
        for bad in [
            "http://game.example/",
            "ftp://game.example/",
            "https://user@game.example/",
            "game.example/",
        ] {
            assert!(target(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_deadline_is_the_default_or_within_the_longest() {
        assert_eq!(deadline(0.0, DEADLINE), Ok(DEADLINE));
        assert_eq!(deadline(2.5, DEADLINE), Ok(Duration::from_millis(2500)));
        for bad in [-1.0, f64::NAN, f64::INFINITY, LONGEST + 1.0] {
            assert!(deadline(bad, DEADLINE).is_err(), "{bad}");
        }
    }

    /// A request's bytes as std's `http::fetch` writes them, with `headers`.
    fn request_bytes(headers: &[(&str, &str)]) -> Vec<u8> {
        let text = |out: &mut Vec<u8>, t: &str| {
            out.extend_from_slice(&(t.len() as u32).to_le_bytes());
            out.extend_from_slice(t.as_bytes());
        };
        let mut out = Vec::new();
        text(&mut out, "GET");
        text(&mut out, "https://game.example/");
        out.extend_from_slice(&(headers.len() as u32).to_le_bytes());
        for (n, v) in headers {
            text(&mut out, n);
            text(&mut out, v);
        }
        out.extend_from_slice(&0f64.to_bits().to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0, 0]);
        out
    }

    #[test]
    fn a_tool_cant_name_the_host_in_a_header() {
        let now = Instant::now();
        let parsed = |headers: &[(&str, &str)]| {
            let bytes = request_bytes(headers);
            parse(&mut Body::new(&bytes), now).map(|_| ())
        };
        assert_eq!(parsed(&[("accept", "text/html")]), Ok(()));
        for (name, value) in [
            ("Host", "elsewhere.example"),
            ("host", "elsewhere.example"),
            (":authority", "elsewhere.example"),
            ("Content-Length", "3"),
            ("x-evil", "a\r\nHost: elsewhere.example"),
        ] {
            assert!(parsed(&[(name, value)]).is_err(), "{name}: {value}");
        }
    }
}
