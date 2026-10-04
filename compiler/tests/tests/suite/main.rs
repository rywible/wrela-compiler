//! The compiler's end-to-end tests, in one binary: each module is one suite (see its docs).

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
mod language;
mod limits;
mod lipschitz;
mod math;
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
mod test_items;
mod warnings;
mod workgroup;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

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

/// `main.wrela`'s text with an empty `frame` added at the end if it has none, so a test of
/// something else doesn't trip over E0703. Nothing before the end moves.
fn with_frame(text: &str) -> String {
    if text.contains("fn frame(") {
        return text.to_string();
    }
    format!("{text}\npub fn frame(time: f32, width: u32, height: u32) {{}}\n")
}

/// Builds the test package `compiler/tests/<pkg>` once per test process (the suites share the
/// build), and returns the build's directory. Panics with the errors if it doesn't build.
fn built(pkg: &str) -> PathBuf {
    static BUILDS: Mutex<BTreeMap<String, Arc<OnceLock<()>>>> = Mutex::new(BTreeMap::new());
    let src = wrela_tests::repo_root().join("compiler/tests").join(pkg);
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(pkg);
    // One build per package: a second test waits for the first's build rather than racing it,
    // and other packages build at the same time.
    let once =
        BUILDS.lock().unwrap_or_else(|p| p.into_inner()).entry(pkg.into()).or_default().clone();
    once.get_or_init(|| wrela_tests::must_build(&src, &out));
    out
}
