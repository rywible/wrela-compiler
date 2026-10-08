//! The herd (examples/herd) against spike 01 (#42 AC1–AC7). The fixtures are the spike's files,
//! unchanged (compiler/tests/fixtures/spike01): its CPU module (`cpu.wasm`), its kernels and its
//! seed-1 parameters.
//!
//! CPU comparisons run both sides in one harness, the native host's wasmtime, alternating after a
//! warm-up round (M1's method): the spike's `cpu.wasm` and the herd's build.

use crate::built;
use std::path::{Path, PathBuf};
use std::time::Instant;
use wrela_host::{CpuBuild, CpuHost, Host, RunResult, Value};
use wrela_tests::spike01::{HARNESS_D, fixture, fixture_path, harness, params_seed1};
use wrela_tests::{files_under, median, percentile, repo_root};

/// The herd, built once for the suite (`built` builds it under the suite's scratch).
fn herd() -> PathBuf {
    built("../../examples/herd")
}

/// The spike's CPU module, loaded with seed 1's parameters.
struct Spike {
    store: wasmtime::Store<()>,
    instance: wasmtime::Instance,
}

impl Spike {
    fn new() -> Spike {
        let mut config = wasmtime::Config::new();
        // As the native host's engine, so the two are timed alike.
        config.wasm_threads(true).shared_memory(true).native_unwind_info(false);
        let engine = wasmtime::Engine::new(&config).expect("an engine");
        let module =
            wasmtime::Module::from_file(&engine, fixture_path("cpu.wasm")).expect("cpu.wasm");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("instantiate");
        let mut s = Spike { store, instance };
        let ptr = s.call_u32("params_ptr") as usize;
        let memory = s.instance.get_memory(&mut s.store, "memory").expect("memory");
        for (i, x) in params_seed1().iter().enumerate() {
            memory.data_mut(&mut s.store)[ptr + 4 * i..ptr + 4 * i + 4]
                .copy_from_slice(&x.to_le_bytes());
        }
        let load = s.instance.get_typed_func::<(), ()>(&mut s.store, "load").expect("load");
        load.call(&mut s.store, ()).expect("load()");
        s
    }

    fn call_u32(&mut self, name: &str) -> u32 {
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

/// Our `bench_eval` over `n` points in the spike's box (`spike_box`).
fn our_eval(host: &mut CpuHost, (lo, hi): ([f32; 3], [f32; 3]), n: u32, pruned: u32) -> f64 {
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

/// #43 §9: the physique per bone, by the spike's method: the bones' masses sum to the whole,
/// which is `adaptive_mass`'s; every bone's moments of inertia meet the triangle inequality;
/// and each bone over 1 kg is within 5% of the derived interval's method (`integrate`). The two
/// lay their cells on different grids, and a cell where two bones meet counts wholly for the
/// bone nearest its centre, so they differ by a share of a cell at each bone's ends: 20% at
/// 4 cm, 3.6% at 2 cm and 2.4% at 1 cm.
#[test]
fn the_physique_gives_each_bone_its_mass_and_moments() {
    let mut host = ours();
    let r = host.call_export("physique", &[Value::I32(1), Value::F32(0.02)]).expect("physique");
    let [Value::F32(total), Value::F32(sum), Value::I32(bones), Value::F32(least)] = r[..] else {
        panic!("{r:?}")
    };
    let mass = f64_of(&host.call_export("mass", &[Value::I32(1), Value::F32(0.02)]).expect("mass"));
    let differ = f64_of(
        &host
            .call_export("physique_methods_differ", &[Value::I32(1), Value::F32(0.02)])
            .expect("differ"),
    );
    eprintln!(
        "seed 1 at 2 cm: {total:.2} kg in {bones} bones (summed {sum:.2}; adaptive_mass {mass:.2}); \
         moments' triangle margin at least {least:.3}; bones within {:.2}% of integrate's",
        differ * 100.0
    );
    assert!((f64::from(total) - mass).abs() <= 1e-6 * mass, "{total} against {mass}");
    assert!((total - sum).abs() <= 1e-4 * total, "{sum} summed against {total}");
    assert!(bones >= 20, "only {bones} bones have mass");
    assert!(least >= -1e-4, "a bone's moments break the triangle inequality: {least}");
    assert!(differ <= 0.05, "a bone is {:.2}% from integrate's", differ * 100.0);
}

/// AC6's timings on the CPU (WASM), in one harness, alternating: the physique at 2 cm (≤ 1.25×
/// the spike's), the grazer's field per evaluation whole and pruned, and a raycast (each ≤ 1.25×).
#[test]
#[ignore = "measure: a timing run: cargo test -p wrela-tests --test suite herd:: -- --ignored --nocapture"]
fn the_cpu_side_costs_what_the_spikes_did() {
    let mut spike = Spike::new();
    let mut host = ours();
    let mut report = Vec::new();
    // The same points, so the same sums (to rounding).
    let (n, at) = (200_000, spike_box());
    for pruned in [0, 1] {
        let (a, b) = (spike.bench_eval(n, pruned), our_eval(&mut host, at, n, pruned));
        assert!(
            (a - b).abs() <= 1e-4 * a.abs().max(1.0),
            "pruned {pruned}: the sums differ: {a} and {b}"
        );
        let (ts, to) = alternate(
            7,
            || _ = spike.bench_eval(n, pruned),
            || _ = our_eval(&mut host, at, n, pruned),
        );
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
#[ignore = "long: needs a GPU and bun"]
fn the_spike_harness_gives_the_recorded_runs_counts() {
    let recorded: serde_json::Value =
        serde_json::from_str(&fixture("run-2026-10-02T00-00-14-979Z.json")).expect("JSON");
    let spike = wrela_tests::spike01::Spike::new();
    for cell in ["0.03", "0.02", "0.015", "0.01"] {
        let (per, _) = spike.extract_herd(cell.parse().expect("a cell size"), 40);
        let med = |f: &dyn Fn(&wrela_tests::spike01::Extracted) -> u32| {
            median(&per.iter().map(|e| f64::from(f(e))).collect::<Vec<_>>())
        };
        let r = &recorded["extraction"][cell];
        let want = |k: &str| r[k].as_f64().expect(k);
        let ours = [
            ("blocks_median", med(&|e| e.blocks)),
            ("live_blocks_median", med(&|e| e.live)),
            ("verts_median", med(&|e| e.verts)),
            ("tris_median", med(&|e| e.tris)),
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

/// A mesh's buffers, in the order `Mesh::new` makes them: the vertices' count and items, the
/// quads' count and items, the draw's arguments, the skin pass's, the holes and the tally.
const MESH_BUFFERS: usize = 8;

/// A mesh's counts and geometry from its buffers (`MESH_BUFFERS`). The draw's arguments begin
/// with its index count; the tally holds the vertices, holes, flags, vertices and quads pushed,
/// and live blocks.
fn read_mesh(host: &mut wrela_host::Host, mesh: &[u32]) -> Ours {
    let draw = wrela_tests::u32s(&host.read_buffer(mesh[4]).expect("the draw's arguments"));
    let tally = wrela_tests::u32s(&host.read_buffer(mesh[7]).expect("the tally"));
    let (nv, ni) = (tally[0] as usize, draw[0] as usize);
    let verts = wrela_tests::positions(&host.read_buffer(mesh[1]).expect("vertices"), nv);
    let indices = wrela_tests::u32s(&host.read_buffer(mesh[3]).expect("quads"));
    Ours {
        live: tally[5],
        tris: draw[0] / 3,
        holes: tally[1],
        flags: tally[2],
        mesh: Tris::new(verts, &indices[..ni]),
    }
}

/// A `Realizer` spreads a level change over frames (`record_vertices` a slice at a time): the
/// mesh is the one a whole realization makes, triangle for triangle, at 3 and 1.5 cm, in 1, 5
/// and 40 slices (40: most are past the live blocks' end, and empty).
#[test]
#[ignore = "needs a GPU"]
fn a_realization_in_slices_is_the_whole_ones() {
    let dir = crate::built("herd-realize");
    let mut host = wrela_host::Host::load(&dir).expect("load");
    let newest = |host: &mut wrela_host::Host| {
        let b = host.buffers();
        b[b.len() - MESH_BUFFERS..].to_vec()
    };
    // Each triangle as its corners' bits, from its lowest corner on (so the vertices' order in
    // the buffer, which slices change, doesn't matter, but the winding does).
    let triangles = |o: &Ours| {
        let mut t: Vec<[[u32; 3]; 3]> = o
            .mesh
            .tris
            .iter()
            .map(|&t| {
                let c = o.mesh.corners(t).map(|p| p.map(f32::to_bits));
                let k = (0..3).min_by_key(|&k| c[k]).expect("a corner");
                [c[k], c[(k + 1) % 3], c[(k + 2) % 3]]
            })
            .collect();
        t.sort();
        t
    };
    for cell in [0.03f32, 0.015] {
        host.call_export("realize", &[Value::I32(1), Value::F32(cell)]).expect("realize");
        let five = newest(&mut host);
        let whole = read_mesh(&mut host, &five);
        let want = triangles(&whole);
        assert!(whole.tris > 1000 && whole.holes == 0 && whole.flags == 0);
        for slices in [1, 5, 40] {
            host.call_export(
                "realize_sliced",
                &[Value::I32(1), Value::F32(cell), Value::I32(slices)],
            )
            .expect("realize_sliced");
            let five = newest(&mut host);
            let sliced = read_mesh(&mut host, &five);
            let what = format!("{cell} m in {slices} slices");
            assert_eq!(
                (sliced.live, sliced.tris, sliced.holes, sliced.flags),
                (whole.live, whole.tris, whole.holes, whole.flags),
                "{what}"
            );
            assert!(triangles(&sliced) == want, "{what}: the triangles differ");
        }
        eprintln!("{cell} m: {} triangles, the same in 1, 5 and 40 slices", whole.tris);
    }
}

/// Ours at `cell`, as the spike's harness runs it: the room calibrated on the largest grid (a
/// quarter more, and 1,024 to spare), then the 40 back to back, twice; the second run's counts
/// and meshes, and each grazer's GPU time (its five dispatches), ms.
fn realize_herd(host: &mut wrela_host::Host, cell: f32) -> (Vec<Ours>, Vec<f64>) {
    let blocks = |host: &mut wrela_host::Host, s: u32| {
        let g = host.call_export("grid", &[Value::I32(s as i32), Value::F32(cell)]).expect("grid");
        let [Value::I32(x), Value::I32(y), Value::I32(z), _] = g[..] else { panic!("{g:?}") };
        (x * y * z) as u32
    };
    let largest = (1..=40).max_by_key(|&s| blocks(host, s)).expect("a seed");
    host.call_export("realize", &[Value::I32(largest as i32), Value::F32(cell)]).expect("realize");
    let b = host.buffers();
    let tally = wrela_tests::u32s(&host.read_buffer(b[b.len() - 1]).expect("the tally"));
    let (v, q) = (tally[3], tally[4]);
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
    let meshes = &b[b.len() - 40 * MESH_BUFFERS..];
    let ours = meshes.chunks_exact(MESH_BUFFERS).map(|mesh| read_mesh(host, mesh)).collect();
    (ours, times)
}

/// AC2: over the 40 grazers at 3, 2, 1.5 and 1 cm cells, on the fixture's own grids, ours
/// against the spike's extraction, run beside it in one process, alternating (the spike's
/// harness runs its batch three times and keeps the last).
#[test]
#[ignore = "measure: needs a GPU and bun"]
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
            their_ms.push(median(&t.iter().map(|e| e.gpu_ms).collect::<Vec<_>>()));
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

/// The herd's grazer, seed 1, at the `n` points herd-realize's export `export` (`evaluate` or
/// `near_surface`) samples, the newest three buffers it holds: the points (vec4s, as bytes), its
/// distances and its samples (vec4s: the distance, then the gradient or the channels); and the
/// command batches it recorded, if `record`.
fn grazer_points(
    export: &str,
    n: u32,
    record: bool,
) -> (Vec<u8>, Vec<f32>, Vec<f32>, Vec<Vec<u8>>) {
    use wrela_tests::f32s;
    let options = wrela_host::Options { record, ..wrela_host::Options::default() };
    let mut host = Host::load_with(built("herd-realize"), &options).expect("load");
    host.call_export(export, &[Value::I32(1), Value::I32(n as i32)]).expect(export);
    let made = host.buffers();
    let [.., p, d, s] = made[..] else { panic!("{export} made {made:?}") };
    let points = host.read_buffer(p).expect("points");
    let dist = f32s(&host.read_buffer(d).expect("distances"));
    let samp = f32s(&host.read_buffer(s).expect("samples"));
    (points, dist, samp, host.take_batches())
}

/// The herd's grazer, seed 1, against field.wgsl with the seed-1 parameters, at 2²⁰ points over
/// its bounds: distances and gradients (every part in, unfiltered), and GPU time per evaluation
/// in one harness, alternating.
#[test]
#[ignore = "measure: needs a GPU"]
fn the_herds_grazer_field_costs_what_field_wgsl_does() {
    use wrela_tests::spike01::compiled_kernel;
    use wrela_tests::{Bind, RawGpu, f32s};
    let n: u32 = 1 << 20;
    let (points, dist, samp, batches) = grazer_points("evaluate", n, true);
    let field = fixture("field.wgsl");
    let gpu = RawGpu::new().expect("a GPU");
    let params = std::fs::read(fixture_path("params-seed1.f32")).expect("params-seed1.f32");
    let run = |wgsl: &str, entry: &str, uniform: &[u8], out: u64| {
        let first = if wgsl.contains("var<uniform> G") {
            Bind::Uniform(uniform)
        } else {
            Bind::Read(uniform)
        };
        gpu.run(wgsl, entry, &[first, Bind::Read(&points), Bind::Write(out)], n / 64, 10)
    };
    let hand_d = format!("{field}{HARNESS_D}");
    let hand_g = format!("{field}{}", harness("eval_g", "vec4f(s.d, s.g)"));
    let (_, hd) = run(&hand_d, "eval_d", &params, u64::from(n) * 4);
    let (_, hg) = run(&hand_g, "eval_g", &params, u64::from(n) * 16);
    let (hd, hg) = (f32s(&hd[0]), f32s(&hg[0]));
    let dir = built("herd-realize");
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

/// The script that asks the herd for a still frame at a level (`key`: Digit1 3 cm, Digit3
/// 1.5 cm, Digit4 LOD's), from the close-up's camera or the herd's, with the terrain or without
/// (KeyT).
fn still_script(key: &str, close: bool, terrain: bool) -> String {
    let mut keys = vec![key];
    keys.extend(close.then_some("KeyC"));
    keys.extend((!terrain).then_some("KeyT"));
    let events: Vec<String> =
        keys.iter().map(|k| format!(r#"{{"frame":0,"type":"key","key":"{k}"}}"#)).collect();
    format!("[{}]", events.join(","))
}

/// The herd's first `frames` frames at 60 a second, `width` × `height`, in the native host with
/// `script` (a [`still_script`]), each pass timed if `timestamps`: the host after them, and the
/// run.
fn still(
    script: &str,
    frames: u32,
    (width, height): (u32, u32),
    timestamps: bool,
) -> (Host, RunResult) {
    let times: Vec<f32> = (0..frames).map(|i| wrela_host::frame_time(i, 60.0)).collect();
    let script = wrela_host::parse_script(script).expect("a script");
    let options = wrela_host::Options { timestamps, ..wrela_host::Options::default() };
    let mut host = Host::load_with(herd(), &options).expect("load the herd");
    let run = host.run_frames_with(&times, width, height, &script).expect("run the herd");
    (host, run)
}

/// AC1's parity frames: the herd draws spike 01's placement with each grazer's phase at its own
/// plus 0.9 t, at t = 1 s, every grazer realized at 3 cm and at 1.5 cm, from the herd's camera
/// and the close-up's. Each frame matches the spike's (its kernels, run by the harness) within a
/// mean of 0.5/255, in the native host and in Chrome; and the two hosts' frames match each
/// other's within 0.5/255.
#[test]
#[ignore = "long: needs Chrome, python3, a GPU and bun"]
fn the_herd_draws_the_spikes_frames() {
    use wrela_tests::spike01::{H, SceneKind, W, image_difference};
    let (dir, rel) = wrela_tests::page("examples/herd", "herd-parity");
    let t = f64::from(STILL_FRAME) / 60.0;
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
        let script = still_script(key, close, true);
        let (_, native) = still(&script, STILL_FRAME + 1, (W, H), false);
        let name = format!("{}-{key}", if close { "close" } else { "herd" });
        native.write_png(dir.join(format!("{name}-native.png"))).expect("png");
        wrela_host::image::write_png(&dir.join(format!("{name}-spike.png")), W, H, theirs)
            .expect("png");
        let run = wrela_tests::ChromeRun {
            script: Some(script),
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
#[ignore = "measure: needs a GPU and bun"]
fn drawing_the_herd_costs_what_the_spikes_did() {
    use wrela_tests::spike01::{H, SceneKind, W};
    let mut spike = wrela_tests::spike01::Spike::new();
    let frames = STILL_FRAME + 1;
    // Realization settles in the first frames; the last 30 are timed.
    let from = (frames - 30) as usize;
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
                let (_, run) = still(&still_script(key, close, terrain), frames, (W, H), true);
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

/// A paced run of the live herd in Chrome at 1080p, 60 Hz, 600 frames, seven helpers, with
/// `script`'s keys: the run, and each frame's GPU time (ms), its passes' and dispatches' own
/// times summed. Not from the first start to the last end: paced, the GPU's clocks fall and rise
/// (the same frame took 3.8 ms, then 14.5, then 5.6), and Chrome leaves gaps between a frame's
/// passes that grow as they fall (up to 4 ms), which the native host doesn't.
fn paced_herd(name: &str, script: &str) -> (PathBuf, wrela_tests::BrowserRun, Vec<f64>) {
    let (dir, rel) = wrela_tests::page("examples/herd", name);
    let run = wrela_tests::ChromeRun {
        script: Some(script.into()),
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
/// all 40 grazers, the bytes downloaded before it, and the memory: the program's GPU buffers and
/// textures at most, and its WASM memory used and reserved.
#[test]
#[ignore = "measure: needs Chrome, python3 and a GPU"]
fn the_herd_keeps_its_frames_in_chrome() {
    let (dir, chrome, ms) = paced_herd("herd-paced", "[]");
    let missed = ms.iter().filter(|&&m| m > 16.7).count();
    let worst = ms.iter().copied().fold(0.0, f64::max);
    let pipelines = wrela_tests::manifest(&dir).pipelines.len();
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
    let ticks = &chrome.ticks.as_ref().expect("the sim's ticks").cpu_ms;
    let p99 = percentile(ticks, 0.99);
    let memory = wrela_tests::result_json(&results, "memory.json");
    let mib = |v: &serde_json::Value| v.as_f64().expect("bytes") / 1048576.0;
    eprintln!(
        "paced: {missed} of {} frames over 16.7 ms (worst {worst:.2} ms, median {:.2}); every grazer drawn within {drawn:.0} ms and at its level within {leveled:.0} ms; {} ticks, 99th percentile {p99:.2} ms; {pipelines} pipelines (the spike's 8); all 40 drawn {to_all:.0} ms after the page opened, {:.2} MB downloaded before; GPU buffers and textures {:.1} MiB at most, WASM memory {:.1} MiB used of {:.0} reserved",
        ms.len(),
        median(&ms),
        ticks.len(),
        bytes / 1e6,
        mib(&memory["gpu"]["peak"]),
        mib(&memory["wasm"]["used"]),
        mib(&memory["wasm"]["reserved"]),
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
/// the herd's grazers are drawn and refined for the herd's view. (The others, off screen in the
/// close-up, were realized at their coarsest level meanwhile, one a frame: `Realizer::spend`.)
#[test]
#[ignore = "measure: needs Chrome, python3 and a GPU"]
fn moving_to_the_herd_keeps_the_frames_in_chrome() {
    let script =
        r#"[{"frame":0,"type":"key","key":"KeyC"},{"frame":300,"type":"key","key":"KeyH"}]"#;
    let (_, chrome, ms) = paced_herd("herd-move", script);
    let after: Vec<f64> = ms[299..].to_vec();
    let missed = after.iter().filter(|&&m| m > 16.7).count();
    let worst = after.iter().copied().fold(0.0, f64::max);
    let drawn = chrome.printed.iter().filter(|(f, l)| *f >= 300 && l.ends_with(" drawn")).count();
    let all = chrome.printed.iter().any(|(f, l)| *f >= 300 && l.starts_with("all 40"));
    eprintln!(
        "close-up to herd: {missed} of {} frames over 16.7 ms (worst {worst:.2} ms, median {:.2}); {drawn} grazers first drawn after the move",
        after.len(),
        median(&after)
    );
    assert!(all, "the herd's 40 grazers weren't all drawn after the move");
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
    let dir = herd();
    let options = wrela_host::Options { record: true, ..wrela_host::Options::default() };
    let mut host = wrela_host::Host::load_with(&dir, &options).expect("load the herd");
    let script = wrela_host::parse_script(
        r#"[{"frame":100,"type":"key","key":"KeyC"},{"frame":160,"type":"key","key":"KeyH"}]"#,
    )
    .expect("a script");
    let times: Vec<f32> = (0..240).map(|i| wrela_host::frame_time(i, 60.0)).collect();
    host.run_frames_with(&times, 960, 540, &script).expect("run the herd");
    let r = host.call_export("realized", &[]).expect("realized");
    let [Value::I32(realizations), Value::I32(readbacks), _, _] = r[..] else { panic!("{r:?}") };
    let manifest = wrela_tests::manifest(&dir);
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
    assert_eq!(reads as i32, readbacks, "readbacks the realizer doesn't count");
    assert_eq!(culls as i32, realizations, "realizations the realizer doesn't count");
}

/// AC2: shared corners agree. In a debug build each live block writes the value of each
/// boundary corner it evaluates (`Scratch::probe`, the sixth newest buffer after
/// `probe_corners`); over the 40 grazers at 3, 2, 1.5 and 1 cm,
/// every corner has one bit pattern, whichever blocks evaluate it (#43 §7.5).
#[test]
#[ignore = "long: needs a GPU"]
fn shared_corners_have_one_value() {
    let mut host = Host::load(crate::built_debug("herd-realize")).expect("load");
    let (mut corners, mut shared, mut values) = (0u64, 0u64, 0u64);
    for cell in [0.03f32, 0.02, 0.015, 0.01] {
        for s in 1..=40 {
            host.call_export("probe_corners", &[Value::I32(s), Value::F32(cell)]).expect("probe");
            let b = host.buffers();
            let at = b.len() - MESH_BUFFERS - 1;
            let words = wrela_tests::u32s(&host.read_buffer(b[at]).expect("the probe"));
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
#[ignore = "measure: needs a GPU and bun"]
fn lod_looks_like_the_finest_and_costs_like_the_coarsest() {
    use wrela_tests::spike01::{H, SceneKind, W, image_difference};
    let frames = 241;
    // A still frame, and each grazer's level, which must be the one LOD gives it.
    let leveled = |key: &str, terrain: bool, timed: bool| {
        let (mut host, run) = still(&still_script(key, false, terrain), frames, (W, H), timed);
        let levels: Vec<u32> = (1..=40)
            .map(|s| {
                let v = host.call_export("level", &[Value::I32(s)]).expect("level");
                let [Value::I32(drawn), Value::I32(target)] = v[..] else { panic!("{v:?}") };
                assert_eq!(drawn, target, "grazer {s} hasn't its level after {frames} frames");
                drawn as u32
            })
            .collect();
        (run, levels)
    };
    let (lod, levels) = leveled("Digit4", true, false);
    let (finest, _) = leveled("Digit3", true, false);
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
            let (run, _) = leveled("Digit4", false, true);
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
#[ignore = "long: needs a GPU and bun"]
fn mesh_memory_is_the_spikes() {
    let mut ours = Vec::new();
    for key in ["Digit1", "Digit3"] {
        let (mut host, _) =
            still(&still_script(key, false, true), STILL_FRAME + 1, (960, 540), false);
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
/// round it, from 30 m out to 4), at 60 frames a second in the native host (a level change takes
/// a slice of frames, `Realizer::advance`, so the frame rate is the game's): no slab overflows,
/// no grazer once drawn is ever without a mesh, and the mesh memory allocated after the tour is
/// within one slab per level of its peak during the first loop.
#[test]
#[ignore = "long: needs a GPU"]
fn a_ten_minute_tour_leaks_no_mesh_memory() {
    let mut host = Host::load(herd()).expect("load the herd");
    let fps = 60.0;
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
            let [Value::I32(drawn), _] = v[..] else { panic!("{v:?}") };
            let has = (drawn as u32) < 4;
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
    let [Value::I32(realizations), _, Value::I32(holes), Value::I32(overflows)] = r[..] else {
        panic!("{r:?}")
    };
    eprintln!(
        "tour: {realizations} realizations, {overflows} overflows, {holes} holes, {lost} meshes lost; mesh memory peaked at {first_peak:.1} MiB in the first loop, {peak:.1} in all, {after:.1} after (a slab per level: {slabs:.2} MiB)"
    );
    assert_eq!(overflows, 0, "slabs overflowed");
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
#[ignore = "measure: needs Chrome, python3 and a GPU"]
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

// ---- AC5: one hash sequence everywhere ------------------------------------------------------------

/// AC5: the herd's world, over 10,000 ticks, has one sequence of per-tick hashes in every host and
/// configuration: the native host without the GPU (`--no-gpu`) with 0, 1 and 7 helpers, and with
/// its jobs (the grazers' physiques) slowed tenfold, which makes the sim wait (AC6); the native
/// host with the GPU, ticking in lockstep with its frames; the native host built for x86-64, under
/// Rosetta, replaying the log; and Chrome on the lockstep schedule at 30, 60 and 144 frames a
/// second, and at 60 with each frame held 20 ms longer. The variations (the slowed jobs; Chrome
/// at 30 and 144, and with its frames held) run the first 1,000 ticks, past the jobs the herd
/// starts with; all 10,000 at full size. Chrome's frames run back to back, not at their times:
/// in lockstep, a frame's ticks follow its number, not the clock, so the schedule is a paced
/// run's.
#[test]
#[ignore = "long: needs Chrome, python3, a GPU and Rosetta"]
fn the_herds_ticks_hash_the_same_everywhere() {
    const TICKS: u32 = 10_000;
    let part = wrela_tests::sized(1_000, TICKS);
    let (dir, rel) = wrela_tests::page("examples/herd", "herd-hashes");
    let hashes =
        |log: &wrela_host::TickLog| -> Vec<u64> { log.ticks.iter().map(|t| t.hash).collect() };
    let built = CpuBuild::load(&dir).expect("load");
    let log = built.record_ticks(TICKS, &[], 1).expect("no helpers");
    let reference = hashes(&log);
    for workers in [2, 8] {
        let other = built.record_ticks(TICKS, &[], workers).expect("helpers");
        assert_eq!(hashes(&other), reference, "{} helpers", workers - 1);
    }
    eprintln!("no GPU: 0, 1 and 7 helpers agree over {TICKS} ticks");
    // Jobs slowed tenfold (each physique held 0.36 s after it's done, one helper): the sim waits
    // for the answers due, logs that it did, and the hashes don't change.
    let mut slow = built.instantiate_in(2, None).expect("instantiate");
    slow.hold_jobs(360_000);
    slow.want_hashes(true);
    slow.init().expect("init");
    let slowed: Vec<u64> =
        (0..part).map(|_| slow.tick().expect("a tick").hash.expect("a hash")).collect();
    let waits = slow.take_logs().into_iter().filter(|l| l.contains("waited for a job")).count();
    assert_eq!(slowed, reference[..part as usize], "jobs slowed tenfold");
    assert!(waits > 0, "the slowed run never waited for a job");
    eprintln!("jobs slowed tenfold agree over {part} ticks; the sim waited at {waits} ticks");
    // With the GPU, in lockstep with 60 frames a second. The host holds the GPU lock until it's
    // dropped, which it must be before Chrome waits for the lock.
    let gpu = {
        let options = wrela_host::Options { defer_init: true, ..wrela_host::Options::default() };
        let mut host = Host::load_with(&dir, &options).expect("load");
        host.want_hashes(true);
        host.init().expect("init");
        let (_, glog) = host.run_lockstep(TICKS, 60.0, 64, 64, &[], true).expect("run");
        hashes(&glog.expect("a tick log"))
    };
    assert_eq!(gpu[..], reference[..gpu.len()], "the native host with the GPU");
    assert!(gpu.len() >= TICKS as usize - 1, "only {} ticks with the GPU", gpu.len());
    eprintln!("the native host with the GPU agrees over {} ticks", gpu.len());
    // x86-64 under Rosetta: the log replays.
    let path = dir.join("herd.ticks");
    std::fs::write(&path, log.encode()).expect("write the log");
    let (ok, text) = crate::replay_on_x86(&crate::x86_host(), &path, &dir);
    assert!(ok, "x86-64 didn't replay the herd's log: {text}");
    assert!(text.contains(&format!("{TICKS} ticks replayed")), "{text}");
    eprintln!("x86-64 under Rosetta replays {TICKS} ticks");
    for (fps, delay) in [(60.0, 0), (30.0, 0), (144.0, 0), (60.0, 20)] {
        let n = if fps == 60.0 && delay == 0 { TICKS } else { part };
        let frames = (f64::from(n) * fps / 60.0).ceil() as u32 + 1;
        let run = wrela_tests::ChromeRun {
            workers: 8,
            framedelay: delay,
            saturate: true,
            ..wrela_tests::ChromeRun::new(frames, 64, 64, fps)
        };
        let chrome = wrela_tests::run_in_chrome_with(&rel, run);
        let ticks = hashes(&chrome.ticks.and_then(|t| t.log).expect("the sim's tick log"));
        let what = format!(
            "Chrome at {fps} fps{}",
            if delay > 0 { ", each frame 20 ms longer" } else { "" }
        );
        assert!(ticks.len() >= n as usize, "{what}: {} ticks", ticks.len());
        assert_eq!(ticks[..n as usize], reference[..n as usize], "{what}");
        eprintln!("{what} agrees over {n} ticks");
    }
}

/// What one failed try at a lock costs, in nanoseconds, while another core holds it: one
/// thread holds an atomic word for 20 ms while this one spins on it with compare-and-swap, as
/// std's allocator does. It turns the allocator's spins into time.
fn spin_ns() -> f64 {
    use std::sync::atomic::{AtomicU32, Ordering};
    let word = AtomicU32::new(1);
    let mut best = f64::MAX;
    for _ in 0..5 {
        word.store(1, Ordering::SeqCst);
        let (spins, took) = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(20));
                word.store(0, Ordering::SeqCst);
            });
            let start = Instant::now();
            let mut spins = 0u64;
            while word.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_err() {
                spins += 1;
            }
            (spins, start.elapsed().as_secs_f64())
        });
        best = best.min(took * 1e9 / spins as f64);
    }
    best
}

/// AC5, measured: with the herd's frames drawing beside them (the native host, the ticks on a
/// thread of their own, neither waiting for the other), how many blocks a steady tick allocates,
/// and how long it loses waiting on the allocator's lock (std::alloc counts the times it found
/// the lock taken and the tries it spun; a try's cost is [`spin_ns`]'s). A steady tick reuses
/// its buffers (#43 §5.2), so it allocates nothing: that's asserted.
#[test]
#[ignore = "measure: the ticks' lock waits and times, beside 1,500 frames at 1080p; needs a GPU"]
fn the_herds_ticks_beside_frames_allocate_and_wait() {
    let dir = herd();
    let options = wrela_host::Options { defer_init: true, ..wrela_host::Options::default() };
    let mut host = wrela_host::Host::load_with(&dir, &options).expect("load");
    host.init().expect("init");
    let ticked = host.ticks_beside_frames(3_000, 1_500, 60.0, 1920, 1080).expect("run");
    // Steady: after every physique has arrived (due a second after the spawn).
    let steady = &ticked[120..];
    let allocs: Vec<f64> = steady.iter().map(|(t, _)| f64::from(t.allocations)).collect();
    let most = allocs.iter().copied().fold(0.0, f64::max);
    let waits: u64 = steady.iter().map(|(t, _)| u64::from(t.lock_waits)).sum();
    let spins: u64 = steady.iter().map(|(t, _)| u64::from(t.lock_spins)).sum();
    let times: Vec<f64> = steady.iter().map(|(_, ms)| *ms).collect();
    let ns = spin_ns();
    let n = steady.len() as f64;
    eprintln!(
        "{} steady ticks beside frames: {:.0} blocks allocated a tick (median; {:.0} at most), \
         the allocator's lock found taken {waits} times ({spins} tries, {:.1} ns each): \
         {:.1} ns a tick lost to it on average; ticks took {:.2} ms (median), {:.2} ms (p99)",
        steady.len(),
        median(&allocs),
        most,
        ns,
        spins as f64 * ns / n,
        median(&times),
        percentile(&times, 0.99),
    );
    assert_eq!(most, 0.0, "a steady tick allocated");
}

// ---- AC7: the herd's grazer against the spike's ---------------------------------------------------

/// AC7: at every vertex of the spike's 1.5 cm mesh of seed 1, the herd's skin weights (the
/// creature API's skin bindings, `Parts::skin_at` under the vertex's mask, a cell wide, falling
/// off over 6 cm) pick the same four bones, each weight within 1/255.
#[test]
#[ignore = "needs a GPU and bun"]
fn the_herds_skin_weights_are_the_spikes() {
    let (vertices, cell) = {
        let spike = wrela_tests::spike01::Spike::new();
        let (_, inds) = spike.extract_herd(0.015, 1);
        (spike.vertices(&inds[0]), 0.015f32)
    };
    let mut host = ours();
    let (mut bones_off, mut weight_off, mut worst) = (0, 0, 0u8);
    for v in &vertices {
        let [x, y, z] = [0, 1, 2].map(|k| f32::from_bits(v[k]));
        let r = host
            .call_export(
                "skin_at",
                &[
                    Value::I32(1),
                    Value::F32(x),
                    Value::F32(y),
                    Value::F32(z),
                    Value::I32(v[8] as i32),
                    Value::I32(0),
                    Value::F32(cell),
                ],
            )
            .expect("skin_at");
        let [Value::I32(b), Value::I32(w)] = r[..] else { panic!("{r:?}") };
        let (ours_b, ours_w) = (b.to_le_bytes(), w.to_le_bytes());
        let (theirs_b, theirs_w) = (v[3].to_le_bytes(), v[7].to_le_bytes());
        // The same bones with the same weights, whatever their order, where weights are equal.
        let mut a: Vec<(u8, u8)> =
            (0..4).filter(|&k| ours_w[k] > 0).map(|k| (ours_b[k], ours_w[k])).collect();
        let mut t: Vec<(u8, u8)> =
            (0..4).filter(|&k| theirs_w[k] > 0).map(|k| (theirs_b[k], theirs_w[k])).collect();
        a.sort();
        t.sort();
        let same_bones = a.len() == t.len() && a.iter().zip(&t).all(|(p, q)| p.0 == q.0);
        if !same_bones {
            bones_off += 1;
            continue;
        }
        let off = a.iter().zip(&t).map(|(p, q)| p.1.abs_diff(q.1)).max().unwrap_or(0);
        worst = worst.max(off);
        weight_off += usize::from(off > 1);
    }
    eprintln!(
        "{} vertices: {bones_off} with other bones, {weight_off} with a weight over 1/255 off (the most {worst}/255)",
        vertices.len()
    );
    assert!(vertices.len() > 10_000, "only {} vertices", vertices.len());
    assert_eq!(bones_off, 0, "vertices whose bones differ");
    assert_eq!(weight_off, 0, "vertices whose weights differ by more than 1/255");
}

/// AC7: the herd's grazer, seed 1, matches field.wgsl with the seed-1 parameters at 4,096 points
/// from `Box3::sample` within 2 cm of its surface, on the GPU: distances within 1e-6 m, channels
/// (torso-ness, hoof-ness) within 1e-5.
#[test]
#[ignore = "needs a GPU"]
fn the_herds_grazer_is_field_wgsls_near_the_surface() {
    use wrela_tests::{Bind, RawGpu, f32s};
    let n: u32 = 4096;
    let (points, dist, samp, _) = grazer_points("near_surface", n, false);
    let field = fixture("field.wgsl");
    let params = std::fs::read(fixture_path("params-seed1.f32")).expect("params-seed1.f32");
    let gpu = RawGpu::new().expect("a GPU");
    let run = |wgsl: String, entry: &str, out: u64| {
        let (_, o) = gpu.run(
            &wgsl,
            entry,
            &[Bind::Uniform(&params), Bind::Read(&points), Bind::Write(out)],
            n / 64,
            1,
        );
        f32s(&o[0])
    };
    let hd = run(format!("{field}{HARNESS_D}"), "eval_d", u64::from(n) * 4);
    let hand_c = format!("{field}{}", harness("eval_c", "vec4f(s.d, s.ch, 0.0)"));
    let hc = run(hand_c, "eval_c", u64::from(n) * 16);
    let mut worst_d = 0.0f32;
    let mut worst_c = 0.0f32;
    for i in 0..n as usize {
        worst_d = worst_d.max((dist[i] - hd[i]).abs()).max((samp[4 * i] - hc[4 * i]).abs());
        worst_c = worst_c
            .max((samp[4 * i + 1] - hc[4 * i + 1]).abs())
            .max((samp[4 * i + 2] - hc[4 * i + 2]).abs());
    }
    eprintln!(
        "{n} points within 2 cm of the surface: distances within {worst_d:.2e} m, channels within {worst_c:.2e}"
    );
    assert!(worst_d <= 1e-6, "a distance {worst_d:.2e} m off");
    assert!(worst_c <= 1e-5, "a channel {worst_c:.2e} off");
}

/// The `.wrela` files of `dir` ([`files_under`] it), each with its text.
fn wrela_files(dir: &Path) -> Vec<(PathBuf, String)> {
    let with_text = |p: PathBuf| {
        let text = std::fs::read_to_string(&p).expect("read");
        (p, text)
    };
    files_under(dir, &["wrela"]).into_iter().map(with_text).collect()
}

/// A file's text without its comments (`//` to the line's end; wrela has no others).
fn code_of(text: &str) -> String {
    text.lines().map(|l| l.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("\n")
}

/// AC7, layering: the engine doesn't depend on the herd, and no code in it names the grazer or
/// the herd (its comments may cite them); neither uses `unsafe`; and the compiler's crates name
/// nothing of the engine (the `wrela` command's tools, which find an `engine/` beside a parts
/// file, aren't the compiler). And AC9's "no ratio is met by hand": the engine and the herd are
/// wrela sources and their manifests alone, with no WGSL or JS of their own.
#[test]
fn the_engine_and_the_herd_keep_their_layers() {
    let root = repo_root();
    for dir in ["engine", "examples/herd"] {
        let listed = std::process::Command::new("git")
            .args(["ls-files", dir])
            .current_dir(&root)
            .output()
            .expect("git ls-files");
        for file in String::from_utf8_lossy(&listed.stdout).lines() {
            assert!(
                file.ends_with(".wrela") || file.ends_with("/wrela.toml"),
                "{file} isn't a wrela source or a manifest"
            );
        }
    }
    let manifest =
        std::fs::read_to_string(root.join("engine/wrela.toml")).expect("engine/wrela.toml");
    assert!(!manifest.contains("herd"), "the engine depends on the herd: {manifest}");
    for (path, text) in wrela_files(&root.join("engine")) {
        let code = code_of(&text);
        for word in ["grazer", "Grazer", "herd", "Herd", "unsafe"] {
            assert!(!code.contains(word), "{} has `{word}` in its code", path.display());
        }
    }
    for (path, text) in wrela_files(&root.join("examples/herd")) {
        assert!(!code_of(&text).contains("unsafe"), "{} uses `unsafe`", path.display());
    }
    for krate in ["syntax", "sema", "lower", "ir", "wasm", "wgsl", "driver", "diag"] {
        for p in files_under(&root.join("compiler").join(krate).join("src"), &["rs"]) {
            let text = std::fs::read_to_string(&p).expect("read");
            assert!(
                !text.contains("engine::") && !text.contains("\"engine\""),
                "{} names the engine",
                p.display()
            );
        }
    }
}

/// AC6: frames don't wait for ticks. Paced in Chrome, 600 frames of the live herd at 640×360,
/// with each tick held 30 ms (two ticks' time): the render worker's frame intervals keep their
/// median and 99th percentile within 1 ms of a run with normal ticks. (Each frame reads the
/// latest complete snapshot: threads::a_hand_off_is_never_read_torn.)
#[test]
#[ignore = "measure: needs Chrome, python3 and a GPU"]
fn frames_dont_wait_for_slow_ticks() {
    let (_, rel) = wrela_tests::page("examples/herd", "herd-slow-ticks");
    let intervals = |tickdelay: u32| {
        let run = wrela_tests::ChromeRun {
            workers: 8,
            paced: true,
            nohash: true,
            tickdelay,
            ..wrela_tests::ChromeRun::new(600, 640, 360, 60.0)
        };
        let chrome = wrela_tests::run_in_chrome_with(&rel, run);
        let ticks = chrome.ticks.as_ref().map_or(0, |t| t.cpu_ms.len());
        let d: Vec<f64> = chrome.began_ms.windows(2).skip(30).map(|w| w[1] - w[0]).collect();
        (median(&d), percentile(&d, 0.99), ticks)
    };
    let (m0, p0, t0) = intervals(0);
    let (m1, p1, t1) = intervals(30);
    eprintln!(
        "frame intervals: normal ticks median {m0:.2} ms, 99th percentile {p0:.2} ({t0} ticks); ticks held 30 ms median {m1:.2}, 99th {p1:.2} ({t1} ticks)"
    );
    assert!(t1 < t0, "the held ticks weren't slower: {t1} ticks against {t0}");
    assert!((m1 - m0).abs() <= 1.0, "the median interval moved {:.2} ms", m1 - m0);
    assert!((p1 - p0).abs() <= 1.0, "the 99th percentile moved {:.2} ms", p1 - p0);
}
