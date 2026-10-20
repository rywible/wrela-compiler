//! `std::tool::drive` (language.md §15.5): a tool loads a build on the native host and runs it:
//! the same frames and script give the same hash as the host run directly, a GPU frame's capture
//! comes back through a blob, an export is called, a trap says what and where, and a deadline
//! stops WASM that never ends.

use crate::common::{manifest, package, wrela};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// A tool that does what its arguments say with the build in `src/build` (or `fx`'s, for the
/// GPU), and prints what it got:
/// - `cpu`: frames with a script on the CPU (the script goes to `out/script.json`), the hash,
///   a call of `add`, a trap in `boom`, and a frame after the trap;
/// - `at`: frames at 0.25 s and 3 s, outside lockstep, on the CPU, and the hash;
/// - `gpu`: three frames on the GPU, the screen captured and written to `out/screen.rgba`, the
///   hash;
/// - `spin`: a call of `spin`, which never ends, with a deadline of 1 s, then the hash;
/// - `spin-forever`: the same with no deadline of its own (the tool's `--timeout` ends it).
const TOOL: &str = r#"
use std::input::{Key, Script}
use std::io::{IoError, print}
use std::job::{Job, Step}
use std::tool::{RUNNING, ToolError, answer, args}
use std::tool::blob::Data
use std::tool::drive
use std::tool::files
use std::tool::time

const SCRIPT: Script = Script::frames()
    .move_to(1, vec2(10.0, 20.0))
    .key_down(2, Key::ShiftLeft)
    .move_to(4, vec2(30.0, 40.0))
    .key_up(5, Key::ShiftLeft)

fn failed(e: ToolError) -> u32 {
    print(f"error: {e.message()}")
    1
}

fn utf8(text: str) -> Vec<u8> {
    var out: Vec<u8> = Vec::new()
    for i in 0..text.len() {
        out.push(text.byte(i))
    }
    out
}

fn report(polled: take Result<Vec<u8>, IoError>) {
    match drive::read_capture(take polled) {
        Ok(_) => print("captured"),
        Err(e) => print(f"refused: {e.message()}"),
    }
}

@job
fn two(n: take u32) -> u32 {
    // The fixture twice, a frame each, then both screens captured at once.
    var builds: Vec<drive::Build> = Vec::new()
    for _ in 0..2 {
        var l = drive::load("fx", drive::gpu())
        var lg = l.poll()
        while lg.is_none() {
            yield
            lg = l.poll()
        }
        let Some(lp) = take lg else { return 90 }
        let loaded = match drive::read_loaded(take lp) {
            Ok(x) => x,
            Err(e) => return failed(e),
        }
        var i = drive::init(loaded.build)
        while i.poll().is_none() { yield }
        var f = drive::frames(loaded.build, 1, Script::frames())
        while f.poll().is_none() { yield }
        builds.push(loaded.build)
    }
    var a = drive::capture(builds[0])
    var b = drive::capture(builds[1])
    var ga = a.poll()
    var gb = b.poll()
    while ga.is_none() || gb.is_none() {
        yield
        if ga.is_none() {
            ga = a.poll()
        }
        if gb.is_none() {
            gb = b.poll()
        }
    }
    let Some(pa) = take ga else { return 90 }
    let Some(pb) = take gb else { return 90 }
    report(take pa)
    report(take pb)
    n
}

@job
fn main(given: take Vec<String>) -> u32 {
    let what = given[0].clone()
    var options = if what.as_str() == "gpu" { drive::gpu() } else { drive::cpu() }
    options.workers = 2
    let at = if what.as_str() == "gpu" { "fx" } else { "src/build" }
    var l = drive::load(at, options)
    var lg = l.poll()
    while lg.is_none() {
        yield
        lg = l.poll()
    }
    let Some(lp) = take lg else { return 90 }
    let loaded = match drive::read_loaded(take lp) {
        Ok(x) => x,
        Err(e) => return failed(e),
    }
    let b = loaded.build
    var i = drive::init(b)
    var ig = i.poll()
    while ig.is_none() {
        yield
        ig = i.poll()
    }
    let Some(ip) = take ig else { return 90 }
    let Ok(init) = drive::read_ran(take ip) else { return 91 }
    assert(init.trap.is_none(), "init")
    if what.as_str() == "cpu" {
        var w = files::write("out/script.json", utf8(SCRIPT.as_json()))
        while w.poll().is_none() { yield }
        var f = drive::frames(b, 8, SCRIPT)
        var fg = f.poll()
        while fg.is_none() {
            yield
            fg = f.poll()
        }
        let Some(fp) = take fg else { return 90 }
        let ran = match drive::read_ran(take fp) {
            Ok(r) => take r,
            Err(e) => return failed(e),
        }
        print(f"frames {ran.steps.len()}")
        var c = drive::call(b, "add", [drive::Scalar::I32(2), drive::Scalar::I32(40)])
        var cg = c.poll()
        while cg.is_none() {
            yield
            cg = c.poll()
        }
        let Some(cp) = take cg else { return 90 }
        let Ok(called) = drive::read_ran(take cp) else { return 92 }
        let drive::Scalar::I32(sum) = called.results[0] else { return 93 }
        print(f"sum {sum}")
        var t = drive::call(b, "boom", [drive::Scalar::I32(13)])
        var tg = t.poll()
        while tg.is_none() {
            yield
            tg = t.poll()
        }
        let Some(tp) = take tg else { return 90 }
        let Ok(boomed) = drive::read_ran(take tp) else { return 94 }
        let Some(trap) = boomed.trap else { return 95 }
        print(f"trap call={trap.stage == drive::Stage::Call} panic={trap.panic.as_str()} at={trap.at.as_str()}")
        var a = drive::frames(b, 1, Script::frames())
        var ag = a.poll()
        while ag.is_none() {
            yield
            ag = a.poll()
        }
        let Some(apol) = take ag else { return 90 }
        if let Err(e) = drive::read_ran(take apol) {
            print(f"after: {e.message()}")
        }
    } else if what.as_str() == "at" {
        var steps = 0
        let times = [0.25, 3.0]
        for k in 0..2 {
            var f = drive::frame_at(b, times[k])
            var fg = f.poll()
            while fg.is_none() {
                yield
                fg = f.poll()
            }
            let Some(fp) = take fg else { return 90 }
            let ran = match drive::read_ran(take fp) {
                Ok(r) => take r,
                Err(e) => return failed(e),
            }
            assert(ran.trap.is_none(), "a frame at a time")
            steps += ran.steps.len()
        }
        print(f"frames {steps}")
    } else if what.as_str() == "fetch" {
        // Loaded: say so, and wait until the build's directory has been swapped.
        var m = files::write("out/loaded", utf8("1"))
        while m.poll().is_none() { yield }
        var waiting = true
        while waiting {
            var st = files::stat("out/go")
            var sg = st.poll()
            while sg.is_none() {
                yield
                sg = st.poll()
            }
            let Some(sp) = take sg else { return 90 }
            if answer(take sp).is_ok() {
                waiting = false
            } else {
                var t = time::after(0.02)
                while t.poll().is_none() { yield }
            }
        }
        var f = drive::frames(b, 2, Script::frames())
        while f.poll().is_none() { yield }
        var g = drive::logs(b)
        var gg = g.poll()
        while gg.is_none() {
            yield
            gg = g.poll()
        }
        let Some(gp) = take gg else { return 90 }
        let Ok(logs) = drive::read_logs(take gp) else { return 98 }
        for line in logs.lines {
            print(f"log {line.as_str()}")
        }
    } else if what.as_str() == "gpu" {
        var f = drive::frames(b, 3, Script::frames())
        while f.poll().is_none() { yield }
        var c = drive::capture(b)
        var cg = c.poll()
        while cg.is_none() {
            yield
            cg = c.poll()
        }
        let Some(cp) = take cg else { return 90 }
        let shot = match drive::read_capture(take cp) {
            Ok(s) => take s,
            Err(e) => return failed(e),
        }
        let Data::Blob(blob) = shot.rgba else { return 96 }
        var w = files::write_blob("out/screen.rgba", blob)
        var wg = w.poll()
        while wg.is_none() {
            yield
            wg = w.poll()
        }
        let Some(wp) = take wg else { return 90 }
        if let Err(e) = answer(take wp) {
            return failed(e)
        }
        print(f"captured {shot.width}x{shot.height}")
    } else {
        let timeout = if what.as_str() == "spin" { 1.0 } else { 0.0 }
        var s = drive::call(b, "spin", [drive::Scalar::I32(0)], timeout: timeout)
        var sg = s.poll()
        while sg.is_none() {
            yield
            sg = s.poll()
        }
        let Some(sp) = take sg else { return 90 }
        if let Err(e) = drive::read_ran(take sp) {
            print(f"spin: {e.message()}")
        }
    }
    var h = drive::hash(b)
    var hg = h.poll()
    while hg.is_none() {
        yield
        hg = h.poll()
    }
    let Some(hp) = take hg else { return 90 }
    let Ok(hash) = drive::read_hash(take hp) else { return 97 }
    print(f"hash {hash}")
    var x = drive::close(b)
    while x.poll().is_none() { yield }
    0
}

pub enum Running {
    Main(Job<main>),
    Two(Job<two>),
}

pub fn tool_init() -> Running {
    let given = args()
    if given.len() > 0 && given[0].as_str() == "two" {
        return Running::Two(two.start(0))
    }
    Running::Main(main.start(take given))
}

pub fn tool(running: mut Running) -> u32 {
    let step = match mut running {
        Running::Main(j) => j.resume(),
        Running::Two(j) => j.resume(),
    }
    match step {
        Step::Yielded => RUNNING,
        Step::Done(status) => status,
    }
}
"#;

/// A program that counts the events it reads and writes its count and the frame's time to a GPU
/// buffer each frame (the hash covers what it submits), with exports to call: `add`, `boom`, which panics on 13,
/// and `spin`, which never ends.
const SUBJECT: &str = r#"
use std::gpu::{GpuBuffer, buffer, write}
use std::input::events

pub struct Game {
    events: u32,
    counts: GpuBuffer<u32>,
}

pub fn init() -> Game {
    Game { events: 0, counts: buffer(2) }
}

pub fn frame(game: mut Game, time: f32, width: u32, height: u32) {
    for _ in events() {
        game.events += 1
    }
    write(mut game.counts, 0, [game.events, bitcast_u32(time)])
}

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn boom(n: i32) -> i32 {
    assert(n != 13, f"boom {n}")
    n
}

pub fn spin(n: i32) -> i32 {
    var i = n
    while i >= 0 {
        i = (i + 1) % 1000
    }
    i
}
"#;

/// The tool's package, the subject built into `src/build`, and the write root `out`, in a
/// scratch directory `name`.
fn setup(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = package(
        name,
        &[
            ("tool/wrela.toml", &manifest("driver")),
            ("tool/main.wrela", TOOL),
            ("subject/wrela.toml", &manifest("subject")),
            ("subject/main.wrela", SUBJECT),
        ],
    );
    let (src, out) = (dir.join("src"), dir.join("out"));
    std::fs::create_dir_all(&out).expect("out");
    let built = wrela()
        .arg("build")
        .arg(dir.join("subject"))
        .arg("-o")
        .arg(src.join("build"))
        .output()
        .expect("wrela build");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    (dir.join("tool"), src, out)
}

/// `wrela tool` on `tool` with `src` read-only, `out` writable, the first-light fixture as `fx`,
/// and `args`.
fn drive(tool: &Path, src: &Path, out: &Path, timeout: &str, args: &[&str]) -> Command {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/fixtures/first-light");
    let mut c = wrela();
    c.arg("tool").arg(tool);
    c.arg("--read").arg(format!("src={}", src.display()));
    c.arg("--read").arg(format!("fx={}", fixture.display()));
    c.arg("--write").arg(format!("out={}", out.display()));
    c.arg("--timeout").arg(timeout).arg("--").args(args);
    c
}

/// Runs `command`: its exit code and what it printed.
fn run(command: &mut Command) -> (Option<i32>, String) {
    let out = command.output().expect("run wrela tool");
    let text =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code(), text)
}

/// The number after `key ` in `text`'s line that starts with it.
fn value<'a>(text: &'a str, key: &str) -> &'a str {
    let line = text.lines().find(|l| l.starts_with(key)).unwrap_or_else(|| panic!("{text}"));
    line[key.len()..].trim()
}

#[test]
fn frames_on_the_cpu_give_the_hosts_hash_and_a_trap_says_what_and_where() {
    let (tool, src, out) = setup("drive-cpu");
    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &["cpu"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("frames 8"), "{printed}");
    assert!(printed.contains("sum 42"), "an export's result: {printed}");
    assert!(printed.contains("trap call=true panic=boom 13 at=main.wrela:"), "{printed}");
    assert!(printed.contains("after: the build runs nothing more: it trapped"), "{printed}");
    // The same build, script and frames on the host, directly. The trap's call added nothing.
    let text = std::fs::read_to_string(out.join("script.json")).expect("the tool's script");
    let script = wrela_host::parse_script(&text).expect("a script");
    let mut host = wrela_host::CpuBuild::load(src.join("build"))
        .expect("load")
        .quiet()
        .start_with(2)
        .expect("start");
    for i in 0..8 {
        host.lockstep_frame(i, 60.0, 640, 360, &script).expect("a frame");
    }
    assert_eq!(value(&printed, "hash"), host.hash().to_string(), "{printed}");
    let mut plain =
        wrela_host::CpuBuild::load(src.join("build")).expect("load").start().expect("start");
    for i in 0..8 {
        plain.lockstep_frame(i, 60.0, 640, 360, &[]).expect("a frame");
    }
    assert_ne!(plain.hash(), host.hash(), "the script changes what the program prints");
}

/// `frame_at` runs one frame at the time the tool gives, as the host's own frame at that time:
/// the same hash, and not lockstep's (whose frames are at 0 and 1/60 s).
#[test]
fn a_frame_at_a_time_is_the_hosts_frame_at_that_time() {
    let (tool, src, out) = setup("drive-at");
    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &["at"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("frames 2"), "{printed}");
    let start = || {
        wrela_host::CpuBuild::load(src.join("build")).expect("load").start_with(2).expect("start")
    };
    let mut host = start();
    for time in [0.25, 3.0] {
        host.frame(time, 640, 360).expect("a frame");
    }
    assert_eq!(value(&printed, "hash"), host.hash().to_string(), "{printed}");
    let mut lockstep = start();
    for i in 0..2 {
        lockstep.lockstep_frame(i, 60.0, 640, 360, &[]).expect("a frame");
    }
    assert_ne!(lockstep.hash(), host.hash(), "the frames' times are the tool's");
}

#[test]
fn a_gpu_frames_capture_comes_back_through_a_blob() {
    let (tool, src, out) = setup("drive-gpu");
    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &["gpu"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("captured 640x360"), "{printed}");
    let screen = std::fs::read(out.join("screen.rgba")).expect("the capture");
    assert_eq!(screen.len(), 640 * 360 * 4, "more than a request holds: a blob");
    // The same frames on the host, directly, on this machine's GPU.
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/fixtures/first-light");
    let storage = out.join("storage");
    let options = wrela_host::Options { storage: Some(storage), workers: 2, ..Default::default() };
    let mut host = wrela_host::Host::load_with(&fixture, &options).expect("a hardware GPU");
    let (run, _) = host.run_lockstep(3, 60.0, 640, 360, &[], false).expect("frames");
    assert_eq!(value(&printed, "hash"), run.hash.to_string(), "{printed}");
    assert_eq!(screen, run.frame, "the capture is the screen");
    assert!(screen.chunks(4).any(|p| *p != screen[..4]), "it drew something");
}

#[test]
fn a_deadline_stops_driven_wasm_that_never_ends() {
    let (tool, src, out) = setup("drive-spin");
    let started = Instant::now();
    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &["spin"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("spin: `call` ran past its deadline of 1 s"), "{printed}");
    assert!(printed.contains("hash "), "the build answers after its call was stopped: {printed}");
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
    // With no deadline of its own, the tool's end stops it.
    let started = Instant::now();
    let (code, printed) = run(&mut drive(&tool, &src, &out, "2", &["spin-forever"]));
    assert_eq!(code, Some(124), "{printed}");
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
}

/// A program whose main thread spins forever after starting two tasks: the first ends at once
/// (and its helper then holds its result back), the second spins forever.
const RACE: &str = r#"
use std::par::spawn

@deterministic
fn quick(n: take u32) -> u32 {
    n + 1
}

@deterministic
fn forever(n: take u32) -> u32 {
    var i = n
    while i < 4000000000 {
        i = (i * 3 + 1) % 1000
    }
    i
}

pub fn frame(time: f32, width: u32, height: u32) {}

pub fn race(n: u32) -> u32 {
    let a = spawn(n, quick)
    let b = spawn(n, forever)
    var i = n
    while i < 4000000000 {
        i = (i * 7 + 1) % 1000
    }
    take a.join() + take b.join()
}
"#;

/// The main thread's call stops at its deadline while its helper is held in a wait (no WASM
/// runs there to see the deadline); the deadline is then cleared, as an answered operation
/// clears it. The helper, when it goes on to the task that never ends, stops too: the first
/// thread to see the deadline stopped the build for good. So dropping the host ends.
#[test]
fn a_helper_held_past_the_main_threads_deadline_stops_too() {
    let dir = package("drive-race", &[("wrela.toml", &manifest("race")), ("main.wrela", RACE)]);
    let build = dir.join("build-out");
    let built = wrela().arg("build").arg(&dir).arg("-o").arg(&build).output().expect("wrela build");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let handle = wrela_host::open_build_dir(&build).expect("the build's directory");
    let stop = wrela_host::Stop::new().expect("a stop");
    let driven = wrela_host::CpuBuild::load_driven(handle, &build, stop.clone()).expect("driven");
    let mut host = driven.instantiate_in(2, Some(&dir.join("storage"))).expect("instantiate");
    host.hold_jobs(1_000_000);
    host.init().expect("init");
    stop.set_deadline(Some(Instant::now() + Duration::from_millis(300)));
    let e = host.call_export("race", &[wrela_host::Value::I32(0)]).expect_err("interrupted");
    assert!(e.to_string().contains("interrupt"), "{e}");
    stop.set_deadline(None);
    // The helper's hold ends, and it starts the task that never ends.
    std::thread::sleep(Duration::from_millis(1500));
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(host);
        let _ = done.send(());
    });
    assert!(ended.recv_timeout(Duration::from_secs(5)).is_ok(), "the helper still runs");
}

/// Two captures at once whose screens together are past the blob budget, or past its handles:
/// the host reserves each one's bytes and blob before it reads the screen back, so one is
/// refused, before it's read, and the other comes back.
#[test]
fn two_captures_past_the_blob_budget_admit_one() {
    let (tool, src, out) = setup("drive-budget");
    for (budget, why) in [("1500K", "budget"), ("2G,1", "blobs")] {
        let c = drive(&tool, &src, &out, "60", &["two"]);
        let at = c.get_args().position(|a| a == "--timeout").expect("a timeout");
        let mut args: Vec<std::ffi::OsString> = c.get_args().map(Into::into).collect();
        args.insert(at, "--blob-budget".into());
        args.insert(at + 1, budget.into());
        let mut c = wrela();
        c.args(args);
        let (code, printed) = run(&mut c);
        assert_eq!(code, Some(0), "{printed}");
        assert_eq!(printed.matches("captured").count(), 1, "{budget}: {printed}");
        assert!(
            printed.contains("refused: the screen's 921600 bytes") && printed.contains(why),
            "{printed}"
        );
    }
}

/// A program that fetches `data.txt` from its build on its first frame, and prints what it got
/// on its second.
const FETCHER: &str = r#"
use std::io::{Pending, fetch, print}

pub struct Game {
    asked: Option<Pending<u8>>,
}

pub fn init() -> Game {
    Game { asked: None }
}

pub fn frame(game: mut Game, time: f32, width: u32, height: u32) {
    match mut game.asked {
        Some(p) => {
            match p.poll() {
                Some(Ok(bytes)) => print(f"fetched {bytes.len()} bytes"),
                Some(Err(e)) => print(f"fetch failed: {e.message()}"),
                None => print("fetch waits"),
            }
            return
        },
        None => {},
    }
    game.asked = Some(fetch("data.txt"))
}
"#;

/// A driven build's program fetches through the build's directory as it was opened at load: the
/// directory swapped for a link out of the root after the load leads nowhere.
#[cfg(unix)]
#[test]
fn a_driven_fetch_reads_the_build_opened_at_load() {
    let dir = package(
        "drive-fetch",
        &[
            ("tool/wrela.toml", &manifest("driver")),
            ("tool/main.wrela", TOOL),
            ("subject/wrela.toml", &manifest("fetcher")),
            ("subject/main.wrela", FETCHER),
        ],
    );
    let (tool, src, out) = (dir.join("tool"), dir.join("src"), dir.join("out"));
    std::fs::create_dir_all(&out).expect("out");
    let built = wrela()
        .arg("build")
        .arg(dir.join("subject"))
        .arg("-o")
        .arg(src.join("build"))
        .output()
        .expect("wrela build");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    std::fs::write(src.join("build/data.txt"), "inside").expect("data");
    let outside = dir.join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(outside.join("data.txt"), "the user's secret").expect("secret");
    let child = drive(&tool, &src, &out, "60", &["fetch"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("wrela tool");
    let started = Instant::now();
    while !out.join("loaded").exists() {
        assert!(started.elapsed() < Duration::from_secs(60), "the tool never loaded the build");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::rename(src.join("build"), src.join("moved")).expect("move the build");
    std::os::unix::fs::symlink(&outside, src.join("build")).expect("a link out");
    std::fs::write(out.join("go"), "").expect("go");
    let output = child.wait_with_output().expect("wait");
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{printed}");
    assert!(printed.contains("log fetched 6 bytes"), "the build as opened: {printed}");
    assert!(!printed.contains("17 bytes"), "nothing from where the link leads: {printed}");
}

/// A program that posts to `notes/a` and `notes/b` on its first frame, and on each later frame
/// says what each post got, and posts to `notes/c`, which nothing answers.
const POSTER: &str = r#"
use std::io::{Pending, post, print}

pub struct Game {
    asked: Vec<Pending<u8>>,
}

pub fn init() -> Game {
    Game { asked: Vec::new() }
}

pub fn frame(game: mut Game, time: f32, width: u32, height: u32) {
    if game.asked.is_empty() {
        game.asked.push(post("notes/a", [u8(1), 2]))
        game.asked.push(post("notes/b", [u8(3)]))
        return
    }
    for i in 0..game.asked.len() {
        match game.asked[i].poll() {
            Some(Ok(bytes)) => print(f"post {i}: {bytes.len()} bytes"),
            Some(Err(e)) => print(f"post {i} failed: {e.message()}"),
            None => {},
        }
    }
    game.asked.push(post("notes/c", [u8(4)]))
}
"#;

/// A tool that drives the poster: its posts, answered (the first), failed (the second), and
/// answered again (refused), then a frame, and the build closed with a post unanswered.
const POST_TOOL: &str = r#"
use std::io::print
use std::job::{Job, Step}
use std::tool::{RUNNING, answer, args}
use std::tool::blob::Data
use std::tool::drive

@job
fn main(given: take Vec<String>) -> u32 {
    var l = drive::load("src/build", drive::cpu())
    var loaded = l.poll()
    while loaded.is_none() {
        yield
        loaded = l.poll()
    }
    let Some(lp) = take loaded else { return 90 }
    let Ok(it) = drive::read_loaded(take lp) else { return 91 }
    let b = it.build
    var i = drive::init(b)
    while i.poll().is_none() { yield }
    var f = drive::frames(b, 1, std::input::Script::frames())
    while f.poll().is_none() { yield }
    var p = drive::posts(b)
    var got = p.poll()
    while got.is_none() {
        yield
        got = p.poll()
    }
    let Some(pp) = take got else { return 92 }
    let Ok(posts) = drive::read_posts(take pp) else { return 93 }
    for post in posts {
        let Data::Bytes(body) = post.body.clone() else { return 94 }
        print(f"posted {post.url.as_str()} {body.len()}")
    }
    var a = drive::answer(b, posts[0].id, Data::Bytes(Vec::from([u8(7), 8, 9])))
    var answered = a.poll()
    while answered.is_none() {
        yield
        answered = a.poll()
    }
    let Some(ap) = take answered else { return 95 }
    print(f"answered {answer(take ap).is_ok()}")
    var x = drive::fail(b, posts[1].id, "no")
    while x.poll().is_none() { yield }
    var again = drive::answer(b, posts[0].id, Data::empty())
    var twice = again.poll()
    while twice.is_none() {
        yield
        twice = again.poll()
    }
    let Some(tp) = take twice else { return 96 }
    print(f"answered twice {answer(take tp).is_ok()}")
    var g = drive::frames(b, 1, std::input::Script::frames())
    while g.poll().is_none() { yield }
    var o = drive::logs(b)
    var logged = o.poll()
    while logged.is_none() {
        yield
        logged = o.poll()
    }
    let Some(op) = take logged else { return 97 }
    let Ok(lines) = drive::read_logs(take op) else { return 98 }
    for line in lines.lines {
        print(f"log {line.as_str()}")
    }
    // The post to `notes/c` waits: closing the build gives it up.
    var c = drive::close(b)
    while c.poll().is_none() { yield }
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

/// A driven build's posts go to the tool: each is given once, with its URL and body, and
/// answered or failed once; the program reads the answers from its next call on. A build closed
/// with a post unanswered closes at once.
#[test]
fn a_driven_builds_posts_go_to_the_tool() {
    let dir = package(
        "drive-posts",
        &[
            ("tool/wrela.toml", &manifest("poster_tool")),
            ("tool/main.wrela", POST_TOOL),
            ("subject/wrela.toml", &manifest("poster")),
            ("subject/main.wrela", POSTER),
        ],
    );
    let (tool, src, out) = (dir.join("tool"), dir.join("src"), dir.join("out"));
    std::fs::create_dir_all(&out).expect("out");
    let built = wrela()
        .arg("build")
        .arg(dir.join("subject"))
        .arg("-o")
        .arg(src.join("build"))
        .output()
        .expect("wrela build");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let started = Instant::now();
    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &[]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(started.elapsed() < Duration::from_secs(30), "closing gave the post up");
    for line in [
        "posted notes/a 2",
        "posted notes/b 1",
        "answered true",
        "answered twice false",
        "log post 0: 3 bytes",
        "log post 1 failed",
    ] {
        assert!(printed.contains(line), "{line}: {printed}");
    }
}

/// A piece with a voice: started on its first frame, it plays a saw in the left channel and a
/// quieter, slower one in the right, so each sample's bits depend on every quantum before it.
const VOICE: &str = r#"
use std::audio::play

pub struct Saw {
    left: f32,
    right: f32,
}

@audio
fn sing(s: mut Saw, left: mut [f32], right: mut [f32]) {
    for i in 0..left.len() {
        s.left += 0.0071
        if s.left > 1.0 {
            s.left -= 2.0
        }
        s.right += 0.0013
        if s.right > 1.0 {
            s.right -= 2.0
        }
        left[i] = s.left * 0.5
        right[i] = s.right * 0.25
    }
}

pub struct Piece {
    started: bool,
}

pub fn init() -> Piece {
    Piece { started: false }
}

pub fn frame(p: mut Piece, time: f32, width: u32, height: u32) {
    if !p.started {
        p.started = true
        play(Saw { left: 0.0, right: 0.0 }, sing)
    }
}
"#;

/// A tool that renders the piece's voice: 300 quanta at once, written to `out/voice.f32`, then
/// 10 more each timed, written to `out/timed.f32`; and with `early`, the voice asked for before
/// `init` and before the voice starts.
const VOICE_TOOL: &str = r#"
use std::io::{IoError, print}
use std::job::{Job, Step}
use std::tool::{RUNNING, answer, args}
use std::tool::blob
use std::tool::blob::Data
use std::tool::drive
use std::tool::files

@job
fn write(blob: take Data, path: take String) -> u32 {
    let Data::Blob(b) = blob else { return 80 }
    var w = files::write_blob(path.as_str(), b)
    var got = w.poll()
    while got.is_none() {
        yield
        got = w.poll()
    }
    let Some(polled) = take got else { return 81 }
    if answer(take polled).is_err() {
        return 82
    }
    0
}

fn rendered(polled: take Result<Vec<u8>, IoError>) -> Option<drive::Audio> {
    match drive::read_audio(take polled) {
        Ok(a) => Some(a),
        Err(e) => {
            print(f"refused: {e.message()}")
            None
        },
    }
}

@job
fn main(given: take Vec<String>) -> u32 {
    let early = !given.is_empty()
    var options = drive::cpu()
    options.width = 16
    options.height = 16
    var l = drive::load("src/build", options)
    var lg = l.poll()
    while lg.is_none() {
        yield
        lg = l.poll()
    }
    let Some(lp) = take lg else { return 90 }
    let Ok(loaded) = drive::read_loaded(take lp) else { return 91 }
    let b = loaded.build
    if early {
        var a = drive::audio(b, 1)
        var ag = a.poll()
        while ag.is_none() {
            yield
            ag = a.poll()
        }
        let Some(ap) = take ag else { return 92 }
        let _ = rendered(take ap)
    }
    var i = drive::init(b)
    while i.poll().is_none() { yield }
    if early {
        var a = drive::audio(b, 1)
        var ag = a.poll()
        while ag.is_none() {
            yield
            ag = a.poll()
        }
        let Some(ap) = take ag else { return 92 }
        let _ = rendered(take ap)
    }
    var f = drive::frames(b, 1, std::input::Script::frames())
    while f.poll().is_none() { yield }
    var a = drive::audio(b, 300)
    var ag = a.poll()
    while ag.is_none() {
        yield
        ag = a.poll()
    }
    let Some(ap) = take ag else { return 92 }
    let Some(voice) = rendered(take ap) else { return 0 }
    if voice.times.is_some() {
        return 93
    }
    var w = Box::new(write.start(take voice.samples, String::from("out/voice.f32")))
    loop {
        match w.get_mut().resume() {
            Step::Yielded => yield,
            Step::Done(0) => break,
            Step::Done(status) => return status,
        }
    }
    var t = drive::audio(b, 10, timed: true)
    var tg = t.poll()
    while tg.is_none() {
        yield
        tg = t.poll()
    }
    let Some(tp) = take tg else { return 94 }
    let Some(timed) = rendered(take tp) else { return 0 }
    let Some(Data::Blob(times)) = timed.times.clone() else { return 95 }
    var n = blob::len(times)
    var ng = n.poll()
    while ng.is_none() {
        yield
        ng = n.poll()
    }
    let Some(np) = take ng else { return 96 }
    let Ok(len) = blob::read_len(take np) else { return 97 }
    print(f"times {len}")
    var w2 = Box::new(write.start(take timed.samples, String::from("out/timed.f32")))
    loop {
        match w2.get_mut().resume() {
            Step::Yielded => yield,
            Step::Done(0) => break,
            Step::Done(status) => return status,
        }
    }
    print("rendered")
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

/// A driven build's voice renders offline: the samples, through a blob, are the host's own
/// (`CpuHost::render_audio` after the first frame), bit for bit, and so are the timed quanta
/// after them; a voice asked for before `init`, or before the program starts it, is refused;
/// and a render past the blob budget is refused before it runs.
#[test]
fn a_driven_voice_renders_the_hosts_samples() {
    let dir = package(
        "drive-voice",
        &[
            ("tool/wrela.toml", &manifest("voice_tool")),
            ("tool/main.wrela", VOICE_TOOL),
            ("subject/wrela.toml", &manifest("voice")),
            ("subject/main.wrela", VOICE),
        ],
    );
    let (tool, src, out) = (dir.join("tool"), dir.join("src"), dir.join("out"));
    std::fs::create_dir_all(&out).expect("out");
    let built = wrela()
        .arg("build")
        .arg(dir.join("subject"))
        .arg("-o")
        .arg(src.join("build"))
        .output()
        .expect("wrela build");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &[]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("times 80") && printed.contains("rendered"), "{printed}");
    let mut host = wrela_host::CpuBuild::load(src.join("build"))
        .expect("load")
        .quiet()
        .start()
        .expect("start");
    host.frame(0.0, 16, 16).expect("the first frame");
    let bytes = |s: Vec<f32>| s.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let voice = std::fs::read(out.join("voice.f32")).expect("the voice");
    assert_eq!(voice.len(), 300 * 128 * 2 * 4);
    assert_eq!(voice, bytes(host.render_audio(300).expect("render")), "the host's samples");
    let timed = std::fs::read(out.join("timed.f32")).expect("the timed quanta");
    assert_eq!(timed, bytes(host.render_audio(10).expect("render")), "the next quanta");
    assert!(voice.chunks(4).any(|s| s != [0; 4]), "it sang");

    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &["early"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("refused: `audio` runs after `init`"), "{printed}");
    assert!(printed.contains("refused: it hasn't started a voice"), "{printed}");

    let c = drive(&tool, &src, &out, "60", &[]);
    let at = c.get_args().position(|a| a == "--timeout").expect("a timeout");
    let mut args: Vec<std::ffi::OsString> = c.get_args().map(Into::into).collect();
    args.insert(at, "--blob-budget".into());
    args.insert(at + 1, "100K".into());
    let mut c = wrela();
    c.args(args);
    let (code, printed) = run(&mut c);
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("refused: the voice's 307200 bytes"), "{printed}");
    assert!(printed.contains("budget"), "{printed}");
}

/// A piece whose voice never finishes its first quantum.
const STUCK_VOICE: &str = r#"
use std::audio::play

pub struct Stuck {
    n: u32,
}

@audio
fn sing(s: mut Stuck, left: mut [f32], right: mut [f32]) {
    while s.n < 4000000000 {
        s.n = (s.n * 3 + 1) % 1000
    }
}

pub struct Piece {
    started: bool,
}

pub fn init() -> Piece {
    Piece { started: false }
}

pub fn frame(p: mut Piece, time: f32, width: u32, height: u32) {
    if !p.started {
        p.started = true
        play(Stuck { n: 0 }, sing)
    }
}
"#;

/// A tool that asks the stuck voice for a quantum with a deadline of 1 s, then the build's hash.
const STUCK_TOOL: &str = r#"
use std::io::print
use std::job::{Job, Step}
use std::tool::{RUNNING, args}
use std::tool::drive

@job
fn main(given: take Vec<String>) -> u32 {
    var l = drive::load("src/build", drive::cpu())
    var lg = l.poll()
    while lg.is_none() {
        yield
        lg = l.poll()
    }
    let Some(lp) = take lg else { return 90 }
    let Ok(loaded) = drive::read_loaded(take lp) else { return 91 }
    let b = loaded.build
    var i = drive::init(b)
    while i.poll().is_none() { yield }
    var f = drive::frames(b, 1, std::input::Script::frames())
    while f.poll().is_none() { yield }
    var a = drive::audio(b, 1, timeout: 1.0)
    var ag = a.poll()
    while ag.is_none() {
        yield
        ag = a.poll()
    }
    let Some(ap) = take ag else { return 92 }
    match drive::read_audio(take ap) {
        Ok(_) => print("rendered"),
        Err(e) => print(f"refused: {e.message()}"),
    }
    var h = drive::hash(b)
    while h.poll().is_none() { yield }
    print("hashed")
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

/// A voice that never ends its quantum is stopped at the request's deadline, and the build still
/// answers what doesn't run it.
#[test]
fn a_driven_voice_past_its_deadline_is_stopped() {
    let dir = package(
        "drive-stuck-voice",
        &[
            ("tool/wrela.toml", &manifest("stuck_tool")),
            ("tool/main.wrela", STUCK_TOOL),
            ("subject/wrela.toml", &manifest("stuck")),
            ("subject/main.wrela", STUCK_VOICE),
        ],
    );
    let (tool, src, out) = (dir.join("tool"), dir.join("src"), dir.join("out"));
    std::fs::create_dir_all(&out).expect("out");
    let built = wrela()
        .arg("build")
        .arg(dir.join("subject"))
        .arg("-o")
        .arg(src.join("build"))
        .output()
        .expect("wrela build");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let started = Instant::now();
    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &[]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("refused: `audio` ran past its deadline of 1 s"), "{printed}");
    assert!(printed.contains("hashed"), "{printed}");
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
}
