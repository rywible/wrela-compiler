//! The clearing's checks (#51, the clearing specified in #28): what examples/clearing draws and
//! how, measured on the clearing itself. Its exports (main.wrela's `test_*`, testing.wrela) set
//! how a frame is drawn and shown, and copy what a frame drew into buffers, which these read
//! back. The frames run in lockstep with the clearing's ticker, so two runs draw the same.
//!
//! The source and WGSL checks need no GPU; the rest do:
//! `cargo test --release -p wrela-tests --test suite clearing:: -- --ignored --nocapture`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use wrela_abi::manifest::{Compare, DepthState, Stage};
use wrela_host::{Host, Options, Scripted, Value, parse_script};
use wrela_tests::{f32s, page, repo_root, u32s};

/// The output's size, and the scene's (half each way).
const W: u32 = 1920;
const H: u32 = 1080;

/// Tag classes (engine::view): a tag is its class times 64, plus which one.
const SKY: u32 = 0;
const GROUND: u32 = 1;
const GRASS: u32 = 2;
const LEAVES: u32 = 3;
const BARK: u32 = 4;
const CREATURE: u32 = 5;

/// What frames leave out and how they're shown (main.wrela's and testing.wrela's constants).
mod off {
    pub const TREES: i32 = 1;
    pub const GRASS: i32 = 2;
    pub const SHADOWS: i32 = 4;
    pub const CREATURE: i32 = 16;
    pub const STONES: i32 = 32;
    pub const SKY: i32 = 64;
    pub const WIND: i32 = 128;
    pub const HISTORY: i32 = 256;
    pub const CLOUD_SHADOWS: i32 = 512;
}
const VIEW_LOOK: i32 = 0;
const VIEW_SCENE: i32 = 1;
const VIEW_TEMPORAL: i32 = 2;

/// The clearing built once per process, as a page (each test's a copy).
fn built(name: &str) -> (PathBuf, String) {
    page("examples/clearing", name)
}

// ---- the program, frame by frame --------------------------------------------------------------

/// The camera a frame was drawn by (main.wrela's `CameraReport`).
#[derive(Clone, Copy, Debug)]
struct Cam {
    eye: [f64; 3],
    forward: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
    tan_half: f64,
    aspect: f64,
    near: f64,
    jitter: [f64; 2],
    screen: [f64; 2],
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add_scaled(a: [f64; 3], b: [f64; 3], k: f64) -> [f64; 3] {
    [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k]
}

fn len(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

impl Cam {
    /// The world point at pixel `px` (scene pixels, its centre at +0.5) of depth `z` (reversed).
    fn world_at(&self, px: [f64; 2], z: f64) -> [f64; 3] {
        let ndc = [
            px[0] / self.screen[0] * 2.0 - 1.0 - self.jitter[0],
            1.0 - px[1] / self.screen[1] * 2.0 - self.jitter[1],
        ];
        let mut ray = add_scaled(self.forward, self.right, ndc[0] * self.tan_half * self.aspect);
        ray = add_scaled(ray, self.up, ndc[1] * self.tan_half);
        let l = len(ray);
        ray = ray.map(|x| x / l);
        let dist = self.near / z.max(1e-12);
        add_scaled(self.eye, ray, dist / dot(ray, self.forward))
    }

    /// Where `p` falls on a target `size` wide and high (pixels, jittered as this frame drew),
    /// and its depth there (reversed); none behind the eye.
    fn project(&self, p: [f64; 3], size: [f64; 2]) -> Option<([f64; 2], f64)> {
        let v = sub(p, self.eye);
        let z = dot(v, self.forward);
        if z <= self.near {
            return None;
        }
        let x = dot(v, self.right) / (z * self.tan_half * self.aspect) + self.jitter[0];
        let y = dot(v, self.up) / (z * self.tan_half) + self.jitter[1];
        Some(([(x + 1.0) * 0.5 * size[0], (1.0 - y) * 0.5 * size[1]], self.near / z))
    }
}

/// The clearing, loaded on the native host and driven a frame at a time.
struct Clearing {
    host: Host,
    frame: u32,
    script: Vec<Scripted>,
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

impl Clearing {
    fn load(name: &str) -> Clearing {
        Clearing::load_with(name, &Options { record: true, ..Options::default() })
    }

    fn load_with(name: &str, options: &Options) -> Clearing {
        let (dir, _) = built(name);
        let host = Host::load_with(&dir, options).expect("load the clearing");
        Clearing { host, frame: 0, script: Vec::new() }
    }

    /// Space pressed at the next frame: the creature walks and the camera's path starts (once
    /// the clearing's ready).
    fn walk(&mut self) {
        let s = format!(r#"[{{"frame":{},"type":"key","key":"Space"}}]"#, self.frame);
        self.script.extend(parse_script(&s).expect("a script"));
    }

    fn step(&mut self) {
        self.host
            .lockstep_frame(self.frame, 60.0, W, H, &self.script)
            .unwrap_or_else(|e| panic!("frame {}: {e}", self.frame));
        self.frame += 1;
    }

    fn steps(&mut self, n: u32) {
        for _ in 0..n {
            self.step();
        }
    }

    fn call(&mut self, name: &str, args: &[Value]) -> Vec<f64> {
        floats(&self.host.call_export(name, args).unwrap_or_else(|e| panic!("{name}: {e}")))
    }

    /// Frames until everything cooked at load is done, then `settle` more.
    fn until_ready(&mut self, settle: u32) {
        while self.call("test_ready", &[])[0] < 0.5 {
            assert!(self.frame < 600, "the clearing isn't ready after {} frames", self.frame);
            self.step();
        }
        self.steps(settle);
    }

    /// Seconds along the camera's path (negative before it starts).
    fn path_time(&mut self) -> f64 {
        self.call("test_ready", &[])[1]
    }

    fn off(&mut self, bits: i32) {
        self.call("test_off", &[Value::I32(bits)]);
    }

    fn view(&mut self, view: i32) {
        self.call("test_view", &[Value::I32(view)]);
    }

    fn hold(&mut self, eye: [f32; 3], at: [f32; 3]) {
        let args: Vec<Value> = eye.iter().chain(&at).map(|&x| Value::F32(x)).collect();
        self.call("test_hold", &args);
    }

    fn camera(&mut self) -> Cam {
        let v = self.call("test_camera", &[]);
        let v3 = |i: usize| [v[i], v[i + 1], v[i + 2]];
        Cam {
            eye: v3(0),
            forward: v3(3),
            right: v3(6),
            up: v3(9),
            tan_half: v[12],
            aspect: v[13],
            near: v[14],
            jitter: [v[15], v[16]],
            screen: [v[17], v[18]],
        }
    }

    /// What the last frame drew (`test_capture`'s `what`), from the newest buffer.
    fn capture(&mut self, what: i32) -> Vec<u8> {
        let n = self.call("test_capture", &[Value::I32(what)]);
        assert!(n[0] > 0.0, "nothing to capture");
        let newest = *self.host.buffers().last().expect("a buffer");
        self.host.read_buffer(newest).expect("read the capture")
    }

    /// The scene's colour and tag at each of its pixels.
    fn scene(&mut self) -> Vec<[f32; 4]> {
        f32s(&self.capture(0)).chunks(4).map(|c| [c[0], c[1], c[2], c[3]]).collect()
    }

    fn depth(&mut self) -> Vec<f32> {
        f32s(&self.capture(1))
    }

    fn motion(&mut self) -> Vec<[f32; 4]> {
        f32s(&self.capture(2)).chunks(4).map(|c| [c[0], c[1], c[2], c[3]]).collect()
    }

    /// Temporal AA's output (linear colour, and the nearest sample's tag), at the output's size.
    fn output(&mut self) -> Vec<[f32; 4]> {
        f32s(&self.capture(3)).chunks(4).map(|c| [c[0], c[1], c[2], c[3]]).collect()
    }

    fn screen(&mut self) -> Vec<u8> {
        self.host.read_screen().expect("read the screen")
    }

    /// The command batches since the last call (a frame's are one).
    fn batches(&mut self) -> Vec<Vec<u8>> {
        self.host.take_batches()
    }
}

/// A tag's class.
fn class(tag: f32) -> u32 {
    (tag.max(0.0) / 64.0 + 0.001) as u32
}

/// The mean of |a − b| over a set of pixels' channels (RGBA8 screens, alpha left out), in
/// 8-bit steps, and the share of those pixels that differ by more than 8 in a channel.
fn compare(a: &[u8], b: &[u8], pixels: impl Iterator<Item = usize>) -> (f64, f64, usize) {
    let (mut sum, mut over, mut n) = (0.0, 0usize, 0usize);
    for p in pixels {
        let mut most = 0u8;
        for c in 0..3 {
            let d = a[4 * p + c].abs_diff(b[4 * p + c]);
            sum += f64::from(d);
            most = most.max(d);
        }
        if most > 8 {
            over += 1;
        }
        n += 1;
    }
    let n_f = n.max(1) as f64;
    (sum / (3.0 * n_f), over as f64 / n_f, n)
}

// ---- the source and its WGSL (no GPU) ---------------------------------------------------------

/// The clearing's pipelines: each one's name, entry points and WGSL.
struct Pipeline {
    name: String,
    vertex: String,
    fragment: String,
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

fn pipelines() -> Vec<Pipeline> {
    let (dir, _) = built("clearing-wgsl");
    let m = wrela_tests::manifest(&dir);
    m.pipelines
        .iter()
        .map(|p| {
            let (vertex, fragment, _) = entries(p);
            Pipeline {
                name: p.name.clone(),
                vertex,
                fragment,
                wgsl: std::fs::read_to_string(dir.join(&p.shader)).expect("read the WGSL"),
            }
        })
        .collect()
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

/// Every function `f` calls, through any depth (the compiler's `callees` query).
fn callees(f: &str) -> BTreeSet<String> {
    let root = repo_root().join("examples/clearing");
    let mut seen = BTreeSet::new();
    let mut todo = vec![f.to_string()];
    while let Some(next) = todo.pop() {
        let text = format!("callees {next}");
        let q = wrela_driver::query::parse(&text).expect("a query");
        let out = wrela_driver::query::run(&root, &[(text, q)]).expect("run the query");
        let Some(list) = out[0]["callees"].as_array() else {
            continue;
        };
        for c in list {
            let callee = c["callee"].as_str().expect("a callee").to_string();
            if seen.insert(callee.clone()) {
                todo.push(callee);
            }
        }
    }
    seen
}

/// AC3: the terrain's vertex shader reads the clipmap and never evaluates the terrain field:
/// nothing it calls is the field's (`Terrain::height`, the scene's height, its noise), and its
/// WGSL holds none of the noise's hashes.
#[test]
fn the_terrain_vertex_shader_reads_the_clipmap_not_the_field() {
    let calls = callees("engine::terrain::ground");
    assert!(calls.contains("engine::terrain::level_height"), "it reads the clipmap: {calls:?}");
    for c in &calls {
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
    let drawn = [
        ("stone_vertex", "stone_shade"),
        ("character", "character_shade"),
        ("card", "leaf_shade"),
        ("limb", "bark_shade"),
        ("far_quad", "far_shade"),
        ("far_quad", "volume_shade"),
        ("blade", "blade_shade"),
        ("flower", "flower_shade"),
    ];
    for (v, f) in drawn {
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
            let path = ps_path(entry);
            for c in callees(&path) {
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

/// AC6: the sky's pass reads the cooked clouds and marches nothing (its WGSL has no loop),
/// where the cooking marches.
#[test]
fn the_sky_pass_reads_the_cooked_clouds_and_marches_nothing() {
    let ps = pipelines();
    let sky = ps.iter().find(|p| p.fragment == "sky_shade").expect("the sky's pipeline");
    let body = wgsl_function(&sky.wgsl, "sky_shade");
    assert!(!body.contains("loop"), "the sky's pass loops:\n{body}");
    assert!(body.contains("textureSampleLevel"), "the sky's pass reads no texture");
    let cook = ps.iter().find(|p| p.fragment == "cook_clouds").expect("the clouds' cooking");
    assert!(cook.wgsl.contains("loop"), "the cooking doesn't march");
}

/// AC4: flowers are cut cards in the depth prepass (cut by `discard`, writing depth), shaded
/// where it left the depth (equal, no writes), and placed with the grass (the grass's placing
/// kernel appends them).
#[test]
fn flowers_are_cut_cards_in_the_prepass_placed_with_the_grass() {
    let (dir, _) = built("clearing-flowers");
    let m = wrela_tests::manifest(&dir);
    let find = |f: &str| {
        m.pipelines
            .iter()
            .find(|p| entries(p).1 == f)
            .unwrap_or_else(|| panic!("no pipeline shades with {f}"))
    };
    let cut = find("flower_cut");
    let shade = find("flower_shade");
    let wgsl = |p: &wrela_abi::manifest::Pipeline| {
        std::fs::read_to_string(dir.join(&p.shader)).expect("read the WGSL")
    };
    assert!(wgsl(cut).contains("discard"), "the flowers' cut doesn't discard");
    assert!(!wgsl(shade).contains("discard"), "the flowers' shading discards");
    let (cs, ss) = (entries(cut).2, entries(shade).2);
    assert!(cs.is_some_and(|d| d.write), "the cut writes no depth: {cs:?}");
    assert!(
        ss.is_some_and(|d| !d.write && d.compare == Compare::Equal),
        "the shading isn't at equal depth without writes: {ss:?}"
    );
    let place = callees("engine::grass::place");
    let pushes = std::fs::read_to_string(repo_root().join("engine/grass.wrela")).expect("read");
    assert!(pushes.contains("flowers.push("), "the grass's placing appends no flowers");
    assert!(!place.is_empty());
}

/// AC4: the wind's sway is a function of time and place only: `sway` is pure (its effects are
/// none) and takes the wind (its direction, strength and time) and a place; the same time and
/// place sway the same, and a different time or place differently.
#[test]
fn the_wind_is_a_function_of_time_and_place() {
    let root = repo_root().join("examples/clearing");
    let text = "effects engine::plant::sway".to_string();
    let q = wrela_driver::query::parse(&text).expect("a query");
    let out = wrela_driver::query::run(&root, &[(text, q)]).expect("run the query");
    let effects = out[0]["effects"].as_array().map(Vec::len).unwrap_or(usize::MAX);
    assert_eq!(effects, 0, "sway has effects: {}", out[0]);
    let text = "callers engine::plant::sway".to_string();
    let q = wrela_driver::query::parse(&text).expect("a query");
    let out = wrela_driver::query::run(&root, &[(text, q)]).expect("run the query");
    let callers: Vec<String> = out[0]["callers"]
        .as_array()
        .expect("callers")
        .iter()
        .map(|c| c["caller"].as_str().unwrap_or_default().to_string())
        .collect();
    for want in ["blade_point", "flower", "card_corner"] {
        assert!(callers.iter().any(|c| c.ends_with(want)), "{want} doesn't sway: {callers:?}");
    }
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

// ---- the terrain (AC3) -----------------------------------------------------------------------

/// The clipmap's texels (heights), and its levels: each (x, z of texel 0, spacing).
struct Clip {
    heights: Vec<f32>,
    levels: Vec<[f64; 3]>,
}

/// engine::terrain's layout.
const SIDE: u32 = 129;
const QUADS: u32 = 128;
const ROW: u32 = 136;
const STRIDE: u32 = 17600;
const TEXEL_BYTES: usize = 20;

impl Clip {
    fn read(c: &mut Clearing) -> Clip {
        let bytes = c.capture(6);
        let heights = bytes
            .chunks(TEXEL_BYTES)
            .map(|t| f32::from_le_bytes(t[0..4].try_into().unwrap()))
            .collect();
        let count = c.call("test_level", &[Value::I32(0)])[3] as u32;
        let levels = (0..count)
            .map(|l| {
                let v = c.call("test_level", &[Value::I32(l as i32)]);
                [v[0], v[1], v[2]]
            })
            .collect();
        Clip { heights, levels }
    }

    fn texel(&self, l: usize, i: u32, j: u32) -> f64 {
        f64::from(self.heights[(l as u32 * STRIDE + j * ROW + i) as usize])
    }

    /// How far into level `l` a point is, as a share of its half side (1 at its edge).
    fn reach(&self, l: usize, x: f64, z: f64) -> f64 {
        let [ax, az, s] = self.levels[l];
        let half = f64::from(QUADS / 2) * s;
        ((x - ax - half).abs()).max((z - az - half).abs()) / half
    }

    /// The finest level holding (x, z), and its height there between its texels (the engine's
    /// `height_at`).
    fn height_at(&self, x: f64, z: f64) -> (usize, f64) {
        let mut l = 0;
        while l + 1 < self.levels.len() && self.reach(l, x, z) > 0.98 {
            l += 1;
        }
        let [ax, az, s] = self.levels[l];
        let g = [
            ((x - ax) / s).clamp(0.0, f64::from(QUADS) - 0.001),
            ((z - az) / s).clamp(0.0, f64::from(QUADS) - 0.001),
        ];
        let (i, j) = (g[0].floor(), g[1].floor());
        let (fx, fz) = (g[0] - i, g[1] - j);
        let (i, j) = (i as u32, j as u32);
        let h = |a: u32, b: u32| self.texel(l, a, b);
        let top = h(i, j) * (1.0 - fx) + h(i + 1, j) * fx;
        let bottom = h(i, j + 1) * (1.0 - fx) + h(i + 1, j + 1) * fx;
        (l, top * (1.0 - fz) + bottom * fz)
    }
}

/// The field's height and slope at (x, z), from the clearing's own terrain (on the CPU).
fn field(host: &mut wrela_host::CpuHost, x: f64, z: f64) -> (f64, f64) {
    let v = floats(
        &host
            .call_export("test_height", &[Value::F32(x as f32), Value::F32(z as f32)])
            .expect("height"),
    );
    (v[0], v[1])
}

/// AC3's accuracy: the clipmap's height is the field's within 2 cm at every texel of the
/// nearest level, and within a texel's width times the slope at every texel of the others;
/// between texels too (at each cell's middle, where the levels are read between texels),
/// within the same bounds. And one terrain: the sim's raycasts (the feet's) meet the field
/// where the drawn clipmap is, within the same accuracy.
#[test]
#[ignore = "needs a GPU"]
fn the_clipmap_holds_the_fields_heights_and_the_raycasts_meet_it() {
    let mut c = Clearing::load("clearing-clipmap");
    c.until_ready(2);
    let clip = Clip::read(&mut c);
    let (dir, _) = built("clearing-clipmap-cpu");
    let mut cpu = wrela_host::CpuHost::load(&dir).expect("load on the CPU");
    let mut worst: BTreeMap<usize, (f64, f64)> = BTreeMap::new();
    for (l, &[ax, az, s]) in clip.levels.iter().enumerate() {
        let (mut at_texels, mut between) = (0.0f64, 0.0f64);
        // Every texel of the nearest level, and of the others every 4th each way.
        let every = if l == 0 { 1 } else { 4 };
        for j in (0..SIDE).step_by(every) {
            for i in (0..SIDE).step_by(every) {
                let (x, z) = (ax + f64::from(i) * s, az + f64::from(j) * s);
                let (h, slope) = field(&mut cpu, x, z);
                let bound = if l == 0 { 0.02 } else { (s * slope).max(0.02) };
                let e = (clip.texel(l, i, j) - h).abs();
                at_texels = at_texels.max(e / bound);
                assert!(
                    e <= bound,
                    "level {l}, texel ({i}, {j}) at ({x}, {z}): {} against the field's {h}, bound {bound}",
                    clip.texel(l, i, j)
                );
                if i < QUADS && j < QUADS {
                    // The cell's middle, read between its four texels: within the cell's
                    // width times its steepest slope (at its corners and middle) of the field.
                    let (xm, zm) = (x + 0.5 * s, z + 0.5 * s);
                    let (hm, slope_m) = field(&mut cpu, xm, zm);
                    let mut steepest = slope.max(slope_m);
                    for (dx, dz) in [(s, 0.0), (0.0, s), (s, s)] {
                        steepest = steepest.max(field(&mut cpu, x + dx, z + dz).1);
                    }
                    let quad = (clip.texel(l, i, j)
                        + clip.texel(l, i + 1, j)
                        + clip.texel(l, i, j + 1)
                        + clip.texel(l, i + 1, j + 1))
                        * 0.25;
                    let bound = if l == 0 { 0.02 } else { (s * steepest).max(0.02) };
                    let e = (quad - hm).abs();
                    between = between.max(e / bound);
                }
            }
        }
        worst.insert(l, (at_texels, between));
    }
    println!("clipmap error against its bound, by level (at texels, between): {worst:?}");
    for (l, (_, between)) in &worst {
        assert!(*between <= 1.0, "level {l}: between texels {between} of its bound");
    }
    // One terrain: raycasts down and along the ground round the creature's trail meet the
    // field; the drawn clipmap's height at each hit is within the accuracy above.
    let mut worst_hit = 0.0f64;
    for k in 0..400 {
        let a = f64::from(k) * 2.399963;
        let r = 2.0 + f64::from(k % 40) * 1.5;
        let (x, z) = (-3.2 + r * a.cos(), 30.0 + r * a.sin());
        let dir =
            if k % 2 == 0 { [0.0f32, -1.0, 0.0] } else { [a.cos() as f32, -0.3, a.sin() as f32] };
        let args = [x as f32, 40.0, z as f32, dir[0], dir[1], dir[2]].map(Value::F32);
        let hit = floats(&cpu.call_export("test_raycast", &args).expect("raycast"));
        if hit[3] < 0.5 {
            continue;
        }
        let (l, drawn) = clip.height_at(hit[0], hit[2]);
        // The bound of the cell it's in: its width times its steepest slope.
        let [ax, az, s] = clip.levels[l];
        let (cx, cz) = (ax + ((hit[0] - ax) / s).floor() * s, az + ((hit[2] - az) / s).floor() * s);
        let mut steepest = field(&mut cpu, hit[0], hit[2]).1;
        for (dx, dz) in [(0.0, 0.0), (s, 0.0), (0.0, s), (s, s)] {
            steepest = steepest.max(field(&mut cpu, cx + dx, cz + dz).1);
        }
        let bound = if l == 0 { 0.02 } else { (s * steepest).max(0.02) };
        let e = (drawn - hit[1]).abs();
        worst_hit = worst_hit.max(e);
        assert!(
            e <= bound,
            "a raycast's hit at ({}, {}) is {e} m off the drawn terrain (level {l})",
            hit[0],
            hit[2]
        );
    }
    println!("raycasts against the drawn clipmap: at most {worst_hit} m apart");
}

/// AC3's cracks: over the camera's path, the terrain drawn alone against a key colour shows
/// the key through it nowhere: no pixel of the key has terrain above it in its column (the
/// sky is above the land; a crack between levels is a hole below the horizon). Every 20th
/// frame of the path's 60 s.
#[test]
#[ignore = "long: the camera's path, needs a GPU"]
fn no_pixel_sees_through_the_terrain_between_levels() {
    let mut c = Clearing::load("clearing-cracks");
    c.until_ready(0);
    c.off(off::TREES | off::GRASS | off::CREATURE | off::STONES | off::SKY);
    c.view(VIEW_SCENE);
    c.walk();
    c.step();
    let mut checked = 0;
    while c.path_time() < 60.0 {
        c.step();
        if c.frame % 20 != 0 {
            continue;
        }
        let s = c.screen();
        let key = |x: u32, y: u32| {
            let i = (4 * (y * W + x)) as usize;
            s[i] == 255 && s[i + 1] == 0 && s[i + 2] == 255
        };
        let mut holes = Vec::new();
        for x in 0..W {
            let mut land_above = false;
            for y in 0..H {
                let k = key(x, y);
                if k && land_above {
                    holes.push((x, y));
                    break;
                }
                land_above |= !k;
            }
        }
        let (frame, t) = (c.frame, c.path_time());
        if !holes.is_empty() {
            let at = repo_root().join(format!("target/tmp/clearing-crack-{frame}.png"));
            wrela_host::image::write_png(&at, W, H, &s).expect("write the frame");
            println!("the frame: {}", at.display());
        }
        assert!(
            holes.is_empty(),
            "frame {frame} (path {t:.2} s): the key shows through the terrain at {:?}",
            &holes[..holes.len().min(8)]
        );
        checked += 1;
    }
    assert!(checked >= 170, "only {checked} frames checked");
}
