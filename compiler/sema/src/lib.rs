//! wrela's semantics (language.md §§3–8, 12): modules and names, types, traits and generics,
//! type inference, the memory model (§6), and the GPU rules (§12).

pub mod borrowck;
pub mod builtins;
pub mod check;
pub mod collect;
pub mod defs;
pub mod effects;
pub mod fieldwise;
pub mod gpu;
mod graph;
pub mod mir;
pub mod program;
pub mod resolve;
pub mod thir;

/// Directories at a package's top level that hold outputs, never modules: builds, results a
/// page saves, and the tools' own.
pub const OUTPUT_DIRS: &[&str] = &["build", "results", "node_modules", "target"];
pub mod traits;
pub mod ty;

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
    ("std::arena", include_str!("../../std/arena.wrela")),
    ("std::units", include_str!("../../std/units.wrela")),
    ("std::hash", include_str!("../../std/hash.wrela")),
    ("std::serialize", include_str!("../../std/serialize.wrela")),
    ("std::par", include_str!("../../std/par.wrela")),
    ("std::audio", include_str!("../../std/audio.wrela")),
    ("std::tick", include_str!("../../std/tick.wrela")),
    ("std::handoff", include_str!("../../std/handoff.wrela")),
    ("std::io", include_str!("../../std/io.wrela")),
    ("std::input", include_str!("../../std/input.wrela")),
    ("std::lift", include_str!("../../std/lift.wrela")),
    ("std::gpu", include_str!("../../std/gpu.wrela")),
    ("std::derive", include_str!("../../std/derive.wrela")),
    ("std::field", include_str!("../../std/field.wrela")),
    ("std::stage", include_str!("../../std/stage.wrela")),
    ("std::math", include_str!("../../std/math.wrela")),
    ("std::quat", include_str!("../../std/quat.wrela")),
    ("std::transform", include_str!("../../std/transform.wrela")),
    ("std::abi", include_str!("../../std/abi.wrela")),
];

/// A checked program: its definitions, every function body's MIR, and every constant.
#[derive(Debug)]
pub struct Checked {
    pub program: Program,
    /// Each function body's MIR.
    pub mir: BTreeMap<ty::FnId, mir::Body>,
    pub consts: BTreeMap<ty::ConstId, (ty::TyId, thir::Expr)>,
    /// The type each function that returns by naming its traits (`-> Creature<Coat>`) returns.
    pub hidden: HashMap<ty::FnId, ty::TyId>,
}

impl Checked {
    /// `t` through the types functions return by naming their traits ([`traits::reveal`]).
    pub fn reveal(&self, t: ty::TyId) -> ty::TyId {
        traits::reveal(&self.program, &self.hidden, t)
    }
}

/// Every file `embed` reads, with where it's read: from the bodies' MIR and the constants'
/// values (§10). A path may appear more than once.
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

/// Every `embed` in the program: its key (`embed_key`), and where it's written.
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
    check(units, packages, diags, false)
}

/// [`check_packages`], memory-checking std's bodies too: for std's own tests.
pub fn check_packages_and_std(
    units: Vec<SourceUnit>,
    packages: &[collect::PackageInfo],
    diags: &mut Vec<Diagnostic>,
) -> Checked {
    check(units, packages, diags, true)
}

fn check(
    units: Vec<SourceUnit>,
    packages: &[collect::PackageInfo],
    diags: &mut Vec<Diagnostic>,
    memory_check_std: bool,
) -> Checked {
    let mut program = collect::collect(units, packages, diags);
    let consts = {
        let p = &program;
        diags.extend(gpu::check_entries(p));
        for t in 0..p.traits.len() {
            diags.extend(fieldwise::check_trait(p, ty::TraitId(t as u32)));
        }
        diags.extend(check::check_declared_types(p));
        check::check_consts(p, diags)
    };
    // A constant's type is the result of the function the build runs to compute it.
    for (&c, &t) in &consts.tys {
        let f = program.const_(c).eval;
        program.fns[f.index()].ret = t;
    }
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
    for i in 0..p.fns.len() {
        let f = ty::FnId(i as u32);
        let check::FnCheck { body, diags: d, incomplete } = check::check_fn(p, &const_tys, f);
        let typed = !wrela_diag::has_errors(&d);
        diags.extend(d);
        if let Some(b) = body {
            if let Some(h) = b.hidden_ret {
                hidden.push((f, h));
            }
            // The memory checker needs a well-typed, whole body; it would only add noise
            // otherwise.
            if typed && !incomplete {
                let (m, d) = mir::build::build(p, &consts, f, &b);
                diags.extend(d);
                if memory_check(f) {
                    diags.extend(borrowck::check(p, &m));
                }
                mir.insert(f, m);
            }
        }
    }
    diags.extend(check::opaque_cycles(p, &hidden));
    diags.extend(effects::check(p, &mir));
    for i in 0..p.adts.len() {
        diags.extend(check::check_field_defaults(p, &const_tys, ty::AdtId(i as u32)));
    }
    let hidden = hidden.into_iter().collect();
    Checked { program, mir, consts, hidden }
}
