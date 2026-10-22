//! `wrela suite`: a workspace's checks in one command (language.md §13.2): its members' format,
//! its allowlist and its members' tests, of every kind, or exactly one of them, chosen by name as
//! `tools/check.sh` chooses; every check runs when another fails, and the status says if any did.

use crate::common::{package, run, wrela};
use std::path::Path;

/// A tool test's package: a tool build, and a tool test that waits on the fake clock.
const TOOLED: &str = r#"pub fn tool() -> u32 {
    0
}

@test(tool)
fn waits_a_minute_of_fake_time() {
    let _ = std::tool::time::after(60.0).wait()
}
"#;

/// A program with a frame test of its state.
const APP: &str = r#"pub struct Game {
    ticks: u32,
}

pub fn init() -> Game {
    Game { ticks: lib::sums::two() }
}

pub fn frame(game: mut Game, time: f32, width: u32, height: u32) {
    game.ticks += 1
}

@test(frames: 3)
fn counts_its_frames(game: Game) {
    assert(game.ticks == 5)
}
"#;

/// A library with tests of code.
const SUMS: &str = r#"pub fn two() -> u32 {
    1 + 1
}

@test
fn adds() {
    assert(two() == 2)
}

@test
fn adds_again() {
    assert(two() + two() == 4)
}
"#;

/// The workspace's manifest: `members`, then `rest`.
fn manifest(members: &[&str], rest: &str) -> String {
    let members: Vec<String> = members.iter().map(|m| format!("{m:?}")).collect();
    format!(
        "[workspace]\nmembers = [{}]\nallow = [\"**/*.wrela\", \"**/wrela.toml\", \"**/*.png\", \".gitignore\"]\n{rest}",
        members.join(", ")
    )
}

/// A workspace of a library, a program, a tool package and a copy of compiler/tests/gpu-frames
/// with only its passing tests: one test of each kind.
fn workspace(name: &str) -> std::path::PathBuf {
    let gpu = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/gpu-frames");
    let mut frames = std::fs::read_to_string(gpu.join("main.wrela")).expect("gpu-frames");
    for failing in [
        "@test(frames: 4, gpu: true)\nfn a_wrong_square_fails(s: State) {\n    assert(s.got[3] == 10)\n}\n",
        "@test(frames: 2, gpu: true, golden: \"goldens/black.png\")\nfn a_ramp_is_not_black(s: State) {}\n",
    ] {
        assert!(frames.contains(failing), "gpu-frames has `{failing}`");
        frames = frames.replace(failing, "");
    }
    let ws = package(
        name,
        &[
            ("wrela.toml", &manifest(&["lib", "app", "tooled", "gpu"], "")),
            // What tests write beside their packages (their constants) isn't the workspace's.
            (".gitignore", "**/build/\n"),
            ("lib/wrela.toml", "[package]\nname = \"lib\"\n"),
            ("lib/sums.wrela", SUMS),
            (
                "app/wrela.toml",
                "[package]\nname = \"app\"\n\n[dependencies]\nlib = { workspace = true }\n",
            ),
            ("app/main.wrela", APP),
            ("tooled/wrela.toml", "[package]\nname = \"tooled\"\n"),
            ("tooled/main.wrela", TOOLED),
            ("gpu/wrela.toml", "[package]\nname = \"gpu\"\n"),
            ("gpu/main.wrela", &frames),
        ],
    );
    std::fs::create_dir_all(ws.join("gpu/goldens")).expect("goldens");
    std::fs::copy(gpu.join("goldens/ramp.png"), ws.join("gpu/goldens/ramp.png")).expect("golden");
    // What the failing tests leave is formatted as the rest.
    crate::common::stdout(wrela().arg("fmt").arg(ws.join("gpu")));
    ws
}

/// Runs `wrela suite` with `args` in `dir`: its status and its standard output and error.
fn suite(dir: &Path, args: &[&str]) -> (Option<i32>, String) {
    run(wrela().arg("suite").args(args).current_dir(dir))
}

#[test]
fn the_suite_runs_every_check_and_every_kind_of_test() {
    let ws = workspace("suite-all");
    // From a member's directory: the suite's workspace is the one above it.
    let (status, out) = suite(&ws.join("app"), &[]);
    assert_eq!(status, Some(0), "{out}");
    for line in [
        "ok fmt in",
        "ok allow in",
        "ok wrela::lib::adds (sums.wrela:",
        "ok wrela::lib::adds_again (",
        "ok wrela::app::counts_its_frames (main.wrela:",
        "ok wrela::tooled::waits_a_minute_of_fake_time (",
        "ok wrela::gpu::squares_come_back (",
        "ok wrela::gpu::the_ramp_is_its_golden (",
        "ok wrela::lib:: in",
        "ok wrela::gpu:: in",
        "6 checks passed, with 6 tests",
        "all checks passed in",
    ] {
        assert!(out.contains(line), "`{line}` in:\n{out}");
    }
    assert!(!out.contains("FAILED"), "{out}");
}

#[test]
fn a_name_selects_exactly_one_check_group_or_test() {
    let ws = workspace("suite-select");
    let at = ws.to_str().expect("a path");
    let ok = |args: &[&str], want: &[&str], not: &[&str]| {
        let (status, out) = suite(&ws, args);
        assert_eq!(status, Some(0), "{args:?}: {out}");
        for w in want {
            assert!(out.contains(w), "{args:?}: `{w}` in:\n{out}");
        }
        for n in not {
            assert!(!out.contains(n), "{args:?}: no `{n}` in:\n{out}");
        }
    };
    ok(
        &["wrela::gpu::the_ramp_is_its_golden"],
        &["ok wrela::gpu::the_ramp_is_its_golden ("],
        &["squares_come_back", "ok fmt"],
    );
    ok(&["wrela::lib::adds"], &["ok wrela::lib::adds ("], &["adds_again"]);
    ok(
        &["wrela::tooled::"],
        &["ok wrela::tooled::waits_a_minute_of_fake_time", "1 checks passed, with 1 tests"],
        &["wrela::lib", "ok allow"],
    );
    ok(&["fmt", "--workspace", at], &["ok fmt in", "1 checks passed"], &["ok allow", "wrela::"]);
    ok(&["allow"], &["ok allow in"], &["ok fmt", "wrela::"]);
    // Malformed, unknown and ambiguous names are refused before anything runs (status 2).
    std::fs::write(ws.join("lib/more.wrela"), "@test\nfn adds() {\n    assert(1 + 1 == 2)\n}\n")
        .expect("write");
    for (args, says) in [
        (&[""][..], "one name, not empty"),
        (&["fmt", "allow"], "one name, not empty"),
        (&["rustfmt"], "unknown check `rustfmt`"),
        (&["wrela::lib"], "Wrela names are"),
        (&["wrela::missing::"], "no member at `missing`"),
        (&["wrela::lib::missing"], "has 0 matches"),
        (&["wrela::lib::adds"], "has 2 matches"),
        (&["wrela::lib::not-a-name"], "isn't a test's name"),
        (&["--fresh"], "unknown option"),
    ] {
        let (status, out) = suite(&ws, args);
        assert_eq!(status, Some(2), "{args:?}: {out}");
        assert!(out.contains(says), "{args:?}: `{says}` in:\n{out}");
        assert!(!out.contains("ok wrela::"), "{args:?} ran nothing: {out}");
    }
    // A group with no tests selects nothing, which fails.
    std::fs::create_dir_all(ws.join("empty")).expect("mkdir");
    std::fs::write(ws.join("empty/wrela.toml"), "[package]\nname = \"empty\"\n").expect("write");
    std::fs::write(ws.join("empty/none.wrela"), "pub fn n() -> u32 {\n    0\n}\n").expect("write");
    std::fs::write(ws.join("wrela.toml"), manifest(&["lib", "empty"], "")).expect("write");
    let (status, out) = suite(&ws, &["wrela::empty::"]);
    assert_eq!(status, Some(1), "{out}");
    assert!(out.contains("wrela::empty:: selects no test"), "{out}");
}

#[test]
fn every_check_runs_when_others_fail_and_each_failure_is_shown() {
    let ws = package(
        "suite-failures",
        &[
            (
                "wrela.toml",
                &manifest(
                    &["lib", "good", "slow"],
                    "pending = [\"waits.txt\", \"gone.txt\"]\nuntested = [\"slow\"]\n",
                ),
            ),
            (".gitignore", "**/build/\n"),
            ("lib/wrela.toml", "[package]\nname = \"lib\"\n"),
            (
                "lib/sums.wrela",
                "@test\nfn holds() {\n    assert(1 + 1 == 2)\n}\n\n@test\nfn breaks() {\n    assert(1 + 1 == 3, \"one and one\")\n}\n",
            ),
            ("good/wrela.toml", "[package]\nname = \"good\"\n"),
            // Not formatted: the line is indented by one space.
            ("good/sums.wrela", "@test\nfn holds() {\n assert(2 == 2)\n}\n"),
            ("slow/wrela.toml", "[package]\nname = \"slow\"\n"),
            ("slow/sums.wrela", "@test\nfn named_only() {\n    assert(true)\n}\n"),
            ("notes.md", "not allowed"),
            ("tools/run.py", "not allowed either"),
            ("waits.txt", "pending"),
        ],
    );
    let (status, out) = suite(&ws, &[]);
    assert_eq!(status, Some(1), "{out}");
    for line in [
        "not run: wrela::slow:: (`untested` in wrela.toml)",
        "pending removal: waits.txt",
        "FAILED fmt in",
        "not formatted: good/sums.wrela",
        "FAILED allow in",
        "not allowed: notes.md",
        "not allowed: tools/run.py",
        "pending, but no file waits: gone.txt",
        "ok wrela::lib::holds (",
        "FAILED wrela::lib::breaks (",
        "FAILED wrela::lib:: in",
        "1 of 2 tests failed: breaks",
        "one and one",
        "ok wrela::good::holds (",
        "ok wrela::good:: in",
        "3 of 4 checks failed: fmt, allow, wrela::lib::",
        "all checks failed in",
    ] {
        assert!(out.contains(line), "`{line}` in:\n{out}");
    }
    assert!(!out.contains("named_only ("), "{out}");
    // Named, an untested member's tests run.
    let (status, out) = suite(&ws, &["wrela::slow::"]);
    assert_eq!(status, Some(0), "{out}");
    assert!(out.contains("ok wrela::slow::named_only ("), "{out}");
    // A file that a workspace's ignore files leave out isn't the workspace's; a pending entry
    // that matches no file still fails, until it goes.
    std::fs::write(ws.join(".gitignore"), "**/build/\n*.md\n/tools/\n").expect("write");
    let (status, out) = suite(&ws, &["allow"]);
    assert_eq!(status, Some(1), "{out}");
    assert!(out.contains("pending, but no file waits: gone.txt"), "{out}");
    assert!(!out.contains("notes.md") && !out.contains("tools/run.py"), "{out}");
    let rest = "pending = [\"waits.txt\"]\n";
    std::fs::write(ws.join("wrela.toml"), manifest(&["lib", "good", "slow"], rest)).expect("write");
    let (status, out) = suite(&ws, &["allow"]);
    assert_eq!(status, Some(0), "{out}");
    assert!(out.contains("pending removal: waits.txt") && !out.contains("notes.md"), "{out}");
}

#[test]
fn a_workspace_that_doesnt_read_or_isnt_there_stops_the_suite() {
    let ws = package(
        "suite-invalid",
        &[("wrela.toml", "[workspace]\nmembers = [\"gone\"]\nallow = [\"/x\"]\n")],
    );
    let (status, out) = suite(&ws, &[]);
    assert_eq!(status, Some(1), "{out}");
    assert!(out.contains("E0219") && out.contains("glob"), "{out}");
    std::fs::write(ws.join("wrela.toml"), "[workspace]\nmembers = [\"gone\"]\n").expect("write");
    let (status, out) = suite(&ws, &[]);
    assert_eq!(status, Some(1), "{out}");
    assert!(out.contains("the member `gone` isn't a directory"), "{out}");
    let (status, out) = suite(Path::new("/"), &[]);
    assert_eq!(status, Some(2), "{out}");
    assert!(out.contains("has `[workspace]`"), "{out}");
}

/// What a failing test printed is in its report, once: a tool test's lines, which its process
/// captured, and a test of code's, which `wrela test` showed as they were made; by its group and
/// by its name alike.
#[test]
fn a_failing_tests_lines_are_in_its_report_once() {
    let tooled = r#"pub fn tool() -> u32 {
    0
}

@test(tool)
fn prints_then_fails() {
    std::io::print("tool-marker-5c1e")
    panic("the tool test fails")
}

@test
fn prints_then_breaks() {
    std::io::print("code-marker-9d2b")
    assert(1 == 2, "the test of code fails")
}
"#;
    let ws = package(
        "suite-printed",
        &[
            ("wrela.toml", &manifest(&["tooled"], "")),
            (".gitignore", "**/build/\n"),
            ("tooled/wrela.toml", "[package]\nname = \"tooled\"\n"),
            ("tooled/main.wrela", tooled),
        ],
    );
    for args in [
        &["wrela::tooled::"][..],
        &["wrela::tooled::prints_then_fails"],
        &["wrela::tooled::prints_then_breaks"],
    ] {
        let (status, out) = suite(&ws, args);
        assert_eq!(status, Some(1), "{args:?}: {out}");
        let ran = |name: &str| args[0].ends_with("::") || args[0].ends_with(name);
        for (test, marker) in
            [("prints_then_fails", "tool-marker-5c1e"), ("prints_then_breaks", "code-marker-9d2b")]
        {
            let want = usize::from(ran(test));
            assert_eq!(out.matches(marker).count(), want, "{args:?}: `{marker}` in:\n{out}");
        }
    }
}
