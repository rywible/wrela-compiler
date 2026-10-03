//! Test vectors for every host: batches, sequences, checks, manifests, state hashes and line
//! tables, each with what this crate makes of it (the commands, or the error and its message).
//! Generated from this crate's own encoders and decoders and checked in as
//! `runtime/abi/vectors.json`; the browser runtime's tests run each one through its decoders
//! and must get the same results and the same messages, so the two hosts can't drift apart.

use crate::check::{self, Checker};
use crate::hash::StateHash;
use crate::lines::Lines;
use crate::manifest::{
    Access, BufferBinding, Manifest, Pipeline, Stage, UniformBlock, UniformSpace,
};
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
pub(crate) fn golden_batch() -> Vec<u8> {
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
            for w in [u32::from_le_bytes(stream::MAGIC), stream::VERSION, 4, 6] {
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

/// The manifest the check vectors (and the native host's tests) run against: pipelines 0 and 2
/// render, 1 and 3 compute; each takes 16 uniform bytes; 0 and 1 bind a read-only buffer then a
/// read-write one, 2 and 3 two read-write ones.
pub fn check_manifest() -> Manifest {
    let mut m = Manifest::new("game.wasm");
    let shape = |name: &str, stage| Pipeline {
        name: name.into(),
        shader: format!("{name}.wgsl"),
        stage,
        uniform: Some(UniformBlock { binding: 0, size: 16, space: UniformSpace::Uniform }),
        buffers: vec![
            BufferBinding { binding: 1, access: Access::Read },
            BufferBinding { binding: 2, access: Access::ReadWrite },
        ],
    };
    let render = Stage::Render { vertex_entry: "vs".into(), fragment_entry: "fs".into() };
    m.pipelines.push(shape("draw", render.clone()));
    let compute = Stage::Compute { entry: "main".into(), workgroup_size: [64, 1, 1] };
    m.pipelines.push(shape("compute", compute.clone()));
    // Two writable bindings, which one command may not give the same buffer.
    let write_twice = |mut p: Pipeline| {
        p.buffers[0].access = Access::ReadWrite;
        p
    };
    m.pipelines.push(write_twice(shape("draw2", render)));
    m.pipelines.push(write_twice(shape("compute2", compute)));
    m
}

/// A batch's commands, which pass the sequencer, run through a [`Checker`]: the first error.
fn check(name: &str, manifest: &Manifest, bytes: &[u8]) -> Value {
    let cmds = stream::decode(bytes).expect("a check vector decodes");
    let mut s = Sequencer::new();
    let mut checker = Checker::new(manifest);
    let mut error = Value::Null;
    for c in &cmds {
        s.step(c).expect("a check vector is in order");
        if let Err(e) = checker.check(c) {
            error = json!({ "opcode": e.opcode.name(), "message": e.to_string() });
            break;
        }
    }
    json!({ "name": name, "bytes": hex(bytes), "error": error })
}

fn checks() -> Value {
    let m = check_manifest();
    let u = [0u8; 16];
    // Buffers 1 and 2, 64 bytes each, then `f`'s commands.
    let with_buffers = |f: &dyn Fn(&mut Encoder)| {
        let mut e = Encoder::new();
        e.create_buffer(1, 64).create_buffer(2, 64);
        f(&mut e);
        e.finish()
    };
    let max = check::MAX_BUFFER_SIZE;
    let cases = vec![
        check(
            "every command, used rightly",
            &m,
            &with_buffers(&|e| {
                e.write_buffer(1, 56, &[7; 8])
                    .dispatch(1, [65_535, 1, 1], &[1, 2], &u)
                    .begin_screen_pass([0.0; 4])
                    .draw(0, 3, 1, &[1, 2], &u)
                    .present()
                    .destroy_buffer(1)
                    .destroy_buffer(2);
            }),
        ),
        check("a handle used twice", &m, &with_buffers(&|e| _ = e.create_buffer(1, 4))),
        check(
            "a buffer over the size limit",
            &m,
            &Encoder::new().create_buffer(1, max).create_buffer(2, max + 4).finish(),
        ),
        check("a write to no buffer", &m, &Encoder::new().write_buffer(3, 0, &[1; 4]).finish()),
        check("a write past the end", &m, &with_buffers(&|e| _ = e.write_buffer(1, 60, &[1; 8]))),
        check("destroying no buffer", &m, &Encoder::new().destroy_buffer(5).finish()),
        check(
            "a buffer used after it's destroyed",
            &m,
            &with_buffers(&|e| _ = e.destroy_buffer(1).dispatch(1, [1; 3], &[1, 2], &u)),
        ),
        check("no such pipeline", &m, &with_buffers(&|e| _ = e.dispatch(7, [1; 3], &[1, 2], &u))),
        check(
            "a draw with a compute pipeline",
            &m,
            &with_buffers(&|e| _ = e.begin_screen_pass([0.0; 4]).draw(1, 3, 1, &[1, 2], &u)),
        ),
        check(
            "a dispatch with a render pipeline",
            &m,
            &with_buffers(&|e| _ = e.dispatch(0, [1; 3], &[1, 2], &u)),
        ),
        check("too few buffers", &m, &with_buffers(&|e| _ = e.dispatch(1, [1; 3], &[1], &u))),
        check(
            "the wrong number of uniform bytes",
            &m,
            &with_buffers(&|e| _ = e.dispatch(1, [1; 3], &[1, 2], &[0; 8])),
        ),
        check(
            "too many workgroups",
            &m,
            &with_buffers(&|e| _ = e.dispatch(1, [1, 65_536, 1], &[1, 2], &u)),
        ),
        check(
            "one buffer read and written in a dispatch",
            &m,
            &with_buffers(&|e| _ = e.dispatch(1, [1; 3], &[1, 1], &u)),
        ),
        check(
            "one buffer read and written across a screen pass",
            &m,
            &with_buffers(&|e| {
                e.begin_screen_pass([0.0; 4]).draw(0, 3, 1, &[1, 2], &u).draw(0, 3, 1, &[2, 1], &u);
            }),
        ),
        check(
            "one buffer written twice in a dispatch",
            &m,
            &with_buffers(&|e| _ = e.dispatch(3, [1; 3], &[1, 1], &u)),
        ),
        check(
            "one buffer written twice in a draw",
            &m,
            &with_buffers(&|e| _ = e.begin_screen_pass([0.0; 4]).draw(2, 3, 1, &[2, 2], &u)),
        ),
        check(
            "two draws in one screen pass may write one buffer",
            &m,
            &with_buffers(&|e| {
                e.begin_screen_pass([0.0; 4]).draw(2, 3, 1, &[1, 2], &u).draw(2, 3, 1, &[1, 2], &u);
            }),
        ),
        check(
            "a clear colour that isn't finite",
            &m,
            &with_buffers(&|e| _ = e.begin_screen_pass([0.0, 0.0, f32::INFINITY, f32::NAN])),
        ),
        check(
            "each dispatch is its own scope",
            &m,
            &with_buffers(&|e| {
                e.dispatch(1, [1; 3], &[1, 2], &u).dispatch(1, [1; 3], &[2, 1], &u);
            }),
        ),
    ];
    json!({ "manifest": m.to_json(), "batches": cases })
}

/// A manifest with one pipeline of each kind.
pub(crate) fn sample_manifest() -> Manifest {
    let mut m = Manifest::new("game.wasm");
    m.pipelines.push(Pipeline {
        name: "sample".into(),
        shader: "pipeline_0.wgsl".into(),
        stage: Stage::Compute { entry: "main".into(), workgroup_size: [64, 1, 1] },
        uniform: Some(UniformBlock { binding: 0, size: 32, space: UniformSpace::Uniform }),
        buffers: vec![BufferBinding { binding: 1, access: Access::ReadWrite }],
    });
    m.pipelines.push(Pipeline {
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

/// JSON that doesn't parse: each host rejects it in its own words.
fn malformed(name: &str, json: String) -> Value {
    assert!(serde_json::from_str::<Value>(&json).is_err(), "{name} parses");
    json!({ "name": name, "json": json, "error": Value::Null, "malformed": true })
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
            "a uniform block over 64 KiB",
            edited(&|m| {
                m.pipelines[0].uniform =
                    Some(UniformBlock { binding: 0, size: 65552, space: UniformSpace::Uniform })
            }),
        ),
        manifest(
            "a storage uniform block over 64 KiB",
            edited(&|m| {
                m.pipelines[0].uniform =
                    Some(UniformBlock { binding: 0, size: 65552, space: UniformSpace::Storage })
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
                m.pipelines[1].buffers =
                    (0..9).map(|i| BufferBinding { binding: i, access: Access::Read }).collect()
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
        // Each field is read one way: serde's other shapes are rejected.
        manifest("a kind as a number", golden.replace("\"kind\": \"compute\"", "\"kind\": 0")),
        manifest(
            "a uniform block as an array",
            golden.replace(
                "{\n        \"binding\": 0,\n        \"size\": 32,\n        \"space\": \"uniform\"\n      }",
                "[0, 32, \"uniform\"]",
            ),
        ),
        manifest(
            "a buffer binding as an array",
            golden.replace(
                "{\n          \"binding\": 1,\n          \"access\": \"read_write\"\n        }",
                "[1, \"read_write\"]",
            ),
        ),
        manifest(
            "a uniform space as an object",
            golden.replace("\"space\": \"uniform\"", "\"space\": {\"uniform\": null}"),
        ),
        manifest("a missing field", golden.replace("\"shader\": \"pipeline_1.wgsl\",", "")),
        manifest("not an object", "[1]".into()),
        manifest(
            "a version as a string",
            golden.replace("\"manifest_version\": 1", "\"manifest_version\": \"1\""),
        ),
        manifest("pipelines not in an array", golden.replace("\"pipelines\": [", "\"pipelines\": {\"x\": [").replace("  ]\n}", "  ]}\n}")),
        manifest("a string for a number", golden.replace("\"size\": 32", "\"size\": \"32\"")),
        manifest("a workgroup size of two", golden.replace("64,\n        1,\n        1", "64, 1")),
        // JSON can't tell 1 from 1.0, so neither host does.
        manifest(
            "whole numbers written with fractions",
            golden.replace("\"manifest_version\": 1", "\"manifest_version\": 1.0").replace("\"size\": 32", "\"size\": 3.2e1"),
        ),
        manifest("a negative number", golden.replace("\"binding\": 1", "\"binding\": -1")),
        manifest("a number past 32 bits", golden.replace("\"size\": 32", "\"size\": 4294967296")),
        manifest("a fraction", golden.replace("\"size\": 32", "\"size\": 32.5")),
        manifest("a large number", golden.replace("\"size\": 32", "\"size\": 1e21")),
        manifest("a small number", golden.replace("\"size\": 32", "\"size\": 1.5e-7")),
        manifest("a negative zero", golden.replace("\"binding\": 0", "\"binding\": -0.0")),
        manifest("a wrong kind", golden.replace("\"kind\": \"render\"", "\"kind\": \"draw\\n\"")),
        malformed("not JSON", golden.replace("}\n", "")),
        malformed("a byte-order mark", format!("\u{feff}{golden}")),
        malformed("a lone surrogate", golden.replace("\"sample\"", "\"\\ud800\"")),
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
    let lines = Lines::new([
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
    let error = |name: &str, b: &[u8]| {
        let error = Lines::decode(b).expect_err("an error vector fails");
        json!({ "name": name, "bytes": hex(b), "at": [], "error": error })
    };
    // A table with no entries and one location of these bytes.
    let location = |b: &[u8]| [&[1, b.len() as u8], b, &[0]].concat();
    let bom = Lines::new([(4, Some("\u{feff}main.wrela:1:1".into()))]);
    vec![
        json!({ "name": "a table", "bytes": hex(&bytes), "at": at, "error": Value::Null }),
        json!({
            "name": "a location starting with a byte-order mark",
            "bytes": hex(&bom.encode()),
            "at": [[4, bom.at(4)]],
            "error": Value::Null,
        }),
        error("a truncated table", &truncated),
        error("a location that isn't UTF-8", &location(&[b'a', 0xff])),
        // 2^32 and 2^35 - 1, in five bytes.
        error("a number over 32 bits", &[0x80, 0x80, 0x80, 0x80, 0x10, 0]),
        error("a number over 32 bits, at most", &[0xff, 0xff, 0xff, 0xff, 0x7f, 0]),
        error("a number over five bytes", &[0x80, 0x80, 0x80, 0x80, 0x80, 0x00, 0]),
        error("bytes after the entries", &[0, 0, 0]),
        error("an entry naming a missing location", &[0, 1, 4, 1]),
        error("entries out of order", &[0, 2, 5, 0, 4, 0]),
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
        "checks": checks(),
        "manifests": manifests(),
        "lines": line_tables(),
    });
    let mut s = serde_json::to_string_pretty(&v).expect("vectors serialize");
    s.push('\n');
    s
}
