//! Tool tests (`@test(tool, ...)`, language.md §13.2): `wrela test` runs each in a `wrela`
//! process of its own, on the package's tool build. The fixture compiler/tests/tool_tests
//! passes (its scratch, fake clock, fake sites and fake program); here, the ways a tool test
//! fails, each within its budget, and the attribute's rules.

use crate::common::{json, manifest, package, wrela};

/// A package with a tool build, of `tests` (each a tool test or not).
fn tool_package(name: &str, tests: &str) -> std::path::PathBuf {
    let main = format!("pub fn tool() -> u32 {{\n    0\n}}\n{tests}");
    package(name, &[("wrela.toml", &manifest(&name.replace('-', "_"))), ("main.wrela", &main)])
}

/// Each test `wrela test --json` ran: its name, whether it passed, and its failure's message.
fn ran(dir: &std::path::Path) -> Vec<(String, bool, String)> {
    let out = json(wrela().arg("test").arg(dir).arg("--json"));
    let tests = out["tests"].as_array().unwrap_or_else(|| panic!("{out}"));
    tests
        .iter()
        .map(|t| {
            let message = t["failure"]["message"].as_str().unwrap_or("").to_string();
            (t["name"].as_str().expect("a name").to_string(), t["passed"] == true, message)
        })
        .collect()
}

#[test]
fn the_tool_tests_fixture_passes() {
    let dir = wrela_root().join("compiler/tests/tool_tests");
    let tests = ran(&dir);
    assert_eq!(tests.len(), 14, "{tests:?}");
    for (name, passed, message) in &tests {
        assert!(passed, "{name}: {message}");
    }
}

/// The repository's root.
fn wrela_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn a_tool_test_fails_on_a_panic_a_trap_or_its_budget_and_passes_otherwise() {
    let tests = r#"
use std::tool::files
use std::tool::time

@test(tool)
fn waits_an_hour_of_fake_time() {
    let _ = time::after(3600.0).wait()
}

@test(tool)
fn panics() {
    let _ = time::after(1.0).wait()
    assert(1 + 1 == 3, "on purpose")
}

@test(tool)
fn traps() {
    let listed = files::list("scratch").wait()
    // An overflow, with a count the build can't know.
    var n: u32 = 4294967295
    if listed.is_ok() {
        n += 1
    }
}

@test(tool, blobs: "1K")
fn holds_its_blob_budget() {
    // 2 KiB in a blob, past a budget of 1 KiB: the append fails, as a request past it does.
    let Ok(b) = std::tool::blob::new().wait() else { panic("a blob") }
    var bytes: Vec<u8> = Vec::new()
    for _ in 0..2048 {
        bytes.push(0)
    }
    assert(std::tool::blob::append(b, bytes).wait().is_ok(), "past the test's blob budget")
}

@test(tool, budget: 1)
fn spins_past_its_budget() {
    var n: u32 = 0
    loop {
        n = n.wrapping_add(1)
    }
}
"#;
    let dir = tool_package("tool-tests-ends", tests);
    let started = std::time::Instant::now();
    let tests = ran(&dir);
    // The spinning one is stopped at its budget of 1 s, not its process's bound.
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
    let get = |name: &str| tests.iter().find(|t| t.0 == name).expect(name).clone();
    assert!(get("waits_an_hour_of_fake_time").1, "{tests:?}");
    let (_, passed, message) = get("panics");
    assert!(!passed && message.contains("on purpose"), "{message}");
    let (_, passed, message) = get("traps");
    assert!(!passed && message.contains("trapped"), "{message}");
    let (_, passed, message) = get("holds_its_blob_budget");
    assert!(!passed && message.contains("past the test's blob budget"), "{message}");
    let (_, passed, message) = get("spins_past_its_budget");
    assert!(!passed && message.contains("past its budget of 1 s"), "{message}");
}

#[test]
fn a_tool_test_is_a_function_of_no_parameters_in_a_package_with_a_tool_build() {
    let check = |name: &str, main: &str| -> Vec<(String, String)> {
        let dir = package(
            name,
            &[("wrela.toml", &manifest(&name.replace('-', "_"))), ("main.wrela", main)],
        );
        let out = json(wrela().arg("check").arg(&dir).arg("--json"));
        out["diagnostics"]
            .as_array()
            .unwrap_or_else(|| panic!("{out}"))
            .iter()
            .filter(|d| d["severity"] == "error")
            .map(|d| {
                (
                    d["code"].as_str().unwrap_or("").to_string(),
                    d["message"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect()
    };
    let tool = "pub fn tool() -> u32 {\n    0\n}\n";
    let cases = [
        ("@test(tool)\nfn takes(n: take u32) {}\n", "E0222", "takes parameters"),
        ("@test(tool)\nfn gives() -> u32 {\n    0\n}\n", "E0222", "returns a value"),
        ("@test(tool, budget: 0)\nfn none() {}\n", "E0222", "`budget` is seconds"),
        ("@test(tool, net: [\"Game.Example\"])\nfn upper() {}\n", "E0222", "`net`"),
        ("@test(tool, run: [(\"x\", \"../out\")])\nfn out() {}\n", "E0222", "`run`"),
        ("@test(tool, frames: 3)\nfn frames() {}\n", "E0222", "takes `budget`"),
        ("@test(tool, blobs: \"8X\")\nfn blobs() {}\n", "E0222", "`blobs` is a blob budget"),
        ("@test(tool, workers: 9)\nfn many() {}\n", "E0222", "`workers` is how many"),
        ("@test(tool)\n@fuel(100)\nfn fuel() {}\n", "E0108", "`@fuel`"),
        // No test is a job: a tool test waits on its calls, and a test of code keeps a
        // constant's effects.
        ("@test(tool)\n@job\nfn a_job() {}\n", "E0336", "can't be a job"),
        ("@test\n@job\nfn plain() {}\n", "E0336", "can't be a job"),
        (
            "@test\nfn reads() {\n    let p = std::tool::time::utc_now()\n}\n",
            "E0600",
            "a test, which the build runs",
        ),
    ];
    for (i, (text, code, words)) in cases.iter().enumerate() {
        let errors = check(&format!("tool-tests-rules-{i}"), &format!("{tool}{text}"));
        assert!(errors.iter().any(|(c, m)| c == code && m.contains(words)), "{text}: {errors:?}");
    }
    // Without a tool build, there's nothing to run it on.
    let errors = check(
        "tool-tests-rules-no-tool",
        "pub fn frame(time: f32, width: u32, height: u32) {}\n@test(tool)\nfn t() {}\n",
    );
    assert!(errors.iter().any(|(c, m)| c == "E0222" && m.contains("no tool build")), "{errors:?}");
    // A good one checks clean.
    let errors = check(
        "tool-tests-rules-ok",
        &format!("{tool}@test(tool, budget: 2.5, net: [\"a.example\"])\nfn t() {{}}\n"),
    );
    assert!(errors.is_empty(), "{errors:?}");
}

/// A fake program that waits a minute on the system's clock, then exits.
const SLEEPER: &str = r#"
use std::tool::time::after

pub fn tool() -> u32 {
    let _ = after(60.0).wait()
    0
}
"#;

/// A package whose one tool test, of budget `budget`, runs the fake program `sleeper` and waits
/// for it: the package, and the sleeper's directory, which its process's command line names.
fn sleeper_package(name: &str, budget: u32) -> (std::path::PathBuf, String) {
    let main = format!(
        r#"pub fn tool() -> u32 {{
    0
}}

@test(tool, run: [("sleeper", "fakes/sleeper")], budget: {budget})
fn waits_for_its_program() {{
    let _ = std::tool::run::run(std::tool::run::command("sleeper", "scratch")).wait()
}}
"#
    );
    let dir = package(
        name,
        &[
            ("wrela.toml", &manifest(&name.replace('-', "_"))),
            ("main.wrela", &main),
            ("fakes/sleeper/wrela.toml", &manifest("sleeper")),
            ("fakes/sleeper/main.wrela", SLEEPER),
        ],
    );
    let sleeper = dir.join("fakes/sleeper").canonicalize().expect("the sleeper");
    (dir, sleeper.display().to_string())
}

/// The processes whose command lines have `marker`.
fn running(marker: &str) -> Vec<String> {
    let out = std::process::Command::new("pgrep").args(["-f", marker]).output().expect("pgrep");
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

/// Waits up to `bound` for `done`.
fn within(bound: std::time::Duration, mut done: impl FnMut() -> bool) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < bound {
        if done() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

/// Waits for `child` up to `bound`: its exit code, or `None` (and it's killed) past it.
fn exit_within(child: &mut std::process::Child, bound: std::time::Duration) -> Option<i32> {
    let mut code = None;
    let exited = within(bound, || match child.try_wait() {
        Ok(Some(status)) => {
            code = status.code();
            true
        }
        _ => false,
    });
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
    }
    code
}

#[test]
fn ctrl_c_ends_wrela_tests_tool_tests_and_every_program_they_started() {
    let (dir, sleeper) = sleeper_package("tool-tests-ctrl-c", 60);
    let mut child = wrela()
        .arg("test")
        .arg(&dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run wrela test");
    let started = within(std::time::Duration::from_secs(60), || !running(&sleeper).is_empty());
    assert!(started, "the fake program started");
    let sent = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill");
    assert!(sent.success());
    assert_eq!(exit_within(&mut child, std::time::Duration::from_secs(10)), Some(130));
    let gone = within(std::time::Duration::from_secs(3), || running(&sleeper).is_empty());
    assert!(gone, "the fake program, in a process group of its own, ended too");
}

#[test]
fn a_test_host_that_cant_end_its_programs_has_them_ended_by_wrela_test() {
    let (dir, sleeper) = sleeper_package("tool-tests-stopped", 3);
    let mut child = wrela()
        .arg("test")
        .arg(&dir)
        .arg("--json")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run wrela test");
    let started = within(std::time::Duration::from_secs(60), || !running(&sleeper).is_empty());
    assert!(started, "the fake program started");
    // The test's host is stopped, so it can't end its program at its budget.
    let host = running("tool --test").into_iter().find(|pid| {
        let parent = std::process::Command::new("ps").args(["-o", "ppid=", "-p", pid]).output();
        parent.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == child.id().to_string())
    });
    let host = host.expect("the test's host");
    let stopped = std::process::Command::new("kill").args(["-STOP", &host]).status().expect("kill");
    assert!(stopped.success());
    let bound = std::time::Duration::from_secs(3 + 10 + 20);
    assert_eq!(exit_within(&mut child, bound), Some(1), "the test failed");
    let mut out = String::new();
    std::io::Read::read_to_string(child.stdout.as_mut().expect("its output"), &mut out)
        .expect("read");
    assert!(out.contains("was killed"), "{out}");
    let gone = within(std::time::Duration::from_secs(3), || running(&sleeper).is_empty());
    assert!(gone, "the fake program, in a process group of its own, ended too");
}

#[test]
fn a_program_started_but_not_yet_recorded_ends_with_its_tests_host() {
    let (dir, sleeper) = sleeper_package("tool-tests-unrecorded", 60);
    // The host pauses between starting its program and recording the program's group.
    let mut child = wrela()
        .arg("test")
        .arg(&dir)
        .env("WRELA_TESTING_SPAWN_PAUSE", "60")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run wrela test");
    let started = within(std::time::Duration::from_secs(60), || !running(&sleeper).is_empty());
    assert!(started, "the fake program started");
    let sent = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill");
    assert!(sent.success());
    assert_eq!(exit_within(&mut child, std::time::Duration::from_secs(10)), Some(130));
    let gone = within(std::time::Duration::from_secs(3), || running(&sleeper).is_empty());
    assert!(gone, "the unrecorded program was still in its host's group, and ended with it");
}

/// Tool tests with a browser (`browser: true`): one serves a page from its scratch, opens,
/// observes and closes it, and passes; one runs past its budget, and one traps, each with a
/// browser open, and each fails, leaving no Chrome and no profile; one without the grant has no
/// browser.
#[test]
#[ignore = "needs the runtime"]
fn a_tool_test_with_a_browser_passes_and_one_that_fails_leaves_nothing() {
    // Each test serves a page of its scratch and opens it: `opened` is what `open` gave.
    let open = r#"
    let wrote = files::write("scratch/page.html", utf8("<!doctype html><title>Guide</title>")).wait()
    assert(wrote.is_ok(), "the page")
    let Ok(started) = serve::start("scratch").wait() else { panic("no server") }
    let opened = browser::open(f"{started.origin.as_str()}/page.html".as_str()).wait()
"#;
    let tests = r#"
use std::tool::browser
use std::tool::files
use std::tool::serve
use std::tool::time

fn utf8(text: str) -> Vec<u8> {
    var out: Vec<u8> = Vec::new()
    for i in 0..text.len() {
        out.push(text.byte(i))
    }
    out
}

@test(tool, browser: true, budget: 60)
fn serves_opens_observes_and_closes() {
OPEN
    let Ok(b) = opened else { panic("no browser") }
    let Ok(title) = browser::title(b).wait() else { panic("no title") }
    assert(title.as_str() == "Guide", title.as_str())
    assert(browser::close(b).wait().is_ok(), "closed")
}

@test(tool, browser: true, budget: 3)
fn runs_past_its_budget_with_a_browser_open() {
OPEN
    assert(opened.is_ok(), "a browser")
    let _ = time::after(600.0).wait()
}

@test(tool, browser: true, budget: 60)
fn traps_with_a_browser_open() {
OPEN
    assert(opened.is_ok(), "a browser")
    panic("on purpose, with a browser open")
}

@test(tool)
fn has_no_browser_without_the_grant() {
OPEN
    let Err(why) = opened else { panic("a browser without the grant") }
    assert(why.message().find("isn't granted").is_some(), why.message())
}
"#
    .replace("OPEN\n", open.trim_start_matches('\n'));
    let dir = tool_package("tool-tests-browser", &tests);
    let tmp = dir.join("tmp");
    std::fs::create_dir_all(&tmp).expect("a temporary directory");
    let out = json(wrela().arg("test").arg(&dir).arg("--json").env("TMPDIR", &tmp));
    let tests: Vec<(String, bool, String)> = out["tests"]
        .as_array()
        .unwrap_or_else(|| panic!("{out}"))
        .iter()
        .map(|t| {
            let message = t["failure"]["message"].as_str().unwrap_or("").to_string();
            (t["name"].as_str().expect("a name").to_string(), t["passed"] == true, message)
        })
        .collect();
    let get = |name: &str| tests.iter().find(|t| t.0 == name).expect(name).clone();
    let (_, passed, message) = get("serves_opens_observes_and_closes");
    assert!(passed, "{message}");
    let (_, passed, message) = get("runs_past_its_budget_with_a_browser_open");
    assert!(!passed && message.contains("past its budget of 3 s"), "{message}");
    let (_, passed, message) = get("traps_with_a_browser_open");
    assert!(!passed && message.contains("on purpose"), "{message}");
    let (_, passed, message) = get("has_no_browser_without_the_grant");
    assert!(passed, "{message}");
    let left = walk(&tmp).into_iter().filter(|p| p.ends_with("chrome.log")).count();
    assert_eq!(left, 0, "no profile is left");
    let ps = std::process::Command::new("ps").args(["-Ao", "command"]).output().expect("ps");
    let named = String::from_utf8_lossy(&ps.stdout)
        .lines()
        .filter(|l| l.contains(&*tmp.to_string_lossy()))
        .count();
    assert_eq!(named, 0, "no Chrome is left");
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
