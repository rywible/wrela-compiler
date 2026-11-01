//! A server over HTTP (language.md §19.5), end to end: a server program on `std::http`, built
//! with its Cloudflare host (`server/cloudflare.js`), driven in Bun through the host's own router
//! (`api`) and Durable Object, with the object's storage on bun:sqlite
//! (runtime/cloudflare/test/e2e.test.ts). It checks the responses and their cookie, a store read
//! answered on the next call and the write committed before the response, the time of day, random
//! bytes and a signature checked by WebCrypto, the headers a server sees (never an address), the
//! refusals before the object (a rate limit's among them), a public answer served from the cache,
//! and that a trap or a misused response fails the instance's open requests and the next starts
//! afresh. Its edge sends every request to the instance `server`.
//!
//! A second program has instances of two kinds (`account/a`, `account/b`, `board/all`) and an
//! edge (runtime/cloudflare/test/instances.test.ts): routing, a call and its reply, messages
//! acknowledged once their effect commits, delivered out of order, twice, and after either side
//! is reset between its commit and the acknowledgement, the time of day's sibling `hmac`, and
//! the build's own verifier artifact loaded through a Worker Loader with no bindings.

/// The program: each route asks what it names, and answers when the host has.
const PROGRAM: &str = r#"
use std::crypto::{Signature, random, verified, verify}
use std::http::{Request, Response, bytes, request, respond, session_cookie, text}
use std::input::{Event, events, payload}
use std::io::Pending
use std::server::{Route, dispatch, me}
use std::store::{get, put, value}
use std::time::{unix_ms, wall_clock}

struct Asked {
    conn: u32,
    op: u8,
    answer: Pending<u8>,
    cookie: bool,
}

pub struct Server {
    edge: bool,
    asked: Vec<Asked>,
    closed: u32,
    served: u32,
}

pub fn serve_init() -> Server {
    Server { edge: me().is_empty(), asked: Vec::new(), closed: 0, served: 0 }
}

fn chunk(b: [u8], at: mut u32) -> Vec<u8> {
    let n = u32(b[at]) | (u32(b[at + 1]) << 8) | (u32(b[at + 2]) << 16) | (u32(b[at + 3]) << 24)
    at += 4
    var out: Vec<u8> = Vec::new()
    for i in 0..n {
        out.push(b[at + i])
    }
    at += n
    out
}

fn count_of(answer: [u8]) -> u32 {
    match value(answer) {
        Some(v) => u32(v[0]) | (u32(v[1]) << 8) | (u32(v[2]) << 16) | (u32(v[3]) << 24),
        None => 0,
    }
}

fn answered(a: Asked, got: Vec<u8>) {
    if a.op == 1 {
        let n = count_of(got) + 1
        put([99, 111, 117, 110, 116], [u8(n & 255), u8((n >> 8) & 255), u8((n >> 16) & 255), u8(n >> 24)])
        var out = text(200, f"{n}")
        if a.cookie {
            out.set("set-cookie", session_cookie("visit", "1", 3600).as_str())
        }
        respond(a.conn, out)
    } else if a.op == 2 {
        let Some(ms) = unix_ms(got) else { panic("the time") }
        respond(a.conn, text(200, f"{ms}"))
    } else if a.op == 3 {
        respond(a.conn, bytes(200, got, "application/octet-stream"))
    } else {
        respond(a.conn, text(200, if verified(got) { "1" } else { "0" }))
    }
}

fn route(s: mut Server, r: Request) {
    let path = r.path.as_str()
    let conn = r.conn
    if s.edge {
        let _ = dispatch(conn, Route::Instance(String::from("server")))
        return
    }
    let cookie = r.cookie("__Host-visit").is_none()
    if path == "/api/count" {
        s.asked.push(Asked { conn, op: 1, answer: get([99, 111, 117, 110, 116]), cookie })
    } else if path == "/api/time" {
        s.asked.push(Asked { conn, op: 2, answer: wall_clock(), cookie })
    } else if path == "/api/random" {
        s.asked.push(Asked { conn, op: 3, answer: random(16), cookie })
    } else if path == "/api/verify" {
        var at = 0
        let key = chunk(r.body, mut at)
        let message = chunk(r.body, mut at)
        let signature = chunk(r.body, mut at)
        s.asked.push(Asked { conn, op: 4, answer: verify(Signature::Ed25519, key, message, signature), cookie })
    } else if path == "/api/headers" {
        var names = String::new()
        for h in r.headers {
            names.push_str(h.name.as_str())
            names.push_str(",")
        }
        names.push_str(f"{r.time > 1700000000000}")
        respond(conn, text(200, names.as_str()))
    } else if path == "/api/public" {
        s.served += 1
        var out = text(200, f"{s.served}")
        out.set("cache-control", "public, max-age=60")
        respond(conn, out)
    } else if path == "/api/closed" {
        respond(conn, text(200, f"{s.closed}"))
    } else if path == "/api/trap" {
        put([116], [1])
        panic("the trap route")
    } else if path == "/api/twice" {
        respond(conn, text(200, "one"))
        respond(conn, text(200, "two"))
    } else if path == "/api/bad-header" {
        var out = Response::new(200)
        out.set("access-control-allow-origin", "*")
        respond(conn, out)
    } else if path == "/api/echo" {
        var out = bytes(201, r.body, "application/octet-stream")
        out.set("x-wrela-method", r.method.as_str())
        out.set("x-wrela-query", r.query.as_str())
        respond(conn, out)
    } else {
        respond(conn, text(404, "no such route"))
    }
}

pub fn serve(s: mut Server, time: f32) {
    var i = 0
    while i < s.asked.len() {
        match s.asked[i].answer.poll() {
            Some(result) => {
                let a = s.asked.remove(i)
                match result {
                    Ok(b) => answered(a, b),
                    Err(_) => respond(a.conn, text(502, "the host failed it")),
                }
            },
            None => i += 1,
        }
    }
    for e in events() {
        match e {
            Event::Message { conn, .. } => {
                let Some(b) = payload(e) else { continue }
                let Some(r) = request(conn, b) else { continue }
                route(mut s, r)
            },
            Event::Connection { open, .. } => {
                if !open {
                    s.closed += 1
                }
            },
            _ => {},
        }
    }
}
"#;

#[test]
fn a_server_answers_http_through_its_cloudflare_host() {
    let root = super::scratch("http-program");
    let pkg = root.join("pkg");
    super::write_package(&pkg, "http", PROGRAM);
    let out = root.join("build");
    wrela_tests::must_build(&pkg, &out);
    for file in ["server/game.wasm", "server/cloudflare.js", "server/cloudflare.json"] {
        assert!(out.join(file).is_file(), "the build writes {file}");
    }
    let host = wrela_tests::repo_root().join("runtime/cloudflare");
    let run = std::process::Command::new("bun")
        .args(["test", "test/e2e.test.ts"])
        .env("WRELA_HTTP_BUILD", &out)
        .current_dir(&host)
        .output()
        .expect("bun runs runtime/cloudflare/test/e2e.test.ts");
    let said =
        format!("{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
    assert!(run.status.success(), "the end-to-end test failed:\n{said}");
    assert!(said.contains(" 0 fail") && !said.contains("skip"), "every case ran:\n{said}");
}

/// The program of instances: accounts that add to a sum and send it, revisioned, to a board that
/// keeps each account's newest; an edge that routes, calls an account, asks an `hmac` and calls
/// the build's verifier (whose edge replies `ok` to `good`, `no` to `bad`, and traps on anything
/// else, as a malformed replay).
const INSTANCES: &str = r#"
use std::crypto::hmac
use std::http::{Request, bytes, request, respond, text}
use std::input::{Event, events, payload}
use std::io::Pending
use std::server::{Call, Route, ack, call, call_build, called, delivered, dispatch, me, reply, send}
use std::store::{entries, get, put, scan, value}

const ADD: u8 = 1
const PEEK: u8 = 2
const UPSERT: u8 = 3
const BOARD: u8 = 4
const PEEKED: u8 = 5
const BYTES: u8 = 6

struct Waiting {
    conn: u32,
    op: u8,
    answer: Pending<u8>,
    n: u32,
    rev: u32,
    from: String,
}

pub struct Server {
    me: String,
    waiting: Vec<Waiting>,
}

pub fn serve_init() -> Server {
    Server { me: me(), waiting: Vec::new() }
}

fn word(b: [u8], at: u32) -> u32 {
    u32(b[at]) | (u32(b[at + 1]) << 8) | (u32(b[at + 2]) << 16) | (u32(b[at + 3]) << 24)
}

fn push_word(out: mut Vec<u8>, x: u32) {
    out.push(u8(x & 255))
    out.push(u8((x >> 8) & 255))
    out.push(u8((x >> 16) & 255))
    out.push(u8(x >> 24))
}

fn push_text(k: mut Vec<u8>, s: str) {
    for i in 0..s.len() {
        k.push(s.byte(i))
    }
}

fn key(s: str) -> Vec<u8> {
    var k: Vec<u8> = Vec::new()
    push_text(mut k, s)
    k
}

fn wait(s: mut Server, conn: u32, op: u8, answer: take Pending<u8>) {
    s.waiting.push(Waiting { conn, op, answer, n: 0, rev: 0, from: String::new() })
}

@server
fn route(r: Request) -> Route {
    let path = r.path.as_str()
    if path == "/api/add/a" || path == "/api/me/a" {
        Route::Instance(String::from("account/a"))
    } else if path == "/api/add/b" {
        Route::Instance(String::from("account/b"))
    } else if path == "/api/board" {
        Route::Instance(String::from("board/all"))
    } else if path == "/api/peek" || path == "/api/me" || path == "/api/hmac" {
        Route::Edge
    } else if path == "/api/verify" || path == "/api/verify-old" {
        Route::Edge
    } else {
        Route::Refuse(404)
    }
}

fn at_edge(s: mut Server, r: Request) {
    let path = r.path.as_str()
    let conn = r.conn
    if path == "/api/me" {
        if s.me.is_empty() {
            respond(conn, text(200, "edge"))
        } else {
            respond(conn, text(200, s.me.as_str()))
        }
    } else if path == "/api/peek" {
        wait(mut s, conn, PEEKED, call("account/a", [PEEK]))
    } else if path == "/api/hmac" {
        wait(mut s, conn, BYTES, hmac([107, 101, 121], r.body))
    } else if path == "/api/verify" {
        wait(mut s, conn, BYTES, call_build("verifiers/ledger/1", r.body))
    } else {
        wait(mut s, conn, BYTES, call_build("verifiers/ledger/0", r.body))
    }
}

fn at_instance(s: mut Server, r: Request) {
    let path = r.path.as_str()
    let conn = r.conn
    if path == "/api/me/a" {
        respond(conn, text(200, s.me.as_str()))
    } else if path == "/api/board" {
        wait(mut s, conn, BOARD, scan(key("best/"), key("best0"), 100))
    } else {
        let n = if r.body.len() > 0 { u32(r.body[0]) } else { 0 }
        s.waiting.push(Waiting { conn, op: ADD, answer: get(key("acct")), n, rev: 0, from: String::new() })
    }
}

/// The verifier: `good` is a valid replay, `bad` an invalid one, and anything else malformed.
fn verify(c: Call) {
    if c.body.len() == 4 && c.body[0] == 103 {
        reply(c.conn, [111, 107])
    } else if c.body.len() == 3 && c.body[0] == 98 {
        reply(c.conn, [110, 111])
    } else {
        panic("a malformed replay")
    }
}

fn heard(s: mut Server, conn: u32, b: [u8]) {
    if let Some(c) = called(conn, b) {
        if s.me.is_empty() {
            verify(c)
        } else {
            wait(mut s, conn, PEEK, get(key("acct")))
        }
        return
    }
    if let Some(m) = delivered(conn, b) {
        var k = key("best/")
        push_text(mut k, m.from.as_str())
        let n = word(m.body, 4)
        let rev = word(m.body, 0)
        s.waiting.push(Waiting { conn, op: UPSERT, answer: get(k), n, rev, from: m.from.clone() })
        return
    }
    let Some(r) = request(conn, b) else { return }
    if !s.me.is_empty() {
        at_instance(mut s, r)
    } else if dispatch(conn, route(r)) {
        at_edge(mut s, r)
    }
}

fn answered(w: Waiting, got: Vec<u8>) {
    if w.op == ADD {
        var sum = w.n
        var rev = 1
        if let Some(v) = value(got) {
            sum += word(v, 0)
            rev += word(v, 4)
        }
        var acct: Vec<u8> = Vec::new()
        push_word(mut acct, sum)
        push_word(mut acct, rev)
        put(key("acct"), acct)
        var up: Vec<u8> = Vec::new()
        push_word(mut up, rev)
        push_word(mut up, sum)
        send("board/all", up)
        respond(w.conn, text(200, f"{sum}"))
    } else if w.op == PEEK {
        var out: Vec<u8> = Vec::new()
        let sum = match value(got) {
            Some(v) => word(v, 0),
            None => 0,
        }
        push_word(mut out, sum)
        reply(w.conn, out)
    } else if w.op == UPSERT {
        // The sender's revision orders its upserts: an older or repeated one changes nothing.
        let newer = match value(got) {
            Some(v) => word(v, 0) < w.rev,
            None => true,
        }
        if newer {
            var k = key("best/")
            push_text(mut k, w.from.as_str())
            var v: Vec<u8> = Vec::new()
            push_word(mut v, w.rev)
            push_word(mut v, w.n)
            put(k, v)
        }
        ack(w.conn)
    } else if w.op == BOARD {
        var out = String::new()
        if let Some(es) = entries(got) {
            for e in es {
                var name: Vec<u8> = Vec::new()
                for i in 5..e.key.len() {
                    name.push(e.key[i])
                }
                out.push_str(String::from_utf8(take name).unwrap_or(String::new()).as_str())
                out.push_str(f"={word(e.value, 4)};")
            }
        }
        respond(w.conn, text(200, out.as_str()))
    } else if w.op == PEEKED {
        respond(w.conn, text(200, f"{word(got, 0)}"))
    } else {
        respond(w.conn, bytes(200, got, "application/octet-stream"))
    }
}

pub fn serve(s: mut Server, time: f32) {
    var i = 0
    while i < s.waiting.len() {
        match s.waiting[i].answer.poll() {
            Some(result) => {
                let w = s.waiting.remove(i)
                match result {
                    Ok(b) => answered(w, b),
                    // A call or a message no one answers fails at its caller, or comes again.
                    Err(_) => if w.op != PEEK && w.op != UPSERT {
                        respond(w.conn, text(502, "it failed"))
                    },
                }
            },
            None => i += 1,
        }
    }
    for e in events() {
        let Event::Message { conn, .. } = e else { continue }
        let Some(b) = payload(e) else { continue }
        heard(mut s, conn, b)
    }
}
"#;

#[test]
fn instances_call_and_send_each_other_through_the_cloudflare_host() {
    let root = super::scratch("http-instances");
    let pkg = root.join("pkg");
    super::write_package(&pkg, "ledger", INSTANCES);
    let out = root.join("build");
    wrela_tests::must_build(&pkg, &out);
    let json =
        std::fs::read_to_string(out.join("server/verifier/verifier.json")).expect("verifier.json");
    let verifier: serde_json::Value = serde_json::from_str(&json).expect("JSON");
    assert_eq!(verifier["schema"], "wrela-verify-1");
    assert_eq!(
        std::fs::read(out.join("server/verifier/game.wasm")).ok(),
        std::fs::read(out.join("server/game.wasm")).ok(),
        "the verifier holds the build's module"
    );
    let host = wrela_tests::repo_root().join("runtime/cloudflare");
    let run = std::process::Command::new("bun")
        .args(["test", "test/instances.test.ts"])
        .env("WRELA_INSTANCES_BUILD", &out)
        .current_dir(&host)
        .output()
        .expect("bun runs runtime/cloudflare/test/instances.test.ts");
    let said =
        format!("{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
    assert!(run.status.success(), "the instances test failed:\n{said}");
    assert!(said.contains(" 0 fail") && !said.contains("skip"), "every case ran:\n{said}");
}

/// A server build's rooms through its Cloudflare host (language.md §19.6), end to end: the suite's
/// rooms program (rooms.rs), built with its host, driven in Bun through the room's Durable Object
/// and the router (runtime/cloudflare/test/rooms-run.ts). A peer joins, proposes and is refused one;
/// the object is reset and reads its journal back, its peer still connected; a second peer catches
/// up and both hear the next commit; a text message closes its connection; and the router lets
/// only this build's pair, a room's name and an allowed page in.
#[test]
fn a_room_runs_on_its_cloudflare_host_and_reads_its_journal_back() {
    use super::rooms::{Down, down, hello, propose};
    let root = super::scratch("rooms-cloudflare");
    let pkg = root.join("pkg");
    super::write_package(&pkg, "rooms", super::rooms::PROGRAM);
    let out = root.join("build");
    wrela_tests::must_build(&pkg, &out);
    let wasm = std::fs::read(out.join("server/game.wasm")).expect("the server build");
    let pair = format!("{:016x}", wrela_abi::read_pair(&wasm).expect("a pair id"));
    let hex = |b: Vec<u8>| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let send = |peer: &str, b: Vec<u8>| serde_json::json!({ "send": peer, "hex": hex(b) });
    let scenario = serde_json::json!([
        { "connect": "a" },
        send("a", hello(5, 1, 0, 0)),
        send("a", propose(1, 1, 1, 10)),
        send("a", propose(1, 1, 2, 14)),
        { "restart": true },
        { "connect": "b" },
        send("b", hello(6, 1, 0, 0)),
        send("a", propose(1, 1, 3, 11)),
        { "connect": "c" },
        { "text": "c" },
        { "route": format!("/room/a?pair={pair}") },
        { "route": "/room/a?pair=0000000000000001" },
        { "route": format!("/room/A?pair={pair}") },
        { "route": format!("/room/a?pair={pair}"), "origin": "https://evil.example" },
        { "route": "/elsewhere" },
    ]);
    let path = root.join("scenario.json");
    std::fs::write(&path, scenario.to_string()).expect("the scenario");
    let host = wrela_tests::repo_root().join("runtime/cloudflare");
    let run = std::process::Command::new("bun")
        .arg("test/rooms-run.ts")
        .arg(&out)
        .arg(&path)
        .current_dir(&host)
        .output()
        .expect("bun runs runtime/cloudflare/test/rooms-run.ts");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{stdout}{}", String::from_utf8_lossy(&run.stderr));
    let result: serde_json::Value =
        serde_json::from_str(stdout.lines().last().expect("a result")).expect("JSON");
    let got = |peer: &str| -> Vec<Down> {
        let bytes = |h: &str| {
            (0..h.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
                .collect::<Vec<u8>>()
        };
        result["sent"][peer]
            .as_array()
            .expect("sent")
            .iter()
            .map(|h| down(&bytes(h.as_str().unwrap())))
            .collect()
    };
    let a = got("a");
    assert_eq!(
        a[..3],
        [
            Down::Welcome { peer: 1, seq: 0 },
            Down::Committed { seq: 1, counter: 1, value: 10 },
            Down::Rejected { counter: 2, reason: 7 }
        ],
        "{a:?}"
    );
    assert!(
        matches!(a.last(), Some(Down::Committed { counter: 3, value: 11, .. })),
        "a hears its commit after the restart: {a:?}"
    );
    let b = got("b");
    assert!(matches!(b.first(), Some(Down::Welcome { peer: 2, .. })), "{b:?}");
    let values: Vec<u32> = b
        .iter()
        .filter_map(|d| if let Down::Committed { value, .. } = d { Some(*value) } else { None })
        .collect();
    assert_eq!(
        values,
        [10, 11],
        "b catches up from the journal read back, then hears a's commit: {b:?}"
    );
    assert_eq!(result["closed"]["c"], 1003, "a text message closes its connection");
    assert_eq!(result["closed"]["a"], serde_json::Value::Null, "a's connection outlives the reset");
    assert_eq!(result["routes"], serde_json::json!([101, 403, 404, 403, 0]));
}
