//! `std::stage` (AC6, language.md §7, D-029): fields built at runtime as tapes, which
//! `interpret` evaluates, against the compiled fields they were built to match
//! (compiler/tests/fields: `sphere_tape`, `waves_tape`, `blob_tape`). Their derived gradients
//! and intervals are in the corpus's gradient and enclosure tests (derive.rs).

use crate::built;
use std::sync::Mutex;
use wrela_host::{CpuBuild, Host, Value};
use wrela_tests::{f32s, one_f32, one_u32, par_each};

const POINTS: u32 = 1_000_000;
const TOLERANCE: f32 = 1e-5;
/// The twins `tape_error_cpu` and `tape_values_gpu` take, by index, with their names: each
/// corpus field (derive.rs, `NAMES`) and its `to_tape()`; then the tapes the corpus builds at
/// runtime and the fields they were built to match.
fn twins() -> Vec<(u32, String)> {
    let corpus = crate::derive::NAMES.iter().enumerate();
    let built = ["sphere", "waves", "blob"].iter().enumerate();
    corpus
        .map(|(k, n)| (k as u32, format!("{n}, to_tape")))
        .chain(built.map(|(k, n)| ((crate::derive::NAMES.len() + k) as u32, format!("{n}, built"))))
        .collect()
}

/// The worst of each twin's measure, reported for every twin, and the twins over `limit`.
fn report(what: &str, mut measured: Vec<(String, f32)>, limit: f32) {
    measured.sort_by(|a, b| b.1.total_cmp(&a.1));
    let worst: Vec<String> = measured.iter().take(3).map(|(n, e)| format!("{n} {e:e}")).collect();
    eprintln!("{what}: {} twins, the worst {}", measured.len(), worst.join(", "));
    let over: Vec<_> = measured.iter().filter(|(_, e)| e.is_nan() || *e > limit).collect();
    assert!(over.is_empty(), "{what} over {limit:e}: {over:?}");
}

/// On the CPU, each tape is within 1e-5 of its compiled field at 10⁶ points in [-2, 2]³.
#[test]
fn tapes_match_their_compiled_fields_on_the_cpu() {
    let corpus = CpuBuild::load(built("fields")).expect("load the corpus");
    let measured = Mutex::new(Vec::new());
    par_each(
        &twins(),
        || corpus.start().expect("start the corpus"),
        |host, (k, name)| {
            let args = [Value::I32(*k as i32), Value::I32(17), Value::I32(POINTS as i32)];
            let worst = one_f32(host, "tape_error_cpu", &args);
            measured.lock().unwrap().push((name.clone(), worst));
        },
    );
    report("tape values", measured.into_inner().unwrap(), TOLERANCE);
}

/// The gradient of each tape (derived through `interpret`) is within 3.4e-4 of its field's,
/// relative to the field's where it's over 1, at 10⁴ points.
#[test]
fn tape_gradients_match_their_fields() {
    let corpus = CpuBuild::load(built("fields")).expect("load the corpus");
    let measured = Mutex::new(Vec::new());
    par_each(
        &twins(),
        || corpus.start().expect("start the corpus"),
        |host, (k, name)| {
            let args = [Value::I32(*k as i32), Value::I32(23), Value::I32(10_000)];
            let worst = one_f32(host, "tape_gradient_error", &args);
            measured.lock().unwrap().push((name.clone(), worst));
        },
    );
    report("tape gradients", measured.into_inner().unwrap(), 3.4e-4);
}

/// On the GPU, the same as on the CPU, relative to the field's value past 1.
#[test]
#[ignore = "needs a GPU"]
fn tapes_match_their_compiled_fields_on_the_gpu() {
    let dir = built("fields");
    let mut host = Host::load(&dir).expect("load");
    let mut measured = Vec::new();
    for (k, name) in twins() {
        let args = [Value::I32(k as i32), Value::I32(17), Value::I32(POINTS as i32)];
        host.call_export("tape_values_gpu", &args).expect("dispatch");
        let made = host.buffers();
        let [.., tape, compiled] = made[..] else { panic!("two buffers") };
        let tape = f32s(&host.read_buffer(tape).expect("tape"));
        let compiled = f32s(&host.read_buffer(compiled).expect("compiled"));
        assert_eq!(tape.len(), POINTS as usize);
        // Relative where the field is past 1: WGSL lets a compiled field's multiply and add
        // be fused, which a tape's two operations aren't, and march's misses travel 10⁶.
        let off = |(a, b): (&f32, &f32)| (a - b).abs() / b.abs().max(1.0);
        let offs: Vec<f32> = tape.iter().zip(&compiled).map(off).collect();
        // March stops where a step comes within 0.001 of the sphere: where the GPU rounds the
        // two differently, they take a different number of steps (a step's 0.01, and its
        // distance). At most one point in a thousand.
        let worst = if name.starts_with("march") {
            let flips = offs.iter().filter(|e| **e > TOLERANCE).count();
            eprintln!("{name}: {flips} of {POINTS} points stop a step apart");
            assert!(flips <= POINTS as usize / 1000, "{name}: {flips} points stop apart");
            offs.iter().copied().filter(|e| *e <= TOLERANCE).fold(0.0f32, f32::max)
        } else {
            offs.iter().copied().fold(0.0f32, f32::max)
        };
        measured.push((name, worst));
    }
    report("tape values on the GPU", measured, TOLERANCE);
}

/// A creature's tape, the herd's grazer's, holds under 2,048 operations.
#[test]
fn a_creature_tapes_in_under_2048_operations() {
    let mut host = crate::cpu_host(&built("fields"));
    assert_eq!(one_u32(&mut host, "grazer_tape_len", &[]), 1131);
}

/// Pruning the blob's tape (35 operations: three spheres of 11, and two smooth minimums) to a
/// box drops the spheres that can't reach it, with their blends, and leaves the value in the box
/// unchanged.
#[test]
fn pruning_keeps_the_value_in_its_box() {
    let dir = built("fields");
    let mut host = crate::cpu_host(&dir);
    assert_eq!(one_u32(&mut host, "blob_tape_len", &[]), 35);
    // Beside the first and third spheres: the second, and its blend, go. Inside the third:
    // all but it go, and the last blend becomes a copy of it.
    for (b, want) in [(0, 23), (1, 12)] {
        let r = one_u32(&mut host, "prune_blob", &[Value::I32(b), Value::I32(10_000)]);
        let (len, differ) = (r >> 16, r & 0xffff);
        assert_eq!(differ, 0, "box {b}: the pruned tape differs at {differ} points");
        assert_eq!(len, want, "box {b}: the pruned tape's length");
    }
}
