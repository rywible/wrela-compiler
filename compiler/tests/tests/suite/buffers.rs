//! A compiled program's buffers live until its next call: each call into the program first
//! destroys the buffers the call before it made (language.md §12), so a game that makes
//! buffers every frame doesn't accumulate them, and a call's results can still be read back
//! before the next one. An empty buffer has room for one element: WebGPU can't bind less.

use crate::package;
use wrela_abi::stream::{Command, decode};
use wrela_host::{CpuHost, Value};
use wrela_tests::must_build;

const SRC: &str = "use std::gpu::{GlobalId, GpuBuffer, Slots, buffer, dispatch}

@compute(64)
fn fill(out: mut Slots<f32>, id: GlobalId) {
    out[id] = 1.0
}

pub fn frame(time: f32, width: u32, height: u32) {
    let out: GpuBuffer<f32> = buffer(64)
    dispatch(fill, groups: 1, out: out)
}

pub fn empty() -> u32 {
    let a: GpuBuffer<vec4> = buffer(0)
    let b: GpuBuffer<f32> = buffer(0)
    let c: GpuBuffer<vec4> = buffer(3)
    1
}

pub fn two() -> u32 {
    let a: GpuBuffer<f32> = buffer(4)
    let b: GpuBuffer<f32> = buffer(4)
    dispatch(fill, groups: 1, out: a)
    dispatch(fill, groups: 1, out: b)
    1
}
";

/// Builds `src` as the one-file package `name` and loads it on the CPU.
fn load(name: &str, src: &str) -> CpuHost {
    let dir = package(name, src);
    must_build(&dir, &dir.join("build"));
    CpuHost::load(dir.join("build")).expect("load")
}

/// What `pick` takes from the commands of every batch submitted since the last look.
fn commands<T>(host: &mut CpuHost, pick: impl FnMut(Command) -> Option<T>) -> Vec<T> {
    let batches = host.take_batches();
    batches.iter().flat_map(|b| decode(b).expect("a valid batch")).filter_map(pick).collect()
}

/// The buffer commands of every batch submitted since the last look: `+h` made, `-h` destroyed.
fn buffer_commands(host: &mut CpuHost) -> Vec<String> {
    commands(host, |c| match c {
        Command::CreateBuffer { handle, .. } => Some(format!("+{handle}")),
        Command::DestroyBuffer { handle } => Some(format!("-{handle}")),
        _ => None,
    })
}

#[test]
fn each_call_destroys_the_buffers_of_the_call_before() {
    let mut host = load("buffers", SRC);
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

#[test]
fn an_empty_buffer_has_room_for_one_element() {
    let mut host = load("empty-buffers", SRC);
    host.call_export("empty", &[]).expect("empty");
    let sizes = commands(&mut host, |c| match c {
        Command::CreateBuffer { size, .. } => Some(size),
        _ => None,
    });
    assert_eq!(sizes, [16, 4, 48]);
}

#[test]
fn a_unit_argument_keeps_the_dispatch() {
    // A `()` argument is a value with nothing in it: the dispatch it's part of is recorded.
    let src = "use std::gpu::{GlobalId, GpuBuffer, Slots, buffer, dispatch}

@compute(64)
fn fill(out: mut Slots<f32>, id: GlobalId, nothing: ()) {
    out[id] = 2.0
}

pub fn frame(time: f32, width: u32, height: u32) {
    let out: GpuBuffer<f32> = buffer(64)
    dispatch(fill, groups: 1, out: out, nothing: ())
}
";
    let mut host = load("unit_dispatch", src);
    host.frame(0.0, 64, 64).expect("frame");
    let dispatches = commands(&mut host, |c| match c {
        Command::Dispatch { pipeline, .. } => Some(pipeline),
        _ => None,
    });
    assert_eq!(dispatches, [0]);
}

#[test]
fn a_long_repeat_has_zeroed_padding() {
    // `[v; N]` past 64 elements is filled in a loop. Its padding (a `vec3`'s fourth float) is
    // still zeros, not what an earlier call left on the stack: a value's bytes are a function of
    // its fields (§11), so the bytes a program submits don't depend on what ran before.
    let src = "use std::gpu::{GpuBuffer, buffer, write}

pub fn dirty(x: f32) -> f32 {
    let a = [vec4(x); 400]
    a[399].w
}

pub fn fill() -> u32 {
    let b: GpuBuffer<vec3> = buffer(65)
    write(b, 0, [vec3(1.0); 65])
    1
}
";
    let mut host = load("repeat_padding", src);
    host.call_export("dirty", &[Value::F32(7.0)]).expect("dirty");
    host.call_export("fill", &[]).expect("fill");
    let data = commands(&mut host, |c| match c {
        Command::WriteBuffer { data, .. } => Some(data.to_vec()),
        _ => None,
    });
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].len(), 65 * 16);
    for (i, e) in data[0].chunks(16).enumerate() {
        assert_eq!(e[12..], [0; 4], "element {i}'s padding");
    }
}

#[test]
fn a_rejected_batch_isnt_sent_again() {
    // The host rejects a buffer too large to make. The next call doesn't send the rejected batch
    // again, nor destroy the buffer the host never made: it starts afresh.
    let src = "use std::gpu::{GpuBuffer, buffer}

pub fn huge() -> u32 {
    let b: GpuBuffer<f32> = buffer(1073741823)
    1
}

pub fn small() -> u32 {
    let c: GpuBuffer<f32> = buffer(2)
    1
}
";
    let mut host = load("rejected_batch", src);
    assert!(host.call_export("huge", &[]).is_err());
    let _ = host.take_batches();
    for _ in 0..2 {
        host.call_export("small", &[]).expect("small");
    }
}

#[test]
fn a_trapped_call_still_releases_the_buffers_before_it() {
    // `made` makes buffer 0. `trapped` destroys it and makes buffer 1, then traps before it
    // submits either. The next call destroys buffer 0 (the host still has it) and doesn't
    // destroy buffer 1 (the host never made it).
    let src = "use std::gpu::{GpuBuffer, buffer}

pub fn made() -> u32 {
    let b: GpuBuffer<f32> = buffer(4)
    1
}

pub fn trapped(x: u32) -> u32 {
    let b: GpuBuffer<f32> = buffer(4)
    1 / x
}
";
    let mut host = load("trapped_release", src);
    host.call_export("made", &[]).expect("made");
    assert_eq!(buffer_commands(&mut host), ["+0"]);
    assert!(host.call_export("trapped", &[Value::I32(0)]).is_err());
    assert_eq!(buffer_commands(&mut host), Vec::<String>::new());
    host.call_export("made", &[]).expect("made");
    assert_eq!(buffer_commands(&mut host), ["-0", "+1"]);
}
