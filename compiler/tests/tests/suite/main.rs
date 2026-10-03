//! The compiler's end-to-end tests, in one binary: each module is one suite (see its docs).

mod buffers;
mod codes;
mod conformance;
mod derive;
mod diagnostics;
mod doc_examples;
mod fuzz;
mod grazer;
mod hello_field;
mod language;
mod limits;
mod math;
mod numerics;
mod render;
mod reproducible;
mod run_pass;
mod warnings;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Mutex;

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
    static DONE: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(pkg);
    // Held while building, so a second test waits for the first's build rather than racing it.
    let mut done = DONE.lock().unwrap_or_else(|p| p.into_inner());
    if !done.contains(pkg) {
        wrela_tests::must_build(&wrela_tests::repo_root().join("compiler/tests").join(pkg), &out);
        done.insert(pkg.to_string());
    }
    out
}
