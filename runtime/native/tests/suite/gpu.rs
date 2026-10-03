//! End to end on the real GPU. Ignored by default: they take the GPU lock (shared with
//! tools/headless.py) and need a GPU. Run them with
//!
//!     cargo test -p wrela-host --test suite gpu:: -- --ignored

use crate::common::{self, TempDir};

use wrela_abi::hash::StateHash;
use wrela_abi::stream::Encoder;
use wrela_host::{Error, Host, Options, Value, frame_time};

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
            .dispatch(1, [1, 1, 1], &[1], &words(&[STEP, 64, 0, 0]))
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
                .draw(0, 3, 1, &[1], &uniforms)
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
#[ignore = "needs the GPU"]
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
    let decoder =
        png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).expect("open")));
    let mut reader = decoder.read_info().expect("png header");
    let mut decoded = vec![0; reader.output_buffer_size().expect("size")];
    reader.next_frame(&mut decoded).expect("png data");
    assert_eq!(decoded, run.frame);
}

#[test]
#[ignore = "needs the GPU"]
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
#[ignore = "needs the GPU"]
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
const STORE_MANIFEST: &str = r#"{
  "manifest_version": 1, "stream_version": 2, "wasm": "game.wasm",
  "pipelines": [
    { "name": "store", "shader": "store.wgsl", "kind": "compute", "entry": "store", "workgroup_size": [1, 1, 1],
      "uniform": { "binding": 0, "size": 16, "space": "uniform" },
      "buffers": [{ "binding": 1, "access": "read_write" }] },
    { "name": "cell", "shader": "cell.wgsl", "kind": "render", "vertex_entry": "vs", "fragment_entry": "fs",
      "uniform": { "binding": 0, "size": 32, "space": "uniform" }, "buffers": [] }
  ]
}"#;

/// A program submitting `frames`, loaded with the store and cell pipelines. The build's
/// directory comes first, so `let (_dir, host) = ...` drops the host before the directory.
fn store_host(name: &str, frames: &[Vec<Vec<u8>>], options: &Options) -> (TempDir, Host) {
    let wasm = wat::parse_str(common::wat::program(frames)).expect("compiles");
    let dir = common::build(
        name,
        STORE_MANIFEST,
        &[("store.wgsl", STORE_WGSL), ("cell.wgsl", CELL_WGSL)],
        &wasm,
    );
    let host = Host::load_with(&dir, options).expect("loads");
    (dir, host)
}

/// More dispatches and draws in one submission than the uniform ring starts with room for
/// (64 KiB at 256 bytes a slice): each must still see its own uniform bytes.
#[test]
#[ignore = "needs the GPU"]
fn every_command_gets_its_own_uniforms() {
    let n = 600u32;
    let (w, h) = (30u32, 20u32);
    let mut e = Encoder::new();
    e.create_buffer(1, n * 4);
    for i in 0..n {
        e.dispatch(0, [1, 1, 1], &[1], &words(&[i, i * 7 + 1, 0, 0]));
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
#[ignore = "needs the GPU"]
fn writes_apply_in_recorded_order() {
    let mut e = Encoder::new();
    e.create_buffer(1, 16)
        .write_buffer(1, 0, &words(&[1, 2, 3, 4]))
        .dispatch(0, [1, 1, 1], &[1], &words(&[1, 20, 0, 0]))
        .write_buffer(1, 8, &words(&[30]))
        .dispatch(0, [1, 1, 1], &[1], &words(&[3, 40, 0, 0]))
        .write_buffer(1, 0, &words(&[10]));
    let (_dir, mut host) = store_host("order", &[vec![e.finish()]], &Options::default());
    host.run_frames(&[0.0], 4, 4).expect("runs");
    assert_eq!(host.read_buffer(1).expect("reads back"), words(&[10, 20, 30, 40]));
}

#[test]
#[ignore = "needs the GPU"]
fn times_dispatches_and_passes_with_timestamps() {
    let frame = |i: u32| {
        let mut e = Encoder::new();
        if i == 0 {
            e.create_buffer(1, 64);
        }
        e.dispatch(0, [1, 1, 1], &[1], &words(&[i, i, 0, 0])).dispatch(
            0,
            [1, 1, 1],
            &[1],
            &words(&[i + 1, i, 0, 0]),
        );
        e.begin_screen_pass([0.0; 4])
            .draw(1, 6, 1, &[], &words(&[0, 0, 1, 1, u32::MAX, 0, 0, 0]))
            .present();
        vec![e.finish()]
    };
    let frames: Vec<_> = (0..5).map(frame).collect();
    let (_dir, mut host) =
        store_host("timestamps", &frames, &Options { timestamps: true, ..Options::default() });
    let run = host.run_frames(&[0.0; 5], 64, 64).expect("runs");
    assert_eq!(run.timings.len(), 15, "{:?}", run.timings);
    for (i, t) in run.timings.iter().enumerate() {
        assert_eq!(t.frame, i / 3);
        assert_eq!(t.label, if i % 3 == 2 { "screen pass" } else { "store" });
        assert!(t.nanos > 0.0 && t.nanos < 1e8, "{} {i} took {} ns", t.label, t.nanos);
    }
}

/// More timed passes in one submission than the query set holds: the host flushes early and
/// every pass still gets its own duration.
#[test]
#[ignore = "needs the GPU"]
fn times_more_passes_than_one_query_set_holds() {
    let mut e = Encoder::new();
    e.create_buffer(1, 4 * 300);
    for i in 0..300 {
        e.dispatch(0, [1, 1, 1], &[1], &words(&[i, i, 0, 0]));
    }
    let (_dir, mut host) = store_host(
        "many-timestamps",
        &[vec![e.finish()]],
        &Options { timestamps: true, ..Options::default() },
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
