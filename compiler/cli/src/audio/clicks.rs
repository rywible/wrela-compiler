//! `wrela audio clicks <file.wav> [--notes <take.json>]`: the clicks in a recording, for an
//! agent that can't hear them. A click is a burst of high frequencies, sharp beside what's
//! around it: the signal's second difference (which favours the highs), its energy over 1 ms,
//! more than 12 dB over its median within 50 ms either side, and the most within 2 ms. With a
//! take's notes (`wrela audio <package> describe`), each click is put with what's happening
//! when it starts: a note struck (within 8 ms after an onset), a note let go or the pedal
//! lifted (within 40 ms after a release, or the sustain pedal rising past half its travel), or
//! neither.

use super::dsp;
use super::measure::round;
use std::path::Path;

const DB_OVER: f64 = 12.0;

pub(super) struct Click {
    pub at: f64,
    pub db: f64,
}

/// The clicks in `mono` (at `rate`).
pub(super) fn clicks(mono: &[f32], rate: f64) -> Vec<Click> {
    let step = ((rate / 2000.0) as usize).max(1); // 0.5 ms, at least one sample
    let width = ((rate / 1000.0) as usize).max(1); // 1 ms, at least one sample
    let d: Vec<f64> = (2..mono.len())
        .map(|i| f64::from(mono[i]) - 2.0 * f64::from(mono[i - 1]) + f64::from(mono[i - 2]))
        .collect();
    let frames: Vec<f64> = (0..d.len().saturating_sub(width) / step)
        .map(|f| {
            let w = &d[f * step..f * step + width];
            (w.iter().map(|x| x * x).sum::<f64>() / width as f64).max(1e-30)
        })
        .collect();
    let around = ((rate * 0.05 / step as f64).round() as usize).max(1);
    let local = ((rate * 0.002 / step as f64).round() as usize).max(1);
    let mut out = Vec::new();
    let mut near = Vec::with_capacity(2 * around);
    let mut i = 0;
    while i < frames.len() {
        if frames[i] <= 1e-10 {
            i += 1;
            continue;
        }
        // Only a local peak can be a click. Most frames need no median at all.
        let local_max = (i.saturating_sub(local)..(i + local + 1).min(frames.len()))
            .all(|j| frames[j] <= frames[i]);
        if !local_max {
            i += 1;
            continue;
        }
        let lo = i.saturating_sub(around);
        let hi = (i + around).min(frames.len());
        near.clear();
        near.extend_from_slice(&frames[lo..hi]);
        let middle = near.len() / 2;
        let (_, median, _) = near.select_nth_unstable_by(middle, f64::total_cmp);
        // Silence's median is no level to stand out from: a click must be over -100 dB too.
        let median = median.max(1e-10);
        let db = 10.0 * (frames[i] / median).log10();
        if db > DB_OVER {
            out.push(Click { at: (i * step) as f64 / rate, db });
            i += local;
        } else {
            i += 1;
        }
    }
    out
}

pub fn run(file: &Path, notes: Option<&Path>) -> Result<(), String> {
    let (samples, channels, rate) = dsp::read_wav(file)?;
    let mono: Vec<f32> =
        samples.chunks_exact(channels).map(|c| c.iter().sum::<f32>() / channels as f32).collect();
    let found = clicks(&mono, rate);
    let seconds = mono.len() as f64 / rate;
    // What's happening at each click, with a take's notes.
    let (mut onsets, mut releases) = (Vec::new(), Vec::new());
    if let Some(path) = notes {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("can't read {}: {e}", path.display()))?;
        let take: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        for n in take["notes"].as_array().ok_or("the take has no notes")? {
            onsets.push(n["onset"].as_f64().unwrap_or(0.0));
            releases.push(n["release"].as_f64().unwrap_or(0.0));
        }
        // The pedal rising past half its travel lifts the dampers... and falling past it lets
        // them land: either is a release of sorts.
        if let Some(points) = take["sustain"].as_array() {
            for w in points.windows(2) {
                let (a, b) = (&w[0], &w[1]);
                let (da, db) = (a[1].as_f64().unwrap_or(0.0), b[1].as_f64().unwrap_or(0.0));
                if (da - 0.5) * (db - 0.5) < 0.0 {
                    releases.push(b[0].as_f64().unwrap_or(0.0));
                }
            }
        }
    }
    let near =
        |list: &[f64], t: f64, within: f64| list.iter().any(|x| t >= *x - 0.002 && t <= x + within);
    let kind = |c: &Click| {
        if notes.is_none() {
            "unknown"
        } else if near(&onsets, c.at, 0.008) {
            "onset"
        } else if near(&releases, c.at, 0.040) {
            "release"
        } else {
            "other"
        }
    };
    let count = |k: &str| found.iter().filter(|c| kind(c) == k).count();
    let mean_db = |k: &str| {
        let v: Vec<f64> = found.iter().filter(|c| kind(c) == k).map(|c| c.db).collect();
        if v.is_empty() { None } else { Some(round(v.iter().sum::<f64>() / v.len() as f64, 1)) }
    };
    // How long after the last release each click elsewhere comes: a pattern there (a click
    // every few ms for a while after each release) is a damper's doing.
    let mut since: Vec<f64> = found
        .iter()
        .filter(|c| kind(c) == "other")
        .filter_map(|c| {
            releases.iter().filter(|r| **r <= c.at).map(|r| c.at - r).min_by(f64::total_cmp)
        })
        .map(|s| 1000.0 * s)
        .collect();
    since.sort_by(f64::total_cmp);
    let quartile = |q: usize| since.get(since.len() * q / 4).map(|x| round(*x, 0));
    let mut strongest: Vec<&Click> = found.iter().collect();
    strongest.sort_by(|a, b| b.db.total_cmp(&a.db));
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "file": file.display().to_string(),
            "seconds": round(seconds, 1),
            "clicks": found.len(),
            "per_minute": round(found.len() as f64 / seconds * 60.0, 1),
            "at_onsets": count("onset"), "at_onsets_mean_db": mean_db("onset"),
            "at_releases": count("release"), "at_releases_mean_db": mean_db("release"),
            "elsewhere": count("other"), "elsewhere_mean_db": mean_db("other"),
            "elsewhere_ms_after_a_release": { "quartile_1": quartile(1), "median": quartile(2), "quartile_3": quartile(3) },
            "strongest": strongest.iter().take(12).map(|c| serde_json::json!({
                "at": round(c.at, 3), "db": round(c.db, 1), "when": kind(c),
            })).collect::<Vec<_>>(),
        }))
        .expect("json")
    );
    Ok(())
}
