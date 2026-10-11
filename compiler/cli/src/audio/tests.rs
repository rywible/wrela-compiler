//! `wrela audio`'s measuring tools, on sounds made here whose numbers are known: struck strings
//! (their partials stretched by a known B, struck at a known point, each decaying in two
//! stages), and a sine with a click in it. The tools that render a piece (`wav`, `describe`,
//! `numbers`, `midi`, `sheet`, `speed`) are tested on `examples/piano` by the command's tests.

use super::{clicks, measure, model};
use std::f64::consts::{PI, TAU};
use std::path::PathBuf;

const RATE: f64 = 48000.0;

/// What a made note's string is.
#[derive(Clone, Copy)]
struct Made {
    key: u32,
    b: f64,
    /// Where the hammer strikes, as a fraction of the string.
    x0: f64,
    /// 0 to 1: louder and brighter when larger.
    velocity: f64,
}

/// A struck string, `seconds` long, mono: each partial a sine whose amplitude falls in two
/// stages, quiet where the strike point is a node of it.
fn string(m: Made, seconds: f64) -> Vec<f32> {
    let f0 = 440.0 * 2f64.powf((f64::from(m.key) - 69.0) / 12.0);
    let mut out = vec![0f64; (seconds * RATE) as usize];
    for p in 1..=60u32 {
        let pf = f64::from(p);
        let f = pf * f0 * (1.0 + m.b * pf * pf).sqrt();
        if f > 11_000.0 {
            break;
        }
        let comb = (PI * pf * m.x0).sin().abs();
        let a = 0.05 * m.velocity * comb / pf.powf(1.5 - 0.5 * m.velocity);
        // Seconds for each stage to fall 60 dB, and the aftersound's share.
        let (t1, t2, r) = (0.6 / (1.0 + 0.05 * pf), 6.0 / (1.0 + 0.05 * pf), 0.1);
        // A rotation each sample, not a sine: the tests make many notes.
        let (c, s) = ((TAU * f / RATE).cos(), (TAU * f / RATE).sin());
        let (d1, d2) = (10f64.powf(-3.0 / (t1 * RATE)), 10f64.powf(-3.0 / (t2 * RATE)));
        let (mut x, mut y, mut e1, mut e2) = (1.0, 0.0, a * (1.0 - r), a * r);
        for o in &mut out {
            *o += (e1 + e2) * y;
            (x, y) = (c * x - s * y, s * x + c * y);
            e1 *= d1;
            e2 *= d2;
        }
    }
    out.into_iter().map(|x| x as f32).collect()
}

/// A scratch directory of the tests' own, `name`, empty.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wrela-audio-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[test]
fn partials_finds_a_strings_tuning_and_stretch() {
    let made = Made { key: 48, b: 3e-4, x0: 0.12, velocity: 0.8 };
    let m = measure::measure(&string(made, 3.0), RATE, made.key, 0.0, 2.0);
    let nominal = 440.0 * 2f64.powf((48.0 - 69.0) / 12.0);
    assert!(m.cents().abs() < 1.0, "f0 {} for {nominal}: {} cents", m.f0, m.cents());
    assert!((m.inharmonicity / made.b - 1.0).abs() < 0.02, "B {}", m.inharmonicity);
    assert!(m.partials.len() >= 50, "{} partials", m.partials.len());
    for p in &m.partials {
        let pf = f64::from(p.n);
        let hz = pf * nominal * (1.0 + made.b * pf * pf).sqrt();
        assert!((p.hz - hz).abs() < 0.1, "partial {} at {} Hz for {hz}", p.n, p.hz);
    }
    // The strike point, 0.12 of the string, is a node of the 25th and 50th partials: they
    // aren't there to find.
    assert!(m.partials.iter().all(|p| p.n != 25 && p.n != 50));
    // Each partial's two stages: a prompt sound that dies faster than the aftersound.
    let second = &m.partials.iter().find(|p| p.n == 2).expect("the 2nd partial");
    let d = measure::two_stage(&second.levels, m.dt).expect("a decay");
    assert!(d.prompt < d.after, "prompt {} s, after {} s", d.prompt, d.after);
    // The report says the same.
    let r = measure::report(&string(made, 3.0), RATE, made.key, 0.0, 2.0);
    assert_eq!(r["key"], 48);
    assert!(r["cents_from_equal"].as_f64().unwrap().abs() < 3.0, "{}", r["cents_from_equal"]);
}

#[test]
fn clicks_finds_a_click_and_only_it() {
    let sine: Vec<f32> = (0..2 * RATE as usize)
        .map(|i| (0.3 * (TAU * 440.0 * i as f64 / RATE).sin()) as f32)
        .collect();
    let mut clicked = sine.clone();
    clicked[RATE as usize] += 0.3;
    let found = clicks::clicks(&sine, RATE);
    assert!(found.is_empty(), "{} clicks in a sine", found.len());
    let found = clicks::clicks(&clicked, RATE);
    assert_eq!(found.len(), 1, "clicks at {:?}", found.iter().map(|c| c.at).collect::<Vec<_>>());
    assert!((found[0].at - 1.0).abs() < 0.002, "a click at {} s", found[0].at);
}

#[test]
fn clicks_in_silence_keep_their_peak_times_and_levels() {
    let rate = 8000.0;
    let mut samples = vec![0.0; 24000];
    assert!(clicks::clicks(&samples, rate).is_empty());
    samples[8000] = 0.5;
    samples[16000] = 0.5;
    let found = clicks::clicks(&samples, rate);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].at, 0.9995);
    assert_eq!(found[1].at, 1.9995);
    let expected_db = 10.0 * (6.0 * 0.5f64.powi(2) / 8.0 / 1e-10).log10();
    assert_eq!(found[0].db, expected_db);
    assert_eq!(found[1].db, expected_db);
}

#[test]
fn spectrum_matches_a_direct_transform_and_clears_reused_windows() {
    let n = 64;
    let samples: Vec<f32> = (0..3 * n)
        .map(|i| (0.4 * (TAU * 7.0 * i as f64 / n as f64).sin() + 0.1 * (i % 11) as f64) as f32)
        .collect();
    let mut spectrum = super::dsp::Spectrum::new(n);
    for at in [0, 17, samples.len() - 11, samples.len() + 5, 0] {
        let mag = spectrum.of(&samples, at);
        for (k, actual) in mag.iter().enumerate() {
            let (mut re, mut im) = (0.0, 0.0);
            for i in 0..n {
                let sample = f64::from(samples.get(at + i).copied().unwrap_or(0.0));
                let window = 0.5 - 0.5 * (TAU * i as f64 / n as f64).cos();
                let angle = -TAU * (i * k) as f64 / n as f64;
                re += sample * window * angle.cos();
                im += sample * window * angle.sin();
            }
            let expected = re.hypot(im) * 4.0 / n as f64;
            assert!((actual - expected).abs() < 1e-12, "at {at}, bin {k}: {actual} vs {expected}");
        }
    }
}

#[test]
fn two_stage_recovers_known_decay_and_ignores_the_floor() {
    let dt = 0.02;
    // Two seconds include both the prompt and the tail; a much longer tail can
    // dominate this grid search's fit to the short prompt.
    let levels: Vec<f64> = (0..100)
        .map(|i| {
            let t = f64::from(i) * dt;
            -12.0
                + 10.0
                    * (0.9 * 10f64.powf(-6.0 * t / 0.6) + 0.1 * 10f64.powf(-6.0 * t / 6.0)).log10()
        })
        .collect();
    let fit = measure::two_stage(&levels, dt).expect("a decay");
    assert!((fit.prompt / 0.6 - 1.0).abs() < 0.05, "prompt {}", fit.prompt);
    assert!((fit.after / 6.0 - 1.0).abs() < 0.05, "after {}", fit.after);
    assert!((fit.after_db - 10.0 * (0.1f64 / 0.9).log10()).abs() < 0.5);
    assert!(fit.error < 0.1, "error {}", fit.error);
    assert!(measure::two_stage(&[-120.0; 20], dt).is_none());
}

#[test]
#[ignore = "long: 80 notes made and fitted, about 20 s"]
fn model_fits_a_piano_to_its_notes() {
    // 16 keys at 5 velocities, as the model wants, each B a grand's at its key.
    let dir = scratch("model-notes");
    let keys: Vec<u32> = (0..16).map(|i| 24 + 5 * i).collect();
    let b_of = |key: u32| 1.1e-4 * (0.0693 * (f64::from(key) - 40.0)).exp().max(1.0);
    for key in &keys {
        for v in [24u32, 48, 72, 96, 120] {
            let made = Made { key: *key, b: b_of(*key), x0: 0.12, velocity: f64::from(v) / 127.0 };
            let path = dir.join(format!("{key}-{v}.wav"));
            super::dsp::write_wav(&path, &string(made, 2.0), 1).expect("write");
        }
    }
    let out = scratch("model-out").join("grand.wrela");
    model::run(&dir, &out, None).expect("the fit");
    let source = std::fs::read_to_string(&out).expect("the model");
    let b = model::array(&source, "inharmonicity").expect("inharmonicity");
    let cents = model::array(&source, "tuning").expect("tuning");
    let strike = model::array(&source, "strike").expect("strike");
    assert_eq!((b.len(), cents.len(), strike.len()), (16, 16, 16));
    for (i, key) in keys.iter().enumerate().filter(|(_, k)| (40..=84).contains(*k)) {
        assert!((b[i] / b_of(*key) - 1.0).abs() < 0.25, "key {key}: B {} for {}", b[i], b_of(*key));
        assert!(cents[i].abs() < 3.0, "key {key}: {} cents", cents[i]);
    }
    // Where the hammer strikes, from the partials it leaves quiet, in the keys with many.
    for (i, key) in keys.iter().enumerate().filter(|(_, k)| (40..=60).contains(*k)) {
        assert!((strike[i] - 0.12).abs() < 0.015, "key {key}: struck at {}", strike[i]);
    }
}
