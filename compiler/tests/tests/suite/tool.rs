//! The tool host (language.md §15.5, `wrela_host::tool`): a tool build whose `tool` runs once,
//! waiting on each request where it needs the answer, with only what its grants reach. The
//! tests run its core
//! services (arguments, secrets, files, blobs, time), its refusals (paths outside the roots,
//! `..`, symbolic links out of a root, one swapped in after a check, writes to a read root,
//! secrets not granted, a blob budget spent), and each way it ends: its status, waiting on
//! nothing, a trap, the timeout and an interrupt, each with its workers stopped and its blobs
//! released; files that aren't regular files refused without waiting; and a worker stuck past
//! its grace, or a clock thread that can't start, ending the run as the host's failure.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wrela_host::tool::{BlobBudget, Caps, End, Ended, Grants, Options, Signal, Testing, ToolBuild};

/// A scratch directory of its own.
pub(crate) fn scratch(name: &str) -> PathBuf {
    let dir = wrela_tests::scratch_root(env!("CARGO_TARGET_TMPDIR")).join(format!("tool-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// The tool build of a package whose `main.wrela` is `main`.
pub(crate) fn tool_build(name: &str, main: &str) -> ToolBuild {
    let dir = scratch(&format!("{name}-package"));
    let ident = name.replace('-', "_");
    std::fs::write(dir.join("wrela.toml"), format!("[package]\nname = \"{ident}\"\n"))
        .expect("write the manifest");
    std::fs::write(dir.join("main.wrela"), main).expect("write main");
    let built = wrela_driver::build(&dir);
    assert!(
        !built.has_errors(),
        "{}",
        wrela_diag::render::render_all(&built.sources, &built.diagnostics)
    );
    let paths: Vec<&str> = built.files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(paths, [wrela_driver::build::TOOL_WASM], "a tool-only program has a tool build");
    ToolBuild::from_wasm(built.files[0].1.clone())
}

/// What the tool printed, and a printer that keeps it, calling `also` with each line first.
pub(crate) fn printer(
    also: impl Fn(&str) + Send + Sync + 'static,
) -> (Arc<Mutex<Vec<String>>>, Options) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let kept = lines.clone();
    let mut options = Options::new(Caps::open(&Grants::default(), |_| None).expect("no grants"));
    options.print = Arc::new(move |line| {
        also(line);
        kept.lock().expect("the lines").push(line.to_string());
    });
    (lines, options)
}

/// A test's signal ([`Signal`]), and the moments it's told of, with when, as they come.
pub(crate) fn signals() -> (Signal, std::sync::mpsc::Receiver<(&'static str, Instant)>) {
    let (sent, told) = std::sync::mpsc::channel();
    let sent = Mutex::new(sent);
    let signal = Signal(Arc::new(move |moment| {
        let _ = sent.lock().expect("the signal").send((moment, Instant::now()));
    }));
    (signal, told)
}

/// Waits for the host to reach `moment`: when it did.
pub(crate) fn reached(
    told: &std::sync::mpsc::Receiver<(&'static str, Instant)>,
    moment: &str,
) -> Instant {
    loop {
        let (m, at) =
            told.recv_timeout(Duration::from_secs(60)).unwrap_or_else(|_| panic!("no {moment}"));
        if m == moment {
            return at;
        }
    }
}

/// Runs `build` with `options`, checking that the run left no worker behind.
pub(crate) fn run(build: &ToolBuild, options: Options) -> Ended {
    let ended = wrela_host::tool::run(build, options);
    assert_eq!(ended.workers_left, 0, "every worker stopped: {ended:?}");
    ended
}

const SERVICES: &str = r#"
use std::io::print
use std::tool::{args, secret}
use std::tool::blob
use std::tool::files
use std::tool::time

pub fn tool() -> u32 {
    var line = String::from("args:")
    for a in args() {
        line.push_str(" ")
        line.push_str(a.as_str())
    }
    print(line.as_str())
    match secret("TOKEN") {
        Some(s) => print(f"TOKEN is {s.len()} bytes"),
        None => print("no TOKEN"),
    }
    if secret("PATH").is_none() {
        print("no PATH: it isn't granted")
    }

    let Ok(bytes) = files::read("in/a.txt").wait() else { return 91 }
    let Some(text) = String::from_utf8(take bytes) else { return 92 }
    print(f"in/a.txt: {text.as_str()}")

    if files::write("out/sub/b.txt", [u8(104), 105]).wait().is_err() { return 94 }

    let Ok(entries) = files::list("in").wait() else { return 96 }
    for e in entries {
        let kind = match e.kind {
            files::Kind::File => "file",
            files::Kind::Dir => "dir",
            files::Kind::Link => "link",
            files::Kind::Other => "other",
        }
        print(f"in has {e.name.as_str()}, a {kind} of {e.size} bytes")
    }
    let Ok(entry) = files::stat("out/sub").wait() else { return 98 }
    if entry.kind == files::Kind::Dir {
        print("out/sub is a directory")
    }

    // 1.5 MiB in three chunks, out to a file, and back in as another blob.
    let Ok(b) = blob::new().wait() else { return 100 }
    for k in 0..3 {
        let chunk = Vec::filled(524288, u8(k + 1))
        if blob::append(b, chunk).wait().is_err() { return 102 }
    }
    if files::write_blob("out/big.bin", b).wait().is_err() { return 104 }
    let _ = blob::release(b).wait()
    let Ok(back) = files::read_blob("out/big.bin").wait() else { return 106 }
    let Ok(size) = blob::len(back).wait() else { return 108 }
    let Ok(piece) = blob::read(back, u64(1048576) + 10, 4).wait() else { return 110 }
    print(f"a blob of {size} bytes, at 1 MiB + 10: {piece[0]} {piece[3]}, {piece.len()} bytes")

    let waited = std::time::now()
    let _ = time::after(0.05).wait()
    if std::time::since(waited) >= 0.05 {
        print("the timer fired after 50 ms")
    }
    let Ok(now) = time::utc_now().wait() else { return 112 }
    if now.seconds > u64(1700000000) {
        print("the date is after 2023")
    }
    // `back` isn't released: the host releases it as the tool ends.
    7
}
"#;

#[test]
fn a_tool_uses_its_arguments_files_blobs_and_time() {
    let build = tool_build("services", SERVICES);
    let root = scratch("services");
    let (input, output) = (root.join("in"), root.join("out"));
    std::fs::create_dir_all(input.join("deep")).expect("in");
    std::fs::create_dir_all(&output).expect("out");
    std::fs::write(input.join("a.txt"), "hello").expect("write");
    let grants =
        Grants::default().root("in", &input, false).root("out", &output, true).secret("TOKEN");
    let (lines, mut options) = printer(|_| {});
    options.caps =
        Caps::open(&grants, |name| (name == "TOKEN").then(|| "abc".into())).expect("granted");
    options.args = vec!["one".into(), "two".into()];
    let ended = run(&build, options);
    let lines = lines.lock().expect("the lines").clone();
    assert_eq!(ended.end, End::Exit(7), "{lines:?}");
    assert_eq!(
        lines,
        [
            "args: one two",
            "TOKEN is 3 bytes",
            "no PATH: it isn't granted",
            "in/a.txt: hello",
            "in has a.txt, a file of 5 bytes",
            "in has deep, a dir of 0 bytes",
            "out/sub is a directory",
            "a blob of 1572864 bytes, at 1 MiB + 10: 3 3, 4 bytes",
            "the timer fired after 50 ms",
            "the date is after 2023",
        ]
    );
    assert_eq!(std::fs::read(output.join("sub/b.txt")).expect("written"), b"hi");
    let big = std::fs::read(output.join("big.bin")).expect("the blob's file");
    assert_eq!(big.len(), 3 << 19);
    assert!(big[..1 << 19].iter().all(|&b| b == 1) && big[1 << 20..].iter().all(|&b| b == 3));
    // The blob the tool didn't release, released by the host as it ended.
    assert_eq!(ended.blobs_released, (1, 3 << 19));
}

const REFUSALS: &str = r#"
use std::io::print
use std::tool::{ToolError, secret}
use std::tool::files

/// Prints what an answer says: `ok`, or why the request failed.
fn say<T>(what: str, answered: take Result<T, ToolError>) {
    match take answered {
        Ok(_) => print(f"{what}: ok"),
        Err(e) => print(f"{what}: {e.message()}"),
    }
}

pub fn tool() -> u32 {
    // Asked all at once, each answered when it's done; printed in the order asked.
    let r0 = files::read("in/../in/a.txt")
    let r1 = files::read("/etc/hosts")
    let r2 = files::read("elsewhere/a.txt")
    let r3 = files::read("in/link/secret.txt")
    let r4 = files::read("in/escape.txt")
    let w5 = files::write("in/new.txt", [u8(1)])
    let d6 = files::remove_all("out")
    let s7 = files::stat("in/swap")
    say("read in/../in/a.txt", take r0.wait())
    say("read /etc/hosts", take r1.wait())
    say("read elsewhere/a.txt", take r2.wait())
    say("read in/link/secret.txt", take r3.wait())
    say("read in/escape.txt", take r4.wait())
    say("write in/new.txt", take w5.wait())
    say("remove_all out", take d6.wait())
    say("stat in/swap", take s7.wait())
    if secret("PATH").is_none() {
        print("secret PATH: not granted")
    }
    // A directory checked, then swapped for a link out of the root before it's read: the test's
    // printer swaps it, on the host's console thread, and the tool waits until it sees the swap.
    print("checked in/swap")
    while files::stat("in/swap").wait().is_ok() {
        let _ = std::tool::time::after(0.01).wait()
    }
    say("read in/swap/secret.txt", files::read("in/swap/secret.txt").wait())
    0
}
"#;

#[test]
fn requests_outside_the_grants_fail_and_the_tool_goes_on() {
    let build = tool_build("refusals", REFUSALS);
    let root = scratch("refusals");
    let (input, output, outside) = (root.join("in"), root.join("out"), root.join("outside"));
    for d in [&input.join("swap"), &output, &outside] {
        std::fs::create_dir_all(d).expect("mkdir");
    }
    std::fs::write(input.join("a.txt"), "a").expect("write");
    std::fs::write(input.join("swap/secret.txt"), "inside").expect("write");
    std::fs::write(outside.join("secret.txt"), "OUTSIDE").expect("write");
    std::os::unix::fs::symlink(&outside, input.join("link")).expect("a link out");
    std::os::unix::fs::symlink(outside.join("secret.txt"), input.join("escape.txt"))
        .expect("a link out");
    let grants = Grants::default().root("in", &input, false).root("out", &output, true);
    let swap = input.join("swap");
    let to = outside.clone();
    // The swap happens while the tool runs, between its check and its read.
    let (lines, mut options) = printer(move |line| {
        if line == "checked in/swap" {
            std::fs::remove_dir_all(&swap).expect("remove the checked directory");
            std::os::unix::fs::symlink(&to, &swap).expect("swap in a link out");
        }
    });
    options.caps = Caps::open(&grants, |_| Some("granted".into())).expect("granted");
    let ended = run(&build, options);
    let lines = lines.lock().expect("the lines").clone();
    assert_eq!(ended.end, End::Exit(0), "{lines:?}");
    let expect = [
        "read in/../in/a.txt: the path `in/../in/a.txt` has an empty, `.` or `..` part",
        "read /etc/hosts: the path `/etc/hosts` isn't in a granted root: there's no root ``",
        "read elsewhere/a.txt: the path `elsewhere/a.txt` isn't in a granted root: there's no root `elsewhere`",
        "read in/link/secret.txt: `in/link/secret.txt` leads outside its root",
        "read in/escape.txt: `in/escape.txt` leads outside its root",
        "write in/new.txt: the path `in/new.txt` is in `in`, which is granted for reading only",
        "remove_all out: `out` is a root, not a file in one",
        "stat in/swap: ok",
        "secret PATH: not granted",
        "checked in/swap",
        "read in/swap/secret.txt: `in/swap/secret.txt` leads outside its root",
    ];
    assert_eq!(lines, expect);
    assert!(!input.join("new.txt").exists(), "a read root isn't written");
    assert!(output.is_dir(), "a root isn't removed");
}

const BUDGET: &str = r#"
use std::io::print
use std::tool::blob

pub fn tool() -> u32 {
    var handles: Vec<blob::Blob> = Vec::new()
    for _ in 0..3 {
        match blob::new().wait() {
            Ok(b) => handles.push(b),
            Err(e) => print(f"new: {e.message()}"),
        }
    }
    let sizes = [u32(524288), 524288, 1]
    for i in 0..3 {
        match blob::append(handles[i % 2], Vec::filled(sizes[i], u8(9))).wait() {
            Ok(_) => print(f"append {sizes[i]}: ok"),
            Err(e) => print(f"append {sizes[i]}: {e.message()}"),
        }
    }
    let _ = blob::release(handles[1]).wait()
    if blob::append(handles[0], [u8(1)]).wait().is_ok() {
        print("append 1 after a release: ok")
    }
    0
}
"#;

#[test]
fn a_spent_blob_budget_fails_the_request_not_the_tool() {
    let build = tool_build("budget", BUDGET);
    let (lines, mut options) = printer(|_| {});
    options.blob_budget = BlobBudget { bytes: 1 << 20, handles: 2 };
    let ended = run(&build, options);
    let lines = lines.lock().expect("the lines").clone();
    assert_eq!(ended.end, End::Exit(0), "{lines:?}");
    assert_eq!(
        lines,
        [
            "new: the tool holds 2 blobs, as many as its budget allows: release one",
            "append 524288: ok",
            "append 524288: ok",
            "append 1: 1 more bytes would take the tool's blobs past their budget of 1048576 bytes",
            "append 1 after a release: ok",
        ]
    );
    assert_eq!(ended.blobs_released, (1, (1 << 19) + 1));
}

#[test]
fn a_tool_that_waits_on_nothing_is_stopped() {
    let main = r#"
use std::tool::{Request, any}

pub fn tool() -> u32 {
    // Waits for the first of no requests: nothing would answer it.
    let none: [Request; 0] = []
    any(none)
}
"#;
    let build = tool_build("nothing", main);
    let (_, options) = printer(|_| {});
    let ended = run(&build, options);
    assert_eq!(ended.end, End::WaitsOnNothing);
    assert_eq!(ended.end.status(), 70);
}

#[test]
fn a_trap_ends_the_tool_with_its_message_and_releases_its_blobs() {
    let main = r#"
use std::tool::blob
use std::tool::time::after

pub fn tool() -> u32 {
    let _ = blob::new().wait()
    // Made and answered, but its answer never read: the host releases it too.
    let unread = blob::new()
    let _ = after(0.05).wait()
    panic("the tool broke after its blobs")
}
"#;
    let build = tool_build("trap", main);
    let (_, options) = printer(|_| {});
    let ended = run(&build, options);
    assert_eq!(ended.end, End::Trap("the tool broke after its blobs".into()));
    assert_ne!(ended.end.status(), 0);
    assert_eq!(ended.blobs_released, (2, 0), "the blob it read, and the one it didn't");
}

const FOREVER: &str = r#"
use std::tool::time::after

pub fn tool() -> u32 {
    if std::tool::args().len() > 0 {
        var n: u32 = 0
        loop {
            n = n.wrapping_add(1)
        }
    }
    let _ = after(3600.0).wait()
    0
}
"#;

#[test]
fn the_timeout_ends_a_loop_and_a_wait_in_bounded_time() {
    let build = tool_build("forever", FOREVER);
    for spin in [true, false] {
        let (_, mut options) = printer(|_| {});
        options.timeout = Some(Duration::from_millis(300));
        if spin {
            options.args = vec!["spin".into()];
        }
        let started = Instant::now();
        let ended = run(&build, options);
        assert_eq!(ended.end, End::Timeout, "spin: {spin}");
        assert_eq!(ended.end.status(), 124);
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    }
}

#[test]
fn an_interrupt_stops_a_tool_inside_a_call() {
    let build = tool_build("interrupt", FOREVER);
    let (_, mut options) = printer(|_| {});
    let interrupt = Arc::new(AtomicBool::new(false));
    options.interrupt = Some(interrupt.clone());
    options.args = vec!["spin".into()];
    let set = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        interrupt.store(true, Ordering::Release);
    });
    let ended = run(&build, options);
    set.join().expect("the interrupting thread");
    assert_eq!(ended.end, End::Interrupted);
    assert_eq!(ended.end.status(), 130);
}

const FIFO: &str = r#"
use std::io::print
use std::tool::ToolError
use std::tool::blob
use std::tool::files

fn said<T>(name: str, answered: take Result<T, ToolError>) {
    match take answered {
        Ok(_) => print(f"{name}: ok"),
        Err(e) => print(f"{name}: {e.message()}"),
    }
}

pub fn tool() -> u32 {
    let Ok(b) = blob::new().wait() else { return 91 }
    let _ = blob::append(b, [u8(1), 2, 3]).wait()
    // A FIFO with nothing at its other end: each is refused at once, and the tool goes on. One
    // at a time, so no two opens meet at the FIFO's ends.
    said("write_blob", files::write_blob("out/fifo", b).wait())
    said("read", files::read("out/fifo").wait())
    said("write", files::write("out/fifo", [u8(1)]).wait())
    said("read_blob", files::read_blob("out/fifo").wait())
    0
}
"#;

#[test]
fn files_that_arent_regular_files_are_refused_without_waiting() {
    let build = tool_build("fifo", FIFO);
    let out = scratch("fifo");
    let made = std::process::Command::new("mkfifo").arg(out.join("fifo")).status();
    assert!(made.is_ok_and(|s| s.success()), "mkfifo");
    let (lines, mut options) = printer(|_| {});
    options.caps = Caps::open(&Grants::default().root("out", &out, true), |_| None).expect("out");
    // Bounded, so a host that waits on the FIFO fails here rather than hangs.
    options.timeout = Some(Duration::from_secs(10));
    let ended = run(&build, options);
    let lines = lines.lock().expect("the lines").clone();
    assert_eq!(ended.end, End::Exit(0), "{lines:?}");
    assert_eq!(lines.len(), 4, "{lines:?}");
    for (line, name) in lines.iter().zip(["write_blob", "read", "write", "read_blob"]) {
        assert!(line.starts_with(&format!("{name}: `out/fifo`")), "{line}");
    }
    assert!(lines[1].ends_with("isn't a regular file"), "{lines:?}");
    assert_eq!(ended.blobs_released, (1, 3), "the blob is released with the run");
}

const SLOW_WRITE: &str = r#"
use std::tool::blob
use std::tool::files

pub fn tool() -> u32 {
    let Ok(b) = blob::new().wait() else { return 91 }
    let _ = blob::append(b, Vec::filled(1024, u8(7))).wait()
    let _ = files::write_blob("out/slow.bin", b).wait()
    0
}
"#;

#[test]
fn a_worker_stuck_in_a_file_write_ends_the_run_in_bounded_time_as_the_hosts() {
    let build = tool_build("slow-write", SLOW_WRITE);
    let out = scratch("slow-write");
    // How late a thread may wake, past what the host itself waits.
    let late = Duration::from_secs(1);
    for interrupted in [false, true] {
        let (_, mut options) = printer(|_| {});
        options.caps =
            Caps::open(&Grants::default().root("out", &out, true), |_| None).expect("out");
        // A disk that takes 5 s to write the blob, a grace of 100 ms.
        let (signal, told) = signals();
        let grace = Duration::from_millis(100);
        options.testing = Testing {
            grace,
            io_delay: Duration::from_secs(5),
            signal: Some(signal),
            ..Testing::default()
        };
        // The end comes once the write has started: an interrupt then, or a timeout the write
        // starts within.
        let timeout = Duration::from_secs(3);
        let interrupt = Arc::new(AtomicBool::new(false));
        if interrupted {
            options.interrupt = Some(interrupt.clone());
        } else {
            options.timeout = Some(timeout);
        }
        let started = Instant::now();
        let build = build.clone();
        let running = std::thread::spawn(move || wrela_host::tool::run(&build, options));
        let writing = reached(&told, "write");
        let end = if interrupted {
            interrupt.store(true, Ordering::Release);
            Instant::now()
        } else {
            assert!(writing < started + timeout, "the write started before the timeout");
            started + timeout
        };
        let ended = running.join().expect("the run");
        // The run ends at its timeout or interrupt plus the grace, not when the write does, and
        // the blob table is released though the write still holds the blob's bytes.
        let took = Instant::now().saturating_duration_since(end);
        assert!(took < grace + late, "interrupted: {interrupted}, {took:?} after the end");
        let End::Host(why) = &ended.end else { panic!("{:?}", ended.end) };
        assert!(why.contains("still running"), "{why}");
        let first = if interrupted { "interrupted" } else { "timeout" };
        assert!(why.contains(first), "{why}");
        assert_eq!(ended.end.status(), 71);
        assert_eq!(ended.workers_left, 1);
        assert_eq!(ended.blobs_released, (1, 1024));
    }
}

#[test]
fn a_clock_thread_that_cant_start_ends_the_run_before_any_wasm() {
    let main = r#"
use std::io::print

pub fn tool() -> u32 {
    print("tool ran")
    0
}
"#;
    let build = tool_build("no-clock", main);
    let (lines, mut options) = printer(|_| {});
    options.testing = Testing { fail_clock_thread: true, ..Testing::default() };
    let ended = run(&build, options);
    let End::Host(why) = &ended.end else { panic!("{:?}", ended.end) };
    assert!(why.contains("can't start the thread that stops calls"), "{why}");
    assert_eq!(ended.end.status(), 71);
    assert!(lines.lock().expect("the lines").is_empty(), "no Wasm ran");
}

const ROLLBACK: &str = r#"
use std::io::print
use std::tool::blob
use std::tool::run

pub fn tool() -> u32 {
    // Its standard output fits the budget, as a blob; its standard error doesn't.
    var c = run::command("sh", "out")
    c.args.push(String::from("-c"))
    c.args.push(String::from("/usr/bin/head -c 614400 /dev/zero; /usr/bin/head -c 614400 /dev/zero >&2"))
    match run::run(c).wait() {
        Ok(_) => print("ran"),
        Err(e) => print(f"failed: {e.message()}"),
    }
    print("after the failure")
    // The whole budget is free again.
    let Ok(b) = blob::new().wait() else { return 92 }
    for _ in 0..2 {
        if blob::append(b, Vec::filled(524288, u8(1))).wait().is_err() {
            print("the budget wasn't given back")
            return 1
        }
    }
    0
}
"#;

#[test]
fn a_request_that_fails_part_way_gives_back_its_blobs_and_files() {
    let build = tool_build("rollback", ROLLBACK);
    let root = scratch("rollback");
    let (out, work) = (root.join("out"), root.join("work"));
    std::fs::create_dir_all(&out).expect("out");
    let grants = Grants::default()
        .root("out", &out, true)
        .program("sh", wrela_host::tool::ProgramGrant::At("/bin/sh".into()));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (dir, kept) = (work.clone(), seen.clone());
    // What the run's directory holds once the request has failed, the tool still running.
    let (lines, mut options) = printer(move |line| {
        if line == "after the failure" {
            let mut names: Vec<String> = std::fs::read_dir(&dir)
                .map(|d| {
                    d.filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            *kept.lock().expect("seen") = names;
        }
    });
    options.caps = Caps::open(&grants, |_| None).expect("granted");
    options.blob_budget = BlobBudget { bytes: 1 << 20, handles: 8 };
    options.testing = Testing { work_dir: Some(work), ..Testing::default() };
    let ended = run(&build, options);
    let lines = lines.lock().expect("the lines").clone();
    assert_eq!(ended.end, End::Exit(0), "{lines:?}");
    assert!(lines[0].starts_with("failed: ") && lines[0].contains("budget"), "{lines:?}");
    assert_eq!(*seen.lock().expect("seen"), ["home"], "the request's files are gone");
    assert_eq!(ended.blobs_released, (1, 1 << 20), "only the blob the tool made after");
}

const STARVE: &str = r#"
use std::io::print
use std::tool::Call
use std::tool::files
use std::tool::run

pub fn tool() -> u32 {
    // Four programs fill the long waits' workers; a fifth queues behind them past its deadline.
    var waits: Vec<Call<run::Ran>> = Vec::new()
    for i in 0..5 {
        var c = run::command("sleep", "out")
        c.args.push(String::from(if i < 4 { "2" } else { "0" }))
        if i == 4 {
            c.timeout = 1.0
        }
        waits.push(run::run(c))
    }
    // A file isn't held up by them.
    let asked = std::time::now()
    let _ = files::stat("out").wait()
    if std::time::since(asked) < 1.0 {
        print("the file was answered at once")
    }
    var last_first: Vec<Call<run::Ran>> = Vec::new()
    loop {
        let Some(w) = waits.pop() else { break }
        last_first.push(take w)
    }
    var i = 0
    loop {
        let Some(w) = last_first.pop() else { break }
        match take w.wait() {
            Ok(ran) => print(f"{i}: status {ran.status}"),
            Err(e) => print(f"{i}: {e.message()}"),
        }
        i += 1
    }
    0
}
"#;

#[test]
fn long_waits_hold_up_no_file_and_a_deadline_counts_from_the_request() {
    let build = tool_build("starve", STARVE);
    let out = scratch("starve");
    let grants = Grants::default()
        .root("out", &out, true)
        .program("sleep", wrela_host::tool::ProgramGrant::OnPath);
    let (lines, mut options) = printer(|_| {});
    options.caps =
        Caps::open(&grants, |n| (n == "PATH").then(|| "/bin:/usr/bin".into())).expect("granted");
    let ended = run(&build, options);
    let lines = lines.lock().expect("the lines").clone();
    assert_eq!(ended.end, End::Exit(0), "{lines:?}");
    assert_eq!(lines[0], "the file was answered at once", "{lines:?}");
    for i in 0..4 {
        assert_eq!(lines[1 + i], format!("{i}: status 0"), "{lines:?}");
    }
    assert_eq!(lines[5], "4: `sleep` didn't start within its deadline of 1 s", "{lines:?}");
}

const FETCH_ONE: &str = r#"
use std::io::print
use std::tool::args
use std::tool::http

pub fn tool() -> u32 {
    let given = args()
    var request = http::get(given[0].as_str())
    request.timeout = if given.len() > 1 { 0.3 } else { 60.0 }
    match http::fetch(request).wait() {
        Ok(r) => print(f"status {r.status}"),
        Err(e) => print(f"error: {e.message()}"),
    }
    0
}
"#;

#[test]
fn the_tools_end_closes_a_connection_that_waits_and_a_stuck_one_is_the_hosts_failure() {
    use std::io::Read;
    // A server that takes the connection and never answers. Once the run has returned, it says
    // whether the client had closed the connection by then: its read finds the end at once.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().expect("its address").port();
    let (accepted, taken) = std::sync::mpsc::channel();
    let (check, checking) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("the connection");
        let _ = accepted.send(Instant::now());
        checking.recv().expect("the run returned");
        s.set_nonblocking(true).expect("nonblocking");
        let mut buf = [0u8; 4096];
        loop {
            match s.read(&mut buf) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
    });
    let host = format!("127.0.0.1:{port}");
    let (_, mut options) = printer(|_| {});
    options.caps = Caps::open(&Grants::default().net(&host), |_| None).expect("granted");
    options.args = vec![format!("https://{host}/")];
    let timeout = Duration::from_secs(3);
    options.timeout = Some(timeout);
    let started = Instant::now();
    let running = std::thread::spawn(move || run(&build_of_fetch(), options));
    let at = taken.recv_timeout(Duration::from_secs(60)).expect("the request connected");
    assert!(at < started + timeout, "the request connected before the timeout");
    let ended = running.join().expect("the run");
    check.send(()).expect("the server waits");
    assert_eq!(ended.end, End::Timeout, "a clean end at the timeout, not a host's failure");
    assert!(server.join().expect("the server"), "closed before the run returned");

    // A request whose client can't be stopped (a name lookup that doesn't return, here a
    // test's stalled resolver) ends the run as the host's failure once the grace after its
    // deadline (0.3 s, from the host's taking it, before its lookup) passes, though the run has
    // no timeout; its worker is left past the grace.
    let (signal, told) = signals();
    let (_, mut options) = printer(|_| {});
    options.caps = Caps::open(&Grants::default().net(&host), |_| None).expect("granted");
    options.args = vec![format!("https://{host}/"), "short".into()];
    let grace = Duration::from_millis(100);
    options.testing = Testing {
        grace,
        resolve_stall: Duration::from_secs(5),
        signal: Some(signal),
        ..Testing::default()
    };
    let running = std::thread::spawn(move || wrela_host::tool::run(&build_of_fetch(), options));
    let resolving = reached(&told, "resolve");
    let ended = running.join().expect("the run");
    let took = resolving.elapsed();
    let late = Duration::from_secs(1);
    assert!(took < Duration::from_millis(300) + grace + late, "{took:?} after the lookup began");
    let End::Host(why) = &ended.end else { panic!("{:?}", ended.end) };
    assert!(why.contains("didn't stop within") && why.contains("still running"), "{why}");
    assert_eq!(ended.end.status(), 71);
    assert_eq!(ended.workers_left, 1);
}

/// The tool of [`FETCH_ONE`], built once.
fn build_of_fetch() -> ToolBuild {
    static BUILT: std::sync::OnceLock<ToolBuild> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| tool_build("fetch-one", FETCH_ONE)).clone()
}

const FAKE_BIG: &str = r#"
use std::io::print
use std::tool::http

pub fn tool() -> u32 {
    match http::fetch(http::get("https://game.example/big.bin")).wait() {
        Ok(r) => print(f"status {r.status}"),
        Err(e) => print(f"error: {e.message()}"),
    }
    0
}
"#;

#[test]
fn a_fake_sites_body_has_a_real_ones_limit() {
    let build = tool_build("fake-big", FAKE_BIG);
    let scratch_root = scratch("fake-big");
    let site = scratch_root.join("sites/game.example");
    std::fs::create_dir_all(&site).expect("the site");
    // 256 MiB and a byte, sparse: no disk is spent on it.
    std::fs::File::create(site.join("big.bin"))
        .and_then(|f| f.set_len((256 << 20) + 1))
        .expect("a sparse file");
    let mut grants = Grants::default().root("scratch", &scratch_root, true);
    grants.fake_net = vec!["game.example".into()];
    let (lines, mut options) = printer(|_| {});
    options.caps = Caps::open(&grants, |_| None).expect("granted");
    options.fake_clock = Some((0, 0));
    let ended = run(&build, options);
    let lines = lines.lock().expect("the lines").clone();
    assert_eq!(ended.end, End::Exit(0), "{lines:?}");
    assert_eq!(lines, ["error: the response's body is more than 268435456 bytes"]);
    assert_eq!(ended.blobs_released, (0, 0));
}
