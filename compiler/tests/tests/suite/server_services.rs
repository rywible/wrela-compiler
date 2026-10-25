//! A server's services (language.md §19.4, `wrela_host::services`), end to end: a server program
//! on `std::store`, `std::crypto` and `std::webauthn`, in a room kept by `wrela_host::rooms`, its
//! requests answered on the call after it makes them. Its writes commit when a call returns and
//! before the call's sends go, a call that traps commits none, the store outlives the build that
//! wrote it, and a passkey's assertion, signed here as an authenticator signs it, checks out.

use crate::rooms::limits;
use std::path::PathBuf;
use std::sync::Arc;
use wrela_host::rooms::{Out, RoomHost};
use wrela_host::server::ServerBuild;
use wrela_host::services::Services;

/// A server that does what each message asks, and answers on the same connection with the op's
/// number first: 1 put (then a key and a value), 2 get (a key), 3 scan a prefix, 4 sixteen random
/// bytes, 5 check an ES256 signature (a key, a message, a signature), 6 check a passkey's assertion
/// for wrela.dev (a challenge, the client's data, the authenticator's data, the signature and the
/// key; 254 if a check before the signature refuses it), 9 put a key and trap (and when it comes
/// back poisoned, it's refused). A request that fails answers its op and 255.
const PROGRAM: &str = r#"
use std::crypto::{Signature, random, verified, verify}
use std::input::{Event, events, payload}
use std::io::Pending
use std::net::{POISONED, send}
use std::store::{entries, get, past, put, scan, value}
use std::webauthn::{Credential, Expected, assertion}

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
            } else if op == 6 {
                let challenge = chunk(b, mut at)
                let client = chunk(b, mut at)
                let data = chunk(b, mut at)
                let signature = chunk(b, mut at)
                let key = chunk(b, mut at)
                let expected = Expected {
                    rp_id: String::from("wrela.dev"),
                    origin: String::from("https://wrela.dev"),
                    challenge,
                }
                let stored = Credential { id: Vec::from([1]), algorithm: -7, key, counter: 0 }
                match assertion(client, data, expected, stored) {
                    Ok(signed) => s.asked.push(
                        Asked { conn, op, answer: verify(stored.signature(), stored.key, signed.message, signature) },
                    ),
                    Err(_) => send(conn, [op, 254]),
                }
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

/// The server program's build and its module's hash, made once for the tests.
fn build() -> &'static (Arc<ServerBuild>, u64) {
    static BUILD: std::sync::OnceLock<(Arc<ServerBuild>, u64)> = std::sync::OnceLock::new();
    BUILD.get_or_init(|| {
        let wasm = super::rooms::server_wasm("services-program", PROGRAM);
        let build = ServerBuild::from_wasm(&wasm).expect("a server build");
        (Arc::new(build), wrela_abi::ticks::wasm_hash(&wasm))
    })
}

/// Room "a" of the server in a fresh scratch directory `name`, on a store there, with
/// connection 1 connected; and the store and the directory.
fn room(name: &str) -> (RoomHost, Arc<Services>, PathBuf) {
    let dir = crate::scratch_root().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    let services = Arc::new(Services::open(&dir.join("store.sqlite")).expect("the store"));
    let (build, hash) = build();
    let mut room =
        RoomHost::open(build.clone(), *hash, &dir.join("a"), "a", limits(), Some(services.clone()))
            .expect("open");
    room.connect(1);
    (room, services, dir)
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

#[test]
fn a_server_keeps_reads_and_scans_its_store_and_the_store_outlives_its_build() {
    let (mut room, services, dir) = room("server-services");
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
    let (build, hash) = build();
    let mut later =
        RoomHost::open(build.clone(), hash ^ 1, &dir.join("b"), "a", limits(), Some(services))
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
    let (mut room, _, dir) = room("server-signatures");
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

/// The base64url of `bytes`, without padding.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let x = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for k in 0..=c.len() {
            out.push(char::from(ALPHABET[((x >> (18 - 6 * k)) & 63) as usize]));
        }
    }
    out
}

#[test]
fn a_server_checks_a_passkeys_assertion_as_an_authenticator_signs_it() {
    use p256::ecdsa::signature::Signer;
    use sha2::Digest;
    let (mut room, _, dir) = room("server-assertions");
    // The authenticator's key, and what it signs: its data for wrela.dev (present and verified,
    // counter 1), then the SHA-256 of the client's data.
    let key = p256::ecdsa::SigningKey::from_slice(&[21u8; 32]).expect("a key");
    let public = key.verifying_key().to_encoded_point(false);
    let challenge = [9u8, 8, 7, 6, 5, 4, 3, 2, 1, 0, 250];
    let client = format!(
        r#"{{"type":"webauthn.get","challenge":"{}","origin":"https://wrela.dev","crossOrigin":false}}"#,
        base64url(&challenge)
    );
    let mut data = sha2::Sha256::digest(b"wrela.dev").to_vec();
    data.extend([0x05, 0, 0, 0, 1]);
    let signed = [&data[..], &sha2::Sha256::digest(client.as_bytes())].concat();
    let sig: p256::ecdsa::Signature = key.sign(&signed);
    let der = sig.to_der();
    let assert = |client: &[u8], challenge: &[u8]| {
        [
            &[6u8][..],
            &chunk(challenge),
            &chunk(client),
            &chunk(&data),
            &chunk(der.as_bytes()),
            &chunk(public.as_bytes()),
        ]
        .concat()
    };
    assert_eq!(ask(&mut room, assert(client.as_bytes(), &challenge)), vec![6, 1], "it checks out");
    // The same signature over client data from another origin doesn't get as far as the signature.
    let elsewhere = client.replace("https://wrela.dev", "https://evil.example");
    room.receive(1, assert(elsewhere.as_bytes(), &challenge));
    room.wake(0.1).expect("refused at once");
    assert_eq!(sent(&mut room), vec![vec![6, 254]]);
    // Client data with one byte changed, under the same challenge and origin: its hash changes, so
    // the signature isn't over it.
    let changed = client.replace("crossOrigin\":false", "crossOrigin\":false ");
    assert_eq!(ask(&mut room, assert(changed.as_bytes(), &challenge)), vec![6, 0]);
    let _ = std::fs::remove_dir_all(&dir);
}
