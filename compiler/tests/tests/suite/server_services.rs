//! A server's services (language.md §19.4, `wrela_host::services`), end to end: a server program
//! on `std::store` and `std::crypto`, in a room kept by `wrela_host::rooms`, its requests answered
//! on the call after it makes them. Its writes commit when a call returns and before the call's
//! sends go, a call that traps commits none, and the store outlives the build that wrote it.

use std::sync::Arc;
use wrela_host::rooms::{Out, RoomHost};
use wrela_host::server::{Limits, ServerBuild};
use wrela_host::services::Services;

/// A server that does what each message asks, and answers on the same connection with the op's
/// number first: 1 put (then a key and a value), 2 get (a key), 3 scan a prefix, 4 sixteen random
/// bytes, 5 check an ES256 signature (a key, a message, a signature), 9 put a key and trap (and
/// when it comes back poisoned, it's refused). A request that fails answers its op and 255.
const PROGRAM: &str = r#"
use std::crypto::{Signature, random, verified, verify}
use std::input::{Event, events, payload}
use std::io::Pending
use std::net::{POISONED, send}
use std::store::{entries, get, past, put, scan, value}

struct Asked {
    conn: u32,
    op: u8,
    answer: Pending<u8>,
}

pub struct Server {
    asked: Vec<Asked>,
}

pub fn serve_init() -> Server {
    Server { asked: Vec::new() }
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

fn reply(conn: u32, op: u8, answer: [u8]) {
    var out: Vec<u8> = Vec::new()
    out.push(op)
    if op == 2 {
        match value(answer) {
            Some(v) => {
                out.push(1)
                for x in v {
                    out.push(x)
                }
            },
            None => out.push(0),
        }
    } else if op == 3 {
        let Some(es) = entries(answer) else { panic("a scan's answer") }
        out.push(u8(es.len()))
        for e in es {
            out.push(u8(e.key.len()))
            for x in e.key {
                out.push(x)
            }
        }
    } else if op == 4 {
        for x in answer {
            out.push(x)
        }
    } else {
        out.push(if verified(answer) { 1 } else { 0 })
    }
    send(conn, out)
}

pub fn serve(s: mut Server, time: f32) {
    var i = 0
    while i < s.asked.len() {
        match s.asked[i].answer.poll() {
            Some(result) => {
                let a = s.asked.remove(i)
                match result {
                    Ok(bytes) => reply(a.conn, a.op, bytes),
                    Err(_) => send(a.conn, [a.op, 255]),
                }
            },
            None => i += 1,
        }
    }
    for e in events() {
        if let Event::Message { conn, .. } = e {
            // A message that trapped the room comes back poisoned, to be refused unrun.
            if conn >= POISONED {
                continue
            }
            let Some(b) = payload(e) else { continue }
            let op = b[0]
            var at = 1
            if op == 1 || op == 9 {
                let key = chunk(b, mut at)
                let v = chunk(b, mut at)
                put(key, v)
                if op == 9 {
                    panic("after a put")
                }
                send(conn, [1])
            } else if op == 2 {
                let key = chunk(b, mut at)
                s.asked.push(Asked { conn, op, answer: get(key) })
            } else if op == 3 {
                let prefix = chunk(b, mut at)
                let Some(to) = past(prefix) else { panic("a prefix") }
                s.asked.push(Asked { conn, op, answer: scan(prefix, to, 10) })
            } else if op == 4 {
                s.asked.push(Asked { conn, op, answer: random(16) })
            } else if op == 5 {
                let key = chunk(b, mut at)
                let message = chunk(b, mut at)
                let signature = chunk(b, mut at)
                s.asked.push(Asked { conn, op, answer: verify(Signature::Es256, key, message, signature) })
            }
        }
    }
}
"#;

/// The server program's module, built once for both tests.
fn wasm() -> &'static [u8] {
    static WASM: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    WASM.get_or_init(|| super::rooms::server_wasm("services-program", PROGRAM))
}

fn chunk(b: &[u8]) -> Vec<u8> {
    [&(b.len() as u32).to_le_bytes()[..], b].concat()
}

fn put(op: u8, key: &[u8], value: &[u8]) -> Vec<u8> {
    [&[op][..], &chunk(key), &chunk(value)].concat()
}

/// What the room sent connection 1, in order.
fn sent(room: &mut RoomHost) -> Vec<Vec<u8>> {
    room.take_outbox()
        .into_iter()
        .filter_map(|o| match o {
            Out::Send(1, bytes) => Some(bytes),
            _ => None,
        })
        .collect()
}

/// Asks the room `message`, and gives it a second call, on which the answer comes.
fn ask(room: &mut RoomHost, message: Vec<u8>) -> Vec<u8> {
    room.receive(1, message);
    room.wake(0.1).expect("the asking call");
    assert!(sent(room).is_empty(), "a request is answered on a later call");
    room.wake(0.2).expect("the answering call");
    let mut out = sent(room);
    assert_eq!(out.len(), 1, "one answer");
    out.remove(0)
}

fn limits() -> Limits {
    Limits {
        memory: 64 << 20,
        deadline: std::time::Duration::from_millis(500),
        ..Limits::default()
    }
}

#[test]
fn a_server_keeps_reads_and_scans_its_store_and_the_store_outlives_its_build() {
    let wasm = wasm();
    let dir = crate::scratch_root().join("server-services");
    let _ = std::fs::remove_dir_all(&dir);
    let services = Arc::new(Services::open(&dir.join("store.sqlite")).expect("the store"));
    let build = Arc::new(ServerBuild::from_wasm(wasm).expect("a server build"));
    let hash = wrela_abi::ticks::wasm_hash(wasm);
    let mut room =
        RoomHost::open(build.clone(), hash, &dir.join("a"), "a", limits(), Some(services.clone()))
            .expect("open");
    room.connect(1);
    for (k, v) in [(&b"lb/2"[..], &b"two"[..]), (b"lb/1", b"one"), (b"other", b"x")] {
        room.receive(1, put(1, k, v));
    }
    room.wake(0.0).expect("three puts");
    assert_eq!(sent(&mut room), vec![vec![1u8]; 3], "each put's answer, once it's kept");
    assert_eq!(
        ask(&mut room, [&[2u8][..], &chunk(b"lb/2")].concat()),
        [&[2u8, 1][..], b"two"].concat()
    );
    assert_eq!(ask(&mut room, [&[2u8][..], &chunk(b"none")].concat()), vec![2, 0]);
    assert_eq!(
        ask(&mut room, [&[3u8][..], &chunk(b"lb/")].concat()),
        [&[3u8, 2, 4][..], b"lb/1", &[4], b"lb/2"].concat(),
        "the prefix's keys, in order"
    );
    let a = ask(&mut room, vec![4]);
    let b = ask(&mut room, vec![4]);
    assert!(a.len() == 17 && b.len() == 17 && a != b, "sixteen random bytes, a new draw each time");

    // A call that writes and then traps commits nothing.
    room.receive(1, put(9, b"lost", b"?"));
    let _ = room.wake(0.3);
    assert_eq!(ask(&mut room, [&[2u8][..], &chunk(b"lost")].concat()), vec![2, 0]);

    // Another build of the server (here, the same module under another hash) doesn't read the
    // first build's journal, but the store is the server's: what was put is still there.
    drop(room);
    let mut later = RoomHost::open(build, hash ^ 1, &dir.join("b"), "a", limits(), Some(services))
        .expect("open again");
    later.connect(1);
    assert_eq!(
        ask(&mut later, [&[2u8][..], &chunk(b"lb/1")].concat()),
        [&[2u8, 1][..], b"one"].concat()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_server_checks_a_signature_and_refuses_another_message() {
    use p256::ecdsa::signature::Signer;
    let wasm = wasm();
    let dir = crate::scratch_root().join("server-signatures");
    let _ = std::fs::remove_dir_all(&dir);
    let services = Arc::new(Services::open(&dir.join("store.sqlite")).expect("the store"));
    let build = Arc::new(ServerBuild::from_wasm(wasm).expect("a server build"));
    let hash = wrela_abi::ticks::wasm_hash(wasm);
    let mut room =
        RoomHost::open(build, hash, &dir.join("a"), "a", limits(), Some(services)).expect("open");
    room.connect(1);
    let key = p256::ecdsa::SigningKey::from_slice(&[11u8; 32]).expect("a key");
    let public = key.verifying_key().to_encoded_point(false);
    let sig: p256::ecdsa::Signature = key.sign(b"an assertion");
    let der = sig.to_der();
    let check = |message: &[u8]| {
        [&[5u8][..], &chunk(public.as_bytes()), &chunk(message), &chunk(der.as_bytes())].concat()
    };
    assert_eq!(ask(&mut room, check(b"an assertion")), vec![5, 1]);
    assert_eq!(ask(&mut room, check(b"an assertioN")), vec![5, 0]);
    let not_a_key = [&[5u8][..], &chunk(b"junk"), &chunk(b"m"), &chunk(der.as_bytes())].concat();
    assert_eq!(ask(&mut room, not_a_key), vec![5, 255], "a key that isn't one fails the request");
    let _ = std::fs::remove_dir_all(&dir);
}
