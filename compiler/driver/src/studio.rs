//! `wrela studio` (AC8–AC10 of #39): the lens's program around a subject package. The lens is
//! the `studio` package (wrela, generic over any surface); this writes the small program that
//! instantiates it with the subject's `subject()` (the glue), and builds it, lifting the
//! subject's literals so an edit is a write to the table.
//!
//! The glue goes in `<subject>/build/studio/lens/`, beside copies of the `studio` and `ui`
//! packages (embedded in the compiler, so the command works from any directory); the build in
//! `<subject>/build/studio/page/`. A subject package has a module `subject.wrela` with
//! `pub fn subject()`, which returns a `Surface + Lipschitz`; if it's a `Field<C>` too, the glue
//! writes each of the channels' public float fields into the lens's probes.

use crate::live::Files;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wrela_sema::Checked;
use wrela_sema::ty::{TyId, TyKind};
use wrela_syntax::token::TokenKind;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/packages.rs"));
}

/// What the glue needs to know of a subject.
#[derive(Clone, Debug, PartialEq)]
struct Subject {
    /// The package's name (its manifest's, or its directory's).
    name: String,
    /// The channels' float fields, each by its path from the channels (`albedo.r`) and how
    /// many floats it holds (1 for an `f32`, 2 to 4 for a vector); none if the subject has no
    /// channels.
    channels: Vec<(String, u8)>,
    /// The channels' type as the glue names it (`wolf::creature::Coat`, `std::field::Color`)
    /// and the path to its colour: its first `r`, `g`, `b` floats (`albedo`, or empty when the
    /// type is the colour). `None` if the channels hold no colour the glue can name.
    albedo: Option<(String, String)>,
    /// Whether the package has a spec (a module `spec` with `pub fn spec<C: Checks>(c: mut
    /// C)`), and which outlines its blueprint has (a module `blueprint` with `pub fn side()`,
    /// `front()`, `top()`, each `-> Vec<vec2>`).
    spec: bool,
    outlines: [bool; 3],
}

/// The lens's exported actions: each one's name and arguments (`f`: an `f32`, `u`: a `u32`).
/// The glue exports each, and the headless runner calls them by this table.
pub const ACTIONS: &[(&str, &str)] = &[
    ("view", "u"),
    ("zoom", "ffff"),
    ("spec", ""),
    ("blueprint", "u"),
    ("fit_blueprint", "uu"),
    ("mode", "uu"),
    ("parts", ""),
    ("probe", "fff"),
    ("project", "fff"),
    ("ray", "ffffff"),
    ("measure", "ffffff"),
    ("diagnose", "ffff"),
    ("describe", ""),
    ("literals", ""),
    ("set", "uf"),
    ("click", "ff"),
    ("move", "ffffff"),
    ("choose", "u"),
    ("choose_none", ""),
    ("silhouette", "uffffu"),
    ("fit", "uffffu"),
    ("write", ""),
];

/// The subject package at `pkg`: its name, and its channels' float fields, from its checked
/// program (the type `subject()` returns).
fn subject(pkg: &Path) -> Result<Subject, String> {
    let name = crate::manifest::name_in(pkg).unwrap_or_else(|| {
        pkg.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    });
    crate::with_checked(pkg, |checked, _| {
        let p = &checked.program;
        let f = p
            .fns
            .iter()
            .enumerate()
            .find(|(_, f)| {
                f.public
                    && f.name == "subject"
                    && f.params.is_empty()
                    && p.module(f.module).path.last().is_some_and(|m| m == "subject")
                    && p.package_of(f.module).kind != wrela_sema::defs::PackageKind::Std
            })
            .map(|(i, _)| wrela_sema::ty::FnId(i as u32))
            .ok_or_else(|| {
                format!(
                    "`{}` has no `pub fn subject()` in a module `subject` (subject.wrela): the lens shows what it returns, a `Surface + Lipschitz`",
                    pkg.display()
                )
            })?;
        let ty = checked.reveal(p.func(f).ret);
        // The channels' type: as `subject()` declares it (`-> Field<Coat> + Lipschitz`), or as
        // its type implements `Field`.
        // std's `Field<C>`: an ordinary trait, found by its path.
        let field_trait = p
            .traits
            .iter()
            .position(|t| t.name == "Field" && p.module(t.module).path == ["std", "field"])
            .map(|i| wrela_sema::ty::TraitId(i as u32));
        let declared = field_trait.and_then(|field| {
            p.func(f)
                .opaque
                .iter()
                .flatten()
                .find(|r| r.trait_ == field)
                .and_then(|r| r.args.first().copied())
        });
        let implemented = || {
            let field = field_trait?;
            let args = wrela_sema::traits::impl_args(p, ty, field);
            args.first().filter(|a| a.len() == 1).map(|a| a[0])
        };
        let mut channels = Vec::new();
        let mut albedo = None;
        if let Some(c) = declared.or_else(implemented) {
            floats(checked, c, String::new(), 0, &mut channels);
            albedo = colour_path(checked, c, &name, &channels);
        }
        // A spec and a blueprint: free functions of the program package's modules `spec` and
        // `blueprint`.
        let own = |module: &str, function: &str| {
            p.fns.iter().any(|g| {
                g.public
                    && g.name == function
                    && g.owner == wrela_sema::defs::FnOwner::Free
                    && p.module(g.module).path == [module]
                    && p.package_of(g.module).kind == wrela_sema::defs::PackageKind::Program
            })
        };
        let spec = own("spec", "spec");
        let outlines =
            [own("blueprint", "side"), own("blueprint", "front"), own("blueprint", "top")];
        Ok(Subject { name: name.clone(), channels, albedo, spec, outlines })
    })?
}

/// The channels' type `c` as the glue can name it (in the subject package `name`, or std), and
/// the path to its first `r`, `g`, `b` floats.
fn colour_path(
    checked: &Checked,
    c: TyId,
    name: &str,
    channels: &[(String, u8)],
) -> Option<(String, String)> {
    let p = &checked.program;
    let TyKind::Adt(a, args) = p.types.kind(c) else { return None };
    if !args.is_empty() {
        return None;
    }
    let adt = p.adt(*a);
    let module = &p.module(adt.module).path;
    let ty = match p.package_of(adt.module).kind {
        wrela_sema::defs::PackageKind::Std => format!("{}::{}", module.join("::"), adt.name),
        wrela_sema::defs::PackageKind::Program => {
            format!("{name}::{}::{}", module.join("::"), adt.name)
        }
        wrela_sema::defs::PackageKind::Dependency => return None,
    };
    if !adt.public {
        return None;
    }
    let has = |path: &str| channels.iter().any(|(q, n)| q == path && *n == 1);
    let prefix = channels.iter().find_map(|(q, _)| {
        let base = q.strip_suffix(".r").or((q == "r").then_some(""))?;
        let at = |c: &str| if base.is_empty() { c.to_string() } else { format!("{base}.{c}") };
        (has(&at("r")) && has(&at("g")) && has(&at("b"))).then(|| base.to_string())
    })?;
    Some((ty, prefix))
}

/// The public float fields of a value of type `t`, by path from it, into `out`.
fn floats(checked: &Checked, t: TyId, path: String, depth: u32, out: &mut Vec<(String, u8)>) {
    let p = &checked.program;
    if depth > 4 {
        return;
    }
    match p.types.kind(t) {
        TyKind::Float(wrela_sema::ty::FloatTy::F32) => out.push((path, 1)),
        TyKind::Vec(wrela_sema::ty::VecElem::F32, n) => out.push((path, *n)),
        TyKind::Adt(a, args) => {
            let adt = p.adt(*a);
            if adt.is_enum() {
                return;
            }
            for (fd, ft) in adt.fields().iter().zip(p.fields_of(*a, args, None)) {
                if fd.public {
                    let sub = if path.is_empty() {
                        fd.name.clone()
                    } else {
                        format!("{path}.{}", fd.name)
                    };
                    floats(checked, ft, sub, depth + 1, out);
                }
            }
        }
        _ => {}
    }
}

/// The glue: the lens's program around `s`.
fn glue(s: &Subject) -> String {
    let mut g = String::new();
    let name = &s.name;
    let _ = write!(
        g,
        "// Written by `wrela studio`: the lens (the `studio` package) on the package `{name}`.\n\
         // Each action is an export, and a command the lens reads as typed text.\n\n\
         use std::collections::Vec\n\
         use std::lift::{{Literal, gradient, reads}}\n\
         use std::string::String\n\
         use studio::json::obj\n\
         use studio::lens::Lens\n\
         use {name}::subject::subject\n"
    );
    if !s.channels.is_empty() {
        g.push_str("use std::field::Field\n");
    }
    g.push_str("use std::field::Lipschitz\nuse studio::paint::Colours\n");
    // The subject as the views see it: coloured by its channels' albedo where they hold one.
    match &s.albedo {
        Some((ty, prefix)) => {
            let at = |c: &str| if prefix.is_empty() { format!("c.{c}") } else { format!("c.{prefix}.{c}") };
            let _ = write!(
                g,
                "use std::field::{{PartName, Surface}}\n\n\
                 /// The subject, coloured by its channels, for the views' colour mode.\n\
                 struct Painted<F>: Copy + GpuData {{\n    field: F,\n}}\n\n\
                 impl<F: Field<{ty}>> Surface for Painted<F> {{\n    \
                 fn distance(self, p: vec3) -> f32 {{\n        self.field.distance(p)\n    }}\n\n    \
                 fn part_offset(self, p: vec3, i: u32, by: f32) -> f32 {{\n        self.field.part_offset(p, i, by)\n    }}\n\n    \
                 fn parts(self) -> u32 {{\n        self.field.parts()\n    }}\n\n    \
                 fn part_at(self, p: vec3) -> (f32, u32) {{\n        self.field.part_at(p)\n    }}\n\n    \
                 fn part_distance(self, p: vec3, i: u32) -> f32 {{\n        self.field.part_distance(p, i)\n    }}\n\n    \
                 fn part_name(self, i: u32) -> PartName {{\n        self.field.part_name(i)\n    }}\n}}\n\n\
                 impl<F: Field<{ty}> + Lipschitz> Lipschitz for Painted<F> {{\n    \
                 fn lipschitz(self, near: f32) -> f32 {{\n        self.field.lipschitz(near)\n    }}\n}}\n\n\
                 impl<F: Field<{ty}>> Colours for Painted<F> {{\n    \
                 fn colour_at(self, p: vec3) -> vec3 {{\n        let c = self.field.channels(p)\n        vec3({}, {}, {})\n    }}\n}}\n\n\
                 fn painted() -> Colours + Lipschitz {{\n    Painted {{ field: subject() }}\n}}\n",
                at("r"),
                at("g"),
                at("b")
            );
        }
        None => g.push_str(
            "\nfn painted() -> Colours + Lipschitz {\n    studio::paint::Clay { field: subject() }\n}\n",
        ),
    }
    g.push_str(
        "\nfn grad(p: vec3, which: [Literal; 16]) -> (f32, [f32; 16]) {\n    \
         gradient(|| subject().distance(p), which)\n}\n\n\
         fn grad8(p: vec3, which: [Literal; 8]) -> (f32, [f32; 8]) {\n    \
         gradient(|| subject().distance(p), which)\n}\n\n\
         fn reached() -> Vec<Literal> {\n    var out: Vec<Literal> = Vec::new()\n    \
         for l in reads(|| subject().distance(vec3(0.0))) {\n        out.push(l)\n    }\n    \
         out\n}\n\n\
         fn part_reads(p: vec3, part: u32) -> Vec<Literal> {\n    var out: Vec<Literal> = Vec::new()\n    \
         for l in reads(|| subject().part_distance(p, part)) {\n        out.push(l)\n    }\n    \
         out\n}\n\n",
    );
    if s.channels.is_empty() {
        g.push_str("fn channels(p: vec3) -> String {\n    String::new()\n}\n\n");
    } else {
        g.push_str(
            "fn channels(p: vec3) -> String {\n    let c = subject().channels(p)\n    obj()",
        );
        for (path, n) in &s.channels {
            match n {
                1 => {
                    let _ = write!(g, "\n        .num(\"{path}\", c.{path})");
                }
                3 => {
                    let _ = write!(g, "\n        .vec(\"{path}\", c.{path})");
                }
                _ => {
                    for axis in ["x", "y", "z", "w"].iter().take(*n as usize) {
                        let _ = write!(g, "\n        .num(\"{path}.{axis}\", c.{path}.{axis})");
                    }
                }
            }
        }
        g.push_str("\n        .done()\n}\n\n");
    }
    // The spec, as a report.
    if s.spec {
        let _ = write!(
            g,
            "fn spec_report() -> String {{\n    var r = std::lift::Report::new()\n    {name}::spec::spec(mut r)\n    r.done()\n}}\n\n"
        );
    } else {
        g.push_str(
            "fn spec_report() -> String {\n    String::from(\"{\\\"error\\\":\\\"the package has no spec: write `pub fn spec<C: Checks>(c: mut C)` in spec.wrela\\\"}\")\n}\n\n",
        );
    }
    // The blueprint's outlines, by facing (0 side, 1 front, 2 top).
    g.push_str("fn outline(facing: u32) -> Vec<vec2> {\n");
    for (i, f) in ["side", "front", "top"].iter().enumerate() {
        if s.outlines[i] {
            let _ = writeln!(
                g,
                "    if facing == {i} {{\n        return {name}::blueprint::{f}()\n    }}"
            );
        }
    }
    g.push_str("    Vec::new()\n}\n\n");
    g.push_str(
        "pub fn init() -> Lens {\n    studio::lens::open(painted)\n}\n\n\
         pub fn frame(state: mut Lens, time: f32, width: u32, height: u32) {\n    \
         studio::lens::frame(mut state, painted, grad, grad8, reached, part_reads, channels, spec_report, outline, width, height)\n}\n\n\
         pub fn busy(state: Lens) -> u32 {\n    if studio::lens::busy(state) { 1 } else { 0 }\n}\n\n\
         pub fn view(state: mut Lens, facing: u32) {\n    studio::lens::show(mut state, facing)\n}\n\n\
         pub fn zoom(state: mut Lens, x: f32, y: f32, z: f32, half: f32) {\n    \
         studio::lens::zoom(mut state, vec3(x, y, z), half)\n}\n\n\
         pub fn spec(state: mut Lens) {\n    studio::lens::spec_answer(mut state, spec_report)\n}\n\n\
         pub fn blueprint(state: mut Lens, facing: u32) {\n    \
         studio::lens::show_blueprint(mut state, painted, outline, facing)\n}\n\n\
         pub fn fit_blueprint(state: mut Lens, facing: u32, res: u32) {\n    \
         studio::lens::fit_blueprint(mut state, outline, facing, res)\n}\n\n\
         pub fn mode(state: mut Lens, mode: u32, part: u32) {\n    \
         studio::lens::set_mode(mut state, mode, part)\n}\n\n\
         pub fn parts(state: mut Lens) {\n    studio::lens::parts(mut state, painted)\n}\n\n\
         pub fn probe(state: mut Lens, x: f32, y: f32, z: f32) {\n    \
         studio::lens::probe(mut state, painted, channels, vec3(x, y, z))\n}\n\n\
         pub fn project(state: mut Lens, x: f32, y: f32, z: f32) {\n    \
         studio::lens::project(mut state, vec3(x, y, z))\n}\n\n\
         pub fn ray(state: mut Lens, x: f32, y: f32, z: f32, dx: f32, dy: f32, dz: f32) {\n    \
         studio::lens::ray(mut state, painted, vec3(x, y, z), vec3(dx, dy, dz))\n}\n\n\
         pub fn measure(state: mut Lens, x0: f32, y0: f32, z0: f32, x1: f32, y1: f32, z1: f32) {\n    \
         studio::lens::measure(mut state, vec3(x0, y0, z0), vec3(x1, y1, z1))\n}\n\n\
         pub fn diagnose(state: mut Lens, x: f32, y: f32, z: f32, radius: f32) {\n    \
         studio::lens::diagnose(mut state, painted, vec3(x, y, z), radius)\n}\n\n\
         pub fn literals(state: mut Lens) {\n    studio::lens::literals(mut state)\n}\n\n\
         pub fn describe(state: mut Lens) {\n    studio::lens::describe_subject(mut state, painted)\n}\n\n\
         pub fn set(state: mut Lens, literal: u32, value: f32) {\n    \
         studio::lens::set(mut state, literal, value)\n}\n\n\
         pub fn click(state: mut Lens, x: f32, y: f32) {\n    \
         studio::lens::click(mut state, painted, grad, reached, part_reads, x, y)\n}\n\n\
         pub fn write(state: mut Lens) {\n    studio::lens::write(mut state)\n}\n\n\
         pub fn move(state: mut Lens, x0: f32, y0: f32, z0: f32, x1: f32, y1: f32, z1: f32) {\n    \
         studio::lens::move_point(mut state, painted, vec3(x0, y0, z0), vec3(x1, y1, z1))\n}\n\n\
         pub fn choose(state: mut Lens, literal: u32) {\n    \
         studio::lens::choose(mut state, literal, none: false)\n}\n\n\
         pub fn choose_none(state: mut Lens) {\n    studio::lens::choose(mut state, 0, none: true)\n}\n\n\
         pub fn silhouette(state: mut Lens, facing: u32, x: f32, y: f32, z: f32, half: f32, res: u32) {\n    \
         studio::lens::silhouette(mut state, facing, vec3(x, y, z), half, res)\n}\n\n\
         pub fn fit(state: mut Lens, facing: u32, x: f32, y: f32, z: f32, half: f32, res: u32) {\n    \
         studio::lens::fit_reference(mut state, facing, vec3(x, y, z), half, res)\n}\n",
    );
    g
}

/// Where the lens's files go for the subject at `pkg`.
pub fn dir(pkg: &Path) -> PathBuf {
    pkg.join("build").join("studio")
}

/// Writes the lens's program for the subject at `pkg` (the glue, and the `studio` and `ui`
/// packages beside it), leaving files that are already as they'd be: the program's directory.
fn write_lens(pkg: &Path, s: &Subject) -> std::io::Result<PathBuf> {
    let base = dir(pkg);
    for (sub, files) in [("studio", embedded::STUDIO), ("ui", embedded::UI)] {
        for (rel, bytes) in files {
            write_if_changed(&base.join(sub).join(rel), bytes)?;
        }
    }
    let lens = base.join("lens");
    let manifest = format!(
        "# Written by `wrela studio`: the lens on `{}`.\n[package]\nname = \"lens\"\n\n[dependencies]\nstudio = {{ path = \"../studio\" }}\n{} = {{ path = \"../../..\" }}\n",
        s.name, s.name
    );
    write_if_changed(&lens.join("wrela.toml"), manifest.as_bytes())?;
    write_if_changed(&lens.join("main.wrela"), glue(s).as_bytes())?;
    Ok(lens)
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, bytes)
}

/// Builds the lens on the subject at `pkg`, lifting the subject's literals, into
/// `<pkg>/build/studio/page/`: the build's output, and that directory.
pub fn build(pkg: &Path, debug: bool) -> Result<(crate::Output, PathBuf), String> {
    let s = subject(pkg)?;
    let lens = write_lens(pkg, &s).map_err(|e| format!("can't write the lens's program: {e}"))?;
    let kind = crate::BuildKind { debug, testing: false };
    let out = crate::build_lifted(&lens, std::slice::from_ref(&s.name), kind)?;
    let page = dir(pkg).join("page");
    if !out.has_errors() {
        let wasm = out.files.iter().find(|(p, _)| p == "game.wasm").map(|(_, w)| w.as_slice());
        clear(&page, wasm);
        out.write_to(&page).map_err(|e| format!("can't write the build: {e}"))?;
    }
    Ok((out, page))
}

/// Empties the page at `page` for a new build, but for the compiled code `wrela_host` keeps of
/// the build's WASM `wasm` (`.native/game-<hash>-*.cwasm`, the hash the WASM's): the next build
/// of the same program has the same WASM, and loading it then needn't compile it again.
fn clear(page: &Path, wasm: Option<&[u8]>) {
    let keep = wasm.map(wrela_host::compiled_code_prefix);
    let Ok(entries) = std::fs::read_dir(page) else { return };
    for e in entries.flatten() {
        let path = e.path();
        let dir = e.file_type().is_ok_and(|t| t.is_dir());
        if dir && e.file_name() == ".native" {
            for c in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                let kept =
                    keep.as_deref().is_some_and(|k| c.file_name().to_string_lossy().starts_with(k));
                if !kept {
                    let _ = std::fs::remove_file(c.path());
                }
            }
        } else if dir {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

// ---- edits: literals by their index in the build, written to the files as they are now -----------

/// The lifted build's report (`lift.json`) in the page at `page`.
pub(crate) fn report(page: &Path) -> Result<crate::lift::Report, String> {
    let text = std::fs::read_to_string(page.join("lift.json"))
        .map_err(|e| format!("the lens's build has no lift.json: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("lift.json doesn't read: {e}"))
}

/// A file's text, lexed once for finding literals in it: its number tokens, in order, as byte
/// ranges (a minus before one, taken into a lifted literal, isn't a separate token here), and
/// what an edit of literals keeps of its tokens (its shape).
pub struct Numbers {
    text: Arc<str>,
    tokens: Vec<(u32, u32)>,
    /// Its tokens but its line breaks and its end, each number a `Float` with no text, and the
    /// minus signs before a number (an edit of a literal may add, drop or move one) left out.
    /// A minus before anything else is the code's.
    shape: Vec<(TokenKind, u32, u32)>,
}

impl Numbers {
    pub fn new(text: impl Into<Arc<str>>) -> Numbers {
        let text = text.into();
        let lexed: Vec<_> = crate::edit::lex(&text).collect();
        let mut tokens = Vec::new();
        let mut shape = Vec::with_capacity(lexed.len());
        for (i, t) in lexed.iter().enumerate() {
            if t.kind.is_number() {
                tokens.push((t.span.start, t.span.end));
                shape.push((TokenKind::Float, 0, 0));
            } else if t.kind != TokenKind::Minus
                || !lexed[i + 1..]
                    .iter()
                    .find(|t| t.kind != TokenKind::Minus)
                    .is_some_and(|t| t.kind.is_number())
            {
                shape.push((t.kind, t.span.start, t.span.end));
            }
        }
        Numbers { text, tokens, shape }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether `other` lexes to the same tokens but for numbers, and the minus signs before a
    /// number: what an edit of literals changes, and nothing else.
    pub(crate) fn same_but_numbers(&self, other: &Numbers) -> bool {
        self.shape.len() == other.shape.len()
            && self.shape.iter().zip(&other.shape).all(|(&(k, s, e), &(l, t, u))| {
                k == l && self.text[s as usize..e as usize] == other.text[t as usize..u as usize]
            })
    }
}

/// Where literal `start..end` of the build's text (`then`) is in the file's text `now`: the
/// same number token by its position among the file's numbers, which an edit of literals keeps.
/// A literal's place takes the unary minus before its number now if it started at its minus
/// (an edit may have dropped it since), or if an edit has put one there since (the build's
/// text had none). `None` if the token isn't there.
pub fn moved(then: &Numbers, now: &Numbers, start: u32, end: u32) -> Option<(u32, u32)> {
    use crate::edit::minus_before;
    let (old, new) = (then.text.as_bytes(), now.text.as_bytes());
    let minus = old.get(start as usize) == Some(&b'-');
    let number_start =
        if minus { crate::edit::blanks_after(old, start as usize + 1) as u32 } else { start };
    let k = then.tokens.binary_search_by_key(&number_start, |t| t.0).ok()?;
    if then.tokens[k].1 != end {
        return None;
    }
    let (s, e) = *now.tokens.get(k)?;
    let had = minus || minus_before(old, start as usize).is_some();
    match minus_before(new, s as usize) {
        Some(m) if minus || !had => Some((m as u32, e)),
        _ => Some((s, e)),
    }
}

/// Each file of a lifted build's report `r` (its paths relative to `base`) as a watcher found
/// it among `files`, with its text when built, lexed: `None` if one isn't among them.
pub(crate) fn lifted_files(
    base: &Path,
    r: &crate::lift::Report,
    files: &Files,
) -> Option<Vec<(PathBuf, Numbers)>> {
    // The files by their canonical paths, as the report names them from `base`.
    let canonical: BTreeMap<PathBuf, &PathBuf> =
        files.keys().filter_map(|p| Some((p.canonicalize().ok()?, p))).collect();
    r.files
        .iter()
        .map(|f| {
            let path = canonical.get(&base.join(&f.path).canonicalize().ok()?)?;
            Some((path.to_path_buf(), Numbers::new(f.text.as_str())))
        })
        .collect()
}

/// The build's literals' values (`literals`) in the files as they are (`now`), if what changed
/// since the build is literals alone: each file it lifted (`lifted`, from [`lifted_files`]) has
/// the build's tokens but for numbers, and every other file is as the build saw it (`built`).
pub(crate) fn literal_values(
    lifted: &[(PathBuf, Numbers)],
    literals: &[crate::lift::ReportLiteral],
    now: &Files,
    built: &Files,
) -> Option<Vec<f32>> {
    // A file added or deleted since the build changes the program, lifted or not.
    let other = |p: &PathBuf| !lifted.iter().any(|(l, _)| l == p);
    if !now.keys().eq(built.keys()) || now.iter().any(|(p, s)| other(p) && !s.same_text(&built[p]))
    {
        return None;
    }
    // Every file the build lifted, as it is now, each lexed once.
    let mut texts = Vec::with_capacity(lifted.len());
    for (path, then) in lifted {
        let current = Numbers::new(Arc::clone(&now[path].text));
        if !then.same_but_numbers(&current) {
            return None;
        }
        texts.push(current);
    }
    let mut values = Vec::with_capacity(literals.len());
    for l in literals {
        let current = texts.get(l.file as usize)?;
        let (s, e) = moved(&lifted[l.file as usize].1, current, l.start, l.end)?;
        values.push(crate::edit::signed_value(&current.text()[s as usize..e as usize])?);
    }
    Some(values)
}

/// Edits of the build's literals, each by its index in the build and its new value, planned
/// against the subject's files as they are now (`pkg`, the subject's package; `page`, the
/// lens's build): what `wrela edit` would write.
fn plan_edits(
    pkg: &Path,
    page: &Path,
    edits: &[(u32, f32)],
) -> Result<Vec<crate::edit::FileEdit>, crate::edit::Refused> {
    let refused = |why: String| crate::edit::Refused { file: String::new(), why };
    let r = report(page).map_err(refused)?;
    // The lens's build is of the program around the subject, so paths are from the lens's
    // package: `../../..` is the subject's.
    let lens = dir(pkg).join("lens");
    // Each file an edit is in, by its index in the build: its text then and now, each lexed once.
    let mut lexed: BTreeMap<u32, (Numbers, Numbers)> = BTreeMap::new();
    let mut out = Vec::new();
    for &(i, value) in edits {
        let Some(l) = r.literals.get(i as usize) else {
            return Err(refused(format!("the build has no literal {i}")));
        };
        let rel = &r.files[l.file as usize].path;
        let (then, now) = match lexed.entry(l.file) {
            std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::btree_map::Entry::Vacant(v) => {
                let path = lens.join(rel);
                let now = std::fs::read_to_string(&path)
                    .map_err(|e| refused(format!("can't read {}: {e}", path.display())))?;
                let (then, now) =
                    (Numbers::new(r.files[l.file as usize].text.as_str()), Numbers::new(now));
                // A literal is found by its place among the file's numbers, which only an edit
                // of literals keeps: any other change since the build and it could be another.
                if !then.same_but_numbers(&now) {
                    return Err(crate::edit::Refused {
                        file: rel.clone(),
                        why: "it has changed past its literals since the lens was built: rebuild"
                            .into(),
                    });
                }
                v.insert((then, now))
            }
        };
        let Some((s, e)) = moved(then, now, l.start, l.end) else {
            return Err(crate::edit::Refused {
                file: rel.clone(),
                why: "it has changed past its literals since the lens was built: rebuild".into(),
            });
        };
        // The value is the text's, minus and all: what the code reads (as the table holds it).
        out.push(crate::edit::LiteralEdit {
            file: rel.clone(),
            start: s,
            end: e,
            text: now.text[s as usize..e as usize].to_string(),
            hash: crate::lift::file_hash(now.text.as_bytes()),
            value,
        });
    }
    crate::edit::plan(&lens, &out)
}

/// The edits of a body `{"edits": [[literal, value], ...]}`, as the lens posts them.
pub fn parse_edits(body: &[u8]) -> Option<Vec<(u32, f32)>> {
    serde_json::from_slice::<serde_json::Value>(body).ok().and_then(|v| {
        v["edits"].as_array().map(|es| {
            es.iter()
                .filter_map(|e| Some((e.get(0)?.as_u64()? as u32, e.get(1)?.as_f64()? as f32)))
                .collect()
        })
    })
}

/// Applies a body of the lens's edits to the subject's files through `wrela edit`: the JSON
/// answer (`wrela edit --json`'s), or why they were refused. The server's and the headless
/// runner's posts both come here, and send it as one line: the lens prints it in its answer,
/// and answers are JSON lines.
pub fn apply(pkg: &Path, page: &Path, body: &[u8]) -> serde_json::Value {
    let Some(edits) = parse_edits(body) else {
        return serde_json::json!({ "written": false, "why": "the edits aren't [[literal, value], ...]" });
    };
    match plan_edits(pkg, page, &edits) {
        Ok(planned) => match crate::edit::write(&dir(pkg).join("lens"), &planned) {
            Ok(_) => crate::edit::to_value(&planned, true),
            Err(e) => serde_json::json!({ "written": false, "why": e.to_string() }),
        },
        Err(r) => serde_json::json!({ "written": false, "file": r.file, "why": r.why }),
    }
}

/// What answers the lens's posts in a native host, as the server answers them in Chrome: its
/// edits (`studio/edit`) go through `wrela edit` to the subject's files, and its fit's reference
/// (`studio/reference`) is `reference`.
pub fn post_handler(
    pkg: &Path,
    page: &Path,
    reference: Option<Vec<u8>>,
) -> wrela_host::PostHandler {
    let (pkg, page) = (pkg.to_path_buf(), page.to_path_buf());
    wrela_host::PostHandler::new(move |url: &str, body: &[u8]| match url {
        "studio/edit" => Ok(apply(&pkg, &page, body).to_string().into_bytes()),
        "studio/reference" => reference
            .clone()
            .ok_or_else(|| "no reference image: give one with --reference".to_string()),
        _ => Err(format!("nothing answers `{url}` here")),
    })
}

/// A reference image for the lens's fit (`fit`), as its post `studio/reference` takes it: the
/// image's size (it must be square) as 4 bytes, little-endian, then a byte a pixel, rows top to
/// bottom: 1 inside (dark: luminance under half, and opaque), 0 outside.
pub fn reference_mask(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    if width != height {
        return Err(format!("a reference is square; this one is {width}x{height}"));
    }
    let mut out = width.to_le_bytes().to_vec();
    for px in rgba.chunks_exact(4) {
        let luminance =
            0.2126 * f32::from(px[0]) + 0.7152 * f32::from(px[1]) + 0.0722 * f32::from(px[2]);
        out.push(u8::from(luminance < 127.5 && px[3] >= 128));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same_but_numbers(a: &str, b: &str) -> bool {
        Numbers::new(a).same_but_numbers(&Numbers::new(b))
    }

    #[test]
    fn only_numbers_and_the_minus_signs_before_them_may_change() {
        // What an edit of literals changes: a number, a minus moved with one, a second minus.
        assert!(same_but_numbers("let a = x - 0.5 * y", "let a = x - 0.25 * y"));
        assert!(same_but_numbers("f(-0.5, 2.0)", "f(0.2, 2.5)"));
        assert!(same_but_numbers("x - 0.5", "x - -0.2"));
        // What only the code's changes do: a minus before a name, an operator, a new number.
        assert!(!same_but_numbers("a - b", "-a - b"));
        assert!(!same_but_numbers("x * y", "x * -y"));
        assert!(!same_but_numbers("a - 0.5", "a + 0.5"));
        assert!(!same_but_numbers("f(1.0)", "f(1.0, 2.0)"));
    }
}
