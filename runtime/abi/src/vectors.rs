//! Test vectors for every host: batches, sequences, manifests, state hashes and line tables,
//! each with what this crate makes of it (the commands, or the error and its message).
//! Generated from this crate's own encoders and decoders and checked in as
//! `runtime/abi/vectors.json`; the browser runtime's tests run each one through its decoders
//! and must get the same results and the same messages, so the two hosts can't drift apart.

use crate::hash::StateHash;
use crate::lines::Lines;
use crate::manifest::{Manifest, Stage, UniformBlock, UniformSpace};
use crate::stream::{self, Command, Encoder, Sequencer, StreamError};
use serde_json::{Value, json};

/// The checked-in vectors, relative to the repository root.
pub const VECTORS_PATH: &str = "runtime/abi/vectors.json";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn kind(e: &StreamError) -> &'static str {
    match e {
        StreamError::TooShort { .. } => "TooShort",
        StreamError::BadMagic(_) => "BadMagic",
        StreamError::WrongVersion { .. } => "WrongVersion",
        StreamError::BodyLength { .. } => "BodyLength",
        StreamError::UnknownOpcode { .. } => "UnknownOpcode",
        StreamError::BadPayload { .. } => "BadPayload",
        StreamError::Sequence { .. } => "Sequence",
        StreamError::UnclosedPass => "UnclosedPass",
    }
}

fn error(e: &StreamError) -> Value {
    json!({ "kind": kind(e), "message": e.to_string() })
}

/// A command as the browser runtime decodes it.
fn command(c: &Command) -> Value {
    match c {
        Command::CreateBuffer { handle, size } => {
            json!({ "op": "CreateBuffer", "handle": handle, "size": size })
        }
        Command::WriteBuffer { handle, offset, data } => {
            json!({ "op": "WriteBuffer", "handle": handle, "offset": offset, "data": data })
        }
        Command::Dispatch { pipeline, groups, buffers, uniforms } => json!({
            "op": "Dispatch", "pipeline": pipeline, "groups": groups, "buffers": buffers,
            "uniforms": uniforms,
        }),
        Command::BeginScreenPass { clear } => json!({ "op": "BeginScreenPass", "clear": clear }),
        Command::Draw { pipeline, vertices, instances, buffers, uniforms } => json!({
            "op": "Draw", "pipeline": pipeline, "vertices": vertices, "instances": instances,
            "buffers": buffers, "uniforms": uniforms,
        }),
        Command::Present => json!({ "op": "Present" }),
        Command::DestroyBuffer { handle } => json!({ "op": "DestroyBuffer", "handle": handle }),
    }
}

/// A batch and what decoding it gives.
fn batch(name: &str, bytes: &[u8]) -> Value {
    let outcome = match stream::decode(bytes) {
        Ok(cmds) => json!({ "commands": cmds.iter().map(command).collect::<Vec<_>>() }),
        Err(e) => json!({ "error": error(&e) }),
    };
    json!({ "name": name, "bytes": hex(bytes), "outcome": outcome })
}

/// The golden batch: every command once.
pub fn golden_batch() -> Vec<u8> {
    Encoder::new()
        .create_buffer(7, 16)
        .write_buffer(7, 4, &[1, 2, 3, 4, 5, 6, 7, 8])
        .dispatch(0, [2, 1, 1], &[7], &[0xAA, 0xBB, 0xCC, 0xDD])
        .begin_screen_pass([0.0, 0.5, 1.0, 1.0])
        .draw(1, 3, 1, &[], &[1, 0, 0, 0, 2, 0, 0, 0])
        .present()
        .destroy_buffer(7)
        .finish()
}

fn batches() -> Vec<Value> {
    let h = stream::HEADER_LEN;
    let edit = |b: Vec<u8>, at: usize, x: u8| {
        let mut b = b;
        b[at] = x;
        b
    };
    let present = || Encoder::new().present().finish();
    vec![
        batch("golden", &golden_batch()),
        batch(
            "decodes what it encodes",
            &Encoder::new()
                .create_buffer(1, 64)
                .dispatch(2, [4, 2, 1], &[1, 3], &[9, 9, 9, 9])
                .begin_screen_pass([0.25, 0.5, 0.75, 1.0])
                .draw(0, 3, 2, &[1], &[])
                .present()
                .finish(),
        ),
        batch("empty", &Encoder::new().finish()),
        batch("too short", b"WRC"),
        batch("bad magic", &edit(present(), 0, b'X')),
        batch("another version", &edit(present(), 4, 9)),
        batch("body length", &edit(present(), 8, 99)),
        batch("unknown opcode", &edit(present(), h, 42)),
        batch("a truncated command header", &{
            let mut b = Vec::new();
            for w in [u32::from_le_bytes(stream::MAGIC), 1, 4, 6] {
                b.extend(w.to_le_bytes());
            }
            b
        }),
        batch("a zero-size buffer", &edit(Encoder::new().create_buffer(1, 4).finish(), h + 12, 0)),
        batch(
            "a uniform length that disagrees with the payload",
            &edit(Encoder::new().draw(0, 3, 1, &[], &[1, 2, 3, 4]).finish(), h + 8 + 16, 8),
        ),
        batch("a payload running past the batch", &{
            let mut b = present();
            b[h + 4] = 4;
            b[8] = 8;
            b
        }),
        batch(
            "a buffer list running past the payload",
            &edit(Encoder::new().dispatch(0, [1, 1, 1], &[], &[]).finish(), h + 8 + 16, 200),
        ),
        batch(
            "a write whose length disagrees with its data",
            &edit(Encoder::new().write_buffer(1, 0, &[0; 8]).finish(), h + 8 + 8, 4),
        ),
    ]
}

/// A batch's commands run through a sequencer, then the frame ended: the first error.
fn sequence(name: &str, bytes: &[u8]) -> Value {
    let cmds = stream::decode(bytes).expect("a sequence vector decodes");
    let mut s = Sequencer::new();
    let result = cmds.iter().try_for_each(|c| s.step(c)).and_then(|()| s.end_frame());
    let error = match result {
        Ok(()) => Value::Null,
        Err(e) => error(&e),
    };
    json!({ "name": name, "bytes": hex(bytes), "error": error })
}

fn sequences() -> Vec<Value> {
    let e = Encoder::new;
    vec![
        sequence("in order", &golden_batch()),
        sequence("a draw outside a pass", &e().draw(0, 3, 1, &[], &[]).finish()),
        sequence("a present outside a pass", &e().present().finish()),
        sequence(
            "a pass inside a pass",
            &e().begin_screen_pass([0.0; 4]).begin_screen_pass([0.0; 4]).finish(),
        ),
        sequence(
            "a buffer made in a pass",
            &e().begin_screen_pass([0.0; 4]).create_buffer(0, 4).finish(),
        ),
        sequence(
            "a dispatch in a pass",
            &e().begin_screen_pass([0.0; 4]).dispatch(0, [1; 3], &[], &[]).finish(),
        ),
        sequence("a pass left open", &e().begin_screen_pass([0.0; 4]).finish()),
        sequence(
            "a buffer destroyed in a pass",
            &e().create_buffer(0, 4).begin_screen_pass([0.0; 4]).destroy_buffer(0).finish(),
        ),
    ]
}

/// A manifest with one pipeline of each kind.
pub fn sample_manifest() -> Manifest {
    let mut m = Manifest::new("game.wasm");
    m.pipelines.push(crate::manifest::Pipeline {
        name: "sample".into(),
        shader: "pipeline_0.wgsl".into(),
        stage: Stage::Compute { entry: "main".into(), workgroup_size: [64, 1, 1] },
        uniform: Some(UniformBlock { binding: 0, size: 32, space: UniformSpace::Uniform }),
        buffers: vec![crate::manifest::BufferBinding {
            binding: 1,
            access: crate::manifest::Access::ReadWrite,
        }],
    });
    m.pipelines.push(crate::manifest::Pipeline {
        name: "cover+shade".into(),
        shader: "pipeline_1.wgsl".into(),
        stage: Stage::Render { vertex_entry: "vs".into(), fragment_entry: "fs".into() },
        uniform: None,
        buffers: vec![],
    });
    m
}

fn manifest(name: &str, json: String) -> Value {
    let error = match Manifest::parse(&json) {
        Ok(_) => Value::Null,
        Err(e) => Value::String(e.to_string()),
    };
    json!({ "name": name, "json": json, "error": error })
}

fn manifests() -> Vec<Value> {
    let golden = sample_manifest().to_json();
    let edited = |f: &dyn Fn(&mut Manifest)| {
        let mut m = sample_manifest();
        f(&mut m);
        m.to_json()
    };
    let compute = |m: &mut Manifest, size: [u32; 3]| {
        if let Stage::Compute { workgroup_size, .. } = &mut m.pipelines[0].stage {
            *workgroup_size = size;
        }
    };
    vec![
        manifest("golden", golden.clone()),
        manifest(
            "another manifest version",
            golden.replace("\"manifest_version\": 1", "\"manifest_version\": 2"),
        ),
        manifest(
            "another stream version",
            golden.replace("\"stream_version\": 2", "\"stream_version\": 9"),
        ),
        manifest("too wide a workgroup", edited(&|m| compute(m, [512, 1, 1]))),
        manifest("too many invocations", edited(&|m| compute(m, [16, 16, 2]))),
        manifest("a binding used twice", edited(&|m| m.pipelines[0].buffers[0].binding = 0)),
        manifest(
            "a uniform block that isn't a multiple of 16",
            edited(&|m| {
                m.pipelines[0].uniform =
                    Some(UniformBlock { binding: 0, size: 20, space: UniformSpace::Uniform })
            }),
        ),
        manifest(
            "a storage uniform block of 20 bytes",
            edited(&|m| {
                m.pipelines[0].uniform =
                    Some(UniformBlock { binding: 0, size: 20, space: UniformSpace::Storage })
            }),
        ),
        manifest(
            "a uniform size that isn't a multiple of 4",
            edited(&|m| {
                m.pipelines[0].uniform =
                    Some(UniformBlock { binding: 0, size: 6, space: UniformSpace::Storage })
            }),
        ),
        manifest(
            "too many storage buffers",
            edited(&|m| {
                m.pipelines[1].buffers = (0..9)
                    .map(|i| crate::manifest::BufferBinding {
                        binding: i,
                        access: crate::manifest::Access::Read,
                    })
                    .collect()
            }),
        ),
        manifest("no shader", edited(&|m| m.pipelines[1].shader = String::new())),
        manifest(
            "a missing entry point",
            edited(&|m| {
                if let Stage::Render { fragment_entry, .. } = &mut m.pipelines[1].stage {
                    fragment_entry.clear();
                }
            }),
        ),
        manifest("no WASM file", edited(&|m| m.wasm = String::new())),
    ]
}

fn hashes() -> Vec<Value> {
    let inputs: [&[u8]; 4] = [b"", b"a", b"foobar", &golden_batch()];
    inputs
        .iter()
        .map(|i| {
            let mut h = StateHash::new();
            h.update(i);
            json!({ "bytes": hex(i), "hash": h.hex() })
        })
        .collect()
}

fn line_tables() -> Vec<Value> {
    let lines = Lines::new(&[
        (10, None),
        (12, Some("main.wrela:3:5".into())),
        (20, Some("shapes/blob.wrela:40:9".into())),
        (300, None),
        (301, Some("main.wrela:3:5".into())),
    ]);
    let bytes = lines.encode();
    let at: Vec<Value> = [0, 10, 11, 12, 19, 20, 299, 300, 301, 100_000]
        .iter()
        .map(|&o| json!([o, lines.at(o)]))
        .collect();
    let mut truncated = bytes.clone();
    truncated.pop();
    let error = |b: &[u8]| Lines::decode(b).err().map_or(Value::Null, Value::String);
    vec![
        json!({ "name": "a table", "bytes": hex(&bytes), "at": at, "error": Value::Null }),
        json!({ "name": "a truncated table", "bytes": hex(&truncated), "at": [], "error": error(&truncated) }),
    ]
}

/// Every vector, as `runtime/abi/vectors.json` holds them.
pub fn vectors() -> String {
    let v = json!({
        "comment": "Generated by the wrela-abi crate (runtime/abi/src/vectors.rs); don't edit. \
                    Run `cargo run -p wrela-abi --bin gen-ts`.",
        "hashes": hashes(),
        "batches": batches(),
        "sequences": sequences(),
        "manifests": manifests(),
        "lines": line_tables(),
    });
    let mut s = serde_json::to_string_pretty(&v).expect("vectors serialize");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checked-in vectors are this crate's results (the browser runtime's tests read them).
    #[test]
    fn checked_in_copy_is_current() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let path = root.join(VECTORS_PATH);
        let actual = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            actual == vectors(),
            "{} is stale; run `cargo run -p wrela-abi --bin gen-ts`",
            path.display()
        );
    }
}
