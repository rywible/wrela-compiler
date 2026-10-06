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

/// The harness (wrela_tests::spike01, the spike's `main.js` ported) is held to the spike's
/// recorded run on the same inputs: at each cell size, the median live blocks, vertices and
/// triangles per individual, the herd's triangles and its holes. The kernels are the spike's,
/// unchanged; the recorded run was in Chrome, this is the native host's wgpu, on the M4.
#[test]
#[ignore = "needs a GPU and bun"]
fn the_spike_harness_gives_the_recorded_runs_counts() {
    let recorded: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture("run-2026-10-02T00-00-14-979Z.json")).expect("the run"),
    )
    .expect("JSON");
    let spike = wrela_tests::spike01::Spike::new();
    for cell in ["0.03", "0.02", "0.015", "0.01"] {
        let (per, _) = spike.extract_herd(cell.parse().expect("a cell size"), 40);
        let med = |f: &dyn Fn(&wrela_tests::spike01::Extracted) -> u32| {
            let mut v: Vec<u32> = per.iter().map(f).collect();
            v.sort_unstable();
            v[v.len() / 2]
        };
        let r = &recorded["extraction"][cell];
        let want = |k: &str| r[k].as_f64().expect(k);
        let ours = [
            ("blocks_median", f64::from(med(&|e| e.blocks))),
            ("live_blocks_median", f64::from(med(&|e| e.live))),
            ("verts_median", f64::from(med(&|e| e.verts))),
            ("tris_median", f64::from(med(&|e| e.tris))),
            ("tris_herd", per.iter().map(|e| f64::from(e.tris)).sum()),
            ("holes_total", per.iter().map(|e| f64::from(e.holes)).sum()),
        ];
        eprintln!(
            "{cell}: {}",
            ours.iter()
                .map(|(k, v)| format!("{k} {v} (recorded {})", want(k)))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for (k, v) in ours {
            assert_eq!(v, want(k), "{cell} m: {k}");
        }
        assert!(per.iter().all(|e| e.flags == 0), "{cell} m: an extraction overflowed");
    }
}

// ---- AC2: realization against the spike's extraction ------------------------------------------

/// A mesh: its vertices and its triangles (three indices each).
struct Tris {
    verts: Vec<[f32; 3]>,
    tris: Vec<[u32; 3]>,
}

impl Tris {
    fn new(verts: Vec<[f32; 3]>, indices: &[u32]) -> Tris {
        let tris = indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
        Tris { verts, tris }
    }

    fn corners(&self, t: [u32; 3]) -> [[f32; 3]; 3] {
        t.map(|i| self.verts[i as usize])
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The squared distance from `p` to triangle `abc` (Ericson's closest point, §5.1.5).
fn point_triangle2(p: [f32; 3], [a, b, c]: [[f32; 3]; 3]) -> f32 {
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let at = |s: f32, t: f32| {
        let q = [
            a[0] + ab[0] * s + ac[0] * t,
            a[1] + ab[1] * s + ac[1] * t,
            a[2] + ab[2] * s + ac[2] * t,
        ];
        let d = sub(p, q);
        dot(d, d)
    };
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return dot(ap, ap);
    }
    let bp = sub(p, b);
    let (d3, d4) = (dot(ab, bp), dot(ac, bp));
    if d3 >= 0.0 && d4 <= d3 {
        return dot(bp, bp);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return at(d1 / (d1 - d3), 0.0);
    }
    let cp = sub(p, c);
    let (d5, d6) = (dot(ab, cp), dot(ac, cp));
    if d6 >= 0.0 && d5 <= d6 {
        return dot(cp, cp);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return at(0.0, d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        let bc = sub(c, b);
        let q = [b[0] + bc[0] * w, b[1] + bc[1] * w, b[2] + bc[2] * w];
        let d = sub(p, q);
        return dot(d, d);
    }
    let denom = 1.0 / (va + vb + vc);
    at(vb * denom, vc * denom)
}

/// The distance from each vertex of `a` to `b`'s surface, searching cells of `h` around it (out
/// to `reach` of them; infinity where nothing is that near).
fn distances_to(a: &Tris, b: &Tris, h: f32, reach: i32) -> Vec<f32> {
    use std::collections::HashMap;
    let key = |p: [f32; 3]| [0, 1, 2].map(|k| (p[k] / h).floor() as i32);
    let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
    for (i, t) in b.tris.iter().enumerate() {
        let c = b.corners(*t);
        let lo = key([0, 1, 2].map(|k| c[0][k].min(c[1][k]).min(c[2][k])));
        let hi = key([0, 1, 2].map(|k| c[0][k].max(c[1][k]).max(c[2][k])));
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    grid.entry([x, y, z]).or_default().push(i as u32);
                }
            }
        }
    }
    a.verts
        .iter()
        .map(|&p| {
            let k = key(p);
            let mut best = f32::INFINITY;
            for x in -reach..=reach {
                for y in -reach..=reach {
                    for z in -reach..=reach {
                        for &t in grid.get(&[k[0] + x, k[1] + y, k[2] + z]).into_iter().flatten() {
                            best = best.min(point_triangle2(p, b.corners(b.tris[t as usize])));
                        }
                    }
                }
            }
            best.sqrt()
        })
        .collect()
}

/// The largest distance from a vertex of `a` to `b`'s surface (`distances_to`): what `a` has
/// that `b` hasn't.
fn one_sided_hausdorff(a: &Tris, b: &Tris, h: f32, reach: i32) -> f32 {
    distances_to(a, b, h, reach).into_iter().fold(0.0, f32::max)
}

/// Our realization of one grazer: its counts and mesh.
struct Ours {
    live: u32,
    tris: u32,
    holes: u32,
    flags: u32,
    mesh: Tris,
}

/// A mesh's counts and geometry from its five buffers (`herd-realize`'s order).
fn read_mesh(host: &mut wrela_host::Host, five: &[u32]) -> Ours {
    let args = wrela_tests::u32s(&host.read_buffer(five[4]).expect("args"));
    let (nv, ni) = (args[5] as usize, args[0] as usize);
    let v = host.read_buffer(five[1]).expect("vertices");
    let verts = v
        .chunks_exact(48)
        .take(nv)
        .map(|c| [0, 1, 2].map(|k| f32::from_le_bytes(c[4 * k..4 * k + 4].try_into().expect("4"))))
        .collect();
    let indices = wrela_tests::u32s(&host.read_buffer(five[3]).expect("quads"));
    Ours {
        live: args[10],
        tris: args[0] / 3,
        holes: args[6],
        flags: args[7],
        mesh: Tris::new(verts, &indices[..ni]),
    }
}

/// Ours at `cell`, as the spike's harness runs it: the room calibrated on the largest grid (a
/// quarter more, and 1,024 to spare), then the 40 back to back, twice; the second run's counts
/// and meshes, and each grazer's GPU time (its five dispatches), ms.
fn realize_herd(host: &mut wrela_host::Host, cell: f32) -> (Vec<Ours>, Vec<f64>) {
    let blocks = |host: &mut wrela_host::Host, s: u32| {
        let g = host.call_export("grid", &[Value::I32(s as i32), Value::F32(cell)]).expect("grid");
        let [Value::F32(x), Value::F32(y), Value::F32(z), _] = g[..] else { panic!("{g:?}") };
        (x * y * z) as u32
    };
    let largest = (1..=40).max_by_key(|&s| blocks(host, s)).expect("a seed");
    host.call_export("realize", &[Value::I32(largest as i32), Value::F32(cell)]).expect("realize");
    let b = host.buffers();
    let args = wrela_tests::u32s(&host.read_buffer(b[b.len() - 1]).expect("args"));
    let (v, q) = (args[8], args[9]);
    let room = [Value::I32((v + v / 4 + 1024) as i32), Value::I32((q + q / 4 + 1024) as i32)];
    let _ = host.take_timings();
    let mut times = Vec::new();
    for _ in 0..2 {
        let mut call = vec![Value::F32(cell)];
        call.extend(room);
        host.call_export("realize_herd", &call).expect("realize_herd");
        let t = host.take_timings().expect("timings");
        assert!(t.is_empty() || t.len() == 200, "five dispatches a grazer");
        times = t.chunks_exact(5).map(|c| c.iter().map(|t| t.nanos).sum::<f64>() / 1e6).collect();
    }
    let b = host.buffers();
    let meshes = &b[b.len() - 200..];
    let ours = meshes.chunks_exact(5).map(|five| read_mesh(host, five)).collect();
    (ours, times)
}

/// AC2: over the 40 grazers at 3, 2, 1.5 and 1 cm cells, on the fixture's own grids, ours
/// against the spike's extraction, run beside it in one process, alternating (the spike's
/// harness runs its batch three times and keeps the last).
#[test]
#[ignore = "needs a GPU and bun"]
fn realization_matches_the_spikes() {
    let dir = built("herd-realize");
    let options = wrela_host::Options { timestamps: true, ..wrela_host::Options::default() };
    let mut host = wrela_host::Host::load_with(&dir, &options).expect("load herd-realize");
    let spike = wrela_tests::spike01::Spike::new();
    let recorded_live = [2156.0, 4700.0, 8210.0, 18603.0];
    let mut report = Vec::new();
    for (k, cell) in [0.03, 0.02, 0.015, 0.01].into_iter().enumerate() {
        let (mut our_ms, mut their_ms) = (Vec::new(), Vec::new());
        let (mut ours, mut theirs, mut inds) = (Vec::new(), Vec::new(), Vec::new());
        for _ in 0..3 {
            let (t, i) = spike.extract_herd(cell, 40);
            their_ms.push(median(&t.iter().map(|e| e.total_ms()).collect::<Vec<_>>()));
            (theirs, inds) = (t, i);
            let (o, times) = realize_herd(&mut host, cell as f32);
            our_ms.push(median(&times));
            ours = o;
        }
        let mut hausdorff: f32 = 0.0;
        let mut tri_off: f64 = 0.0;
        // Where the spike's mesh has a hole (which this milestone fixes, #43 §7.5), ours has
        // vertices it hasn't: a few at each, within a cell of its surface. Elsewhere, ours is
        // within a tenth of a cell.
        let (mut at_holes, mut holes_ok) = (0, true);
        let h = cell as f32;
        for s in 0..40 {
            let (v, i) = spike.mesh(&inds[s]);
            let t = Tris::new(v, &i);
            let ds = distances_to(&ours[s].mesh, &t, h, 2);
            let far: Vec<f32> = ds.iter().copied().filter(|&d| d > 0.1 * h).collect();
            holes_ok &= far.len() as u32 <= 4 * theirs[s].holes && far.iter().all(|&d| d <= h);
            at_holes += far.len();
            let near = ds.iter().copied().filter(|&d| d <= 0.1 * h).fold(0.0, f32::max);
            hausdorff = hausdorff.max(near.max(one_sided_hausdorff(&t, &ours[s].mesh, h, 2)) / h);
            tri_off =
                tri_off.max((f64::from(ours[s].tris) / f64::from(theirs[s].tris) - 1.0).abs());
        }
        let live = median(&ours.iter().map(|o| f64::from(o.live)).collect::<Vec<_>>());
        let their_live = median(&theirs.iter().map(|e| f64::from(e.live)).collect::<Vec<_>>());
        let (ms, tms) = (median(&our_ms), median(&their_ms));
        let holes: u32 = ours.iter().map(|o| o.holes).sum();
        let line = format!(
            "{cell} m: live blocks {live} (spike {their_live}), holes {holes} (spike {}), triangles within {:.2}%, Hausdorff {:.1}% of a cell (but {at_holes} vertices at the spike's holes, within a cell), GPU {ms:.2} ms (spike {tms:.2}): {:.2}x",
            theirs.iter().map(|e| e.holes).sum::<u32>(),
            tri_off * 100.0,
            hausdorff * 100.0,
            ms / tms,
        );
        eprintln!("{line}");
        report.push((
            line,
            live <= recorded_live[k],
            holes == 0,
            ours.iter().all(|o| o.flags == 0),
            tri_off <= 0.01,
            hausdorff <= 0.10 && holes_ok,
            ms <= 1.25 * tms,
        ));
    }
    for (line, live, holes, room, tris, haus, time) in report {
        assert!(live, "live blocks over the spike's: {line}");
        assert!(holes, "holes: {line}");
        assert!(room, "a mesh overflowed: {line}");
        assert!(tris, "triangles more than 1% off: {line}");
        assert!(haus, "Hausdorff over 10% of a cell: {line}");
        assert!(time, "GPU time over 1.25x: {line}");
    }
}

// ---- AC7: the herd's grazer against field.wgsl ------------------------------------------------

const HARNESS_D: &str = "
@group(0) @binding(0) var<uniform> G: Grazer;
@group(0) @binding(1) var<storage, read> points: array<vec4f>;
@group(0) @binding(2) var<storage, read_write> dist: array<f32>;

@compute @workgroup_size(64)
fn eval_d(@builtin(global_invocation_id) id: vec3u) {
  dist[id.x] = grazer_d(points[id.x].xyz, ALL_PARTS, 0.0);
}
";

const HARNESS_G: &str = "
@group(0) @binding(0) var<uniform> G: Grazer;
@group(0) @binding(1) var<storage, read> points: array<vec4f>;
@group(0) @binding(2) var<storage, read_write> samp: array<vec4f>;

@compute @workgroup_size(64)
fn eval_g(@builtin(global_invocation_id) id: vec3u) {
  let s = grazer_g(points[id.x].xyz, ALL_PARTS, 0.0);
  samp[id.x] = vec4f(s.d, s.g);
}
";

/// A compute pipeline of a build: its WGSL and entry point, and the uniform bytes of its first
/// dispatch in `batches`.
fn compiled_kernel(
    dir: &std::path::Path,
    name: &str,
    batches: &[Vec<u8>],
) -> (String, String, Vec<u8>) {
    use wrela_abi::manifest::Stage;
    use wrela_abi::stream::{Command, decode};
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest");
    let manifest = wrela_abi::Manifest::parse(&manifest).expect("a valid manifest");
    let (index, p) = manifest
        .pipelines
        .iter()
        .enumerate()
        .find(|(_, p)| p.name == name)
        .unwrap_or_else(|| panic!("no pipeline `{name}`"));
    let Stage::Compute { entry, .. } = &p.stage else { panic!("`{name}` isn't a kernel") };
    let wgsl = std::fs::read_to_string(dir.join(&p.shader)).expect("the shader");
    for batch in batches {
        for cmd in decode(batch).expect("a valid batch") {
            if let Command::Dispatch { pipeline, uniforms, .. } = cmd
                && pipeline as usize == index
            {
                return (wgsl, entry.clone(), uniforms.to_vec());
            }
        }
    }
    panic!("nothing dispatched `{name}`")
}

/// The herd's grazer, seed 1, against field.wgsl with the seed-1 parameters, at 2²⁰ points over
/// its bounds: distances and gradients (every part in, unfiltered), and GPU time per evaluation
/// in one harness, alternating.
#[test]
#[ignore = "needs a GPU"]
fn the_herds_grazer_field_costs_what_field_wgsl_does() {
    use wrela_tests::{Bind, RawGpu, f32s};
    let n: u32 = 1 << 20;
    let dir = built("herd-realize");
    let options = wrela_host::Options { record: true, ..wrela_host::Options::default() };
    let mut host = wrela_host::Host::load_with(&dir, &options).expect("load");
    host.call_export("evaluate", &[Value::I32(1), Value::I32(n as i32)]).expect("evaluate");
    let made = host.buffers();
    let [.., p, d, s] = made[..] else { panic!("evaluate made {made:?}") };
    let points = host.read_buffer(p).expect("points");
    let dist = f32s(&host.read_buffer(d).expect("distances"));
    let samp = f32s(&host.read_buffer(s).expect("samples"));
    let batches = host.take_batches();
    drop(host);
    let field = std::fs::read_to_string(fixture("field.wgsl")).expect("field.wgsl");
    let gpu = RawGpu::new().expect("a GPU");
    let params: Vec<u8> = params_seed1().iter().flat_map(|x| x.to_le_bytes()).collect();
    let run = |wgsl: &str, entry: &str, uniform: &[u8], out: u64| {
        let first = if wgsl.contains("var<uniform> G") {
            Bind::Uniform(uniform)
        } else {
            Bind::Read(uniform)
        };
        gpu.run(wgsl, entry, &[first, Bind::Read(&points), Bind::Write(out)], n / 64, 10)
            .expect(entry)
    };
    let (hand_d, hand_g) = (format!("{field}{HARNESS_D}"), format!("{field}{HARNESS_G}"));
    let (_, hd) = run(&hand_d, "eval_d", &params, u64::from(n) * 4);
    let (_, hg) = run(&hand_g, "eval_g", &params, u64::from(n) * 16);
    let (hd, hg) = (f32s(&hd[0]), f32s(&hg[0]));
    let (ow, oe, ou) = compiled_kernel(&dir, "distances", &batches);
    let (gw, ge, gu) = compiled_kernel(&dir, "samples", &batches);
    let (mut od, mut og, mut td, mut tg) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for round in 0..=5 {
        let t = [
            run(&ow, &oe, &ou, u64::from(n) * 4).0,
            run(&hand_d, "eval_d", &params, u64::from(n) * 4).0,
            run(&gw, &ge, &gu, u64::from(n) * 16).0,
            run(&hand_g, "eval_g", &params, u64::from(n) * 16).0,
        ];
        if round > 0 {
            od.extend(&t[0]);
            td.extend(&t[1]);
            og.extend(&t[2]);
            tg.extend(&t[3]);
        }
    }
    let worst = (0..n as usize)
        .map(|i| f64::from((dist[i] - hd[i]).abs()).max(f64::from((samp[4 * i] - hg[4 * i]).abs())))
        .fold(0.0f64, f64::max);
    let (md, mg, hmd, hmg) = (median(&od), median(&og), median(&td), median(&tg));
    eprintln!(
        "distance {:.3} ms vs {:.3} ms ({:.2}x); with gradient {:.3} ms vs {:.3} ms ({:.2}x); worst |Δd| {worst:.2e} m",
        md / 1e6,
        hmd / 1e6,
        md / hmd,
        mg / 1e6,
        hmg / 1e6,
        mg / hmg
    );
}

// ---- AC1: the herd's frames against the spike's -------------------------------------------------

/// A still frame's time: one second in, 60 frames a second (frame 60).
const STILL_FRAME: u32 = 60;

/// The script that asks the herd for a still frame at `level` (`key`: Digit1 3 cm, Digit3
/// 1.5 cm), from the close-up's camera or the herd's.
fn still_script(key: &str, close: bool) -> String {
    let camera = if close { r#",{"frame":0,"type":"key","key":"KeyC"}"# } else { "" };
    format!(r#"[{{"frame":0,"type":"key","key":"{key}"}}{camera}]"#)
}

/// AC1's parity frames: the herd draws spike 01's placement with each grazer's phase at its own
/// plus 0.9 t, at t = 1 s, every grazer realized at 3 cm and at 1.5 cm, from the herd's camera
/// and the close-up's. Each frame matches the spike's (its kernels, run by the harness) within a
/// mean of 0.5/255, in the native host and in Chrome; and the two hosts' frames match each
/// other's within 0.5/255.
#[test]
#[ignore = "needs Chrome, python3, a GPU and bun"]
fn the_herd_draws_the_spikes_frames() {
    use wrela_tests::spike01::{H, SceneKind, W, image_difference};
    let (dir, rel) = wrela_tests::page("examples/herd", "herd-parity");
    let mut spike = wrela_tests::spike01::Spike::new();
    let t = f64::from(STILL_FRAME) / 60.0;
    let times: Vec<f32> = (0..=STILL_FRAME).map(|i| wrela_host::frame_time(i, 60.0)).collect();
    let mut report = Vec::new();
    for (key, cell) in [("Digit1", 0.03), ("Digit3", 0.015)] {
        for close in [false, true] {
            let what = format!("{} at {cell} m", if close { "the close-up" } else { "the herd" });
            let script = still_script(key, close);
            let (_, inds) = spike.extract_herd(cell, if close { 1 } else { 40 });
            let kind = if close { SceneKind::CloseUp } else { SceneKind::Herd };
            let scene = spike.scene(kind, inds.iter().collect());
            let theirs = spike.render(&scene, t);
            let parsed = wrela_host::parse_script(&script).expect("a script");
            let native = wrela_host::Host::load(&dir)
                .expect("load the herd")
                .run_frames_with(&times, W, H, &parsed)
                .expect("run the herd");
            let name = format!("{}-{key}", if close { "close" } else { "herd" });
            native.write_png(dir.join(format!("results/{name}-native.png"))).expect("png");
            wrela_host::image::write_png(
                &dir.join(format!("results/{name}-spike.png")),
                W,
                H,
                &theirs,
            )
            .expect("png");
            std::fs::write(dir.join("still.json"), &script).expect("write the script");
            let run = wrela_tests::ChromeRun {
                input: "still.json".into(),
                ..wrela_tests::ChromeRun::new(STILL_FRAME + 1, W, H, 60.0)
            };
            let chrome = wrela_tests::run_in_chrome_with(&rel, run);
            let (n, n_far) = image_difference(&native.frame, &theirs);
            let (c, c_far) = image_difference(&chrome.frame, &theirs);
            let (h, h_far) = image_difference(&chrome.frame, &native.frame);
            let line = format!(
                "{what}: native vs the spike {n:.3}/255 ({:.2}% over 8), Chrome vs the spike {c:.3}/255 ({:.2}%), Chrome vs native {h:.3}/255 ({:.2}%)",
                100.0 * n_far,
                100.0 * c_far,
                100.0 * h_far
            );
            eprintln!("{line}");
            report.push((line, n <= 0.5 && c <= 0.5, h <= 0.5));
        }
    }
    for (line, spike_ok, hosts_ok) in report {
        assert!(spike_ok, "over 0.5/255 from the spike's frame: {line}");
        assert!(hosts_ok, "the hosts' frames differ by over 0.5/255: {line}");
    }
}

// ---- AC3: what drawing the herd costs -----------------------------------------------------------

/// Each frame's GPU time (ms) over frames `from..` of a run: its passes' own times summed
/// (`passes`: the shadow pass and the screen pass, as the spike's creatures are its shadow and
/// shading passes'), or the whole frame, from its first pass's start to its last one's end (the
/// spike's frame, as passes overlap).
fn frame_ms(timings: &[wrela_host::GpuTiming], from: usize, passes: bool) -> Vec<f64> {
    let last = timings.iter().map(|t| t.frame).max().unwrap_or(0);
    (from..=last)
        .map(|f| {
            let mut ts = timings.iter().filter(|t| t.frame == f).peekable();
            if ts.peek().is_none() {
                return 0.0;
            }
            if passes {
                return ts.filter(|t| t.label.ends_with("pass")).map(|t| t.nanos).sum::<f64>()
                    / 1e6;
            }
            let (mut lo, mut hi) = (f64::INFINITY, 0.0f64);
            for t in ts {
                lo = lo.min(t.start);
                hi = hi.max(t.start + t.nanos);
            }
            (hi - lo) / 1e6
        })
        .collect()
}

/// AC3: GPU time for the herd at 3 cm and at 1.5 cm, and the close-up, against the spike's
/// kernels run by the harness, alternating, as still frames: the creatures (shadow and shading)
/// and the whole frame (ours poses on the GPU; the spike posed on the CPU). Each ≤ 1.25×.
#[test]
#[ignore = "needs a GPU and bun"]
fn drawing_the_herd_costs_what_the_spikes_did() {
    use wrela_tests::spike01::{H, SceneKind, W};
    let dir = herd_gpu();
    let mut spike = wrela_tests::spike01::Spike::new();
    let frames = STILL_FRAME + 1;
    let times: Vec<f32> = (0..frames).map(|i| wrela_host::frame_time(i, 60.0)).collect();
    // Realization settles in the first frames; the last 30 are timed.
    let from = (frames - 30) as usize;
    let options = wrela_host::Options { timestamps: true, ..wrela_host::Options::default() };
    let mut report = Vec::new();
    for (key, cell, close) in
        [("Digit1", 0.03, false), ("Digit3", 0.015, false), ("Digit3", 0.015, true)]
    {
        let what = format!("{} at {cell} m", if close { "the close-up" } else { "the herd" });
        let (_, inds) = spike.extract_herd(cell, if close { 1 } else { 40 });
        let kind = if close { SceneKind::CloseUp } else { SceneKind::Herd };
        let scene = spike.scene(kind, inds.iter().collect());
        let (mut ours_c, mut ours_f, mut theirs_c, mut theirs_f) = (vec![], vec![], vec![], vec![]);
        let (mut ours_s, mut theirs_s) = (vec![], vec![]);
        for _ in 0..3 {
            for terrain in [false, true] {
                let mut script = still_script(key, close);
                if !terrain {
                    script = script.replace(']', r#",{"frame":0,"type":"key","key":"KeyT"}]"#);
                }
                let parsed = wrela_host::parse_script(&script).expect("a script");
                let mut host = wrela_host::Host::load_with(&dir, &options).expect("load the herd");
                let run = host.run_frames_with(&times, W, H, &parsed).expect("run the herd");
                // Without the terrain, the passes are the creatures'; with it, the whole frame.
                let ms = frame_ms(&run.timings, from, !terrain);
                if !terrain {
                    let shadow =
                        run.timings.iter().filter(|t| t.frame >= from && t.label == "pass");
                    ours_s.extend(shadow.map(|t| t.nanos / 1e6));
                }
                if terrain { ours_f.extend(ms) } else { ours_c.extend(ms) }
            }
            for t in spike.measure(&scene, 10, 30) {
                theirs_c.push(t.creatures);
                theirs_s.push(t.shadow);
                theirs_f.push(t.frame);
            }
        }
        let (oc, of, tc, tf) =
            (median(&ours_c), median(&ours_f), median(&theirs_c), median(&theirs_f));
        let (os, ts) = (median(&ours_s), median(&theirs_s));
        let line = format!(
            "{what}: creatures {oc:.2} ms (spike {tc:.2}, {:.2}x; shadow {os:.2}, spike {ts:.2}), whole frame {of:.2} ms (spike {tf:.2}, {:.2}x)",
            oc / tc,
            of / tf
        );
        eprintln!("{line}");
        report.push((line, oc <= 1.25 * tc, close || of <= 1.25 * tf));
    }
    for (line, creatures, frame) in report {
        assert!(creatures, "the creatures cost over 1.25x: {line}");
        assert!(frame, "the frame costs over 1.25x: {line}");
    }
}

/// The herd, built once for the GPU tests (`built` builds it under the suite's scratch).
fn herd_gpu() -> PathBuf {
    built("../../examples/herd")
}

/// AC1, paced: 600 frames of the live herd at 60 Hz in Chrome, with LOD and the sim worker
/// running and seven helpers: no frame's GPU time (its passes' and dispatches' timestamps) is
/// over 16.7 ms. Reported beside: the pipelines (at most 64; the spike's 8), the time from
/// opening the page to the first frame with all 40 grazers, and the bytes downloaded before it.
#[test]
#[ignore = "needs Chrome, python3 and a GPU"]
fn the_herd_keeps_its_frames_in_chrome() {
    let (dir, rel) = wrela_tests::page("examples/herd", "herd-paced");
    let run = wrela_tests::ChromeRun {
        workers: 8,
        timestamps: true,
        paced: true,
        nohash: true,
        ..wrela_tests::ChromeRun::new(600, 1920, 1080, 60.0)
    };
    let chrome = wrela_tests::run_in_chrome_with(&rel, run);
    // Each frame from its first pass's start to its last one's end (`timings.json`).
    let mut spans = std::collections::BTreeMap::new();
    let timings = wrela_tests::result_json(&dir.join("results"), "timings.json");
    for t in timings.as_array().expect("timings") {
        let f = t["frame"].as_u64().expect("a frame");
        let (start, ns) = (t["start"].as_f64().expect("a start"), t["nanos"].as_f64().expect("ns"));
        let e = spans.entry(f).or_insert((f64::INFINITY, 0.0f64));
        *e = (e.0.min(start), e.1.max(start + ns));
    }
    let ms: Vec<f64> = spans.values().map(|(lo, hi)| (hi - lo) / 1e6).collect();
    let missed = ms.iter().filter(|&&m| m > 16.7).count();
    let worst = ms.iter().copied().fold(0.0, f64::max);
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest");
    let pipelines = wrela_abi::Manifest::parse(&manifest).expect("a manifest").pipelines.len();
    let results = dir.join("results");
    let load = wrela_tests::result_json(&results, "load.json");
    let opened = load["opened_ms"].as_f64().expect("opened_ms");
    let all = chrome.printed.iter().find(|(_, l)| l.contains("grazers drawn")).map(|(f, _)| *f);
    let (to_all, bytes) = match all {
        Some(f) => {
            let at = chrome.began_ms[f];
            let bytes: f64 = load["resources"]
                .as_array()
                .expect("resources")
                .iter()
                .filter(|r| r["end_ms"].as_f64().expect("end_ms") <= at)
                .map(|r| r["bytes"].as_f64().expect("bytes"))
                .sum();
            (at - opened, bytes)
        }
        None => (f64::NAN, f64::NAN),
    };
    eprintln!(
        "paced: {missed} of {} frames over 16.7 ms (worst {worst:.2} ms, median {:.2}); {pipelines} pipelines (the spike's 8); all 40 drawn {to_all:.0} ms after the page opened, {:.2} MB downloaded before",
        ms.len(),
        median(&ms),
        bytes / 1e6
    );
    assert!(all.is_some(), "the herd never drew all 40 grazers");
    assert!(ms.len() >= 590, "only {} frames were timed", ms.len());
    assert_eq!(missed, 0, "{missed} frames missed their deadline (worst {worst:.2} ms)");
    assert!(pipelines <= 64, "{pipelines} pipelines");
}

/// AC2: no count is read back between extraction's passes. Over 240 frames of the live herd
/// (LOD and the realization queue at work, the camera moving to the close-up and back), every
/// `ReadBuffer` the program records comes after a realization's last pass, never between its
/// first (`cull_blocks`) and its last (`skin_vertices`); and there are as many as the realizer
/// counts for its room calibrations and overflow checks (#43 §7.8).
#[test]
#[ignore = "needs a GPU"]
fn realization_reads_nothing_back_between_its_passes() {
    use wrela_abi::stream::{Command, decode};
    let dir = herd_gpu();
    let options = wrela_host::Options { record: true, ..wrela_host::Options::default() };
    let mut host = wrela_host::Host::load_with(&dir, &options).expect("load the herd");
    let script = wrela_host::parse_script(
        r#"[{"frame":100,"type":"key","key":"KeyC"},{"frame":160,"type":"key","key":"KeyH"}]"#,
    )
    .expect("a script");
    let times: Vec<f32> = (0..240).map(|i| wrela_host::frame_time(i, 60.0)).collect();
    host.run_frames_with(&times, 960, 540, &script).expect("run the herd");
    let r = host.call_export("realized", &[]).expect("realized");
    let [Value::F32(realizations), Value::F32(readbacks), _, _] = r[..] else { panic!("{r:?}") };
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest");
    let manifest = wrela_abi::Manifest::parse(&manifest).expect("a manifest");
    let name = |p: u32| manifest.pipelines[p as usize].name.clone();
    let (mut inside, mut reads, mut between, mut culls) = (false, 0u32, 0u32, 0u32);
    for batch in host.take_batches() {
        for cmd in decode(&batch).expect("a valid batch") {
            match cmd {
                Command::Dispatch { pipeline, .. } | Command::DispatchIndirect { pipeline, .. } => {
                    let n = name(pipeline);
                    if n.starts_with("cull_blocks") {
                        inside = true;
                        culls += 1;
                    } else if n.starts_with("skin_vertices") {
                        inside = false;
                    }
                }
                Command::ReadBuffer { .. } => {
                    reads += 1;
                    between += u32::from(inside);
                }
                _ => {}
            }
        }
    }
    eprintln!(
        "{culls} realizations recorded ({realizations} counted), {reads} readbacks ({readbacks} counted), {between} between passes"
    );
    assert!(culls > 40, "only {culls} realizations");
    assert_eq!(between, 0, "a readback between a realization's passes");
    assert_eq!(reads as f32, readbacks, "readbacks the realizer doesn't count");
    assert_eq!(culls as f32, realizations, "realizations the realizer doesn't count");
}
