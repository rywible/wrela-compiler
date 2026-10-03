//! The program ABI without a GPU: imports and exports, bounds, decoding, sequencing, the checks,
//! the state hash, and calling exports. Commands go to a [`Recorder`].

use super::*;
use crate::check::{Checker, Limits};
use wrela_abi::Manifest;
use wrela_abi::manifest::{Access, BufferBinding, Pipeline, Stage, UniformBlock, UniformSpace};
use wrela_abi::stream::{Encoder, Opcode, StreamError, VERSION};

#[path = "../../tests/suite/common/wat.rs"]
mod wat_gen;

/// Records each command's opcode, and `end` at each frame's end.
#[derive(Default)]
struct Recorder {
    log: Vec<&'static str>,
}

impl Executor for Recorder {
    fn execute(&mut self, cmd: &Command<'_>) -> Result<()> {
        self.log.push(cmd.opcode().name());
        Ok(())
    }

    fn end_frame(&mut self) -> Result<()> {
        self.log.push("end");
        Ok(())
    }
}

/// Pipeline 0 renders and pipeline 1 computes; each takes 16 uniform bytes and binds a
/// read-only buffer then a read-write one.
fn manifest() -> Manifest {
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
    m.pipelines.push(shape(
        "draw",
        Stage::Render { vertex_entry: "vs".into(), fragment_entry: "fs".into() },
    ));
    m.pipelines.push(shape(
        "compute",
        Stage::Compute { entry: "main".into(), workgroup_size: [64, 1, 1] },
    ));
    m.validate().expect("valid");
    m
}

fn load_wat(wat: &str) -> Result<Program<Recorder>> {
    let wasm = wat::parse_str(wat).expect("test WAT compiles");
    let compiled = compile(&wasm)?;
    let checker = Checker::new(&manifest(), Limits::webgpu_defaults());
    Program::instantiate(&compiled, checker, Recorder::default())
}

/// A program submitting `frames`; runs every frame and returns the first error.
fn run(frames: &[Vec<Vec<u8>>]) -> (Program<Recorder>, Result<()>) {
    let mut program = load_wat(&wat_gen::program(frames)).expect("loads");
    let result = (0..frames.len()).try_for_each(|i| program.frame(i as f32 / 60.0, 64, 64));
    (program, result)
}

fn run_err(frames: &[Vec<Vec<u8>>]) -> Error {
    run(frames).1.expect_err("the run fails")
}

fn command_err(batch: Vec<u8>) -> (Opcode, String) {
    match run_err(&[vec![batch]]) {
        Error::Command { opcode, why } => (opcode, why),
        e => panic!("expected a command error, got {e}"),
    }
}

/// Buffers 1 and 2, 64 bytes each.
fn with_buffers(f: impl FnOnce(&mut Encoder)) -> Vec<u8> {
    let mut e = Encoder::new();
    e.create_buffer(1, 64).create_buffer(2, 64);
    f(&mut e);
    e.finish()
}

const U16: [u8; 16] = [0; 16];

#[test]
fn imports_only_wrela_submit() {
    let err = load_wat(
        r#"(module (import "env" "log" (func (param i32))) (memory (export "memory") 1)
             (func (export "frame") (param f32 i32 i32)))"#,
    )
    .err()
    .expect("rejected");
    assert!(
        matches!(&err, Error::Program(m) if m.contains("`env.log`") && m.contains("only `wrela.submit`")),
        "{err}"
    );

    let err = load_wat(
        r#"(module (import "wrela" "submit" (func (param i32))) (memory (export "memory") 1)
             (func (export "frame") (param f32 i32 i32)))"#,
    )
    .err()
    .expect("rejected");
    assert_eq!(
        err.to_string(),
        "invalid program: its import `wrela.submit` must be a function (i32, i32) -> (), not a function (i32) -> ()"
    );

    let err = load_wat(
        r#"(module (import "wrela" "memory" (memory 1)) (func (export "frame") (param f32 i32 i32)))"#,
    )
    .err()
    .expect("rejected");
    assert!(matches!(&err, Error::Program(m) if m.contains("`wrela.memory`")), "{err}");

    // No imports at all is fine (if useless).
    load_wat(
        r#"(module (memory (export "memory") 1) (func (export "frame") (param f32 i32 i32)))"#,
    )
    .expect("loads");
}

#[test]
fn needs_memory_and_frame_exports() {
    let no_memory = load_wat(r#"(module (memory 1) (func (export "frame") (param f32 i32 i32)))"#);
    assert!(matches!(no_memory.err(), Some(Error::Program(m)) if m.contains("`memory`")));
    let no_frame = load_wat(r#"(module (memory (export "memory") 1))"#);
    assert!(matches!(no_frame.err(), Some(Error::Program(m)) if m.contains("`frame(")));
    let wrong_frame =
        load_wat(r#"(module (memory (export "memory") 1) (func (export "frame") (param f64)))"#);
    assert!(matches!(wrong_frame.err(), Some(Error::Program(m)) if m.contains("`frame(")));
    assert!(matches!(compile(b"not wasm").err(), Some(Error::Program(_))));
}

#[test]
fn hashes_every_submitted_byte() {
    let a = Encoder::new().create_buffer(1, 16).finish();
    let b = Encoder::new().begin_screen_pass([0.0; 4]).present().finish();
    let c = Encoder::new().finish(); // an empty batch is valid
    let (program, result) = run(&[vec![a.clone(), b.clone()], vec![c.clone()]]);
    result.expect("runs");
    let mut expected = StateHash::new();
    for batch in [&a, &b, &c] {
        expected.update(batch);
    }
    assert_eq!(program.hash(), expected);
    let mut program = program;
    assert_eq!(
        program.executor().log,
        ["CreateBuffer", "BeginScreenPass", "Present", "end", "end"]
    );
}

#[test]
fn rejects_another_stream_version() {
    let mut batch = Encoder::new().present().finish();
    batch[4] = 9;
    let err = run_err(&[vec![batch]]);
    assert!(
        matches!(err, Error::Stream(StreamError::WrongVersion { expected: VERSION, got: 9 })),
        "{err}"
    );
}

#[test]
fn rejects_malformed_batches_before_running_any_command() {
    // A valid command followed by an unknown opcode: neither runs.
    let mut batch = Encoder::new().create_buffer(1, 16).present().finish();
    batch[12 + 16] = 99;
    let (mut program, result) = run(&[vec![batch]]);
    assert!(matches!(result, Err(Error::Stream(StreamError::UnknownOpcode { opcode: 99, .. }))));
    assert!(program.executor().log.is_empty());

    let short = Encoder::new().create_buffer(1, 16).finish();
    let err = run_err(&[vec![short[..short.len() - 4].to_vec()]]);
    assert!(matches!(err, Error::Stream(StreamError::BodyLength { .. })), "{err}");
    let err = run_err(&[vec![b"WRC".to_vec()]]);
    assert!(matches!(err, Error::Stream(StreamError::TooShort { .. })), "{err}");
}

#[test]
fn rejects_out_of_order_commands() {
    let draw = Encoder::new().draw(0, 3, 1, &[1, 2], &U16).finish();
    assert!(matches!(
        run_err(&[vec![draw]]),
        Error::Stream(StreamError::Sequence { opcode: Opcode::Draw, .. })
    ));
    let open = Encoder::new().begin_screen_pass([0.0; 4]).finish();
    assert!(matches!(run_err(&[vec![open]]), Error::Stream(StreamError::UnclosedPass)));
    // A pass may span batches within a frame.
    let begin = Encoder::new().begin_screen_pass([0.0; 4]).finish();
    let end = Encoder::new().present().finish();
    run(&[vec![begin, end]]).1.expect("one pass over two batches");
}

#[test]
fn rejects_submits_outside_memory() {
    let wat = r#"(module (import "wrela" "submit" (func $submit (param i32 i32)))
        (memory (export "memory") 1)
        (func (export "frame") (param f32 i32 i32) (call $submit (i32.const 65530) (i32.const 100))))"#;
    let err = load_wat(wat).expect("loads").frame(0.0, 1, 1).expect_err("fails");
    assert!(matches!(&err, Error::Trap(m) if m.contains("submit(65530, 100)")), "{err}");
}

#[test]
fn reports_program_traps() {
    let wat = r#"(module (memory (export "memory") 1) (func (export "frame") (param f32 i32 i32) unreachable))"#;
    let err = load_wat(wat).expect("loads").frame(0.0, 1, 1).expect_err("traps");
    assert!(matches!(&err, Error::Trap(m) if m.contains("unreachable")), "{err}");
}

#[test]
fn checks_buffers() {
    let (op, why) = command_err(Encoder::new().create_buffer(1, 16).create_buffer(1, 16).finish());
    assert_eq!((op, why.as_str()), (Opcode::CreateBuffer, "buffer 1 already exists"));
    let (op, why) = command_err(Encoder::new().create_buffer(1, 1 << 30).finish());
    assert_eq!(
        (op, why.as_str()),
        (Opcode::CreateBuffer, "buffer 1 is 1073741824 bytes; the limit is 134217728")
    );
    let (op, why) = command_err(Encoder::new().write_buffer(3, 0, &[0; 4]).finish());
    assert_eq!((op, why.as_str()), (Opcode::WriteBuffer, "there's no buffer 3"));
    let (_, why) = command_err(with_buffers(|e| {
        e.write_buffer(1, 60, &[0; 8]);
    }));
    assert_eq!(why, "writing 8 bytes at offset 60 overruns buffer 1 (64 bytes)");
    // Exactly to the end is fine.
    run(&[vec![with_buffers(|e| {
        e.write_buffer(1, 56, &[0; 8]);
    })]])
    .1
    .expect("fits");
}

#[test]
fn checks_dispatches() {
    let (op, why) = command_err(with_buffers(|e| {
        e.dispatch(7, [1; 3], &[1, 2], &U16);
    }));
    assert_eq!(
        (op, why.as_str()),
        (Opcode::Dispatch, "there's no pipeline 7 (the manifest has 2)")
    );
    let (_, why) = command_err(with_buffers(|e| {
        e.dispatch(0, [1; 3], &[1, 2], &U16);
    }));
    assert_eq!(why, "pipeline 0 (draw) is a render pipeline; Dispatch needs a compute pipeline");
    let (_, why) = command_err(with_buffers(|e| {
        e.dispatch(1, [1; 3], &[1], &U16);
    }));
    assert_eq!(why, "pipeline 1 (compute) binds 2 buffers, but the command lists 1");
    let (_, why) = command_err(with_buffers(|e| {
        e.dispatch(1, [1; 3], &[1, 2], &[0; 8]);
    }));
    assert_eq!(why, "pipeline 1 (compute) takes 16 uniform bytes, but the command has 8");
    let (_, why) = command_err(with_buffers(|e| {
        e.dispatch(1, [1; 3], &[1, 9], &U16);
    }));
    assert_eq!(why, "there's no buffer 9");
    let (_, why) = command_err(with_buffers(|e| {
        e.dispatch(1, [1; 3], &[1, 1], &U16);
    }));
    assert_eq!(why, "buffer 1 is bound both read-only and read-write in one dispatch");
    let (_, why) = command_err(with_buffers(|e| {
        e.dispatch(1, [65536, 1, 1], &[1, 2], &U16);
    }));
    assert_eq!(why, "65536x1x1 workgroups is over the limit of 65535 per dimension");
    // Usage scopes are per dispatch: reading then writing a buffer in two dispatches is fine.
    run(&[vec![with_buffers(|e| {
        e.dispatch(1, [1; 3], &[1, 2], &U16).dispatch(1, [1; 3], &[2, 1], &U16);
    })]])
    .1
    .expect("separate scopes");
}

#[test]
fn checks_draws_across_the_pass() {
    let (op, why) = command_err(with_buffers(|e| {
        e.begin_screen_pass([0.0; 4]).draw(1, 3, 1, &[1, 2], &U16).present();
    }));
    assert_eq!(
        (op, why.as_str()),
        (Opcode::Draw, "pipeline 1 (compute) is a compute pipeline; Draw needs a render pipeline")
    );
    // Buffer 1 read by one draw and written by another in the same pass.
    let (_, why) = command_err(with_buffers(|e| {
        e.begin_screen_pass([0.0; 4])
            .draw(0, 3, 1, &[1, 2], &U16)
            .draw(0, 3, 1, &[2, 1], &U16)
            .present();
    }));
    assert_eq!(why, "buffer 2 is bound both read-only and read-write in one screen pass");
    // The same in two passes is fine.
    run(&[vec![with_buffers(|e| {
        e.begin_screen_pass([0.0; 4]).draw(0, 3, 1, &[1, 2], &U16).present();
        e.begin_screen_pass([0.0; 4]).draw(0, 3, 1, &[2, 1], &U16).present();
    })]])
    .1
    .expect("separate passes");
}

#[test]
fn calls_other_exports() {
    let wasm = include_bytes!("../../../fixtures/first-light/game.wasm");
    let compiled = compile(wasm).expect("the fixture is a valid program");
    let mut program = Program::instantiate(
        &compiled,
        Checker::new(&manifest(), Limits::webgpu_defaults()),
        Recorder::default(),
    )
    .expect("instantiates");
    for (x, expected) in [(0.0, -1.0), (0.25, 0.0), (0.5, 1.0), (0.75, 0.0), (1.125, -0.5)] {
        assert_eq!(
            program.call("tri", &[Value::F32(x)]).expect("calls"),
            vec![Value::F32(expected)],
            "tri({x})"
        );
    }
    let err = program.call("tri", &[Value::I32(1)]).expect_err("wrong type");
    assert!(matches!(&err, Error::Program(m) if m.contains("can't take (i32)")), "{err}");
    assert!(program.call("nope", &[]).is_err());
}
