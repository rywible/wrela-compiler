//! The local proxy a browser held to a policy goes through for every connection it makes:
//! HTTP, HTTPS and WebSockets (secure or not), its pages', its workers' and Chrome's own. Chrome
//! is given it as its proxy with no implicit bypass (`<-loopback>`), so no loopback, local or
//! link-local address goes around it, and with QUIC and WebTransport off, which a proxy can't
//! carry.
//!
//! A plain HTTP request (`GET http://host:port/...`) is let through only to an exact
//! [`Dest::Http`] the policy grants, and as one request on its connection (the proxy says
//! `Connection: close`, so a later request can't ride along to another host). A tunnel (`CONNECT
//! host:port`: HTTPS, and every WebSocket) is let through only to an exact [`Dest::Tunnel`] the
//! policy grants. Anything else is answered 403 before any connection is made, and reported.
//!
//! **A request names its browser.** Each plain HTTP request it lets through carries the
//! browser's session ([`SESSION_HEADER`]), set by the proxy (a page's own is dropped), so a
//! server of the tool's knows which browser's page put a result.

use crate::lock;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Where a connection would go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dest<'a> {
    /// A plain `http://host:port` request.
    Http { host: &'a str, port: u16 },
    /// A tunnel to `host:port`: HTTPS, or a WebSocket.
    Tunnel { host: &'a str, port: u16 },
}

/// Whether a connection to a destination may go.
pub type Policy = Arc<dyn Fn(Dest) -> bool + Send + Sync>;

/// What the proxy refused: each as the console says it.
pub type Refused = Arc<dyn Fn(String) + Send + Sync>;

/// The header that names the browser a request came from: the proxy's own, never a page's.
pub const SESSION_HEADER: &str = "X-Wrela-Browser";

/// The most bytes a request's head may have.
const MOST_HEAD: u64 = 64 << 10;

/// The most connections at once.
const MOST_CONNECTIONS: usize = 256;

/// How long a connection waits for a read, a write or its destination.
const IDLE: Duration = Duration::from_secs(30);

/// A running proxy: its port, and its threads and sockets, which [`Proxy::close`] ends.
pub(crate) struct Proxy {
    pub port: u16,
    stopping: Arc<AtomicBool>,
    open: Arc<Mutex<HashMap<u64, TcpStream>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Proxy {
    /// Starts a proxy on a free port of 127.0.0.1.
    pub(crate) fn start(
        policy: Policy,
        refused: Refused,
        session: Option<String>,
    ) -> Result<Proxy, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("the proxy: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stopping = Arc::new(AtomicBool::new(false));
        let open = Arc::new(Mutex::new(HashMap::new()));
        let (s, o) = (stopping.clone(), open.clone());
        let thread = std::thread::Builder::new()
            .name("wrela-chrome-proxy".into())
            .spawn(move || accept(&listener, &policy, &refused, session.as_deref(), &s, &o))
            .map_err(|e| format!("can't start the proxy: {e}"))?;
        Ok(Proxy { port, stopping, open, thread: Mutex::new(Some(thread)) })
    }

    /// Closes it: its listener and every connection, its threads joined within `grace`.
    /// Whether they ended.
    pub(crate) fn close(&self, grace: Duration) -> bool {
        self.stopping.store(true, Ordering::Release);
        for c in lock(&self.open).values() {
            let _ = c.shutdown(Shutdown::Both);
        }
        let Some(t) = lock(&self.thread).take() else { return true };
        if !crate::finished_within(&t, grace) {
            return false;
        }
        let _ = t.join();
        true
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.close(Duration::from_secs(2));
    }
}

fn accept(
    listener: &TcpListener,
    policy: &Policy,
    refused: &Refused,
    session: Option<&str>,
    stopping: &AtomicBool,
    open: &Arc<Mutex<HashMap<u64, TcpStream>>>,
) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut threads: Vec<std::thread::JoinHandle<()>> = Vec::new();
    while !stopping.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((client, _)) => {
                threads.retain(|t| !t.is_finished());
                if threads.len() >= MOST_CONNECTIONS {
                    continue;
                }
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                let Ok(kept) = client.try_clone() else { continue };
                lock(open).insert(id, kept);
                let (policy, refused, open2) = (policy.clone(), refused.clone(), open.clone());
                let session = session.map(str::to_string);
                let spawned = std::thread::Builder::new()
                    .name("wrela-chrome-proxy-conn".into())
                    .spawn(move || {
                        let _ = serve(client, &policy, &refused, session.as_deref(), &open2, id);
                        if let Some(c) = lock(&open2).remove(&id) {
                            let _ = c.shutdown(Shutdown::Both);
                        }
                    });
                match spawned {
                    Ok(t) => threads.push(t),
                    Err(_) => {
                        lock(open).remove(&id);
                    }
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    for c in lock(open).values() {
        let _ = c.shutdown(Shutdown::Both);
    }
    for t in threads {
        let _ = t.join();
    }
}

/// `host:port`, or `[v6]:port`: the host (without brackets) and the port.
fn host_port(text: &str, default: u16) -> Option<(String, u16)> {
    let (host, port) = if let Some(rest) = text.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if after.is_empty() => default,
            None => return None,
        };
        (h, port)
    } else {
        match text.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().ok()?),
            None => (text, default),
        }
    };
    (!host.is_empty()).then(|| (host.to_ascii_lowercase(), port))
}

fn respond(out: &mut TcpStream, status: &str, why: &str) -> std::io::Result<()> {
    write!(
        out,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{why}",
        why.len()
    )
}

/// One connection's request: refused, or let through to its destination.
fn serve(
    client: TcpStream,
    policy: &Policy,
    refused: &Refused,
    session: Option<&str>,
    open: &Arc<Mutex<HashMap<u64, TcpStream>>>,
    id: u64,
) -> std::io::Result<()> {
    client.set_nonblocking(false)?;
    client.set_read_timeout(Some(IDLE))?;
    client.set_write_timeout(Some(IDLE))?;
    let mut out = client.try_clone()?;
    let mut reader = BufReader::new(client);
    let mut head = Vec::new();
    // The head's bytes so far: each read takes at most what's left of the allowance.
    let mut read = 0u64;
    loop {
        let mut line = Vec::new();
        let n = (&mut reader).take(MOST_HEAD - read).read_until(b'\n', &mut line)? as u64;
        read += n;
        if n == 0 {
            return respond(&mut out, "400 Bad Request", "a request's head ended too soon");
        }
        if !line.ends_with(b"\n") {
            let why = format!("a request's head is at most {MOST_HEAD} bytes");
            return respond(&mut out, "431 Request Header Fields Too Large", &why);
        }
        let blank = line == b"\r\n" || line == b"\n";
        head.push(String::from_utf8_lossy(&line).into_owned());
        if blank {
            break;
        }
    }
    let first = head[0].trim_end().to_string();
    let mut parts = first.split(' ');
    let (method, target, version) =
        (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if method == "CONNECT" {
        let Some((host, port)) = host_port(target, 443) else {
            return respond(&mut out, "400 Bad Request", "CONNECT names a host and a port");
        };
        if !policy(Dest::Tunnel { host: &host, port }) {
            refused(format!("the host refused a connection to {host}:{port}: it isn't granted"));
            return respond(&mut out, "403 Forbidden", "the tool isn't granted this host");
        }
        let dest = connect(&host, port)?;
        out.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
        return relay(reader, out, dest, open, id);
    }
    let Some(rest) = target.strip_prefix("http://") else {
        refused(format!("the host refused {method} {target}: it isn't granted"));
        return respond(&mut out, "403 Forbidden", "the proxy takes only http:// and CONNECT");
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let Some((host, port)) = host_port(authority, 80).filter(|_| !authority.contains('@')) else {
        return respond(&mut out, "400 Bad Request", "a request names a host");
    };
    if !policy(Dest::Http { host: &host, port }) {
        refused(format!("the host refused {method} http://{authority}{path}: it isn't granted"));
        return respond(&mut out, "403 Forbidden", "the tool isn't granted this origin");
    }
    let upgrade = head.iter().any(|h| h.to_ascii_lowercase().starts_with("upgrade:"));
    let mut dest = connect(&host, port)?;
    let mut sent = format!("{method} {path} {version}\r\n");
    for h in &head[1..] {
        let lower = h.to_ascii_lowercase();
        let hop = ["proxy-connection:", "proxy-authorization:", "keep-alive:"];
        let named = format!("{}:", SESSION_HEADER.to_ascii_lowercase());
        if h.trim().is_empty()
            || hop.iter().any(|p| lower.starts_with(p))
            || lower.starts_with(&named)
            || (!upgrade && lower.starts_with("connection:"))
        {
            continue;
        }
        sent.push_str(h);
    }
    if !upgrade {
        // One request on this connection: a later one can't ride along to another host.
        sent.push_str("Connection: close\r\n");
    }
    if let Some(s) = session {
        sent.push_str(&format!("{SESSION_HEADER}: {s}\r\n"));
    }
    sent.push_str("\r\n");
    dest.write_all(sent.as_bytes())?;
    relay(reader, out, dest, open, id)
}

/// A connection to `host:port`, within [`IDLE`].
fn connect(host: &str, port: u16) -> std::io::Result<TcpStream> {
    let addr = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::other("the host has no address"))?;
    let dest = TcpStream::connect_timeout(&addr, IDLE)?;
    dest.set_read_timeout(Some(IDLE))?;
    dest.set_write_timeout(Some(IDLE))?;
    Ok(dest)
}

/// Copies both ways between the client (what's left of its reader, and its writer) and `dest`,
/// until either side ends: then both are shut.
fn relay(
    mut from_client: BufReader<TcpStream>,
    mut to_client: TcpStream,
    dest: TcpStream,
    open: &Arc<Mutex<HashMap<u64, TcpStream>>>,
    id: u64,
) -> std::io::Result<()> {
    // The destination is shut with the proxy too.
    if let Ok(kept) = dest.try_clone() {
        lock(open).insert(id | 1 << 63, kept);
    }
    let mut to_dest = dest.try_clone()?;
    let mut from_dest = dest;
    let up = std::thread::Builder::new().name("wrela-chrome-proxy-up".into()).spawn(move || {
        let _ = std::io::copy(&mut from_client, &mut to_dest);
        let _ = to_dest.shutdown(Shutdown::Both);
    })?;
    let _ = std::io::copy(&mut from_dest, &mut to_client);
    let _ = from_dest.shutdown(Shutdown::Both);
    let _ = to_client.shutdown(Shutdown::Both);
    let _ = up.join();
    lock(open).remove(&(id | 1 << 63));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_ports_read_as_a_connection_names_them() {
        assert_eq!(host_port("Example.org:8443", 443), Some(("example.org".into(), 8443)));
        assert_eq!(host_port("example.org", 443), Some(("example.org".into(), 443)));
        assert_eq!(host_port("[::1]:9", 443), Some(("::1".into(), 9)));
        assert_eq!(host_port("127.0.0.1:x", 80), None);
        assert_eq!(host_port(":80", 80), None);
    }

    /// A proxy that grants only `http://127.0.0.1:<served>` and tunnels to `localhost:443`:
    /// a request each way, refused or through.
    #[test]
    fn only_exact_destinations_are_reached() {
        let served = TcpListener::bind("127.0.0.1:0").expect("listen");
        let other = TcpListener::bind("127.0.0.1:0").expect("listen");
        other.set_nonblocking(true).expect("nonblocking");
        let (sp, op) = (served.local_addr().unwrap().port(), other.local_addr().unwrap().port());
        let policy: Policy = Arc::new(move |d| match d {
            Dest::Http { host, port } => host == "127.0.0.1" && port == sp,
            Dest::Tunnel { host, port } => host == "localhost" && port == 443,
        });
        let notes = Arc::new(Mutex::new(Vec::new()));
        let kept = notes.clone();
        let proxy = Proxy::start(
            policy,
            Arc::new(move |n| kept.lock().unwrap().push(n)),
            Some("b7".into()),
        )
        .expect("a proxy");
        let ask = |request: String| {
            let mut s = TcpStream::connect(("127.0.0.1", proxy.port)).expect("connect");
            s.write_all(request.as_bytes()).expect("send");
            s.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
            let mut got = String::new();
            let _ = s.read_to_string(&mut got);
            got
        };
        let server = std::thread::spawn(move || {
            let (mut c, _) = served.accept().expect("the served request");
            let mut head = String::new();
            let mut r = BufReader::new(c.try_clone().unwrap());
            loop {
                let mut l = String::new();
                r.read_line(&mut l).unwrap();
                if l == "\r\n" {
                    break;
                }
                head.push_str(&l);
            }
            c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi").unwrap();
            (head, served)
        });
        let through = ask(format!(
            "GET http://127.0.0.1:{sp}/page HTTP/1.1\r\nHost: 127.0.0.1:{sp}\r\nProxy-Connection: keep-alive\r\nX-Wrela-Browser: forged\r\n\r\n"
        ));
        assert!(through.ends_with("hi"), "{through}");
        let (head, served) = server.join().expect("the server");
        served.set_nonblocking(true).expect("nonblocking");
        assert!(head.starts_with("GET /page HTTP/1.1\r\n"), "{head}");
        assert!(head.contains("Connection: close") && !head.contains("Proxy-Connection"), "{head}");
        assert!(head.contains("X-Wrela-Browser: b7") && !head.contains("forged"), "{head}");
        for refused in [
            format!("GET http://127.0.0.1:{op}/ HTTP/1.1\r\nHost: x\r\n\r\n"),
            format!("CONNECT 127.0.0.1:{op} HTTP/1.1\r\nHost: x\r\n\r\n"),
            format!("CONNECT localhost:{op} HTTP/1.1\r\nHost: x\r\n\r\n"),
            format!("CONNECT 127.0.0.1:{sp} HTTP/1.1\r\nHost: x\r\n\r\n"),
            "GET https://localhost/ HTTP/1.1\r\nHost: x\r\n\r\n".to_string(),
        ] {
            let said = ask(refused.clone());
            assert!(said.starts_with("HTTP/1.1 403"), "{refused}: {said}");
        }
        assert!(other.accept().is_err(), "nothing reached the other port");
        // Two headers, each well under the head's allowance, that pass it together.
        let half = "a".repeat(40 << 10);
        let long = format!(
            "GET http://127.0.0.1:{sp}/ HTTP/1.1\r\nHost: x\r\nA: {half}\r\nB: {half}\r\n\r\n"
        );
        let said = ask(long);
        assert!(said.starts_with("HTTP/1.1 431"), "{said}");
        assert!(served.accept().is_err(), "a head past its allowance reached no destination");
        assert_eq!(notes.lock().unwrap().len(), 5, "{:?}", notes.lock().unwrap());
        assert!(proxy.close(Duration::from_secs(5)));
    }
}
