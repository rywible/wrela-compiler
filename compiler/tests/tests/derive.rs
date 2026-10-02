//! AC3: the derived interpretations over the test corpus (compiler/tests/fields/corpus.wrela
//! and the grazer): gradients against central differences, and intervals against sampled
//! values in 10⁶ random boxes per function, on the CPU (WASM under wasmtime) and on the GPU.
//! Each interval over a single point must also hold the point's value strictly inside: the
//! widening is at least an ulp.
//!
//! The CPU interval test takes about 90 s (the grazer, 40 of it). The GPU one needs a GPU:
//! `cargo test -p wrela-tests --test derive -- --ignored --nocapture`.

use std::path::PathBuf;
use wrela_host::{Host, Options, Value};
use wrela_tests::{build, root, u32s};

const NAMES: [&str; 17] = [
    "sphere",
    "ellipsoid",
    "round cone",
    "hoof",
    "blob",
    "value noise",
    "fbm",
    "creature",
    "grazer",
    "waves",
    "logs",
    "trig",
    "rational",
    "pieces",
    "vectors",
    "branchy",
    "cells",
];
const BOXES: u32 = 1_000_000;

fn load() -> Host {
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fields-derive");
    if let Err(e) = build(&root().join("compiler/tests/fields"), &out) {
        panic!("the corpus doesn't build:\n{e}");
    }
    Host::load_with(&out, &Options::default()).expect("load the corpus")
}

fn u32_of(v: &[Value]) -> u32 {
    match v {
        [Value::I32(x)] => *x as u32,
        other => panic!("expected a u32, got {other:?}"),
    }
}

/// SplitMix64, for the gradient test's points.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn value(host: &mut Host, fi: u32, p: [f32; 3]) -> f64 {
    let args = [Value::I32(fi as i32), Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2])];
    match host.call_export("value", &args).expect("value").as_slice() {
        [Value::F32(x)] => f64::from(*x),
        other => panic!("value returned {other:?}"),
    }
}

/// A fourth-order central difference along `axis` with step `h` (the step actually taken,
/// after rounding the points to f32).
fn central(host: &mut Host, fi: u32, p: [f32; 3], axis: usize, h: f32) -> f64 {
    let at = |host: &mut Host, k: f32| {
        let mut q = p;
        q[axis] = p[axis] + k * h;
        (value(host, fi, q), f64::from(q[axis]) - f64::from(p[axis]))
    };
    let (a, ha) = at(host, 1.0);
    let (b, hb) = at(host, -1.0);
    let (c, hc) = at(host, 2.0);
    let (d, hd) = at(host, -2.0);
    // Richardson: (8 (f(h) - f(-h)) - (f(2h) - f(-2h))) / 12h, with the steps as taken.
    let first = (a - b) / (ha - hb);
    let second = (c - d) / (hc - hd);
    (4.0 * first - second) / 3.0
}

#[test]
fn gradients_agree_with_central_differences() {
    let mut host = load();
    let mut rng = Rng(0x5eed);
    let mut report = Vec::new();
    for (fi, name) in NAMES.iter().enumerate() {
        let fi = fi as u32;
        let (mut checked, mut skipped, mut worst) = (0, 0, 0.0f64);
        for _ in 0..400 {
            let p = [0, 1, 2].map(|_| (rng.unit() * 4.0 - 2.0) as f32);
            let args =
                [Value::I32(fi as i32), Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2])];
            let g = match host.call_export("gradient", &args).expect("gradient").as_slice() {
                [Value::F32(x), Value::F32(y), Value::F32(z)] => [*x, *y, *z].map(f64::from),
                other => panic!("gradient returned {other:?}"),
            };
            // Richardson estimates at halving steps. The reference is the middle of the finest
            // three consecutive steps that agree: larger steps can straddle a kink or a noise
            // cell (a pair of them can agree while both are off), smaller ones drown in f32
            // rounding. Where no three agree, the function isn't smooth at this scale (a kink,
            // a step, a branch's seam) and the point is skipped.
            let norm = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            let diff = |a: [f64; 3], b: [f64; 3]| norm([a[0] - b[0], a[1] - b[1], a[2] - b[2]]);
            let est: Vec<[f64; 3]> = (0..6)
                .map(|i| {
                    let h = 1.6e-2 / f32::powi(2.0, i);
                    [0, 1, 2].map(|a| central(&mut host, fi, p, a, h))
                })
                .collect();
            let tolerance = |v: [f64; 3]| 2.5e-4 * norm(v).max(1e-2);
            let Some(fd) = (2..est.len())
                .rev()
                .map(|i| (est[i - 2], est[i - 1], est[i]))
                .find(|&(a, b, c)| diff(a, b).max(diff(b, c)).max(diff(a, c)) <= tolerance(b))
                .map(|(_, b, _)| b)
            else {
                skipped += 1;
                continue;
            };
            let scale = norm(fd).max(1e-2);
            checked += 1;
            worst = worst.max(diff(g, fd) / scale);
        }
        report.push(format!(
            "  {name:>11}: worst relative error {worst:.1e} over {checked} points ({skipped} \
             skipped as not smooth)"
        ));
        assert!(checked >= 100, "{name}: only {checked} smooth points");
        assert!(worst <= 1e-3, "{name}: gradient relative error {worst:.2e}");
    }
    println!("AC3 gradients against central differences:\n{}", report.join("\n"));
}

#[test]
fn intervals_enclose_every_sample_on_the_cpu() {
    let mut host = load();
    let mut report = Vec::new();
    for (fi, name) in NAMES.iter().enumerate() {
        let started = std::time::Instant::now();
        let args = [Value::I32(fi as i32), Value::I32(7), Value::I32(BOXES as i32)];
        let misses = u32_of(&host.call_export("misses_cpu", &args).expect("misses_cpu"));
        let args = [Value::I32(fi as i32), Value::I32(11), Value::I32(100_000)];
        let unwidened = u32_of(&host.call_export("unwidened_cpu", &args).expect("unwidened_cpu"));
        report.push(format!(
            "  {name:>11}: {misses} misses of {} samples in {BOXES} boxes, {unwidened} of 10⁵ \
             point intervals not strictly wider ({:.1} s)",
            16 * BOXES,
            started.elapsed().as_secs_f64()
        ));
        assert_eq!(misses, 0, "{name}: the interval missed sampled values");
        assert_eq!(unwidened, 0, "{name}: an interval over a point wasn't widened");
    }
    println!("AC3 intervals on the CPU:\n{}", report.join("\n"));
}

#[test]
#[ignore = "needs a GPU"]
fn intervals_enclose_every_sample_on_the_gpu() {
    let mut host = load();
    let mut report = Vec::new();
    for (fi, name) in NAMES.iter().enumerate() {
        let args = [Value::I32(fi as i32), Value::I32(13), Value::I32(BOXES as i32)];
        host.call_export("check_gpu", &args).expect("check_gpu");
        let out = u32s(&host.read_buffer(fi as u32).expect("results"));
        let misses: u64 = out.iter().map(|&x| u64::from(x % 256)).sum();
        let unwidened: u64 = out.iter().map(|&x| u64::from(x / 256)).sum();
        report.push(format!(
            "  {name:>11}: {misses} misses of {} samples in {BOXES} boxes, {unwidened} of {BOXES} \
             point intervals not strictly wider",
            16 * BOXES
        ));
        assert_eq!(misses, 0, "{name}: the interval missed sampled values on the GPU");
        assert_eq!(unwidened, 0, "{name}: a GPU interval over a point wasn't widened");
    }
    println!("AC3 intervals on the GPU:\n{}", report.join("\n"));
}
