//! GPU programs run by the native host: a draw whose shaders read buffers (bound in the order
//! the pipeline declares them), and `len()` of a storage buffer. Need a GPU:
//! `cargo test -p wrela-tests --test suite render:: -- --ignored`.

use std::path::PathBuf;
use wrela_host::{Host, Value};
use wrela_tests::{build, root};

#[test]
#[ignore = "needs a GPU"]
fn shaders_read_buffers() {
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("render");
    if let Err(e) = build(&root().join("compiler/tests/render"), &out) {
        panic!("doesn't build:\n{e}");
    }
    let run = Host::load(&out).expect("load").run_frames(&[0.0], 16, 4).expect("run");
    let px = |x: usize, y: usize| &run.frame[4 * (y * 16 + x)..4 * (y * 16 + x) + 4];
    // Half of red on the left, half of blue on the right (rgba8unorm rounds 127.5 up).
    assert_eq!(px(2, 1), [128, 0, 0, 128]);
    assert_eq!(px(12, 2), [0, 0, 128, 128]);
}

#[test]
#[ignore = "needs a GPU"]
fn lengths_of_runs_and_buffers() {
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("lengths");
    if let Err(e) = build(&root().join("compiler/tests/lengths"), &out) {
        panic!("doesn't build:\n{e}");
    }
    let mut host = Host::load(&out).expect("load");
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
