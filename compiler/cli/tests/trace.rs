//! `wrela trace` and `wrela bisect` (#39, the owner's additions): a falling ball, watched frame
//! by frame; the first frame it's below the ground; and a debug build that stops where a
//! simulation makes a NaN, where a release build goes on with one.

mod common;

use std::path::PathBuf;
use std::process::Output;

const BALL: &str = "/// A ball dropped from 10 m.
struct Ball {
    y: f32,
    v: f32,
}

pub fn init() -> Ball {
    Ball { y: 10.0, v: 0.0 }
}

pub fn frame(b: mut Ball, time: f32, width: u32, height: u32) {
    let dt = 1.0 / 60.0
    b.v -= 9.8 * dt
    b.y += b.v * dt
}

pub fn height(b: Ball) -> f32 {
    b.y
}

pub fn motion(b: Ball) -> vec2 {
    vec2(b.y, b.v)
}

/// How long the ball has had to fall this far: a NaN once it's below the ground.
pub fn fallen(b: Ball) -> f32 {
    sqrt(2.0 * b.y / 9.8)
}
";

/// The ball package in its own directory, `name`.
fn package(name: &str) -> PathBuf {
    common::package(name, &[("wrela.toml", &common::manifest("ball")), ("main.wrela", BALL)])
}

fn wrela(args: &[&str]) -> Output {
    common::wrela().args(args).output().expect("run wrela")
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

#[test]
fn trace_prints_the_watched_exports_after_each_frame() {
    let dir = package("trace-ball-trace");
    let d = dir.to_str().unwrap();
    let out = wrela(&["trace", d, "--watch", "height,motion", "--frames", "120", "--csv"]);
    assert!(out.status.success(), "{}", text(&out));
    let csv = String::from_utf8_lossy(&out.stdout).to_string();
    let rows: Vec<&str> = csv.lines().collect();
    assert_eq!(rows[0], "frame,time,height,motion.x,motion.y");
    assert_eq!(rows.len(), 121, "{csv}");
    let cells = |r: &str| r.split(',').map(|c| c.parse::<f64>().unwrap()).collect::<Vec<_>>();
    // The ball falls ever faster, and an export's vector is its components.
    let mut last = 10.0;
    for (i, r) in rows[1..].iter().enumerate() {
        let c = cells(r);
        assert_eq!(c[0], i as f64);
        assert!(c[2] < last && c[2] == c[3], "{r}");
        last = c[2];
    }
    let v = cells(rows[120])[4];
    assert!((v + 9.8 * 2.0).abs() < 1e-3, "after 2 s, v = {v}");
    // Without --csv, a line a frame.
    let out = wrela(&["trace", d, "--watch", "height", "--frames", "2"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 2, "{}", text(&out));
}

#[test]
fn bisect_finds_the_first_frame_a_condition_holds() {
    let dir = package("trace-ball-bisect");
    let d = dir.to_str().unwrap();
    let out = wrela(&["bisect", d, "--until", "height < 0", "--json"]);
    assert!(out.status.success(), "{}", text(&out));
    let a: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let frame = a["frame"].as_u64().unwrap();
    // The same frame as the trace's first negative height.
    let trace = wrela(&["trace", d, "--watch", "height", "--frames", "200", "--csv"]);
    let first = String::from_utf8_lossy(&trace.stdout)
        .lines()
        .skip(1)
        .find(|r| r.split(',').nth(2).unwrap().parse::<f64>().unwrap() < 0.0)
        .map(|r| r.split(',').next().unwrap().parse::<u64>().unwrap());
    assert_eq!(Some(frame), first, "{a}");
    assert!(a["values"][0].as_f64().unwrap() < 0.0, "{a}");
    // A component, and a condition that never holds (exit 1).
    let out = wrela(&["bisect", d, "--until", "motion.y < -5", "--json"]);
    let a: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(a["values"][1].as_f64().unwrap() < -5.0, "{a}");
    let out = wrela(&["bisect", d, "--until", "height > 10", "--frames", "100"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let out = wrela(&["bisect", d, "--until", "height about 3"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}

#[test]
fn a_debug_build_stops_at_the_nan_a_release_build_goes_on_with() {
    let dir = package("trace-ball-nan");
    let d = dir.to_str().unwrap();
    let below = serde_json::from_slice::<serde_json::Value>(
        &wrela(&["bisect", d, "--until", "height < 0", "--json"]).stdout,
    )
    .unwrap()["frame"]
        .as_u64()
        .unwrap();
    // Debug, by default: the export that makes the NaN fails, at the frame it does.
    let out = wrela(&["trace", d, "--watch", "fallen", "--frames", "200"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    let shown = text(&out);
    assert!(shown.contains("`fallen`"), "{shown}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count() as u64, below, "{shown}");
    // Release: the NaN comes out, and bisect finds it.
    let out = wrela(&["bisect", d, "--until", "nan fallen", "--release", "--json"]);
    assert!(out.status.success(), "{}", text(&out));
    let a: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(a["frame"].as_u64(), Some(below), "{a}");
}
