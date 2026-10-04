//! Calls: functions, methods, built-ins, constructors, conversions, and the GPU intrinsics.

use super::expr::{ValueRes, variant_expr};
use super::{Checker, zonk};
use crate::builtins::{self, BuiltinFn, BuiltinTy};
use crate::defs::*;
use crate::resolve;
use crate::thir::*;
use crate::traits;
use crate::ty::*;
use wrela_diag::{Diagnostic, Edit, Span, codes};
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
    /// For a parameter whose type is a generic bounded by a function type: that function
    /// type, as the call instantiates it. A closure given for it is checked against it.
    pub fn_bound: Option<TyId>,
}

/// What looking up an inherent method found.
enum Inherent {
    Missing,
    /// The method, and its impl's arguments for the type.
    Method(FnId, Vec<TyId>),
    /// More than one impl has it, and the type doesn't say which (E0306, reported).
    Ambiguous,
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
            // A stored closure or function: `(self.f)(p)`.
            if let Some((params, ret, modes)) = self.stored_callable(c.ty) {
                return self.call_value(Callee::Value(Box::new(c)), params, ret, modes, args, span);
            }
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
        // Generic arguments where nothing takes them: on a module (`shapes::<i32>::blob`), a
        // built-in function or a built-in type.
        let n = path.segments.len();
        let modules = match res {
            ValueRes::Item(Res::Variant(..)) => n.saturating_sub(2),
            ValueRes::Item(_) => n - 1,
            _ => 0,
        };
        let takes_none = |i: usize| {
            i < modules
                || (i == n - 1
                    && matches!(res, ValueRes::Item(Res::BuiltinFn(_) | Res::BuiltinTy(_))))
        };
        if let Some(seg) = path
            .segments
            .iter()
            .enumerate()
            .find(|&(i, s)| s.generics.is_some() && takes_none(i))
            .map(|(_, s)| s)
        {
            let what = if takes_none(n - 1) && seg.ident.span == path.segments[n - 1].ident.span {
                format!("the built-in `{}`", seg.ident.name)
            } else {
                format!("the module `{}`", seg.ident.name)
            };
            self.err(Diagnostic::new(
                codes::E0322,
                seg.ident.span,
                format!("{what} takes no generic arguments"),
            ));
        }
        match res {
            ValueRes::Local(l) => self.call_local(l, args, expected, span),
            ValueRes::ConstParam(_) => {
                self.err(Diagnostic::new(codes::E0310, callee.span, "a `u32` can't be called"));
                self.failed_call(args, span)
            }
            ValueRes::Item(Res::Fn(f)) => {
                match self.p.func(f).lang {
                    Some(Lang::Dispatch) => return self.check_dispatch(args, span),
                    Some(Lang::Draw) => return self.check_draw(args, span),
                    _ => {}
                }
                if let Some((e, _)) = self.p.func(f).attrs.entry {
                    let kind = match e {
                        Entry::Compute(_) => {
                            "`@compute` kernel; record it with `dispatch(kernel.bind(...), groups: n)`"
                        }
                        Entry::Vertex | Entry::Fragment => {
                            "render entry point; record it with `draw(vs.bind(...), fs.bind(...), vertices: n)`"
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
                // `Trait::<A>::method(x)` names the trait's arguments; without them they're
                // inferred.
                let written = path
                    .segments
                    .len()
                    .checked_sub(2)
                    .and_then(|i| path.segments[i].generics.as_deref());
                let trait_args: Vec<TyId> = match written {
                    Some(g) if g.len() == ntraits => {
                        g.iter().map(|t| self.resolve_type(t)).collect()
                    }
                    Some(g) => {
                        let tname = &self.p.trait_(t).name;
                        self.err(resolve::wrong_generic_count(span, tname, ntraits, g.len()));
                        (0..ntraits).map(|_| self.p.types.error).collect()
                    }
                    None => (0..ntraits).map(|_| self.new_var(VarKind::General, span)).collect(),
                };
                let own = TraitRef { trait_: t, args: trait_args };
                // The method is the trait's own or a supertrait's (`Sub::base(x)`).
                let error = self.p.types.error;
                if traits::trait_method_via(self.p, error, own.clone(), &name.name).is_empty() {
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
                self.trait_obligation(rt, &own, recv.span, &name.name);
                let found = traits::trait_method_via(self.p, rt, own, &name.name);
                if found.len() > 1 {
                    let names: Vec<String> = found
                        .iter()
                        .map(|(_, r)| format!("`{}`", self.p.trait_(r.trait_).name))
                        .collect();
                    self.err(
                        Diagnostic::new(
                            codes::E0406,
                            name.span,
                            format!(
                                "`{}` is a method of more than one of `{}`'s supertraits: {}",
                                name.name,
                                self.p.trait_(t).name,
                                names.join(", ")
                            ),
                        )
                        .with_help(format!(
                            "call it through the trait that declares it: `{}::{}(x, ...)`",
                            self.p.trait_(found[0].1.trait_).name,
                            name.name
                        )),
                    );
                    return self.failed_call(&args[1..], span);
                }
                let Some((m, via)) = found.into_iter().next() else {
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
        let sig = match self.kind(ty) {
            TyKind::FnPtr(..) => self.fn_ptr_sig(ty),
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
            TyKind::Closure(c, _) => match self.closure_def(c.id) {
                Some(def) => Some(self.closure_sig(def)),
                None => {
                    self.err(Diagnostic::new(codes::E0310, span, "a closure can't call itself"));
                    return self.error_expr(span);
                }
            },
            // A value of a type bounded by a function type: called as that type.
            TyKind::Param(g) if let Some(b) = self.p.param(*g).fn_bound => self.fn_ptr_sig(b),
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
        let Some((params, ret, modes)) = sig else { return self.failed_call(args, span) };
        let call = self.call_value(Callee::Local(l), params, ret, modes, args, span);
        self.note_use(l, false);
        call
    }

    /// A function type's parameter types, return type and parameter modes.
    fn fn_ptr_sig(&self, t: TyId) -> Option<(Vec<TyId>, TyId, Vec<Mode>)> {
        match self.kind(t) {
            TyKind::FnPtr(ps, r, flags) => {
                Some((ps.clone(), *r, (0..ps.len()).map(|i| flags.mode(i)).collect()))
            }
            _ => None,
        }
    }

    /// A closure's parameter types, return type and parameter modes.
    fn closure_sig(&self, def: &ClosureDef) -> (Vec<TyId>, TyId, Vec<Mode>) {
        let tys = def.params.iter().map(|p| self.locals[p.index()].ty).collect();
        let modes = def
            .params
            .iter()
            .map(|p| match self.locals[p.index()].kind {
                LocalKind::Param(m) => m,
                _ => Mode::Borrow,
            })
            .collect();
        (tys, def.ret, modes)
    }

    /// The signature a value of type `t` is called with, if it's a stored closure or
    /// function: a type bounded by a function type, a named function, or a closure.
    fn stored_callable(&mut self, t: TyId) -> Option<(Vec<TyId>, TyId, Vec<Mode>)> {
        let t = self.shallow(t);
        let fn_ty = match self.kind(t) {
            TyKind::Param(g) => self.p.param(*g).fn_bound?,
            // A closure this function made: its own signature.
            &TyKind::Closure(c, _) if c.owner == self.fn_id => {
                return self.closure_def(c.id).map(|def| self.closure_sig(def));
            }
            &TyKind::FnDef(f, ref args) => {
                let args = args.clone();
                let (params, ret, _) = self.fn_params(f, &args);
                return Some((
                    params.iter().map(|p| p.ty).collect(),
                    ret,
                    params.iter().map(|p| p.mode).collect(),
                ));
            }
            _ => return None,
        };
        self.fn_ptr_sig(fn_ty)
    }

    /// A call of the stored closure or function `callee`, with the signature `params`, `ret`
    /// and `modes`. Its arguments are positional.
    fn call_value(
        &mut self,
        callee: Callee,
        params: Vec<TyId>,
        ret: TyId,
        modes: Vec<Mode>,
        args: &[ast::Arg],
        span: Span,
    ) -> Expr {
        if args.len() != params.len() {
            self.err(Diagnostic::new(
                codes::E0301,
                span,
                format!("this takes {} arguments, not {}", params.len(), args.len()),
            ));
        }
        let module = self.scope.module;
        let mut out = Vec::new();
        for (i, (a, &pt)) in args.iter().zip(&params).enumerate() {
            if let Some(n) = &a.name {
                self.err(Diagnostic::new(
                    codes::E0302,
                    n.span,
                    "a closure's arguments are positional",
                ));
            }
            let name = format!("#{}", i + 1);
            let p = CallParam {
                name: &name,
                mode: modes[i],
                ty: pt,
                default: None,
                module,
                fn_bound: None,
            };
            out.push(self.check_arg(&p, &a.value));
        }
        let mut call = plain_call(callee, out, false, ret, span);
        if let ExprKind::Call(c) = &mut call.kind {
            c.modes = modes.into_iter().take(c.args.len()).collect();
        }
        call
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
                let fn_bound = match p.types.kind(ps.ty) {
                    TyKind::Param(g) => p.param(*g).fn_bound.map(|b| p.types.subst(b, subst)),
                    _ => None,
                };
                CallParam { name: &ps.name, mode: ps.mode, ty, default, module, fn_bound }
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
        if !def.params.iter().map(|ps| ps.ty).chain([def.ret]).any(|t| p.types.has_projections(t)) {
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
        // Settling one can settle another's types (`conv2(q)` where `q` is known only once an
        // earlier call's projection is): passes until one settles nothing.
        loop {
            let before = self.projections.len();
            self.settle_projections_once(last);
            if self.projections.len() == before {
                break;
            }
        }
    }

    fn settle_projections_once(&mut self, last: bool) {
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
                    if let Some(default) = self.infer.literal_default(&self.p.types, v) {
                        self.try_unify(v, default);
                    }
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
    pub(crate) fn bound_obligations(
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
                self.infer_from_bound(a, &r);
                self.obligation(a, r, span, format!("`{}` in `{name}`", param.name));
            }
        }
    }

    /// A bound's trait arguments that are still variables (`C` in `F: Field<C>`, which only
    /// the bound mentions) are inferred from the one way the type has the trait: its one
    /// declared bound of it (a generic parameter's, or a return-position trait's), or its one
    /// impl of it.
    pub(crate) fn infer_from_bound(&mut self, ty: TyId, r: &TraitRef) {
        let p = self.p;
        let open = |c: &Self, a: TyId| p.types.has_vars(c.infer.resolve(&p.types, a));
        if !r.args.iter().any(|&a| open(self, a)) {
            return;
        }
        let ty = self.infer.resolve(&p.types, ty);
        if p.types.has_vars(ty) || matches!(p.types.kind(ty), TyKind::Error) {
            return;
        }
        let ty = traits::normalize(p, ty, None);
        let candidates: Vec<Vec<TyId>> = match traits::declared_bounds(p, ty) {
            Some(bounds) => {
                bounds.into_iter().filter(|b| b.trait_ == r.trait_).map(|b| b.args).collect()
            }
            // An impl whose arguments it doesn't fix (`impl<C> Tr<C> for S`) can't say.
            None => traits::impl_args(p, ty, r.trait_)
                .into_iter()
                .filter(|args| {
                    !args.iter().any(|&a| p.types.any(a, &mut |k| matches!(k, TyKind::Param(_))))
                })
                .collect(),
        };
        if let [args] = candidates.as_slice()
            && !args.iter().any(|&a| {
                p.types.any(a, &mut |k| matches!(k, TyKind::Param(q) if p.param(*q).is_self))
            })
        {
            for (&want, &have) in r.args.iter().zip(args) {
                self.try_unify(want, have);
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
        if self.p.func(f).attrs.unsafe_call && self.unsafe_depth == 0 {
            let name = self.p.fn_display_name(f);
            self.err(
                Diagnostic::new(
                    codes::E0215,
                    span,
                    format!(
                        "`{name}` is part of std's unsafe core, so it's called only inside `unsafe`"
                    ),
                )
                .with_note("nothing checks what it does with memory (§6.14)"),
            );
        }
        let (params, ret) = self.sig_under(f, &subst);
        let (params, ret) = self.defer_projections(f, params, ret, span);
        // Let the expected type guide inference of the generics (`let x: T = f()`), but only
        // if it fits: a mismatch is reported where the value is used. One not known yet (a
        // literal's, in `0.5 * gradient(f, p)`) only after the arguments, which know better:
        // a scalar times a vector is a vector.
        let hint = expected.filter(|_| !matches!(self.kind(ret), TyKind::Opaque(..)));
        let early = hint.filter(|&e| self.infer.var_kind(&self.p.types, e).is_none());
        if let Some(e) = early {
            self.try_unify(ret, e);
        }
        let name = self.p.fn_display_name(f);
        let has_receiver = receiver.is_some();
        let (call_args, modes, order) = self.match_args(&name, &params, receiver, args, span, f);
        if let Some(e) = hint
            && early.is_none()
        {
            self.try_unify(ret, e);
        }
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
        self.match_args_as(name, params, receiver, args, span, f, false)
    }

    /// [`match_args`](Self::match_args), with each argument checked as a GPU entry point's is
    /// when `gpu` (a buffer for a `[T]` parameter, §12).
    #[allow(clippy::too_many_arguments)]
    fn match_args_as(
        &mut self,
        name: &str,
        params: &[CallParam],
        receiver: Option<Expr>,
        args: &[ast::Arg],
        span: Span,
        f: FnId,
        gpu: bool,
    ) -> (Vec<Expr>, Vec<Mode>, Vec<usize>) {
        let offset = usize::from(receiver.is_some());
        self.positional_literals(name, params, offset, args);
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
            let e = if gpu { self.gpu_arg(p, &a.value) } else { self.check_arg(p, &a.value) };
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
                        let owned = params[i].mode == Mode::Take;
                        out.push(self.check_default(d, params[i].ty, params[i].module, owned));
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

    /// W0005: a bare literal passed by position where a swap would still compile (§3): two
    /// neighbouring parameters of one type both given bare numbers (`round_cone(a, b, 0.3,
    /// 0.1)`), or a bare `bool` to a function that takes more than one argument. One per call;
    /// its fix names that argument and every positional one after it, since a named argument
    /// can't be followed by a positional one.
    fn positional_literals(
        &mut self,
        name: &str,
        params: &[CallParam],
        offset: usize,
        args: &[ast::Arg],
    ) {
        let positional = args.iter().take_while(|a| a.name.is_none()).count();
        let takes = params.len().saturating_sub(offset);
        // Too many arguments is an error of its own (E0301).
        if args.len() > takes {
            return;
        }
        let literal = |k: usize| (k < positional).then(|| bare_literal(&args[k].value)).flatten();
        for k in 0..positional {
            let idx = offset + k;
            if idx >= params.len() {
                return;
            }
            let Some(boolean) = literal(k) else { continue };
            let message = if boolean {
                if takes < 2 {
                    continue;
                }
                let word = if bare_true(&args[k].value) { "true" } else { "false" };
                format!("a bare `{word}` says nothing at the call: name it")
            } else {
                // The next parameter, of the same type, also given a bare number.
                let next = idx + 1;
                let ty = self.infer.resolve(&self.p.types, params[idx].ty);
                let same =
                    next < params.len() && self.infer.resolve(&self.p.types, params[next].ty) == ty;
                if !same || literal(k + 1) != Some(false) {
                    continue;
                }
                let shown = self.display(ty);
                let of = if shown.contains('_') {
                    "of one type".to_string()
                } else {
                    format!("both `{shown}`")
                };
                format!(
                    "`{name}` takes `{}` and `{}`, {of}: name these numbers, so a swap can't compile",
                    params[idx].name, params[next].name
                )
            };
            let edits: Vec<Edit> = (k..positional)
                .take_while(|&m| offset + m < params.len())
                .map(|m| Edit {
                    span: args[m].value.span.shrink_to_start(),
                    replacement: format!("{}: ", params[offset + m].name),
                })
                .collect();
            self.err(
                Diagnostic::new(codes::W0005, args[k].value.span, message)
                    .with_note("an argument can be named at any call (§3)")
                    .with_fix_edits("name the arguments", edits),
            );
            return;
        }
    }

    /// One argument against its parameter: type, and the call-site marker for its mode.
    fn check_arg(&mut self, p: &CallParam, value: &ast::Expr) -> Expr {
        // The arguments before it may have settled its type (`T::K` once `T` is known).
        self.settle_projections(false);
        let e = match p.fn_bound {
            // A closure or function for `F: fn(..)`: checked as the function type, and `F` is
            // its own type, which can be stored (§6.7).
            Some(b) => {
                let e = self.check_expr(value, Some(b));
                self.coerce_arg(&e, b);
                if !self.try_unify(e.ty, p.ty) && !matches!(self.kind(e.ty), TyKind::Error) {
                    let shown = self.display(e.ty);
                    self.err(Diagnostic::new(
                        codes::E0300,
                        e.span,
                        format!("`{}` takes a closure or a function, not `{shown}`", p.name),
                    ));
                }
                return e;
            }
            None => self.check_expr(value, Some(p.ty)),
        };
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

    /// Argument coercions: an array or a `Vec` passes as a run (`[T; N]` and `Vec<T>` to
    /// `[T]`), and a `Text` or a `String` as a `str`; otherwise unify.
    fn coerce_arg(&mut self, e: &Expr, param_ty: TyId) {
        if matches!(self.kind(param_ty), TyKind::Str) && self.is_stringy(e.ty) {
            return;
        }
        if let TyKind::Slice(b) = *self.kind(param_ty)
            && let Some(a) = self.vec_elem(e.ty)
        {
            self.expect(a, b, e.span);
            return;
        }
        // `Bytes` passes as a `[u8]`.
        if let TyKind::Slice(_) = self.kind(param_ty)
            && self.run_coerces(e.ty, param_ty)
        {
            return;
        }
        match (self.kind(e.ty), self.kind(param_ty)) {
            (TyKind::Array(a, _) | TyKind::ArrayN(a, _), TyKind::Slice(b)) => {
                self.expect(*a, *b, e.span);
            }
            (TyKind::Closure(c, _), TyKind::FnPtr(ps, r, _)) => {
                let Some(def) = self.closure_def(c.id) else { return };
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
            (&TyKind::FnDef(f, ref args), TyKind::FnPtr(ps, r, flags)) => {
                let (params, ret, ret_mode) = self.fn_params(f, args);
                // A `fn(..)` type's parameters have the modes it gives them, and its result is
                // owned.
                if let Some((i, p)) =
                    params.iter().enumerate().find(|(i, p)| p.mode != flags.mode(*i))
                {
                    let fname = &self.p.func(f).name;
                    let want = match flags.mode(i) {
                        Mode::Borrow => "by borrow".to_string(),
                        m => format!("as `{}`", m.keyword()),
                    };
                    self.err(
                        Diagnostic::new(
                            codes::E0300,
                            e.span,
                            format!(
                                "`{fname}` takes `{}` as `{}`, but the function type passes it {want}",
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
            }
            _ => {
                self.expect(e.ty, param_ty, e.span);
            }
        }
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
        for (i, ft) in ftys.iter().enumerate() {
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
        variant_expr(ty, span, a, gargs, v, fields)
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
        if matches!(b, BuiltinFn::Panic | BuiltinFn::Assert) && receiver.is_none() {
            return self.check_panic(b, args, span);
        }
        if b == BuiltinFn::Embed && receiver.is_none() {
            return self.check_embed(args, span);
        }
        let mut xs: Vec<Expr> = receiver.into_iter().collect();
        // Where each argument goes, in the order written: its parameter's index.
        let mut written: Vec<usize> = Vec::new();
        if let Some(names) = b.param_names() {
            match self.builtin_named_args(b, names, args, span) {
                Some((checked, order)) => (xs, written) = (checked, order),
                None => return self.error_expr(span),
            }
        } else {
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
        let concrete = xs
            .iter()
            .map(|x| self.infer.resolve(&self.p.types, x.ty))
            .find(|&t| !matches!(self.kind(t), TyKind::Var(_)));
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
        // A bit pattern written as a literal is a `u32`: `bitcast_f32(0xBF800000)` doesn't fit
        // an `i32`. (A negated one stays open, and settles on `i32`.)
        if b == BuiltinFn::BitcastF32
            && let Some(x) = xs.first()
            && matches!(x.kind, ExprKind::Lit(_))
            && self.infer.var_kind(&self.p.types, x.ty) == Some(VarKind::Int)
        {
            let u = self.p.types.u32;
            let _ = self.infer.unify(&self.p.types, x.ty, u);
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
            let f32 = self.p.types.f32;
            for (x, t) in xs.iter_mut().zip(&mut tys).filter(|(_, t)| **t == f32) {
                // Splat the scalar argument.
                let at = x.span;
                let scalar = std::mem::replace(x, Expr { ty: v, span: at, kind: ExprKind::Error });
                *x = Expr { ty: v, span: at, kind: ExprKind::Construct(vec![scalar]) };
                *t = v;
            }
        }
        if tys.iter().any(|&t| matches!(self.p.types.kind(t), TyKind::Error)) {
            return self.error_expr(span);
        }
        // Lowering knows which closure or function a local holds, and `select` would choose at
        // run time, as `if` and `match` can't either (E0702 in lowering).
        if b == BuiltinFn::Select && crate::mir::is_callable(&self.p.types, tys[0]) {
            self.err(
                Diagnostic::new(
                    codes::E0702,
                    span,
                    "choosing between closures or functions at run time isn't supported yet",
                )
                .with_help("call each one in its own branch"),
            );
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
                let stmts = self.bind_in_written_order(&mut xs, &written);
                let call = plain_call(Callee::Builtin(b), xs, false, ty, span);
                if stmts.is_empty() {
                    return call;
                }
                Expr {
                    ty,
                    span,
                    kind: ExprKind::Block(Block { stmts, tail: Some(Box::new(call)), ty, span }),
                }
            }
            Err(msg) => {
                self.err(Diagnostic::new(codes::E0305, span, msg));
                self.error_expr(span)
            }
        }
    }

    /// The arguments of a built-in whose parameters have names (`names`; only `select`'s), by
    /// §3's rules: positional ones first, then named ones, each parameter given once. Each is
    /// checked in the order written; returned in the parameters' order, with the order
    /// written (by parameter index). `None` after an error. A positional `select` is W0005.
    fn builtin_named_args(
        &mut self,
        b: BuiltinFn,
        names: &[&str],
        args: &[ast::Arg],
        span: Span,
    ) -> Option<(Vec<Expr>, Vec<usize>)> {
        let mut slots: Vec<Option<Expr>> = vec![None; names.len()];
        let mut written = Vec::new();
        let mut ok = true;
        let mut hint: Option<TyId> = None;
        for (k, a) in args.iter().enumerate() {
            let i = match &a.name {
                None if k < names.len() => k,
                None => {
                    ok = false;
                    continue;
                }
                Some(n) => match names.iter().position(|p| *p == n.name) {
                    Some(i) if slots[i].is_none() && !written.contains(&i) => i,
                    Some(_) => {
                        self.err(Diagnostic::new(
                            codes::E0304,
                            n.span,
                            format!("`{}` is given twice", n.name),
                        ));
                        ok = false;
                        continue;
                    }
                    None => {
                        let all = names.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>();
                        self.err(
                            Diagnostic::new(
                                codes::E0302,
                                n.span,
                                format!("`{}` has no parameter `{}`", b.name(), n.name),
                            )
                            .with_note(format!("its parameters are {}", all.join(", "))),
                        );
                        ok = false;
                        continue;
                    }
                },
            };
            // The values share a type; the condition is a `bool`.
            let e = self.check_expr(&a.value, if i < 2 { hint } else { None });
            if i < 2 && hint.is_none() {
                hint = Some(e.ty);
            }
            slots[i] = Some(e);
            written.push(i);
        }
        if !ok {
            return None;
        }
        if slots.iter().any(Option::is_none) {
            let missing: Vec<String> = names
                .iter()
                .zip(&slots)
                .filter(|(_, s)| s.is_none())
                .map(|(n, _)| format!("`{n}`"))
                .collect();
            self.err(Diagnostic::new(
                codes::E0301,
                span,
                format!(
                    "`{}` takes {} arguments, and {} isn't given",
                    b.name(),
                    names.len(),
                    missing.join(" or ")
                ),
            ));
            return None;
        }
        // The two values by position: which is which is only their order.
        let positional = args.iter().take_while(|a| a.name.is_none()).count();
        if positional >= 2 {
            let edits = args[..positional]
                .iter()
                .zip(names)
                .map(|(a, n)| Edit {
                    span: a.value.span.shrink_to_start(),
                    replacement: format!("{n}: "),
                })
                .collect();
            self.err(
                Diagnostic::new(
                    codes::W0005,
                    args[0].value.span.to(args[1].value.span),
                    format!(
                        "`{}` takes the value for `false` first: name its arguments, so the order is plain",
                        b.name()
                    ),
                )
                .with_note("an argument can be named at any call (§3)")
                .with_fix_edits("name the arguments", edits),
            );
        }
        Some((slots.into_iter().map(|s| s.expect("each given")).collect(), written))
    }

    /// Arguments checked out of their parameters' order are evaluated in the order written
    /// (§3): when two or more aren't literal values, each is bound first, in that order, and
    /// its local passed instead. The bindings to put before the call.
    fn bind_in_written_order(&mut self, xs: &mut [Expr], written: &[usize]) -> Vec<Stmt> {
        let pure = |e: &Expr| zonk::non_literal(e).is_none();
        if written.is_sorted() || xs.iter().filter(|e| !pure(e)).count() < 2 {
            return Vec::new();
        }
        let mut stmts = Vec::new();
        for &i in written {
            let (ty, at) = (xs[i].ty, xs[i].span);
            let e = std::mem::replace(&mut xs[i], Expr { ty, span: at, kind: ExprKind::Error });
            let l = self.declare_unnamed(ty, LocalKind::Owned { mutable: false }, at);
            let pat = Pat { ty, kind: PatKind::Bind(l), span: at };
            stmts.push(Stmt { kind: StmtKind::Bind { pat, init: e, else_: None }, span: at });
            xs[i] = Expr { ty, span: at, kind: ExprKind::Local(l) };
        }
        stmts
    }

    /// `embed("path")` (§10): `Bytes` of the file at `path`, relative to the package's root.
    /// The path is a string literal that stays inside the package; the driver reads the file.
    fn check_embed(&mut self, args: &[ast::Arg], span: Span) -> Expr {
        let path = match args {
            [a] if a.name.is_none() => match &a.value.kind {
                ast::ExprKind::Lit(l) if l.kind == ast::LitKind::Str => {
                    Some((wrela_syntax::lexer::string_value(&l.text), l.span))
                }
                _ => None,
            },
            _ => None,
        };
        let Some((path, at)) = path else {
            self.err(
                Diagnostic::new(
                    codes::E0218,
                    span,
                    "`embed` takes one string literal: the path of a file in the package",
                )
                .with_note("the build reads the file, so its path is known when the program is built (§10)"),
            );
            return self.failed_call(args, span);
        };
        if let Some(why) = embed_path_problem(&path) {
            self.err(
                Diagnostic::new(codes::E0218, at, format!("`embed` can't read `{path}`: {why}"))
                    .with_note("a path is relative to the package's root, and uses `/` (§10)"),
            );
            return self.error_expr(span);
        }
        // The key names the package too: each package's paths are its own (`embed_key`).
        let package = self.p.modules[self.scope.module.index()].package.index();
        match self.p.lang_adt(Lang::Bytes) {
            Some(a) => Expr {
                ty: self.p.types.intern(TyKind::Adt(a, Vec::new())),
                span,
                kind: ExprKind::Embed(crate::embed_key(package, &path).into()),
            },
            None => self.error_expr(span),
        }
    }

    /// `panic(message)` and `assert(cond)`, `assert(cond, message)` (§15). A message is text: a
    /// string, a `Text`, a `String` or a `str`.
    fn check_panic(&mut self, b: BuiltinFn, args: &[ast::Arg], span: Span) -> Expr {
        let str_ty = self.p.types.intern(TyKind::Str);
        let bool_ty = self.p.types.bool;
        let (min, max) = if b == BuiltinFn::Panic { (1, 1) } else { (1, 2) };
        if args.len() < min || args.len() > max {
            let want = if b == BuiltinFn::Panic {
                "`panic` takes its message"
            } else {
                "`assert` takes a condition, and a message if you want one"
            };
            self.err(Diagnostic::new(codes::E0301, span, want));
            return self.failed_call(args, span);
        }
        let mut xs = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if let Some(n) = &a.name {
                self.err(Diagnostic::new(
                    codes::E0302,
                    n.span,
                    format!("`{}` takes positional arguments", b.name()),
                ));
            }
            if b == BuiltinFn::Assert && i == 0 {
                let e = self.check_expr(&a.value, Some(bool_ty));
                self.expect(e.ty, bool_ty, e.span);
                xs.push(e);
            } else {
                let e = self.check_expr(&a.value, Some(str_ty));
                if !self.is_stringy(e.ty) {
                    self.expect(e.ty, str_ty, e.span);
                }
                xs.push(e);
            }
        }
        if xs.len() == 1 && b == BuiltinFn::Assert {
            xs.push(self.text_expr("assertion failed".into(), span));
        }
        let ty = if b == BuiltinFn::Panic { self.p.types.never } else { self.p.types.unit };
        if b != BuiltinFn::Assert {
            return plain_call(Callee::Builtin(b), xs, false, ty, span);
        }
        // The operands of a comparison, each bound to a local and passed on after the message:
        // where a failure is explained (a debug build, a test, a constant), the panic shows
        // their values, with their source (§10).
        let mut stmts = Vec::new();
        let shown = self.bind_compared(&mut xs[0], &mut stmts);
        xs.extend(shown);
        // `if !cond { assert(false, message, ...) }`: the message, an f-string say, is made
        // only when the assert fails.
        let bool_ty = self.p.types.bool;
        let no = Expr { ty: bool_ty, span, kind: ExprKind::Lit(Lit::Bool(false)) };
        let cond = std::mem::replace(&mut xs[0], no);
        let at = cond.span;
        let not =
            Expr { ty: bool_ty, span: at, kind: ExprKind::Unary(ast::UnOp::Not, Box::new(cond)) };
        let fail = plain_call(Callee::Builtin(b), xs, false, ty, span);
        let then = Block { stmts: Vec::new(), tail: Some(Box::new(fail)), ty, span };
        let check =
            Expr { ty, span, kind: ExprKind::If { cond: Box::new(not), then, else_: None } };
        if stmts.is_empty() {
            return check;
        }
        Expr {
            ty,
            span,
            kind: ExprKind::Block(Block { stmts, tail: Some(Box::new(check)), ty, span }),
        }
    }

    /// The operands of an `assert`'s comparison (`a == b`, `a < b`, through `Eq` and `Ord` too)
    /// that aren't literals, each bound to a local in `stmts` (in the order written, so each is
    /// evaluated once) and read from it by the comparison: a copy of a `Copy` value, the value
    /// of a temporary, or else a projection of the place. Each comes back as its local, at the
    /// operand's span, to be shown.
    fn bind_compared(&mut self, cond: &mut Expr, stmts: &mut Vec<Stmt>) -> Vec<Expr> {
        let Some((a, b)) = comparison_operands(self.p, cond) else { return Vec::new() };
        let mut out = Vec::new();
        for e in [a, b] {
            if zonk::non_literal(e).is_none() {
                continue;
            }
            let (ty, at) = (e.ty, e.span);
            let t = self.infer.resolve(&self.p.types, ty);
            let copy = match self.p.types.kind(t) {
                TyKind::Var(_) => self.is_number_var(t),
                _ => crate::traits::implements_builtin(self.p, t, Lang::Copy),
            };
            let kind = if e.is_place() && !copy {
                LocalKind::Projection { mutable: false }
            } else {
                LocalKind::Owned { mutable: false }
            };
            let init = std::mem::replace(e, Expr { ty, span: at, kind: ExprKind::Error });
            let l = self.declare_unnamed(ty, kind, at);
            let pat = Pat { ty, kind: PatKind::Bind(l), span: at };
            stmts.push(Stmt { kind: StmtKind::Bind { pat, init, else_: None }, span: at });
            *e = Expr { ty, span: at, kind: ExprKind::Local(l) };
            out.push(Expr { ty, span: at, kind: ExprKind::Local(l) });
        }
        out
    }

    /// `t` with each number literal type not settled yet replaced by its default: `i32` for an
    /// integer, `f32` for a float.
    pub(crate) fn literal_defaults(&self, t: TyId) -> TyId {
        let t = self.infer.resolve(&self.p.types, t);
        self.p.types.map(t, &mut |types, x| self.infer.literal_default(types, x))
    }

    /// `f32(x)` and friends convert; `vec3(...)` and `mat3(...)` construct.
    fn call_type(&mut self, t: BuiltinTy, args: &[ast::Arg], span: Span) -> Expr {
        let ty = t.ty(&self.p.types);
        match t {
            BuiltinTy::Str => {
                for a in args {
                    self.check_expr(&a.value, None);
                }
                self.err(
                    Diagnostic::new(codes::E0319, span, "`str` isn't a conversion")
                        .with_help("`s.to_string()` makes an owned `String` from a `str`"),
                );
                self.error_expr(span)
            }
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
                    // Columns are positional: a name would read as a choice of column.
                    if let Some(name) = &a.name {
                        self.err(Diagnostic::new(
                            codes::E0302,
                            name.span,
                            format!("`mat{n}` takes its columns as positional arguments"),
                        ));
                    }
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
                        // An `i32` (it defaults to one), or an `i64` if it doesn't fit one:
                        // `u64(-3000000000)`.
                        let wide = negative_literal(&args[0].value)
                            .is_some_and(|v| v < i128::from(i32::MIN));
                        wide.then(|| self.p.types.int(IntTy::I64))
                    } else {
                        Some(ty)
                    };
                    if let Some(target) = target {
                        let _ = self.infer.unify(&self.p.types, x.ty, target);
                    }
                }
                // An enum whose variants hold nothing converts to an integer: its variant's
                // index, in declaration order (§21's "an enum's integer value").
                if let &TyKind::Adt(a, _) = self.kind(x.ty)
                    && self.p.adt(a).is_enum()
                    && self.p.adt(a).variants().iter().all(|v| v.fields.is_empty())
                    && matches!(t, BuiltinTy::Int(_))
                {
                    let u = self.p.types.u32;
                    let index = Expr { ty: u, span, kind: ExprKind::Discriminant(Box::new(x)) };
                    if ty == u {
                        return index;
                    }
                    return Expr { ty, span, kind: ExprKind::Convert(Box::new(index)) };
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
                .map(|_| Expr { ty: f32, span, kind: ExprKind::Lit(Lit::Float(0.0, 0.0)) })
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
                    let d = Diagnostic::new(
                        codes::E0323,
                        e.span,
                        format!("a vector's components are `f32`s or vectors, not `{shown}`"),
                    );
                    // A number converts; anything else has no `f32` to give.
                    let number = matches!(self.kind(e.ty), TyKind::Int(_) | TyKind::Float(_));
                    self.err(if number { d.with_help("convert it: `f32(x)`") } else { d });
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
        mut comps: Vec<Option<Expr>>,
        written: &[usize],
        span: Span,
    ) -> Expr {
        let f32 = self.p.types.f32;
        let zero = Expr { ty: f32, span, kind: ExprKind::Lit(Lit::Float(0.0, 0.0)) };
        let pure = |e: &Option<Expr>| e.as_ref().is_none_or(|e| zonk::non_literal(e).is_none());
        if written.is_sorted() || comps.iter().filter(|c| !pure(c)).count() < 2 {
            let xs = comps.into_iter().map(|c| c.unwrap_or_else(|| zero.clone())).collect();
            return Expr { ty, span, kind: ExprKind::Construct(xs) };
        }
        let mut stmts = Vec::new();
        for &i in written {
            let Some(e) = comps[i].take() else { continue };
            let at = e.span;
            let l = self.declare_unnamed(f32, LocalKind::Owned { mutable: false }, at);
            let pat = Pat { ty: f32, kind: PatKind::Bind(l), span: at };
            stmts.push(Stmt { kind: StmtKind::Bind { pat, init: e, else_: None }, span: at });
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
        match self.find_inherent(ty, &name.name, name.span) {
            Inherent::Method(f, impl_args) => {
                let mut gargs = impl_args;
                gargs.extend(self.method_generic_args(f, generics, span));
                return self.call_fn(f, gargs, None, args, expected, span);
            }
            Inherent::Ambiguous => return self.failed_call(args, span),
            Inherent::Missing => {}
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

    /// An inherent method of `ty` named `name`, and the impl's arguments for `ty`. When more
    /// than one impl of `ty`'s type has it (`impl S<f32>` and `impl S<u32>`) and `ty`'s
    /// arguments don't say which, choosing one would depend on the order the impls are written
    /// in: that's E0306.
    fn find_inherent(&mut self, ty: TyId, name: &str, span: Span) -> Inherent {
        let p = self.p;
        // `str`'s methods are std's `impl str`; a `Text` or a `String` passes as one.
        if matches!(self.kind(ty), TyKind::Str) {
            return self.str_method(name);
        }
        let TyKind::Adt(a, _) = self.kind(ty) else { return self.builtin_method(ty, name, span) };
        if self.is_stringy(ty) && !self.has_associated_fn(*a, name) {
            return self.str_method(name);
        }
        let mut fitting = Vec::new();
        for &i in p.inherent_impls.get(a).into_iter().flatten() {
            let imp = p.impl_(i);
            let Some(f) = imp.methods.iter().copied().find(|&f| p.func(f).name == name) else {
                continue;
            };
            let receiver = self.infer.resolve(&p.types, ty);
            if !traits::could_unify(p, imp.self_ty, receiver) {
                continue;
            }
            let vars: Vec<TyId> =
                imp.generics.iter().map(|_| self.new_var(VarKind::General, span)).collect();
            let subst = Subst::from_pairs(&imp.generics, &vars);
            let self_ty = p.types.subst(imp.self_ty, &subst);
            if self.fits(self_ty, ty) {
                fitting.push((f, vars, self_ty));
            }
        }
        if fitting.len() > 1 {
            let shown = self.display(ty);
            self.err(
                Diagnostic::new(
                    codes::E0306,
                    span,
                    format!(
                        "more than one impl of `{shown}` has `{name}`, so its type must be known here"
                    ),
                )
                .with_help(format!(
                    "give its arguments, as in `{}::<...>`, or annotate the value's type",
                    p.adt(*a).name
                )),
            );
            return Inherent::Ambiguous;
        }
        let Some((f, vars, self_ty)) = fitting.pop() else { return Inherent::Missing };
        let _ = self.try_unify(self_ty, ty);
        let def = p.func(f);
        if !def.public && def.module != self.scope.module {
            let m = p.module(def.module).name();
            self.err(
                Diagnostic::new(codes::E0203, span, format!("`{name}` is private to `{m}`"))
                    .with_secondary(def.name_span, "declared here without `pub`")
                    .with_help(format!("mark it `pub` in `{m}` to use it here")),
            );
        } else if def.package_only && !p.same_package(def.module, self.scope.module) {
            let pkg = &p.package_of(def.module).name;
            self.err(
                Diagnostic::new(
                    codes::E0203,
                    span,
                    format!("`{name}` is visible only inside the package `{pkg}`"),
                )
                .with_secondary(def.name_span, "declared `pub(package)` here"),
            );
        }
        Inherent::Method(f, vars)
    }

    /// Method `name` of std's `impl str`.
    /// A method of std's `impl` of another built-in type, such as a fixed array's
    /// (`impl<S: Surface, const N: u32> [S; N]`), whose type fits `ty`.
    fn builtin_method(&mut self, ty: TyId, name: &str, span: Span) -> Inherent {
        let p = self.p;
        for &i in &p.builtin_inherent {
            let imp = p.impl_(i);
            if matches!(p.types.kind(imp.self_ty), TyKind::Str) {
                continue;
            }
            let Some(f) = imp.methods.iter().copied().find(|&f| p.func(f).name == name) else {
                continue;
            };
            if !p.func(f).public && p.func(f).module != self.scope.module {
                continue;
            }
            // Variables are made only for an impl that could fit: one left unbound would be
            // reported as a type that can't be inferred.
            let receiver = self.infer.resolve(&p.types, ty);
            if !traits::could_unify(p, imp.self_ty, receiver) {
                continue;
            }
            let vars: Vec<TyId> =
                imp.generics.iter().map(|_| self.new_var(VarKind::General, span)).collect();
            let subst = Subst::from_pairs(&imp.generics, &vars);
            let self_ty = p.types.subst(imp.self_ty, &subst);
            if self.fits(self_ty, ty) {
                let _ = self.try_unify(self_ty, ty);
                return Inherent::Method(f, vars);
            }
        }
        Inherent::Missing
    }

    fn str_method(&self, name: &str) -> Inherent {
        let p = self.p;
        for &i in &p.builtin_inherent {
            let imp = p.impl_(i);
            if !matches!(p.types.kind(imp.self_ty), TyKind::Str) {
                continue;
            }
            if let Some(f) = imp.methods.iter().copied().find(|&f| p.func(f).name == name)
                && (p.func(f).public || p.func(f).module == self.scope.module)
            {
                return Inherent::Method(f, Vec::new());
            }
        }
        Inherent::Missing
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
            // The traits it's declared with, and their supertraits, with their arguments
            // (`Scale<f32>`).
            TyKind::Opaque(..) | TyKind::Projection { .. } => traits::declared_bounds(self.p, ty),
            _ => None,
        };
        match declared {
            Some(bounds) => {
                for b in bounds {
                    if traits::trait_method(self.p, b.trait_, name).is_some()
                        && !out.iter().any(|(t, ..)| *t == b.trait_)
                    {
                        out.push((b.trait_, b.args, false));
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
        // `k.bind(...)` outside `dispatch` and `draw`: a bound entry point isn't a value.
        if name.name == "bind"
            && let Some(f) = self.fn_named(receiver)
            && self.p.func(f).attrs.entry.is_some()
        {
            let n = &self.p.func(f).name;
            self.err(
                Diagnostic::new(
                    codes::E0603,
                    span,
                    format!("`{n}.bind(...)` goes straight into `dispatch` or `draw`"),
                )
                .with_note("an entry point bound to its arguments is recorded where it's bound; it isn't a value to keep (§12)"),
            );
            return self.failed_call(args, span);
        }
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
            let Some(default) = self.infer.literal_default(&self.p.types, rt) else {
                self.err(
                    Diagnostic::new(
                        codes::E0306,
                        recv.span,
                        "the type of this must be known before calling a method on it",
                    )
                    .with_help("annotate its type: `let x: T = ...`"),
                );
                return self.failed_call(args, span);
            };
            let _ = self.infer.unify(&self.p.types, rt, default);
        }
        let rt = self.shallow(recv.ty);
        // `T::Out` with `T` known is the type the impl gives it.
        let rt = if matches!(self.p.types.kind(rt), TyKind::Projection { .. }) {
            self.normalized(rt)
        } else {
            rt
        };
        let inherent = self.find_inherent(rt, &name.name, name.span);
        let pick = if let Inherent::Method(f, impl_args) = inherent {
            Some(Pick::Inherent(f, impl_args))
        } else if let Inherent::Ambiguous = inherent {
            return self.failed_call(args, span);
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
            let sequence = matches!(
                self.p.types.kind(rt),
                TyKind::Slice(_) | TyKind::Array(..) | TyKind::ArrayN(..)
            );
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
            // Rust's iterators and their adapters (a `for` over `.iter()` is fixed where the
            // loop is checked).
            const ITERATOR: &[&str] = &[
                "iter",
                "iter_mut",
                "into_iter",
                "map",
                "filter",
                "collect",
                "sum",
                "fold",
                "enumerate",
                "zip",
                "rev",
                "any",
                "all",
                "find",
                "count",
                "for_each",
            ];
            // A trait's method, on a type that doesn't implement the trait. Traits that share
            // a name are told apart by their modules' paths.
            let traits: Vec<&TraitDef> = self
                .p
                .traits
                .iter()
                .filter(|t| {
                    t.methods
                        .iter()
                        .any(|&m| self.p.func(m).name == name.name && self.p.func(m).has_self())
                })
                .collect();
            let owners: Vec<String> = traits
                .iter()
                .map(|t| {
                    let path = &self.p.module(t.module).path;
                    // The entry module's names are written bare (`use` starts at the root).
                    let entry = path.len() == 1 && path[0] == "main";
                    if traits.iter().filter(|o| o.name == t.name).count() > 1 && !entry {
                        format!("`{}::{}`", path.join("::"), t.name)
                    } else {
                        format!("`{}`", t.name)
                    }
                })
                .collect();
            if !owners.is_empty() && !ITERATOR.contains(&name.name.as_str()) {
                let shown = self.display(rt);
                d = d.with_note(format!(
                    "`{}` is a method of {}, which `{shown}` doesn't implement",
                    name.name,
                    owners.join(" and ")
                ));
                // A closure has a trait through an impl for every function of one signature.
                if matches!(self.kind(rt), TyKind::Closure(..) | TyKind::FnDef(..)) {
                    for t in &traits {
                        let id = self.p.traits.iter().position(|x| std::ptr::eq(x, *t));
                        let Some(id) = id else { continue };
                        for &i in self.p.impls_of(TraitId(id as u32)) {
                            let imp = self.p.impl_(i);
                            if let TyKind::Param(g) = self.p.types.kind(imp.self_ty)
                                && let Some(b) = self.p.param(*g).fn_bound
                            {
                                d = d.with_note(format!(
                                    "a closure or function is `{}` when it's a `{}`",
                                    t.name,
                                    self.p.display_ty(b)
                                ));
                            }
                        }
                    }
                }
            }
            if ITERATOR.contains(&name.name.as_str()) {
                d = d
                    .with_note("wrela has no iterators: a `for` loop goes over a range, an array, a run, a `Vec` or an arena itself")
                    .with_help("write the loop: `for x in xs { ... }`, or `for mut x in xs` to change each element");
            } else {
                let names = self.method_names(rt);
                if let Some(s) = resolve::closest(&name.name, names.iter().map(String::as_str)) {
                    d = d.with_fix(format!("did you mean `{s}`?"), name.span, s);
                }
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
        self.generic_args(f, &self.p.func(f).generics, explicit, span)
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
                self.clone_of(recv, rt, span)
            }
        }
    }

    /// `recv.clone()`, of type `rt`: `Clone` is an obligation (E0514).
    pub(crate) fn clone_of(&mut self, recv: Expr, rt: TyId, span: Span) -> Expr {
        let recv_span = recv.span;
        if let Some(c) = self.p.lang_trait(Lang::Clone) {
            self.obligation_coded(
                codes::E0514,
                rt,
                TraitRef { trait_: c, args: Vec::new() },
                recv_span,
                "`.clone()`".into(),
            );
        }
        plain_call(Callee::Clone, vec![recv], true, rt, span)
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

    /// What a `dispatch` or `draw` argument binds (§12): an entry point of stage `want`, named
    /// alone when it takes nothing (`cover`) or bound to its arguments (`shade.bind(scene,
    /// field)`). Its generic arguments are fresh, and its arguments aren't checked yet.
    fn bound_entry<'a>(&mut self, e: &'a ast::Expr, want: Entry) -> Option<Bound<'a>> {
        let what = want.describe();
        let (path, args) = match &e.kind {
            ast::ExprKind::Path(path) => (path, None),
            ast::ExprKind::MethodCall { receiver, name, args, .. } if name.name == "bind" => {
                match &receiver.kind {
                    ast::ExprKind::Path(path) => (path, Some(args.as_slice())),
                    _ => {
                        self.err(Diagnostic::new(
                            codes::E0603,
                            receiver.span,
                            format!("expected the name of {what}, before `.bind(...)`"),
                        ));
                        self.check_args_alone(args);
                        return None;
                    }
                }
            }
            _ => {
                self.err(
                    Diagnostic::new(
                        codes::E0603,
                        e.span,
                        format!("expected {what}, named or bound to its arguments"),
                    )
                    .with_help("write `name` for one that takes nothing, or `name.bind(...)`"),
                );
                self.check_expr(e, None);
                return None;
            }
        };
        let found = match self.resolve_value_path(path) {
            Some(ValueRes::Item(Res::Fn(f))) => {
                let entry = self.p.func(f).attrs.entry.map(|e| e.0);
                let ok = entry
                    .is_some_and(|e| std::mem::discriminant(&want) == std::mem::discriminant(&e));
                if ok {
                    Some(f)
                } else {
                    let n = &self.p.func(f).name;
                    self.err(
                        Diagnostic::new(codes::E0603, path.span, format!("`{n}` isn't {what}"))
                            .with_secondary(self.p.func(f).sig_span, "declared here"),
                    );
                    None
                }
            }
            Some(_) => {
                self.err(Diagnostic::new(
                    codes::E0603,
                    path.span,
                    format!("expected the name of {what}"),
                ));
                None
            }
            None => None,
        };
        let Some(func) = found else {
            if let Some(args) = args {
                self.check_args_alone(args);
            }
            return None;
        };
        let gen_args = self.fresh_fn_args(func, path, path.span);
        Some(Bound { func, gen_args, args, span: e.span })
    }

    /// Arguments checked by themselves, for what they say, when there's no function to check
    /// them against.
    fn check_args_alone(&mut self, args: &[ast::Arg]) {
        for a in args {
            self.check_expr(&a.value, None);
        }
    }

    /// A bound entry point's arguments, checked against its parameters as a call's are (§3):
    /// every parameter but the GPU's builtins and `skip` (a fragment shader's varyings). Each
    /// with its parameter's index, in the order written, then the defaults.
    fn bind_args(&mut self, b: &Bound, skip: Option<TyId>) -> Vec<(usize, Expr)> {
        let (params, _, _) = self.fn_params(b.func, &b.gen_args);
        self.fn_obligations(b.func, &b.gen_args, b.span);
        let skip = skip.map(|t| self.infer.resolve(&self.p.types, t));
        let user: Vec<(usize, CallParam)> = params
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, p)| {
                !self.is_gpu_builtin(p.ty) && skip != Some(self.infer.resolve(&self.p.types, p.ty))
            })
            .collect();
        let name = self.p.func(b.func).name.clone();
        let args = match b.args {
            Some(args) => args,
            None => {
                // Named alone: it takes nothing, or only parameters with defaults.
                let needed: Vec<&str> =
                    user.iter().filter(|(_, p)| p.default.is_none()).map(|(_, p)| p.name).collect();
                if !needed.is_empty() {
                    let list: Vec<String> = needed.iter().map(|n| format!("`{n}`")).collect();
                    self.err(
                        Diagnostic::new(
                            codes::E0303,
                            b.span,
                            format!("`{name}` takes {}, so it's bound to them", list.join(", ")),
                        )
                        .with_secondary(self.p.func(b.func).sig_span, "declared here")
                        .with_help(format!("write `{name}.bind({})`", needed.join(", "))),
                    );
                    return Vec::new();
                }
                &[]
            }
        };
        let ups: Vec<CallParam> = user.iter().map(|(_, p)| *p).collect();
        let fname = format!("{name}.bind");
        let (out, _, order) = self.match_args_as(&fname, &ups, None, args, b.span, b.func, true);
        let mut out: Vec<Option<Expr>> = out.into_iter().map(Some).collect();
        order.into_iter().filter_map(|k| out[k].take().map(|e| (user[k].0, e))).collect()
    }

    /// E0603 for `dispatch` or `draw` in the form before entry points were bound (§12): their
    /// arguments among the call's own. With a fix that binds them, when the source is at hand
    /// and each argument has one place to go.
    fn unbound_gpu_call(&mut self, call: &str, rewrite: Option<(Span, String)>, span: Span) {
        let (form, note) = if call == "dispatch" {
            (
                "`dispatch(kernel.bind(...), groups: n)`",
                "a kernel's arguments go in its `.bind(...)`, matched as a call's are",
            )
        } else {
            (
                "`draw(vs.bind(...), fs.bind(...), vertices: n)`",
                "each shader's arguments go in its own `.bind(...)`, matched as a call's are; one that takes nothing is named alone",
            )
        };
        let mut d = Diagnostic::new(
            codes::E0603,
            span,
            format!("`{call}` takes its entry points bound to their arguments: {form}"),
        )
        .with_note(format!("{note} (§12)"));
        if let Some((at, text)) = rewrite {
            d = d.with_fix("bind the arguments", at, text);
        }
        self.err(d);
    }

    /// What's written at each of `args`, if the source is at hand.
    fn texts(&self, args: &[&ast::Arg]) -> Option<Vec<String>> {
        args.iter().map(|a| self.p.text(a.span).map(str::to_string)).collect()
    }

    /// `name.bind(a, b)`, or `name` with no arguments.
    fn bound_text(name: &str, args: &[String]) -> String {
        if args.is_empty() { name.to_string() } else { format!("{name}.bind({})", args.join(", ")) }
    }

    /// A user parameter of `f` (not a GPU builtin) called `name`.
    fn takes(&self, f: FnId, name: &str) -> bool {
        self.p.func(f).params.iter().any(|p| p.name == name && !self.is_gpu_builtin(p.ty))
    }

    /// The function a path names, quietly.
    fn fn_named(&self, e: &ast::Expr) -> Option<FnId> {
        let ast::ExprKind::Path(path) = &e.kind else { return None };
        match resolve::resolve_value_item(self.p, self.scope.module, path) {
            Some(Res::Fn(f)) => Some(f),
            _ => None,
        }
    }

    /// Whether a parameter type is a GPU builtin (supplied by the GPU, not the caller).
    /// Whether an entry point's parameter of type `t` is supplied by the GPU, not passed: a
    /// builtin input, or workgroup memory.
    pub(crate) fn is_gpu_builtin(&self, t: TyId) -> bool {
        crate::gpu::BuiltinInput::of(self.p, t).is_some()
            || matches!(self.kind(t), &TyKind::Adt(a, _) if self.p.is_lang_adt(a, Lang::Shared))
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

    /// Whether `t` holds a dispatch's or draw's counts: a `GpuBuffer<u32>` or `GpuSpan<u32>`.
    fn counts_buffer(&mut self, t: TyId) -> bool {
        let t = self.infer.resolve(&self.p.types, t);
        let u32_ty = self.p.types.u32;
        match self.kind(t) {
            TyKind::Adt(a, args) => {
                matches!(self.p.adt(*a).lang, Some(Lang::GpuBuffer | Lang::GpuSpan))
                    && args.first().is_some_and(|&e| self.infer.resolve(&self.p.types, e) == u32_ty)
            }
            _ => false,
        }
    }

    /// A dispatch's or draw's argument for an entry point's parameter `p` (§6.13). A buffer
    /// parameter takes a `GpuBuffer<T>` or a span of one: for `[T]`, the buffer borrowed or a
    /// `GpuSpan<T>`; for `mut Slots<T>` (or `mut [T]`), `mut buf` or a `GpuSpanMut<T>`. Any
    /// other parameter takes its own type.
    fn gpu_arg(&mut self, p: &CallParam, value: &ast::Expr) -> Expr {
        // An `Append<T>` takes `mut` an `AppendBuffer<T>`, an `AtomicMap` an `AtomicMapBuffer`.
        let container = match self.kind(p.ty) {
            TyKind::Adt(s, args) if self.p.is_lang_adt(*s, Lang::Append) => {
                Some((Lang::AppendBuffer, args.clone()))
            }
            TyKind::Adt(s, _) if self.p.is_lang_adt(*s, Lang::AtomicMap) => {
                Some((Lang::AtomicMapBuffer, Vec::new()))
            }
            _ => None,
        };
        if let Some((l, args)) = container {
            let want = self.p.lang_adt(l).map_or(self.p.types.error, |g| self.p.types.adt(g, args));
            let e = self.check_expect(value, want);
            if !matches!(e.kind, ExprKind::MutArg(_)) && e.is_place() {
                self.err(
                    Diagnostic::new(
                        codes::E0503,
                        e.span,
                        format!("`{}` is written by the GPU, so it's passed `mut ...`", p.name),
                    )
                    .with_note("every mutation is visible where it happens (§6.2, §6.13)")
                    .with_fix("add `mut`", e.span.shrink_to_start(), "mut "),
                );
            }
            return e;
        }
        let elem = match self.kind(p.ty) {
            TyKind::Slice(t) => Some(*t),
            TyKind::Adt(s, args)
                if self.p.is_lang_adt(*s, Lang::Slots) || self.p.is_lang_adt(*s, Lang::Atomics) =>
            {
                args.first().copied()
            }
            _ => None,
        };
        let Some(elem) = elem else { return self.check_expect(value, p.ty) };
        let writes = p.mode == Mode::Mut || matches!(self.kind(p.ty), TyKind::Adt(..));
        let e = self.check_expr(value, None);
        let of = |this: &mut Self, l: Lang| {
            this.p.lang_adt(l).map_or(this.p.types.error, |g| this.p.types.adt(g, vec![elem]))
        };
        let t = self.infer.resolve(&self.p.types, e.ty);
        let lang = match self.kind(t) {
            TyKind::Adt(a, _) => self.p.adt(*a).lang,
            _ => None,
        };
        match lang {
            Some(Lang::GpuSpanMut) => {
                let want = of(self, Lang::GpuSpanMut);
                self.expect(e.ty, want, e.span);
            }
            Some(Lang::GpuSpan) if !writes => {
                let want = of(self, Lang::GpuSpan);
                self.expect(e.ty, want, e.span);
            }
            Some(Lang::GpuSpan) => {
                self.err(
                    Diagnostic::new(
                        codes::E0603,
                        e.span,
                        format!("`{}` is written by the GPU, and a `GpuSpan` is read-only", p.name),
                    )
                    .with_help("pass a `GpuSpanMut`: `buf.span_mut(start, count)`"),
                );
            }
            _ => {
                let want = of(self, Lang::GpuBuffer);
                self.expect(e.ty, want, e.span);
                let marked = matches!(e.kind, ExprKind::MutArg(_));
                if writes && !marked && e.is_place() {
                    self.err(
                        Diagnostic::new(
                            codes::E0503,
                            e.span,
                            format!(
                                "`{}` is written by the GPU, so its buffer is passed `mut ...`",
                                p.name
                            ),
                        )
                        .with_note("every mutation is visible where it happens (§6.2, §6.13)")
                        .with_fix(
                            "add `mut`",
                            e.span.shrink_to_start(),
                            "mut ",
                        ),
                    );
                } else if !writes && marked {
                    let ExprKind::MutArg(inner) = &e.kind else { unreachable!() };
                    self.err(
                        Diagnostic::new(
                            codes::E0504,
                            e.span,
                            format!("`{}` is only read, so its buffer isn't passed `mut`", p.name),
                        )
                        .with_fix(
                            "remove `mut`",
                            Span::new(e.span.file, e.span.start, inner.span.start),
                            "",
                        ),
                    );
                }
            }
        }
        e
    }

    /// `dispatch(kernel.bind(...), groups: n)`: `groups` is the workgroup count, a `u32`, a
    /// `(u32, u32, u32)`, or a buffer holding them (indirect).
    fn check_dispatch(&mut self, args: &[ast::Arg], span: Span) -> Expr {
        let unit = self.p.types.unit;
        let u32_ty = self.p.types.u32;
        let is_groups =
            |i: usize, a: &ast::Arg| a.name.as_ref().map_or(i == 1, |n| n.name == "groups");
        // The form before binding: the kernel's arguments among `dispatch`'s own.
        let extra: Vec<&ast::Arg> = args
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(i, a)| !is_groups(*i, a))
            .map(|(_, a)| a)
            .collect();
        if !extra.is_empty()
            && args.first().is_some_and(|a| matches!(a.value.kind, ast::ExprKind::Path(_)))
        {
            let groups =
                args.iter().enumerate().skip(1).find(|(i, a)| is_groups(*i, a)).map(|(_, a)| a);
            let rewrite = (|| {
                let kernel = self.p.text(args[0].value.span)?;
                let kargs = self.texts(&extra)?;
                let g = groups?;
                let gtext = self.p.text(g.value.span)?;
                let first = args[0].span;
                let last = args[args.len() - 1].span;
                let at = Span::new(first.file, first.start, last.end);
                Some((at, format!("{}, groups: {gtext}", Self::bound_text(kernel, &kargs))))
            })();
            self.unbound_gpu_call("dispatch", rewrite, span);
            self.check_args_alone(&args[1..]);
            return self.error_expr(span);
        }
        let Some(first) = args.first().filter(|a| a.name.is_none()) else {
            self.err(Diagnostic::new(
                codes::E0603,
                span,
                "`dispatch` takes the kernel first: `dispatch(kernel.bind(...), groups: n)`",
            ));
            self.check_args_alone(args);
            return self.error_expr(span);
        };
        let Some(bound) = self.bound_entry(&first.value, Entry::Compute([1, 1, 1])) else {
            self.check_args_alone(&args[1..]);
            return self.error_expr(span);
        };
        let kernel_args = self.bind_args(&bound, None);
        let mut groups: Option<Expr> = None;
        let mut indirect = false;
        for (i, a) in args.iter().enumerate().skip(1) {
            if !is_groups(i, a) {
                continue;
            }
            let e = self.check_expr(&a.value, None);
            if groups.is_some() {
                self.err(Diagnostic::new(codes::E0304, a.span, "`groups` is given twice"));
                continue;
            }
            match self.kind(e.ty) {
                TyKind::Tuple(ts) if ts.len() == 3 => {
                    for &t in ts {
                        self.expect(t, u32_ty, e.span);
                    }
                }
                _ if self.counts_buffer(e.ty) => indirect = true,
                _ => {
                    self.expect(e.ty, u32_ty, e.span);
                }
            }
            groups = Some(e);
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
        let groups_at = kernel_args.len();
        let d = Dispatch {
            kernel: bound.func,
            kernel_args: bound.gen_args,
            groups: Box::new(groups),
            indirect,
            args: kernel_args,
            groups_at,
        };
        Expr { ty: unit, span, kind: ExprKind::Dispatch(Box::new(d)) }
    }

    /// `draw(vs.bind(...), fs.bind(...), vertices: n)`, with `instances: m` or, for both,
    /// `indirect: buf`.
    fn check_draw(&mut self, args: &[ast::Arg], span: Span) -> Expr {
        let unit = self.p.types.unit;
        let u32_ty = self.p.types.u32;
        let counts = |a: &ast::Arg| {
            a.name
                .as_ref()
                .is_some_and(|n| matches!(n.name.as_str(), "vertices" | "instances" | "indirect"))
        };
        // The form before binding: the shaders' arguments among `draw`'s own, by name.
        let extra: Vec<&ast::Arg> =
            args.iter().skip(2).filter(|a| a.name.is_some() && !counts(a)).collect();
        if !extra.is_empty() && args.len() >= 2 {
            let shaders = (self.fn_named(&args[0].value), self.fn_named(&args[1].value));
            let rewrite = (|| {
                let (Some(vs), Some(fs)) = shaders else { return None };
                let (mut vargs, mut fargs) = (Vec::new(), Vec::new());
                for a in &extra {
                    let name = &a.name.as_ref()?.name;
                    let text = self.p.text(a.span)?.to_string();
                    let (v, f) = (self.takes(vs, name), self.takes(fs, name));
                    if !v && !f {
                        return None;
                    }
                    if v {
                        vargs.push(text.clone());
                    }
                    if f {
                        fargs.push(text);
                    }
                }
                let rest: Vec<&ast::Arg> = args.iter().skip(2).filter(|a| counts(a)).collect();
                let mut parts = vec![
                    Self::bound_text(self.p.text(args[0].value.span)?, &vargs),
                    Self::bound_text(self.p.text(args[1].value.span)?, &fargs),
                ];
                parts.extend(self.texts(&rest)?);
                let first = args[0].span;
                let last = args[args.len() - 1].span;
                Some((Span::new(first.file, first.start, last.end), parts.join(", ")))
            })();
            self.unbound_gpu_call("draw", rewrite, span);
            self.check_args_alone(&args[2..]);
            return self.error_expr(span);
        }
        if args.len() < 2 || args[0].name.is_some() || args[1].name.is_some() {
            self.err(Diagnostic::new(codes::E0603, span, "`draw` takes the vertex and fragment shaders first: `draw(vs, fs.bind(...), vertices: n)`"));
            self.check_args_alone(args);
            return self.error_expr(span);
        }
        let vs = self.bound_entry(&args[0].value, Entry::Vertex);
        let fs = self.bound_entry(&args[1].value, Entry::Fragment);
        let (Some(vs), Some(fs)) = (vs, fs) else {
            self.check_args_alone(&args[2..]);
            return self.error_expr(span);
        };
        // The fragment shader's parameter of the vertex output's struct is the vertex output,
        // whatever its generic arguments are inferred to be (`draw(half, shade, ...)`); the
        // pipeline passes it, not the draw.
        let (_, vret, _) = self.fn_params(vs.func, &vs.gen_args);
        let (fparams, _, _) = self.fn_params(fs.func, &fs.gen_args);
        let mut varyings = None;
        if let &TyKind::Adt(out, _) = self.kind(vret)
            && let Some(p) =
                fparams.iter().find(|p| matches!(self.kind(p.ty), &TyKind::Adt(a, _) if a == out))
        {
            let _ = self.try_unify(p.ty, vret);
            varyings = Some(vret);
        }
        let mut bound: Vec<(usize, usize, Expr)> = Vec::new();
        for (e, i, x) in self.bind_args(&vs, None).into_iter().map(|(i, x)| (0, i, x)) {
            bound.push((e, i, x));
        }
        for (i, x) in self.bind_args(&fs, varyings) {
            bound.push((1, i, x));
        }
        let mut vertices = None;
        let mut instances = None;
        let mut indirect: Option<Box<Expr>> = None;
        let mut counts = Vec::new();
        for a in &args[2..] {
            let Some(n) = &a.name else {
                self.err(
                    Diagnostic::new(
                        codes::E0603,
                        a.span,
                        "after the shaders, `draw`'s counts are named",
                    )
                    .with_help("e.g. `vertices: 3`, `instances: n` or `indirect: counts`"),
                );
                self.check_expr(&a.value, None);
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
                "indirect" => {
                    let e = self.check_expr(&a.value, None);
                    if !self.counts_buffer(e.ty) && !matches!(self.kind(e.ty), TyKind::Error) {
                        self.err(
                            Diagnostic::new(
                                codes::E0603,
                                e.span,
                                format!("`indirect:` takes a `GpuBuffer<u32>` or a `GpuSpan<u32>` of the counts, not a `{}`", self.display(e.ty)),
                            )
                            .with_note("the buffer holds the vertex count, the instance count, the first vertex and the first instance"),
                        );
                    }
                    if indirect.is_some() {
                        twice(self);
                        continue;
                    }
                    indirect = Some(Box::new(e));
                    counts.push(DrawCount::Indirect);
                }
                "vertices" | "instances" => {
                    let e = self.check_expect(&a.value, u32_ty);
                    let (slot, c) = if n.name == "vertices" {
                        (&mut vertices, DrawCount::Vertices)
                    } else {
                        (&mut instances, DrawCount::Instances)
                    };
                    if slot.is_some() {
                        twice(self);
                        continue;
                    }
                    *slot = Some(e);
                    counts.push(c);
                }
                _ => unreachable!("the shaders' arguments are bound (above)"),
            }
        }
        if indirect.is_some() && (vertices.is_some() || instances.is_some()) {
            self.err(Diagnostic::new(
                codes::E0603,
                span,
                "a draw takes its counts from `indirect:` or from `vertices:` and `instances:`, not both",
            ));
        }
        let vertices = match (vertices, &indirect) {
            (Some(v), _) => v,
            (None, Some(_)) => Expr { ty: u32_ty, span, kind: ExprKind::Lit(Lit::Int(0)) },
            (None, None) => {
                self.err(Diagnostic::new(
                    codes::E0603,
                    span,
                    "`draw` needs `vertices:`, how many vertices to draw",
                ));
                return self.error_expr(span);
            }
        };
        let instances =
            instances.unwrap_or(Expr { ty: u32_ty, span, kind: ExprKind::Lit(Lit::Int(1)) });
        let d = Draw {
            vertex: (vs.func, vs.gen_args),
            fragment: (fs.func, fs.gen_args),
            vertices,
            instances,
            indirect,
            args: bound,
            counts,
        };
        Expr { ty: unit, span, kind: ExprKind::Draw(Box::new(d)) }
    }
}

/// An entry point as `dispatch` and `draw` take it (§12): which one, its generic arguments,
/// and the arguments of its `.bind(...)` (none when it's named alone).
struct Bound<'a> {
    func: FnId,
    gen_args: Vec<TyId>,
    args: Option<&'a [ast::Arg]>,
    span: Span,
}

/// Whether `e` is a bare literal: `Some(true)` for a `bool`, `Some(false)` for a number (with a
/// unit suffix or a minus too), `None` otherwise.
fn bare_literal(e: &ast::Expr) -> Option<bool> {
    match &e.kind {
        ast::ExprKind::Lit(l) => match l.kind {
            ast::LitKind::Bool(_) => Some(true),
            ast::LitKind::Int(_) | ast::LitKind::Float(_) | ast::LitKind::Suffixed => Some(false),
            ast::LitKind::Str => None,
        },
        ast::ExprKind::Unary(ast::UnOp::Neg, x) => bare_literal(x).filter(|b| !b),
        _ => None,
    }
}

fn bare_true(e: &ast::Expr) -> bool {
    matches!(&e.kind, ast::ExprKind::Lit(l) if l.kind == ast::LitKind::Bool(true))
}

/// A call that borrows each argument, evaluates them in order, and returns a value.
fn plain_call(callee: Callee, args: Vec<Expr>, receiver: bool, ty: TyId, span: Span) -> Expr {
    let n = args.len();
    let modes = vec![Mode::Borrow; n];
    let call =
        Call { callee, args, modes, order: (0..n).collect(), receiver, ret_mode: RetMode::Owned };
    Expr { ty, span, kind: ExprKind::Call(call) }
}

/// The two operands of a comparison: of numbers, `bool` and vectors (`Binary`), or through
/// `Eq` (`a.eq(b)`, negated for `!=`) or `Ord` (`a.cmp(b) == Ordering::Less`).
fn comparison_operands<'e>(
    p: &crate::program::Program,
    e: &'e mut Expr,
) -> Option<(&'e mut Expr, &'e mut Expr)> {
    let is_cmp_call = |e: &Expr| match &e.kind {
        ExprKind::Call(c) => match c.callee {
            Callee::TraitMethod { method, .. } => {
                matches!(p.func(method).owner, FnOwner::Trait(t)
                    if p.is_lang_trait(t, Lang::Eq) || p.is_lang_trait(t, Lang::Ord))
                    && c.args.len() == 2
            }
            _ => false,
        },
        _ => false,
    };
    let operands = |e: &'e mut Expr| match &mut e.kind {
        ExprKind::Call(c) => match c.args.as_mut_slice() {
            [a, b] => Some((a, b)),
            _ => None,
        },
        _ => None,
    };
    if is_cmp_call(e) {
        return operands(e);
    }
    match &mut e.kind {
        ExprKind::Unary(ast::UnOp::Not, inner) if is_cmp_call(inner) => operands(inner),
        ExprKind::Binary(op, a, b) if op.is_comparison() => {
            if is_cmp_call(a) {
                operands(a)
            } else {
                Some((a, b))
            }
        }
        _ => None,
    }
}

/// For a number literal, possibly negated or parenthesized: whether it's negative.
/// The value of a negated integer literal (`-3000000000`), if `e` is one.
fn negative_literal(e: &ast::Expr) -> Option<i128> {
    fn int(e: &ast::Expr) -> Option<i128> {
        match &e.kind {
            ast::ExprKind::Lit(ast::Lit {
                kind: ast::LitKind::Int(ast::IntValue::Ok(v)), ..
            }) => Some(i128::from(*v)),
            ast::ExprKind::Paren(x) => int(x),
            _ => None,
        }
    }
    match &e.kind {
        ast::ExprKind::Unary(ast::UnOp::Neg, x) => int(x).map(|v| -v),
        ast::ExprKind::Paren(x) => negative_literal(x),
        _ => None,
    }
}

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

/// Why `embed` can't read `path`, if it can't: it must be relative to the package's root, use
/// `/`, and stay inside the package (§10).
pub(crate) fn embed_path_problem(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        return Some("the path is empty");
    }
    if path.starts_with('/') || path.contains('\\') || path.contains(':') {
        return Some("the path must be relative to the package's root, with `/` between its parts");
    }
    if path.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
        return Some(
            "each part of the path names a directory or the file: no `.`, `..` or empty parts",
        );
    }
    None
}
