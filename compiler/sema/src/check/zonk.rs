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
        | ExprKind::Text(_)
        | ExprKind::Embed(_)
        | ExprKind::ConstParam(_)
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
        | ExprKind::Discriminant(x)
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
                Callee::Value(v) => walk_tys(v, f),
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
        ExprKind::Match { scrutinee, arms, .. } => {
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
            if let Some(i) = &mut d.indirect {
                walk_tys(i, f);
            }
            for (_, _, a) in &mut d.args {
                walk_tys(a, f);
            }
        }
    }
}

fn walk_block(b: &mut Block, f: &mut impl FnMut(&mut TyId)) {
    f(&mut b.ty);
    for s in &mut b.stmts {
        match &mut s.kind {
            StmtKind::Bind { pat, init, else_ } => {
                walk_pat(pat, f);
                walk_tys(init, f);
                if let Some(b) = else_ {
                    walk_block(b, f);
                }
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

/// Infers generic parameters that only a bound mentions, which a bound recorded before its
/// type was known (a dispatch's kernel's, a draw's shaders') couldn't infer then: until nothing
/// more is learned.
fn infer_from_bounds(c: &mut Checker) {
    let open = |c: &Checker| {
        c.obligations
            .iter()
            .flat_map(|o| o.trait_ref.args.iter())
            .filter(|&&a| c.p.types.has_vars(c.infer.resolve(&c.p.types, a)))
            .count()
    };
    loop {
        let before = open(c);
        if before == 0 {
            return;
        }
        let pending: Vec<(TyId, TraitRef)> =
            c.obligations.iter().map(|o| (o.ty, o.trait_ref.clone())).collect();
        for (ty, r) in pending {
            c.infer_from_bound(ty, &r);
        }
        if open(c) == before {
            return;
        }
    }
}

/// Resolves every type, and checks what needed resolved types.
pub(super) fn finish_common(c: &mut Checker) {
    infer_from_bounds(c);
    c.settle_projections(true);
    let unknown = c.infer.apply_defaults(&c.p.types);
    // A signature with an error in it was reported where it's written: what the body can't
    // infer from it follows from that.
    let bad_sig = c.fn_id.is_some_and(|f| {
        let def = c.p.func(f);
        let error = |t: TyId| c.p.types.any(t, &mut |k| matches!(k, TyKind::Error));
        error(def.ret) || def.params.iter().any(|ps| error(ps.ty))
    });
    if let Some(&span) = unknown.first()
        && !wrela_diag::has_errors(&c.diags)
        && !bad_sig
    {
        c.err(
            Diagnostic::new(codes::E0306, span, "the type of this can't be inferred")
                .with_help("annotate it, as in `let x: f32 = ...`"),
        );
    }
    c.binding_kinds();
    // `let x = place` owns: only a `Copy` value can be copied out (§6.3).
    for (id, at, keyword) in std::mem::take(&mut c.owned_lets) {
        let ty = c.infer.resolve(&c.p.types, c.locals[id.index()].ty);
        if matches!(c.p.types.kind(ty), TyKind::Error)
            || crate::mir::is_callable(&c.p.types, ty)
            || crate::traits::implements_builtin(c.p, ty, crate::defs::Lang::Copy)
        {
            continue;
        }
        let name = c.locals[id.index()].name.clone();
        let shown = c.p.display_ty(ty);
        let mut d = Diagnostic::new(
            codes::E0518,
            at,
            format!("`let` owns its value, and `{shown}` isn't `Copy`, so `{name}` can't be a copy of this place"),
        )
        .with_note("`let x = place` copies only a `Copy` value (§6.3)")
        .with_fix("read it in place: `borrow`", keyword, "borrow");
        if crate::traits::implements_builtin(c.p, ty, crate::defs::Lang::Clone) {
            d = d.with_fix("copy it: `.clone()`", at.shrink_to_end(), ".clone()");
        }
        d = d.with_fix("move it: `take`", at.shrink_to_start(), "take ");
        c.err(d);
        // Read as a projection, so nothing else is reported about it.
        c.locals[id.index()].kind = LocalKind::Projection { mutable: false };
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
            // A library's own message replaces the default (§7), at the user's code.
            if let Some(msg) = c.p.custom_message(ty, &r) {
                c.diags.push(
                    Diagnostic::new(o.code, o.span, msg).with_note(format!(
                        "`{t}` doesn't implement `{tr}`, which {} needs",
                        o.why
                    )),
                );
                continue;
            }
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
    let mut closures: Vec<ClosureDef> = Vec::new();
    for (k, cl) in std::mem::take(&mut c.closures).into_iter().enumerate() {
        let Some(mut cl) = cl else { continue };
        resolve(&mut cl.ret);
        walk_tys(&mut cl.body, &mut resolve);
        let sig = cl.params.iter().map(|p| locals[p.index()].ty).collect();
        let id = ClosureRef { owner: c.fn_id, id: ClosureId(k as u32) };
        c.p.closure_sigs.borrow_mut().insert(id, (sig, cl.ret));
        closures.push(cl);
    }
    let hidden_ret = hidden.map(|h| infer.resolve(types, h));
    let own = Own { owner: c.fn_id, closures: &closures, locals: &locals };
    stored_callables(c.p, &own, &value, &mut c.diags);
    for cl in &closures {
        stored_callables(c.p, &own, &cl.body, &mut c.diags);
    }
    let body = Body { params, locals, closures, value, hidden_ret };
    (body, c.diags)
}

/// E0504 for a `mut` marker that passes nothing mutably: it marks an argument, what a
/// `-> mut T` function returns, or what a `mut` binding projects.
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
                    // A call through a local is checked against the modes its type gives.
                    Callee::Local(_) | Callee::Value(_) => true,
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
            ExprKind::Match { scrutinee, arms, .. } => {
                self.expr(scrutinee, false, out);
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
            // A borrow struct's fields are checked against their modes where it's checked: a
            // `mut` one takes `mut place`.
            ExprKind::Adt { adt, fields, .. } if self.p.adt(*adt).borrow => {
                fields.iter().for_each(|f| self.expr(f, true, out));
            }
            _ => e.for_each_child(&mut |c| match c {
                Child::Expr(x) => self.expr(x, false, out),
                Child::Block(b) => self.block(b, false, out),
            }),
        }
    }

    fn block(&self, b: &Block, tail: bool, out: &mut Vec<Diagnostic>) {
        for s in &b.stmts {
            match &s.kind {
                StmtKind::Bind { pat, init, else_ } => {
                    let projects_mut = matches!(&pat.kind, PatKind::Bind(l)
                        if matches!(self.body.local(*l).kind, LocalKind::Projection { mutable: true }));
                    self.expr(init, projects_mut, out);
                    if let Some(b) = else_ {
                        self.block(b, false, out);
                    }
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

/// The function being finished, for whether its closures can be stored.
struct Own<'a> {
    owner: Option<FnId>,
    closures: &'a [ClosureDef],
    locals: &'a [LocalDecl],
}

impl Own<'_> {
    /// The first capture that keeps closure `c` from being stored: one it writes, or a value
    /// that isn't `Copy` (a projection). `None` if it captures only `Copy` values it reads.
    fn projection_capture(&self, p: &Program, c: ClosureRef) -> Option<LocalId> {
        if c.owner != self.owner {
            return None; // checked where it's made
        }
        let def = self.closures.get(c.id.0 as usize)?;
        def.captures.iter().find_map(|&(l, written)| {
            let ty = self.locals[l.index()].ty;
            (written || !traits::implements_builtin(p, ty, Lang::Copy)).then_some(l)
        })
    }

    /// What in a value of type `t` can't be stored: a `fn(..)` parameter's value, or a closure
    /// that captures a projection (with that capture).
    fn unstorable(&self, p: &Program, t: TyId) -> Option<Option<LocalId>> {
        fn go(
            own: &Own,
            p: &Program,
            t: TyId,
            clear: &mut std::collections::HashSet<TyId>,
        ) -> Option<Option<LocalId>> {
            if clear.contains(&t) {
                return None;
            }
            let found = match p.types.kind(t) {
                TyKind::FnPtr(..) => Some(None),
                &TyKind::Closure(c, _) => own.projection_capture(p, c).map(Some),
                TyKind::Tuple(ts) | TyKind::Adt(_, ts) => {
                    ts.iter().find_map(|&t| go(own, p, t, clear))
                }
                TyKind::Array(e, _) | TyKind::ArrayN(e, _) | TyKind::Slice(e) => {
                    go(own, p, *e, clear)
                }
                _ => None,
            };
            if found.is_none() {
                clear.insert(t);
            }
            found
        }
        go(self, p, t, &mut std::collections::HashSet::new())
    }
}

/// E0510 (§6.7): a closure that captures a projection, and a `fn(..)` parameter's value, can't
/// be put in a tuple, an array, a struct or an enum, or be a generic's type argument (generic
/// code could return or keep it). A closure that captures only `Copy` values can: it holds
/// copies of them. `resolve_type` checks the types written; this checks the types inferred.
fn stored_callables(p: &Program, own: &Own, e: &Expr, out: &mut Vec<Diagnostic>) {
    let bad = |t: TyId| own.unstorable(p, t);
    let what = match &e.kind {
        ExprKind::Tuple(_) => bad(e.ty).map(|c| ("stored in a tuple", c)),
        ExprKind::Array(_) | ExprKind::ArrayRepeat(..) => {
            bad(e.ty).map(|c| ("stored in an array", c))
        }
        ExprKind::Adt { .. } => bad(e.ty).map(|c| ("stored in a struct or enum", c)),
        ExprKind::Call(c) => {
            let generic = match &c.callee {
                Callee::Fn { args, .. } => args.iter().find_map(|&t| bad(t)),
                Callee::TraitMethod { self_ty, trait_args, method_args, .. } => {
                    std::iter::once(self_ty)
                        .chain(trait_args)
                        .chain(method_args)
                        .find_map(|&t| bad(t))
                }
                _ => None,
            };
            generic.map(|c| ("a generic's type argument", c))
        }
        ExprKind::FnRef(_, args) => {
            args.iter().find_map(|&t| bad(t)).map(|c| ("a generic's type argument", c))
        }
        _ => None,
    };
    if let Some((what, capture)) = what {
        let d = match capture {
            Some(l) => {
                let name = &own.locals[l.index()].name;
                Diagnostic::new(
                    codes::E0510,
                    e.span,
                    format!("a closure that captures `{name}`, a projection, can't be {what}"),
                )
                .with_note("a closure that captures only `Copy` values holds copies of them, and can be stored; one that borrows or changes what it uses can only be passed down (§6.7)")
                .with_help(format!("copy what it needs into a local first (`let x = {name}.clone()`), or pass the closure straight to a parameter"))
            }
            None => Diagnostic::new(
                codes::E0510,
                e.span,
                format!("a `fn(..)` parameter's value can't be {what}"),
            )
            .with_note("a function type is a parameter's type only (§6.7)")
            .with_help("take a generic `F: fn(..)` instead: its values can be stored"),
        };
        out.push(d);
    }
    e.for_each_child(&mut |c| match c {
        Child::Expr(x) => stored_callables(p, own, x, out),
        Child::Block(b) => stored_callables_block(p, own, b, out),
    });
}

fn stored_callables_block(p: &Program, own: &Own, b: &Block, out: &mut Vec<Diagnostic>) {
    for s in &b.stmts {
        let (exprs, body): (&[&Expr], _) = match &s.kind {
            StmtKind::Bind { init: e, else_, .. } => (&[e], else_.as_ref()),
            StmtKind::Expr(e) => (&[e], None),
            StmtKind::Assign { place, value, .. } => (&[place, value], None),
            StmtKind::While { cond, body } => (&[cond], Some(body)),
            StmtKind::Loop { body } => (&[], Some(body)),
            StmtKind::ForRange { start, end, body, .. } => (&[start, end], Some(body)),
            StmtKind::ForEach { array, body, .. } => (&[array], Some(body)),
        };
        exprs.iter().for_each(|e| stored_callables(p, own, e, out));
        if let Some(body) = body {
            stored_callables_block(p, own, body, out);
        }
    }
    if let Some(t) = &b.tail {
        stored_callables(p, own, t, out);
    }
}

/// Finishes a constant's value, checked as the body of the function that computes it (§10).
/// The value must be one that lives as long as the program (E0332).
pub(super) fn finish_const(c: Checker, e: Expr, ret: TyId) -> (TyId, Body, Vec<Diagnostic>) {
    let p = c.p;
    let declared = c.infer.resolve(&p.types, ret);
    let (body, mut diags) = finish(c, Vec::new(), e, None);
    let ty = match p.types.kind(body.value.ty) {
        TyKind::Never => declared,
        _ => body.value.ty,
    };
    if let Some(what) = not_constant(p, ty, &mut std::collections::HashSet::new()) {
        // Its uses stay quiet: the error type has every property.
        let error = p.types.error;
        diags.push(
            Diagnostic::new(
                codes::E0332,
                body.value.span,
                format!("a constant can't hold {what}: its type is `{}`", p.display_ty(ty)),
            )
            .with_note("a constant is an owned value that lives as long as the program; the build computes it and lays it out as read-only data (§10)"),
        );
        return (error, body, diags);
    }
    (ty, body, diags)
}

/// What in a value of type `t` can't be part of a constant: a function or closure, a run or
/// `str` (a projection of another place), or a borrow struct (it holds projections).
fn not_constant(
    p: &Program,
    t: TyId,
    seen: &mut std::collections::HashSet<TyId>,
) -> Option<&'static str> {
    if !seen.insert(t) {
        return None;
    }
    match p.types.kind(t) {
        TyKind::FnPtr(..) | TyKind::Closure(..) | TyKind::FnDef(..) => {
            Some("a function or a closure")
        }
        TyKind::Slice(_) | TyKind::Str => Some("a run, a projection of another place"),
        TyKind::Tuple(ts) => ts.clone().into_iter().find_map(|t| not_constant(p, t, seen)),
        TyKind::Array(e, _) | TyKind::ArrayN(e, _) => not_constant(p, *e, seen),
        TyKind::Adt(a, args) => {
            let (a, args) = (*a, args.clone());
            if p.adt(a).borrow {
                return Some("a borrow struct, which holds projections");
            }
            // The type arguments too: a `Vec<T>`'s elements are no field's type.
            let variants = match p.adt(a).variants().len() {
                0 => vec![None],
                n => (0..n as u32).map(Some).collect(),
            };
            let fields = variants.into_iter().flat_map(|v| p.fields_of(a, &args, v));
            args.iter()
                .copied()
                .chain(fields.collect::<Vec<_>>())
                .find_map(|t| not_constant(p, t, seen))
        }
        _ => None,
    }
}

/// The first part of a typed constant that isn't a literal value: with one, the build computes
/// the constant (`crate::mir::build::is_literal`, but a constant it names may be computed).
pub fn non_literal(e: &Expr) -> Option<&Expr> {
    match &e.kind {
        ExprKind::Lit(_)
        | ExprKind::Text(_)
        | ExprKind::Embed(_)
        | ExprKind::Const(_)
        | ExprKind::Error => None,
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
