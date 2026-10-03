//! Finishing a body: defaults for literals, every type resolved, bounds and literal ranges
//! checked.

use super::Checker;
use crate::defs::*;
use crate::program::Program;
use crate::thir::*;
use crate::traits;
use crate::ty::*;
use wrela_diag::{Diagnostic, codes};

/// Applies `f` to every type in an expression tree.
pub fn walk_tys(e: &mut Expr, f: &mut impl FnMut(&mut TyId)) {
    f(&mut e.ty);
    match &mut e.kind {
        ExprKind::Lit(_)
        | ExprKind::Local(_)
        | ExprKind::Const(_)
        | ExprKind::Closure(_)
        | ExprKind::FromBase
        | ExprKind::Break
        | ExprKind::Continue
        | ExprKind::Error => {}
        ExprKind::Unary(_, x)
        | ExprKind::Field(x, _)
        | ExprKind::Swizzle(x, _)
        | ExprKind::ArrayRepeat(x, _)
        | ExprKind::Convert(x)
        | ExprKind::Take(x)
        | ExprKind::MutArg(x) => walk_tys(x, f),
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => {
            walk_tys(a, f);
            walk_tys(b, f);
        }
        ExprKind::Call(c) => {
            match &mut c.callee {
                Callee::Fn { args, .. } => args.iter_mut().for_each(&mut *f),
                Callee::TraitMethod { self_ty, trait_args, method_args, .. } => {
                    f(self_ty);
                    trait_args.iter_mut().for_each(&mut *f);
                    method_args.iter_mut().for_each(&mut *f);
                }
                _ => {}
            }
            for a in &mut c.args {
                walk_tys(a, f);
            }
        }
        ExprKind::Adt { args, fields, base, .. } => {
            args.iter_mut().for_each(&mut *f);
            for x in fields {
                walk_tys(x, f);
            }
            if let Some(b) = base {
                walk_tys(b, f);
            }
        }
        ExprKind::Tuple(xs) | ExprKind::Array(xs) | ExprKind::Construct(xs) => {
            xs.iter_mut().for_each(|x| walk_tys(x, f))
        }
        ExprKind::Block(b) => walk_block(b, f),
        ExprKind::If { cond, then, else_ } => {
            walk_tys(cond, f);
            walk_block(then, f);
            if let Some(e) = else_ {
                walk_tys(e, f);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_tys(scrutinee, f);
            for a in arms {
                walk_pat(&mut a.pat, f);
                if let Some(g) = &mut a.guard {
                    walk_tys(g, f);
                }
                walk_tys(&mut a.body, f);
            }
        }
        ExprKind::FnRef(_, args) => args.iter_mut().for_each(&mut *f),
        ExprKind::Return(v) => {
            if let Some(v) = v {
                walk_tys(v, f);
            }
        }
        ExprKind::Dispatch(d) => {
            d.kernel_args.iter_mut().for_each(&mut *f);
            walk_tys(&mut d.groups, f);
            for (_, a) in &mut d.args {
                walk_tys(a, f);
            }
        }
        ExprKind::Draw(d) => {
            d.vertex.1.iter_mut().for_each(&mut *f);
            d.fragment.1.iter_mut().for_each(&mut *f);
            walk_tys(&mut d.vertices, f);
            walk_tys(&mut d.instances, f);
            for (_, a) in &mut d.args {
                walk_tys(a, f);
            }
        }
    }
}

fn walk_block(b: &mut Block, f: &mut impl FnMut(&mut TyId)) {
    f(&mut b.ty);
    for s in &mut b.stmts {
        match &mut s.kind {
            StmtKind::Bind { pat, init } => {
                walk_pat(pat, f);
                walk_tys(init, f);
            }
            StmtKind::Assign { place, value, .. } => {
                walk_tys(place, f);
                walk_tys(value, f);
            }
            StmtKind::Expr(e) => walk_tys(e, f),
            StmtKind::While { cond, body } => {
                walk_tys(cond, f);
                walk_block(body, f);
            }
            StmtKind::Loop { body } => walk_block(body, f),
            StmtKind::ForRange { start, end, body, .. } => {
                walk_tys(start, f);
                walk_tys(end, f);
                walk_block(body, f);
            }
            StmtKind::ForEach { array, body, .. } => {
                walk_tys(array, f);
                walk_block(body, f);
            }
        }
    }
    if let Some(t) = &mut b.tail {
        walk_tys(t, f);
    }
}

fn walk_pat(p: &mut Pat, f: &mut impl FnMut(&mut TyId)) {
    f(&mut p.ty);
    match &mut p.kind {
        PatKind::Adt { args, fields, .. } => {
            args.iter_mut().for_each(&mut *f);
            for (_, x) in fields {
                walk_pat(x, f);
            }
        }
        PatKind::Tuple(ps) | PatKind::Or(ps) => ps.iter_mut().for_each(|x| walk_pat(x, f)),
        _ => {}
    }
}

/// Resolves every type, and checks what needed resolved types.
pub(super) fn finish_common(c: &mut Checker) {
    c.settle_projections(true);
    let unknown = c.infer.apply_defaults(&c.p.types);
    if let Some(&span) = unknown.first()
        && !wrela_diag::has_errors(&c.diags)
    {
        c.err(
            Diagnostic::new(codes::E0306, span, "the type of this can't be inferred")
                .with_help("annotate it, as in `let x: f32 = ...`"),
        );
    }
    c.binding_kinds();
    // `-x` of an integer whose type inference settled later: it must be signed.
    for (ty, span) in std::mem::take(&mut c.negated_ints) {
        let ty = c.infer.resolve(&c.p.types, ty);
        if let TyKind::Int(it) = c.p.types.kind(ty)
            && !it.signed()
        {
            let shown = c.p.display_ty(ty);
            c.err(
                Diagnostic::new(codes::E0305, span, format!("`-` doesn't apply to `{shown}`"))
                    .with_note("unsigned integers can't be negative; this one's type was inferred from how it's used later")
                    .with_help("annotate it with a signed type, as in `let x: i32 = ...`"),
            );
        }
    }
    // Integer-only operators on a literal whose type settled later: not a float.
    for (ty, op, span) in std::mem::take(&mut c.int_ops) {
        let ty = c.infer.resolve(&c.p.types, ty);
        if c.p.types.is_float(ty) {
            let shown = c.p.display_ty(ty);
            c.err(
                Diagnostic::new(codes::E0305, span, format!("`{op}` doesn't apply to `{shown}`"))
                    .with_note("it applies to integers; this one's type was inferred from how it's used later")
                    .with_help("annotate it with an integer type, as in `let x: u32 = ...`"),
            );
        }
    }
    // Built-in calls whose literal arguments settled later.
    for (b, tys, span) in std::mem::take(&mut c.builtin_calls) {
        let tys: Vec<TyId> = tys.iter().map(|&t| c.infer.resolve(&c.p.types, t)).collect();
        if tys.iter().any(|&t| c.has_error(t)) {
            continue;
        }
        match b.result(c.p, &tys) {
            Ok(ty) if b.cpu_impl().is_some() && ty == c.p.types.f64 => {
                c.err(crate::builtins::f64_math(b.name(), span));
            }
            Ok(_) => {}
            Err(msg) => c.err(
                Diagnostic::new(codes::E0305, span, msg)
                    .with_note("a literal argument's type was inferred from how it's used later"),
            ),
        }
    }
    // `**` whose base's type settled later.
    for (base, exp, span) in std::mem::take(&mut c.pows) {
        let (b, e) = (c.infer.resolve(&c.p.types, base), c.infer.resolve(&c.p.types, exp));
        if b == c.p.types.f64 {
            c.err(crate::builtins::f64_math("**", span));
        }
        let ok = match c.p.types.kind(b) {
            TyKind::Int(_) => e == c.p.types.u32,
            TyKind::Float(_) | TyKind::Vec(_) => e == b,
            _ => true,
        };
        if !ok && !matches!(c.p.types.kind(e), TyKind::Error) {
            let (bs, es) = (c.p.display_ty(b), c.p.display_ty(e));
            c.err(
                Diagnostic::new(
                    codes::E0305,
                    span,
                    format!("`**` doesn't apply to `{bs}` and `{es}`"),
                )
                .with_note("an integer's exponent is a `u32`; a float's is the same float type"),
            );
        }
    }
    // Integer literals must fit their types.
    let lits = std::mem::take(&mut c.int_literals);
    for (ty, v, neg, span) in lits {
        let t = c.infer.resolve(&c.p.types, ty);
        match *c.p.types.kind(t) {
            TyKind::Int(i) => {
                let max = if neg && i.signed() { i.max() + 1 } else { i.max() };
                if v > max {
                    c.err(
                        Diagnostic::new(
                            codes::E0006,
                            span,
                            format!(
                                "`{}{v}` doesn't fit in `{}`, whose largest value is {}",
                                if neg { "-" } else { "" },
                                i.name(),
                                i.max()
                            ),
                        )
                        .with_help("use a wider integer type"),
                    );
                }
            }
            TyKind::Float(f) => {
                let exact = match f {
                    FloatTy::F32 => v <= (1 << 24) || (v as f32) as u64 == v,
                    FloatTy::F64 => v <= (1 << 53) || (v as f64) as u64 == v,
                };
                if !exact {
                    c.err(
                        Diagnostic::new(
                            codes::E0006,
                            span,
                            format!("`{v}` can't be an `{}` exactly", c.p.display_ty(t)),
                        )
                        .with_help("write it as a float literal to accept the rounding"),
                    );
                }
            }
            _ => {}
        }
    }
    // Float literals must be finite in their types (rounding is accepted).
    for (ty, v, span) in std::mem::take(&mut c.float_literals) {
        let t = c.infer.resolve(&c.p.types, ty);
        let largest = match c.p.types.kind(t) {
            TyKind::Float(FloatTy::F32) if !(v as f32).is_finite() => "3.4e38",
            TyKind::Float(FloatTy::F64) if !v.is_finite() => "1.8e308",
            _ => continue,
        };
        let shown = c.p.display_ty(t);
        c.err(
            Diagnostic::new(
                codes::E0006,
                span,
                format!("this number is too large for an `{shown}`"),
            )
            .with_note(format!("an `{shown}`'s largest value is about {largest}")),
        );
    }
    // Bounds.
    let obligations = std::mem::take(&mut c.obligations);
    for o in obligations {
        let ty = c.infer.resolve(&c.p.types, o.ty);
        if c.p.types.has_vars(ty) || matches!(c.p.types.kind(ty), TyKind::Error) {
            continue;
        }
        let args: Vec<TyId> = o
            .trait_ref
            .args
            .iter()
            .map(|&a| traits::normalize(c.p, c.infer.resolve(&c.p.types, a), None))
            .collect();
        let r = TraitRef { trait_: o.trait_ref.trait_, args };
        let ty = traits::normalize(c.p, ty, None);
        if !traits::implements(c.p, ty, &r) {
            let (t, tr) = (c.p.display_ty(ty), c.p.display_trait_ref(&r));
            let mut d = Diagnostic::new(
                o.code,
                o.span,
                format!("`{t}` doesn't implement `{tr}`, which {} needs", o.why),
            );
            if let Some(l) = c.p.trait_(r.trait_).lang {
                if let TyKind::Adt(a, _) = c.p.types.kind(ty) {
                    let a = *a;
                    let name = &c.p.adt(a).name;
                    d = d.with_help(format!(
                        "opt in where `{name}` is declared: `struct {name}: {tr} {{ ... }}`"
                    ));
                    if let Some((at, text)) = c.p.opt_in_fix(a, &tr) {
                        d = d.with_fix(format!("opt `{name}` in to `{tr}`"), at, text);
                    }
                }
                if l == Lang::Clone || l == Lang::Copy {
                    d = d.with_note(
                        "`Copy` and `Clone` are opted in to in a type's declaration (§6.1)",
                    );
                }
            } else if let TyKind::Param(p) = c.p.types.kind(ty) {
                let pname = &c.p.param(*p).name;
                d = d.with_help(format!("add the bound: `{pname}: {tr}`"));
            }
            c.err(d);
        }
    }
}

pub(super) fn finish(
    mut c: Checker,
    params: Vec<LocalId>,
    mut value: Expr,
    hidden: Option<TyId>,
) -> (Body, Vec<Diagnostic>) {
    finish_common(&mut c);
    let infer = &c.infer;
    let types = &c.p.types;
    let mut resolve = |t: &mut TyId| *t = infer.resolve(types, *t);
    walk_tys(&mut value, &mut resolve);
    let mut locals = std::mem::take(&mut c.locals);
    for l in &mut locals {
        resolve(&mut l.ty);
    }
    let mut closures: Vec<ClosureDef> =
        std::mem::take(&mut c.closures).into_iter().flatten().collect();
    for cl in &mut closures {
        resolve(&mut cl.ret);
        walk_tys(&mut cl.body, &mut resolve);
    }
    let hidden_ret = hidden.map(|h| infer.resolve(types, h));
    stored_callables(c.p, &value, &mut c.diags);
    for cl in &closures {
        stored_callables(c.p, &cl.body, &mut c.diags);
    }
    let body = Body { params, locals, closures, value, hidden_ret };
    (body, c.diags)
}

/// E0510 (§6.7): a function type is a parameter's type only, so a closure or function can't
/// be put in a tuple, an array, a struct or an enum, or be a generic's type argument (generic
/// code could return or keep it). `resolve_type` checks the types written; this checks the
/// types inferred.
fn stored_callables(p: &Program, e: &Expr, out: &mut Vec<Diagnostic>) {
    let holds = |t: TyId| holds_callable(&p.types, t);
    let what = match &e.kind {
        ExprKind::Tuple(_) if holds(e.ty) => Some("stored in a tuple"),
        ExprKind::Array(_) | ExprKind::ArrayRepeat(..) if holds(e.ty) => Some("stored in an array"),
        ExprKind::Adt { .. } if holds(e.ty) => Some("stored in a struct or enum"),
        ExprKind::Call(c) => {
            let generic = match &c.callee {
                Callee::Fn { args, .. } => args.iter().any(|&t| holds(t)),
                Callee::TraitMethod { self_ty, trait_args, method_args, .. } => {
                    holds(*self_ty) || trait_args.iter().chain(method_args).any(|&t| holds(t))
                }
                _ => false,
            };
            generic.then_some("a generic's type argument")
        }
        ExprKind::FnRef(_, args) if args.iter().any(|&t| holds(t)) => {
            Some("a generic's type argument")
        }
        _ => None,
    };
    if let Some(what) = what {
        out.push(
            Diagnostic::new(codes::E0510, e.span, format!("a closure or function can't be {what}"))
                .with_note("closures don't escape the call they're passed to (§6.7); storing or returning one is tier 1 (`@escaping`)")
                .with_help("pass it straight to a parameter of type `fn(..)`"),
        );
    }
    e.for_each_child(&mut |c| match c {
        Child::Expr(x) => stored_callables(p, x, out),
        Child::Block(b) => stored_callables_block(p, b, out),
    });
}

fn stored_callables_block(p: &Program, b: &Block, out: &mut Vec<Diagnostic>) {
    for s in &b.stmts {
        let (exprs, body): (&[&Expr], _) = match &s.kind {
            StmtKind::Bind { init: e, .. } | StmtKind::Expr(e) => (&[e], None),
            StmtKind::Assign { place, value, .. } => (&[place, value], None),
            StmtKind::While { cond, body } => (&[cond], Some(body)),
            StmtKind::Loop { body } => (&[], Some(body)),
            StmtKind::ForRange { start, end, body, .. } => (&[start, end], Some(body)),
            StmtKind::ForEach { array, body, .. } => (&[array], Some(body)),
        };
        exprs.iter().for_each(|e| stored_callables(p, e, out));
        if let Some(body) = body {
            stored_callables_block(p, body, out);
        }
    }
    if let Some(t) = &b.tail {
        stored_callables(p, t, out);
    }
}

/// Whether a value of type `t` is or holds a closure or function.
fn holds_callable(types: &Types, t: TyId) -> bool {
    match types.kind(t) {
        TyKind::Closure(..) | TyKind::FnPtr(..) | TyKind::FnDef(..) => true,
        TyKind::Tuple(ts) | TyKind::Adt(_, ts) => ts.iter().any(|&t| holds_callable(types, t)),
        TyKind::Array(e, _) | TyKind::Slice(e) => holds_callable(types, *e),
        _ => false,
    }
}

pub(super) fn finish_const(mut c: Checker, mut e: Expr) -> ((TyId, Expr), Vec<Diagnostic>) {
    finish_common(&mut c);
    let infer = &c.infer;
    let types = &c.p.types;
    walk_tys(&mut e, &mut |t| *t = infer.resolve(types, *t));
    if let Some(bad) = non_literal(&e) {
        c.err(
            Diagnostic::new(
                codes::E0906,
                bad.span,
                "a constant's value must be a literal value in tier 0",
            )
            .with_note("evaluating calls and arithmetic at compile time is tier 1 (D-073)"),
        );
    }
    ((e.ty, e), c.diags)
}

/// The first part of a typed constant that isn't a literal value.
pub fn non_literal(e: &Expr) -> Option<&Expr> {
    match &e.kind {
        ExprKind::Lit(_) | ExprKind::Const(_) | ExprKind::Error => None,
        ExprKind::Unary(wrela_syntax::ast::UnOp::Neg, x) if matches!(x.kind, ExprKind::Lit(_)) => {
            None
        }
        ExprKind::Adt { fields, base, .. } => {
            fields.iter().chain(base.as_deref()).find_map(non_literal)
        }
        ExprKind::Tuple(xs) | ExprKind::Array(xs) | ExprKind::Construct(xs) => {
            xs.iter().find_map(non_literal)
        }
        ExprKind::ArrayRepeat(x, _) => non_literal(x),
        ExprKind::Convert(x) if matches!(x.kind, ExprKind::Lit(_)) => None,
        _ => Some(e),
    }
}

/// A function whose return type names traits: its body's type, `hidden`, must have them.
pub(super) fn check_opaque(
    p: &Program,
    f: FnId,
    hidden: Option<TyId>,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(hidden) = hidden else { return };
    let Some(traits_) = &p.func(f).opaque else { return };
    let span = p.func(f).sig_span;
    if matches!(p.types.kind(hidden), TyKind::Error) {
        return;
    }
    // One defined by calling itself is reported with the program's other cycles
    // (`opaque_cycles`).
    for r in traits_ {
        if !traits::implements(p, hidden, r) {
            let (t, tr) = (p.display_ty(hidden), p.display_trait_ref(r));
            diags.push(Diagnostic::new(
                codes::E0400,
                span,
                format!("this function returns `{t}`, which doesn't implement `{tr}`"),
            ));
        }
    }
}

/// E0409 for each cycle of functions whose return types name traits and are defined by calling
/// each other (or by calling itself, perhaps through a combinator: `f(n).translate(..)`):
/// no single type is the answer. `hidden` is each such function's body type.
pub fn opaque_cycles(p: &Program, hidden: &[(FnId, TyId)]) -> Vec<Diagnostic> {
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); p.fns.len()];
    for &(f, h) in hidden {
        p.types.any(h, &mut |k| {
            if let TyKind::Opaque(g, _) = k
                && !edges[f.index()].contains(&g.index())
            {
                edges[f.index()].push(g.index());
            }
            false
        });
    }
    let mut out = Vec::new();
    crate::graph::dfs_cycles(
        &edges,
        |&g| g,
        |v| {
            let crate::graph::Visit::Cycle(cycle) = v else { return };
            let last = FnId(cycle[cycle.len() - 1].0 as u32);
            let mut d = if cycle.len() == 1 {
                Diagnostic::new(
                    codes::E0409,
                    p.func(last).sig_span,
                    "this function's return type is defined by calling itself",
                )
            } else {
                let names: Vec<String> =
                    cycle.iter().map(|&(g, _)| format!("`{}`", p.fns[g].name)).collect();
                Diagnostic::new(
                    codes::E0409,
                    p.func(last).sig_span,
                    format!(
                        "the return types of {} are defined by calling each other",
                        crate::collect::and_list(&names)
                    ),
                )
            };
            for &(g, _) in &cycle[..cycle.len() - 1] {
                d = d.with_secondary(p.fns[g].sig_span, "a return type in the cycle");
            }
            out.push(d.with_help("name the concrete type in one of the signatures"));
        },
    );
    out
}
