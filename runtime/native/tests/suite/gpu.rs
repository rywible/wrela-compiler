//! End to end on the real GPU. `tools/check.sh` includes these tests. Run them directly with
//!
//!     cargo test -p wrela-host --test suite gpu:: -- --ignored

use crate::common::{self, TempDir};

use wrela_abi::hash::StateHash;
use wrela_abi::stream::{self, Binding, Encoder, Pass, TextureFormat};
use wrela_host::{Error, Host, Options, Timing, Value, frame_time, image};

const SEEDS: [u32; 4] = [0xff30_1810, 0xff20_70e0, 0xff40_1030, 0xff60_c0f0];
const STEP: u32 = 0x0002_0408;

/// first-light's `tri`, in the same f32 operations as its WAT.
fn tri(x: f32) -> f32 {
    4.0 * (x - (x + 0.5).floor()).abs() - 1.0
}

/// first-light's batches, rebuilt independently with the reference encoder.
fn first_light_hash(times: &[f32], width: u32, height: u32) -> u64 {
    let mut hash = StateHash::new();
    hash.update(
        &Encoder::new()
            .create_buffer(1, 256)
            .write_buffer(1, 0, &words(&SEEDS))
            .dispatch(1, [1, 1, 1], &[Binding::range(1, 0, 256)], &words(&[STEP, 64, 0, 0]))
            .finish(),
    );
    for &t in times {
        let (w, h) = (width as f32, height as f32);
        let phase = t * 0.25;
        let uniforms = [
            w,
            h,
            w * (0.5 + 0.3 * tri(phase)),
            h * (0.5 + 0.2 * tri(phase + 0.25)),
            t,
            0.15 * w.min(h),
            0.0,
            0.0,
        ];
        let uniforms = words(&uniforms.map(f32::to_bits));
        hash.update(
            &Encoder::new()
                .begin_screen_pass([0.02, 0.03, 0.06, 1.0])
                .draw(0, 3, 1, &[Binding::range(1, 0, 256)], &uniforms)
                .present()
                .finish(),
        );
    }
    hash.value()
}

fn words(ws: &[u32]) -> Vec<u8> {
    ws.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn pixel(frame: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    frame[i..i + 4].try_into().expect("4 bytes")
}

fn assert_near(got: [u8; 4], want: [f32; 4], what: &str) {
    for (g, w) in got.iter().zip(want) {
        assert!(
            (f32::from(*g) - w * 255.0).abs() <= 2.0,
            "{what}: got {got:?}, want {want:?} x 255"
        );
    }
}

#[test]
#[ignore = "needs a GPU"]
fn first_light_runs_end_to_end() {
    let (width, height) = (640, 360);
    let times: Vec<f32> = (0..60).map(|i| frame_time(i, 60.0)).collect();
    let mut host = Host::load(common::first_light()).expect("loads");
    let run = host.run_frames(&times, width, height).expect("runs");

    assert_eq!(run.hash, first_light_hash(&times, width, height), "state hash");
    assert_eq!(run.frame.len(), (width * height * 4) as usize);

    // The palette: the seeds, then what `fill` derived from them on the GPU.
    let palette: Vec<u32> = host
        .read_buffer(1)
        .expect("reads back")
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().expect("4 bytes")))
        .collect();
    let expected: Vec<u32> = (0..64u32)
        .map(|i| SEEDS[(i % 4) as usize].wrapping_add(if i < 4 { 0 } else { (i / 4) * STEP }))
        .collect();
    assert_eq!(palette, expected);

    // The disc's centre is the derived colour palette[5], pulsing; a far corner is background.
    let t = *times.last().expect("frames");
    let (cx, cy) =
        (640.0 * (0.5 + 0.3 * tri(t * 0.25)), 360.0 * (0.5 + 0.2 * tri(t * 0.25 + 0.25)));
    let pulse = 0.85 + 0.15 * (t * 3.0).cos();
    let disc = palette[5].to_le_bytes().map(|c| f32::from(c) / 255.0);
    let centre = pixel(&run.frame, width, cx as u32, cy as u32);
    assert_near(centre, [disc[0] * pulse, disc[1] * pulse, disc[2] * pulse, 1.0], "disc centre");
    let top = SEEDS[0].to_le_bytes().map(|c| f32::from(c) / 255.0);
    assert_near(pixel(&run.frame, width, 0, 0), [top[0], top[1], top[2], 1.0], "top-left corner");

    // CPU evaluation of another export.
    assert_eq!(host.call_export("tri", &[Value::F32(0.25)]).expect("calls"), [Value::F32(0.0)]);

    // A PNG of the frame.
    let dir = common::temp_dir("png");
    let path = dir.join("frame.png");
    run.write_png(&path).expect("writes");
    let decoded = image::read_png(&path).expect("decodes");
    assert!(decoded == (width, height, run.frame), "the PNG isn't the frame");
}

#[test]
#[ignore = "needs a GPU"]
fn runs_are_deterministic() {
    let times: Vec<f32> = (0..10).map(|i| frame_time(i, 30.0)).collect();
    let a = Host::load(common::first_light())
        .expect("loads")
        .run_frames(&times, 160, 90)
        .expect("runs");
    let b = Host::load(common::first_light())
        .expect("loads")
        .run_frames(&times, 160, 90)
        .expect("runs");
    assert_eq!(a.hash, b.hash);
    assert!(a.frame == b.frame, "frames differ between identical runs");
}

#[test]
#[ignore = "needs a GPU"]
fn captures_skip_preroll_and_reuse_readback_without_changing_frames() {
    let mut host = Host::load(common::first_light()).expect("loads");
    let mut all = Vec::new();
    let (full, _) = host
        .run_lockstep_seen(7, 60.0, 67, 41, &[], false, 2, |i, rgba| {
            all.push((i, rgba.to_vec()));
            Ok(())
        })
        .expect("captures every second frame");
    host.restart_with(&Options::default()).expect("fresh program");
    let mut kept = Vec::new();
    let (window, _) = host
        .run_lockstep_seen_from(7, 60.0, 67, 41, &[], false, 2, 3, |i, rgba| {
            kept.push((i, rgba.to_vec()));
            Ok(())
        })
        .expect("captures after warmup");
    assert_eq!(kept, all[2..]);
    assert_eq!(full.hash, window.hash, "every frame ran, including warmup");
    assert_eq!(full.frame, window.frame);
    // A smaller region reuses a larger allocation but must return just the requested pixels.
    let region = host.read_screen_region(2, 3, 5, 7).expect("small region");
    assert_eq!(region.len(), 5 * 7 * 4);
    for y in 0..7 {
        for x in 0..5 {
            assert_eq!(pixel(&region, 5, x, y), pixel(&full.frame, 67, x + 2, y + 3));
        }
    }
    // Growing the capture replaces the allocation; later smaller screens still decode cleanly.
    host.frame(1.0, 80, 50).expect("larger screen");
    assert_eq!(host.read_screen_region(0, 0, 80, 50).unwrap().len(), 80 * 50 * 4);
    host.frame(2.0, 16, 12).expect("smaller screen");
    assert_eq!(host.read_screen_region(0, 0, 16, 12).unwrap().len(), 16 * 12 * 4);
}

#[test]
#[ignore = "needs a GPU"]
fn shader_errors_name_the_pipeline_and_shader() {
    let dir = common::first_light_copy("bad-shader");
    std::fs::write(
        dir.join("fill.wgsl"),
        "@compute @workgroup_size(64) fn fill() { let x: u32 = 1.5; }",
    )
    .expect("write");
    let err = Host::load(&dir).err().expect("rejected");
    match &err {
        Error::Shader { pipeline, shader, message } => {
            assert_eq!((pipeline.as_str(), shader.as_str()), ("fill", "fill.wgsl"));
            assert!(!message.is_empty());
        }
        e => panic!("expected a shader error, got {e}"),
    }
}

/// A compute pipeline that stores `value` at `index`, and a render pipeline that fills the
/// pixel at `cell` with a colour; both take 16 uniform bytes.
const STORE_WGSL: &str = r#"
struct P { index: u32, value: u32, _pad: vec2u }
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read_write> out: array<u32>;
@compute @workgroup_size(1) fn store() { out[p.index] = p.value; }
"#;
const CELL_WGSL: &str = r#"
struct C { cell: vec2u, size: vec2u, colour: u32, _pad: u32, _pad2: vec2u }
@group(0) @binding(0) var<uniform> c: C;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let corner = array(vec2f(0, 0), vec2f(1, 0), vec2f(0, 1), vec2f(1, 0), vec2f(1, 1), vec2f(0, 1))[i];
    let px = (vec2f(c.cell) + corner) / vec2f(c.size);
    return vec4f(px.x * 2 - 1, 1 - px.y * 2, 0, 1);
}
@fragment fn fs() -> @location(0) vec4f { return unpack4x8unorm(c.colour); }
"#;
const STORE_PIPELINES: &str = r#"[
    { "name": "store", "shader": "store.wgsl", "kind": "compute", "entry": "store", "workgroup_size": [1, 1, 1],
      "uniform": { "binding": 0, "size": 16, "space": "uniform" },
      "bindings": [{ "binding": 1, "kind": "read_write" }] },
    { "name": "cell", "shader": "cell.wgsl", "kind": "render", "vertex_entry": "vs", "fragment_entry": "fs",
      "targets": [{ "colors": ["rgba8unorm"], "depth": false, "screen": true }], "outputs": ["float"],
      "uniform": { "binding": 0, "size": 32, "space": "uniform" }, "bindings": [] }
  ]"#;

/// A build of a program submitting `frames` (`common::wat::program`), with `pipelines` (a JSON
/// array) and `shaders`.
fn program_build(
    name: &str,
    pipelines: &str,
    shaders: &[(&str, &str)],
    frames: &[Vec<Vec<u8>>],
) -> TempDir {
    let wasm = wat::parse_str(common::wat::program(frames)).expect("compiles");
    common::build(name, &common::manifest(pipelines), shaders, &wasm)
}

/// The store and cell pipelines' shaders.
const STORE_SHADERS: [(&str, &str); 2] = [("store.wgsl", STORE_WGSL), ("cell.wgsl", CELL_WGSL)];

/// A program submitting `frames`, loaded with the store and cell pipelines. The build's
/// directory comes first, so `let (_dir, host) = ...` drops the host before the directory.
fn store_host(name: &str, frames: &[Vec<Vec<u8>>], options: &Options) -> (TempDir, Host) {
    let dir = program_build(name, STORE_PIPELINES, &STORE_SHADERS, frames);
    let host = Host::load_with(&dir, options).expect("loads");
    (dir, host)
}

/// A copy of first-light whose program traps as it starts: a build that never loads.
fn bad_start(name: &str) -> TempDir {
    let wasm = wat::parse_str(
        r#"(module
  (import "wrela" "memory" (memory 1 16384 shared))
  (export "memory" (memory 0))
  (func $start unreachable)
  (start $start)
  (func (export "frame") (param f32 i32 i32)))"#,
    )
    .expect("wasm");
    common::first_light_with(name, &wasm)
}

/// A comparison's next package starts as a cold load would, without preparing its unchanged
/// pipelines again. Even its first frame's empty screen and its IO directory must be fresh.
#[test]
#[ignore = "needs a GPU"]
fn restarting_in_another_build_reuses_pipelines_but_starts_fresh() {
    let (w, h) = (4, 4);
    let old = Encoder::new()
        .create_buffer(1, 16)
        .write_buffer(1, 0, &words(&[99; 4]))
        .storage_write(4, "save", b"old")
        .begin_screen_pass([1.0, 0.0, 0.0, 1.0])
        .present()
        .finish();
    let options = Options { record: true, ..Default::default() };
    let (old_dir, mut host) = store_host("restart-old", &[vec![old]], &options);
    host.run_frames(&[0.0, 1.0], w, h).expect("old program runs");
    let pipelines = host.pipelines_built();
    assert_eq!(pipelines, 2);

    let next = Encoder::new()
        .create_buffer(1, 16)
        .dispatch(0, [1, 1, 1], &[Binding::range(1, 0, 16)], &words(&[1, 42, 0, 0]))
        .storage_write(4, "save", b"next")
        .begin_screen_pass([0.0, 0.0, 0.0, 1.0])
        .draw(1, 6, 1, &[], &words(&[1, 2, w, h, 0xff00_ff00, 0, 0, 0]))
        .present()
        .finish();
    // The first frame submits nothing; the second exercises fresh WASM globals, resources
    // and request IDs (the previous program already used buffer 1 and request 4).
    let next_dir =
        program_build("restart-next", STORE_PIPELINES, &STORE_SHADERS, &[vec![], vec![next]]);
    host.restart_in(&next_dir, &options).expect("restarts in the next directory");
    assert_eq!(host.pipelines_built(), pipelines, "unchanged pipelines are reused");
    assert!(host.take_batches().is_empty(), "old recorded commands are gone");
    assert!(host.read_buffer(1).is_err(), "old resources are gone");
    let blank = host.run_frames(&[0.0], w, h).expect("empty first frame");
    assert!(blank.frame.iter().all(|b| *b == 0), "the old screen is gone");
    let restarted = host.run_frames(&[1.0], w, h).expect("new program runs");
    let buffer = host.read_buffer(1).expect("new buffer");
    assert_eq!(buffer, words(&[0, 42, 0, 0]));
    assert_eq!(pixel(&restarted.frame, w, 1, 2), [0, 255, 0, 255]);
    assert_eq!(std::fs::read(old_dir.join("storage/save")).expect("old storage"), b"old");
    assert_eq!(std::fs::read(next_dir.join("storage/save")).expect("new storage"), b"next");
    let batches = host.take_batches();

    let mut cold = Host::load_with(&next_dir, &options).expect("cold load of the next build");
    let cold_blank = cold.run_frames(&[0.0], w, h).expect("cold empty first frame");
    let cold_run = cold.run_frames(&[1.0], w, h).expect("cold program runs");
    assert_eq!(blank.hash, cold_blank.hash, "initial command state");
    assert!(blank.frame == cold_blank.frame, "initial pixels");
    assert_eq!(restarted.hash, cold_run.hash, "fresh command state");
    assert!(restarted.frame == cold_run.frame, "fresh pixels");
    assert_eq!(buffer, cold.read_buffer(1).expect("cold buffer"));
    assert_eq!(batches, cold.take_batches(), "fresh recording");
    drop(cold);

    // The same-directory wrapper now refers to the new package, and a changed shader is
    // rebuilt while its companion compute pipeline is kept.
    std::fs::write(
        next_dir.join("cell.wgsl"),
        CELL_WGSL.replace("unpack4x8unorm(c.colour)", "vec4f(0, 0, 1, 1)"),
    )
    .expect("change one shader");
    host.restart_with(&options).expect("restarts the new package");
    assert_eq!(host.pipelines_built(), pipelines + 1, "only the changed pipeline is built");
    let changed = host.run_frames(&[0.0, 1.0], w, h).expect("changed shader runs");
    assert_eq!(pixel(&changed.frame, w, 1, 2), [0, 0, 255, 255]);
}

/// A bad replacement must leave the running host able to execute its old build.
#[test]
#[ignore = "needs a GPU"]
fn failed_reload_and_restart_leave_the_old_build_usable() {
    let old_dir = common::first_light_copy("recover-old");
    let bad_shader = common::first_light_copy("recover-bad-shader");
    std::fs::write(bad_shader.join("fill.wgsl"), "this is not WGSL;").expect("bad shader");
    let bad_start = bad_start("recover-bad-start");
    let options = Options { reloadable: true, ..Options::default() };
    let mut host = Host::load_with(&old_dir, &options).expect("old build loads");
    host.run_frames(&[0.0], 8, 8).expect("old build starts");

    for candidate in [&bad_shader, &bad_start] {
        assert!(host.reload(Some(candidate)).is_err(), "candidate must fail to reload");
        assert_eq!(
            host.call_export("tri", &[Value::F32(0.25)]).expect("old build is callable"),
            [Value::F32(0.0)]
        );
        host.run_frames(&[0.1], 8, 8).expect("old build still draws");
        assert!(host.restart_in(candidate, &options).is_err(), "candidate must fail to restart");
        assert_eq!(
            host.call_export("tri", &[Value::F32(0.25)]).expect("old build is callable"),
            [Value::F32(0.0)]
        );
        host.run_frames(&[0.2], 8, 8).expect("old build still draws");
    }
}

#[test]
#[ignore = "needs a GPU"]
fn failed_restart_does_not_run_a_deferred_old_init() {
    let old_dir = common::first_light_with(
        "deferred-old",
        &wat::parse_str(
            r#"(module
  (import "wrela" "memory" (memory 1 16384 shared))
  (export "memory" (memory 0))
  (func (export "init") unreachable)
  (func (export "frame") (param f32 i32 i32))
  (func (export "ping") (result i32) (i32.const 7)))"#,
        )
        .expect("wasm"),
    );
    let bad_start = bad_start("deferred-bad-start");
    let options = Options { defer_init: true, ..Options::default() };
    let mut host = Host::load_with(&old_dir, &options).expect("loads without running init");

    assert!(host.restart_in(&bad_start, &options).is_err(), "new build fails");
    assert_eq!(host.call_export("ping", &[]).expect("old build stays callable"), [Value::I32(7)]);
    assert!(host.init().is_err(), "the old init is still deferred");
}

#[test]
#[ignore = "needs a GPU"]
fn failed_old_init_still_leaves_a_callable_host() {
    let old_dir = common::first_light_with(
        "recover-old-init-fails",
        &wat::parse_str(
            r#"(module
  (import "wrela" "memory" (memory 1 16384 shared))
  (export "memory" (memory 0))
  (import "wrela" "keep" (func $keep (param i32 i32)))
  (import "wrela" "kept" (func $kept (param i32 i32) (result i32)))
  (data (i32.const 100) "x")
  (func (export "init")
    (if (i32.ne (call $kept (i32.const 0) (i32.const 1)) (i32.const 0))
      (then unreachable))
    (call $keep (i32.const 100) (i32.const 1)))
  (func (export "frame") (param f32 i32 i32))
  (func (export "ping") (result i32) (i32.const 7)))"#,
        )
        .expect("wasm"),
    );
    let bad_start = bad_start("recover-init-bad-candidate");
    let options = Options { reloadable: true, ..Options::default() };
    let mut host = Host::load_with(&old_dir, &options).expect("old init first succeeds");

    assert!(host.reload(Some(&bad_start)).is_err(), "replacement fails");
    assert_eq!(host.call_export("ping", &[]).expect("host remains callable"), [Value::I32(7)]);
}

#[test]
#[ignore = "needs a GPU"]
fn failed_hand_over_keeps_the_old_build_usable_with_fresh_request_numbers() {
    let bad_start = bad_start("hand-over-bad-start");
    let path = format!("../{}", bad_start.file_name().expect("name").to_string_lossy());
    let command = Encoder::new().hand_over(1, &path).finish();
    let next = Encoder::new().storage_read(1, "missing").finish();
    let next_offset = command.len().next_multiple_of(4);
    let wasm = wat::parse_str(format!(
        r#"(module
  (import "wrela" "memory" (memory 1 16384 shared))
  (export "memory" (memory 0))
  (import "wrela" "submit" (func $submit (param i32 i32)))
  (import "wrela" "request_status" (func $status (param i32) (result i32)))
  (data (i32.const 0) "{}")
  (data (i32.const {next_offset}) "{}")
  (func (export "frame") (param f32 i32 i32)
    (call $submit (i32.const 0) (i32.const {})))
  (func (export "request")
    (call $submit (i32.const {next_offset}) (i32.const {})))
  (func (export "status") (result i32) (call $status (i32.const 1))))"#,
        common::wat::escape(&command),
        common::wat::escape(&next),
        command.len(),
        next.len(),
    ))
    .expect("wasm");
    let old_dir = common::first_light_with("hand-over-old", &wasm);
    let mut host = Host::load(&old_dir).expect("old build loads");

    host.run_frames(&[0.0], 8, 8).expect("failed hand-over returns to old build");
    assert_eq!(host.call_export("status", &[]).expect("new request ID is free"), [Value::I32(-1)]);
    host.call_export("request", &[]).expect("reuses request ID 1");
    assert_eq!(
        host.call_export("status", &[]).expect("old build remains callable"),
        [Value::I32(-3)],
        "the new request was answered: nothing is stored at its path"
    );
}

#[test]
#[ignore = "needs a GPU"]
fn init_and_export_handovers_take_effect_when_the_call_returns() {
    let program = |batch: &[u8], init: bool, marker: u32| {
        let submit = format!("(call $submit (i32.const 0) (i32.const {}))", batch.len());
        let init = if init { submit.as_str() } else { "" };
        wat::parse_str(format!(
            r#"(module
                (import "wrela" "submit" (func $submit (param i32 i32)))
                (import "wrela" "memory" (memory 1 16384 shared))
                (export "memory" (memory 0))
                (data (i32.const 0) "{}")
                (func (export "init") {init})
                (func (export "frame") (param f32 i32 i32))
                (func (export "switch") {submit})
                (func (export "marker") (result i32) (i32.const {marker})))"#,
            common::wat::escape(batch),
        ))
        .unwrap()
    };
    let target = common::build(
        "init-handover-target",
        &common::manifest("[]"),
        &[],
        &program(&Encoder::new().finish(), false, 42),
    );
    let path = format!("../{}", target.file_name().unwrap().to_string_lossy());
    let batch = Encoder::new().hand_over(1, &path).finish();
    let source = common::build(
        "init-handover-source",
        &common::manifest("[]"),
        &[],
        &program(&batch, false, 11),
    );
    let next = common::build(
        "init-handover-next",
        &common::manifest("[]"),
        &[],
        &program(&batch, true, 22),
    );
    for trigger in ["load", "deferred", "export", "reload", "restart", "new-device"] {
        let options =
            Options { defer_init: trigger == "deferred", reloadable: true, ..Options::default() };
        let dir = if matches!(trigger, "load" | "deferred") { &next } else { &source };
        let mut host = Host::load_with(dir, &options).unwrap();
        match trigger {
            "load" => {}
            "deferred" => host.init().unwrap(),
            "export" => {
                host.call_export("switch", &[]).unwrap();
            }
            "reload" => host.reload(Some(&next)).unwrap(),
            "restart" => host.restart_in(&next, &options).unwrap(),
            "new-device" => host
                .restart_in(&next, &Options { timing: Timing::Span, ..options.clone() })
                .unwrap(),
            _ => unreachable!(),
        }
        assert_eq!(host.build_dir(), target.as_ref(), "{trigger}");
        assert_eq!(host.call_export("marker", &[]).unwrap(), [Value::I32(42)], "{trigger}");
    }
}

/// More dispatches and draws in one submission than the uniform ring starts with room for
/// (64 KiB at 256 bytes a slice): each must still see its own uniform bytes.
#[test]
#[ignore = "needs a GPU"]
fn every_command_gets_its_own_uniforms() {
    let n = 600u32;
    let (w, h) = (30u32, 20u32);
    let mut e = Encoder::new();
    e.create_buffer(1, n * 4);
    for i in 0..n {
        e.dispatch(0, [1, 1, 1], &[Binding::range(1, 0, n * 4)], &words(&[i, i * 7 + 1, 0, 0]));
    }
    e.begin_screen_pass([0.0, 0.0, 0.0, 1.0]);
    for i in 0..w * h {
        e.draw(1, 6, 1, &[], &words(&[i % w, i / w, w, h, 0xff00_0000 | i, 0, 0, 0]));
    }
    e.present();
    let (_dir, mut host) = store_host("uniforms", &[vec![e.finish()]], &Options::default());
    let run = host.run_frames(&[0.0], w, h).expect("runs");
    let out = host.read_buffer(1).expect("reads back");
    assert_eq!(out, words(&(0..n).map(|i| i * 7 + 1).collect::<Vec<_>>()));
    for i in 0..w * h {
        let want = (0xff00_0000 | i).to_le_bytes();
        assert_eq!(pixel(&run.frame, w, i % w, i / w), want, "cell {i}");
    }
}

/// A `WriteBuffer` after a dispatch doesn't change what the dispatch saw.
#[test]
#[ignore = "needs a GPU"]
fn writes_apply_in_recorded_order() {
    let mut e = Encoder::new();
    e.create_buffer(1, 16)
        .write_buffer(1, 0, &words(&[1, 2, 3, 4]))
        .dispatch(0, [1, 1, 1], &[Binding::range(1, 0, 16)], &words(&[1, 20, 0, 0]))
        .write_buffer(1, 8, &words(&[30]))
        .dispatch(0, [1, 1, 1], &[Binding::range(1, 0, 16)], &words(&[3, 40, 0, 0]))
        .write_buffer(1, 0, &words(&[10]));
    let (_dir, mut host) = store_host("order", &[vec![e.finish()]], &Options::default());
    host.run_frames(&[0.0], 4, 4).expect("runs");
    assert_eq!(host.read_buffer(1).expect("reads back"), words(&[10, 20, 30, 40]));
}

/// Two pipelines share the same three resources. Destroying and recreating each input must
/// invalidate both cached groups, while recorded dispatches retain the old resource.
#[test]
#[ignore = "needs a GPU"]
fn cached_bind_groups_follow_resource_lifetimes() {
    let shader = r#"
@group(0) @binding(0) var<storage, read> a: array<u32>;
@group(0) @binding(1) var<storage, read> b: array<u32>;
@group(0) @binding(2) var<storage, read_write> out: array<u32>;
@compute @workgroup_size(1) fn sum() { out[0] += a[0] + b[0]; }
"#;
    let pipelines: Vec<_> = ["sum_a", "sum_b"]
        .into_iter()
        .map(|name| {
            format!(
                r#"{{
            "name": "{name}", "shader": "sum.wgsl", "kind": "compute", "entry": "sum",
            "workgroup_size": [1, 1, 1], "uniform": null,
            "bindings": [
                {{ "binding": 0, "kind": "read" }},
                {{ "binding": 1, "kind": "read" }},
                {{ "binding": 2, "kind": "read_write" }}
            ]
        }}"#
            )
        })
        .collect();
    let bindings = [1, 2, 3].map(|handle| Binding::range(handle, 0, 4));
    let mut encoder = Encoder::new();
    for (handle, value) in [(1, 7), (2, 11), (3, 1)] {
        encoder.create_buffer(handle, 4).write_buffer(handle, 0, &words(&[value]));
    }
    for replacement in [None, Some((1, 10)), Some((2, 3))] {
        if let Some((handle, value)) = replacement {
            encoder.destroy_buffer(handle).create_buffer(handle, 4).write_buffer(
                handle,
                0,
                &words(&[value]),
            );
        }
        for pipeline in 0..2 {
            encoder.dispatch(pipeline, [1; 3], &bindings, &[]);
        }
    }
    let dir = program_build(
        "bind-group-lifetimes",
        &format!("[{}]", pipelines.join(",")),
        &[("sum.wgsl", shader)],
        &[vec![encoder.finish()]],
    );
    let mut host = Host::load(&dir).expect("loads");
    host.frame(0.0, 16, 16).expect("dispatches");
    assert_eq!(host.read_buffer(3).expect("recorded work finishes"), words(&[105]));
}

#[test]
#[ignore = "needs a GPU"]
fn screen_readback_removes_padding_and_keeps_region_rows_in_order() {
    let widths = [3, 64, 65];
    let height = 3;
    let frames: Vec<_> = widths
        .iter()
        .map(|&width| {
            let mut encoder = Encoder::new();
            encoder.begin_screen_pass([0.0; 4]);
            for y in 0..height {
                for x in 0..width {
                    let colour = 0xff00_0000 | (y << 8) | x;
                    encoder.draw(1, 6, 1, &[], &words(&[x, y, width, height, colour, 0, 0, 0]));
                }
            }
            vec![encoder.present().finish()]
        })
        .collect();
    let (_dir, mut host) = store_host("screen-row-padding", &frames, &Options::default());
    for (i, width) in widths.into_iter().enumerate() {
        host.frame(i as f32, width, height).expect("draws uniquely coloured pixels");
        let frame = host.read_screen().expect("reads packed screen rows");
        assert_eq!(frame.len(), (width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                assert_eq!(pixel(&frame, width, x, y), [x as u8, y as u8, 0, 255]);
            }
        }
        let size = [width - 2, height - 1];
        let region = host.read_screen_region(1, 1, size[0], size[1]).expect("reads interior rows");
        assert_eq!(region.len(), (size[0] * size[1] * 4) as usize);
        for y in 0..size[1] {
            for x in 0..size[0] {
                assert_eq!(pixel(&region, size[0], x, y), pixel(&frame, width, x + 1, y + 1));
            }
        }
        assert!(host.read_screen_region(0, 0, 0, 1).is_err());
        assert!(host.read_screen_region(width - 1, 0, 2, 1).is_err());
    }
}

/// Three draws in one pass keep their pipeline and uniform order.
#[test]
#[ignore = "needs a GPU"]
fn serial_draws_keep_pipeline_and_uniform_order() {
    let pipelines: Vec<_> = ["cell_a", "cell_b"]
        .into_iter()
        .map(|name| {
            format!(
                r#"{{ "name": "{name}", "shader": "cell.wgsl", "kind": "render",
                    "vertex_entry": "vs", "fragment_entry": "fs",
                    "targets": [{{ "colors": ["rgba8unorm"], "depth": false, "screen": true }}], "outputs": ["float"],
                    "uniform": {{ "binding": 0, "size": 32, "space": "uniform" }},
                    "bindings": [] }}"#
            )
        })
        .collect();
    let mut encoder = Encoder::new();
    encoder.label("ordered draws").begin_screen_pass([0.0, 0.0, 0.0, 1.0]);
    // Two whole cells, then the first cell's first triangle over its earlier colour.
    for (pipeline, vertices, cell, colour) in
        [(0, 6, 0, 0xff00_00ff), (1, 6, 1, 0xff00_ff00), (0, 3, 0, 0xffff_0000)]
    {
        encoder.draw(pipeline, vertices, 1, &[], &words(&[cell, 0, 2, 1, colour, 0, 0, 0]));
    }
    let dir = program_build(
        "serial-draw-order",
        &format!("[{}]", pipelines.join(",")),
        &[("cell.wgsl", CELL_WGSL)],
        &[vec![encoder.present().finish()]],
    );
    let options = Options { timing: Timing::Serial, ..Options::default() };
    let mut host = Host::load_with(&dir, &options).expect("loads");
    host.frame(0.0, 8, 4).expect("three ordered draws");
    let frame = host.read_screen().expect("the draws' output");
    assert_eq!(pixel(&frame, 8, 0, 0), [0, 0, 255, 255], "the last draw overwrote the first");
    assert_eq!(pixel(&frame, 8, 3, 3), [255, 0, 0, 255], "outside its triangle stays red");
    assert_eq!(pixel(&frame, 8, 7, 3), [0, 255, 0, 255], "the second draw's uniforms");
    let timings = host.take_timings().expect("timings");
    assert_eq!(timings.len(), 1);
    assert_eq!(timings[0].label, "ordered draws");
}

#[test]
#[ignore = "needs a GPU"]
fn export_timings_are_collected_without_running_a_frame() {
    for timing in [Timing::Span, Timing::Serial] {
        let batch = Encoder::new()
            .create_buffer(1, 16)
            .dispatch(0, [1, 1, 1], &[Binding::range(1, 0, 16)], &words(&[0, 42, 0, 0]))
            .finish();
        let (_dir, mut host) =
            store_host("export-timings", &[vec![batch]], &Options { timing, ..Options::default() });
        host.call_export("frame", &[Value::F32(0.0), Value::I32(4), Value::I32(4)])
            .expect("the export dispatches");
        let timings = host.take_timings().expect("collects the export's GPU work");
        assert_eq!(timings.len(), 1, "{timing:?}");
        assert_eq!(timings[0].label, "store");
        assert!(host.take_timings().unwrap().is_empty(), "a timing is taken once");
        assert_eq!(host.read_buffer(1).unwrap()[..4], 42u32.to_le_bytes());
    }
}

#[test]
#[ignore = "needs a GPU"]
fn manual_frames_do_not_inherit_the_previous_frames_gpu_label() {
    let first = Encoder::new()
        .create_buffer(1, 16)
        .label("first frame")
        .dispatch(0, [1, 1, 1], &[Binding::range(1, 0, 16)], &words(&[0, 11, 0, 0]))
        .finish();
    let second = Encoder::new()
        .dispatch(0, [1, 1, 1], &[Binding::range(1, 0, 16)], &words(&[0, 42, 0, 0]))
        .finish();
    let (_dir, mut host) = store_host(
        "manual-frame-labels",
        &[vec![first], vec![second]],
        &Options { timing: Timing::Span, ..Options::default() },
    );
    host.frame(0.0, 4, 4).unwrap();
    host.frame(1.0, 4, 4).unwrap();
    let timings = host.take_timings().unwrap();
    assert_eq!(
        timings.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(),
        ["first frame", "store"]
    );
    assert_eq!(host.read_buffer(1).unwrap()[..4], 42u32.to_le_bytes());
}

#[test]
#[ignore = "needs a GPU"]
fn a_two_byte_texture_upload_reaches_the_shader() {
    let shader = r#"
        @group(0) @binding(0) var source: texture_2d<f32>;
        @group(0) @binding(1) var<storage, read_write> out: array<f32>;
        @compute @workgroup_size(1) fn read_texel() {
            out[0] = textureLoad(source, vec2i(0, 0), 0).x;
        }
    "#;
    let pipelines = r#"[{ "name": "read_texel", "shader": "read.wgsl", "kind": "compute",
        "entry": "read_texel", "workgroup_size": [1, 1, 1],
        "bindings": [{ "binding": 0, "kind": "texture", "format": "r16float" },
                     { "binding": 1, "kind": "read_write" }] }]"#;
    let batch = Encoder::new()
        .create_buffer(2, 4)
        .create_texture(1, 1, 1, TextureFormat::R16Float, false)
        .write_texture(1, [0, 0], [1, 1], &[0x00, 0x3c])
        .dispatch(0, [1, 1, 1], &[Binding::of(1), Binding::range(2, 0, 4)], &[])
        .finish();
    let dir =
        program_build("two-byte-texture", pipelines, &[("read.wgsl", shader)], &[vec![batch]]);
    let mut host = Host::load(&dir).unwrap();
    host.frame(0.0, 4, 4).expect("the two-byte half-float texel uploads");
    assert_eq!(host.read_buffer(2).unwrap(), 1.0f32.to_le_bytes());
}

#[test]
#[ignore = "needs a GPU"]
fn hot_reload_clears_the_retired_builds_debug_flag() {
    let pipelines = r#"[{ "name": "flag", "shader": "flag.wgsl", "kind": "compute",
        "entry": "flag_error", "workgroup_size": [1, 1, 1], "bindings": [], "debug_flag": 0 }]"#;
    let shader = r#"
        @group(0) @binding(0) var<storage, read_write> flag: atomic<u32>;
        @compute @workgroup_size(1) fn flag_error() { atomicStore(&flag, 1u); }
    "#;
    let failing = Encoder::new().dispatch(0, [1, 1, 1], &[], &[]).finish();
    let old =
        program_build("reload-debug-old", pipelines, &[("flag.wgsl", shader)], &[vec![failing]]);
    let clean = Encoder::new().begin_screen_pass([0.0, 1.0, 0.0, 1.0]).present().finish();
    let next = program_build("reload-debug-next", "[]", &[], &[vec![clean]]);
    let options = Options { reloadable: true, ..Options::default() };
    let mut host = Host::load_with(&old, &options).unwrap();
    assert!(host.frame(0.0, 4, 4).unwrap_err().to_string().contains("indexed an array"));
    host.reload(Some(&next)).expect("loads the next build");
    host.frame(0.0, 4, 4).expect("the retired build's error does not poison its replacement");
    assert_eq!(pixel(&host.read_screen().unwrap(), 4, 0, 0), [0, 255, 0, 255]);
}

#[test]
#[ignore = "needs a GPU"]
fn adjacent_passes_with_different_buffer_access_do_not_join() {
    let vertex = r#"
        @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
            let corners = array(vec2f(-1, -1), vec2f(3, -1), vec2f(-1, 3));
            return vec4f(corners[i], 0, 1);
        }
    "#;
    let read = format!(
        r#"{vertex}
        @group(0) @binding(0) var<storage, read> data: array<u32>;
        @fragment fn fs() -> @location(0) vec4f {{ return unpack4x8unorm(data[0]); }}"#
    );
    let write = format!(
        r#"{vertex}
        @group(0) @binding(0) var<storage, read_write> data: array<atomic<u32>>;
        @fragment fn fs() -> @location(0) vec4f {{
            atomicStore(&data[0], 42u);
            return vec4f(0, 1, 0, 1);
        }}"#
    );
    let pipelines = r#"[
        { "name": "read", "shader": "read.wgsl", "kind": "render",
          "vertex_entry": "vs", "fragment_entry": "fs",
          "targets": [{ "colors": ["rgba8unorm"], "depth": false }], "outputs": ["float"],
          "bindings": [{ "binding": 0, "kind": "read", "stage": "fragment" }] },
        { "name": "write", "shader": "write.wgsl", "kind": "render",
          "vertex_entry": "vs", "fragment_entry": "fs",
          "targets": [{ "colors": ["rgba8unorm"], "depth": false }], "outputs": ["float"],
          "bindings": [{ "binding": 0, "kind": "read_write", "stage": "fragment" }] }
    ]"#;
    let pass =
        Pass { join: false, ..Pass::one(2, false, [0.0; 4]).with_depth(stream::NONE, false, 1.0) };
    let batch = Encoder::new()
        .create_buffer(1, 16)
        .write_buffer(1, 0, &words(&[0xffff_ffff, 0, 0, 0]))
        .create_texture(2, 4, 4, TextureFormat::Rgba8, false)
        .begin_pass(pass)
        .draw(0, 3, 1, &[Binding::range(1, 0, 16)], &[])
        .end_pass()
        .begin_pass(Pass { join: true, ..pass.keeping_color(true) })
        .draw(0, 3, 1, &[Binding::range(1, 0, 16)], &[])
        .end_pass()
        .begin_pass(Pass { join: true, ..pass.keeping_color(true) })
        .draw(1, 3, 1, &[Binding::range(1, 0, 16)], &[])
        .end_pass()
        .finish();
    let dir = program_build(
        "incompatible-pass-join",
        pipelines,
        &[("read.wgsl", &read), ("write.wgsl", &write)],
        &[vec![batch]],
    );
    let mut host = Host::load_with(&dir, &Options { timing: Timing::Span, ..Options::default() })
        .expect("both render pipelines are valid");
    host.frame(0.0, 4, 4).expect("individually valid usage scopes stay separate");
    assert_eq!(host.take_timings().unwrap().len(), 2, "only compatible passes join");
    assert_eq!(host.read_buffer(1).unwrap()[..4], 42u32.to_le_bytes());
}

#[test]
#[ignore = "needs a GPU"]
fn times_dispatches_and_passes_with_timestamps() {
    let frame = |i: u32| {
        let mut e = Encoder::new();
        if i == 0 {
            e.create_buffer(1, 64);
        }
        e.dispatch(0, [1, 1, 1], &[Binding::range(1, 0, 64)], &words(&[i, i, 0, 0])).dispatch(
            0,
            [1, 1, 1],
            &[Binding::range(1, 0, 64)],
            &words(&[i + 1, i, 0, 0]),
        );
        e.begin_screen_pass([0.0; 4])
            .draw(1, 6, 1, &[], &words(&[0, 0, 1, 1, u32::MAX, 0, 0, 0]))
            .present();
        vec![e.finish()]
    };
    let frames: Vec<_> = (0..5).map(frame).collect();
    let (_dir, mut host) =
        store_host("timestamps", &frames, &Options { timing: Timing::Span, ..Options::default() });
    let run = host.run_frames(&[0.0; 5], 64, 64).expect("runs");
    assert_eq!(run.timings.len(), 15, "{:?}", run.timings);
    for (i, t) in run.timings.iter().enumerate() {
        assert_eq!(t.frame, i / 3);
        assert_eq!(t.label, if i % 3 == 2 { "screen pass" } else { "store" });
        assert!(
            t.nanos.is_finite() && t.nanos >= 0.0,
            "{} {i} has invalid duration {} ns",
            t.label,
            t.nanos
        );
    }
}

/// More timed passes in one submission than the query set holds: the host flushes early and
/// every pass still gets its own duration.
#[test]
#[ignore = "needs a GPU"]
fn times_more_passes_than_one_query_set_holds() {
    let mut e = Encoder::new();
    e.create_buffer(1, 4 * 300);
    for i in 0..300 {
        e.dispatch(0, [1, 1, 1], &[Binding::range(1, 0, 4 * 300)], &words(&[i, i, 0, 0]));
    }
    let (_dir, mut host) = store_host(
        "many-timestamps",
        &[vec![e.finish()]],
        &Options { timing: Timing::Span, ..Options::default() },
    );
    let run = host.run_frames(&[0.0], 4, 4).expect("runs");
    assert_eq!(run.timings.len(), 300);
    assert!(
        run.timings.iter().all(|t| t.nanos > 0.0),
        "{:?}",
        run.timings.iter().filter(|t| t.nanos <= 0.0).count()
    );
    assert_eq!(host.read_buffer(1).expect("reads back"), words(&(0..300).collect::<Vec<_>>()));
}

/// A render pipeline that draws `pos[i]` at depth `m.z` in colour `m.colour`, three times over:
/// with no culling and no bias, culling back faces with a depth bias of -1000 steps, and culling
/// back faces with none.
const MESH_WGSL: &str = r#"
struct M { colour: u32, z: f32, _pad: vec2u }
@group(0) @binding(0) var<uniform> m: M;
@group(0) @binding(1) var<storage, read> pos: array<vec2f>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f { return vec4f(pos[i], m.z, 1); }
@fragment fn fs() -> @location(0) vec4f { return unpack4x8unorm(m.colour); }
"#;
const MESH_PIPELINES: &str = r#"[
    { "name": "plain", "shader": "mesh.wgsl", "kind": "render", "vertex_entry": "vs", "fragment_entry": "fs",
      "targets": [{ "colors": ["rgba8unorm"], "depth": true, "screen": true }], "outputs": ["float"],
      "uniform": { "binding": 0, "size": 16, "space": "uniform" }, "bindings": [{ "binding": 1, "kind": "read" }] },
    { "name": "biased", "shader": "mesh.wgsl", "kind": "render", "vertex_entry": "vs", "fragment_entry": "fs",
      "cull": "back", "depth_bias": { "constant": -1000, "slope_scale": 0, "clamp": 0 },
      "targets": [{ "colors": ["rgba8unorm"], "depth": true, "screen": true }], "outputs": ["float"],
      "uniform": { "binding": 0, "size": 16, "space": "uniform" }, "bindings": [{ "binding": 1, "kind": "read" }] },
    { "name": "culled", "shader": "mesh.wgsl", "kind": "render", "vertex_entry": "vs", "fragment_entry": "fs",
      "cull": "back",
      "targets": [{ "colors": ["rgba8unorm"], "depth": true, "screen": true }], "outputs": ["float"],
      "uniform": { "binding": 0, "size": 16, "space": "uniform" }, "bindings": [{ "binding": 1, "kind": "read" }] }
  ]"#;

/// Indexed indirect draws (stream v6), with each render pipeline's cull mode and depth bias
/// (manifest v4): two quads from one index buffer, the left one's triangles facing the screen
/// and the right one's facing away. Red covers both; green, culling back faces and biased
/// towards the eye, covers only the left one, though it's at red's depth; blue, culling back
/// faces with no bias, is hidden by green's depth.
#[test]
#[ignore = "needs a GPU"]
fn indexed_indirect_draws_cull_and_bias() {
    let f = |x: f32| x.to_bits();
    let positions = [
        [-1.0, -1.0],
        [0.0, -1.0],
        [0.0, 1.0],
        [-1.0, 1.0], // left: counter-clockwise
        [0.0, -1.0],
        [1.0, -1.0],
        [1.0, 1.0],
        [0.0, 1.0], // right
    ];
    let pos: Vec<u32> = positions.iter().flat_map(|[x, y]: &[f32; 2]| [f(*x), f(*y)]).collect();
    // The right quad's triangles run clockwise: they face away.
    let indices = [0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6];
    let (w, h) = (8, 4);
    let mut e = Encoder::new();
    e.create_buffer(1, 64)
        .write_buffer(1, 0, &words(&pos))
        .create_buffer(2, 48)
        .write_buffer(2, 0, &words(&indices))
        .create_buffer(3, 32)
        .write_buffer(3, 0, &words(&[12, 1, 0, 0, 0, 0, 0, 0]))
        .create_texture(9, w, h, TextureFormat::Depth32Float, false)
        .begin_pass(Pass {
            join: false,
            ..Pass::one(stream::SCREEN, false, [0.0, 0.0, 0.0, 1.0]).with_depth(9, false, 1.0)
        });
    let pos = [Binding::range(1, 0, 64)];
    for (pipeline, colour) in [(0, 0xff00_00ff_u32), (1, 0xff00_ff00), (2, 0xffff_0000)] {
        e.draw_indexed_indirect(pipeline, [2, 0, 48], 3, 0, &pos, &words(&[colour, f(0.5), 0, 0]));
    }
    e.present();
    let dir =
        program_build("indexed", MESH_PIPELINES, &[("mesh.wgsl", MESH_WGSL)], &[vec![e.finish()]]);
    let mut host = Host::load(&dir).expect("loads");
    let run = host.run_frames(&[0.0], w, h).expect("runs");
    for y in 0..h {
        for x in 0..w {
            let want = if x < w / 2 { [0, 255, 0, 255] } else { [255, 0, 0, 255] };
            assert_eq!(pixel(&run.frame, w, x, y), want, "pixel ({x}, {y})");
        }
    }
}

/// An indirect draw starts at the instance named by its arguments, including indexed draws.
#[test]
#[ignore = "needs a GPU"]
fn indirect_draws_keep_their_first_instance() {
    let source = r#"
@vertex fn vs(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32)
    -> @builtin(position) vec4f {
    let corners = array(vec2f(-1, -1), vec2f(3, -1), vec2f(-1, 3));
    let position = corners[vertex] + vec2f(f32(instance) - 5.0, 0.0) * 10.0;
    return vec4f(position, 0, 1);
}
@fragment fn fs() -> @location(0) vec4f { return vec4f(0, 1, 0, 1); }
"#;
    let pipelines = r#"[{ "name": "instance", "shader": "instance.wgsl", "kind": "render",
        "vertex_entry": "vs", "fragment_entry": "fs",
        "targets": [{ "colors": ["rgba8unorm"], "depth": false, "screen": true }], "outputs": ["float"], "bindings": [] }]"#;
    for indexed in [false, true] {
        let mut encoder = Encoder::new();
        encoder.create_buffer(1, 20).create_buffer(2, 12);
        encoder.write_buffer(2, 0, &words(&[0, 1, 2]));
        let args = if indexed { vec![3, 1, 0, 0, 5] } else { vec![3, 1, 0, 5] };
        encoder.write_buffer(1, 0, &words(&args)).begin_screen_pass([0.0, 0.0, 0.0, 1.0]);
        if indexed {
            encoder.draw_indexed_indirect(0, [2, 0, 12], 1, 0, &[], &[]);
        } else {
            encoder.draw_indirect(0, 1, 0, &[], &[]);
        }
        encoder.present();
        let dir = program_build(
            "indirect-first-instance",
            pipelines,
            &[("instance.wgsl", source)],
            &[vec![encoder.finish()]],
        );
        let mut host = Host::load(&dir).expect("loads");
        let run = host.run_frames(&[0.0], 4, 4).expect("runs");
        assert_eq!(pixel(&run.frame, 4, 2, 2), [0, 255, 0, 255], "indexed: {indexed}");
    }
}
