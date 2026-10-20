//! The herd (examples/herd) against spike 01 (#42 AC1–AC7). The fixtures are the spike's files,
//! unchanged (compiler/tests/fixtures/spike01): its CPU module (`cpu.wasm`), its kernels and its
//! seed-1 parameters. The compiled grazer's checks against them (its field and its skin weights)
//! build frozen copies of the herd and the engine (compiler/tests/frozen, herd-field): they test
//! the compiler, so their input doesn't change with the engine.
//!
//! CPU comparisons run both sides in one harness, the native host's wasmtime, alternating after a
//! warm-up round (M1's method): the spike's `cpu.wasm` and the herd's build.

use crate::built;
use std::path::PathBuf;
use wrela_host::{CpuBuild, CpuHost, Host, Value};
use wrela_tests::spike01::{HARNESS_D, fixture, fixture_path, harness};

/// The herd's frozen copy (compiler/tests/frozen/herd, a compiler-regression input), built once
/// for the suite (`built` builds it under the suite's scratch).
fn herd() -> PathBuf {
    built("frozen/herd")
}

fn ours() -> CpuHost {
    CpuBuild::load(herd()).expect("load the herd").start_with(1).expect("start")
}

// ---- AC7: the herd's grazer against field.wgsl ------------------------------------------------

/// The herd's grazer, seed 1, at the `n` points herd-field's export `export` (`evaluate` or
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
    let mut host = Host::load_with(built("herd-field"), &options).expect("load");
    host.call_export(export, &[Value::I32(1), Value::I32(n as i32)]).expect(export);
    let made = host.buffers();
    let [.., p, d, s] = made[..] else { panic!("{export} made {made:?}") };
    let points = host.read_buffer(p).expect("points");
    let dist = f32s(&host.read_buffer(d).expect("distances"));
    let samp = f32s(&host.read_buffer(s).expect("samples"));
    (points, dist, samp, host.take_batches())
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
