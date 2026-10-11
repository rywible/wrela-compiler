//! `wrela audio model <dir> --out <file.wrela> [--against <dir>]`: fits engine::piano's `Model`
//! to recorded notes. `<dir>` holds `<key>-<velocity>.wav`, each one note (MIDI's key and
//! velocity) struck at the start and held, for 16 keys at 5 velocities. For each key the fit
//! measures:
//! - its tuning and inharmonicity (B), from its partials' frequencies;
//! - where its hammer strikes, from the partials it leaves quiet;
//! - its partials' levels when struck, at each velocity, by frequency band (the strike's
//!   quiet partials put back first, so a band's level is the hammer's and the board's);
//! - its partials' decays, by band: the prompt sound's and the aftersound's times to fall
//!   60 dB, and the aftersound's level.
//!
//! A band's numbers are fitted by least squares to its partials, as engine::piano reads them
//! (linearly between bands' centres), a little smoothed. Measuring a level takes a window of a
//! third of a second, in which a fast decay falls a few dB, so a model made from measurements
//! alone plays quieter partials than it measured. `--against <dir>` closes that loop: it's the
//! same notes played by the model now in `--out` (by `examples/piano`), and the fit moves each
//! number by what separates its measurement from the recording's. A few rounds settle it.
//!
//! It writes the model as a wrela constant, `GRAND`, with its source said in a comment.

use super::measure::{Measured, line, measure, two_stage};
use std::collections::BTreeMap;
use std::path::Path;

/// The bands' count. Their centres run from 25 Hz to 12 kHz, evenly in log frequency, as
/// engine::piano spaces them.
pub const BANDS: usize = 16;
const KEYS: usize = 16;
const VELOCITIES: usize = 5;

/// The band (as a fraction between centres) of a frequency.
fn band_of(hz: f64) -> f64 {
    ((hz / 25.0).ln() / (12000.0f64 / 25.0).ln() * (BANDS - 1) as f64)
        .clamp(0.0, (BANDS - 1) as f64)
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(f64::total_cmp);
    Some(xs[xs.len() / 2])
}

/// A band table fitted to `points` (a band position and a value each): least squares as
/// engine::piano interpolates (each point is its two nearest bands, weighted by nearness),
/// with a little smoothing. Bands below the points take the first band they reach; above
/// them, each falls `slope` from the one before.
fn band_fit(points: &[(f64, f64)], slope: f64) -> Vec<f64> {
    if points.is_empty() {
        return vec![0.0; BANDS];
    }
    let lo = points.iter().map(|(b, _)| b.floor() as usize).min().unwrap_or(0);
    let hi =
        points.iter().map(|(b, _)| (b.ceil() as usize).min(BANDS - 1)).max().unwrap_or(0).max(lo);
    let n = hi - lo + 1;
    // The normal equations: (WᵀW + λ DᵀD) x = Wᵀy, D the second difference.
    let mut a = vec![vec![0.0; n]; n];
    let mut rhs = vec![0.0; n];
    for (b, y) in points {
        let i = (b.floor() as usize).min(hi) - lo;
        let f = b - b.floor();
        let w = [(i, 1.0 - f), ((i + 1).min(n - 1), f)];
        for (r, wr) in w {
            rhs[r] += wr * y;
            for (c, wc) in w {
                a[r][c] += wr * wc;
            }
        }
    }
    let lambda = 0.05 * (points.len() as f64 / n as f64).max(1.0);
    for i in 1..n.saturating_sub(1) {
        let d = [(i - 1, 1.0), (i, -2.0), (i + 1, 1.0)];
        for (r, wr) in d {
            for (c, wc) in d {
                a[r][c] += lambda * wr * wc;
            }
        }
    }
    // A band with no point near it is held near its neighbours by the smoothing alone; a tiny
    // pull to the points' mean keeps the system solvable.
    let mean = points.iter().map(|(_, y)| y).sum::<f64>() / points.len() as f64;
    for i in 0..n {
        a[i][i] += 1e-6;
        rhs[i] += 1e-6 * mean;
    }
    let x = solve(a, rhs);
    (0..BANDS)
        .map(|b| {
            if b < lo {
                x[0]
            } else if b > hi {
                x[n - 1] + slope * (b - hi) as f64
            } else {
                x[b - lo]
            }
        })
        .collect()
}

/// `a x = b`, by Gaussian elimination with partial pivoting.
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Vec<f64> {
    let n = b.len();
    for c in 0..n {
        let p = (c..n).max_by(|i, j| a[*i][c].abs().total_cmp(&a[*j][c].abs())).unwrap_or(c);
        a.swap(c, p);
        b.swap(c, p);
        let d = if a[c][c].abs() < 1e-12 { 1e-12 } else { a[c][c] };
        let (pivot, below) = a.split_at_mut(c + 1);
        let pivot = &pivot[c];
        for (r, row) in below.iter_mut().enumerate() {
            let f = row[c] / d;
            for (x, p) in row[c..].iter_mut().zip(&pivot[c..]) {
                *x -= f * p;
            }
            b[c + 1 + r] -= f * b[c];
        }
    }
    let mut x = vec![0.0; n];
    for r in (0..n).rev() {
        let s: f64 = (r + 1..n).map(|k| a[r][k] * x[k]).sum();
        x[r] = (b[r] - s) / if a[r][r].abs() < 1e-12 { 1e-12 } else { a[r][r] };
    }
    x
}

/// Where the hammer strikes, as a fraction of the string: the fraction whose quiet partials
/// (those with a node there) best explain the dips in `m`'s levels.
fn strike(m: &Measured) -> f64 {
    let levels: Vec<(u32, f64)> = m.partials.iter().map(|p| (p.n, p.peak_db())).collect();
    // Each level's difference from its neighbours' median: its dip.
    let dips: Vec<(u32, f64)> = (0..levels.len())
        .map(|i| {
            let around: Vec<f64> = (i.saturating_sub(3)..(i + 4).min(levels.len()))
                .filter(|j| *j != i)
                .map(|j| levels[j].1)
                .collect();
            (levels[i].0, levels[i].1 - median(around).unwrap_or(levels[i].1))
        })
        .collect();
    let mut best = (0.125, f64::MIN);
    let mut x = 0.07;
    while x <= 0.17 {
        // How well 20 log₁₀ |sin(π n x)| follows the dips: their correlation.
        let comb: Vec<f64> = dips
            .iter()
            .map(|(n, _)| {
                20.0 * (std::f64::consts::PI * f64::from(*n) * x).sin().abs().max(0.05).log10()
            })
            .collect();
        let d: Vec<f64> = dips.iter().map(|(_, d)| *d).collect();
        let (mc, md) =
            (comb.iter().sum::<f64>() / comb.len() as f64, d.iter().sum::<f64>() / d.len() as f64);
        let cov: f64 = comb.iter().zip(&d).map(|(c, d)| (c - mc) * (d - md)).sum();
        let (vc, vd): (f64, f64) =
            (comb.iter().map(|c| (c - mc).powi(2)).sum(), d.iter().map(|d| (d - md).powi(2)).sum());
        let r = cov / (vc * vd).sqrt().max(1e-12);
        if r > best.1 {
            best = (x, r);
        }
        x += 0.0025;
    }
    best.0
}

/// Recorded notes, measured: by key, then velocity.
struct Notes {
    keys: Vec<u32>,
    velocities: Vec<u32>,
    by: BTreeMap<u32, BTreeMap<u32, Measured>>,
}

fn read_notes(dir: &Path) -> Result<Notes, String> {
    let mut by: BTreeMap<u32, BTreeMap<u32, Measured>> = BTreeMap::new();
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("can't read {}: {e}", dir.display()))?;
    for e in entries.filter_map(Result::ok) {
        let path = e.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        if path.extension().is_none_or(|x| x != "wav") {
            continue;
        }
        let Some((key, vel)) = stem.split_once('-') else { continue };
        let (Ok(key), Ok(vel)) = (key.parse::<u32>(), vel.parse::<u32>()) else { continue };
        let (samples, channels, rate) = super::dsp::read_wav(&path)?;
        let mono: Vec<f32> = samples
            .chunks_exact(channels)
            .map(|c| c.iter().sum::<f32>() / channels as f32)
            .collect();
        let seconds = (mono.len() as f64 / rate - 0.5).min(12.0);
        by.entry(key).or_default().insert(vel, measure(&mono, rate, key, 0.0, seconds));
    }
    let keys: Vec<u32> = by.keys().copied().collect();
    let velocities: Vec<u32> = by
        .values()
        .next()
        .ok_or(format!("{} has no notes", dir.display()))?
        .keys()
        .copied()
        .collect();
    if keys.len() != KEYS
        || velocities.len() != VELOCITIES
        || by.values().any(|v| v.keys().copied().collect::<Vec<_>>() != velocities)
    {
        return Err(format!(
            "the model needs {KEYS} keys, each at the same {VELOCITIES} velocities; {} has {} keys at {:?}",
            dir.display(),
            keys.len(),
            velocities
        ));
    }
    Ok(Notes { keys, velocities, by })
}

/// A model's numbers, in the order engine::piano's `Model` keeps them.
struct Tables {
    inharmonicity: Vec<f64>,
    tuning: Vec<f64>,
    strike: Vec<f64>,
    levels: Vec<f64>,
    prompt: Vec<f64>,
    after: Vec<f64>,
    after_db: Vec<f64>,
}

/// The tables `notes` measure. `strikes`, if given, are used instead of measuring them, so a
/// model's own notes are measured with the strike points it was made with.
fn tables(notes: &Notes, strikes: Option<&[f64]>) -> (Tables, Vec<serde_json::Value>) {
    let mut t = Tables {
        inharmonicity: Vec::new(),
        tuning: Vec::new(),
        strike: Vec::new(),
        levels: Vec::new(),
        prompt: Vec::new(),
        after: Vec::new(),
        after_db: Vec::new(),
    };
    let mut report = Vec::new();
    for (i, key) in notes.keys.iter().enumerate() {
        let by_vel = &notes.by[key];
        // The loudest velocity but one: many partials, and the hammer not yet at its hardest.
        let main = &by_vel[&notes.velocities[3]];
        t.inharmonicity
            .push(median(by_vel.values().map(|m| m.inharmonicity).collect()).unwrap_or(0.0));
        t.tuning.push(median(by_vel.values().map(Measured::cents).collect()).unwrap_or(0.0));
        // Too few partials show no pattern of quiet ones: then 0.12, typical of a grand's.
        let x0 = match strikes {
            Some(s) => s[i],
            None if main.partials.len() >= 30 => strike(main).clamp(0.09, 0.15),
            None => 0.12,
        };
        t.strike.push(x0);
        for vel in &notes.velocities {
            let m = &by_vel[vel];
            let points: Vec<(f64, f64)> = m
                .partials
                .iter()
                .map(|p| {
                    let comb = (std::f64::consts::PI * f64::from(p.n) * x0).sin().abs().max(0.15);
                    (band_of(p.hz), p.peak_db() - 20.0 * comb.log10())
                })
                .collect();
            t.levels.extend(band_fit(&points, -10.0));
        }
        // Decays don't depend much on velocity: the three loudest velocities' partials, pooled.
        let (mut p, mut a, mut r) = (Vec::new(), Vec::new(), Vec::new());
        for vel in &notes.velocities[2..] {
            let m = &by_vel[vel];
            let loudest = m.partials.iter().map(|p| p.peak_db()).fold(-200.0, f64::max);
            for part in &m.partials {
                if part.peak_db() < loudest - 50.0 {
                    continue;
                }
                let Some(d) = two_stage(&part.levels, m.dt) else { continue };
                if d.error > 4.0 {
                    continue;
                }
                let b = band_of(part.hz);
                p.push((b, d.prompt.clamp(0.05, 60.0).ln()));
                a.push((b, d.after.clamp(0.1, 300.0).ln()));
                r.push((b, d.after_db.clamp(-50.0, -3.0)));
            }
        }
        // Above the last band measured, decays shorten: a band's time is 0.7 of the one below.
        let pf = band_fit(&p, 0.7f64.ln());
        let af = band_fit(&a, 0.7f64.ln());
        t.prompt.extend(pf.iter().map(|x| x.exp()));
        t.after.extend(af.iter().zip(&pf).map(|(a, p)| a.exp().max(p.exp())));
        t.after_db.extend(band_fit(&r, 0.0));
        report.push(serde_json::json!({
            "key": key, "inharmonicity": t.inharmonicity.last(), "cents": t.tuning.last(), "strike": x0,
            "partials": main.partials.len(),
        }));
    }
    // A top key has too few partials to fit B: its B follows the trend of the keys above middle
    // C that have enough (log B is close to a line in the key there). A tuning more than 30
    // cents out is a fundamental not found: its neighbours' is taken.
    let counts: Vec<usize> =
        notes.keys.iter().map(|k| notes.by[k][&notes.velocities[3]].partials.len()).collect();
    let trend: Vec<(f64, f64)> = notes
        .keys
        .iter()
        .zip(&counts)
        .zip(&t.inharmonicity)
        .filter(|((k, n), _)| **k >= 60 && **n >= 10)
        .map(|((k, _), b)| (f64::from(*k), b.ln()))
        .collect();
    if trend.len() >= 2 {
        let (slope, at0) = line(&trend);
        for ((key, count), b) in notes.keys.iter().zip(&counts).zip(&mut t.inharmonicity) {
            if *key >= 60 && *count < 10 {
                *b = (at0 + slope * f64::from(*key)).exp();
            }
        }
    }
    let tuning = t.tuning.clone();
    for i in 0..notes.keys.len() {
        if tuning[i].abs() > 30.0 {
            let near: Vec<f64> = (i.saturating_sub(2)..(i + 3).min(notes.keys.len()))
                .filter(|j| *j != i && tuning[*j].abs() <= 30.0)
                .map(|j| tuning[j])
                .collect();
            t.tuning[i] = median(near).unwrap_or(0.0);
        }
    }
    (t, report)
}

/// The numbers of array `name` in a model's source: `name: [ ... ]`.
pub(super) fn array(source: &str, name: &str) -> Option<Vec<f64>> {
    let start = source.find(&format!("{name}: ["))? + name.len() + 3;
    let end = start + source[start..].find(']')?;
    source[start..end]
        .split(',')
        .map(|x| x.trim())
        .filter(|x| !x.is_empty())
        .map(|x| x.parse().ok())
        .collect()
}

fn array_exact(source: &str, name: &str, expected: usize) -> Result<Vec<f64>, String> {
    let values = array(source, name).ok_or_else(|| format!("the current model has no `{name}`"))?;
    if values.len() != expected {
        return Err(format!(
            "the current model has {} `{name}` values, but needs {expected}",
            values.len()
        ));
    }
    Ok(values)
}

pub fn run(dir: &Path, out: &Path, against: Option<&Path>) -> Result<(), String> {
    let reference = read_notes(dir)?;
    let (mut t, report) = tables(&reference, None);
    if let Some(played) = against {
        // The model now, and how its notes measure: each number moves by what separates its
        // notes' measurement from the recording's.
        let source = std::fs::read_to_string(out)
            .map_err(|e| format!("can't read the model now, {}: {e}", out.display()))?;
        let (levels, prompt, after, after_db) = (
            array_exact(&source, "levels", t.levels.len())?,
            array_exact(&source, "prompt", t.prompt.len())?,
            array_exact(&source, "after", t.after.len())?,
            array_exact(&source, "after_db", t.after_db.len())?,
        );
        let (m, _) = tables(&read_notes(played)?, Some(&t.strike));
        t.levels = (0..levels.len())
            .map(|i| levels[i] + (t.levels[i] - m.levels[i]).clamp(-12.0, 12.0))
            .collect();
        let ratio = |now: &[f64], want: &[f64], got: &[f64], i: usize| {
            now[i] * (want[i] / got[i]).clamp(0.5, 2.0)
        };
        t.prompt = (0..prompt.len())
            .map(|i| ratio(&prompt, &t.prompt, &m.prompt, i).clamp(0.05, 60.0))
            .collect();
        t.after = (0..after.len())
            .map(|i| ratio(&after, &t.after, &m.after, i).clamp(0.1, 300.0))
            .collect();
        t.after_db = (0..after_db.len())
            .map(|i| {
                (after_db[i] + (t.after_db[i] - m.after_db[i]).clamp(-6.0, 6.0)).clamp(-50.0, -3.0)
            })
            .collect();
    }
    let numbers = |xs: &[f64], digits: usize| -> String {
        xs.chunks(BANDS)
            .map(|c| c.iter().map(|x| format!("{x:.digits$}")).collect::<Vec<_>>().join(", "))
            .collect::<Vec<_>>()
            .join(",\n        ")
    };
    let keys_f: Vec<f64> = reference.keys.iter().map(|k| f64::from(*k)).collect();
    let vels_f: Vec<f64> = reference.velocities.iter().map(|v| f64::from(*v) / 127.0).collect();
    let source = format!(
        "// Written by `wrela audio model {dir}` (spike 14, #48); edit the tool, not this file.\n\
         //\n\
         // engine::piano's model fitted to the recordings in {dir}: {n} notes, {k} keys at {v}\n\
         // velocities each. The numbers are measurements of those recordings, moved by rounds\n\
         // of the model's own notes measured against them.\n\
         \n\
         use piano::Model\n\
         \n\
         pub const GRAND: Model = Model {{\n    \
             keys: [{keys}],\n    \
             velocities: [{vels}],\n    \
             inharmonicity: [{b}],\n    \
             tuning: [{c}],\n    \
             strike: [{s}],\n    \
             levels: [\n        {levels}\n    ],\n    \
             prompt: [\n        {prompt}\n    ],\n    \
             after: [\n        {after}\n    ],\n    \
             after_db: [\n        {after_db}\n    ],\n\
         }}\n",
        dir = dir.display(),
        n = KEYS * VELOCITIES,
        k = KEYS,
        v = VELOCITIES,
        keys = numbers(&keys_f, 1),
        vels = numbers(&vels_f, 4),
        b = t.inharmonicity.iter().map(|x| format!("{x:.3e}")).collect::<Vec<_>>().join(", "),
        c = numbers(&t.tuning, 2),
        s = numbers(&t.strike, 4),
        levels = numbers(&t.levels, 1),
        prompt = numbers(&t.prompt, 3),
        after = numbers(&t.after, 2),
        after_db = numbers(&t.after_db, 1),
    );
    // As `wrela fmt` leaves it, so the file is formatted as every other is.
    let source = crate::fmt::format_text(&source).ok_or("the model's source doesn't parse")?;
    std::fs::write(out, source).map_err(|e| format!("can't write {}: {e}", out.display()))?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({ "model": out.display().to_string(), "keys": report })
        )
        .expect("json")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_model_tables_need_the_fitted_length() {
        let source = "levels: [1.0, 2.0]";
        assert_eq!(array_exact(source, "levels", 2).expect("matching table"), [1.0, 2.0]);
        assert!(array_exact(source, "levels", 3).is_err());
    }
}
