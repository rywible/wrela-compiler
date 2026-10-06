//! The herd (examples/herd) against spike 01 (#42 AC1–AC7). The fixtures are the spike's files,
//! unchanged (compiler/tests/fixtures/spike01): its CPU module (`cpu.wasm`), its kernels and its
//! seed-1 parameters.
//!
//! CPU comparisons run both sides in one harness, the native host's wasmtime, alternating after a
//! warm-up round (M1's method): the spike's `cpu.wasm` and the herd's build.

use crate::built;
use std::path::PathBuf;
use std::time::Instant;
use wrela_host::{CpuBuild, CpuHost, Value};
use wrela_tests::{median, repo_root};

fn herd() -> PathBuf {
    built("../../examples/herd")
}

fn fixture(name: &str) -> PathBuf {
    repo_root().join("compiler/tests/fixtures/spike01").join(name)
}

/// Seed 1's parameters: the spike's `Grazer` uniform, 328 f32s.
fn params_seed1() -> Vec<f32> {
    let bytes = std::fs::read(fixture("params-seed1.f32")).expect("params-seed1.f32");
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

/// The spike's CPU module, loaded with seed 1's parameters.
struct Spike {
    store: wasmtime::Store<()>,
    instance: wasmtime::Instance,
}

impl Spike {
    fn new() -> Spike {
        let mut config = wasmtime::Config::new();
        config.wasm_threads(true).shared_memory(true);
        let engine = wasmtime::Engine::new(&config).expect("an engine");
        let module = wasmtime::Module::from_file(&engine, fixture("cpu.wasm")).expect("cpu.wasm");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("instantiate");
        let mut s = Spike { store, instance };
        let ptr = s.call_u32("params_ptr", &[]) as usize;
        let memory = s.instance.get_memory(&mut s.store, "memory").expect("memory");
        for (i, x) in params_seed1().iter().enumerate() {
            memory.data_mut(&mut s.store)[ptr + 4 * i..ptr + 4 * i + 4]
                .copy_from_slice(&x.to_le_bytes());
        }
        let load = s.instance.get_typed_func::<(), ()>(&mut s.store, "load").expect("load");
        load.call(&mut s.store, ()).expect("load()");
        s
    }

    fn call_u32(&mut self, name: &str, _args: &[u32]) -> u32 {
        let f = self.instance.get_typed_func::<(), u32>(&mut self.store, name).expect(name);
        f.call(&mut self.store, ()).expect(name)
    }

    fn bench_eval(&mut self, n: u32, pruned: u32) -> f64 {
        let f = self
            .instance
            .get_typed_func::<(u32, u32), f64>(&mut self.store, "bench_eval_js")
            .expect("bench_eval_js");
        f.call(&mut self.store, (n, pruned)).expect("bench_eval_js")
    }

    fn mass(&mut self, finest: f32) -> f64 {
        let f =
            self.instance.get_typed_func::<f32, f64>(&mut self.store, "mass_js").expect("mass_js");
        f.call(&mut self.store, finest).expect("mass_js")
    }

    fn raycasts(&mut self, n: u32) -> f64 {
        let f = self
            .instance
            .get_typed_func::<u32, f64>(&mut self.store, "raycast_bench_js")
            .expect("raycast_bench_js");
        f.call(&mut self.store, n).expect("raycast_bench_js")
    }
}

/// The spike's sampling box for seed 1 (its `Grazer::new`): the union of its parts' bounding
/// spheres.
fn spike_box() -> ([f32; 3], [f32; 3]) {
    let p = params_seed1();
    let v = |i: usize, k: usize| {
        let o = 8 + i * 16 + k * 4;
        [p[o], p[o + 1], p[o + 2], p[o + 3]]
    };
    let sub = |a: [f32; 4], b: [f32; 4]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let len = |a: [f32; 3]| (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    let mid =
        |a: [f32; 4], b: [f32; 4]| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, (a[2] + b[2]) * 0.5];
    let amp = p[1] * 1.875;
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for i in 0..20 {
        let (c, r): ([f32; 3], f32) = match i {
            0 => {
                let (c1, r1, c2, r2) = (v(0, 0), v(0, 1), v(0, 2), v(0, 3));
                let m1 = r1[0].max(r1[1]).max(r1[2]);
                let m2 = r2[0].max(r2[1]).max(r2[2]);
                (mid(c1, c2), len(sub(c1, c2)) * 0.5 + m1.max(m2) + amp)
            }
            2 => {
                let (ec, er, a, b) = (v(2, 0), v(2, 1), v(2, 2), v(2, 3));
                let rm = er[0].max(er[1]).max(er[2]);
                let cc = mid(a, b);
                let cr = len(sub(b, a)) * 0.5 + a[3].max(b[3]);
                let c = [(ec[0] + cc[0]) * 0.5, (ec[1] + cc[1]) * 0.5, (ec[2] + cc[2]) * 0.5];
                let ecc = len([ec[0] - c[0], ec[1] - c[1], ec[2] - c[2]]);
                let ccc = len([cc[0] - c[0], cc[1] - c[1], cc[2] - c[2]]);
                let r = ecc + rm.max(ccc + cr);
                (c, r.max(ccc + cr))
            }
            _ => {
                let (a, b) = (v(i, 0), v(i, 1));
                (mid(a, b), len(sub(b, a)) * 0.5 + a[3].max(b[3]))
            }
        };
        for k in 0..3 {
            lo[k] = lo[k].min(c[k] - r);
            hi[k] = hi[k].max(c[k] + r);
        }
    }
    (lo, hi)
}

fn f64_of(v: &[Value]) -> f64 {
    match v {
        [Value::F64(x)] => *x,
        [Value::F32(x)] => f64::from(*x),
        other => panic!("expected a number, got {other:?}"),
    }
}

fn ours() -> CpuHost {
    CpuBuild::load(herd()).expect("load the herd").start_with(1).expect("start")
}

fn our_eval(host: &mut CpuHost, n: u32, pruned: u32) -> f64 {
    let (lo, hi) = spike_box();
    let mut args = vec![Value::I32(1), Value::I32(n as i32), Value::I32(pruned as i32)];
    args.extend(lo.iter().chain(&hi).map(|x| Value::F32(*x)));
    f64_of(&host.call_export("bench_eval", &args).expect("bench_eval"))
}

/// Times `a` and `b` alternately, `rounds` rounds after a warm-up of each: their medians, ms.
fn alternate(rounds: usize, mut a: impl FnMut(), mut b: impl FnMut()) -> (f64, f64) {
    a();
    b();
    let (mut ta, mut tb) = (Vec::new(), Vec::new());
    for _ in 0..rounds {
        let t = Instant::now();
        a();
        ta.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        b();
        tb.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    (median(&ta), median(&tb))
}

/// AC6's physique: seed 1's mass at a 2 cm finest cell within 0.05% of the spike's ground truth
/// (1,119.26 kg, a uniform 1 cm grid of 25M samples), as a job's work computes it.
#[test]
fn seed_1s_mass_is_within_0_05_percent_of_the_ground_truth() {
    let mut host = ours();
    let mass = f64_of(&host.call_export("mass", &[Value::I32(1), Value::F32(0.02)]).expect("mass"));
    let truth = 1119.26;
    let off = (mass - truth) / truth * 100.0;
    eprintln!(
        "seed 1's mass at 2 cm: {mass:.2} kg, {off:+.3}% from {truth} kg (the spike's: 1,118.94)"
    );
    assert!(off.abs() <= 0.05, "{mass} kg is {off:.3}% from {truth}");
}

/// AC6's timings on the CPU (WASM), in one harness, alternating: the physique at 2 cm (≤ 1.25×
/// the spike's), the grazer's field per evaluation whole and pruned, and a raycast (each ≤ 1.25×).
#[test]
#[ignore = "a timing run: cargo test -p wrela-tests --test suite herd:: -- --ignored --nocapture"]
fn the_cpu_side_costs_what_the_spikes_did() {
    let mut spike = Spike::new();
    let mut host = ours();
    let mut report = Vec::new();
    // The same points, so the same sums (to rounding).
    let n = 200_000;
    for pruned in [0, 1] {
        let (a, b) = (spike.bench_eval(n, pruned), our_eval(&mut host, n, pruned));
        assert!(
            (a - b).abs() <= 1e-4 * a.abs().max(1.0),
            "pruned {pruned}: the sums differ: {a} and {b}"
        );
        let (ts, to) =
            alternate(7, || _ = spike.bench_eval(n, pruned), || _ = our_eval(&mut host, n, pruned));
        report.push((if pruned == 1 { "evaluation, pruned" } else { "evaluation, whole" }, ts, to));
    }
    let (ts, to) = alternate(
        7,
        || _ = spike.mass(0.02),
        || _ = host.call_export("mass", &[Value::I32(1), Value::F32(0.02)]).expect("mass"),
    );
    report.push(("physique at 2 cm", ts, to));
    let rays = 20_000;
    let (ts, to) = alternate(
        7,
        || _ = spike.raycasts(rays),
        || _ = host.call_export("raycast", &[Value::I32(rays as i32)]).expect("raycast"),
    );
    report.push(("raycasts", ts, to));
    let mut over = Vec::new();
    for (what, ts, to) in &report {
        let ratio = to / ts;
        eprintln!("{what}: spike {ts:.2} ms, ours {to:.2} ms: {ratio:.2}x");
        if ratio > 1.25 {
            over.push(format!("{what} {ratio:.2}x"));
        }
    }
    assert!(over.is_empty(), "over 1.25x: {over:?}");
}
