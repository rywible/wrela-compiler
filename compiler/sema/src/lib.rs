//! wrela's semantics (language.md §§3–8, 12): modules and names, types, traits and generics,
//! type inference, the memory model (§6), and the GPU rules (§18).

pub mod borrowck;
pub mod builtins;
pub mod check;
pub mod collect;
pub mod defs;
pub mod effects;
pub mod fieldwise;
pub mod gpu;
mod graph;
mod lints;
pub mod mir;
pub mod program;
pub mod resolve;
pub mod set;
pub mod thir;
pub mod traits;
pub mod ty;

/// Directories at a package's top level that hold outputs, never modules: builds, results a
/// page saves, and the tools' own.
pub const OUTPUT_DIRS: &[&str] = &["build", "results", "node_modules", "target"];

use std::collections::{BTreeMap, HashMap};
use wrela_diag::Diagnostic;

pub use collect::SourceUnit;
pub use program::Program;

/// The std library's sources, embedded in the compiler: (module path, text).
pub const STD_SOURCES: &[(&str, &str)] = &[
    ("std::prelude", include_str!("../../std/prelude.wrela")),
    ("std::mem", include_str!("../../std/mem.wrela")),
    ("std::alloc", include_str!("../../std/alloc.wrela")),
    ("std::collections", include_str!("../../std/collections.wrela")),
    ("std::string", include_str!("../../std/string.wrela")),
    ("std::cmp", include_str!("../../std/cmp.wrela")),
    ("std::fmt", include_str!("../../std/fmt.wrela")),
    ("std::json", include_str!("../../std/json.wrela")),
    ("std::arena", include_str!("../../std/arena.wrela")),
    ("std::units", include_str!("../../std/units.wrela")),
    ("std::hash", include_str!("../../std/hash.wrela")),
    ("std::hash::sha256", include_str!("../../std/hash/sha256.wrela")),
    ("std::serialize", include_str!("../../std/serialize.wrela")),
    ("std::pack", include_str!("../../std/pack.wrela")),
    ("std::net", include_str!("../../std/net.wrela")),
    ("std::store", include_str!("../../std/store.wrela")),
    ("std::crypto", include_str!("../../std/crypto.wrela")),
    ("std::webauthn", include_str!("../../std/webauthn.wrela")),
    ("std::par", include_str!("../../std/par.wrela")),
    ("std::audio", include_str!("../../std/audio.wrela")),
    ("std::tick", include_str!("../../std/tick.wrela")),
    ("std::handoff", include_str!("../../std/handoff.wrela")),
    ("std::io", include_str!("../../std/io.wrela")),
    ("std::host", include_str!("../../std/host.wrela")),
    ("std::tool", include_str!("../../std/tool.wrela")),
    ("std::tool::blob", include_str!("../../std/tool/blob.wrela")),
    ("std::tool::browser", include_str!("../../std/tool/browser.wrela")),
    ("std::tool::drive", include_str!("../../std/tool/drive.wrela")),
    ("std::tool::files", include_str!("../../std/tool/files.wrela")),
    ("std::tool::http", include_str!("../../std/tool/http.wrela")),
    ("std::tool::image", include_str!("../../std/tool/image.wrela")),
    ("std::tool::run", include_str!("../../std/tool/run.wrela")),
    ("std::tool::serve", include_str!("../../std/tool/serve.wrela")),
    ("std::tool::testing", include_str!("../../std/tool/testing.wrela")),
    ("std::tool::time", include_str!("../../std/tool/time.wrela")),
    ("std::tool::toolchain", include_str!("../../std/tool/toolchain.wrela")),
    ("std::input", include_str!("../../std/input.wrela")),
    ("std::semantics", include_str!("../../std/semantics.wrela")),
    ("std::time", include_str!("../../std/time.wrela")),
    ("std::lift", include_str!("../../std/lift.wrela")),
    ("std::reload", include_str!("../../std/reload.wrela")),
    ("std::job", include_str!("../../std/job.wrela")),
    ("std::cache", include_str!("../../std/cache.wrela")),
    ("std::gpu", include_str!("../../std/gpu.wrela")),
    ("std::derive", include_str!("../../std/derive.wrela")),
    ("std::field", include_str!("../../std/field.wrela")),
    ("std::stage", include_str!("../../std/stage.wrela")),
    ("std::math", include_str!("../../std/math.wrela")),
    ("std::quat", include_str!("../../std/quat.wrela")),
    ("std::transform", include_str!("../../std/transform.wrela")),
    ("std::abi", include_str!("../../std/abi.wrela")),
];

#[cfg(test)]
pub(crate) fn test_units(source: &str) -> Vec<SourceUnit> {
    let unit = |file, name: &str, text: &str, package| {
        let parsed = wrela_syntax::parse(wrela_diag::FileId(file), text);
        assert!(!parsed.has_errors(), "{:#?}", parsed.diagnostics);
        SourceUnit {
            path: name.split("::").map(str::to_string).collect(),
            ast: std::sync::Arc::new(parsed.file),
            package,
            syntax_errors: Vec::new(),
            text: std::sync::Arc::from(text),
        }
    };
    let mut units: Vec<SourceUnit> = STD_SOURCES
        .iter()
        .enumerate()
        .map(|(i, &(name, text))| unit(i as u32, name, text, 0))
        .collect();
    units.push(unit(units.len() as u32, "main", source, 1));
    units
}

/// A checked program: its definitions, every function body's MIR, and every constant.
#[derive(Debug)]
pub struct Checked {
    pub program: Program,
    /// Each function body's MIR.
    pub mir: BTreeMap<ty::FnId, mir::Body>,
    pub consts: BTreeMap<ty::ConstId, (ty::TyId, thir::Expr)>,
    /// Each associated constant's value as it's written (§3.4), in its function's generics:
    /// what lowering folds where a `const` argument names it.
    pub assoc_consts: BTreeMap<ty::FnId, thir::Expr>,
    /// The type each function that returns by naming its traits (`-> Creature<Coat>`) returns.
    pub hidden: HashMap<ty::FnId, ty::TyId>,
    /// Each body's own effects, for lowering's checks.
    pub direct: effects::Direct,
}

impl Checked {
    /// `t` through the types functions return by naming their traits ([`traits::reveal`]).
    pub fn reveal(&self, t: ty::TyId) -> ty::TyId {
        traits::reveal(&self.program, &self.hidden, t)
    }
}

/// How an `embed` names its file: the package that embeds it (its index, std's 0 and the
/// program's 1) and its path in that package, as `index:path`.
pub fn embed_key(package: usize, path: &str) -> String {
    format!("{package}:{path}")
}

/// An embed key's package index and path.
pub fn split_embed_key(key: &str) -> Option<(usize, &str)> {
    let (package, path) = key.split_once(':')?;
    Some((package.parse().ok()?, path))
}

/// Every `embed` in the program (§13): its key ([`embed_key`]), and where it's written, from the
/// bodies' MIR and the constants' values. A key may appear more than once.
pub fn embeds(checked: &Checked) -> Vec<(std::sync::Arc<str>, wrela_diag::Span)> {
    let mut out = Vec::new();
    for body in checked.mir.values() {
        for b in body.fns.iter().flat_map(|f| &f.blocks) {
            for s in &b.stmts {
                if let mir::StatementKind::Assign(_, mir::Rvalue::Embed(p))
                | mir::StatementKind::Eval(mir::Rvalue::Embed(p)) = &s.kind
                {
                    out.push((p.clone(), s.span));
                }
            }
        }
    }
    // A literal constant has no MIR, and no blocks.
    fn walk(e: &thir::Expr, out: &mut Vec<(std::sync::Arc<str>, wrela_diag::Span)>) {
        if let thir::ExprKind::Embed(p) = &e.kind {
            out.push((p.clone(), e.span));
        }
        e.for_each_child(&mut |c| {
            if let thir::Child::Expr(x) = c {
                walk(x, out);
            }
        });
    }
    for (_, e) in checked.consts.values() {
        walk(e, &mut out);
    }
    out
}

/// Collects and type-checks a whole program of one package and std (each unit's `package` is
/// 0 for std, 1 for the program's).
pub fn check_program(units: Vec<SourceUnit>, diags: &mut Vec<Diagnostic>) -> Checked {
    check_packages(units, &collect::PackageInfo::std_and_program(), diags)
}

/// Collects and type-checks a whole program, made of `packages` (language.md §3). std's bodies
/// aren't memory-checked: they're the same in every program, and its own tests check them
/// ([`check_packages_and_std`]).
pub fn check_packages(
    units: Vec<SourceUnit>,
    packages: &[collect::PackageInfo],
    diags: &mut Vec<Diagnostic>,
) -> Checked {
    check(units, packages, diags, false, &[])
}

/// [`check_packages`], with the program's root-module constants that `sets` names given its
/// values (`wrela build --set NAME=VALUE`, [`set`]).
pub fn check_packages_set(
    units: Vec<SourceUnit>,
    packages: &[collect::PackageInfo],
    sets: &[(String, String)],
    diags: &mut Vec<Diagnostic>,
) -> Checked {
    check(units, packages, diags, false, sets)
}

/// [`check_packages`], memory-checking std's bodies too: for std's own tests.
pub fn check_packages_and_std(
    units: Vec<SourceUnit>,
    packages: &[collect::PackageInfo],
    diags: &mut Vec<Diagnostic>,
) -> Checked {
    check(units, packages, diags, true, &[])
}

fn check(
    mut units: Vec<SourceUnit>,
    packages: &[collect::PackageInfo],
    diags: &mut Vec<Diagnostic>,
    memory_check_std: bool,
    sets: &[(String, String)],
) -> Checked {
    diags.extend(set::apply(&mut units, packages, sets));
    let mut program = collect::collect(units, packages, diags);
    let consts = {
        let p = &program;
        diags.extend(gpu::check_entries(p));
        diags.extend(check::job::check_jobs(p));
        for t in 0..p.traits.len() {
            diags.extend(fieldwise::check_trait(p, ty::TraitId(t as u32)));
        }
        let consts = check::check_consts(p, diags);
        diags.extend(set::check_types(p, &consts.tys, sets));
        diags.extend(collect::check_test_inputs(p, &consts.tys));
        consts
    };
    // A constant's type is the result of the function the build runs to compute it.
    for (&c, &t) in &consts.tys {
        let f = program.const_(c).eval;
        program.fns[f.index()].ret = t;
    }
    // What each job holds across its `yield`s, a field each of its value's (§16.3): its traits
    // are its locals', so they're known before the code that uses them is checked. (A job's
    // body is checked again below, with its diagnostics.)
    let jobs: Vec<ty::FnId> = (0..program.fns.len() as u32)
        .map(ty::FnId)
        .filter(|&f| program.func(f).attrs.job.is_some())
        .collect();
    for f in jobs {
        if let Some(b) = check::check_fn(&program, &consts.tys, f).body {
            let (m, _) = mir::build::build(&program, &consts.values, f, &b);
            let held = defs::JobHeld::of(&program, f, &m);
            program.job_held.insert(f, held);
        }
    }
    // What the trait solver decided about a job's value before it knew the job's locals.
    if !program.job_held.is_empty() {
        program.builtin_impls.borrow_mut().clear();
        program.fieldwise_impls.borrow_mut().clear();
    }
    // A type's declared traits, each a job's value's among them.
    diags.extend(check::check_declared_types(&program));
    diags.extend(collect::check_job_fields(&program));
    // From here on the definitions are read-only.
    let p = &program;
    let memory_check = |f: ty::FnId| {
        memory_check_std || p.package_of(p.func(f).module).kind != defs::PackageKind::Std
    };
    let check::CheckedConsts { tys: const_tys, values: consts, bodies: const_bodies } = consts;
    let mut mir = BTreeMap::new();
    for (c, body) in &const_bodies {
        let f = p.const_(*c).eval;
        let (m, d) = mir::build::build(p, &consts, f, body);
        diags.extend(d);
        if memory_check(f) {
            diags.extend(borrowck::check(p, &m));
        }
        mir.insert(f, m);
    }
    let mut hidden = Vec::new();
    let mut assoc_consts = BTreeMap::new();
    for i in 0..p.fns.len() {
        let f = ty::FnId(i as u32);
        let check::FnCheck { body, diags: d, incomplete } = check::check_fn(p, &const_tys, f);
        let typed = !wrela_diag::has_errors(&d);
        diags.extend(d);
        if let Some(b) = body {
            if let Some(h) = b.hidden_ret {
                hidden.push((f, h));
            }
            if p.func(f).attrs.assoc_const {
                assoc_consts.insert(f, b.value.clone());
            }
            // The memory checker needs a well-typed, whole body; it would only add noise
            // otherwise. A `@fieldwise` trait's walk has no memory IR of its own: each type that
            // declares the trait has it unrolled, and that's checked (`trait.fieldwise-walk`).
            let walk = matches!(p.func(f).owner, defs::FnOwner::Trait(t) if p.trait_(t).fieldwise)
                && fieldwise::walks_fields(p, f);
            if typed && !incomplete && !walk {
                let (m, d) = mir::build::build(p, &consts, f, &b);
                diags.extend(d);
                if memory_check(f) {
                    diags.extend(borrowck::check(p, &m));
                }
                diags.extend(lints::lint_fn(p, f, &m));
                mir.insert(f, m);
            }
        }
    }
    diags.extend(check::opaque_cycles(p, &hidden));
    let (effect_diags, direct) = effects::check(p, &mir);
    diags.extend(effect_diags);
    for i in 0..p.adts.len() {
        diags.extend(check::check_field_defaults(p, &const_tys, ty::AdtId(i as u32)));
    }
    if !wrela_diag::has_errors(diags) {
        diags.extend(unused_items(p, &mir));
    }
    let hidden = hidden.into_iter().collect();
    Checked { program, mir, consts, assoc_consts, hidden, direct }
}

/// W0008: a private function or constant that no other code uses (a function that only calls
/// itself counts as unused). Std's items, trait methods, tests, intrinsics, and names that
/// start with `_` aren't reported. Run only on a program with no errors, whose every body has
/// its memory IR, which says what it uses.
fn unused_items(p: &Program, mir: &BTreeMap<ty::FnId, mir::Body>) -> Vec<Diagnostic> {
    let mut fns = std::collections::HashSet::new();
    let mut consts = std::collections::HashSet::new();
    for (&owner, body) in mir {
        // Std's code can't name the program's private items.
        if p.is_std(p.func(owner).module) {
            continue;
        }
        for (s, r) in mir::rvalues(body) {
            if let mir::Rvalue::Const(c) = r {
                consts.insert(*c);
            }
            mir::fn_uses(p, s, r, |u, _| {
                if u.func() != owner {
                    fns.insert(u.func());
                }
            });
        }
    }
    let mut out = Vec::new();
    let unused = |name: &str, kind: &str, span| {
        Diagnostic::new(
            wrela_diag::codes::W0008,
            span,
            format!("the {kind} `{name}` is never used"),
        )
        .with_help(format!("remove it, or name it `_{name}` if it's kept on purpose"))
    };
    for (i, f) in p.fns.iter().enumerate() {
        let id = ty::FnId(i as u32);
        let inherent = match f.owner {
            defs::FnOwner::Free => true,
            defs::FnOwner::Impl(im) => p.impl_(im).trait_ref.is_none(),
            defs::FnOwner::Trait(_) | defs::FnOwner::Const(_) => false,
        };
        if !inherent
            || f.public
            || p.is_std(f.module)
            || f.attrs.test.is_some()
            || f.attrs.intrinsic
            || f.lang.is_some()
            || f.derived.is_some()
            || f.name.starts_with('_')
            || fns.contains(&id)
        {
            continue;
        }
        let kind = if f.attrs.entry.is_some() {
            "entry point"
        } else if f.attrs.assoc_const {
            "constant"
        } else {
            "function"
        };
        out.push(unused(&f.name, kind, f.name_span));
    }
    for (i, c) in p.consts.iter().enumerate() {
        let id = ty::ConstId(i as u32);
        if c.public
            || c.default
            || p.is_std(c.module)
            || c.name.starts_with('_')
            || consts.contains(&id)
            || p.const_used(id)
        {
            continue;
        }
        out.push(unused(&c.name, "constant", c.span));
    }
    out
}
