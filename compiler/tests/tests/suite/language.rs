//! AC10: every example compiles. Each ```wrela block of docs/language.md, and each one in a doc
//! comment of std, is checked; std's are also run, and §19's tier-0 program is built.
//!
//! Most of the spec's blocks are fragments: they name a `world` or a `grazer` they don't
//! declare. A block gets the context it names, as rustdoc's hidden lines give one: the items
//! it uses, and the parameters and statements its statements see. Its own top-level items stay
//! items, and its statements go in a function after the context's. A block whose comments say
//! `// error: <message>` must fail with exactly those errors, in that wording, and a block with
//! `@test`s must pass them.

use crate::{package, scratch, with_frame};
use wrela_tests::{build, repo_root};

/// What a block needs around it, named by the block's first line.
struct Context {
    first: &'static str,
    /// Items the block uses.
    items: &'static str,
    /// The parameters of the function its statements go in.
    params: &'static str,
    /// Statements before the block's.
    prelude: &'static str,
    /// Other files of the package: (path, text).
    files: &'static [(&'static str, &'static str)],
}

const NONE: Context = Context { first: "", items: "", params: "", prelude: "", files: &[] };

/// The herd the spec's examples are about.
const HERD: &str = "
use std::units::m

pub struct Root: Copy {
    pub pos: vec3,
}

pub struct GrazerSim: Clone {
    pub pos: vec3,
    pub age: f32,
    pub root: Root,
}

pub enum Edit: Copy {
    Dig { at: vec3, radius: f32 },
    Fill { at: vec3, radius: f32 },
}

pub struct Terrain: Clone {
    pub edits: Vec<Edit>,
}

pub struct World: Clone {
    pub grazers: Arena<GrazerSim>,
    pub terrain: Terrain,
}

pub struct Intent: Copy {
    pub speed: f32,
}
";

const CONTEXTS: &[Context] = &[
    Context {
        first: "let r = ellipsoid(radii: vec3(0.45m, 0.50m, 0.90m))",
        items: "use std::field::{Sphere, ellipsoid, fbm}\nuse std::units::m",
        params: "haunch: Sphere, base: f32, extra: f32",
        ..NONE
    },
    Context {
        first: "fn leg_segment(len: f32, r_top: f32, r_bottom: f32 = 0.06) -> Surface {",
        items: "use std::field::{Surface, round_cone}",
        ..NONE
    },
    Context { first: "pub struct Look {", ..NONE },
    Context { first: "pub struct Game {", ..NONE },
    Context { first: "pub enum Edit: Copy {", ..NONE },
    Context { first: "@fieldwise", items: "use std::hash::{Hasher, canonical_bits}", ..NONE },
    Context {
        first: "const HIDE_DENSITY = 1050 * kg/m**3                       // 1050.0, in kg/m³ (§5)",
        items: "
use std::units::{kg, m}

pub struct GaitNet {
    pub weights: Bytes,
}

pub struct Hoof: Copy {
    pub mass: f32,
}

fn hoof() -> Hoof {
    Hoof { mass: 0.4 }
}

/// A stand-in for the solve: the hoof's lowest modes, in hertz.
fn modal_modes(h: Hoof) -> [f32; 2] {
    [120.0 / h.mass, 310.0 / h.mass]
}

pub struct Skeleton: Copy {
    pub bones: u32,
}

pub struct Bone: Copy {
    pub index: u32,
}

impl Skeleton {
    pub fn bone(self, name: str) -> Bone {
        if name == \"root\" {
            return Bone { index: 0 }
        }
        if name == \"chest\" {
            return Bone { index: 1 }
        }
        panic(\"no bone has that name\")
    }
}

fn grazer_skeleton() -> Skeleton {
    Skeleton { bones: 2 }
}
",
        files: &[("grazer_gait.wnn", "weights")],
        ..NONE
    },
    Context {
        first: "use shapes::blob::blob      // from shapes/blob.wrela",
        files: &[(
            "shapes/blob.wrela",
            "use std::field::{Surface, sphere}\n\npub fn blob(r: f32) -> Surface {\n    sphere(r)\n}\n",
        )],
        ..NONE
    },
    Context {
        first: "let density = 1050 * kg/m**3      // 1050.0: kg/m³ by the SI convention",
        items: "use std::units::{kg, m}",
        ..NONE
    },
    Context {
        first: "let a = vec3(1m, 2m, 3m)",
        items: "
pub struct Edit: Copy {
    pub at: vec3,
}

pub struct EditLog: Clone {
    pub edits: Vec<Edit>,
}

impl EditLog {
    pub fn new() -> EditLog {
        EditLog { edits: Vec::new() }
    }

    pub fn push(mut self, e: Edit) {
        self.edits.push(e)
    }
}

fn archive(log: take EditLog) {}
",
        params: "edit: Edit",
        ..NONE
    },
    Context {
        first: "borrow g = world.grazers[h]    // read-only projection: no copy",
        items: HERD,
        params: "world: mut World, h: Handle<GrazerSim>, at: vec3",
        ..NONE
    },
    Context {
        first: "fn grazer(world: mut World, h: Handle<GrazerSim>) -> mut GrazerSim {",
        items: HERD,
        params: "world: mut World, h: Handle<GrazerSim>",
        ..NONE
    },
    Context {
        first: "for mut g in world.grazers {",
        items: concat!(
            "use std::units::m\n",
            "pub struct GrazerSim: Clone {\n    pub pos: vec3,\n}\n",
            "pub struct World: Clone {\n    pub grazers: Arena<GrazerSim>,\n}\n",
            "pub struct Intent: Copy {\n    pub speed: f32,\n}\n",
            "fn step(g: mut GrazerSim, world: World, intent: Intent) {}\n",
        ),
        params: "world: mut World, intent: Intent, h1: Handle<GrazerSim>, h2: Handle<GrazerSim>",
        ..NONE
    },
    Context {
        first: "borrow struct DrawCtx {",
        items: "
pub struct World: Clone {
    pub time: f32,
}

pub struct Assets: Clone {
    pub count: u32,
}

pub struct Frame: Clone {
    pub drawn: u32,
}

fn draw_herd(ctx: DrawCtx) {
    ctx.frame.drawn += ctx.assets.count
}
",
        params: "world: World, assets: Assets, frame: mut Frame, dt: f32",
        ..NONE
    },
    Context {
        first: "for mut g in world.grazers { g.age += dt }             // an arena",
        items: concat!(
            "use std::units::m\n",
            "pub struct GrazerSim: Clone {\n    pub pos: vec3,\n    pub age: f32,\n}\n",
            "pub struct World: Clone {\n    pub grazers: Arena<GrazerSim>,\n}\n",
            "pub struct LegPose: Copy {\n    pub lift: f32,\n}\n",
            "pub struct Pose: Copy {\n    pub legs: [LegPose; 4],\n}\n",
            "
pub struct SpatialGrid: Clone {
    pub near: Vec<Handle<GrazerSim>>,
}

impl SpatialGrid {
    pub fn each_within(self, p: vec3, r: f32, f: fn(Handle<GrazerSim>)) {
        for i in 0..self.near.len() {
            f(self.near[i])
        }
    }

    pub fn within(self, p: vec3, r: f32) -> Vec<Handle<GrazerSim>> {
        self.near.clone()
    }
}
",
        ),
        params: "world: mut World, pose: Pose, grid: SpatialGrid, p: vec3, dt: f32",
        prelude: "var lift = 0.0\nvar count = 0",
        ..NONE
    },
    Context {
        first: "world.grazers.par_each_mut(|g| step(mut g, world.terrain, intent))   // captures a projection: passed down only",
        items: concat!(
            "use std::field::{Surface, sphere}\n",
            "pub struct GrazerSim: Clone {\n    pub pos: vec3,\n}\n",
            "pub struct Terrain: Clone {\n    pub height: f32,\n}\n",
            "pub struct World: Clone {\n    pub grazers: Arena<GrazerSim>,\n    pub terrain: Terrain,\n}\n",
            "pub struct Intent: Copy {\n    pub speed: f32,\n}\n",
            "fn step(g: mut GrazerSim, terrain: Terrain, intent: Intent) {\n",
            "    g.pos.y = terrain.height\n}\n",
            "pub struct Tissue: Copy + GpuData {\n    pub albedo: vec3,\n    pub roughness: f32,\n}\n",
            "pub const HIDE: Tissue = Tissue { albedo: vec3(0.4), roughness: 0.8 }\n",
            "fn dapple(p: vec3, seed: f32) -> vec3 {\n    vec3(0.5 + 0.5 * sin(p.x * 40.0 + seed))\n}\n",
        ),
        params: "world: mut World, intent: Intent, seed: f32",
        prelude: "let torso = sphere(0.4)",
        ..NONE
    },
    Context {
        first: "/// Generic over any field; monomorphized per concrete field type.",
        items: "
use std::derive::Box3
use std::field::Surface
use std::gpu::{GlobalId, Slots}

pub struct Block: Copy + GpuData {
    pub lo: vec3,
    pub hi: vec3,
}

pub struct Grid: Copy + GpuData {
    pub origin: vec3,
    pub cell: f32,
    pub n: u32,
}

impl Grid {
    pub fn block(self, i: u32) -> Block {
        let c = vec3(f32(i % self.n), f32((i / self.n) % self.n), f32(i / (self.n * self.n)))
        let lo = self.origin + c * self.cell
        Block { lo, hi: lo + vec3(self.cell) }
    }
}
",
        ..NONE
    },
    Context {
        first: "@fragment",
        items: "
use std::field::Surface
use std::gpu::FragCoord

pub struct Scene: Copy + GpuData {
    pub eye: vec3,
    pub sun: vec3,
}

impl Scene {
    /// Where the ray through `xy` first meets `field`'s surface.
    pub fn hit<F: Surface>(self, xy: vec2, field: F) -> vec3 {
        let dir = normalize(vec3(xy * 0.002 - 1.0, 1.0))
        var t = 0.0
        for i in 0..64 {
            t += field.distance(self.eye + dir * t)
        }
        self.eye + dir * t
    }

    pub fn light(self, n: vec3) -> vec3 {
        vec3(max(dot(n, self.sun), 0.0))
    }
}
",
        ..NONE
    },
    Context { first: "use std::derive::{Interval, interval}", ..NONE },
    Context { first: "use std::field::{Surface, sphere}", ..NONE },
    Context {
        first: "pub trait Lipschitz: Surface {",
        items: "use std::field::{Displace, Noise, Surface}",
        ..NONE
    },
    Context { first: "use std::field::{Surface, round_cone, sphere}", ..NONE },
    Context {
        first: "pub struct Herd: Clone {",
        items: "pub struct GrazerSim: Clone {\n    pub pos: vec3,\n}\n",
        ..NONE
    },
    Context {
        first: "struct Ctx<'a> { world: &'a World }",
        items: "pub struct World: Clone {\n    pub time: f32,\n}\n",
        ..NONE
    },
    Context {
        first: "fn keep(g: take GrazerSim) {}",
        items: HERD,
        params: "world: mut World, h: Handle<GrazerSim>",
        ..NONE
    },
    Context {
        first: "let f = blob.with_at(|p| p.y - world.terrain.height(p))",
        items: "
use std::field::{Sphere, Surface}

pub struct Terrain: Clone {
    pub base: f32,
}

impl Terrain {
    pub fn height(self, p: vec3) -> f32 {
        self.base
    }
}

pub struct World: Clone {
    pub terrain: Terrain,
}
",
        params: "world: World, blob: Sphere",
        ..NONE
    },
];

/// A ```wrela block: the line it starts on and its text, dedented.
struct Block {
    at: String,
    code: String,
}

/// The ```wrela blocks of markdown `text` (`file` names it), indented ones in lists included.
fn markdown_blocks(file: &str, text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    // The open fence: its line, its indent and whether it's `wrela`.
    let mut open: Option<(usize, usize, bool)> = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let Some(info) = t.strip_prefix("```") else { continue };
        match open.take() {
            None => open = Some((i, line.len() - t.len(), info.trim() == "wrela")),
            Some((start, indent, true)) => out.push(Block {
                at: format!("{file}:{}", start + 1),
                code: lines[start + 1..i]
                    .iter()
                    .map(|l| format!("{}\n", l.get(indent..).unwrap_or("")))
                    .collect(),
            }),
            Some(_) => {}
        }
    }
    out
}

/// The ```wrela blocks in std's doc comments.
fn std_blocks() -> Vec<Block> {
    let mut out = Vec::new();
    for (module, text) in wrela_driver::STD_SOURCES {
        // A doc comment's text, line for line (other lines empty), is markdown.
        let docs: String = text
            .lines()
            .map(|l| match l.trim_start().strip_prefix("///") {
                Some(d) => format!("{}\n", d.strip_prefix(' ').unwrap_or(d)),
                None => "\n".into(),
            })
            .collect();
        out.extend(markdown_blocks(module, &docs));
    }
    out
}

/// Whether a top-level line starts an item.
fn starts_item(line: &str) -> bool {
    [
        "fn ",
        "pub ",
        "struct ",
        "enum ",
        "trait ",
        "impl",
        "const ",
        "use ",
        "type ",
        "borrow struct ",
    ]
    .iter()
    .any(|k| line.starts_with(k))
}

/// The block split into its items and its statements, each with the other's lines left
/// empty, so both keep the block's line numbers. A top-level line starts a part; attributes
/// and doc comments go with the line after them, and indented lines and lines that close or
/// continue (`}`, `)`, `.`) stay in the part they're in.
fn split(code: &str) -> (String, String) {
    let lines: Vec<&str> = code.lines().collect();
    let mut item = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        let mut head = i;
        while head < lines.len() && (lines[head].starts_with('@') || lines[head].starts_with("///"))
        {
            head += 1;
        }
        let is_item = head < lines.len() && starts_item(lines[head]);
        let mut end = head + 1;
        while end < lines.len() {
            let l = lines[end];
            let top = !l.is_empty() && !l.starts_with([' ', '}', ')', ']', '.']);
            if top {
                break;
            }
            end += 1;
        }
        for k in item.iter_mut().take(end.min(lines.len())).skip(i) {
            *k = is_item;
        }
        i = end;
    }
    let pick = |want: bool| -> String {
        lines
            .iter()
            .zip(&item)
            .map(|(l, &it)| if it == want { format!("{l}\n") } else { "\n".into() })
            .collect()
    };
    (pick(true), pick(false))
}

/// An error a block's comments name: `// error: <message>`, then any `//   help: <text>` and
/// `//   fix: <message>` lines for it.
#[derive(Debug, Default, PartialEq)]
struct Expected {
    message: String,
    help: Vec<String>,
    fixes: Vec<String>,
}

fn expected_errors(code: &str) -> Vec<Expected> {
    let mut out: Vec<Expected> = Vec::new();
    for line in code.lines() {
        let Some((_, comment)) = line.split_once("//") else { continue };
        let comment = comment.trim();
        if let Some(m) = comment.strip_prefix("error: ") {
            out.push(Expected { message: m.to_string(), ..Expected::default() });
        } else if let Some(last) = out.last_mut() {
            if let Some(h) = comment.strip_prefix("help: ") {
                last.help.push(h.to_string());
            } else if let Some(f) = comment.strip_prefix("fix: ") {
                last.fixes.push(f.to_string());
            }
        }
    }
    out
}

/// Whether an error is what's expected: its message, and every help and fix named.
fn meets(d: &wrela_diag::Diagnostic, e: &Expected, head: usize) -> bool {
    block_lines(&d.message, head) == e.message
        && e.help.iter().all(|h| d.help.contains(h))
        && e.fixes.iter().all(|f| d.fixes.iter().any(|x| &x.message == f))
}

/// A block's package: its context's items and files, its own items, and a function holding
/// the context's prelude and its statements. Also how many lines come before the block's first,
/// so a message's `line N` can name the block's line.
fn example_package(
    name: &str,
    block: &Block,
    cx: &Context,
    export: bool,
) -> (std::path::PathBuf, usize) {
    let (items, statements) = split(&block.code);
    let dir = scratch(name);
    for (path, text) in cx.files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).expect("make a directory");
        std::fs::write(p, text).expect("write a context file");
    }
    let vis = if export { "pub " } else { "" };
    let head = format!(
        "{vis}fn example({}) {{\n{}",
        cx.params,
        cx.prelude.lines().map(|l| format!("{l}\n")).collect::<String>()
    );
    let main = format!("{head}{statements}}}\n\n{items}\n{}\n", cx.items);
    std::fs::write(dir.join("main.wrela"), with_frame(&main)).expect("write main.wrela");
    (dir, head.lines().count())
}

/// `line N` in a message, as the block's line: `head` lines come before the block's first.
fn block_lines(message: &str, head: usize) -> String {
    let mut out = String::new();
    let mut rest = message;
    while let Some(at) = rest.find("line ") {
        out.push_str(&rest[..at + 5]);
        rest = &rest[at + 5..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        match rest[..digits].parse::<usize>() {
            Ok(n) if n > head => out.push_str(&(n - head).to_string()),
            _ => out.push_str(&rest[..digits]),
        }
        rest = &rest[digits..];
    }
    out.push_str(rest);
    out
}

/// Every problem with the blocks: each is checked in its context. Without contexts, a block
/// that should compile is also run, its statements as an export, so its `assert`s hold.
fn check_blocks(name: &str, blocks: &[Block], contexts: &[Context]) -> Vec<String> {
    let mut problems = Vec::new();
    for (n, block) in blocks.iter().enumerate() {
        let first = block.code.lines().next().unwrap_or("");
        let cx = contexts.iter().find(|c| c.first == first).unwrap_or(&NONE);
        if cx.first.is_empty() && !contexts.is_empty() {
            problems.push(format!("{}: no context names the block starting `{first}`", block.at));
            continue;
        }
        let run = contexts.is_empty();
        let (dir, head) = example_package(&format!("{name}/{n}"), block, cx, run);
        let out = wrela_driver::check(&dir);
        let errors: Vec<_> = out.diagnostics.iter().filter(|d| d.is_error()).collect();
        let expected = expected_errors(&block.code);
        let ok = errors.len() == expected.len()
            && errors.iter().zip(&expected).all(|(d, e)| meets(d, e, head));
        if !ok {
            problems.push(format!(
                "{}: expected {} error(s) {expected:#?}, got:\n{}",
                block.at,
                expected.len(),
                wrela_diag::render::render_all(&out.sources, &out.diagnostics)
            ));
        } else if run
            && expected.is_empty()
            && let Err(e) = run_example(&dir)
        {
            problems.push(format!("{}: {e}", block.at));
        } else if expected.is_empty() && block.code.contains("@test") {
            // A block with tests: they pass.
            let t = wrela_driver::test(&dir, None);
            if !t.passed() || t.results.is_empty() {
                let mut all = t.diagnostics.clone();
                all.extend(t.results.iter().filter_map(|r| r.failure.clone()));
                let shown = wrela_diag::render::render_all(&t.sources, &all);
                problems.push(format!("{}: its tests don't pass:\n{shown}", block.at));
            }
        }
    }
    problems
}

/// Builds the package at `dir` and calls its `example` export.
fn run_example(dir: &std::path::Path) -> Result<(), String> {
    let built = build(dir).map_err(|e| format!("doesn't build:\n{e}"))?;
    let out = dir.join("build");
    built.write_to(&out).expect("write the build");
    let mut host = wrela_host::CpuHost::load(&out).map_err(|e| format!("doesn't load: {e}"))?;
    host.call_export("example", &[]).map(|_| ()).map_err(|e| format!("fails when run: {e:?}"))
}

#[test]
fn every_example_in_language_md_compiles() {
    let doc = std::fs::read_to_string(repo_root().join("docs/language.md")).expect("language.md");
    let blocks = markdown_blocks("language.md", &doc);
    assert!(blocks.len() >= 20, "only {} wrela blocks in language.md", blocks.len());
    let problems = check_blocks("language-md-examples", &blocks, CONTEXTS);
    // Every context names a block, so none outlives its example.
    let firsts: Vec<&str> = blocks.iter().filter_map(|b| b.code.lines().next()).collect();
    let stale: Vec<&str> =
        CONTEXTS.iter().map(|c| c.first).filter(|f| !firsts.contains(f)).collect();
    assert!(stale.is_empty(), "contexts naming no block: {stale:?}");
    assert!(
        problems.is_empty(),
        "{} of {} examples:\n\n{}",
        problems.len(),
        blocks.len(),
        problems.join("\n\n")
    );
}

#[test]
fn every_example_in_std_docs_compiles_and_runs() {
    let blocks = std_blocks();
    assert!(!blocks.is_empty(), "std's doc comments have no examples");
    let problems = check_blocks("std-examples", &blocks, &[]);
    assert!(
        problems.is_empty(),
        "{} of {} examples:\n\n{}",
        problems.len(),
        blocks.len(),
        problems.join("\n\n")
    );
}

#[test]
fn the_tier_0_program_in_language_md_builds() {
    let doc = std::fs::read_to_string(repo_root().join("docs/language.md")).expect("language.md");
    let section = &doc[doc.find("## 19. A tier-0 program").expect("§19")..];
    let code = &section[section.find("```wrela\n").expect("a wrela block") + 9..];
    let code = &code[..code.find("```").expect("the block ends")];
    if let Err(e) = build(&package("language-md", code)) {
        panic!("§19's program doesn't build:\n{e}");
    }
}
