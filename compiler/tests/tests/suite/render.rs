//! GPU programs run by the native host: a draw whose shaders read buffers (bound in the order
//! the pipeline declares them), `len()` of a storage buffer, and what the WGSL back end must
//! take care with (compiler/tests/shaders). Need a GPU:
//! `cargo test -p wrela-tests --test suite render:: -- --ignored`.

use crate::built;
use wrela_host::{Host, Value};

#[test]
#[ignore = "needs a GPU"]
fn shaders_read_buffers() {
    let run = Host::load(built("render")).expect("load").run_frames(&[0.0], 16, 4).expect("run");
    let px = |x: usize, y: usize| &run.frame[4 * (y * 16 + x)..4 * (y * 16 + x) + 4];
    // Half of red on the left, half of blue on the right (rgba8unorm rounds 127.5 up).
    assert_eq!(px(2, 1), [128, 0, 0, 128]);
    assert_eq!(px(12, 2), [0, 0, 128, 128]);
}

#[test]
#[ignore = "needs a GPU"]
fn lengths_of_runs_and_buffers() {
    let mut host = Host::load(built("lengths")).expect("load");
    // On the CPU: a run's length, and an array's.
    match host.call_export("lens", &[]).expect("lens").as_slice() {
        [Value::F32(sum), Value::F32(len), _] => assert_eq!((*sum, *len), (10.0, 4.0)),
        other => panic!("lens returned {other:?}"),
    }
    // On the GPU: a buffer of 10 elements has length 10, whatever the host rounds its size to.
    host.run_frames(&[0.0], 4, 4).expect("run");
    let counts = wrela_tests::u32s(&host.read_buffer(1).expect("out"));
    assert_eq!(&counts[..4], &[10, 11, 12, 13]);
}

/// Without a GPU too: the WGSL back end takes every shader (naga validates it, and parses the
/// WGSL it writes).
#[test]
fn shaders_build() {
    built("shaders");
}

#[test]
#[ignore = "needs a GPU"]
fn shaders_get_the_values_the_program_means() {
    let mut host = Host::load(built("shaders")).expect("load");
    let run = host.run_frames(&[0.0], 16, 4).expect("run");
    let px = |x: usize, y: usize| &run.frame[4 * (y * 16 + x)..4 * (y * 16 + x) + 4];
    // A vertex output of only a `ClipPosition` covers the screen; the generic pair, drawn with
    // `f32` then `vec4`, the bottom left and bottom right.
    assert_eq!(px(3, 0), [0, 0, 255, 255]);
    assert_eq!(px(3, 3), [255, 0, 0, 255]);
    assert_eq!(px(12, 3), [0, 255, 0, 255]);
    let got = wrela_tests::f32s(&host.read_buffer(0).expect("out"));
    // Integer `/ 0` gives the dividend, a shift takes its amount modulo 32, and `1.0 / 0.0` and
    // `3e38 * 10.0` are infinite; then `^` of bools, `-` of a matrix, `select` of enums, an enum
    // in a struct, a uniform after one with no value, the last row of a 80 KB table, a buffer's
    // length through a closure, and a second `GlobalId`.
    let want = [7.0, 2.0, 5.0, 6.0, 1.0, -5.0, 6.0, 7.5, 3.0, 9.0, 3.0, 11.0];
    assert_eq!(&got[..want.len()], &want);
}
