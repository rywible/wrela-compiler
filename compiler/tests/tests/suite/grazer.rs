//! AC2 and AC3 on the grazer: sketch 01's creature written in tier-0 wrela
//! (compiler/tests/fields) against spike 01's hand-written WGSL.
//!
//! The fixtures are spike 01's files at the `design-archive-2026-10` tag, unchanged:
//! `fixtures/spike01/field.wgsl` (the hand-written field) and `params-seed1.f32` (grazer.js's
//! `makeGrazer(1)`, the `Grazer` uniform). The harness kernels (`wrela_tests::spike01`'s) are
//! appended to field.wgsl, as the spike's own extract.wgsl and draw.wgsl are.
//!
//! GPU time is compared in one harness ([`RawGpu`]): the WGSL `wrela build` emitted, with the
//! uniform bytes the program submitted, against the hand-written kernel, alternating, after a
//! warm-up. (Through wrela-host the same compiled kernel times within a few percent of this
//! when the GPU is warm, but timings there swing with whatever else the GPU did just before.)
//!
//! These need a GPU: `cargo test -p wrela-tests --test suite grazer:: -- --ignored --nocapture`.

use crate::built;
use wrela_host::{Host, Options, Value};
use wrela_tests::spike01::{HARNESS_D, compiled_kernel, fixture, harness, params_seed1};
use wrela_tests::{Bind, RawGpu, bytes_of, f32s, median, one_f32, one_u32, u32s, worse};

/// 2²⁰ points ("1M").
const POINTS: u32 = 1 << 20;
const REPS: u32 = 20;
const ROUNDS: usize = 5;

fn load() -> Host {
    Host::load_with(built("fields"), &Options { record: true, ..Options::default() })
        .expect("load the grazer")
}

fn spike_params(host: &mut Host, seed: u32) -> Vec<f32> {
    (0..328).map(|i| one_f32(host, "param", &[Value::I32(seed as i32), Value::I32(i)])).collect()
}

/// The angle between two gradients; NaN (which fails) where it isn't defined: a gradient that
/// is zero or not a number, unless the two are the same.
fn angle(a: [f32; 3], b: [f32; 3]) -> f64 {
    if a == b {
        return 0.0;
    }
    let (a, b) = (a.map(f64::from), b.map(f64::from));
    let norm = |v: [f64; 3]| v[0].hypot(v[1]).hypot(v[2]);
    if !(norm(a) > 0.0 && norm(b) > 0.0) {
        return f64::NAN;
    }
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let cross = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    (cross[0].hypot(cross[1]).hypot(cross[2])).atan2(dot)
}

#[test]
#[ignore = "needs a GPU"]
fn the_compiled_grazer_matches_the_hand_written_one() {
    let mut host = load();

    // The same numbers: wrela's `params` is grazer.js's `makeGrazer`, bit for bit.
    let (theirs, params) = (params_seed1(), spike_params(&mut host, 1));
    let mismatched: Vec<usize> =
        (0..328).filter(|&i| params[i].to_bits() != theirs[i].to_bits()).collect();
    assert!(mismatched.is_empty(), "parameters differ from the spike's at {mismatched:?}");

    // The compiled field: points over the grazer's bounds, the distance, and the distance and
    // gradient: the newest three buffers, which the program's state holds.
    host.call_export("evaluate", &[Value::I32(1), Value::I32(POINTS as i32)]).expect("evaluate");
    let made = host.buffers();
    let [.., p, d, s] = made[..] else { panic!("evaluate made {made:?}") };
    let points = host.read_buffer(p).expect("points");
    let dist = f32s(&host.read_buffer(d).expect("distances"));
    let samp = f32s(&host.read_buffer(s).expect("samples"));
    let batches = host.take_batches();
    drop(host);

    // The hand-written field, on the same points.
    let field = fixture("field.wgsl");
    let gpu = RawGpu::new().expect("a GPU");
    let spike_uniform = bytes_of(&params);
    let n = u64::from(POINTS);
    let run = |wgsl: &str, entry: &str, uniform: &[u8], out: u64| {
        gpu.run(
            wgsl,
            entry,
            &[Bind::Uniform(uniform), Bind::Read(&points), Bind::Write(out)],
            POINTS / 64,
            REPS,
        )
    };
    let hand_d = format!("{field}{HARNESS_D}");
    let hand_g = format!("{field}{}", harness("eval_g", "vec4f(s.d, s.g)"));
    let (_, out_d) = run(&hand_d, "eval_d", &spike_uniform, n * 4);
    let (_, out_g) = run(&hand_g, "eval_g", &spike_uniform, n * 16);
    let (hd, hg) = (f32s(&out_d[0]), f32s(&out_g[0]));

    // GPU time: the compiled kernels (the build's WGSL, the program's uniforms) and the
    // hand-written ones, alternating; the first round warms the GPU up and isn't counted.
    let (ours_dw, ours_de, ours_du) = compiled_kernel(&built("fields"), "distances", &batches);
    let (ours_gw, ours_ge, ours_gu) = compiled_kernel(&built("fields"), "samples", &batches);
    let (mut ours_d, mut ours_g, mut theirs_d, mut theirs_g) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for round in 0..=ROUNDS {
        let t = [
            run(&ours_dw, &ours_de, &ours_du, n * 4).0,
            run(&hand_d, "eval_d", &spike_uniform, n * 4).0,
            run(&ours_gw, &ours_ge, &ours_gu, n * 16).0,
            run(&hand_g, "eval_g", &spike_uniform, n * 16).0,
        ];
        if round > 0 {
            ours_d.extend(&t[0]);
            theirs_d.extend(&t[1]);
            ours_g.extend(&t[2]);
            theirs_g.extend(&t[3]);
        }
    }

    let mut worst_d = 0.0f64;
    let mut worst_sd = 0.0f64;
    let mut worst_angle = 0.0f64;
    let mut over_d = 0;
    let mut over_angle = Vec::new();
    for i in 0..POINTS as usize {
        let e = (f64::from(dist[i]) - f64::from(hd[i])).abs();
        let es = (f64::from(samp[4 * i]) - f64::from(hg[4 * i])).abs();
        // NaN counts as over, and as the worst.
        if !(e <= 1e-5 && es <= 1e-5) {
            over_d += 1;
        }
        worst_d = worse(worst_d, e);
        worst_sd = worse(worst_sd, es);
        let g = [samp[4 * i + 1], samp[4 * i + 2], samp[4 * i + 3]];
        let h = [hg[4 * i + 1], hg[4 * i + 2], hg[4 * i + 3]];
        let a = angle(g, h);
        // NaN counts as over.
        if a.is_nan() || a > 0.01 {
            over_angle.push(i);
        }
        if a.is_finite() {
            worst_angle = worst_angle.max(a);
        }
    }
    let (md, mg) = (median(&ours_d), median(&ours_g));
    let (hmd, hmg) = (median(&theirs_d), median(&theirs_g));
    println!("AC2: {POINTS} points over grazer 1's bounds");
    println!(
        "  distance: worst |Δ| {worst_d:.3e} m (distance kernel), {worst_sd:.3e} m (gradient kernel)"
    );
    println!("  gradient: worst angle {worst_angle:.3e} rad; over 0.01 rad: {}", over_angle.len());
    println!(
        "  GPU time, median of {}: distance {:.3} ms vs hand-written {:.3} ms ({:.2}x); \
         distance and gradient {:.3} ms vs {:.3} ms ({:.2}x)",
        ours_d.len(),
        md / 1e6,
        hmd / 1e6,
        md / hmd,
        mg / 1e6,
        hmg / 1e6,
        mg / hmg
    );
    for &i in over_angle.iter().take(5) {
        let p = f32s(&points[16 * i..16 * i + 12]);
        println!(
            "    {p:?}: ours {:?}, theirs {:?}",
            &samp[4 * i..4 * i + 4],
            &hg[4 * i..4 * i + 4]
        );
    }
    // A GPU without timestamps gives zeros, which would pass the time gates.
    for (what, t) in
        [("ours", &ours_d), ("theirs", &theirs_d), ("ours", &ours_g), ("theirs", &theirs_g)]
    {
        assert!(
            t.iter().all(|&x| x > 0.0 && x.is_finite()),
            "{what}: GPU times {t:?} aren't all positive"
        );
    }
    assert_eq!(over_d, 0, "distances differ by more than 1e-5 m");
    assert!(over_angle.is_empty(), "gradients differ by more than 0.01 rad");
    assert!(md <= 1.25 * hmd, "the distance kernel is {:.2}x the hand-written one", md / hmd);
    assert!(mg <= 1.25 * hmg, "the gradient kernel is {:.2}x the hand-written one", mg / hmg);
}

/// Measured, not gated (AC3): the fraction of spike 01's blocks (4×4×4 cells of 1.5 cm over
/// each of the herd's 40 grazers) that the derived interval keeps, against the spike's
/// hand-written Lipschitz bound, computed here on the same grid.
#[test]
#[ignore = "needs a GPU"]
fn block_culling_on_the_spikes_grid() {
    let mut host = load();
    let mut kept = [0u64; 2];
    let mut blocks = 0u64;
    for seed in 1..=40 {
        for (rule, k) in kept.iter_mut().enumerate() {
            let args = [Value::I32(seed), Value::F32(0.015), Value::I32(rule as i32)];
            let n = one_u32(&mut host, "cull", &args);
            // The newest buffer, which the program's state holds.
            let live = *host.buffers().last().expect("a buffer");
            let live = u32s(&host.read_buffer(live).expect("live blocks"));
            *k += live.iter().map(|&x| u64::from(x)).sum::<u64>();
            if rule == 0 {
                blocks += u64::from(n);
            }
        }
    }
    let frac = |k: u64| k as f64 / blocks as f64;
    println!(
        "AC3 block culling at 1.5 cm, 40 grazers, {blocks} blocks: derived interval keeps {:.1}%, \
         the spike's Lipschitz bound {:.1}% (spike 01 reported 19.5%)",
        100.0 * frac(kept[0]),
        100.0 * frac(kept[1])
    );
    // The grid is the spike's: its rule, recomputed here, gives its number.
    assert!((frac(kept[1]) - 0.195).abs() < 0.0051, "the Lipschitz rule kept {:.3}", frac(kept[1]));
}
