//! `wrela profile --chrome`: the same report from headless Chrome, through test mode
//! (runtime/browser `testmode.ts`). Two runs of the build, each its warmup and measured frames:
//! - saturated (`timestamps=1&saturate=1&nohash=1`, or `timestamps=2` with `--serial`): each
//!   frame starts once the last is recorded, at most two on the GPU, so the GPU's times are its
//!   work; and the executor's counters;
//! - paced (`paced=1&inflight=2&nohash=1`): frames at their times, two in flight, no timestamp
//!   queries, as a player's are: each frame's CPU time (the program's `frame`, its commands
//!   encoded, up to submission), its phases, and the intervals between frames.
//!
//! Both run with the command checker off (`checks=0`), as a release build plays, unless
//! `--checked` keeps it (to compare with a build whose runtime predates `checks`).
//! `--features` passes test mode's mask (`features=none`, or a comma list to keep). Chrome's
//! timestamps are full resolution under the developer flag `runtime/chrome` sets.

use super::{Args, Label, Profile, Times, by_label, median};
use serde_json::Value;
use std::path::Path;

/// One test-mode run of the page at `built`, its `results/` read: each named file's JSON.
fn run_page(
    built: &Path,
    fragment: &str,
    frames: u32,
    files: &[&str],
) -> Result<Vec<Value>, String> {
    let built = std::path::absolute(built).map_err(|e| format!("{}: {e}", built.display()))?;
    // Real time for the paced run, and the GPU's pipelines first: generous, but bounded.
    let timeout = std::time::Duration::from_secs_f64((f64::from(frames) / 60.0 * 10.0).max(120.0));
    // The build's directory alone is served: it is the page.
    let run = || {
        wrela_host::pages::run(wrela_host::pages::Page {
            root: &built,
            page: "",
            fragment,
            timeout,
            script: None,
        })
    };
    // A browser that quits under a long series of runs says nothing of the build: once, again.
    if let Err(why) = run() {
        if !why.contains("chrome exited") {
            return Err(format!("Chrome's run failed: {why}"));
        }
        eprintln!("Chrome quit ({why}); running it again");
        run().map_err(|why| format!("Chrome's run failed: {why}"))?;
    }
    let results = built.join("results");
    files
        .iter()
        .map(|name| {
            let text = std::fs::read_to_string(results.join(name))
                .map_err(|e| format!("Chrome's `{name}`: {e}"))?;
            serde_json::from_str(&text).map_err(|e| format!("Chrome's `{name}`: {e}"))
        })
        .collect()
}

/// The fragment's parameters every run shares.
fn common(a: &Args) -> String {
    let (w, h) = a.size;
    let mut s = format!(
        "&frames={}&width={w}&height={h}&fps=60&workers={}",
        a.warmup + a.frames,
        a.workers
    );
    if a.input.is_some() {
        s.push_str("&input=input.json");
    }
    if let Some(keep) = &a.features {
        s.push_str(&format!("&features={keep}"));
    }
    if !a.checked {
        s.push_str("&checks=0");
    }
    s
}

/// The numbers of `v` (a JSON array) from index `from`.
fn numbers(v: &Value, from: usize) -> Vec<f64> {
    v.as_array()
        .map_or_else(Vec::new, |xs| xs.iter().skip(from).filter_map(Value::as_f64).collect())
}

/// What Chrome measured of the build at `built` (its input script, if any, beside it as
/// `input.json`), and the adapter it ran on (`device.json`).
pub(super) fn measure(a: &Args, built: &Path) -> Result<(Profile, Value), String> {
    let frames = a.warmup + a.frames;
    let timestamps = if a.serial { 2 } else { 1 };
    let fragment = format!("#test{}&timestamps={timestamps}&saturate=1&nohash=1", common(a));
    let [timings, counters, device] = <[Value; 3]>::try_from(run_page(
        built,
        &fragment,
        frames,
        &["timings.json", "counters.json", "device.json"],
    )?)
    .map_err(|_| "Chrome's results are incomplete")?;
    // The paced run: CPU times, phases and frame intervals as a player sees them.
    let fragment = format!("#test{}&paced=1&inflight=2&nohash=1", common(a));
    let [framed] = <[Value; 1]>::try_from(run_page(built, &fragment, frames, &["frames.json"])?)
        .map_err(|_| "Chrome's paced results are incomplete")?;
    let warmup = a.warmup as usize;
    let timings: Vec<wrela_host::GpuTiming> = timings
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            Some(wrela_host::GpuTiming {
                frame: t["frame"].as_u64()? as usize,
                label: t["label"].as_str()?.to_string(),
                nanos: t["nanos"].as_f64()?,
                start: t["start"].as_f64()?,
                end: t["end"].as_f64()?,
                submission: t["submission"].as_u64()? as usize,
            })
        })
        .filter(|t| t.frame >= warmup)
        .collect();
    let spans: Vec<f64> = wrela_host::frame_spans(&timings).into_iter().map(|(_, ms)| ms).collect();
    let calls = numbers(&framed["cpu_ms"], warmup);
    let began = numbers(&framed["began_ms"], 0);
    let init_warmup_ms = match (began.first(), began.get(warmup)) {
        (Some(first), Some(measured)) => measured - first,
        _ => 0.0,
    };
    // Each phase's time summed over the measured frames, in how many frames it ran.
    let mut phases: Vec<wrela_host::Phase> = Vec::new();
    for frame in framed["phases"].as_array().into_iter().flatten().skip(warmup) {
        for (name, ms) in frame.as_object().into_iter().flatten() {
            let ms = ms.as_f64().unwrap_or(0.0);
            match phases.iter_mut().find(|p| &p.name == name) {
                Some(p) => {
                    p.seconds += ms / 1e3;
                    p.times += 1;
                }
                None => phases.push(wrela_host::Phase {
                    name: name.clone(),
                    seconds: ms / 1e3,
                    times: 1,
                }),
            }
        }
    }
    let per_frame: Vec<&Value> = counters.as_array().into_iter().flatten().skip(warmup).collect();
    let counters = super::counter_names()
        .map(|name| {
            let xs: Vec<f64> = per_frame.iter().filter_map(|c| c[name].as_f64()).collect();
            (name, median(xs).unwrap_or(0.0))
        })
        .collect();
    let intervals: Vec<f64> =
        began.windows(2).skip(warmup.saturating_sub(1)).map(|w| w[1] - w[0]).collect();
    let labels: Vec<Label> = by_label(&timings);
    Ok((
        Profile {
            host: "chrome",
            environment: Value::Null,
            workload: Value::Null,
            build_sha256: super::comparison::build_hash(built)?,
            init_warmup_ms,
            labels,
            span: Times::of(spans),
            frame: Times::of(calls).unwrap_or_default(),
            interval: Some(Times::of(intervals).unwrap_or_default()),
            counters,
            phases: wrela_host::Traced { lines: Vec::new(), phases },
            wgsl: super::wgsl(built)?,
        },
        device,
    ))
}

/// The machine and the adapter Chrome ran on (`device.json`), as a report names its hardware.
pub(super) fn environment(device: &Value) -> Result<Value, String> {
    let mut v = super::comparison::machine()?;
    let part = |k: &str| device[k].as_str().unwrap_or("").to_string();
    let adapter = [part("vendor"), part("architecture"), part("device"), part("description")]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    v["adapter"] = format!("Chrome: {adapter}").into();
    v["host"] = "chrome".into();
    v["features"] = device["features"].clone();
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_runs_pass_the_feature_mask_and_the_input() {
        let mut a = super::super::parse(&["scene".into(), "--chrome".into()]).unwrap();
        assert_eq!(
            common(&a),
            format!("&frames=180&width=1280&height=720&fps=60&workers={}&checks=0", a.workers)
        );
        a.features = Some("none".into());
        a.input = Some("input.json".into());
        assert!(common(&a).ends_with("&input=input.json&features=none&checks=0"));
        a.checked = true;
        assert!(common(&a).ends_with("&features=none"));
        assert!(
            super::super::parse(&["scene".into(), "--features".into(), "none".into()]).is_none()
        );
        assert!(
            super::super::parse(&[
                "scene".into(),
                "--chrome".into(),
                "--features".into(),
                "a b".into()
            ])
            .is_none()
        );
    }
}
