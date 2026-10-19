//! `std::tool::serve` (language.md §15.5) on the tool host, reached by a raw TCP client (a tool
//! test can't reach it: `std::tool::http` is HTTPS only). A served root answers GETs with its
//! files, cross-origin isolated, and refuses another host, a post from another origin, a link
//! and a body past the blob budget; a post round-trips through the tool, as a blob past 512
//! KiB; a post the tool doesn't answer fails at its deadline, as a `next_post` that no post
//! comes for does; a PUT into `results/` writes in a write root, is refused in a read root, and
//! leaves nothing when its body ends short; and however the tool ends, its server's port is
//! closed and its connections shut, in bounded time, also a server whose answer it never read.

use crate::tool::{printer, reached, run, scratch, signals, tool_build};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use wrela_host::tool::{BlobBudget, Caps, End, Ended, Grants, Testing};

/// A tool that serves the root path its second argument names, and does what its first says:
/// - `echo`: answers each post with its path and its body's size (`/ignore` it doesn't answer,
///   and `/quit` it answers `bye`, then ends);
/// - `nothing`: waits 0.2 s for a post that doesn't come, and ends;
/// - `end`: serves until it's interrupted;
/// - `unread`: starts serving on the port its third argument names, and waits, without reading
///   the answer, until it's interrupted.
///
/// A start that fails prints why, and waits until it's interrupted.
///
/// Its posts each wait at most 0.3 s for an answer (600 s for `end`).
const SERVE_TOOL: &str = r#"
use std::io::print
use std::job::{Job, Step}
use std::tool::{RUNNING, answer, args}
use std::tool::blob
use std::tool::blob::Data
use std::tool::serve
use std::tool::time

fn utf8(text: str) -> Vec<u8> {
    var out: Vec<u8> = Vec::new()
    for i in 0..text.len() {
        out.push(text.byte(i))
    }
    out
}

@job
fn main(given: take Vec<String>) -> u32 {
    let what = given[0].clone()
    let root = given[1].clone()
    var port = u32(0)
    if given.len() > 2 {
        let Some(p) = given[2].as_str().parse_u32() else { return 3 }
        port = p
    }
    // Long enough, for `end`, that a post is still waiting when the tool ends.
    let post_timeout = if what.as_str() == "end" { 600.0 } else { 0.3 }
    var s = serve::start(root.as_str(), u16(port), post_timeout)
    if what.as_str() == "unread" {
        var t = time::after(600.0)
        while t.poll().is_none() { yield }
        return 0
    }
    var sg = s.poll()
    while sg.is_none() {
        yield
        sg = s.poll()
    }
    let Some(sp) = take sg else { return 90 }
    let started = match serve::read_started(take sp) {
        Ok(x) => take x,
        Err(e) => {
            print(f"error: {e.message()}")
            // Until the test, having seen the failed start's server gone, interrupts it.
            var t = time::after(600.0)
            while t.poll().is_none() { yield }
            return 1
        },
    }
    print(f"serving {started.origin.as_str()}")
    if what.as_str() == "end" {
        var t = time::after(600.0)
        while t.poll().is_none() { yield }
        return 0
    }
    if what.as_str() == "nothing" {
        var n = serve::next_post(started.server, 0.2)
        var ng = n.poll()
        while ng.is_none() {
            yield
            ng = n.poll()
        }
        let Some(np) = take ng else { return 90 }
        match serve::read_post(take np) {
            Ok(_) => print("a post"),
            Err(e) => print(f"no post: {e.message()}"),
        }
        return 0
    }
    while true {
        var n = serve::next_post(started.server)
        var ng = n.poll()
        while ng.is_none() {
            yield
            ng = n.poll()
        }
        let Some(np) = take ng else { return 90 }
        let post = match serve::read_post(take np) {
            Ok(p) => take p,
            Err(e) => {
                print(f"next_post: {e.message()}")
                return 2
            },
        }
        if post.path.as_str() == "/late" {
            print("got /late")
        }
        if post.path.as_str() == "/ignore" {
            print("ignored a post")
            continue
        }
        var size = String::new()
        match post.body.clone() {
            Data::Bytes(b) => size = f"{b.len()} bytes",
            Data::Blob(b) => {
                var l = blob::len(b)
                var lg = l.poll()
                while lg.is_none() {
                    yield
                    lg = l.poll()
                }
                let Some(lp) = take lg else { return 90 }
                let Ok(n) = blob::read_len(take lp) else { return 91 }
                size = f"a blob of {n} bytes"
            },
        }
        let quit = post.path.as_str() == "/quit"
        let reply = if quit { String::from("bye") } else { f"{post.path.as_str()}: {size.as_str()}" }
        var a = serve::answer(started.server, post.id, Data::Bytes(utf8(reply.as_str())))
        var ag = a.poll()
        while ag.is_none() {
            yield
            ag = a.poll()
        }
        let Some(ap) = take ag else { return 90 }
        if let Err(e) = answer(take ap) {
            print(f"answer: {e.message()}")
            return 4
        }
        if quit {
            return 0
        }
    }
    0
}

pub fn tool_init() -> Job<main> {
    main.start(args())
}

pub fn tool(job: mut Job<main>) -> u32 {
    match job.resume() {
        Step::Yielded => RUNNING,
        Step::Done(status) => status,
    }
}
"#;

/// The serve tool, running on its own thread with `grants` and `args`: the lines it prints, as
/// they come, and its thread, which gives how it ended.
fn serving(
    name: &str,
    grants: Grants,
    args: &[String],
    budget: Option<BlobBudget>,
) -> (mpsc::Receiver<String>, std::thread::JoinHandle<Ended>) {
    serving_with(name, grants, args, budget, Testing::default(), None)
}

/// [`serving`], with a test's settings.
fn serving_with(
    name: &str,
    grants: Grants,
    args: &[String],
    budget: Option<BlobBudget>,
    testing: Testing,
    interrupt: Option<Arc<AtomicBool>>,
) -> (mpsc::Receiver<String>, std::thread::JoinHandle<Ended>) {
    let build = tool_build(&format!("serve-{name}"), SERVE_TOOL);
    let (sent, lines) = mpsc::channel();
    let sent = std::sync::Mutex::new(sent);
    let (_, mut options) = printer(move |line| {
        let _ = sent.lock().expect("the lines").send(line.to_string());
    });
    options.caps = Caps::open(&grants, |_| None).expect("granted");
    options.args = args.to_vec();
    options.timeout = Some(Duration::from_secs(30));
    if let Some(b) = budget {
        options.blob_budget = b;
    }
    options.testing = testing;
    options.interrupt = interrupt;
    (lines, std::thread::spawn(move || run(&build, options)))
}

/// The port of the origin the tool printed first.
fn port_of(lines: &mpsc::Receiver<String>) -> u16 {
    let line = lines.recv_timeout(Duration::from_secs(20)).expect("the tool serves");
    let origin = line.strip_prefix("serving http://127.0.0.1:").unwrap_or_else(|| panic!("{line}"));
    origin.parse().expect("a port")
}

/// Sends `request` to the server on `port` and reads its whole response.
fn exchange(port: u16, request: &[u8]) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10))).expect("a timeout");
    s.write_all(request).expect("the request");
    let mut response = Vec::new();
    let _ = s.read_to_end(&mut response);
    String::from_utf8_lossy(&response).into_owned()
}

/// A request of `method` for `path`, naming this machine as its host, from `origin` if given,
/// with `body`.
fn request(port: u16, method: &str, path: &str, origin: Option<&str>, body: &[u8]) -> Vec<u8> {
    let mut r = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n");
    if let Some(o) = origin {
        r.push_str(&format!("Origin: {o}\r\n"));
    }
    r.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut bytes = r.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// Whether nothing listens on `port` any more.
fn closed(port: u16) -> bool {
    TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(500)).is_err()
}

#[test]
fn a_served_write_root_answers_files_refusals_posts_and_results() {
    let root = scratch("serve-write");
    std::fs::write(root.join("index.html"), "<p>served</p>").expect("a page");
    std::fs::create_dir_all(root.join("sub")).expect("a directory");
    std::fs::write(root.join("sub/index.html"), "deeper").expect("its page");
    let outside = scratch("serve-outside");
    std::fs::write(outside.join("secret.txt"), "private").expect("a secret");
    std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("linked.txt"))
        .expect("a link");
    let grants = Grants::default().root("site", &root, true);
    let budget = BlobBudget { bytes: 1 << 20, handles: 16 };
    let (lines, tool) = serving("write", grants, &["echo".into(), "site".into()], Some(budget));
    let port = port_of(&lines);
    let own = format!("http://127.0.0.1:{port}");

    let page = exchange(port, &request(port, "GET", "/index.html", None, b""));
    assert!(page.starts_with("HTTP/1.1 200") && page.ends_with("<p>served</p>"), "{page}");
    for header in [
        "Cross-Origin-Opener-Policy: same-origin",
        "Cross-Origin-Embedder-Policy: require-corp",
        "Content-Type: text/html; charset=utf-8",
    ] {
        assert!(page.contains(header), "{header}: {page}");
    }
    assert!(exchange(port, &request(port, "GET", "/", None, b"")).ends_with("<p>served</p>"));
    assert!(exchange(port, &request(port, "GET", "/sub/", None, b"")).ends_with("deeper"));
    for missing in ["/none.txt", "/linked.txt", "/%2e%2e/serve-outside/secret.txt"] {
        let r = exchange(port, &request(port, "GET", missing, None, b""));
        assert!(r.starts_with("HTTP/1.1 404") && !r.contains("private"), "{missing}: {r}");
    }
    let elsewhere = exchange(port, b"GET / HTTP/1.1\r\nHost: evil.example\r\n\r\n");
    assert!(elsewhere.starts_with("HTTP/1.1 403"), "another host: {elsewhere}");
    for origin in [None, Some("http://evil.example")] {
        let r = exchange(port, &request(port, "POST", "/echo", origin, b"x"));
        assert!(r.starts_with("HTTP/1.1 403"), "a post from {origin:?}: {r}");
    }
    // It listens on the loopback address alone: this machine's other addresses reach nothing.
    let outside = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|u| u.connect("192.0.2.1:9").and_then(|_| u.local_addr()))
        .map(|a| a.ip());
    if let Ok(ip) = outside
        && !ip.is_loopback()
        && !ip.is_unspecified()
    {
        assert!(std::net::TcpStream::connect((ip, port)).is_err(), "the server answered on {ip}");
    }

    // A post's answer is the tool's, inline or through a blob.
    let echoed = exchange(port, &request(port, "POST", "/echo?k=v", Some(&own), b"hello"));
    assert!(
        echoed.starts_with("HTTP/1.1 200") && echoed.ends_with("/echo?k=v: 5 bytes"),
        "{echoed}"
    );
    let big = vec![7u8; 600 << 10];
    let echoed = exchange(port, &request(port, "POST", "/big", Some(&own), &big));
    assert!(echoed.ends_with("/big: a blob of 614400 bytes"), "{echoed}");
    // Past the blob budget, refused from its length alone: the body is never sent.
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(5))).expect("a timeout");
    let head = format!(
        "POST /big HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: {own}\r\nContent-Length: {}\r\n\r\n",
        2 << 20
    );
    s.write_all(head.as_bytes()).expect("the head");
    let mut refused = String::new();
    let _ = s.read_to_string(&mut refused);
    assert!(refused.starts_with("HTTP/1.1 413") && refused.contains("budget"), "{refused}");

    // A post the tool doesn't answer fails at its deadline.
    let asked = Instant::now();
    let ignored = exchange(port, &request(port, "POST", "/ignore", Some(&own), b""));
    assert!(ignored.starts_with("HTTP/1.1 504"), "{ignored}");
    assert!(asked.elapsed() >= Duration::from_millis(300), "{:?}", asked.elapsed());
    assert!(asked.elapsed() < Duration::from_secs(5), "{:?}", asked.elapsed());

    // A put whose body ends short leaves nothing: not its file, nor the results/ it made.
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let head = format!(
        "PUT /results/cut.json HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 100\r\n\r\n0123456789"
    );
    s.write_all(head.as_bytes()).expect("part of a put");
    s.shutdown(Shutdown::Write).expect("cut short");
    let mut said = String::new();
    let _ = s.read_to_string(&mut said);
    assert!(!root.join("results").exists(), "a short put made nothing: {said}");
    let put = exchange(port, &request(port, "PUT", "/results/r.json", None, b"{\"ok\":1}"));
    assert!(put.starts_with("HTTP/1.1 201"), "{put}");
    assert_eq!(std::fs::read(root.join("results/r.json")).expect("the result"), b"{\"ok\":1}");
    for name in ["/results/.hidden", "/results/a/b", "/elsewhere.json"] {
        let r = exchange(port, &request(port, "PUT", name, None, b"x"));
        assert!(r.starts_with("HTTP/1.1 403") || r.starts_with("HTTP/1.1 404"), "{name}: {r}");
    }

    let bye = exchange(port, &request(port, "POST", "/quit", Some(&own), b""));
    assert!(bye.ends_with("bye"), "{bye}");
    let ended = tool.join().expect("the tool's thread");
    assert_eq!(ended.end, End::Exit(0));
    assert!(closed(port), "the port is closed once the tool has ended");
    let said: Vec<String> = lines.try_iter().collect();
    assert_eq!(said, ["ignored a post"]);
}

#[test]
fn a_served_read_root_takes_no_results() {
    let root = scratch("serve-read");
    std::fs::write(root.join("index.html"), "read only").expect("a page");
    let grants = Grants::default().root("site", &root, false);
    let (lines, tool) = serving("read", grants, &["echo".into(), "site".into()], None);
    let port = port_of(&lines);
    let put = exchange(port, &request(port, "PUT", "/results/r.json", None, b"{}"));
    assert!(put.starts_with("HTTP/1.1 403") && put.contains("reading only"), "{put}");
    assert!(!root.join("results").exists());
    assert!(exchange(port, &request(port, "GET", "/", None, b"")).ends_with("read only"));
    let own = format!("http://127.0.0.1:{port}");
    exchange(port, &request(port, "POST", "/quit", Some(&own), b""));
    assert_eq!(tool.join().expect("the tool's thread").end, End::Exit(0));
}

#[test]
fn a_next_post_that_no_post_comes_for_fails_at_its_deadline() {
    let root = scratch("serve-nothing");
    let grants = Grants::default().root("site", &root, false);
    let started = Instant::now();
    let (lines, tool) = serving("nothing", grants, &["nothing".into(), "site".into()], None);
    let port = port_of(&lines);
    let ended = tool.join().expect("the tool's thread");
    assert_eq!(ended.end, End::Exit(0));
    let said: Vec<String> = lines.try_iter().collect();
    assert_eq!(said, ["no post: no post came within the deadline of 0.2 s"]);
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    assert!(closed(port));
}

#[test]
fn the_tools_end_shuts_its_servers_connections_in_bounded_time() {
    let root = scratch("serve-end");
    std::fs::write(root.join("index.html"), "x").expect("a page");
    let grants = Grants::default().root("site", &root, false);
    let (signal, told) = signals();
    let testing = Testing { signal: Some(signal), ..Testing::default() };
    let interrupt = Arc::new(AtomicBool::new(false));
    let args = ["end".into(), "site".into()];
    let (lines, tool) = serving_with("end", grants, &args, None, testing, Some(interrupt.clone()));
    let port = port_of(&lines);
    // A connection that sends half a head and stalls, and a post nobody takes, each taken by
    // the server (and the post queued) before the tool ends.
    let mut stalled = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stalled.write_all(b"GET / HTTP/1.1\r\nHost: 127.0").expect("half a head");
    reached(&told, "connection");
    let mut posted = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let own = format!("http://127.0.0.1:{port}");
    posted.write_all(&request(port, "POST", "/waits", Some(&own), b"p")).expect("a post");
    reached(&told, "post");
    interrupt.store(true, Ordering::Release);
    let ended = tool.join().expect("the tool's thread");
    assert_eq!(ended.end, End::Interrupted);
    assert_eq!(ended.workers_left, 0, "every thread of the server's was joined");
    for s in [&mut stalled, &mut posted] {
        // Shut as the run ended: the read ends at once, not at this timeout. (A socket the
        // server has shut may refuse a timeout, on macOS: its read ends at once too.)
        let _ = s.set_read_timeout(Some(Duration::from_secs(60)));
        let mut rest = Vec::new();
        if let Err(e) = s.read_to_end(&mut rest) {
            let waited =
                matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut);
            assert!(!waited, "the connection was left open: {e}");
        }
        // Answered 503 as it closed, or shut first, perhaps part way through that answer.
        let text = String::from_utf8_lossy(&rest);
        let closing = "HTTP/1.1 503 Service Unavailable";
        assert!(text.starts_with(closing) || closing.starts_with(&*text), "{text}");
    }
    assert!(closed(port));
}

#[test]
fn a_server_whose_answer_the_tool_never_read_is_closed_with_the_run() {
    let root = scratch("serve-unread");
    let grants = Grants::default().root("site", &root, false);
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        l.local_addr().expect("its address").port()
    };
    let (signal, told) = signals();
    let testing = Testing { signal: Some(signal), ..Testing::default() };
    let interrupt = Arc::new(AtomicBool::new(false));
    let args = ["unread".into(), "site".into(), port.to_string()];
    let (_lines, tool) =
        serving_with("unread", grants, &args, None, testing, Some(interrupt.clone()));
    // It listens while the tool waits, though the tool never learns of it.
    reached(&told, "listening");
    assert!(TcpStream::connect(("127.0.0.1", port)).is_ok(), "the server started");
    interrupt.store(true, Ordering::Release);
    let ended = tool.join().expect("the tool's thread");
    assert_eq!(ended.end, End::Interrupted);
    assert_eq!(ended.workers_left, 0);
    assert!(closed(port), "the request's server went with the run");
}

#[test]
fn a_start_that_fails_once_its_server_listens_closes_it_at_once() {
    let root = scratch("serve-failed");
    let grants = Grants::default().root("site", &root, false);
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        l.local_addr().expect("its address").port()
    };
    let (signal, told) = signals();
    let testing = Testing { fail_server_start: true, signal: Some(signal), ..Testing::default() };
    let interrupt = Arc::new(AtomicBool::new(false));
    let args = ["echo".into(), "site".into(), port.to_string()];
    let (lines, tool) =
        serving_with("failed", grants, &args, None, testing, Some(interrupt.clone()));
    reached(&told, "listening");
    let said = lines.recv_timeout(Duration::from_secs(60)).expect("the tool says why");
    assert_eq!(said, "error: a test's failure, once the server listens");
    // The tool still runs, and waits: the server its request made was closed with the request,
    // so its port is closed already, not held by a listener nothing answers.
    assert!(closed(port), "the failed request's listener is gone");
    assert!(!tool.is_finished(), "the tool still runs");
    interrupt.store(true, Ordering::Release);
    let ended = tool.join().expect("the tool's thread");
    assert_eq!(ended.end, End::Interrupted);
    assert_eq!(ended.workers_left, 0);
}

#[test]
fn a_post_whose_body_comes_past_its_deadline_fails_then_and_reaches_no_one() {
    let root = scratch("serve-late");
    let grants = Grants::default().root("site", &root, false);
    let (lines, tool) = serving("late", grants, &["echo".into(), "site".into()], None);
    let port = port_of(&lines);
    let own = format!("http://127.0.0.1:{port}");
    // Half a body, and the rest only after the answer: the answer can only be the deadline's.
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let started = Instant::now();
    let head = format!(
        "POST /late HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: {own}\r\nContent-Length: 10\r\n\r\n01234"
    );
    s.write_all(head.as_bytes()).expect("half a post");
    // Past the post's 0.3 s and before a connection's 10 s idle limit.
    s.set_read_timeout(Some(Duration::from_secs(9))).expect("a timeout");
    let mut response = String::new();
    let _ = s.read_to_string(&mut response);
    assert!(response.starts_with("HTTP/1.1 504"), "{response}");
    assert!(started.elapsed() >= Duration::from_millis(300), "{:?}", started.elapsed());
    let _ = s.write_all(b"56789");
    exchange(port, &request(port, "POST", "/quit", Some(&own), b""));
    assert_eq!(tool.join().expect("the tool's thread").end, End::Exit(0));
    let said: Vec<String> = lines.try_iter().collect();
    assert!(said.is_empty(), "the late post reached the tool: {said:?}");
}
