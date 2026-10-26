//! Frame and tick tests (language.md §13): `@test(frames: n) fn name(state: State)`, and
//! `@test(ticks: n)`. The program is built as a debug build is, with each such test exported,
//! and run on the native host's CPU (`wrela_host::CpuHost`), which checks every command a frame
//! records and answers storage and fetch requests, but has no GPU: `init`, then `n` frames at
//! 60 a second on a 640 × 480 screen (a program with a ticker ticks in lockstep with them,
//! #43 §2.3), or `n` ticks with no frames, then the test with the program's state. Each test
//! runs on a new instance, with storage of its own, and each call (`init`, a frame, a tick, the
//! test) gets a test's fuel. A test with `input: NAME`, a `std::input::Script` constant, or
//! `input: "path"`, a file of the package in runtime/abi `input`'s script format, gets the
//! script's input events, each queued before the frame it's for, or as its tick's records; a
//! tick test's file can instead be a tick log (runtime/abi `ticks`), whose records its ticks
//! get, and whose state hashes each tick's must be.

use crate::build;
use crate::consts::{Fault, Item, failure};
use std::path::Path;
use wrela_diag::{Diagnostic, SourceMap, codes, has_errors};
use wrela_lower::BuildData;
use wrela_sema::Checked;
use wrela_sema::defs::{Golden, TestInput, TestRun};
use wrela_sema::ty::FnId;

/// Frames a second, and the screen's size in pixels, that a frame test's frames get.
pub const FPS: f64 = 60.0;
pub const WIDTH: u32 = 640;
pub const HEIGHT: u32 = 480;

/// Runs frame and tick tests `tests` (§13), given the constants computed into `data`: each
/// test's failure, if it failed, with the errors building them found.
#[allow(clippy::too_many_arguments)]
pub fn run(
    checked: &Checked,
    sources: &SourceMap,
    data: &BuildData,
    tests: &[FnId],
    root: &Path,
    fuel: u64,
    bless: bool,
    reach: &crate::reach::Reach,
) -> (Vec<crate::consts::Ran>, Vec<Diagnostic>) {
    let mut roots = wrela_lower::Roots::of(checked);
    roots.tests = tests.to_vec();
    let mut full_build = tests.iter().any(|&f| checked.program.func(f).attrs.test_gpu);
    let (mut lowered, mut diags) = if full_build {
        wrela_lower::lower(checked, &roots, data, true)
    } else {
        wrela_lower::lower_for_cpu(checked, &roots, data)
    };
    if has_errors(&diags) {
        return (Vec::new(), diags);
    }
    // A CPU test can fetch the build's own manifest, shaders or browser files. Keep the full
    // artifact set whenever its optimized code can fetch; looking at the lowered code avoids
    // counting std's unused fetch function or a branch removed by `test_build()`.
    if !full_build && fetches(&lowered.cpu) {
        full_build = true;
        (lowered, diags) = wrela_lower::lower(checked, &roots, data, true);
        if has_errors(&diags) {
            return (Vec::new(), diags);
        }
    }
    let out = if full_build {
        build::emit(&lowered, sources, !data.no_simd, false, &[], false)
    } else {
        build::emit_for_cpu(&lowered, sources, !data.no_simd)
    };
    diags.extend(out.diagnostics);
    if has_errors(&diags) {
        return (Vec::new(), diags);
    }
    let written = crate::Scratch::new("wrela-test").and_then(|dir| {
        for (path, bytes) in &out.files {
            std::fs::write(dir.0.join(path), bytes).map_err(|e| e.to_string())?;
        }
        Ok(dir)
    });
    let dir = match written {
        Ok(dir) => dir,
        Err(e) => {
            diags.push(Diagnostic::internal(format!("writing a test's build: {e}")));
            return (Vec::new(), diags);
        }
    };
    let loaded = if full_build {
        wrela_host::CpuBuild::load_metered(&dir.0, fuel)
    } else {
        wrela_host::CpuBuild::load_metered_cpu(&dir.0, fuel)
    };
    let built = match loaded {
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
        let ticks = matches!(run, TestRun::Ticks(_));
        let input = match &attrs.test_input {
            None => Input::Script(Vec::new(), "[]".to_string()),
            Some((input, span)) => match input_of(checked, data, root, input, ticks, reach) {
                Ok(s) => s,
                Err((shown, why)) => {
                    let d = Diagnostic::new(
                        codes::E0222,
                        *span,
                        format!("the test's input {shown} {why}"),
                    )
                    .with_note("a script is a JSON array of input events, by the frame each arrives before or the tick it's a record of (runtime/abi `input`), which a `std::input::Script` constant holds; a tick test's file may be a tick log instead (runtime/abi `ticks`)");
                    results.push((f, Some(d), wrela_host::Traced::default()));
                    continue;
                }
            },
        };
        let storage = dir.0.join(format!("storage.{i}"));
        let (ran, traced) = match (attrs.test_gpu, run, &input) {
            (true, TestRun::Frames(n), Input::Script(script, json)) => {
                let golden = attrs.test_golden.as_ref().map(|g| (g, root, bless, reach));
                let both = attrs.test_both.then_some(json.as_str());
                let given = Given { script, hold: &attrs.test_hold, both };
                run_gpu(&mut gpu, &dir.0, &storage, i, n, given, golden)
            }
            _ => run_one(&built, &storage, i, run, &input, &attrs.test_hold),
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
            .with_note("a GPU's results differ a little from another's (§11): `within` is how far its screen may be from the golden, a mean in 8-bit steps, 0.5 unless the test says")
            .with_help("compare the images it wrote under `build/goldens/`; if the new frame is right, `wrela test --bless` writes it as the golden"),
            // Not a trap: a command the host can't carry out, say.
            (None, Why::Host(error)) => Diagnostic::new(
                codes::E0706,
                checked.program.func(f).span,
                format!("the test `{name}` failed {phase}: {error}"),
            )
            .with_note("a frame test passes unless its frames or the test fail (§13)")
            .with_help("fix the command the host refused; `wrela trace <package> --frames n` runs the frames up to it"),
        });
        results.push((f, failed, traced));
    }
    (results, diags)
}

/// Whether the emitted CPU code can fetch a file from its build directory.
fn fetches(module: &wrela_ir::Module) -> bool {
    module.functions.iter().any(|f| {
        wrela_ir::visit::any(&f.body, &mut |s| {
            matches!(
                s.expr(),
                Some(wrela_ir::Expr::Host(wrela_ir::HostOp::Command { opcode, .. }, _))
                    if *opcode == wrela_abi::stream::Opcode::Fetch as u32
            )
        })
    })
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
    /// Only a GPU frame test has a golden.
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
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

/// What a test's runs get: a script of input events (and its JSON, runtime/abi `input`'s, for
/// Chrome), or (a tick test) a tick log.
enum Input {
    Script(Vec<wrela_host::Scripted>, String),
    Log(wrela_host::TickLog),
}

/// What a GPU frame test's frames get: its script's events, the paths it holds until a frame,
/// and with `hosts: both`, the script's JSON for Chrome's run.
#[cfg_attr(not(feature = "gpu"), allow(dead_code))]
struct Given<'a> {
    script: &'a [wrela_host::Scripted],
    hold: &'a [(String, u32)],
    both: Option<&'a str>,
}

/// A test's input (a tick test's, with `ticks`): a `Script` constant's computed value, or the
/// file at its path in the package at `root`. Else how to name it in an error, and why.
fn input_of(
    checked: &Checked,
    data: &BuildData,
    root: &Path,
    input: &TestInput,
    ticks: bool,
    reach: &crate::reach::Reach,
) -> Result<Input, (String, String)> {
    match input {
        TestInput::File(path) => {
            read_input(reach, root, path, ticks).map_err(|why| (format!("`{path}`"), why))
        }
        TestInput::Const(c) => {
            let shown = checked.program.const_(*c).short();
            let Some(json) = crate::script::json_of(checked, data, *c) else {
                return Err((shown, "has no computed value".into()));
            };
            let script = wrela_host::parse_script(&json)
                .map_err(|e| (shown.clone(), format!("isn't a script: {e}")))?;
            if ticks && script.iter().any(|s| matches!(s.at, wrela_abi::input::At::Frame(_))) {
                let why = "has events at frames, and a tick test runs no frames: start it with `Script::ticks()`";
                return Err((shown, why.into()));
            }
            Ok(Input::Script(script, json))
        }
        // Names resolve as the program is checked: one that didn't was reported then.
        TestInput::Named(_) => Err(("name".into(), "names no constant".into())),
    }
}

/// The input at `path` in the package at `root`, or why it can't be read: a tick log (for a
/// tick test, `ticks`) if its bytes start as one, else a script.
fn read_input(
    reach: &crate::reach::Reach,
    root: &Path,
    path: &str,
    ticks: bool,
) -> Result<Input, String> {
    if wrela_abi::check::path_problem(path).is_some() {
        return Err("must be a path inside the package, with `/` between its parts".into());
    }
    let root = root.canonicalize().map_err(|e| format!("can't resolve its package: {e}"))?;
    let full = root.join(path).canonicalize().map_err(|e| format!("can't be read: {e}"))?;
    if !full.starts_with(&root) {
        return Err("is outside the package".into());
    }
    if !reach.is_file(&full) {
        return Err("isn't a regular file".into());
    }
    let bytes = reach.read(&full).map_err(|e| format!("can't be read: {e}"))?;
    if ticks && bytes.starts_with(&wrela_abi::ticks::MAGIC) {
        let log =
            wrela_host::TickLog::decode(&bytes).map_err(|e| format!("isn't a tick log: {e}"))?;
        return Ok(Input::Log(log));
    }
    let text = String::from_utf8(bytes).map_err(|_| "isn't UTF-8".to_string())?;
    let script = wrela_host::parse_script(&text).map_err(|e| format!("isn't a script: {e}"))?;
    Ok(Input::Script(script, text))
}

/// A GPU frame test's golden, with the package's root, whether to bless it, and where it may be
/// read ([`against_golden`]).
type GoldenAt<'a> = (&'a Golden, &'a Path, bool, &'a crate::reach::Reach);

/// The native host the GPU frame tests of a run share (none yet, or none after one failed):
/// loading the build and opening the device and its pipelines once.
#[derive(Default)]
struct GpuHost(#[cfg(feature = "gpu")] Option<wrela_host::Host>);

/// Runs GPU frame test `i` (`@test(frames: n, gpu: true)`, §13) on the native host's GPU:
/// `init`, its `frames` with `script`'s events, each frame's readbacks answered before the next,
/// then the test. Why it failed, if it did. The GPU host meters no fuel: it runs the program as
/// `wrela run` does. Each test is a new program, nothing of the last one's left; the host is
/// `gpu`'s, loaded for the first, and loaded again after one that failed. What it printed and
/// timed comes with it. The paths `hold` names are held from `init` on, each until its frame.
/// With a golden (and the package's root, and whether to bless it), the screen after the
/// frames is compared with it.
#[cfg(feature = "gpu")]
fn run_gpu(
    gpu: &mut GpuHost,
    dir: &Path,
    storage: &Path,
    i: usize,
    frames: u32,
    given: Given,
    golden: Option<GoldenAt>,
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
    let ran = run_gpu_frames(&mut host, dir, i, frames, &given, golden);
    let traced = host.trace().take();
    // What it printed, shown as a CPU test's is (the host runs quiet): a feature's test that
    // passed trivially says so.
    for line in &traced.lines {
        eprintln!("{line}");
    }
    if ran.is_none() {
        gpu.0 = Some(host);
    }
    (ran, traced)
}

/// [`run_gpu`]'s test, on its host.
#[cfg(feature = "gpu")]
fn run_gpu_frames(
    host: &mut wrela_host::Host,
    dir: &Path,
    i: usize,
    frames: u32,
    given: &Given,
    golden: Option<GoldenAt>,
) -> Option<Failed> {
    let Given { script, hold, both } = *given;
    for (path, _) in hold {
        host.hold(path);
    }
    if let Err(e) = host.init() {
        return failed(e, host.last_failure(), "in `init`".to_string());
    }
    for k in 0..frames {
        for (path, _) in hold.iter().filter(|h| h.1 == k) {
            host.release(path);
        }
        if let Err(e) = host.lockstep_frame(k, FPS, WIDTH, HEIGHT, script) {
            return failed(e, host.last_failure(), format!("in frame {k} of {frames}"));
        }
    }
    // The screen, once read for the golden: Chrome's run is compared with it too.
    let mut screen = None;
    if let Some((g, root, bless, reach)) = golden {
        let after = format!("after {frames} frames");
        let image = match &g.pass {
            None => host.read_screen().map(|s| (WIDTH, HEIGHT, s)),
            Some(label) => host.read_target(label),
        };
        let (w, h, image) = match image {
            Ok(i) => i,
            Err(e) => return failed(e, None, after),
        };
        if let Err(why) = against_golden(g, root, (w, h, &image), bless, reach) {
            return Some(Failed { why: Why::Golden(why), fault: None, phase: after });
        }
        if g.pass.is_none() {
            screen = Some(image);
        }
    }
    if let Some(json) = both {
        let after = format!("after {frames} frames");
        let within = golden.map_or(wrela_sema::defs::GOLDEN_WITHIN, |g| g.0.within);
        if let Err(why) = against_chrome(host, screen, dir, frames, json, within) {
            return Some(Failed { why: Why::Golden(why), fault: None, phase: after });
        }
    }
    match host.call_export(&format!("test.{i}"), &[]) {
        Ok(_) => None,
        Err(e) => failed(e, host.last_failure(), format!("after {frames} frames")),
    }
}

/// The screen (or a pass's target: `image`, its width, height and RGBA8) after a GPU frame
/// test's frames against its golden (§13): within its mean
/// difference, or why not. With `bless`, the golden is written from the screen first. It's read
/// through `reach`: in a tool's toolchain, through its roots' handles, so a package swapped
/// for a link out of them reads nothing.
#[cfg(feature = "gpu")]
fn against_golden(
    g: &Golden,
    root: &Path,
    (width, height, screen): (u32, u32, &[u8]),
    bless: bool,
    reach: &crate::reach::Reach,
) -> Result<(), String> {
    let path = &g.path;
    let full = golden_path(root, path)?;
    let what = match &g.pass {
        None => "the screen".to_string(),
        Some(label) => format!("the target of the pass `{label}`"),
    };
    if bless {
        if let Some(dir) = full.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        wrela_host::image::write_png(&full, width, height, screen)
            .map_err(|e| format!("writing its golden `{path}`: {e}"))?;
    }
    let (w, h, want) = reach
        .read(&full)
        .map_err(|e| e.to_string())
        .and_then(|bytes| wrela_host::image::png_of(&bytes, &full).map_err(|e| e.to_string()))
        .map_err(|why| {
            format!("it has no golden `{path}` to compare {what} with ({why}): `wrela test --bless` writes it from this machine's")
        })?;
    if (w, h) != (width, height) {
        return Err(format!("its golden `{path}` is {w}×{h}, and {what} is {width}×{height}"));
    }
    let d = wrela_host::image::compare(screen, &want)?;
    if d.mean > g.within {
        let shown = shown_beside(root, path, (width, height, screen), &want, reach);
        return Err(format!(
            "{what} is {:.3}/255 from its golden `{path}` on average, more than the {} it may be (a channel differs by {}/255 at most; {} of {} channels differ){shown}",
            d.mean, g.within, d.max, d.differing, d.channels
        ));
    }
    Ok(())
}

/// What a failed golden saw, and how far each pixel is from the golden (each channel's
/// difference times 8, opaque), written under the package's `build/goldens/` beside the
/// golden's own path (`frame.actual.png`, `frame.diff.png`) for `wrela test` run on the
/// package: the failure's words for them. Not under a tool's roots (a confined compile writes
/// nothing beside a package), nothing through a symbolic link (it could lead out of the
/// package), and nothing if the writes fail.
#[cfg(feature = "gpu")]
fn shown_beside(
    root: &Path,
    path: &str,
    (width, height, seen): (u32, u32, &[u8]),
    want: &[u8],
    reach: &crate::reach::Reach,
) -> String {
    if reach.is_confined() {
        return String::new();
    }
    let stem = path.strip_suffix(".png").unwrap_or(path);
    let Ok(root) = root.canonicalize() else { return String::new() };
    let at = root.join("build/goldens");
    let (actual, diff) =
        (at.join(format!("{stem}.actual.png")), at.join(format!("{stem}.diff.png")));
    // No link on the way to either file, or at it, before and after making their directories.
    let plain = || [&actual, &diff].iter().all(|p| crate::ensure_no_symlink_path(&root, p).is_ok());
    let difference: Vec<u8> = seen
        .chunks_exact(4)
        .zip(want.chunks_exact(4))
        .flat_map(|(a, b)| {
            let d = |i: usize| a[i].abs_diff(b[i]).saturating_mul(8);
            [d(0), d(1), d(2), 255]
        })
        .collect();
    let written = plain()
        && actual.parent().map_or(Ok(()), std::fs::create_dir_all).is_ok()
        && plain()
        && wrela_host::image::write_png(&actual, width, height, seen).is_ok()
        && wrela_host::image::write_png(&diff, width, height, &difference).is_ok();
    if written {
        format!(
            ": what it saw is `build/goldens/{stem}.actual.png`, and how far, `build/goldens/{stem}.diff.png`"
        )
    } else {
        String::new()
    }
}

/// The native host's last frame (`screen`, if it was read already) and state hash after
/// `frames` frames against Chrome's: the same build (in `dir`) run in headless Chrome's test
/// mode with the same frames, screen, rate, one worker and the script `json`, whose state hash
/// must be the same and whose last frame must be within `within` (a mean difference in 8-bit
/// steps) of the native host's (§13, `hosts: both`).
#[cfg(feature = "gpu")]
fn against_chrome(
    host: &mut wrela_host::Host,
    screen: Option<Vec<u8>>,
    dir: &Path,
    frames: u32,
    json: &str,
    within: f64,
) -> Result<(), String> {
    let screen = match screen {
        Some(s) => s,
        None => host.read_screen().map_err(|e| e.to_string())?,
    };
    let hash = format!("{:016x}", host.hash());
    std::fs::write(dir.join("input.json"), json).map_err(|e| format!("its script: {e}"))?;
    let mut fragment = format!(
        "#test&frames={frames}&width={WIDTH}&height={HEIGHT}&fps={FPS}&workers=1&input=input.json"
    );
    // The native host's feature mask (`WRELA_FEATURES`), given to Chrome's test mode too, so
    // both run the program's fallback paths.
    if let Ok(keep) = std::env::var("WRELA_FEATURES") {
        if !keep
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == ',' || c == '-')
        {
            return Err(format!(
                "WRELA_FEATURES is `{keep}`: `none`, or a comma list of WebGPU feature names"
            ));
        }
        fragment.push_str(&format!("&features={keep}"));
    }
    let timeout = std::time::Duration::from_secs_f64((f64::from(frames) / FPS * 3.0).max(120.0));
    // The build's directory alone is served: it is the page.
    wrela_host::pages::run(wrela_host::pages::Page {
        root: dir,
        page: "",
        fragment: &fragment,
        timeout,
        script: None,
    })
    .map_err(|why| format!("Chrome's run failed: {why}"))?;
    let results = dir.join("results");
    let read = |name: &str| {
        std::fs::read(results.join(name)).map_err(|e| format!("Chrome's `{name}`: {e}"))
    };
    let chrome_hash = String::from_utf8_lossy(&read("hash.txt")?).trim().to_string();
    if chrome_hash != hash {
        return Err(format!(
            "Chrome's state hash is {chrome_hash}, and the native host's {hash}: the hosts submitted different commands"
        ));
    }
    let d = wrela_host::image::compare(&screen, &read("frame.rgba")?)?;
    if d.mean > within {
        return Err(format!(
            "Chrome's last frame is {:.3}/255 from the native host's on average, more than the {within} it may be (a channel differs by {}/255 at most; {} of {} channels differ)",
            d.mean, d.max, d.differing, d.channels
        ));
    }
    Ok(())
}

/// A golden's path inside the package, with no symbolic link through which reading or blessing
/// it could escape. The package root itself may have been named through a link.
#[cfg(any(feature = "gpu", test))]
fn golden_path(root: &Path, path: &str) -> Result<std::path::PathBuf, String> {
    if wrela_abi::check::path_problem(path).is_some() {
        return Err(format!(
            "its golden `{path}` must be a path inside the package, with `/` between its parts"
        ));
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("can't resolve the package for its golden `{path}`: {e}"))?;
    let full = root.join(path);
    crate::ensure_no_symlink_path(&root, &full)
        .map_err(|e| format!("its golden `{path}` isn't a plain path in the package: {e}"))?;
    Ok(full)
}

/// [`run_gpu`], in a compiler built without the GPU host: the test can't run.
#[cfg(not(feature = "gpu"))]
fn run_gpu(
    _: &mut GpuHost,
    _: &Path,
    _: &Path,
    _: usize,
    _: u32,
    _: Given,
    _: Option<GoldenAt>,
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
/// the test; the paths `hold` names are held from `init` on, each until its frame. Why it
/// failed, if it did, and what it printed and timed.
fn run_one(
    built: &wrela_host::CpuBuild,
    storage: &Path,
    i: usize,
    run: TestRun,
    input: &Input,
    hold: &[(String, u32)],
) -> (Option<Failed>, wrela_host::Traced) {
    let _ = std::fs::create_dir_all(storage);
    // One thread: a parallel job's chunks all run on it, and the result is the same (§16.1).
    match built.instantiate_in(1, Some(storage)) {
        Ok(mut host) => {
            let failed = run_on(&mut host, i, run, input, hold);
            (failed, host.trace().take())
        }
        Err(e) => (failed(e, None, "as it started".to_string()), Default::default()),
    }
}

/// [`run_one`] on its host.
fn run_on(
    host: &mut wrela_host::CpuHost,
    i: usize,
    run: TestRun,
    input: &Input,
    hold: &[(String, u32)],
) -> Option<Failed> {
    let (log, script): (_, &[wrela_host::Scripted]) = match input {
        Input::Log(log) => (Some(log), &[]),
        Input::Script(s, _) => (None, s),
    };
    host.want_hashes(log.is_some());
    for (path, _) in hold {
        host.hold(path);
    }
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
                for (path, _) in hold.iter().filter(|h| h.1 == k) {
                    host.release(path);
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn test_input_stays_inside_its_package() {
        use std::os::unix::fs::symlink;

        let scratch = crate::Scratch::new("wrela-test").expect("a scratch directory");
        let pkg = scratch.0.join("package");
        let outside = scratch.0.join("outside");
        std::fs::create_dir_all(&pkg).expect("package");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(pkg.join("input.json"), "[]").expect("input");
        std::fs::write(outside.join("input.json"), "[]").expect("outside input");
        symlink(&outside, pkg.join("linked")).expect("directory symlink");
        symlink(outside.join("input.json"), pkg.join("outside.json")).expect("file symlink");
        symlink(&pkg, scratch.0.join("package-link")).expect("package symlink");

        assert!(
            read_input(&crate::reach::Reach::anywhere(), &pkg, "linked/input.json", false).is_err()
        );
        assert!(read_input(&crate::reach::Reach::anywhere(), &pkg, "outside.json", false).is_err());
        assert!(read_input(&crate::reach::Reach::anywhere(), &pkg, "input.json", false).is_ok());
        assert!(
            read_input(
                &crate::reach::Reach::anywhere(),
                &scratch.0.join("package-link"),
                "input.json",
                false
            )
            .is_ok()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_golden_cannot_escape_the_package_through_a_symbolic_link() {
        use std::os::unix::fs::symlink;

        let scratch = crate::Scratch::new("wrela-test").expect("a scratch directory");
        let pkg = scratch.0.join("package");
        let outside = scratch.0.join("outside");
        std::fs::create_dir_all(&pkg).expect("package");
        std::fs::create_dir_all(&outside).expect("outside");
        symlink(&outside, pkg.join("shots")).expect("symlink");

        assert!(golden_path(&pkg, "shots/frame.png").is_err());
        assert_eq!(
            golden_path(&pkg, "goldens/frame.png").expect("ordinary path"),
            pkg.canonicalize().expect("canonical package").join("goldens/frame.png")
        );
    }

    /// A failed golden's pictures go beside the package's goldens, never through a link there.
    #[cfg(all(unix, feature = "gpu"))]
    #[test]
    fn a_failed_goldens_pictures_are_not_written_through_a_link() {
        let scratch = crate::Scratch::new("wrela-test").expect("a scratch directory");
        let (pkg, outside) = (scratch.0.join("package"), scratch.0.join("outside.png"));
        std::fs::create_dir_all(pkg.join("build/goldens/goldens")).expect("package");
        std::fs::write(&outside, b"keep").expect("outside file");
        let actual = pkg.join("build/goldens/goldens/frame.actual.png");
        std::os::unix::fs::symlink(&outside, &actual).expect("a link out");
        let (seen, want) = (&[0u8; 16][..], &[255u8; 16][..]);
        let reach = crate::reach::Reach::anywhere();
        let shown = shown_beside(&pkg, "goldens/frame.png", (2, 2, seen), want, &reach);
        assert_eq!(shown, "");
        assert_eq!(std::fs::read(&outside).expect("outside file"), b"keep");
        // Without the link, both are written.
        std::fs::remove_file(&actual).expect("the link");
        let shown = shown_beside(&pkg, "goldens/frame.png", (2, 2, seen), want, &reach);
        assert!(shown.contains("frame.diff.png"), "{shown}");
        assert!(actual.is_file() && pkg.join("build/goldens/goldens/frame.diff.png").is_file());
    }

    /// In a tool's toolchain, a golden is read through the roots' handles: a package swapped for
    /// a link out of them, after it was loaded, reads no golden from where the link leads.
    #[cfg(all(unix, feature = "gpu"))]
    #[test]
    fn a_golden_is_read_through_the_roots_alone() {
        let scratch = crate::Scratch::new("wrela-test").expect("a scratch directory");
        let (root, outside) = (scratch.0.join("root"), scratch.0.join("outside"));
        std::fs::create_dir_all(root.join("package/goldens")).expect("package");
        std::fs::create_dir_all(outside.join("goldens")).expect("outside");
        let screen = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
        for dir in [&root.join("package"), &outside] {
            wrela_host::image::write_png(&dir.join("goldens/frame.png"), WIDTH, HEIGHT, &screen)
                .expect("a golden");
        }
        let golden = Golden {
            path: "goldens/frame.png".into(),
            span: wrela_diag::Span::new(wrela_diag::FileId(0), 0, 0),
            within: 0.5,
            pass: None,
        };
        let reach = crate::reach::Reach::within(std::slice::from_ref(&root)).expect("the root");
        let pkg = root.join("package");
        let seen = (WIDTH, HEIGHT, screen.as_slice());
        against_golden(&golden, &pkg, seen, false, &reach).expect("the package's golden");
        // The package, swapped for a link to a directory outside the root.
        std::fs::rename(&pkg, root.join("moved")).expect("move");
        std::os::unix::fs::symlink(&outside, &pkg).expect("a link out");
        let refused = against_golden(&golden, &pkg, seen, false, &reach).expect_err("outside");
        assert!(refused.contains("outside the roots"), "{refused}");
    }
}
