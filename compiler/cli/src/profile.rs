//! `wrela profile <package-dir> [--frames n] [--size WxH] [--input script.json] [--serial]
//! [--json] [--baseline report.json]`: where a frame's time goes, on this machine. It builds the
//! program as `wrela build` does, runs its frames on the native host's GPU (in lockstep with its
//! ticker, 60 a second; 60 warmup frames and 120 measured frames unless said), and reports:
//! - the GPU's time: each label's median a frame and its 95th percentile (a pass or a dispatch,
//!   named by `std::gpu::label`, else by its pipeline), and the frame's span, its first start
//!   to its last end. Passes overlap on some GPUs, so labels can add up to more than the span;
//!   `--serial` runs each pass alone, so each label's time is its own and the span isn't a
//!   frame's;
//! - the CPU's: a frame call's median time and its 95th percentile, the host's work on its
//!   commands included, and the phases the program timed (`std::time::phase`), summed over the
//!   run;
//! - each pipeline's WGSL, largest first.
//!
//! A budget is about the slow frames as much as the median, so the 95th percentile is beside
//! it. The first frames make the GPU's pipelines, so a run of a few frames says less. Times are
//! this machine's: compare a change with a profile from before it, on the same machine.
//! `--baseline` does: given an earlier `--json` report, each time says what it was and how it
//! changed, and the labels no longer run are listed. Reports must match machine, adapter,
//! driver and workload; host/build hashes identify the measured implementation.
//! `--compare <built-baseline>` alternates five pairs on
//! one device, resetting state each time, and reports run variation without timing gates.
//! Use `--scenario` to name the scene; seeds and quality remain fixed by its program/input.
//! `--warmup`, `--workers` and `--pairs` control the replay; `--warmup 0` includes startup frames.
//! Run benchmarks with other builds and GPU workloads idle.
//!
//! `--optimize size|speed` profiles the build `wasm-opt` made, as a shipped one is.
//! `--relaxed-simd` profiles a relaxed-SIMD build (`wrela build --relaxed-simd`) running its
//! relaxed module, as a host that runs relaxed SIMD does; `--compare` a build without it for the
//! difference.
//!
//! Beside the times, each report has the executor's counters, each a frame's median: passes,
//! dispatches, draws and draw calls, buffer and texture writes, bind groups and pipelines set
//! (the physical half of a draw count). `--chrome` measures in headless Chrome instead
//! (`profile/chrome.rs`): the same report, `"host": "chrome"`, with the paced frame interval a
//! player sees; `--features none` (or a comma list to keep) hides the device's optional
//! features from the program there, so its fallback paths are measured. `--baseline` and
//! `--compare` take Chrome reports and builds as they take native ones.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;
use wrela_abi::input::Scripted;
use wrela_host::{GpuTiming, Host, Options, Timing};

mod chrome;
mod comparison;

/// The executor's counters' names, in the order reports list them (`wrela_host::Counters`, and
/// the browser's `counters.json`).
fn counter_names() -> impl Iterator<Item = &'static str> {
    wrela_host::Counters::default().fields().into_iter().map(|(name, _)| name)
}

pub(crate) use comparison::{Storage, build_hash, overwritten_by};

struct Args {
    dir: PathBuf,
    frames: u32,
    size: (u32, u32),
    input: Option<PathBuf>,
    serial: bool,
    json: bool,
    baseline: Option<PathBuf>,
    compare: Option<PathBuf>,
    warmup: u32,
    workers: u32,
    pairs: u32,
    scenario: String,
    /// Measure in headless Chrome (`--chrome`), with test mode's feature mask (`--features`).
    chrome: bool,
    features: Option<String>,
    /// Chrome's command checker on (`--checked`): off by default, as a release build plays.
    checked: bool,
    /// The WASM through `wasm-opt` (`--optimize size|speed`), as a shipped build's is.
    optimize: Option<wrela_driver::optimize::Level>,
    /// How many of the slowest frames to name (`--worst n`).
    worst: usize,
    /// A relaxed-SIMD build (`--relaxed-simd`), its relaxed module run where the host runs it.
    relaxed_simd: bool,
}

fn parse(args: &[String]) -> Option<Args> {
    let mut a = Args {
        dir: PathBuf::new(),
        frames: 120,
        size: (1280, 720),
        input: None,
        serial: false,
        json: false,
        baseline: None,
        compare: None,
        warmup: 60,
        workers: wrela_host::default_workers(),
        pairs: 5,
        scenario: String::new(),
        chrome: false,
        features: None,
        checked: false,
        optimize: None,
        worst: 5,
        relaxed_simd: false,
    };
    let mut dir = None;
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--warmup" => a.warmup = it.next()?.parse().ok()?,
            "--workers" => {
                a.workers = it
                    .next()?
                    .parse()
                    .ok()
                    .filter(|&n| n > 0 && n <= wrela_abi::memory::MAX_WORKERS + 1)?
            }
            "--pairs" => a.pairs = it.next()?.parse().ok().filter(|&n| n >= 3)?,
            "--scenario" => a.scenario = it.next()?.clone(),
            "--compare" => a.compare = Some(PathBuf::from(it.next()?)),
            "--frames" => a.frames = it.next()?.parse().ok().filter(|&n| n > 0)?,
            "--size" => a.size = crate::parse_size(it.next()?)?,
            "--input" => a.input = Some(PathBuf::from(it.next()?)),
            "--serial" => a.serial = true,
            "--worst" => a.worst = it.next()?.parse().ok()?,
            "--chrome" => a.chrome = true,
            "--checked" => a.checked = true,
            "--relaxed-simd" => a.relaxed_simd = true,
            "--optimize" => a.optimize = Some(wrela_driver::optimize::Level::parse(it.next()?)?),
            "--features" => {
                let keep = it.next()?;
                let plain =
                    |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == ',' || c == '-';
                if keep.is_empty() || !keep.chars().all(plain) {
                    return None;
                }
                a.features = Some(keep.clone());
            }
            "--json" => a.json = true,
            "--baseline" => a.baseline = Some(PathBuf::from(it.next()?)),
            _ if dir.is_none() && !x.starts_with('-') => dir = Some(PathBuf::from(x)),
            _ => return None,
        }
    }
    a.dir = dir?;
    a.frames.checked_add(a.warmup)?;
    // The native host's mask is `WRELA_FEATURES`, read as it opens its device.
    if (a.features.is_some() || a.checked) && !a.chrome {
        return None;
    }
    if a.baseline.is_some() && a.compare.is_some() {
        return None;
    }
    if a.compare.is_none() && args.iter().any(|x| x == "--pairs") {
        return None;
    }
    if a.scenario.is_empty() {
        a.scenario = a
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| a.dir.display().to_string());
    }
    Some(a)
}

/// What a profile found.
struct Profile {
    /// `native` or `chrome`.
    host: &'static str,
    environment: serde_json::Value,
    workload: serde_json::Value,
    build_sha256: String,
    init_warmup_ms: f64,
    /// Each label's time: the longest first.
    labels: Vec<Label>,
    /// A frame's span on the GPU, ms: the median and the 95th percentile.
    span: Option<Times>,
    /// A frame call's time on the CPU, ms: the median and the 95th percentile.
    frame: Times,
    /// Chrome's: the interval between paced frames, ms, as a player sees them.
    interval: Option<Times>,
    /// Each executor counter's median a frame, in [`counter_names`]' order.
    counters: Vec<(&'static str, f64)>,
    phases: wrela_host::Traced,
    /// Each pipeline's name and WGSL bytes, the largest first.
    wgsl: Vec<(String, u64)>,
    /// Chrome's: the bytes the page loaded over the wire (compressed where the server
    /// compressed them), and how many files.
    loaded: Option<(u64, usize)>,
    /// The slowest frames (`--worst`): by their interval in Chrome, by their call natively.
    worst: Vec<Worst>,
}

/// A frame of the run, as `--worst` names it: when it ran, how long it took, and where its
/// time went.
#[derive(Clone, Debug, Default)]
struct Worst {
    frame: usize,
    /// Chrome's: from its start to the next frame's, ms.
    interval: Option<f64>,
    /// Its call, ms.
    cpu: f64,
    /// Its span on the GPU, ms.
    gpu: Option<f64>,
    /// Its phases and its labels' GPU time, ms, largest first.
    phases: Vec<(String, f64)>,
    labels: Vec<(String, f64)>,
}

/// The `n` frames of `frames` that took longest by `by`, longest first.
fn slowest(mut frames: Vec<Worst>, n: usize, by: impl Fn(&Worst) -> f64) -> Vec<Worst> {
    frames.sort_by(|x, y| by(y).total_cmp(&by(x)));
    frames.truncate(n);
    for f in &mut frames {
        f.phases.sort_by(|x, y| y.1.total_cmp(&x.1));
        f.labels.sort_by(|x, y| y.1.total_cmp(&x.1));
    }
    frames
}

/// Each frame's labels and their GPU ms (a label run twice in a frame summed), by frame.
fn labels_by_frame(timings: &[GpuTiming]) -> BTreeMap<usize, Vec<(String, f64)>> {
    let mut per: BTreeMap<usize, Vec<(String, f64)>> = BTreeMap::new();
    for t in timings {
        let labels = per.entry(t.frame).or_default();
        match labels.iter_mut().find(|(l, _)| *l == t.label) {
            Some((_, ms)) => *ms += t.nanos / 1e6,
            None => labels.push((t.label.clone(), t.nanos / 1e6)),
        }
    }
    per
}

/// A time over the frames, ms: the median and the 95th percentile.
#[derive(Clone, Copy, Default)]
struct Times {
    median: f64,
    p95: f64,
}

impl Times {
    fn of(xs: Vec<f64>) -> Option<Times> {
        Some(Times { p95: percentile(&xs, 95)?, median: median(xs)? })
    }
}

/// An earlier report (`--json`): how it ran and its medians. `--baseline` compares a run with one.
struct Baseline {
    environment: serde_json::Value,
    workload: serde_json::Value,
    frames: u64,
    size: (u64, u64),
    /// Whether it ran with `--serial`.
    serial: bool,
    span: Option<f64>,
    frame: Option<f64>,
    labels: BTreeMap<String, f64>,
}

impl Baseline {
    fn read(path: &std::path::Path) -> Result<Baseline, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("can't read {}: {e}", path.display()))?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            format!("{} isn't a `wrela profile --json` report: {e}", path.display())
        })?;
        if v["schema"] != 2 || !v["environment"].is_object() || !v["workload"].is_object() {
            return Err(
                "baseline lacks hardware/workload identity; record it again with this profiler"
                    .into(),
            );
        }
        let labels = v["gpu"]["labels"]
            .as_array()
            .ok_or_else(|| format!("{} has no `gpu.labels`", path.display()))?
            .iter()
            .filter_map(|l| Some((l["label"].as_str()?.to_string(), l["median_ms"].as_f64()?)))
            .collect();
        Ok(Baseline {
            environment: v["environment"].clone(),
            workload: v["workload"].clone(),
            frames: v["frames"].as_u64().unwrap_or(0),
            size: (v["size"][0].as_u64().unwrap_or(0), v["size"][1].as_u64().unwrap_or(0)),
            serial: v["serial"] == serde_json::Value::Bool(true),
            span: v["gpu"]["span_ms"].as_f64(),
            frame: v["cpu"]["frame_ms"].as_f64(),
            labels,
        })
    }
}

/// A serial profile times each pass on its own; a non-serial profile times them together in a
/// frame. Their label times are therefore not a like-for-like comparison.
fn baseline_problem(serial: bool, baseline: &Baseline) -> Option<String> {
    if serial == baseline.serial {
        return None;
    }
    let mode = |serial| if serial { "with `--serial`" } else { "without `--serial`" };
    Some(format!(
        "the baseline ran {}, but this profile would run {}; compare profiles made in the same mode",
        mode(baseline.serial),
        mode(serial)
    ))
}

pub fn run(args: &[String]) -> ExitCode {
    let Some(a) = parse(args) else { return crate::usage() };
    match run_report(&a) {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::from(2)
        }
    }
}

fn run_report(a: &Args) -> Result<String, String> {
    let baseline = a.baseline.as_deref().map(Baseline::read).transpose()?;
    if let Some(why) = baseline.as_ref().and_then(|b| baseline_problem(a.serial, b)) {
        return Err(why);
    }
    if let Some(before) = &a.compare {
        comparison::validate_saved_build(&a.dir, before)?;
    }
    // A lock coordinates profiler instances on this machine; ordinary builds must be idle too.
    let _lease = comparison::lease(&std::env::temp_dir().join("wrela-profile.lock"))?;
    if a.chrome {
        return chrome_report(a, baseline.as_ref());
    }
    let storage = comparison::Storage::new()?;
    let options = Options {
        storage: Some(storage.0.clone()),
        timing: if a.serial { Timing::Serial } else { Timing::Span },
        quiet: true,
        defer_init: true,
        workers: a.workers,
        ..Options::default()
    };
    if a.relaxed_simd {
        wrela_host::load_relaxed_simd();
    }
    let (mut host, script, built) =
        load(&a.dir, a.input.as_deref(), &options, a.optimize, a.relaxed_simd)?;
    let environment = comparison::environment(&mut host)?;
    let workload = comparison::workload(a, &script);
    let frozen = comparison::build_hash(&built)?;
    if let Some(b) = &baseline {
        comparison::compatible_hardware(&b.environment, &environment)?;
        comparison::compatible(&b.workload, &workload, "workload")?;
    }
    if a.compare.is_some() {
        let mut sample = |dir: &Path| -> Result<Profile, String> {
            host.restart_in(dir, &options).map_err(|e| e.to_string())?;
            // Restart has joined the old program's workers. Each deferred init sees empty
            // storage.
            let storage = options.storage.as_ref().expect("isolated profile storage");
            std::fs::remove_dir_all(storage).map_err(|e| e.to_string())?;
            std::fs::create_dir(storage).map_err(|e| e.to_string())?;
            measure(a, &mut host, &script, dir, &environment, &workload)
        };
        return comparison::run(a, &mut sample, &built, &|| Ok(environment.clone()), &workload);
    }
    let p = measure(a, &mut host, &script, &built, &environment, &workload)?;
    if p.build_sha256 != frozen {
        return Err("build changed during measurement; discard this run".into());
    }
    Ok(if a.json { json(a, &p, baseline.as_ref()) } else { text(a, &p, baseline.as_ref()) })
}

/// A package built as `wrela build` builds it (into `build/profile`) and loaded on the native
/// host with `options`, not yet initialized, with its input script (`--input`) read.
fn load(
    dir: &Path,
    input: Option<&Path>,
    options: &Options,
    optimize: Option<wrela_driver::optimize::Level>,
    relaxed: bool,
) -> Result<(Host, Vec<Scripted>, PathBuf), String> {
    let built = crate::build_into(dir, "profile", optimize, relaxed)?;
    let script = crate::read_script(input)?;
    let host = Host::load_with(&built, options).map_err(|e| e.to_string())?;
    Ok((host, script, built))
}

/// Copies the input script `from` (`--input`), if there's one, to `dir/input.json`, where test
/// mode reads it beside the page: as a new file, so a link there is replaced, never followed.
fn place_input(from: Option<&Path>, dir: &Path) -> Result<(), String> {
    let Some(from) = from else { return Ok(()) };
    let bytes = std::fs::read(from)
        .map_err(|e| format!("can't read the input script {}: {e}", from.display()))?;
    let to = dir.join("input.json");
    let cant = |e: std::io::Error| format!("can't copy the input script to {}: {e}", to.display());
    match std::fs::remove_file(&to) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(cant(e)),
        _ => {}
    }
    let mut file =
        std::fs::OpenOptions::new().write(true).create_new(true).open(&to).map_err(cant)?;
    std::io::Write::write_all(&mut file, &bytes).map_err(cant)
}

/// `--chrome`'s report: the build measured in headless Chrome (`profile/chrome.rs`).
fn chrome_report(a: &Args, baseline: Option<&Baseline>) -> Result<String, String> {
    let built = crate::build_into(&a.dir, "profile", a.optimize, a.relaxed_simd)?;
    let script = crate::read_script(a.input.as_deref())?;
    let workload = comparison::workload(a, &script);
    let frozen = comparison::build_hash(&built)?;
    if a.compare.is_some() {
        let device = std::cell::RefCell::new(serde_json::Value::Null);
        let mut sample = |dir: &Path| -> Result<Profile, String> {
            place_input(a.input.as_deref(), dir)?;
            let (p, d) = chrome::measure(a, dir)?;
            *device.borrow_mut() = d;
            Ok(p)
        };
        return comparison::run(
            a,
            &mut sample,
            &built,
            &|| chrome::environment(&device.borrow()),
            &workload,
        );
    }
    place_input(a.input.as_deref(), &built)?;
    let (mut p, device) = chrome::measure(a, &built)?;
    let environment = chrome::environment(&device)?;
    if let Some(b) = baseline {
        comparison::compatible_hardware(&b.environment, &environment)?;
        comparison::compatible(&b.workload, &workload, "workload")?;
    }
    p.environment = environment;
    p.workload = workload;
    if p.build_sha256 != frozen {
        return Err("build changed during measurement; discard this run".into());
    }
    Ok(if a.json { json(a, &p, baseline) } else { text(a, &p, baseline) })
}

fn measure(
    a: &Args,
    host: &mut Host,
    script: &[Scripted],
    built: &Path,
    environment: &serde_json::Value,
    workload: &serde_json::Value,
) -> Result<Profile, String> {
    let started = Instant::now();
    host.init().map_err(|e| format!("in `init`: {e}"))?;
    let (w, h) = a.size;
    for k in 0..a.warmup {
        host.lockstep_frame(k, 60.0, w, h, script)
            .map_err(|e| format!("in warmup frame {k}: {e}"))?;
    }
    host.take_timings().map_err(|e| e.to_string())?;
    host.trace().take();
    host.take_counters();
    let init_warmup_ms = started.elapsed().as_secs_f64() * 1e3;
    let mut calls = Vec::with_capacity(a.frames as usize);
    let mut counted: Vec<wrela_host::Counters> = Vec::with_capacity(a.frames as usize);
    // Each frame's phases, and the run's.
    let mut phases = wrela_host::Traced::default();
    let mut frames = Vec::with_capacity(a.frames as usize);
    for k in a.warmup..a.warmup + a.frames {
        let t = Instant::now();
        host.lockstep_frame(k, 60.0, w, h, script).map_err(|e| format!("in frame {k}: {e}"))?;
        let ms = t.elapsed().as_secs_f64() * 1e3;
        calls.push(ms);
        counted.push(host.take_counters());
        let traced = host.trace().take();
        let mine = traced.phases.iter().map(|ph| (ph.name.clone(), ph.seconds * 1e3)).collect();
        frames.push(Worst { frame: k as usize, cpu: ms, phases: mine, ..Worst::default() });
        phases.merge(traced);
    }
    let counters = counter_names()
        .enumerate()
        .map(|(i, name)| {
            let xs = counted.iter().map(|c| c.fields()[i].1 as f64).collect();
            (name, median(xs).unwrap_or(0.0))
        })
        .collect();
    let timings = host.take_timings().map_err(|e| e.to_string())?;
    let framed_spans = wrela_host::frame_spans(&timings);
    let mut labels = labels_by_frame(&timings);
    for f in &mut frames {
        f.gpu = framed_spans.iter().find(|(k, _)| *k == f.frame).map(|(_, ms)| *ms);
        f.labels = labels.remove(&f.frame).unwrap_or_default();
    }
    let spans: Vec<f64> = framed_spans.into_iter().map(|(_, ms)| ms).collect();
    Ok(Profile {
        worst: slowest(frames, a.worst, |f| f.cpu),
        host: "native",
        interval: None,
        counters,
        environment: environment.clone(),
        workload: workload.clone(),
        build_sha256: comparison::build_hash(built)?,
        init_warmup_ms,
        labels: by_label(&timings),
        span: Times::of(spans),
        frame: Times::of(calls).unwrap_or_default(),
        phases,
        wgsl: wgsl(built)?,
        loaded: None,
    })
}

/// A label's time on the GPU: its median ms and 95th percentile over the frames it ran in (a
/// label run twice in a frame is summed), its ms in all, and in how many frames it ran.
struct Label {
    name: String,
    median: f64,
    p95: f64,
    total: f64,
    frames: usize,
}

/// Each label's time: the longest median first.
fn by_label(timings: &[GpuTiming]) -> Vec<Label> {
    let mut per: Vec<(String, std::collections::BTreeMap<usize, f64>)> = Vec::new();
    for t in timings {
        let i = match per.iter().position(|(l, _)| *l == t.label) {
            Some(i) => i,
            None => {
                per.push((t.label.clone(), Default::default()));
                per.len() - 1
            }
        };
        *per[i].1.entry(t.frame).or_default() += t.nanos / 1e6;
    }
    let mut out: Vec<Label> = per
        .into_iter()
        .map(|(name, frames)| {
            let ms: Vec<f64> = frames.into_values().collect();
            let total = ms.iter().sum();
            let n = ms.len();
            let Times { median, p95 } = Times::of(ms).unwrap_or_default();
            Label { name, median, p95, total, frames: n }
        })
        .collect();
    out.sort_by(|x, y| y.median.total_cmp(&x.median));
    out
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(f64::total_cmp);
    Some(xs[xs.len() / 2])
}

/// The `p`th percentile of `xs`, by nearest rank: the smallest value that at least `p`% of
/// them are at or under (of 120 frames, the 95th is the seventh slowest).
fn percentile(xs: &[f64], p: u32) -> Option<f64> {
    let mut xs = xs.to_vec();
    xs.sort_by(f64::total_cmp);
    let rank = (xs.len() * p as usize).div_ceil(100);
    xs.get(rank.max(1) - 1).copied()
}

/// Each pipeline's name and the bytes of its WGSL, from the build's manifest: the largest first.
fn wgsl(built: &std::path::Path) -> Result<Vec<(String, u64)>, String> {
    let manifest =
        std::fs::read_to_string(built.join("manifest.json")).map_err(|e| e.to_string())?;
    let m: serde_json::Value = serde_json::from_str(&manifest).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for p in m["pipelines"].as_array().into_iter().flatten() {
        let (Some(name), Some(shader)) = (p["name"].as_str(), p["shader"].as_str()) else {
            continue;
        };
        let bytes = std::fs::metadata(built.join(shader)).map_err(|e| e.to_string())?.len();
        out.push((name.to_string(), bytes));
    }
    out.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
    Ok(out)
}

fn kib(bytes: u64) -> String {
    format!("{:.1} KiB", bytes as f64 / 1024.0)
}

/// What a time was in the baseline, and how it changed: empty without one, `new` for a label it
/// didn't run.
fn was(baseline: Option<&Baseline>, then: Option<f64>, now: f64) -> String {
    match (baseline, then) {
        (None, _) => String::new(),
        (Some(_), None) => "   new".into(),
        (Some(_), Some(then)) if then > 0.0 => {
            format!("   was {then:.3} ms, {:+.1}%", (now - then) / then * 100.0)
        }
        (Some(_), Some(then)) => format!("   was {then:.3} ms"),
    }
}

/// The report for people.
fn text(a: &Args, p: &Profile, baseline: Option<&Baseline>) -> String {
    let (w, h) = a.size;
    let mut s = format!("`{}`: {} frames at {w}×{h}, on {}\n", a.dir.display(), a.frames, p.host);
    s.push_str(&format!(
        "{}\nwarmup: {} frames; init and warmup: {:.3} ms\n",
        p.environment["adapter"].as_str().unwrap_or(""),
        a.warmup,
        p.init_warmup_ms
    ));
    if let Some(b) = baseline {
        let path = a.baseline.as_deref().unwrap_or(std::path::Path::new("")).display();
        let (bw, bh) = b.size;
        s.push_str(&format!("compared with {path}: {} frames at {bw}×{bh}", b.frames));
        if (b.frames, b.size) != (u64::from(a.frames), (u64::from(w), u64::from(h))) {
            s.push_str(", which isn't how this run ran: compare like with like");
        }
        s.push('\n');
    }
    s.push_str(&format!("\n{:<34} {:>9}    {:>9}\n", "GPU, each frame's", "median", "95th"));
    match p.span {
        Some(span) if !a.serial => s.push_str(&format!(
            "  {:<32} {:>9.3} ms {:>9.3} ms{}\n",
            "the frame's span",
            span.median,
            span.p95,
            was(baseline, baseline.and_then(|b| b.span), span.median)
        )),
        _ => {}
    }
    let every = |l: &&Label| l.frames == a.frames as usize;
    for l in p.labels.iter().filter(every) {
        let then = baseline.and_then(|b| b.labels.get(&l.name).copied());
        s.push_str(&format!(
            "  {:<32} {:>9.3} ms {:>9.3} ms{}\n",
            l.name,
            l.median,
            l.p95,
            was(baseline, then, l.median)
        ));
    }
    let mut some: Vec<&Label> = p.labels.iter().filter(|l| !every(l)).collect();
    some.sort_by(|x, y| y.total.total_cmp(&x.total));
    if !some.is_empty() {
        s.push_str("in some frames only: the ms in all, and in how many\n");
        for l in some {
            s.push_str(&format!("  {:<32} {:>9.3} ms in {}\n", l.name, l.total, l.frames));
        }
    }
    if p.labels.is_empty() {
        s.push_str("  nothing: the program records no GPU work\n");
    }
    let gone = baseline.map(|b| gone(b, p)).unwrap_or_default();
    if !gone.is_empty() {
        s.push_str(&format!("no longer run: {}\n", gone.join(", ")));
    }
    s.push_str("\nCPU\n");
    s.push_str(&format!(
        "  {:<32} {:>9.3} ms {:>9.3} ms{} (host work and GPU waits included; not browser FPS)\n",
        "a frame's call",
        p.frame.median,
        p.frame.p95,
        was(baseline, baseline.and_then(|b| b.frame), p.frame.median)
    ));
    if let Some(i) = p.interval {
        s.push_str(&format!(
            "  {:<32} {:>9.3} ms {:>9.3} ms (paced, two frames in flight: as a player sees them)\n",
            "the interval between frames", i.median, i.p95
        ));
    }
    if !p.phases.phases.is_empty() {
        s.push_str(&format!("  phases over the run: {}\n", p.phases.phases_line()));
    }
    if !p.worst.is_empty() {
        let by = if p.interval.is_some() { "interval" } else { "call" };
        s.push_str(&format!("\nthe slowest frames, by their {by}\n"));
        for f in &p.worst {
            let mut line = format!("  frame {}:", f.frame);
            if let Some(i) = f.interval {
                line.push_str(&format!(" {i:.3} ms to the next,"));
            }
            line.push_str(&format!(" its call {:.3} ms", f.cpu));
            if let Some(g) = f.gpu {
                line.push_str(&format!(", the GPU's span {g:.3} ms"));
            }
            s.push_str(&line);
            s.push('\n');
            let list = |xs: &[(String, f64)]| {
                xs.iter().map(|(n, ms)| format!("{n} {ms:.3}")).collect::<Vec<_>>().join(", ")
            };
            if !f.phases.is_empty() {
                s.push_str(&format!("    phases, ms: {}\n", list(&f.phases)));
            }
            if !f.labels.is_empty() {
                s.push_str(&format!("    labels, GPU ms: {}\n", list(&f.labels)));
            }
        }
    }
    s.push_str("\nthe executor, each frame's median\n");
    for (name, v) in &p.counters {
        s.push_str(&format!("  {:<32} {:>9}\n", name.replace('_', " "), v));
    }
    if let Some((bytes, files)) = p.loaded {
        s.push_str(&format!("\nloaded: {} over the wire, in {files} files\n", kib(bytes)));
    }
    let total: u64 = p.wgsl.iter().map(|(_, b)| b).sum();
    s.push_str(&format!(
        "\nWGSL: {}, {} in all\n",
        crate::count(p.wgsl.len(), "pipeline", "pipelines"),
        kib(total)
    ));
    for (name, bytes) in &p.wgsl {
        s.push_str(&format!("  {name:<32} {:>10}\n", kib(*bytes)));
    }
    s
}

/// The labels the baseline ran that this run didn't.
fn gone(b: &Baseline, p: &Profile) -> Vec<String> {
    b.labels.keys().filter(|l| !p.labels.iter().any(|x| &x.name == *l)).cloned().collect()
}

/// The report for tools: times in ms (phases in seconds), WGSL in bytes. With a baseline, each
/// time it had is beside this run's as `was_...`, and `baseline` says how it ran and which of
/// its labels this run didn't.
fn json(a: &Args, p: &Profile, baseline: Option<&Baseline>) -> String {
    let labels: Vec<_> = p
        .labels
        .iter()
        .map(|l| {
            let mut v = serde_json::json!({
                "label": l.name,
                "median_ms": l.median,
                "p95_ms": l.p95,
                "total_ms": l.total,
                "frames": l.frames,
            });
            if let Some(then) = baseline.and_then(|b| b.labels.get(&l.name)) {
                v["was_median_ms"] = (*then).into();
            }
            v
        })
        .collect();
    let phases: Vec<_> = p
        .phases
        .phases
        .iter()
        .map(|ph| serde_json::json!({ "name": ph.name, "seconds": ph.seconds, "times": ph.times }))
        .collect();
    let wgsl: Vec<_> =
        p.wgsl.iter().map(|(n, b)| serde_json::json!({ "pipeline": n, "bytes": b })).collect();
    let span = p.span.filter(|_| !a.serial);
    let counters: serde_json::Map<String, serde_json::Value> =
        p.counters.iter().map(|(k, v)| ((*k).to_string(), (*v).into())).collect();
    let mut all = serde_json::json!({
        "schema": 2,
        "host": p.host,
        "environment": p.environment,
        "workload": p.workload,
        "build_sha256": p.build_sha256,
        "init_warmup_ms": p.init_warmup_ms,
        "warmup": a.warmup,
        "frames": a.frames,
        "size": [a.size.0, a.size.1],
        "serial": a.serial,
        "gpu": {
            "span_ms": span.map(|t| t.median),
            "span_p95_ms": span.map(|t| t.p95),
            "labels": labels,
        },
        "cpu": { "frame_ms": p.frame.median, "frame_p95_ms": p.frame.p95, "phases": phases },
        "counters": counters,
        "wgsl": wgsl,
    });
    if let Some((bytes, files)) = p.loaded {
        all["loaded"] = serde_json::json!({ "bytes": bytes, "files": files });
    }
    let pairs = |xs: &[(String, f64)]| -> Vec<serde_json::Value> {
        xs.iter().map(|(n, ms)| serde_json::json!([n, ms])).collect()
    };
    all["worst"] = p
        .worst
        .iter()
        .map(|f| {
            serde_json::json!({
                "frame": f.frame,
                "interval_ms": f.interval,
                "cpu_ms": f.cpu,
                "gpu_span_ms": f.gpu,
                "phases": pairs(&f.phases),
                "labels": pairs(&f.labels),
            })
        })
        .collect::<Vec<_>>()
        .into();
    if let Some(i) = p.interval {
        all["cpu"]["frame_interval_ms"] = i.median.into();
        all["cpu"]["frame_interval_p95_ms"] = i.p95.into();
    }
    if let Some(b) = baseline {
        if let Some(then) = b.span {
            all["gpu"]["was_span_ms"] = then.into();
        }
        if let Some(then) = b.frame {
            all["cpu"]["was_frame_ms"] = then.into();
        }
        all["baseline"] = serde_json::json!({
            "frames": b.frames,
            "size": [b.size.0, b.size.1],
            "gone": gone(b, p),
        });
    }
    format!("{all}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_input_script_replaces_a_link_and_leaves_where_it_led() {
        let d = std::env::temp_dir().join(format!("wrela-profile-input-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("baseline")).expect("baseline");
        std::fs::write(d.join("replay.json"), "[]").expect("script");
        std::fs::write(d.join("other.json"), "keep").expect("other file");
        let to = d.join("baseline/input.json");
        std::os::unix::fs::symlink(d.join("other.json"), &to).expect("link");
        place_input(Some(&d.join("replay.json")), &d.join("baseline")).expect("placed");
        assert_eq!(std::fs::read_to_string(d.join("other.json")).expect("other file"), "keep");
        assert!(!std::fs::symlink_metadata(&to).expect("input").file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&to).expect("input"), "[]");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn baseline(serial: bool) -> Baseline {
        Baseline {
            environment: serde_json::json!({}),
            workload: serde_json::json!({}),
            frames: 120,
            size: (1280, 720),
            serial,
            span: None,
            frame: None,
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn a_baseline_must_use_the_same_serial_mode() {
        assert_eq!(baseline_problem(false, &baseline(false)), None);
        assert_eq!(baseline_problem(true, &baseline(true)), None);
        assert_eq!(
            baseline_problem(false, &baseline(true)).as_deref(),
            Some(
                "the baseline ran with `--serial`, but this profile would run without `--serial`; compare profiles made in the same mode"
            )
        );
        assert_eq!(
            baseline_problem(true, &baseline(false)).as_deref(),
            Some(
                "the baseline ran without `--serial`, but this profile would run with `--serial`; compare profiles made in the same mode"
            )
        );
    }

    /// The 95th percentile by nearest rank: of 120 frames the seventh slowest, of one frame
    /// that frame, of none nothing.
    #[test]
    fn the_slowest_frames_come_first_with_their_largest_parts() {
        let f = |frame, cpu: f64, labels: Vec<(&str, f64)>| Worst {
            frame,
            cpu,
            labels: labels.into_iter().map(|(n, ms)| (n.to_string(), ms)).collect(),
            ..Worst::default()
        };
        let frames =
            vec![f(1, 2.0, vec![]), f(2, 9.0, vec![("a", 1.0), ("b", 3.0)]), f(3, 5.0, vec![])];
        let worst = slowest(frames, 2, |w| w.cpu);
        assert_eq!(worst.iter().map(|w| w.frame).collect::<Vec<_>>(), [2, 3]);
        assert_eq!(worst[0].labels[0].0, "b");
    }

    /// `--worst 2` in Chrome names two paced frames, each with its interval, its call, its
    /// phases and its labels (the paced run's `frames.json` carries them).
    #[test]
    #[ignore = "needs a GPU and Chrome"]
    fn chrome_names_the_two_slowest_frames() {
        let dir = wrela_tests_root().join("compiler/tests/frozen/hello-field");
        let args: Vec<String> = [
            dir.to_str().unwrap(),
            "--chrome",
            "--worst",
            "2",
            "--frames",
            "20",
            "--warmup",
            "5",
            "--json",
        ]
        .map(String::from)
        .to_vec();
        let report = run_report(&parse(&args).unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&report).unwrap();
        let worst = v["worst"].as_array().unwrap();
        assert_eq!(worst.len(), 2);
        for w in worst {
            assert!(w["interval_ms"].as_f64().unwrap() > 0.0, "{w}");
            assert!(w["cpu_ms"].as_f64().is_some() && w["phases"].is_array(), "{w}");
            assert!(!w["labels"].as_array().unwrap().is_empty(), "{w}");
        }
        assert!(worst[0]["interval_ms"].as_f64() >= worst[1]["interval_ms"].as_f64());
        let framed =
            std::fs::read_to_string(dir.join("build/profile/results/frames.json")).unwrap();
        let framed: serde_json::Value = serde_json::from_str(&framed).unwrap();
        assert!(framed["labels"][10][0][0].is_string(), "a frame's labels");
    }

    fn wrela_tests_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn percentiles_are_by_nearest_rank() {
        let frames: Vec<f64> = (1..=120).rev().map(f64::from).collect();
        assert_eq!(percentile(&frames, 95), Some(114.0));
        assert_eq!(percentile(&[3.0], 95), Some(3.0));
        assert_eq!(percentile(&[], 95), None);
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 50), Some(2.0));
    }

    #[cfg(unix)]
    #[test]
    fn profile_refuses_a_linked_build_directory() {
        let dir = std::env::temp_dir().join(format!("wrela-profile-link-{}", std::process::id()));
        let outside =
            std::env::temp_dir().join(format!("wrela-profile-outside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&dir).expect("package");
        std::fs::create_dir_all(&outside).expect("outside");
        std::os::unix::fs::symlink(&outside, dir.join("build")).expect("linked build");
        assert!(load(&dir, None, &Options::default(), None, false).is_err());
        assert!(std::fs::read_dir(&outside).expect("outside directory").next().is_none());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(outside);
    }
}
