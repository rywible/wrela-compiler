//! Frame and tick tests (language.md §10): `@test(frames: n) fn name(state: State)`, and
//! `@test(ticks: n)`. The program is built as a debug build is, with each such test exported,
//! and run on the native host's CPU (`wrela_host::CpuHost`), which checks every command a frame
//! records and answers storage and fetch requests, but has no GPU: `init`, then `n` frames at
//! 60 a second on a 640 × 480 screen (a program with a ticker ticks in lockstep with them,
//! #43 §2.3), or `n` ticks with no frames, then the test with the program's state. Each test
//! runs on a new instance, with storage of its own, and each call (`init`, a frame, a tick, the
//! test) gets a test's fuel. A test with `input: "script.json"` gets the script's input events
//! (runtime/abi `input`), each queued before the frame it's for, or as its tick's records; a
//! tick test's input can instead be a tick log (runtime/abi `ticks`), whose records its ticks
//! get, and whose state hashes each tick's must be.

use crate::build;
use crate::consts::{Fault, Item, failure};
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
    root: &Path,
    fuel: u64,
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
    let built = match wrela_host::CpuBuild::load_metered(&dir.0, fuel) {
        Ok(b) => b,
        Err(e) => {
            diags.push(Diagnostic::internal(format!("loading a frame test's build: {e}")));
            return (Vec::new(), diags);
        }
    };
    let mut results = Vec::new();
    for (i, &f) in tests.iter().enumerate() {
        let attrs = &checked.program.func(f).attrs;
        let run = match attrs.test_ticks {
            Some(n) => Run::Ticks(n),
            None => Run::Frames(attrs.test_frames.unwrap_or(1)),
        };
        let input = match &attrs.test_input {
            None => Input::Script(Vec::new()),
            Some((path, span)) => match read_input(root, path, matches!(run, Run::Ticks(_))) {
                Ok(s) => s,
                Err(why) => {
                    let d = Diagnostic::new(
                        codes::E0222,
                        *span,
                        format!("the test's input `{path}` {why}"),
                    )
                    .with_note("a script is a JSON array of input events, by the frame each arrives before or the tick it's a record of (runtime/abi `input`); a tick test's input may be a tick log instead (runtime/abi `ticks`)");
                    results.push((f, Some(d)));
                    continue;
                }
            },
        };
        let storage = dir.0.join(format!("storage.{i}"));
        let failed =
            run_one(&built, &storage, i, run, &input).map(|Failed { error, fault, phase }| {
                match fault {
                    Some(fault) => failure(
                        checked,
                        sources,
                        &out.lines,
                        Item::Test(f, fuel),
                        &fault,
                        Some(&phase),
                    ),
                    // Not a trap: a command the host can't carry out, say.
                    None => Diagnostic::new(
                        codes::E0706,
                        checked.program.func(f).span,
                        format!(
                            "the test `{}` failed {phase}: {error}",
                            checked.program.func(f).name
                        ),
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

/// What a test runs before it.
#[derive(Clone, Copy)]
enum Run {
    Frames(u32),
    Ticks(u32),
}

/// What a test's runs get: a script of input events, or (a tick test) a tick log.
enum Input {
    Script(Vec<wrela_host::Scripted>),
    Log(wrela_host::TickLog),
}

/// The input at `path` in the package at `root`, or why it can't be read: a tick log (for a
/// tick test, `ticks`) if its bytes start as one, else a script.
fn read_input(root: &Path, path: &str, ticks: bool) -> Result<Input, String> {
    if wrela_abi::check::path_problem(path).is_some() {
        return Err("must be a path inside the package, with `/` between its parts".into());
    }
    let bytes = std::fs::read(root.join(path)).map_err(|e| format!("can't be read: {e}"))?;
    if ticks && bytes.starts_with(&wrela_abi::ticks::MAGIC) {
        let log =
            wrela_host::TickLog::decode(&bytes).map_err(|e| format!("isn't a tick log: {e}"))?;
        return Ok(Input::Log(log));
    }
    let text = String::from_utf8(bytes).map_err(|_| "isn't UTF-8".to_string())?;
    wrela_host::parse_script(&text).map(Input::Script).map_err(|e| format!("isn't a script: {e}"))
}

/// Runs test `i`: `init`, its frames (and ticks in lockstep) or its ticks, with `input`, then
/// the test. Why it failed, if it did.
fn run_one(
    built: &wrela_host::CpuBuild,
    storage: &Path,
    i: usize,
    run: Run,
    input: &Input,
) -> Option<Failed> {
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
    let log = match input {
        Input::Log(log) => Some(log),
        Input::Script(_) => None,
    };
    let script: &[wrela_host::Scripted] = match input {
        Input::Script(s) => s,
        Input::Log(_) => &[],
    };
    host.want_hashes(log.is_some());
    if let Err(e) = host.init() {
        return failed(e, Some(&host), "in `init`".to_string());
    }
    let mismatch = |why: String| {
        Some(Failed { error: wrela_host::Error::Program(why), fault: None, phase: String::new() })
    };
    let after = match run {
        Run::Frames(frames) => {
            for k in 0..frames {
                if let Err(e) = host.lockstep_frame(k, FPS, WIDTH, HEIGHT, script) {
                    return failed(e, Some(&host), format!("in frame {k} of {frames}"));
                }
            }
            format!("after {frames} frames")
        }
        Run::Ticks(ticks) => {
            if host.ticker_hz().is_none() {
                return mismatch(
                    "in `init`: the program starts no ticker (`std::tick::start`)".into(),
                );
            }
            if let Some(log) = log {
                if log.wasm_hash != built.wasm_hash() || log.hz != host.ticker_hz().unwrap_or(0) {
                    return mismatch("its tick log is for another build, or another rate".into());
                }
                if host.reported_hash() != log.first {
                    return mismatch("the first world's state hash isn't its tick log's".into());
                }
                if log.ticks.len() < ticks as usize {
                    let n = log.ticks.len();
                    return mismatch(format!("its tick log has {n} ticks, not {ticks}"));
                }
            }
            for k in 0..ticks {
                match log {
                    Some(log) => host.push_raw_records(&log.ticks[k as usize].records),
                    None => host.push_records(wrela_abi::input::records_at(script, k)),
                }
                match host.tick(log.is_some()) {
                    Err(e) => return failed(e, Some(&host), format!("in tick {k} of {ticks}")),
                    Ok(t) => {
                        if let (Some(log), Some(h)) = (log, t.hash)
                            && log.ticks[k as usize].hash != h
                        {
                            return mismatch(format!(
                                "tick {k}'s state hash isn't its tick log's: the log has {}, the test {}",
                                wrela_abi::hash::hex(log.ticks[k as usize].hash),
                                wrela_abi::hash::hex(h)
                            ));
                        }
                    }
                }
            }
            format!("after {ticks} ticks")
        }
    };
    match host.call_export(&format!("test.{i}"), &[]) {
        Ok(_) => None,
        Err(e) => failed(e, Some(&host), after),
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
