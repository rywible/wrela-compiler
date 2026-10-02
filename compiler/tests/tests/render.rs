//! A draw whose vertex and fragment shaders read buffers, run by the native host: the buffers
//! are bound in the order the pipeline declares them. Needs a GPU:
//! `cargo test -p wrela-tests --test render -- --ignored`.

use std::path::PathBuf;
use wrela_host::Host;
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
