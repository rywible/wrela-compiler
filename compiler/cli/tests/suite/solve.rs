//! `wrela solve` (#39, the owner's additions): a function of no arguments is minimized over the
//! literals on the lines named, which end rounded to their decimals, and `--write` writes them
//! back through `wrela edit`: only those literals' characters change.

use crate::common;

use std::path::PathBuf;

const FIT: &str = "use std::field::{Surface, sphere}

/// The ball's radius, and where it sits.
pub const R: f32 = 0.50
pub const AT: vec3 = vec3(0.0, 0.3, 0.0)

/// How far the ball's surface is from passing through two points.
pub fn miss() -> f32 {
    let b = sphere(R).translate(AT)
    let (a, c) = (b.distance(vec3(0.9, 0.3, 0.0)), b.distance(vec3(0.0, 1.0, 0.0)))
    a * a + c * c
}
";

const MISSES: &str = "use std::field::{Surface, sphere}

/// The ball's radius, and where it sits.
pub const R: f32 = 0.50
pub const AT: vec3 = vec3(0.0, 0.3, 0.0)

/// How far the ball's surface is from each of four points.
pub fn misses() -> [f32; 4] {
    let b = sphere(R).translate(AT)
    [
        b.distance(vec3(0.9, 0.3, 0.0)),
        b.distance(vec3(0.0, 1.0, 0.0)),
        b.distance(vec3(-0.2, 0.5, 0.6)),
        b.distance(vec3(0.1, -0.4, -0.3)),
    ]
}
";

/// A least-squares fit (`--fit`): the function gives each point's miss, and Levenberg–Marquardt
/// brings the sum of their squares down in a few steps.
#[test]
fn a_fit_is_solved_by_least_squares_in_a_few_steps() {
    let dir = common::package(
        "solve-ball-fit",
        &[
            ("wrela.toml", &common::manifest("ball")),
            ("fit.wrela", MISSES),
            ("main.wrela", common::FRAME),
        ],
    );
    let a = common::answer(common::wrela().arg("solve").arg(&dir).args([
        "--fit",
        "fit::misses",
        "--free",
        "fit.wrela:4-5",
        "--json",
    ]));
    let (before, after) = (a["loss_before"].as_f64().unwrap(), a["loss_after"].as_f64().unwrap());
    assert!(before > 0.1 && after < 1e-3, "{a}");
    assert_eq!(a["fit"], true, "{a}");
    assert!(a["steps"].as_u64().unwrap() <= 12, "a fit of four literals takes a few steps: {a}");
    assert_eq!(a["literals"].as_array().unwrap().len(), 4, "R and AT's three: {a}");
}

fn package() -> PathBuf {
    common::package(
        "solve-ball",
        &[
            ("wrela.toml", &common::manifest("ball")),
            ("fit.wrela", FIT),
            ("main.wrela", common::FRAME),
        ],
    )
}

fn solve(dir: &PathBuf, extra: &[&str]) -> serde_json::Value {
    common::answer(
        common::wrela()
            .arg("solve")
            .arg(dir)
            .args(["--minimize", "fit::miss", "--free", "fit.wrela:4-5", "--json"])
            .args(extra),
    )
}

#[test]
fn a_loss_is_minimized_over_the_literals_named_and_written_back() {
    let dir = package();
    let a = solve(&dir, &[]);
    let (before, after) = (a["loss_before"].as_f64().unwrap(), a["loss_after"].as_f64().unwrap());
    assert!(before > 0.1 && after < 1e-3, "{a}");
    assert_eq!(a["literals"].as_array().unwrap().len(), 4, "R and AT's three: {a}");
    assert_eq!(
        std::fs::read_to_string(dir.join("fit.wrela")).unwrap(),
        FIT,
        "nothing written without --write"
    );
    let b = solve(&dir, &["--write"]);
    assert_eq!(b["written"], true);
    let text = std::fs::read_to_string(dir.join("fit.wrela")).unwrap();
    let changed: Vec<usize> = FIT
        .lines()
        .zip(text.lines())
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(i, _)| i + 1)
        .collect();
    assert!(
        !changed.is_empty() && changed.iter().all(|l| (4..=5).contains(l)),
        "lines {changed:?} changed:\n{text}"
    );
    // The written literals read back as the values solved.
    for l in b["literals"].as_array().unwrap() {
        let after = l["after"].as_f64().unwrap() as f32;
        assert!(after.is_finite());
    }
    // Solving the written source again starts where the last one ended.
    let c = solve(&dir, &[]);
    assert!(
        (c["loss_before"].as_f64().unwrap() - b["loss_after"].as_f64().unwrap()).abs() < 1e-6,
        "{c}"
    );
}

#[test]
fn a_negative_loss_uses_its_magnitude_for_rounding_tolerance() {
    let fit = "pub const X: f32 = 1.0\n\npub fn miss() -> f32 {\n    let d = X - 1.25\n    d * d - 100.0\n}\n";
    let dir = common::package(
        "solve-negative-loss",
        &[
            ("wrela.toml", &common::manifest("negative_loss")),
            ("fit.wrela", fit),
            ("main.wrela", common::FRAME),
        ],
    );
    let answer = common::answer(common::wrela().arg("solve").arg(&dir).args([
        "--minimize",
        "fit::miss",
        "--free",
        "fit.wrela:1",
        "--json",
    ]));
    let solved = answer["loss_solved"].as_f64().unwrap();
    let after = answer["loss_after"].as_f64().unwrap();
    let x = answer["literals"][0]["after"].as_f64().unwrap();
    assert!(solved < -99.9999 && after < -99.99, "{answer}");
    assert!((x - 1.3).abs() < 1e-6, "the source-precision value is kept: {answer}");
}

/// A solve that gains 7%, all of it given up when `a` and `c` are rounded to their own decimals
/// (each within the 5% a rounding may cost). No number of decimals of `b`, rounded last, brings
/// it back: `b` keeps its own two, not seven.
const OUT_OF_REACH: &str = "pub fn miss() -> f32 {
    let a = 1.0
    let c = 3.0
    let b = 2.00
    let da = a - 1.04
    let dc = c - 3.03
    let db = b - 2.0043217
    1.0 + 25.0 * da * da + 44.444443 * dc * dc + 50.0 * db * db
}
";

/// The same with one more decimal of `b` keeping the solve within 5%: it gets that one.
const IN_REACH: &str = "pub fn miss() -> f32 {
    let a = 1.0
    let b = 2.00
    let da = a - 1.04
    let db = b - 2.004
    1.0 + 25.0 * da * da + 1250.0 * db * db
}
";

#[test]
fn a_literal_gets_no_decimals_that_buy_nothing() {
    let solve = |name: &str, fit: &str, lines: &str| {
        let dir = common::package(
            name,
            &[
                ("wrela.toml", &common::manifest("decimals")),
                ("fit.wrela", fit),
                ("main.wrela", common::FRAME),
            ],
        );
        common::answer(common::wrela().arg("solve").arg(&dir).args([
            "--minimize",
            "fit::miss",
            "--free",
            lines,
            "--json",
        ]))
    };
    let after = |answer: &serde_json::Value, text: &str| {
        let literals = answer["literals"].as_array().unwrap();
        let literal = literals.iter().find(|l| l["text"] == text).expect("the literal freed");
        literal["after"].as_f64().unwrap()
    };
    let out = solve("solve-decimals-out-of-reach", OUT_OF_REACH, "fit.wrela:2-4");
    let (before, solved) =
        (out["loss_before"].as_f64().unwrap(), out["loss_solved"].as_f64().unwrap());
    assert!(before > 1.08 && solved < 1.001, "the solve gains 7%: {out}");
    assert!((after(&out, "1.0") - 1.0).abs() < 1e-6, "{out}");
    assert!((after(&out, "3.0") - 3.0).abs() < 1e-6, "{out}");
    assert!((after(&out, "2.00") - 2.0).abs() < 1e-6, "decimals that buy nothing: {out}");
    let within = solve("solve-decimals-in-reach", IN_REACH, "fit.wrela:2-3");
    assert!((after(&within, "1.0") - 1.0).abs() < 1e-6, "{within}");
    assert!((after(&within, "2.00") - 2.004).abs() < 1e-6, "the decimal that buys it: {within}");
    assert!(within["loss_after"].as_f64().unwrap() < 1.05, "{within}");
}

/// A solve that gains under 1%, where `X` rounded to its one decimal lands on the steep side:
/// within the 5% a rounding may cost, and above where the solve began. `X` stays where it was.
#[test]
fn rounding_never_leaves_a_function_larger_than_the_solve_found_it() {
    let fit = "pub const X: f32 = 1.0\n\npub fn miss() -> f32 {\n    let d = X - 1.06\n    let steep = if d > 0.0 { 8.0 } else { 1.0 }\n    1.0 + 2.0 * steep * d * d\n}\n";
    let dir = common::package(
        "solve-never-larger",
        &[
            ("wrela.toml", &common::manifest("never_larger")),
            ("fit.wrela", fit),
            ("main.wrela", common::FRAME),
        ],
    );
    let answer = common::answer(common::wrela().arg("solve").arg(&dir).args([
        "--minimize",
        "fit::miss",
        "--free",
        "fit.wrela:1",
        "--json",
    ]));
    let loss = |key: &str| answer[key].as_f64().unwrap();
    assert!(loss("loss_before") > 1.007 && loss("loss_solved") < 1.0001, "{answer}");
    assert!(loss("loss_after") <= loss("loss_before"), "{answer}");
    let x = answer["literals"][0]["after"].as_f64().unwrap();
    assert!((x - 1.0).abs() < 1e-6, "1.1 is within 5% of the solve, and above its start: {answer}");
}

#[test]
fn repeated_free_options_include_each_range_of_the_same_file_once() {
    let dir = common::package(
        "solve-repeated-free",
        &[
            ("wrela.toml", &common::manifest("repeated_free")),
            (
                "fit.wrela",
                "pub const X: f32 = 1.0\npub const Y: f32 = 2.0\n\npub fn miss() -> f32 {\n    let x = X - 3.0\n    let y = Y - 4.0\n    x * x + y * y\n}\n",
            ),
            ("main.wrela", common::FRAME),
        ],
    );
    let answer = common::answer(common::wrela().arg("solve").arg(&dir).args([
        "--minimize",
        "fit::miss",
        "--free",
        "fit.wrela:1",
        "--free",
        "./fit.wrela:2",
        "--free",
        "fit.wrela:1-2",
        "--json",
    ]));
    assert_eq!(answer["literals"].as_array().unwrap().len(), 2, "{answer}");
    assert!(answer["loss_after"].as_f64().unwrap() < 1e-6, "{answer}");
}

const BALL: &str = "use std::field::{Surface, sphere}

/// A ball on the ground.
pub fn ball() -> Surface {
    sphere(0.40).translate(vec3(0.0, 0.40, 0.0))
}
";

const SPEC: &str = "use ball::ball
use std::field::{top, width}
use std::lift::Checks

pub fn spec<C: Checks>(c: mut C) {
    let f = ball()
    c.near(\"height\", top(f, x: 0.0, z: 0.0), 1.10, within: 0.01)
    c.at_least(\"width\", width(f, y: 0.55, z: 0.0), 1.00, within: 0.01)
}
";

/// `--spec`: the package's spec, solved over a whole file's literals, holds after; its checks
/// are reported before and after.
#[test]
fn a_spec_is_solved_over_a_files_literals() {
    let dir = common::package(
        "solve-spec",
        &[
            ("wrela.toml", &common::manifest("ball")),
            ("ball.wrela", BALL),
            ("spec.wrela", SPEC),
            ("main.wrela", common::FRAME),
        ],
    );
    let r = common::answer(common::wrela().arg("solve").arg(&dir).args([
        "--spec",
        "--free",
        "ball.wrela",
        "--json",
    ]));
    assert_eq!(r["spec_before"]["held"], 0, "{r}");
    assert_eq!(r["held_before"], false, "{r}");
    assert_eq!(r["spec_after"]["held"], 2, "{r}");
    assert_eq!(r["spec_after"]["missed"], 0, "{r}");
    let names: Vec<&str> = r["spec_after"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["height", "width"]);
}

#[test]
fn a_non_finite_loss_is_not_reported_as_solved() {
    let dir = common::package(
        "solve-non-finite",
        &[
            ("wrela.toml", &common::manifest("bad_loss")),
            ("fit.wrela", "pub const X: f32 = 1.0\n\npub fn miss() -> f32 {\n    sqrt(-X)\n}\n"),
            ("main.wrela", common::FRAME),
        ],
    );
    let out = common::wrela()
        .arg("solve")
        .arg(&dir)
        .args(["--minimize", "fit::miss", "--free", "fit.wrela:1", "--json"])
        .output()
        .expect("run wrela");
    assert_eq!(out.status.code(), Some(1));
    let answer: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(answer["solved"], false);
    assert!(answer["why"].as_str().is_some_and(|w| w.contains("not finite")), "{answer}");
}

/// The ball's spec with a check it can't meet: its top is asked to be at two heights.
const SPEC_APART: &str = "use ball::ball
use std::field::top
use std::lift::Checks

pub fn spec<C: Checks>(c: mut C) {
    let f = ball()
    c.near(\"height\", top(f, x: 0.0, z: 0.0), 1.10, within: 0.01)
    c.near(\"height again\", top(f, x: 0.0, z: 0.0), 0.50, within: 0.01)
}
";

fn spec_package(name: &str, spec: &str) -> PathBuf {
    common::package(
        name,
        &[
            ("wrela.toml", &common::manifest("ball")),
            ("ball.wrela", BALL),
            ("spec.wrela", spec),
            ("main.wrela", common::FRAME),
        ],
    )
}

/// `--spec --write` writes a solve whose every check holds.
#[test]
fn a_spec_that_holds_is_written() {
    let dir = spec_package("solve-spec-written", SPEC);
    let r = common::answer(common::wrela().arg("solve").arg(&dir).args([
        "--spec",
        "--free",
        "ball.wrela",
        "--write",
        "--json",
    ]));
    assert_eq!(r["spec_after"]["missed"], 0, "{r}");
    assert_eq!(r["written"], true, "{r}");
    assert!(r["refused"].is_null(), "{r}");
    assert_ne!(std::fs::read_to_string(dir.join("ball.wrela")).unwrap(), BALL);
}

/// `--spec --write` of a solve that stopped short (a check still misses) writes nothing, says
/// which checks miss, and fails.
#[test]
fn a_spec_that_misses_is_not_written() {
    let dir = spec_package("solve-spec-refused", SPEC_APART);
    let solve = |extra: &[&str]| {
        let out = common::wrela()
            .arg("solve")
            .arg(&dir)
            .args(["--spec", "--free", "ball.wrela", "--write"])
            .args(extra)
            .output()
            .expect("run wrela");
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        (out.status.code(), text(&out.stdout), text(&out.stderr))
    };
    let (code, stdout, _) = solve(&["--json"]);
    let r: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    assert_eq!(code, Some(1), "{r}");
    assert_eq!(r["solved"], true, "the solve ran: {r}");
    assert_eq!(r["written"], false, "{r}");
    assert!(r["spec_after"]["missed"].as_u64().unwrap() >= 1, "{r}");
    assert!(r["refused"].as_str().is_some_and(|w| w.contains("of the spec's 2 checks")), "{r}");
    assert!(r["edit"].is_null(), "{r}");
    assert_eq!(std::fs::read_to_string(dir.join("ball.wrela")).unwrap(), BALL, "nothing written");
    // As text: the report, then the error and the checks that miss where errors are read.
    let (code, stdout, stderr) = solve(&[]);
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(stdout.contains("the spec: ") && !stdout.contains("written"), "{stdout}");
    assert!(stderr.contains("error: nothing written: "), "{stderr}");
    assert!(stderr.contains("  misses height"), "{stderr}");
    assert_eq!(std::fs::read_to_string(dir.join("ball.wrela")).unwrap(), BALL, "nothing written");
    // Without `--write` a solve that stops short is reported, and isn't a failure.
    let out = common::wrela()
        .arg("solve")
        .arg(&dir)
        .args(["--spec", "--free", "ball.wrela", "--json"])
        .output()
        .expect("run wrela");
    assert_eq!(out.status.code(), Some(0));
}

/// The ball's spec holding as the ball is written, 10cm off a target it may miss by 20.
const SPEC_HELD: &str = "use ball::ball
use std::field::top
use std::lift::Checks

pub fn spec<C: Checks>(c: mut C) {
    let f = ball()
    c.near(\"height\", top(f, x: 0.0, z: 0.0), 0.90, within: 0.20)
}
";

/// `--spec` of a spec whose every check already holds takes no step: no literal moves, though
/// a solve could lower its loss, and `--write` writes nothing.
#[test]
fn a_spec_that_already_holds_is_left_as_it_is() {
    let dir = spec_package("solve-spec-held", SPEC_HELD);
    let r = common::answer(common::wrela().arg("solve").arg(&dir).args([
        "--spec",
        "--free",
        "ball.wrela",
        "--write",
        "--json",
    ]));
    assert_eq!(r["held_before"], true, "{r}");
    assert_eq!(r["spec_before"]["missed"], 0, "{r}");
    assert!(r["loss_before"].as_f64().unwrap() > 0.1, "a loss a solve could lower: {r}");
    assert_eq!(r["steps"], 0, "{r}");
    assert_eq!(r["rounded"], false, "{r}");
    assert_eq!(r["written"], false, "nothing to write: {r}");
    assert!(r["refused"].is_null(), "{r}");
    assert_eq!(r["loss_after"], r["loss_before"], "{r}");
    for l in r["literals"].as_array().unwrap() {
        assert_eq!(l["after"], l["before"], "{r}");
    }
    assert_eq!(std::fs::read_to_string(dir.join("ball.wrela")).unwrap(), BALL, "nothing changed");
    // As text, it says so, and that isn't a failure.
    let (code, stdout, all) = common::outputs(&[
        "solve",
        dir.to_str().unwrap(),
        "--spec",
        "--free",
        "ball.wrela",
        "--write",
    ]);
    assert_eq!(code, Some(0), "{all}");
    assert!(stdout.contains("every check already held: nothing solved"), "{all}");
    assert!(!stdout.lines().any(|l| l == "written"), "{all}");
}

const NAMED: &str = "/// How far the reach misses.
pub const BIAS: f32 = 0.25

pub fn miss() -> f32 {
    let reach = 1.0
    let at = vec2(2.0, 5.0)
    let held = 7.0
    let x = reach + BIAS - 3.0
    let y = at.x + at.y - 4.0
    x * x + y * y + held
}
";

/// `--free <file>:<name>` frees the literals of the value the file binds the name to (a `let`,
/// a `var`, a `const`), wherever its line is: lines added above it don't move it.
#[test]
fn literals_are_freed_by_the_name_they_are_bound_to() {
    let files = |fit: &str| {
        common::package(
            "solve-named",
            &[
                ("wrela.toml", &common::manifest("named")),
                ("fit.wrela", fit),
                ("main.wrela", common::FRAME),
            ],
        )
    };
    let solve = |dir: &PathBuf, free: &str| {
        common::answer(common::wrela().arg("solve").arg(dir).args([
            "--minimize",
            "fit::miss",
            "--free",
            free,
            "--json",
        ]))
    };
    let named = |r: &serde_json::Value| -> Vec<(String, u64, String)> {
        r["literals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                (
                    l["name"].as_str().unwrap_or("").to_string(),
                    l["line"].as_u64().unwrap(),
                    l["text"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    let lit = |name: &str, line: u64, text: &str| (name.to_string(), line, text.to_string());
    let dir = files(NAMED);
    // A `let`'s one literal, a `let`'s vector of two, and a `const`'s; `held` stays as it is.
    let r = solve(&dir, "fit.wrela:reach,at,BIAS");
    assert_eq!(
        named(&r),
        [lit("BIAS", 2, "0.25"), lit("reach", 5, "1.0"), lit("at", 6, "2.0"), lit("at", 6, "5.0")],
        "{r}"
    );
    assert!(r["loss_after"].as_f64().unwrap() < 7.0 + 1e-3, "{r}");
    // With lines too, in one `--free`.
    let r = solve(&dir, "fit.wrela:reach,7");
    assert_eq!(named(&r), [lit("reach", 5, "1.0"), lit("", 7, "7.0")], "{r}");
    // By its line in one `--free` and by its name in another: once, with its name.
    let r = common::answer(common::wrela().arg("solve").arg(&dir).args([
        "--minimize",
        "fit::miss",
        "--free",
        "fit.wrela:5",
        "--free",
        "fit.wrela:reach",
        "--json",
    ]));
    assert_eq!(named(&r), [lit("reach", 5, "1.0")], "{r}");
    // The file's lines move; the names don't.
    let dir = files(&format!("// A line above.\n// And another.\n{NAMED}"));
    let r = solve(&dir, "fit.wrela:reach");
    assert_eq!(named(&r), [lit("reach", 7, "1.0")], "{r}");
}

/// A name the file doesn't bind, binds twice, or binds to a value with no literal is an error
/// that says so, before anything is solved.
#[test]
fn a_name_that_frees_no_one_value_is_an_error() {
    let fit = "pub fn miss() -> f32 {\n    let reach = 1.0\n    let same = reach\n    (same - 3.0) * (same - 3.0)\n}\n\npub fn other() -> f32 {\n    let reach = 2.0\n    reach\n}\n";
    let dir = common::package(
        "solve-named-errors",
        &[
            ("wrela.toml", &common::manifest("named_errors")),
            ("fit.wrela", fit),
            ("main.wrela", common::FRAME),
        ],
    );
    let why = |free: &str| {
        let out = common::wrela()
            .arg("solve")
            .arg(&dir)
            .args(["--minimize", "fit::miss", "--free", free, "--json"])
            .output()
            .expect("run wrela");
        assert_eq!(out.status.code(), Some(1), "{free}");
        let answer: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
        assert_eq!(answer["solved"], false, "{answer}");
        answer["why"].as_str().expect("why").to_string()
    };
    let w = why("fit.wrela:grasp");
    assert!(w.contains("binds no `grasp`"), "{w}");
    let w = why("fit.wrela:reach");
    assert!(w.contains("binds `reach` 2 times (lines 2, 8)"), "{w}");
    let w = why("fit.wrela:same");
    assert!(w.contains("`same` in `fit.wrela` (line 3)") && w.contains("no lifted literal"), "{w}");
}

/// A freed literal the function never changes with (its derivative by it was 0 wherever the
/// solve took one) is warned of, by its file and line: the solve couldn't have moved it.
#[test]
fn a_literal_the_function_never_changes_with_is_warned_of() {
    let fit = "pub const X: f32 = 1.0\npub const STEP: f32 = 2.5\n\npub fn miss() -> f32 {\n    let d = X - 3.0\n    d * d + floor(STEP)\n}\n";
    let dir = common::package(
        "solve-unchanging",
        &[
            ("wrela.toml", &common::manifest("unchanging")),
            ("fit.wrela", fit),
            ("main.wrela", common::FRAME),
        ],
    );
    let solve = |extra: &[&str]| {
        let out = common::wrela()
            .arg("solve")
            .arg(&dir)
            .args(["--minimize", "fit::miss", "--free", "fit.wrela:1-2"])
            .args(extra)
            .output()
            .expect("run wrela");
        assert_eq!(out.status.code(), Some(0), "a warning isn't a failure");
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        (text(&out.stdout), text(&out.stderr))
    };
    let (stdout, _) = solve(&["--json"]);
    let r: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    let changes: Vec<bool> =
        r["literals"].as_array().unwrap().iter().map(|l| l["changes"].as_bool().unwrap()).collect();
    assert_eq!(changes, [true, false], "{r}");
    let warnings = r["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{r}");
    let w = warnings[0].as_str().unwrap();
    assert!(w.starts_with("fit.wrela:2: fit::miss never changed with the literal `2.5`"), "{w}");
    assert_eq!(r["literals"][1]["after"], 2.5, "left where it was: {r}");
    let (_, stderr) = solve(&[]);
    assert!(stderr.contains("warning: fit.wrela:2: fit::miss never changed"), "{stderr}");
    assert!(!stderr.contains("fit.wrela:1:"), "{stderr}");
}

/// A literal kept in a `Vec` by one function and read back by another: the derivation can't
/// follow it there. It was a solve of 0 steps that said nothing; it's E0700, at the line that
/// puts it in the `Vec`.
#[test]
fn a_literal_kept_in_heap_memory_is_an_error_not_a_solve_of_no_steps() {
    let fit = "use std::collections::Vec\n\npub const X: f32 = 1.0\n\n/// `x` kept in a `Vec`.\nfn kept(x: f32) -> Vec<f32> {\n    var v: Vec<f32> = Vec::new()\n    v.push(x)\n    v\n}\n\n/// `x` read back out of it.\nfn read(x: f32) -> f32 {\n    let v = kept(x)\n    v[0]\n}\n\npub fn miss() -> f32 {\n    let d = read(X) - 3.0\n    d * d\n}\n";
    let dir = common::package(
        "solve-heap",
        &[
            ("wrela.toml", &common::manifest("heap")),
            ("fit.wrela", fit),
            ("main.wrela", common::FRAME),
        ],
    );
    let (code, all) = common::run(common::wrela().arg("solve").arg(&dir).args([
        "--minimize",
        "fit::miss",
        "--free",
        "fit.wrela:3",
    ]));
    assert_eq!(code, Some(1), "{all}");
    assert!(all.contains("error[E0700]") && all.contains("in heap memory"), "{all}");
    assert!(all.contains("fit.wrela:8:5"), "at the `push`: {all}");
    assert!(all.contains("keep what depends on the input in arrays and values"), "{all}");
}

/// A sphere above a box, each size 20% from the blueprint's.
const BODY: &str = "use std::field::{Surface, cuboid, sphere}

pub fn body() -> Surface {
    let head = sphere(0.30).translate(vec3(0.0, 0.70, 0.0))
    let block = cuboid(vec3(0.48, 0.18, 0.24)).translate(vec3(0.0, 0.15, 0.0))
    head.union(block)
}
";

/// The blueprint, drawn from the sizes the body should have: a head of radius 0.25 and a box
/// half 0.40 across, 0.15 high and 0.20 deep. Each view's outline is its two shapes, one loop
/// after the other (the step between them and back cancels in the even-odd rule).
const BLUEPRINT: &str = "use std::collections::Vec

fn loops(r: f32, hw: f32) -> Vec<vec2> {
    var v: Vec<vec2> = Vec::new()
    for i in 0..49 {
        let a = 6.2831853 * f32(i % 48) / 48.0
        v.push(vec2(r * cos(a), 0.70 + r * sin(a)))
    }
    let corners = [vec2(-hw, 0.0), vec2(hw, 0.0), vec2(hw, 0.30), vec2(-hw, 0.30), vec2(-hw, 0.0)]
    for c in corners {
        v.push(c)
    }
    v.push(vec2(r, 0.70))
    v
}

/// From the side, (z, y).
pub fn side() -> Vec<vec2> {
    loops(0.25, 0.20)
}

/// From the front, (x, y).
pub fn front() -> Vec<vec2> {
    loops(0.25, 0.40)
}
";

const OUTLINE_SPEC: &str = "use body::body
use std::field::{View, outline_miss}
use std::lift::Checks

pub fn spec<C: Checks>(c: mut C) {
    let f = body()
    c.at_most(\"side outline\", outline_miss(f, View::Side, blueprint::side(), 0.02, near: -1.0, far: 1.0), 0.0, within: 0.05)
    c.at_most(\"front outline\", outline_miss(f, View::Front, blueprint::front(), 0.02, near: -1.0, far: 1.0), 0.0, within: 0.05)
}
";

/// A spec that holds a body to its blueprint's outlines (§23.6, §23.7), solved from sizes 20%
/// off, recovers each size within 2 mm, writes them, and feels every size it freed: the
/// outline's derivative reaches each through the silhouette's edge.
#[test]
fn a_spec_of_outlines_recovers_the_sizes() {
    let dir = common::package(
        "solve-outlines",
        &[
            ("wrela.toml", &common::manifest("outlined")),
            ("body.wrela", BODY),
            ("blueprint.wrela", BLUEPRINT),
            ("spec.wrela", OUTLINE_SPEC),
            ("main.wrela", common::FRAME),
        ],
    );
    let out = common::wrela()
        .arg("solve")
        .arg(&dir)
        .args(["--spec", "--free", "body.wrela:4-5", "--write", "--json"])
        .output()
        .expect("run wrela");
    let all =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let r: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {all}"));
    println!("{}", serde_json::to_string_pretty(&r["literals"]).unwrap());
    println!("{}", serde_json::to_string_pretty(&r["spec_after"]).unwrap());
    assert_eq!(r["written"], true, "{all}");
    assert_eq!(r["spec_after"]["missed"], 0, "{all}");
    assert!(!all.contains("never changed"), "a freed size wasn't felt: {all}");
    let want: std::collections::BTreeMap<&str, f64> =
        [("0.30", 0.25), ("0.48", 0.40), ("0.18", 0.15), ("0.24", 0.20)].into();
    for lit in r["literals"].as_array().expect("literals") {
        let text = lit["text"].as_str().unwrap();
        if let Some(w) = want.get(text) {
            let after = lit["after"].as_f64().unwrap();
            assert!((after - w).abs() <= 0.002, "{text}: solved to {after}, the blueprint's {w}");
        }
    }
}
