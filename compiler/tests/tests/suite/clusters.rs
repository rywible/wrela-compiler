//! The virtual geometry spike (examples/clusters, `engine::clusters`) in Chrome: the browser
//! draws what the native host draws. Its native checks (a cut holds every quad it wants, opens
//! no crack and keeps its silhouettes) are tool tests in examples/clusters/tests/cuts.
//! `cargo test -p wrela-tests --test suite clusters:: -- --ignored`.

use wrela_host::Host;
use wrela_tests::page;

const W: u32 = 640;
const H: u32 = 360;

/// Chrome draws the moving eye's cut of the 64 copies as the native host does: frames within a
/// mean of 0.5/255 (Tint takes the clusters' shaders, and its atomics keep each cluster's quads
/// together as wgpu's do). Not the same state hash: cooking's buffers are fitted once its counts
/// are read back, at a frame each host chooses.
#[test]
#[ignore = "needs a GPU and Chrome"]
fn the_browser_draws_the_native_hosts_clusters() {
    let (dir, rel) = page("examples/clusters", "clusters-browser");
    let frames = 30;
    let browser = wrela_tests::run_in_chrome(&rel, frames, W, H, 60.0);
    let times: Vec<f32> = (0..frames).map(|i| i as f32 / 60.0).collect();
    let run = Host::load(&dir).expect("load").run_frames(&times, W, H).expect("run");
    let diff = wrela_host::image::compare(&browser.frame, &run.frame).expect("same size");
    eprintln!("Chrome against the native host: mean {:.4}/255, max {}/255", diff.mean, diff.max);
    assert!(diff.mean <= 0.5, "mean difference {:.4}", diff.mean);
}
