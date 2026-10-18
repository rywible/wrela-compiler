//! The tool host (language.md §15.5, `wrela_host::tool`): a tool build run as a job resumed on
//! answers, with only what its grants reach. Each program here is a `@job fn` that `tool_init`
//! starts and `tool` resumes, waiting on each request with `yield`. The tests run its core
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
use wrela_host::tool::{BlobBudget, Caps, End, Ended, Grants, Options, Testing, ToolBuild};

/// A scratch directory of its own.
fn scratch(name: &str) -> PathBuf {
    let dir = wrela_tests::scratch_root(env!("CARGO_TARGET_TMPDIR")).join(format!("tool-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// The tool build of a package whose `main.wrela` is `main`.
fn tool_build(name: &str, main: &str) -> ToolBuild {
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
fn printer(also: impl Fn(&str) + Send + Sync + 'static) -> (Arc<Mutex<Vec<String>>>, Options) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let kept = lines.clone();
    let mut options = Options::new(Caps::open(&Grants::default(), |_| None).expect("no grants"));
    options.print = Arc::new(move |line| {
        also(line);
        kept.lock().expect("the lines").push(line.to_string());
    });
    (lines, options)
}

/// Runs `build` with `options`, checking that the run left no worker behind.
fn run(build: &ToolBuild, options: Options) -> Ended {
    let ended = wrela_host::tool::run(build, options);
    assert_eq!(ended.workers_left, 0, "every worker stopped: {ended:?}");
    ended
}

/// The `tool_init` and `tool` that start and resume a job `main` of no parameters.
const ENTRIES: &str = r#"
pub fn tool_init() -> Job<main> {
    main.start()
}

pub fn tool(job: mut Job<main>) -> u32 {
    match job.resume() {
        Step::Yielded => RUNNING,
        Step::Done(status) => status,
    }
}
"#;

const SERVICES: &str = r#"
use std::io::print
use std::job::{Job, Step}
use std::tool::{RUNNING, answer, args, secret}
use std::tool::blob
use std::tool::files
use std::tool::time

@job
fn main(given: take Vec<String>) -> u32 {
    var line = String::from("args:")
    for a in given {
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

    var p = files::read("in/a.txt")
    var got = p.poll()
    while got.is_none() {
        yield
        got = p.poll()
    }
    let Some(read) = take got else { return 90 }
    let Ok(bytes) = answer(take read) else { return 91 }
    let Some(text) = String::from_utf8(take bytes) else { return 92 }
    print(f"in/a.txt: {text.as_str()}")

    var w = files::write("out/sub/b.txt", [u8(104), 105])
    var wrote = w.poll()
    while wrote.is_none() {
        yield
        wrote = w.poll()
    }
    let Some(done) = take wrote else { return 93 }
    if answer(take done).is_err() { return 94 }

    var l = files::list("in")
    var listed = l.poll()
    while listed.is_none() {
        yield
        listed = l.poll()
    }
    let Some(polled) = take listed else { return 95 }
    let Ok(entries) = files::read_list(take polled) else { return 96 }
    for e in entries {
        let kind = match e.kind {
            files::Kind::File => "file",
            files::Kind::Dir => "dir",
            files::Kind::Link => "link",
            files::Kind::Other => "other",
        }
        print(f"in has {e.name.as_str()}, a {kind} of {e.size} bytes")
    }
    var s = files::stat("out/sub")
    var stated = s.poll()
    while stated.is_none() {
        yield
        stated = s.poll()
    }
    let Some(st) = take stated else { return 97 }
    let Ok(entry) = files::read_entry(take st) else { return 98 }
    if entry.kind == files::Kind::Dir {
        print("out/sub is a directory")
    }

    // 1.5 MiB in three chunks, out to a file, and back in as another blob.
    var nb = blob::new()
    var made = nb.poll()
    while made.is_none() {
        yield
        made = nb.poll()
    }
    let Some(m) = take made else { return 99 }
    let Ok(b) = blob::read_handle(take m) else { return 100 }
    for k in 0..3 {
        let chunk = Vec::filled(524288, u8(k + 1))
        var ap = blob::append(b, chunk)
        var app = ap.poll()
        while app.is_none() {
            yield
            app = ap.poll()
        }
        let Some(a) = take app else { return 101 }
        if answer(take a).is_err() { return 102 }
    }
    var wb = files::write_blob("out/big.bin", b)
    var wbs = wb.poll()
    while wbs.is_none() {
        yield
        wbs = wb.poll()
    }
    let Some(x) = take wbs else { return 103 }
    if answer(take x).is_err() { return 104 }
    var rel = blob::release(b)
    while rel.poll().is_none() { yield }
    var rb = files::read_blob("out/big.bin")
    var rbs = rb.poll()
    while rbs.is_none() {
        yield
        rbs = rb.poll()
    }
    let Some(y) = take rbs else { return 105 }
    let Ok(back) = blob::read_handle(take y) else { return 106 }
    var n = blob::len(back)
    var ns = n.poll()
    while ns.is_none() {
        yield
        ns = n.poll()
    }
    let Some(z) = take ns else { return 107 }
    let Ok(size) = blob::read_len(take z) else { return 108 }
    var c = blob::read(back, u64(1048576) + 10, 4)
    var cs = c.poll()
    while cs.is_none() {
        yield
        cs = c.poll()
    }
    let Some(q) = take cs else { return 109 }
    let Ok(piece) = answer(take q) else { return 110 }
    print(f"a blob of {size} bytes, at 1 MiB + 10: {piece[0]} {piece[3]}, {piece.len()} bytes")

    let waited = std::time::now()
    var t = time::after(0.05)
    while t.poll().is_none() { yield }
    if std::time::since(waited) >= 0.05 {
        print("the timer fired after 50 ms")
    }
    var u = time::utc_now()
    var us = u.poll()
    while us.is_none() {
        yield
        us = u.poll()
    }
    let Some(v) = take us else { return 111 }
    let Ok(now) = time::read_utc(take v) else { return 112 }
    if now.seconds > u64(1700000000) {
        print("the date is after 2023")
    }
    // `back` isn't released: the host releases it as the tool ends.
    7
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
use std::io::{IoError, Pending, print}
use std::job::{Job, Step}
use std::tool::{RUNNING, answer, secret}
use std::tool::files
use std::tool::time::after

/// Prints what an answer says: `ok`, or why the request failed.
fn say(what: str, polled: take Result<Vec<u8>, IoError>) {
    match answer(take polled) {
        Ok(bytes) => print(f"{what}: ok, {bytes.len()} bytes"),
        Err(e) => print(f"{what}: {e.message()}"),
    }
}

@job
fn main() -> u32 {
    // Asked all at once, each answered when it's done; printed in the order asked.
    var names: Vec<String> = Vec::new()
    var asked: Vec<Pending<u8>> = Vec::new()
    names.push(String::from("read in/../in/a.txt"))
    asked.push(files::read("in/../in/a.txt"))
    names.push(String::from("read /etc/hosts"))
    asked.push(files::read("/etc/hosts"))
    names.push(String::from("read elsewhere/a.txt"))
    asked.push(files::read("elsewhere/a.txt"))
    names.push(String::from("read in/link/secret.txt"))
    asked.push(files::read("in/link/secret.txt"))
    names.push(String::from("read in/escape.txt"))
    asked.push(files::read("in/escape.txt"))
    names.push(String::from("write in/new.txt"))
    asked.push(files::write("in/new.txt", [u8(1)]))
    names.push(String::from("remove_all out"))
    asked.push(files::remove_all("out"))
    names.push(String::from("stat in/swap"))
    asked.push(files::stat("in/swap"))
    var answers: Vec<Option<Result<Vec<u8>, IoError>>> = Vec::new()
    for _ in 0..asked.len() {
        answers.push(None)
    }
    var left = asked.len()
    while left > 0 {
        for i in 0..asked.len() {
            if let Some(a) = asked[i].poll() {
                answers[i] = Some(take a)
                left -= 1
            }
        }
        if left > 0 {
            yield
        }
    }
    for i in 0..answers.len() {
        if let Some(a) = replace(mut answers[i], None) {
            say(names[i].as_str(), take a)
        }
    }
    if secret("PATH").is_none() {
        print("secret PATH: not granted")
    }
    // A directory checked, then swapped for a link out of the root before it's read: the test's
    // printer swaps it, on the host's console thread, and the tool waits until it sees the swap.
    print("checked in/swap")
    loop {
        var s = files::stat("in/swap")
        var st = s.poll()
        while st.is_none() {
            yield
            st = s.poll()
        }
        let Some(a) = take st else { return 91 }
        if answer(take a).is_err() {
            break
        }
        var t = after(0.01)
        while t.poll().is_none() { yield }
    }
    var p = files::read("in/swap/secret.txt")
    var got = p.poll()
    while got.is_none() {
        yield
        got = p.poll()
    }
    let Some(r) = take got else { return 90 }
    say("read in/swap/secret.txt", take r)
    0
}
"#;

#[test]
fn requests_outside_the_grants_fail_and_the_tool_goes_on() {
    let build = tool_build("refusals", &format!("{REFUSALS}{ENTRIES}"));
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
        "stat in/swap: ok, 9 bytes",
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
use std::job::{Job, Step}
use std::tool::{RUNNING, answer}
use std::tool::blob

@job
fn main() -> u32 {
    var handles: Vec<blob::Blob> = Vec::new()
    for _ in 0..3 {
        var p = blob::new()
        var got = p.poll()
        while got.is_none() {
            yield
            got = p.poll()
        }
        let Some(a) = take got else { return 90 }
        match blob::read_handle(take a) {
            Ok(b) => handles.push(b),
            Err(e) => print(f"new: {e.message()}"),
        }
    }
    let sizes = [u32(524288), 524288, 1]
    for i in 0..3 {
        var p = blob::append(handles[i % 2], Vec::filled(sizes[i], u8(9)))
        var got = p.poll()
        while got.is_none() {
            yield
            got = p.poll()
        }
        let Some(a) = take got else { return 91 }
        match answer(take a) {
            Ok(_) => print(f"append {sizes[i]}: ok"),
            Err(e) => print(f"append {sizes[i]}: {e.message()}"),
        }
    }
    var r = blob::release(handles[1])
    while r.poll().is_none() { yield }
    var p = blob::append(handles[0], [u8(1)])
    var got = p.poll()
    while got.is_none() {
        yield
        got = p.poll()
    }
    let Some(a) = take got else { return 92 }
    if answer(take a).is_ok() {
        print("append 1 after a release: ok")
    }
    0
}
"#;

#[test]
fn a_spent_blob_budget_fails_the_request_not_the_tool() {
    let build = tool_build("budget", &format!("{BUDGET}{ENTRIES}"));
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
use std::job::{Job, Step}
use std::tool::RUNNING

@job
fn main() -> u32 {
    // Yields with no request made: nothing would call it again.
    yield
    0
}
"#;
    let build = tool_build("nothing", &format!("{main}{ENTRIES}"));
    let (_, options) = printer(|_| {});
    let ended = run(&build, options);
    assert_eq!(ended.end, End::WaitsOnNothing);
    assert_eq!(ended.end.status(), 70);
}

#[test]
fn a_trap_ends_the_tool_with_its_message_and_releases_its_blobs() {
    let main = r#"
use std::job::{Job, Step}
use std::tool::RUNNING
use std::tool::blob
use std::tool::time::after

@job
fn main() -> u32 {
    var read = blob::new()
    while read.poll().is_none() { yield }
    // Made and answered, but its answer never read: the host releases it too.
    var unread = blob::new()
    var t = after(0.05)
    while t.poll().is_none() { yield }
    if !unread.is_done() {
        panic("the tool broke after its blobs")
    }
    0
}
"#;
    let build = tool_build("trap", &format!("{main}{ENTRIES}"));
    let (_, options) = printer(|_| {});
    let ended = run(&build, options);
    assert_eq!(ended.end, End::Trap("the tool broke after its blobs".into()));
    assert_ne!(ended.end.status(), 0);
    assert_eq!(ended.blobs_released, (2, 0), "the blob it read, and the one it didn't");
}

const FOREVER: &str = r#"
use std::job::{Job, Step}
use std::tool::RUNNING
use std::tool::time::after

@job
fn main(spin: take bool) -> u32 {
    if spin {
        var n: u32 = 0
        loop {
            n = n.wrapping_add(1)
        }
    }
    var t = after(3600.0)
    while t.poll().is_none() { yield }
    0
}

pub fn tool_init() -> Job<main> {
    main.start(std::tool::args().len() > 0)
}

pub fn tool(job: mut Job<main>) -> u32 {
    match job.resume() {
        Step::Yielded => RUNNING,
        Step::Done(status) => status,
    }
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
use std::job::{Job, Step}
use std::tool::{RUNNING, answer}
use std::tool::blob
use std::tool::files

@job
fn main() -> u32 {
    var nb = blob::new()
    var made = nb.poll()
    while made.is_none() {
        yield
        made = nb.poll()
    }
    let Some(m) = take made else { return 90 }
    let Ok(b) = blob::read_handle(take m) else { return 91 }
    var ap = blob::append(b, [u8(1), 2, 3])
    while ap.poll().is_none() { yield }
    // A FIFO with nothing at its other end: each is refused at once, and the tool goes on. One
    // at a time, so no two opens meet at the FIFO's ends.
    let names = ["write_blob", "read", "write", "read_blob"]
    for i in 0..4 {
        var p = match i {
            0 => files::write_blob("out/fifo", b),
            1 => files::read("out/fifo"),
            2 => files::write("out/fifo", [u8(1)]),
            _ => files::read_blob("out/fifo"),
        }
        var got = p.poll()
        while got.is_none() {
            yield
            got = p.poll()
        }
        let Some(a) = take got else { return 92 }
        match answer(take a) {
            Ok(_) => print(f"{names[i]}: ok"),
            Err(e) => print(f"{names[i]}: {e.message()}"),
        }
    }
    0
}
"#;

#[test]
fn files_that_arent_regular_files_are_refused_without_waiting() {
    let build = tool_build("fifo", &format!("{FIFO}{ENTRIES}"));
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
use std::job::{Job, Step}
use std::tool::{RUNNING, answer}
use std::tool::blob
use std::tool::files

@job
fn main() -> u32 {
    var nb = blob::new()
    var made = nb.poll()
    while made.is_none() {
        yield
        made = nb.poll()
    }
    let Some(m) = take made else { return 90 }
    let Ok(b) = blob::read_handle(take m) else { return 91 }
    var ap = blob::append(b, Vec::filled(1024, u8(7)))
    while ap.poll().is_none() { yield }
    var w = files::write_blob("out/slow.bin", b)
    while w.poll().is_none() { yield }
    0
}
"#;

#[test]
fn a_worker_stuck_in_a_file_write_ends_the_run_in_bounded_time_as_the_hosts() {
    let build = tool_build("slow-write", &format!("{SLOW_WRITE}{ENTRIES}"));
    let out = scratch("slow-write");
    for interrupted in [false, true] {
        let (_, mut options) = printer(|_| {});
        options.caps =
            Caps::open(&Grants::default().root("out", &out, true), |_| None).expect("out");
        // A disk that takes 3 s to write the blob, a grace of 100 ms.
        options.testing = Testing {
            grace: Duration::from_millis(100),
            io_delay: Duration::from_secs(3),
            ..Testing::default()
        };
        if interrupted {
            let interrupt = Arc::new(AtomicBool::new(false));
            options.interrupt = Some(interrupt.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                interrupt.store(true, Ordering::Release);
            });
        } else {
            options.timeout = Some(Duration::from_millis(300));
        }
        let started = Instant::now();
        let ended = wrela_host::tool::run(&build, options);
        let took = started.elapsed();
        // The run ends at its timeout or interrupt plus the grace, not when the write does, and
        // the blob table is released though the write still holds the blob's bytes.
        assert!(took < Duration::from_secs(2), "interrupted: {interrupted}, {took:?}");
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
use std::job::{Job, Step}
use std::tool::RUNNING

@job
fn main() -> u32 {
    0
}

pub fn tool_init() -> Job<main> {
    print("tool_init ran")
    main.start()
}

pub fn tool(job: mut Job<main>) -> u32 {
    match job.resume() {
        Step::Yielded => RUNNING,
        Step::Done(status) => status,
    }
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
