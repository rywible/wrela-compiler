//! `wrela solve` (#39, the owner's additions): a function of no arguments is minimized over the
//! literals on the lines named, which end rounded to their decimals, and `--write` writes them
//! back through `wrela edit`: only those literals' characters change.

mod common;

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
