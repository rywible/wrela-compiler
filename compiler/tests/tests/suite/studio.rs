//! AC9 of #39 (with AC6 on its diffs): the lens's drags on the test subjects, run headless on
//! the native host as an agent runs them (`wrela studio <pkg> ray|move|write`). Each subject
//! is copied to a scratch directory and the lens is built on the copy, so writes go through
//! `wrela edit` to the copy's files.
//!
//! The drags are spike 09's: 13 on the wolf (7 the solver's weights were tuned on, 6 held
//! out) and 6 on the grazer, chosen before the run. Each starts at a ray's hit and moves that
//! point by a fixed displacement. What the lens says it did is checked, and so is the source
//! it wrote, independently of the lens: the copy is rebuilt from the written files (a normal
//! build) and measured on the CPU.
//!
//! - The dragged point lands within 1 mm of its target after rounding: the rebuilt subject's
//!   distance at the target.
//! - At most 8 literals change.
//! - Locality: surface points of other parts (by provenance, a part and its mirror image being
//!   one part) move ≤ 0.5 mm RMS and ≤ 2 mm at most: each point's change of distance over the
//!   gradient's length, at 4,096 points of the original's surface.
//! - The piece count doesn't change (the lens's diagnosis after the write).
//! - The diff is minimal: only number tokens change, one per changed literal, and the changed
//!   lines are the lines holding them; a file `wrela fmt --check` accepted still passes.
//! - Each written literal reads back as the exact `f32` the solver chose.

use std::path::PathBuf;
use std::time::Instant;
use wrela_host::{CpuHost, Host, Value};
use wrela_tests::{one_f32, repo_root};

/// The lens's screen in these runs.
const SIZE: (u32, u32) = (512, 512);

/// How many frames to wait for the lens at most.
const WAIT: u32 = 400;

#[test]
fn ordinary_views_size_their_buffers_after_silhouettes() {
    assert_eq!(super::tests_pass(&repo_root().join("studio/tests/views")), 1);
}

#[test]
fn literal_rounding_uses_the_source_unit_and_exponent() {
    let pkg = super::scratch("studio-literal-rounding-source");
    std::fs::write(
        pkg.join("wrela.toml"),
        format!(
            "[package]\nname = \"rounding\"\n[dependencies]\nstudio = {{ path = {:?} }}\n",
            repo_root().join("studio").to_string_lossy()
        ),
    )
    .expect("manifest");
    std::fs::write(
        pkg.join("main.wrela"),
        "pub fn centimetres() -> f32 { 15cm }\n\
         pub fn zero() -> f32 { 0cm }\n\
         pub fn exponent() -> f32 { 1.0e-3 }\n\
         pub fn combined() -> f32 { 2.5e-3cm }\n\
         pub fn rounded(i: u32, v: f32) -> f32 { studio::drag::to_decimals(i, v) }\n\
         pub fn frame(time: f32, width: u32, height: u32) {}\n",
    )
    .expect("source");
    let dir = super::lift::build_into("studio-literal-rounding", &pkg, &["rounding"], false);
    let report = super::lift::report(&dir);
    let literals = report["literals"].as_array().expect("literals");
    let mut host = CpuHost::load(&dir).expect("host");
    for (text, value, expected) in [
        ("15cm", 0.156, 0.16),
        ("0cm", 0.156, 0.16),
        ("1.0e-3", 0.00126, 0.0013),
        ("2.5e-3cm", 0.0000264, 0.000026),
    ] {
        let index = literals.iter().position(|l| l["text"] == text).expect("lifted literal");
        let rounded = one_f32(&mut host, "rounded", &[Value::I32(index as i32), Value::F32(value)]);
        assert_eq!(rounded, expected, "rounding {value} in the style of {text}");
    }
}

/// AC9's limits, in metres.
const ERROR: f64 = 0.001;
const LITERALS: usize = 8;

/// A subject copied into a scratch directory, with its dependencies by absolute path, and the
/// lens built on the copy. A test that writes the copy's files has a subject of its own; the
/// tests that only read one share it ([`Subject::shared`]).
struct Subject {
    name: &'static str,
    pkg: PathBuf,
    page: PathBuf,
    /// Each source file of the copy and its text as built.
    files: Vec<(PathBuf, String)>,
}

impl Subject {
    fn new(name: &'static str, tag: &str) -> Subject {
        Subject::edited(name, tag, &[])
    }

    /// [`Subject::new`], with each `(from, to)` replacing text in the copy's sources first.
    fn edited(name: &'static str, tag: &str, edits: &[(&str, &str)]) -> Subject {
        let (pkg, files) = Subject::copy(name, tag, edits);
        Subject::built(name, pkg, files, true)
    }
    /// The subject `name` copied to a scratch directory, with `edits` made: the copy, and each
    /// of its source files with its text.
    fn copy(
        name: &'static str,
        tag: &str,
        edits: &[(&str, &str)],
    ) -> (PathBuf, Vec<(PathBuf, String)>) {
        let pkg = super::scratch(&format!("studio-{name}-{tag}"));
        wrela_tests::copy_package(&repo_root().join("examples").join(name), &pkg);
        let mut files = Vec::new();
        for path in wrela_tests::files_under(&pkg, &["wrela"]) {
            let mut text = std::fs::read_to_string(&path).expect("read");
            for (a, b) in edits {
                text = text.replace(a, b);
            }
            std::fs::write(&path, &text).expect("write");
            files.push((path, text));
        }
        (pkg, files)
    }

    /// A subject package `name` made of `files` (each a name and its text), beside a manifest
    /// and an empty main.
    fn made(name: &'static str, files: &[(&str, &str)]) -> Subject {
        let pkg = super::scratch(&format!("studio-{name}"));
        std::fs::write(pkg.join("wrela.toml"), format!("[package]\nname = \"{name}\"\n"))
            .expect("write");
        let mut all = vec![("main.wrela", "pub fn frame(time: f32, width: u32, height: u32) {}\n")];
        all.extend_from_slice(files);
        let mut written = Vec::new();
        for (file, text) in all {
            std::fs::write(pkg.join(file), text).expect("write");
            written.push((pkg.join(file), text.to_string()));
        }
        Subject::built(name, pkg, written, true)
    }

    /// The lens built on the package at `pkg`, and if `precompile`, its compiled code beside
    /// it, as `wrela studio` makes it after a build.
    fn built(
        name: &'static str,
        pkg: PathBuf,
        files: Vec<(PathBuf, String)>,
        precompile: bool,
    ) -> Subject {
        let (out, page) = wrela_driver::studio::build(&pkg, false).expect("the lens builds");
        assert!(
            !out.has_errors(),
            "{}",
            wrela_diag::render::render_all(&out.sources, &out.diagnostics)
        );
        if precompile {
            wrela_host::precompile(&page).expect("the lens compiles");
        }
        Subject { name, pkg, page, files }
    }

    /// Puts the copy's files back as they were built.
    fn restore(&self) {
        for (path, text) in &self.files {
            std::fs::write(path, text).expect("restore");
        }
    }

    /// The copy built now (a normal build, not lifted), on the CPU.
    fn rebuilt(&self, tag: &str) -> CpuHost {
        let dir = super::scratch(&format!("studio-{}-{tag}-cpu", self.name));
        wrela_tests::must_build(&self.pkg, &dir);
        CpuHost::load(&dir).expect("load")
    }

    /// The lens, loaded fresh: its writes go to the copy through `wrela edit`, as the
    /// studio's server writes them.
    fn lens(&self) -> Lens {
        self.lens_with(None)
    }

    /// [`Subject::lens`], whose fit's reference (`studio/reference`) is `reference`.
    fn lens_with(&self, reference: Option<Vec<u8>>) -> Lens {
        let post = wrela_driver::studio::post_handler(&self.pkg, &self.page, reference);
        let options = wrela_host::Options { post: Some(post), ..Default::default() };
        let mut host = Host::load_with(&self.page, &options).expect("the lens loads");
        host.frame(0.0, SIZE.0, SIZE.1).expect("a frame");
        let _ = host.take_logs();
        Lens { host, frame: 0, times: Vec::new() }
    }
}

/// What a lens's actions need of its host: the GPU's, which draws its frames, or the CPU's.
trait LensHost {
    fn call_export(&mut self, name: &str, args: &[Value]) -> wrela_host::Result<Vec<Value>>;
    fn frame(&mut self, time: f32, width: u32, height: u32) -> wrela_host::Result<()>;
    fn take_logs(&mut self) -> Vec<String>;
}

impl LensHost for Host {
    fn call_export(&mut self, name: &str, args: &[Value]) -> wrela_host::Result<Vec<Value>> {
        Host::call_export(self, name, args)
    }
    fn frame(&mut self, time: f32, width: u32, height: u32) -> wrela_host::Result<()> {
        Host::frame(self, time, width, height)
    }
    fn take_logs(&mut self) -> Vec<String> {
        Host::take_logs(self)
    }
}

impl LensHost for CpuHost {
    fn call_export(&mut self, name: &str, args: &[Value]) -> wrela_host::Result<Vec<Value>> {
        CpuHost::call_export(self, name, args)
    }
    fn frame(&mut self, time: f32, width: u32, height: u32) -> wrela_host::Result<()> {
        CpuHost::frame(self, time, width, height)
    }
    fn take_logs(&mut self) -> Vec<String> {
        CpuHost::take_logs(self)
    }
}

struct Lens<H = Host> {
    host: H,
    frame: u32,
    /// Each frame's wall time since the last action began, in ms.
    times: Vec<f64>,
}

impl<H: LensHost> Lens<H> {
    /// Runs an action: calls its export, frames until the lens isn't busy, and returns its
    /// answer (the last JSON line it printed).
    fn act(&mut self, name: &str, args: &[f32]) -> serde_json::Value {
        let values: Vec<Value> = args.iter().map(|a| Value::F32(*a)).collect();
        self.act_with(name, &values)
    }

    fn act_with(&mut self, name: &str, values: &[Value]) -> serde_json::Value {
        self.host.call_export(name, values).unwrap_or_else(|e| panic!("{name}: {e}"));
        self.times.clear();
        for _ in 0..WAIT {
            let busy = matches!(
                self.host.call_export("busy", &[]).expect("busy").as_slice(),
                [Value::I32(b)] if *b != 0
            );
            self.frame += 1;
            let t = Instant::now();
            self.host.frame(self.frame as f32 / 60.0, SIZE.0, SIZE.1).expect("a frame");
            self.times.push(t.elapsed().as_secs_f64() * 1000.0);
            if !busy {
                return self.answer(name);
            }
        }
        panic!("{name}: the lens was still busy after {WAIT} frames");
    }

    fn answer(&mut self, name: &str) -> serde_json::Value {
        let logs = self.host.take_logs();
        let last = logs.iter().rev().find(|l| l.starts_with('{'));
        let line = last.unwrap_or_else(|| panic!("{name}: no answer in {logs:?}"));
        serde_json::from_str(line).unwrap_or_else(|e| panic!("{name}: {e} in {line}"))
    }

    /// Where a ray meets the subject, and the part there.
    fn ray(&mut self, o: [f32; 3], d: [f32; 3]) -> Option<([f32; 3], String)> {
        let a = self.act("ray", &[o[0], o[1], o[2], d[0], d[1], d[2]]);
        if a["hit"] != true {
            return None;
        }
        Some((vec3(&a["point"]), a["part"]["name"].as_str().unwrap_or("").to_string()))
    }

    /// Names literals for the next drags (none: the drags choose).
    fn choose(&mut self, literals: &[u32]) {
        self.act_with("choose_none", &[]);
        for &i in literals {
            self.act_with("choose", &[Value::I32(i as i32)]);
        }
    }
    /// A drag of the point a ray hits by `delta`: the lens's answer.
    fn drag(&mut self, o: [f32; 3], d: [f32; 3], delta: [f32; 3]) -> serde_json::Value {
        let (from, _) = self.ray(o, d).expect("the ray hits");
        let to = add(from, delta);
        self.act("move", &[from[0], from[1], from[2], to[0], to[1], to[2]])
    }
}

fn vec3(v: &serde_json::Value) -> [f32; 3] {
    let n = |i: usize| v[i].as_f64().expect("a number") as f32;
    [n(0), n(1), n(2)]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

impl Subject {
    /// [`Subject::literal_on`], in `file`.
    fn literal_in(&self, file: &str, pattern: &str, k: usize) -> u32 {
        let r = super::lift::report(&self.page);
        let text = &self.files.iter().find(|(p, _)| p.ends_with(file)).unwrap().1;
        let line = 1 + text.lines().position(|l| l.contains(pattern)).expect(pattern);
        let files = r["files"].as_array().unwrap();
        let mut on: Vec<(u64, u32)> = r["literals"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| {
                let f = &files[l["file"].as_u64().unwrap() as usize];
                f["path"].as_str().unwrap().ends_with(file)
                    && l["line"].as_u64() == Some(line as u64)
            })
            .map(|l| (l["column"].as_u64().unwrap(), l["index"].as_u64().unwrap() as u32))
            .collect();
        on.sort();
        on[k].1
    }
}

fn args(p: [f32; 3]) -> [Value; 3] {
    [Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2])]
}

fn distance(host: &mut CpuHost, p: [f32; 3]) -> f32 {
    one_f32(host, "distance", &args(p))
}

/// AC10: the studio server answers only requests addressed to this machine by name (a page
/// elsewhere can't reach it through DNS rebinding), takes posts only from its own pages, and
/// listens on 127.0.0.1 alone.
#[test]
fn the_studio_server_answers_only_this_machine() {
    use std::io::{Read, Write};
    let dir = super::scratch("studio-server-local");
    let page = dir.join("page");
    std::fs::create_dir_all(&page).unwrap();
    std::fs::write(page.join("index.html"), "<html><body>lens</body></html>").unwrap();
    std::fs::write(dir.join("wrela.toml"), "[package]\nname = \"empty\"\n").unwrap();
    let server =
        wrela_driver::serve::start(&dir, &page, 0, false, None).expect("the server starts");
    let ask = |request: String| -> String {
        let mut s = std::net::TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
        s.write_all(request.as_bytes()).unwrap();
        let mut answer = String::new();
        let _ = s.read_to_string(&mut answer);
        answer.lines().next().unwrap_or("").to_string()
    };
    let port = server.port;
    assert!(ask(format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n")).contains("200"));
    assert!(ask(format!("GET / HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n")).contains("200"));
    assert!(
        ask("GET / HTTP/1.1\r\nHost: attacker.example\r\n\r\n".into()).contains("403"),
        "another host's name"
    );
    let edit = |origin: &str| {
        let body = r#"{"edits":[]}"#;
        format!(
            "POST /studio/edit HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{origin}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    assert!(
        ask(edit("Origin: https://attacker.example\r\n")).contains("403"),
        "a post from another site"
    );
    assert!(ask(edit("")).contains("403"), "a post with no origin");
    assert!(
        ask(edit(&format!("Origin: http://127.0.0.1:{port}\r\n"))).contains("200"),
        "a post from its page"
    );
    // It listens on the loopback address alone.
    let outside = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|u| u.connect("192.0.2.1:9").and_then(|_| u.local_addr()))
        .map(|a| a.ip());
    if let Ok(ip) = outside
        && !ip.is_loopback()
        && !ip.is_unspecified()
    {
        assert!(std::net::TcpStream::connect((ip, port)).is_err(), "the server answered on {ip}");
    }
}

/// AC10: nothing of the studio is in a game's build: the wolf's (its own program, which the
/// lens is built around) holds none of the lens's code, kernels or strings.
#[test]
fn nothing_of_the_studio_is_in_a_games_build() {
    let dir = super::scratch("studio-not-in-game");
    wrela_tests::must_build(&repo_root().join("examples/wolf"), &dir);
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
    for kernel in
        ["start_rays", "step_rays", "sample_grid", "snap_points", "show_canvas", "step_minima"]
    {
        assert!(!manifest.contains(kernel), "the game's build has the lens's kernel {kernel}");
    }
    let wasm = std::fs::read(dir.join("game.wasm")).unwrap();
    for text in [&b"studio/edit"[..], b"studio/reference", b"\"action\""] {
        assert!(
            !wasm.windows(text.len()).any(|w| w == text),
            "the game's build has {:?}",
            String::from_utf8_lossy(text)
        );
    }
    let toml = std::fs::read_to_string(repo_root().join("examples/wolf/wrela.toml")).unwrap();
    assert!(!toml.contains("studio"), "the wolf depends on the studio");
}
/// A ball 30 cm across on the ground, its spec (one check holds, one misses) and its blueprint
/// (from the side, a circle 2 cm wider all round; from the front, its own circle).
const BALL: [(&str, &str); 3] = [
    (
        "subject.wrela",
        "use std::field::{Surface, sphere}

pub fn subject() -> Surface {
    sphere(0.30).translate(vec3(0.0, 0.30, 0.0))
}
",
    ),
    (
        "spec.wrela",
        "use subject::subject
use std::field::{top, width}
use std::lift::Checks

pub fn spec<C: Checks>(c: mut C) {
    let f = subject()
    c.near(\"height\", top(f, x: 0.0, z: 0.0), 0.60)
    c.near(\"width\", width(f, y: 0.30, z: 0.0), 0.70, within: 0.01)
}
",
    ),
    (
        "blueprint.wrela",
        "use std::collections::Vec

fn circle(r: f32) -> Vec<vec2> {
    var out: Vec<vec2> = Vec::new()
    for i in 0..96 {
        let a = f32(i) / 96.0 * 6.2831855
        out.push(vec2(r * cos(a), 0.30 + r * sin(a)))
    }
    out
}

pub fn side() -> Vec<vec2> {
    circle(0.32)
}

pub fn front() -> Vec<vec2> {
    circle(0.30)
}
",
    ),
];

/// The tools of authoring round 3 (#39), on the ball: `spec` reports each check; `blueprint`
/// measures where the silhouette misses an outline, by how much and which way; `fit_blueprint`
/// fits a literal to it; `zoom` frames a close-up and the colour mode draws (clay, for a
/// subject without channels).
#[test]
#[ignore = "needs a GPU"]
fn specs_blueprints_and_close_ups_answer_on_a_ball() {
    let ball = Subject::made("ball", &BALL);
    let mut lens = ball.lens();
    let spec = lens.act("spec", &[]);
    assert_eq!((spec["held"].as_u64(), spec["missed"].as_u64()), (Some(1), Some(1)), "{spec}");
    let width = &spec["checks"][1];
    assert_eq!(width["name"], "width");
    assert!((width["miss"].as_f64().unwrap() + 0.1).abs() < 0.001, "{spec}");

    let side = lens.act_with("blueprint", &[Value::I32(0)]);
    let mean = side["mean_miss_cm"].as_f64().unwrap();
    assert!((mean - 2.0).abs() < 0.2, "{side}");
    assert_eq!(side["within_1cm"].as_f64(), Some(0.0), "{side}");
    let worst = &side["misses"][0];
    assert_eq!(worst["so"], "the subject falls short of the outline", "{side}");
    let front = lens.act_with("blueprint", &[Value::I32(1)]);
    assert_eq!(front["within_1cm"].as_f64(), Some(1.0), "{front}");
    let top = lens.act_with("blueprint", &[Value::I32(2)]);
    assert!(top["error"].as_str().is_some_and(|e| e.contains("no top outline")), "{top}");

    let radius = ball.literal_in("subject.wrela", "sphere(0.30)", 0);
    lens.choose(&[radius]);
    let fit = lens.act_with("fit_blueprint", &[Value::I32(0), Value::I32(256)]);
    let after = fit["literals"].as_array().unwrap().iter().find(|l| l["literal"] == radius);
    let after = after.expect("the radius")["after"].as_f64().unwrap();
    assert!((after - 0.32).abs() < 0.003, "the radius fitted to {after}: {fit}");
    let side = lens.act_with("blueprint", &[Value::I32(0)]);
    assert!(side["mean_miss_cm"].as_f64().unwrap() < 0.3, "{side}");

    let zoom = lens.act("zoom", &[0.0, 0.6, 0.0, 0.05]);
    assert_eq!(zoom["half"].as_f64(), Some(0.05), "{zoom}");
    let colour = lens.act_with("mode", &[Value::I32(5), Value::I32(0)]);
    assert_eq!(colour["mode"], "colour", "{colour}");
    lens.act_with("view", &[Value::I32(0)]);
    let rgba = lens.host.read_screen().expect("the screen");
    // Zoomed on the top of the ball: clay fills the lower half of the view (and the views' grid
    // is the fitted ball's: a stale one culls bands of it).
    let at = |x: usize, y: usize| &rgba[(y * SIZE.0 as usize + x) * 4..][..3];
    let clay = at(SIZE.0 as usize / 2, SIZE.1 as usize * 3 / 4);
    assert!(clay.iter().all(|&c| c > 150), "{clay:?}");
    let off = lens.act("zoom", &[0.0, 0.0, 0.0, 0.0]);
    assert_eq!(off["framing"], "the subject", "{off}");
}

/// AC9 of #51: the great tree is a plant in wrela source (examples/great-tree, `engine::plant`),
/// and the lens draws it, probes it and drags its literals, as it does a creature's. A tree is
/// ten times a creature's size: the lens searches for its bounds in a box that grows until it
/// holds the subject, and its rays reach past the subject's far side.
#[test]
#[ignore = "needs a GPU"]
fn the_lens_draws_probes_and_drags_the_great_tree() {
    let subject = Subject::new("great-tree", "lens");
    let mut lens = subject.lens();
    // Drawn: the whole tree, its crown 16 m up, framed in the side view.
    let d = lens.act("describe", &[]);
    let (lo, hi) = (vec3(&d["lo"]), vec3(&d["hi"]));
    assert!(hi[1] > 15.0 && hi[0] - lo[0] > 18.0, "the tree's bounds: {lo:?} to {hi:?}");
    lens.act_with("view", &[Value::I32(0)]);
    let screen = lens.host.read_screen().expect("the screen");
    let background = &screen[..4];
    let drawn = screen
        .chunks(4)
        .filter(|p| p[..3].iter().zip(background).any(|(a, b)| a.abs_diff(*b) > 24))
        .count();
    let share = drawn as f64 / (screen.len() / 4) as f64;
    assert!(share > 0.1, "the tree covers {:.1}% of the side view", share * 100.0);
    // Probed: the trunk's wood and the crown's clumps, each part named.
    let (_, trunk) = lens.ray([5.0, 0.5, 0.0], [-1.0, 0.0, 0.0]).expect("the trunk");
    let (_, crown) = lens.ray([30.0, 12.0, 0.0], [-1.0, 0.0, 0.0]).expect("the crown");
    assert_eq!((trunk.as_str(), crown.as_str()), ("limb", "clump"));
    let p = lens.act("probe", &[0.0, 0.5, 0.0]);
    assert!(p["distance"].as_f64().expect("a distance") < -0.5, "inside the trunk: {p}");
    // Dragged: a point on the crown's side 25 cm out, written through `wrela edit`, landing
    // within 1 mm of its target in the rebuilt tree.
    let moved = lens.drag([30.0, 12.0, 0.0], [-1.0, 0.0, 0.0], [0.25, 0.0, 0.0]);
    let changed = moved["literals"].as_array().map_or(0, Vec::len);
    assert!((1..=LITERALS).contains(&changed), "{moved}");
    assert!(moved["error_mm"].as_f64().expect("an error") <= 1.0, "{moved}");
    let to = vec3(&moved["to"]);
    let written = lens.act("write", &[]);
    assert_eq!(written["answer"]["written"], true, "{written}");
    let mut rebuilt = subject.rebuilt("drag");
    let landed = distance(&mut rebuilt, to).abs() as f64;
    assert!(landed <= ERROR, "the rebuilt tree is {:.2} mm from the target", landed * 1000.0);
    subject.restore();
}
