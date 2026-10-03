//! Calls: functions, methods, built-ins, constructors, conversions, and the GPU intrinsics.

use super::expr::ValueRes;
use super::{Checker, zonk};
use crate::builtins::{self, BuiltinFn, BuiltinTy};
use crate::defs::*;
use crate::resolve;
use crate::thir::*;
use crate::traits;
use crate::ty::*;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_syntax::ast;

/// A parameter as call checking sees it.
#[derive(Clone, Copy)]
pub(crate) struct CallParam<'p> {
    pub name: &'p str,
    pub mode: Mode,
    pub ty: TyId,
    pub default: Option<&'p ast::Expr>,
    /// The module the function is declared in: its default's names are that module's.
    pub module: ModuleId,
}

/// What a method call resolved to.
enum Pick {
    /// An inherent method, with its impl's arguments solved against the receiver.
    Inherent(FnId, Vec<TyId>),
    Trait {
        method: FnId,
        trait_args: Vec<TyId>,
    },
    Builtin(BuiltinFn),
    Clone,
}

impl<'p> Checker<'p> {
    /// A call that can't be checked against its callee: its arguments are still checked (for
    /// their own errors), and the call is an error expression.
    fn failed_call(&mut self, args: &[ast::Arg], span: Span) -> Expr {
        for a in args {
            self.check_expr(&a.value, None);
        }
        self.error_expr(span)
    }

    pub(crate) fn check_call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Arg],
        expected: Option<TyId>,
        span: Span,
    ) -> Expr {
        let ast::ExprKind::Path(path) = &callee.kind else {
            let c = self.check_expr(callee, None);
            if !matches!(c.kind, ExprKind::Error) {
                let shown = self.display(c.ty);
                self.err(Diagnostic::new(
                    codes::E0310,
                    c.span,
                    format!("`{shown}` can't be called"),
                ));
            }
            return self.failed_call(args, span);
        };
        let Some(res) = self.resolve_value_path(path) else {
            return self.failed_call(args, span);
        };
        match res {
            ValueRes::Local(l) => self.call_local(l, args, expected, span),
            ValueRes::Item(Res::Fn(f)) => {
                match self.p.func(f).lang {
                    Some(Lang::Dispatch) => return self.check_dispatch(args, span),
                    Some(Lang::Draw) => return self.check_draw(args, span),
                    _ => {}
                }
                if let Some((e, _)) = self.p.func(f).attrs.entry {
                    let kind = match e {
                        Entry::Compute(_) => "`@compute` kernel; record it with `dispatch(...)`",
                        Entry::Vertex | Entry::Fragment => {
                            "render entry point; record it with `draw(...)`"
                        }
                    };
                    self.err(
                        Diagnostic::new(
                            codes::E0606,
                            callee.span,
                            format!("`{}` is a GPU {kind}", self.p.func(f).name),
                        )
                        .with_note("entry points run on the GPU and can't be called directly"),
                    );
                    // Its arguments are still checked, but not against its parameters.
                    return self.failed_call(args, span);
                }
                let gen_args = self.fresh_fn_args(f, path, span);
                self.call_fn(f, gen_args, None, args, expected, span)
            }
            ValueRes::Item(Res::Variant(a, v)) => {
                self.call_variant(a, v, path, args, expected, span)
            }
            ValueRes::Item(Res::BuiltinFn(b)) => self.call_builtin(b, None, args, span),
            ValueRes::Item(Res::BuiltinTy(t)) => self.call_type(t, args, span),
            ValueRes::TypeRelative(ty, name) => {
                let generics = path.segments.last().and_then(|s| s.generics.as_deref());
                self.call_type_relative(ty, &name, generics, args, expected, span)
            }
            ValueRes::TraitRelative(t, name) => {
                let ntraits = self.p.trait_(t).generics.len();
                let trait_args: Vec<TyId> =
                    (0..ntraits).map(|_| self.new_var(VarKind::General, span)).collect();
                let own = TraitRef { trait_: t, args: trait_args };
                // The method is the trait's own or a supertrait's (`Sub::base(x)`).
                let error = self.p.types.error;
                if traits::trait_method_via(self.p, error, own.clone(), &name.name).is_none() {
                    self.err(Diagnostic::new(
                        codes::E0207,
                        name.span,
                        format!("`{}` has no method `{}`", self.p.trait_(t).name, name.name),
                    ));
                    return self.error_expr(span);
                }
                // `Trait::method(x, ...)`: the first argument is the receiver.
                let Some(first) = args.first() else {
                    self.err(Diagnostic::new(
                        codes::E0303,
                        span,
                        "this call needs the value to call the method on",
                    ));
                    return self.error_expr(span);
                };
                let recv = self.check_expr(&first.value, None);
                let rt = self.shallow(recv.ty);
                self.obligation(
                    rt,
                    own.clone(),
                    recv.span,
                    format!("calling `{}::{}`", self.p.trait_(t).name, name.name),
                );
                let Some((m, via)) = traits::trait_method_via(self.p, rt, own, &name.name) else {
                    unreachable!("the method was found above")
                };
                let generics = path.segments.last().and_then(|s| s.generics.as_deref());
                self.finish_method(
                    Pick::Trait { method: m, trait_args: via.args },
                    recv,
                    generics,
                    &args[1..],
                    expected,
                    span,
                    name.span,
                )
            }
            ValueRes::Item(r) => {
                let what = match r {
                    Res::Adt(a) if !self.p.adt(a).is_enum() => format!(
                        "`{}` is a struct; build one with `{} {{ ... }}`",
                        self.p.adt(a).name,
                        self.p.adt(a).name
                    ),
                    Res::Adt(a) => {
                        format!("`{}` is an enum; call one of its variants", self.p.adt(a).name)
                    }
                    Res::Const(c) => {
                        format!("the constant `{}` can't be called", self.p.const_(c).name)
                    }
                    Res::Module(m) => {
                        format!("the module `{}` can't be called", self.p.module(m).name())
                    }
                    Res::Trait(t) => {
                        format!("the trait `{}` can't be called", self.p.trait_(t).name)
                    }
                    _ => "this can't be called".into(),
                };
                self.err(Diagnostic::new(codes::E0310, callee.span, what));
                self.failed_call(args, span)
            }
        }
    }

    fn call_local(
        &mut self,
        l: LocalId,
        args: &[ast::Arg],
        expected: Option<TyId>,
        span: Span,
    ) -> Expr {
        let ty = self.locals[l.index()].ty;
        let (params, ret) = match self.kind(ty) {
            TyKind::FnPtr(ps, r) => (ps.clone(), *r),
            // A named function: its arguments are matched as a direct call matches them (names,
            // defaults, modes), and the local stands for the function.
            &TyKind::FnDef(f, ref gen_args) => {
                let gen_args = gen_args.clone();
                let mut call = self.call_fn(f, gen_args, None, args, expected, span);
                if let ExprKind::Call(c) = &mut call.kind {
                    c.callee = Callee::Local(l);
                }
                self.note_use(l, false);
                return call;
            }
            TyKind::Closure(c, _) => match self.closure_def(*c) {
                Some(def) => {
                    let ps = def.params.iter().map(|p| self.locals[p.index()].ty).collect();
                    (ps, def.ret)
                }
                None => {
                    self.err(Diagnostic::new(codes::E0310, span, "a closure can't call itself"));
                    return self.error_expr(span);
                }
            },
            // Its type's error is reported.
            TyKind::Error => return self.failed_call(args, span),
            _ => {
                let shown = self.display(ty);
                let name = self.locals[l.index()].name.clone();
                self.err(Diagnostic::new(
                    codes::E0310,
                    span,
                    format!("`{name}` is a `{shown}`, which can't be called"),
                ));
                return self.error_expr(span);
            }
        };
        if args.len() != params.len() {
            self.err(Diagnostic::new(
                codes::E0301,
                span,
                format!("this takes {} arguments, not {}", params.len(), args.len()),
            ));
        }
        let mut out = Vec::new();
        for (a, &pt) in args.iter().zip(&params) {
            if let Some(n) = &a.name {
                self.err(Diagnostic::new(
                    codes::E0302,
                    n.span,
                    "a closure's arguments are positional",
                ));
            }
            out.push(self.check_expect(&a.value, pt));
        }
        self.note_use(l, false);
        let modes = vec![Mode::Borrow; out.len()];
        let n_args = out.len();
        Expr {
            ty: ret,
            span,
            kind: ExprKind::Call(Call {
                callee: Callee::Local(l),
                args: out,
                modes,
                order: (0..n_args).collect(),
                receiver: false,
                ret_mode: RetMode::Owned,
            }),
        }
    }

    /// `f`'s parameters and return type, with `subst` applied.
    fn sig_under(&self, f: FnId, subst: &Subst) -> (Vec<CallParam<'p>>, TyId) {
        let p = self.p;
        let def = p.func(f);
        let module = def.module;
        let params = def
            .params
            .iter()
            .map(|ps| {
                let ty = traits::normalize(p, p.types.subst(ps.ty, subst), None);
                let default = ps.default.as_ref();
                CallParam { name: &ps.name, mode: ps.mode, ty, default, module }
            })
            .collect();
        let ret = traits::normalize(p, p.types.subst(def.ret, subst), None);
        (params, ret)
    }

    /// A call's signature with each projection on a type not inferred yet (`T::Out` while `T`
    /// is a variable) replaced by a variable, which [`Self::settle_projections`] binds once
    /// the type is known.
    fn defer_projections(
        &mut self,
        f: FnId,
        params: Vec<CallParam<'p>>,
        ret: TyId,
        span: Span,
    ) -> (Vec<CallParam<'p>>, TyId) {
        let p = self.p;
        // The declared signature says whether there are any, cheaply: the instantiated types
        // can be large.
        let def = p.func(f);
        let is_projection = |k: &TyKind| matches!(k, TyKind::Projection { .. });
        if !def
            .params
            .iter()
            .map(|ps| ps.ty)
            .chain([def.ret])
            .any(|t| p.types.any(t, &mut { is_projection }))
        {
            return (params, ret);
        }
        let defer = |this: &mut Self, t: TyId| {
            p.types.map(t, &mut |types, x| match types.kind(x) {
                TyKind::Projection { self_ty, trait_args, .. }
                    if types.has_vars(*self_ty)
                        || trait_args.iter().any(|&a| types.has_vars(a)) =>
                {
                    let v = this.new_var(VarKind::General, span);
                    this.projections.push((v, x, span));
                    Some(v)
                }
                _ => None,
            })
        };
        let params =
            params.into_iter().map(|cp| CallParam { ty: defer(self, cp.ty), ..cp }).collect();
        let ret = defer(self, ret);
        (params, ret)
    }

    /// Binds the variable of each deferred projection whose types are known by now to what it
    /// normalizes to: an impl's associated type, or the projection itself on a generic
    /// parameter. `last`: the body is done, so a literal it waits for takes its default type.
    pub(crate) fn settle_projections(&mut self, last: bool) {
        let mut i = 0;
        while i < self.projections.len() {
            let (var, proj, span) = self.projections[i];
            let mut r = self.infer.resolve(&self.p.types, proj);
            if last && self.p.types.has_vars(r) {
                let mut literals = Vec::new();
                self.p.types.any(r, &mut |k| {
                    if let TyKind::Var(v) = k {
                        literals.push(self.p.types.var(*v));
                    }
                    false
                });
                for v in literals {
                    let default = match self.infer.var_kind(&self.p.types, v) {
                        Some(VarKind::Int) => self.p.types.i32,
                        Some(VarKind::Float) => self.p.types.f32,
                        _ => continue,
                    };
                    self.try_unify(v, default);
                }
                r = self.infer.resolve(&self.p.types, proj);
            }
            if self.p.types.has_vars(r) {
                i += 1;
                continue;
            }
            self.projections.remove(i);
            let n = traits::normalize(self.p, r, None);
            self.expect(var, n, span);
        }
    }

    /// The parameters of `f`, with its generics substituted by `gen_args`.
    fn fn_params(&self, f: FnId, gen_args: &[TyId]) -> (Vec<CallParam<'p>>, TyId, RetMode) {
        let subst = Subst::from_pairs(&self.p.fn_all_generics(f), gen_args);
        let (params, ret) = self.sig_under(f, &subst);
        (params, ret, self.p.func(f).ret_mode)
    }

    /// Bounds on `f`'s generics become obligations.
    fn fn_obligations(&mut self, f: FnId, gen_args: &[TyId], span: Span) {
        let all = self.p.fn_all_generics(f);
        let subst = Subst::from_pairs(&all, gen_args);
        let name = self.p.fn_display_name(f);
        self.bound_obligations(&all, gen_args, &subst, &vec![span; all.len()], &name);
    }

    /// Bounds on the generic parameters `params` become obligations on their arguments `args`:
    /// each bound with `subst` applied, reported at the parameter's span in `spans`, as needed
    /// by `` `P` in `name` ``.
    fn bound_obligations(
        &mut self,
        params: &[ParamId],
        args: &[TyId],
        subst: &Subst,
        spans: &[Span],
        name: &str,
    ) {
        let p = self.p;
        for ((&g, &a), &span) in params.iter().zip(args).zip(spans) {
            let param = p.param(g);
            if param.is_self {
                continue; // the receiver's own trait: the method was found through it
            }
            for b in &param.bounds {
                let r = b.subst(&p.types, subst);
                self.obligation(a, r, span, format!("`{}` in `{name}`", param.name));
            }
        }
    }

    pub(crate) fn call_fn(
        &mut self,
        f: FnId,
        gen_args: Vec<TyId>,
        receiver: Option<Expr>,
        args: &[ast::Arg],
        expected: Option<TyId>,
        span: Span,
    ) -> Expr {
        let all = self.p.fn_all_generics(f);
        let subst = Subst::from_pairs(&all, &gen_args);
        let (params, ret) = self.sig_under(f, &subst);
        let (params, ret) = self.defer_projections(f, params, ret, span);
        if let Some(e) = expected
            && !matches!(self.kind(ret), TyKind::Opaque(..))
        {
            // Let the expected type guide inference of the generics (`let x: T = f()`), but
            // only if it fits: a mismatch is reported where the value is used.
            self.try_unify(ret, e);
        }
        let name = self.p.fn_display_name(f);
        let has_receiver = receiver.is_some();
        let (call_args, modes, order) = self.match_args(&name, &params, receiver, args, span, f);
        // A bound on a generic that's a parameter's whole type is reported at that argument.
        let def = self.p.func(f);
        let spans: Vec<Span> = all
            .iter()
            .map(|&g| {
                def.params
                    .iter()
                    .position(|ps| matches!(self.p.types.kind(ps.ty), TyKind::Param(q) if *q == g))
                    .and_then(|j| call_args.get(j))
                    .map_or(span, |a| a.span)
            })
            .collect();
        self.bound_obligations(&all, &gen_args, &subst, &spans, &name);
        self.settle_projections(false);
        Expr {
            ty: ret,
            span,
            kind: ExprKind::Call(Call {
                callee: Callee::Fn { func: f, args: gen_args },
                args: call_args,
                modes,
                order,
                receiver: has_receiver,
                ret_mode: def.ret_mode,
            }),
        }
    }

    /// Matches arguments to parameters: positional first, then named (D-039), then defaults.
    pub(crate) fn match_args(
        &mut self,
        name: &str,
        params: &[CallParam],
        receiver: Option<Expr>,
        args: &[ast::Arg],
        span: Span,
        f: FnId,
    ) -> (Vec<Expr>, Vec<Mode>, Vec<usize>) {
        let offset = usize::from(receiver.is_some());
        let mut slots: Vec<Option<Expr>> = vec![None; params.len()];
        let mut suggested: Vec<&str> = Vec::new();
        // The order the arguments are written in: evaluation order (language.md §3).
        let mut order: Vec<usize> = Vec::new();
        if let Some(r) = receiver {
            slots[0] = Some(r);
            order.push(0);
        }
        let mut next = offset;
        for a in args {
            let idx = match &a.name {
                None => {
                    if next >= params.len() {
                        let given = args.len();
                        let takes = params.len() - offset;
                        self.err(
                            Diagnostic::new(
                                codes::E0301,
                                a.span,
                                format!(
                                    "`{name}` takes {takes} argument{}, but {given} were given",
                                    wrela_diag::plural(takes)
                                ),
                            )
                            .with_secondary(self.p.func(f).sig_span, "declared here"),
                        );
                        self.check_expr(&a.value, None);
                        continue;
                    }
                    next += 1;
                    next - 1
                }
                Some(n) => match params.iter().position(|p| p.name == n.name) {
                    Some(i) if i >= offset => i,
                    _ => {
                        let mut d = Diagnostic::new(
                            codes::E0302,
                            n.span,
                            format!("`{name}` has no parameter `{}`", n.name),
                        );
                        if let Some(s) =
                            resolve::closest(&n.name, params[offset..].iter().map(|p| p.name))
                        {
                            d = d.with_fix(format!("did you mean `{s}`?"), n.span, s);
                            // The misspelling is that argument: don't also call it missing.
                            suggested.push(s);
                        } else {
                            let names: Vec<String> =
                                params[offset..].iter().map(|p| format!("`{}`", p.name)).collect();
                            d = d.with_note(format!("its parameters are {}", names.join(", ")));
                        }
                        self.err(d);
                        self.check_expr(&a.value, None);
                        continue;
                    }
                },
            };
            if slots[idx].is_some() {
                let pname = params[idx].name;
                self.err(Diagnostic::new(
                    codes::E0304,
                    a.span,
                    format!("`{pname}` is given twice"),
                ));
                continue;
            }
            let p = &params[idx];
            let e = self.check_arg(p, &a.value);
            slots[idx] = Some(e);
            order.push(idx);
        }
        let mut out = Vec::new();
        let mut missing = Vec::new();
        for (i, s) in slots.into_iter().enumerate() {
            match s {
                Some(e) => out.push(e),
                None => match &params[i].default {
                    Some(d) => {
                        out.push(self.check_default(d, params[i].ty, params[i].module));
                        order.push(i);
                    }
                    None => {
                        if !suggested.contains(&params[i].name) {
                            missing.push(params[i].name);
                        }
                        out.push(self.error_expr(span));
                    }
                },
            }
        }
        if !missing.is_empty() {
            let list = missing.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", ");
            self.err(
                Diagnostic::new(
                    codes::E0303,
                    span,
                    format!("this call to `{name}` is missing {list}"),
                )
                .with_secondary(self.p.func(f).sig_span, "declared here"),
            );
        }
        // Arguments missing (already reported) are placeholders; evaluate them last.
        for i in 0..out.len() {
            if !order.contains(&i) {
                order.push(i);
            }
        }
        (out, params.iter().map(|p| p.mode).collect(), order)
    }

    /// One argument against its parameter: type, and the call-site marker for its mode.
    fn check_arg(&mut self, p: &CallParam, value: &ast::Expr) -> Expr {
        // The arguments before it may have settled its type (`T::K` once `T` is known).
        self.settle_projections(false);
        let e = self.check_expr(value, Some(p.ty));
        self.coerce_arg(&e, p.ty);
        let marked_mut = matches!(e.kind, ExprKind::MutArg(_));
        match p.mode {
            Mode::Mut if !marked_mut && !matches!(e.kind, ExprKind::Error) => {
                self.err(
                    Diagnostic::new(codes::E0503, e.span, format!("`{}` is a `mut` parameter, so its argument is written `mut ...` at the call site", p.name))
                        .with_note("every mutation is visible where it happens (§6.2)")
                        .with_fix("add `mut`", e.span.shrink_to_start(), "mut "),
                );
            }
            Mode::Borrow | Mode::Take if marked_mut => {
                let ExprKind::MutArg(inner) = &e.kind else { unreachable!() };
                self.err(
                    Diagnostic::new(
                        codes::E0504,
                        e.span,
                        format!("`{}` isn't a `mut` parameter, so it can't take `mut ...`", p.name),
                    )
                    .with_fix(
                        "remove `mut`",
                        Span::new(e.span.file, e.span.start, inner.span.start),
                        "",
                    ),
                );
            }
            _ => {}
        }
        e
    }

    /// Argument coercions: an array passes as a run (`[T; N]` to `[T]`); otherwise unify.
    fn coerce_arg(&mut self, e: &Expr, param_ty: TyId) {
        if let (TyKind::Array(a, _), TyKind::Slice(b)) = (self.kind(e.ty), self.kind(param_ty)) {
            self.expect(*a, *b, e.span);
            return;
        }
        if let (TyKind::Closure(c, _), TyKind::FnPtr(ps, r)) =
            (self.kind(e.ty), self.kind(param_ty))
        {
            if let Some(def) = self.closure_def(*c) {
                let (params, ret) = (def.params.clone(), def.ret);
                if params.len() != ps.len() {
                    self.err(Diagnostic::new(
                        codes::E0301,
                        e.span,
                        format!(
                            "this closure takes {} parameters, but {} are expected",
                            params.len(),
                            ps.len()
                        ),
                    ));
                    return;
                }
                for (l, &pt) in params.iter().zip(ps) {
                    let lt = self.locals[l.index()].ty;
                    self.expect(lt, pt, e.span);
                }
                self.expect(ret, *r, e.span);
            }
            return;
        }
        if let (&TyKind::FnDef(f, ref args), TyKind::FnPtr(ps, r)) =
            (self.kind(e.ty), self.kind(param_ty))
        {
            let (params, ret, ret_mode) = self.fn_params(f, args);
            // A `fn(..)` type's parameters are borrowed and its result is owned.
            if let Some(p) = params.iter().find(|p| p.mode != Mode::Borrow) {
                let fname = &self.p.func(f).name;
                self.err(
                    Diagnostic::new(
                        codes::E0300,
                        e.span,
                        format!(
                            "`{fname}` takes `{}` as `{}`, but a `fn(..)` passes its arguments by borrow",
                            p.name,
                            p.mode.keyword()
                        ),
                    )
                    .with_help(format!("pass a closure that calls it: `|..| {fname}(..)`")),
                );
                return;
            }
            if ret_mode != RetMode::Owned {
                let fname = &self.p.func(f).name;
                self.err(Diagnostic::new(
                    codes::E0300,
                    e.span,
                    format!("`{fname}` returns a projection, but a `fn(..)` returns a value"),
                ));
                return;
            }
            if params.len() != ps.len() {
                self.err(Diagnostic::new(
                    codes::E0300,
                    e.span,
                    format!(
                        "`{}` takes {} arguments, but a function taking {} is expected",
                        self.p.func(f).name,
                        params.len(),
                        ps.len()
                    ),
                ));
                return;
            }
            for (p, &pt) in params.iter().zip(ps) {
                self.expect(p.ty, pt, e.span);
            }
            self.expect(ret, *r, e.span);
            return;
        }
        self.expect(e.ty, param_ty, e.span);
    }

    fn call_variant(
        &mut self,
        a: AdtId,
        v: u32,
        path: &ast::Path,
        args: &[ast::Arg],
        expected: Option<TyId>,
        span: Span,
    ) -> Expr {
        let variant = &self.p.adt(a).variants()[v as usize];
        let gargs = self.adt_args(a, path, expected, span);
        let ty = self.p.types.adt(a, gargs.clone());
        if variant.shape != VariantShape::Tuple {
            let how = match variant.shape {
                VariantShape::Unit => format!("write `{}` without parentheses", variant.name),
                _ => format!("write `{} {{ ... }}`", variant.name),
            };
            self.err(
                Diagnostic::new(
                    codes::E0310,
                    span,
                    format!("`{}` isn't built with `(...)`", variant.name),
                )
                .with_help(how),
            );
            return self.error_expr(span);
        }
        let ftys = self.p.fields_of(a, &gargs, Some(v));
        if args.len() != ftys.len() {
            self.err(Diagnostic::new(
                codes::E0301,
                span,
                format!(
                    "`{}` holds {} value{}, but {} were given",
                    variant.name,
                    ftys.len(),
                    wrela_diag::plural(ftys.len()),
                    args.len()
                ),
            ));
        }
        let mut fields = Vec::new();
        for (i, (_, ft)) in ftys.iter().enumerate() {
            match args.get(i) {
                Some(arg) => {
                    if let Some(n) = &arg.name {
                        self.err(Diagnostic::new(
                            codes::E0302,
                            n.span,
                            "a variant's values are positional",
                        ));
                    }
                    fields.push(self.check_expect(&arg.value, *ft));
                }
                None => fields.push(self.error_expr(span)),
            }
        }
        for extra in args.iter().skip(ftys.len()) {
            self.check_expr(&extra.value, None);
        }
        let order = (0..fields.len() as u32).collect();
        Expr {
            ty,
            span,
            kind: ExprKind::Adt {
                adt: a,
                args: gargs,
                variant: Some(v),
                fields,
                order,
                base: None,
            },
        }
    }

    /// A built-in function. Arguments are checked first; then literals take the type the
    /// others agree on; then the built-in's own rule gives the result.
    pub(crate) fn call_builtin(
        &mut self,
        b: BuiltinFn,
        receiver: Option<Expr>,
        args: &[ast::Arg],
        span: Span,
    ) -> Expr {
        let mut xs: Vec<Expr> = receiver.into_iter().collect();
        for a in args {
            if let Some(n) = &a.name {
                self.err(Diagnostic::new(
                    codes::E0302,
                    n.span,
                    format!("`{}` takes positional arguments", b.name()),
                ));
            }
            let hint = xs.first().map(|x| x.ty);
            xs.push(self.check_expr(&a.value, hint));
        }
        if xs.len() != b.arity() {
            self.err(Diagnostic::new(
                codes::E0301,
                span,
                format!(
                    "`{}` takes {} argument{}, not {}",
                    b.name(),
                    b.arity(),
                    wrela_diag::plural(b.arity()),
                    xs.len()
                ),
            ));
            return self.error_expr(span);
        }
        // Literals take the type of the first concrete argument of the same role (all but
        // `select`'s condition): a vector's own type, unless the built-in broadcasts a scalar
        // across it; else that argument's scalar type.
        let concrete = xs.iter().map(|x| x.ty).find(|&t| !matches!(self.kind(t), TyKind::Var(_)));
        if let Some(c) = concrete {
            let broadcasts = matches!(
                b,
                BuiltinFn::Mix
                    | BuiltinFn::Smoothstep
                    | BuiltinFn::Step
                    | BuiltinFn::Clamp
                    | BuiltinFn::Min
                    | BuiltinFn::Max
            );
            let target = if matches!(self.kind(c), TyKind::Vec(_)) && !broadcasts {
                c
            } else {
                builtins::scalar_of(&self.p.types, c)
            };
            for (i, x) in xs.iter().enumerate() {
                let same_role = b != BuiltinFn::Select || i < 2;
                if same_role && matches!(self.kind(x.ty), TyKind::Var(_)) {
                    let _ = self.infer.unify(&self.p.types, x.ty, target);
                }
            }
        }
        // Literals of the same role agree with each other, as they do under `+`: `max(0, 0.5)`
        // is a float.
        let mut first: Option<TyId> = None;
        for (i, x) in xs.iter().enumerate() {
            if (b == BuiltinFn::Select && i >= 2) || !self.is_number_var(x.ty) {
                continue;
            }
            match first {
                Some(f) => {
                    let _ = self.try_unify(f, x.ty);
                }
                None => first = Some(x.ty),
            }
        }
        // A float built-in makes an integer literal a float one (`sqrt(4)`). Otherwise literals
        // stay open, as under operators: `let n = 7; min(n, 3); let k: u32 = n` is fine.
        if b.wants_float() {
            for x in &xs {
                if self.infer.var_kind(&self.p.types, x.ty) == Some(VarKind::Int) {
                    let f = self.new_var(VarKind::Float, x.span);
                    let _ = self.infer.unify(&self.p.types, x.ty, f);
                }
            }
        }
        // A scalar literal next to a vector broadcasts for min/max/clamp/step/smoothstep.
        let mut tys: Vec<TyId> =
            xs.iter().map(|x| self.infer.resolve(&self.p.types, x.ty)).collect();
        if matches!(
            b,
            BuiltinFn::Min
                | BuiltinFn::Max
                | BuiltinFn::Clamp
                | BuiltinFn::Step
                | BuiltinFn::Smoothstep
        ) && let Some(&v) = tys.iter().find(|&&t| matches!(self.p.types.kind(t), TyKind::Vec(_)))
        {
            for (i, t) in tys.iter_mut().enumerate() {
                if *t == self.p.types.f32 {
                    // Splat the scalar argument.
                    let n = match self.p.types.kind(v) {
                        TyKind::Vec(n) => *n,
                        _ => 4,
                    };
                    let x = xs[i].clone();
                    let vt = self.p.types.vec(n);
                    xs[i] = Expr { ty: vt, span: x.span, kind: ExprKind::Construct(vec![x]) };
                    *t = vt;
                }
            }
        }
        if tys.iter().any(|&t| matches!(self.p.types.kind(t), TyKind::Error)) {
            return self.error_expr(span);
        }
        // Open literals are checked as their default types now, and as the types they settle
        // on once the body is checked. A result that is an open argument's type stays open.
        let shown: Vec<TyId> = tys.iter().map(|&t| self.literal_defaults(t)).collect();
        let open = shown != tys;
        match b.result(self.p, &shown) {
            Ok(r) => {
                let ty = if open {
                    self.builtin_calls.push((b, tys.clone(), span));
                    let same = tys.iter().zip(&shown).find(|&(&t, &d)| d == r && t != d);
                    same.map_or(r, |(&t, _)| t)
                } else {
                    r
                };
                if b.cpu_impl().is_some() && ty == self.p.types.f64 {
                    self.err(crate::builtins::f64_math(b.name(), span));
                }
                let modes = vec![Mode::Borrow; xs.len()];
                let n_args = xs.len();
                Expr {
                    ty,
                    span,
                    kind: ExprKind::Call(Call {
                        callee: Callee::Builtin(b),
                        args: xs,
                        modes,
                        order: (0..n_args).collect(),
                        receiver: false,
                        ret_mode: RetMode::Owned,
                    }),
                }
            }
            Err(msg) => {
                self.err(Diagnostic::new(codes::E0305, span, msg));
                self.error_expr(span)
            }
        }
    }

    /// `t` with each number literal type not settled yet replaced by its default: `i32` for an
    /// integer, `f32` for a float.
    pub(crate) fn literal_defaults(&self, t: TyId) -> TyId {
        let t = self.infer.resolve(&self.p.types, t);
        self.p.types.map(t, &mut |types, x| match self.infer.var_kind(types, x) {
            Some(VarKind::Int) => Some(types.i32),
            Some(VarKind::Float) => Some(types.f32),
            _ => None,
        })
    }

    /// `f32(x)` and friends convert; `vec3(...)` and `mat3(...)` construct.
    fn call_type(&mut self, t: BuiltinTy, args: &[ast::Arg], span: Span) -> Expr {
        let ty = t.ty(&self.p.types);
        match t {
            BuiltinTy::Vec(n) => self.vec_ctor(n, ty, args, span),
            BuiltinTy::Mat(n) => {
                let col = self.p.types.vec(n);
                if args.len() != n as usize {
                    self.err(Diagnostic::new(
                        codes::E0323,
                        span,
                        format!("`mat{n}` is built from {n} column vectors"),
                    ));
                    return self.error_expr(span);
                }
                let mut cols = Vec::new();
                for a in args {
                    cols.push(self.check_expect(&a.value, col));
                }
                Expr { ty, span, kind: ExprKind::Construct(cols) }
            }
            BuiltinTy::Bool | BuiltinTy::Int(_) | BuiltinTy::Float(_) => {
                if args.len() != 1 || args[0].name.is_some() {
                    self.err(Diagnostic::new(codes::E0319, span, "a conversion takes one value"));
                    return self.error_expr(span);
                }
                let x = self.check_expr(&args[0].value, None);
                // `f32(1)`: a literal is the target type already. Only a literal: a variable
                // whose type isn't known yet keeps its own (`let n = 7; f32(n)` converts an
                // `i32`). A negative literal converted to an unsigned type keeps its low bits
                // (`u32(-1)`), so it's a signed integer first.
                if let Some(negative) = literal_sign(&args[0].value)
                    && let Some(k) = self.infer.var_kind(&self.p.types, x.ty)
                {
                    let unsigned = matches!(self.p.types.kind(ty), TyKind::Int(i) if !i.signed());
                    let target = if k == VarKind::Float && !self.p.types.is_float(ty) {
                        Some(self.p.types.f32)
                    } else if negative && unsigned {
                        None
                    } else {
                        Some(ty)
                    };
                    if let Some(target) = target {
                        let _ = self.infer.unify(&self.p.types, x.ty, target);
                    }
                }
                // A number whose type isn't settled yet defaults like any other.
                let ok = self.is_number_var(x.ty)
                    || matches!(
                        self.kind(x.ty),
                        TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Error
                    );
                if !ok
                    || (t == BuiltinTy::Bool
                        && !matches!(self.kind(x.ty), TyKind::Bool | TyKind::Error))
                {
                    let shown = self.display(x.ty);
                    self.err(Diagnostic::new(
                        codes::E0319,
                        x.span,
                        format!("`{shown}` can't be converted to `{}`", self.p.display_ty(ty)),
                    ));
                    return self.error_expr(span);
                }
                Expr { ty, span, kind: ExprKind::Convert(Box::new(x)) }
            }
        }
    }

    /// `vec3()` is zero, `vec3(s)` splats, `vec3(a, b, c)` and `vec3(v2, z)` concatenate, and
    /// `vec3(y: 1.0)` names components (the rest are zero).
    fn vec_ctor(&mut self, n: u8, ty: TyId, args: &[ast::Arg], span: Span) -> Expr {
        let f32 = self.p.types.f32;
        if args.iter().any(|a| a.name.is_some()) {
            let names = ["x", "y", "z", "w"];
            let mut comps: Vec<Option<Expr>> = vec![None; n as usize];
            let mut written = Vec::new();
            for a in args {
                let Some(name) = &a.name else {
                    self.err(Diagnostic::new(
                        codes::E0105,
                        a.span,
                        "name every component, or none",
                    ));
                    continue;
                };
                let Some(i) = names[..n as usize].iter().position(|c| *c == name.name) else {
                    self.err(Diagnostic::new(
                        codes::E0302,
                        name.span,
                        format!("`vec{n}` has no component `{}`", name.name),
                    ));
                    continue;
                };
                let e = self.check_expect(&a.value, f32);
                if comps[i].is_some() {
                    self.err(Diagnostic::new(
                        codes::E0304,
                        name.span,
                        format!("`{}` is given twice", name.name),
                    ));
                    continue;
                }
                comps[i] = Some(e);
                written.push(i);
            }
            return self.construct_in_order(ty, comps, &written, span);
        }
        if args.is_empty() {
            let xs = (0..n)
                .map(|_| Expr { ty: f32, span, kind: ExprKind::Lit(Lit::Float(0.0)) })
                .collect();
            return Expr { ty, span, kind: ExprKind::Construct(xs) };
        }
        let mut xs = Vec::new();
        let mut count = 0u32;
        for a in args {
            let e = self.check_expr(&a.value, if args.len() == 1 { None } else { Some(f32) });
            match self.kind(e.ty) {
                TyKind::Vec(m) => count += u32::from(*m),
                TyKind::Float(FloatTy::F32) => count += 1,
                TyKind::Var(_) => {
                    let _ = self.infer.unify(&self.p.types, e.ty, f32);
                    count += 1;
                }
                TyKind::Error => count += 1,
                _ => {
                    let shown = self.display(e.ty);
                    self.err(
                        Diagnostic::new(
                            codes::E0323,
                            e.span,
                            format!("a vector's components are `f32`s or vectors, not `{shown}`"),
                        )
                        .with_help("convert it: `f32(x)`"),
                    );
                    count += 1;
                }
            }
            xs.push(e);
        }
        if !(count == n as u32 || (args.len() == 1 && count == 1)) {
            self.err(Diagnostic::new(
                codes::E0323,
                span,
                format!("`vec{n}` needs {n} components, but these make {count}"),
            ));
        }
        Expr { ty, span, kind: ExprKind::Construct(xs) }
    }

    /// A vector from named components, evaluated in the order written (`written`, by index
    /// into `comps`); a component not given is zero. Components written out of their order
    /// that aren't literal values are bound first, in a block: `{ let a = z; let b = x;
    /// vec3(b, 0.0, a) }`.
    fn construct_in_order(
        &mut self,
        ty: TyId,
        comps: Vec<Option<Expr>>,
        written: &[usize],
        span: Span,
    ) -> Expr {
        let f32 = self.p.types.f32;
        let zero = Expr { ty: f32, span, kind: ExprKind::Lit(Lit::Float(0.0)) };
        let pure = |e: &Option<Expr>| e.as_ref().is_none_or(|e| zonk::non_literal(e).is_none());
        if written.is_sorted() || comps.iter().filter(|c| !pure(c)).count() < 2 {
            let xs = comps.into_iter().map(|c| c.unwrap_or_else(|| zero.clone())).collect();
            return Expr { ty, span, kind: ExprKind::Construct(xs) };
        }
        let mut comps: Vec<Option<Expr>> = comps;
        let mut stmts = Vec::new();
        for &i in written {
            let Some(e) = comps[i].take() else { continue };
            let at = e.span;
            let l = self.declare_unnamed(f32, LocalKind::Owned { mutable: false }, at);
            let pat = Pat { ty: f32, kind: PatKind::Bind(l), span: at };
            stmts.push(Stmt { kind: StmtKind::Bind { pat, init: e }, span: at });
            comps[i] = Some(Expr { ty: f32, span: at, kind: ExprKind::Local(l) });
        }
        let xs = comps.into_iter().map(|c| c.unwrap_or_else(|| zero.clone())).collect();
        let tail = Expr { ty, span, kind: ExprKind::Construct(xs) };
        let block = Block { stmts, tail: Some(Box::new(tail)), ty, span };
        Expr { ty, span, kind: ExprKind::Block(block) }
    }

    /// `Type::name(...)`: an associated function (inherent or from a trait the type has), or a
    /// variant of an enum named through a type parameter.
    fn call_type_relative(
        &mut self,
        ty: TyId,
        name: &ast::Ident,
        generics: Option<&[ast::TypeExpr]>,
        args: &[ast::Arg],
        expected: Option<TyId>,
        span: Span,
    ) -> Expr {
        if let &TyKind::Adt(a, _) = self.kind(ty)
            && let Some(v) = self.p.adt(a).variants().iter().position(|v| v.name == name.name)
        {
            let path = ast::Path {
                segments: vec![ast::PathSegment { ident: name.clone(), generics: None }],
                span: name.span,
            };
            return self.call_variant(a, v as u32, &path, args, expected.or(Some(ty)), span);
        }
        if let Some((f, impl_args)) = self.find_inherent(ty, &name.name, name.span) {
            let mut gargs = impl_args;
            gargs.extend(self.method_generic_args(f, generics, span));
            return self.call_fn(f, gargs, None, args, expected, span);
        }
        // A trait's function, through a type that has the trait.
        let traits = self.traits_with_method(ty, &name.name, name.span);
        if let [(t, trait_args, unsure)] = traits.as_slice()
            && let Some(m) = traits::trait_method(self.p, *t, &name.name)
        {
            if self.p.func(m).has_self() {
                self.err(Diagnostic::new(
                    codes::E0207,
                    name.span,
                    format!(
                        "`{}` is a method; call it on a value: `x.{}(...)`",
                        name.name, name.name
                    ),
                ));
                return self.error_expr(span);
            }
            let r = TraitRef { trait_: *t, args: trait_args.clone() };
            if *unsure {
                self.trait_obligation(ty, &r, span, &name.name);
            }
            return self.call_trait_method(m, ty, r.args, generics, None, args, span);
        }
        let shown = self.display(ty);
        self.err(Diagnostic::new(
            codes::E0207,
            name.span,
            format!("`{shown}` has no associated function `{}`", name.name),
        ));
        self.failed_call(args, span)
    }

    /// An inherent method of `ty` named `name`, and the impl's arguments for `ty`.
    fn find_inherent(&mut self, ty: TyId, name: &str, span: Span) -> Option<(FnId, Vec<TyId>)> {
        let p = self.p;
        let TyKind::Adt(a, _) = self.kind(ty) else { return None };
        for &i in p.inherent_impls.get(a).into_iter().flatten() {
            let imp = p.impl_(i);
            let Some(f) = imp.methods.iter().copied().find(|&f| p.func(f).name == name) else {
                continue;
            };
            let vars: Vec<TyId> =
                imp.generics.iter().map(|_| self.new_var(VarKind::General, span)).collect();
            let subst = Subst::from_pairs(&imp.generics, &vars);
            let self_ty = p.types.subst(imp.self_ty, &subst);
            if self.try_unify(self_ty, ty) {
                let def = p.func(f);
                if !def.public && def.module != self.scope.module {
                    let m = p.module(def.module).name();
                    self.err(
                        Diagnostic::new(
                            codes::E0203,
                            span,
                            format!("`{name}` is private to `{m}`"),
                        )
                        .with_secondary(def.name_span, "declared here without `pub`")
                        .with_help(format!("mark it `pub` in `{m}` to use it here")),
                    );
                }
                return Some((f, vars));
            }
        }
        None
    }

    /// Whether code being checked can see trait `t`: it's `pub`, or this module's own.
    fn trait_visible(&self, t: TraitId) -> bool {
        let tr = self.p.trait_(t);
        tr.public || tr.module == self.scope.module
    }

    /// `ty` must implement `r`, the trait a call of its method `name` was found through, when
    /// lookup couldn't tell (see [`Self::traits_with_method`]).
    fn trait_obligation(&mut self, ty: TyId, r: &TraitRef, span: Span, name: &str) {
        let why = format!("calling `{}::{name}`", self.p.trait_(r.trait_).name);
        self.obligation(ty, r.clone(), span, why);
    }

    /// The traits (with arguments) through which `ty` has a method `name`; and for each,
    /// whether that's still unsure: its arguments are variables, or `ty` holds a literal's, so
    /// whether `ty` has the trait is checked once the body's types are known.
    fn traits_with_method(
        &mut self,
        ty: TyId,
        name: &str,
        span: Span,
    ) -> Vec<(TraitId, Vec<TyId>, bool)> {
        let ty = self.infer.resolve(&self.p.types, ty);
        let mut out: Vec<(TraitId, Vec<TyId>, bool)> = Vec::new();
        let declared = match self.p.types.kind(ty) {
            TyKind::Param(id) => Some(resolve::param_bounds_closure(self.p, *id)),
            TyKind::Opaque(..) | TyKind::Projection { .. } => {
                // Reuse the solver's view of declared bounds by probing each trait.
                let mut v = Vec::new();
                for t in 0..self.p.traits.len() {
                    let tid = TraitId(t as u32);
                    if self.p.trait_(tid).generics.is_empty()
                        && traits::implements(
                            self.p,
                            ty,
                            &TraitRef { trait_: tid, args: Vec::new() },
                        )
                    {
                        v.push(TraitRef { trait_: tid, args: Vec::new() });
                    }
                }
                Some(v)
            }
            _ => None,
        };
        match declared {
            Some(bounds) => {
                for b in bounds {
                    if traits::trait_method(self.p, b.trait_, name).is_some()
                        && !out.iter().any(|(t, ..)| *t == b.trait_)
                    {
                        out.push((b.trait_, b.args.clone(), false));
                    }
                }
            }
            None => {
                let has_vars = self.p.types.has_vars(ty);
                for t in 0..self.p.traits.len() {
                    let tid = TraitId(t as u32);
                    if !self.trait_visible(tid) || traits::trait_method(self.p, tid, name).is_none()
                    {
                        continue;
                    }
                    // The trait's arguments: an impl's, when one applies. A type that still
                    // holds a literal's variable (`W<{float}>`) has the trait if an impl could
                    // be for it, and impls for several arguments (`Scale<f32>`, `Scale<u32>`)
                    // leave the choice to inference: then they're variables.
                    let known = if has_vars {
                        let could = self
                            .p
                            .impls_of(tid)
                            .iter()
                            .any(|&i| traits::could_unify(self.p, self.p.impl_(i).self_ty, ty));
                        if !could {
                            continue;
                        }
                        None
                    } else {
                        match traits::impl_args(self.p, ty, tid).as_slice() {
                            [] => continue,
                            [args] => Some(args.clone()),
                            _ => None,
                        }
                    };
                    let unsure = known.is_none();
                    let args = known.unwrap_or_else(|| {
                        let n = self.p.trait_(tid).generics.len();
                        (0..n).map(|_| self.new_var(VarKind::General, span)).collect()
                    });
                    out.push((tid, args, unsure));
                }
            }
        }
        out
    }

    /// A trait method's parameters and return type for a given `Self`, trait and method args;
    /// and the substitution that gives them.
    fn trait_method_sig(
        &self,
        m: FnId,
        self_ty: TyId,
        trait_args: &[TyId],
        method_args: &[TyId],
    ) -> (Vec<CallParam<'p>>, TyId, Subst) {
        let def = self.p.func(m);
        let FnOwner::Trait(t) = def.owner else {
            return (Vec::new(), self.p.types.error, Subst::new());
        };
        let tr = self.p.trait_(t);
        let mut subst = Subst::from_pairs(&tr.generics, trait_args);
        subst.insert(tr.self_param, self_ty);
        for (g, &a) in def.generics.iter().zip(method_args) {
            subst.insert(*g, a);
        }
        let (params, ret) = self.sig_under(m, &subst);
        (params, ret, subst)
    }

    // ---- methods ---------------------------------------------------------------------------

    pub(crate) fn check_method_call(
        &mut self,
        receiver: &ast::Expr,
        name: &ast::Ident,
        generics: Option<&[ast::TypeExpr]>,
        args: &[ast::Arg],
        expected: Option<TyId>,
        span: Span,
    ) -> Expr {
        let recv = self.check_expr(receiver, None);
        let rt = self.shallow(recv.ty);
        if matches!(self.p.types.kind(rt), TyKind::Error) {
            return self.failed_call(args, span);
        }
        // A built-in method on a literal (`x.sqrt()` after `let x = 2.0`) leaves its type open,
        // as `sqrt(x)` does, unless a trait has a method of that name.
        if self.is_number_var(rt)
            && generics.is_none()
            && let Some(b) = BuiltinFn::lookup_method(&name.name)
            && b != BuiltinFn::Len
            && !(0..self.p.traits.len())
                .any(|t| traits::trait_method(self.p, TraitId(t as u32), &name.name).is_some())
        {
            return self.call_builtin(b, Some(recv), args, span);
        }
        if matches!(self.p.types.kind(rt), TyKind::Var(_)) {
            match self.infer.var_kind(&self.p.types, rt) {
                Some(VarKind::Float) => {
                    let f = self.p.types.f32;
                    let _ = self.infer.unify(&self.p.types, rt, f);
                }
                Some(VarKind::Int) => {
                    let i = self.p.types.i32;
                    let _ = self.infer.unify(&self.p.types, rt, i);
                }
                _ => {
                    self.err(
                        Diagnostic::new(
                            codes::E0306,
                            recv.span,
                            "the type of this must be known before calling a method on it",
                        )
                        .with_help("annotate its type: `let x: T = ...`"),
                    );
                    return self.failed_call(args, span);
                }
            }
        }
        let rt = self.shallow(recv.ty);
        let pick = if let Some((f, impl_args)) = self.find_inherent(rt, &name.name, name.span) {
            Some(Pick::Inherent(f, impl_args))
        } else {
            let mut traits = self.traits_with_method(rt, &name.name, name.span);
            match traits.len() {
                0 => None,
                1 => {
                    let (t, trait_args, unsure) = traits.remove(0);
                    let r = TraitRef { trait_: t, args: trait_args };
                    if unsure {
                        self.trait_obligation(rt, &r, recv.span, &name.name);
                    }
                    traits::trait_method(self.p, t, &name.name)
                        .map(|method| Pick::Trait { method, trait_args: r.args })
                }
                _ => {
                    let names: Vec<String> = traits
                        .iter()
                        .map(|(t, ..)| format!("`{}`", self.p.trait_(*t).name))
                        .collect();
                    self.err(
                        Diagnostic::new(
                            codes::E0406,
                            name.span,
                            format!(
                                "`{}` is a method of more than one trait here: {}",
                                name.name,
                                names.join(", ")
                            ),
                        )
                        .with_help(format!(
                            "call it through the trait: `Trait::{}(x, ...)`",
                            name.name
                        )),
                    );
                    return self.failed_call(args, span);
                }
            }
        };
        let pick = pick.or_else(|| {
            let builtin_ok = matches!(
                self.p.types.kind(rt),
                TyKind::Int(_) | TyKind::Float(_) | TyKind::Vec(_) | TyKind::Mat(_)
            );
            let sequence = matches!(self.p.types.kind(rt), TyKind::Slice(_) | TyKind::Array(..));
            match BuiltinFn::lookup_method(&name.name) {
                Some(BuiltinFn::Len) if sequence => return Some(Pick::Builtin(BuiltinFn::Len)),
                Some(BuiltinFn::Len) => {}
                Some(b) if builtin_ok => return Some(Pick::Builtin(b)),
                _ => {}
            }
            if name.name == "clone" {
                return Some(Pick::Clone);
            }
            None
        });
        let Some(pick) = pick else {
            let shown = self.display(rt);
            let mut d = Diagnostic::new(
                codes::E0207,
                name.span,
                format!("`{shown}` has no method `{}`", name.name),
            );
            if let &TyKind::Adt(a, _) = self.kind(rt)
                && self.p.adt(a).fields().iter().any(|f| f.name == name.name)
            {
                d = d.with_note(format!("`{}` is a field; a field can't be called", name.name));
            } else if let &TyKind::Param(p) = self.kind(rt) {
                let pname = &self.p.param(p).name;
                d = d.with_help(format!("add a bound that has `{}`: `{pname}: Trait`", name.name));
            }
            let names = self.method_names(rt);
            if let Some(s) = resolve::closest(&name.name, names.iter().map(String::as_str)) {
                d = d.with_fix(format!("did you mean `{s}`?"), name.span, s);
            }
            self.err(d);
            return self.failed_call(args, span);
        };
        self.finish_method(pick, recv, generics, args, expected, span, name.span)
    }

    /// A method's own generic arguments: those given with `::<...>`, in order, and fresh
    /// variables for the rest (all of them when none are given).
    fn method_generic_args(
        &mut self,
        f: FnId,
        explicit: Option<&[ast::TypeExpr]>,
        span: Span,
    ) -> Vec<TyId> {
        let own = &self.p.func(f).generics;
        let fresh = |this: &mut Self| -> Vec<TyId> {
            own.iter().map(|_| this.new_var(VarKind::General, span)).collect()
        };
        let Some(given) = explicit else { return fresh(self) };
        // Implicit parameters (from `x: Trait`) can't be named; only declared ones count.
        let declared: Vec<usize> =
            (0..own.len()).filter(|&i| !self.p.param(own[i]).name.starts_with("impl ")).collect();
        if given.len() != declared.len() {
            let (name, want) = (&self.p.func(f).name, declared.len());
            let s = wrela_diag::plural(want);
            self.err(resolve::wrong_generic_count(span, name, want, given.len(), s));
            return fresh(self);
        }
        let mut out: Vec<Option<TyId>> = vec![None; own.len()];
        for (&i, t) in declared.iter().zip(given) {
            out[i] = Some(self.resolve_type(t));
        }
        out.into_iter().map(|t| t.unwrap_or_else(|| self.new_var(VarKind::General, span))).collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_method(
        &mut self,
        pick: Pick,
        recv: Expr,
        generics: Option<&[ast::TypeExpr]>,
        args: &[ast::Arg],
        expected: Option<TyId>,
        span: Span,
        name_span: Span,
    ) -> Expr {
        let rt = self.shallow(recv.ty);
        if generics.is_some() && matches!(pick, Pick::Builtin(_) | Pick::Clone) {
            self.err(Diagnostic::new(codes::E0322, span, "this method takes no generic arguments"));
        }
        match pick {
            Pick::Inherent(f, impl_args) => {
                if !self.p.func(f).has_self() {
                    let n = &self.p.func(f).name;
                    let shown = self.display(rt);
                    self.err(Diagnostic::new(
                        codes::E0207,
                        name_span,
                        format!("`{n}` doesn't take `self`; call it as `{shown}::{n}(...)`"),
                    ));
                    return self.error_expr(span);
                }
                let mut gargs = impl_args;
                gargs.extend(self.method_generic_args(f, generics, span));
                self.receiver_mode_check(f, &recv);
                self.call_fn(f, gargs, Some(recv), args, expected, span)
            }
            Pick::Trait { method, trait_args } => {
                let def = self.p.func(method);
                if !def.has_self() {
                    self.err(Diagnostic::new(
                        codes::E0207,
                        name_span,
                        format!("`{}` doesn't take `self`", def.name),
                    ));
                    return self.error_expr(span);
                }
                self.receiver_mode_check(method, &recv);
                self.call_trait_method(method, rt, trait_args, generics, Some(recv), args, span)
            }
            Pick::Builtin(b) => self.call_builtin(b, Some(recv), args, span),
            Pick::Clone => {
                if !args.is_empty() {
                    self.err(Diagnostic::new(codes::E0301, span, "`.clone()` takes no arguments"));
                }
                let clone = self.p.lang_trait(Lang::Clone);
                if let Some(c) = clone {
                    self.obligation_coded(
                        codes::E0514,
                        rt,
                        TraitRef { trait_: c, args: Vec::new() },
                        recv.span,
                        "`.clone()`".into(),
                    );
                }
                Expr {
                    ty: rt,
                    span,
                    kind: ExprKind::Call(Call {
                        callee: Callee::Clone,
                        args: vec![recv],
                        modes: vec![Mode::Borrow],
                        order: vec![0],
                        receiver: true,
                        ret_mode: RetMode::Owned,
                    }),
                }
            }
        }
    }

    /// A call of trait method `method` for `Self = self_ty`, as a method on `recv` or as
    /// `Type::method(...)`: its arguments matched, and its own generics' bounds obligations.
    #[allow(clippy::too_many_arguments)]
    fn call_trait_method(
        &mut self,
        method: FnId,
        self_ty: TyId,
        trait_args: Vec<TyId>,
        generics: Option<&[ast::TypeExpr]>,
        recv: Option<Expr>,
        args: &[ast::Arg],
        span: Span,
    ) -> Expr {
        let def = self.p.func(method);
        let FnOwner::Trait(tr) = def.owner else { unreachable!("a trait method is a trait's") };
        let method_args = self.method_generic_args(method, generics, span);
        let (params, ret, subst) =
            self.trait_method_sig(method, self_ty, &trait_args, &method_args);
        let (params, ret) = self.defer_projections(method, params, ret, span);
        let spans = vec![span; def.generics.len()];
        self.bound_obligations(&def.generics, &method_args, &subst, &spans, &def.name);
        let fname = match recv {
            Some(_) => format!("{}::{}", self.p.trait_(tr).name, def.name),
            None => format!("{}::{}", self.p.display_ty(self_ty), def.name),
        };
        let receiver = recv.is_some();
        let (call_args, modes, order) = self.match_args(&fname, &params, recv, args, span, method);
        self.settle_projections(false);
        let callee = Callee::TraitMethod { method, self_ty, trait_args, method_args };
        Expr {
            ty: ret,
            span,
            kind: ExprKind::Call(Call {
                callee,
                args: call_args,
                modes,
                order,
                receiver,
                ret_mode: def.ret_mode,
            }),
        }
    }

    /// A `mut self` method needs a mutable receiver; that's the memory checker's job, but
    /// marking the receiver as written here records the closure capture correctly.
    fn receiver_mode_check(&mut self, f: FnId, recv: &Expr) {
        if self.p.func(f).params.first().is_some_and(|p| p.mode == Mode::Mut) {
            self.mark_written(recv);
        }
    }

    // ---- GPU intrinsics --------------------------------------------------------------------

    /// The entry point of stage `want` that a `dispatch`/`draw` argument names.
    fn entry_arg(&mut self, a: &ast::Arg, want: Entry) -> Option<(FnId, Vec<TyId>)> {
        let what = want.describe();
        let ast::ExprKind::Path(path) = &a.value.kind else {
            self.err(Diagnostic::new(
                codes::E0603,
                a.value.span,
                format!("expected the name of {what}"),
            ));
            return None;
        };
        match self.resolve_value_path(path) {
            Some(ValueRes::Item(Res::Fn(f))) => {
                let entry = self.p.func(f).attrs.entry.map(|e| e.0);
                let ok = entry
                    .is_some_and(|e| std::mem::discriminant(&want) == std::mem::discriminant(&e));
                if !ok {
                    let n = &self.p.func(f).name;
                    self.err(
                        Diagnostic::new(codes::E0603, a.value.span, format!("`{n}` isn't {what}"))
                            .with_secondary(self.p.func(f).sig_span, "declared here"),
                    );
                    return None;
                }
                let args = self.fresh_fn_args(f, path, a.value.span);
                Some((f, args))
            }
            Some(_) => {
                self.err(Diagnostic::new(
                    codes::E0603,
                    a.value.span,
                    format!("expected the name of {what}"),
                ));
                None
            }
            None => None,
        }
    }

    /// Whether a parameter type is a GPU builtin (supplied by the GPU, not the caller).
    pub(crate) fn is_gpu_builtin(&self, t: TyId) -> bool {
        crate::gpu::BuiltinInput::of(self.p, t).is_some()
    }

    /// The methods a value of type `t` has: its inherent ones, and those of every trait it
    /// implements (or, for a generic parameter, of its bounds). For suggestions.
    fn method_names(&self, t: TyId) -> Vec<String> {
        let p = self.p;
        let mut out = Vec::new();
        if let TyKind::Adt(a, _) = self.kind(t)
            && let Some(impls) = p.inherent_impls.get(a)
        {
            for &i in impls {
                for &f in &p.impl_(i).methods {
                    out.push(p.func(f).name.clone());
                }
            }
        }
        let traits: Vec<TraitId> = match self.kind(t) {
            TyKind::Param(param) => {
                resolve::param_bounds_closure(p, *param).into_iter().map(|r| r.trait_).collect()
            }
            _ => (0..p.traits.len() as u32)
                .map(TraitId)
                .filter(|&tr| {
                    self.trait_visible(tr)
                        && p.trait_(tr).generics.is_empty()
                        && traits::implements(p, t, &TraitRef { trait_: tr, args: Vec::new() })
                })
                .collect(),
        };
        for tr in traits {
            for &f in &p.trait_(tr).methods {
                out.push(p.func(f).name.clone());
            }
        }
        out
    }

    /// The type a CPU caller passes for a kernel parameter: a `GpuBuffer<T>` for `[T]` and
    /// `Slots<T>`, the value itself otherwise.
    fn cpu_side_type(&mut self, param_ty: TyId) -> TyId {
        let gpu_buffer = self.p.lang_adt(Lang::GpuBuffer);
        match self.kind(param_ty) {
            TyKind::Slice(t) => {
                gpu_buffer.map_or(self.p.types.error, |g| self.p.types.adt(g, vec![*t]))
            }
            TyKind::Adt(a, args) if self.p.is_lang_adt(*a, Lang::Slots) => {
                gpu_buffer.map_or(self.p.types.error, |g| self.p.types.adt(g, args.clone()))
            }
            _ => param_ty,
        }
    }

    fn check_dispatch(&mut self, args: &[ast::Arg], span: Span) -> Expr {
        let unit = self.p.types.unit;
        let Some(first) = args.first().filter(|a| a.name.is_none()) else {
            self.err(Diagnostic::new(
                codes::E0603,
                span,
                "`dispatch` takes the kernel first: `dispatch(kernel, groups: n, ...)`",
            ));
            return self.error_expr(span);
        };
        let Some((kernel, kernel_args)) = self.entry_arg(first, Entry::Compute([1, 1, 1])) else {
            for a in &args[1..] {
                self.check_expr(&a.value, None);
            }
            return self.error_expr(span);
        };
        let (params, _, _) = self.fn_params(kernel, &kernel_args);
        self.fn_obligations(kernel, &kernel_args, span);
        let kname = &self.p.func(kernel).name;
        let u32_ty = self.p.types.u32;
        let mut groups: Option<Expr> = None;
        let user: Vec<(usize, CallParam)> = params
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, p)| !self.is_gpu_builtin(p.ty))
            .collect();
        let mut given: Vec<Option<Expr>> = vec![None; user.len()];
        let mut written: Vec<usize> = Vec::new();
        let mut groups_at = 0;
        let mut next = 0usize;
        for (i, a) in args.iter().enumerate().skip(1) {
            let is_groups =
                a.name.as_ref().is_some_and(|n| n.name == "groups") || (a.name.is_none() && i == 1);
            if is_groups {
                let e = self.check_expr(&a.value, None);
                if groups.is_some() {
                    self.err(Diagnostic::new(codes::E0304, a.span, "`groups` is given twice"));
                    continue;
                }
                groups_at = written.len();
                match self.kind(e.ty) {
                    TyKind::Tuple(ts) if ts.len() == 3 => {
                        for &t in ts {
                            self.expect(t, u32_ty, e.span);
                        }
                    }
                    _ => {
                        self.expect(e.ty, u32_ty, e.span);
                    }
                }
                groups = Some(e);
                continue;
            }
            let slot = match &a.name {
                Some(n) => user.iter().position(|(_, p)| p.name == n.name),
                None => {
                    next += 1;
                    Some(next - 1).filter(|&k| k < user.len())
                }
            };
            let Some(k) = slot else {
                let what = a
                    .name
                    .as_ref()
                    .map_or("a positional argument".to_string(), |n| format!("`{}`", n.name));
                self.err(
                    Diagnostic::new(
                        codes::E0603,
                        a.span,
                        format!("`{kname}` has no parameter {what} to pass"),
                    )
                    .with_secondary(self.p.func(kernel).sig_span, "the kernel"),
                );
                self.check_expr(&a.value, None);
                continue;
            };
            let want = self.cpu_side_type(user[k].1.ty);
            let e = self.check_expect(&a.value, want);
            if given[k].is_some() {
                self.err(Diagnostic::new(
                    codes::E0304,
                    a.span,
                    format!("`{}` is given twice", user[k].1.name),
                ));
                continue;
            }
            written.push(k);
            given[k] = Some(e);
        }
        let Some(groups) = groups else {
            self.err(
                Diagnostic::new(
                    codes::E0603,
                    span,
                    "`dispatch` needs `groups:`, the number of workgroups",
                )
                .with_help("e.g. `groups: (n + 63) / 64` for a `@compute(64)` kernel"),
            );
            return self.error_expr(span);
        };
        let mut out = Vec::new();
        for (k, g) in given.iter().enumerate() {
            match g {
                Some(_) => {}
                None => {
                    self.err(Diagnostic::new(
                        codes::E0603,
                        span,
                        format!(
                            "this dispatch doesn't pass `{}`'s parameter `{}`",
                            kname, user[k].1.name
                        ),
                    ));
                }
            }
        }
        for k in written {
            if let Some(e) = given[k].take() {
                out.push((user[k].0, e));
            }
        }
        let d = Dispatch { kernel, kernel_args, groups: Box::new(groups), args: out, groups_at };
        Expr { ty: unit, span, kind: ExprKind::Dispatch(Box::new(d)) }
    }

    fn check_draw(&mut self, args: &[ast::Arg], span: Span) -> Expr {
        let unit = self.p.types.unit;
        let u32_ty = self.p.types.u32;
        if args.len() < 2 || args[0].name.is_some() || args[1].name.is_some() {
            self.err(Diagnostic::new(codes::E0603, span, "`draw` takes the vertex and fragment shaders first: `draw(vs, fs, vertices: n, ...)`"));
            return self.error_expr(span);
        }
        let vs = self.entry_arg(&args[0], Entry::Vertex);
        let fs = self.entry_arg(&args[1], Entry::Fragment);
        let (Some(vs), Some(fs)) = (vs, fs) else {
            for a in &args[2..] {
                self.check_expr(&a.value, None);
            }
            return self.error_expr(span);
        };
        let (vparams, vret, _) = self.fn_params(vs.0, &vs.1);
        let (fparams, _, _) = self.fn_params(fs.0, &fs.1);
        self.fn_obligations(vs.0, &vs.1, span);
        self.fn_obligations(fs.0, &fs.1, span);
        // The uniforms: both shaders' parameters that aren't builtins or the vertex output.
        let mut wanted: Vec<(&str, TyId)> = Vec::new();
        for p in vparams.iter().chain(&fparams) {
            if self.is_gpu_builtin(p.ty) || self.shallow(p.ty) == self.shallow(vret) {
                continue;
            }
            match wanted.iter().find(|(n, _)| *n == p.name) {
                Some((_, t)) => {
                    let t = *t;
                    if self.infer.unify(&self.p.types, t, p.ty).is_err() {
                        self.err(Diagnostic::new(
                            codes::E0603,
                            span,
                            format!("the shaders both take `{}`, with different types", p.name),
                        ));
                    }
                }
                None => wanted.push((p.name, p.ty)),
            }
        }
        let mut vertices = None;
        let mut instances = None;
        let mut counts_at = [0; 2];
        let mut given: Vec<Option<Expr>> = vec![None; wanted.len()];
        let mut written: Vec<usize> = Vec::new();
        for a in &args[2..] {
            let Some(n) = &a.name else {
                self.err(
                    Diagnostic::new(
                        codes::E0603,
                        a.span,
                        "after the shaders, `draw`'s arguments are named",
                    )
                    .with_help("e.g. `vertices: 3, scene: scene`"),
                );
                continue;
            };
            let twice = |this: &mut Self| {
                this.err(Diagnostic::new(
                    codes::E0304,
                    a.span,
                    format!("`{}` is given twice", n.name),
                ));
            };
            match n.name.as_str() {
                "vertices" | "instances" => {
                    let e = self.check_expect(&a.value, u32_ty);
                    let (slot, at) =
                        if n.name == "vertices" { (&mut vertices, 0) } else { (&mut instances, 1) };
                    if slot.is_some() {
                        twice(self);
                        continue;
                    }
                    *slot = Some(e);
                    counts_at[at] = written.len();
                }
                other => match wanted.iter().position(|(w, _)| *w == other) {
                    Some(k) => {
                        // A buffer parameter (`[T]`) is passed a `GpuBuffer<T>`.
                        let want = self.cpu_side_type(wanted[k].1);
                        let e = self.check_expect(&a.value, want);
                        if given[k].is_some() {
                            twice(self);
                            continue;
                        }
                        given[k] = Some(e);
                        written.push(k);
                    }
                    None => {
                        self.err(Diagnostic::new(
                            codes::E0603,
                            n.span,
                            format!("neither shader takes `{other}`"),
                        ));
                        self.check_expr(&a.value, None);
                    }
                },
            }
        }
        let Some(vertices) = vertices else {
            self.err(Diagnostic::new(
                codes::E0603,
                span,
                "`draw` needs `vertices:`, how many vertices to draw",
            ));
            return self.error_expr(span);
        };
        let instances =
            instances.unwrap_or(Expr { ty: u32_ty, span, kind: ExprKind::Lit(Lit::Int(1)) });
        for (k, g) in given.iter().enumerate() {
            if g.is_none() {
                self.err(Diagnostic::new(
                    codes::E0603,
                    span,
                    format!("this draw doesn't pass the shaders' `{}`", wanted[k].0),
                ));
            }
        }
        let mut out = Vec::new();
        for k in written {
            if let Some(e) = given[k].take() {
                out.push((wanted[k].0.to_string(), e));
            }
        }
        let d = Draw { vertex: vs, fragment: fs, vertices, instances, args: out, counts_at };
        Expr { ty: unit, span, kind: ExprKind::Draw(Box::new(d)) }
    }
}

/// For a number literal, possibly negated or parenthesized: whether it's negative.
fn literal_sign(e: &ast::Expr) -> Option<bool> {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit {
            kind: ast::LitKind::Int(_) | ast::LitKind::Float(_),
            ..
        }) => Some(false),
        ast::ExprKind::Unary(ast::UnOp::Neg, x) => literal_sign(x).map(|n| !n),
        ast::ExprKind::Paren(x) => literal_sign(x),
        _ => None,
    }
}
