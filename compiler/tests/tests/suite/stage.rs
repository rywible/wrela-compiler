//! `std::stage` (AC6, language.md §7, D-029): fields built at runtime as tapes, which
//! `interpret` evaluates, against the compiled fields they were built to match
//! (compiler/tests/fields: `sphere_tape`, `waves_tape`, `blob_tape`). Their derived gradients
//! and intervals are in the corpus's gradient and enclosure tests (derive.rs).

use crate::built;
use wrela_host::{CpuBuild, Host, Value};
use wrela_tests::{f32s, one_f32, one_u32};

const TWINS: [&str; 3] = ["sphere", "waves", "blob"];
const POINTS: u32 = 1_000_000;
const TOLERANCE: f32 = 1e-5;

/// On the CPU, each tape is within 1e-5 of its compiled field at 10⁶ points in [-2, 2]³.
#[test]
fn tapes_match_their_compiled_fields_on_the_cpu() {
    let dir = built("fields");
    let mut host = CpuBuild::load(&dir).expect("load").start_with(1).expect("start");
    for (k, name) in TWINS.iter().enumerate() {
        let args = [Value::I32(k as i32), Value::I32(17), Value::I32(POINTS as i32)];
        let worst = one_f32(&mut host, "tape_error_cpu", &args);
        assert!(worst <= TOLERANCE, "{name}: the tape is {worst} from the compiled field");
    }
}

/// On the GPU, the same.
#[test]
#[ignore = "needs a GPU"]
fn tapes_match_their_compiled_fields_on_the_gpu() {
    let dir = built("fields");
    let mut host = Host::load(&dir).expect("load");
    for (k, name) in TWINS.iter().enumerate() {
        let args = [Value::I32(k as i32), Value::I32(17), Value::I32(POINTS as i32)];
        host.call_export("tape_values_gpu", &args).expect("dispatch");
        let made = host.buffers();
        let [.., tape, compiled] = made[..] else { panic!("two buffers") };
        let tape = f32s(&host.read_buffer(tape).expect("tape"));
        let compiled = f32s(&host.read_buffer(compiled).expect("compiled"));
        assert_eq!(tape.len(), POINTS as usize);
        let worst = tape.iter().zip(&compiled).fold(0.0f32, |w, (a, b)| w.max((a - b).abs()));
        assert!(worst <= TOLERANCE, "{name}: the tape is {worst} from the compiled field");
    }
}

/// Pruning the blob's tape (35 operations: three spheres of 11, and two smooth minimums) to a
/// box drops the spheres that can't reach it, with their blends, and leaves the value in the box
/// unchanged.
#[test]
fn pruning_keeps_the_value_in_its_box() {
    let dir = built("fields");
    let mut host = CpuBuild::load(&dir).expect("load").start_with(1).expect("start");
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
