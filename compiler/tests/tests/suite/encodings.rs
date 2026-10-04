//! Lossy encodings (AC5, D-049; compiler/tests/encodings): `Unorm8x4`, `Half2` and `Oct32`
//! round-trip within their documented error, and the GPU decodes what the CPU encoded as the
//! CPU does.

use crate::built;
use wrela_host::{CpuBuild, CpuHost, Host, Value};
use wrela_tests::{f32s, one_f32, one_u32};

fn host() -> CpuHost {
    CpuBuild::load(built("encodings")).expect("load").start_with(1).expect("start")
}

fn args(seed: i32, n: i32) -> [Value; 2] {
    [Value::I32(seed), Value::I32(n)]
}

#[test]
fn each_round_trips_within_its_documented_error() {
    let mut host = host();
    let unorm = one_f32(&mut host, "unorm_error", &args(1, 100_000));
    assert!(unorm <= 0.5 / 255.0 + 1e-7, "Unorm8x4: {unorm}");
    let half = one_f32(&mut host, "half_error", &args(2, 100_000));
    assert!(half <= 1.0 / 2048.0, "Half2: relative error {half}");
    let oct = one_f32(&mut host, "oct_error", &args(3, 100_000));
    println!("worst errors: Unorm8x4 {unorm:e}, Half2 {half:e} relative, Oct32 {oct:e} rad");
    assert!(oct <= 1.5e-4, "Oct32: {oct} radians");
}

/// Every half float survives a round trip, and every midpoint between neighbours rounds to
/// the even one: 65,536 values and their midpoints.
#[test]
fn half_floats_convert_exactly() {
    assert_eq!(one_u32(&mut host(), "half_exhaustive", &[]), 0);
}

#[test]
#[ignore = "needs a GPU"]
fn the_gpu_decodes_as_the_cpu_does() {
    let dir = built("encodings");
    let mut gpu = Host::load(&dir).expect("load");
    let out = *gpu.buffers().last().expect("a buffer");
    let floats = f32s(&gpu.read_buffer(out).expect("read"));
    let mut cpu = host();
    for i in 0..1024 {
        for k in [0, 1, 2, 3, 4, 5, 6, 8, 9] {
            let want = one_f32(&mut cpu, "cpu_decoded", &[Value::I32(i), Value::I32(k)]);
            let got = floats[i as usize * 12 + k as usize];
            // WGSL's division may differ from the CPU's by an ulp or two, and its normalize
            // (an inverse square root, 2 ulp) by a few ulp of 1 in each component.
            let tol = if (4..=6).contains(&k) { 1e-6 } else { 4.0 * f32::EPSILON * want.abs() };
            assert!((got - want).abs() <= tol, "value {i}, component {k}: {got} vs {want}");
        }
    }
}
