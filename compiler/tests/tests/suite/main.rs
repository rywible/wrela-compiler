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
mod input;
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
mod warnings;
mod workgroup;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
    built_lifted(pkg, &[])
}

/// [`built`], lifting the packages `lift` names (language.md §22; none: a normal build).
fn built_lifted(pkg: &str, lift: &[&str]) -> PathBuf {
    static BUILDS: Mutex<BTreeMap<String, Arc<OnceLock<()>>>> = Mutex::new(BTreeMap::new());
    let src = wrela_tests::repo_root().join("compiler/tests").join(pkg);
    // The build's directory is under the scratch directory whatever the package's path (a
    // `../` in it would otherwise climb out, into the source tree).
    let mut name = pkg.trim_start_matches("../").replace("../", "").replace('/', "-");
    if !lift.is_empty() {
        name = format!("{name}-lifted-{}", lift.join("-"));
    }
    // One build per package and lift: a second test waits for the first's build rather than
    // racing it, and other builds run at the same time.
    let once =
        BUILDS.lock().unwrap_or_else(|p| p.into_inner()).entry(name.clone()).or_default().clone();
    once.get_or_init(|| {
        lift::build_into(&name, &src, lift);
    });
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}
