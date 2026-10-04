//! A GPU buffer is an owned value (language.md §6.13, §12): dropping it records its
//! destruction, in order with the work that uses it, and the host releases it when that work is
//! done. A buffer in the program's state lasts with the state, so a game that makes buffers
//! every frame doesn't accumulate them, and one it keeps is there in the next call. A span
//! binds part of a buffer. An empty buffer has room for one element: WebGPU can't bind less.

use crate::package;
use wrela_abi::stream::{Binding, Command, decode};
use wrela_host::{CpuHost, Value};
use wrela_tests::must_build;

const SRC: &str = "use std::gpu::{GlobalId, GpuBuffer, Slots, buffer, dispatch}

@compute(64)
fn fill(out: mut Slots<f32>, id: GlobalId) {
    out[id] = 1.0
}

pub fn frame(time: f32, width: u32, height: u32) {
    var out: GpuBuffer<f32> = buffer(64)
    dispatch(fill.bind(out: mut out), groups: 1)
}

pub fn empty() -> u32 {
    let a: GpuBuffer<vec4> = buffer(0)
    let b: GpuBuffer<f32> = buffer(0)
    let c: GpuBuffer<vec4> = buffer(3)
    1
}

pub fn two() -> u32 {
    var a: GpuBuffer<f32> = buffer(4)
    var b: GpuBuffer<f32> = buffer(4)
    dispatch(fill.bind(out: mut a), groups: 1)
    dispatch(fill.bind(out: mut b), groups: 1)
    1
}

pub fn moved() -> u32 {
    let a: GpuBuffer<f32> = buffer(4)
    let b = take a
    b.len()
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
fn dropping_a_buffer_destroys_it() {
    let mut host = load("buffers", SRC);
    // Each frame's buffer is dropped at the end of the frame. Handles aren't used again.
    for k in 1..4 {
        host.frame(k as f32 / 60.0, 64, 64).expect("frame");
        assert_eq!(buffer_commands(&mut host), [format!("+{k}"), format!("-{k}")]);
    }
    // In the reverse of the order they were made, after the work that uses them.
    host.call_export("two", &[]).expect("two");
    let order = commands(&mut host, |c| match c {
        Command::CreateBuffer { handle, .. } => Some(format!("+{handle}")),
        Command::DestroyBuffer { handle } => Some(format!("-{handle}")),
        Command::Dispatch { bindings, .. } => Some(format!("dispatch {}", bindings[0].handle)),
        _ => None,
    });
    assert_eq!(order, ["+4", "+5", "dispatch 4", "dispatch 5", "-5", "-4"]);
    // A buffer moved to another binding is destroyed once.
    assert_eq!(host.call_export("moved", &[]).expect("moved"), [Value::I32(4)]);
    assert_eq!(buffer_commands(&mut host), ["+6", "-6"]);
}

#[test]
fn a_buffer_in_the_state_lasts_with_it() {
    let src = "use std::gpu::{GlobalId, GpuBuffer, Slots, buffer, dispatch}

@compute(64)
fn fill(out: mut Slots<f32>, id: GlobalId) {
    out[id] = 1.0
}

pub struct Kept {
    out: GpuBuffer<f32>,
}

pub fn init() -> Kept {
    Kept { out: buffer(64) }
}

pub fn frame(state: mut Kept, time: f32, width: u32, height: u32) {
    dispatch(fill.bind(out: mut state.out), groups: 1)
}

pub fn replace(state: mut Kept) -> u32 {
    state.out = buffer(128)
    state.out.len()
}
";
    let mut host = load("kept_buffer", src);
    host.frame(0.0, 64, 64).expect("frame");
    assert_eq!(buffer_commands(&mut host), ["+1"]);
    host.frame(1.0 / 60.0, 64, 64).expect("frame");
    assert_eq!(buffer_commands(&mut host), Vec::<String>::new());
    // Assigning a new buffer drops the old one, after the new one is made.
    assert_eq!(host.call_export("replace", &[]).expect("replace"), [Value::I32(128)]);
    assert_eq!(buffer_commands(&mut host), ["+2", "-1"]);
}

#[test]
fn a_span_binds_part_of_a_buffer() {
    // A span binds its range: at a byte offset and with a byte size. Two spans of one buffer
    // bound together are rejected when the program is compiled (conformance gpu_spans_reject).
    let src = "use std::gpu::{GlobalId, GpuBuffer, GpuSpan, Slots, buffer, copy, dispatch}

@compute(64)
fn twice(input: [f32], out: mut Slots<f32>, id: GlobalId) {
    out[id] = input[id.x] * 2.0
}

pub fn frame(time: f32, width: u32, height: u32) {
    var a: GpuBuffer<f32> = buffer(256)
    var b: GpuBuffer<f32> = buffer(128)
    dispatch(twice.bind(input: a.span(64, 64), out: b.span_mut(64, 64)), groups: 1)
    copy(from: a.span(0, 64), to: b.span_mut(0, 64))
    dispatch(twice.bind(input: a, out: mut b), groups: 1)
}
";
    let mut host = load("spans", src);
    host.frame(0.0, 64, 64).expect("frame");
    let work = commands(&mut host, |c| match c {
        Command::Dispatch { bindings, .. } => Some(format!("{bindings:?}")),
        Command::CopyBuffer { source, source_offset, destination, destination_offset, size } => {
            Some(format!(
                "copy {source}@{source_offset} to {destination}@{destination_offset}, {size}"
            ))
        }
        _ => None,
    });
    let b = |handle, offset, size| Binding { handle, offset, size };
    assert_eq!(
        work,
        [
            format!("{:?}", [b(1, 256, 256), b(2, 256, 256)]),
            "copy 1@0 to 2@0, 256".to_string(),
            format!("{:?}", [b(1, 0, 1024), b(2, 0, 512)]),
        ]
    );
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
    var out: GpuBuffer<f32> = buffer(64)
    dispatch(fill.bind(out: mut out, nothing: ()), groups: 1)
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
    var b: GpuBuffer<vec3> = buffer(65)
    write(mut b, 0, [vec3(1.0); 65])
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
    // again: it starts afresh.
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
fn a_trapped_call_submits_nothing() {
    // `trapped` makes a buffer, then traps before it submits it or drops it. The host never
    // saw the buffer, so the next call makes its handle again.
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
    assert_eq!(buffer_commands(&mut host), ["+1", "-1"]);
    assert!(host.call_export("trapped", &[Value::I32(0)]).is_err());
    assert_eq!(buffer_commands(&mut host), Vec::<String>::new());
    host.call_export("made", &[]).expect("made");
    assert_eq!(buffer_commands(&mut host), ["+2", "-2"]);
}

#[test]
fn nans_are_canonical_where_bytes_are_observed() {
    // A NaN's bits can't be told apart in the language except where its bytes are seen: an
    // upload, a uniform, a bit cast. There each NaN is the canonical one (§11), whatever bits
    // the arithmetic or a bit cast left.
    let src = "use std::gpu::{GlobalId, GpuBuffer, Slots, buffer, dispatch, write}

struct Odd: Copy + GpuData {
    a: f32,
    v: vec2,
}

@compute(64)
fn fill(out: mut Slots<f32>, id: GlobalId, odd: Odd) {
    out[id] = odd.a
}

fn odd_nan() -> f32 {
    bitcast_f32(0xffc00123)
}

pub fn frame(time: f32, width: u32, height: u32) {
    var out: GpuBuffer<f32> = buffer(64)
    write(mut out, 0, [odd_nan(), 1.0, odd_nan()])
    dispatch(fill.bind(out: mut out, odd: Odd { a: odd_nan(), v: vec2(odd_nan(), 2.0) }), groups: 1)
}

pub fn bits() -> u32 {
    bitcast_u32(odd_nan())
}

pub fn bits64() -> u64 {
    bitcast_u64(f64(odd_nan()) * 2.0)
}
";
    let mut host = load("canonical_nans", src);
    let canonical = 0x7fc0_0000u32.to_le_bytes();
    assert_eq!(host.call_export("bits", &[]).expect("bits"), [Value::I32(0x7fc0_0000)]);
    assert_eq!(
        host.call_export("bits64", &[]).expect("bits64"),
        [Value::I64(0x7ff8_0000_0000_0000)]
    );
    host.frame(0.0, 64, 64).expect("frame");
    let uploads = commands(&mut host, |c| match c {
        Command::WriteBuffer { data, .. } => Some(data.to_vec()),
        Command::Dispatch { uniforms, .. } => Some(uniforms.to_vec()),
        _ => None,
    });
    let written: Vec<u8> = [canonical, 1.0f32.to_le_bytes(), canonical].concat();
    assert_eq!(uploads[0], written);
    // The uniform: `a`, padding, then `v`'s two floats (WGSL's layout).
    assert_eq!(&uploads[1][0..4], &canonical);
    assert_eq!(&uploads[1][8..12], &canonical);
    assert_eq!(&uploads[1][12..16], &2.0f32.to_le_bytes());
}
