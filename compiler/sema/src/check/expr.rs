//! Expressions.

use super::Checker;
use crate::defs::*;
use crate::resolve::{self, PathLookup};
use crate::thir::*;
use crate::ty::*;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_syntax::ast::{self, BinOp, UnOp};

/// What a path in an expression names.
pub(crate) enum ValueRes {
    Local(LocalId),
    Item(Res),
    /// `Type::name`: an associated function or variant of a type.
    TypeRelative(TyId, ast::Ident),
    /// `Trait::method`.
    TraitRelative(TraitId, ast::Ident),
}

impl<'p> Checker<'p> {
    pub(crate) fn error_expr(&self, span: Span) -> Expr {
        Expr { ty: self.p.types.error, kind: ExprKind::Error, span }
    }

    pub(crate) fn check_expr(&mut self, e: &ast::Expr, expected: Option<TyId>) -> Expr {
        let out = self.check_expr_kind(e, expected);
        // Where types grow: each is checked against the bound before it can grow further.
        let grows = matches!(
            e.kind,
            ast::ExprKind::Tuple(_)
                | ast::ExprKind::Array(_)
                | ast::ExprKind::ArrayRepeat { .. }
                | ast::ExprKind::StructLit { .. }
                | ast::ExprKind::Call { .. }
                | ast::ExprKind::MethodCall { .. }
        );
        if grows && self.p.types.size(self.infer.resolve(&self.p.types, out.ty)) > MAX_TYPE_SIZE {
            if std::mem::replace(&mut self.too_large, true) {
                return self.error_expr(out.span);
            }
            self.err(
                Diagnostic::new(
                    codes::E0329,
                    out.span,
                    format!(
                        "this value's type is too large: it has more than {MAX_TYPE_SIZE} parts"
                    ),
                )
                .with_note(
                    "a type that doubles with each step, like `(a, a)`, grows past any machine",
                ),
            );
            return self.error_expr(out.span);
        }
        out
    }

    fn check_expr_kind(&mut self, e: &ast::Expr, expected: Option<TyId>) -> Expr {
        let span = e.span;
        match &e.kind {
            ast::ExprKind::Lit(lit) => self.check_lit(lit, expected),
            ast::ExprKind::Path(path) => self.check_path_expr(path, expected, span),
            ast::ExprKind::Paren(inner) => self.check_expr(inner, expected),
            ast::ExprKind::Unary(op, inner) => self.check_unary(*op, inner, expected, span),
            ast::ExprKind::Binary(op, a, b) => self.check_binary(*op, a, b, expected, span),
            ast::ExprKind::Take(inner) => self.check_take(inner, expected, span),
            ast::ExprKind::MutArg(inner) => {
                let i = self.check_expr(inner, expected);
                if !i.is_place() && !matches!(i.kind, ExprKind::Error) {
                    self.err(
                        Diagnostic::new(
                            codes::E0511,
                            i.span,
                            "`mut` marks a place passed mutably, and this isn't a place",
                        )
                        .with_fix(
                            "remove `mut`",
                            Span::new(span.file, span.start, i.span.start),
                            "",
                        ),
                    );
                }
                self.mark_written(&i);
                Expr { ty: i.ty, span, kind: ExprKind::MutArg(Box::new(i)) }
            }
            ast::ExprKind::Call { callee, args, .. } => {
                self.check_call(callee, args, expected, span)
            }
            ast::ExprKind::MethodCall { receiver, name, generics, args, .. } => {
                self.check_method_call(receiver, name, generics.as_deref(), args, expected, span)
            }
            ast::ExprKind::Field { base, name } => self.check_field(base, name, span),
            ast::ExprKind::Index { base, index } => self.check_index(base, index, span),
            ast::ExprKind::StructLit { path, fields, base, .. } => {
                self.check_struct_lit(path, fields, base.as_deref(), expected, span)
            }
            ast::ExprKind::Tuple(items) => {
                let exp: Vec<Option<TyId>> = match expected.map(|t| self.kind(t)) {
                    Some(TyKind::Tuple(ts)) if ts.len() == items.len() => {
                        ts.iter().copied().map(Some).collect()
                    }
                    _ => vec![None; items.len()],
                };
                let xs: Vec<Expr> =
                    items.iter().zip(exp).map(|(x, t)| self.check_expr(x, t)).collect();
                let ty = self.p.types.tuple(xs.iter().map(|x| x.ty).collect());
                Expr { ty, span, kind: ExprKind::Tuple(xs) }
            }
            ast::ExprKind::Array(items) => {
                let elem_exp = match expected.map(|t| self.kind(t)) {
                    Some(TyKind::Array(t, _)) | Some(TyKind::Slice(t)) => Some(*t),
                    _ => None,
                };
                let elem = elem_exp.unwrap_or_else(|| self.new_var(VarKind::General, span));
                let xs: Vec<Expr> = items.iter().map(|x| self.check_expect(x, elem)).collect();
                if items.is_empty() {
                    self.err(
                        Diagnostic::new(
                            codes::E0306,
                            span,
                            "an empty array has no element type to infer",
                        )
                        .with_help("write `[value; 0]`"),
                    );
                }
                let ty = self.p.types.array(elem, xs.len() as u32);
                Expr { ty, span, kind: ExprKind::Array(xs) }
            }
            ast::ExprKind::ArrayRepeat { value, count } => {
                let elem_exp = match expected.map(|t| self.kind(t)) {
                    Some(TyKind::Array(t, _)) => Some(*t),
                    _ => None,
                };
                let v = self.check_expr(value, elem_exp);
                let n = match self.const_u32(count) {
                    Some(n) => n,
                    None => {
                        let local = match &count.kind {
                            ast::ExprKind::Path(p) if p.is_single() => {
                                self.env.lookup(&p.segments[0].ident.name).is_some()
                            }
                            _ => false,
                        };
                        // A variable isn't a constant; anything else says why it isn't a length.
                        self.err(if local {
                            Diagnostic::new(codes::E0325, count.span, "an array length is an integer literal or a constant holding one, not a variable")
                        } else {
                            resolve::length_error(self.p, self.scope.module, count)
                        });
                        0
                    }
                };
                let vt = self.infer.resolve(&self.p.types, v.ty);
                if self.p.types.has_vars(vt) {
                    // Decided once inference is done.
                    if let Some(copy) = self.p.lang_trait(Lang::Copy) {
                        let r = crate::defs::TraitRef { trait_: copy, args: Vec::new() };
                        self.obligation(vt, r, v.span, "`[x; n]`, which copies `x`,".into());
                    }
                } else if !crate::traits::implements_builtin(self.p, vt, Lang::Copy) {
                    let shown = self.display(v.ty);
                    self.err(Diagnostic::new(
                        codes::E0400,
                        v.span,
                        format!("`[x; n]` copies `x`, so it must be `Copy`, and `{shown}` isn't"),
                    ));
                }
                let ty = self.p.types.array(v.ty, n);
                Expr { ty, span, kind: ExprKind::ArrayRepeat(Box::new(v), n) }
            }
            ast::ExprKind::Block(b) => {
                let blk = self.check_block(b, expected);
                Expr { ty: blk.ty, span, kind: ExprKind::Block(blk) }
            }
            ast::ExprKind::If { cond, then, else_ } => {
                self.check_if(cond, then, else_.as_deref(), expected, span)
            }
            ast::ExprKind::Match { scrutinee, arms } => {
                self.check_match(scrutinee, arms, expected, span)
            }
            ast::ExprKind::Closure { params, ret, body } => {
                self.check_closure(params, ret.as_ref(), body, expected, span)
            }
            ast::ExprKind::Return(value) => self.check_return(value.as_deref(), span),
            ast::ExprKind::Break | ast::ExprKind::Continue => {
                let (what, kind) = match e.kind {
                    ast::ExprKind::Break => ("break", ExprKind::Break),
                    _ => ("continue", ExprKind::Continue),
                };
                if !self.in_loop() {
                    self.err(Diagnostic::new(
                        codes::E0315,
                        span,
                        format!("`{what}` outside a loop"),
                    ));
                }
                Expr { ty: self.p.types.never, span, kind }
            }
            ast::ExprKind::Error => {
                self.saw_syntax_error = true;
                self.error_expr(span)
            }
        }
    }

    pub(crate) fn const_u32(&self, e: &ast::Expr) -> Option<u32> {
        resolve::const_u32(self.p, self.scope.module, e)
    }

    /// What `Self` (a struct) or `Self::Variant` names in an impl of an ADT, for patterns and
    /// struct literals. `None` when the path doesn't start with `Self`, or names nothing.
    pub(crate) fn self_item(&self, segs: &[ast::PathSegment]) -> Option<Res> {
        if segs.first()?.ident.name != "Self" {
            return None;
        }
        let TyKind::Adt(a, _) = self.kind(self.scope.self_ty?) else { return None };
        match segs {
            [_] => Some(Res::Adt(*a)),
            [_, v] => {
                let i = self.p.adt(*a).variants().iter().position(|x| x.name == v.ident.name)?;
                Some(Res::Variant(*a, i as u32))
            }
            _ => None,
        }
    }

    fn check_lit(&mut self, lit: &ast::Lit, expected: Option<TyId>) -> Expr {
        let span = lit.span;
        match lit.kind {
            ast::LitKind::Bool(b) => {
                Expr { ty: self.p.types.bool, span, kind: ExprKind::Lit(Lit::Bool(b)) }
            }
            ast::LitKind::Int(value) => {
                let v = match value {
                    ast::IntValue::Ok(v) => v,
                    // A malformed number (`0x`, `1e`) has its error from the lexer (E0004).
                    ast::IntValue::Malformed => return self.error_expr(span),
                    ast::IntValue::TooLarge => {
                        self.err(Diagnostic::new(
                            codes::E0006,
                            span,
                            "this integer is too large for any integer type",
                        ));
                        return self.error_expr(span);
                    }
                };
                let ty = match expected {
                    Some(t) if matches!(self.kind(t), TyKind::Int(_) | TyKind::Float(_)) => {
                        self.shallow(t)
                    }
                    _ => self.new_var(VarKind::Int, span),
                };
                self.int_literals.push((ty, v, false, span));
                Expr { ty, span, kind: ExprKind::Lit(Lit::Int(i128::from(v))) }
            }
            ast::LitKind::Float(v) => {
                let ty = match expected {
                    Some(t) if matches!(self.kind(t), TyKind::Float(_)) => self.shallow(t),
                    _ => self.new_var(VarKind::Float, span),
                };
                let f = wrela_syntax::lexer::float_value_f32(&lit.text);
                self.float_literals.push((ty, v, f, span));
                Expr { ty, span, kind: ExprKind::Lit(Lit::Float(v, f)) }
            }
            ast::LitKind::Str => {
                self.err(Diagnostic::new(codes::E0901, span, "strings are tier 1"));
                self.error_expr(span)
            }
            ast::LitKind::Suffixed => {
                // The number as written, exponent and radix included (`1e-3m` is `1e-3`).
                let (digits, _) = wrela_syntax::lexer::split_suffix(&lit.text);
                self.err(
                    Diagnostic::new(
                        codes::E0900,
                        span,
                        format!("`{}` has a unit, and units are tier 1", lit.text),
                    )
                    .with_help("in tier 0, write the number in the unit your code works in")
                    .with_fix("drop the unit", span, digits),
                );
                self.error_expr(span)
            }
        }
    }

    // ---- paths -----------------------------------------------------------------------------

    pub(crate) fn resolve_value_path(&mut self, path: &ast::Path) -> Option<ValueRes> {
        let segs = &path.segments;
        let first = &segs[0].ident;
        if segs.len() == 1 {
            if (first.name == "self" || segs[0].generics.is_none())
                && let Some(l) = self.lookup_local(&first.name)
            {
                return Some(ValueRes::Local(l));
            }
            if first.name == "self" {
                self.err(Diagnostic::new(
                    codes::E0200,
                    first.span,
                    "`self` is only in scope in a method that takes `self`",
                ));
                return None;
            }
            if let Some(r) = resolve::lookup_name(self.p, self.scope.module, &first.name) {
                return Some(ValueRes::Item(r));
            }
            if self.scope.param(&first.name).is_some() {
                self.err(Diagnostic::new(
                    codes::E0212,
                    first.span,
                    format!("`{}` is a type parameter, not a value", first.name),
                ));
                return None;
            }
            if self.saw_syntax_error || resolve::is_broken(self.p, self.scope.module, &first.name) {
                return None;
            }
            let mut names: Vec<String> = self.env.names().map(String::from).collect();
            names.extend(resolve::visible_names(self.p, self.scope.module));
            let mut d = Diagnostic::new(
                codes::E0200,
                first.span,
                format!("there's no `{}` in scope", first.name),
            );
            if let Some(s) = resolve::closest(&first.name, names.iter().map(String::as_str)) {
                d = d.with_fix(format!("did you mean `{s}`?"), first.span, s);
            }
            self.err(d);
            return None;
        }
        // `Self::x`, `T::x`.
        let param = self.scope.param(&first.name);
        if first.name == "Self" || param.is_some() {
            if segs.len() != 2 {
                self.err(Diagnostic::new(
                    codes::E0200,
                    path.span,
                    "a type-relative path is `Type::name`",
                ));
                return None;
            }
            let base = if first.name == "Self" {
                match self.scope.self_ty {
                    Some(t) => t,
                    None => {
                        self.err(Diagnostic::new(
                            codes::E0200,
                            first.span,
                            "`Self` only exists inside an `impl` or a `trait`",
                        ));
                        return None;
                    }
                }
            } else {
                let Some(p) = param else { unreachable!("checked above") };
                self.p.types.param(p)
            };
            return Some(ValueRes::TypeRelative(base, segs[1].ident.clone()));
        }
        match resolve::resolve_module_path_in(self.p, self.scope.module, segs, true) {
            PathLookup::Found(r) => Some(ValueRes::Item(r)),
            PathLookup::NotYet(..) | PathLookup::Broken => None,
            PathLookup::Error(d) => {
                // Maybe `Type::assoc` or `Trait::method`.
                let prefix = &segs[..segs.len() - 1];
                let last = segs[segs.len() - 1].ident.clone();
                if let PathLookup::Found(r) =
                    resolve::resolve_module_path_in(self.p, self.scope.module, prefix, true)
                {
                    match r {
                        Res::Adt(a) => {
                            let n = self.p.adt(a).generics.len();
                            let args: Vec<TyId> = match &segs[segs.len() - 2].generics {
                                Some(g) => g.iter().map(|t| self.resolve_type(t)).collect(),
                                None => (0..n)
                                    .map(|_| self.new_var(VarKind::General, path.span))
                                    .collect(),
                            };
                            let ty = self.p.types.adt(a, args);
                            return Some(ValueRes::TypeRelative(ty, last));
                        }
                        Res::BuiltinTy(b) => {
                            let ty = b.ty(&self.p.types);
                            return Some(ValueRes::TypeRelative(ty, last));
                        }
                        Res::Trait(t) => return Some(ValueRes::TraitRelative(t, last)),
                        _ => {}
                    }
                }
                self.err(*d);
                None
            }
        }
    }

    fn check_path_expr(&mut self, path: &ast::Path, expected: Option<TyId>, span: Span) -> Expr {
        let Some(r) = self.resolve_value_path(path) else { return self.error_expr(span) };
        match r {
            ValueRes::Local(l) => {
                Expr { ty: self.locals[l.index()].ty, span, kind: ExprKind::Local(l) }
            }
            ValueRes::Item(Res::Const(c)) => {
                Expr { ty: self.const_ty(c), span, kind: ExprKind::Const(c) }
            }
            ValueRes::Item(Res::Variant(a, v)) => {
                let variant = &self.p.adt(a).variants()[v as usize];
                let args = self.adt_args(a, path, expected, span);
                let ty = self.p.types.adt(a, args.clone());
                match variant.shape {
                    VariantShape::Unit => variant_expr(ty, span, a, args, v, Vec::new()),
                    VariantShape::Tuple => {
                        self.err(
                            Diagnostic::new(
                                codes::E0303,
                                span,
                                format!(
                                    "`{}` holds {} value{}",
                                    variant.name,
                                    variant.fields.len(),
                                    wrela_diag::plural(variant.fields.len())
                                ),
                            )
                            .with_help(format!("write `{}(...)`", variant.name)),
                        );
                        self.error_expr(span)
                    }
                    VariantShape::Struct => {
                        self.err(
                            Diagnostic::new(
                                codes::E0303,
                                span,
                                format!("`{}` has fields", variant.name),
                            )
                            .with_help(format!("write `{} {{ ... }}`", variant.name)),
                        );
                        self.error_expr(span)
                    }
                }
            }
            ValueRes::Item(Res::Fn(f)) => {
                let def = self.p.func(f);
                // Only a call reaches what the compiler does for these, not the function's body.
                if def.attrs.entry.is_some() {
                    self.err(
                        Diagnostic::new(
                            codes::E0606,
                            span,
                            format!("`{}` is a GPU entry point, not a value", def.name),
                        )
                        .with_note("entry points run on the GPU and can't be called directly"),
                    );
                    return self.error_expr(span);
                }
                if def.attrs.intrinsic {
                    self.err(
                        Diagnostic::new(
                            codes::E0212,
                            span,
                            format!(
                                "`{}` is built into the compiler, so it can only be called",
                                def.name
                            ),
                        )
                        .with_help(format!("wrap it in a closure: `|..| {}(..)`", def.name)),
                    );
                    return self.error_expr(span);
                }
                let args = self.fresh_fn_args(f, path, span);
                let ty = self.p.types.intern(TyKind::FnDef(f, args.clone()));
                Expr { ty, span, kind: ExprKind::FnRef(f, args) }
            }
            ValueRes::Item(Res::Adt(a)) => {
                let name = &self.p.adt(a).name;
                let shape = if self.p.adt(a).is_enum() {
                    format!("one of its variants, like `{name}::...`")
                } else {
                    format!("a struct literal: `{name} {{ ... }}`")
                };
                self.err(
                    Diagnostic::new(codes::E0212, span, format!("`{name}` is a type, not a value"))
                        .with_help(format!("make a value with {shape}")),
                );
                self.error_expr(span)
            }
            ValueRes::Item(Res::Trait(t)) => {
                let name = &self.p.trait_(t).name;
                self.err(Diagnostic::new(
                    codes::E0212,
                    span,
                    format!("`{name}` is a trait, not a value"),
                ));
                self.error_expr(span)
            }
            ValueRes::Item(Res::Module(m)) => {
                let name = self.p.module(m).name();
                self.err(Diagnostic::new(
                    codes::E0212,
                    span,
                    format!("`{name}` is a module, not a value"),
                ));
                self.error_expr(span)
            }
            ValueRes::Item(Res::BuiltinTy(_)) => {
                let name = path.last().name.clone();
                self.err(
                    Diagnostic::new(codes::E0212, span, format!("`{name}` is a type, not a value"))
                        .with_help(format!("call it to make one: `{name}(...)`")),
                );
                self.error_expr(span)
            }
            ValueRes::Item(Res::BuiltinFn(b)) => {
                self.err(
                    Diagnostic::new(
                        codes::E0212,
                        span,
                        format!("the built-in `{}` can only be called", b.name()),
                    )
                    .with_help(format!("wrap it in a closure: `|x| {}(x)`", b.name())),
                );
                self.error_expr(span)
            }
            ValueRes::TypeRelative(ty, name) => {
                if let &TyKind::Adt(a, ref args) = self.kind(ty)
                    && let Some(v) =
                        self.p.adt(a).variants().iter().position(|v| v.name == name.name)
                    && self.p.adt(a).variants()[v].shape == VariantShape::Unit
                {
                    return variant_expr(ty, span, a, args.clone(), v as u32, Vec::new());
                }
                // An enum without that variant or method: say so, rather than that it must be
                // called.
                if let &TyKind::Adt(a, _) = self.kind(ty)
                    && self.p.adt(a).is_enum()
                    && !self.has_associated_fn(a, &name.name)
                {
                    let adt = self.p.adt(a);
                    let mut d = Diagnostic::new(
                        codes::E0211,
                        name.span,
                        format!("`{}` has no variant `{}`", adt.name, name.name),
                    );
                    let names = adt.variants().iter().map(|v| v.name.as_str());
                    if let Some(s) = resolve::closest(&name.name, names) {
                        d = d.with_fix(format!("did you mean `{s}`?"), name.span, s);
                    }
                    self.err(d);
                    return self.error_expr(span);
                }
                self.err(Diagnostic::new(
                    codes::E0212,
                    span,
                    format!("`{}` can only be called here", name.name),
                ));
                self.error_expr(span)
            }
            ValueRes::TraitRelative(_, name) => {
                self.err(Diagnostic::new(
                    codes::E0212,
                    span,
                    format!("`{}` can only be called here", name.name),
                ));
                self.error_expr(span)
            }
        }
    }

    /// Whether a type has an inherent associated function or method of this name.
    fn has_associated_fn(&self, a: AdtId, name: &str) -> bool {
        self.p.inherent_impls.get(&a).is_some_and(|impls| {
            impls
                .iter()
                .any(|&i| self.p.impl_(i).methods.iter().any(|&f| self.p.func(f).name == name))
        })
    }

    /// An ADT's generic arguments for a path: explicit (`Option::<u32>::None`), or fresh
    /// variables unified with the expected type.
    pub(crate) fn adt_args(
        &mut self,
        a: AdtId,
        path: &ast::Path,
        expected: Option<TyId>,
        span: Span,
    ) -> Vec<TyId> {
        let n = self.p.adt(a).generics.len();
        let explicit = path.segments.iter().rev().find_map(|s| s.generics.as_deref());
        let args: Vec<TyId> = match explicit {
            Some(g) if g.len() == n => g.iter().map(|t| self.resolve_type(t)).collect(),
            Some(g) => {
                let name = &self.p.adt(a).name;
                self.err(resolve::wrong_generic_count(span, name, n, g.len()));
                (0..n).map(|_| self.p.types.error).collect()
            }
            None => (0..n).map(|_| self.new_var(VarKind::General, span)).collect(),
        };
        if let Some(e) = expected
            && let &TyKind::Adt(b, _) = self.kind(e)
            && b == a
        {
            let ty = self.p.types.adt(a, args.clone());
            let _ = self.infer.unify(&self.p.types, ty, e);
        }
        // The arguments must have the generics' bounds (`S { t: 1 }` for `struct S<T: Tr>`).
        let adt = self.p.adt(a);
        let subst = Subst::from_pairs(&adt.generics, &args);
        let name = adt.name.clone();
        self.bound_obligations(&adt.generics, &args, &subst, &vec![span; n], &name);
        args
    }

    /// Fresh variables (or turbofish types) for all of a function's generics.
    pub(crate) fn fresh_fn_args(&mut self, f: FnId, path: &ast::Path, span: Span) -> Vec<TyId> {
        let explicit = path.segments.last().and_then(|s| s.generics.as_deref());
        self.generic_args(f, &self.p.fn_all_generics(f), explicit, span)
    }

    /// Arguments for `params`, which hold `f`'s own generics: the types given with `::<...>`
    /// for its declared ones, in order, and fresh variables for the rest (all of them when no
    /// types are given, or the wrong number).
    pub(crate) fn generic_args(
        &mut self,
        f: FnId,
        params: &[ParamId],
        explicit: Option<&[ast::TypeExpr]>,
        span: Span,
    ) -> Vec<TyId> {
        let p = self.p;
        let mut given: Vec<Option<TyId>> = vec![None; params.len()];
        if let Some(g) = explicit {
            // Implicit parameters (from `x: Trait`) can't be named; only declared ones count.
            let declared: Vec<ParamId> = p
                .func(f)
                .generics
                .iter()
                .copied()
                .filter(|&q| !p.param(q).name.starts_with("impl "))
                .collect();
            if g.len() != declared.len() {
                let name = &p.func(f).name;
                self.err(resolve::wrong_generic_count(span, name, declared.len(), g.len()));
            } else {
                for (q, t) in declared.iter().zip(g) {
                    let ty = self.resolve_type(t);
                    if let Some(i) = params.iter().position(|x| x == q) {
                        given[i] = Some(ty);
                    }
                }
            }
        }
        given
            .into_iter()
            .map(|g| g.unwrap_or_else(|| self.new_var(VarKind::General, span)))
            .collect()
    }

    // ---- operators -------------------------------------------------------------------------

    fn check_unary(
        &mut self,
        op: UnOp,
        inner: &ast::Expr,
        expected: Option<TyId>,
        span: Span,
    ) -> Expr {
        let i = self.check_expr(inner, expected);
        let k = self.kind(i.ty);
        let vk = self.infer.var_kind(&self.p.types, i.ty);
        let ok = match op {
            UnOp::Neg => match k {
                TyKind::Int(it) => it.signed(),
                TyKind::Float(_)
                | TyKind::Vec(_)
                | TyKind::Mat(_)
                | TyKind::Error
                | TyKind::Never => true,
                TyKind::Var(_) => vk != Some(VarKind::General),
                _ => false,
            },
            UnOp::Not => {
                matches!(k, TyKind::Bool | TyKind::Int(_) | TyKind::Error | TyKind::Never)
                    || vk == Some(VarKind::Int)
            }
        };
        if !ok {
            let shown = self.display(i.ty);
            let mut d = Diagnostic::new(
                codes::E0305,
                span,
                format!("`{}` doesn't apply to `{shown}`", if op == UnOp::Neg { "-" } else { "!" }),
            );
            if op == UnOp::Neg && matches!(k, TyKind::Int(_)) {
                d = d
                    .with_note("unsigned integers can't be negative")
                    .with_help("use a signed type such as `i32`");
            }
            self.err(d);
        }
        if op == UnOp::Neg && vk == Some(VarKind::Int) {
            self.negated_ints.push((i.ty, span));
        }
        if op == UnOp::Not {
            self.int_only(i.ty, "!", span);
        }
        if op == UnOp::Neg
            && let ExprKind::Lit(Lit::Int(v)) = i.kind
            && let Some(last) = self.int_literals.last_mut()
            && i128::from(last.1) == v
        {
            last.2 = true; // negated: `-2147483648` fits an i32
        }
        Expr { ty: i.ty, span, kind: ExprKind::Unary(op, Box::new(i)) }
    }

    fn check_binary(
        &mut self,
        op: BinOp,
        a: &ast::Expr,
        b: &ast::Expr,
        expected: Option<TyId>,
        span: Span,
    ) -> Expr {
        let (ea, eb) = match op {
            BinOp::And | BinOp::Or => {
                let t = self.p.types.bool;
                let x = self.check_expr(a, Some(t));
                let y = self.check_expr(b, Some(t));
                self.expect(x.ty, t, x.span);
                self.expect(y.ty, t, y.span);
                return Expr { ty: t, span, kind: ExprKind::Binary(op, Box::new(x), Box::new(y)) };
            }
            BinOp::Pow | BinOp::Shl | BinOp::Shr => {
                let x = self.check_expr(a, expected);
                let hint = self.right_hint(op, x.ty);
                let y = self.check_expr(b, Some(hint));
                (x, y)
            }
            _ => {
                let hint = if op.is_comparison() { None } else { expected };
                let x = self.check_expr(a, hint);
                // A literal on the right takes the left's type: `x * 2` with x: f32.
                let y = self.check_expr(b, Some(x.ty));
                (x, y)
            }
        };
        // Literals on the left take the right's type: `2 * x`.
        let ty = self.binary_result(op, ea.ty, eb.ty, span).unwrap_or(self.p.types.error);
        Expr { ty, span, kind: ExprKind::Binary(op, Box::new(ea), Box::new(eb)) }
    }

    /// The type the right operand of `op` is checked against, when the left is an `a`: a shift
    /// amount is a `u32`, so is an integer's exponent; anything else is `a`'s type.
    pub(crate) fn right_hint(&self, op: BinOp, a: TyId) -> TyId {
        match op {
            BinOp::Shl | BinOp::Shr => self.p.types.u32,
            BinOp::Pow if self.is_intlike(a) => self.p.types.u32,
            _ => a,
        }
    }

    /// An integer literal's type not settled yet, as an operand of an operator that only
    /// applies to integers: it's checked once it's settled, in case it became a float.
    pub(crate) fn int_only(&mut self, t: TyId, op: &'static str, span: Span) {
        if self.infer.var_kind(&self.p.types, t) == Some(VarKind::Int) {
            self.int_ops.push((t, op, span));
        }
    }

    /// The type of `a op b`, unifying literal variables where the operator needs it.
    pub(crate) fn binary_result(
        &mut self,
        op: BinOp,
        a: TyId,
        b: TyId,
        span: Span,
    ) -> Option<TyId> {
        let (ka, kb) = (self.kind(a), self.kind(b));
        if matches!(ka, TyKind::Error) || matches!(kb, TyKind::Error) {
            return Some(self.p.types.error);
        }
        let bool_ty = self.p.types.bool;
        let f32 = self.p.types.f32;
        let fail = |this: &mut Self| {
            let (x, y) = (this.display(a), this.display(b));
            let mut d = Diagnostic::new(
                codes::E0305,
                span,
                format!("`{}` doesn't apply to `{x}` and `{y}`", op.text()),
            );
            let (ka, kb) = (this.kind(a), this.kind(b));
            if matches!(op, BinOp::Shl | BinOp::Shr) && matches!(ka, TyKind::Int(_)) {
                d = d.with_help(
                    "a shift's amount is a `u32`, whatever the shifted type: `x << u32(n)`",
                );
            } else if matches!(
                (&ka, &kb),
                (TyKind::Int(_), TyKind::Float(_)) | (TyKind::Float(_), TyKind::Int(_))
            ) {
                d = d.with_help(
                    "there are no implicit conversions; convert one side, as in `f32(i)`",
                );
            } else if matches!((&ka, &kb), (TyKind::Int(_), TyKind::Int(_))) {
                d = d.with_help(
                    "both sides must have the same integer type; convert one, as in `u32(x)`",
                );
            }
            this.err(d);
            None
        };
        match op {
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                if self.infer.unify(&self.p.types, a, b).is_err() {
                    return fail(self);
                }
                let k = self.kind(a);
                let ordered = matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge);
                let ok = match k {
                    TyKind::Int(_) | TyKind::Float(_) => true,
                    TyKind::Var(_) => self.is_number_var(a),
                    TyKind::Bool | TyKind::Vec(_) => !ordered,
                    &TyKind::Adt(adt, _) => {
                        !ordered
                            && self.p.adt(adt).is_enum()
                            && self.p.adt(adt).variants().iter().all(|v| v.fields.is_empty())
                    }
                    _ => false,
                };
                if !ok {
                    let shown = self.display(a);
                    let mut d = Diagnostic::new(
                        codes::E0305,
                        span,
                        format!("`{}` can't compare `{shown}` values", op.text()),
                    );
                    if matches!(k, TyKind::Adt(..)) {
                        d = d.with_help("use `match` to look inside it");
                    }
                    self.err(d);
                }
                Some(bool_ty)
            }
            BinOp::And | BinOp::Or => Some(bool_ty),
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                if self.infer.unify(&self.p.types, a, b).is_err() {
                    return fail(self);
                }
                if self.is_intlike(a) || matches!(self.kind(a), TyKind::Bool) {
                    self.int_only(a, op.text(), span);
                    Some(a)
                } else {
                    fail(self)
                }
            }
            BinOp::Shl | BinOp::Shr => {
                let u = self.p.types.u32;
                if !self.is_intlike(a) || self.infer.unify(&self.p.types, b, u).is_err() {
                    return fail(self);
                }
                self.int_only(a, op.text(), span);
                Some(a)
            }
            BinOp::Pow if self.is_intlike(a) => {
                let u = self.p.types.u32;
                if self.infer.unify(&self.p.types, b, u).is_err() {
                    self.err(Diagnostic::new(
                        codes::E0305,
                        span,
                        "an integer's exponent must be a `u32`",
                    ));
                }
                self.pows.push((a, b, span));
                Some(a)
            }
            BinOp::Pow => {
                if self.infer.unify(&self.p.types, a, b).is_err() {
                    return fail(self);
                }
                self.pows.push((a, b, span));
                match self.kind(a) {
                    TyKind::Float(_) | TyKind::Vec(_) => Some(a),
                    // A literal is a float literal, still open: `2.0 ** 3.0` may be an `f64`.
                    TyKind::Var(_) if self.is_number_var(a) => {
                        let f = self.new_var(VarKind::Float, span);
                        let _ = self.infer.unify(&self.p.types, a, f);
                        Some(a)
                    }
                    TyKind::Var(_) => {
                        let _ = self.infer.unify(&self.p.types, a, f32);
                        Some(a)
                    }
                    _ => fail(self),
                }
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                // Vector and scalar, matrix and vector.
                match (&ka, &kb) {
                    (TyKind::Vec(_), TyKind::Float(_) | TyKind::Var(_))
                    | (TyKind::Mat(_), TyKind::Float(_) | TyKind::Var(_))
                        if !matches!(kb, TyKind::Var(_)) || self.is_number_var(b) =>
                    {
                        if matches!(kb, TyKind::Var(_)) {
                            let _ = self.infer.unify(&self.p.types, b, f32);
                        }
                        if self.shallow(b) != f32 {
                            return fail(self);
                        }
                        if matches!(ka, TyKind::Mat(_)) && !matches!(op, BinOp::Mul) {
                            return fail(self);
                        }
                        return Some(a);
                    }
                    (TyKind::Float(_) | TyKind::Var(_), TyKind::Vec(_) | TyKind::Mat(_))
                        if !matches!(ka, TyKind::Var(_)) || self.is_number_var(a) =>
                    {
                        if matches!(ka, TyKind::Var(_)) {
                            let _ = self.infer.unify(&self.p.types, a, f32);
                        }
                        if self.shallow(a) != f32
                            || matches!(op, BinOp::Div | BinOp::Rem) && matches!(kb, TyKind::Mat(_))
                        {
                            return fail(self);
                        }
                        if matches!(kb, TyKind::Mat(_)) && !matches!(op, BinOp::Mul) {
                            return fail(self);
                        }
                        return Some(b);
                    }
                    (TyKind::Mat(n), TyKind::Vec(m)) | (TyKind::Vec(m), TyKind::Mat(n))
                        if op == BinOp::Mul =>
                    {
                        if n != m {
                            return fail(self);
                        }
                        return Some(if matches!(ka, TyKind::Mat(_)) { b } else { a });
                    }
                    _ => {}
                }
                if self.infer.unify(&self.p.types, a, b).is_err() {
                    return fail(self);
                }
                match self.kind(a) {
                    TyKind::Int(_) | TyKind::Float(_) | TyKind::Vec(_) => Some(a),
                    TyKind::Mat(_) if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) => Some(a),
                    TyKind::Var(_) if self.is_number_var(a) => Some(a),
                    _ => fail(self),
                }
            }
        }
    }

    fn check_take(&mut self, inner: &ast::Expr, expected: Option<TyId>, span: Span) -> Expr {
        let i = self.check_expr(inner, expected);
        // `take recv.method()`: a method that consumes its receiver, a named place. (One that
        // returns a projection is a place, which `take` moves out of.)
        if let ExprKind::Call(c) = &i.kind
            && c.receiver
            && c.ret_mode == RetMode::Owned
            && c.args.first().is_some_and(|r| r.is_place())
            && c.modes.first() != Some(&Mode::Take)
        {
            self.err(
                Diagnostic::new(
                    codes::E0511,
                    i.span,
                    "`take` marks a call that consumes its receiver (`take self`), and this one doesn't",
                )
                .with_note("the call's result is a temporary, which moves without a marker")
                .with_fix("remove `take`", Span::new(span.file, span.start, i.span.start), ""),
            );
            return Expr { ty: i.ty, span, kind: ExprKind::Take(Box::new(i)) };
        }
        let ok = i.is_place()
            || matches!(&i.kind, ExprKind::Call(c) if c.receiver && c.args.first().is_some_and(|r| r.is_place()))
            || matches!(i.kind, ExprKind::Error);
        if !ok {
            self.err(
                Diagnostic::new(
                    codes::E0511,
                    i.span,
                    "`take` moves out of a named place, and this is a temporary",
                )
                .with_note("temporaries move without a marker")
                .with_fix(
                    "remove `take`",
                    Span::new(span.file, span.start, i.span.start),
                    "",
                ),
            );
        }
        Expr { ty: i.ty, span, kind: ExprKind::Take(Box::new(i)) }
    }

    // ---- fields and indexing ---------------------------------------------------------------

    fn check_field(&mut self, base: &ast::Expr, name: &ast::FieldName, span: Span) -> Expr {
        let b = self.check_expr(base, None);
        // Resolved through: a field `T::Out` normalizes only once `T` is known.
        let bt = self.infer.resolve(&self.p.types, b.ty);
        match (self.kind(bt), name) {
            // Its error is reported.
            (TyKind::Error, _) | (_, ast::FieldName::BadIndex(_)) => self.error_expr(span),
            (&TyKind::Adt(a, ref args), ast::FieldName::Ident(n)) if !self.p.adt(a).is_enum() => {
                let fields = self.p.adt(a).fields();
                match fields.iter().position(|f| f.name == n.name) {
                    Some(i) => {
                        let f = &fields[i];
                        let adt_mod = self.p.adt(a).module;
                        if !f.public && adt_mod != self.scope.module {
                            self.err(
                                Diagnostic::new(
                                    codes::E0210,
                                    n.span,
                                    format!(
                                        "the field `{}` of `{}` is private",
                                        n.name,
                                        self.p.adt(a).name
                                    ),
                                )
                                .with_secondary(f.span, "declared here without `pub`"),
                            );
                        }
                        let ty = self.p.field_ty(a, args, None, i).unwrap_or(self.p.types.error);
                        Expr { ty, span, kind: ExprKind::Field(Box::new(b), i as u32) }
                    }
                    None => {
                        let mut d = Diagnostic::new(
                            codes::E0206,
                            n.span,
                            format!("`{}` has no field `{}`", self.p.adt(a).name, n.name),
                        );
                        if let Some(s) =
                            resolve::closest(&n.name, fields.iter().map(|f| f.name.as_str()))
                        {
                            d = d.with_fix(format!("did you mean `{s}`?"), n.span, s);
                        } else {
                            let names: Vec<String> =
                                fields.iter().map(|f| format!("`{}`", f.name)).collect();
                            if !names.is_empty() {
                                d = d.with_note(format!("its fields are {}", names.join(", ")));
                            }
                        }
                        self.err(d);
                        self.error_expr(span)
                    }
                }
            }
            (TyKind::Tuple(ts), ast::FieldName::Index(i, s)) => match ts.get(*i as usize) {
                Some(&t) => Expr { ty: t, span, kind: ExprKind::Field(Box::new(b), *i) },
                None => {
                    self.err(Diagnostic::new(
                        codes::E0206,
                        *s,
                        format!("this tuple has {} elements, so `.{i}` doesn't exist", ts.len()),
                    ));
                    self.error_expr(span)
                }
            },
            (TyKind::Vec(n), ast::FieldName::Ident(id)) => {
                let comps: Option<Vec<u8>> = id
                    .name
                    .chars()
                    .map(|c| match c {
                        'x' | 'r' => Some(0),
                        'y' | 'g' => Some(1),
                        'z' | 'b' => Some(2),
                        'w' | 'a' => Some(3),
                        _ => None,
                    })
                    .collect();
                let mixed = id.name.chars().any(|c| "xyzw".contains(c))
                    && id.name.chars().any(|c| "rgba".contains(c));
                match comps {
                    Some(cs)
                        if !cs.is_empty()
                            && cs.len() <= 4
                            && cs.iter().all(|&c| c < *n)
                            && !mixed =>
                    {
                        let ty = if cs.len() == 1 {
                            self.p.types.f32
                        } else {
                            self.p.types.vec(cs.len() as u8)
                        };
                        Expr { ty, span, kind: ExprKind::Swizzle(Box::new(b), cs) }
                    }
                    _ => {
                        self.err(
                            Diagnostic::new(
                                codes::E0317,
                                id.span,
                                format!("`.{}` isn't a component of a `vec{n}`", id.name),
                            )
                            .with_note(format!(
                                "a `vec{n}`'s components are {}",
                                ["x", "y", "z", "w"][..*n as usize].join(", ")
                            )),
                        );
                        self.error_expr(span)
                    }
                }
            }
            (TyKind::Var(_), _) => {
                self.err(
                    Diagnostic::new(
                        codes::E0306,
                        b.span,
                        "the type of this must be known before using its fields",
                    )
                    .with_help("annotate its type: `let x: T = ...`"),
                );
                self.error_expr(span)
            }
            _ => {
                let shown = self.display(b.ty);
                let what = match name {
                    ast::FieldName::Ident(n) => format!("`.{}`", n.name),
                    ast::FieldName::Index(..) | ast::FieldName::BadIndex(_) => {
                        format!("`.{}`", name.text())
                    }
                };
                self.err(Diagnostic::new(
                    codes::E0311,
                    name.span(),
                    format!("`{shown}` has no field {what}"),
                ));
                self.error_expr(span)
            }
        }
    }

    fn check_index(&mut self, base: &ast::Expr, index: &ast::Expr, span: Span) -> Expr {
        let b = self.check_expr(base, None);
        let k = self.kind(b.ty);
        if let TyKind::Adt(a, args) = &k
            && self.p.is_lang_adt(*a, Lang::Slots)
        {
            let gid = self.p.lang_adt(Lang::GlobalId).map(|g| self.p.types.adt(g, Vec::new()));
            let i = self.check_expr(index, gid);
            if let Some(g) = gid
                && self.infer.unify(&self.p.types, i.ty, g).is_err()
            {
                let shown = self.display(i.ty);
                self.err(
                    Diagnostic::new(
                        codes::E0601,
                        i.span,
                        format!(
                            "a `Slots` is indexed by the invocation's `GlobalId`, not `{shown}`"
                        ),
                    )
                    .with_note("each invocation may write only its own slot (§6.13)")
                    .with_help(
                        "index it with the kernel's `id: GlobalId` parameter: `out[id] = value`",
                    ),
                );
            }
            let elem = args[0];
            return Expr { ty: elem, span, kind: ExprKind::Index(Box::new(b), Box::new(i)) };
        }
        let i = self.check_expr(index, None);
        // An integer literal is a `u32` index. A value whose literal type isn't settled yet
        // (`let i = 1`) can be either index type: it's checked once it's settled.
        let open = self.infer.var_kind(&self.p.types, i.ty) == Some(VarKind::Int);
        let int_ok =
            open || matches!(self.kind(i.ty), TyKind::Int(IntTy::I32 | IntTy::U32) | TyKind::Error);
        if open {
            if matches!(i.kind, ExprKind::Lit(_)) {
                let u = self.p.types.u32;
                let _ = self.infer.unify(&self.p.types, i.ty, u);
            } else {
                self.indexes.push((i.ty, i.span));
            }
        }
        if !int_ok {
            let shown = self.display(i.ty);
            let d = Diagnostic::new(
                codes::E0312,
                i.span,
                format!("an index must be a `u32` or `i32`, not `{shown}`"),
            );
            // A number converts; a vector has components to choose from.
            self.err(match self.kind(i.ty) {
                TyKind::Int(_) | TyKind::Float(_) => d.with_help("convert it: `u32(i)`"),
                TyKind::Vec(_) => d.with_help("index with one of its components: `[v.x]`"),
                _ => d,
            });
        }
        // A vector's or matrix's components are fixed: a literal index past them is caught here
        // (a computed one traps at run time, like an array's).
        if let (TyKind::Vec(n) | TyKind::Mat(n), ExprKind::Lit(Lit::Int(v))) = (&k, &i.kind)
            && *v >= i128::from(*n)
        {
            let shown = self.display(b.ty);
            let what = if matches!(k, TyKind::Vec(_)) { "components" } else { "columns" };
            self.err(Diagnostic::new(
                codes::E0312,
                i.span,
                format!("index {v} is out of range: a `{shown}` has {n} {what}"),
            ));
        }
        let elem = match k {
            TyKind::Array(t, _) | TyKind::Slice(t) => *t,
            TyKind::Vec(_) => self.p.types.f32,
            TyKind::Mat(n) => self.p.types.vec(*n),
            TyKind::Error => self.p.types.error,
            _ => {
                let shown = self.display(b.ty);
                self.err(Diagnostic::new(
                    codes::E0312,
                    b.span,
                    format!("`{shown}` can't be indexed"),
                ));
                self.p.types.error
            }
        };
        Expr { ty: elem, span, kind: ExprKind::Index(Box::new(b), Box::new(i)) }
    }

    // ---- struct literals -------------------------------------------------------------------

    fn check_struct_lit(
        &mut self,
        path: &ast::Path,
        fields: &[ast::FieldInit],
        base: Option<&ast::Expr>,
        expected: Option<TyId>,
        span: Span,
    ) -> Expr {
        let segs = &path.segments;
        let is_self = segs.first().is_some_and(|s| s.ident.name == "Self");
        let res = if is_self {
            self.self_item(segs)
        } else {
            match resolve::resolve_module_path_in(self.p, self.scope.module, segs, true) {
                PathLookup::Found(r) => Some(r),
                PathLookup::Error(d) => {
                    self.err(*d);
                    return self.error_expr(span);
                }
                PathLookup::NotYet(..) | PathLookup::Broken => None,
            }
        };
        let (adt, variant) = match res {
            Some(Res::Adt(a)) if !self.p.adt(a).is_enum() => (a, None),
            Some(Res::Variant(a, v))
                if self.p.adt(a).variants()[v as usize].shape == VariantShape::Struct =>
            {
                (a, Some(v))
            }
            Some(r) => {
                let what = match r {
                    Res::Adt(a) => {
                        format!("`{}` is an enum; name one of its variants", self.p.adt(a).name)
                    }
                    Res::Variant(..) => {
                        "this variant has no named fields; write `Variant(...)`".into()
                    }
                    _ => format!("`{}` isn't a struct", path.last().name),
                };
                self.err(Diagnostic::new(codes::E0212, path.span, what));
                return self.error_expr(span);
            }
            None => {
                self.err(Diagnostic::new(
                    codes::E0200,
                    path.span,
                    format!("there's no struct `{}` here", path.last().name),
                ));
                return self.error_expr(span);
            }
        };
        let args = if is_self {
            match self.scope.self_ty.map(|t| self.kind(t)) {
                Some(TyKind::Adt(_, args)) => args.clone(),
                _ => Vec::new(),
            }
        } else {
            self.adt_args(adt, path, expected, span)
        };
        let ty = self.p.types.adt(adt, args.clone());
        if self.p.is_lang_adt(adt, Lang::GlobalId) {
            self.err(
                Diagnostic::new(codes::E0601, path.span, "a `GlobalId` can't be built")
                    .with_note("each invocation gets its own from the GPU, and a `Slots` is indexed by it, so each invocation writes only its own slot (§6.13)")
                    .with_help("take it as a kernel parameter: `id: GlobalId`"),
            );
        }
        let decls = self.p.adt_fields(adt, variant);
        let field_tys = self.p.fields_of(adt, &args, variant);
        let base_expr = base.map(|b| self.check_expect(b, ty));
        if let (Some(v), Some(b)) = (variant, &base_expr) {
            // Its fields are a variant's: the base, an enum value, may hold another one.
            let shown = self.display(ty);
            let name = &self.p.adt(adt).variants()[v as usize].name;
            self.err(
                Diagnostic::new(
                    codes::E0300,
                    b.span,
                    format!("`..base` fills a struct's fields, but this is an `{shown}`, which may hold a variant other than `{name}`"),
                )
                .with_help(format!("give every field of `{name}`, or `match` the base to take its fields")),
            );
        }
        let mut given: Vec<Option<Expr>> = vec![None; decls.len()];
        let mut order: Vec<u32> = Vec::new();
        let adt_mod = self.p.adt(adt).module;
        for f in fields {
            let Some(i) = decls.iter().position(|d| d.name == f.name.name) else {
                let mut d = Diagnostic::new(
                    codes::E0206,
                    f.name.span,
                    format!("`{}` has no field `{}`", self.p.adt(adt).name, f.name.name),
                );
                if let Some(s) =
                    resolve::closest(&f.name.name, decls.iter().map(|d| d.name.as_str()))
                {
                    d = d.with_fix(format!("did you mean `{s}`?"), f.name.span, s);
                }
                self.err(d);
                continue;
            };
            if !decls[i].public && adt_mod != self.scope.module && variant.is_none() {
                self.err(
                    Diagnostic::new(
                        codes::E0210,
                        f.name.span,
                        format!(
                            "the field `{}` of `{}` is private",
                            f.name.name,
                            self.p.adt(adt).name
                        ),
                    )
                    .with_help("use one of the type's constructor functions"),
                );
            }
            if given[i].is_some() {
                self.err(Diagnostic::new(
                    codes::E0308,
                    f.name.span,
                    format!("the field `{}` is given twice", f.name.name),
                ));
                continue;
            }
            let fty = field_tys[i];
            let value = match &f.value {
                Some(v) => self.check_expr(v, Some(fty)),
                None => {
                    // `S { time }`: the local `time`.
                    let p = ast::Path {
                        segments: vec![ast::PathSegment { ident: f.name.clone(), generics: None }],
                        span: f.name.span,
                    };
                    self.check_path_expr(&p, Some(fty), f.name.span)
                }
            };
            self.expect(value.ty, fty, value.span);
            given[i] = Some(value);
            order.push(i as u32);
        }
        let mut out = Vec::new();
        let mut missing = Vec::new();
        for (i, g) in given.into_iter().enumerate() {
            match g {
                Some(e) => out.push(e),
                None => {
                    if let Some(b) = &base_expr {
                        let fty = field_tys[i];
                        out.push(Expr { ty: fty, span: b.span, kind: ExprKind::FromBase });
                    } else if let Some(d) = &decls[i].default {
                        out.push(self.check_default(d, field_tys[i], adt_mod));
                        order.push(i as u32);
                    } else {
                        missing.push((decls[i].name.clone(), field_tys[i]));
                        out.push(self.error_expr(span));
                    }
                }
            }
        }
        if !missing.is_empty() {
            let list = missing.iter().map(|(m, _)| format!("`{m}`")).collect::<Vec<_>>().join(", ");
            let inserts =
                missing.iter().map(|(m, _)| format!("{m}: _")).collect::<Vec<_>>().join(", ");
            let mut d = Diagnostic::new(
                codes::E0307,
                path.span,
                format!("`{}` is missing {list}", self.p.adt(adt).name),
            )
            .with_help(format!(
                "give every field a value, or a default in the declaration: {inserts}"
            ));
            // A fix when every missing field has an obvious zero: after the last field given,
            // or just inside the `{`.
            let zeros: Option<Vec<String>> = missing
                .iter()
                .map(|(m, t)| self.zero_value(*t).map(|z| format!("{m}: {z}")))
                .collect();
            if let Some(zeros) = zeros {
                let (at, text) = match fields.last() {
                    Some(f) => (f.span.end, format!(", {}", zeros.join(", "))),
                    None => (span.end - 1, zeros.join(", ")),
                };
                let what = if missing.len() == 1 { "it" } else { "them" };
                d = d.with_fix(format!("add {what} as zero"), Span::new(span.file, at, at), text);
            }
            self.err(d);
        }
        let base = base_expr.map(Box::new);
        Expr { ty, span, kind: ExprKind::Adt { adt, args, variant, fields: out, order, base } }
    }

    /// The source text of a zero of a scalar or vector type, for fixes.
    fn zero_value(&self, t: TyId) -> Option<String> {
        Some(match self.kind(t) {
            TyKind::Float(_) => "0.0".into(),
            TyKind::Int(_) => "0".into(),
            TyKind::Bool => "false".into(),
            TyKind::Vec(n) => format!("vec{n}()"),
            _ => return None,
        })
    }

    /// A parameter's or field's default where a call or literal leaves it out. It's checked
    /// as where it's declared, in `module`: its names are that module's, never the caller's.
    pub(crate) fn check_default(&mut self, d: &ast::Expr, ty: TyId, module: ModuleId) -> Expr {
        let scope = std::mem::replace(&mut self.scope, resolve::Scope::new(module));
        let env = std::mem::take(&mut self.env);
        let e = self.check_expect(d, ty);
        self.env = env;
        self.scope = scope;
        e
    }

    /// `e`, checked with `ty` expected, then unified with `ty` (E0300 if they don't fit).
    pub(crate) fn check_expect(&mut self, e: &ast::Expr, ty: TyId) -> Expr {
        let x = self.check_expr(e, Some(ty));
        self.expect(x.ty, ty, x.span);
        x
    }

    // ---- control flow ----------------------------------------------------------------------

    fn check_if(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Block,
        else_: Option<&ast::Expr>,
        expected: Option<TyId>,
        span: Span,
    ) -> Expr {
        let bool_ty = self.p.types.bool;
        let c = self.check_expr(cond, Some(bool_ty));
        self.expect_cond(&c);
        let t = self.check_block(then, expected);
        let (else_e, ty) = match else_ {
            Some(e) => {
                let ex = self.check_expr(e, expected.or(Some(t.ty)));
                let ty = if matches!(self.kind(t.ty), TyKind::Never) {
                    ex.ty
                } else {
                    if !matches!(self.kind(ex.ty), TyKind::Never)
                        && self.infer.unify(&self.p.types, ex.ty, t.ty).is_err()
                    {
                        let (a, b) = (self.display(t.ty), self.display(ex.ty));
                        // Point at the values, not the blocks around them.
                        let at = match &ex.kind {
                            ExprKind::Block(b) => b.tail.as_ref().map_or(ex.span, |t| t.span),
                            _ => ex.span,
                        };
                        let then_at = t.tail.as_ref().map_or(t.span, |x| x.span);
                        let d = Diagnostic::new(
                            codes::E0300,
                            at,
                            format!(
                                "the branches of this `if` have different types: `{a}` and `{b}`"
                            ),
                        )
                        .with_secondary(then_at, format!("this is `{a}`"));
                        self.err(self.branch_mismatch(d, t.ty, ex.ty));
                    }
                    t.ty
                };
                (Some(Box::new(ex)), ty)
            }
            None => {
                let unit = self.p.types.unit;
                if let Some(tail) = &t.tail
                    && !matches!(
                        self.kind(tail.ty),
                        TyKind::Never | TyKind::Error | TyKind::Tuple(_)
                    )
                    && expected.is_some_and(|e| self.shallow(e) != unit)
                {
                    let shown = self.display(tail.ty);
                    self.err(
                        Diagnostic::new(codes::E0300, span, format!("this `if` has no `else`, so it has no value when the condition is false, but its branch is a `{shown}`"))
                            .with_help("add an `else` branch"),
                    );
                }
                (None, unit)
            }
        };
        Expr { ty, span, kind: ExprKind::If { cond: Box::new(c), then: t, else_: else_e } }
    }

    fn check_return(&mut self, value: Option<&ast::Expr>, span: Span) -> Expr {
        let target = self.closure_ret().unwrap_or(self.ret_ty);
        let v = match value {
            Some(v) => Some(Box::new(self.check_expect(v, target))),
            None => {
                let unit = self.p.types.unit;
                if !matches!(self.kind(target), TyKind::Var(_)) && self.shallow(target) != unit {
                    let shown = self.display(target);
                    self.err(Diagnostic::new(
                        codes::E0314,
                        span,
                        format!("`return` needs a `{shown}` here"),
                    ));
                } else {
                    let _ = self.infer.unify(&self.p.types, target, unit);
                }
                None
            }
        };
        Expr { ty: self.p.types.never, span, kind: ExprKind::Return(v) }
    }

    fn check_closure(
        &mut self,
        params: &[ast::ClosureParam],
        ret: Option<&ast::TypeExpr>,
        body: &ast::Expr,
        expected: Option<TyId>,
        span: Span,
    ) -> Expr {
        let exp = match expected.map(|t| self.kind(t)) {
            Some(TyKind::FnPtr(ps, r)) if ps.len() == params.len() => Some((ps.clone(), *r)),
            Some(TyKind::FnPtr(ps, _)) => {
                self.err(Diagnostic::new(
                    codes::E0301,
                    span,
                    format!(
                        "this closure takes {} parameters, but {} are expected",
                        params.len(),
                        ps.len()
                    ),
                ));
                None
            }
            // Expected where a type failed (its error is reported): the closure's unannotated
            // types are errors too, not left to infer.
            Some(TyKind::Error) => {
                let e = self.p.types.error;
                Some((vec![e; params.len()], e))
            }
            _ => None,
        };
        let ret_ty = match ret {
            Some(t) => self.resolve_type(t),
            None => match &exp {
                Some((_, r)) => *r,
                None => self.new_var(VarKind::General, span),
            },
        };
        let id = self.begin_closure(ret_ty);
        let env_len = self.env.len();
        let mut locals = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let ty = match &p.ty {
                Some(t) => {
                    let t = self.resolve_type(t);
                    if let Some((ps, _)) = &exp {
                        self.expect(ps[i], t, p.name.span);
                    }
                    t
                }
                None => match &exp {
                    Some((ps, _)) => ps[i],
                    None => self.new_var(VarKind::General, p.name.span),
                },
            };
            if let Some(first) = params[..i].iter().find(|q| q.name.name == p.name.name) {
                self.err(
                    Diagnostic::new(
                        codes::E0201,
                        p.name.span,
                        format!("`{}` is a parameter of this closure twice", p.name.name),
                    )
                    .with_secondary(first.name.span, "first here"),
                );
            }
            locals.push(self.declare_closure_param(&p.name, ty));
        }
        let b = self.without_loops(|this| this.check_expect(body, ret_ty));
        self.env.truncate(env_len);
        let id2 = self.end_closure(locals, ret_ty, b, span);
        debug_assert_eq!(id, id2);
        let env = self.scope_args();
        let ty = self.p.types.intern(TyKind::Closure(id, env));
        Expr { ty, span, kind: ExprKind::Closure(id) }
    }

    /// The enclosing function's generic parameters, as types (a closure's environment).
    pub(crate) fn scope_args(&mut self) -> Vec<TyId> {
        match self.fn_id {
            Some(f) => {
                let gens = self.p.fn_all_generics(f);
                gens.iter().map(|&g| self.p.types.param(g)).collect()
            }
            None => Vec::new(),
        }
    }
}

/// A value of variant `v` of the enum `adt`, with `fields` in declared order.
pub(super) fn variant_expr(
    ty: TyId,
    span: Span,
    adt: AdtId,
    args: Vec<TyId>,
    v: u32,
    fields: Vec<Expr>,
) -> Expr {
    let order = (0..fields.len() as u32).collect();
    Expr {
        ty,
        span,
        kind: ExprKind::Adt { adt, args, variant: Some(v), fields, order, base: None },
    }
}
