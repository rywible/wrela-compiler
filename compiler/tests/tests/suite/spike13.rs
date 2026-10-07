//! Spike 13's program (compiler/tests/spike13, from the tag spike-13-field-math) builds, and
//! the failures the spike found are fixed (AC5). Plantinga & Vegter's certificate, from the
//! derived bounds: a box holds no surface (0 ∉ □f), or any two gradients in it are less than
//! 90° apart (0 ∉ ⟨□∇f, □∇f⟩), so the surface crosses it as one sheet; else it's split.

use crate::built;
use wrela_host::{CpuBuild, CpuHost, Value};
use wrela_tests::{one_f32, one_u32};

/// An axis-aligned box.
#[derive(Clone, Copy, Debug)]
struct Aabb {
    lo: [f32; 3],
    hi: [f32; 3],
}

impl Aabb {
    fn cube(c: [f32; 3], half: f32) -> Aabb {
        Aabb { lo: c.map(|x| x - half), hi: c.map(|x| x + half) }
    }

    fn split(&self) -> [Aabb; 8] {
        let m: [f32; 3] = std::array::from_fn(|a| 0.5 * (self.lo[a] + self.hi[a]));
        std::array::from_fn(|k| {
            let mut b = *self;
            for (a, &mid) in m.iter().enumerate() {
                if (k >> a) & 1 == 0 {
                    b.hi[a] = mid;
                } else {
                    b.lo[a] = mid;
                }
            }
            b
        })
    }

    fn centre(&self) -> [f32; 3] {
        std::array::from_fn(|a| 0.5 * (self.lo[a] + self.hi[a]))
    }
}

fn host() -> CpuHost {
    CpuBuild::load(built("spike13")).expect("load").start_with(1).expect("start")
}

fn pv_class(host: &mut CpuHost, fi: u32, b: &Aabb) -> u32 {
    let mut args = vec![Value::I32(fi as i32)];
    args.extend(b.lo.iter().chain(&b.hi).map(|&x| Value::F32(x)));
    one_u32(host, "pv_class", &args)
}

/// An octree to `depth`: how many boxes are certified, and the ones still open at the end.
fn certify(host: &mut CpuHost, fi: u32, root: Aabb, depth: usize) -> (u64, Vec<Aabb>) {
    let (mut certified, mut open) = (0, Vec::new());
    let mut level = vec![root];
    for d in 0..=depth {
        let mut next = Vec::new();
        for b in level {
            match pv_class(host, fi, &b) {
                0 => {}
                1 => certified += 1,
                _ if d == depth => open.push(b),
                _ => next.extend(b.split()),
            }
        }
        level = next;
    }
    (certified, open)
}

/// The spike's cube around its creatures.
fn creature_cube() -> Aabb {
    Aabb::cube([0.2, 0.225, 0.0], 1.0)
}

/// std's `smin` chooses once, with one branch, so a blend's seam certifies: the smooth
/// creature (two smooth unions) certifies to 7.8 mm leaves wherever its surface is. (The spike
/// found std's earlier `smin`, which chose twice, with `min` and `abs`, left the seams open; its
/// own `smin_if` certified at 3.9 mm.) No box is left open: std's ellipsoid has no zero inside
/// the body (#28 §10.4). Its bound divided k0 (k0 − 1) by a length that's 0 at the centre, so
/// its interval reached 0 there, the spurious zero the spike's topology test found, and the two
/// boxes around the body's centre stayed open.
#[test]
fn smins_seams_certify() {
    let mut host = host();
    let depth = 8;
    let leaf = 2.0 / (1 << depth) as f32;
    let (certified, open) = certify(&mut host, 5, creature_cube(), depth);
    assert!(certified > 5000, "only {certified} boxes certified");
    for b in &open {
        let c = b.centre();
        let args = [Value::I32(5), Value::F32(c[0]), Value::F32(c[1]), Value::F32(c[2])];
        let v = one_f32(&mut host, "value", &args);
        assert!(v.abs() > 2.0 * leaf, "an open box near the surface, at {c:?} (distance {v})");
    }
    assert!(
        open.is_empty(),
        "{} boxes open, at {:?}",
        open.len(),
        open.first().map(|b| b.centre())
    );
}

/// A box's flat faces certify: with std's `cuboid` (one branch on inside or outside) and
/// branch guards narrowing the intervals, the only boxes left open reach within a leaf of an
/// edge, where the faces' normals are 90° apart (inside, the gradient jumps where two faces
/// are equally near), and no certificate can hold.
#[test]
fn a_boxs_flat_faces_certify() {
    let mut host = host();
    let half = [0.5f32, 0.3, 0.4];
    let depth = 7;
    let root = Aabb::cube([0.0; 3], 0.8);
    let leaf = 1.6 / (1 << depth) as f32;
    let (certified, open) = certify(&mut host, 14, root, depth);
    assert!(certified > 1000, "only {certified} boxes certified");
    for b in &open {
        let c = b.centre();
        // Within a leaf of a face plane, from the box's nearest point.
        let near = (0..3).filter(|&a| (c[a].abs() - half[a]).abs() <= 1.5 * leaf + 1e-6).count();
        assert!(near >= 2, "an open box on a face, not an edge: {b:?}");
    }
    println!(
        "cuboid to {:.1} mm: {certified} certified, {} open, all on edges",
        1000.0 * leaf,
        open.len()
    );
}

/// SplitMix64: the test's points, the same every run.
struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32
    }

    fn in_box(&mut self, b: &Aabb) -> [f32; 3] {
        std::array::from_fn(|a| b.lo[a] + (b.hi[a] - b.lo[a]) * self.unit())
    }
}

/// Whether the box crosses a lattice plane of the bark's fbm (std::field's `Fbm`: octave o
/// samples value noise at (p + offset) × 6 × 2^o + 17 o): where `floor` isn't one integer.
fn crosses_lattice(b: &Aabb) -> bool {
    let offset = [3.0f64, 1.0, 7.0];
    (0..3).any(|o| {
        let fr = 6.0 * f64::from(1u32 << o);
        (0..3).any(|a| {
            let at = |x: f32| ((f64::from(x) + offset[a]) * fr + 17.0 * o as f64).floor();
            at(b.lo[a]) != at(b.hi[a])
        })
    })
}

/// The bark (0.015 × fbm, 3 octaves from 6 per metre): where a box crosses a noise lattice
/// plane, `floor` splits into cases (one per integer), so the derived bound of its gradient
/// tends to the largest gradient sampled in the box as the box shrinks. The spike measured it
/// 7–30× above that, however small the box.
#[test]
fn the_barks_gradient_bound_tends_to_the_truth() {
    let mut host = host();
    let mut rng = Rng(7);
    let domain = Aabb { lo: [-0.8, -0.65, -0.6], hi: [1.2, 1.1, 0.6] };
    let mut medians = Vec::new();
    for half in [0.02f32, 0.005, 0.00125, 0.0003] {
        let mut ratios = Vec::new();
        while ratios.len() < 300 {
            let b = Aabb::cube(rng.in_box(&domain), half);
            if !crosses_lattice(&b) {
                continue;
            }
            let mut args = vec![Value::I32(11)];
            args.extend(b.lo.iter().chain(&b.hi).map(|&x| Value::F32(x)));
            let bound = one_f32(&mut host, "lipschitz", &args);
            let mut most = 0.0f32;
            for _ in 0..16 {
                let p = rng.in_box(&b);
                let args = [Value::I32(11), Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2])];
                let g = host.call_export("grad", &args).expect("grad");
                let [Value::F32(x), Value::F32(y), Value::F32(z)] = g[..] else { panic!("{g:?}") };
                most = most.max((x * x + y * y + z * z).sqrt());
            }
            assert!(bound >= most, "a bound below a sampled gradient: {bound} < {most}");
            ratios.push(bound / most.max(1e-6));
        }
        ratios.sort_by(f32::total_cmp);
        medians.push(ratios[ratios.len() / 2]);
    }
    println!("bark, boxes crossing a lattice plane: median bound / sampled max {medians:?}");
    assert!(medians.windows(2).all(|w| w[1] <= w[0] * 1.05), "not tending down: {medians:?}");
    assert!(medians[3] < 1.5, "at 0.6 mm the bound is {}x the sampled maximum", medians[3]);
}

/// `creature(t)`, the creature with its bark, derives over t (the breath): activity is tracked
/// by field, so the bark's fbm stays a constant of a displacement built from an animated body,
/// and its loop over octaves runs 3 times for every t. Its range over boxes of space and time
/// holds every sampled value, and its derivative over t agrees with a finite difference.
#[test]
fn the_creature_derives_over_time() {
    let mut host = host();
    let mut rng = Rng(11);
    let domain = Aabb { lo: [-0.8, -0.65, -0.6], hi: [1.2, 1.1, 0.6] };
    let at = |p: [f32; 3], t: f32| {
        let mut args = vec![Value::I32(0)];
        args.extend(p.iter().chain([&t]).map(|&x| Value::F32(x)));
        args
    };
    for half in [0.2f32, 0.05, 0.01] {
        for _ in 0..100 {
            let b = Aabb::cube(rng.in_box(&domain), half);
            let (t0, t1) = (rng.unit(), rng.unit());
            let (t0, t1) = (t0.min(t1), t0.max(t1));
            let mut args = vec![Value::I32(0)];
            args.extend(b.lo.iter().chain([&t0]).chain(&b.hi).chain([&t1]).map(|&x| Value::F32(x)));
            let r = host.call_export("timed_ival", &args).expect("timed_ival");
            let [Value::F32(lo), Value::F32(hi)] = r[..] else { panic!("{r:?}") };
            assert!(lo.is_finite() && hi.is_finite(), "an unbounded range: [{lo}, {hi}]");
            for _ in 0..8 {
                let (p, t) = (rng.in_box(&b), t0 + (t1 - t0) * rng.unit());
                let v = one_f32(&mut host, "timed_value", &at(p, t));
                assert!(lo <= v && v <= hi, "{v} outside [{lo}, {hi}] at {p:?}, t = {t}");
            }
        }
    }
    for _ in 0..200 {
        let (p, t) = (rng.in_box(&domain), 0.1 + 0.8 * rng.unit());
        let d = one_f32(&mut host, "timed_dt", &at(p, t));
        let h = 1e-2;
        let up = one_f32(&mut host, "timed_value", &at(p, t + h));
        let down = one_f32(&mut host, "timed_value", &at(p, t - h));
        let fd = (up - down) / (2.0 * h);
        assert!((d - fd).abs() <= 2e-3 + 0.02 * fd.abs(), "d/dt {d}, finite difference {fd}");
    }
}

/// Spike 13's GPU experiment (AC12): the nested derivations classify a 128³ grid over each
/// creature in one dispatch. Loading takes at most 1 s cold (unique shader source, so no
/// driver cache helps; M2 began at 4.7 s); every certificate checked by sampling is right;
/// and each pipeline's WGSL is at most 256 KiB. The GPU times are printed.
#[test]
#[ignore = "long: alone: needs a GPU"]
fn the_gpu_certificate_is_small_quick_and_right() {
    let dir = built("spike13");
    // A copy whose shaders differ from any run before: cold.
    let cold = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("spike13-cold");
    let _ = std::fs::remove_dir_all(&cold);
    wrela_tests::copy_dir(&dir, &cold);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("time");
    for e in std::fs::read_dir(&cold).expect("read") {
        let p = e.expect("entry").path();
        if p.extension().is_some_and(|x| x == "wgsl") {
            let text = std::fs::read_to_string(&p).expect("wgsl");
            assert!(text.len() <= 256 << 10, "{}: {} bytes of WGSL", p.display(), text.len());
            // A function of its own name: naga drops comments, but writes every function into
            // the Metal source, so the driver sees source it hasn't compiled before.
            let n = stamp.as_nanos();
            let unique =
                format!("\nfn cold_{n}() -> u32 {{\n    return {}u;\n}}\n", n % 1_000_000_007);
            std::fs::write(&p, format!("{text}{unique}")).expect("write");
        }
    }
    let start = std::time::Instant::now();
    let options = wrela_host::Options { timestamps: true, ..wrela_host::Options::default() };
    let mut gpu = wrela_host::Host::load_with(&cold, &options).expect("load");
    let load = start.elapsed().as_secs_f64();
    println!("load, with pipeline compilation, cold: {load:.2} s");
    let mut cpu = host();
    let n = 128u32;
    let mut rng = Rng(3);
    for (which, fi, name) in [(0, 7u32, "smin_if creature"), (1, 0, "creature with bark")] {
        let root = creature_cube();
        let cell = (root.hi[0] - root.lo[0]) / n as f32;
        let args = [
            Value::I32(which),
            Value::I32(n as i32),
            Value::F32(root.lo[0]),
            Value::F32(root.lo[1]),
            Value::F32(root.lo[2]),
            Value::F32(cell),
        ];
        // Twice: the first may include lazy pipeline work.
        let mut classes = Vec::new();
        let mut nanos = 0.0;
        for _ in 0..2 {
            gpu.call_export("gpu_classify", &args).expect("classify");
            let out = *gpu.buffers().last().expect("a buffer");
            classes = wrela_tests::u32s(&gpu.read_buffer(out).expect("read"));
            nanos = gpu.take_timings().expect("timings").iter().map(|t| t.nanos).sum::<f64>();
        }
        let count = |k: u32| classes.iter().filter(|&&c| c == k).count();
        println!(
            "{name}: {n}³ cells of {:.1} mm, GPU {:.2} ms: {} empty, {} certified, {} open",
            1000.0 * cell,
            nanos / 1e6,
            count(0),
            count(1),
            count(2)
        );
        let cell_box = |i: usize| {
            let i = i as u32;
            let c = [i % n, (i / n) % n, i / (n * n)];
            let lo: [f32; 3] = std::array::from_fn(|a| root.lo[a] + c[a] as f32 * cell);
            Aabb { lo, hi: lo.map(|x| x + cell) }
        };
        // Every certificate in a sample of the cells, checked by sampling the CPU's values and
        // gradients in the cell.
        let (mut wrong, mut checked) = (0, 0);
        for (i, &c) in classes.iter().enumerate() {
            if c == 2 || rng.unit() > 0.02 {
                continue;
            }
            checked += 1;
            let b = cell_box(i);
            let value = |cpu: &mut CpuHost, p: [f32; 3]| {
                let args =
                    [Value::I32(fi as i32), Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2])];
                one_f32(cpu, "value", &args)
            };
            if c == 0 {
                let sign = value(&mut cpu, rng.in_box(&b)) < 0.0;
                if (0..15).any(|_| (value(&mut cpu, rng.in_box(&b)) < 0.0) != sign) {
                    wrong += 1;
                }
            } else {
                let gs: Vec<[f32; 3]> = (0..16)
                    .map(|_| {
                        let p = rng.in_box(&b);
                        let args = [
                            Value::I32(fi as i32),
                            Value::F32(p[0]),
                            Value::F32(p[1]),
                            Value::F32(p[2]),
                        ];
                        let g = cpu.call_export("grad", &args).expect("grad");
                        let [Value::F32(x), Value::F32(y), Value::F32(z)] = g[..] else { panic!() };
                        [x, y, z]
                    })
                    .collect();
                let ok = gs
                    .iter()
                    .all(|a| gs.iter().all(|b| a[0] * b[0] + a[1] * b[1] + a[2] * b[2] > 0.0));
                if !ok {
                    wrong += 1;
                }
            }
        }
        println!("  {checked} certificates checked by sampling, {wrong} wrong");
        assert_eq!(wrong, 0, "{name}: wrong certificates");
        assert!(checked > 1000, "{name}: only {checked} certificates checked");
    }
    // A debug build's wgpu validates as it goes, and its wasmtime compiles slowly: the budget
    // is the release host's (`cargo test --release`).
    if cfg!(debug_assertions) {
        println!("  (a debug build: the 1 s budget is checked in release)");
    } else {
        assert!(load <= 1.0, "loading took {load:.2} s cold");
    }
}
