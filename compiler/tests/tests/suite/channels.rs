//! Channels (AC5, language.md §17, D-002, D-026): compiler/tests/channels's `Tissue`, whose
//! `Blend` is derived field by field, on a body and a hoof joined by a smooth union.

use crate::built;
use wrela_host::{CpuBuild, CpuHost, Host, Value};
use wrela_tests::{f32s, one_f32, one_u32, one_vec3, u32s};

fn host() -> CpuHost {
    CpuBuild::load(built("channels")).expect("load").start_with(1).expect("start")
}

fn at(x: f32, y: f32, z: f32) -> [Value; 3] {
    [Value::F32(x), Value::F32(y), Value::F32(z)]
}

#[test]
fn each_part_carries_its_own_channels() {
    let mut host = host();
    // Inside the body, away from the hoof: HIDE, its colour varying with height.
    let body = at(0.0, 0.3, 0.0);
    assert!((one_f32(&mut host, "albedo_r", &body) - 0.39).abs() < 1e-6);
    assert_eq!(one_f32(&mut host, "roughness", &body), 0.7);
    assert_eq!(one_vec3(&mut host, "bias", &body), [0.0, 1.0, 0.0]);
    assert_eq!(one_u32(&mut host, "material", &body), 0);
    // At the hoof's tip: HOOF.
    let hoof = at(0.9, 0.0, 0.0);
    assert_eq!(one_f32(&mut host, "albedo_r", &hoof), 0.12);
    assert_eq!(one_f32(&mut host, "roughness", &hoof), 0.35);
    assert_eq!(one_vec3(&mut host, "bias", &hoof), [1.0, 0.0, 0.0]);
    assert_eq!(one_u32(&mut host, "material", &hoof), 1);
}

#[test]
fn channels_blend_where_the_parts_blend() {
    let mut host = host();
    // Along the axis between them: roughness falls from the body's to the hoof's, the bias
    // turns from up to forward at unit length, and the material switches once.
    let (mut last, mut switches, mut mixed) = (0.7f32, 0, 0);
    let mut material = 0;
    for i in 0..=100 {
        let p = at(0.3 + 0.006 * i as f32, 0.0, 0.0);
        let r = one_f32(&mut host, "roughness", &p);
        assert!(r <= last + 1e-6 && (0.35..=0.7).contains(&r), "roughness {r} at step {i}");
        if r > 0.35 && r < 0.7 {
            mixed += 1;
        }
        last = r;
        let b = one_vec3(&mut host, "bias", &p);
        let len = (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt();
        assert!((len - 1.0).abs() < 1e-5, "the bias isn't unit length: {b:?}");
        let m = one_u32(&mut host, "material", &p);
        if m != material {
            switches += 1;
            material = m;
        }
    }
    assert!(mixed > 5, "only {mixed} steps blend");
    assert_eq!((switches, material), (1, 1), "the material switches once, to the hoof's");
}

/// On the GPU, the channels are the CPU's (within float rounding: WGSL's sqrt and division
/// may differ by an ulp or two).
#[test]
#[ignore = "needs a GPU"]
fn the_gpu_computes_the_same_channels() {
    let dir = built("channels");
    let mut host = Host::load(&dir).expect("load");
    let out = *host.buffers().last().expect("a buffer");
    let bytes = host.read_buffer(out).expect("read");
    let (floats, words) = (f32s(&bytes), u32s(&bytes));
    let mut cpu = self::host();
    for i in 0..64 {
        let c = cpu.call_export("cpu_at", &[Value::I32(i)]).expect("cpu");
        let [Value::F32(albedo), Value::F32(rough), Value::F32(bias), Value::F32(m)] = c[..] else {
            panic!("{c:?}")
        };
        let g = &floats[i as usize * 8..];
        assert!((g[0] - albedo).abs() < 1e-5, "albedo at {i}: {} vs {albedo}", g[0]);
        assert!((g[3] - rough).abs() < 1e-5, "roughness at {i}: {} vs {rough}", g[3]);
        assert!((g[4] - bias).abs() < 1e-5, "bias at {i}: {} vs {bias}", g[4]);
        assert_eq!(words[i as usize * 8 + 7] as f32, m, "material at {i}");
    }
}

/// A chain of blended parts evaluates each part once for its channels, not once per blend it's
/// under: eight parts' channels in a kernel's WGSL evaluate eight distances (before joint
/// evaluation, 35), and an `n`-ary union's channels do too. Each part's distance multiplies by
/// a marker constant, so its evaluations can be counted in the WGSL.
#[test]
fn channels_evaluate_each_part_once() {
    let src = "use std::field::{Blend, Field, Surface}
use std::gpu::{GlobalId, GpuBuffer, Slots, buffer, dispatch}

struct Tint: Copy + GpuData + Blend {
    r: f32,
}

struct Ball: Copy + GpuData {
    at: vec3,
    r: f32,
}

impl Surface for Ball {
    fn distance(self, p: vec3) -> f32 {
        length(p - self.at) * 1.375 - self.r
    }
}

impl Field<Tint> for Ball {
    fn channels(self, p: vec3) -> Tint {
        Tint { r: self.r }
    }
}

fn ball(i: f32) -> Ball {
    Ball { at: vec3(x: i), r: 0.5 + i }
}

@compute(64)
fn chain<F: Field<Tint>>(field: F, out: mut Slots<f32>, id: GlobalId) {
    out[id] = field.channels(vec3(x: f32(id.x) * 0.1)).r
}

pub fn frame(time: f32, width: u32, height: u32) {
    var out: GpuBuffer<f32> = buffer(64)
    let eight = ball(0.0)
        .smooth_union(ball(1.0), k: 0.1)
        .smooth_union(ball(2.0), k: 0.1)
        .smooth_union(ball(3.0), k: 0.1)
        .smooth_union(ball(4.0), k: 0.1)
        .smooth_union(ball(5.0), k: 0.1)
        .smooth_union(ball(6.0), k: 0.1)
        .smooth_union(ball(7.0), k: 0.1)
    dispatch(chain.bind(eight, mut out), groups: 1)
    let parts = [ball(0.0), ball(1.0), ball(2.0), ball(3.0), ball(4.0), ball(5.0)]
    dispatch(chain.bind(parts.smooth_union(k: 0.1), mut out), groups: 1)
}
";
    let pkg = crate::package("channels-once", src);
    let built = wrela_tests::build(&pkg).unwrap_or_else(|e| panic!("doesn't build:\n{e}"));
    let mut counts: Vec<usize> = built
        .files
        .iter()
        .filter(|(n, _)| n.ends_with(".wgsl"))
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).matches("1.375").count())
        .collect();
    counts.sort();
    // The array's union loops over its parts: one evaluation in the loop, one for the first.
    assert_eq!(counts, [2, 8], "evaluations of a part's distance in each pipeline's WGSL");
}
