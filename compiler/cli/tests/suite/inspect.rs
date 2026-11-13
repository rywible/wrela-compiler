//! `wrela inspect` (language.md §23.10): the frozen wolf is one certain piece; an ear moved off
//! a head floats, named; a literal multiplied by zero is a dead literal at its line; and the
//! three views are written.

use crate::common;
use std::path::Path;

/// `wrela inspect <dir> <subject> <args...> --json`.
fn inspect(dir: &Path, subject: &str, args: &[&str]) -> serde_json::Value {
    common::answer(common::wrela().arg("inspect").arg(dir).arg(subject).args(args).arg("--json"))
}

/// A copy of `compiler/tests/frozen` (the wolf and the packages it depends on), in `name`.
fn frozen(name: &str) -> std::path::PathBuf {
    let root = common::package(name, &[]);
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/frozen");
    for file in common::walk(&from) {
        let rel = file.strip_prefix(&from).expect("under frozen");
        if rel.components().any(|c| c.as_os_str() == "build") {
            continue;
        }
        let to = root.join(rel);
        std::fs::create_dir_all(to.parent().expect("a directory")).expect("mkdir");
        std::fs::copy(&file, &to).expect("copy");
    }
    root
}

/// The frozen wolf is one certain piece, and its three views are written.
#[test]
fn the_wolf_is_one_piece() {
    let root = frozen("inspect-wolf");
    let views = root.join("views");
    let r = inspect(
        &root.join("wolf"),
        "subject::subject",
        &["--region", " -0.6..0.6,-0.1..1.3,-1.2..1.2", "--views", views.to_str().unwrap()],
    );
    assert_eq!(r["pieces"]["certain"], 1, "{r:#}");
    for name in ["side", "front", "top"] {
        assert!(views.join(format!("{name}.png")).is_file(), "{name}.png");
    }
}

const EARS: &str = "use std::field::{Surface, sphere}

pub fn head() -> Surface {
    let h = sphere(0.3).named(\"head\")
    let ear = sphere(0.08).translate(vec3(0.0, 0.55, 0.0)).named(\"left ear\")
    h.union(ear)
}
";

/// An ear moved off the head is a floating piece, named.
#[test]
fn an_ear_off_the_head_floats() {
    let dir = common::package(
        "inspect-ear",
        &[
            ("wrela.toml", &common::manifest("eared")),
            ("head.wrela", EARS),
            ("main.wrela", common::FRAME),
        ],
    );
    let r = inspect(
        &dir,
        "head::head",
        &["--region", " -0.7..0.7,-0.7..0.8,-0.7..0.7", "--depth", "6"],
    );
    let floating: Vec<&serde_json::Value> =
        r["findings"].as_array().unwrap().iter().filter(|f| f["kind"] == "floating").collect();
    assert_eq!(floating.len(), 1, "{r:#}");
    assert_eq!(floating[0]["parts"], serde_json::json!(["left ear"]), "{r:#}");
    assert_eq!(floating[0]["proof"], "certified");
}

const DEAD: &str = "use std::field::{Surface, sphere}

pub fn ball() -> Surface {
    let r = 0.5 + 0.3 * 0.0
    sphere(r)
}
";

/// A literal multiplied by zero moves the surface nowhere: a dead literal at its line.
#[test]
fn a_literal_times_zero_is_dead() {
    let dir = common::package(
        "inspect-dead",
        &[
            ("wrela.toml", &common::manifest("deadlit")),
            ("ball.wrela", DEAD),
            ("main.wrela", common::FRAME),
        ],
    );
    let r = inspect(&dir, "ball::ball", &["--lift", "--depth", "5"]);
    let dead: Vec<&serde_json::Value> =
        r["findings"].as_array().unwrap().iter().filter(|f| f["kind"] == "dead-literal").collect();
    assert!(dead.iter().any(|f| f["text"] == "0.3" && f["line"] == 4), "{r:#}");
    assert!(!dead.iter().any(|f| f["text"] == "0.5"), "{r:#}");
}
