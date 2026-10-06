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
    let t = f64::from(STILL_FRAME) / 60.0;
    let times: Vec<f32> = (0..=STILL_FRAME).map(|i| wrela_host::frame_time(i, 60.0)).collect();
    let cases = [
        ("Digit1", 0.03, false),
        ("Digit1", 0.03, true),
        ("Digit3", 0.015, false),
        ("Digit3", 0.015, true),
    ];
    // The spike's frames first: its harness holds the GPU lock, which Chrome's runs wait for.
    let spike_frames: Vec<Vec<u8>> = {
        let mut spike = wrela_tests::spike01::Spike::new();
        cases
            .iter()
            .map(|&(_, cell, close)| {
                let (_, inds) = spike.extract_herd(cell, if close { 1 } else { 40 });
                let kind = if close { SceneKind::CloseUp } else { SceneKind::Herd };
                let scene = spike.scene(kind, inds.iter().collect());
                spike.render(&scene, t)
            })
            .collect()
    };
    let mut report = Vec::new();
    for (&(key, cell, close), theirs) in cases.iter().zip(&spike_frames) {
        let what = format!("{} at {cell} m", if close { "the close-up" } else { "the herd" });
        let script = still_script(key, close);
        let parsed = wrela_host::parse_script(&script).expect("a script");
        let native = wrela_host::Host::load(&dir)
            .expect("load the herd")
            .run_frames_with(&times, W, H, &parsed)
            .expect("run the herd");
        let name = format!("{}-{key}", if close { "close" } else { "herd" });
        native.write_png(dir.join(format!("{name}-native.png"))).expect("png");
        wrela_host::image::write_png(&dir.join(format!("{name}-spike.png")), W, H, theirs)
            .expect("png");
        std::fs::write(dir.join("still.json"), &script).expect("write the script");
        let run = wrela_tests::ChromeRun {
            input: "still.json".into(),
            ..wrela_tests::ChromeRun::new(STILL_FRAME + 1, W, H, 60.0)
        };
        let chrome = wrela_tests::run_in_chrome_with(&rel, run);
        let (n, n_far) = image_difference(&native.frame, theirs);
        let (c, c_far) = image_difference(&chrome.frame, theirs);
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
    for (line, spike_ok, hosts_ok) in report {
        assert!(spike_ok, "over 0.5/255 from the spike's frame: {line}");
        assert!(hosts_ok, "the hosts' frames differ by over 0.5/255: {line}");
    }
}

// ---- AC3: what drawing the herd costs -----------------------------------------------------------

/// Each frame's GPU time (ms) over frames `from..` of a run: its passes' own times summed
/// (`passes`: the shadow pass and the screen pass, as the spike's creatures are its shadow and
/// shading passes'), or every pass's and dispatch's. Summed, not from the first start to the
/// last end: on the native host the spike's terrain pass runs beside its shadow pass, though
/// it reads the shadow map, and ours waits for it, so spans compare scheduling, not work.
fn frame_ms(timings: &[wrela_host::GpuTiming], from: usize, passes: bool) -> Vec<f64> {
    let last = timings.iter().map(|t| t.frame).max().unwrap_or(0);
    let counted = |t: &&wrela_host::GpuTiming| !passes || t.label.ends_with("pass");
    (from..=last)
        .map(|f| {
            timings.iter().filter(|t| t.frame == f).filter(counted).map(|t| t.nanos).sum::<f64>()
                / 1e6
        })
        .collect()
}

/// AC3: GPU time for the herd at 3 cm and at 1.5 cm, and the close-up, against the spike's
/// kernels run by the harness, alternating, as still frames: the creatures (shadow and shading)
/// and the whole frame, its passes' times summed (ours poses on the GPU; the spike posed on the
/// CPU). Each ≤ 1.25×.
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
        let (mut parts, mut theirs_t) = (std::collections::BTreeMap::new(), vec![]);
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
                if terrain {
                    ours_f.extend(ms);
                    for t in run.timings.iter().filter(|t| t.frame >= from) {
                        parts.entry(t.label.clone()).or_insert_with(Vec::new).push(t.nanos / 1e6);
                    }
                } else {
                    ours_c.extend(ms)
                }
            }
            for t in spike.measure(&scene, 10, 30) {
                theirs_c.push(t.creatures);
                theirs_s.push(t.shadow);
                theirs_t.push(t.terrain);
                theirs_f.push(t.passes);
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
        let each: Vec<String> = parts
            .iter()
            .map(|(l, v)| format!("{} {:.3}", l.split("::").next().unwrap_or(l), median(v)))
            .collect();
        eprintln!(
            "  ours, with the terrain: {}; the spike's terrain {:.3}",
            each.join(", "),
            median(&theirs_t)
        );
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

/// A paced run of the live herd in Chrome at 1080p, 60 Hz, 600 frames, seven helpers, with
/// `script`'s keys: the run, and each frame's GPU time (ms), its passes' and dispatches' own
/// times summed. Not from the first start to the last end: paced, the GPU's clocks fall and rise
/// (the same frame took 3.8 ms, then 14.5, then 5.6), and Chrome leaves gaps between a frame's
/// passes that grow as they fall (up to 4 ms), which the native host doesn't.
fn paced_herd(name: &str, script: &str) -> (std::path::PathBuf, wrela_tests::BrowserRun, Vec<f64>) {
    let (dir, rel) = wrela_tests::page("examples/herd", name);
    std::fs::write(dir.join("paced.json"), script).expect("write the script");
    let run = wrela_tests::ChromeRun {
        input: "paced.json".into(),
        workers: 8,
        timestamps: true,
        paced: true,
        nohash: true,
        ..wrela_tests::ChromeRun::new(600, 1920, 1080, 60.0)
    };
    let chrome = wrela_tests::run_in_chrome_with(&rel, run);
    let mut per_frame = std::collections::BTreeMap::new();
    for (f, _, ns) in &chrome.timings {
        *per_frame.entry(*f).or_insert(0.0) += ns / 1e6;
    }
    // The program's `init` records the shadow map's first clear before frame 0: not a frame's.
    let ms: Vec<f64> = per_frame.iter().filter(|(f, _)| **f > 0).map(|(_, m)| *m).collect();
    (dir, chrome, ms)
}

/// AC1, paced: 600 frames of the live herd (`paced_herd`), with LOD and the sim worker running:
/// no frame's GPU time is over 16.7 ms, from the 40 grazers' spawn on (AC4's frame time under
/// load). Each grazer is drawn within 0.5 s of its spawn and has its level within 2 s (AC4's
/// latency); a tick takes at most 4 ms at the 99th percentile (AC5). Reported beside: the
/// pipelines (at most 64; the spike's 8), the time from opening the page to the first frame with
/// all 40 grazers, and the bytes downloaded before it.
#[test]
#[ignore = "needs Chrome, python3 and a GPU"]
fn the_herd_keeps_its_frames_in_chrome() {
    let (dir, chrome, ms) = paced_herd("herd-paced", "[]");
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
    // Each grazer's latency, from the first frame (they spawn in `init`, before it).
    let first = |what: &str, s: u32| {
        let line = format!("grazer {s} {what}");
        let f = chrome.printed.iter().find(|(_, l)| *l == line).map(|(f, _)| *f);
        f.map_or(f64::INFINITY, |f| chrome.began_ms[f] - chrome.began_ms[0])
    };
    let drawn = (1..=40).map(|s| first("drawn", s)).fold(0.0, f64::max);
    let leveled = (1..=40).map(|s| first("at its level", s)).fold(0.0, f64::max);
    let mut ticks = chrome.ticks.as_ref().expect("the sim's ticks").cpu_ms.clone();
    ticks.sort_by(f64::total_cmp);
    let p99 = ticks[(ticks.len() * 99 / 100).min(ticks.len() - 1)];
    eprintln!(
        "paced: {missed} of {} frames over 16.7 ms (worst {worst:.2} ms, median {:.2}); every grazer drawn within {drawn:.0} ms and at its level within {leveled:.0} ms; {} ticks, 99th percentile {p99:.2} ms; {pipelines} pipelines (the spike's 8); all 40 drawn {to_all:.0} ms after the page opened, {:.2} MB downloaded before",
        ms.len(),
        median(&ms),
        ticks.len(),
        bytes / 1e6
    );
    assert!(all.is_some(), "the herd never drew all 40 grazers");
    assert!(ms.len() >= 590, "only {} frames were timed", ms.len());
    assert_eq!(missed, 0, "{missed} frames missed their deadline (worst {worst:.2} ms)");
    assert!(pipelines <= 64, "{pipelines} pipelines");
    assert!(drawn <= 500.0, "a grazer was drawn {drawn:.0} ms after its spawn");
    assert!(leveled <= 2000.0, "a grazer had its level {leveled:.0} ms after its spawn");
    assert!(p99 <= 4.0, "a tick's 99th percentile is {p99:.2} ms");
}

/// AC4, frame time under load: the camera moves from the close-up to the herd (at frame 300,
/// the close-up's grazer at its finest level), and no frame from then on is over 16.7 ms while
/// the herd's grazers are realized for the herd's view.
#[test]
#[ignore = "needs Chrome, python3 and a GPU"]
fn moving_to_the_herd_keeps_the_frames_in_chrome() {
    let script =
        r#"[{"frame":0,"type":"key","key":"KeyC"},{"frame":300,"type":"key","key":"KeyH"}]"#;
    let (_, chrome, ms) = paced_herd("herd-move", script);
    let after: Vec<f64> = ms[299..].to_vec();
    let missed = after.iter().filter(|&&m| m > 16.7).count();
    let worst = after.iter().copied().fold(0.0, f64::max);
    let drawn = chrome.printed.iter().filter(|(f, l)| *f >= 300 && l.ends_with("drawn")).count();
    eprintln!(
        "close-up to herd: {missed} of {} frames over 16.7 ms (worst {worst:.2} ms, median {:.2}); {drawn} grazers first drawn after the move",
        after.len(),
        median(&after)
    );
    assert!(drawn >= 30, "the move drew only {drawn} grazers anew");
    assert_eq!(missed, 0, "{missed} frames missed their deadline (worst {worst:.2} ms)");
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

/// AC2: shared corners agree. In a debug build each live block writes the value of each
/// boundary corner it evaluates (`Scratch::probe`, the sixth newest buffer after
/// `probe_corners`); over the 40 grazers at 3, 2, 1.5 and 1 cm,
/// every corner has one bit pattern, whichever blocks evaluate it (#43 §7.5).
#[test]
#[ignore = "needs a GPU"]
fn shared_corners_have_one_value() {
    let src = repo_root().join("compiler/tests/herd-realize");
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("herd-realize-debug");
    let _ = std::fs::remove_dir_all(&dir);
    let out = wrela_driver::build_debug(&src);
    assert!(!out.has_errors(), "herd-realize doesn't build in debug");
    out.write_to(&dir).expect("write the build");
    let mut host = wrela_host::Host::load(&dir).expect("load");
    let (mut corners, mut shared, mut values) = (0u64, 0u64, 0u64);
    for cell in [0.03f32, 0.02, 0.015, 0.01] {
        for s in 1..=40 {
            host.call_export("probe_corners", &[Value::I32(s), Value::F32(cell)]).expect("probe");
            let b = host.buffers();
            let words = wrela_tests::u32s(&host.read_buffer(b[b.len() - 6]).expect("the probe"));
            let mut seen: std::collections::HashMap<u32, (u32, u32)> =
                std::collections::HashMap::new();
            let mut n = 0;
            for p in words.chunks_exact(4) {
                for k in 0..2 {
                    if p[k] == 0 {
                        continue;
                    }
                    n += 1;
                    let e = seen.entry(p[k]).or_insert((p[2 + k], 0));
                    assert_eq!(
                        e.0,
                        p[2 + k],
                        "seed {s} at {cell} m: corner {} has two values",
                        p[k] - 1
                    );
                    e.1 += 1;
                }
            }
            values += n as u64;
            corners += seen.len() as u64;
            shared += seen.values().filter(|v| v.1 > 1).count() as u64;
        }
    }
    eprintln!(
        "{values} boundary corner values, {corners} corners, {shared} shared by blocks: each one value"
    );
    assert!(shared > 1000, "too few shared corners to show anything: {shared}");
}

// ---- AC4: LOD and mesh memory ---------------------------------------------------------------------

/// AC4, LOD: the herd's still frame with each grazer at the level LOD gives it (key 4) matches
/// the frame with all at 1.5 cm (key 3) within a mean of 0.1/255, in the native host; its
/// creatures' GPU time (shadow and shading, the terrain left out) is ≤ 1.25× the spike's herd
/// at 3 cm, run by the harness, alternating.
#[test]
#[ignore = "needs a GPU and bun"]
fn lod_looks_like_the_finest_and_costs_like_the_coarsest() {
    use wrela_tests::spike01::{H, SceneKind, W, image_difference};
    let dir = herd_gpu();
    let frames = 241;
    let times: Vec<f32> = (0..frames).map(|i| wrela_host::frame_time(i, 60.0)).collect();
    let still = |key: &str, terrain: bool, timed: bool| {
        let mut script = still_script(key, false);
        if !terrain {
            script = script.replace(']', r#",{"frame":0,"type":"key","key":"KeyT"}]"#);
        }
        let parsed = wrela_host::parse_script(&script).expect("a script");
        let options = wrela_host::Options { timestamps: timed, ..wrela_host::Options::default() };
        let mut host = wrela_host::Host::load_with(&dir, &options).expect("load the herd");
        let run = host.run_frames_with(&times, W, H, &parsed).expect("run the herd");
        let levels: Vec<u32> = (1..=40)
            .map(|s| {
                let v = host.call_export("level", &[Value::I32(s)]).expect("level");
                let [Value::F32(drawn), Value::F32(target)] = v[..] else { panic!("{v:?}") };
                assert_eq!(drawn, target, "grazer {s} hasn't its level after {frames} frames");
                drawn as u32
            })
            .collect();
        (run, levels)
    };
    let (lod, levels) = still("Digit4", true, false);
    let (finest, _) = still("Digit3", true, false);
    let (mean, far) = image_difference(&lod.frame, &finest.frame);
    let count = |l: u32| levels.iter().filter(|&&x| x == l).count();
    eprintln!(
        "LOD: {} grazers at 3 cm, {} at 2 cm, {} at 1.5 cm; against all at 1.5 cm {mean:.3}/255 ({:.3}% over 8)",
        count(0),
        count(1),
        count(2),
        100.0 * far
    );
    let (mut ours, mut theirs) = (Vec::new(), Vec::new());
    {
        let mut spike = wrela_tests::spike01::Spike::new();
        let (_, inds) = spike.extract_herd(0.03, 40);
        let scene = spike.scene(SceneKind::Herd, inds.iter().collect());
        for _ in 0..3 {
            let (run, _) = still("Digit4", false, true);
            ours.extend(frame_ms(&run.timings, (frames - 30) as usize, true));
            theirs.extend(spike.measure(&scene, 10, 30).iter().map(|t| t.creatures));
        }
    }
    let (o, t) = (median(&ours), median(&theirs));
    eprintln!("LOD's creatures {o:.2} ms, the spike's at 3 cm {t:.2} ms: {:.2}x", o / t);
    assert!(mean <= 0.1, "LOD's frame is {mean:.3}/255 from the finest's");
    assert!(o <= 1.25 * t, "LOD's creatures cost {:.2}x the spike's at 3 cm", o / t);
}

/// AC4, memory: the herd's allocated mesh memory with every grazer at 3 cm, and at 1.5 cm, is
/// ≤ 1.25× what the spike's harness allocates for the same (each individual's room sized by a
/// calibration on the largest, plus a quarter).
#[test]
#[ignore = "needs a GPU and bun"]
fn mesh_memory_is_the_spikes() {
    let dir = herd_gpu();
    let times: Vec<f32> = (0..=STILL_FRAME).map(|i| wrela_host::frame_time(i, 60.0)).collect();
    let mut ours = Vec::new();
    for key in ["Digit1", "Digit3"] {
        let parsed = wrela_host::parse_script(&still_script(key, false)).expect("a script");
        let mut host = wrela_host::Host::load(&dir).expect("load the herd");
        host.run_frames_with(&times, 960, 540, &parsed).expect("run the herd");
        ours.push(wrela_tests::one_f32(&mut host, "mesh_memory", &[]) as f64);
    }
    let spike = wrela_tests::spike01::Spike::new();
    let mut report = Vec::new();
    for (k, cell) in [0.03, 0.015].into_iter().enumerate() {
        let (_, inds) = spike.extract_herd(cell, 40);
        let theirs: f64 = inds
            .iter()
            .map(|i| (u64::from(i.vcap) * 48 + u64::from(i.icap) * 4 + 32) as f64)
            .sum::<f64>()
            / 1048576.0;
        let line = format!(
            "{cell} m: {:.1} MiB allocated (the spike's harness {theirs:.1}): {:.2}x",
            ours[k],
            ours[k] / theirs
        );
        eprintln!("{line}");
        report.push((line, ours[k] <= 1.25 * theirs));
    }
    for (line, ok) in report {
        assert!(ok, "over 1.25x the spike's mesh memory: {line}");
    }
}

/// AC4, no leaks: a scripted 10-minute camera tour of the live herd (key O: five 2-minute loops
/// round it, from 30 m out to 4), at 20 frames a second in the native host: no slab overflows, no
/// grazer once drawn is ever without a mesh, and the mesh memory allocated after the tour is
/// within one slab per level of its peak during the first loop.
#[test]
#[ignore = "needs a GPU"]
fn a_ten_minute_tour_leaks_no_mesh_memory() {
    let dir = herd_gpu();
    let mut host = wrela_host::Host::load(&dir).expect("load the herd");
    let fps = 20.0;
    let start =
        wrela_host::parse_script(r#"[{"frame":0,"type":"key","key":"KeyO"}]"#).expect("a script");
    let (mut first_peak, mut peak, mut lost) = (0.0f64, 0.0f64, 0u32);
    let mut drawn_once = [false; 40];
    for second in 0..600u32 {
        let times: Vec<f32> =
            (0..fps as u32).map(|i| wrela_host::frame_time(second * fps as u32 + i, fps)).collect();
        let script = if second == 0 { &start[..] } else { &[] };
        host.run_frames_with(&times, 960, 540, script).expect("run the herd");
        let mib = f64::from(wrela_tests::one_f32(&mut host, "mesh_memory", &[]));
        peak = peak.max(mib);
        if second < 120 {
            first_peak = first_peak.max(mib);
        }
        for (s, once) in drawn_once.iter_mut().enumerate() {
            let v = host.call_export("level", &[Value::I32(s as i32 + 1)]).expect("level");
            let [Value::F32(drawn), _] = v[..] else { panic!("{v:?}") };
            let has = drawn < 4.0;
            if *once && !has {
                lost += 1;
            }
            *once |= has;
        }
    }
    let after = f64::from(wrela_tests::one_f32(&mut host, "mesh_memory", &[]));
    let slabs: f64 = (0..3)
        .map(|l| f64::from(wrela_tests::one_f32(&mut host, "slab_mib", &[Value::I32(l)])))
        .sum();
    let r = host.call_export("realized", &[]).expect("realized");
    let [Value::F32(realizations), _, Value::F32(holes), Value::F32(overflows)] = r[..] else {
        panic!("{r:?}")
    };
    eprintln!(
        "tour: {realizations} realizations, {overflows} overflows, {holes} holes, {lost} meshes lost; mesh memory peaked at {first_peak:.1} MiB in the first loop, {peak:.1} in all, {after:.1} after (a slab per level: {slabs:.2} MiB)"
    );
    assert_eq!(overflows, 0.0, "slabs overflowed");
    assert_eq!(lost, 0, "a drawn grazer lost its mesh");
    assert!(drawn_once.iter().all(|&d| d), "a grazer was never drawn");
    assert!(
        after <= first_peak + slabs,
        "{after:.1} MiB after the tour, over the first loop's peak"
    );
}

/// AC4, cold pipelines: creating every one of the herd's pipelines cold (each shader made unique),
/// all at once, in Chrome takes ≤ 1.5× creating spike 01's 12 cold, all at once, as its main.js
/// did (compiler/tests/spike01/pipelines), alternating, three of each.
#[test]
#[ignore = "needs Chrome, python3 and a GPU"]
fn the_herds_pipelines_load_cold_as_fast() {
    let (dir, rel) = wrela_tests::page("examples/herd", "herd-cold");
    let spike = "compiler/tests/spike01/pipelines";
    let (mut ours, mut theirs, mut count) = (Vec::new(), Vec::new(), 0);
    for k in 0..3u32 {
        let salt = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock")
            .as_secs() as u32
            % 1_000_000
            + k
            + 1;
        let run = wrela_tests::ChromeRun {
            salt,
            nohash: true,
            ..wrela_tests::ChromeRun::new(1, 64, 64, 60.0)
        };
        wrela_tests::run_in_chrome_with(&rel, run);
        let p = wrela_tests::result_json(&dir.join("results"), "pipelines.json");
        ours.push(p["ms"].as_f64().expect("ms"));
        count = p["count"].as_u64().expect("count");
        assert!(wrela_tests::run_page_in_chrome(spike, "#", 120), "the spike's page failed");
        let q =
            wrela_tests::result_json(&repo_root().join(spike).join("results"), "pipelines.json");
        theirs.push(q["ms"].as_f64().expect("ms"));
    }
    let (o, t) = (median(&ours), median(&theirs));
    eprintln!(
        "cold, all at once: the herd's {count} pipelines {o:.0} ms, the spike's 12 {t:.0} ms: {:.2}x",
        o / t
    );
    assert!(o <= 1.5 * t, "the herd's pipelines take {:.2}x the spike's", o / t);
}
