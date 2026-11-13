//! `wrela twin` (language.md §14.3): a term proven tiny over the box is dropped and one that
//! isn't is kept; a written twin is current to `--check` until its original changes; and the
//! twin builds and, at 10,000 points on the CPU, is within its tolerance of the original.

use crate::common;

const SHADE: &str = "/// How much light reaches a point `d` metres away, a lobe `k` wide.
pub fn falloff(q: vec2) -> f32 {
    let d = q.x
    let k = q.y
    let base = exp(-k * d / 10.0) * pow(1.0 / (1.0 + d), 2.0)
    base + 0.0003 * sin(8.0 * d) + 0.2 * cos(d)
}
";

const CHECK: &str = "use shade::{falloff, falloff_twin}

pub fn frame(time: f32, width: u32, height: u32) {}

@test
fn the_twin_is_within_its_tolerance() {
    var worst = 0.0
    for i in 0..10000 {
        let q = vec2(40.0 * f32(i % 100) / 99.0, 0.5 + 1.5 * f32(i / 100) / 99.0)
        worst = max(worst, abs(falloff(q) - falloff_twin(q)))
    }
    assert(worst <= 0.002)
}
";

fn shade(name: &str) -> std::path::PathBuf {
    common::package(
        name,
        &[
            ("wrela.toml", &common::manifest(&name.replace('-', "_"))),
            ("shade.wrela", SHADE),
            ("main.wrela", common::FRAME),
        ],
    )
}

fn twin(dir: &std::path::Path, extra: &[&str]) -> (Option<i32>, String) {
    let out = common::wrela()
        .arg("twin")
        .arg(dir)
        .args(["shade::falloff", "--box", "0..40,0.5..2", "--within", "0.002"])
        .args(extra)
        .output()
        .expect("run wrela");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code(), text)
}

#[test]
fn a_tiny_term_is_dropped_and_a_large_one_kept() {
    let dir = shade("twin-terms");
    let (code, out) = twin(&dir, &["--json"]);
    assert_eq!(code, Some(0), "{out}");
    let j: serde_json::Value = serde_json::from_str(&out).expect("JSON");
    // The function, not its certificate (which names what it dropped).
    let src = j["source"].as_str().unwrap().split("pub fn").nth(1).unwrap();
    assert!(!src.contains("0.0003 * sin(8.0 * d)"), "{src}");
    assert!(src.contains("0.2 * cos(d)"), "{src}");
    assert!(j["proven"].as_f64().unwrap() <= 0.002, "{j}");
    assert!(j["work"]["after"].as_u64() < j["work"]["before"].as_u64(), "{j}");
}

#[test]
fn a_written_twin_builds_holds_and_goes_stale_with_its_original() {
    let dir = shade("twin-write");
    let (code, out) = twin(&dir, &["--write"]);
    assert_eq!(code, Some(0), "{out}");
    // Current, by the box and tolerance its certificate gives.
    let check = |dir: &std::path::Path| {
        let out = common::wrela()
            .arg("twin")
            .arg(dir)
            .args(["shade::falloff", "--check"])
            .output()
            .unwrap();
        (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned())
    };
    assert_eq!(check(&dir).0, Some(0));
    // It builds, and holds at 10,000 points on the CPU.
    std::fs::write(dir.join("main.wrela"), CHECK).unwrap();
    let (code, _, out) = common::outputs(&["test", dir.to_str().unwrap()]);
    assert_eq!(code, Some(0), "{out}");
    // The original edited: the twin is stale.
    let shade = std::fs::read_to_string(dir.join("shade.wrela")).unwrap();
    std::fs::write(dir.join("shade.wrela"), shade.replacen("0.2 * cos(d)", "0.3 * cos(d)", 1))
        .unwrap();
    let (code, out) = check(&dir);
    assert_eq!(code, Some(1), "{out}");
}
