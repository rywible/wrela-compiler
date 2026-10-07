//! Fieldwise derivation (language.md §3): the methods of a `@fieldwise` trait, for a type that
//! declares the trait and for tuples and arrays, derived field by field (element by element)
//! and built as a typed tree. No source is written or resolved: each derived method is an
//! ordinary function to the memory checker and to lowering. An array's is a loop over its
//! elements; a tuple's or an array's elements have no names, so no field hook is called.
//!
//! A method's **shape** is what it returns, and decides how the fields' results combine:
//!
//! | Returns | Takes `self` | Derived as |
//! |---|---|---|
//! | `()` | yes | each field's call, in order |
//! | `Result<(), E>` | yes | each field's call, in order, stopping at the first `Err` |
//! | `bool` | yes | every field's call, `&&`: the first `false` decides |
//! | `Ordering` | yes | the first field's call that isn't `Equal` decides |
//! | `Self` | either | a value built from each field's result |
//! | `Result<Self, E>` | either | the same, stopping at the first `Err` |
//!
//! A parameter of type `Self` is taken field by field too; any other parameter is passed to
//! every field's call. For an enum, the variant comes first: with two `Self` values of
//! different variants, a `bool` method is false, an `Ordering` one orders the variants as
//! they're declared, and anything else runs the trait's default body (a method without one
//! can't be derived for an enum). A method without `self` builds an enum through its trait's
//! variant hook.
//!
//! **Hooks** are how a trait learns the names, and chooses the variant. For a method `m`, the
//! trait may declare, with default bodies:
//! - `m_field(name: Text, ...)`, called before each field with its name and the method's
//!   other parameters (it returns `()`, or `Result<(), E>` for a method that returns a
//!   `Result`);
//! - `m_variant(index: u32, name: Text, ...)`, called before an enum's fields with its variant;
//!   or, for a method that builds a value without `self`,
//!   `m_variant(names: [Text], ...) -> u32` (or `Result<u32, E>`), which chooses the variant
//!   by its index into `names`.

use crate::defs::*;
use crate::program::Program;
use crate::thir::*;
use crate::ty::*;
use wrela_diag::{Diagnostic, Span, codes};

/// What a derivable method returns: how the fields' results combine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Unit,
    TryUnit,
    All,
    Order,
    Build,
    TryBuild,
}

impl Shape {
    fn tries(self) -> bool {
        matches!(self, Shape::TryUnit | Shape::TryBuild)
    }
}

/// Whether `f`, a method of trait `t`, is a hook (`m_field` or `m_variant` of a method `m` of
/// `t`).
pub fn is_hook(p: &Program, t: TraitId, f: FnId) -> bool {
    let name = &p.func(f).name;
    let tr = p.trait_(t);
    ["_field", "_variant"].iter().any(|suffix| {
        name.strip_suffix(suffix)
            .is_some_and(|m| tr.methods.iter().any(|&g| g != f && p.func(g).name == m))
    })
}

/// The hook of trait `t` named `{m}{suffix}`, for its method `m`.
fn hook(p: &Program, t: TraitId, m: FnId, suffix: &str) -> Option<FnId> {
    crate::traits::trait_method(p, t, &format!("{}{suffix}", p.func(m).name))
}

/// The shape of method `m` of trait `t`, or why it can't be derived.
pub fn shape(p: &Program, t: TraitId, m: FnId) -> Result<Shape, String> {
    let tr = p.trait_(t);
    let def = p.func(m);
    let self_ty = p.types.param(tr.self_param);
    let mentions_self = |ty: TyId| p.types.any(ty, &mut |k| *k == TyKind::Param(tr.self_param));
    let receiver = def.has_self();
    for (k, ps) in def.params.iter().enumerate() {
        if ps.is_self || ps.ty == self_ty {
            if !receiver && k > 0 {
                return Err(format!("`{}` takes a `Self` but not `self`", ps.name));
            }
            continue;
        }
        if mentions_self(ps.ty) {
            return Err(format!(
                "`{}` has type `{}`, which holds a `Self` but isn't one",
                ps.name,
                p.display_ty(ps.ty)
            ));
        }
        if ps.mode == Mode::Take {
            return Err(format!("`{}` is `take`, and it would be passed to every field", ps.name));
        }
    }
    if def.ret_mode != RetMode::Owned {
        return Err("it returns a projection".into());
    }
    let result = p.lang_adt(Lang::Result);
    let ordering = p.lang_adt(Lang::Ordering);
    let shape = match p.types.kind(def.ret) {
        _ if def.ret == p.types.unit => Shape::Unit,
        _ if def.ret == p.types.bool => Shape::All,
        _ if def.ret == self_ty => Shape::Build,
        TyKind::Adt(a, _) if Some(*a) == ordering => Shape::Order,
        TyKind::Adt(a, args) if Some(*a) == result && args[0] == self_ty => Shape::TryBuild,
        TyKind::Adt(a, args) if Some(*a) == result && args[0] == p.types.unit => Shape::TryUnit,
        _ => {
            return Err(format!(
                "it returns `{}`; a derived method returns `()`, `bool`, `Ordering`, `Self`, `Result<Self, E>` or `Result<(), E>`",
                p.display_ty(def.ret)
            ));
        }
    };
    if !receiver && !matches!(shape, Shape::Build | Shape::TryBuild) {
        return Err("without `self`, a derived method builds a `Self`".into());
    }
    if let TyKind::Adt(_, args) = p.types.kind(def.ret)
        && shape.tries()
        && mentions_self(args[1])
    {
        return Err("its error type holds a `Self`".into());
    }
    Ok(shape)
}

/// E0416: a `@fieldwise` trait's methods that can't be derived and have no default, and hooks
/// whose signatures don't fit their methods.
pub fn check_trait(p: &Program, t: TraitId) -> Vec<Diagnostic> {
    let tr = p.trait_(t);
    let mut out = Vec::new();
    if !tr.fieldwise || tr.lang.is_some_and(Lang::is_structural) {
        return out;
    }
    for &m in &tr.methods {
        let def = p.func(m);
        if is_hook(p, t, m) {
            if def.body.is_none() {
                out.push(Diagnostic::new(
                    codes::E0414,
                    def.sig_span,
                    format!("the hook `{}` needs a default body: a derivation calls it", def.name),
                ));
            }
            continue;
        }
        let s = match shape(p, t, m) {
            Ok(s) => s,
            Err(why) => {
                if def.body.is_none() {
                    out.push(
                        Diagnostic::new(
                            codes::E0416,
                            def.sig_span,
                            format!("`{}` can't be derived field by field: {why}", def.name),
                        )
                        .with_note("a `@fieldwise` trait's methods are derived for each type that declares it, unless they have a default (§3)")
                        .with_help("give it a default body, or change its signature"),
                    );
                }
                continue;
            }
        };
        let passthrough: Vec<&ParamSig> = def
            .params
            .iter()
            .filter(|ps| !ps.is_self && ps.ty != p.types.param(tr.self_param))
            .collect();
        let text = p.lang_adt(Lang::Text);
        let is_text = |ty: TyId| {
            matches!(p.types.kind(ty), TyKind::Str)
                || matches!(p.types.kind(ty), TyKind::Adt(a, _) if Some(*a) == text)
        };
        let unit_or_try = |ty: TyId| {
            if s.tries() {
                matches!(p.types.kind(ty), TyKind::Adt(a, args) if Some(*a) == p.lang_adt(Lang::Result) && args[0] == p.types.unit && Some(args[1]) == result_err(p, def.ret))
            } else {
                ty == p.types.unit
            }
        };
        let rest_fits = |ps: &[ParamSig]| {
            ps.len() == passthrough.len()
                && ps.iter().zip(&passthrough).all(|(a, b)| a.ty == b.ty && a.mode == b.mode)
        };
        if let Some(h) = hook(p, t, m, "_field") {
            let hd = p.func(h);
            let ok = !hd.has_self()
                && hd.params.first().is_some_and(|ps| is_text(ps.ty))
                && rest_fits(&hd.params[1..])
                && unit_or_try(hd.ret);
            if !ok {
                out.push(bad_hook(
                    p,
                    h,
                    &format!(
                        "`fn {}(name: Text, ...)`, then {}'s other parameters, returning {}",
                        hd.name,
                        def.name,
                        if s.tries() { "`Result<(), E>`" } else { "`()`" }
                    ),
                ));
            }
        }
        if let Some(h) = hook(p, t, m, "_variant") {
            let hd = p.func(h);
            let ok = if def.has_self() {
                !hd.has_self()
                    && hd.params.len() >= 2
                    && hd.params[0].ty == p.types.u32
                    && is_text(hd.params[1].ty)
                    && rest_fits(&hd.params[2..])
                    && unit_or_try(hd.ret)
            } else {
                let index_ok = if s.tries() {
                    matches!(p.types.kind(hd.ret), TyKind::Adt(a, args) if Some(*a) == p.lang_adt(Lang::Result) && args[0] == p.types.u32)
                } else {
                    hd.ret == p.types.u32
                };
                !hd.has_self()
                    && hd.params.first().is_some_and(
                        |ps| matches!(p.types.kind(ps.ty), TyKind::Slice(e) if is_text(*e)),
                    )
                    && rest_fits(&hd.params[1..])
                    && index_ok
            };
            if !ok {
                let want = if def.has_self() {
                    format!(
                        "`fn {}(index: u32, name: Text, ...)`, then {}'s other parameters",
                        hd.name, def.name
                    )
                } else {
                    format!(
                        "`fn {}(names: [Text], ...) -> u32`, then {}'s other parameters",
                        hd.name, def.name
                    )
                };
                out.push(bad_hook(p, h, &want));
            }
        }
    }
    out
}

/// The error type of a `Result<_, E>`.
fn result_err(p: &Program, t: TyId) -> Option<TyId> {
    match p.types.kind(t) {
        TyKind::Adt(a, args) if Some(*a) == p.lang_adt(Lang::Result) => args.get(1).copied(),
        _ => None,
    }
}

fn bad_hook(p: &Program, h: FnId, want: &str) -> Diagnostic {
    Diagnostic::new(
        codes::E0414,
        p.func(h).sig_span,
        format!("the hook `{}` doesn't fit its method", p.func(h).name),
    )
    .with_note(format!("a derivation calls it as {want} (§3)"))
}

/// The derived body of `f`, a method of a type's `@fieldwise` impl.
pub fn derive(p: &Program, f: FnId) -> (Option<Body>, Vec<Diagnostic>) {
    let def = p.func(f);
    let Some(d) = &def.derived else { return (None, Vec::new()) };
    let span = match def.owner {
        FnOwner::Impl(i) => p.impl_(i).span,
        _ => def.span,
    };
    let tr = d.trait_ref.trait_;
    let shape = match shape(p, tr, d.method) {
        Ok(s) => s,
        Err(_) => return (None, Vec::new()), // reported at the trait
    };
    let FnOwner::Impl(imp) = def.owner else { return (None, Vec::new()) };
    let self_ty = p.impl_(imp).self_ty;
    let of = match p.types.kind(self_ty) {
        TyKind::Adt(a, args) => Of::Adt(*a, args.clone()),
        TyKind::Tuple(ts) => Of::Tuple(ts.clone()),
        TyKind::ArrayN(e, n) => Of::Array(*e, *n),
        _ => return (None, Vec::new()),
    };
    let method_args: Vec<TyId> = def.generics.iter().map(|&g| p.types.param(g)).collect();
    let mut b = Builder {
        p,
        locals: Vec::new(),
        span,
        of,
        self_ty,
        trait_: tr,
        method: d.method,
        trait_args: d.trait_ref.args.clone(),
        method_args,
        shape,
        params: Vec::new(),
        self_params: Vec::new(),
        ret: def.ret,
        diags: Vec::new(),
    };
    for (k, ps) in def.params.iter().enumerate() {
        let l = b.local(&ps.name, ps.ty, LocalKind::Param(ps.mode));
        b.params.push(l);
        if ps.is_self || ps.ty == self_ty {
            b.self_params.push(k);
        }
    }
    let value = b.body();
    let body = value.map(|v| Body {
        params: b.params.clone(),
        locals: std::mem::take(&mut b.locals),
        closures: Vec::new(),
        value: v,
        hidden_ret: None,
    });
    (body, b.diags)
}

/// What a derivation takes apart: a struct or an enum, a tuple, or an array.
enum Of {
    Adt(AdtId, Vec<TyId>),
    Tuple(Vec<TyId>),
    /// `[T; N]`: the element type, and the length (a `const` parameter).
    Array(TyId, TyId),
}

struct Builder<'p> {
    p: &'p Program,
    locals: Vec<LocalDecl>,
    span: Span,
    of: Of,
    self_ty: TyId,
    trait_: TraitId,
    method: FnId,
    trait_args: Vec<TyId>,
    method_args: Vec<TyId>,
    shape: Shape,
    params: Vec<LocalId>,
    /// The parameters of type `Self`, the receiver first if it is one.
    self_params: Vec<usize>,
    ret: TyId,
    diags: Vec<Diagnostic>,
}

impl Builder<'_> {
    fn e(&self, ty: TyId, kind: ExprKind) -> Expr {
        Expr { ty, kind, span: self.span }
    }

    fn local(&mut self, name: &str, ty: TyId, kind: LocalKind) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(LocalDecl {
            name: name.into(),
            ty,
            kind,
            span: self.span,
            keyword: None,
            shorthand: false,
            closure: None,
        });
        id
    }

    fn var(&self, l: LocalId) -> Expr {
        self.e(self.locals[l.index()].ty, ExprKind::Local(l))
    }

    fn block(&self, stmts: Vec<Stmt>, tail: Option<Expr>) -> Expr {
        let ty = tail.as_ref().map_or(self.p.types.unit, |t| t.ty);
        let span = self.span;
        self.e(ty, ExprKind::Block(Block { stmts, tail: tail.map(Box::new), ty, span }))
    }

    fn stmt(&self, e: Expr) -> Stmt {
        Stmt { kind: StmtKind::Expr(e), span: self.span }
    }

    fn text(&self, s: &str) -> Expr {
        let ty = match self.p.lang_adt(Lang::Text) {
            Some(a) => self.p.types.adt(a, Vec::new()),
            None => self.p.types.error,
        };
        self.e(ty, ExprKind::Text(s.into()))
    }

    fn u32_lit(&self, n: u32) -> Expr {
        self.e(self.p.types.u32, ExprKind::Lit(Lit::Int(i128::from(n))))
    }

    fn is_copy(&self, t: TyId) -> bool {
        crate::traits::implements_builtin(self.p, t, Lang::Copy)
    }

    /// `x` as an argument of a parameter in `mode`.
    fn arg(&self, x: Expr, mode: Mode) -> Expr {
        let ty = x.ty;
        match mode {
            Mode::Borrow => x,
            Mode::Mut => self.e(ty, ExprKind::MutArg(Box::new(x))),
            Mode::Take => self.e(ty, ExprKind::Take(Box::new(x))),
        }
    }

    /// A value read whole: moved if it isn't `Copy` (the bindings here own what they hold).
    fn moved(&self, x: Expr) -> Expr {
        if self.is_copy(x.ty) {
            x
        } else {
            let ty = x.ty;
            self.e(ty, ExprKind::Take(Box::new(x)))
        }
    }

    /// The generic arguments of a method of the trait (its own, and the trait's), with `Self`
    /// as `self_ty`.
    fn args_for(&self, f: FnId, self_ty: TyId) -> Vec<TyId> {
        let tr = self.p.trait_(self.trait_);
        self.p
            .fn_all_generics(f)
            .iter()
            .map(|&g| {
                if g == tr.self_param {
                    self_ty
                } else if let Some(i) = tr.generics.iter().position(|&x| x == g) {
                    self.trait_args[i]
                } else {
                    let own = &self.p.func(f).generics;
                    own.iter()
                        .position(|&x| x == g)
                        .and_then(|i| self.method_args.get(i).copied())
                        .unwrap_or(self.p.types.error)
                }
            })
            .collect()
    }

    /// A call of trait method `f` for `Self = self_ty`.
    fn trait_call(&self, f: FnId, self_ty: TyId, args: Vec<Expr>) -> Expr {
        let def = self.p.func(f);
        let subst = Subst::from_pairs(&self.p.fn_all_generics(f), &self.args_for(f, self_ty));
        let ret = self.p.types.subst(def.ret, &subst);
        let n = args.len();
        let call = Call {
            callee: Callee::TraitMethod {
                method: f,
                self_ty,
                trait_args: self.trait_args.clone(),
                method_args: self.method_args.clone(),
            },
            args,
            modes: def.params.iter().map(|ps| ps.mode).collect(),
            receiver: def.has_self(),
            order: (0..n).collect(),
            ret_mode: def.ret_mode,
        };
        self.e(ret, ExprKind::Call(call))
    }

    /// The method's parameters that aren't `Self`, as arguments (to a hook, or to a field's
    /// call in their places).
    fn passthrough(&self) -> Vec<Expr> {
        let def = self.p.func(self.method);
        def.params
            .iter()
            .enumerate()
            .filter(|(k, _)| !self.self_params.contains(k))
            .map(|(k, ps)| self.arg(self.var(self.params[k]), ps.mode))
            .collect()
    }

    /// The call of the method on one field of type `ft`: `parts` is the field of each `Self`
    /// parameter, in the order of [`Self::self_params`].
    fn field_call(&self, ft: TyId, parts: Vec<Expr>) -> Expr {
        let def = self.p.func(self.method);
        let mut parts = parts.into_iter();
        let args: Vec<Expr> = def
            .params
            .iter()
            .enumerate()
            .map(|(k, ps)| {
                let x = if self.self_params.contains(&k) {
                    parts.next().expect("a part per `Self` parameter")
                } else {
                    self.var(self.params[k])
                };
                self.arg(x, ps.mode)
            })
            .collect();
        self.trait_call(self.method, ft, args)
    }

    /// `e?` for a `Result`: its `Ok` value, or a return of its `Err`.
    fn try_(&mut self, e: Expr) -> Expr {
        let Some(result) = self.p.lang_adt(Lang::Result) else { return e };
        let TyKind::Adt(_, args) = self.p.types.kind(e.ty).clone() else { return e };
        let (value_ty, err_ty) = (args[0], args[1]);
        let v = self.local("value", value_ty, LocalKind::Owned { mutable: false });
        let x = self.local("error", err_ty, LocalKind::Owned { mutable: false });
        let bind = |l: LocalId, ty: TyId, span: Span| Pat { ty, kind: PatKind::Bind(l), span };
        let ok_pat = Pat {
            ty: e.ty,
            kind: PatKind::Adt {
                adt: result,
                args: args.clone(),
                variant: Some(0),
                fields: vec![(0, bind(v, value_ty, self.span))],
            },
            span: self.span,
        };
        let err_pat = Pat {
            ty: e.ty,
            kind: PatKind::Adt {
                adt: result,
                args: args.clone(),
                variant: Some(1),
                fields: vec![(0, bind(x, err_ty, self.span))],
            },
            span: self.span,
        };
        let ret_args = match self.p.types.kind(self.ret) {
            TyKind::Adt(_, a) => a.clone(),
            _ => vec![self.self_ty, err_ty],
        };
        let err_val = self.e(
            self.ret,
            ExprKind::Adt {
                adt: result,
                args: ret_args,
                variant: Some(1),
                fields: vec![self.moved(self.var(x))],
                order: vec![0],
                base: None,
            },
        );
        let never = self.p.types.never;
        let ret = self.e(never, ExprKind::Return(Some(Box::new(err_val))));
        let arms = vec![
            Arm { pat: ok_pat, guard: None, body: self.moved(self.var(v)), span: self.span },
            Arm { pat: err_pat, guard: None, body: ret, span: self.span },
        ];
        self.e(value_ty, ExprKind::Match { scrutinee: Box::new(e), arms, mutable: false })
    }

    /// A hook's call, through `?` if the method's shape tries.
    fn hook_call(&mut self, h: FnId, mut first: Vec<Expr>) -> Expr {
        first.extend(self.passthrough());
        let call = self.trait_call(h, self.self_ty, first);
        if self.shape.tries() { self.try_(call) } else { call }
    }

    /// `Ok(x)` of the method's result type.
    fn ok(&self, x: Expr) -> Expr {
        let Some(result) = self.p.lang_adt(Lang::Result) else { return x };
        let args = match self.p.types.kind(self.ret) {
            TyKind::Adt(_, a) => a.clone(),
            _ => return x,
        };
        self.e(
            self.ret,
            ExprKind::Adt {
                adt: result,
                args,
                variant: Some(0),
                fields: vec![x],
                order: vec![0],
                base: None,
            },
        )
    }

    fn ordering(&self, variant: u32) -> Expr {
        match self.p.lang_adt(Lang::Ordering) {
            Some(o) => self.e(
                self.p.types.adt(o, Vec::new()),
                ExprKind::Adt {
                    adt: o,
                    args: Vec::new(),
                    variant: Some(variant),
                    fields: Vec::new(),
                    order: Vec::new(),
                    base: None,
                },
            ),
            None => self.e(self.p.types.error, ExprKind::Error),
        }
    }

    /// The ADT taken apart, and its arguments.
    fn adt(&self) -> (AdtId, Vec<TyId>) {
        match &self.of {
            Of::Adt(a, args) => (*a, args.clone()),
            _ => unreachable!("only a struct or an enum has variants"),
        }
    }

    fn body(&mut self) -> Option<Expr> {
        let (fields, names) = match &self.of {
            Of::Adt(a, _) if self.p.adt(*a).is_enum() => return self.enum_body(),
            Of::Adt(a, args) => {
                let names: Vec<String> =
                    self.p.adt_fields(*a, None).iter().map(|f| f.name.clone()).collect();
                (self.p.fields_of(*a, args, None), Some(names))
            }
            // A tuple's fields have no names: the hooks aren't called for them.
            Of::Tuple(ts) => (ts.clone(), None),
            Of::Array(e, n) => {
                let (e, n) = (*e, *n);
                return Some(self.array_body(e, n));
            }
        };
        let parts: Vec<Vec<Expr>> = (0..fields.len())
            .map(|i| {
                self.self_params
                    .iter()
                    .map(|&k| {
                        let base = self.var(self.params[k]);
                        self.e(fields[i], ExprKind::Field(Box::new(base), i as u32))
                    })
                    .collect()
            })
            .collect();
        Some(self.combine(None, &fields, names.as_deref(), parts))
    }

    /// The fields' calls combined by the method's shape; `variant` builds that variant (for a
    /// `Self` result).
    fn combine(
        &mut self,
        variant: Option<u32>,
        fields: &[TyId],
        names: Option<&[String]>,
        parts: Vec<Vec<Expr>>,
    ) -> Expr {
        let field_hook = hook(self.p, self.trait_, self.method, "_field");
        let mut calls = Vec::new();
        for (i, (&ft, ps)) in fields.iter().zip(parts).enumerate() {
            let name = names.map(|ns| ns[i].clone());
            let hk = field_hook.zip(name).map(|(h, name)| {
                let t = self.text(&name);
                self.hook_call(h, vec![t])
            });
            let call = self.field_call(ft, ps);
            calls.push((hk, call));
        }
        let bool_ty = self.p.types.bool;
        match self.shape {
            Shape::Unit | Shape::TryUnit => {
                let mut stmts = Vec::new();
                for (hk, call) in calls {
                    if let Some(h) = hk {
                        stmts.push(self.stmt(h));
                    }
                    let call = if self.shape == Shape::TryUnit { self.try_(call) } else { call };
                    stmts.push(self.stmt(call));
                }
                let tail = (self.shape == Shape::TryUnit).then(|| {
                    let unit = self.e(self.p.types.unit, ExprKind::Tuple(Vec::new()));
                    self.ok(unit)
                });
                self.block(stmts, tail)
            }
            Shape::All => {
                let mut acc = self.e(bool_ty, ExprKind::Lit(Lit::Bool(true)));
                for (_, call) in calls.into_iter().rev() {
                    acc = self.e(
                        bool_ty,
                        ExprKind::Binary(
                            wrela_syntax::ast::BinOp::And,
                            Box::new(call),
                            Box::new(acc),
                        ),
                    );
                }
                acc
            }
            Shape::Order => {
                let mut acc = self.ordering(1);
                for (_, call) in calls.into_iter().rev() {
                    let ty = call.ty;
                    let o = self.local("order", ty, LocalKind::Owned { mutable: false });
                    let bind = Stmt {
                        kind: StmtKind::Bind {
                            pat: Pat { ty, kind: PatKind::Bind(o), span: self.span },
                            init: call,
                            else_: None,
                        },
                        span: self.span,
                    };
                    let decided = self.e(
                        bool_ty,
                        ExprKind::Binary(
                            wrela_syntax::ast::BinOp::Ne,
                            Box::new(self.var(o)),
                            Box::new(self.ordering(1)),
                        ),
                    );
                    let then = Block {
                        stmts: Vec::new(),
                        tail: Some(Box::new(self.var(o))),
                        ty,
                        span: self.span,
                    };
                    let choice = self.e(
                        ty,
                        ExprKind::If { cond: Box::new(decided), then, else_: Some(Box::new(acc)) },
                    );
                    acc = self.block(vec![bind], Some(choice));
                }
                acc
            }
            Shape::Build | Shape::TryBuild => {
                let mut vals = Vec::new();
                for (hk, call) in calls {
                    let call = if self.shape == Shape::TryBuild { self.try_(call) } else { call };
                    vals.push(match hk {
                        Some(h) => {
                            let s = self.stmt(h);
                            self.block(vec![s], Some(call))
                        }
                        None => call,
                    });
                }
                let n = vals.len() as u32;
                let kind = match &self.of {
                    Of::Tuple(_) => ExprKind::Tuple(vals),
                    _ => {
                        let (adt, args) = self.adt();
                        ExprKind::Adt {
                            adt,
                            args,
                            variant,
                            fields: vals,
                            order: (0..n).collect(),
                            base: None,
                        }
                    }
                };
                let built = self.e(self.self_ty, kind);
                if self.shape == Shape::TryBuild { self.ok(built) } else { built }
            }
        }
    }

    /// An array's derivation: a loop over its elements, from the first to its length `n` (a
    /// `const` parameter), in order. A method that builds one starts from the first element's
    /// result, copied to every element (`implements_fieldwise` wants a `Copy` element then), and
    /// builds an empty one zeroed, calling nothing.
    fn array_body(&mut self, e: TyId, n: TyId) -> Expr {
        let (u, bool_ty, unit, never) =
            (self.p.types.u32, self.p.types.bool, self.p.types.unit, self.p.types.never);
        let len = match self.p.types.kind(n) {
            TyKind::Param(g) => self.e(u, ExprKind::ConstParam(*g)),
            TyKind::ConstU32(k) => self.u32_lit(*k),
            _ => self.e(u, ExprKind::Error),
        };
        let i = self.local("i", u, LocalKind::Owned { mutable: false });
        // The method's call on element `at` of each `Self` parameter.
        let call_at = |b: &mut Self, at: &Expr| -> Expr {
            let parts = b
                .self_params
                .clone()
                .into_iter()
                .map(|k| {
                    let base = b.var(b.params[k]);
                    b.e(e, ExprKind::Index(Box::new(base), Box::new(at.clone())))
                })
                .collect();
            b.field_call(e, parts)
        };
        let block =
            |b: &Self, stmts: Vec<Stmt>| Block { stmts, tail: None, ty: unit, span: b.span };
        let each = |b: &Self, from: Expr, body: Vec<Stmt>| Stmt {
            kind: StmtKind::ForRange {
                var: i,
                start: from,
                end: len.clone(),
                inclusive: false,
                body: block(b, body),
            },
            span: b.span,
        };
        // `if cond { return value }`.
        let return_if = |b: &Self, cond: Expr, value: Expr| {
            let ret = b.e(never, ExprKind::Return(Some(Box::new(value))));
            let then = block(b, vec![b.stmt(ret)]);
            b.stmt(b.e(unit, ExprKind::If { cond: Box::new(cond), then, else_: None }))
        };
        let at_i = self.var(i);
        match self.shape {
            Shape::Unit | Shape::TryUnit => {
                let call = call_at(self, &at_i);
                let call = if self.shape == Shape::TryUnit { self.try_(call) } else { call };
                let l = each(self, self.u32_lit(0), vec![self.stmt(call)]);
                let tail = (self.shape == Shape::TryUnit).then(|| {
                    let unit = self.e(unit, ExprKind::Tuple(Vec::new()));
                    self.ok(unit)
                });
                self.block(vec![l], tail)
            }
            Shape::All => {
                let call = call_at(self, &at_i);
                let not =
                    self.e(bool_ty, ExprKind::Unary(wrela_syntax::ast::UnOp::Not, Box::new(call)));
                let no = self.e(bool_ty, ExprKind::Lit(Lit::Bool(false)));
                let check = return_if(self, not, no);
                let l = each(self, self.u32_lit(0), vec![check]);
                let yes = self.e(bool_ty, ExprKind::Lit(Lit::Bool(true)));
                self.block(vec![l], Some(yes))
            }
            Shape::Order => {
                let call = call_at(self, &at_i);
                let ty = call.ty;
                let o = self.local("order", ty, LocalKind::Owned { mutable: false });
                let bind = Stmt {
                    kind: StmtKind::Bind {
                        pat: Pat { ty, kind: PatKind::Bind(o), span: self.span },
                        init: call,
                        else_: None,
                    },
                    span: self.span,
                };
                let decided = self.e(
                    bool_ty,
                    ExprKind::Binary(
                        wrela_syntax::ast::BinOp::Ne,
                        Box::new(self.var(o)),
                        Box::new(self.ordering(1)),
                    ),
                );
                let check = return_if(self, decided, self.var(o));
                let l = each(self, self.u32_lit(0), vec![bind, check]);
                let equal = self.ordering(1);
                self.block(vec![l], Some(equal))
            }
            Shape::Build | Shape::TryBuild => {
                let tries = self.shape == Shape::TryBuild;
                // Empty: zeroed, with no call.
                let empty = self.e(
                    bool_ty,
                    ExprKind::Binary(
                        wrela_syntax::ast::BinOp::Eq,
                        Box::new(len.clone()),
                        Box::new(self.u32_lit(0)),
                    ),
                );
                let zeroed = match self.p.lang_fn(Lang::MemZeroed) {
                    Some(z) => self.e(
                        self.self_ty,
                        ExprKind::Call(Call {
                            callee: Callee::Fn { func: z, args: vec![self.self_ty] },
                            args: Vec::new(),
                            modes: Vec::new(),
                            receiver: false,
                            order: Vec::new(),
                            ret_mode: RetMode::Owned,
                        }),
                    ),
                    None => self.e(self.self_ty, ExprKind::Error),
                };
                let zeroed = if tries { self.ok(zeroed) } else { zeroed };
                let early = return_if(self, empty, zeroed);
                let first = call_at(self, &self.u32_lit(0));
                let first = if tries { self.try_(first) } else { first };
                let out = self.local("out", self.self_ty, LocalKind::Owned { mutable: true });
                let filled = self.e(self.self_ty, ExprKind::ArrayRepeat(Box::new(first), 0));
                let bind = Stmt {
                    kind: StmtKind::Bind {
                        pat: Pat { ty: self.self_ty, kind: PatKind::Bind(out), span: self.span },
                        init: filled,
                        else_: None,
                    },
                    span: self.span,
                };
                let next = call_at(self, &at_i);
                let next = if tries { self.try_(next) } else { next };
                let place =
                    self.e(e, ExprKind::Index(Box::new(self.var(out)), Box::new(self.var(i))));
                let assign = Stmt {
                    kind: StmtKind::Assign { place, op: None, value: next },
                    span: self.span,
                };
                let l = each(self, self.u32_lit(1), vec![assign]);
                let built = self.var(out);
                let tail = if tries { self.ok(built) } else { built };
                self.block(vec![early, bind, l], Some(tail))
            }
        }
    }

    fn enum_body(&mut self) -> Option<Expr> {
        let (a, args) = self.adt();
        let adt = self.p.adt(a);
        let variants: Vec<(String, Vec<TyId>, Vec<String>)> = (0..adt.variants().len() as u32)
            .map(|v| {
                let fields = self.p.fields_of(a, &args, Some(v));
                let names = self.p.adt_fields(a, Some(v)).iter().map(|f| f.name.clone()).collect();
                (adt.variants()[v as usize].name.clone(), fields, names)
            })
            .collect();
        let has_self = self.p.func(self.method).has_self();
        if !has_self {
            return self.enum_build(&variants);
        }
        let receiver_mode = self.p.func(self.method).params[0].mode;
        if receiver_mode == Mode::Take {
            self.cant(format!(
                "`{}` takes `self`, and it can't be moved out of an enum's variant field by field",
                self.p.func(self.method).name
            ));
            return None;
        }
        let variant_hook = hook(self.p, self.trait_, self.method, "_variant");
        // One arm per variant of the receiver; inside, the other `Self` values matched to the
        // same variant.
        let mut arms = Vec::new();
        for (v, (vname, fields, names)) in variants.iter().enumerate() {
            let v = v as u32;
            let mut binds: Vec<Vec<LocalId>> = Vec::new();
            for k in self.self_params.clone() {
                let mutable = self.p.func(self.method).params[k].mode == Mode::Mut;
                let ls = fields
                    .iter()
                    .zip(names)
                    .map(|(&ft, n)| self.local(n, ft, LocalKind::Projection { mutable }))
                    .collect();
                binds.push(ls);
            }
            let parts: Vec<Vec<Expr>> = (0..fields.len())
                .map(|i| binds.iter().map(|ls| self.var(ls[i])).collect())
                .collect();
            let mut inner = self.combine(Some(v), fields, Some(names), parts);
            if let Some(h) = variant_hook {
                let (idx, name) = (self.u32_lit(v), self.text(vname));
                let call = self.hook_call(h, vec![idx, name]);
                let s = self.stmt(call);
                inner = self.block(vec![s], Some(inner));
            }
            // The other `Self` values, innermost last.
            for j in (1..self.self_params.len()).rev() {
                let k = self.self_params[j];
                let pat = self.variant_pat(v, fields, &binds[j]);
                let mismatch = self.mismatch()?;
                let mutable = self.p.func(self.method).params[k].mode == Mode::Mut;
                let scrutinee = self.var(self.params[k]);
                let wild = Pat { ty: self.self_ty, kind: PatKind::Wild, span: self.span };
                let ty = inner.ty;
                inner = self.e(
                    ty,
                    ExprKind::Match {
                        scrutinee: Box::new(scrutinee),
                        arms: vec![
                            Arm { pat, guard: None, body: inner, span: self.span },
                            Arm { pat: wild, guard: None, body: mismatch, span: self.span },
                        ],
                        mutable,
                    },
                );
            }
            let pat = self.variant_pat(v, fields, &binds[0]);
            arms.push(Arm { pat, guard: None, body: inner, span: self.span });
        }
        let k = self.self_params[0];
        let mutable = receiver_mode == Mode::Mut;
        let ret = self.ret;
        Some(self.e(
            ret,
            ExprKind::Match { scrutinee: Box::new(self.var(self.params[k])), arms, mutable },
        ))
    }

    fn variant_pat(&self, v: u32, fields: &[TyId], binds: &[LocalId]) -> Pat {
        let fs = fields
            .iter()
            .zip(binds)
            .enumerate()
            .map(|(i, (&ft, &l))| {
                (i as u32, Pat { ty: ft, kind: PatKind::Bind(l), span: self.span })
            })
            .collect();
        let (adt, args) = self.adt();
        Pat {
            ty: self.self_ty,
            kind: PatKind::Adt { adt, args, variant: Some(v), fields: fs },
            span: self.span,
        }
    }

    /// The value when two `Self` values hold different variants.
    fn mismatch(&mut self) -> Option<Expr> {
        match self.shape {
            Shape::All => Some(self.e(self.p.types.bool, ExprKind::Lit(Lit::Bool(false)))),
            Shape::Order => {
                // The variants in the order they're declared: their indices compared.
                let ord = self.p.lang_trait(Lang::Ord)?;
                let cmp = crate::traits::trait_method(self.p, ord, "cmp")?;
                let u = self.p.types.u32;
                let a = self.self_params[0];
                let b = self.self_params[1];
                let da = self.e(u, ExprKind::Discriminant(Box::new(self.var(self.params[a]))));
                let db = self.e(u, ExprKind::Discriminant(Box::new(self.var(self.params[b]))));
                let def = self.p.func(cmp);
                Some(self.e(
                    def.ret,
                    ExprKind::Call(Call {
                        callee: Callee::TraitMethod {
                            method: cmp,
                            self_ty: u,
                            trait_args: Vec::new(),
                            method_args: Vec::new(),
                        },
                        args: vec![da, db],
                        modes: vec![Mode::Borrow, Mode::Borrow],
                        receiver: true,
                        order: vec![0, 1],
                        ret_mode: RetMode::Owned,
                    }),
                ))
            }
            _ => {
                // The trait's default body.
                let def = self.p.func(self.method);
                if def.body.is_none() {
                    self.cant(format!(
                        "`{}` takes two `Self` values, which may hold different variants, and the trait gives no default for that",
                        def.name
                    ));
                    return None;
                }
                let gargs = self.args_for(self.method, self.self_ty);
                let args: Vec<Expr> = def
                    .params
                    .iter()
                    .enumerate()
                    .map(|(k, ps)| self.arg(self.var(self.params[k]), ps.mode))
                    .collect();
                let n = args.len();
                Some(self.e(
                    self.ret,
                    ExprKind::Call(Call {
                        callee: Callee::Fn { func: self.method, args: gargs },
                        args,
                        modes: def.params.iter().map(|ps| ps.mode).collect(),
                        receiver: true,
                        order: (0..n).collect(),
                        ret_mode: RetMode::Owned,
                    }),
                ))
            }
        }
    }

    /// An enum built without `self`: the trait's variant hook chooses the variant by index.
    fn enum_build(&mut self, variants: &[(String, Vec<TyId>, Vec<String>)]) -> Option<Expr> {
        let Some(h) = hook(self.p, self.trait_, self.method, "_variant") else {
            self.cant(format!(
                "`{}` builds an enum, and the trait has no `{}_variant` hook to choose the variant",
                self.p.func(self.method).name,
                self.p.func(self.method).name
            ));
            return None;
        };
        let text_ty = self.text("").ty;
        let names: Vec<Expr> = variants.iter().map(|(n, ..)| self.text(n)).collect();
        let arr_ty = self.p.types.array(text_ty, names.len() as u32);
        let arr = self.e(arr_ty, ExprKind::Array(names));
        let index = self.hook_call(h, vec![arr]);
        let u = self.p.types.u32;
        let k = self.local("variant", u, LocalKind::Owned { mutable: false });
        let bind = Stmt {
            kind: StmtKind::Bind {
                pat: Pat { ty: u, kind: PatKind::Bind(k), span: self.span },
                init: index,
                else_: None,
            },
            span: self.span,
        };
        // `if k == 0 { .. } else if k == 1 { .. } else { panic }`.
        let msg = self.text("the variant index is past the enum's variants");
        let never = self.p.types.never;
        let panic = self.e(
            never,
            ExprKind::Call(Call {
                callee: Callee::Builtin(crate::builtins::BuiltinFn::Panic),
                args: vec![msg],
                modes: vec![Mode::Borrow],
                receiver: false,
                order: vec![0],
                ret_mode: RetMode::Owned,
            }),
        );
        let mut acc = panic;
        for (v, (_, fields, names)) in variants.iter().enumerate().rev() {
            let parts = vec![Vec::new(); fields.len()];
            let built = self.combine(Some(v as u32), fields, Some(names), parts);
            let bool_ty = self.p.types.bool;
            let test = self.e(
                bool_ty,
                ExprKind::Binary(
                    wrela_syntax::ast::BinOp::Eq,
                    Box::new(self.var(k)),
                    Box::new(self.u32_lit(v as u32)),
                ),
            );
            let ty = built.ty;
            let then =
                Block { stmts: Vec::new(), tail: Some(Box::new(built)), ty, span: self.span };
            acc =
                self.e(ty, ExprKind::If { cond: Box::new(test), then, else_: Some(Box::new(acc)) });
        }
        Some(self.block(vec![bind], Some(acc)))
    }

    /// E0417: this type can't derive the method.
    fn cant(&mut self, why: String) {
        let tr = &self.p.trait_(self.trait_).name;
        let ty = &self.p.display_ty(self.self_ty);
        self.diags.push(
            Diagnostic::new(codes::E0417, self.span, format!("`{ty}` can't derive `{tr}`: {why}"))
                .with_help(format!("write `impl {tr} for {ty}` by hand instead of declaring it")),
        );
    }
}
