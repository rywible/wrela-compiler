//! Roles (language.md §15): a program that exports `frame` has a client build, one that exports
//! `serve` a server build, and one with both has both, each lowered from its own entries, with
//! one pair id in both modules. What only one role may run, reached from the other role's
//! entries, is E0708 with the call chain.

use std::path::{Path, PathBuf};

/// A package in a scratch directory of its own, with `main.wrela` holding `main`.
fn package(name: &str, main: &str) -> PathBuf {
    let dir = wrela_tests::scratch_root(env!("CARGO_TARGET_TMPDIR")).join(format!("roles-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch package");
    let ident = name.replace('-', "_");
    std::fs::write(dir.join("wrela.toml"), format!("[package]\nname = \"{ident}\"\n"))
        .expect("write the manifest");
    std::fs::write(dir.join("main.wrela"), main).expect("write main");
    dir
}

/// The built files, by path.
fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let built = wrela_driver::build(dir);
    assert!(
        !built.has_errors(),
        "{}",
        wrela_diag::render::render_all(&built.sources, &built.diagnostics)
    );
    built.files
}

fn file<'a>(files: &'a [(String, Vec<u8>)], path: &str) -> &'a [u8] {
    &files.iter().find(|(p, _)| p == path).unwrap_or_else(|| panic!("no {path}")).1
}

/// The module's exports, and whether it imports its memory shared.
fn exports_and_memory(wasm: &[u8]) -> (Vec<String>, bool) {
    let engine =
        wasmtime::Engine::new(wasmtime::Config::new().wasm_threads(true).shared_memory(true))
            .expect("an engine");
    let module = wasmtime::Module::new(&engine, wasm).expect("a module");
    let exports = module.exports().map(|e| e.name().to_string()).collect();
    let shared = module
        .imports()
        .any(|i| matches!(i.ty(), wasmtime::ExternType::Memory(m) if m.is_shared()));
    (exports, shared)
}

/// The rendered errors of checking `main`.
fn errors(name: &str, main: &str) -> (Vec<String>, String) {
    let dir = package(name, main);
    let out = wrela_driver::check(&dir);
    let codes = out.diagnostics.iter().map(|d| d.code.as_str().to_string()).collect();
    (codes, wrela_diag::render::render_all(&out.sources, &out.diagnostics))
}

const BOTH: &str = r#"
struct Room: Clone {
    ticks: u32,
}

/// Code both builds run.
fn shared<T: Copy>(x: T) -> T {
    x
}

pub fn init() -> u32 {
    shared(1)
}

pub fn frame(state: mut u32, time: f32, width: u32, height: u32) {
    state = shared(state) + 1
}

pub fn serve_init() -> Room {
    Room { ticks: shared(0) }
}

pub fn serve(room: mut Room, time: f32) {
    room.ticks = shared(room.ticks) + 1
}
"#;

#[test]
fn a_program_with_both_roles_builds_a_module_for_each() {
    let files = files(&package("both", BOTH));
    let client = file(&files, "game.wasm");
    let server = file(&files, "server/game.wasm");
    // Each lowered from its own entries: the client's exports aren't the server's.
    let (client_exports, client_shared) = exports_and_memory(client);
    let (server_exports, server_shared) = exports_and_memory(server);
    assert!(client_exports.contains(&"frame".into()) && client_exports.contains(&"init".into()));
    assert!(!client_exports.iter().any(|e| e.starts_with("serve")), "{client_exports:?}");
    assert!(
        server_exports.contains(&"serve".into()) && server_exports.contains(&"serve_init".into())
    );
    assert!(!server_exports.iter().any(|e| e == "frame" || e == "init"), "{server_exports:?}");
    // The client's memory is shared with its workers; the server's is its own.
    assert!(client_shared && !server_shared);
    // One pair id in both: the hash of both modules without it.
    let pair = wrela_abi::read_pair(client).expect("the client's pair id");
    assert_eq!(wrela_abi::read_pair(server), Some(pair));
    assert!(files.iter().any(|(p, _)| p == "manifest.json" || p == "index.html"));
}

#[test]
fn a_server_only_program_builds_no_client() {
    let main = "pub fn serve(time: f32) {}\n";
    let files = files(&package("server-only", main));
    let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(paths, ["server/game.wasm"]);
    assert!(wrela_abi::read_pair(&files[0].1).is_some());
}

#[test]
fn a_client_only_program_builds_as_before() {
    let main = "pub fn frame(time: f32, width: u32, height: u32) {}\n";
    let files = files(&package("client-only", main));
    assert!(!files.iter().any(|(p, _)| p.starts_with("server/")));
    assert!(wrela_abi::read_pair(file(&files, "game.wasm")).is_some());
}

#[test]
fn a_server_function_reached_from_the_client_is_e0708_with_its_chain() {
    let main = r#"
@server
fn keep(x: u32) -> u32 {
    x
}

fn helper(x: u32) -> u32 {
    keep(x)
}

pub fn frame(time: f32, width: u32, height: u32) {
    let _ = helper(width)
}

pub fn serve(time: f32) {
    let _ = keep(3)
}
"#;
    let (codes, text) = errors("server-from-client", main);
    assert_eq!(codes, ["E0708"], "{text}");
    assert!(text.contains("runs only in a server build"), "{text}");
    assert!(text.contains("`frame` → `helper`"), "{text}");
}

#[test]
fn client_code_reached_from_the_server_is_e0708_through_generics_and_closures() {
    let main = r#"
@client
fn show(x: u32) -> u32 {
    x
}

fn apply<F: fn(u32) -> u32>(f: F, x: u32) -> u32 {
    f(x)
}

pub fn frame(time: f32, width: u32, height: u32) {
    let _ = show(1)
}

pub fn serve(time: f32) {
    let _ = apply(|x| show(x), 2)
}
"#;
    let (codes, text) = errors("client-from-server", main);
    assert_eq!(codes, ["E0708"], "{text}");
    assert!(text.contains("runs only in a client build"), "{text}");
    assert!(text.contains("`serve` → `apply`"), "{text}");
}

#[test]
fn std_client_functions_and_gpu_work_are_e0708_in_a_server() {
    let main = r#"
use std::gpu::{GlobalId, Slots, buffer, dispatch}
use std::tick::{Ticked, start}

@compute(64, 1, 1)
fn fill(id: GlobalId, out: mut Slots<u32>) {
    out[id] = 1
}

@deterministic
fn step(s: mut u32, t: Ticked) {
    s += 1
}

@deterministic
fn hash(s: u32) -> u64 {
    u64(s)
}

fn ticking() {
    start(0, hz: 30, step: step, hash: hash)
}

pub fn serve(time: f32) {
    var out = buffer::<u32>(64)
    dispatch(fill.bind(mut out), over: 64)
    ticking()
}
"#;
    let (codes, text) = errors("std-from-server", main);
    assert_eq!(codes, ["E0708", "E0708"], "{text}");
    assert!(text.contains("GPU work in the server build's code"), "{text}");
    assert!(text.contains("`start` runs only in a client build"), "{text}");
    assert!(text.contains("`serve` → `ticking`"), "{text}");
}

#[test]
fn the_server_entries_signatures_are_checked() {
    let main = r#"
pub fn serve_init(seed: u32) -> u32 {
    seed
}

pub fn serve(state: mut u32, time: f32, extra: u32) {}
"#;
    let (codes, text) = errors("server-signatures", main);
    assert_eq!(codes, ["E0703", "E0703"], "{text}");
    assert!(text.contains("`serve_init`'s signature is `serve_init() -> State`"), "{text}");
    assert!(text.contains("`serve`'s signature is `serve(time: f32)`"), "{text}");
}

#[test]
fn a_function_is_client_or_server_not_both() {
    let main = r#"
@client
@server
fn both() {}

pub fn frame(time: f32, width: u32, height: u32) {}
"#;
    let (codes, text) = errors("both-roles", main);
    assert!(codes.contains(&"E0204".to_string()), "{text}");
    assert!(text.contains("`@client` or `@server`, not both"), "{text}");
}

// ---- the server host's rooms ------------------------------------------------------------------

use wrela_host::server::{Failure, Limits, Room, ServerBuild};

/// The server build of `main`.
fn server_build(name: &str, main: &str) -> ServerBuild {
    let files = files(&package(name, main));
    ServerBuild::from_wasm(file(&files, "server/game.wasm")).expect("a server build")
}

const ROOMS: &str = r#"
use std::host::request
use std::io::{Pending, print}

struct Room {
    serves: u32,
    asked: Option<Pending<u8>>,
}

pub fn serve_init() -> Room {
    Room { serves: 0, asked: None }
}

pub fn serve(room: mut Room, time: f32) {
    room.serves += 1
    print(f"serve {room.serves}")
    if room.serves == 1 {
        room.asked = Some(request("echo", [7, 8, 9]))
    }
    if let Some(p) = replace(mut room.asked, None) {
        var pending = take p
        match pending.poll() {
            Some(Ok(bytes)) => print(f"answered {bytes.len()} bytes, first {bytes[0]}"),
            Some(Err(_)) => print("failed"),
            None => {
                print("waiting")
                room.asked = Some(take pending)
            },
        }
    }
    if room.serves == 3 {
        loop {}
    }
}
"#;

#[test]
fn a_room_keeps_its_state_and_reads_answers_on_the_next_call() {
    let build = server_build("rooms", ROOMS);
    let echo: wrela_host::server::Answers = std::sync::Arc::new(|service, body| {
        assert_eq!(service, "echo");
        Ok(body.to_vec())
    });
    let mut room = Room::start(&build, Limits::default(), Some(echo)).expect("started");
    room.serve(0.0).expect("serve 1");
    room.serve(0.1).expect("serve 2");
    let (printed, dropped) = room.take_printed();
    // The request made in the first call isn't answered within it: the next call reads it.
    assert_eq!(
        printed,
        ["serve 1", "waiting", "serve 2", "answered 3 bytes, first 7"],
        "{printed:?}"
    );
    assert_eq!(dropped, 0);
}

#[test]
fn a_room_that_loops_fails_at_its_deadline_and_others_go_on() {
    let build = server_build("rooms_loop", ROOMS);
    let limits = Limits {
        memory: 64 << 20,
        deadline: std::time::Duration::from_millis(200),
        ..Limits::default()
    };
    let mut bad = Room::start(&build, limits, None).expect("started");
    let mut good = Room::start(&build, limits, None).expect("started");
    bad.serve(0.0).expect("serve 1");
    bad.serve(0.1).expect("serve 2");
    // The third loops: the deadline stops it, and that's the host's limit, not the program's.
    let looped = bad.serve(0.2).expect_err("it loops");
    assert_eq!(looped, Failure::Deadline);
    assert!(!looped.is_program());
    // A room beside it goes on.
    good.serve(0.0).expect("the other room serves");
    good.serve(0.1).expect("and again");
}

const GROWS: &str = r#"
// 40 MB of numbers: past a 64 MiB room, as the program needs about 29 MiB besides.
pub fn serve(time: f32) {
    var all: Vec<u32> = Vec::new()
    for i in 0..10000000 {
        all.push(i)
    }
}
"#;

#[test]
fn a_room_past_its_memory_cap_fails_as_the_hosts() {
    let build = server_build("rooms_grow", GROWS);
    let limits = Limits {
        memory: 64 << 20,
        deadline: std::time::Duration::from_secs(20),
        ..Limits::default()
    };
    let mut room = Room::start(&build, limits, None).expect("started");
    let grew = room.serve(0.0).expect_err("it outgrows its cap");
    assert_eq!(grew, Failure::Memory);
    assert!(!grew.is_program());
    // With room to grow, the same call succeeds: the cap was the host's, not the program's.
    let roomy = Limits {
        memory: 1 << 30,
        deadline: std::time::Duration::from_secs(20),
        ..Limits::default()
    };
    let mut room = Room::start(&build, roomy, None).expect("started");
    room.serve(0.0).expect("it fits");
}

const PANICS: &str = r#"
pub fn serve(time: f32) {
    if time > 1.0 {
        panic("the time is past 1")
    }
}
"#;

#[test]
fn a_rooms_own_panic_is_the_programs() {
    let build = server_build("rooms_panic", PANICS);
    let mut room = Room::start(&build, Limits::default(), None).expect("started");
    room.serve(0.5).expect("fine");
    let failed = room.serve(2.0).expect_err("it panics");
    assert!(failed.is_program(), "{failed:?}");
    assert_eq!(failed, Failure::Program("the time is past 1".into()));
}

const LOOPS_AT_INIT: &str = r#"
pub fn serve_init() -> u32 {
    loop {}
    0
}

pub fn serve(state: mut u32, time: f32) {}
"#;

#[test]
fn starting_a_room_has_a_deadline_too() {
    let build = server_build("rooms_init", LOOPS_AT_INIT);
    let limits = Limits {
        memory: 64 << 20,
        deadline: std::time::Duration::from_millis(100),
        ..Limits::default()
    };
    match Room::start(&build, limits, None) {
        Err(Failure::Deadline) => {}
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("it started"),
    }
}

#[test]
fn a_client_build_isnt_a_server_build() {
    let files = files(&package("client_not_server", BOTH));
    match ServerBuild::from_wasm(file(&files, "game.wasm")) {
        Err(Failure::Host(why)) => assert!(why.contains("isn't a server build"), "{why}"),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("a client build loaded as a server's"),
    }
}
