//! AC3: the derived interpretations over the test corpus (compiler/tests/fields/corpus.wrela
//! and the grazer): gradients against central differences, and intervals against sampled
//! values in random boxes, on the CPU (WASM under wasmtime) and on the GPU. Each interval over
//! a single point must also hold the point's value strictly inside: the widening is at least
//! an ulp.
//!
//! The CPU interval test samples 2·10⁴ boxes per function, or AC3's 10⁶ with `WRELA_FULL`
//! (about 90 s of work, spread over the machine's threads). The GPU one needs a GPU and always
//! runs 10⁶: `cargo test -p wrela-tests --test suite derive:: -- --ignored --nocapture`.

use crate::built;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use wrela_host::{CpuHost, Host, Value};
use wrela_tests::{Rng, one_f32, one_u32, par_each, sized, u32s};

/// The corpus, in the order of `with_function` in compiler/tests/fields/main.wrela.
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
/// Boxes per function on the GPU, and on the CPU at full size.
const BOXES: u32 = 1_000_000;

/// The arguments of `value` and `gradient`: function `fi` at `p`.
fn point_args(fi: u32, p: [f32; 3]) -> [Value; 4] {
    [Value::I32(fi as i32), Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2])]
}

fn value(host: &mut CpuHost, fi: u32, p: [f32; 3]) -> f64 {
    f64::from(one_f32(host, "value", &point_args(fi, p)))
}

/// The larger error; NaN if either is (`f64::max` drops a NaN, which would pass every check).
fn worse(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
}

/// A fourth-order central difference along `axis` with step `h` (the step actually taken,
/// after rounding the points to f32).
fn central(host: &mut CpuHost, fi: u32, p: [f32; 3], axis: usize, h: f32) -> f64 {
    let at = |host: &mut CpuHost, k: f32| {
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
    let corpus = built("fields");
    let report = Mutex::new(Vec::new());
    let functions: Vec<usize> = (0..NAMES.len()).collect();
    // A function per thread at a time, each with its own points.
    par_each(
        &functions,
        || CpuHost::load(&corpus).expect("load the corpus"),
        |host, &fi| {
            let name = NAMES[fi];
            let mut rng = Rng::new(0x5eed + fi as u64);
            let fi = fi as u32;
            let (mut checked, mut skipped, mut worst) = (0, 0, 0.0f64);
            for _ in 0..400 {
                let p = [0, 1, 2].map(|_| (rng.unit() * 4.0 - 2.0) as f32);
                let args = point_args(fi, p);
                let g = match host.call_export("gradient", &args).expect("gradient").as_slice() {
                    [Value::F32(x), Value::F32(y), Value::F32(z)] => [*x, *y, *z].map(f64::from),
                    other => panic!("gradient returned {other:?}"),
                };
                // Even where the function isn't smooth, the gradient is a number.
                assert!(g.iter().all(|x| x.is_finite()), "{name}: gradient {g:?} at {p:?}");
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
                        [0, 1, 2].map(|a| central(host, fi, p, a, h))
                    })
                    .collect();
                let tolerance = |v: [f64; 3]| 2.5e-4 * norm(v).max(1e-2);
                let Some(fd) = (2..est.len())
                    .rev()
                    .map(|i| (est[i - 2], est[i - 1], est[i]))
                    .find(|&(a, b, c)| {
                        worse(worse(diff(a, b), diff(b, c)), diff(a, c)) <= tolerance(b)
                    })
                    .map(|(_, b, _)| b)
                else {
                    skipped += 1;
                    continue;
                };
                let scale = norm(fd).max(1e-2);
                checked += 1;
                worst = worse(worst, diff(g, fd) / scale);
            }
            assert!(checked >= 100, "{name}: only {checked} smooth points");
            assert!(worst <= 1e-3, "{name}: gradient relative error {worst:.2e}");
            report.lock().expect("report").push(format!(
                "  {name:>11}: worst relative error {worst:.1e} over {checked} points ({skipped} \
             skipped as not smooth)"
            ));
        },
    );
    let mut report = report.into_inner().expect("report");
    report.sort();
    println!("AC3 gradients against central differences:\n{}", report.join("\n"));
}

/// Boxes per call of `misses_cpu`: the calls of all the functions are shared out between
/// threads, each with its own host.
const CHUNK: u32 = 50_000;

/// A call into the corpus on the CPU.
enum Job {
    /// `unwidened_cpu` of a function.
    Unwidened(usize),
    /// `misses_cpu` of a function, over a chunk of boxes from the first given.
    Misses(usize, u32),
}

#[test]
fn intervals_enclose_every_sample_on_the_cpu() {
    let corpus = built("fields");
    let (boxes, points) = (sized(20_000, BOXES), sized(10_000, 100_000));
    // Each function's point intervals, then the boxes, a chunk at a time.
    let jobs: Vec<Job> = (0..NAMES.len())
        .map(Job::Unwidened)
        .chain((0..NAMES.len()).flat_map(|fi| {
            (0..boxes).step_by(CHUNK as usize).map(move |first| Job::Misses(fi, first))
        }))
        .collect();
    let misses: Vec<AtomicU32> = NAMES.iter().map(|_| AtomicU32::new(0)).collect();
    let unwidened: Vec<AtomicU32> = NAMES.iter().map(|_| AtomicU32::new(0)).collect();
    let started = std::time::Instant::now();
    par_each(
        &jobs,
        || CpuHost::load(&corpus).expect("load the corpus"),
        |host, job| match *job {
            Job::Unwidened(fi) => {
                let args = [fi as i32, 11, points].map(Value::I32);
                unwidened[fi].store(one_u32(host, "unwidened_cpu", &args), Ordering::Relaxed);
            }
            Job::Misses(fi, first) => {
                let count = CHUNK.min(boxes - first);
                let args = [fi as i32, 7, first as i32, count as i32].map(Value::I32);
                misses[fi].fetch_add(one_u32(host, "misses_cpu", &args), Ordering::Relaxed);
            }
        },
    );
    let mut report = Vec::new();
    for (fi, name) in NAMES.iter().enumerate() {
        let (misses, unwidened) =
            (misses[fi].load(Ordering::Relaxed), unwidened[fi].load(Ordering::Relaxed));
        report.push(format!(
            "  {name:>11}: {misses} misses of {} samples in {boxes} boxes, {unwidened} of {points} \
             point intervals not strictly wider",
            16 * boxes,
        ));
        assert_eq!(misses, 0, "{name}: the interval missed sampled values");
        assert_eq!(unwidened, 0, "{name}: an interval over a point wasn't widened");
    }
    println!(
        "AC3 intervals on the CPU ({} threads, {:.1} s):\n{}",
        std::thread::available_parallelism().map_or(4, |n| n.get()),
        started.elapsed().as_secs_f64(),
        report.join("\n")
    );
}

#[test]
#[ignore = "needs a GPU"]
fn intervals_enclose_every_sample_on_the_gpu() {
    let mut host = Host::load(built("fields")).expect("load the corpus");
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
