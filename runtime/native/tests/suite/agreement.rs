//! The two hosts agree: first-light run in headless Chrome (the browser runtime, via
//! tools/headless.py) and in the native host gives the same state hash, and frames within a
//! mean absolute difference of 0.5/255.
//!
//! Ignored by default: it needs Chrome stable, python3 and the GPU. Run it with
//!
//!     cargo test -p wrela-host --test suite agreement:: -- --ignored --nocapture
//!
//! It stages the page from the checked-in runtime (runtime/browser/dist), so rebuild that first
//! if the runtime's sources changed (`bun run build` in runtime/browser).

use crate::common;

use std::path::Path;
use std::process::Command;
use wrela_abi::Manifest;
use wrela_host::{Host, frame_time, image};

const FRAMES: u32 = 60;
const FPS: f64 = 60.0;
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
/// The largest mean absolute difference allowed, in 8-bit steps.
const MEAN_LIMIT: f64 = 0.5;

/// Copies the runtime and a build into a page directory, with an empty `results/`.
fn stage(build: &Path, runtime: &Path, page: &Path) {
    let manifest =
        Manifest::parse(&std::fs::read_to_string(build.join("manifest.json")).expect("manifest"))
            .expect("valid manifest");
    let _ = std::fs::remove_dir_all(page);
    std::fs::create_dir_all(page.join("results")).expect("page dir");
    for entry in std::fs::read_dir(runtime).expect("runtime/browser/dist") {
        let path = entry.expect("entry").path();
        std::fs::copy(&path, page.join(path.file_name().expect("name"))).expect("copy runtime");
    }
    let files = ["manifest.json", manifest.wasm.as_str()]
        .into_iter()
        .chain(manifest.pipelines.iter().map(|p| p.shader.as_str()));
    for name in files {
        std::fs::copy(build.join(name), page.join(name)).expect("copy build");
    }
}

#[test]
#[ignore = "long: needs Chrome, python3 and the GPU"]
fn browser_and_native_agree_on_first_light() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let build = common::first_light();
    let page_rel = "runtime/fixtures/first-light/page";
    let page = root.join(page_rel);
    stage(&build, &root.join("runtime/browser/dist"), &page);

    // The browser first; this process holds no GPU lock while Chrome runs.
    let fragment = format!("#test&frames={FRAMES}&width={WIDTH}&height={HEIGHT}&fps={FPS}");
    let status = Command::new("python3")
        .arg(root.join("tools/headless.py"))
        .args([page_rel, &fragment, "180"])
        .current_dir(&root)
        .status()
        .expect("python3 runs tools/headless.py");
    assert!(status.success(), "the browser run failed; see {page_rel}/results/console.log");
    let results = page.join("results");
    let browser_hash = std::fs::read_to_string(results.join("hash.txt")).expect("hash.txt");
    let browser_frame = std::fs::read(results.join("frame.rgba")).expect("frame.rgba");

    let times: Vec<f32> = (0..FRAMES).map(|i| frame_time(i, FPS)).collect();
    let native =
        Host::load(&build).expect("loads").run_frames(&times, WIDTH, HEIGHT).expect("runs");

    assert_eq!(browser_hash.trim(), native.hash_hex(), "state hashes differ");
    let diff = image::compare(&browser_frame, &native.frame).expect("same size");
    eprintln!(
        "state hash {} in both hosts; frames differ by mean {:.4}/255, max {}/255, in {} of {} channels",
        native.hash_hex(),
        diff.mean,
        diff.max,
        diff.differing,
        diff.channels
    );
    assert!(diff.mean <= MEAN_LIMIT, "mean difference {} is over {MEAN_LIMIT}", diff.mean);
}
