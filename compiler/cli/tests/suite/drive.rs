//! `std::tool::drive` (language.md §15.5): a tool loads a build on the native host, in a process
//! of its own, and runs it in steps: the same frames and script give the same hash as the host
//! run directly, a GPU frame's capture comes back through a blob, an export is called, a trap
//! says what and where and skips the later steps that run code, a deadline kills the build's
//! process, and a killed tool host leaves no build's process behind.

use crate::common::{group_alive, manifest, package, run, within, wrela};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// A tool that does what its arguments say with the build in `src/build` (or `fx`'s, for the
/// GPU), and prints what it got:
/// - `cpu`: in one request, `init`, frames with a script (the script goes to
///   `out/script.json`), a call of `add`, a trap in `boom`, then a frame and a call that the
///   trap skips, with a hash after the trap and at the end; then a later request, whose frame
///   is skipped too and whose hash runs;
/// - `at`: frames at 0.25 s and 3 s, outside lockstep, on the CPU, and the hash;
/// - `run`: `run`'s one request: the build loaded, two frames and the hash, and closed;
/// - `record`: the subject on the GPU, recording its commands: two frames, its buffers (the
///   newest's bytes go to `out/buffer.bin`), and the commands it submitted, a line each;
/// - `gpu`: three frames on the GPU, the screen captured and written to `out/screen.rgba`, the
///   hash;
/// - `limits`: 1025 steps, then 40000 frames' times after a capture, then a capture alone;
/// - `loads`: the build loaded three times, each load's code's source and time;
/// - `spin`: a call of `spin`, which never ends, with a deadline of 1 s (`out/asked` says the
///   request is made), then the hash; `out/stopped` says it's done, and it waits for `out/go`;
/// - `hold <export>`: a call of the export, which never ends (`out/asked` says it's made);
/// - `spin-forever`: a call of `spin` with no deadline of its own (the tool's `--timeout` ends
///   it).
const TOOL: &str = r#"
use std::input::{Key, Script}
use std::io::print
use std::tool::{ToolError, args}
use std::tool::blob::Data
use std::tool::drive
use std::tool::drive::{Scalar, Step}
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

fn mark(path: str) {
    let _ = files::write(path, utf8("1")).wait()
}

fn wait_for(path: str) {
    while files::stat(path).wait().is_err() {
        let _ = time::after(0.02).wait()
    }
}

fn call(export: str, n: i32) -> Step {
    Step::Call(String::from(export), Vec::from([Scalar::I32(n)]))
}

fn report(seen: take Result<drive::Seen, ToolError>) {
    match take seen {
        Ok(s) => print(f"captured {s.captures.len()}"),
        Err(e) => print(f"refused: {e.message()}"),
    }
}

fn two() -> u32 {
    // The fixture twice, a frame each, then both screens captured at once.
    var builds: Vec<drive::Build> = Vec::new()
    for _ in 0..2 {
        let loaded = match drive::load("fx", drive::gpu()).wait() {
            Ok(x) => x,
            Err(e) => return failed(e),
        }
        let _ = drive::steps(loaded.build, [Step::Init, Step::Frames(1, Script::frames())]).wait()
        builds.push(loaded.build)
    }
    let a = drive::steps(builds[0], [Step::Capture])
    let b = drive::steps(builds[1], [Step::Capture])
    report(take a.wait())
    report(take b.wait())
    0
}

fn loads() -> u32 {
    for _ in 0..3 {
        let Ok(loaded) = drive::load("src/build", drive::cpu()).wait() else { return 91 }
        let code = match loaded.code {
            drive::Code::Compiled => "compiled",
            drive::Code::Cached => "cached",
            drive::Code::Reused => "reused",
        }
        print(f"load {code} {loaded.elapsed}")
        let _ = drive::close(loaded.build).wait()
    }
    0
}

pub fn tool() -> u32 {
    let given = args()
    borrow what = given[0]
    if what.as_str() == "two" {
        return two()
    }
    if what.as_str() == "loads" {
        return loads()
    }
    if what.as_str() == "write" {
        match files::write(given[1].as_str(), utf8("changed")).wait() {
            Ok(_) => print("write: ok"),
            Err(e) => print(f"write: {e.message()}"),
        }
        return 0
    }
    if what.as_str() == "hold-load" {
        // The load is made, and its compile held by the time the tool ends.
        let _ = drive::load("src/build", drive::cpu())
        let _ = time::after(1.0).wait()
        return 0
    }
    if what.as_str() == "wait-load" {
        let _ = drive::load("src/build", drive::cpu()).wait()
        return 0
    }
    if what.as_str() == "load-deadline" {
        var quick = drive::cpu()
        quick.timeout = 1.0
        let Ok(t) = time::utc_now().wait() else { return 90 }
        match drive::load("src/build", quick).wait() {
            Ok(_) => print("loaded"),
            Err(e) => {
                let Ok(u) = time::utc_now().wait() else { return 90 }
                let took = f64(u.seconds - t.seconds) + (f64(u.nanos) - f64(t.nanos)) * 0.000000001
                print(f"load: {e.message()} after {took}")
            },
        }
        return 0
    }
    var options = if what.as_str() == "gpu" || what.as_str() == "record" || what.as_str() == "limits" { drive::gpu() } else { drive::cpu() }
    options.workers = 2
    options.record = what.as_str() == "record"
    let at = if what.as_str() == "gpu" || what.as_str() == "limits" { "fx" } else { "src/build" }
    if what.as_str() == "run" {
        let seen = match drive::run(at, options, [Step::Init, Step::Frames(2, Script::frames()), Step::Hash]).wait() {
            Ok(s) => take s,
            Err(e) => return failed(e),
        }
        print(f"ran {seen.ran.len()}")
        print(f"hash {seen.hashes[0]}")
        return 0
    }
    let loaded = match drive::load(at, options).wait() {
        Ok(x) => x,
        Err(e) => return failed(e),
    }
    let b = loaded.build
    if what.as_str() == "cpu" {
        let _ = files::write("out/script.json", utf8(SCRIPT.as_json())).wait()
        let seen = match drive::steps(
            b,
            [
                Step::Init,
                Step::Frames(8, SCRIPT.clone()),
                Step::Call(String::from("add"), Vec::from([Scalar::I32(2), Scalar::I32(40)])),
                call("boom", 13),
                Step::Hash,
                Step::Frames(1, Script::frames()),
                call("boom", 1),
                Step::Phases,
                Step::Hash,
            ],
        ).wait() {
            Ok(s) => take s,
            Err(e) => return failed(e),
        }
        print(f"ran {seen.ran.len()} frames {seen.ran[1].spans.len()} hashes {seen.hashes.len()} phases {seen.phases.len()}")
        let Scalar::I32(sum) = seen.ran[2].results[0] else { return 93 }
        print(f"sum {sum}")
        let Some(trap) = seen.trap else { return 95 }
        print(f"trap call={trap.stage == drive::Stage::Call} step={trap.step} panic={trap.panic.as_str()} at={trap.at.as_str()}")
        print(f"same {seen.hashes[0] == seen.hashes[1]}")
        let later = match drive::steps(b, [Step::Frames(1, Script::frames()), Step::Hash]).wait() {
            Ok(s) => take s,
            Err(e) => return failed(e),
        }
        print(f"later ran={later.ran.len()} trapped={later.trap.is_some()} same={later.hashes[0] == seen.hashes[0]}")
        print(f"hash {seen.hashes[0]}")
    } else if what.as_str() == "record" {
        let seen = match drive::steps(b, [Step::Init, Step::Frames(2, Script::frames()), Step::Buffers]).wait() {
            Ok(s) => take s,
            Err(e) => return failed(e),
        }
        let held = seen.buffers[0].clone()
        print(f"buffers {held.len()}")
        let read = match drive::steps(b, [Step::ReadBuffer(held[held.len() - 1]), Step::Commands]).wait() {
            Ok(s) => take s,
            Err(e) => return failed(e),
        }
        let Data::Blob(blob) = read.bytes[0].clone() else { return 96 }
        let _ = files::write_blob("out/buffer.bin", blob).wait()
        let recorded = read.commands[0].clone()
        for k in 0..recorded.commands.len() {
            let c = recorded.commands[k].clone()
            var bound = String::new()
            for x in c.bindings {
                bound.push_str(f" {x.handle}:{x.offset}:{x.size}")
            }
            var carried = String::new()
            for x in c.bytes {
                carried.push_str(f" {x}")
            }
            print(f"command {c.kind.as_str()} {c.label.as_str()} {c.pipeline.as_str()} {c.groups.x},{c.groups.y},{c.groups.z}{bound.as_str()} {c.target.as_str()}{carried.as_str()}")
        }
        print(f"waiting {recorded.waiting}")
        if let Err(e) = drive::steps(b, [Step::ReadBuffer(999999)]).wait() {
            print(f"missing: {e.message()}")
        }
    } else if what.as_str() == "at" {
        let seen = match drive::steps(b, [Step::Init, Step::FrameAt(0.25), Step::FrameAt(3.0)]).wait() {
            Ok(s) => take s,
            Err(e) => return failed(e),
        }
        assert(seen.trap.is_none(), "frames at a time")
        print(f"frames {seen.ran[1].spans.len() + seen.ran[2].spans.len()}")
    } else if what.as_str() == "fetch" {
        // Loaded: say so, and wait until the build's directory has been swapped.
        mark("out/loaded")
        wait_for("out/go")
        let Ok(seen) = drive::steps(b, [Step::Init, Step::Frames(2, Script::frames())]).wait() else { return 98 }
        for line in seen.lines {
            print(f"log {line.as_str()}")
        }
    } else if what.as_str() == "gpu" {
        let shot = match drive::steps(b, [Step::Init, Step::Frames(3, Script::frames()), Step::Capture]).wait() {
            Ok(s) => s.captures[0].clone(),
            Err(e) => return failed(e),
        }
        let Data::Blob(blob) = shot.rgba else { return 96 }
        if let Err(e) = files::write_blob("out/screen.rgba", blob).wait() {
            return failed(e)
        }
        print(f"captured {shot.width}x{shot.height}")
    } else if what.as_str() == "limits" {
        var many: Vec<Step> = Vec::new()
        for _ in 0..1025 {
            many.push(Step::Hash)
        }
        if let Err(e) = drive::steps(b, many).wait() {
            print(f"many: {e.message()}")
        }
        let _ = drive::steps(b, [Step::Init, Step::Frames(1, Script::frames())]).wait()
        if let Err(e) = drive::steps(b, [Step::Capture, Step::Frames(40000, Script::frames())]).wait() {
            print(f"past: {e.message()}")
        }
        report(drive::steps(b, [Step::Capture]).wait())
    } else if what.as_str() == "many-lines" {
        match drive::steps(b, [Step::Init, call("chatter", 30000)]).wait() {
            Ok(got) => {
                var seen = take got
                let log = seen.log.is_some()
                let Ok(all) = drive::lines(mut seen) else { return 99 }
                print(f"all {all.len()} first {all[0].as_str()} last {all[all.len() - 1].as_str()} inline {seen.lines.len()} log {log} dropped {seen.dropped_lines} kept {seen.log.is_some()}")
            },
            Err(e) => print(f"refused: {e.message()}"),
        }
        let Ok(got) = drive::steps(b, [call("chatter", 1)]).wait() else { return 98 }
        var one = take got
        let log = one.log.is_some()
        let Ok(all) = drive::lines(mut one) else { return 99 }
        print(f"then {all.len()} inline {one.lines.len()} log {log}")
    } else if what.as_str() == "many-lines-again" {
        // Each log is released once read: a budget for one log holds them one after another.
        let Ok(_) = drive::steps(b, [Step::Init]).wait() else { return 97 }
        for k in 0..3 {
            let Ok(got) = drive::steps(b, [call("chatter", 30000)]).wait() else { return 98 }
            var seen = take got
            let Ok(all) = drive::lines(mut seen) else { return 99 }
            print(f"again {k}: {all.len()}")
        }
    } else if what.as_str() == "lines" {
        if let Err(e) = drive::steps(b, [Step::Init, call("chatter", 5000), call("missing", 0)]).wait() {
            print(f"failed with its lines={e.message().len() > 10000}")
        }
        let Ok(seen) = drive::steps(b, [call("chatter", 1)]).wait() else { return 98 }
        print(f"lines {seen.lines.len()} dropped {seen.dropped_lines} first {seen.lines[0].as_str()}")
    } else if what.as_str() == "hold" {
        let _ = drive::steps(b, [Step::Init]).wait()
        let held = drive::steps(b, [call(given[1].as_str(), 0)], timeout: 600.0)
        mark("out/asked")
        let _ = take held.wait()
    } else {
        let _ = drive::steps(b, [Step::Init]).wait()
        let timeout = if what.as_str() == "spin" { 1.0 } else { 0.0 }
        let spun = drive::steps(b, [call("spin", 0)], timeout: timeout)
        mark("out/asked")
        if let Err(e) = take spun.wait() {
            print(f"spin: {e.message()}")
        }
        if let Err(e) = drive::steps(b, [Step::Hash]).wait() {
            print(f"after: {e.message()}")
        }
        mark("out/stopped")
        wait_for("out/go")
    }
    if what.as_str() != "spin" {
        let Ok(seen) = drive::steps(b, [Step::Hash]).wait() else { return 97 }
        print(f"hash {seen.hashes[0]}")
    }
    let _ = drive::close(b).wait()
    0
}
"#;

/// A program that counts the events it reads and writes its count and the frame's time to a GPU
/// buffer each frame (the hash covers what it submits), with exports to call: `add`, `boom`,
/// which panics on 13, `spin`, which never ends, and `join`, which waits for a task that never
/// ends.
const SUBJECT: &str = r#"
use std::gpu::{GpuBuffer, buffer, label, write}
use std::input::events
use std::io::store
use std::par::spawn

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
    label("counts")
    write(mut game.counts, 0, [game.events, bitcast_u32(time)])
    let stored: [u8; 3] = [7, 8, 9]
    let _ = store("probe", stored)
}

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn chatter(n: i32) -> i32 {
    for i in 0..n {
        std::io::print(f"chatter {i}")
    }
    n
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

@deterministic
fn forever(n: take u32) -> u32 {
    var i = n
    while i < 4000000000 {
        i = (i * 3 + 1) % 1000
    }
    i
}

pub fn join(n: u32) -> u32 {
    let t = spawn(n, forever)
    take t.join()
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
    build(&dir.join("subject"), &src.join("build"));
    (dir.join("tool"), src, out)
}

/// Builds the package `dir` into `out`.
fn build(dir: &Path, out: &Path) {
    let built = wrela().arg("build").arg(dir).arg("-o").arg(out).output().expect("wrela build");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
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

/// `command` with `--blob-budget budget` before its `--timeout`.
fn budgeted(command: Command, budget: &str) -> Command {
    let at = command.get_args().position(|a| a == "--timeout").expect("a timeout");
    let mut args: Vec<std::ffi::OsString> = command.get_args().map(Into::into).collect();
    args.insert(at, "--blob-budget".into());
    args.insert(at + 1, budget.into());
    let mut c = wrela();
    c.args(args);
    c
}

/// The number after `key ` in `text`'s line that starts with it.
fn value<'a>(text: &'a str, key: &str) -> &'a str {
    let line = text.lines().find(|l| l.starts_with(key)).unwrap_or_else(|| panic!("{text}"));
    line[key.len()..].trim()
}

/// The pids of the driven builds' processes of the tool host `host`: their command lines name it.
fn driven_by(host: u32) -> Vec<i32> {
    let marker = format!("tool --drive {host}$");
    let out = Command::new("pgrep").args(["-f", &marker]).output().expect("pgrep");
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect()
}

#[test]
fn steps_run_in_order_and_a_trap_skips_later_code_but_not_observations() {
    let (tool, src, out) = setup("drive-cpu");
    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &["cpu"]));
    assert_eq!(code, Some(0), "{printed}");
    // `init`, the frames, `add`'s call and `boom`'s ran; the frame and the call after the trap
    // didn't, and both hashes and the phases did.
    assert!(printed.contains("ran 4 frames 8 hashes 2 phases 1"), "{printed}");
    assert!(printed.contains("sum 42"), "an export's result: {printed}");
    assert!(printed.contains("trap call=true step=3 panic=boom 13 at=main.wrela:"), "{printed}");
    assert!(printed.contains("same true"), "nothing ran after the trap: {printed}");
    assert!(printed.contains("later ran=0 trapped=true same=true"), "{printed}");
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

/// `run` loads the build, runs its steps and closes it, in one request: the host's hash.
#[test]
fn a_run_loads_steps_and_closes_in_one_request() {
    let (tool, src, out) = setup("drive-run");
    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &["run"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("ran 2"), "{printed}");
    let mut host =
        wrela_host::CpuBuild::load(src.join("build")).expect("load").start_with(2).expect("start");
    for i in 0..2 {
        host.lockstep_frame(i, 60.0, 640, 360, &[]).expect("a frame");
    }
    assert_eq!(value(&printed, "hash"), host.hash().to_string(), "{printed}");
}

/// A build loaded to record gives the tool its GPU buffers, one's bytes as the host reads them
/// back, and the commands it submitted, each batch marked, as the host's own recording decodes
/// them; a buffer it doesn't hold is refused.
#[test]
fn a_recording_builds_buffers_and_commands_come_back() {
    let (tool, src, out) = setup("drive-record");
    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &["record"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(
        printed.contains("missing: step 0 (read_buffer): GPU error: there's no buffer 999999"),
        "{printed}"
    );
    let storage = out.join("storage");
    let options = wrela_host::Options {
        storage: Some(storage),
        workers: 2,
        record: true,
        defer_init: true,
        ..Default::default()
    };
    let mut host =
        wrela_host::Host::load_with(src.join("build"), &options).expect("a hardware GPU");
    host.init().expect("init");
    for i in 0..2 {
        host.lockstep_frame(i, 60.0, 640, 360, &[]).expect("a frame");
    }
    let held = host.buffers();
    assert_eq!(value(&printed, "buffers"), held.len().to_string(), "{printed}");
    let newest = host.read_buffer(held[held.len() - 1]).expect("read");
    assert_eq!(std::fs::read(out.join("buffer.bin")).expect("the bytes"), newest);
    use wrela_abi::stream::Command as C;
    let names: Vec<String> = pipeline_names(&src.join("build"));
    let mut want = Vec::new();
    for batch in host.take_batches() {
        want.push("command Batch   0,0,0 ".to_string());
        for c in wrela_abi::stream::decode(&batch).expect("a batch") {
            let (mut label, mut pipeline, mut groups, mut bound) =
                (String::new(), String::new(), [0; 3], String::new());
            let (mut target, mut carried) = (String::new(), String::new());
            let bind = |b: &[wrela_abi::stream::Binding]| -> String {
                b.iter().map(|b| format!(" {}:{}:{}", b.handle, b.offset, b.size)).collect()
            };
            match &c {
                C::Label { name } => label = name.to_string(),
                C::Dispatch { pipeline: p, groups: g, bindings, .. } => {
                    (pipeline, groups, bound) = (names[*p as usize].clone(), *g, bind(bindings));
                }
                C::Draw { pipeline: p, bindings, .. } => {
                    (pipeline, bound) = (names[*p as usize].clone(), bind(bindings));
                }
                C::StorageWrite { path, data, .. } => {
                    target = path.to_string();
                    carried = data.iter().map(|b| format!(" {b}")).collect();
                }
                _ => {}
            }
            let [x, y, z] = groups;
            let kind = c.opcode().name();
            want.push(format!(
                "command {kind} {label} {pipeline} {x},{y},{z}{bound} {target}{carried}"
            ));
        }
    }
    let got: Vec<&str> = printed.lines().filter(|l| l.starts_with("command ")).collect();
    assert_eq!(got, want, "{printed}");
    assert!(got.iter().any(|l| l.starts_with("command Label counts ")), "{printed}");
    assert!(got.iter().any(|l| l.ends_with(" probe 7 8 9")), "a write's path and bytes: {printed}");
    assert!(printed.contains("waiting 0"), "{printed}");
}

/// `FrameAt` runs one frame at the time the tool gives, as the host's own frame at that time:
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

/// More than 1024 steps fail before any runs; steps whose answer would be past 512 KiB fail,
/// saying which, and the capture reserved before them goes back to the budget, so a capture
/// that only fits once it's back comes back after.
#[test]
fn a_request_past_its_steps_or_its_answers_bytes_fails_and_gives_its_blobs_back() {
    let (tool, src, out) = setup("drive-limits");
    // One screen's bytes, and a little: two would be past it.
    let mut c = budgeted(drive(&tool, &src, &out, "60", &["limits"]), "1000K");
    let (code, printed) = run(&mut c);
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("many: 1025 steps are more than a request runs (1024)"), "{printed}");
    assert!(
        printed.contains("past: step 1 (frames): what the steps saw is more than an answer holds"),
        "{printed}"
    );
    assert!(printed.contains("captured 1"), "the first capture's blob went back: {printed}");
}

/// A deadline kills the build's process group: the call is stopped, and the build can't be
/// observed after it, saying so.
#[test]
fn a_deadline_kills_the_builds_process_and_it_cant_be_observed_after() {
    let (tool, src, out) = setup("drive-spin");
    let started = Instant::now();
    let child = drive(&tool, &src, &out, "60", &["spin"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("wrela tool");
    assert!(within(Duration::from_secs(60), || out.join("asked").exists()), "the call was made");
    let mut driven = Vec::new();
    assert!(within(Duration::from_secs(10), || {
        driven = driven_by(child.id());
        !driven.is_empty()
    }));
    assert!(within(Duration::from_secs(20), || out.join("stopped").exists()), "it was stopped");
    assert!(driven.iter().all(|&g| !group_alive(g)), "the build's process group is gone");
    std::fs::write(out.join("go"), "").expect("go");
    let output = child.wait_with_output().expect("wait");
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{printed}");
    assert!(
        printed
            .contains("spin: `steps` ran past its deadline of 1 s, and the host stopped the build"),
        "{printed}"
    );
    assert!(
        printed
            .contains("after: the driven build 1 can't be observed: `steps` ran past its deadline"),
        "{printed}"
    );
    assert!(started.elapsed() < Duration::from_secs(30), "{:?}", started.elapsed());
    // With no deadline of its own, the tool's end stops it.
    std::fs::remove_file(out.join("asked")).expect("again");
    let started = Instant::now();
    let (code, printed) = run(&mut drive(&tool, &src, &out, "2", &["spin-forever"]));
    assert_eq!(code, Some(124), "{printed}");
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
}

/// A tool host killed (`SIGKILL`, with no supervisor) while its build's process computes, waits
/// in a join, or writes a large answer that nobody reads, leaves no build's process: each ends
/// within about a second, its helpers with it.
#[test]
fn a_killed_tool_host_leaves_no_builds_process() {
    let (tool, src, out) = setup("drive-killed");
    let voice =
        package("drive-killed-voice", &[("wrela.toml", &manifest("voice")), ("main.wrela", VOICE)]);
    build(&voice, &src.join("voice"));
    for (args, hold) in
        [(vec!["hold", "spin"], "0"), (vec!["hold", "join"], "0"), (vec!["voice"], "60")]
    {
        let _ = std::fs::remove_file(out.join("asked"));
        let mut c = if args[0] == "voice" {
            let mut c = wrela();
            c.arg("tool").arg(voice_tool()).arg("--read");
            c.arg(format!("src={}", src.display()));
            c.arg("--write").arg(format!("out={}", out.display()));
            c.args(["--timeout", "120", "--", "big"]);
            c
        } else {
            drive(&tool, &src, &out, "120", &args)
        };
        c.env("WRELA_TESTING_DRIVE_HOLD", hold);
        let mut host = c.stdout(std::process::Stdio::null()).spawn().expect("wrela tool");
        assert!(within(Duration::from_secs(60), || out.join("asked").exists()), "{args:?}");
        // The build runs the call, or has rendered and writes what the host doesn't read.
        std::thread::sleep(Duration::from_millis(if hold == "0" { 300 } else { 3000 }));
        let driven = driven_by(host.id());
        assert_eq!(driven.len(), 1, "{args:?}: {driven:?}");
        host.kill().expect("SIGKILL");
        let _ = host.wait();
        let since = Instant::now();
        let gone = within(Duration::from_millis(1500), || !group_alive(driven[0]));
        assert!(gone, "{args:?}: the build's process outlived its host ({:?})", since.elapsed());
    }
}

/// Two captures at once whose screens together are past the blob budget, or past its handles:
/// the build's process asks for each one's bytes and blob before it reads the screen back, so
/// one is refused, before it's read, and the other comes back.
#[test]
fn two_captures_past_the_blob_budget_admit_one() {
    let (tool, src, out) = setup("drive-budget");
    for (budget, why) in [("1500K", "budget"), ("2G,1", "blobs")] {
        let mut c = budgeted(drive(&tool, &src, &out, "60", &["two"]), budget);
        let (code, printed) = run(&mut c);
        assert_eq!(code, Some(0), "{printed}");
        assert_eq!(printed.matches("captured 1").count(), 1, "{budget}: {printed}");
        assert!(
            printed.contains("refused: step 0 (capture): the screen's 921600 bytes")
                && printed.contains(why),
            "{printed}"
        );
    }
}

/// N loads of one build compile it once: the first load's code is compiled (and kept in the code
/// cache), the others' reused; a later run reads it back from the cache; with the cache off,
/// nothing is kept.
#[test]
fn loads_of_one_build_compile_it_once_and_the_cache_keeps_it() {
    let (tool, src, out) = setup("drive-loads");
    let cache = out.parent().expect("a directory").join("code-cache");
    let _ = std::fs::remove_dir_all(&cache);
    let codes = |cache: &str| {
        let mut c = drive(&tool, &src, &out, "60", &["loads"]);
        c.env("WRELA_CODE_CACHE", cache);
        let (code, printed) = run(&mut c);
        assert_eq!(code, Some(0), "{printed}");
        let codes: Vec<String> = printed
            .lines()
            .filter_map(|l| l.strip_prefix("load "))
            .map(|l| l.split(' ').next().unwrap_or_default().to_string())
            .collect();
        codes
    };
    let at = cache.display().to_string();
    assert_eq!(codes(&at), ["compiled", "reused", "reused"]);
    let kept = std::fs::read_dir(&cache).expect("the cache").count();
    assert_eq!(kept, 2, "the tool's code and the build's");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cache).expect("the cache").permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "the user's alone");
    }
    assert_eq!(codes(&at), ["cached", "reused", "reused"]);
    assert_eq!(codes("off"), ["compiled", "reused", "reused"]);
}

/// A grant whose directory is at or above the code cache stops the command (status 2), whatever
/// names it (a link among them), so no tool can write code that later runs read back; a grant
/// beside it, or a cache that's off, runs.
#[test]
fn a_grant_that_holds_the_code_cache_is_refused() {
    let dir = package(
        "drive-cache-grant",
        &[
            ("tool/wrela.toml", &manifest("plain")),
            ("tool/main.wrela", "pub fn tool() -> u32 {\n    0\n}\n"),
        ],
    );
    let cache = dir.join("home/cache/code");
    std::fs::create_dir_all(&cache).expect("the cache");
    std::fs::create_dir_all(dir.join("beside")).expect("beside");
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.join("home"), dir.join("link")).expect("a link");
    let tool = |flag: &str, root: &Path, cache: &str| {
        let mut c = wrela();
        c.arg("tool").arg(dir.join("tool")).arg(flag).arg(format!("r={}", root.display()));
        c.env("WRELA_CODE_CACHE", cache);
        run(&mut c)
    };
    let at = cache.display().to_string();
    for (flag, root) in [
        ("--read", dir.join("home")),
        ("--write", cache.clone()),
        ("--read", dir.clone()),
        ("--read", dir.join("link")),
    ] {
        let (code, printed) = tool(flag, &root, &at);
        assert_eq!(code, Some(2), "{flag} {}: {printed}", root.display());
        assert!(printed.contains("holds the code cache"), "{printed}");
    }
    assert_eq!(tool("--write", &dir.join("beside"), &at).0, Some(0));
    assert_eq!(tool("--read", &dir.join("home"), "off").0, Some(0), "the cache is off");
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
    build(&dir.join("subject"), &src.join("build"));
    std::fs::write(src.join("build/data.txt"), "inside").expect("data");
    let outside = dir.join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(outside.join("data.txt"), "the user's secret").expect("secret");
    let child = drive(&tool, &src, &out, "60", &["fetch"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("wrela tool");
    assert!(within(Duration::from_secs(60), || out.join("loaded").exists()), "never loaded");
    std::fs::rename(src.join("build"), src.join("moved")).expect("move the build");
    std::os::unix::fs::symlink(&outside, src.join("build")).expect("a link out");
    std::fs::write(out.join("go"), "").expect("go");
    let output = child.wait_with_output().expect("wait");
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{printed}");
    assert!(printed.contains("log fetched 6 bytes"), "the build as opened: {printed}");
    assert!(!printed.contains("17 bytes"), "nothing from where the link leads: {printed}");
}

/// A program that fetches `data.txt` in `init`, and says on the frame it arrives how many bytes
/// it holds.
const LATE: &str = r#"
use std::io::{Pending, fetch, print}

pub struct Game {
    data: Pending<u8>,
    frames: u32,
}

pub fn init() -> Game {
    Game { data: fetch("data.txt"), frames: 0 }
}

pub fn frame(game: mut Game, time: f32, width: u32, height: u32) {
    if let Some(Ok(bytes)) = game.data.poll() {
        print(f"fetched {bytes.len()} bytes on frame {game.frames}")
    }
    game.frames += 1
}
"#;

/// A tool that holds the late program's `data.txt`, runs three frames, then lets it go and runs
/// one more: the fetch arrives on that frame alone.
const HOLD_TOOL: &str = r#"
use std::input::Script
use std::io::print
use std::tool::drive
use std::tool::drive::Step

pub fn tool() -> u32 {
    let Ok(it) = drive::load("src/build", drive::cpu()).wait() else { return 91 }
    let b = it.build
    let held = [Step::Hold(String::from("data.txt")), Step::Init, Step::Frames(3, Script::frames())]
    let Ok(seen) = drive::steps(b, held).wait() else { return 92 }
    print(f"held {seen.lines.len()} lines")
    let let_go = [Step::Release(String::from("data.txt")), Step::Frames(1, Script::frames())]
    let Ok(seen) = drive::steps(b, let_go).wait() else { return 93 }
    for line in seen.lines {
        print(f"log {line.as_str()}")
    }
    let _ = drive::close(b).wait()
    0
}
"#;

/// A held path's fetch is carried out but not answered while it's held, and arrives on the first
/// frame after it's let go (`Step::Hold`, `Step::Release`).
#[test]
fn a_held_fetch_arrives_once_its_path_is_let_go() {
    let dir = package(
        "drive-hold",
        &[
            ("tool/wrela.toml", &manifest("hold_tool")),
            ("tool/main.wrela", HOLD_TOOL),
            ("subject/wrela.toml", &manifest("late")),
            ("subject/main.wrela", LATE),
        ],
    );
    let (tool, src, out) = (dir.join("tool"), dir.join("src"), dir.join("out"));
    std::fs::create_dir_all(&out).expect("out");
    build(&dir.join("subject"), &src.join("build"));
    std::fs::write(src.join("build/data.txt"), "inside").expect("data");
    let output = drive(&tool, &src, &out, "60", &[]).output().expect("wrela tool");
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{printed}");
    assert!(printed.contains("held 0 lines"), "nothing arrived while held: {printed}");
    assert!(printed.contains("log fetched 6 bytes on frame 3"), "{printed}");
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
use std::input::Script
use std::io::print
use std::tool::blob::Data
use std::tool::drive
use std::tool::drive::Step

pub fn tool() -> u32 {
    let Ok(it) = drive::load("src/build", drive::cpu()).wait() else { return 91 }
    let b = it.build
    let _ = drive::steps(b, [Step::Init, Step::Frames(1, Script::frames())]).wait()
    let Ok(posts) = drive::posts(b).wait() else { return 93 }
    for post in posts {
        let Data::Bytes(body) = post.body.clone() else { return 94 }
        print(f"posted {post.url.as_str()} {body.len()}")
    }
    let answered = drive::answer(b, posts[0].id, Data::Bytes(Vec::from([u8(7), 8, 9]))).wait()
    print(f"answered {answered.is_ok()}")
    let _ = drive::fail(b, posts[1].id, "no").wait()
    let twice = drive::answer(b, posts[0].id, Data::empty()).wait()
    print(f"answered twice {twice.is_ok()}")
    let Ok(seen) = drive::steps(b, [Step::Frames(1, Script::frames())]).wait() else { return 98 }
    for line in seen.lines {
        print(f"log {line.as_str()}")
    }
    // The post to `notes/c` waits: closing the build gives it up.
    let _ = drive::close(b).wait()
    0
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
    build(&dir.join("subject"), &src.join("build"));
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

/// A tool that renders the piece's voice (`src/voice`, else `src/build`): 300 quanta at once,
/// written to `out/voice.f32`, then 10 more each timed, written to `out/timed.f32`; with
/// `early`, the voice asked for before `init` and before the voice starts; with `big`, 20000
/// quanta (`out/asked` says the request is made), which it doesn't wait for.
const VOICE_TOOL: &str = r#"
use std::input::Script
use std::io::print
use std::tool::{ToolError, args}
use std::tool::blob
use std::tool::blob::Data
use std::tool::drive
use std::tool::drive::Step
use std::tool::files

fn write(blob: take Data, path: str) -> u32 {
    let Data::Blob(b) = blob else { return 80 }
    if files::write_blob(path, b).wait().is_err() {
        return 82
    }
    0
}

fn rendered(answered: take Result<drive::Seen, ToolError>) -> Option<drive::Audio> {
    match take answered {
        Ok(s) => Some(s.audio[0].clone()),
        Err(e) => {
            print(f"refused: {e.message()}")
            None
        },
    }
}

pub fn tool() -> u32 {
    let given = args()
    var options = drive::cpu()
    options.width = 16
    options.height = 16
    if !given.is_empty() && given[0].as_str() == "big" {
        let Ok(loaded) = drive::load("src/voice", options).wait() else { return 91 }
        let b = loaded.build
        let _ = drive::steps(b, [Step::Init, Step::Frames(1, Script::frames())]).wait()
        let big = drive::steps(b, [Step::Audio(20000, false)], timeout: 600.0)
        let _ = files::write("out/asked", Vec::from([u8(1)])).wait()
        let _ = take big.wait()
        return 0
    }
    let early = !given.is_empty()
    let Ok(loaded) = drive::load("src/build", options).wait() else { return 91 }
    let b = loaded.build
    if early {
        let _ = rendered(drive::steps(b, [Step::Audio(1, false)]).wait())
    }
    let _ = drive::steps(b, [Step::Init]).wait()
    if early {
        let _ = rendered(drive::steps(b, [Step::Audio(1, false)]).wait())
    }
    let _ = drive::steps(b, [Step::Frames(1, Script::frames())]).wait()
    let Some(voice) = rendered(drive::steps(b, [Step::Audio(300, false)]).wait()) else { return 0 }
    if voice.times.is_some() {
        return 93
    }
    let wrote = write(take voice.samples, "out/voice.f32")
    if wrote != 0 {
        return wrote
    }
    let Some(timed) = rendered(drive::steps(b, [Step::Audio(10, true)]).wait()) else { return 0 }
    let Some(Data::Blob(times)) = timed.times.clone() else { return 95 }
    let Ok(len) = blob::len(times).wait() else { return 97 }
    print(f"times {len}")
    let wrote = write(take timed.samples, "out/timed.f32")
    if wrote != 0 {
        return wrote
    }
    print("rendered")
    0
}
"#;

/// The voice tool's package, made once for the tests that use it.
fn voice_tool() -> PathBuf {
    package(
        "drive-voice-tool",
        &[("wrela.toml", &manifest("voice_tool")), ("main.wrela", VOICE_TOOL)],
    )
}

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
    build(&dir.join("subject"), &src.join("build"));
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
    assert!(printed.contains("refused: step 0 (audio): `audio` runs after `init`"), "{printed}");
    assert!(printed.contains("refused: step 0 (audio): it hasn't started a voice"), "{printed}");

    let mut c = budgeted(drive(&tool, &src, &out, "60", &[]), "100K");
    let (code, printed) = run(&mut c);
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("refused: step 0 (audio): the voice's 307200 bytes"), "{printed}");
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
use std::input::Script
use std::io::print
use std::tool::drive
use std::tool::drive::Step

pub fn tool() -> u32 {
    let Ok(loaded) = drive::load("src/build", drive::cpu()).wait() else { return 91 }
    let b = loaded.build
    let _ = drive::steps(b, [Step::Init, Step::Frames(1, Script::frames())]).wait()
    match drive::steps(b, [Step::Audio(1, false)], timeout: 1.0).wait() {
        Ok(_) => print("rendered"),
        Err(e) => print(f"refused: {e.message()}"),
    }
    match drive::steps(b, [Step::Hash]).wait() {
        Ok(_) => print("hashed"),
        Err(e) => print(f"after: {e.message()}"),
    }
    0
}
"#;

/// A voice that never ends its quantum is stopped at the request's deadline, with its build.
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
    build(&dir.join("subject"), &src.join("build"));
    let started = Instant::now();
    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &[]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("refused: `steps` ran past its deadline of 1 s"), "{printed}");
    assert!(printed.contains("after: the driven build 1 can't be observed"), "{printed}");
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
}

/// The pipelines' names of the build in `dir`, by index (its manifest's).
fn pipeline_names(dir: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest.json");
    wrela_abi::manifest::Manifest::parse(&text)
        .expect("a manifest")
        .pipelines
        .into_iter()
        .map(|p| p.name)
        .collect()
}

/// A tool that loads `src/a` on the GPU with its storage in `out/store`, says so
/// (`out/loaded`), waits for `out/go`, runs three frames, and prints which build runs.
const HANDING_TOOL: &str = r#"
use std::input::Script
use std::io::print
use std::tool::drive
use std::tool::drive::Step
use std::tool::files
use std::tool::time

pub fn tool() -> u32 {
    var options = drive::gpu()
    options.storage = String::from("out/store")
    let Ok(loaded) = drive::load("src/a", options).wait() else { return 91 }
    let b = loaded.build
    let _ = files::write("out/loaded", Vec::from([u8(1)])).wait()
    while files::stat("out/go").wait().is_err() {
        let _ = time::after(0.02).wait()
    }
    let Ok(seen) = drive::steps(b, [Step::Init, Step::Frames(3, Script::frames()), Step::Running]).wait() else { return 92 }
    if let Some(trap) = seen.trap {
        print(f"trap {trap.message.as_str()}")
    }
    print(f"running {seen.running[0].as_str()}")
    let _ = drive::close(b).wait()
    0
}
"#;

/// A program that hands its place to the build beside it, `../b`, at its first frame.
const HANDS_OVER: &str = r#"
use std::reload::hand_over

pub fn frame(time: f32, width: u32, height: u32) {
    let _ = hand_over("../b")
}
"#;

/// The build handed over to: it stores `bytes` as `probe` each frame.
fn handed_to(bytes: &str) -> String {
    format!(
        "use std::io::store\n\npub fn frame(time: f32, width: u32, height: u32) {{\n    let stored: [u8; 3] = [{bytes}]\n    let _ = store(\"probe\", stored)\n}}\n"
    )
}

/// A driven build hands over to the build beside it in its root, which its process opens
/// through the root's directory as the tool host opened it (a link put in place of the root
/// after the load leads nowhere), and runs (`Running` names it); a load's storage in a write root
/// stays when the build closes, holding what the build stored. A path out of the root is refused
/// (handover.rs).
#[cfg(unix)]
#[test]
fn a_driven_build_hands_over_within_its_root_and_keeps_its_storage_where_asked() {
    let (b, other) = (handed_to("7, 8, 9"), handed_to("1, 2, 3"));
    let dir = package(
        "drive-handover",
        &[
            ("tool/wrela.toml", &manifest("handing")),
            ("tool/main.wrela", HANDING_TOOL),
            ("a/wrela.toml", &manifest("a")),
            ("a/main.wrela", HANDS_OVER),
            ("b/wrela.toml", &manifest("b")),
            ("b/main.wrela", &b),
            ("other/wrela.toml", &manifest("other")),
            ("other/main.wrela", &other),
        ],
    );
    let (src, out, elsewhere) = (dir.join("src"), dir.join("out"), dir.join("elsewhere"));
    std::fs::create_dir_all(out.join("store")).expect("out");
    for name in ["a", "b"] {
        build(&dir.join(name), &src.join(name));
    }
    build(&dir.join("other"), &elsewhere.join("b"));
    let mut c = wrela();
    c.arg("tool").arg(dir.join("tool"));
    c.arg("--read").arg(format!("src={}", src.display()));
    c.arg("--write").arg(format!("out={}", out.display()));
    c.arg("--timeout").arg("60");
    let child = c.stdout(std::process::Stdio::piped()).spawn().expect("wrela tool");
    assert!(within(Duration::from_secs(60), || out.join("loaded").exists()), "never loaded");
    std::fs::rename(&src, dir.join("moved")).expect("move the root");
    std::os::unix::fs::symlink(&elsewhere, &src).expect("a link elsewhere");
    std::fs::write(out.join("go"), "").expect("go");
    let output = child.wait_with_output().expect("wait");
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{printed}");
    assert!(printed.contains("running src/b"), "{printed}");
    assert!(!printed.contains("trap"), "{printed}");
    let stored = std::fs::read(out.join("store/probe")).expect("the stored file stays");
    assert_eq!(stored, [7, 8, 9], "the build beside it in the root as it was opened");
}

/// A failed request's lines go with its failure, and the lines it printed, and the quota they
/// filled, aren't the next request's: a request that prints one line sees that line alone.
#[test]
fn a_failed_requests_lines_are_its_own() {
    let (tool, src, out) = setup("drive-lines");
    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &["lines"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("failed with its lines=true"), "{printed}");
    assert!(printed.contains("lines 1 dropped 0 first chatter 0"), "{printed}");
}

/// A cached artifact with another name (a hard link in a write root) isn't written through
/// that name, and isn't read back: the next load compiles it again. A link in the cache in
/// an artifact's place isn't followed: a miss too.
#[cfg(unix)]
#[test]
fn a_cache_entry_with_another_name_or_a_link_is_not_read_back() {
    let (tool, src, out) = setup("drive-cache-alias");
    let cache = out.parent().expect("a directory").join("code-cache");
    let _ = std::fs::remove_dir_all(&cache);
    let loads = |args: &[&str]| {
        let mut c = drive(&tool, &src, &out, "60", args);
        c.env("WRELA_CODE_CACHE", &cache);
        run(&mut c)
    };
    let first = |printed: &str| {
        let line =
            printed.lines().find(|l| l.starts_with("load ")).unwrap_or_else(|| panic!("{printed}"));
        line.split(' ').nth(1).unwrap_or_default().to_string()
    };
    let (_, printed) = loads(&["loads"]);
    assert_eq!(first(&printed), "compiled", "{printed}");
    let artifacts = |cache: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(cache).expect("the cache").flatten().map(|e| e.path()).collect()
    };
    for (i, a) in artifacts(&cache).iter().enumerate() {
        std::fs::hard_link(a, out.join(format!("alias-{i}"))).expect("a hard link");
    }
    let kept = std::fs::read(out.join("alias-0")).expect("the alias");
    // With the cache off, so this run reads none of it (a read would remove the linked entry).
    let mut c = drive(&tool, &src, &out, "60", &["write", "out/alias-0"]);
    let (code, printed) = run(c.env("WRELA_CODE_CACHE", "off"));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("write: `out/alias-0` has other names"), "{printed}");
    assert_eq!(std::fs::read(out.join("alias-0")).expect("the alias"), kept, "unchanged");
    let (_, printed) = loads(&["loads"]);
    assert_eq!(first(&printed), "compiled", "a linked artifact is a miss: {printed}");
    let (_, printed) = loads(&["loads"]);
    assert_eq!(first(&printed), "cached", "its own again: {printed}");
    for (i, a) in artifacts(&cache).iter().enumerate() {
        let copy = out.join(format!("copy-{i}"));
        std::fs::copy(a, &copy).expect("a copy");
        std::fs::remove_file(a).expect("removed");
        std::os::unix::fs::symlink(&copy, a).expect("a link");
    }
    let (_, printed) = loads(&["loads"]);
    assert_eq!(first(&printed), "compiled", "a link is a miss: {printed}");
}

/// A build's code whose compile holds past the grace doesn't hold up the tool's end: the run
/// ends within the grace, normally or at its timeout, as the host's failure (71); and a load's
/// deadline answers on time while its compile is held.
#[test]
fn a_held_compile_bounds_the_end_and_a_loads_deadline_answers_on_time() {
    let (tool, src, out) = setup("drive-compile-hold");
    for (args, timeout) in [("hold-load", "60"), ("wait-load", "2")] {
        let started = Instant::now();
        let mut c = drive(&tool, &src, &out, timeout, &[args]);
        c.env("WRELA_CODE_CACHE", "off").env("WRELA_TESTING_COMPILE_HOLD", "60");
        let (code, printed) = run(&mut c);
        assert_eq!(code, Some(71), "{args}: {printed}");
        assert!(started.elapsed() < Duration::from_secs(30), "{args}: {:?}", started.elapsed());
    }
    let mut c = drive(&tool, &src, &out, "60", &["load-deadline"]);
    c.env("WRELA_CODE_CACHE", "off").env("WRELA_TESTING_COMPILE_HOLD", "3");
    let (code, printed) = run(&mut c);
    assert_eq!(code, Some(0), "{printed}");
    let line =
        printed.lines().find(|l| l.starts_with("load: ")).unwrap_or_else(|| panic!("{printed}"));
    assert!(line.contains("past its deadline"), "{printed}");
    let took: f64 = line.rsplit(' ').next().and_then(|t| t.parse().ok()).expect("a time");
    assert!(took < 2.5, "the deadline answered on time: {took}");
}

/// Lines past what the answer holds come back whole, in a blob, which `drive::lines` reads; a
/// blob past the budget fails the request before its bytes cross, and gives its reservation
/// back; lines that fit stay in the answer.
#[test]
fn every_printed_line_comes_back_through_a_blob_when_the_answer_cant_hold_them() {
    let (tool, src, out) = setup("drive-many-lines");
    let (code, printed) = run(&mut drive(&tool, &src, &out, "60", &["many-lines"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(
        printed
            .contains("all 30000 first chatter 0 last chatter 29999 inline 0 log true dropped 0"),
        "{printed}"
    );
    assert!(printed.contains("dropped 0 kept false"), "the log is released: {printed}");
    assert!(printed.contains("then 1 inline 1 log false"), "{printed}");
    // A budget that holds one log, not two: each is released once read, so three in turn fit.
    let mut c = budgeted(drive(&tool, &src, &out, "60", &["many-lines-again"]), "600K");
    let (code, printed) = run(&mut c);
    assert_eq!(code, Some(0), "{printed}");
    for k in 0..3 {
        assert!(printed.contains(&format!("again {k}: 30000")), "{printed}");
    }
    let mut c = budgeted(drive(&tool, &src, &out, "60", &["many-lines"]), "100K");
    let (code, printed) = run(&mut c);
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("refused: the 30000 lines it printed, "), "{printed}");
    assert!(printed.contains("budget"), "{printed}");
    assert!(printed.contains("then 1 inline 1 log false"), "{printed}");
}
