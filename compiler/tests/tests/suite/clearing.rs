//! The clearing's checks (#51, the clearing specified in #28): what examples/clearing draws and
//! how, measured on the clearing itself. Its exports (main.wrela's `test_*`, testing.wrela) set
//! how a frame is drawn and shown, and copy what a frame drew into buffers, which these read
//! back. The frames run in lockstep with the clearing's ticker, so two runs draw the same.
//!
//! The source, WGSL and command-stream checks need no GPU; the rest do:
//! `cargo test --release -p wrela-tests --test suite clearing:: -- --ignored --nocapture`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use wrela_abi::manifest::{Compare, DepthState, Stage};
use wrela_host::Value;
use wrela_tests::camera::len;
use wrela_tests::{page, repo_root};

/// The output's size, and the scene's (half each way).
const W: u32 = 320;
const H: u32 = 180;

/// The clearing built once per process, as a page (each test's a copy).
fn built(name: &str) -> (PathBuf, String) {
    page("examples/clearing", name)
}

fn floats(v: &[Value]) -> Vec<f64> {
    v.iter()
        .map(|x| match x {
            Value::F32(f) => f64::from(*f),
            Value::I32(i) => f64::from(*i),
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

// ---- the source and its WGSL (no GPU) ---------------------------------------------------------

/// The clearing's pipelines: each one's name, entry points, depth state (a render pipeline's)
/// and WGSL.
struct Pipeline {
    name: String,
    vertex: String,
    fragment: String,
    depth: Option<DepthState>,
    wgsl: String,
}

/// A render pipeline's entry points and depth state; a kernel's entry and none.
fn entries(p: &wrela_abi::manifest::Pipeline) -> (String, String, Option<DepthState>) {
    match &p.stage {
        Stage::Render { vertex_entry, fragment_entry, depth, .. } => {
            (vertex_entry.clone(), fragment_entry.clone(), Some(*depth))
        }
        Stage::Compute { entry, .. } => (entry.clone(), String::new(), None),
    }
}

/// The clearing's pipelines, read once from the build its pages copy (which only they write).
fn pipelines() -> &'static [Pipeline] {
    static PIPELINES: std::sync::OnceLock<Vec<Pipeline>> = std::sync::OnceLock::new();
    PIPELINES.get_or_init(|| {
        let dir = wrela_tests::page_build("examples/clearing");
        let m = wrela_tests::manifest(&dir);
        m.pipelines
            .iter()
            .map(|p| {
                let (vertex, fragment, depth) = entries(p);
                Pipeline {
                    name: p.name.clone(),
                    vertex,
                    fragment,
                    depth,
                    wgsl: std::fs::read_to_string(dir.join(&p.shader)).expect("read the WGSL"),
                }
            })
            .collect()
    })
}

/// The text of WGSL function `name` (its signature to its closing brace).
fn wgsl_function<'a>(wgsl: &'a str, name: &str) -> &'a str {
    let start =
        wgsl.find(&format!("\nfn {name}(")).unwrap_or_else(|| panic!("no `fn {name}` in the WGSL"));
    let end = wgsl[start + 1..].find("\n}\n").map_or(wgsl.len(), |e| start + 1 + e + 3);
    &wgsl[start..end]
}

/// The hashes the fields' noise is built from (std::field's `hash_u32`, engine::noise's lattice
/// hashes), as WGSL writes them: a field evaluated in a shader brings them in.
const FIELD_HASHES: [&str; 6] =
    ["747796405u", "2891336453u", "277803737u", "1597334677u", "3812015801u", "2798796415u"];

/// Answers `queries` (the compiler's, as `wrela query` takes them) on the clearing, in one run:
/// one check of the package.
fn query(queries: &[String]) -> Vec<serde_json::Value> {
    let queries: Vec<_> = queries
        .iter()
        .map(|text| (text.clone(), wrela_driver::query::parse(text).expect("a query")))
        .collect();
    let root = repo_root().join("examples/clearing");
    wrela_driver::query::run(&root, &queries).expect("run the queries")
}

/// The functions whose callees the tests check through any depth: the terrain's vertex shader,
/// and the entry points of the pipelines that draw each frame.
fn walked() -> Vec<String> {
    let drawn = DRAWN.iter().flat_map(|(v, f)| [ps_path(v), ps_path(f)]);
    ["engine::terrain::ground".to_string()].into_iter().chain(drawn).collect()
}

/// What the tests ask of the clearing's call graph, worked out once for the process.
struct Calls {
    /// Every function each of `walked()` calls, through any depth.
    closures: BTreeMap<String, BTreeSet<String>>,
    /// What the grass's placing calls (not through any depth).
    place: Vec<String>,
}

/// The compiler's `callees` query gives what a function calls directly; the graph is walked
/// from all of `walked()` together, a level at a time, each level's queries in one run (where a
/// query a function took a check of the package each). The grass's placing is asked with the
/// first level.
fn calls() -> &'static Calls {
    static CALLS: std::sync::OnceLock<Calls> = std::sync::OnceLock::new();
    CALLS.get_or_init(|| {
        let place = "engine::grass::place".to_string();
        let roots = walked();
        let mut direct: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut level: BTreeSet<String> = roots.iter().cloned().collect();
        let mut seen = BTreeSet::new();
        let mut also = Some(place.clone());
        while !level.is_empty() {
            let asked: Vec<String> = level
                .iter()
                .filter(|f| !direct.contains_key(*f))
                .cloned()
                .chain(also.take())
                .collect();
            if !asked.is_empty() {
                let texts: Vec<String> = asked.iter().map(|f| format!("callees {f}")).collect();
                for (f, answer) in asked.iter().zip(query(&texts)) {
                    let list = answer["callees"].as_array().map_or(Vec::new(), |list| {
                        list.iter()
                            .map(|c| c["callee"].as_str().expect("a callee").to_string())
                            .collect()
                    });
                    direct.insert(f.clone(), list);
                }
            }
            seen.extend(level.iter().cloned());
            level = level
                .iter()
                .flat_map(|f| &direct[f])
                .filter(|c| !seen.contains(*c))
                .cloned()
                .collect();
        }
        let closures = roots
            .into_iter()
            .map(|f| {
                let mut reached = BTreeSet::new();
                let mut todo = vec![f.clone()];
                while let Some(next) = todo.pop() {
                    for c in &direct[&next] {
                        if reached.insert(c.clone()) {
                            todo.push(c.clone());
                        }
                    }
                }
                (f, reached)
            })
            .collect();
        Calls { closures, place: direct[&place].clone() }
    })
}

/// Every function `f` (one of `walked()`) calls, through any depth.
fn callees(f: &str) -> &'static BTreeSet<String> {
    &calls().closures[f]
}

/// AC3: the terrain's vertex shader reads the clipmap and never evaluates the terrain field:
/// nothing it calls is the field's (`Terrain::height`, the scene's height, its noise), and its
/// WGSL holds none of the noise's hashes.
#[test]
fn the_terrain_vertex_shader_reads_the_clipmap_not_the_field() {
    let calls = callees("engine::terrain::ground");
    assert!(calls.contains("engine::terrain::level_height"), "it reads the clipmap: {calls:?}");
    for c in calls {
        assert!(
            !c.contains("height(") && !c.starts_with("scene::") && !c.starts_with("engine::noise"),
            "the terrain's vertex shader calls {c}"
        );
        assert!(!c.contains("Terrain::") && !c.contains("Ground::"), "it calls {c}");
    }
    let ps = pipelines();
    let p = ps.iter().find(|p| p.vertex == "ground").expect("the terrain's pipeline");
    let body = wgsl_function(&p.wgsl, "ground");
    for h in FIELD_HASHES {
        assert!(!body.contains(h), "{}: the vertex shader holds the noise's {h}", p.name);
    }
    // The cooking kernel does evaluate it, so the check would see it.
    let cook = ps.iter().find(|p| p.name == "cook_level").expect("the clipmap's cooking");
    assert!(FIELD_HASHES.iter().any(|h| cook.wgsl.contains(h)), "the cook holds no hash");
}

/// AC4 (stones), AC5 (the creature and the plants): drawn from meshes and cards whose normals
/// and colours were cooked, so no pipeline that draws them each frame evaluates a field: none
/// calls a field's distance or its colour (the stones' facets and stain, the crowns' clumps and
/// limbs, the creature's parts), and the stones' and the creature's WGSL hold none of the
/// noise's hashes (the plants' shaders hash a card's seed for its tint, so theirs is checked by
/// what they call).
#[test]
fn no_field_is_evaluated_per_vertex_or_per_pixel() {
    let ps = pipelines();
    for (v, f) in DRAWN {
        let p = ps
            .iter()
            .find(|p| p.vertex == v && p.fragment == f)
            .unwrap_or_else(|| panic!("no pipeline {v}+{f}"));
        if v == "stone_vertex" || v == "character" {
            for h in FIELD_HASHES {
                assert!(!p.wgsl.contains(h), "{}: its WGSL holds the noise's {h}", p.name);
            }
        }
        for entry in [v, f] {
            for c in callees(&ps_path(entry)) {
                let field = c.starts_with("std::field::") && !c.ends_with("hash_u32")
                    || c.contains("::crown")
                    || c.contains("::wood")
                    || c.contains("::part_")
                    || c.ends_with("::distance")
                    || c.contains("Stain::at")
                    || c.contains("Ground::")
                    || c.contains("Terrain::");
                assert!(!field, "{entry} ({}) calls {c}", p.name);
            }
        }
    }
}

/// The entry points (vertex, fragment) of the pipelines that draw the stones, the creature and
/// the plants each frame.
const DRAWN: [(&str, &str); 8] = [
    ("stone_vertex", "stone_shade"),
    ("character", "character_shade"),
    ("card", "leaf_shade"),
    ("limb", "bark_shade"),
    ("far_quad", "far_shade"),
    ("far_quad", "volume_shade"),
    ("blade", "blade_shade"),
    ("flower", "flower_shade"),
];

/// An entry point's path in the engine.
fn ps_path(entry: &str) -> String {
    let module = match entry {
        "stone_vertex" | "stone_shade" => "stone",
        "character" | "character_shade" => "character",
        "card" | "leaf_shade" | "limb" | "bark_shade" => "vegetation",
        "far_quad" | "far_shade" | "volume_shade" => "impostor",
        "blade" | "blade_shade" | "flower" | "flower_shade" => "grass",
        "sky_shade" => "sky",
        other => panic!("where is {other}?"),
    };
    format!("engine::{module}::{entry}")
}

/// AC5: a crown's three levels light their leaves alike, so they match where they meet (as
/// `a_crown_matches_where_its_levels_meet` measures): the cards', the impostors' and the
/// volumes' shading each light them by one function, `impostor::leaf_light`, with its gloss
/// as rough as a leaf's size on the screen makes it (`impostor::leaf_rough`). No level gains or
/// loses the gloss alone.
#[test]
fn a_crowns_levels_light_their_leaves_by_one_light_with_gloss() {
    for f in ["leaf_shade", "far_shade", "volume_shade"] {
        let calls = callees(&ps_path(f));
        for want in
            ["engine::impostor::leaf_light", "engine::impostor::leaf_rough", "engine::light::gloss"]
        {
            assert!(calls.contains(want), "{f} doesn't call {want}");
        }
    }
}

/// AC6: the sky's pass reads the clouds' dome and marches nothing (its WGSL, with the functions
/// it calls, has no loop), where the dome's kernel marches.
#[test]
fn the_sky_pass_reads_the_cooked_clouds_and_marches_nothing() {
    let ps = pipelines();
    let sky = ps.iter().find(|p| p.fragment == "sky_shade").expect("the sky's pipeline");
    assert!(!sky.wgsl.contains("loop"), "the sky's pass loops:\n{}", sky.wgsl);
    assert!(sky.wgsl.contains("textureSampleLevel"), "the sky's pass reads no texture");
    // A kernel's entry is its pipeline's first (`entries`).
    let march = ps.iter().find(|p| p.vertex == "march_dome").expect("the dome's march");
    assert!(march.wgsl.contains("loop"), "the dome's kernel doesn't march");
}

/// AC4: flowers are cut cards in the depth prepass (cut by `discard`, writing depth), shaded
/// where it left the depth (equal, no writes), and placed with the grass (the grass's placing
/// kernel appends them).
#[test]
fn flowers_are_cut_cards_in_the_prepass_placed_with_the_grass() {
    let ps = pipelines();
    let find = |f: &str| {
        ps.iter().find(|p| p.fragment == f).unwrap_or_else(|| panic!("no pipeline shades with {f}"))
    };
    let cut = find("flower_cut");
    let shade = find("flower_shade");
    assert!(cut.wgsl.contains("discard"), "the flowers' cut doesn't discard");
    assert!(!shade.wgsl.contains("discard"), "the flowers' shading discards");
    let (cs, ss) = (cut.depth, shade.depth);
    assert!(cs.is_some_and(|d| d.write), "the cut writes no depth: {cs:?}");
    assert!(
        ss.is_some_and(|d| !d.write && d.compare == Compare::Equal),
        "the shading isn't at equal depth without writes: {ss:?}"
    );
    let pushes = std::fs::read_to_string(repo_root().join("engine/grass.wrela")).expect("read");
    assert!(pushes.contains("flowers.push("), "the grass's placing appends no flowers");
    assert!(!calls().place.is_empty(), "the grass's placing calls nothing");
}

/// AC4: the wind's sway is a function of time and place only: `sway` is pure (its effects are
/// none) and takes the wind (its direction, strength and time) and a place; the same time and
/// place sway the same, and a different time or place differently.
#[test]
fn the_wind_is_a_function_of_time_and_place() {
    let out = query(&["effects engine::plant::sway".into(), "callers engine::plant::sway".into()]);
    let effects = out[0]["effects"].as_array().map(Vec::len).unwrap_or(usize::MAX);
    assert_eq!(effects, 0, "sway has effects: {}", out[0]);
    let callers: Vec<String> = out[1]["callers"]
        .as_array()
        .expect("callers")
        .iter()
        .map(|c| c["caller"].as_str().unwrap_or_default().to_string())
        .collect();
    for want in ["blade_point", "flower_head", "card_corner"] {
        assert!(callers.iter().any(|c| c.ends_with(want)), "{want} doesn't sway: {callers:?}");
    }
    let heads = query(&["callers engine::grass::flower_head".into()]);
    assert!(
        heads[0]["callers"]
            .as_array()
            .expect("flower-head callers")
            .iter()
            .any(|c| { c["caller"].as_str().is_some_and(|name| name == "engine::grass::flower") }),
        "the flower vertex no longer uses the shared moving stem tip: {}",
        heads[0]
    );
    let (dir, _) = built("clearing-wind");
    let mut host = wrela_host::CpuHost::load(&dir).expect("load");
    let mut sway = |t: f32, p: [f32; 3], h: f32| {
        let args =
            [Value::F32(t), Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2]), Value::F32(h)];
        floats(&host.call_export("test_sway", &args).expect("sway"))
    };
    let a = sway(3.5, [2.0, 1.0, 20.0], 1.5);
    assert_eq!(a, sway(3.5, [2.0, 1.0, 20.0], 1.5), "the same time and place sway alike");
    assert_ne!(a, sway(3.6, [2.0, 1.0, 20.0], 1.5), "the sway doesn't move with time");
    assert_ne!(a, sway(3.5, [5.0, 1.0, 20.0], 1.5), "the sway doesn't move with place");
    assert!(len([a[0], a[1], a[2]]) > 1e-4, "no sway: {a:?}");
}

/// The broad culling cone must contain the camera's corners, including a square viewport.
/// This is CPU visibility state: the empty prototype needs no mesh realization or GPU work.
#[test]
fn rock_culling_keeps_each_view_edge_and_corner() {
    let source = r#"
use engine::realize::{Mesh, Room}
use engine::stone::{Placement, Realized, Rocks, normal_matrix}
use engine::view::first_camera
use std::gpu::buffer
use std::transform::{identity, translate}

fn prototype() -> Realized {
    Realized {
        mesh: Mesh::new(Room { vertices: 1, quads: 1 }),
        colours: buffer(1),
        at: identity(),
        seed: 1,
    }
}

pub fn frame(time: f32, width: u32, height: u32) {
    var near: Vec<Realized> = Vec::new()
    near.push(prototype())
    var far: Vec<Realized> = Vec::new()
    far.push(prototype())
    let origin = vec3(100.0, 20.0, -50.0)
    let placements = [Placement {
        prototype: 0,
        at: translate(origin),
        normal_at: normal_matrix(identity()),
        reach: 0.25,
        seed: 1,
    }]
    var rocks = Rocks::new(take near, take far, placements)
    for width in [640, 360, 960] {
        let cam = first_camera(vec3(0.0), vec3(0.0, 0.0, 1.0), 1.0, width, 640)
        // The centre, four edges and four corners project inside the actual view.
        for y in [-0.98, 0.0, 0.98] {
            for x in [-0.98, 0.0, 0.98] {
                let point = cam.eye + 20.0 * (cam.forward + cam.tan_half *
                    (cam.right * (x * cam.aspect) + cam.up * y))
                let clip = cam.view_proj * vec4(point, 1.0)
                assert(abs(clip.x) < clip.w && abs(clip.y) < clip.w &&
                    clip.z > 0.0 && clip.z < clip.w, "the sample is outside the view")
                rocks.spheres[0] = vec4(point + origin, 0.25)
                rocks.cull(cam, origin)
                assert(rocks.seen_count[0] == 0, "a distant sphere used its near mesh")
                assert(rocks.seen_count[1] == 1, "a visible edge or corner rock was culled")
            }
        }
        rocks.spheres[0] = vec4(origin - cam.forward * 20.0, 0.25)
        rocks.cull(cam, origin)
        assert(rocks.seen_count[0] + rocks.seen_count[1] == 0,
            "a rock behind the camera was retained")
        rocks.spheres[0] = vec4(origin + cam.forward * 20.0 + cam.right * 100.0, 0.25)
        rocks.cull(cam, origin)
        assert(rocks.seen_count[0] + rocks.seen_count[1] == 0,
            "a rock well outside the view was retained")
    }
}

@test(frames: 1)
fn each_visible_rock_is_kept() {}
"#;
    let package = super::engine_package("clearing/rock-culling", "rock_culling", source);
    assert_eq!(super::tests_pass(&package), 1);
}

/// Replacing a wrapped field block invalidates the level over the evicted tile as well as
/// the incoming tile, even when the new tile lies outside every visible level. A tile at
/// a level's final row or column also invalidates its sampled endpoint.
#[test]
fn replacing_a_field_block_recooks_the_evicted_tile() {
    let source = r#"use engine::terrain::{Clipmap, Cover, Terrain}

struct Flat: Copy + GpuData {}

impl Terrain for Flat {
    fn height(self, x: f32, z: f32) -> f32 {
        0.0
    }

    fn cover(self, at: vec3, n: vec3) -> Cover {
        Cover { albedo: vec3(0.5), grass: 0.0 }
    }
}

pub struct State {
    clip: Clipmap,
}

pub fn init() -> State {
    State { clip: Clipmap::new(1, 1.0) }
}

pub fn frame(s: mut State, time: f32, width: u32, height: u32) {
    s.clip.fields_on(tile: 256.0, side: 1, blocks: 1)
    s.clip.write_fields(vec2i(0), [255, 0, 0, 0])
    assert(s.clip.follow(Flat {}, vec3(64.0, 0.0, 64.0), again: false) == 1)
    assert(s.clip.follow(Flat {}, vec3(64.0, 0.0, 64.0), again: false) == 0)
    s.clip.write_fields(vec2i(100), [0, 0, 0, 0])
    assert(s.clip.follow(Flat {}, vec3(64.0, 0.0, 64.0), again: false) == 1,
        "replacing the wrapped field block left the old tile cooked")
    assert(s.clip.follow(Flat {}, vec3(-64.0, 0.0, 64.0), again: false) == 1)
    s.clip.write_fields(vec2i(0), [255, 0, 0, 0])
    assert(s.clip.follow(Flat {}, vec3(-64.0, 0.0, 64.0), again: false) == 1,
        "writing the final texel at the tile's low boundary left the level cooked")
}

@test(frames: 1)
fn the_old_fields_were_invalidated(s: State) {
    assert(s.clip.cooks == 4)
}
"#;
    let package = super::engine_package("clearing/field-eviction", "field_eviction", source);
    assert_eq!(super::tests_pass(&package), 1);
}
// ---- light (AC5) -----------------------------------------------------------------------------

/// How many draws a frame's batch makes after the label `name` (to the next label).
fn draws_after(batch: &[u8], name: &str) -> usize {
    use wrela_abi::stream::{Command, decode};
    let mut inside = false;
    let mut n = 0;
    for cmd in decode(batch).expect("a batch") {
        match cmd {
            Command::Label { name: l } => inside = l == name,
            Command::Draw { .. }
            | Command::DrawIndirect { .. }
            | Command::DrawIndexedIndirect { .. }
                if inside =>
            {
                n += 1
            }
            _ => {}
        }
    }
    n
}

/// The clearing's commands and traps, under the same WASM and frame schedule as [`Clearing`].
/// The CPU host validates and records every command without rasterizing the frames.
struct RecordedClearing {
    host: wrela_host::CpuHost,
    frame: u32,
}

impl RecordedClearing {
    fn load(name: &str) -> Self {
        let (dir, _) = built(name);
        let host = wrela_host::CpuBuild::load(dir)
            .expect("load the clearing")
            .recording()
            .start()
            .expect("start the clearing");
        Self { host, frame: 0 }
    }

    fn steps(&mut self, count: u32) {
        for _ in 0..count {
            self.host
                .lockstep_frame(self.frame, 60.0, W, H, &[])
                .unwrap_or_else(|e| panic!("frame {}: {e}", self.frame));
            self.frame += 1;
        }
    }

    fn call(&mut self, name: &str, args: &[Value]) -> Vec<f64> {
        floats(&self.host.call_export(name, args).unwrap_or_else(|e| panic!("{name}: {e}")))
    }

    fn until_ready(&mut self, settle: u32) {
        while self.call("test_ready", &[])[0] < 0.5 {
            assert!(self.frame < 600, "the clearing isn't ready after {} frames", self.frame);
            self.steps(1);
        }
        self.steps(settle);
    }

    fn hold(&mut self, eye: [f32; 3], at: [f32; 3]) {
        let args: Vec<Value> = eye.iter().chain(&at).map(|&x| Value::F32(x)).collect();
        self.call("test_hold", &args);
    }
}

/// AC5: static shadows are cached: once the forest and the boulders are cooked (a few at a
/// frame, at load), one frame draws them into the static cascades, a steady frame draws nothing
/// into them, and moving the sun draws them again (counted in the frames' command streams).
#[test]
fn static_shadows_are_drawn_once_and_again_when_the_sun_moves() {
    // The draws into the static cascades in each batch since the last call.
    let statics = |c: &mut RecordedClearing| -> Vec<usize> {
        c.host.take_batches().iter().map(|b| draws_after(b, "shadows static")).collect()
    };
    let mut c = RecordedClearing::load("clearing-static");
    let mut drawn = Vec::new();
    for _ in 0..30 {
        c.steps(1);
        drawn.push(statics(&mut c).iter().sum::<usize>());
    }
    let frames: Vec<usize> = (0..drawn.len()).filter(|&i| drawn[i] > 0).collect();
    assert_eq!(frames.len(), 1, "the frames that drew static shadows at load: {drawn:?}");
    let first = drawn[frames[0]];
    assert!(first > 10, "the load drew {first} times into the static cascades");
    c.until_ready(2);
    c.host.take_batches();
    c.steps(30);
    let steady = statics(&mut c);
    assert!(steady.iter().all(|&n| n == 0), "steady frames draw static shadows: {steady:?}");
    let sun = [0.85f32, 0.45, -0.25];
    c.call(
        "test_sun",
        &[Value::F32(sun[0]), Value::F32(sun[1]), Value::F32(sun[2]), Value::F32(1.0)],
    );
    c.steps(1);
    let moved: usize = statics(&mut c).iter().sum();
    assert_eq!(moved, first, "moving the sun drew {moved} times, the first frame {first}");
    c.steps(5);
    let after: usize = statics(&mut c).iter().sum();
    assert_eq!(after, 0, "frames after the sun moved still draw static shadows");
}

/// The camera held inside a crown, exactly at a plant's middle (where its levels' patterns are
/// anchored, `lod::anchor`) and a step either side, looking each way, and inside a trunk: every
/// frame submits valid commands (M6 AC4, AC15). Playing spike 17, a camera there made `engine::lod::packed`
/// convert an infinite pixel to an integer, which trapped (`lod.wrela:125`).
#[test]
fn the_camera_inside_a_crown_draws_its_frames() {
    let mut c = RecordedClearing::load("clearing-inside-a-crown");
    c.until_ready(1);
    let trees = c.call("test_trees", &[]);
    assert!(trees[0] > 0.0, "the forest is drawn");
    for i in [0, 1, 7, 40] {
        let m = c.call("test_middle", &[Value::I32(i)]);
        let middle = [m[0] as f32, m[1] as f32, m[2] as f32];
        for (dx, dy) in [(0.0, 0.0), (0.05, 0.0), (0.0, -0.05)] {
            let eye = [middle[0] + dx, middle[1] + dy, middle[2]];
            for (x, z) in [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)] {
                c.hold(eye, [eye[0] + x, eye[1], eye[2] + z]);
                c.steps(2);
            }
        }
        // Inside its trunk, a metre up, looking along the ground.
        let foot = [m[0] as f32, m[1] as f32 - m[3] as f32 * 0.5 + 1.0, m[2] as f32];
        c.hold(foot, [foot[0] + 3.0, foot[1], foot[2] + 1.0]);
        c.steps(2);
    }
}
