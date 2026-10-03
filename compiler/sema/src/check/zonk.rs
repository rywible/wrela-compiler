//! Finishing a body: defaults for literals, every type resolved, bounds and literal ranges
//! checked.

use super::Checker;
use crate::defs::*;
use crate::program::Program;
use crate::thir::*;
use crate::traits;
use crate::ty::*;
use wrela_diag::{Diagnostic, Span, codes};

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
    // Indexes whose literal type settled later.
    for (ty, span) in std::mem::take(&mut c.indexes) {
        let ty = c.infer.resolve(&c.p.types, ty);
        if !matches!(c.p.types.kind(ty), TyKind::Int(IntTy::I32 | IntTy::U32) | TyKind::Error) {
            let shown = c.p.display_ty(ty);
            c.err(
                Diagnostic::new(
                    codes::E0312,
                    span,
                    format!("an index must be a `u32` or `i32`, not `{shown}`"),
                )
                .with_note("its type was inferred from how it's used later")
                .with_help("convert it: `u32(i)`"),
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
                // Rounded and converted back, it's the same number. 2⁶⁴ (u64::MAX rounded)
                // doesn't fit back in a u64, where the conversion would saturate.
                const TWO_64: f64 = 18446744073709551616.0;
                let exact = match f {
                    FloatTy::F32 => {
                        let x = v as f32;
                        f64::from(x) < TWO_64 && x as u64 == v
                    }
                    FloatTy::F64 => {
                        let x = v as f64;
                        x < TWO_64 && x as u64 == v
                    }
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
    for (ty, v, f, span) in std::mem::take(&mut c.float_literals) {
        let t = c.infer.resolve(&c.p.types, ty);
        let largest = match c.p.types.kind(t) {
            TyKind::Float(FloatTy::F32) if !f.is_finite() => "3.4e38",
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
    // A type can grow past the bound after the expression that made it was checked, through
    // variables bound later (`a = Some((b, b))` before `b`'s type is known): checked here.
    if !c.too_large
        && let Some(l) = locals.iter().find(|l| types.size(l.ty) > MAX_TYPE_SIZE)
    {
        c.diags.push(Diagnostic::new(
            codes::E0329,
            l.span,
            format!("`{}`'s type is too large: it has more than {MAX_TYPE_SIZE} parts", l.name),
        ));
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

/// E0504 for a `mut` marker that passes nothing mutably: it marks an argument, what a
/// `-> mut T` function returns, or what a `mut` binding projects. And E0908 for `match mut`,
/// which is tier 1 (its bindings would be read-only).
pub(super) fn stray_markers(p: &Program, body: &Body, ret_mut: bool, out: &mut Vec<Diagnostic>) {
    let walk = Markers { p, body, ret_mut };
    walk.expr(&body.value, ret_mut, out);
    for cl in &body.closures {
        walk.expr(&cl.body, false, out);
    }
}

struct Markers<'a> {
    p: &'a Program,
    body: &'a Body,
    ret_mut: bool,
}

impl Markers<'_> {
    /// `here`: whether a marker on `e` itself is one that passes a place mutably.
    fn expr(&self, e: &Expr, here: bool, out: &mut Vec<Diagnostic>) {
        match &e.kind {
            ExprKind::MutArg(inner) => {
                if !here {
                    let marker = Span::new(e.span.file, e.span.start, inner.span.start);
                    out.push(
                        Diagnostic::new(
                            codes::E0504,
                            marker,
                            "`mut` marks a place passed mutably, and nothing takes this one mutably",
                        )
                        .with_note("it goes on an argument for a `mut` parameter, or on what a `-> mut T` function returns")
                        .with_fix("remove `mut`", marker, ""),
                    );
                }
                self.expr(inner, false, out);
            }
            // A function's arguments are checked against its parameters at the call (E0503,
            // E0504); a built-in's or a closure's are checked here.
            ExprKind::Call(c) => {
                let checked = match c.callee {
                    Callee::Fn { .. } | Callee::TraitMethod { .. } => true,
                    // A local that names a function is called as the function is.
                    Callee::Local(l) => {
                        matches!(self.p.types.kind(self.body.local(l).ty), TyKind::FnDef(..))
                    }
                    Callee::Builtin(_) | Callee::Clone => false,
                };
                for (i, a) in c.args.iter().enumerate() {
                    self.expr(a, checked || c.modes.get(i) == Some(&Mode::Mut), out);
                }
            }
            ExprKind::Dispatch(d) => {
                self.expr(&d.groups, false, out);
                d.args.iter().for_each(|(_, a)| self.expr(a, true, out));
            }
            ExprKind::Match { scrutinee, arms } => {
                match &scrutinee.kind {
                    ExprKind::MutArg(inner) => {
                        out.push(
                            Diagnostic::new(
                                codes::E0908,
                                scrutinee.span,
                                "`match mut` is tier 1: the names it binds would be read-only",
                            )
                            .with_help(
                                "match without `mut`, and write a changed value back to the place",
                            ),
                        );
                        self.expr(inner, false, out);
                    }
                    _ => self.expr(scrutinee, false, out),
                }
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.expr(g, false, out);
                    }
                    self.expr(&arm.body, here, out);
                }
            }
            ExprKind::If { cond, then, else_ } => {
                self.expr(cond, false, out);
                self.block(then, here, out);
                if let Some(x) = else_ {
                    self.expr(x, here, out);
                }
            }
            ExprKind::Block(b) => self.block(b, here, out),
            ExprKind::Return(Some(v)) => self.expr(v, self.ret_mut, out),
            _ => e.for_each_child(&mut |c| match c {
                Child::Expr(x) => self.expr(x, false, out),
                Child::Block(b) => self.block(b, false, out),
            }),
        }
    }

    fn block(&self, b: &Block, tail: bool, out: &mut Vec<Diagnostic>) {
        for s in &b.stmts {
            match &s.kind {
                StmtKind::Bind { pat, init } => {
                    let projects_mut = matches!(&pat.kind, PatKind::Bind(l)
                        if matches!(self.body.local(*l).kind, LocalKind::Projection { mutable: true }));
                    self.expr(init, projects_mut, out);
                }
                StmtKind::Assign { place, value, .. } => {
                    self.expr(place, false, out);
                    self.expr(value, false, out);
                }
                StmtKind::Expr(e) => self.expr(e, false, out),
                StmtKind::While { cond, body } => {
                    self.expr(cond, false, out);
                    self.block(body, false, out);
                }
                StmtKind::Loop { body } => self.block(body, false, out),
                StmtKind::ForRange { start, end, body, .. } => {
                    self.expr(start, false, out);
                    self.expr(end, false, out);
                    self.block(body, false, out);
                }
                StmtKind::ForEach { array, body, .. } => {
                    self.expr(array, false, out);
                    self.block(body, false, out);
                }
            }
        }
        if let Some(t) = &b.tail {
            self.expr(t, tail, out);
        }
    }
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
    // `clear`: parts found not to hold one, tested once however often a type shares them.
    fn holds(types: &Types, t: TyId, clear: &mut std::collections::HashSet<TyId>) -> bool {
        if clear.contains(&t) {
            return false;
        }
        let found = match types.kind(t) {
            TyKind::Closure(..) | TyKind::FnPtr(..) | TyKind::FnDef(..) => true,
            TyKind::Tuple(ts) | TyKind::Adt(_, ts) => ts.iter().any(|&t| holds(types, t, clear)),
            TyKind::Array(e, _) | TyKind::Slice(e) => holds(types, *e, clear),
            _ => false,
        };
        if !found {
            clear.insert(t);
        }
        found
    }
    holds(types, t, &mut std::collections::HashSet::new())
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
        // A rejected value has no type for its uses: they stay quiet, and don't take what's
        // made here (a closure belongs to this body) into another body.
        let error = c.p.types.error;
        return ((error, Expr { ty: error, span: e.span, kind: ExprKind::Error }), c.diags);
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
