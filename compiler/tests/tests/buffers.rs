//! A compiled program's buffers live until its next call: each call into the program first
//! destroys the buffers the call before it made (language.md §12), so a game that makes
//! buffers every frame doesn't accumulate them, and a call's results can still be read back
//! before the next one.

use std::path::PathBuf;
use wrela_abi::stream::{Command, decode};
use wrela_host::CpuHost;

const SRC: &str = "use std::gpu::{GlobalId, GpuBuffer, Slots, buffer, dispatch}

@compute(64)
fn fill(out: mut Slots<f32>, id: GlobalId) {
    out[id] = 1.0
}

pub fn frame(time: f32, width: u32, height: u32) {
    let out: GpuBuffer<f32> = buffer(64)
    dispatch(fill, groups: 1, out: out)
}

pub fn two() -> u32 {
    let a: GpuBuffer<f32> = buffer(4)
    let b: GpuBuffer<f32> = buffer(4)
    dispatch(fill, groups: 1, out: a)
    dispatch(fill, groups: 1, out: b)
    1
}
";

/// The buffer commands of every batch submitted since the last look: `+h` made, `-h` destroyed.
fn buffer_commands(host: &mut CpuHost) -> Vec<String> {
    let mut out = Vec::new();
    for batch in host.take_batches() {
        for c in decode(&batch).expect("a valid batch") {
            match c {
                Command::CreateBuffer { handle, .. } => out.push(format!("+{handle}")),
                Command::DestroyBuffer { handle } => out.push(format!("-{handle}")),
                _ => {}
            }
        }
    }
    out
}

#[test]
fn each_call_destroys_the_buffers_of_the_call_before() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("buffers");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("main.wrela"), SRC).expect("write");
    if let Err(e) = wrela_tests::build(&dir, &dir.join("build")) {
        panic!("doesn't build:\n{e}");
    }
    let mut host = CpuHost::load(dir.join("build")).expect("load");
    host.frame(0.0, 64, 64).expect("frame 0");
    assert_eq!(buffer_commands(&mut host), ["+0"]);
    for k in 1..4 {
        host.frame(k as f32 / 60.0, 64, 64).expect("frame");
        assert_eq!(buffer_commands(&mut host), [format!("-{}", k - 1), format!("+{k}")]);
    }
    host.call_export("two", &[]).expect("two");
    assert_eq!(buffer_commands(&mut host), ["-3", "+4", "+5"]);
    host.frame(1.0, 64, 64).expect("frame");
    assert_eq!(buffer_commands(&mut host), ["-4", "-5", "+6"]);
}
