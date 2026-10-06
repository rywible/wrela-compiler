//! Lipschitz bounds (AC5, language.md §17, D-092), over the corpus's surfaces that state one
//! (compiler/tests/fields: `with_lipschitz`): each composed bound holds where its consumers look,
//! by sampling the derived gradient and by std's debug-build check; `to_bound` gives a field
//! sphere tracing can step by; and a debug build catches a wrong bound.

use crate::built;
use wrela_host::{CpuBuild, CpuHost, Value};
use wrela_tests::{one_f32, one_u32};

const NAMES: [&str; 10] = [
    "sphere",
    "ellipsoid",
    "round cone",
    "hoof",
    "blob",
    "fbm",
    "creature",
    "grazer",
    "oval",
    "tube",
];
/// Where each is sampled: [-2, 2]³ times this, so enough points are near each surface.
const SCALES: [f32; 10] = [1.0, 1.0, 1.0, 0.1, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
/// The scopes consumers pass: a sphere tracer's step, and a block's radius.
const SCOPES: [f32; 2] = [0.02, 0.1];

fn host() -> CpuHost {
    CpuBuild::load(built("fields")).expect("load").start_with(1).expect("start")
}

#[test]
fn composed_bounds_hold_where_consumers_look() {
    let mut host = host();
    for (i, name) in NAMES.iter().enumerate() {
        for near in SCOPES {
            let args = [
                Value::I32(i as i32),
                Value::F32(near),
                Value::I32(3),
                Value::I32(400_000),
                Value::F32(SCALES[i]),
            ];
            let r = host.call_export("gradient_ratio", &args).expect("ratio");
            let [Value::F32(worst), Value::F32(count)] = r[..] else { panic!("{r:?}") };
            // The sampled gradient is computed in f32: allow its rounding.
            assert!(worst <= 1.0001, "{name}, near {near}: a gradient is {worst} times the bound");
            assert!(count > 100.0, "{name}, near {near}: only {count} points in scope");
            // std's debug-build check, which panics on a bound the derived gradient disproves.
            host.call_export("check_bound", &[Value::I32(i as i32), Value::F32(near)])
                .unwrap_or_else(|e| panic!("{name}, near {near}: {e}"));
        }
    }
}

#[test]
fn sphere_tracing_can_step_by_to_bound() {
    let mut host = host();
    for (i, name) in NAMES.iter().enumerate() {
        let near = 0.1;
        let l = one_f32(&mut host, "lipschitz_of", &[Value::I32(i as i32), Value::F32(near)]);
        let args = [Value::I32(i as i32), Value::F32(near), Value::I32(5), Value::I32(20_000)];
        let r = one_u32(&mut host, "step_violations", &args);
        let (bad, balls) = (r >> 16, r & 0xffff);
        assert_eq!(bad, 0, "{name}: {bad} points of the other sign within the bound");
        if l.is_finite() {
            assert!(balls > 1000, "{name}: only {balls} steps tried");
        }
    }
}

/// A sound bound isn't always a useful one: these are the bounds the corpus states at 10 cm,
/// for the record. The grazer's twenty-part fold scopes its torso's ellipsoids past their
/// centres, where std's ellipsoid has no finite bound.
#[test]
fn the_corpus_bounds_at_10_cm() {
    let mut host = host();
    let got: Vec<f32> = (0..NAMES.len())
        .map(|i| one_f32(&mut host, "lipschitz_of", &[Value::I32(i as i32), Value::F32(0.1)]))
        .collect();
    for (name, l) in NAMES.iter().zip(&got) {
        println!("{name:12} {l}");
    }
    assert_eq!(&got[..4], &[1.0, got[1], 1.0, 1.0]);
    assert!(got[1] > 1.0 && got[1] < 10.0, "the ellipsoid's bound: {}", got[1]);
    assert!(got[7].is_infinite(), "the grazer's bound: {}", got[7]);
    // The oval's is finite where its ellipsoid's isn't, and smaller at 10 cm; the tube is exact.
    assert!(got[8] < got[1], "the oval's bound: {}", got[8]);
    assert_eq!(got[9], 1.0, "the tube's bound");
}

/// A release build trusts a stated bound; a debug build checks it when `to_bound` uses it.
#[test]
fn a_debug_build_catches_a_wrong_bound() {
    let src = wrela_tests::repo_root().join("compiler/tests/lipschitz");
    let tmp = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let release = tmp.join("lipschitz-release");
    wrela_tests::must_build(&src, &release);
    let mut host = CpuBuild::load(&release).expect("load").start_with(1).expect("start");
    assert_eq!(one_f32(&mut host, "bound_at", &[]), 0.2);
    let debug = tmp.join("lipschitz-debug");
    let built = wrela_driver::build_debug(&src);
    assert!(!built.has_errors(), "the debug build fails");
    built.write_to(&debug).expect("write");
    let mut host = CpuBuild::load(&debug).expect("load").start_with(1).expect("start");
    let err = host.call_export("bound_at", &[]).expect_err("a panic").to_string();
    assert!(err.contains("a Lipschitz bound of 0.5 for |distance| < 0.1 is too small"), "{err}");
}
