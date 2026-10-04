//! Frame tests (language.md §10): `@test(frames: n) fn name(state: State)`. The program is
//! built as a debug build is, with each frame test exported, and run on the native host's CPU
//! (`wrela_host::CpuHost`), which checks every command a frame records and answers storage
//! and fetch requests, but has no GPU: `init`, then `n` frames at 60 a second on a 640 × 480
//! screen, then the test with the program's state. Each test runs on a new instance, with
//! storage of its own, and each call (`init`, a frame, the test) gets a test's fuel.

use crate::build;
use crate::consts::{Fault, Item, TEST_FUEL, failure};
use std::path::{Path, PathBuf};
use wrela_diag::{Diagnostic, SourceMap, codes, has_errors};
use wrela_lower::BuildData;
use wrela_sema::Checked;
use wrela_sema::ty::FnId;

/// Frames a second, and the screen's size in pixels, that a frame test's frames get.
pub const FPS: f64 = 60.0;
pub const WIDTH: u32 = 640;
pub const HEIGHT: u32 = 480;

/// Runs frame tests `tests` (§10), given the constants computed into `data`: each test's
/// failure, if it failed, with the errors building them found.
pub fn run(
    checked: &Checked,
    sources: &SourceMap,
    data: &BuildData,
    tests: &[FnId],
) -> (Vec<(FnId, Option<Diagnostic>)>, Vec<Diagnostic>) {
    let mut roots = wrela_lower::Roots::of(checked);
    roots.tests = tests.to_vec();
    let (lowered, mut diags) = wrela_lower::lower(checked, &roots, data, true);
    if has_errors(&diags) {
        return (Vec::new(), diags);
    }
    let out = build::emit(&lowered, sources, !data.no_simd);
    diags.extend(out.diagnostics);
    if has_errors(&diags) {
        return (Vec::new(), diags);
    }
    let dir = Scratch::new();
    for (path, bytes) in &out.files {
        if let Err(e) = std::fs::write(dir.0.join(path), bytes) {
            diags.push(Diagnostic::internal(format!("writing a frame test's build: {e}")));
            return (Vec::new(), diags);
        }
    }
    let built = match wrela_host::CpuBuild::load_metered(&dir.0, TEST_FUEL) {
        Ok(b) => b,
        Err(e) => {
            diags.push(Diagnostic::internal(format!("loading a frame test's build: {e}")));
            return (Vec::new(), diags);
        }
    };
    let mut results = Vec::new();
    for (i, &f) in tests.iter().enumerate() {
        let frames = checked.program.func(f).attrs.test_frames.unwrap_or(1);
        let storage = dir.0.join(format!("storage.{i}"));
        let failed = run_one(&built, &storage, i, frames).map(|Failed { error, fault, phase }| {
            match fault {
                Some(fault) => {
                    failure(checked, sources, &out.lines, Item::Test(f), &fault, Some(&phase))
                }
                // Not a trap: a command the host can't carry out, say.
                None => Diagnostic::new(
                    codes::E0706,
                    checked.program.func(f).span,
                    format!("the test `{}` failed {phase}: {error}", checked.program.func(f).name),
                )
                .with_note("a frame test passes unless its frames or the test fail (§10)"),
            }
        });
        results.push((f, failed));
    }
    (results, diags)
}

/// Why a frame test failed: the host's error, the fault behind it if it was a trap, and in
/// which call.
struct Failed {
    error: wrela_host::Error,
    fault: Option<Fault>,
    phase: String,
}

/// Runs frame test `i`: `init`, `frames` frames, then the test. Why it failed, if it did.
fn run_one(built: &wrela_host::CpuBuild, storage: &Path, i: usize, frames: u32) -> Option<Failed> {
    let failed = |error, host: Option<&wrela_host::CpuHost>, phase: String| {
        let fault = host.and_then(|h| h.last_failure()).map(|f| Fault {
            frames: f.frames.clone(),
            trap: f.trap,
            panic: f.panic.clone(),
            text: String::new(),
        });
        Some(Failed { error, fault, phase })
    };
    let _ = std::fs::create_dir_all(storage);
    // One thread: a parallel job's chunks all run on it, and the result is the same (§6.12).
    let mut host = match built.instantiate_in(1, Some(storage)) {
        Ok(h) => h,
        Err(e) => return failed(e, None, "as it started".to_string()),
    };
    if let Err(e) = host.init() {
        return failed(e, Some(&host), "in `init`".to_string());
    }
    for k in 0..frames {
        let time = wrela_host::frame_time(k, FPS);
        if let Err(e) = host.frame(time, WIDTH, HEIGHT) {
            return failed(e, Some(&host), format!("in frame {k} of {frames}"));
        }
    }
    match host.call_export(&format!("test.{i}"), &[]) {
        Ok(_) => None,
        Err(e) => failed(e, Some(&host), format!("after {frames} frames")),
    }
}

/// A directory of its own for a run's build and storage, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("wrela-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
