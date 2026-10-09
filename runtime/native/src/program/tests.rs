//! The program ABI without a GPU: imports and exports, bounds, decoding, sequencing, the checks,
//! the state hash, and calling exports. Commands go to a [`Recorder`].

use super::*;
use wrela_abi::check::CommandError;
use wrela_abi::stream::{Binding, Encoder, Opcode, StreamError, VERSION};
use wrela_abi::vectors::check_manifest as manifest;

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

    fn read_back(&mut self, _handle: u32, _offset: u32, size: u32) -> Result<Vec<u8>> {
        Ok(vec![7; size as usize])
    }
}

fn load_wat(wat: &str) -> Result<Program<Recorder>> {
    let wasm = wat::parse_str(wat).expect("test WAT compiles");
    Program::instantiate(
        &compile_with(&wasm, None)?,
        &manifest(),
        Recorder::default(),
        Io::default(),
        false,
        1,
    )
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
        Error::Command(CommandError { opcode, why }) => (opcode, why),
        e => panic!("expected a command error, got {e}"),
    }
}

/// All of buffer `h`, as the buffers [`with_buffers`] makes are: 64 bytes.
fn b(h: u32) -> Binding {
    Binding::range(h, 0, 64)
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
fn imports_only_the_hosts_functions() {
    let err = load_wat(
        r#"(module (import "env" "log" (func (param i32))) (import "wrela" "memory" (memory 1 16384 shared)) (export "memory" (memory 0))
             (func (export "frame") (param f32 i32 i32)))"#,
    )
    .err()
    .expect("rejected");
    assert!(
        matches!(&err, Error::Program(m) if m.contains("`env.log`") && m.contains("isn't one of the host's")),
        "{err}"
    );

    let err = load_wat(
        r#"(module (import "wrela" "submit" (func (param i32))) (import "wrela" "memory" (memory 1 16384 shared)) (export "memory" (memory 0))
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
        r#"(module (import "wrela" "memory" (memory 1 16384 shared)) (export "memory" (memory 0)) (func (export "frame") (param f32 i32 i32)))"#,
    )
    .expect("loads");
}

#[test]
fn needs_memory_and_frame_exports() {
    // Its own memory, rather than the shared one the host gives it.
    let own = load_wat(
        r#"(module (memory (export "memory") 1) (func (export "frame") (param f32 i32 i32)))"#,
    );
    assert!(matches!(own.err(), Some(Error::Program(m)) if m.contains("`wrela.memory`")));
    let unexported = load_wat(
        r#"(module (import "wrela" "memory" (memory 1 16384 shared)) (func (export "frame") (param f32 i32 i32)))"#,
    );
    assert!(matches!(unexported.err(), Some(Error::Program(m)) if m.contains("as `memory`")));
    let no_frame = load_wat(
        r#"(module (import "wrela" "memory" (memory 1 16384 shared)) (export "memory" (memory 0)))"#,
    );
    assert!(matches!(no_frame.err(), Some(Error::Program(m)) if m.contains("`frame(")));
    let wrong_frame = load_wat(
        r#"(module (import "wrela" "memory" (memory 1 16384 shared)) (export "memory" (memory 0)) (func (export "frame") (param f64)))"#,
    );
    assert!(matches!(wrong_frame.err(), Some(Error::Program(m)) if m.contains("`frame(")));
    assert!(matches!(compile_with(b"not wasm", None).err(), Some(Error::Program(_))));
}

/// The ABI's default limits are WebGPU's, as wgpu has them (`wrela_abi` can't see wgpu).
#[test]
fn the_default_limits_are_webgpus() {
    assert_eq!(crate::gpu::limits_of(&wgpu::Limits::default()), wrela_abi::Limits::DEFAULT);
}

#[test]
fn a_start_function_cant_call_the_host() {
    let err = load_wat(
        r#"(module (import "wrela" "submit" (func $submit (param i32 i32)))
             (import "wrela" "memory" (memory 1 16384 shared)) (export "memory" (memory 0))
             (func (export "frame") (param f32 i32 i32))
             (func $start (call $submit (i32.const 0) (i32.const 0)))
             (start $start))"#,
    )
    .err()
    .expect("rejected");
    assert_eq!(
        err.to_string(),
        "invalid program: its start function called the host; a wrela program calls the host only from an export"
    );
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
    let draw = Encoder::new().draw(0, 3, 1, &[b(1), b(2)], &U16).finish();
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
        (import "wrela" "memory" (memory 1 16384 shared)) (export "memory" (memory 0))
        (func (export "frame") (param f32 i32 i32) (call $submit (i32.const 65530) (i32.const 100))))"#;
    let err = load_wat(wat).expect("loads").frame(0.0, 1, 1).expect_err("fails");
    assert!(matches!(&err, Error::Trap(m) if m.contains("submit(65530, 100)")), "{err}");
}

#[test]
fn reports_program_traps() {
    let wat = r#"(module (import "wrela" "memory" (memory 1 16384 shared)) (export "memory" (memory 0)) (func (export "frame") (param f32 i32 i32) unreachable))"#;
    let err = load_wat(wat).expect("loads").frame(0.0, 1, 1).expect_err("traps");
    assert!(matches!(&err, Error::Trap(m) if m.contains("unreachable")), "{err}");
}

#[test]
fn checks_buffers() {
    let (op, why) = command_err(Encoder::new().create_buffer(1, 16).create_buffer(1, 16).finish());
    assert_eq!((op, why.as_str()), (Opcode::CreateBuffer, "handle 1 already names a buffer"));
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
        e.dispatch(9, [1; 3], &[b(1), b(2)], &U16);
    }));
    assert_eq!(
        (op, why.as_str()),
        (Opcode::Dispatch, "there's no pipeline 9 (the manifest has 9)")
    );
    let (_, why) = command_err(with_buffers(|e| {
        e.dispatch(0, [1; 3], &[b(1), b(2)], &U16);
    }));
    assert_eq!(why, "pipeline 0 (draw) is a render pipeline; Dispatch needs a compute pipeline");
    let (_, why) = command_err(with_buffers(|e| {
        e.dispatch(1, [1; 3], &[b(1)], &U16);
    }));
    assert_eq!(why, "pipeline 1 (compute) binds 2 resources, but the command lists 1");
    let (_, why) = command_err(with_buffers(|e| {
        e.dispatch(1, [1; 3], &[b(1), b(2)], &[0; 8]);
    }));
    assert_eq!(why, "pipeline 1 (compute) takes 16 uniform bytes, but the command has 8");
    let (_, why) = command_err(with_buffers(|e| {
        e.dispatch(1, [1; 3], &[b(1), b(9)], &U16);
    }));
    assert_eq!(why, "there's no buffer 9");
    let (_, why) = command_err(with_buffers(|e| {
        e.dispatch(1, [1; 3], &[b(1), b(1)], &U16);
    }));
    assert_eq!(why, "buffer 1 is used both read-only and read-write in one dispatch");
    let (_, why) = command_err(with_buffers(|e| {
        e.dispatch(1, [65536, 1, 1], &[b(1), b(2)], &U16);
    }));
    assert_eq!(why, "65536x1x1 workgroups is over the limit of 65535 per dimension");
    // Usage scopes are per dispatch: reading then writing a buffer in two dispatches is fine.
    run(&[vec![with_buffers(|e| {
        e.dispatch(1, [1; 3], &[b(1), b(2)], &U16).dispatch(1, [1; 3], &[b(2), b(1)], &U16);
    })]])
    .1
    .expect("separate scopes");
}

#[test]
fn checks_draws_across_the_pass() {
    let (op, why) = command_err(with_buffers(|e| {
        e.begin_screen_pass([0.0; 4]).draw(1, 3, 1, &[b(1), b(2)], &U16).present();
    }));
    assert_eq!(
        (op, why.as_str()),
        (Opcode::Draw, "pipeline 1 (compute) is a compute pipeline; Draw needs a render pipeline")
    );
    // Buffer 1 read by one draw and written by another in the same pass.
    let (_, why) = command_err(with_buffers(|e| {
        e.begin_screen_pass([0.0; 4])
            .draw(0, 3, 1, &[b(1), b(2)], &U16)
            .draw(0, 3, 1, &[b(2), b(1)], &U16)
            .present();
    }));
    assert_eq!(why, "buffer 2 is used both read-only and read-write in one pass");
    // The same in two passes is fine.
    run(&[vec![with_buffers(|e| {
        e.begin_screen_pass([0.0; 4]).draw(0, 3, 1, &[b(1), b(2)], &U16).present();
        e.begin_screen_pass([0.0; 4]).draw(0, 3, 1, &[b(2), b(1)], &U16).present();
    })]])
    .1
    .expect("separate passes");
}

#[test]
fn checks_writable_aliases_and_clear_colours() {
    // One buffer bound read-write twice by one dispatch or draw: WebGPU rejects it.
    let (_, why) = command_err(with_buffers(|e| {
        e.dispatch(3, [1; 3], &[b(1), b(1)], &U16);
    }));
    assert_eq!(why, "buffer 1 is bound read-write twice in one dispatch");
    let (_, why) = command_err(with_buffers(|e| {
        e.begin_screen_pass([0.0; 4]).draw(2, 3, 1, &[b(2), b(2)], &U16).present();
    }));
    assert_eq!(why, "buffer 2 is bound read-write twice in one draw");
    // Written by two draws of one pass is fine: both are writes.
    run(&[vec![with_buffers(|e| {
        e.begin_screen_pass([0.0; 4]).draw(2, 3, 1, &[b(1), b(2)], &U16).draw(
            2,
            3,
            1,
            &[b(2), b(1)],
            &U16,
        );
        e.present();
    })]])
    .1
    .expect("both writes");
    // WebGPU's clear colour is finite.
    for (clear, part) in [([f32::NAN, 0.5, 0.25, 1.0], "r"), ([0.0, 0.0, 0.0, f32::INFINITY], "a")]
    {
        let (_, why) = command_err(Encoder::new().begin_screen_pass(clear).present().finish());
        assert_eq!(why, format!("the clear colour's {part} isn't a finite number"));
    }
}

#[test]
fn a_rejected_command_leaves_no_trace() {
    // The sequencer passes a `BeginScreenPass` the checker then rejects: no pass is open
    // after it. (The frame traps before it ends, so the next one sends the same batch: it's
    // rejected the same way, not as a second pass.)
    let nan = Encoder::new().begin_screen_pass([f32::NAN, 0.0, 0.0, 1.0]).finish();
    let mut program = load_wat(&wat_gen::program(&[vec![nan]])).expect("loads");
    for i in 0..2 {
        let e = program.frame(i as f32 / 60.0, 64, 64).expect_err("the clear colour is rejected");
        assert!(matches!(e, Error::Command(_)), "{e}");
    }
    assert!(program.executor().log.is_empty(), "{:?}", program.executor().log);
}

/// Fails to carry out the first `Present`.
#[derive(Default)]
struct FailsOnce {
    failed: bool,
    log: Vec<&'static str>,
}

impl Executor for FailsOnce {
    fn execute(&mut self, cmd: &Command<'_>) -> Result<()> {
        if cmd.opcode() == Opcode::Present && !std::mem::replace(&mut self.failed, true) {
            return Err(Error::Gpu("lost".into()));
        }
        self.log.push(cmd.opcode().name());
        Ok(())
    }

    fn end_frame(&mut self) -> Result<()> {
        Ok(())
    }

    fn read_back(&mut self, _handle: u32, _offset: u32, size: u32) -> Result<Vec<u8>> {
        Ok(vec![0; size as usize])
    }
}

#[test]
fn nothing_runs_after_the_host_fails_a_command() {
    // What the executor holds after it fails isn't known: every later command fails.
    let pass = Encoder::new().begin_screen_pass([0.0; 4]).present().finish();
    let wasm = wat::parse_str(wat_gen::program(&[vec![pass.clone()], vec![pass]])).expect("WAT");
    let mut program = Program::instantiate(
        &compile_with(&wasm, None).expect("compiles"),
        &manifest(),
        FailsOnce::default(),
        Io::default(),
        false,
        1,
    )
    .expect("instantiates");
    let first = program.frame(0.0, 64, 64).expect_err("the host fails");
    assert!(matches!(first, Error::Gpu(_)), "{first}");
    let next = program.frame(1.0 / 60.0, 64, 64).expect_err("the host failed before");
    assert!(
        matches!(&next, Error::Command(CommandError { why, .. }) if why.contains("failed to carry out an earlier command")),
        "{next}"
    );
    assert_eq!(program.executor().log, ["BeginScreenPass"]);
}

#[test]
fn calls_other_exports() {
    let wasm = include_bytes!("../../../fixtures/first-light/game.wasm");
    let compiled = compile_with(wasm, None).expect("the fixture is a valid program");
    let mut program =
        Program::instantiate(&compiled, &manifest(), Recorder::default(), Io::default(), false, 1)
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

/// A program whose `ask` submits `batch`, `status` is request 4's status, and `take` takes its
/// answer into memory and returns the answer's first word.
fn requester(batch: &[u8], io: Io) -> Program<Recorder> {
    let wat = format!(
        r#"(module
  (import "wrela" "submit" (func $submit (param i32 i32)))
  (import "wrela" "request_status" (func $status (param i32) (result i32)))
  (import "wrela" "request_take" (func $take (param i32 i32)))
  (import "wrela" "memory" (memory 1 16384 shared)) (export "memory" (memory 0))
  (data (i32.const 16) "{}")
  (func (export "frame") (param f32 i32 i32))
  (func (export "ask") (call $submit (i32.const 16) (i32.const {})))
  (func (export "status") (result i32) (call $status (i32.const 4)))
  (func (export "take") (result i32)
    (call $take (i32.const 4) (i32.const 1024))
    (i32.load (i32.const 1024))))"#,
        wat_gen::escape(batch),
        batch.len()
    );
    let wasm = wat::parse_str(&wat).expect("test WAT compiles");
    Program::instantiate(
        &compile_with(&wasm, None).expect("compiles"),
        &manifest(),
        Recorder::default(),
        io,
        false,
        1,
    )
    .expect("instantiates")
}

fn call_i32(p: &mut Program<Recorder>, name: &str) -> i32 {
    match p.call(name, &[]).expect(name).as_slice() {
        [Value::I32(v)] => *v,
        other => panic!("{name} returned {other:?}"),
    }
}

/// A new, empty directory for a test's files.
fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wrela-requests-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn requests_are_answered_from_the_next_call_then_taken_once() {
    let build = scratch_dir("fetch");
    std::fs::create_dir_all(build.join("data")).expect("dir");
    std::fs::write(build.join("data/level.bin"), [1, 2, 3, 4, 5]).expect("write");
    let batch = Encoder::new().fetch(4, "data/level.bin").finish();
    let mut p =
        requester(&batch, Io { build: Some(build), storage: None, post: None, quiet: false });
    p.call("ask", &[]).expect("ask");
    assert_eq!(call_i32(&mut p, "status"), 5);
    assert_eq!(call_i32(&mut p, "take"), 0x0403_0201);
    assert_eq!(call_i32(&mut p, "status"), REQUEST_PENDING);
}

#[test]
fn storage_keeps_bytes_at_paths() {
    let storage = scratch_dir("storage");
    let io = Io { build: None, storage: Some(storage.clone()), post: None, quiet: false };
    let write = Encoder::new().storage_write(4, "saves/a", &[9, 8, 7, 6]).finish();
    let mut p = requester(&write, io.clone());
    p.call("ask", &[]).expect("ask");
    assert_eq!(call_i32(&mut p, "status"), 0);
    let read = Encoder::new().storage_read(4, "saves/a").finish();
    let mut p = requester(&read, io);
    p.call("ask", &[]).expect("ask");
    assert_eq!(call_i32(&mut p, "status"), 4);
    assert_eq!(call_i32(&mut p, "take"), 0x0607_0809);
    assert_eq!(std::fs::read(storage.join("saves/a")).expect("stored"), [9, 8, 7, 6]);
}

#[test]
fn requests_fail_without_a_place_to_go_and_each_number_is_used_once_at_a_time() {
    let batch = Encoder::new().storage_read(4, "saves/slot1").finish();
    let mut p = requester(&batch, Io::default());
    p.call("ask", &[]).expect("ask");
    let err = p.call("ask", &[]).expect_err("in use");
    assert_eq!(err.to_string(), "StorageRead failed: request 4 is already in use");
    assert_eq!(call_i32(&mut p, "status"), REQUEST_FAILED);
    assert_eq!(call_i32(&mut p, "take"), 0);
    assert!(
        p.call("take", &[]).expect_err("taken").to_string().contains("the request isn't answered")
    );
}

#[test]
fn a_readback_is_the_executors() {
    let batch =
        Encoder::new().create_buffer(1, 16).read_buffer(4, 1, 0, 8).destroy_buffer(1).finish();
    let mut p = requester(&batch, Io::default());
    p.call("ask", &[]).expect("ask");
    assert_eq!(call_i32(&mut p, "status"), 8);
    assert_eq!(call_i32(&mut p, "take"), 0x0707_0707);
}

#[test]
fn storage_paths_stay_inside_the_programs_storage() {
    let batch = Encoder::new().storage_write(4, "../escape", &[1]).finish();
    let mut p = requester(&batch, Io::default());
    let err = p.call("ask", &[]).expect_err("rejected");
    assert_eq!(
        err.to_string(),
        "StorageWrite failed: the storage path `../escape` has an empty, `.` or `..` part"
    );
}

#[test]
fn posts_go_to_the_handler_and_fail_without_one() {
    let batch = Encoder::new().post(4, "studio/echo", &[1, 2, 3, 4]).finish();
    let echo = PostHandler::new(|url, body| {
        assert_eq!(url, "studio/echo");
        Ok(body.iter().rev().copied().collect())
    });
    let mut p = requester(&batch, Io { post: Some(echo), ..Io::default() });
    p.call("ask", &[]).expect("ask");
    assert_eq!(call_i32(&mut p, "status"), 4);
    assert_eq!(call_i32(&mut p, "take"), 0x0102_0304);
    let mut p = requester(&batch, Io::default());
    p.call("ask", &[]).expect("ask");
    assert_eq!(call_i32(&mut p, "status"), REQUEST_FAILED);
    let elsewhere = Encoder::new().post(4, "https://example.com/x", &[]).finish();
    let err = requester(&elsewhere, Io::default()).call("ask", &[]).expect_err("rejected");
    assert!(err.to_string().contains("must be relative"), "{err}");
}

/// A program whose `take(cap)` reads up to `cap` input events to address 1024, and whose
/// `word(at)` reads the word at an address.
fn reader() -> Program<Recorder> {
    let wat = r#"(module
  (import "wrela" "input" (func $input (param i32 i32) (result i32)))
  (import "wrela" "memory" (memory 1 16384 shared)) (export "memory" (memory 0))
  (func (export "frame") (param f32 i32 i32))
  (func (export "take") (param i32) (result i32) (call $input (i32.const 1024) (local.get 0)))
  (func (export "past_the_end") (result i32) (call $input (i32.const 65530) (i32.const 1)))
  (func (export "word") (param i32) (result i32) (i32.load (local.get 0))))"#;
    let wasm = wat::parse_str(wat).expect("test WAT compiles");
    Program::instantiate(
        &compile_with(&wasm, None).expect("compiles"),
        &manifest(),
        Recorder::default(),
        Io::default(),
        false,
        1,
    )
    .expect("instantiates")
}

#[test]
fn input_is_read_oldest_first_and_what_isnt_read_waits() {
    use wrela_abi::input::{EventKind, SHIFT};
    let mut p = reader();
    let take = |p: &mut Program<Recorder>, cap: i32| match p.call("take", &[Value::I32(cap)]) {
        Ok(v) => v,
        Err(e) => panic!("take: {e}"),
    };
    let word = |p: &mut Program<Recorder>, at: i32| match p.call("word", &[Value::I32(at)]) {
        Ok(v) => v[0],
        Err(e) => panic!("word: {e}"),
    };
    p.push_input(Event::pointer(EventKind::PointerDown, 3.5, 4.0, 2, SHIFT));
    p.push_input(Event::text('é'));
    p.push_input(Event::key(true, 40, false, 0));
    assert_eq!(take(&mut p, 2), [Value::I32(2)]);
    assert_eq!(word(&mut p, 1024), Value::I32(EventKind::PointerDown as i32));
    assert_eq!(word(&mut p, 1028), Value::I32(SHIFT as i32));
    assert_eq!(word(&mut p, 1032), Value::I32(3.5f32.to_bits() as i32));
    assert_eq!(word(&mut p, 1040), Value::I32(2));
    assert_eq!(word(&mut p, 1024 + 24 + 8), Value::I32('é' as i32));
    assert_eq!(take(&mut p, 8), [Value::I32(1)]);
    assert_eq!(word(&mut p, 1024), Value::I32(EventKind::KeyDown as i32));
    assert_eq!(take(&mut p, 8), [Value::I32(0)]);
    p.push_input(Event::text('x'));
    let err = p.call("past_the_end", &[]).expect_err("past the end");
    assert!(err.to_string().contains("past the end of the program's memory"), "{err}");
}
