//! Stand-ins (language.md §18.12): `@standin(cheap)` on a kernel or fragment shader gives its
//! pipeline a second, of the stand-in, with the same bindings and uniform block; the manifest
//! names it (`standin`), and the browser runs it in the first one's place until that's built.
//! One that can't stand in is E0711.

use crate::{scratch, write_package};

const PROGRAM: &str = r#"use std::gpu::{ClipPosition, GlobalId, GpuBuffer, Slots, VertexIndex, begin_screen_pass, buffer, dispatch, draw}

@compute(64)
@standin(quick)
fn slow(out: mut Slots<f32>, id: GlobalId, scale: f32) {
    var a = f32(id.x)
    for _i in 0..64 {
        a = sin(a) * scale + cos(a)
    }
    out[id] = a
}

@compute(64)
fn quick(out: mut Slots<f32>, id: GlobalId, _scale: f32) {
    out[id] = 0.0
}

@vertex
fn tri(v: VertexIndex) -> ClipPosition {
    let x = f32(v.index % 2)
    ClipPosition { position: vec4(x, f32(v.index / 2), 0.0, 1.0) }
}

@fragment
@standin(grey)
fn shade(tint: vec4, time: f32) -> vec4 {
    var c = tint
    for _i in 0..32 {
        c = c * 0.5 + vec4(sin(time + c.x))
    }
    c
}

@fragment
fn grey(_tint: vec4, _time: f32) -> vec4 {
    vec4(0.5, 0.5, 0.5, 1.0)
}

pub struct State {
    out: GpuBuffer<f32>,
}

pub fn init() -> State {
    State { out: buffer(64) }
}

pub fn frame(s: mut State, time: f32, width: u32, height: u32) {
    dispatch(slow.bind(mut s.out, scale: time), over: 64)
    let pass = begin_screen_pass(clear: vec4(0.0, 0.0, 0.0, 1.0))
    draw(pass, tri.bind(), shade.bind(vec4(1.0), time), vertices: 3)
}
"#;

#[test]
fn a_standin_is_a_pipeline_of_the_same_bindings_the_manifest_names() {
    let dir = scratch("standin_build");
    write_package(&dir, "standin_build", PROGRAM);
    let out = wrela_driver::build_for_tests(&dir, false);
    assert!(
        !out.has_errors(),
        "{}",
        wrela_diag::render::render_all(&out.sources, &out.diagnostics)
    );
    let built = scratch("standin_build_out");
    out.write_to(&built).expect("write the build");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(built.join("manifest.json")).expect("manifest"),
    )
    .expect("json");
    let ps = manifest["pipelines"].as_array().expect("pipelines");
    assert_eq!(ps.len(), 4, "two pipelines and their stand-ins: {manifest}");
    let mut seen = 0;
    for p in ps {
        let Some(j) = p["standin"].as_u64() else { continue };
        seen += 1;
        let s = &ps[j as usize];
        assert!(s["standin"].is_null(), "{manifest}");
        assert_eq!(p["bindings"], s["bindings"], "{manifest}");
        assert_eq!(p["uniform"], s["uniform"], "{manifest}");
        let name = p["name"].as_str().unwrap_or_default();
        let stand = s["name"].as_str().unwrap_or_default();
        assert!(
            (name.contains("slow") && stand.contains("quick"))
                || (name.contains("shade") && stand.contains("grey")),
            "{name} -> {stand}"
        );
    }
    assert_eq!(seen, 2, "{manifest}");
    // A debug build's flag is each's own, and the manifest still holds.
    let debug = wrela_driver::build_for_tests(&dir, true);
    assert!(
        !debug.has_errors(),
        "{}",
        wrela_diag::render::render_all(&debug.sources, &debug.diagnostics)
    );

    // The pipeline query shows each, and the stand-ins do far less work.
    let q = wrela_driver::check_pipelines(&dir);
    assert!(!q.has_errors(), "{:?}", q.diagnostics);
    for p in &q.pipelines {
        if let Some(j) = p.standin {
            let s = &q.pipelines[j];
            let (a, b) = (p.work.last().copied().flatten(), s.work.last().copied().flatten());
            assert!(s.stands_in && b.unwrap() * 4 < a.unwrap(), "{:?} {:?}", p.names, s.names);
        }
    }
}

fn e0711(name: &str, src: &str) -> Vec<String> {
    let dir = scratch(name);
    write_package(&dir, name, src);
    let out = wrela_driver::check_pipelines(&dir);
    out.diagnostics
        .iter()
        .filter(|d| d.code == wrela_diag::codes::E0711)
        .map(|d| d.message.clone())
        .collect()
}

#[test]
fn a_standin_that_cant_stand_in_is_e0711() {
    let cases = [
        (
            "@standin(quick)",
            "fn quick(out: mut Slots<f32>, id: GlobalId, _scale: u32)",
            "doesn't take and return",
        ),
        (
            "@standin(quick)",
            "fn quick(out: mut Slots<f32>, id: GlobalId)",
            "parameters aren't this one's",
        ),
        (
            "@standin(nowhere)",
            "fn quick(out: mut Slots<f32>, id: GlobalId, _scale: f32)",
            "no function `nowhere`",
        ),
        (
            "@standin(slow)",
            "fn quick(out: mut Slots<f32>, id: GlobalId, _scale: f32)",
            "names itself",
        ),
    ];
    for (k, (attr, sig, want)) in cases.iter().enumerate() {
        let src = PROGRAM
            .replace("@standin(quick)", attr)
            .replace("fn quick(out: mut Slots<f32>, id: GlobalId, _scale: f32)", sig);
        let msgs = e0711(&format!("standin_bad_{k}"), &src);
        assert!(msgs.iter().any(|m| m.contains(want)), "{attr} {sig}: {msgs:?}");
    }
    // A stand-in of another stage, and one on a vertex shader.
    let src = PROGRAM.replace("@standin(grey)", "@standin(quick)");
    let msgs = e0711("standin_bad_stage", &src);
    assert!(msgs.iter().any(|m| m.contains("isn't a `@fragment` shader")), "{msgs:?}");
    let src = PROGRAM.replace("@vertex\nfn tri", "@vertex\n@standin(tri)\nfn tri");
    let msgs = e0711("standin_bad_vertex", &src);
    assert!(
        msgs.iter().any(|m| m.contains("only a `@compute` kernel or a `@fragment`")),
        "{msgs:?}"
    );
}

/// A sampler the entry point gathers a depth texture through binds as one that doesn't filter
/// (its own code says): its stand-in, which doesn't, binds it so too, and takes its tier.
#[test]
fn a_standin_binds_its_samplers_as_its_entry_point_does() {
    let src = std::fs::read_to_string(
        wrela_tests::repo_root().join("compiler/tests/gather/main.wrela"),
    )
    .expect("the gather test")
    .replacen("@compute(4)\nfn probe(", "@compute(4)\n@standin(cheap)\nfn probe(", 1)
        + "\n@compute(4)\nfn cheap(_t: Texture<Rgba16Float>, _d: DepthTexture, _s: Sampler, _cs: ComparisonSampler, out: mut Slots<vec4>, id: GlobalId) {\n    out[id] = vec4(0.0)\n}\n";
    assert!(src.contains("@standin(cheap)"));
    let dir = scratch("standin_gather");
    write_package(&dir, "standin_gather", &src);
    let out = wrela_driver::build_for_tests(&dir, false);
    assert!(
        !out.has_errors(),
        "{}",
        wrela_diag::render::render_all(&out.sources, &out.diagnostics)
    );
    let built = scratch("standin_gather_out");
    out.write_to(&built).expect("write the build");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(built.join("manifest.json")).expect("manifest"),
    )
    .expect("json");
    let ps = manifest["pipelines"].as_array().expect("pipelines");
    let real = ps.iter().find(|p| p["standin"].is_u64()).expect("a stand-in");
    let cheap = &ps[real["standin"].as_u64().unwrap() as usize];
    assert_eq!(real["bindings"], cheap["bindings"], "{manifest}");
    assert!(real["bindings"].to_string().contains("non_filtering_sampler"), "{manifest}");
    // `probe` is dispatched under a condition: tier 1, and so is its stand-in.
    assert_eq!(
        (real["conditional"].as_bool(), cheap["conditional"].as_bool()),
        (Some(true), Some(true))
    );
}

/// Stage J.3: compiler/tests/standin_tape's `shade` marches a creature's compiled field, and its
/// stand-in `shade_taped` the creature's tape, read in place (`std::stage::interpret_at`). The
/// manifest names the stand-in, and on the native host, made to run each draw on its stand-in,
/// the frame is the real pipeline's within 2/255 (a mean over every channel).
#[test]
#[ignore = "needs a GPU"]
fn a_tape_stand_in_draws_the_field() {
    let dir = crate::built("standin_tape");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("manifest.json")).expect("manifest"),
    )
    .expect("json");
    let ps = manifest["pipelines"].as_array().expect("pipelines");
    let real = ps.iter().position(|p| p["name"] == "whole+shade").expect("shade's pipeline");
    let stand = ps.iter().position(|p| p["name"] == "whole+shade_taped").expect("the stand-in");
    assert_eq!(ps[real]["standin"], stand, "{manifest}");
    let frame = |standins: bool| {
        let mut host = wrela_host::Host::load(&dir).expect("load");
        host.use_standins(standins);
        host.run_frames(&[0.0], 128, 128).expect("run").frame
    };
    let (real, taped) = (frame(false), frame(true));
    let diff = wrela_host::image::compare(&real, &taped).expect("same size");
    eprintln!(
        "the tape stand-in against the field: mean {:.4}/255, max {}/255",
        diff.mean, diff.max
    );
    assert!(diff.mean <= 2.0, "mean difference {:.4} over 2/255", diff.mean);
    // Not a blank screen: the creature is drawn.
    let lit = real.chunks(4).filter(|p| p[0] > 16).count();
    assert!(lit > 128 * 128 / 20, "only {lit} pixels lit");
}

/// Stage J.3 in Chrome, with stand-ins: frame 0 ran on the tape stand-in (`starts.json`'s
/// `stood_in`), and a later frame on the real pipeline.
#[test]
#[ignore = "needs Chrome and a GPU"]
fn in_chrome_the_tape_stands_in_until_the_field_is_built() {
    let (_, rel) = wrela_tests::page("compiler/tests/standin_tape", "standin_tape");
    let run = wrela_tests::ChromeRun {
        standins: true,
        ..wrela_tests::ChromeRun::new(120, 128, 128, 60.0)
    };
    wrela_tests::run_in_chrome_with(&rel, run);
    let results = wrela_tests::repo_root().join(&rel).join("results");
    let starts = wrela_tests::result_json(&results, "starts.json");
    let frames: Vec<u64> = starts["stood_in"]
        .as_array()
        .expect("stood_in")
        .iter()
        .map(|s| s["frame"].as_u64().expect("a frame"))
        .collect();
    eprintln!("frames on a stand-in: {frames:?}");
    assert!(frames.contains(&0), "frame 0 didn't run on the stand-in: {starts}");
    assert!(frames.iter().all(|&f| f < 119), "the last frame ran on the stand-in: {starts}");
}

/// The heavy program of stage J.4: eight creatures, each its own instantiation of `shade` (so
/// eight slow pipelines), each in its eighth of the screen; `standin` is the stand-in's
/// attribute and body, or nothing.
fn heavy(standin: &str) -> String {
    let (attr, stand) = match standin {
        "flat" => (
            "@standin(shade_flat)\n",
            "@fragment\nfn shade_flat<F: Surface>(_q: Quad, _field: F, _tapes: [Tape], _which: u32) -> vec4 {\n    vec4(0.5, 0.5, 0.5, 1.0)\n}\n",
        ),
        "tape" => (
            "@standin(shade_taped)\n",
            "@fragment\nfn shade_taped<F: Surface>(q: Quad, _field: F, tapes: [Tape], which: u32) -> vec4 {\n    grey(march_taped(tapes, which, origin(q.uv)))\n}\n",
        ),
        _ => ("", ""),
    };
    let mut kinds = String::from("fn c0() -> Surface {\n    creature()\n}\n\n");
    for i in 1..8 {
        kinds += &format!("fn c{i}() -> Surface {{\n    c{}().translate(vec3(0.0))\n}}\n\n", i - 1);
    }
    let tapes: Vec<String> = (0..8).map(|i| format!("c{i}().to_tape()")).collect();
    let draws: Vec<String> = (0..8)
        .map(|i| {
            format!(
                "    draw(screen, strip.bind({i}), shade.bind(c{i}(), s.tapes, {i}), vertices: 6)"
            )
        })
        .collect();
    format!(
        "use std::field::{{Surface, ellipsoid, fbm, round_cone, sphere}}
use std::gpu::{{ClipPosition, GpuBuffer, VertexIndex, begin_screen_pass, buffer_of, draw}}
use std::stage::{{Tape, interpret_at}}

fn creature() -> Surface {{
    let body = ellipsoid(vec3(0.62, 0.435, 0.42))
    let neck = round_cone(vec3(0.35, 0.15, 0.0), vec3(0.75, 0.62, 0.0), r_top: 0.16, r_bottom: 0.1)
    let head = sphere(radius: 0.2).translate(vec3(0.82, 0.72, 0.0))
    body
        .smooth_union(neck, k: 0.12)
        .smooth_union(head, k: 0.08)
        .displace(fbm(freq: 6.0, octaves: 3, offset: vec3(3.0, 1.0, 7.0)), amp: 0.015)
}}

{kinds}pub struct State {{
    tapes: GpuBuffer<Tape>,
}}

pub fn init() -> State {{
    State {{ tapes: buffer_of([{tapes}]) }}
}}

struct Quad: Copy {{
    pos: ClipPosition,
    uv: vec2,
}}

/// Strip `i` of eight, across the screen: two triangles, `uv` across the strip and down it.
@vertex
fn strip(v: VertexIndex, i: u32) -> Quad {{
    let corners = [vec2(0.0, -1.0), vec2(1.0, -1.0), vec2(0.0, 1.0), vec2(0.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0)]
    let c = corners[v.index]
    let x = -1.0 + 0.25 * (f32(i) + c.x)
    Quad {{ pos: ClipPosition {{ position: vec4(x, c.y, 0.5, 1.0) }}, uv: vec2(2.0 * c.x - 1.0, c.y) }}
}}

fn origin(uv: vec2) -> vec3 {{
    vec3(0.2 + 1.2 * uv.x, 0.25 - 1.2 * uv.y, -2.0)
}}

fn march<F: Surface>(field: F, o: vec3) -> f32 {{
    var t = 0.0
    for _ in 0..96 {{
        let s = field.distance(o + vec3(0.0, 0.0, t))
        if s < 0.001 || t > 4.0 {{
            break
        }}
        t += s
    }}
    min(t, 4.0)
}}

fn march_taped(tapes: [Tape], which: u32, o: vec3) -> f32 {{
    var t = 0.0
    for _ in 0..96 {{
        let s = interpret_at(tapes, which, o + vec3(0.0, 0.0, t))
        if s < 0.001 || t > 4.0 {{
            break
        }}
        t += s
    }}
    min(t, 4.0)
}}

fn grey(t: f32) -> vec4 {{
    let v = 1.0 - t / 4.0
    vec4(v, v, v, 1.0)
}}

@fragment
{attr}fn shade<F: Surface>(q: Quad, field: F, _tapes: [Tape], _which: u32) -> vec4 {{
    grey(march(field, origin(q.uv)))
}}

{stand}
pub fn frame(s: mut State, time: f32, width: u32, height: u32) {{
    let screen = begin_screen_pass(clear: vec4(0.0))
{draws}
    screen.present()
}}
",
        draws = draws.join("\n"),
        tapes = tapes.join(", ")
    )
}

/// Stage J.4 (G-cold): in Chrome, cold (each run's shaders salted, so no cache serves them),
/// three runs each of `heavy` with no stand-in, a flat colour's and the tape's: when the first
/// frame was submitted, and the first frame on the real pipelines, after the page opened; the
/// stand-ins' own builds; and the tape stand-in's frame 0 against the real frame. A measurement:
/// it prints its numbers and asserts only that the runs ran.
#[test]
#[ignore = "needs Chrome and a GPU; a measurement"]
fn g_cold_tape_stand_ins() {
    let root = wrela_tests::repo_root();
    let mut real_frame = Vec::new();
    for (v, kind) in ["none", "flat", "tape"].iter().enumerate() {
        let src = root.join(format!("target/tmp/j4src-{kind}"));
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("wrela.toml"), format!("[package]\nname = \"j4_{kind}\"\n"))
            .unwrap();
        std::fs::write(src.join("main.wrela"), heavy(kind)).unwrap();
        for run in 0..3u32 {
            let (_, rel) =
                wrela_tests::page(&format!("target/tmp/j4src-{kind}"), &format!("j4-{kind}-{run}"));
            let chrome = wrela_tests::ChromeRun {
                standins: *kind != "none",
                salt: 1 + 10 * v as u32 + run,
                paced: true,
                ..wrela_tests::ChromeRun::new(600, 256, 128, 60.0)
            };
            let ran = wrela_tests::run_in_chrome_with(&rel, chrome);
            let results = root.join(&rel).join("results");
            let load = wrela_tests::result_json(&results, "load.json");
            let starts = wrela_tests::result_json(&results, "starts.json");
            let opened = load["opened_ms"].as_f64().unwrap();
            let submitted = load["first_submitted_ms"].as_f64().unwrap() - opened;
            let stood: Vec<u64> = starts["stood_in"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["frame"].as_u64().unwrap())
                .collect();
            let first_real = stood.iter().max().map_or(0, |m| m + 1) as usize;
            let real_ms = ran.began_ms.get(first_real).map(|b| b - opened);
            let built: Vec<(String, f64)> = starts["built"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| {
                    (
                        b["name"].as_str().unwrap().to_string(),
                        b["ms"].as_f64().map_or(-1.0, |m| m - opened),
                    )
                })
                .collect();
            let stand_builds: Vec<f64> =
                built.iter().filter(|(n, _)| n.contains("shade_")).map(|b| b.1.round()).collect();
            eprintln!(
                "J4 {kind} run {run}: first submitted {submitted:.0} ms; first real frame {first_real} at {} ms; stand-ins built at {stand_builds:?} ms",
                real_ms.map_or("never".into(), |m| format!("{m:.0}"))
            );
            if *kind == "none" && run == 0 {
                real_frame = ran.frame.clone();
            }
        }
    }
    // The tape stand-in's frame 0 (one frame, stand-ins on, cold) against the real frame.
    let (_, rel) = wrela_tests::page("target/tmp/j4src-tape", "j4-tape-frame0");
    let chrome = wrela_tests::ChromeRun {
        standins: true,
        salt: 99,
        ..wrela_tests::ChromeRun::new(1, 256, 128, 60.0)
    };
    let ran = wrela_tests::run_in_chrome_with(&rel, chrome);
    let starts = wrela_tests::result_json(&root.join(&rel).join("results"), "starts.json");
    let diff = wrela_host::image::compare(&real_frame, &ran.frame).expect("same size");
    eprintln!(
        "J4 tape frame 0 (stood in: {}) against the real frame: mean {:.4}/255, max {}/255",
        starts["stood_in"], diff.mean, diff.max
    );
}
