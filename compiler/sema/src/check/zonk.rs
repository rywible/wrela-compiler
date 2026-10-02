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
        ExprKind::Adt { args, fields, .. } => {
            args.iter_mut().for_each(&mut *f);
            for x in fields {
                walk_tys(x, f);
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
            for g in &mut d.groups {
                walk_tys(g, f);
            }
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
    let unknown = c.infer.apply_defaults(&c.p.types);
    let has_errors = c.diags.iter().any(|d| d.is_error());
    if !has_errors {
        for span in unknown.into_iter().take(1) {
            c.err(
                Diagnostic::new(codes::E0306, span, "the type of this can't be inferred")
                    .with_help("annotate it, as in `let x: f32 = ...`"),
            );
        }
    }
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
    // Integer literals must fit their types.
    let lits = std::mem::take(&mut c.int_literals);
    for (ty, v, neg, span) in lits {
        let t = c.infer.resolve(&c.p.types, ty);
        match c.p.types.kind(t).clone() {
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
            TyKind::Float(FloatTy::F32) if v > (1 << 24) && (v as f32) as u64 != v => {
                c.err(
                    Diagnostic::new(codes::E0006, span, format!("`{v}` can't be an `f32` exactly"))
                        .with_help("write it as a float literal to accept the rounding"),
                );
            }
            _ => {}
        }
    }
    // Bounds.
    let obligations = std::mem::take(&mut c.obligations);
    for o in obligations {
        let ty = c.infer.resolve(&c.p.types, o.ty);
        let args: Vec<TyId> =
            o.trait_ref.args.iter().map(|&a| c.infer.resolve(&c.p.types, a)).collect();
        let r = TraitRef { trait_: o.trait_ref.trait_, args };
        if c.p.types.has_vars(ty) || matches!(c.p.types.kind(ty), TyKind::Error) {
            continue;
        }
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
                    let name = c.p.adt(a).name.clone();
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
                let pname = c.p.param(*p).name.clone();
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
    f: FnId,
) -> (Option<Body>, Vec<Diagnostic>) {
    finish_common(&mut c);
    let infer = c.infer.clone();
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
    let hidden_ret = hidden.map(|h| {
        let mut h = h;
        resolve(&mut h);
        h
    });
    let _ = f;
    let body = Body { params, locals, closures, value, hidden_ret };
    let diags = c.diags;
    (Some(body), diags)
}

pub(super) fn finish_const(mut c: Checker, mut e: Expr) -> (Option<(TyId, Expr)>, Vec<Diagnostic>) {
    finish_common(&mut c);
    let infer = c.infer.clone();
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
    let diags = c.diags;
    (Some((e.ty, e)), diags)
}

/// The first part of a typed constant that isn't a literal value.
pub fn non_literal(e: &Expr) -> Option<&Expr> {
    match &e.kind {
        ExprKind::Lit(_) | ExprKind::Const(_) | ExprKind::Error => None,
        ExprKind::Unary(wrela_syntax::ast::UnOp::Neg, x) if matches!(x.kind, ExprKind::Lit(_)) => {
            None
        }
        ExprKind::Adt { fields, .. } => fields.iter().find_map(non_literal),
        ExprKind::Tuple(xs) | ExprKind::Array(xs) | ExprKind::Construct(xs) => {
            xs.iter().find_map(non_literal)
        }
        ExprKind::ArrayRepeat(x, _) => non_literal(x),
        ExprKind::Convert(x) if matches!(x.kind, ExprKind::Lit(_)) => None,
        _ => Some(e),
    }
}

/// A function whose return type names traits: its body's type must have them.
pub(super) fn check_opaque(p: &Program, f: FnId, body: &mut Body, diags: &mut Vec<Diagnostic>) {
    let Some(hidden) = body.hidden_ret else { return };
    let Some(traits_) = p.func(f).opaque.clone() else { return };
    let span = p.func(f).sig_span;
    if matches!(p.types.kind(hidden), TyKind::Error) {
        return;
    }
    if let TyKind::Opaque(g, _) = p.types.kind(hidden)
        && *g == f
    {
        diags.push(
            Diagnostic::new(
                codes::E0409,
                span,
                "this function's return type is defined by calling itself",
            )
            .with_help("name the concrete type in the signature"),
        );
        return;
    }
    for r in traits_ {
        if !traits::implements(p, hidden, &r) {
            let (t, tr) = (p.display_ty(hidden), p.display_trait_ref(&r));
            diags.push(Diagnostic::new(
                codes::E0400,
                span,
                format!("this function returns `{t}`, which doesn't implement `{tr}`"),
            ));
        }
    }
}
