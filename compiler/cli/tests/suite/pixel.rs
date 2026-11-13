//! `wrela pixel` on its fixture (compiler/tests/pixel): a pixel under two triangles, its
//! history and the traced draw's interpreted colour; a pixel where a shader divides by zero,
//! whose first value that isn't finite is named at its line; and a pixel no draw touched.
//! They need the native host's GPU.

use crate::common;

const FIXTURE: &str = include_str!("../../../tests/pixel/main.wrela");

fn pixel(name: &str, at: &str) -> serde_json::Value {
    let dir = common::package(
        name,
        &[("wrela.toml", &common::manifest("pixel")), ("main.wrela", FIXTURE)],
    );
    common::answer(
        common::wrela()
            .arg("pixel")
            .arg(&dir)
            .args(["--frame", "0", "--at", at, "--size", "64x64", "--json"]),
    )
}

#[test]
#[ignore = "needs a GPU"]
fn a_pixel_under_two_triangles_shows_both_draws() {
    let j = pixel("pixel-two", "27,36");
    let history = j["history"].as_array().expect("a history");
    assert_eq!(history.len(), 2, "{j:#}");
    assert_eq!(history[0]["draw"], 1);
    assert_eq!(history[1]["draw"], 2);
    assert_eq!(history[1]["before"], history[0]["after"]);
    assert_eq!(j["traced"]["draw"], 2);
    assert_eq!(j["traced"]["shader"], "orange");
    assert!(j["result"]["differs_by"].as_f64().expect("a difference") <= 1.0 / 255.0, "{j:#}");
    // The traced shader's line, and its value.
    let lines = j["lines"].as_array().expect("lines");
    assert!(
        lines
            .iter()
            .any(|l| l["text"].as_str().is_some_and(|t| t.contains("vec4(0.9, 0.5, 0.1, 1.0)"))),
        "{j:#}"
    );
}

#[test]
#[ignore = "needs a GPU"]
fn a_division_by_zero_is_named_at_its_line() {
    let j = pixel("pixel-nan", "32,63");
    assert_eq!(j["traced"]["shader"], "ratio", "{j:#}");
    let f = &j["first_not_finite"];
    assert_eq!(f["text"], "let k = 0.01 / d", "{j:#}");
    let line = 1 + FIXTURE.lines().position(|l| l.trim() == "let k = 0.01 / d").expect("the line");
    assert_eq!(f["line"], line as u64);
    assert!(!j["undefined_on_gpu"].as_array().expect("marks").is_empty(), "{j:#}");
}

#[test]
#[ignore = "needs a GPU"]
fn a_pixel_no_draw_touched_says_so() {
    let dir = common::package(
        "pixel-none",
        &[("wrela.toml", &common::manifest("pixel")), ("main.wrela", FIXTURE)],
    );
    let (code, out) = common::run(
        common::wrela()
            .arg("pixel")
            .arg(&dir)
            .args(["--frame", "0", "--at", "60,20", "--size", "64x64"]),
    );
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("no draw touched it"), "{out}");
    let j = pixel("pixel-none-json", "60,20");
    assert_eq!(j["history"], serde_json::json!([]));
    assert!(j["traced"].is_null());
}

const KERNELS: &str =
    "use std::gpu::{Atomics, GlobalId, GpuBuffer, Slots, buffer, buffer_of, dispatch}

pub struct S {
    squares: GpuBuffer<f32>,
    values: GpuBuffer<u32>,
    bins: GpuBuffer<u32>,
}

pub fn init() -> S {
    S { squares: buffer(64), values: buffer_of([3, 4, 5, 6]), bins: buffer(4) }
}

@compute(64)
fn squares(out: mut Slots<f32>, id: GlobalId) {
    out[id] = f32(id.x) * f32(id.x)
}

@compute(64)
fn histogram(values: [u32], bins: mut Atomics<u32>, id: GlobalId) {
    if id.x < values.len() {
        let _ = bins.add(values[id.x] % bins.len(), 1)
    }
}

pub fn frame(s: mut S, time: f32, width: u32, height: u32) {
    dispatch(squares.bind(out: mut s.squares), groups: 1)
    dispatch(histogram.bind(values: s.values, bins: mut s.bins), groups: 1)
}
";

/// One invocation of a kernel's last dispatch, interpreted; and a kernel that uses what the
/// interpreter doesn't run (atomics) says so.
#[test]
#[ignore = "needs a GPU"]
fn a_kernels_invocation_is_interpreted() {
    let dir = common::package(
        "pixel-kernels",
        &[("wrela.toml", &common::manifest("kernels")), ("main.wrela", KERNELS)],
    );
    let j = common::answer(
        common::wrela()
            .arg("pixel")
            .arg(&dir)
            .args(["--frame", "0", "--kernel", "squares", "--id", "7", "--json"]),
    );
    assert_eq!(j["result"]["interpreted"], serde_json::json!([49.0]), "{j:#}");
    let (code, out) = common::run(common::wrela().arg("pixel").arg(&dir).args([
        "--frame",
        "0",
        "--kernel",
        "histogram",
        "--id",
        "1",
    ]));
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("the interpreter doesn't run atomics"), "{out}");
}
