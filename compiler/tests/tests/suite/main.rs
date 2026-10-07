//! The compiler's end-to-end tests, in one binary: each module is one suite (see its docs).

// macOS's allocator spent 30% of the suite's time in malloc and free, its threads contending.
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod audio;
mod bounds;
mod buffers;
mod channels;
mod closed_list;
mod codes;
mod conformance;
mod derive;
mod diagnostics;
mod doc_examples;
mod encodings;
mod explain;
mod format;
mod fuzz;
mod gpu_limits;
mod grazer;
mod hello_field;
mod herd;
mod input;
mod keys;
mod language;
mod lift;
mod limits;
mod lipschitz;
mod loft;
mod math;
mod measures;
mod noise;
mod numerics;
mod parallel;
mod queries;
mod render;
mod renderer;
mod reproducible;
mod requests;
mod run_pass;
mod simd;
mod sketches;
mod snapshots;
mod spike13;
mod stage;
mod std_docs;
mod studio;
mod subjects;
mod sweep;
mod test_items;
mod text;
mod threads;
mod warnings;
mod workgroup;

use std::path::{Path, PathBuf};
use wrela_host::{CpuBuild, ReplayError, TickLog};

/// An empty directory `name` (a relative path) for a test's files, under the binary's scratch
/// directory.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("make a scratch directory");
    dir
}

/// A one-file package: [`scratch`] directory `name` with `text` as its `main.wrela`, plus an
/// empty `frame` if `text` has none (every program exports one, E0703).
fn package(name: &str, text: &str) -> PathBuf {
    let dir = scratch(name);
    std::fs::write(dir.join("main.wrela"), with_frame(text)).expect("write main.wrela");
    dir
}

/// [`package`], named `name` in its manifest, which depends on the engine (engine/).
fn engine_package(dir: &str, name: &str, text: &str) -> PathBuf {
    let dir = package(dir, text);
    let engine = wrela_tests::repo_root().join("engine").display().to_string();
    let manifest = format!(
        "[package]\nname = \"{name}\"\n\n[dependencies]\nengine = {{ path = {engine:?} }}\n"
    );
    std::fs::write(dir.join("wrela.toml"), manifest).expect("write wrela.toml");
    dir
}

/// `main.wrela`'s text with an empty `frame` added at the end if it has none, so a test of
/// something else doesn't trip over E0703. Nothing before the end moves.
fn with_frame(text: &str) -> String {
    if text.contains("fn frame(") {
        return text.to_string();
    }
    format!("{text}\npub fn frame(time: f32, width: u32, height: u32) {{}}\n")
}

/// Runs the package's tests as `wrela test` does, which must all pass: panics with the errors
/// or the failures if not. How many ran.
fn tests_pass(dir: &Path) -> usize {
    let out = wrela_driver::test(dir, None);
    if !out.passed() {
        let errors: Vec<_> = out.diagnostics.iter().filter(|d| d.is_error()).collect();
        let failed: Vec<String> = out
            .results
            .iter()
            .filter_map(|r| r.failure.as_ref().map(|d| format!("{}: {}", r.name, d.message)))
            .collect();
        panic!("errors: {errors:#?}\nfailed: {failed:#?}");
    }
    out.results.len()
}

/// Builds the test package `compiler/tests/<pkg>` once per test process (the suites share the
/// build), and returns the build's directory. Panics with the errors if it doesn't build.
fn built(pkg: &str) -> PathBuf {
    built_as(pkg, &[], false)
}

/// [`built`], lifting the packages `lift` names (language.md §22; none: a normal build).
fn built_lifted(pkg: &str, lift: &[&str]) -> PathBuf {
    built_as(pkg, lift, false)
}

/// [`built`] in debug mode (language.md §11's checks).
fn built_debug(pkg: &str) -> PathBuf {
    built_as(pkg, &[], true)
}

fn built_as(pkg: &str, lift: &[&str], debug: bool) -> PathBuf {
    // One build per package, lift and mode: a second test waits for the first's build rather
    // than racing it, and other builds run at the same time.
    static BUILDS: wrela_tests::OncePerKey = wrela_tests::OncePerKey::new();
    let src = wrela_tests::repo_root().join("compiler/tests").join(pkg);
    // The build's directory is under the scratch directory whatever the package's path (a
    // `../` in it would otherwise climb out, into the source tree).
    let mut name = pkg.trim_start_matches("../").replace("../", "").replace('/', "-");
    if !lift.is_empty() {
        name = format!("{name}-lifted-{}", lift.join("-"));
    }
    if debug {
        name.push_str("-debug");
    }
    BUILDS.run(&name, || {
        lift::build_into(&name, &src, lift, debug);
    });
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

/// The newest `n` buffers of the test package `pkg` after its first frame on the GPU (64 × 64),
/// read back, oldest first.
fn first_frame_buffers(pkg: &str, n: usize) -> Vec<Vec<u8>> {
    let mut gpu = wrela_host::Host::load(built(pkg)).expect("load");
    gpu.frame(0.0, 64, 64).expect("a frame");
    let b = gpu.buffers();
    b[b.len() - n..].iter().map(|&h| gpu.read_buffer(h).expect("read")).collect()
}

/// Checks that a buffer of the CPU's values and one of the GPU's have the same words, and that
/// the CPU's aren't all zero.
fn same_words(what: &str, cpu: &[u8], gpu: &[u8]) {
    assert_eq!(cpu.len(), gpu.len(), "{what}");
    assert!(cpu.iter().any(|&x| x != 0), "{what}: the CPU's values are all zero");
    let differ = cpu.chunks(4).zip(gpu.chunks(4)).position(|(a, b)| a != b);
    assert_eq!(differ, None, "{what}: the CPU's and the GPU's words differ (the first)");
}

/// A tick log, through its bytes: it reads back, and replays to the same hashes with 1, 2 and 8
/// threads. The log read back.
fn replays_with_any_helpers(build: &CpuBuild, log: &TickLog) -> TickLog {
    let read = TickLog::decode(&log.encode()).expect("reads back");
    for workers in [1, 2, 8] {
        let ticks = build.replay(&read, workers).expect("replays");
        assert_eq!(ticks as usize, log.ticks.len(), "{workers} threads");
    }
    read
}

/// `log` with tick `k`'s first record, a key going down, made one going up.
fn flipped(log: &TickLog, k: usize) -> TickLog {
    let mut changed = log.clone();
    changed.ticks[k].records[0][0] = wrela_abi::input::EventKind::KeyUp as u8;
    changed
}

/// [`flipped`]'s log fails to replay at tick `k`, its hash the first that differs.
fn a_flipped_record_fails_at_its_tick(build: &CpuBuild, log: &TickLog, k: usize) {
    match build.replay(&flipped(log, k), 2) {
        Err(ReplayError::Tick { tick, .. }) => assert_eq!(tick as usize, k),
        other => panic!("expected tick {k} to differ: {other:?}"),
    }
}

/// `wrela-host`, built for x86-64 without the GPU, to run under Rosetta: its path.
fn x86_host() -> PathBuf {
    let root = wrela_tests::repo_root();
    let status = std::process::Command::new("cargo")
        .args(["build", "--release", "-p", "wrela-host", "--no-default-features"])
        .args(["--target", "x86_64-apple-darwin"])
        .current_dir(&root)
        .status()
        .expect("cargo runs");
    assert!(status.success(), "building wrela-host for x86-64 failed");
    root.join("target/x86_64-apple-darwin/release/wrela-host")
}

/// `wrela-host --replay <log> --no-gpu <build>` for x86-64, under Rosetta: its output, and
/// whether it passed.
fn replay_on_x86(host: &Path, log: &Path, build: &Path) -> (bool, String) {
    let out = std::process::Command::new("arch")
        .arg("-x86_64")
        .arg(host)
        .arg("--replay")
        .arg(log)
        .arg("--no-gpu")
        .arg(build)
        .output()
        .expect("arch -x86_64 runs (Rosetta 2)");
    let text =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}
