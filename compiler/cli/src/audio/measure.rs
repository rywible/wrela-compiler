//! A recorded note's partials, measured: their frequencies, their levels over time, and their
//! decays in two stages. `wrela audio partials` reports one note; `wrela audio model` fits
//! engine::piano's tables to many.

use super::dsp::{Spectrum, db, peak, spectrum};

/// A partial of a recorded note: its number, its frequency, and its level (dB of full scale)
/// every `Measured::dt` seconds.
pub struct Partial {
    pub n: u32,
    pub hz: f64,
    pub levels: Vec<f64>,
}

impl Partial {
    /// Its loudest level, near the start.
    pub fn peak_db(&self) -> f64 {
        self.levels.iter().take(20).copied().fold(-200.0, f64::max)
    }
}

/// A recorded note's partials.
pub struct Measured {
    pub nominal: f64,
    pub f0: f64,
    pub inharmonicity: f64,
    pub partials: Vec<Partial>,
    pub dt: f64,
}

impl Measured {
    /// The first partial's frequency, in cents from equal temperament at A4 = 440 Hz.
    pub fn cents(&self) -> f64 {
        1200.0 * (self.f0 * (1.0 + self.inharmonicity).sqrt() / self.nominal).log2()
    }
}

/// A note's partials, measured from `samples` (mono, at `rate`) from `from` seconds on, over
/// `seconds`: their frequencies, which fit an inharmonicity B, and their levels over time.
pub fn measure(samples: &[f32], rate: f64, key: u32, from: f64, seconds: f64) -> Measured {
    let n = 16384;
    let hop = 1024;
    let start = (from * rate) as usize;
    let nominal = 440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0);
    // The frequencies come from a long spectrum just after the strike (longer for low keys,
    // whose partials are close), searched twice: first where a typical piano's B puts each
    // partial, widely; then where the B those fit puts it, narrowly. A low key's fundamental
    // is often too weak to find, so no partial is relied on alone.
    let long = if key < 48 { 65536 } else { 16384 };
    let fine = rate / long as f64;
    let early = spectrum(samples, start + hop, long);
    let loudest = early.iter().copied().fold(0.0, f64::max);
    let typical = typical_b(key);
    // A sample can be 40 cents out (a top key's, recorded sharp): the first partials are
    // searched for within 3%.
    let wide = |f: f64, p: f64| {
        let off = if p <= 2.0 { 0.03 * f } else { 0.01 * f };
        (off + 0.3 * p * p * p * nominal * typical).min(0.4 * nominal).max(2.0 * fine)
    };
    let first =
        search(&early, fine, nominal, typical, loudest * 10f64.powf(-55.0 / 20.0), &wide, false);
    // Too few partials to fit (a top key's): the first one found sets f₀, with a typical B.
    let found_first = first.iter().find(|(p, _)| *p == 1).map(|(_, f)| f / (1.0 + typical).sqrt());
    let (f0, b) = fit(&first).unwrap_or((found_first.unwrap_or(nominal), typical));
    let narrow = |f: f64, _: f64| (0.008 * f).min(0.4 * nominal).max(4.0 * fine);
    let found = search(&early, fine, f0, b, loudest * 10f64.powf(-70.0 / 20.0), &narrow, true);
    let (f0, b) = fit(&found).unwrap_or((f0, b));
    let b = b.clamp(2e-5, 0.05);
    let bin = rate / n as f64;
    let frames = ((seconds * rate) as usize / hop).max(1);
    let mut levels: Vec<Vec<f64>> = vec![Vec::with_capacity(frames); found.len()];
    let mut spectrum = Spectrum::new(n);
    for t in 0..frames {
        let mag = spectrum.of(samples, start + t * hop);
        for (i, (_, f)) in found.iter().enumerate() {
            let k = (f / bin).round() as usize;
            let a = (k.saturating_sub(2)..=(k + 2).min(mag.len() - 1))
                .map(|j| mag[j])
                .fold(0.0, f64::max);
            levels[i].push(db(a));
        }
    }
    let partials =
        found.into_iter().zip(levels).map(|((n, hz), levels)| Partial { n, hz, levels }).collect();
    Measured { nominal, f0, inharmonicity: b, partials, dt: hop as f64 / rate }
}

/// A typical grand piano's inharmonicity at `key`: about 10⁻⁴ in the bass's wound strings,
/// doubling every 10 keys above them (Young; Fletcher and Rossing).
fn typical_b(key: u32) -> f64 {
    let k = f64::from(key);
    if k >= 40.0 {
        1.1e-4 * (0.0693 * (k - 40.0)).exp()
    } else {
        1.1e-4 * (0.035 * (40.0 - k)).exp()
    }
}

/// The partials found in `mag` (bins `bin` Hz apart) where `f0` and `b` put them, each within
/// `window(predicted, n)` Hz and louder than `floor`: their numbers and frequencies. With
/// `refit`, f₀ and B are fitted again to the partials found so far before each next one is
/// looked for: a B far from the guess puts the high partials many windows away from where it
/// says, and an error there grows as p³.
#[allow(clippy::too_many_arguments)]
fn search(
    mag: &[f64],
    bin: f64,
    f0: f64,
    b: f64,
    floor: f64,
    window: &dyn Fn(f64, f64) -> f64,
    refit: bool,
) -> Vec<(u32, f64)> {
    let mut found = Vec::new();
    let (mut f0, mut b) = (f0, b);
    for p in 1..=96u32 {
        if refit && found.len() >= 6 {
            (f0, b) = fit(&found).unwrap_or((f0, b));
        }
        let pf = f64::from(p);
        let predict = pf * f0 * (1.0 + b * pf * pf).sqrt();
        if predict > 14_000.0 || predict / bin >= (mag.len() - 2) as f64 {
            break;
        }
        let w = window(predict, pf);
        let (lo, hi) = (((predict - w) / bin).max(1.0) as usize, ((predict + w) / bin) as usize);
        let Some((k, a)) = peak(mag, lo, hi) else { continue };
        // A peak at the window's edge is a neighbour's slope, not this partial.
        if a < floor || k <= lo as f64 || k >= hi as f64 {
            continue;
        }
        found.push((p, k * bin));
    }
    found
}

/// f₀ and B fitted to partials' numbers and frequencies: (f_p / p)² = f₀² + f₀² B p² is a line
/// in p². `None` with fewer than four, or a fit that makes no sense.
fn fit(found: &[(u32, f64)]) -> Option<(f64, f64)> {
    if found.len() < 4 {
        return None;
    }
    let pts: Vec<(f64, f64)> =
        found.iter().map(|(q, f)| (f64::from(*q).powi(2), (f / f64::from(*q)).powi(2))).collect();
    let (slope, a0) = line(&pts);
    (a0 > 0.0 && slope >= 0.0).then(|| (a0.sqrt(), slope / a0))
}

/// A least-squares line through `pts`: its slope and its value at 0.
pub fn line(pts: &[(f64, f64)]) -> (f64, f64) {
    let m = pts.len() as f64;
    let (sx, sy) = pts.iter().fold((0.0, 0.0), |(x, y), (a, c)| (x + a, y + c));
    let (sxx, sxy) = pts.iter().fold((0.0, 0.0), |(x, y), (a, c)| (x + a * a, y + a * c));
    let slope = (m * sxy - sx * sy) / (m * sxx - sx * sx);
    (slope, (sy - slope * sx) / m)
}

/// A partial's decay in two stages: its prompt sound's and its aftersound's seconds to fall
/// 60 dB, and the aftersound's level when struck, in dB below the prompt sound's.
pub struct Decay {
    pub prompt: f64,
    pub after: f64,
    pub after_db: f64,
    /// The fit's RMS error, in dB.
    pub error: f64,
}

/// Fits a level's fall (dB, every `dt` seconds) with two exponentials:
/// L(t) = L₀ + 10 log₁₀((1 − r) 10^(−6t/T₁) + r 10^(−6t/T₂)), from its peak until it's 60 dB
/// down (or near the floor). `None` if there's too little of it.
pub fn two_stage(levels: &[f64], dt: f64) -> Option<Decay> {
    let (top, peak) = levels
        .iter()
        .enumerate()
        .take(20)
        .fold((0, -200.0), |m, (i, l)| if *l > m.1 { (i, *l) } else { m });
    let end = (top..levels.len())
        .find(|i| levels[*i] < (peak - 60.0).max(-110.0))
        .unwrap_or(levels.len());
    // Every point at first, then sparser: the slow part needs fewer.
    let pts: Vec<(f64, f64)> = (top..end)
        .filter(|i| (i - top) < 40 || (i - top) % 4 == 0)
        .map(|i| ((i - top) as f64 * dt, levels[i]))
        .collect();
    if pts.len() < 8 {
        return None;
    }
    // All mixtures on a grid share a small set of exponential curves. Compute
    // each curve once, then reuse the model levels for both residual passes.
    let curve = |log_seconds: f64| {
        let seconds = 10f64.powf(log_seconds);
        pts.iter().map(|(t, _)| 10f64.powf(-6.0 * t / seconds)).collect::<Vec<_>>()
    };
    let mut model = vec![0.0; pts.len()];
    let mut cost = |prompt: &[f64], after: &[f64], r: f64| -> f64 {
        for ((m, p), a) in model.iter_mut().zip(prompt).zip(after) {
            *m = 10.0 * ((1.0 - r) * p + r * a).max(1e-30).log10();
        }
        // L₀ is the mean residual.
        let l0 = pts.iter().zip(&model).map(|((_, l), m)| l - m).sum::<f64>() / pts.len() as f64;
        pts.iter().zip(&model).map(|((_, l), m)| (l - l0 - m).powi(2)).sum::<f64>()
    };
    // A coarse grid in logs, then finer ones around the best.
    let (mut lt1, mut lt2, mut lr) = (0.0f64, 1.0f64, -2.0f64);
    let mut best = f64::MAX;
    let afters: Vec<_> = (0..14)
        .map(|j| {
            let b = -0.5 + 3.0 * f64::from(j) / 13.0;
            (b, curve(b))
        })
        .collect();
    for i in 0..14 {
        let a = -1.0 + 2.6 * f64::from(i) / 13.0;
        let prompt = curve(a);
        for (b, after) in &afters {
            for k in 0..10 {
                let c = -4.0 + 3.7 * f64::from(k) / 9.0;
                if *b < a {
                    continue;
                }
                let e = cost(&prompt, after, 10f64.powf(c));
                if e < best {
                    (best, lt1, lt2, lr) = (e, a, *b, c);
                }
            }
        }
    }
    let mut step = (0.2, 0.23, 0.4);
    for _ in 0..4 {
        let (c1, c2, c3) = (lt1, lt2, lr);
        let afters: Vec<_> = (-2..=2)
            .map(|j| {
                let b = c2 + step.1 * f64::from(j) / 2.0;
                (b, curve(b))
            })
            .collect();
        for i in -2..=2 {
            let a = c1 + step.0 * f64::from(i) / 2.0;
            let prompt = curve(a);
            for (b, after) in &afters {
                for k in -2..=2 {
                    let c = (c3 + step.2 * f64::from(k) / 2.0).min(-0.01);
                    if *b < a {
                        continue;
                    }
                    let e = cost(&prompt, after, 10f64.powf(c));
                    if e < best {
                        (best, lt1, lt2, lr) = (e, a, *b, c);
                    }
                }
            }
        }
        step = (step.0 / 2.0, step.1 / 2.0, step.2 / 2.0);
    }
    let r = 10f64.powf(lr);
    Some(Decay {
        prompt: 10f64.powf(lt1),
        after: 10f64.powf(lt2),
        after_db: 10.0 * (r / (1.0 - r)).log10(),
        error: (best / pts.len() as f64).sqrt(),
    })
}

/// The strike's noise: in the first 43 ms, the energy between the partials (more than 70 Hz
/// from each, 200 Hz to 8 kHz) over the energy at them, in dB. The hammer's knock and the
/// action's are most of it. `None` where the partials are too close together to leave room
/// between them (a fundamental under 150 Hz).
pub fn onset_noise(samples: &[f32], rate: f64, m: &Measured, from: f64) -> Option<f64> {
    if m.f0 < 150.0 {
        return None;
    }
    let n = 2048;
    let bin = rate / n as f64;
    let mag = spectrum(samples, (from * rate) as usize, n);
    let (mut tonal, mut between) = (0.0, 0.0);
    for (k, a) in mag.iter().enumerate() {
        let hz = k as f64 * bin;
        if !(200.0..=8000.0).contains(&hz) {
            continue;
        }
        // The nearest partial, where f0 and B put it.
        let p = (hz / m.f0).round().max(1.0);
        let near = p * m.f0 * (1.0 + m.inharmonicity * p * p).sqrt();
        if (hz - near).abs() <= 70.0 {
            tonal += a * a;
        } else {
            between += a * a;
        }
    }
    (tonal > 0.0).then(|| 10.0 * (between.max(1e-30) / tonal).log10())
}

/// `wrela audio partials`' report: a note's partials as JSON.
pub fn report(samples: &[f32], rate: f64, key: u32, from: f64, seconds: f64) -> serde_json::Value {
    let m = measure(samples, rate, key, from, seconds);
    let noise = onset_noise(samples, rate, &m, from);
    let loudest = m.partials.iter().map(Partial::peak_db).fold(-200.0, f64::max);
    let partials: Vec<_> = m
        .partials
        .iter()
        .map(|p| {
            let d = two_stage(&p.levels, m.dt);
            serde_json::json!({
                "n": p.n,
                "hz": round(p.hz, 2),
                "cents_above_harmonic": round(1200.0 * (p.hz / (f64::from(p.n) * m.f0)).log2(), 2),
                "level_db": round(p.peak_db() - loudest, 1),
                "prompt_t60": d.as_ref().map(|d| round(d.prompt, 3)),
                "after_t60": d.as_ref().map(|d| round(d.after, 3)),
                "after_db": d.as_ref().map(|d| round(d.after_db, 1)),
                "fit_error_db": d.as_ref().map(|d| round(d.error, 2)),
            })
        })
        .collect();
    serde_json::json!({
        "key": key,
        "nominal_hz": round(m.nominal, 3),
        "f0_hz": round(m.f0, 3),
        "cents_from_equal": round(m.cents(), 2),
        "inharmonicity": m.inharmonicity,
        "loudest_db": round(loudest, 1),
        "onset_noise_db": noise.map(|x| round(x, 1)),
        "partials": partials,
    })
}

pub fn round(x: f64, places: i32) -> f64 {
    let s = 10f64.powi(places);
    (x * s).round() / s
}
