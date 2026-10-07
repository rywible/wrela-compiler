//! AC1: examples/hello-field end to end. `wrela build` output runs in headless Chrome (the
//! standard browser runtime, via tools/headless.py) and in the native host; both give the same
//! state hash, both frames are within a mean of 0.5/255 of the checked-in golden frame
//! (fixtures/hello-field.png, from the native host), and fixed pixels match the same shading
//! evaluated on the CPU (`probe`, in WASM).
//!
//! These need a GPU, and the browser test Chrome stable and python3:
//! `cargo test -p wrela-tests --test suite hello_field:: -- --ignored --nocapture`. `WRELA_BLESS=1`
//! rewrites the golden from the native host.

use std::path::{Path, PathBuf};
use std::process::Command;
use wrela_host::{Host, RunResult, Value, frame_time, image};
use wrela_tests::{must_build, repo_root};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const FRAMES: u32 = 60;
const FPS: f64 = 60.0;
/// The largest mean absolute difference from the golden, in 8-bit steps.
const MEAN_LIMIT: f64 = 0.5;
/// The largest difference between a GPU pixel and the CPU's evaluation of it, in 8-bit steps.
const PROBE_LIMIT: f64 = 2.0;

/// The build, in a directory under the repo root (tools/headless.py serves the repo), so it's
/// also the browser's page.
fn page(name: &str) -> (PathBuf, String) {
    let rel = format!("target/tmp/hello-field-{name}");
    let dir = repo_root().join(&rel);
    let _ = std::fs::remove_dir_all(&dir);
    must_build(&repo_root().join("examples/hello-field"), &dir);
    std::fs::create_dir_all(dir.join("results")).expect("results dir");
    (dir, rel)
}

fn golden() -> PathBuf {
    repo_root().join("compiler/tests/fixtures/hello-field.png")
}

fn times() -> Vec<f32> {
    (0..FRAMES).map(|i| frame_time(i, FPS)).collect()
}

fn native(dir: &Path) -> RunResult {
    Host::load(dir).expect("load").run_frames(&times(), WIDTH, HEIGHT).expect("run")
}

/// Checks a frame against the golden; returns the mean difference.
fn against_golden(frame: &[u8], who: &str) -> f64 {
    let (w, h, gold) = image::read_png(&golden()).expect("the golden frame");
    assert_eq!((w, h), (WIDTH, HEIGHT), "the golden is {w}x{h}");
    let diff = image::compare(frame, &gold).expect("same size");
    eprintln!(
        "{who} against the golden: mean {:.4}/255, max {}/255, {} of {} channels differ",
        diff.mean, diff.max, diff.differing, diff.channels
    );
    assert!(diff.mean <= MEAN_LIMIT, "{who}: mean difference {:.4} over {MEAN_LIMIT}", diff.mean);
    diff.mean
}

#[test]
#[ignore = "needs a GPU"]
fn the_native_frame_matches_the_golden_and_the_cpu() {
    let (dir, _) = page("native");
    let run = native(&dir);
    if std::env::var_os("WRELA_BLESS").is_some() {
        run.write_png(golden()).expect("write the golden");
        eprintln!("wrote {}", golden().display());
    }
    against_golden(&run.frame, "the native host");

    // Fixed pixels, on an 8×8 grid, against the same shading on the CPU.
    let mut host = Host::load(&dir).expect("load");
    let t = frame_time(FRAMES - 1, FPS);
    let mut worst = 0.0f64;
    for gy in 0..8 {
        for gx in 0..8 {
            let (x, y) = (WIDTH * (2 * gx + 1) / 16, HEIGHT * (2 * gy + 1) / 16);
            let args = [
                Value::F32(x as f32 + 0.5),
                Value::F32(y as f32 + 0.5),
                Value::F32(t),
                Value::I32(WIDTH as i32),
                Value::I32(HEIGHT as i32),
            ];
            let cpu = match host.call_export("probe", &args).expect("probe").as_slice() {
                [Value::F32(r), Value::F32(g), Value::F32(b), Value::F32(a)] => [*r, *g, *b, *a],
                other => panic!("probe returned {other:?}"),
            };
            let at = 4 * (y * WIDTH + x) as usize;
            for (c, v) in cpu.iter().enumerate() {
                let want = f64::from(v.clamp(0.0, 1.0)) * 255.0;
                let d = (want - f64::from(run.frame[at + c])).abs();
                assert!(
                    d <= PROBE_LIMIT,
                    "pixel ({x}, {y}) channel {c}: GPU {}, CPU {want:.2}",
                    run.frame[at + c]
                );
                worst = worst.max(d);
            }
        }
    }
    eprintln!("64 pixels match the CPU's evaluation within {worst:.2}/255");
}

#[test]
#[ignore = "long: needs Chrome, python3 and a GPU"]
fn the_browser_matches_the_golden_and_the_native_host() {
    let (dir, rel) = page("browser");
    // The browser first: this thread holds no GPU lock while Chrome runs (Chrome waits for
    // another thread's).
    let fragment = format!("#test&frames={FRAMES}&width={WIDTH}&height={HEIGHT}&fps={FPS}");
    let status = Command::new("python3")
        .arg(repo_root().join("tools/headless.py"))
        .args([rel.as_str(), &fragment, "300"])
        .current_dir(repo_root())
        .status()
        .expect("python3 runs tools/headless.py");
    assert!(status.success(), "the browser run failed; see {rel}/results/console.log");
    let results = dir.join("results");
    let browser_hash = std::fs::read_to_string(results.join("hash.txt")).expect("hash.txt");
    let browser_frame = std::fs::read(results.join("frame.rgba")).expect("frame.rgba");

    let run = native(&dir);
    assert_eq!(browser_hash.trim(), run.hash_hex(), "the hosts' state hashes differ");
    eprintln!("state hash {} in Chrome and in the native host", run.hash_hex());
    against_golden(&browser_frame, "headless Chrome");
    let diff = image::compare(&browser_frame, &run.frame).expect("same size");
    eprintln!("Chrome against the native host: mean {:.4}/255, max {}/255", diff.mean, diff.max);
}

/// How long loading the build may take: compiling its WASM and creating its pipelines (the
/// shader compile dominates, and grows with the WGSL the compiler emits). Drivers cache compiled
/// shaders, so after the first run this mostly measures a warm load.
const LOAD_BUDGET_SECONDS: f64 = 1.0;

#[test]
#[ignore = "long: needs a GPU"]
fn loading_creates_the_pipelines_within_budget() {
    let (dir, _) = page("load");
    // Held across the loads (each takes it again), so the time doesn't count a wait for it.
    let _gpu = wrela_host::lock::GpuLock::acquire("wrela-tests: load budget").expect("lock");
    let mut slowest = 0.0f64;
    for _ in 0..3 {
        let started = std::time::Instant::now();
        let host = Host::load(&dir).expect("load");
        slowest = slowest.max(started.elapsed().as_secs_f64());
        drop(host);
    }
    eprintln!("hello-field loads in at most {slowest:.3} s (budget {LOAD_BUDGET_SECONDS} s)");
    // A debug build's wgpu validates as it goes (its first device takes seconds to open), and
    // its wasmtime compiles slowly: the budget is the release host's (`tools/check.sh --long`
    // runs the GPU tests in release).
    if !cfg!(debug_assertions) {
        assert!(slowest <= LOAD_BUDGET_SECONDS, "loading took {slowest:.3} s");
    }
}
