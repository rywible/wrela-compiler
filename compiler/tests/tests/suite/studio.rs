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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use wrela_host::{CpuHost, Host, Value};
use wrela_tests::{median, one_f32, one_u32, percentile, repo_root};

/// The lens's screen in these runs.
const SIZE: (u32, u32) = (512, 512);

/// How many frames to wait for the lens at most.
const WAIT: u32 = 400;

/// AC9's limits, in metres.
const ERROR: f64 = 0.001;
const RMS: f64 = 0.0005;
const MOST: f64 = 0.002;
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

    /// [`Subject::new`] for runs in Chrome alone: nothing loads its lens natively, so its
    /// compiled code isn't made.
    fn for_chrome(name: &'static str, tag: &str) -> Subject {
        let (pkg, files) = Subject::copy(name, tag, &[]);
        Subject::built(name, pkg, files, false)
    }

    /// [`Subject::edited`], made once per test process and shared by the tests that only read
    /// it (`tag` names the edits): none may write its files.
    fn shared(name: &'static str, tag: &str, edits: &[(&str, &str)]) -> Arc<Subject> {
        type Shared = Mutex<BTreeMap<String, Arc<OnceLock<Arc<Subject>>>>>;
        static SUBJECTS: Shared = Mutex::new(BTreeMap::new());
        let tag = format!("shared-{tag}");
        let once = SUBJECTS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(format!("{name}-{tag}"))
            .or_default()
            .clone();
        once.get_or_init(|| Arc::new(Subject::edited(name, &tag, edits))).clone()
    }

    /// The subject `name` copied to a scratch directory, with `edits` made: the copy, and each
    /// of its source files with its text.
    fn copy(
        name: &'static str,
        tag: &str,
        edits: &[(&str, &str)],
    ) -> (PathBuf, Vec<(PathBuf, String)>) {
        let pkg = super::scratch(&format!("studio-{name}-{tag}"));
        let from = repo_root().join("examples").join(name);
        let mut files = Vec::new();
        for e in std::fs::read_dir(&from).expect("the subject's directory") {
            let path = e.expect("an entry").path();
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            if !path.is_file() || !(file.ends_with(".wrela") || file == "wrela.toml") {
                continue;
            }
            let mut text = std::fs::read_to_string(&path).expect("read");
            if file == "wrela.toml" {
                let abs = |rel: &str| repo_root().join(rel).display().to_string();
                text = text
                    .replace("\"../../engine\"", &format!("{:?}", abs("engine")))
                    .replace("\"../round1\"", &format!("{:?}", abs("examples/round1")));
            } else {
                for (a, b) in edits {
                    text = text.replace(a, b);
                }
                files.push((pkg.join(&file), text.clone()));
            }
            std::fs::write(pkg.join(&file), text).expect("write");
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

struct Lens {
    host: Host,
    frame: u32,
    /// Each frame's wall time since the last action began, in ms.
    times: Vec<f64>,
}

impl Lens {
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

    /// Whether every literal's value in the lens is finite.
    fn finite(&mut self) -> bool {
        let a = self.act("literals", &[]);
        let all = a["literals"].as_array().expect("literals");
        all.iter().all(|l| l["value"].as_f64().is_some_and(f64::is_finite))
    }

    /// A drag of the point a ray hits by `delta`: the lens's answer.
    fn drag(&mut self, o: [f32; 3], d: [f32; 3], delta: [f32; 3]) -> serde_json::Value {
        let (from, _) = self.ray(o, d).expect("the ray hits");
        let to = add(from, delta);
        self.act("move", &[from[0], from[1], from[2], to[0], to[1], to[2]])
    }

    /// The pieces the lens's diagnosis counts, framing the subject's bounds.
    fn pieces(&mut self) -> u64 {
        let a = self.act("diagnose", &[0.0, 0.0, 0.0, 0.0]);
        a["pieces"].as_u64().unwrap_or_else(|| panic!("no pieces in {a}"))
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
    /// The index of literal `k` (from 0) among those on the first line of the subject's
    /// creature.wrela holding `pattern`, by the lens's build's report.
    fn literal_on(&self, pattern: &str, k: usize) -> u32 {
        self.literal_in("creature.wrela", pattern, k)
    }

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

/// A drag: its name, where it starts (the hit of a ray from `from` along `dir`; `None` for the
/// wolf's ear tip, found as spike 09 found it), and the displacement.
struct Case {
    name: &'static str,
    ray: Option<([f32; 3], [f32; 3])>,
    delta: [f32; 3],
    held_out: bool,
}

const fn case(
    name: &'static str,
    from: [f32; 3],
    dir: [f32; 3],
    delta: [f32; 3],
    held_out: bool,
) -> Case {
    Case { name, ray: Some((from, dir)), delta, held_out }
}

/// Spike 09's 13 drags on the wolf: 7 tuned, 6 held out.
const WOLF: [Case; 13] = [
    Case { name: "ear tip up 15 mm", ray: None, delta: [0.0, 0.015, 0.0], held_out: false },
    case("nose forward 12 mm", [0.0, 0.87, 2.0], [0.0, 0.0, -1.0], [0.0, 0.0, 0.012], false),
    case("belly down 10 mm", [0.0, 0.1, 0.0], [0.0, 1.0, 0.0], [0.0, -0.010, 0.0], false),
    case("tail tip down 20 mm", [0.0, 0.0, -0.69], [0.0, 1.0, 0.0], [0.0, -0.020, 0.0], false),
    case("back of thigh back 10 mm", [0.08, 0.5, -2.0], [0.0, 0.0, 1.0], [0.0, 0.0, -0.010], false),
    case("ribcage side out 10 mm", [2.0, 0.6, 0.1], [-1.0, 0.0, 0.0], [0.010, 0.0, 0.0], false),
    case("ear side up 15 mm", [2.0, 1.035, 0.56], [-1.0, 0.0, 0.0], [0.0, 0.015, 0.0], false),
    case("forepaw toe forward 8 mm", [0.07, 0.012, 2.0], [0.0, 0.0, -1.0], [0.0, 0.0, 0.008], true),
    case("hock back 10 mm", [0.078, 0.21, -2.0], [0.0, 0.0, 1.0], [0.0, 0.0, -0.010], true),
    case("withers up 10 mm", [0.0, 2.0, 0.2], [0.0, -1.0, 0.0], [0.0, 0.010, 0.0], true),
    case("croup down 10 mm", [0.0, 2.0, -0.47], [0.0, -1.0, 0.0], [0.0, -0.010, 0.0], true),
    case("cheek ruff out 8 mm", [2.0, 0.86, 0.55], [-1.0, 0.0, 0.0], [0.008, 0.0, 0.0], true),
    case("chest front forward 10 mm", [0.0, 0.56, 2.0], [0.0, 0.0, -1.0], [0.0, 0.0, 0.010], true),
];

/// 6 drags on the grazer, chosen as the wolf's were: its landmarks, by rays from outside.
const GRAZER: [Case; 6] = [
    case("horn tip up 20 mm", [0.2, 3.5, 1.814], [0.0, -1.0, 0.0], [0.0, 0.020, 0.0], true),
    case("muzzle forward 15 mm", [0.0, 1.7, 4.0], [0.0, 0.0, -1.0], [0.0, 0.0, 0.015], true),
    case("hump up 20 mm", [0.0, 3.5, 0.8], [0.0, -1.0, 0.0], [0.0, 0.020, 0.0], true),
    case("tail tuft down 20 mm", [0.0, 0.0, -0.41], [0.0, 1.0, 0.0], [0.0, -0.020, 0.0], true),
    case("fore hoof forward 10 mm", [0.205, 0.04, 3.0], [0.0, 0.0, -1.0], [0.0, 0.0, 0.010], true),
    case("barrel side out 15 mm", [2.0, 1.1, 0.5], [-1.0, 0.0, 0.0], [0.015, 0.0, 0.0], true),
];

/// The wolf's ear tip as spike 09 found it: the highest hit on the ear of a grid of rays down.
fn ear_tip(lens: &mut Lens) -> [f32; 3] {
    let mut best: Option<[f32; 3]> = None;
    for i in 0..=10 {
        for j in 0..=12 {
            let o = [0.02 + 0.008 * i as f32, 1.4, 0.48 + 0.012 * j as f32];
            if let Some((p, part)) = lens.ray(o, [0.0, -1.0, 0.0])
                && part == "ear"
                && best.is_none_or(|b| p[1] > b[1])
            {
                best = Some(p);
            }
        }
    }
    best.expect("a ray hits the ear")
}

/// Points on the original subject's surface: the first 4,096 of `Box3::sample` within 2 cm,
/// each moved onto the surface by three Newton steps, with the part there and the distance.
fn surface(host: &mut CpuHost) -> Vec<([f32; 3], u32, f32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while out.len() < 4096 {
        let p = match host.call_export("sample", &[Value::I32(7), Value::I32(i)]).expect("sample")[..]
        {
            [Value::F32(x), Value::F32(y), Value::F32(z)] => [x, y, z],
            ref other => panic!("sample returned {other:?}"),
        };
        i += 1;
        if distance(host, p).abs() >= 0.02 {
            continue;
        }
        let mut q = p;
        for _ in 0..3 {
            let d = distance(host, q);
            let g = gradient(host, q);
            let gg = g[0] * g[0] + g[1] * g[1] + g[2] * g[2];
            if gg > 0.0 {
                q = [q[0] - g[0] * d / gg, q[1] - g[1] * d / gg, q[2] - g[2] * d / gg];
            }
        }
        let d = distance(host, q);
        if d.abs() < 1e-4 {
            out.push((q, part(host, q), d));
        }
    }
    out
}

fn args(p: [f32; 3]) -> [Value; 3] {
    [Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2])]
}

fn distance(host: &mut CpuHost, p: [f32; 3]) -> f32 {
    one_f32(host, "distance", &args(p))
}

fn gradient(host: &mut CpuHost, p: [f32; 3]) -> [f32; 3] {
    match host.call_export("gradient_at", &args(p)).expect("gradient_at")[..] {
        [Value::F32(x), Value::F32(y), Value::F32(z)] => [x, y, z],
        ref other => panic!("gradient_at returned {other:?}"),
    }
}

fn part(host: &mut CpuHost, p: [f32; 3]) -> u32 {
    one_u32(host, "part", &args(p))
}

/// Whether part `i` has a share in the surface at `p` (over 0.1%): there it's part of a blend,
/// which moves with it.
fn shares(host: &mut CpuHost, p: [f32; 3], i: u32) -> bool {
    let mut a = args(p).to_vec();
    a.push(Value::I32(i as i32));
    one_f32(host, "part_share_at", &a).abs() > 1.0e-3
}

/// What one drag did, as measured.
struct Outcome {
    name: &'static str,
    held_out: bool,
    part: String,
    /// The literals changed, as `file:line old -> new`.
    changes: Vec<String>,
    /// The rebuilt source's distance at the target, and the other parts' motion: RMS and most.
    error: f64,
    rms: f64,
    most: f64,
    /// The same, as the lens measured them.
    lens: (f64, f64, f64),
    pieces: (u64, u64),
    stopped: bool,
    /// Wall time of the frames that stepped the drag (each a solve, an upload, a frame of the
    /// views, and the GPU's piece check started): median and most, in ms.
    step_ms: (f64, f64),
    /// What failed.
    problems: Vec<String>,
}

fn drags(name: &'static str, cases: &[Case]) -> Vec<Outcome> {
    let subject = Subject::new(name, "drags");
    let mut original = subject.rebuilt("original");
    let points = surface(&mut original);
    let mut out = Vec::new();
    let mut ear = None;
    for c in cases {
        subject.restore();
        let mut lens = subject.lens();
        let pieces_before = lens.pieces();
        let (from, part_name) = match c.ray {
            Some((o, d)) => lens.ray(o, d).unwrap_or_else(|| panic!("{}: the ray misses", c.name)),
            None => {
                let p = *ear.get_or_insert_with(|| ear_tip(&mut lens));
                (p, "ear".into())
            }
        };
        let to = add(from, c.delta);
        let moved = lens.act("move", &[from[0], from[1], from[2], to[0], to[1], to[2]]);
        let step_ms = median_and_most(&lens.times);
        let written = lens.act("write", &[]);
        let pieces_after = lens.pieces();
        out.push(check(
            &subject,
            &mut original,
            c,
            &part_name,
            &points,
            to,
            &moved,
            &written,
            (pieces_before, pieces_after),
            step_ms,
        ));
    }
    subject.restore();
    out
}

fn median_and_most(times: &[f64]) -> (f64, f64) {
    if times.is_empty() {
        return (0.0, 0.0);
    }
    (median(times), percentile(times, 1.0))
}

#[allow(clippy::too_many_arguments)]
fn check(
    subject: &Subject,
    original: &mut CpuHost,
    c: &Case,
    part_name: &str,
    points: &[([f32; 3], u32, f32)],
    to: [f32; 3],
    moved: &serde_json::Value,
    written: &serde_json::Value,
    pieces: (u64, u64),
    step_ms: (f64, f64),
) -> Outcome {
    let mut problems = Vec::new();
    let num = |v: &serde_json::Value| v.as_f64().unwrap_or(f64::NAN);
    let lits = moved["literals"].as_array().cloned().unwrap_or_default();
    // The solver's values, exactly.
    let after: Vec<(u64, f32)> =
        lits.iter().map(|l| (l["literal"].as_u64().unwrap(), num(&l["after"]) as f32)).collect();
    let changes: Vec<String> = lits
        .iter()
        .map(|l| {
            format!(
                "{}:{} {} -> {}",
                l["source"]["file"].as_str().unwrap_or("?"),
                l["source"]["line"],
                l["before"],
                l["after"]
            )
        })
        .collect();
    if after.len() > LITERALS {
        problems.push(format!("{} literals changed", after.len()));
    }
    // The write: what `wrela edit` answered, and the files now.
    let answer = &written["answer"];
    if !after.is_empty() && answer["written"] != true {
        problems.push(format!("not written: {written}"));
    }
    let mut edited = 0;
    for f in answer["files"].as_array().cloned().unwrap_or_default() {
        let file = f["file"].as_str().unwrap();
        let name = Path::new(file).file_name().unwrap();
        let (path, old) = subject
            .files
            .iter()
            .find(|(p, _)| p.file_name() == Some(name))
            .unwrap_or_else(|| panic!("{file} isn't the subject's"));
        let new = std::fs::read_to_string(path).expect("read");
        edited += minimal(old, &new, &f, &mut problems);
        let fmt = |t: &str| {
            let parsed = wrela_syntax::parse(wrela_diag::FileId(0), t);
            wrela_syntax::fmt::format(&parsed, t) == t
        };
        if fmt(old) && !fmt(&new) {
            problems.push(format!("{file} no longer passes `wrela fmt --check`"));
        }
        // Each edit reads back as the solver's value.
        use wrela_driver::studio::Numbers;
        let (then, now) = (Numbers::new(old.clone()), Numbers::new(new.clone()));
        for e in f["edits"].as_array().cloned().unwrap_or_default() {
            let (s, t) = (e["start"].as_u64().unwrap() as u32, e["end"].as_u64().unwrap() as u32);
            let Some((s2, t2)) = wrela_driver::studio::moved(&then, &now, s, t) else {
                problems.push(format!("{file}: the literal at {s} is gone"));
                continue;
            };
            let text = &new[s2 as usize..t2 as usize];
            let read = wrela_driver::edit::signed_value(text);
            let want = num(&e["value"]) as f32;
            if read.map(f32::to_bits) != Some(want.to_bits()) {
                problems.push(format!("{file}: `{text}` reads {read:?}, not {want}"));
            }
            if !after.iter().any(|(_, v)| v.to_bits() == want.to_bits()) {
                problems.push(format!("{file}: wrote {want}, which the solver didn't choose"));
            }
        }
    }
    if edited != after.len() {
        problems.push(format!("{edited} literals written, {} changed", after.len()));
    }
    // The written source, rebuilt: the target, and the other parts' motion.
    let mut rebuilt = subject.rebuilt("edited");
    let error = f64::from(distance(&mut rebuilt, to).abs());
    let dragged = part(original, vec3(&moved["from"]));
    let from_index = one_u32(original, "part_index", &args(vec3(&moved["from"])));
    let twin = one_u32(original, "image_part", &[Value::I32(from_index as i32)]);
    let (mut sum, mut most, mut n) = (0.0f64, 0.0f64, 0usize);
    let mut worst: Vec<(f64, [f32; 3], u32)> = Vec::new();
    for &(p, i, d0) in points {
        if i == dragged || shares(original, p, from_index) || shares(original, p, twin) {
            continue;
        }
        let d0 = f64::from(d0);
        let d1 = f64::from(distance(&mut rebuilt, p));
        let g = gradient(&mut rebuilt, p);
        let len = f64::from((g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt()).max(1e-6);
        let m = (d1 - d0).abs() / len;
        sum += m * m;
        most = most.max(m);
        n += 1;
        if m > MOST {
            worst.push((m, p, i));
        }
    }
    worst.sort_by(|a, b| b.0.total_cmp(&a.0));
    let from = vec3(&moved["from"]);
    for (m, p, i) in worst.iter().take(6) {
        let gap =
            ((p[0] - from[0]).powi(2) + (p[1] - from[1]).powi(2) + (p[2] - from[2]).powi(2)).sqrt();
        eprintln!(
            "      {}: moved {:.2} mm at {p:?}, part {i}, {:.0} mm from the drag",
            c.name,
            m * 1000.0,
            gap * 1000.0
        );
    }
    let rms = (sum / n.max(1) as f64).sqrt();
    let stopped = moved["stopped"] == true;
    if error > ERROR {
        problems.push(format!("the target is {:.3} mm off", error * 1000.0));
    }
    if rms > RMS || most > MOST {
        problems.push(format!(
            "other parts moved {:.3} mm RMS, {:.3} mm at most",
            rms * 1000.0,
            most * 1000.0
        ));
    }
    if pieces.0 != pieces.1 {
        problems.push(format!("{} pieces became {}", pieces.0, pieces.1));
    }
    if stopped {
        problems.push("stopped: a step would have split the subject".into());
    }
    Outcome {
        name: c.name,
        held_out: c.held_out,
        part: part_name.to_string(),
        changes,
        error,
        rms,
        most,
        lens: (
            num(&moved["error_mm"]) / 1000.0,
            num(&moved["others_rms_mm"]) / 1000.0,
            num(&moved["others_most_mm"]) / 1000.0,
        ),
        pieces,
        stopped,
        step_ms,
        problems,
    }
}

/// Whether `wrela edit`'s answer `f` for a file is a minimal diff of `old` into `new`: each
/// edit replaces one literal (a number, with its minus if it took one) with another, nothing
/// else changed, and the changed lines are the lines holding changed literals. The literals
/// changed.
fn minimal(old: &str, new: &str, f: &serde_json::Value, problems: &mut Vec<String>) -> usize {
    let file = f["file"].as_str().unwrap_or("?");
    let one_number = |t: &str| {
        use wrela_syntax::token::TokenKind;
        let t = t.strip_prefix('-').unwrap_or(t).trim_start();
        let lexed = wrela_syntax::lexer::lex(wrela_diag::FileId(0), t);
        let tokens: Vec<_> =
            lexed.tokens.iter().filter(|k| !matches!(k.kind, TokenKind::Eof)).collect();
        tokens.len() == 1 && tokens[0].kind.is_number() && tokens[0].span.end as usize == t.len()
    };
    let mut rebuilt = String::new();
    let mut at = 0;
    let mut changed = 0;
    let mut lines = std::collections::BTreeSet::new();
    let mut edits = f["edits"].as_array().cloned().unwrap_or_default();
    edits.sort_by_key(|e| e["start"].as_u64());
    for e in &edits {
        let (s, t) = (e["start"].as_u64().unwrap() as usize, e["end"].as_u64().unwrap() as usize);
        let (was, now) = (e["old"].as_str().unwrap_or(""), e["new"].as_str().unwrap_or(""));
        if &old[s..t] != was || !one_number(was) || !one_number(now) {
            problems.push(format!("{file}: edit `{was}` -> `{now}` isn't one literal for another"));
        }
        rebuilt.push_str(&old[at..s]);
        rebuilt.push_str(now);
        at = t;
        if was != now {
            changed += 1;
            lines.insert(old[..s].matches('\n').count());
        }
    }
    rebuilt.push_str(&old[at..]);
    if rebuilt != new {
        problems.push(format!("{file}: more changed than the edits"));
    }
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let differ: std::collections::BTreeSet<usize> =
        (0..a.len().max(b.len())).filter(|&i| a.get(i) != b.get(i)).collect();
    if a.len() != b.len() || differ != lines {
        problems.push(format!(
            "{file}: lines {differ:?} changed, the changed literals are on {lines:?}"
        ));
    }
    changed
}

/// Prints the drags as a table, and fails on any drag that missed a limit.
fn report(subject: &str, outcomes: &[Outcome]) {
    eprintln!("\n{subject}: {} drags", outcomes.len());
    for o in outcomes {
        eprintln!(
            "{}{} (on `{}`): error {:.3} mm, others {:.3} mm RMS / {:.3} mm most \
             (the lens: {:.3}, {:.3} / {:.3}), pieces {} -> {}{}, step {:.1} ms median, {:.1} most",
            if o.held_out { "[held out] " } else { "" },
            o.name,
            o.part,
            o.error * 1000.0,
            o.rms * 1000.0,
            o.most * 1000.0,
            o.lens.0 * 1000.0,
            o.lens.1 * 1000.0,
            o.lens.2 * 1000.0,
            o.pieces.0,
            o.pieces.1,
            if o.stopped { " (stopped)" } else { "" },
            o.step_ms.0,
            o.step_ms.1,
        );
        for c in &o.changes {
            eprintln!("    {c}");
        }
        for p in &o.problems {
            eprintln!("    FAIL: {p}");
        }
    }
    let failed: Vec<&str> =
        outcomes.iter().filter(|o| !o.problems.is_empty()).map(|o| o.name).collect();
    assert!(failed.is_empty(), "{subject}: drags that missed AC9: {failed:?}");
}

#[test]
#[ignore = "long: needs a GPU"]
fn the_wolfs_drags_meet_ac9() {
    report("wolf", &drags("wolf", &WOLF));
}

#[test]
#[ignore = "long: needs a GPU"]
fn the_grazers_drags_meet_ac9() {
    report("grazer", &drags("grazer", &GRAZER));
}

/// A drag that would split the subject stops at the last point found whole, and says so: the
/// wolf's nose pad, dragged 30 mm forward, leaves the muzzle (as spike 09 found). The pieces the
/// lens's diagnosis counts don't change.
#[test]
#[ignore = "long: needs a GPU"]
fn a_drag_that_would_split_the_wolf_stops_while_it_is_whole() {
    let subject = Subject::shared("wolf", "as-is", &[]);
    let mut lens = subject.lens();
    let before = lens.pieces();
    let moved = lens.drag([0.0, 0.87, 2.0], [0.0, 0.0, -1.0], [0.0, 0.0, 0.030]);
    eprintln!("{moved}");
    assert_eq!(moved["stopped"], true, "the drag says it stopped");
    let reached = moved["reached"].as_f64().unwrap();
    assert!(reached > 0.0 && reached < 1.0, "it went part of the way: {reached}");
    assert!(moved["error_mm"].as_f64().unwrap() < 1.0, "it reached the point it stopped at");
    assert_eq!(lens.pieces(), before, "the pieces didn't change");
}

/// No NaN and no crash (AC9's robustness): with every literal named for a drag, with a literal
/// four parts share (the wolf's toes'), with literals the dragged point doesn't depend on (zero
/// derivatives), and with two that move it alike (collinear derivatives).
#[test]
#[ignore = "long: needs a GPU"]
fn drags_with_odd_literals_stay_finite() {
    let subject = Subject::shared("wolf", "as-is", &[]);
    let count = super::lift::report(&subject.page)["literals"].as_array().unwrap().len() as u32;
    // A ray's origin and direction, and the drag's displacement.
    type Ray = ([f32; 3], [f32; 3], [f32; 3]);
    let nose: Ray = ([0.0, 0.87, 2.0], [0.0, 0.0, -1.0], [0.0, 0.0, 0.012]);
    let cases: Vec<(&str, Vec<u32>, Ray)> = vec![
        ("every literal", (0..count).collect(), nose),
        (
            "a literal the four paws share",
            vec![subject.literal_on("const TOE_IN", 2)],
            ([0.07, 0.012, 2.0], [0.0, 0.0, -1.0], [0.0, 0.0, 0.008]),
        ),
        (
            "the tail's, for the nose: zero derivatives",
            vec![
                subject.literal_on("pub const J_TAIL3", 1),
                subject.literal_on("pub const J_TAIL3", 2),
            ],
            nose,
        ),
        (
            "the head's height and the cranium's: collinear derivatives",
            vec![
                subject.literal_on("pub const J_HEAD", 1),
                subject.literal_on("ell(vec3(0.064, 0.058, 0.072))", 4),
            ],
            ([0.0, 1.5, 0.57], [0.0, -1.0, 0.0], [0.0, 0.008, 0.0]),
        ),
    ];
    for (name, named, (o, d, delta)) in cases {
        let mut lens = subject.lens();
        lens.choose(&named);
        let moved = lens.drag(o, d, delta);
        eprintln!(
            "{name} ({} named): error {} mm, others {} mm at most, {} changed",
            named.len(),
            moved["error_mm"],
            moved["others_most_mm"],
            moved["changed"]
        );
        for k in ["error_mm", "others_rms_mm", "others_most_mm"] {
            assert!(moved[k].as_f64().is_some_and(f64::is_finite), "{name}: {k} is {}", moved[k]);
        }
        assert!(lens.finite(), "{name}: a literal isn't finite");
        if name.starts_with("the tail") {
            assert_eq!(
                moved["changed"], 0,
                "{name}: literals that don't reach the point don't change"
            );
        }
        if name.starts_with("the head") {
            assert!(moved["error_mm"].as_f64().unwrap() < 1.0, "{name}: {moved}");
        }
    }
}

/// The wolf with a longer neck (its head 40 mm up and 45 mm forward) and bigger ears (86 mm to
/// 110): the fits' target.
const LONGER_NECK: [(&str, &str); 2] = [
    (
        "pub const J_HEAD: vec3 = vec3(0.0, 0.912, 0.592)",
        "pub const J_HEAD: vec3 = vec3(0.0, 0.952, 0.637)",
    ),
    ("const EAR_H: f32 = 0.086", "const EAR_H: f32 = 0.110"),
];

/// AC9's fit, as spike 09 ran it: the target is the wolf with a longer neck (its head 40 mm up
/// and 45 mm forward) and bigger ears (86 mm to 110). Its side silhouette (512², 4 mm a pixel,
/// the lens's own framing) is the reference. Fit A names the 3 literals that changed; fit B
/// adds 6 that also shape the head and neck. Each fit's silhouette matches the reference (IoU
/// ≥ 0.99), the changed literals are recovered within 2 mm, the others move the surface 2 mm at
/// most (on a separate CPU build: the head's scale and pitch have no millimetres of their own),
/// and the fit takes 10 s at most.
#[test]
#[ignore = "measure: needs a GPU"]
fn fits_recover_the_wolfs_longer_neck_and_bigger_ears() {
    const RES: u32 = 512;
    let target = Subject::shared("wolf", "longer-neck", &LONGER_NECK);
    let subject = Subject::shared("wolf", "as-is", &[]);
    // The side view's framing, the lens's own.
    let framings = subject.lens().act_with("view", &[Value::I32(4)]);
    let side = &framings["framings"][0];
    let (centre, half) = (vec3(&side["centre"]), side["half"].as_f64().unwrap() as f32);
    let frame_args = |facing: u32| {
        vec![
            Value::I32(facing as i32),
            Value::F32(centre[0]),
            Value::F32(centre[1]),
            Value::F32(centre[2]),
            Value::F32(half),
            Value::I32(RES as i32),
        ]
    };
    // The reference: the target's silhouette, read off the screen.
    let mut t = target.lens();
    t.act_with("silhouette", &frame_args(0));
    let rgba = t.host.read_screen().expect("the screen");
    let mut crop = Vec::with_capacity((RES * RES * 4) as usize);
    for y in 0..RES as usize {
        let row = y * SIZE.0 as usize * 4;
        crop.extend_from_slice(&rgba[row..row + RES as usize * 4]);
    }
    let reference = wrela_driver::studio::reference_mask(RES, RES, &crop).expect("a mask");
    let at = |pattern: &str, k: usize| subject.literal_on(pattern, k);
    let changed = [
        (at("pub const J_HEAD", 1), 0.952f32),
        (at("pub const J_HEAD", 2), 0.637),
        (at("const EAR_H", 0), 0.110),
    ];
    let distractors = [
        at("pub const HEAD_PITCH", 0),
        at("pub const HEAD_SCALE", 0),
        at("pub const J_NECK", 1),
        at("pub const J_NECK", 2),
        at("r_top: 0.122, r_bottom: 0.075", 0),
        at("r_top: 0.122, r_bottom: 0.075", 1),
    ];
    let mut problems = Vec::new();
    for (name, extra) in [("A", &[][..]), ("B", &distractors[..])] {
        let mut lens = subject.lens_with(Some(reference.clone()));
        let named: Vec<u32> = changed.iter().map(|c| c.0).chain(extra.iter().copied()).collect();
        lens.choose(&named);
        let started = Instant::now();
        let fit = lens.act_with("fit", &frame_args(0));
        let seconds = started.elapsed().as_secs_f64();
        let iou = fit["iou_after"].as_f64().unwrap_or(0.0);
        let after = |i: u32| -> f32 {
            let l = fit["literals"].as_array().unwrap().iter().find(|l| l["literal"] == i).unwrap();
            l["after"].as_f64().unwrap() as f32
        };
        let recovered: Vec<f64> =
            changed.iter().map(|&(i, truth)| f64::from((after(i) - truth).abs())).collect();
        let moves: Vec<(u32, f32)> = extra.iter().map(|&i| (i, after(i))).collect();
        let others = surface_motion(&subject, &moves);
        eprintln!(
            "fit {name} ({} literals): IoU {} -> {iou:.5} in {seconds:.1} s ({} steps); recovered within {:.3?} mm; the others move the surface {:.3} mm at most",
            named.len(),
            fit["iou_before"],
            fit["steps"],
            recovered.iter().map(|r| r * 1000.0).collect::<Vec<_>>(),
            others * 1000.0
        );
        for l in fit["literals"].as_array().unwrap() {
            eprintln!(
                "    {} -> {}  ({})",
                l["before"],
                l["after"],
                l["source"]["source"].as_str().unwrap_or("").trim()
            );
        }
        if iou < 0.99 {
            problems.push(format!("fit {name}: IoU {iou}"));
        }
        if recovered.iter().any(|&r| r > 0.002) {
            problems.push(format!("fit {name}: recovered within {recovered:?} m"));
        }
        if others > 0.002 {
            problems.push(format!("fit {name}: the others moved the surface {others} m"));
        }
        if seconds > 10.0 {
            problems.push(format!("fit {name}: {seconds} s"));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// How far the literals' new values (by the lens's index) move the subject's surface: the most
/// change of distance over the gradient's length at 4,096 points of its surface, on a lifted CPU
/// build of the subject (its literals matched to the lens's by place).
fn surface_motion(subject: &Subject, changes: &[(u32, f32)]) -> f64 {
    if changes.is_empty() {
        return 0.0;
    }
    let dir = super::lift::build_into(
        &format!("studio-{}-motion", subject.name),
        &subject.pkg,
        &[subject.name],
        false,
    );
    let mut host = CpuHost::load(&dir).expect("load");
    let place = |r: &serde_json::Value, i: usize| {
        let l = &r["literals"][i];
        let f = &r["files"][l["file"].as_u64().unwrap() as usize];
        // The file's name without a package's prefix (`[wolf] creature.wrela`).
        let name = f["name"].as_str().unwrap();
        let name = name.split_once("] ").map_or(name, |(_, n)| n).to_string();
        (name, l["start"].as_u64().unwrap())
    };
    let (lens, cpu) = (super::lift::report(&subject.page), super::lift::report(&dir));
    let n = cpu["literals"].as_array().unwrap().len();
    let points = surface(&mut host);
    for &(i, v) in changes {
        let want = place(&lens, i as usize);
        let k = (0..n).find(|&k| place(&cpu, k) == want).expect("the literal in the CPU build");
        host.call_export("set", &[Value::I32(k as i32), Value::F32(v)]).expect("set");
    }
    let mut most = 0.0f64;
    for &(p, _, d0) in &points {
        let d1 = distance(&mut host, p);
        let g = gradient(&mut host, p);
        let len = f64::from((g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt()).max(1e-6);
        most = most.max(f64::from((d1 - d0).abs()) / len);
    }
    most
}

/// A landmark: its name, where it is in the side view (y and z, metres), and the part a click
/// there should find (its name, and its group where a name repeats).
struct Landmark {
    name: &'static str,
    at: [f32; 2],
    part: &'static str,
    group: Option<&'static str>,
}

const fn mark(
    name: &'static str,
    at: [f32; 2],
    part: &'static str,
    group: Option<&'static str>,
) -> Landmark {
    Landmark { name, at, part, group }
}

/// The wolf's landmarks, chosen from its side view before the run.
const WOLF_MARKS: [Landmark; 10] = [
    mark("nose", [0.873, 0.806], "nose", None),
    mark("ear", [1.023, 0.570], "ear", None),
    mark("muzzle", [0.879, 0.728], "muzzle", None),
    mark("cheek ruff", [0.890, 0.523], "cheek ruff", None),
    mark("ribcage", [0.623, 0.040], "ribcage", None),
    mark("thigh", [0.561, -0.411], "thigh", None),
    mark("forearm", [0.294, 0.266], "forearm", None),
    mark("tail tip", [0.376, -0.699], "tail tip", None),
    mark("fore paw", [0.027, 0.307], "paw", Some("fore leg")),
    mark("hind paw", [0.017, -0.442], "paw", Some("hind leg")),
];

/// The grazer's landmarks, chosen from its side view before the run.
const GRAZER_MARKS: [Landmark; 10] = [
    mark("horn tip", [2.147, 1.800], "horn tip", None),
    mark("muzzle", [1.764, 1.966], "muzzle", None),
    mark("ear", [2.067, 1.654], "ear", None),
    mark("shoulder hump", [1.637, 0.818], "shoulder hump", None),
    mark("barrel", [1.095, 0.499], "barrel", None),
    mark("rump", [1.318, -0.043], "rump", None),
    mark("thigh", [0.935, -0.043], "thigh", None),
    mark("tail tuft", [0.728, -0.409], "tuft", None),
    mark("fore hoof", [0.058, 1.041], "hoof", Some("fore leg")),
    mark("hind hoof", [0.058, 0.005], "hoof", Some("hind leg")),
];

/// AC5's clicks: on each subject, 10 clicks on landmarks in its side view (chosen before the
/// run). The part is right for at least 9, and the top literal belongs to the clicked part's
/// code, or to a constant it uses, for at least 8: the part's own distance reads it (`own`, by
/// `std::lift::reads`). Each click answers every literal that reaches the field there, ranked,
/// with its source line.
fn clicks(name: &'static str, marks: &[Landmark]) {
    let subject = Subject::shared(name, "as-is", &[]);
    let mut lens = subject.lens();
    lens.act_with("view", &[Value::I32(0)]);
    let (mut parts, mut owns, mut times) = (0, 0, Vec::new());
    for m in marks {
        let projected = lens.act("project", &[0.5, m.at[0], m.at[1]]);
        let v = &projected["views"][0];
        let (x, y) = (v["x"].as_f64().unwrap() as f32, v["y"].as_f64().unwrap() as f32);
        let started = Instant::now();
        let click = lens.act("click", &[x, y]);
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(click["hit"], true, "{name}: the click on the {} hits", m.name);
        let part = &click["part"];
        let right = part["name"] == m.part && m.group.is_none_or(|g| part["group"] == g);
        let lits = click["literals"].as_array().unwrap();
        let top = &lits[0];
        let own = top["own"] == true;
        parts += usize::from(right);
        owns += usize::from(own);
        eprintln!(
            "{name} {}: part {}/{}{}; {} literals reach it; top: {} = {} ({}), {}",
            m.name,
            part["name"],
            part["group"],
            if right { "" } else { " (WRONG)" },
            lits.len(),
            top["literal"]["text"],
            top["literal"]["derivative"],
            top["literal"]["source"].as_str().unwrap_or("").trim(),
            if own { "the part's own" } else { "NOT the part's own" },
        );
        // Every literal ranked, with its source line, most influence first.
        let ds: Vec<f64> =
            lits.iter().map(|l| l["literal"]["derivative"].as_f64().unwrap().abs()).collect();
        assert!(ds.windows(2).all(|w| w[0] >= w[1]), "{name} {}: ranked", m.name);
        assert!(lits.iter().all(|l| l["literal"]["line"].as_u64().is_some()), "{name}: lines");
    }
    let (median, most) = median_and_most(&times);
    eprintln!(
        "{name}: {parts}/10 parts right, {owns}/10 top literals the part's own; a click {median:.1} ms median, {most:.1} most (native)"
    );
    assert!(parts >= 9, "{name}: {parts} of 10 parts right");
    assert!(owns >= 8, "{name}: {owns} of 10 top literals the part's own");
}

#[test]
#[ignore = "long: needs a GPU"]
fn clicks_on_the_wolfs_landmarks_find_their_parts_and_source() {
    clicks("wolf", &WOLF_MARKS);
}

#[test]
#[ignore = "long: needs a GPU"]
fn clicks_on_the_grazers_landmarks_find_their_parts_and_source() {
    clicks("grazer", &GRAZER_MARKS);
}

// ---- AC10: one session, both hosts ----------------------------------------------------------------

/// The frames a command gets before the next one is typed.
fn frames_for(command: &str) -> u32 {
    match command.split(' ').next().unwrap_or("") {
        "move" | "fit" => 300,
        "diagnose" | "write" => 60,
        _ => 20,
    }
}

/// A script (runtime/abi `input`) typing each command and Enter, each at the frame where its
/// predecessor's allowance ends: the script, the frame each command is typed at, and the frames
/// in all.
fn session_script(commands: &[String]) -> (String, Vec<u32>, u32) {
    let (mut events, mut at, mut frame) = (Vec::new(), Vec::new(), 2);
    for c in commands {
        events.push(serde_json::json!({ "frame": frame, "type": "text", "text": c }));
        events.push(serde_json::json!({ "frame": frame, "type": "key", "key": "Enter" }));
        at.push(frame);
        frame += frames_for(c);
    }
    (serde_json::Value::Array(events).to_string(), at, frame + 2)
}

/// What a session gave: the lens's answers, its last frame, and (in Chrome) each frame's CPU
/// time and each pass's GPU time by frame, in ms.
struct Session {
    answers: Vec<serde_json::Value>,
    frame: Vec<u8>,
    cpu: Vec<f64>,
    gpu: Vec<(usize, String, f64)>,
}

/// A session's answers to `action`.
fn answers_of(s: &Session, action: &str) -> Vec<serde_json::Value> {
    s.answers.iter().filter(|a| a["action"] == action).cloned().collect()
}

fn answers(lines: impl Iterator<Item = String>) -> Vec<serde_json::Value> {
    lines
        .filter(|l| l.starts_with('{'))
        .map(|l| serde_json::from_str(&l).expect("an answer"))
        .collect()
}

/// The session on the native host, frame by frame with the script's events.
fn native_session(
    subject: &Subject,
    script: &str,
    frames: u32,
    size: (u32, u32),
    reference: Vec<u8>,
) -> Session {
    let mut lens = subject.lens_with(Some(reference));
    let events = wrela_abi::input::parse_script(script).expect("the script");
    let mut lines = Vec::new();
    for i in 0..frames {
        for e in wrela_abi::input::events_at(&events, i) {
            lens.host.push_input(e);
        }
        lens.host.frame(i as f32 / 60.0, size.0, size.1).expect("a frame");
        lines.extend(lens.host.take_logs());
    }
    let frame = lens.host.read_screen().expect("the screen");
    Session { answers: answers(lines.into_iter()), frame, cpu: Vec::new(), gpu: Vec::new() }
}

/// The session in headless Chrome, its lens served by the studio server (which writes its
/// edits and answers its fit's reference), with more of test mode's parameters (`extra`, as
/// `&key=value`).
fn chrome_session_with(
    subject: &Subject,
    script: &str,
    frames: u32,
    size: (u32, u32),
    reference: Vec<u8>,
    extra: &str,
) -> Session {
    std::fs::write(subject.page.join("session.json"), script).expect("the script");
    let server = wrela_driver::serve::start(&subject.pkg, &subject.page, 0, false, Some(reference))
        .expect("the studio server");
    let url = format!(
        "http://127.0.0.1:{}/#test&frames={frames}&width={}&height={}&fps=60&timestamps=1&input=session.json{extra}",
        server.port, size.0, size.1
    );
    wrela_tests::run_url_in_chrome(&url, &subject.page, 600);
    drop(server);
    let results = subject.page.join("results");
    let log = std::fs::read_to_string(results.join("log.txt")).expect("log.txt");
    let times: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(results.join("frames.json")).expect("frames.json"),
    )
    .unwrap();
    Session {
        answers: answers(log.lines().map(String::from)),
        frame: std::fs::read(results.join("frame.rgba")).expect("frame.rgba"),
        cpu: times["cpu_ms"].as_array().unwrap().iter().map(|t| t.as_f64().unwrap()).collect(),
        gpu: wrela_tests::timings(&results)
            .into_iter()
            .map(|(f, l, ns)| (f, l, ns / 1e6))
            .collect(),
    }
}

/// Whether two answers agree: the same keys and texts, numbers within 10⁻⁴ (relative, or 10⁻⁵
/// absolute: the two hosts' GPUs compile shaders apart).
fn agree(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value::*;
    match (a, b) {
        (Number(x), Number(y)) => {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            (x - y).abs() <= 1e-5_f64.max(1e-4 * x.abs().max(y.abs()))
        }
        (Array(x), Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| agree(a, b)),
        (Object(x), Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| agree(v, w)))
        }
        _ => a == b,
    }
}

/// AC10: a scripted agent session (view, probe, click, a drag and its write, a fit and its
/// write, a diagnosis) gives the same results headless and in Chrome: the same answers, the same
/// files written, and last frames within a mean of 0.5/255. Chrome's times are reported: a
/// click's frame (AC5: ≤ 50 ms), a drag's updates (AC9: ≤ 33 ms with a 512² view).
#[test]
#[ignore = "measure: needs Chrome, python3 and a GPU"]
fn a_session_gives_the_same_results_headless_and_in_chrome() {
    const SIZE2: (u32, u32) = (512, 512);
    const RES: u32 = 256;
    let native = Subject::new("wolf", "session-native");
    let chrome = Subject::for_chrome("wolf", "session-chrome");
    // The fit's reference: the wolf with a longer neck and bigger ears (as the fit test's).
    let target = Subject::shared("wolf", "longer-neck", &LONGER_NECK);
    let framings = native.lens().act_with("view", &[Value::I32(0)]);
    let side = &framings["framings"][0];
    let (c, half) = (vec3(&side["centre"]), side["half"].as_f64().unwrap() as f32);
    // (Each native lens holds the GPU lock while it lives: none may live while Chrome runs.)
    let reference = {
        let mut t = target.lens();
        t.act_with(
            "silhouette",
            &[
                Value::I32(0),
                Value::F32(c[0]),
                Value::F32(c[1]),
                Value::F32(c[2]),
                Value::F32(half),
                Value::I32(RES as i32),
            ],
        );
        let rgba = t.host.read_screen().expect("the screen");
        let crop: Vec<u8> = (0..RES as usize)
            .flat_map(|y| rgba[y * SIZE.0 as usize * 4..][..RES as usize * 4].to_vec())
            .collect();
        wrela_driver::studio::reference_mask(RES, RES, &crop).expect("a mask")
    };
    let lits = [
        native.literal_on("pub const J_HEAD", 1),
        native.literal_on("pub const J_HEAD", 2),
        native.literal_on("const EAR_H", 0),
    ];
    let commands: Vec<String> = vec![
        "view sheet".into(),
        "probe 0 0.9 0.7".into(),
        "view side".into(),
        "mode parts".into(),
        "click 256 340".into(),
        "mode shaded".into(),
        "ray 0 0.87 2 0 0 -1".into(),
        "move 0 0.87 0.8133 0 0.87 0.8253".into(),
        "write".into(),
        "move 0 0.25 -0.69 0 0.23 -0.69".into(),
        "write".into(),
        "move 0.13 0.62 0.1 0.14 0.62 0.1".into(),
        "write".into(),
        format!(
            "fit 0 {} {} {} {half} {RES} {} {} {}",
            c[0], c[1], c[2], lits[0], lits[1], lits[2]
        ),
        "write".into(),
        "diagnose 0 0 0 0".into(),
        "view sheet".into(),
    ];
    let (script, typed, frames) = session_script(&commands);
    let a = native_session(&native, &script, frames, SIZE2, reference.clone());
    let b = chrome_session_with(&chrome, &script, frames, SIZE2, reference, "&nohash=1");
    // The answers.
    let names = |s: &Session| {
        s.answers
            .iter()
            .map(|a| a["action"].as_str().unwrap_or("?").to_string())
            .collect::<Vec<_>>()
    };
    eprintln!("native: {:?}\nchrome: {:?}", names(&a), names(&b));
    assert_eq!(names(&a), names(&b), "the same answers, in order");
    for (x, y) in a.answers.iter().zip(&b.answers) {
        assert!(agree(x, y), "the hosts disagree:\n  native {x}\n  chrome {y}");
    }
    // The files written.
    for ((pa, ta), (pb, _)) in native.files.iter().zip(&chrome.files) {
        let (na, nb) = (std::fs::read_to_string(pa).unwrap(), std::fs::read_to_string(pb).unwrap());
        assert_eq!(na, nb, "{} differs between the hosts' writes", pa.display());
        if pa.ends_with("creature.wrela") {
            assert_ne!(&na, ta, "the session wrote creature.wrela");
        }
    }
    // The last frames.
    let d = wrela_host::image::compare(&a.frame, &b.frame).expect("the same size");
    eprintln!("last frames: mean {:.3}/255, most {}", d.mean, d.max);
    assert!(d.mean <= 0.5, "the last frames differ by a mean of {}/255", d.mean);
    // Chrome's times: the click's frame, and each drag's updates (its steps' frames: the first
    // 8 frames after the move that drew the view again), CPU and GPU.
    let gpu_of = |f: usize| b.gpu.iter().filter(|g| g.0 == f).fold(0.0, |a, g| a + g.2);
    let click = typed[4] as usize;
    eprintln!("chrome: the click's frame {:.1} ms CPU, {:.1} ms GPU", b.cpu[click], gpu_of(click));
    let mut updates = Vec::new();
    for (k, c) in commands.iter().enumerate().filter(|(_, c)| c.starts_with("move")) {
        let typed_at = typed[k] as usize;
        let redraws: Vec<usize> = (typed_at + 1..typed_at + frames_for(c) as usize)
            .filter(|&f| b.gpu.iter().any(|g| g.0 == f && g.1 == "pass"))
            .take(8)
            .collect();
        updates.extend(redraws.iter().map(|&f| b.cpu[f] + gpu_of(f)));
    }
    let (update, most) = median_and_most(&updates);
    eprintln!(
        "chrome: {} drag updates (CPU + GPU, a 512² view): median {update:.1} ms, most {most:.1}: {updates:.1?}",
        updates.len()
    );
    assert!(
        updates.len() >= 20 && update <= 33.0,
        "drag updates: median {update} ms of {}",
        updates.len()
    );
    std::fs::write(
        chrome.page.join("results/times.json"),
        serde_json::json!({ "click_cpu_ms": b.cpu[click], "click_gpu_ms": gpu_of(click), "drag_updates_ms": updates })
            .to_string(),
    )
    .unwrap();
}

/// AC3, AC5 and AC8 in Chrome: a frame of the lens's UI at 1080p costs ≤ 1 ms of CPU and ≤ 0.5
/// ms of GPU (idle, and with the pointer over the panel); a contact sheet at 1024² renders in ≤
/// 100 ms (after an edit, the grid that guides its rays included, and after a change of mode);
/// a click query takes ≤ 50 ms (its frame's CPU time: the part, every literal that reaches the
/// field ranked, their source lines); Chrome's sheet and the native host's match within a mean
/// of 0.5/255; and a headless view takes ≤ 1 s after a build (loading the lens, its first
/// frame, and reading the screen, natively). Times are medians of 20.
#[test]
#[ignore = "measure: needs Chrome, python3 and a GPU"]
fn the_lens_is_fast_enough_and_the_same_in_both_hosts() {
    let subject = Subject::new("wolf", "speed");
    // The sheet's screen: 1024² of views beside the panel (400 wide).
    const SHEET: (u32, u32) = (1424, 1024);
    let views = |rgba: &[u8]| -> Vec<u8> {
        (0..SHEET.1 as usize)
            .flat_map(|y| rgba[y * SHEET.0 as usize * 4..][..1024 * 4].to_vec())
            .collect()
    };
    // A headless view after the build.
    let started = Instant::now();
    let sheet_native = {
        let mut host = Host::load(&subject.page).expect("the lens loads");
        host.frame(0.0, SHEET.0, SHEET.1).expect("a frame");
        host.read_screen().expect("the screen")
    };
    let headless = started.elapsed().as_secs_f64();
    // The sheet at 1024² in Chrome, drawn 20 times after an edit (a literal set, each time
    // to a new value: the grid that guides the rays is sampled again) and 20 times after a
    // change of mode alone; then drawn as the native host drew it, for the comparison.
    let lit = subject.literal_on("pub const J_HEAD", 1);
    let mut sheet_cmds: Vec<serde_json::Value> = Vec::new();
    for k in 0..20u32 {
        let f = 4 + 2 * k;
        sheet_cmds.push(serde_json::json!({ "frame": f, "type": "text", "text": format!("set {lit} {}", 0.912 + 0.001 * f64::from(k + 1)) }));
        sheet_cmds.push(serde_json::json!({ "frame": f, "type": "key", "key": "Enter" }));
    }
    for k in 0..20u32 {
        let f = 44 + 2 * k;
        let mode = if k % 2 == 0 { "parts" } else { "shaded" };
        sheet_cmds.push(
            serde_json::json!({ "frame": f, "type": "text", "text": format!("mode {mode}") }),
        );
        sheet_cmds.push(serde_json::json!({ "frame": f, "type": "key", "key": "Enter" }));
    }
    sheet_cmds.push(serde_json::json!({ "frame": 86, "type": "text", "text": "set 16 0.912" }));
    sheet_cmds.push(serde_json::json!({ "frame": 86, "type": "key", "key": "Enter" }));
    sheet_cmds.push(serde_json::json!({ "frame": 88, "type": "text", "text": "mode shaded" }));
    sheet_cmds.push(serde_json::json!({ "frame": 88, "type": "key", "key": "Enter" }));
    let sheet = chrome_session_with(
        &subject,
        &serde_json::Value::Array(sheet_cmds).to_string(),
        92,
        SHEET,
        Vec::new(),
        "&nohash=1",
    );
    let gpu_of =
        |s: &Session, f: usize| s.gpu.iter().filter(|g| g.0 == f).fold(0.0, |a, g| a + g.2);
    let (edited, _) =
        median_and_most(&(0..20).map(|k| gpu_of(&sheet, 4 + 2 * k)).collect::<Vec<_>>());
    let (moded, _) =
        median_and_most(&(0..20).map(|k| gpu_of(&sheet, 44 + 2 * k)).collect::<Vec<_>>());
    let sheet_ms = edited.max(moded);
    let d = wrela_host::image::compare(&views(&sheet_native), &views(&sheet.frame))
        .expect("the same size");
    // At 1080p: frames 20 to 39 idle, then 40 to 59 with the pointer moving over the panel each
    // frame (so the panel is laid out each frame).
    // (V8 runs WASM on its baseline compiler first and optimizes hot code later: the moving
    // frames' medians are of the last 50 of 200.)
    let moves: Vec<serde_json::Value> = (40..240)
        .map(|f| serde_json::json!({ "frame": f, "type": "move", "x": 1700 + (f % 7) * 9, "y": 300 + (f % 5) * 23 }))
        .collect();
    let ui = chrome_session_with(
        &subject,
        &serde_json::Value::Array(moves).to_string(),
        240,
        (1920, 1080),
        Vec::new(),
        "&nohash=1",
    );
    let idle: Vec<usize> = (20..40).collect();
    let (cpu_idle, _) = median_and_most(&idle.iter().map(|&f| ui.cpu[f]).collect::<Vec<_>>());
    let (gpu_idle, _) = median_and_most(&idle.iter().map(|&f| gpu_of(&ui, f)).collect::<Vec<_>>());
    let active: Vec<usize> = (190..240).collect();
    let (cpu, _) = median_and_most(&active.iter().map(|&f| ui.cpu[f]).collect::<Vec<_>>());
    let (gpu, _) = median_and_most(&active.iter().map(|&f| gpu_of(&ui, f)).collect::<Vec<_>>());
    eprintln!(
        "a headless view {headless:.2} s after the build; Chrome: a 1024² sheet {edited:.1} ms of GPU after an edit, {moded:.1} after a change of mode (medians of 20); \
         Chrome against native {:.3}/255 mean, {} most; a 1080p frame idle {cpu_idle:.2} ms CPU, {gpu_idle:.3} ms GPU, \
         with the pointer over the panel {cpu:.2} ms CPU, {gpu:.3} ms GPU (medians)",
        d.mean, d.max
    );
    // 20 clicks on the side view at 512², spread over the wolf's ribcage and flank.
    let mut clicks: Vec<serde_json::Value> = vec![
        serde_json::json!({ "frame": 2, "type": "text", "text": "view side" }),
        serde_json::json!({ "frame": 2, "type": "key", "key": "Enter" }),
    ];
    let at: Vec<(u32, u32)> = (0..20).map(|k| (210 + (k % 5) * 28, 330 + (k / 5) * 14)).collect();
    for (k, &(x, y)) in at.iter().enumerate() {
        let f = 6 + 4 * k as u32;
        clicks.push(
            serde_json::json!({ "frame": f, "type": "text", "text": format!("click {x} {y}") }),
        );
        clicks.push(serde_json::json!({ "frame": f, "type": "key", "key": "Enter" }));
    }
    let c = chrome_session_with(
        &subject,
        &serde_json::Value::Array(clicks).to_string(),
        88,
        (512, 512),
        Vec::new(),
        "&nohash=1",
    );
    let hits = answers_of(&c, "click").iter().filter(|a| a["hit"] == true).count();
    let (click, _) = median_and_most(&(0..20).map(|k| c.cpu[6 + 4 * k]).collect::<Vec<_>>());
    eprintln!("Chrome: a click query {click:.1} ms of CPU (median of 20, {hits} hit the wolf)");
    assert_eq!(hits, 20, "every click hits the wolf");
    assert!(click <= 50.0, "a click query took {click} ms");
    assert!(headless <= 1.0, "a headless view took {headless} s");
    assert!(sheet_ms <= 100.0, "a 1024² sheet took {sheet_ms} ms");
    assert!(d.mean <= 0.5, "Chrome's sheet differs from the native host's by {}/255", d.mean);
    assert!(
        cpu_idle <= 1.0 && gpu_idle <= 0.5,
        "an idle frame took {cpu_idle} ms CPU, {gpu_idle} ms GPU"
    );
    assert!(cpu <= 1.0 && gpu <= 0.5, "a frame with input took {cpu} ms CPU, {gpu} ms GPU");
}

// ---- AC8: the diagnostics against round 1's tool --------------------------------------------------

/// What round 1's tool (fieldview, design-archive-2026-10:
/// experiments/agent-authoring/fieldview, `report`) found in a grid of samples, its arithmetic
/// ported: the inside's bounds (cells' centres), its pieces (6-connected), and the largest
/// difference between neighbouring samples over a cell, where either is within 10 cm.
struct Fieldview {
    lo: [f32; 3],
    hi: [f32; 3],
    pieces: usize,
    largest_gradient: f32,
}

fn fieldview(d: &[f32], origin: [f32; 3], cell: f32, n: usize) -> Fieldview {
    let idx = |i: usize, j: usize, k: usize| i + n * (j + n * k);
    let pos = |i: usize, j: usize, k: usize| {
        [
            origin[0] + (i as f32 + 0.5) * cell,
            origin[1] + (j as f32 + 0.5) * cell,
            origin[2] + (k as f32 + 0.5) * cell,
        ]
    };
    let (mut lo, mut hi, mut gmax) = ([f32::MAX; 3], [f32::MIN; 3], 0f32);
    for k in 0..n {
        for j in 0..n {
            for i in 0..n {
                let v = d[idx(i, j, k)];
                if !v.is_finite() {
                    continue;
                }
                for (ni, nj, nk) in [(i + 1, j, k), (i, j + 1, k), (i, j, k + 1)] {
                    if ni < n && nj < n && nk < n {
                        let w = d[idx(ni, nj, nk)];
                        if w.is_finite() && (v.abs() < 0.1 || w.abs() < 0.1) {
                            gmax = gmax.max((w - v).abs() / cell);
                        }
                    }
                }
                if v < 0.0 {
                    let p = pos(i, j, k);
                    for t in 0..3 {
                        lo[t] = lo[t].min(p[t]);
                        hi[t] = hi[t].max(p[t]);
                    }
                }
            }
        }
    }
    let mut label = vec![u32::MAX; n * n * n];
    let mut pieces = 0;
    for start in 0..n * n * n {
        // (Not `>= 0.0`: a NaN sample is outside, as fieldview has it.)
        if d[start].partial_cmp(&0.0) != Some(std::cmp::Ordering::Less) || label[start] != u32::MAX
        {
            continue;
        }
        let id = pieces as u32;
        pieces += 1;
        let mut queue = std::collections::VecDeque::from([start]);
        label[start] = id;
        while let Some(c) = queue.pop_front() {
            let (i, j, k) = (c % n, (c / n) % n, c / (n * n));
            let mut push = |e: usize| {
                if d[e] < 0.0 && label[e] == u32::MAX {
                    label[e] = id;
                    queue.push_back(e);
                }
            };
            if i > 0 {
                push(idx(i - 1, j, k));
            }
            if i + 1 < n {
                push(idx(i + 1, j, k));
            }
            if j > 0 {
                push(idx(i, j - 1, k));
            }
            if j + 1 < n {
                push(idx(i, j + 1, k));
            }
            if k > 0 {
                push(idx(i, j, k - 1));
            }
            if k + 1 < n {
                push(idx(i, j, k + 1));
            }
        }
    }
    Fieldview { lo, hi, pieces, largest_gradient: gmax }
}

/// AC8: on a ported subject, the lens's diagnostics agree with round 1's tool on its original
/// (the WGSL, compiler/tests/fixtures/round1, sampled on the GPU at the lens's grid): the same
/// piece count, the bounds within 1 mm, the largest gradient within 1%.
fn diagnostics_agree_with_fieldview(name: &'static str) {
    let subject = Subject::shared(name, "as-is", &[]);
    let ours = subject.lens().act("diagnose", &[0.0, 0.0, 0.0, 0.0]);
    let origin = vec3(&ours["grid_origin"]);
    let cell = ours["cell"].as_f64().unwrap() as f32;
    let n = ours["cells"].as_u64().unwrap() as usize;
    let mut points = Vec::with_capacity(n * n * n);
    for k in 0..n {
        for j in 0..n {
            for i in 0..n {
                points.push([
                    origin[0] + (i as f32 + 0.5) * cell,
                    origin[1] + (j as f32 + 0.5) * cell,
                    origin[2] + (k as f32 + 0.5) * cell,
                ]);
            }
        }
    }
    let theirs = fieldview(&super::subjects::originals(name, &points), origin, cell, n);
    let (lo, hi) = (vec3(&ours["lo"]), vec3(&ours["hi"]));
    let bounds_gap = (0..3)
        .map(|t| (lo[t] - theirs.lo[t]).abs().max((hi[t] - theirs.hi[t]).abs()))
        .fold(0f32, f32::max);
    let g = ours["largest_gradient"].as_f64().unwrap() as f32;
    let g_gap = (g - theirs.largest_gradient).abs() / theirs.largest_gradient;
    eprintln!(
        "{name}: pieces {} (fieldview on the original {}), bounds within {:.3} mm, largest gradient {g:.4} ({:.4}: {:.3}%)",
        ours["pieces"],
        theirs.pieces,
        bounds_gap * 1000.0,
        theirs.largest_gradient,
        g_gap * 100.0
    );
    assert_eq!(ours["pieces"].as_u64(), Some(theirs.pieces as u64), "{name}: the pieces");
    assert!(bounds_gap <= 0.001, "{name}: the bounds differ by {bounds_gap} m");
    assert!(g_gap <= 0.01, "{name}: the largest gradient differs by {}%", g_gap * 100.0);
}

#[test]
#[ignore = "long: needs a GPU"]
fn the_wolfs_diagnostics_agree_with_round_1s_tool() {
    diagnostics_agree_with_fieldview("wolf");
}

#[test]
#[ignore = "long: needs a GPU"]
fn the_grazers_diagnostics_agree_with_round_1s_tool() {
    diagnostics_agree_with_fieldview("grazer");
}

/// `describe` (the owner's additions to #39): one answer says what a subject is. It agrees with
/// the diagnosis on the same grid (box, volume, pieces); its parts' volumes add up to the whole's;
/// each contact is reported by both parts, with the same area; a mirrored part and its image have
/// nearly the same volume; and the asymmetry is the fur's alone (its noise isn't mirrored: under
/// 0.75 of a cell).
fn describe_agrees_with_the_diagnosis(subject: &Subject) -> serde_json::Value {
    let name = subject.name;
    let mut lens = subject.lens();
    let whole = lens.act("diagnose", &[0.0, 0.0, 0.0, 0.0]);
    let a = lens.act("describe", &[]);
    let parts = a["parts"].as_array().expect("parts");
    let total: f64 = parts.iter().map(|p| p["volume"].as_f64().unwrap()).sum();
    let volume = a["volume"].as_f64().unwrap();
    let cell = a["cell"].as_f64().unwrap();
    eprintln!(
        "{name}: {} parts, volume {volume:.6} m^3 (its parts' {total:.6}), mass {} kg, pieces {}, asymmetry {} mm (cell {:.1} mm)",
        parts.len(),
        a["mass"],
        a["pieces"],
        a["asymmetry_mm"],
        cell * 1000.0
    );
    assert_eq!(a["cells"], whole["cells"]);
    assert_eq!(a["pieces"], whole["pieces"], "{a}");
    assert_eq!(a["mass"], whole["mass"]);
    let (lo, hi, wlo, whi) =
        (vec3(&a["lo"]), vec3(&a["hi"]), vec3(&whole["lo"]), vec3(&whole["hi"]));
    for t in 0..3 {
        // The diagnosis gives the inside cells' centres; the description, their faces.
        assert!(
            (lo[t] - wlo[t]).abs() <= cell as f32 && (hi[t] - whi[t]).abs() <= cell as f32,
            "{a}\n{whole}"
        );
    }
    assert!(
        parts.len() >= 5 && parts.iter().all(|p| p["name"].as_str().is_some_and(|n| !n.is_empty()))
    );
    assert!((total - volume).abs() <= 1e-6 * volume, "parts {total}, whole {volume}");
    for p in parts {
        let i = p["index"].as_u64().unwrap();
        for t in p["touches"].as_array().unwrap() {
            let other = &parts[t["part"].as_u64().unwrap() as usize];
            let back =
                other["touches"].as_array().unwrap().iter().find(|b| b["part"].as_u64() == Some(i));
            assert_eq!(
                back.map(|b| &b["area_cm2"]),
                Some(&t["area_cm2"]),
                "{} and {}",
                p["name"],
                other["name"]
            );
        }
        if p["mirrored"] == true {
            let v = p["volume"].as_f64().unwrap();
            let image = parts
                .iter()
                .filter(|q| {
                    q["name"] == p["name"] && q["group"] == p["group"] && q["mirrored"] == false
                })
                .map(|q| q["volume"].as_f64().unwrap())
                .min_by(|x, y| (x - v).abs().total_cmp(&(y - v).abs()));
            let w = image.unwrap_or_else(|| panic!("{}: no image", p["name"]));
            assert!(
                (v - w).abs() <= (0.2 * v.max(w)).max(2.0 * cell.powi(3)),
                "{}: {v} and {w}",
                p["name"]
            );
        }
    }
    a
}

#[test]
#[ignore = "long: needs a GPU"]
fn the_wolfs_description_agrees_with_its_diagnosis() {
    let a = describe_agrees_with_the_diagnosis(&Subject::shared("wolf", "as-is", &[]));
    let fur = a["asymmetry_mm"].as_f64().unwrap();
    assert!(fur < 0.75 * a["cell"].as_f64().unwrap() * 1000.0, "{fur} mm");
    // Moved 10 mm to its left, the wolf's flanks are 20 mm from their mirror images: the
    // asymmetry is that, give or take the fur's (and 2 mm, for the points sampled).
    let moved = Subject::edited(
        "wolf",
        "describe-moved",
        &[(
            "Lipschitz {\n    wolf().field()",
            "Lipschitz {\n    wolf().field().translate(vec3(0.010, 0.0, 0.0))",
        )],
    );
    let b = describe_agrees_with_the_diagnosis(&moved);
    let lopsided = b["asymmetry_mm"].as_f64().unwrap();
    assert!(
        (lopsided - 20.0).abs() <= fur + 2.0 && b["mirror_symmetric"] == false,
        "{lopsided} mm"
    );
}

#[test]
#[ignore = "long: needs a GPU"]
fn the_grazers_description_agrees_with_its_diagnosis() {
    let a = describe_agrees_with_the_diagnosis(&Subject::shared("grazer", "as-is", &[]));
    let fur = a["asymmetry_mm"].as_f64().unwrap();
    assert!(fur < 0.75 * a["cell"].as_f64().unwrap() * 1000.0, "{fur} mm");
}

// ---- AC10: the server ------------------------------------------------------------------------------

/// AC10: an edit saved by another tool (a literal in the subject's file) shows in the lens open
/// in Chrome within 2 s: the server sees the file change (it looks every 200 ms), the page asks
/// what's new (every 250 ms) and types `set` into the lens, and the lens draws its views again.
/// Timed from the file's write to the start of the frame that set the literal.
#[test]
#[ignore = "measure: needs Chrome, python3 and a GPU"]
fn an_edit_by_another_tool_shows_in_the_open_lens_within_2_s() {
    let subject = Subject::for_chrome("wolf", "external-edit");
    let file = subject.files.iter().find(|(p, _)| p.ends_with("creature.wrela")).unwrap().0.clone();
    let lit = subject.literal_on("const EAR_H", 0);
    let edited_at = std::sync::Arc::new(std::sync::Mutex::new(0f64));
    let (path, at) = (file.clone(), std::sync::Arc::clone(&edited_at));
    let editor = std::thread::spawn(move || {
        // Long enough for Chrome to start and the page to run.
        std::thread::sleep(std::time::Duration::from_secs(5));
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("const EAR_H: f32 = 0.086", "const EAR_H: f32 = 0.120"))
            .unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
        *at.lock().unwrap() = now.as_secs_f64() * 1000.0;
    });
    let s = chrome_session_with(&subject, "[]", 720, (512, 512), Vec::new(), "&nohash=1");
    editor.join().unwrap();
    subject.restore();
    let edited = *edited_at.lock().unwrap();
    let results = subject.page.join("results");
    let frames: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(results.join("frames.json")).unwrap())
            .unwrap();
    let log = std::fs::read_to_string(results.join("log.txt")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    let k = lines
        .iter()
        .position(|l| {
            serde_json::from_str::<serde_json::Value>(l).is_ok_and(|a| {
                a["action"] == "set"
                    && a["literal"] == lit
                    && a["value"].as_f64().map(|v| v as f32) == Some(0.120)
            })
        })
        .unwrap_or_else(|| panic!("the lens never set EAR_H to 0.120: {lines:?}"));
    let frame = frames["printed_in"][k].as_u64().unwrap() as usize;
    let began = frames["began_ms"][frame].as_f64().unwrap();
    let redrawn = s.gpu.iter().any(|g| (g.0 == frame || g.0 == frame + 1) && g.1 == "pass");
    eprintln!(
        "the edit showed {:.0} ms after it was saved (frame {frame}); the views drawn again: {redrawn}",
        began - edited
    );
    assert!(began - edited <= 2000.0, "the edit took {} ms to show", began - edited);
    assert!(redrawn, "the lens didn't draw its views again");
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

// ---- AC4 on the subjects: lifted against normal builds, by the lens's contact sheets ---------------

/// The share of pixels (RGBA8) any of whose channels differ by more than 8.
fn off_by_more_than_8(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len() / 4;
    let off = a
        .chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(p, q)| p.iter().zip(*q).any(|(x, y)| x.abs_diff(*y) > 8))
        .count();
    off as f64 / n as f64
}

/// The lens on `subject` built normally (its literals in the code, not a table), into
/// `<scratch>/<tag>`: the lens's own program, which `wrela studio` wrote beside the subject.
fn normal_lens(subject: &Subject, tag: &str) -> PathBuf {
    let dir = super::scratch(&format!("studio-{}-{tag}", subject.name));
    wrela_tests::must_build(&wrela_driver::studio::dir(&subject.pkg).join("lens"), &dir);
    dir
}

/// AC4 on a subject: the lifted lens's contact sheet (1024² of views) matches the normal build's
/// within a mean of 0.5/255 with at most 0.1% of pixels off by more than 8/255; drawing it costs
/// at most 1.6 times as much (GPU time, median of 20 draws); and a literal set in the lifted
/// lens shows in the next frame, as the normal build of the source with that value draws it.
/// (The literal is inside the subject's outline: one that moved the subject's bounds would frame
/// the edited source's views apart from the lifted lens's, whose framing is its opening's.)
fn lifted_sheets_match(
    name: &'static str,
    pattern: &str,
    k: usize,
    value: f32,
    edit: (&str, &str),
) {
    const SCREEN: (u32, u32) = (1424, 1024);
    let views = |rgba: &[u8]| -> Vec<u8> {
        (0..SCREEN.1 as usize)
            .flat_map(|y| rgba[y * SCREEN.0 as usize * 4..][..1024 * 4].to_vec())
            .collect()
    };
    let subject = Subject::shared(name, "as-is", &[]);
    let normal = normal_lens(&subject, "ac4-normal");
    // The sheet, and the GPU time of 20 draws of it (a change of mode each).
    let draw = |dir: &Path| -> (Vec<u8>, f64) {
        let options = wrela_host::Options { timestamps: true, ..Default::default() };
        let mut host = Host::load_with(dir, &options).expect("the lens loads");
        host.frame(0.0, SCREEN.0, SCREEN.1).expect("a frame");
        let sheet = host.read_screen().expect("the screen");
        let _ = host.take_timings();
        let mut times = Vec::new();
        for k in 1..=20 {
            host.call_export("mode", &[Value::I32(k % 2), Value::I32(0)]).expect("mode");
            host.frame(k as f32 / 60.0, SCREEN.0, SCREEN.1).expect("a frame");
            times.push(
                host.take_timings()
                    .unwrap()
                    .iter()
                    .filter(|t| t.label != "screen pass")
                    .map(|t| t.nanos / 1e6)
                    .sum(),
            );
        }
        (views(&sheet), median_and_most(&times).0)
    };
    let (lifted_sheet, lifted_ms) = draw(&subject.page);
    let (normal_sheet, normal_ms) = draw(&normal);
    let d = wrela_host::image::compare(&lifted_sheet, &normal_sheet).unwrap();
    let off = off_by_more_than_8(&lifted_sheet, &normal_sheet);
    // A literal set in the lifted lens, against the normal build of the edited source.
    let lit = subject.literal_on(pattern, k);
    let set_sheet = {
        let mut host = Host::load(&subject.page).expect("the lens loads");
        host.frame(0.0, SCREEN.0, SCREEN.1).expect("a frame");
        host.call_export("set", &[Value::I32(lit as i32), Value::F32(value)]).expect("set");
        host.frame(1.0 / 60.0, SCREEN.0, SCREEN.1).expect("the next frame");
        views(&host.read_screen().expect("the screen"))
    };
    let edited = Subject::edited(name, "ac4-edited", &[edit]);
    let edited_normal = normal_lens(&edited, "ac4-edited-normal");
    let framings = |s: &Subject| s.lens().act_with("view", &[Value::I32(4)])["framings"].clone();
    assert_eq!(
        framings(&subject),
        framings(&edited),
        "{name}: the edit moves the framing: choose another literal"
    );
    let edited_sheet = {
        let mut host = Host::load(&edited_normal).expect("the lens loads");
        host.frame(0.0, SCREEN.0, SCREEN.1).expect("a frame");
        views(&host.read_screen().expect("the screen"))
    };
    let e = wrela_host::image::compare(&set_sheet, &edited_sheet).unwrap();
    if std::env::var("WRELA_SAVE_SHEETS").is_ok() {
        let out = super::scratch(&format!("studio-{name}-ac4-sheets"));
        for (f, img) in
            [("set.png", &set_sheet), ("edited.png", &edited_sheet), ("lifted.png", &lifted_sheet)]
        {
            wrela_host::image::write_png(&out.join(f), 1024, 1024, img).unwrap();
        }
        eprintln!("sheets in {}", out.display());
    }
    let e_off = off_by_more_than_8(&set_sheet, &edited_sheet);
    let moved = wrela_host::image::compare(&set_sheet, &lifted_sheet).unwrap();
    eprintln!(
        "{name}: lifted against normal {:.3}/255 mean, {:.4}% off by more than 8; drawing {lifted_ms:.1} ms against {normal_ms:.1} ({:.2}x); \
         after a literal set the next frame against the edited source's {:.3}/255, {:.4}% (it changed {} channels)",
        d.mean,
        off * 100.0,
        lifted_ms / normal_ms,
        e.mean,
        e_off * 100.0,
        moved.differing
    );
    assert!(d.mean <= 0.5 && off <= 0.001, "{name}: lifted and normal sheets differ");
    assert!(
        lifted_ms <= 1.6 * normal_ms,
        "{name}: lifted drawing costs {}x",
        lifted_ms / normal_ms
    );
    assert!(moved.differing > 0, "{name}: the set didn't show in the next frame");
    assert!(
        e.mean <= 0.5 && e_off <= 0.001,
        "{name}: the set's frame differs from the edited source's"
    );
}

#[test]
#[ignore = "long: needs a GPU"]
fn the_wolfs_lifted_sheets_match_the_normal_builds() {
    // The cranium 1 cm further back in the head.
    lifted_sheets_match(
        "wolf",
        "ell(vec3(0.064, 0.058, 0.072)).translate(vec3(0.0, 0.010, -0.025))",
        5,
        -0.035,
        (
            "ell(vec3(0.064, 0.058, 0.072)).translate(vec3(0.0, 0.010, -0.025))",
            "ell(vec3(0.064, 0.058, 0.072)).translate(vec3(0.0, 0.010, -0.035))",
        ),
    );
}

#[test]
#[ignore = "long: needs a GPU"]
fn the_grazers_lifted_sheets_match_the_normal_builds() {
    // The brow 1 cm higher.
    lifted_sheets_match(
        "grazer",
        "ell(vec3(0.032, 0.018, 0.05)).translate(vec3(0.08, 0.066, 0.18))",
        4,
        0.076,
        (
            "ell(vec3(0.032, 0.018, 0.05)).translate(vec3(0.08, 0.066, 0.18))",
            "ell(vec3(0.032, 0.018, 0.05)).translate(vec3(0.08, 0.076, 0.18))",
        ),
    );
}

/// A person's drag: the pointer goes down on the wolf's back in the side view, just below its
/// outline (where the surface faces up more than toward the viewer: a drag stays on the plane
/// facing the view, so elsewhere it's along the surface, which spike 09 found ill-posed), moves
/// 5 pixels up (past the 4 that make a press a drag) and back to 3 (12 mm), over 10 frames, and
/// comes up; the drag finishes (rounded, checked whole, measured), and the back has moved there.
#[test]
#[ignore = "long: needs a GPU"]
fn a_pointer_drag_moves_the_surface_under_it() {
    let subject = Subject::shared("wolf", "as-is", &[]);
    let mut lens = subject.lens();
    lens.act_with("view", &[Value::I32(0)]);
    let projected = lens.act("project", &[0.5, 0.74, -0.2]);
    let v = &projected["views"][0];
    let (x, y) = (v["x"].as_f64().unwrap() as f32, v["y"].as_f64().unwrap() as f32);
    use wrela_abi::input::{Event, EventKind};
    let primary = 0;
    lens.host.push_input(Event::pointer(EventKind::PointerDown, x, y, primary, 0));
    lens.host.frame(0.1, SIZE.0, SIZE.1).unwrap();
    for k in 1..=10 {
        // Forward is to the left in the side view.
        let dy = if k == 1 { 5.0 } else { 3.0 };
        lens.host.push_input(Event::pointer(EventKind::PointerMove, x, y - dy, 0, 0));
        lens.host.frame(0.1 + k as f32 / 60.0, SIZE.0, SIZE.1).unwrap();
    }
    lens.host.push_input(Event::pointer(EventKind::PointerUp, x, y - 3.0, primary, 0));
    let moved = lens.act("busy", &[]);
    eprintln!("{moved}");
    assert_eq!(moved["action"], "move", "the drag answered");
    assert!(moved["changed"].as_u64().unwrap() > 0, "it changed literals");
    assert!(
        moved["error_mm"].as_f64().unwrap() < 1.0,
        "the surface reached the pointer's point: {moved}"
    );
    let from = vec3(&moved["from"]);
    let to = vec3(&moved["to"]);
    assert!(to[1] - from[1] > 0.01, "the target moved up with the pointer: {from:?} -> {to:?}");
    assert!(moved["others_most_mm"].as_f64().unwrap() <= 2.0, "other parts stayed: {moved}");
}

/// A ball 30 cm across on the ground, its spec (one check holds, one misses) and its blueprint
/// (from the side, a circle 2 cm wider all round; from the front, its own circle).
const BALL: [(&str, &str); 3] = [
    (
        "subject.wrela",
        "use std::field::{Lipschitz, sphere}

pub fn subject() -> Lipschitz {
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
