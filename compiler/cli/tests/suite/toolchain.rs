//! `std::tool::toolchain` (language.md §15.5): a tool builds, tests, checks and formats packages
//! in its roots by the compiler's own code, reading only through its grants, keeping its
//! constants' cache outside them, and stopping (`wasm-opt` too) at its deadline or its end.

use crate::common::{manifest, package, wrela};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// A tool that does what its arguments say, through the toolchain, and prints what it got:
/// `build PKG OUT`, `build-optimized PKG OUT` (with a 2 s deadline), `build-testing PKG OUT` (a
/// test build, with the `@testing` exports), `test PKG`, `check PKG`,
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
    let building = what.as_str() == "build" || what.as_str() == "build-optimized" || what.as_str() == "build-timed" || what.as_str() == "build-testing"
    var p = if building {
        var options = toolchain::options()
        options.testing = what.as_str() == "build-testing"
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
    } else if what.as_str() == "test-with" || what.as_str() == "check-with" {
        // `test-with PKG NAME DIR`: the dependency `NAME` read from `DIR`.
        var overrides: Vec<toolchain::Override> = Vec::new()
        overrides.push(toolchain::Override { name: given[2].clone(), dir: given[3].clone() })
        if what.as_str() == "test-with" {
            toolchain::test_with(given[1].as_str(), "", overrides)
        } else {
            toolchain::check_with(given[1].as_str(), overrides)
        }
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
    if building {
        let built = match toolchain::read_built(take polled) {
            Ok(b) => take b,
            Err(e) => return failed(e),
        }
        print(f"built ok={built.ok} files={built.files.len()}")
        diagnostics(built.diagnostics)
    } else if what.as_str() == "test" || what.as_str() == "test-with" {
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
    } else if what.as_str() == "check" || what.as_str() == "check-with" {
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

/// A test build through the toolchain (`testing`) is `wrela build --testing`'s, byte for byte,
/// with the program's `@testing` export, which a normal build leaves out.
#[test]
fn a_test_build_through_the_toolchain_keeps_the_testing_exports() {
    let main = format!("{SUBJECT}\n@testing\npub fn probe() -> u32 {{\n    SUM\n}}\n");
    let subject = [("subject/wrela.toml", manifest("subject")), ("subject/main.wrela", main)];
    let subject: Vec<(&str, &str)> = subject.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let (tool, src, out) = packages("toolchain-testing", &subject);
    for (mode, into) in [("build-testing", "out/testing"), ("build", "out/normal")] {
        let (code, printed) = run(&mut probe(&tool, &src, &out, &[mode, "src/subject", into]));
        assert_eq!(code, Some(0), "{printed}");
        assert!(printed.contains("built ok=true"), "{printed}");
    }
    let reference = out.join("reference");
    let built = wrela()
        .arg("build")
        .arg(src.join("subject"))
        .arg("--testing")
        .arg("-o")
        .arg(&reference)
        .output()
        .expect("wrela build --testing");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let testing = files(&out.join("testing"));
    assert_eq!(testing, files(&reference), "the service's test build is `wrela build --testing`'s");
    let exports = |dir: &Path| {
        let wasm = std::fs::read(dir.join("game.wasm")).expect("game.wasm");
        wasm.windows(5).any(|w| w == b"probe")
    };
    assert!(exports(&out.join("testing")), "the test build exports `probe`");
    assert!(!exports(&out.join("normal")), "a normal build doesn't");
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

/// A test or a check reads a dependency from the package an override names, wherever its
/// manifest declares it: here a workspace member with no workspace, which fails without it.
#[test]
fn test_and_check_read_dependencies_through_overrides() {
    let main = "use geo::value::value\n\npub fn frame(time: f32, width: u32, height: u32) {}\n\n@test\nfn reads_its_dependency() {\n    assert(value() == 1)\n}\n";
    let manifest_text =
        "[package]\nname = \"subject\"\n\n[dependencies]\ngeo = { workspace = true }\n";
    let (tool, src, out) = packages(
        "toolchain-overrides",
        &[
            ("subject/wrela.toml", manifest_text),
            ("subject/main.wrela", main),
            ("geo/wrela.toml", "[package]\nname = \"geo\"\n"),
            ("geo/value.wrela", "pub fn value() -> u32 {\n    1\n}\n"),
        ],
    );
    let (code, printed) =
        run(&mut probe(&tool, &src, &out, &["test-with", "src/subject", "geo", "src/geo"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("tested passed=true"), "{printed}");
    assert!(printed.contains("test reads_its_dependency passed=true"), "{printed}");
    let (code, printed) =
        run(&mut probe(&tool, &src, &out, &["check-with", "src/subject", "geo", "src/geo"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("checked ok=true"), "{printed}");
    let (code, printed) = run(&mut probe(&tool, &src, &out, &["check", "src/subject"]));
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("checked ok=false") && printed.contains("E0220"), "{printed}");
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

/// A tool for a lifted build's literals, through the toolchain, printing what it got:
/// `lift PKG OUT NAME` (a build lifting package `NAME`), `literals PROG BUILD`, `edit PROG BUILD
/// LITERAL VALUE` and `query PKG QUERY NAME DIR` (`NAME` read from `DIR`). A request that fails
/// prints `error: ...` and exits 1.
const LIFT_TOOL: &str = r#"
use std::io::print
use std::job::{Job, Step}
use std::tool::{RUNNING, args}
use std::tool::toolchain
use std::tool::toolchain::Override

@job
fn main(given: take Vec<String>) -> u32 {
    let what = given[0].clone()
    var p = if what.as_str() == "lift" {
        var options = toolchain::options()
        options.lift.push(given[3].clone())
        toolchain::build(given[1].as_str(), given[2].as_str(), options)
    } else if what.as_str() == "literals" {
        toolchain::literals(given[1].as_str(), given[2].as_str())
    } else if what.as_str() == "edit" || what.as_str() == "edit-timed" {
        let Some(i) = given[3].as_str().parse_u32() else { return 2 }
        let Some(v) = given[4].as_str().parse_f32() else { return 2 }
        let timeout = if what.as_str() == "edit-timed" { 1.0 } else { 0.0 }
        toolchain::edit(given[1].as_str(), given[2].as_str(), [(i, v)], timeout)
    } else {
        var queries: Vec<String> = Vec::new()
        queries.push(given[2].clone())
        toolchain::query(given[1].as_str(), queries, [Override { name: given[3].clone(), dir: given[4].clone() }])
    }
    var got = p.poll()
    while got.is_none() {
        yield
        got = p.poll()
    }
    let Some(polled) = take got else { return 90 }
    if what.as_str() == "lift" {
        match toolchain::read_built(take polled) {
            Ok(b) => print(f"built ok={b.ok}"),
            Err(e) => {
                print(f"error: {e.message()}")
                return 1
            },
        }
    } else if what.as_str() == "literals" {
        match toolchain::read_literals(take polled) {
            Ok(l) => {
                for f in l.files {
                    print(f"file {f.as_str()}")
                }
                for x in l.built {
                    print(f"built {x}")
                }
                match l.values {
                    Some(v) => {
                        for x in v {
                            print(f"value {x}")
                        }
                    },
                    None => print("values none"),
                }
            },
            Err(e) => {
                print(f"error: {e.message()}")
                return 1
            },
        }
    } else {
        let answer = if what.as_str() != "query" {
            toolchain::read_edited(take polled)
        } else {
            toolchain::read_queried(take polled)
        }
        match answer {
            Ok(text) => print(text.as_str()),
            Err(e) => {
                print(f"error: {e.message()}")
                return 1
            },
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

/// A lifted program and the package it lifts, in the write root `work`; the tool beside them.
fn lifted_packages(name: &str) -> (PathBuf, PathBuf) {
    let dir = package(
        name,
        &[
            ("tool/wrela.toml", &manifest("lift_probe")),
            ("tool/main.wrela", LIFT_TOOL),
            (
                "work/subject/wrela.toml",
                "[package]\nname = \"subject\"\n\n[dependencies]\nshape = { path = \"../shape\" }\n",
            ),
            ("work/subject/main.wrela", "pub fn frame(time: f32, width: u32, height: u32) {}\n"),
            (
                "work/subject/probe.wrela",
                "fn at(p: vec3) {\n    let c = shape::ball::tint(p.x)\n}\n",
            ),
            ("work/shape/wrela.toml", "[package]\nname = \"shape\"\n"),
            (
                "work/shape/ball.wrela",
                "pub struct Tint: Copy {\n    pub r: f32,\n    g: f32,\n}\n\npub fn tint(r: f32) -> Tint {\n    Tint { r, g: 1.0 }\n}\n\npub fn radius() -> f32 {\n    0.5 + 0.25\n}\n",
            ),
        ],
    );
    (dir.join("tool"), dir.join("work"))
}

/// `wrela tool` on `tool`, with `work` its write root, and `args`.
fn lift_probe(tool: &Path, work: &Path, args: &[&str]) -> (Option<i32>, String) {
    lift_probe_with(tool, work, args, &[])
}

/// [`lift_probe`], with the environment's `env` too.
fn lift_probe_with(
    tool: &Path,
    work: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> (Option<i32>, String) {
    let mut c = wrela();
    c.envs(env.iter().copied());
    c.arg("tool").arg(tool).arg("--write").arg(format!("work={}", work.display()));
    c.arg("--timeout").arg("120").arg("--").args(args);
    run(&mut c)
}

#[test]
fn a_lifted_builds_literals_are_read_and_written_through_the_grants() {
    let (tool, work) = lifted_packages("toolchain-lifted");
    let (code, printed) =
        lift_probe(&tool, &work, &["lift", "work/subject", "work/build", "shape"]);
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("built ok=true"), "{printed}");
    let literals =
        |work: &Path| lift_probe(&tool, work, &["literals", "work/subject", "work/build"]);
    let (code, printed) = literals(&work);
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("file work/shape/ball.wrela"), "{printed}");
    assert!(printed.contains("value 1\nvalue 0.5\nvalue 0.25"), "{printed}");
    // Another tool's edit of a literal: its new value, with no build.
    let ball = work.join("shape/ball.wrela");
    let text = std::fs::read_to_string(&ball).expect("ball");
    std::fs::write(&ball, text.replace("0.25", "0.125")).expect("edit");
    let (_, printed) = literals(&work);
    assert!(printed.contains("value 1\nvalue 0.5\nvalue 0.125"), "{printed}");
    assert!(printed.contains("built 1\nbuilt 0.5\nbuilt 0.25"), "as the build has them: {printed}");
    // The tool's edit, by the literal's index, written in the literal's own style.
    let (code, printed) =
        lift_probe(&tool, &work, &["edit", "work/subject", "work/build", "1", "0.75"]);
    assert_eq!(code, Some(0), "{printed}");
    assert!(printed.contains("\"written\":true"), "{printed}");
    assert!(std::fs::read_to_string(&ball).expect("ball").contains("0.75 + 0.125"));
    // A change past the literals: no values, and no edit.
    std::fs::write(&ball, text.replace("0.5 + 0.25", "0.5 * 0.25")).expect("edit");
    let (_, printed) = literals(&work);
    assert!(printed.contains("values none"), "{printed}");
    let (_, printed) =
        lift_probe(&tool, &work, &["edit", "work/subject", "work/build", "1", "0.6"]);
    assert!(printed.contains("\"written\":false") && printed.contains("rebuild"), "{printed}");
    assert!(std::fs::read_to_string(&ball).expect("ball").contains("0.5 * 0.25"));
    // A lifted file the build named that is now a symbolic link is neither read nor written:
    // the link, and the file it names, stay as they are.
    std::fs::write(&ball, &text).expect("restore");
    let elsewhere = work.join("elsewhere.wrela");
    std::fs::write(&elsewhere, &text).expect("a file to link to");
    std::fs::remove_file(&ball).expect("remove");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&elsewhere, &ball).expect("link");
    let (code, printed) =
        lift_probe(&tool, &work, &["edit", "work/subject", "work/build", "1", "0.6"]);
    assert!(code != Some(0) || printed.contains("\"written\":false"), "{printed}");
    assert_eq!(std::fs::read_to_string(&elsewhere).expect("elsewhere"), text);
    assert!(std::fs::symlink_metadata(&ball).expect("link").file_type().is_symlink());
    // A package in a read root is read, and not written.
    let mut c = wrela();
    c.arg("tool").arg(&tool).arg("--read").arg(format!("work={}", work.display()));
    c.arg("--timeout").arg("120").args(["--", "edit", "work/subject", "work/build", "1", "0.6"]);
    let (_, printed) = run(&mut c);
    assert!(printed.contains("write root") || printed.contains("\"written\":false"), "{printed}");
}

#[test]
fn a_query_reads_a_values_public_fields_through_the_overrides() {
    let (tool, work) = lifted_packages("toolchain-query");
    // The subject's dependency is named elsewhere: the override reads it from `shape`.
    let manifest = work.join("subject/wrela.toml");
    std::fs::write(
        &manifest,
        "[package]\nname = \"subject\"\n\n[dependencies]\nshape = { path = \"../nowhere\" }\n",
    )
    .expect("manifest");
    let (code, printed) = lift_probe(
        &tool,
        &work,
        &["query", "work/subject", "fields probe.wrela:2:9", "shape", "work/shape"],
    );
    assert_eq!(code, Some(0), "{printed}");
    let answers: serde_json::Value = serde_json::from_str(printed.trim()).expect("JSON");
    let a = &answers[0];
    assert_eq!(a["path"], "shape::ball::Tint", "{a}");
    assert_eq!(a["fields"], serde_json::json!([{ "path": "r", "type": "f32" }]), "{a}");
}

/// An edit cancelled while its file waits to take the old one's place (held until the request
/// is cancelled, at its 1 s deadline) replaces nothing: the source is as it was, and no file of
/// the edit's is left beside it.
#[test]
fn a_cancelled_edit_leaves_its_source_as_it_was() {
    let (tool, work) = lifted_packages("toolchain-edit-cancelled");
    let (code, printed) =
        lift_probe(&tool, &work, &["lift", "work/subject", "work/build", "shape"]);
    assert_eq!(code, Some(0), "{printed}");
    let ball = work.join("shape/ball.wrela");
    let before = std::fs::read_to_string(&ball).expect("ball");
    let (code, printed) = lift_probe_with(
        &tool,
        &work,
        &["edit-timed", "work/subject", "work/build", "1", "0.75"],
        &[("WRELA_TESTING_HOLD", "ball.wrela:60")],
    );
    assert_eq!(code, Some(1), "{printed}");
    assert!(printed.contains("past its deadline"), "{printed}");
    assert_eq!(std::fs::read_to_string(&ball).expect("ball"), before, "the source is unchanged");
    assert_eq!(names(&work.join("shape")), ["ball.wrela", "wrela.toml"], "nothing left beside it");
    // Not held, the same edit is written.
    let (code, printed) =
        lift_probe(&tool, &work, &["edit-timed", "work/subject", "work/build", "1", "0.75"]);
    assert!(code == Some(0) && printed.contains("\"written\":true"), "{printed}");
}
