//! `wrela same`: whether a package built now behaves as a saved build of it does, frame by
//! frame (the commands it submits), at its last frame (on a GPU), and in its pipelines.

use crate::common;

/// A ball dropped from 10 m whose screen's clear colour is its height, so its commands carry
/// its state each frame.
const BALL_DRAWN: &str = "struct Ball {
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
    let screen = std::gpu::begin_screen_pass(clear: vec4(b.y / 10.0, 0.0, 0.0, 1.0))
    std::gpu::draw(screen, cover, colour, vertices: 3)
    screen.present()
}

@vertex
fn cover(v: std::gpu::VertexIndex) -> std::gpu::ClipPosition {
    std::gpu::ClipPosition { position: vec4(f32(v.index % 2) * 4.0 - 1.0, f32(v.index / 2) * 4.0 - 1.0, 0.0, 1.0) }
}

@fragment
fn colour() -> vec4 { vec4(1.0, 0.0, 0.0, 0.5) }
";

fn ball(name: &str, source: &str) -> std::path::PathBuf {
    common::package(name, &[("wrela.toml", &common::manifest("ball")), ("main.wrela", source)])
}

/// `wrela same`'s exit status and its JSON report (or its error).
fn same(dir: &std::path::Path, against: &std::path::Path, extra: &[&str]) -> (Option<i32>, String) {
    let out = common::wrela()
        .arg("same")
        .arg(dir)
        .arg("--against")
        .arg(against)
        .args(["--no-gpu", "--json"])
        .args(extra)
        .output()
        .expect("run wrela");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code(), text)
}

fn saved(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let to = dir.parent().unwrap().join(name);
    let out = common::wrela().arg("build").arg(dir).arg("-o").arg(&to).output().expect("build");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    to
}

#[test]
fn a_build_is_the_same_as_itself() {
    let dir = ball("same-itself", BALL_DRAWN);
    let base = saved(&dir, "same-itself-base");
    // A first run: nothing is in `build/same` yet.
    assert!(!dir.join("build/same").exists());
    let (code, text) = same(&dir, &base, &[]);
    assert_eq!(code, Some(0), "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!((&v["same"], &v["first_differing_frame"]), (&true.into(), &serde_json::Value::Null));
    assert_eq!(v["frames"], 60);
}

#[test]
fn a_change_to_what_the_program_does_is_found_at_its_first_frame() {
    let dir = ball("same-changed", BALL_DRAWN);
    let base = saved(&dir, "same-changed-base");
    std::fs::write(dir.join("main.wrela"), BALL_DRAWN.replace("9.8 * dt", "9.0 * dt")).unwrap();
    let (code, text) = same(&dir, &base, &["--frames", "10"]);
    assert_eq!(code, Some(1), "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    // Frames count from 0, and the ball moves in its first.
    assert_eq!((&v["same"], &v["first_differing_frame"]), (&false.into(), &0.into()));
}

#[test]
fn a_pipeline_gained_or_lost_is_a_difference() {
    let dir = ball("same-pipelines", BALL_DRAWN);
    let base = saved(&dir, "same-pipelines-base");
    let more = BALL_DRAWN.replace(
        "    screen.present()",
        "    std::gpu::draw(screen, cover, other, vertices: 3)\n    screen.present()",
    ) + "\n@fragment\nfn other() -> vec4 { vec4(0.0, 1.0, 0.0, 0.5) }\n";
    std::fs::write(dir.join("main.wrela"), more).unwrap();
    let (code, text) = same(&dir, &base, &[]);
    assert_eq!(code, Some(1), "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(v["pipelines"]["added"].as_array().map(Vec::len), Some(1), "{v}");
}

#[test]
fn a_build_it_would_overwrite_or_that_isnt_one_is_refused() {
    let dir = ball("same-refused", BALL_DRAWN);
    std::fs::create_dir_all(dir.join("build/same")).unwrap();
    std::fs::write(dir.join("build/same/keep.txt"), "mine").unwrap();
    for against in [dir.join("build/same"), dir.join("build")] {
        let (code, text) = same(&dir, &against, &[]);
        assert_eq!(code, Some(2), "{text}");
        assert!(text.contains("would be overwritten"), "{text}");
    }
    assert_eq!(std::fs::read_to_string(dir.join("build/same/keep.txt")).unwrap(), "mine");
    let empty = dir.parent().unwrap().join("same-refused-empty");
    std::fs::create_dir_all(&empty).unwrap();
    let (code, text) = same(&dir, &empty, &[]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("isn't a build"), "{text}");
}
