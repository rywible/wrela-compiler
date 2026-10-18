//! `std::tool::toolchain` (language.md §15.5): a tool builds, tests, checks and formats packages
//! in its roots by the compiler's own code, reading only through its grants, keeping its
//! constants' cache outside them, and stopping (`wasm-opt` too) at its deadline or its end.

use crate::common::{manifest, package, wrela};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// A tool that does what its arguments say, through the toolchain, and prints what it got:
/// `build PKG OUT`, `build-optimized PKG OUT` (with a 2 s deadline), `test PKG`, `check PKG`,
/// `fmt PATH` and `fmt-check PATH`. A request that fails prints `error: ...` and exits 1.
const TOOL: &str = r#"
use std::io::print
use std::job::{Job, Step}
use std::tool::{RUNNING, ToolError, args}
use std::tool::toolchain

fn failed(e: ToolError) -> u32 {
    print(f"error: {e.message()}")
    1
}

fn diagnostics(ds: [toolchain::Diagnostic]) {
    for d in ds {
        print(f"diagnostic {d.code.as_str()} error={d.error} at={d.at.as_str()} {d.message.as_str()}")
    }
}

@job
fn main(given: take Vec<String>) -> u32 {
    let what = given[0].clone()
    var paths: Vec<String> = Vec::new()
    paths.push(given[1].clone())
    var p = if what.as_str() == "build" || what.as_str() == "build-optimized" || what.as_str() == "build-timed" {
        var options = toolchain::options()
        if what.as_str() == "build-optimized" {
            options.optimize = true
            options.timeout = 2.0
        }
        if what.as_str() == "build-timed" {
            options.timeout = 2.0
        }
        toolchain::build(given[1].as_str(), given[2].as_str(), options)
    } else if what.as_str() == "test" {
        toolchain::test(given[1].as_str(), "")
    } else if what.as_str() == "check" {
        toolchain::check(given[1].as_str())
    } else if what.as_str() == "doc" || what.as_str() == "doc-timed" {
        let timeout = if what.as_str() == "doc-timed" { 2.0 } else { 0.0 }
        toolchain::doc_export(paths, given[2].as_str(), "https://example.test/repo", "", timeout)
    } else {
        toolchain::fmt(paths, what.as_str() == "fmt-check")
    }
    var got = p.poll()
    while got.is_none() {
        yield
        got = p.poll()
    }
    let Some(polled) = take got else { return 90 }
    if what.as_str() == "build" || what.as_str() == "build-optimized" || what.as_str() == "build-timed" {
        let built = match toolchain::read_built(take polled) {
            Ok(b) => take b,
            Err(e) => return failed(e),
        }
        print(f"built ok={built.ok} files={built.files.len()}")
        diagnostics(built.diagnostics)
    } else if what.as_str() == "test" {
        let tested = match toolchain::read_tested(take polled) {
            Ok(t) => take t,
            Err(e) => return failed(e),
        }
        print(f"tested passed={tested.passed}")
        for r in tested.results {
            print(f"test {r.name.as_str()} passed={r.passed} at={r.at.as_str()} printed={r.printed.len()}")
            if !r.passed {
                print(f"message {r.message.as_str()}")
            }
        }
    } else if what.as_str() == "doc" || what.as_str() == "doc-timed" {
        let pages = match toolchain::read_exported(take polled) {
            Ok(n) => n,
            Err(e) => return failed(e),
        }
        print(f"exported {pages}")
    } else if what.as_str() == "check" {
        let checked = match toolchain::read_checked(take polled) {
            Ok(c) => take c,
            Err(e) => return failed(e),
        }
        print(f"checked ok={checked.ok}")
        diagnostics(checked.diagnostics)
    } else {
        let formatted = match toolchain::read_formatted(take polled) {
            Ok(f) => take f,
            Err(e) => return failed(e),
        }
        for c in formatted.changed {
            print(f"changed {c.as_str()}")
        }
        for f in formatted.failed {
            print(f"failed {f.path.as_str()}")
        }
    }
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

/// A program with a computed constant (so a build keeps one in its cache), `frame`, and a test
/// that passes and one that fails.
const SUBJECT: &str = r#"
use std::io::print

fn sum_to(n: u32) -> u32 {
    var s = 0
    for i in 0..n {
        s += i
    }
    s
}

const SUM: u32 = sum_to(1000)

pub fn frame(time: f32, width: u32, height: u32) {
    print(f"{SUM}")
}

@test
fn passes() {
    assert(SUM == 499500)
}

@test
fn fails() {
    print("about to fail")
    assert(SUM == 1, "on purpose")
}
"#;

/// The tool's package and a subject package beside it in a scratch directory `name`, and the
/// subject's own files: `src/subject` in the read root `src`, and the write root `out`.
fn packages(name: &str, subject: &[(&str, &str)]) -> (PathBuf, PathBuf, PathBuf) {
    let dir = package(name, &[("tool/wrela.toml", &manifest("probe")), ("tool/main.wrela", TOOL)]);
    for (path, text) in subject {
        let file = dir.join("src").join(path);
        std::fs::create_dir_all(file.parent().expect("a directory")).expect("mkdir");
        std::fs::write(file, text).expect("write");
    }
    std::fs::create_dir_all(dir.join("out")).expect("out");
    (dir.join("tool"), dir.join("src"), dir.join("out"))
}

/// `wrela tool` on `tool`, `src` read-only and `out` writable, with `args`.
fn probe(tool: &Path, src: &Path, out: &Path, args: &[&str]) -> Command {
    let mut c = wrela();
    c.arg("tool").arg(tool);
    c.arg("--read").arg(format!("src={}", src.display()));
    c.arg("--write").arg(format!("out={}", out.display()));
    c.arg("--timeout").arg("120").arg("--").args(args);
    c
}

/// Runs `command`: its exit code and standard output.
fn run(command: &mut Command) -> (Option<i32>, String) {
    let out = command.output().expect("run wrela tool");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    (out.status.code(), format!("{stdout}{stderr}"))
}

/// The files under `dir`, by their paths in it, with their bytes.
fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("read_dir").map(|e| e.expect("entry")) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(dir).expect("inside").display().to_string();
                out.push((rel, std::fs::read(&p).expect("read")));
            }
        }
    }
    out.sort();
    out
}

/// The names in `dir`.
fn names(dir: &Path) -> Vec<String> {
    let mut n: Vec<String> = std::fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    n.sort();
    n
}

#[test]
fn a_build_through_the_toolchain_is_wrela_builds_byte_for_byte_and_writes_nothing_beside_it() {
    let subject =
        [("subject/wrela.toml", manifest("subject")), ("subject/main.wrela", SUBJECT.into())];
    let subject: Vec<(&str, &str)> = subject.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let (tool, src, out) = packages("toolchain-build", &subject);
    for into in ["out/a", "out/b"] {
        let (code, printed) = run(&mut probe(&tool, &src, &out, &["build", "src/subject", into]));
        assert_eq!(code, Some(0), "{printed}");
        assert!(printed.contains("built ok=true"), "{printed}");
    }
    // Nothing beside the read-only package, or the tool's: their constants are cached elsewhere.
    assert_eq!(names(&src.join("subject")), ["main.wrela", "wrela.toml"]);
    assert_eq!(names(&tool), ["main.wrela", "wrela.toml"], "`wrela tool` keeps no cache there");
    let (a, b) = (files(&out.join("a")), files(&out.join("b")));
    assert!(
        a.iter().any(|(f, _)| f == "game.wasm"),
        "{:?}",
        a.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    assert_eq!(a, b, "two builds through the service are the same bytes");
    let reference = out.join("reference");
    let built = wrela()
        .arg("build")
        .arg(src.join("subject"))
        .arg("-o")
        .arg(&reference)
        .output()
        .expect("wrela build");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    assert_eq!(a, files(&reference), "the service's build is `wrela build`'s");
}

#[test]
fn a_dependency_outside_the_read_roots_is_refused() {
    let main = "use helper::value\n\npub fn frame(time: f32, width: u32, height: u32) {}\n";
    let (tool, src, out) = packages(
        "toolchain-outside",
        &[
            (
                "subject/wrela.toml",
                "[package]\nname = \"subject\"\n\n[dependencies]\nhelper = { path = \"../../helper\" }\n",
            ),
            ("subject/main.wrela", main),
        ],
    );
    // The dependency is there, beside the root, but outside it.
    let helper = src.parent().expect("the scratch directory").join("helper");
    std::fs::create_dir_all(&helper).expect("helper");
    std::fs::write(helper.join("wrela.toml"), manifest("helper")).expect("manifest");
    std::fs::write(helper.join("value.wrela"), "pub fn value() -> u32 {\n    1\n}\n")
        .expect("source");
    let (code, printed) = run(&mut probe(&tool, &src, &out, &["build", "src/subject", "out/b"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("built ok=false"), "{printed}");
    assert!(
        printed.contains("diagnostic E0220 error=true") && printed.contains("outside the roots"),
        "{printed}"
    );
    assert!(!out.join("b/game.wasm").exists(), "a failed build writes nothing");
    // A path outside the roots fails the request itself.
    let (code, printed) = run(&mut probe(&tool, &src, &out, &["check", "elsewhere/subject"]));
    assert_eq!(code, Some(1), "{printed}");
    assert!(printed.contains("error: ") && printed.contains("no root `elsewhere`"), "{printed}");
}

#[test]
fn test_and_check_answer_structured_results() {
    let subject =
        [("subject/wrela.toml", manifest("subject")), ("subject/main.wrela", SUBJECT.into())];
    let subject: Vec<(&str, &str)> = subject.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let (tool, src, out) = packages("toolchain-test", &subject);
    let (code, printed) = run(&mut probe(&tool, &src, &out, &["test", "src/subject"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("tested passed=false"), "{printed}");
    assert!(printed.contains("test passes passed=true at=main.wrela:"), "{printed}");
    assert!(
        printed.contains("test fails passed=false at=main.wrela:") && printed.contains("printed=1"),
        "{printed}"
    );
    assert!(printed.contains("message ") && printed.contains("on purpose"), "{printed}");
    // A type error: its code, where it is, and its message.
    std::fs::write(
        src.join("subject/main.wrela"),
        "pub fn frame(time: f32, width: u32, height: u32) {\n    let x: u32 = 1.5\n}\n",
    )
    .expect("write");
    let (code, printed) = run(&mut probe(&tool, &src, &out, &["check", "src/subject"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("checked ok=false"), "{printed}");
    assert!(printed.contains("error=true at=main.wrela:2:"), "{printed}");
    assert_eq!(names(&src.join("subject")), ["main.wrela", "wrela.toml"], "nothing beside it");
}

#[test]
fn fmt_in_check_mode_reports_without_writing() {
    let messy = "pub fn frame(time: f32, width: u32, height: u32) {\nlet x =   1\n}\n";
    let (tool, _, out) = packages("toolchain-fmt", &[]);
    std::fs::create_dir_all(out.join("pkg")).expect("pkg");
    std::fs::write(out.join("pkg/main.wrela"), messy).expect("write");
    std::fs::write(out.join("pkg/wrela.toml"), manifest("pkg")).expect("write");
    let src = out.parent().expect("scratch").join("src");
    std::fs::create_dir_all(&src).expect("src");
    let (code, printed) = run(&mut probe(&tool, &src, &out, &["fmt-check", "out/pkg"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("changed out/pkg/main.wrela"), "{printed}");
    assert_eq!(std::fs::read_to_string(out.join("pkg/main.wrela")).expect("read"), messy);
    let (code, printed) = run(&mut probe(&tool, &src, &out, &["fmt", "out/pkg"]));
    assert_eq!(code, Some(0), "{printed}");
    let formatted = std::fs::read_to_string(out.join("pkg/main.wrela")).expect("read");
    assert!(formatted.contains("    let x = 1\n"), "{formatted}");
}

/// Whether the process `pid` is still running, after waiting up to 2 s for it to go.
fn alive(pid: &str) -> bool {
    let started = Instant::now();
    loop {
        let gone = !Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("kill")
            .success();
        if gone {
            return false;
        }
        if started.elapsed() > Duration::from_secs(2) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
#[test]
fn a_deadline_or_the_tools_end_kills_wasm_opt_and_ends_in_bounded_time() {
    let subject =
        [("subject/wrela.toml", manifest("subject")), ("subject/main.wrela", SUBJECT.into())];
    let subject: Vec<(&str, &str)> = subject.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let (tool, src, out) = packages("toolchain-optimize", &subject);
    // A `wasm-opt` that says it's new enough, records its process, and never ends.
    let fake = out.parent().expect("scratch").join("wasm-opt");
    let pids = out.parent().expect("scratch").join("pids");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'wasm-opt version 200'; exit 0; fi\necho $$ >> {}\nexec sleep 1000\n",
            pids.display()
        ),
    )
    .expect("fake");
    let mut perms = std::fs::metadata(&fake).expect("fake").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&fake, perms).expect("chmod");
    // The request's deadline, 2 s.
    let started = Instant::now();
    let (code, printed) =
        run(probe(&tool, &src, &out, &["build-optimized", "src/subject", "out/o"])
            .env("WRELA_WASM_OPT", &fake));
    assert_eq!(code, Some(1), "{printed}");
    assert!(printed.contains("past its deadline of 2 s"), "{printed}");
    assert!(started.elapsed() < Duration::from_secs(30), "{:?}", started.elapsed());
    let pid = std::fs::read_to_string(&pids).expect("wasm-opt ran");
    assert!(!alive(pid.trim()), "wasm-opt's group is killed at the deadline");
    // The tool's end, with `wasm-opt` running: its timeout ends both.
    std::fs::remove_file(&pids).expect("pids");
    let started = Instant::now();
    let mut command = wrela();
    command.arg("tool").arg(&tool);
    command.arg("--read").arg(format!("src={}", src.display()));
    command.arg("--write").arg(format!("out={}", out.display()));
    command.args(["--timeout", "1.5", "--", "build-optimized", "src/subject", "out/p"]);
    let (code, printed) = run(command.env("WRELA_WASM_OPT", &fake));
    assert_eq!(code, Some(124), "{printed}");
    assert!(started.elapsed() < Duration::from_secs(30), "{:?}", started.elapsed());
    let pid = std::fs::read_to_string(&pids).expect("wasm-opt ran");
    assert!(!alive(pid.trim()), "the tool's end kills wasm-opt's group");
}

/// A root granted as the package itself (`--read pkg=DIR`): its directory is outside it, and
/// it's still a package to build, check and test, and a directory to format.
#[test]
fn a_granted_root_is_itself_a_package_and_a_directory_to_format() {
    let messy = "pub fn helper() -> u32 {\nreturn   1\n}\n";
    let subject = [
        ("subject/wrela.toml", manifest("subject")),
        ("subject/main.wrela", SUBJECT.into()),
        ("subject/extra.wrela", messy.into()),
    ];
    let subject: Vec<(&str, &str)> = subject.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let (tool, src, out) = packages("toolchain-root", &subject);
    let probe_pkg = |args: &[&str]| {
        let mut c = wrela();
        c.arg("tool").arg(&tool);
        c.arg("--read").arg(format!("pkg={}", src.join("subject").display()));
        c.arg("--write").arg(format!("out={}", out.display()));
        c.arg("--timeout").arg("120").arg("--").args(args);
        run(&mut c)
    };
    let (code, printed) = probe_pkg(&["build", "pkg", "out/b"]);
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("built ok=true"), "{printed}");
    let (code, printed) = probe_pkg(&["check", "pkg"]);
    assert!(code == Some(0) && printed.contains("checked ok=true"), "{printed}");
    let (code, printed) = probe_pkg(&["test", "pkg"]);
    assert!(code == Some(0) && printed.contains("test passes passed=true"), "{printed}");
    let (code, printed) = probe_pkg(&["fmt-check", "pkg"]);
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("changed pkg/extra.wrela"), "{printed}");
    // A directory with nothing to format is an error, not a pass.
    std::fs::create_dir_all(out.join("empty")).expect("empty");
    let (code, printed) = probe_pkg(&["fmt-check", "out/empty"]);
    assert_eq!(code, Some(1), "{printed}");
    assert!(printed.contains("holds no `.wrela` file"), "{printed}");
}

/// The builds' outputs follow `wrela build`'s rules, and a request that doesn't finish leaves
/// none of its own: cancelled while it writes, its files go, so a retry finds nothing in its way;
/// a list that isn't a file stops it before anything is written; a link at an output's path
/// stops it, and a list naming a file no build writes doesn't remove it.
#[cfg(unix)]
#[test]
fn a_builds_outputs_follow_wrela_builds_rules_and_a_failed_one_leaves_none() {
    let subject =
        [("subject/wrela.toml", manifest("subject")), ("subject/main.wrela", SUBJECT.into())];
    let subject: Vec<(&str, &str)> = subject.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let (tool, src, out) = packages("toolchain-outputs", &subject);
    let build =
        |into: &str, mode: &str| run(&mut probe(&tool, &src, &out, &[mode, "src/subject", into]));
    // Cancelled at its 2 s deadline while it writes (each file takes 0.5 s).
    let (code, printed) = run(probe(&tool, &src, &out, &["build-timed", "src/subject", "out/a"])
        .env("WRELA_TESTING_IO_DELAY", "0.5"));
    assert_eq!(code, Some(1), "{printed}");
    assert!(printed.contains("past its deadline of 2 s"), "{printed}");
    assert!(!there(&out.join("a")), "nothing it wrote, or made, is left");
    let (code, printed) = build("out/a", "build");
    assert!(code == Some(0) && printed.contains("built ok=true"), "a retry builds: {printed}");
    // A list that is a directory: nothing is written, and once it's gone a retry builds.
    std::fs::create_dir_all(out.join("b/.wrela-build")).expect("a directory");
    let (code, printed) = build("out/b", "build");
    assert_eq!(code, Some(1), "{printed}");
    assert!(printed.contains("isn't a regular file"), "{printed}");
    assert_eq!(names(&out.join("b")), [".wrela-build"], "nothing written");
    std::fs::remove_dir(out.join("b/.wrela-build")).expect("remove");
    let (code, printed) = build("out/b", "build");
    assert!(code == Some(0) && printed.contains("built ok=true"), "{printed}");
    // game.wasm replaced by a link to the user's file: the build stops, and the file is kept.
    std::fs::write(out.join("user.txt"), "mine").expect("the user's file");
    std::fs::remove_file(out.join("b/game.wasm")).expect("remove");
    std::os::unix::fs::symlink("../user.txt", out.join("b/game.wasm")).expect("a link inside");
    let (code, printed) = build("out/b", "build");
    assert_eq!(code, Some(1), "{printed}");
    assert!(printed.contains("game.wasm` is a symbolic link"), "{printed}");
    assert_eq!(std::fs::read_to_string(out.join("user.txt")).expect("kept"), "mine");
    // A forged list naming the user's files: they aren't a build's, so they're kept.
    std::fs::remove_file(out.join("b/game.wasm")).expect("remove the link");
    std::fs::write(out.join("b/notes.txt"), "mine").expect("the user's notes");
    let list = std::fs::read_to_string(out.join("b/.wrela-build")).expect("the list");
    std::fs::write(out.join("b/.wrela-build"), format!("{list}notes.txt\n../user.txt\n"))
        .expect("forge");
    let (code, printed) = build("out/b", "build");
    assert!(code == Some(0) && printed.contains("built ok=true"), "{printed}");
    assert_eq!(std::fs::read_to_string(out.join("b/notes.txt")).expect("kept"), "mine");
    assert_eq!(std::fs::read_to_string(out.join("user.txt")).expect("kept"), "mine");
}

/// Whether anything is at `path`.
fn there(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// A request that doesn't finish leaves none of its outputs, files or directories, so a retry
/// into the same place needs no cleaning: a doc export cancelled after its first nested page;
/// a write that fails part way, of a build and of a doc export; and a build cancelled while
/// its last write is held, which finds it cancelled once the write is done.
#[cfg(unix)]
#[test]
fn a_request_that_doesnt_finish_leaves_none_of_its_outputs() {
    let subject =
        [("subject/wrela.toml", manifest("subject")), ("subject/main.wrela", SUBJECT.into())];
    let subject: Vec<(&str, &str)> = subject.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let (tool, src, out) = packages("toolchain-rollback", &subject);
    let ask = |args: &[&str], env: &[(&str, &str)]| {
        let mut c = probe(&tool, &src, &out, args);
        for (k, v) in env {
            c.env(k, v);
        }
        run(&mut c)
    };
    // A doc export cancelled at its 2 s deadline while its second page is held.
    let (code, printed) =
        ask(&["doc-timed", "src/subject", "out/docs/d"], &[("WRELA_TESTING_HOLD", "#2:3")]);
    assert_eq!(code, Some(1), "{printed}");
    assert!(printed.contains("past its deadline"), "{printed}");
    assert!(!there(&out.join("docs")), "its pages and the directories it made are gone");
    let (code, printed) = ask(&["doc", "src/subject", "out/docs/d"], &[]);
    assert!(code == Some(0) && printed.contains("exported"), "a retry: {printed}");
    // Writes that fail part way.
    let (code, printed) =
        ask(&["build", "src/subject", "out/b"], &[("WRELA_TESTING_WRITE_FAIL", "game.wasm")]);
    assert_eq!(code, Some(1), "{printed}");
    assert!(printed.contains("failed part way"), "{printed}");
    assert!(!there(&out.join("b")), "no partial game.wasm, and no directory");
    let (code, printed) = ask(&["build", "src/subject", "out/b"], &[]);
    assert!(code == Some(0) && printed.contains("built ok=true"), "a retry: {printed}");
    let (code, printed) =
        ask(&["doc", "src/subject", "out/docs/e"], &[("WRELA_TESTING_WRITE_FAIL", "#3")]);
    assert_eq!(code, Some(1), "{printed}");
    assert!(!there(&out.join("docs/e")), "no partial page");
    let (code, printed) = ask(&["doc", "src/subject", "out/docs/e"], &[]);
    assert!(code == Some(0) && printed.contains("exported"), "a retry: {printed}");
    // Cancelled at its deadline while its last write (the list) is held, and let go within the
    // grace: the request fails, and every output it made goes.
    let (code, printed) =
        ask(&["build-timed", "src/subject", "out/c"], &[("WRELA_TESTING_HOLD", ".wrela-build:3")]);
    assert_eq!(code, Some(1), "{printed}");
    assert!(printed.contains("past its deadline of 2 s"), "{printed}");
    assert!(!there(&out.join("c")), "nothing it made is left");
}
