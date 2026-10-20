//! Spike 14 (#48): a piano piece as a program. engine::score reads notation in constants (a
//! mistake in it fails the build where it is: engine/tests/errors); examples/gymnopedie's score
//! and performance pass their own tests; every number of its taste and marks is lifted;
//! engine::piano renders the same samples every time, and in Chrome the same as on the native
//! host, in real time without dropping out (long checks: a minute of the piece, the whole of it
//! at full size).

use std::path::Path;
use wrela_host::CpuBuild;
use wrela_tests::{page, repo_root};

/// Every number of the taste and the marks is lifted, so a tool can change it while
/// the piece plays (AC4).
#[test]
fn every_number_of_the_performance_is_lifted() {
    let dir = super::lift::build_into(
        "piano-lift",
        &repo_root().join("examples/gymnopedie"),
        &["gymnopedie"],
        false,
    );
    let r = super::lift::report(&dir);
    let files = r["files"].as_array().expect("files");
    let file = files
        .iter()
        .position(|f| f["path"] == "performance.wrela")
        .expect("performance.wrela is lifted");
    let in_performance = |list: &str| -> Vec<serde_json::Value> {
        r[list].as_array().expect("a list").iter().filter(|l| l["file"] == file).cloned().collect()
    };
    // The report lists every float literal, lifted or not: none left out is every one lifted.
    let lifted = in_performance("literals");
    assert!(lifted.len() >= 30, "{} literals lifted", lifted.len());
    assert_eq!(in_performance("not_lifted"), Vec::<serde_json::Value>::new());
}

/// The piece's samples from the native host's offline run: `seconds` of take 1.
fn native(dir: &Path, seconds: f64) -> Vec<f32> {
    let mut host = CpuBuild::load(dir).expect("load").start_with(1).expect("start");
    host.frame(0.0, 16, 16).expect("frame");
    host.render_audio((seconds * 48000.0 / 128.0).ceil() as u32).expect("render")
}

/// The piece's first notes, rendered twice: the same samples. (The left hand's first chord is on
/// beat 2 of bar 1; two seconds hold it.)
#[test]
fn the_piano_renders_the_same_samples_every_time() {
    let (dir, _) = page("examples/gymnopedie", "gymnopedie-native");
    let samples = native(&dir, 2.0);
    let peak = samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(peak > 0.01 && peak < 1.0, "peak {peak}");
    assert!(samples.iter().all(|x| x.is_finite()));
    assert_eq!(native(&dir, 2.0), samples);
}
