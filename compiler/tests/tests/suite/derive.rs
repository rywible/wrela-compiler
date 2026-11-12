//! AC3: the derived interpretations over the test corpus (compiler/tests/fields/corpus.wrela
//! and the grazer): gradients against central differences, and intervals against sampled
//! values in random boxes, on the CPU (WASM under wasmtime) and on the GPU. Each interval over
//! a single point must also hold the point's value strictly inside: the widening is at least
//! an ulp.
//!
//! Fixed CPU and GPU samples cover every function, including strictly widened point intervals.

use crate::built;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use wrela_host::{CpuBuild, CpuHost, Host, Value};
use wrela_tests::{Rng, one_f32, one_u32, par_each, par_map_with, u32s, worse};

/// The corpus, in the order of `with_function` in compiler/tests/fields/main.wrela.
pub(crate) const NAMES: [&str; 25] = [
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
    "sphere (tape)",
    "waves (tape)",
    "blob (tape)",
    "closures",
    "table",
    "hyperbolic",
    "powers",
    "march",
];
/// Bounded boxes per function on the GPU (a whole number of 64-thread workgroups).
const BOXES: u32 = 1024;

/// The arguments of `value` and `gradient`: function `fi` at `p`.
fn point_args(fi: u32, p: [f32; 3]) -> [Value; 4] {
    [Value::I32(fi as i32), Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2])]
}

fn value(host: &mut CpuHost, fi: u32, p: [f32; 3]) -> f64 {
    f64::from(one_f32(host, "value", &point_args(fi, p)))
}

/// Fourth-order central differences along `axis` with the steps h = 1.6e-2 / 2^i for i < 6
/// (the steps actually taken, after rounding the points to f32). A step's 2h is the step
/// before's h, so each point is evaluated once.
fn centrals(host: &mut CpuHost, fi: u32, p: [f32; 3], axis: usize) -> [f64; 6] {
    let mut at = |s: f32| {
        let mut q = p;
        q[axis] = p[axis] + s;
        (value(host, fi, q), f64::from(q[axis]) - f64::from(p[axis]))
    };
    // The slope from p - s to p + s for s = 2h₀, h₀, h₁ ... h₅, with the steps as taken.
    let slopes: Vec<f64> = (-1..6)
        .map(|i| {
            let s = 1.6e-2 / f32::powi(2.0, i);
            let ((a, ha), (b, hb)) = (at(s), at(-s));
            (a - b) / (ha - hb)
        })
        .collect();
    // Richardson: (8 (f(h) - f(-h)) - (f(2h) - f(-2h))) / 12h.
    std::array::from_fn(|i| (4.0 * slopes[i + 1] - slopes[i]) / 3.0)
}

#[test]
fn gradients_agree_with_central_differences() {
    let corpus = CpuBuild::load(built("fields")).expect("load the corpus");
    let report = Mutex::new(Vec::new());
    let functions: Vec<usize> = (0..NAMES.len()).collect();
    // A function per thread at a time, each with its own points.
    par_each(
        &functions,
        || corpus.start().expect("start the corpus"),
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
                let axes = [0, 1, 2].map(|a| centrals(host, fi, p, a));
                let est: Vec<[f64; 3]> = (0..6).map(|i| axes.map(|c| c[i])).collect();
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
            // What language.md §14 says they agree within.
            assert!(worst <= 3.4e-4, "{name}: gradient relative error {worst:.2e}");
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

/// Reverse mode against forward mode (stage C's gate): each corpus function's gradient by
/// `std::derive::reverse`, through a closure of the point's three numbers, is within
/// `1e-5 (1 + |g|)` of `gradient`'s at 1,000 points in [-2, 2]³, `|g|` the gradient's size (as
/// the corpus's gradient test scales its error). Taken a component at a time instead, `march`
/// misses by 3.5e-5 where its gradient is 17,000 across and its z component 2.87, the sum of
/// terms of 10⁴ that cancel: there, against f64, reverse mode's z is off by 2e-5 and forward
/// mode's by 1.2e-4, so it's the order of the sum, not either derivation.
#[test]
fn reverse_gradients_agree_with_forward_ones() {
    let corpus = CpuBuild::load(built("fields")).expect("load the corpus");
    let functions: Vec<usize> = (0..NAMES.len()).collect();
    let worst_of = par_map_with(
        &functions,
        || corpus.start().expect("start the corpus"),
        |host, &fi| {
            let mut rng = Rng::new(0xbac + fi as u64);
            let mut worst = 0.0f64;
            let mut at_worst = ([0.0f32; 3], [0.0f64; 3], [0.0f64; 3]);
            for _ in 0..1000 {
                let p = [0, 1, 2].map(|_| (rng.unit() * 4.0 - 2.0) as f32);
                let args = point_args(fi as u32, p);
                let vec = |host: &mut CpuHost, name: &str| -> [f64; 3] {
                    match host.call_export(name, &args).expect(name).as_slice() {
                        [Value::F32(x), Value::F32(y), Value::F32(z)] => {
                            [*x, *y, *z].map(f64::from)
                        }
                        other => panic!("{name} returned {other:?}"),
                    }
                };
                let (f, r) = (vec(host, "gradient"), vec(host, "reverse_gradient"));
                let norm = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                let d = norm([f[0] - r[0], f[1] - r[1], f[2] - r[2]]) / (1.0 + norm(f));
                if d.is_nan() || d > worst {
                    worst = d;
                    at_worst = (p, f, r);
                }
            }
            if worst.is_nan() || worst > 1e-5 {
                println!(
                    "{}: at {:?}, forward {:?}, reverse {:?}",
                    NAMES[fi], at_worst.0, at_worst.1, at_worst.2
                );
            }
            worst
        },
    );
    let mut report: Vec<String> =
        NAMES.iter().zip(&worst_of).map(|(n, w)| format!("  {n:>13}: worst {w:.1e}")).collect();
    report.sort();
    println!("reverse against forward gradients, 1,000 points each:\n{}", report.join("\n"));
    let over: Vec<_> =
        NAMES.iter().zip(&worst_of).filter(|(_, w)| w.is_nan() || **w > 1e-5).collect();
    assert!(over.is_empty(), "over 1e-5: {over:?}");
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
    let corpus = CpuBuild::load(built("fields")).expect("load the corpus");
    let (boxes, points) = (512, 256);
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
        || corpus.start().expect("start the corpus"),
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
        let out = *host.buffers().last().expect("a buffer");
        let out = u32s(&host.read_buffer(out).expect("results"));
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

/// AC12: on the GPU, the box of a vector's interval (the creature's gradient) is the three
/// components' intervals, over 1024 fixed seeded boxes. (The CPU's: run/interval_vectors.)
#[test]
#[ignore = "needs a GPU"]
fn the_interval_of_a_vector_is_its_components_on_the_gpu() {
    let mut host = Host::load(built("fields")).expect("load");
    let n = 1024;
    host.call_export("check_vector_intervals", &[Value::I32(5), Value::I32(n)]).expect("check");
    let out = *host.buffers().last().expect("a buffer");
    let same = u32s(&host.read_buffer(out).expect("read"));
    assert_eq!(same.iter().filter(|&&s| s == 1).count(), n as usize);
}

/// The tightness ratchet: on the enclosure test's boxes, each corpus function's interval is no
/// looser than recorded in compiler/tests/fields/tightness.json. `ratio` is the median, over
/// the boxes, of the interval's width over the spread of the 16 sampled values (1 is as tight
/// as sampling shows), and `decided` the share of boxes whose interval excludes 0. A ratio that
/// grows by more than 5%, or a share that falls by more than 0.01, fails, as does a record of
/// other functions than the corpus's; `WRELA_BLESS=1` records the numbers as they are (after a
/// change that makes them better, say so).
#[test]
fn intervals_stay_as_tight_as_recorded() {
    let corpus = CpuBuild::load(built("fields")).expect("load the corpus");
    let boxes = 512;
    let path = wrela_tests::repo_root().join("compiler/tests/fields/tightness.json");
    let fis: Vec<usize> = (0..NAMES.len()).collect();
    // Each function's median ratio and share decided, in `NAMES`' order.
    let measured = par_map_with(
        &fis,
        || corpus.start().expect("start the corpus"),
        |host, &fi| {
            let mut ratios = Vec::with_capacity(boxes);
            let mut decided = 0.0;
            for i in 0..boxes {
                let args = [Value::I32(fi as i32), Value::I32(7), Value::I32(i as i32)];
                let out = host.call_export("tightness_cpu", &args).expect("tightness_cpu");
                let (Value::F32(r), Value::F32(d)) = (out[0], out[1]) else { panic!("{out:?}") };
                ratios.push(if r.is_nan() { f64::INFINITY } else { f64::from(r) });
                decided += f64::from(d);
            }
            ratios.sort_by(f64::total_cmp);
            (ratios[boxes / 2], decided / boxes as f64)
        },
    );
    let now: serde_json::Map<String, serde_json::Value> = NAMES
        .iter()
        .zip(measured)
        .map(|(name, (ratio, decided))| {
            let round = |x: f64| if x.is_finite() { (x * 1e4).round() / 1e4 } else { 1e30 };
            (
                name.to_string(),
                serde_json::json!({ "ratio": round(ratio), "decided": round(decided) }),
            )
        })
        .collect();
    let report: Vec<String> = now
        .iter()
        .map(|(n, v)| format!("  {n:>13}: ratio {}, decided {}", v["ratio"], v["decided"]))
        .collect();
    println!("interval tightness on {boxes} boxes:\n{}", report.join("\n"));
    if std::env::var_os("WRELA_BLESS").is_some() {
        let text = serde_json::to_string_pretty(&serde_json::Value::Object(now)).unwrap() + "\n";
        std::fs::write(&path, text).expect("bless tightness.json");
        return;
    }
    let recorded: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("tightness.json; bless it"))
            .expect("JSON");
    // Exactly the corpus's functions are recorded: one renamed or removed isn't left behind.
    let names: std::collections::BTreeSet<&str> = NAMES.iter().copied().collect();
    let kept: std::collections::BTreeSet<&str> =
        recorded.as_object().expect("an object").keys().map(String::as_str).collect();
    assert_eq!(kept, names, "tightness.json records other functions; bless with WRELA_BLESS=1");
    let mut worse_than = Vec::new();
    for (name, v) in &now {
        let was = &recorded[name];
        let (r, d) = (v["ratio"].as_f64().unwrap(), v["decided"].as_f64().unwrap());
        let (r0, d0) = (was["ratio"].as_f64().unwrap(), was["decided"].as_f64().unwrap());
        if r > r0 * 1.05 + 1e-9 {
            worse_than.push(format!("{name}: ratio {r}, recorded {r0}"));
        }
        if d < d0 - 0.01 {
            worse_than.push(format!("{name}: decided {d}, recorded {d0}"));
        }
    }
    assert!(
        worse_than.is_empty(),
        "intervals are looser than recorded:\n{}",
        worse_than.join("\n")
    );
}

/// A loop that leaves early on the input runs its count with flags (§14.2), and the flags carry
/// the input into what the loop writes: the count it returns is a range, 0 where `x > 0` leaves
/// at once to 4 where it never does, called directly or through another function.
#[test]
fn what_a_loop_counts_before_an_early_exit_is_a_range() {
    let src = "use std::derive::{Interval, interval}

fn count(x: f32) -> f32 {
    var n = 0.0
    for _ in 0..4 {
        if x > 0.0 {
            break
        }
        n += 1.0
    }
    n
}

pub fn direct(lo: f32, hi: f32) -> vec2 {
    let r = interval(count, Interval { lo, hi })
    vec2(r.lo, r.hi)
}

pub fn called(lo: f32, hi: f32) -> vec2 {
    let r = interval(|x: f32| count(x) * 2.0, Interval { lo, hi })
    vec2(r.lo, r.hi)
}
";
    let dir = crate::package("derive-early-exit", src);
    wrela_tests::must_build(&dir, &dir.join("build"));
    let mut host = CpuBuild::load(dir.join("build")).expect("load").start().expect("start");
    let mut range = |name: &str, lo: f32, hi: f32| match host
        .call_export(name, &[Value::F32(lo), Value::F32(hi)])
        .expect(name)[..]
    {
        [Value::F32(lo), Value::F32(hi)] => (f64::from(lo), f64::from(hi)),
        ref other => panic!("{name} returned {other:?}"),
    };
    // Each bound within a hundredth of the counts it holds.
    let near = |(lo, hi): (f64, f64), (a, b): (f64, f64)| {
        lo <= a && hi >= b && lo > a - 0.01 && hi < b + 0.01
    };
    for (name, k) in [("direct", 1.0), ("called", 2.0)] {
        let (both, leaves, stays) =
            (range(name, -1.0, 1.0), range(name, 0.5, 1.0), range(name, -1.0, -0.5));
        assert!(near(both, (0.0, 4.0 * k)), "{name} over [-1, 1]: {both:?}");
        assert!(near(leaves, (0.0, 0.0)), "{name} over [0.5, 1]: {leaves:?}");
        assert!(near(stays, (4.0 * k, 4.0 * k)), "{name} over [-1, -0.5]: {stays:?}");
    }
}

/// What every heap case below starts with: a gradient taken through a parameter, so the
/// function it derives is known only when it's built.
const SLOPE: &str = "use std::collections::{Box, Vec}
use std::derive::gradient

fn slope(f: fn(f32) -> f32, x: f32) -> f32 {
    gradient(f, x)
}

struct Rows {
    cells: Vec<f32>,
}

impl Rows {
    fn cell(mut self, i: u32) -> mut f32 {
        mut self.cells[i]
    }
}

fn fill(xs: mut [f32], x: f32) {
    for i in 0..xs.len() {
        xs[i] = x
    }
}

fn set(slot: mut f32, x: f32) {
    slot = x
}
";

/// A value that depends on a gradient's input put in heap memory is E0700 at the line that
/// puts it there (§14.2), however it gets there: a `Vec`'s `push`, an element assigned, changed
/// in place, named or looped over, a `Box`'s value, a projection of the program's own, an
/// element or the elements lent to a function that writes them. The function that stores isn't
/// derived (a `Vec` it returns carries no derivative, as far as its code shows), nor the one
/// that reads the value back, so each was run as it was and the gradient was 0, with no error.
#[test]
fn a_value_that_depends_on_the_input_put_in_heap_memory_is_an_error() {
    heap_errors(&heap_programs(), "derive-heap");
}

/// The programs of the heap test: each case's name and text, a gradient taken through `slope`.
fn heap_programs() -> Vec<(String, String)> {
    // Each: what keeps `x` (its line marked `// here`), and what reads it back.
    let cases = [
        ("push", "    var v: Vec<f32> = Vec::new()\n    v.push(x) // here\n    v", "v[0]"),
        (
            "element",
            "    var v: Vec<f32> = Vec::filled(2, value: 0.0)\n    v[1] = x // here\n    v",
            "v[1]",
        ),
        (
            "in-place",
            "    var v: Vec<f32> = Vec::filled(2, value: 1.0)\n    v[1] *= x // here\n    v",
            "v[1]",
        ),
        (
            "named",
            "    var v: Vec<f32> = Vec::filled(2, value: 0.0)\n    mut slot = v[0]\n    slot = x // here\n    v",
            "v[0]",
        ),
        (
            "looped",
            "    var v: Vec<f32> = Vec::filled(2, value: 0.0)\n    for mut r in v {\n        r = x // here\n    }\n    v",
            "v[0]",
        ),
        (
            "lent-element",
            "    var v: Vec<f32> = Vec::filled(2, value: 0.0)\n    set(mut v[1], x) // here\n    v",
            "v[1]",
        ),
        (
            "lent-elements",
            "    var v: Vec<f32> = Vec::filled(2, value: 0.0)\n    fill(mut v, x) // here\n    v",
            "v[1]",
        ),
    ];
    let mut programs: Vec<(String, String)> = cases
        .iter()
        .map(|(name, keep, read)| {
            let text = format!(
                "{SLOPE}\nfn kept(x: f32) -> Vec<f32> {{\n{keep}\n}}\n\nfn read(x: f32) -> f32 {{\n    let v = kept(x)\n    {read}\n}}\n\nfn curve(x: f32) -> f32 {{\n    read(x) - 3.0\n}}\n\npub fn frame(time: f32, width: u32, height: u32) {{\n    let s = slope(curve, time)\n}}\n"
            );
            (name.to_string(), text)
        })
        .collect();
    // A `Box`'s value, and a projection of the program's own.
    programs.push((
        "boxed".into(),
        format!(
            "{SLOPE}\nfn kept(x: f32) -> Box<f32> {{\n    var b = Box::new(0.0)\n    b.get_mut() = x // here\n    b\n}}\n\nfn read(x: f32) -> f32 {{\n    let b = kept(x)\n    b.get()\n}}\n\nfn curve(x: f32) -> f32 {{\n    read(x) - 3.0\n}}\n\npub fn frame(time: f32, width: u32, height: u32) {{\n    let s = slope(curve, time)\n}}\n"
        ),
    ));
    programs.push((
        "projected".into(),
        format!(
            "{SLOPE}\nfn kept(x: f32) -> Rows {{\n    var r = Rows {{ cells: Vec::filled(2, value: 0.0) }}\n    r.cell(1) = x // here\n    r\n}}\n\nfn read(x: f32) -> f32 {{\n    let r = kept(x)\n    r.cells[1]\n}}\n\nfn curve(x: f32) -> f32 {{\n    read(x) - 3.0\n}}\n\npub fn frame(time: f32, width: u32, height: u32) {{\n    let s = slope(curve, time)\n}}\n"
        ),
    ));
    programs
}

/// Checks each heap program is E0700 at its marked line, with the heap rule's words.
fn heap_errors(programs: &[(String, String)], stem: &str) {
    for (name, text) in programs {
        let dir = crate::package(&format!("{stem}-{name}"), text);
        let (codes, rendered) = crate::checked(&dir);
        let errors: Vec<&String> = codes.iter().filter(|c| c.starts_with('E')).collect();
        assert_eq!(errors, ["E0700"], "{name}:\n{rendered}");
        assert!(
            rendered.contains("puts a value that depends on the input in heap memory"),
            "{name}:\n{rendered}"
        );
        let line = 1 + text.lines().position(|l| l.ends_with("// here")).expect("the marked line");
        assert!(
            rendered.contains(&format!("main.wrela:{line}:")),
            "{name}, line {line}:\n{rendered}"
        );
        assert!(
            rendered.contains("keep what depends on the input in arrays and values"),
            "{name}:\n{rendered}"
        );
    }
}

/// The same cases with `reverse` in place of `gradient` (§14.2): a recorded function is held to
/// the heap rule as a forward one is, with the same words at the same lines.
#[test]
fn a_value_put_in_heap_memory_is_an_error_under_reverse_too() {
    let programs: Vec<(String, String)> =
        heap_programs().into_iter().map(|(name, text)| (name, as_reverse(&text))).collect();
    heap_errors(&programs, "derive-heap-reverse");
}

/// A heap program with its gradient taken by `reverse`, on the same lines.
fn as_reverse(text: &str) -> String {
    let out = text
        .replace("use std::derive::gradient", "use std::derive::reverse")
        .replace(
            "fn slope(f: fn(f32) -> f32, x: f32) -> f32 {\n    gradient(f, x)\n}",
            "fn slope(f: fn([f32; 1]) -> f32, x: f32) -> f32 {\n    reverse(f, [x]).1[0]\n}",
        )
        .replace(
            "fn curve(x: f32) -> f32 {\n    read(x) - 3.0\n}",
            "fn curve(xs: [f32; 1]) -> f32 {\n    read(xs[0]) - 3.0\n}",
        );
    assert_eq!(out.lines().count(), text.lines().count(), "the lines stay where they were");
    assert!(out.contains("reverse(f, [x])") && out.contains("read(xs[0])"), "{out}");
    out
}

/// Weights made in a `Vec` by a function given nothing of the input, handed back as an array,
/// leave a reverse gradient right; and `reverse` named inside a forward derivation is refused,
/// since it allocates its trace (§8.3).
#[test]
fn heap_memory_that_holds_nothing_of_the_input_is_left_alone_under_reverse() {
    let src = "use std::collections::Vec
use std::derive::{gradient, reverse}

fn slope(f: fn([f32; 2]) -> f32, x: [f32; 2]) -> [f32; 2] {
    reverse(f, x).1
}

/// Weights made in a `Vec`, given back as values.
fn weights() -> [f32; 2] {
    var v: Vec<f32> = Vec::new()
    v.push(0.5)
    v.push(4.0)
    [v[0], v[1]]
}

fn curve(x: [f32; 2]) -> f32 {
    let w = weights()
    x[0] * w[0] + x[1] * x[1] * w[1]
}

@test
fn the_slope_is_the_weights() {
    let g = slope(curve, [2.5, 3.0])
    assert(abs(g[0] - 0.5) < 1.0e-6)
    assert(abs(g[1] - 24.0) < 1.0e-5)
}
";
    let dir = crate::package("derive-heap-reverse-constants", src);
    assert_eq!(crate::tests_pass(&dir), 1);
    let nested = "use std::derive::{gradient, reverse}

fn inner(x: f32) -> f32 {
    reverse(|y: [f32; 1]| y[0] * y[0] * x, [x]).1[0]
}

pub fn frame(time: f32, width: u32, height: u32) {
    let _s = gradient(inner, time)
}
";
    let dir = crate::package("derive-reverse-in-gradient", nested);
    let (codes, rendered) = crate::checked(&dir);
    let errors: Vec<&String> = codes.iter().filter(|c| c.starts_with('E')).collect();
    assert_eq!(errors, ["E0700"], "{rendered}");
    assert!(rendered.contains("`reverse` allocates"), "{rendered}");
}

/// Heap memory that holds nothing of the input stays as it was (§14.2): a function given
/// nothing that depends on the input runs as it is, a `Vec` and all, and so does one given the
/// input whose result carries no derivative; what they give is a constant to the gradient.
#[test]
fn heap_memory_that_holds_nothing_of_the_input_is_left_alone() {
    let src = "use std::collections::Vec
use std::derive::gradient

fn slope(f: fn(f32) -> f32, x: f32) -> f32 {
    gradient(f, x)
}

/// Weights made in a `Vec`, given back as values.
fn weights() -> [f32; 2] {
    var v: Vec<f32> = Vec::new()
    v.push(0.5)
    v.push(4.0)
    [v[0], v[1]]
}

/// How many of a table's entries are below `x`: a count, which carries no derivative.
fn below(x: f32) -> u32 {
    let table: Vec<f32> = Vec::from([1.0, 2.0, 3.0])
    var n = 0
    for t in table {
        if t < x {
            n += 1
        }
    }
    n
}

fn curve(x: f32) -> f32 {
    let w = weights()
    x * w[0] + w[1] + f32(below(x))
}

@test
fn the_slope_is_the_weight() {
    assert(abs(slope(curve, 2.5) - 0.5) < 1.0e-6)
    assert(curve(2.5) == 1.25 + 4.0 + 2.0)
}
";
    let dir = crate::package("derive-heap-constants", src);
    assert_eq!(crate::tests_pass(&dir), 1);
}

/// A two-layer net over 4,096 weights: 64 inputs, 60 hidden `tanh`s, one output (W1 at 0, b1 at
/// 3840, W2 at 3900, b2 at 3960; the rest unused).
const NET: &str = "use std::derive::reverse

fn input(i: u32) -> f32 {
    sin(f32(i) * 0.37)
}

fn net(w: [f32; 4096]) -> f32 {
    var out = w[3960]
    for j in 0..60 {
        var s = w[3840 + j]
        for i in 0..64 {
            s += w[j * 64 + i] * input(i)
        }
        out += w[3900 + j] * tanh(s)
    }
    out
}

fn weights() -> [f32; 4096] {
    var w = [0.0; 4096]
    for k in 0..4096 {
        w[k] = 0.2 * sin(f32(k) * 1.618)
    }
    w
}

pub fn plain() -> f32 {
    net(weights())
}

pub fn reversed(k: u32) -> f32 {
    let (_, g) = reverse(net, weights())
    g[k]
}

pub fn nudged(k: u32, h: f32) -> f32 {
    var w = weights()
    w[k] += h
    net(w)
}
";

/// Reverse mode at scale: a function of 4,096 numbers. Its gradient agrees with central
/// differences on 16 of them within 1e-3 relative, and the reverse call costs less than ten
/// plain evaluations, in fuel (work, not time).
#[test]
fn reverse_is_a_few_evaluations() {
    let dir = crate::package("derive-reverse-net", NET);
    wrela_tests::must_build(&dir, &dir.join("build"));
    let fuel = 1u64 << 40;
    let mut host = CpuBuild::load_metered(dir.join("build"), fuel)
        .expect("load")
        .start_with(1)
        .expect("start");
    let spent = |host: &mut CpuHost, name: &str, args: &[Value]| -> (f32, u64) {
        let v = one_f32(host, name, args);
        (v, fuel - host.fuel_left().expect("metered"))
    };
    let (_, plain) = spent(&mut host, "plain", &[]);
    let (_, reversed) = spent(&mut host, "reversed", &[Value::I32(0)]);
    let ratio = reversed as f64 / plain as f64;
    let mut worst = 0.0f64;
    for k in (0..3961i32).step_by(3961 / 16).take(16) {
        let g = f64::from(one_f32(&mut host, "reversed", &[Value::I32(k)]));
        let h = 1.0e-2f32;
        let up = f64::from(one_f32(&mut host, "nudged", &[Value::I32(k), Value::F32(h)]));
        let down = f64::from(one_f32(&mut host, "nudged", &[Value::I32(k), Value::F32(-h)]));
        let central = (up - down) / (2.0 * f64::from(h));
        let err = (g - central).abs() / central.abs().max(1e-2);
        worst = worse(worst, err);
        assert!(err <= 1e-3, "weight {k}: reverse {g}, central differences {central}");
    }
    println!(
        "a net of 4,096 weights: a plain evaluation {plain} fuel, the reverse gradient {reversed} \
         ({ratio:.2} times); worst relative difference from central differences {worst:.1e}"
    );
    assert!(ratio < 10.0, "the reverse gradient costs {ratio:.2} evaluations");
}

/// A trace past its most nodes panics, saying what to do (`Trace::holding` sets a small most).
#[test]
fn a_trace_past_its_most_panics() {
    let src = "use std::derive::Trace

@test
fn past() {
    var t = Trace::holding(4)
    for _ in 0..4 {
        let _ = t.leaf()
    }
}
";
    let dir = crate::package("derive-trace-most", src);
    let out = wrela_driver::test(&dir, None);
    let failure = out.results.iter().find_map(|r| r.failure.clone()).expect("the test fails");
    let rendered = wrela_diag::render::render_all(&out.sources, &[failure]);
    assert!(
        rendered.contains("a reverse gradient's trace passed 2^26 numbers"),
        "{rendered}"
    );
}
