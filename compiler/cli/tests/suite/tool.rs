//! `wrela tool`: builds a package's tool and runs it with the command line's grants and
//! arguments; the process's exit status is the tool's.

use crate::common::{manifest, package, wrela};
use std::process::Command;

/// A tool that copies the file its first argument names from `in` to `out`, prints its secret's
/// length, and exits with as many arguments as it was given.
const COPY: &str = r#"
use std::io::print
use std::job::{Job, Step}
use std::tool::{RUNNING, answer, args, secret}
use std::tool::files

@job
fn main(given: take Vec<String>) -> u32 {
    if given.len() == 0 {
        return 64
    }
    let name = given[0].clone()
    var p = files::read(f"in/{name.as_str()}")
    var got = p.poll()
    while got.is_none() {
        yield
        got = p.poll()
    }
    let Some(read) = take got else { return 90 }
    let Ok(bytes) = answer(take read) else { return 91 }
    var w = files::write(f"out/copy/{name.as_str()}", bytes)
    var wrote = w.poll()
    while wrote.is_none() {
        yield
        wrote = w.poll()
    }
    let Some(done) = take wrote else { return 92 }
    if answer(take done).is_err() {
        return 93
    }
    if let Some(s) = secret("WRELA_TOOL_TEST_TOKEN") {
        print(f"copied {name.as_str()}; the token is {s.len()} bytes")
    }
    u32(given.len())
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
fn wrela_tool_runs_the_tool_with_its_grants_and_exits_with_its_status() {
    let dir = package(
        "tool-copy",
        &[
            ("wrela.toml", &manifest("tool_copy")),
            ("main.wrela", COPY),
            ("data/in/notes.txt", "kept"),
            ("data/out/.keep", ""),
        ],
    );
    let (input, output) = (dir.join("data/in"), dir.join("data/out"));
    let out = wrela()
        .arg("tool")
        .arg(&dir)
        .arg("--read")
        .arg(format!("in={}", input.display()))
        .arg("--write")
        .arg(format!("out={}", output.display()))
        .args(["--secret", "WRELA_TOOL_TEST_TOKEN", "--", "notes.txt", "b", "c"])
        .env("WRELA_TOOL_TEST_TOKEN", "abcd")
        .output()
        .expect("run wrela tool");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{stdout}{stderr}");
    assert_eq!(stdout, "copied notes.txt; the token is 4 bytes\n");
    assert!(!stdout.contains("abcd") && !stderr.contains("abcd"), "the secret isn't shown");
    assert_eq!(std::fs::read_to_string(output.join("copy/notes.txt")).expect("copied"), "kept");

    // Without the write root, the write fails and the tool says so with its status.
    let out = wrela()
        .arg("tool")
        .arg(&dir)
        .arg("--read")
        .arg(format!("in={}", input.display()))
        .args(["--", "notes.txt"])
        .output()
        .expect("run wrela tool");
    assert_eq!(out.status.code(), Some(93), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn wrela_tool_ends_a_tool_at_its_timeout_and_refuses_bad_grants() {
    let main = r#"
pub fn tool() -> u32 {
    var n: u32 = 0
    loop {
        n = n.wrapping_add(1)
    }
    0
}
"#;
    let dir = package("tool-loop", &[("wrela.toml", &manifest("tool_loop")), ("main.wrela", main)]);
    let started = std::time::Instant::now();
    // Helpers are 0 to 8.
    for bad in ["9", "-1", "two"] {
        let out = wrela().arg("tool").arg(&dir).args(["--workers", bad]).output().expect("run");
        assert_eq!(out.status.code(), Some(2), "--workers {bad}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("--workers takes"), "{bad}");
    }
    let out = wrela().arg("tool").arg(&dir).args(["--timeout", "0.3"]).output().expect("run");
    assert_eq!(out.status.code(), Some(124), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("ran past its timeout"));
    assert!(started.elapsed() < std::time::Duration::from_secs(60));
    // A timeout too large for the clock is a usage error, not a panic.
    for huge in ["1e100", "1e300", "inf", "NaN", "-1"] {
        let out = wrela().arg("tool").arg(&dir).args(["--timeout", huge]).output().expect("run");
        assert_eq!(out.status.code(), Some(2), "--timeout {huge}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("--timeout takes seconds"));
    }
    // A grant that names no directory, or a secret that isn't set, stops it before it starts.
    let out = wrela().arg("tool").arg(&dir).args(["--read", "src"]).output().expect("run");
    assert_eq!(out.status.code(), Some(2));
    let out = wrela()
        .arg("tool")
        .arg(&dir)
        .args(["--secret", "WRELA_TOOL_TEST_UNSET"])
        .env_remove("WRELA_TOOL_TEST_UNSET")
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("isn't set in the environment"));
}

/// Waits for `child` at most `bound`: its exit code, or `None` (and it's killed) past it.
fn exit_within(child: &mut std::process::Child, bound: std::time::Duration) -> Option<i32> {
    let started = std::time::Instant::now();
    while started.elapsed() < bound {
        if let Some(status) = child.try_wait().expect("wait") {
            return status.code();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

#[test]
fn a_console_nobody_reads_holds_up_neither_the_timeout_nor_ctrl_c() {
    use std::io::Read;
    use std::process::Stdio;
    // Prints without end, in one call: its standard output fills and nobody drains it.
    let main = r#"
use std::io::print

pub fn tool() -> u32 {
    let line = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    loop {
        print(line)
    }
    0
}
"#;
    let dir =
        package("tool-console", &[("wrela.toml", &manifest("tool_console")), ("main.wrela", main)]);
    let bound = std::time::Duration::from_secs(30);
    let mut child = wrela()
        .arg("tool")
        .arg(&dir)
        .args(["--timeout", "1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run wrela tool");
    assert_eq!(exit_within(&mut child, bound), Some(124), "the timeout ends it");

    let mut child = wrela()
        .arg("tool")
        .arg(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run wrela tool");
    // Once it prints, it runs (its Ctrl-C handler is set); then nothing reads what it prints.
    let mut first = [0u8; 1];
    child.stdout.as_mut().expect("its output").read_exact(&mut first).expect("a line");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let sent = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill");
    assert!(sent.success());
    assert_eq!(exit_within(&mut child, bound), Some(130), "the first Ctrl-C ends it");
}

/// A tool that runs one granted program and prints what it gave: `<program> [--stdin=TEXT]
/// [--env=NAME=VALUE]... [--secret=NAME]... [--timeout=S] -- [args...]`, in the write root
/// `work`. It prints the program's standard output, then `status N`; or `error: ...` and exits 1.
const RUN: &str = r#"
use std::io::print
use std::job::{Job, Step}
use std::tool::{RUNNING, args}
use std::tool::blob::Data
use std::tool::run

/// The bytes of `text`.
fn utf8(text: str) -> Vec<u8> {
    var out: Vec<u8> = Vec::new()
    for i in 0..text.len() {
        out.push(text.byte(i))
    }
    out
}

/// `text` after its first `=`, and before it.
fn split(text: str) -> (String, String) {
    for i in 0..text.len() {
        if text.byte(i) == 61 {
            return (String::from(text.slice(0, i)), String::from(text.slice(i + 1, text.len())))
        }
    }
    (String::from(text), String::new())
}

@job
fn main(given: take Vec<String>) -> u32 {
    var c = run::command(given[0].as_str(), "work")
    var i = 1
    while i < given.len() {
        let a = given[i].as_str()
        i += 1
        if a == "--" {
            break
        }
        let (flag, value) = split(a)
        if flag.as_str() == "--stdin" {
            c.stdin = Data::Bytes(utf8(value.as_str()))
        } else if flag.as_str() == "--env" {
            let (name, v) = split(value.as_str())
            c.env.push(run::Var { name, value: v })
        } else if flag.as_str() == "--secret" {
            c.secrets.push(take value)
        } else if flag.as_str() == "--timeout" {
            c.timeout = if value.as_str() == "0.5" { 0.5 } else { 30.0 }
        }
    }
    while i < given.len() {
        c.args.push(given[i].clone())
        i += 1
    }
    var r = run::run(c)
    var got = r.poll()
    while got.is_none() {
        yield
        got = r.poll()
    }
    let Some(polled) = take got else { return 90 }
    match run::read_ran(take polled) {
        Ok(ran) => {
            if let Data::Bytes(out) = ran.stdout {
                if let Some(text) = String::from_utf8(out.clone()) {
                    print(text.as_str())
                }
            }
            print(f"status {ran.status}")
            0
        }
        Err(e) => {
            print(f"error: {e.message()}")
            1
        }
    }
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

/// The package of [`RUN`] and its write root `work`, in a scratch directory named `name`.
fn run_package(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = package(
        name,
        &[("wrela.toml", &manifest("tool_run")), ("main.wrela", RUN), ("work/.keep", "")],
    );
    let work = dir.join("work");
    (dir, work)
}

/// `wrela tool` on `dir` with the write root `work`, granting each of `programs`.
fn run_tool(dir: &std::path::Path, work: &std::path::Path, programs: &[&str]) -> Command {
    let mut c = wrela();
    c.arg("tool").arg(dir).arg("--write").arg(format!("work={}", work.display()));
    for p in programs {
        c.args(["--allow-run", p]);
    }
    c
}

/// Whether a process whose command line has `marker` is still running, after waiting up to 2 s
/// for one to go.
fn still_running(marker: &str) -> bool {
    let started = std::time::Instant::now();
    loop {
        let found = Command::new("pgrep").args(["-f", marker]).output().expect("pgrep");
        if !found.status.success() {
            return false;
        }
        if started.elapsed() > std::time::Duration::from_secs(2) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn a_granted_program_runs_with_its_arguments_and_input() {
    let (dir, work) = run_package("tool-run-echo");
    let out = run_tool(&dir, &work, &["echo"]).args(["--", "echo", "--", "a", "b c"]).output();
    let out = out.expect("run wrela tool");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, "a b c\n\nstatus 0\n");
    let out = run_tool(&dir, &work, &["cat"])
        .args(["--", "cat", "--stdin=piped in", "--"])
        .output()
        .expect("run wrela tool");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "piped in\nstatus 0\n");
    // A program that isn't granted isn't run.
    let out = run_tool(&dir, &work, &["cat"]).args(["--", "echo", "--"]).output().expect("run");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("isn't granted"));
    // Nor is one that isn't on PATH.
    let out = run_tool(&dir, &work, &["no-such-program-wrela"]).args(["--", "x"]).output();
    let out = out.expect("run");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("isn't on PATH"));
}

#[test]
fn a_program_gets_an_empty_environment_and_its_baseline() {
    let (dir, work) = run_package("tool-run-env");
    let path = std::env::var("PATH").expect("PATH");
    let out = run_tool(&dir, &work, &["env"])
        .args(["--secret", "WRELA_RUN_TOKEN"])
        .args(["--", "env", "--env=EXTRA=1", "--secret=WRELA_RUN_TOKEN", "--"])
        .env("WRELA_RUN_SENTINEL", "leaked")
        .env("WRELA_RUN_TOKEN", "t0ken")
        .output()
        .expect("run wrela tool");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    let mut names: Vec<&str> =
        stdout.lines().filter_map(|l| l.split_once('=').map(|(n, _)| n)).collect();
    names.sort();
    assert_eq!(names, ["EXTRA", "HOME", "LANG", "PATH", "WRELA_RUN_TOKEN"], "{stdout}");
    assert!(stdout.contains(&format!("PATH={path}\n")), "{stdout}");
    assert!(stdout.contains("LANG=C.UTF-8\n") && stdout.contains("WRELA_RUN_TOKEN=t0ken\n"));
    assert!(!stdout.contains("leaked"), "the parent's environment isn't the program's");
    let home = stdout.lines().find_map(|l| l.strip_prefix("HOME=")).expect("HOME");
    assert!(!std::path::Path::new(home).exists(), "the host removes its HOME as the tool ends");
    // A granted secret goes only to a program whose request names it, and an ungranted one to
    // none.
    let out = run_tool(&dir, &work, &["env"])
        .args(["--secret", "WRELA_RUN_TOKEN", "--", "env", "--"])
        .env("WRELA_RUN_TOKEN", "t0ken")
        .output()
        .expect("run wrela tool");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("t0ken"));
    let out = run_tool(&dir, &work, &["env"])
        .args(["--", "env", "--secret=WRELA_RUN_TOKEN", "--"])
        .env("WRELA_RUN_TOKEN", "t0ken")
        .output()
        .expect("run wrela tool");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("isn't granted"));
}

#[test]
fn a_program_is_the_file_found_when_the_grants_were_opened() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, work) = run_package("tool-run-path");
    let (a, b) = (dir.join("a"), dir.join("b"));
    for (d, who) in [(&a, "A"), (&b, "B")] {
        std::fs::create_dir_all(d).expect("mkdir");
        let script = d.join("hello");
        std::fs::write(&script, format!("#!/bin/sh\necho {who} \"$PATH\"\n")).expect("write");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    // Found on the PATH of `wrela tool`; a PATH the request gives is the program's, but doesn't
    // change which program runs.
    let out = run_tool(&dir, &work, &["hello"])
        .args(["--", "hello", &format!("--env=PATH={}", b.display()), "--"])
        .env("PATH", format!("{}:/usr/bin:/bin", a.display()))
        .output()
        .expect("run wrela tool");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout, format!("A {}\n\nstatus 0\n", b.display()), "{stdout}");
    // `NAME=PATH` names the file itself.
    let out = run_tool(&dir, &work, &[&format!("hello={}", b.join("hello").display())])
        .args(["--", "hello", "--"])
        .output()
        .expect("run wrela tool");
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("B "), "{out:?}");
}

#[test]
fn a_deadline_or_the_tools_end_kills_the_programs_whole_process_group() {
    let (dir, work) = run_package("tool-run-deadline");
    // Its deadline: the shell and the sleep it started in the background both go.
    let marker = "sleep 31.4159";
    let started = std::time::Instant::now();
    let out = run_tool(&dir, &work, &["sh"])
        .args(["--", "sh", "--timeout=0.5", "--", "-c", &format!("{marker} & {marker}")])
        .output()
        .expect("run wrela tool");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("ran past its deadline of 0.5 s"), "{stdout}");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(!still_running(marker), "the program's whole group is killed");
    // The tool's end, with the program still running: the tool's timeout ends both.
    let marker = "sleep 27.1828";
    let started = std::time::Instant::now();
    let out = run_tool(&dir, &work, &["sh"])
        .args(["--timeout", "0.5", "--", "sh", "--", "-c", &format!("{marker} & {marker}")])
        .output()
        .expect("run wrela tool");
    assert_eq!(out.status.code(), Some(124), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(!still_running(marker), "the tool's end kills the program's group");
}

/// A tool that fetches the URL it's given, with a deadline of 0.5 s if a second argument says
/// `short`: it prints the status, or `error: ...` and exits 1.
const FETCH: &str = r#"
use std::io::print
use std::job::{Job, Step}
use std::tool::{RUNNING, args}
use std::tool::http

@job
fn main(given: take Vec<String>) -> u32 {
    var request = http::get(given[0].as_str())
    if given.len() > 1 {
        request.timeout = 0.5
    }
    var f = http::fetch(request)
    var got = f.poll()
    while got.is_none() {
        yield
        got = f.poll()
    }
    let Some(polled) = take got else { return 90 }
    match http::read_response(take polled) {
        Ok(response) => {
            print(f"status {response.status}")
            0
        }
        Err(e) => {
            print(f"error: {e.message()}")
            1
        }
    }
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
fn http_reaches_only_a_granted_host_and_a_stuck_server_holds_up_no_end() {
    let dir =
        package("tool-fetch", &[("wrela.toml", &manifest("tool_fetch")), ("main.wrela", FETCH)]);
    // Refused before any connection: no grant, or not HTTPS.
    for (url, grant, words) in [
        ("https://example.com/", None, "isn't granted"),
        ("https://example.com:444/", Some("example.com"), "isn't granted"),
        ("http://example.com/", Some("example.com"), "isn't an `https://` URL"),
    ] {
        let mut c = wrela();
        c.arg("tool").arg(&dir);
        if let Some(g) = grant {
            c.args(["--net", g]);
        }
        let out = c.args(["--", url]).output().expect("run wrela tool");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(1), "{url}: {stdout}");
        assert!(stdout.contains(words), "{url}: {stdout}");
    }
    // A server that takes the connection and never answers: the request's deadline fails it,
    // and the tool's timeout ends the tool however long the request would wait.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().expect("its address").port();
    let held = std::thread::spawn(move || {
        let mut kept = Vec::new();
        for s in listener.incoming().take(2) {
            kept.push(s);
        }
        std::thread::sleep(std::time::Duration::from_secs(30));
    });
    let host = format!("127.0.0.1:{port}");
    let url = format!("https://{host}/");
    let started = std::time::Instant::now();
    let out = wrela()
        .arg("tool")
        .arg(&dir)
        .args(["--net", &host, "--", &url, "short"])
        .output()
        .expect("run wrela tool");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.starts_with("error: "), "{stdout}");
    assert!(started.elapsed() < std::time::Duration::from_secs(15), "{:?}", started.elapsed());
    let started = std::time::Instant::now();
    let out = wrela()
        .arg("tool")
        .arg(&dir)
        .args(["--net", &host, "--timeout", "0.5", "--", &url])
        .output()
        .expect("run wrela tool");
    assert_eq!(out.status.code(), Some(124), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(started.elapsed() < std::time::Duration::from_secs(15), "{:?}", started.elapsed());
    // A name lookup that stalls past the request's deadline, which no socket cuts, with no
    // `--timeout`: the host's failure, the grace after the deadline, and the process (with the
    // client's thread) ends.
    let started = std::time::Instant::now();
    let out = wrela()
        .arg("tool")
        .arg(&dir)
        .args(["--net", &host, "--", &url, "short"])
        .env("WRELA_TESTING_RESOLVE_STALL", "60")
        .output()
        .expect("run wrela tool");
    assert_eq!(out.status.code(), Some(71), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("didn't stop within"));
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
    drop(held);
}

/// A tool that serves its write root `scratch`, opens its `index.html` in a browser, says so,
/// and waits until it's stopped.
const BROWSE: &str = r#"
use std::io::print
use std::job::{Job, Step}
use std::tool::RUNNING
use std::tool::browser
use std::tool::serve
use std::tool::time

@job
fn main() -> u32 {
    var s = serve::start("scratch")
    var sg = s.poll()
    while sg.is_none() {
        yield
        sg = s.poll()
    }
    let Some(sp) = take sg else { return 90 }
    let Ok(started) = serve::read_started(take sp) else { return 91 }
    var o = browser::open(f"{started.origin.as_str()}/".as_str())
    var og = o.poll()
    while og.is_none() {
        yield
        og = o.poll()
    }
    let Some(op) = take og else { return 90 }
    match browser::read_opened(take op) {
        Ok(_) => print("opened"),
        Err(e) => {
            print(f"open: {e.message()}")
            return 1
        },
    }
    var t = time::after(600.0)
    while t.poll().is_none() { yield }
    0
}

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

/// Whether process group `group` still has a process.
fn group_alive(group: i32) -> bool {
    Command::new("kill")
        .args(["-0", &format!("-{group}")])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The browse tool's package, with its site and a temporary directory of its own.
fn browse_package(name: &str) -> std::path::PathBuf {
    package(
        name,
        &[
            ("wrela.toml", &manifest("tool_browse")),
            ("main.wrela", BROWSE),
            ("site/index.html", "<!doctype html><title>Site</title>"),
            ("tmp/.keep", ""),
        ],
    )
}

/// `wrela tool` running the browse tool of `dir`, under a supervisor's file of groups, with
/// `dir/tmp` as its temporary directory, once it has said its browser opened: the child, and
/// the one group recorded, Chrome's.
fn browsing(dir: &std::path::Path) -> (std::process::Child, i32) {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let groups = dir.join("groups.txt");
    let _ = std::fs::remove_file(&groups);
    let mut child = wrela()
        .arg("tool")
        .arg(dir)
        .arg("--write")
        .arg(format!("scratch={}", dir.join("site").display()))
        .arg("--browser")
        .env("WRELA_TOOL_GROUPS", &groups)
        .env("TMPDIR", dir.join("tmp"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("run wrela tool");
    let mut first = String::new();
    BufReader::new(child.stdout.as_mut().expect("its output"))
        .read_line(&mut first)
        .expect("a line");
    assert_eq!(first.trim(), "opened");
    let recorded: Vec<i32> = std::fs::read_to_string(&groups)
        .expect("the groups")
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    assert_eq!(recorded.len(), 1, "Chrome's group: {recorded:?}");
    assert!(group_alive(recorded[0]), "Chrome runs in the recorded group");
    (child, recorded[0])
}

/// The profiles under `at`: each holds the log the driver keeps there.
fn profiles(at: &std::path::Path) -> usize {
    walk(at).into_iter().filter(|p| p.ends_with("chrome.log")).count()
}

/// Under a supervisor, Chrome's process group is recorded before Chrome runs; Ctrl-C ends the
/// tool in bounded time, and with it every process of that group, and the profile.
#[test]
#[ignore = "needs the runtime"]
fn a_tools_browser_is_supervised_and_ends_with_it() {
    let dir = browse_package("tool-browse");
    let (mut child, group) = browsing(&dir);
    assert_eq!(profiles(&dir.join("tmp")), 1, "its profile is in the run's own directory");
    let sent = Command::new("kill").args(["-INT", &child.id().to_string()]).status().expect("kill");
    assert!(sent.success());
    assert_eq!(exit_within(&mut child, std::time::Duration::from_secs(30)), Some(130));
    assert!(!group_alive(group), "no process of Chrome's group is left");
    assert_eq!(profiles(&dir.join("tmp")), 0, "and no profile");
}

/// A tool host killed outright (`SIGKILL`, which it can't clean up after) takes its Chrome with
/// it: Chrome's end of its DevTools pipe closes, and it shuts down, at once. What's left is the
/// run's directory, with the profile; the next run sweeps it.
#[test]
#[ignore = "needs the runtime"]
fn a_killed_tool_host_leaves_no_chrome_and_its_profile_goes_at_the_next_start() {
    let dir = browse_package("tool-browse-killed");
    let (mut child, group) = browsing(&dir);
    child.kill().expect("SIGKILL");
    child.wait().expect("reaped");
    let since = std::time::Instant::now();
    while group_alive(group) {
        assert!(since.elapsed() < std::time::Duration::from_secs(10), "Chrome outlived its host");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(profiles(&dir.join("tmp")), 1, "the killed host's profile is left");
    // The next run's start sweeps it.
    let (mut next, _) = browsing(&dir);
    assert_eq!(profiles(&dir.join("tmp")), 1, "only the new run's profile is there");
    let sent = Command::new("kill").args(["-INT", &next.id().to_string()]).status().expect("kill");
    assert!(sent.success());
    assert_eq!(exit_within(&mut next, std::time::Duration::from_secs(30)), Some(130));
    assert_eq!(profiles(&dir.join("tmp")), 0);
}

/// The files under `dir`, at any depth.
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// A workspace with the tool `copy` (the package `copier`, [`COPY`]), its grants `extra` added to
/// what it declares: `in` to read, `out` to write (not made yet), its secret, and a minute.
fn copy_workspace(name: &str, extra: &str) -> std::path::PathBuf {
    let tool = format!(
        "[tools.copy]\npackage = \"copier\"\nread = [\"in=data/in\"]\nwrite = [\"out=data/out\"]\nsecrets = [\"WRELA_TOOL_TEST_TOKEN\"]\ntimeout = 60\n{extra}"
    );
    package(
        name,
        &[
            ("wrela.toml", &format!("[workspace]\nmembers = [\"copier\", \"plain\"]\n\n{tool}")),
            ("copier/wrela.toml", &manifest("copier")),
            ("copier/main.wrela", COPY),
            ("plain/wrela.toml", &manifest("plain")),
            ("plain/main.wrela", "pub fn frame(time: f32, width: u32, height: u32) {}\n"),
            ("data/in/notes.txt", "kept"),
            ("data/in/sub/notes.txt", "narrowed"),
        ],
    )
}

/// `wrela tool <name>` in `dir` with `args`: its status and what it printed.
fn named(dir: &std::path::Path, args: &[&str]) -> (Option<i32>, String) {
    let out = wrela()
        .arg("tool")
        .args(args)
        .current_dir(dir)
        .env("WRELA_TOOL_TEST_TOKEN", "abcd")
        .output()
        .expect("run wrela tool");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code(), text)
}

#[test]
fn a_named_tool_runs_with_the_grants_its_workspace_declares() {
    let ws = copy_workspace("tool-named", "");
    assert!(!ws.join("data/out").exists());
    // From below the workspace's directory, the name finds the tool.
    let (status, out) = named(&ws.join("plain"), &["copy", "--", "notes.txt", "b", "c"]);
    assert_eq!(status, Some(3), "{out}");
    assert_eq!(out, "copied notes.txt; the token is 4 bytes\n");
    assert_eq!(
        std::fs::read_to_string(ws.join("data/out/copy/notes.txt")).expect("copied"),
        "kept"
    );
    // A path is a package's directory, as before: with no grants, it can read nothing.
    let (status, out) = named(&ws, &["./copier", "--", "notes.txt"]);
    assert_eq!(status, Some(91), "{out}");
    // A word that names no tool and no directory says what the workspace has.
    let (status, out) = named(&ws, &["missing"]);
    assert_eq!(status, Some(2), "{out}");
    assert!(
        out.contains("neither a package's directory nor a tool") && out.contains("copy"),
        "{out}"
    );
}

#[test]
fn the_command_line_may_narrow_a_named_tools_grants_and_never_widen_them() {
    let ws = copy_workspace("tool-narrow", "net = [\"a.example\"]\nrun = [\"ls\"]\nworkers = 2\n");
    let at = |p: &str| ws.join(p).canonicalize().expect("a directory").display().to_string();
    std::fs::create_dir_all(ws.join("data/out/sub")).expect("mkdir");
    // Narrower: a root below the declared one, no secret, a shorter timeout, fewer helpers.
    let read = format!("in={}", at("data/in/sub"));
    let write = format!("out={}", at("data/out/sub"));
    let (status, out) = named(
        &ws,
        &["copy", "--read", &read, "--write", &write, "--net", "a.example", "--allow-run", "ls"],
    );
    assert_eq!(status, Some(64), "the tool ran, with no arguments: {out}");
    let (status, out) = named(
        &ws,
        &[
            "copy",
            "--read",
            &read,
            "--write",
            &write,
            "--timeout",
            "30",
            "--workers",
            "1",
            "--",
            "notes.txt",
        ],
    );
    assert_eq!(status, Some(1), "{out}");
    assert!(out.is_empty(), "no secret was granted: {out}");
    assert_eq!(
        std::fs::read_to_string(ws.join("data/out/sub/copy/notes.txt")).expect("copied"),
        "narrowed"
    );
    // Wider: each is refused before the tool starts.
    let above = format!("in={}", at("data"));
    let write_in = format!("in={}", at("data/in"));
    let other = format!("other={}", at("data/in"));
    for (args, says) in [
        (&["--read", above.as_str()][..], "`--read in="),
        (&["--write", write_in.as_str()], "`--write in="),
        (&["--read", other.as_str()], "`--read other="),
        (&["--net", "b.example"], "`--net b.example`"),
        (&["--secret", "HOME"], "`--secret HOME`"),
        (&["--browser"], "`--browser`"),
        (&["--allow-run", "cat"], "`--allow-run cat`"),
        (&["--allow-run", "ls=/bin/cat"], "`--allow-run ls=...`"),
        (&["--timeout", "61"], "`--timeout 61`"),
        (&["--workers", "3"], "`--workers 3`"),
    ] {
        let mut all = vec!["copy"];
        all.extend(args);
        let (status, out) = named(&ws, &all);
        assert_eq!(status, Some(2), "{args:?}: {out}");
        assert!(out.contains(says) && out.contains("widens"), "{args:?}: {out}");
    }
}

#[test]
fn a_workspaces_tools_are_checked_when_it_is_read() {
    let tool = |body: &str| {
        format!("[workspace]\nmembers = [\"copier\", \"plain\"]\n\n[tools.copy]\n{body}")
    };
    for (body, code, says) in [
        ("package = \"copier\"\ncolour = \"red\"\n", "E0219", "`colour` isn't a key"),
        ("read = [\"in=data/in\"]\n", "E0219", "names no `package`"),
        ("package = \"copier\"\nwrite = [\"out=../out\"]\n", "E0219", "isn't a root"),
        ("package = \"elsewhere\"\n", "E0223", "isn't a member of this workspace"),
        ("package = \"plain\"\n", "E0223", "has no tool build"),
    ] {
        let ws = copy_workspace("tool-invalid", "");
        std::fs::write(ws.join("wrela.toml"), tool(body)).expect("write");
        let (status, out) = named(&ws, &["copy"]);
        assert_eq!(status, Some(2), "{body}: {out}");
        assert!(out.contains(code) && out.contains(says), "{body}: {out}");
        let suite = wrela().arg("suite").arg("fmt").current_dir(&ws).output().expect("run");
        let (status, out) = (suite.status.code(), String::from_utf8_lossy(&suite.stderr));
        assert_eq!(status, Some(1), "{body}: {out}");
        assert!(out.contains(code) && out.contains(says), "the suite reads it too: {body}: {out}");
    }
    // A write root that a link leads outside the workspace is never made there.
    let ws = copy_workspace("tool-link", "");
    let outside = package("tool-link-outside", &[]);
    std::fs::create_dir_all(ws.join("data")).expect("mkdir");
    std::os::unix::fs::symlink(&outside, ws.join("data/out")).expect("link");
    let (status, out) = named(&ws, &["copy", "--", "notes.txt"]);
    assert_eq!(status, Some(2), "{out}");
    assert!(out.contains("isn't a directory inside the workspace"), "{out}");
    assert_eq!(std::fs::read_dir(&outside).expect("read").count(), 0, "nothing was made outside");
}

/// A directory of the workspace swapped for a link outside it, after the grants are checked and
/// before the tool runs (at the moment the command says it has checked them), neither widens a
/// narrowed root nor moves a write root made inside the workspace: the run keeps the
/// directories it checked, and nothing outside is read or made.
#[test]
fn a_directory_swapped_after_the_grants_are_checked_leads_nowhere_new() {
    let ws = copy_workspace("tool-swap", "");
    let outside = package(
        "tool-swap-outside",
        &[("in/sub/notes.txt", "outside"), ("in/notes.txt", "outside"), ("out/.keep", "")],
    );
    let hold = package("tool-swap-hold", &[]);
    let canonical = ws.canonicalize().expect("canonical");
    let read = format!("in={}", canonical.join("data/in/sub").display());
    let mut child = wrela()
        .arg("tool")
        .args(["copy", "--read", &read, "--write"])
        .arg(format!("out={}", canonical.join("data/out").display()))
        .args(["--", "notes.txt"])
        .current_dir(&ws)
        .env("WRELA_TESTING_GRANTS_HOLD", &hold)
        .spawn()
        .expect("run wrela tool");
    // The write root wasn't there: the command made it, before it said it had checked.
    assert!(
        within(std::time::Duration::from_secs(60), || hold.join("checked").exists()),
        "the grants were checked"
    );
    assert!(ws.join("data/out").is_dir(), "the write root was made");
    std::fs::rename(ws.join("data"), ws.join("data-kept")).expect("move the checked directory");
    std::os::unix::fs::symlink(&outside, ws.join("data")).expect("a link outside");
    std::fs::write(hold.join("go"), "").expect("go");
    let status = child.wait().expect("the tool ends");
    assert_eq!(status.code(), Some(1), "the tool copied its one file");
    let copied = std::fs::read_to_string(ws.join("data-kept/out/copy/notes.txt"));
    assert_eq!(copied.expect("copied into the checked root").as_str(), "narrowed");
    assert!(!outside.join("out/copy").exists(), "nothing was made outside the workspace");
}

/// Whether `done` holds within `bound`, asked every few milliseconds.
fn within(bound: std::time::Duration, mut done: impl FnMut() -> bool) -> bool {
    let until = std::time::Instant::now() + bound;
    while std::time::Instant::now() < until {
        if done() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    done()
}
