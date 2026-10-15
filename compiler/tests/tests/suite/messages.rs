//! Messages (language.md §12, runtime/abi `input`'s Messages): a message is an event whose bytes
//! the program reads (`std::input::payload`) in the call that was given it, and no later. Both
//! the client's host and the server's rooms give them; a tick never does.

use std::path::PathBuf;
use wrela_host::server::{Limits, Room, ServerBuild};
use wrela_host::{CpuBuild, Value};

/// `main` built into a scratch directory of its own: the build's directory.
fn built(name: &str, main: &str) -> PathBuf {
    let root =
        wrela_tests::scratch_root(env!("CARGO_TARGET_TMPDIR")).join(format!("messages-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    let pkg = root.join("pkg");
    std::fs::create_dir_all(&pkg).expect("scratch package");
    std::fs::write(pkg.join("wrela.toml"), format!("[package]\nname = \"{name}\"\n"))
        .expect("manifest");
    std::fs::write(pkg.join("main.wrela"), main).expect("main");
    let out = root.join("build");
    wrela_tests::must_build(&pkg, &out);
    out
}

fn errors(name: &str, main: &str) -> (Vec<String>, String) {
    let dir =
        wrela_tests::scratch_root(env!("CARGO_TARGET_TMPDIR")).join(format!("messages-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch package");
    std::fs::write(dir.join("wrela.toml"), format!("[package]\nname = \"{name}\"\n"))
        .expect("manifest");
    std::fs::write(dir.join("main.wrela"), main).expect("main");
    let out = wrela_driver::check(&dir);
    let codes = out.diagnostics.iter().map(|d| d.code.as_str().to_string()).collect();
    (codes, wrela_diag::render::render_all(&out.sources, &out.diagnostics))
}

const CLIENT: &str = r#"
use std::input::{Event, events, payload}

struct Seen {
    messages: u32,
    bytes: u32,
    first: u32,
    stale: u32,
    opened: u32,
    kept: Option<Event>,
}

pub fn init() -> Seen {
    Seen { messages: 0, bytes: 0, first: 0, stale: 0, opened: 0, kept: None }
}

pub fn frame(s: mut Seen, time: f32, width: u32, height: u32) {
    // A message kept from an earlier frame has no bytes now.
    if let Some(old) = s.kept {
        if payload(old).is_none() {
            s.stale += 1
        }
        s.kept = None
    }
    for e in events() {
        match e {
            Event::Message { conn, len, .. } => {
                let Some(bytes) = payload(e) else { continue }
                s.messages += 1
                s.bytes += bytes.len()
                if s.messages == 1 {
                    s.first = u32(bytes[0]) + 100 * conn
                }
                s.kept = Some(e)
            },
            Event::Connection { open, .. } => if open { s.opened += 1 },
            _ => {},
        }
    }
}

pub fn report(s: Seen) -> vec4u {
    vec4u(s.messages, s.bytes, s.first, s.stale * 1000 + s.opened)
}
"#;

#[test]
fn a_client_reads_a_messages_bytes_in_the_frame_given_it() {
    let dir = built("client", CLIENT);
    let build = CpuBuild::load(&dir).expect("load");
    let mut host = build.start_with(1).expect("start");
    let script = wrela_host::parse_script(
        r#"[
            {"frame": 0, "type": "connection", "conn": 2, "open": true},
            {"frame": 0, "type": "message", "conn": 2, "bytes": [7, 8, 9]},
            {"frame": 2, "type": "message", "conn": 2, "text": "hello"}
        ]"#,
    )
    .expect("a script");
    for i in 0..4u32 {
        for e in wrela_abi::input::events_at(&script, i) {
            host.push_scripted(e);
        }
        host.frame(wrela_host::frame_time(i, 60.0), 64, 64).expect("frame");
    }
    // Two messages, 3 + 5 bytes, the first's first byte from connection 2; each kept message
    // read stale in the frame after it (2), and one connection opened.
    let report = host.call_export("report", &[]).expect("report");
    assert_eq!(report, [Value::I32(2), Value::I32(8), Value::I32(207), Value::I32(2001)]);
}

const ROOM: &str = r#"
use std::input::{Event, events, payload}
use std::io::print

pub fn serve(time: f32) {
    for e in events() {
        match e {
            Event::Message { conn, .. } => match payload(e) {
                Some(bytes) => print(f"{conn}: {bytes.len()} bytes, last {bytes[bytes.len() - 1]}"),
                None => print("no bytes"),
            },
            Event::Connection { conn, open } => print(f"{conn} open {open}"),
            _ => {},
        }
    }
}
"#;

#[test]
fn a_room_reads_messages_in_serve() {
    let dir = built("room", ROOM);
    let build = ServerBuild::load(&dir).expect("a server build");
    let mut room = Room::start(&build, Limits::default(), None).expect("started");
    room.push_connection(5, true);
    room.push_message(5, vec![1, 2, 3, 4]);
    room.serve(0.0).expect("serve");
    room.push_connection(5, false);
    room.serve(0.1).expect("serve");
    let (printed, _) = room.take_printed();
    assert_eq!(printed, ["5 open true", "5: 4 bytes, last 4", "5 open false"]);
}

#[test]
fn deterministic_code_cant_read_a_payload() {
    let main = r#"
use std::input::{Event, payload}

@deterministic
fn size(e: Event) -> u32 {
    match payload(e) {
        Some(b) => b.len(),
        None => 0,
    }
}

pub fn frame(time: f32, width: u32, height: u32) {
    let _ = size(Event::Text { char: 65 })
}
"#;
    let (codes, text) = errors("deterministic", main);
    assert_eq!(codes, ["E0600"], "{text}");
    assert!(text.contains("nondet"), "{text}");
}
