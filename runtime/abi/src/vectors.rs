//! Test vectors for every host: batches, sequences, checks, manifests, state hashes and line
//! tables, each with what this crate makes of it (the commands, or the error and its message).
//! Generated from this crate's own encoders and decoders and checked in as
//! `runtime/abi/vectors.json`; the browser runtime's tests run each one through its decoders
//! and must get the same results and the same messages, so the two hosts can't drift apart.

use crate::check::Checker;
use crate::hash::StateHash;
use crate::lines::Lines;
use crate::manifest::{
    BindingKind, BindingStage, Cull, DepthBias, DepthState, Manifest, Pipeline, RenderTarget,
    ResourceBinding, Stage, UniformBlock, UniformSpace,
};
use crate::stream::{
    self, Binding, Command, Compare, Encoder, NONE, Pass, SCREEN, Sequencer, StreamError,
    TextureFormat,
};
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

fn bindings(bs: &[Binding]) -> Value {
    bs.iter().map(|b| json!({ "handle": b.handle, "offset": b.offset, "size": b.size })).collect()
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
        Command::Dispatch { pipeline, groups, bindings: bs, uniforms } => json!({
            "op": "Dispatch", "pipeline": pipeline, "groups": groups, "bindings": bindings(bs),
            "uniforms": uniforms,
        }),
        Command::BeginScreenPass { clear } => json!({ "op": "BeginScreenPass", "clear": clear }),
        Command::Draw { pipeline, vertices, instances, bindings: bs, uniforms } => json!({
            "op": "Draw", "pipeline": pipeline, "vertices": vertices, "instances": instances,
            "bindings": bindings(bs), "uniforms": uniforms,
        }),
        Command::Present => json!({ "op": "Present" }),
        Command::DestroyBuffer { handle } => json!({ "op": "DestroyBuffer", "handle": handle }),
        Command::CopyBuffer { source, source_offset, destination, destination_offset, size } => {
            json!({
                "op": "CopyBuffer", "source": source, "sourceOffset": source_offset,
                "destination": destination, "destinationOffset": destination_offset, "size": size,
            })
        }
        Command::CreateTexture { handle, width, height, format, writable, depth } => json!({
            "op": "CreateTexture", "handle": handle, "width": width, "height": height,
            "format": format.name(), "writable": writable, "depth": depth,
        }),
        Command::WriteTexture { handle, x, y, width, height, data } => json!({
            "op": "WriteTexture", "handle": handle, "x": x, "y": y, "width": width,
            "height": height, "data": data,
        }),
        Command::DestroyTexture { handle } => json!({ "op": "DestroyTexture", "handle": handle }),
        Command::CreateSampler { handle, linear, repeat, compare } => json!({
            "op": "CreateSampler", "handle": handle, "linear": linear, "repeat": repeat,
            "compare": compare.map(Compare::name),
        }),
        Command::DestroySampler { handle } => json!({ "op": "DestroySampler", "handle": handle }),
        Command::BeginPass(p) => json!({ "op": "BeginPass", "pass": pass_json(p) }),
        Command::EndPass => json!({ "op": "EndPass" }),
        Command::DispatchIndirect { pipeline, arguments, offset, bindings: bs, uniforms } => {
            json!({
                "op": "DispatchIndirect", "pipeline": pipeline, "arguments": arguments,
                "offset": offset, "bindings": bindings(bs), "uniforms": uniforms,
            })
        }
        Command::DrawIndirect { pipeline, arguments, offset, bindings: bs, uniforms } => json!({
            "op": "DrawIndirect", "pipeline": pipeline, "arguments": arguments,
            "offset": offset, "bindings": bindings(bs), "uniforms": uniforms,
        }),
        Command::DrawIndexedIndirect {
            pipeline,
            indices,
            index_offset,
            index_size,
            arguments,
            offset,
            bindings: bs,
            uniforms,
        } => json!({
            "op": "DrawIndexedIndirect", "pipeline": pipeline, "indices": indices,
            "index_offset": index_offset, "index_size": index_size, "arguments": arguments,
            "offset": offset, "bindings": bindings(bs), "uniforms": uniforms,
        }),
        Command::ReadBuffer { request, handle, offset, size } => json!({
            "op": "ReadBuffer", "request": request, "handle": handle, "offset": offset,
            "size": size,
        }),
        Command::StorageRead { request, path } => {
            json!({ "op": "StorageRead", "request": request, "path": path })
        }
        Command::StorageWrite { request, path, data } => {
            json!({ "op": "StorageWrite", "request": request, "path": path, "data": data })
        }
        Command::Fetch { request, url } => json!({ "op": "Fetch", "request": request, "url": url }),
        Command::Post { request, url, body } => {
            json!({ "op": "Post", "request": request, "url": url, "body": body })
        }
        Command::Log { text } => json!({ "op": "Log", "text": text }),
        Command::Label { name } => json!({ "op": "Label", "name": name }),
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

/// An offscreen pass into texture 8, with depth texture 9.
fn offscreen() -> Pass {
    Pass {
        color: 8,
        keep_color: false,
        join: false,
        clear: [0.0, 0.0, 0.0, 1.0],
        depth: 9,
        keep_depth: false,
        clear_depth: 1.0,
    }
}

/// The golden batch: every command once.
pub(crate) fn golden_batch() -> Vec<u8> {
    Encoder::new()
        .create_buffer(7, 16)
        .write_buffer(7, 4, &[1, 2, 3, 4, 5, 6, 7, 8])
        .dispatch(0, [2, 1, 1], &[Binding::range(7, 0, 16)], &[0xAA, 0xBB, 0xCC, 0xDD])
        .begin_screen_pass([0.0, 0.5, 1.0, 1.0])
        .draw(1, 3, 1, &[], &[1, 0, 0, 0, 2, 0, 0, 0])
        .present()
        .destroy_buffer(7)
        .copy_buffer(1, 4, 2, 8, 12)
        .create_texture(8, 2, 1, TextureFormat::Rgba8, false)
        .write_texture(8, [0, 0], [2, 1], &[1, 2, 3, 4, 5, 6, 7, 8])
        .destroy_texture(8)
        .create_sampler(10, true, false, Some(Compare::Less))
        .destroy_sampler(10)
        .begin_pass(offscreen())
        .end_pass()
        .dispatch_indirect(0, 3, 4, &[Binding::of(5)], &[])
        .draw_indirect(1, 3, 16, &[], &[])
        .read_buffer(1, 3, 0, 8)
        .storage_read(2, "saves/slot1")
        .storage_write(3, "saves/a", &[1, 2, 3, 4, 5])
        .fetch(4, "data/level.bin")
        .log("frame 3: 2 grazers, é")
        .post(5, "studio/edit", &[123, 125])
        .draw_indexed_indirect(1, [3, 0, 12], 3, 16, &[], &[])
        .label("terrain")
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
    let b = |h: u32| Binding::range(h, 0, 64);
    vec![
        batch("golden", &golden_batch()),
        batch(
            "decodes what it encodes",
            &Encoder::new()
                .create_buffer(1, 64)
                .dispatch(2, [4, 2, 1], &[b(1), b(3)], &[9, 9, 9, 9])
                .begin_screen_pass([0.25, 0.5, 0.75, 1.0])
                .draw(0, 3, 2, &[b(1)], &[])
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
            "a binding list running past the payload",
            &edit(Encoder::new().dispatch(0, [1, 1, 1], &[], &[]).finish(), h + 8 + 16, 200),
        ),
        batch(
            "a write whose length disagrees with its data",
            &edit(Encoder::new().write_buffer(1, 0, &[0; 8]).finish(), h + 8 + 8, 4),
        ),
        batch(
            "an unknown texture format",
            &edit(
                Encoder::new().create_texture(1, 4, 4, TextureFormat::Rgba8, false).finish(),
                h + 8 + 12,
                9,
            ),
        ),
        batch(
            "a 3D texture of no depth",
            &edit(
                Encoder::new().create_texture_3d(1, 4, 4, 4, TextureFormat::Rgba8, false).finish(),
                h + 8 + 16,
                0,
            ),
        ),
        batch(
            "a 3D depth texture",
            &Encoder::new()
                .create_texture_3d(1, 4, 4, 4, TextureFormat::Depth32Float, false)
                .finish(),
        ),
        batch(
            "a zero-width texture",
            &edit(
                Encoder::new().create_texture(1, 4, 4, TextureFormat::Rgba8, false).finish(),
                h + 8 + 4,
                0,
            ),
        ),
        batch(
            "an unknown comparison",
            &edit(Encoder::new().create_sampler(1, true, false, None).finish(), h + 8 + 12, 9),
        ),
        batch(
            "a load that isn't 0 or 1",
            &edit(Encoder::new().begin_pass(offscreen()).finish(), h + 8 + 4, 2),
        ),
        batch(
            "an unaligned copy",
            &edit(Encoder::new().copy_buffer(1, 0, 2, 0, 8).finish(), h + 8 + 16, 6),
        ),
        batch("a path that isn't UTF-8", &{
            let mut b = Encoder::new().storage_read(1, "ab").finish();
            b[h + 16] = 0xFF;
            b
        }),
        batch(
            "a path length that disagrees with the payload",
            &edit(Encoder::new().fetch(1, "data").finish(), h + 12, 9),
        ),
        batch(
            "a post body length that disagrees with the payload",
            &edit(Encoder::new().post(1, "x", &[1, 2]).finish(), h + 16, 9),
        ),
        batch(
            "a log line that isn't UTF-8",
            &edit(Encoder::new().log("ok").finish(), h + 12, 0xFF),
        ),
        batch(
            "an unaligned indirect offset",
            &edit(Encoder::new().draw_indirect(0, 1, 0, &[], &[]).finish(), h + 8 + 8, 2),
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
    let screen = Pass { color: SCREEN, depth: NONE, ..offscreen() };
    vec![
        sequence("in order", &golden_batch()),
        sequence("a draw outside a pass", &e().draw(0, 3, 1, &[], &[]).finish()),
        sequence("a present outside a pass", &e().present().finish()),
        sequence(
            "a pass inside a pass",
            &e().begin_screen_pass([0.0; 4]).begin_pass(offscreen()).finish(),
        ),
        sequence(
            "a buffer made in a pass",
            &e().begin_screen_pass([0.0; 4]).create_buffer(0, 4).finish(),
        ),
        sequence(
            "a label in a pass",
            &e().begin_pass(offscreen()).label("terrain").end_pass().finish(),
        ),
        sequence(
            "a dispatch in a pass",
            &e().begin_pass(offscreen()).dispatch(0, [1; 3], &[], &[]).finish(),
        ),
        sequence("a pass left open", &e().begin_pass(offscreen()).finish()),
        sequence(
            "a buffer destroyed in a pass",
            &e().create_buffer(0, 4).begin_screen_pass([0.0; 4]).destroy_buffer(0).finish(),
        ),
        sequence(
            "an offscreen pass ended by Present",
            &e().begin_pass(offscreen()).present().finish(),
        ),
        sequence(
            "a screen pass ended by EndPass",
            &e().begin_screen_pass([0.0; 4]).end_pass().finish(),
        ),
        sequence(
            "a pass on the screen with depth",
            &e().begin_pass(Pass { depth: 9, ..screen })
                .draw_indirect(0, 1, 0, &[], &[])
                .present()
                .finish(),
        ),
        sequence(
            "a copy in a pass",
            &e().begin_pass(offscreen()).copy_buffer(1, 0, 2, 0, 4).finish(),
        ),
        sequence(
            "a readback in a pass",
            &e().begin_screen_pass([0.0; 4]).read_buffer(0, 1, 0, 4).finish(),
        ),
    ]
}

/// The format a hand-made binding of `kind` names: the vectors' sampled textures are `Rgba8`,
/// and their storage textures `Rgba16Float`.
fn format_of(kind: BindingKind) -> Option<TextureFormat> {
    match kind {
        BindingKind::Texture | BindingKind::Texture3d => Some(TextureFormat::Rgba8),
        BindingKind::StorageTexture | BindingKind::StorageTexture3d => {
            Some(TextureFormat::Rgba16Float)
        }
        _ => None,
    }
}

/// A hand-made binding at `binding` of `kind`, seen by every shader (its format `format_of`).
fn bind(binding: u32, kind: BindingKind) -> ResourceBinding {
    ResourceBinding { binding, kind, stage: BindingStage::Both, format: format_of(kind) }
}

/// The manifest the check vectors (and the native host's tests) run against: pipelines 0 and 2
/// render, 1, 3 and 4 compute; 0 to 3 take 16 uniform bytes, 0 and 1 bind a read-only buffer
/// then a read-write one, 2 and 3 two read-write ones; 4 takes nothing; 5 renders with a
/// texture, a sampler, a depth texture and a comparison sampler; 6 samples a 3D texture and
/// writes another; 7 renders giving its fragments their depth, 8 renders `u32`s, and 9 is drawn
/// only on the screen, none of them taking anything. The other render pipelines may be drawn
/// into any targets their kind takes.
pub fn check_manifest() -> Manifest {
    let mut m = Manifest::new("game.wasm");
    let shape = |name: &str, stage| Pipeline {
        name: name.into(),
        shader: format!("{name}.wgsl"),
        stage,
        uniform: Some(UniformBlock { binding: 0, size: 16, space: UniformSpace::Uniform }),
        bindings: vec![bind(1, BindingKind::Read), bind(2, BindingKind::ReadWrite)],
        debug_flag: None,
    };
    let render = target_bound(false, false);
    m.pipelines.push(shape("draw", render.clone()));
    let compute = Stage::Compute { entry: "main".into(), workgroup_size: [64, 1, 1] };
    m.pipelines.push(shape("compute", compute.clone()));
    // Two writable bindings, which one command may not give the same buffer.
    let write_twice = |mut p: Pipeline| {
        p.bindings[0].kind = BindingKind::ReadWrite;
        p
    };
    m.pipelines.push(write_twice(shape("draw2", render.clone())));
    m.pipelines.push(write_twice(shape("compute2", compute.clone())));
    // A kernel with no uniform block and no bindings.
    m.pipelines.push(Pipeline { uniform: None, bindings: Vec::new(), ..shape("bare", compute) });
    m.pipelines.push(Pipeline {
        uniform: None,
        bindings: vec![
            bind(0, BindingKind::Texture),
            bind(1, BindingKind::Sampler),
            bind(2, BindingKind::DepthTexture),
            bind(3, BindingKind::ComparisonSampler),
        ],
        ..shape("textured", render)
    });
    // A kernel that samples one 3D texture and writes another.
    m.pipelines.push(Pipeline {
        uniform: None,
        bindings: vec![
            bind(0, BindingKind::Texture3d),
            bind(1, BindingKind::Sampler),
            bind(2, BindingKind::StorageTexture3d),
        ],
        ..shape("volume", Stage::Compute { entry: "main".into(), workgroup_size: [4, 4, 4] })
    });
    // Render pipelines that only some passes' targets take.
    let screen_only = render_stage(false, false, vec![RenderTarget::screen(false)]);
    for (name, stage) in [
        ("with_depth", target_bound(true, false)),
        ("ids", target_bound(false, true)),
        ("screen_only", screen_only),
    ] {
        m.pipelines.push(Pipeline { uniform: None, bindings: Vec::new(), ..shape(name, stage) });
    }
    m
}

/// A render stage (`vs`, `fs`, the defaults) whose fragments give their own depth
/// (`writes_depth`) or whose fragment shader returns a `u32` (`uint`), with every target its
/// kind takes.
fn target_bound(writes_depth: bool, uint: bool) -> Stage {
    let colors = TextureFormat::ALL.into_iter().filter(|f| !f.is_depth() && f.is_uint() == uint);
    let mut targets: Vec<RenderTarget> = colors
        .flat_map(|c| [false, true].map(|depth| RenderTarget { color: Some(c), depth }))
        .collect();
    targets.push(RenderTarget { color: None, depth: true });
    targets.retain(|t| t.depth || !writes_depth);
    render_stage(writes_depth, uint, targets)
}

/// A render stage (`vs`, `fs`, the defaults) with these targets.
fn render_stage(writes_depth: bool, uint: bool, targets: Vec<RenderTarget>) -> Stage {
    Stage::Render {
        vertex_entry: "vs".into(),
        fragment_entry: "fs".into(),
        blend: false,
        cull: Cull::None,
        depth_bias: DepthBias::default(),
        depth: DepthState::default(),
        writes_depth,
        uint,
        targets,
    }
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
    // Buffers 1 and 2, 512 bytes each, then `f`'s commands.
    let with_buffers = |f: &dyn Fn(&mut Encoder)| {
        let mut e = Encoder::new();
        e.create_buffer(1, 512).create_buffer(2, 512);
        f(&mut e);
        e.finish()
    };
    // Also texture 8 (4x4, rgba8), depth texture 9 (4x4), sampler 10 and comparison sampler 11.
    let with_textures = |f: &dyn Fn(&mut Encoder)| {
        with_buffers(&|e| {
            e.create_texture(8, 4, 4, TextureFormat::Rgba8, false)
                .create_texture(9, 4, 4, TextureFormat::Depth32Float, false)
                .create_sampler(10, true, false, None)
                .create_sampler(11, false, false, Some(Compare::Less));
            f(e);
        })
    };
    let b = |h: u32| Binding::range(h, 0, 512);
    let ab = [b(1), b(2)];
    // Buffer 2 read, buffer 1 written.
    let rw = [Binding::range(2, 256, 4), b(1)];
    let tex = [Binding::of(8), Binding::of(10), Binding::of(9), Binding::of(11)];
    let pass = Pass {
        color: 8,
        keep_color: false,
        join: false,
        clear: [0.0; 4],
        depth: 9,
        keep_depth: false,
        clear_depth: 1.0,
    };
    let max = crate::Limits::DEFAULT.max_buffer_size;
    let cases = vec![
        check(
            "every command, used rightly",
            &m,
            &with_textures(&|e| {
                e.write_buffer(1, 504, &[7; 8])
                    .dispatch(1, [65_535, 1, 1], &ab, &u)
                    .copy_buffer(1, 0, 2, 256, 256)
                    .dispatch_indirect(1, 1, 500, &ab, &u)
                    .write_texture(8, [1, 1], [2, 2], &[0; 16])
                    .begin_screen_pass([0.0; 4])
                    .draw(0, 3, 1, &ab, &u)
                    .draw(5, 3, 1, &tex, &[])
                    .present()
                    .begin_pass(Pass { color: stream::NONE, ..pass })
                    .draw_indirect(0, 2, 496, &[Binding::range(2, 256, 4), b(1)], &u)
                    .draw_indexed_indirect(
                        0,
                        [2, 256, 236],
                        2,
                        492,
                        &[Binding::range(2, 256, 4), b(1)],
                        &u,
                    )
                    .end_pass()
                    .read_buffer(1, 1, 8, 16)
                    .destroy_buffer(1)
                    .destroy_buffer(2)
                    .destroy_texture(8)
                    .destroy_sampler(10);
            }),
        ),
        check(
            "indices past the end of their buffer",
            &m,
            &with_buffers(&|e| {
                e.begin_screen_pass([0.0; 4]).draw_indexed_indirect(
                    0,
                    [2, 256, 260],
                    2,
                    0,
                    &rw,
                    &u,
                );
            }),
        ),
        check(
            "an empty index range",
            &m,
            &with_buffers(&|e| {
                e.begin_screen_pass([0.0; 4]).draw_indexed_indirect(0, [2, 0, 0], 2, 0, &rw, &u);
            }),
        ),
        check(
            "indexed arguments past the end of their buffer",
            &m,
            &with_buffers(&|e| {
                e.begin_screen_pass([0.0; 4]).draw_indexed_indirect(0, [2, 0, 16], 2, 496, &rw, &u);
            }),
        ),
        check(
            "an index buffer the draw writes",
            &m,
            &with_buffers(&|e| {
                e.begin_screen_pass([0.0; 4]).draw_indexed_indirect(0, [1, 0, 16], 2, 0, &rw, &u);
            }),
        ),
        check("a handle used twice", &m, &with_buffers(&|e| _ = e.create_buffer(1, 4))),
        check(
            "a handle used twice by different kinds",
            &m,
            &with_buffers(&|e| _ = e.create_sampler(2, true, true, None)),
        ),
        check(
            "a buffer over the size limit",
            &m,
            &Encoder::new().create_buffer(1, max).create_buffer(2, max + 4).finish(),
        ),
        check("a write to no buffer", &m, &Encoder::new().write_buffer(3, 0, &[1; 4]).finish()),
        check("a write past the end", &m, &with_buffers(&|e| _ = e.write_buffer(1, 508, &[1; 8]))),
        check("destroying no buffer", &m, &Encoder::new().destroy_buffer(5).finish()),
        check("destroying a texture as a buffer", &m, &with_textures(&|e| _ = e.destroy_buffer(8))),
        check(
            "a buffer used after it's destroyed",
            &m,
            &with_buffers(&|e| _ = e.destroy_buffer(1).dispatch(1, [1; 3], &ab, &u)),
        ),
        check("no such pipeline", &m, &with_buffers(&|e| _ = e.dispatch(9, [1; 3], &ab, &u))),
        check(
            "a pipeline without uniforms",
            &m,
            &with_buffers(&|e| _ = e.dispatch(4, [1; 3], &[], &[])),
        ),
        check(
            "uniform bytes for a pipeline without uniforms",
            &m,
            &with_buffers(&|e| _ = e.dispatch(4, [1; 3], &[], &u)),
        ),
        check(
            "a draw with a compute pipeline",
            &m,
            &with_buffers(&|e| _ = e.begin_screen_pass([0.0; 4]).draw(1, 3, 1, &ab, &u)),
        ),
        check(
            "a dispatch with a render pipeline",
            &m,
            &with_buffers(&|e| _ = e.dispatch(0, [1; 3], &ab, &u)),
        ),
        check("too few bindings", &m, &with_buffers(&|e| _ = e.dispatch(1, [1; 3], &[b(1)], &u))),
        check(
            "the wrong number of uniform bytes",
            &m,
            &with_buffers(&|e| _ = e.dispatch(1, [1; 3], &ab, &[0; 8])),
        ),
        check(
            "too many workgroups",
            &m,
            &with_buffers(&|e| _ = e.dispatch(1, [1, 65_536, 1], &ab, &u)),
        ),
        check(
            "one buffer read and written in a dispatch",
            &m,
            &with_buffers(&|e| _ = e.dispatch(1, [1; 3], &[b(1), b(1)], &u)),
        ),
        check(
            "two ranges of one buffer read and written in a dispatch",
            &m,
            &with_buffers(&|e| {
                e.dispatch(
                    1,
                    [1; 3],
                    &[Binding::range(1, 0, 256), Binding::range(1, 256, 256)],
                    &u,
                );
            }),
        ),
        check(
            "one buffer read and written across a screen pass",
            &m,
            &with_buffers(&|e| {
                e.begin_screen_pass([0.0; 4]).draw(0, 3, 1, &ab, &u).draw(
                    0,
                    3,
                    1,
                    &[b(2), b(1)],
                    &u,
                );
            }),
        ),
        check(
            "one buffer written twice in a dispatch",
            &m,
            &with_buffers(&|e| _ = e.dispatch(3, [1; 3], &[b(1), b(1)], &u)),
        ),
        check(
            "one buffer written twice in a draw",
            &m,
            &with_buffers(&|e| _ = e.begin_screen_pass([0.0; 4]).draw(2, 3, 1, &[b(2), b(2)], &u)),
        ),
        check(
            "two draws in one screen pass may write one buffer",
            &m,
            &with_buffers(&|e| {
                e.begin_screen_pass([0.0; 4]).draw(2, 3, 1, &ab, &u).draw(2, 3, 1, &ab, &u);
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
                e.dispatch(1, [1; 3], &ab, &u).dispatch(1, [1; 3], &[b(2), b(1)], &u);
            }),
        ),
        check(
            "a binding past the end of its buffer",
            &m,
            &with_buffers(&|e| _ = e.dispatch(1, [1; 3], &[Binding::range(1, 256, 512), b(2)], &u)),
        ),
        check(
            "a binding that doesn't start on 256 bytes",
            &m,
            &with_buffers(&|e| _ = e.dispatch(1, [1; 3], &[Binding::range(1, 4, 8), b(2)], &u)),
        ),
        check(
            "an empty binding",
            &m,
            &with_buffers(&|e| _ = e.dispatch(1, [1; 3], &[Binding::range(1, 0, 0), b(2)], &u)),
        ),
        check(
            "a copy within one buffer",
            &m,
            &with_buffers(&|e| _ = e.copy_buffer(1, 0, 1, 256, 4)),
        ),
        check("a copy past the end", &m, &with_buffers(&|e| _ = e.copy_buffer(1, 0, 2, 256, 260))),
        check(
            "indirect arguments past the end",
            &m,
            &with_buffers(&|e| _ = e.dispatch_indirect(1, 2, 504, &ab, &u)),
        ),
        check(
            "indirect arguments written by the same dispatch",
            &m,
            &with_buffers(&|e| _ = e.dispatch_indirect(1, 2, 0, &ab, &u)),
        ),
        check(
            "a texture over the size limit",
            &m,
            &Encoder::new().create_texture(1, 8193, 1, TextureFormat::Rgba8, false).finish(),
        ),
        check(
            "a 3D texture sampled and another written",
            &m,
            &with_textures(&|e| {
                e.create_texture_3d(13, 4, 4, 4, TextureFormat::Rgba8, false)
                    .create_texture_3d(14, 4, 4, 4, TextureFormat::Rgba16Float, true)
                    .dispatch(
                        6,
                        [1, 1, 1],
                        &[Binding::of(13), Binding::of(10), Binding::of(14)],
                        &[],
                    );
            }),
        ),
        check(
            "a 2D texture where a 3D texture goes",
            &m,
            &with_textures(&|e| {
                e.create_texture_3d(14, 4, 4, 4, TextureFormat::Rgba16Float, true).dispatch(
                    6,
                    [1, 1, 1],
                    &[Binding::of(8), Binding::of(10), Binding::of(14)],
                    &[],
                );
            }),
        ),
        check(
            "a 3D texture where a 2D texture goes",
            &m,
            &with_textures(&|e| {
                e.create_texture_3d(13, 4, 4, 4, TextureFormat::Rgba8, false)
                    .begin_pass(pass)
                    .draw(
                        5,
                        3,
                        1,
                        &[Binding::of(13), Binding::of(10), Binding::of(9), Binding::of(11)],
                        &[],
                    );
            }),
        ),
        check(
            "a write to a 3D texture",
            &m,
            &with_textures(&|e| {
                e.create_texture_3d(13, 4, 4, 4, TextureFormat::Rgba8, false).write_texture(
                    13,
                    [0, 0],
                    [1, 1],
                    &[0; 4],
                );
            }),
        ),
        check(
            "a 3D texture as a pass's target",
            &m,
            &with_textures(&|e| {
                e.create_texture_3d(13, 4, 4, 4, TextureFormat::Rgba8, false).begin_pass(Pass {
                    color: 13,
                    depth: stream::NONE,
                    ..pass
                });
            }),
        ),
        check(
            "a 3D texture over the size limit",
            &m,
            &Encoder::new().create_texture_3d(1, 4, 4, 2049, TextureFormat::Rgba8, false).finish(),
        ),
        check(
            "a texture write past the edge",
            &m,
            &with_textures(&|e| _ = e.write_texture(8, [3, 0], [2, 1], &[0; 8])),
        ),
        check(
            "a texture write of the wrong length",
            &m,
            &with_textures(&|e| _ = e.write_texture(8, [0, 0], [2, 1], &[0; 4])),
        ),
        check(
            "a write to a depth texture",
            &m,
            &with_textures(&|e| _ = e.write_texture(9, [0, 0], [1, 1], &[0; 4])),
        ),
        check(
            "a depth texture where a colour one goes",
            &m,
            &with_textures(&|e| {
                e.begin_screen_pass([0.0; 4]).draw(
                    5,
                    3,
                    1,
                    &[Binding::of(9), Binding::of(10), Binding::of(9), Binding::of(11)],
                    &[],
                );
            }),
        ),
        check(
            "a comparison sampler where a filtering one goes",
            &m,
            &with_textures(&|e| {
                e.begin_screen_pass([0.0; 4]).draw(
                    5,
                    3,
                    1,
                    &[Binding::of(8), Binding::of(11), Binding::of(9), Binding::of(11)],
                    &[],
                );
            }),
        ),
        check(
            "a buffer where a texture goes",
            &m,
            &with_textures(&|e| {
                e.begin_screen_pass([0.0; 4]).draw(
                    5,
                    3,
                    1,
                    &[Binding::of(1), Binding::of(10), Binding::of(9), Binding::of(11)],
                    &[],
                );
            }),
        ),
        check(
            "a pass's target bound by its draw",
            &m,
            &with_textures(&|e| {
                _ = e.begin_pass(Pass { depth: stream::NONE, ..pass }).draw(5, 3, 1, &tex, &[])
            }),
        ),
        check(
            "a depth texture as a colour target",
            &m,
            &with_textures(&|e| _ = e.begin_pass(Pass { color: 9, depth: stream::NONE, ..pass })),
        ),
        check(
            "a colour texture as a depth target",
            &m,
            &with_textures(&|e| _ = e.begin_pass(Pass { depth: 8, ..pass })),
        ),
        check(
            "targets of different sizes",
            &m,
            &with_textures(&|e| {
                e.create_texture(12, 2, 2, TextureFormat::Rgba8, false)
                    .begin_pass(Pass { color: 12, ..pass });
            }),
        ),
        check(
            "a pass with no target",
            &m,
            &with_textures(&|e| {
                _ = e.begin_pass(Pass { color: stream::NONE, depth: stream::NONE, ..pass })
            }),
        ),
        check(
            "a clear depth that isn't finite",
            &m,
            &with_textures(&|e| _ = e.begin_pass(Pass { clear_depth: f32::NAN, ..pass })),
        ),
        check(
            "fragments with their own depth, drawn with a depth target",
            &m,
            &with_textures(&|e| {
                e.begin_pass(pass).draw(7, 3, 1, &[], &[]).end_pass();
                e.begin_pass(Pass { color: stream::NONE, ..pass }).draw(7, 3, 1, &[], &[]);
            }),
        ),
        check(
            "fragments with their own depth, drawn without a depth target",
            &m,
            &with_textures(&|e| _ = e.begin_screen_pass([0.0; 4]).draw(7, 3, 1, &[], &[])),
        ),
        check(
            "u32 fragments drawn into an r32uint target",
            &m,
            &with_textures(&|e| {
                e.create_texture(12, 4, 4, TextureFormat::R32Uint, false)
                    .begin_pass(Pass { color: 12, ..pass })
                    .draw(8, 3, 1, &[], &[]);
            }),
        ),
        check(
            "u32 fragments drawn on the screen",
            &m,
            &with_textures(&|e| _ = e.begin_screen_pass([0.0; 4]).draw(8, 3, 1, &[], &[])),
        ),
        check(
            "colour fragments drawn into an r32uint target",
            &m,
            &with_textures(&|e| {
                e.create_texture(12, 4, 4, TextureFormat::R32Uint, false)
                    .begin_pass(Pass { color: 12, depth: stream::NONE, ..pass })
                    .draw_indirect(0, 2, 0, &rw, &u);
            }),
        ),
        check(
            "a pipeline drawn on the screen, its one target",
            &m,
            &with_textures(&|e| _ = e.begin_screen_pass([0.0; 4]).draw(9, 3, 1, &[], &[])),
        ),
        check(
            "a pipeline drawn into a texture, which isn't one of its targets",
            &m,
            &with_textures(&|e| {
                _ = e.begin_pass(Pass { depth: stream::NONE, ..pass }).draw(9, 3, 1, &[], &[])
            }),
        ),
        check(
            "a pipeline drawn on the screen with depth, which isn't one of its targets",
            &m,
            &with_textures(&|e| {
                _ = e.begin_pass(Pass { color: SCREEN, ..pass }).draw(9, 3, 1, &[], &[])
            }),
        ),
        check("a readback past the end", &m, &with_buffers(&|e| _ = e.read_buffer(0, 1, 256, 260))),
        check(
            "a storage path out of storage",
            &m,
            &Encoder::new().storage_read(1, "../etc").finish(),
        ),
        check(
            "an absolute storage path",
            &m,
            &Encoder::new().storage_write(1, "/x", &[1]).finish(),
        ),
        check(
            "a URL to another site",
            &m,
            &Encoder::new().fetch(1, "https://example.com/x").finish(),
        ),
        check(
            "a post to another site",
            &m,
            &Encoder::new().post(1, "https://example.com/x", &[1]).finish(),
        ),
        check(
            "requests, made rightly",
            &m,
            &Encoder::new().storage_write(1, "a/b", &[1]).fetch(2, "c.bin").finish(),
        ),
    ];
    json!({ "manifest": m.to_json(), "batches": cases })
}

pub(crate) fn sample_manifest() -> Manifest {
    let mut m = Manifest::new("game.wasm");
    m.pipelines.push(Pipeline {
        name: "sample".into(),
        shader: "pipeline_0.wgsl".into(),
        stage: Stage::Compute { entry: "main".into(), workgroup_size: [64, 1, 1] },
        uniform: Some(UniformBlock { binding: 0, size: 32, space: UniformSpace::Uniform }),
        bindings: vec![bind(1, BindingKind::ReadWrite)],
        debug_flag: None,
    });
    m.pipelines.push(Pipeline {
        name: "cover+shade".into(),
        shader: "pipeline_1.wgsl".into(),
        stage: render_stage(false, false, vec![RenderTarget::screen(false)]),
        uniform: None,
        bindings: vec![bind(0, BindingKind::Texture), bind(1, BindingKind::Sampler)],
        debug_flag: None,
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
            golden.replace(
                &format!("\"manifest_version\": {}", crate::manifest::VERSION),
                &format!("\"manifest_version\": {}", crate::manifest::VERSION - 1),
            ),
        ),
        manifest(
            "another stream version",
            golden.replace(
                &format!("\"stream_version\": {}", stream::VERSION),
                "\"stream_version\": 99",
            ),
        ),
        manifest("too wide a workgroup", edited(&|m| compute(m, [512, 1, 1]))),
        manifest("too many invocations", edited(&|m| compute(m, [16, 16, 2]))),
        manifest("a binding used twice", edited(&|m| m.pipelines[0].bindings[0].binding = 0)),
        manifest("a debug build's flag", edited(&|m| m.pipelines[0].debug_flag = Some(2))),
        manifest("a debug flag on a binding", edited(&|m| m.pipelines[0].debug_flag = Some(1))),
        manifest("a debug flag as a string", golden.replace("\"bindings\": [", "\"debug_flag\": \"2\",\n      \"bindings\": [")),
        manifest(
            "a render pipeline that culls back faces, with a depth bias",
            edited(&|m| {
                if let Stage::Render { cull, depth_bias, .. } = &mut m.pipelines[1].stage {
                    *cull = Cull::Back;
                    *depth_bias = DepthBias { constant: 4, slope_scale: 2.0, clamp: 0.0 };
                }
            }),
        ),
        manifest(
            "a render pipeline that draws where depths are equal, writing none",
            edited(&|m| {
                if let Stage::Render { depth, .. } = &mut m.pipelines[1].stage {
                    *depth = DepthState { compare: Compare::Equal, write: false };
                }
            }),
        ),
        manifest(
            "a depth test that isn't one",
            golden.replace(
                "\"fragment_entry\": \"fs\",",
                "\"fragment_entry\": \"fs\", \"depth\": { \"compare\": \"nearer\", \"write\": true },",
            ),
        ),
        manifest(
            "a depth write that isn't a bool",
            golden.replace(
                "\"fragment_entry\": \"fs\",",
                "\"fragment_entry\": \"fs\", \"depth\": { \"compare\": \"always\", \"write\": 0 },",
            ),
        ),
        manifest(
            "a cull mode that isn't one",
            golden.replace(
                "\"fragment_entry\": \"fs\",",
                "\"fragment_entry\": \"fs\", \"cull\": \"sideways\",",
            ),
        ),
        manifest(
            "a depth bias that isn't whole",
            golden.replace(
                "\"fragment_entry\": \"fs\",",
                "\"fragment_entry\": \"fs\", \"depth_bias\": { \"constant\": 1.5, \"slope_scale\": 0, \"clamp\": 0 },",
            ),
        ),
        manifest(
            "a blended render pipeline",
            edited(&|m| {
                if let Stage::Render { blend, .. } = &mut m.pipelines[1].stage {
                    *blend = true;
                }
            }),
        ),
        manifest("a blend that isn't a bool", golden.replace("\"fragment_entry\": \"fs\",", "\"fragment_entry\": \"fs\",\n      \"blend\": 1,")),
        manifest("a blend that's null", golden.replace("\"fragment_entry\": \"fs\",", "\"fragment_entry\": \"fs\",\n      \"blend\": null,")),
        manifest(
            "a render pipeline with no targets",
            edited(&|m| {
                if let Stage::Render { targets, .. } = &mut m.pipelines[1].stage {
                    targets.clear();
                }
            }),
        ),
        manifest(
            "a render pipeline's target named twice",
            edited(&|m| {
                if let Stage::Render { targets, .. } = &mut m.pipelines[1].stage {
                    targets.push(RenderTarget::screen(false));
                }
            }),
        ),
        manifest(
            "fragments with their own depth, and a target without depth",
            edited(&|m| {
                if let Stage::Render { writes_depth, targets, .. } = &mut m.pipelines[1].stage {
                    *writes_depth = true;
                    *targets = vec![RenderTarget::screen(true), RenderTarget::screen(false)];
                }
            }),
        ),
        manifest(
            "u32 fragments with a colour target",
            edited(&|m| {
                if let Stage::Render { uint, .. } = &mut m.pipelines[1].stage {
                    *uint = true;
                }
            }),
        ),
        manifest(
            "colour fragments with an r32uint target",
            edited(&|m| {
                if let Stage::Render { targets, .. } = &mut m.pipelines[1].stage {
                    targets[0].color = Some(TextureFormat::R32Uint);
                }
            }),
        ),
        manifest(
            "a target whose colour is a depth format",
            edited(&|m| {
                if let Stage::Render { targets, .. } = &mut m.pipelines[1].stage {
                    targets[0].color = Some(TextureFormat::Depth32Float);
                }
            }),
        ),
        manifest(
            "a target whose colour isn't a format",
            golden.replace("\"color\": \"rgba8unorm\"", "\"color\": \"rgb8\""),
        ),
        manifest(
            "a target without its depth",
            golden.replace("\"depth\": false", "\"deep\": false"),
        ),
        // Each of WebGPU's per-stage limits, one past it: in a fragment shader, in a vertex
        // shader (not counting what the other stage binds), and in a kernel.
        manifest(
            "too many sampled textures in a fragment shader",
            edited(&|m| {
                m.pipelines[1].bindings = (0..17)
                    .map(|b| ResourceBinding { stage: BindingStage::Fragment, ..bind(b, BindingKind::Texture) })
                    .collect()
            }),
        ),
        manifest(
            "sampled textures at the limit in each of two stages",
            edited(&|m| {
                m.pipelines[1].bindings = (0..32)
                    .map(|b| {
                        let stage = if b < 16 { BindingStage::Vertex } else { BindingStage::Fragment };
                        ResourceBinding { stage, ..bind(b, BindingKind::Texture) }
                    })
                    .collect()
            }),
        ),
        manifest(
            "too many samplers in a vertex shader",
            edited(&|m| {
                m.pipelines[1].bindings = (0..17)
                    .map(|b| ResourceBinding { stage: BindingStage::Vertex, ..bind(b, BindingKind::Sampler) })
                    .collect()
            }),
        ),
        manifest(
            "too many storage textures in a kernel",
            edited(&|m| {
                m.pipelines[0].bindings =
                    (1..6).map(|b| bind(b, BindingKind::StorageTexture)).collect()
            }),
        ),
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
            edited(&|m| m.pipelines[1].bindings = (0..9).map(|i| bind(i, BindingKind::Read)).collect()),
        ),
        manifest(
            "eight storage buffers in each stage, sixteen in all",
            edited(&|m| {
                m.pipelines[1].bindings = (0..16)
                    .map(|i| ResourceBinding {
                        stage: if i < 8 { BindingStage::Vertex } else { BindingStage::Fragment },
                        ..bind(i, BindingKind::Read)
                    })
                    .collect()
            }),
        ),
        manifest(
            "too many storage buffers in a vertex shader",
            edited(&|m| {
                m.pipelines[1].bindings = (0..9)
                    .map(|i| ResourceBinding {
                        stage: if i < 4 { BindingStage::Both } else { BindingStage::Vertex },
                        ..bind(i, BindingKind::Read)
                    })
                    .collect()
            }),
        ),
        manifest(
            "a kernel's binding with a stage",
            edited(&|m| m.pipelines[0].bindings[0].stage = BindingStage::Fragment),
        ),
        manifest(
            "a written binding in a vertex shader",
            edited(&|m| {
                m.pipelines[1].bindings.push(ResourceBinding {
                    stage: BindingStage::Vertex,
                    ..bind(2, BindingKind::ReadWrite)
                })
            }),
        ),
        manifest(
            "a binding stage that isn't one",
            golden.replace("\"kind\": \"sampler\"", "\"kind\": \"sampler\",\n          \"stage\": \"geometry\""),
        ),
        manifest(
            "too many storage buffers with a debug flag",
            edited(&|m| {
                m.pipelines[1].bindings = (0..8).map(|i| bind(i, BindingKind::Read)).collect();
                m.pipelines[1].debug_flag = Some(8);
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
        // A file is one in the build directory: not a path out of it, nor a URL.
        manifest("an absolute shader path", edited(&|m| m.pipelines[1].shader = "/etc/hosts".into())),
        manifest("a shader path out of the directory", edited(&|m| m.pipelines[0].shader = "../x.wgsl".into())),
        manifest("a WASM file as a URL", edited(&|m| m.wasm = "data:application/wasm,".into())),
        manifest("a WASM file with a fragment", edited(&|m| m.wasm = "game.wasm#x".into())),
        manifest("a hidden shader file", edited(&|m| m.pipelines[0].shader = ".hidden.wgsl".into())),
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
            "a binding as an array",
            golden.replace(
                "{\n          \"binding\": 1,\n          \"kind\": \"read_write\"\n        }",
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
            golden.replace("\"manifest_version\": 2", "\"manifest_version\": \"2\""),
        ),
        manifest("pipelines not in an array", golden.replace("\"pipelines\": [", "\"pipelines\": {\"x\": [").replace("  ]\n}", "  ]}\n}")),
        manifest("a string for a number", golden.replace("\"size\": 32", "\"size\": \"32\"")),
        manifest("a workgroup size of two", golden.replace("64,\n        1,\n        1", "64, 1")),
        // JSON can't tell 1 from 1.0, so neither host does.
        manifest(
            "whole numbers written with fractions",
            golden.replace("\"manifest_version\": 2", "\"manifest_version\": 2.0").replace("\"size\": 32", "\"size\": 3.2e1"),
        ),
        manifest("a negative number", golden.replace("\"binding\": 1", "\"binding\": -1")),
        manifest("a number past 32 bits", golden.replace("\"size\": 32", "\"size\": 4294967296")),
        manifest("a fraction", golden.replace("\"size\": 32", "\"size\": 32.5")),
        manifest("a large number", golden.replace("\"size\": 32", "\"size\": 1e21")),
        manifest("a small number", golden.replace("\"size\": 32", "\"size\": 1.5e-7")),
        manifest("a negative zero", golden.replace("\"binding\": 0", "\"binding\": -0.0")),
        manifest("a wrong kind", golden.replace("\"kind\": \"render\"", "\"kind\": \"draw\\n\"")),
        manifest("an unknown binding kind", golden.replace("\"kind\": \"sampler\"", "\"kind\": \"cube_texture\"")),
        malformed("not JSON", golden.replace("}\n", "")),
        malformed("a byte-order mark", format!("\u{feff}{golden}")),
        malformed("a lone surrogate", golden.replace("\"sample\"", "\"\\ud800\"")),
        // Numbers are read as JavaScript reads them, correctly rounded: this one is `1`.
        manifest(
            "a fraction that rounds to a whole number",
            golden.replace("\"manifest_version\": 2", "\"manifest_version\": 2.0000000000000001110"),
        ),
        malformed(
            "a number past f64",
            golden.replace("\"wasm\": \"game.wasm\"", "\"wasm\": \"game.wasm\", \"extra\": 1e400"),
        ),
        malformed(
            "arrays nested 128 deep",
            golden.replace(
                "\"wasm\": \"game.wasm\"",
                &format!("\"wasm\": \"game.wasm\", \"extra\": {}{}", "[".repeat(127), "]".repeat(127)),
            ),
        ),
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

/// Input scripts (`crate::input`), each with the events it gives, as their bytes in hex and
/// their frames, or the error it's refused with.
fn input_scripts() -> Value {
    let scripts = [
        (
            "every kind",
            r#"[
  {"frame": 0, "type": "move", "x": 10.5, "y": 20},
  {"frame": 1, "type": "down", "x": 10.5, "y": 20, "button": "secondary", "shift": true},
  {"frame": 1, "type": "up", "x": 11, "y": 21, "alt": true},
  {"frame": 2, "type": "wheel", "x": 1, "y": 2, "dx": -3, "dy": 120.25},
  {"frame": 3, "type": "key", "key": "Tab", "ctrl": true, "meta": true},
  {"frame": 3, "type": "keydown", "key": "KeyA", "repeat": true},
  {"frame": 3, "type": "keyup", "key": "KeyA"},
  {"frame": 4, "type": "text", "text": "aéÿ"}
]"#,
        ),
        ("not an array", "{}"),
        ("not JSON", "[{"),
        ("no frame", r#"[{"type": "move"}]"#),
        ("a fractional frame", r#"[{"frame": 1.5, "type": "move"}]"#),
        ("frames that decrease", r#"[{"frame": 2, "type": "move"}, {"frame": 1, "type": "move"}]"#),
        (
            "ticks and frames",
            r#"[{"tick": 3, "type": "key", "key": "Space"}, {"frame": 0, "type": "move"}, {"tick": 3, "type": "text", "text": "q"}]"#,
        ),
        ("ticks that decrease", r#"[{"tick": 2, "type": "move"}, {"tick": 1, "type": "move"}]"#),
        ("a frame and a tick", r#"[{"frame": 2, "tick": 1, "type": "move"}]"#),
        ("an unknown type", r#"[{"frame": 0, "type": "jump"}]"#),
        ("an unknown key", r#"[{"frame": 0, "type": "key", "key": "A"}]"#),
        ("an unknown button", r#"[{"frame": 0, "type": "down", "button": "left"}]"#),
        ("a coordinate that isn't a number", r#"[{"frame": 0, "type": "move", "x": "1"}]"#),
        ("a modifier that isn't a bool", r#"[{"frame": 0, "type": "move", "shift": 1}]"#),
        ("text that's missing", r#"[{"frame": 0, "type": "text"}]"#),
    ];
    scripts
        .iter()
        .map(|(name, text)| match crate::input::parse_script(text) {
            Ok(events) => json!({
                "name": name,
                "script": text,
                "at": events
                    .iter()
                    .map(|e| match e.at {
                        crate::input::At::Frame(n) => format!("frame {n}"),
                        crate::input::At::Tick(n) => format!("tick {n}"),
                    })
                    .collect::<Vec<_>>(),
                "events": events.iter().map(|e| hex(&e.event.bytes())).collect::<Vec<_>>(),
            }),
            Err(e) => json!({ "name": name, "script": text, "error": e }),
        })
        .collect()
}

/// A tick log's bytes (runtime/abi `ticks`), which the browser runtime's writer must give too.
fn tick_logs() -> Value {
    use crate::input::Event;
    use crate::ticks::{Tick, TickLog};
    let mut log = TickLog::new(crate::ticks::wasm_hash(b"\0asm"), 60, 0x0102_0304_0506_0708);
    log.ticks.push(Tick { records: Vec::new(), hash: 0xfedc_ba98_7654_3210 });
    log.ticks.push(Tick {
        records: vec![Event::key(true, 41, false, 0).bytes(), Event::text('w').bytes()],
        hash: 1,
    });
    let records = |t: &Tick| t.records.iter().map(|r| hex(r)).collect::<Vec<_>>();
    json!([{
        "wasm_hash": crate::hash::hex(log.wasm_hash),
        "hz": log.hz,
        "first": crate::hash::hex(log.first),
        "ticks": log.ticks.iter().map(|t| json!({ "records": records(t), "hash": crate::hash::hex(t.hash) })).collect::<Vec<_>>(),
        "bytes": hex(&log.encode()),
    }])
}

/// Test mode's schedule (runtime/abi `ticks`): at each rate and frame rate, the ticks that run
/// before each frame in lockstep, and each frame's time (an `f32`).
fn lockstep() -> Value {
    use crate::ticks::{frame_time, lockstep_ticks};
    let frames = [0, 1, 2, 11, 59, 143, 1000, 9999];
    let schedules = [(60, 60.0), (60, 30.0), (60, 144.0), (30, 59.94), (120, 60.0)];
    schedules
        .iter()
        .map(|&(hz, fps)| {
            json!({
                "hz": hz,
                "fps": fps,
                "frames": frames,
                "ticks": frames.map(|i| lockstep_ticks(i, hz, fps)),
                "times": frames.map(|i| frame_time(i, fps)),
            })
        })
        .collect()
}

/// A pass as the browser runtime decodes it.
fn pass_json(p: &Pass) -> Value {
    json!({
        "color": p.color, "keepColor": p.keep_color, "join": p.join, "clear": p.clear,
        "depth": p.depth, "keepDepth": p.keep_depth, "clearDepth": p.clear_depth,
    })
}

/// Pairs of passes, one ended and the next beginning right after it: whether the next runs as
/// part of the one before ([`Pass::joins`]).
fn joins() -> Vec<Value> {
    let ended = Pass { keep_color: true, keep_depth: true, ..offscreen() };
    let next = Pass { join: true, ..ended };
    let pairs = [
        ("the same targets, kept", ended, next),
        ("one that may not join", ended, Pass { join: false, ..next }),
        ("on the screen", Pass { color: SCREEN, ..ended }, Pass { color: SCREEN, ..next }),
        ("another colour target", ended, Pass { color: 12, ..next }),
        ("another depth target", ended, Pass { depth: 12, ..next }),
        ("its colour cleared", ended, Pass { keep_color: false, ..next }),
        ("its depth cleared", ended, Pass { keep_depth: false, ..next }),
        (
            "depth alone, kept",
            Pass { color: NONE, ..ended },
            Pass { color: NONE, keep_color: false, ..next },
        ),
        (
            "colour alone, kept",
            Pass { depth: NONE, ..ended },
            Pass { depth: NONE, keep_depth: false, ..next },
        ),
        ("the one before cleared what it drew into", offscreen(), next),
    ];
    pairs
        .iter()
        .map(|(name, ended, next)| {
            json!({
                "name": name,
                "ended": pass_json(ended),
                "next": pass_json(next),
                "joins": next.joins(ended),
            })
        })
        .collect()
}

/// Every vector, as `runtime/abi/vectors.json` holds them.
pub fn vectors() -> String {
    let v = json!({
        "comment": "Generated by the wrela-abi crate (runtime/abi/src/vectors.rs); don't edit. \
                    Run `cargo run -p wrela-abi --bin generate`.",
        "hashes": hashes(),
        "batches": batches(),
        "sequences": sequences(),
        "joins": joins(),
        "checks": checks(),
        "manifests": manifests(),
        "lines": line_tables(),
        "input": input_scripts(),
        "tick_logs": tick_logs(),
        "lockstep": lockstep(),
    });
    let mut s = serde_json::to_string_pretty(&v).expect("vectors serialize");
    s.push('\n');
    s
}
