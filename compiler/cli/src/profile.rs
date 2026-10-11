//! `wrela profile <package-dir> [--frames n] [--size WxH] [--input script.json] [--serial]
//! [--json] [--baseline report.json]`: where a frame's time goes, on this machine. It builds the
//! program as `wrela build` does, runs its frames on the native host's GPU (in lockstep with its
//! ticker, 60 a second; 120 frames unless said), and reports:
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
//! changed, and the labels no longer run are listed.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;
use wrela_host::{GpuTiming, Host, Options, Timing};

struct Args {
    dir: PathBuf,
    frames: u32,
    size: (u32, u32),
    input: Option<PathBuf>,
    serial: bool,
    json: bool,
    baseline: Option<PathBuf>,
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
    };
    let mut dir = None;
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--frames" => a.frames = it.next()?.parse().ok().filter(|&n| n > 0)?,
            "--size" => a.size = crate::parse_size(it.next()?)?,
            "--input" => a.input = Some(PathBuf::from(it.next()?)),
            "--serial" => a.serial = true,
            "--json" => a.json = true,
            "--baseline" => a.baseline = Some(PathBuf::from(it.next()?)),
            _ if dir.is_none() && !x.starts_with('-') => dir = Some(PathBuf::from(x)),
            _ => return None,
        }
    }
    a.dir = dir?;
    Some(a)
}

/// What a profile found.
struct Profile {
    /// Each label's time: the longest first.
    labels: Vec<Label>,
    /// A frame's span on the GPU, ms: the median and the 95th percentile.
    span: Option<Times>,
    /// A frame call's time on the CPU, ms: the median and the 95th percentile.
    frame: Times,
    phases: wrela_host::Traced,
    /// Each pipeline's name and WGSL bytes, the largest first.
    wgsl: Vec<(String, u64)>,
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

/// An earlier report (`--json`) that this one is compared with: how it ran, and its medians.
struct Baseline {
    frames: u64,
    size: (u64, u64),
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
        let labels = v["gpu"]["labels"]
            .as_array()
            .ok_or_else(|| format!("{} has no `gpu.labels`", path.display()))?
            .iter()
            .filter_map(|l| Some((l["label"].as_str()?.to_string(), l["median_ms"].as_f64()?)))
            .collect();
        Ok(Baseline {
            frames: v["frames"].as_u64().unwrap_or(0),
            size: (v["size"][0].as_u64().unwrap_or(0), v["size"][1].as_u64().unwrap_or(0)),
            span: v["gpu"]["span_ms"].as_f64(),
            frame: v["cpu"]["frame_ms"].as_f64(),
            labels,
        })
    }
}

pub fn run(args: &[String]) -> ExitCode {
    let Some(a) = parse(args) else { return crate::usage() };
    // The baseline is read first, so a wrong path doesn't wait for a profile.
    let baseline = match a.baseline.as_deref().map(Baseline::read).transpose() {
        Ok(b) => b,
        Err(why) => {
            eprintln!("error: {why}");
            return ExitCode::from(2);
        }
    };
    match profile(&a) {
        Ok(p) => {
            if a.json {
                print!("{}", json(&a, &p, baseline.as_ref()));
            } else {
                print!("{}", text(&a, &p, baseline.as_ref()));
            }
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::from(2)
        }
    }
}

fn profile(a: &Args) -> Result<Profile, String> {
    let out = wrela_driver::build(&a.dir);
    let built = a.dir.join("build").join("profile");
    crate::write_build(&out, &built)?;
    let script = match &a.input {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| format!("can't read {}: {e}", p.display()))?;
            wrela_abi::input::parse_script(&text)?
        }
        None => Vec::new(),
    };
    let timing = if a.serial { Timing::Serial } else { Timing::Span };
    let options = Options { timing, quiet: true, defer_init: true, ..Options::default() };
    let mut host = Host::load_with(&built, &options).map_err(|e| e.to_string())?;
    host.init().map_err(|e| format!("in `init`: {e}"))?;
    let (w, h) = a.size;
    let mut calls = Vec::with_capacity(a.frames as usize);
    for k in 0..a.frames {
        let t = Instant::now();
        host.lockstep_frame(k, 60.0, w, h, &script).map_err(|e| format!("in frame {k}: {e}"))?;
        calls.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let timings = host.take_timings().map_err(|e| e.to_string())?;
    let phases = host.trace().take();
    drop(host);
    let spans: Vec<f64> = wrela_host::frame_spans(&timings).into_iter().map(|(_, ms)| ms).collect();
    Ok(Profile {
        labels: by_label(&timings),
        span: Times::of(spans),
        frame: Times::of(calls).unwrap_or_default(),
        phases,
        wgsl: wgsl(&built)?,
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
    let mut s = format!("`{}`: {} frames at {w}×{h}\n", a.dir.display(), a.frames);
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
        "  {:<32} {:>9.3} ms {:>9.3} ms{} (the host's work included)\n",
        "a frame's call",
        p.frame.median,
        p.frame.p95,
        was(baseline, baseline.and_then(|b| b.frame), p.frame.median)
    ));
    if !p.phases.phases.is_empty() {
        s.push_str(&format!("  phases over the run: {}\n", p.phases.phases_line()));
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
    let mut all = serde_json::json!({
        "frames": a.frames,
        "size": [a.size.0, a.size.1],
        "serial": a.serial,
        "gpu": {
            "span_ms": span.map(|t| t.median),
            "span_p95_ms": span.map(|t| t.p95),
            "labels": labels,
        },
        "cpu": { "frame_ms": p.frame.median, "frame_p95_ms": p.frame.p95, "phases": phases },
        "wgsl": wgsl,
    });
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

    /// The 95th percentile by nearest rank: of 120 frames the seventh slowest, of one frame
    /// that frame, of none nothing.
    #[test]
    fn percentiles_are_by_nearest_rank() {
        let frames: Vec<f64> = (1..=120).rev().map(f64::from).collect();
        assert_eq!(percentile(&frames, 95), Some(114.0));
        assert_eq!(percentile(&[3.0], 95), Some(3.0));
        assert_eq!(percentile(&[], 95), None);
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 50), Some(2.0));
    }
}
