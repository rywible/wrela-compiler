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
use wrela_sema::defs::{Golden, TestRun};
use wrela_sema::ty::FnId;

/// Frames a second, and the screen's size in pixels, that a frame test's frames get.
pub const FPS: f64 = 60.0;
pub const WIDTH: u32 = 640;
pub const HEIGHT: u32 = 480;

/// Runs frame and tick tests `tests` (§10), given the constants computed into `data`: each
/// test's failure, if it failed, with the errors building them found.
pub fn run(
    checked: &Checked,
    sources: &SourceMap,
    data: &BuildData,
    tests: &[FnId],
    root: &Path,
    fuel: u64,
    bless: bool,
) -> (Vec<crate::consts::Ran>, Vec<Diagnostic>) {
    let mut roots = wrela_lower::Roots::of(checked);
    roots.tests = tests.to_vec();
    let (lowered, mut diags) = wrela_lower::lower(checked, &roots, data, true);
    if has_errors(&diags) {
        return (Vec::new(), diags);
    }
    let out = build::emit(&lowered, sources, !data.no_simd, false);
    diags.extend(out.diagnostics);
    if has_errors(&diags) {
        return (Vec::new(), diags);
    }
    let dir = Scratch::new();
    for (path, bytes) in &out.files {
        if let Err(e) = std::fs::write(dir.0.join(path), bytes) {
            diags.push(Diagnostic::internal(format!("writing a test's build: {e}")));
            return (Vec::new(), diags);
        }
    }
    let built = match wrela_host::CpuBuild::load_metered(&dir.0, fuel) {
        Ok(b) => b,
        Err(e) => {
            diags.push(Diagnostic::internal(format!("loading a test's build: {e}")));
            return (Vec::new(), diags);
        }
    };
    let mut results = Vec::new();
    // The GPU tests share one host: each starts a new program on its device and pipelines.
    let mut gpu = GpuHost::default();
    for (i, &f) in tests.iter().enumerate() {
        let attrs = &checked.program.func(f).attrs;
        let run = attrs.test_run.unwrap_or(TestRun::Frames(1));
        let input = match &attrs.test_input {
            None => Input::Script(Vec::new()),
            Some((path, span)) => match read_input(root, path, matches!(run, TestRun::Ticks(_))) {
                Ok(s) => s,
                Err(why) => {
                    let d = Diagnostic::new(
                        codes::E0222,
                        *span,
                        format!("the test's input `{path}` {why}"),
                    )
                    .with_note("a script is a JSON array of input events, by the frame each arrives before or the tick it's a record of (runtime/abi `input`); a tick test's input may be a tick log instead (runtime/abi `ticks`)");
                    results.push((f, Some(d), wrela_host::Traced::default()));
                    continue;
                }
            },
        };
        let storage = dir.0.join(format!("storage.{i}"));
        let (ran, traced) = match (attrs.test_gpu, run, &input) {
            (true, TestRun::Frames(n), Input::Script(script)) => {
                let golden = attrs.test_golden.as_ref().map(|g| (g, root, bless));
                run_gpu(&mut gpu, &dir.0, &storage, i, n, script, golden)
            }
            _ => run_one(&built, &storage, i, run, &input),
        };
        let name = &checked.program.func(f).name;
        let failed = ran.map(|Failed { why, fault, phase }| match (fault, why) {
            (Some(fault), _) => {
                failure(checked, sources, &out.lines, Item::Test(f, fuel), &fault, Some(&phase))
            }
            (None, Why::Golden(why)) => Diagnostic::new(
                codes::E0706,
                attrs.test_golden.as_ref().map_or(checked.program.func(f).span, |g| g.span),
                format!("the test `{name}` failed {phase}: {why}"),
            )
            .with_note("a GPU's results differ a little from another's (§11): `within` is how far its screen may be from the golden, a mean in 8-bit steps, 0.5 unless the test says"),
            // Not a trap: a command the host can't carry out, say.
            (None, Why::Host(error)) => Diagnostic::new(
                codes::E0706,
                checked.program.func(f).span,
                format!("the test `{name}` failed {phase}: {error}"),
            )
            .with_note("a frame test passes unless its frames or the test fail (§10)"),
        });
        results.push((f, failed, traced));
    }
    (results, diags)
}

/// Why a test failed (the fault behind it, if it was a trap), and in which call.
struct Failed {
    why: Why,
    fault: Option<Fault>,
    phase: String,
}

/// The host's error, or its screen too far from its golden.
enum Why {
    Host(wrela_host::Error),
    Golden(String),
}

/// A test failed `phase` (where in its run) with `error`, the host's last failure `failure`
/// behind it.
fn failed(
    error: wrela_host::Error,
    failure: Option<&wrela_host::Failure>,
    phase: String,
) -> Option<Failed> {
    let fault = failure.map(|f| Fault {
        frames: f.frames.clone(),
        trap: f.trap,
        panic: f.panic.clone(),
        text: String::new(),
    });
    Some(Failed { why: Why::Host(error), fault, phase })
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

/// The native host the GPU frame tests of a run share (none yet, or none after one failed):
/// loading the build and opening the device and its pipelines once.
#[derive(Default)]
struct GpuHost(#[cfg(feature = "gpu")] Option<wrela_host::Host>);

/// Runs GPU frame test `i` (`@test(frames: n, gpu: true)`, §10) on the native host's GPU:
/// `init`, its `frames` with `script`'s events, each frame's readbacks answered before the next,
/// then the test. Why it failed, if it did. The GPU host meters no fuel: it runs the program as
/// `wrela run` does. Each test is a new program, nothing of the last one's left; the host is
/// `gpu`'s, loaded for the first, and loaded again after one that failed. What it printed and
/// timed comes with it. With a golden (and the package's root, and whether to bless it), the
/// screen after the frames is compared with it.
#[cfg(feature = "gpu")]
fn run_gpu(
    gpu: &mut GpuHost,
    dir: &Path,
    storage: &Path,
    i: usize,
    frames: u32,
    script: &[wrela_host::Scripted],
    golden: Option<(&Golden, &Path, bool)>,
) -> (Option<Failed>, wrela_host::Traced) {
    let _ = std::fs::create_dir_all(storage);
    let options = wrela_host::Options {
        storage: Some(storage.to_path_buf()),
        workers: 1,
        quiet: true,
        defer_init: true,
        ..Default::default()
    };
    let started = match gpu.0.take() {
        Some(mut host) => host.restart_with(&options).map(|()| host),
        None => wrela_host::Host::load_with(dir, &options),
    };
    let mut host = match started {
        Ok(h) => h,
        Err(e) => return (failed(e, None, "as it started".to_string()), Default::default()),
    };
    let ran = run_gpu_frames(&mut host, i, frames, script, golden);
    let traced = host.trace().take();
    if ran.is_none() {
        gpu.0 = Some(host);
    }
    (ran, traced)
}

/// [`run_gpu`]'s test, on its host.
#[cfg(feature = "gpu")]
fn run_gpu_frames(
    host: &mut wrela_host::Host,
    i: usize,
    frames: u32,
    script: &[wrela_host::Scripted],
    golden: Option<(&Golden, &Path, bool)>,
) -> Option<Failed> {
    if let Err(e) = host.init() {
        return failed(e, host.last_failure(), "in `init`".to_string());
    }
    for k in 0..frames {
        if let Err(e) = host.lockstep_frame(k, FPS, WIDTH, HEIGHT, script) {
            return failed(e, host.last_failure(), format!("in frame {k} of {frames}"));
        }
    }
    if let Some((g, root, bless)) = golden {
        let after = format!("after {frames} frames");
        let screen = match host.read_screen() {
            Ok(s) => s,
            Err(e) => return failed(e, None, after),
        };
        if let Err(why) = against_golden(g, root, &screen, bless) {
            return Some(Failed { why: Why::Golden(why), fault: None, phase: after });
        }
    }
    match host.call_export(&format!("test.{i}"), &[]) {
        Ok(_) => None,
        Err(e) => failed(e, host.last_failure(), format!("after {frames} frames")),
    }
}

/// The screen after a GPU frame test's frames against its golden (§10): within its mean
/// difference, or why not. With `bless`, the golden is written from the screen first.
#[cfg(feature = "gpu")]
fn against_golden(g: &Golden, root: &Path, screen: &[u8], bless: bool) -> Result<(), String> {
    let path = &g.path;
    if wrela_abi::check::path_problem(path).is_some() {
        return Err(format!(
            "its golden `{path}` must be a path inside the package, with `/` between its parts"
        ));
    }
    let full = root.join(path);
    if bless {
        if let Some(dir) = full.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        wrela_host::image::write_png(&full, WIDTH, HEIGHT, screen)
            .map_err(|e| format!("writing its golden `{path}`: {e}"))?;
    }
    let (w, h, want) = wrela_host::image::read_png(&full).map_err(|_| {
        format!("it has no golden `{path}` to compare the screen with: `wrela test --bless` writes it from this machine's")
    })?;
    if (w, h) != (WIDTH, HEIGHT) {
        return Err(format!(
            "its golden `{path}` is {w}×{h}, and a frame test's screen is {WIDTH}×{HEIGHT}"
        ));
    }
    let d = wrela_host::image::compare(screen, &want)?;
    if d.mean > g.within {
        return Err(format!(
            "the screen is {:.3}/255 from its golden `{path}` on average, more than the {} it may be (a channel differs by {}/255 at most; {} of {} channels differ)",
            d.mean, g.within, d.max, d.differing, d.channels
        ));
    }
    Ok(())
}

/// [`run_gpu`], in a compiler built without the GPU host: the test can't run.
#[cfg(not(feature = "gpu"))]
fn run_gpu(
    _: &mut GpuHost,
    _: &Path,
    _: &Path,
    _: usize,
    _: u32,
    _: &[wrela_host::Scripted],
    _: Option<(&Golden, &Path, bool)>,
) -> (Option<Failed>, wrela_host::Traced) {
    let why = "its frames run on the GPU, and this build of the compiler has no GPU host";
    let failed = Failed {
        why: Why::Host(wrela_host::Error::Program(why.into())),
        fault: None,
        phase: String::new(),
    };
    (Some(failed), Default::default())
}

/// Runs test `i`: `init`, its frames (and ticks in lockstep) or its ticks, with `input`, then
/// the test. Why it failed, if it did, and what it printed and timed.
fn run_one(
    built: &wrela_host::CpuBuild,
    storage: &Path,
    i: usize,
    run: TestRun,
    input: &Input,
) -> (Option<Failed>, wrela_host::Traced) {
    let _ = std::fs::create_dir_all(storage);
    // One thread: a parallel job's chunks all run on it, and the result is the same (§6.12).
    match built.instantiate_in(1, Some(storage)) {
        Ok(mut host) => {
            let failed = run_on(&mut host, i, run, input);
            (failed, host.trace().take())
        }
        Err(e) => (failed(e, None, "as it started".to_string()), Default::default()),
    }
}

/// [`run_one`] on its host.
fn run_on(host: &mut wrela_host::CpuHost, i: usize, run: TestRun, input: &Input) -> Option<Failed> {
    let (log, script): (_, &[wrela_host::Scripted]) = match input {
        Input::Log(log) => (Some(log), &[]),
        Input::Script(s) => (None, s),
    };
    host.want_hashes(log.is_some());
    if let Err(e) = host.init() {
        return failed(e, host.last_failure(), "in `init`".to_string());
    }
    let mismatch = |why: String| {
        Some(Failed {
            why: Why::Host(wrela_host::Error::Program(why)),
            fault: None,
            phase: String::new(),
        })
    };
    let after = match run {
        TestRun::Frames(frames) => {
            for k in 0..frames {
                if let Err(e) = host.lockstep_frame(k, FPS, WIDTH, HEIGHT, script) {
                    return failed(e, host.last_failure(), format!("in frame {k} of {frames}"));
                }
            }
            format!("after {frames} frames")
        }
        TestRun::Ticks(ticks) => {
            if host.ticker_hz().is_none() {
                return mismatch(
                    "in `init`: the program starts no ticker (`std::tick::start`)".into(),
                );
            }
            let in_tick = |k| format!("in tick {k} of {ticks}");
            match log {
                // The log's records, each tick's state hash checked against the log's. A log for
                // another build or rate says so first, whatever its length.
                Some(log) => {
                    if let Err(e) = host.replay_ticks(log, ticks) {
                        return match e {
                            wrela_host::ReplayError::Failed { tick, error } => failed(
                                error,
                                host.last_failure(),
                                tick.map_or_else(String::new, in_tick),
                            ),
                            why => mismatch(format!("its tick log doesn't replay: {why}")),
                        };
                    }
                    if log.ticks.len() < ticks as usize {
                        let n = log.ticks.len();
                        return mismatch(format!("its tick log has {n} ticks, not {ticks}"));
                    }
                }
                None => {
                    for k in 0..ticks {
                        host.push_records(wrela_abi::input::records_at(script, k));
                        if let Err(e) = host.tick() {
                            return failed(e, host.last_failure(), in_tick(k));
                        }
                    }
                }
            }
            format!("after {ticks} ticks")
        }
    };
    match host.call_export(&format!("test.{i}"), &[]) {
        Ok(_) => None,
        Err(e) => failed(e, host.last_failure(), after),
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
