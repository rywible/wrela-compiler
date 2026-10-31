//! A server over HTTP (language.md §19.5), end to end: a server program on `std::http`, built
//! with its Cloudflare host (`server/cloudflare.js`), driven in Bun through the host's own router
//! (`api`) and Durable Object, with the object's storage on bun:sqlite
//! (runtime/cloudflare/test/e2e.test.ts). It checks the responses and their cookie, a store read
//! answered on the next call and the write committed before the response, the time of day, random
//! bytes and a signature checked by WebCrypto, the headers a server sees (never an address), the
//! refusals before the object (a rate limit's among them), a public answer served from the cache,
//! and that a trap or a misused response fails the instance's open requests and the next starts
//! afresh.

/// The program: each route asks what it names, and answers when the host has.
const PROGRAM: &str = r#"
use std::crypto::{Signature, random, verified, verify}
use std::http::{Request, Response, bytes, request, respond, session_cookie, text}
use std::input::{Event, events, payload}
use std::io::Pending
use std::store::{get, put, value}
use std::time::{unix_ms, wall_clock}

struct Asked {
    conn: u32,
    op: u8,
    answer: Pending<u8>,
    cookie: bool,
}

pub struct Server {
    asked: Vec<Asked>,
    closed: u32,
    served: u32,
}

pub fn serve_init() -> Server {
    Server { asked: Vec::new(), closed: 0, served: 0 }
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
