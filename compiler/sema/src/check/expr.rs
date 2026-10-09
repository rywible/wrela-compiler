//! Expressions.

use super::Checker;
use crate::defs::*;
use crate::resolve::{self, PathLookup};
use crate::thir::*;
use crate::ty::*;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_syntax::ast::{self, BinOp, UnOp};
use wrela_syntax::lexer::parse_spec;

/// What a path in an expression names.
pub(crate) enum ValueRes {
    Local(LocalId),
    Item(Res),
    /// `Type::name`: an associated function or variant of a type.
    TypeRelative(TyId, ast::Ident),
    /// `Trait::method`.
    TraitRelative(TraitId, ast::Ident),
    /// A `const N: u32` generic parameter's value.
    ConstParam(ParamId),
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
                    Some(TyKind::Array(t, _) | TyKind::ArrayN(t, _) | TyKind::Slice(t)) => Some(*t),
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
                    Some(TyKind::Array(t, _) | TyKind::ArrayN(t, _)) => Some(*t),
                    _ => None,
                };
                let v = self.check_expr(value, elem_exp);
                // `[x; N]` with `const N: u32`: its length is known once it's instantiated.
                let param_len = match &count.kind {
                    ast::ExprKind::Path(p) if p.is_single() => self
                        .scope
                        .param(&p.segments[0].ident.name)
                        .filter(|&g| self.p.param(g).is_const),
                    _ => None,
                };
                let n = if param_len.is_some() { Some(0) } else { self.const_u32(count) };
                let n = match n {
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
                let ty = match param_len {
                    Some(g) => self.p.types.intern(TyKind::ArrayN(v.ty, self.p.types.param(g))),
                    None => self.p.types.array(v.ty, n),
                };
                Expr { ty, span, kind: ExprKind::ArrayRepeat(Box::new(v), n) }
            }
            ast::ExprKind::Block(b) => {
                let blk = self.check_block(b, expected);
                Expr { ty: blk.ty, span, kind: ExprKind::Block(blk) }
            }
            ast::ExprKind::If { pat: None, cond, then, else_ } => {
                self.check_if(cond, then, else_.as_deref(), expected, span)
            }
            ast::ExprKind::If { pat: Some(pat), cond, then, else_ } => {
                self.check_if_let(pat, cond, then, else_.as_deref(), expected, span)
            }
            ast::ExprKind::Match { mutable, scrutinee, arms } => {
                self.check_match(scrutinee, arms, *mutable, expected, span)
            }
            ast::ExprKind::Try(inner) => self.check_try(inner, span),
            ast::ExprKind::Assign { target, op, value } => {
                let st = self.check_assign(target, *op, value, span);
                let unit = self.p.types.unit;
                let block = Block { stmts: vec![st], tail: None, ty: unit, span };
                Expr { ty: unit, span, kind: ExprKind::Block(block) }
            }
            ast::ExprKind::Unsafe(b) => {
                if !self.p.package_of(self.scope.module).unsafe_ok {
                    self.err(
                        Diagnostic::new(
                            codes::E0214,
                            span.shrink_to_start().to(b.span.shrink_to_start()),
                            "`unsafe` is only for packages that declare it",
                        )
                        .with_note("`unsafe` exists for the stdlib's core: the checker can't verify what's inside (§6.14)")
                        .with_help("declare `unsafe = true` in the package's `wrela.toml`, if it must"),
                    );
                }
                self.unsafe_depth += 1;
                let blk = self.check_block(b, expected);
                self.unsafe_depth -= 1;
                Expr { ty: blk.ty, span, kind: ExprKind::Block(blk) }
            }
            ast::ExprKind::FString(parts) => self.check_fstring(parts, span),
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
                let text = wrela_syntax::lexer::string_value(&lit.text);
                self.text_expr(text, span)
            }
            ast::LitKind::Suffixed => self.check_suffixed(lit, expected),
        }
    }

    /// A string literal's value: a `Text`.
    pub(crate) fn text_expr(&mut self, text: String, span: Span) -> Expr {
        match self.p.lang_adt(Lang::Text) {
            Some(a) => Expr {
                ty: self.p.types.intern(TyKind::Adt(a, Vec::new())),
                span,
                kind: ExprKind::Text(text.into()),
            },
            None => self.error_expr(span),
        }
    }

    /// `15cm`: the number times its unit's constant, `std::units::cm`, folded now (§5). When
    /// both are decimals, as every unit in std is, the product is exact and rounded once to
    /// each float type, so `15cm` is `0.15`, as an `f32` and as an `f64`. Otherwise it's worked
    /// out in `f64` and rounded once to an `f32`.
    fn check_suffixed(&mut self, lit: &ast::Lit, expected: Option<TyId>) -> Expr {
        let span = lit.span;
        let (digits, suffix) = wrela_syntax::lexer::split_suffix(&lit.text);
        let Some(&c) = self.p.units.get(suffix) else {
            let mut d = Diagnostic::new(codes::E0216, span, format!("`{suffix}` isn't a unit"))
                .with_note("units are the constants in `std::units`, in SI: `m`, `cm`, `kg`, `s`, `deg`, ...");
            let names = self.p.units.keys().map(String::as_str);
            if let Some(s) = resolve::closest(suffix, names) {
                let at = Span::new(span.file, span.start + digits.len() as u32, span.end);
                d = d.with_fix(format!("did you mean `{s}`?"), at, s);
            }
            self.err(d);
            return self.error_expr(span);
        };
        let number = match wrela_syntax::lexer::int_value(digits) {
            ast::IntValue::Ok(v) => v as f64,
            _ => wrela_syntax::lexer::float_value(digits),
        };
        let value = &self.p.const_(c).value.kind;
        let unit = match value {
            ast::ExprKind::Lit(ast::Lit { kind: ast::LitKind::Float(v), .. }) => *v,
            ast::ExprKind::Lit(ast::Lit {
                kind: ast::LitKind::Int(ast::IntValue::Ok(v)), ..
            }) => *v as f64,
            _ => 1.0,
        };
        let exact = match value {
            ast::ExprKind::Lit(u) => decimal_product(digits, &u.text),
            _ => None,
        };
        let (v, f) = match exact {
            Some(text) => (text.parse().unwrap_or(f64::NAN), text.parse().unwrap_or(f32::NAN)),
            None => (number * unit, (number * unit) as f32),
        };
        let ty = match expected {
            Some(t) if matches!(self.kind(t), TyKind::Float(_)) => self.shallow(t),
            _ => self.new_var(VarKind::Float, span),
        };
        self.float_literals.push((ty, v, f, span));
        Expr { ty, span, kind: ExprKind::Lit(Lit::Float(v, f)) }
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
            if let Some(g) = self.scope.param(&first.name)
                && self.p.param(g).is_const
            {
                return Some(ValueRes::ConstParam(g));
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
            let d = resolve::with_std_import(self.p, d, &first.name, first.span);
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
            ValueRes::ConstParam(g) => {
                Expr { ty: self.p.types.u32, span, kind: ExprKind::ConstParam(g) }
            }
            ValueRes::Item(Res::Const(c)) => {
                self.p.note_const_use(c);
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
            ValueRes::Item(Res::TraitSet(s)) => {
                let name = &self.p.trait_sets[s.index()].name;
                self.err(Diagnostic::new(
                    codes::E0212,
                    span,
                    format!("`{name}` is a trait set, not a value"),
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
            ValueRes::Item(Res::BuiltinTy(_) | Res::Alias(_)) => {
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
                // `T::f` as a value: an associated function of the type, its own or a trait's.
                let generics = path.segments.last().and_then(|s| s.generics.as_deref());
                if let Some(e) = self.associated_fn_value(ty, &name, generics, span) {
                    return e;
                }
                self.err(Diagnostic::new(
                    codes::E0207,
                    name.span,
                    format!("`{}` has no associated function `{}`", self.display(ty), name.name),
                ));
                self.error_expr(span)
            }
            ValueRes::TraitRelative(t, name) => {
                let tname = &self.p.trait_(t).name;
                self.err(
                    Diagnostic::new(
                        codes::E0212,
                        span,
                        format!(
                            "`{tname}::{}` names no implementation, so it can only be called here",
                            name.name
                        ),
                    )
                    .with_help(format!(
                        "name the type whose implementation it is, as in `T::{}` with `T: {tname}`",
                        name.name
                    )),
                );
                self.error_expr(span)
            }
        }
    }

    /// Whether a type has an inherent associated function or method of this name.
    pub(crate) fn has_associated_fn(&self, a: AdtId, name: &str) -> bool {
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
                TyKind::Vec(e, _) => *e != VecElem::U32,
                TyKind::Float(_) | TyKind::Mat(..) | TyKind::Error | TyKind::Never => true,
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
        self.check_units(op, a, b);
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
                if op.is_comparison() {
                    match self.trait_comparison(op, x, y, span) {
                        Ok(e) => return e,
                        Err(xy) => *xy,
                    }
                } else {
                    (x, y)
                }
            }
        };
        // `"a" + b`: text is joined with an f-string.
        if op == BinOp::Add
            && self.is_stringy(self.shallow(ea.ty))
            && self.is_stringy(self.shallow(eb.ty))
        {
            let mut d = Diagnostic::new(codes::E0305, span, "`+` doesn't join text in wrela")
                .with_note(
                    "text is joined by an f-string, which formats each part into one `String` (§4)",
                );
            match (f_part(a), f_part(b)) {
                (Some(x), Some(y)) => {
                    d = d.with_fix("write an f-string", span, format!("f\"{x}{y}\""));
                }
                _ => d = d.with_help("write an f-string: `f\"{a}{b}\"`"),
            }
            self.err(d);
            return self.error_expr(span);
        }
        // Literals on the left take the right's type: `2 * x`.
        let ty = self.binary_result(op, ea.ty, eb.ty, span).unwrap_or(self.p.types.error);
        Expr { ty, span, kind: ExprKind::Binary(op, Box::new(ea), Box::new(eb)) }
    }

    /// A comparison that goes through `Eq` or `Ord` (§3): of strings (as `str`), or of values
    /// whose types declare the trait. `==` calls `a.eq(b)`, and `a < b` is
    /// `a.cmp(b) == Ordering::Less`. Numbers, `bool`, vectors and enums whose variants hold
    /// nothing compare without a trait (the operands are handed back).
    fn trait_comparison(
        &mut self,
        op: BinOp,
        a: Expr,
        b: Expr,
        span: Span,
    ) -> Result<Expr, Box<(Expr, Expr)>> {
        let (ta, tb) = (self.shallow(a.ty), self.shallow(b.ty));
        let ordered = matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge);
        let lang = if ordered { Lang::Ord } else { Lang::Eq };
        let Some(tr) = self.p.lang_trait(lang) else { return Err(Box::new((a, b))) };
        let self_ty = if self.is_stringy(ta) && self.is_stringy(tb) {
            self.p.types.intern(TyKind::Str)
        } else {
            let builtin = match self.kind(ta) {
                TyKind::Adt(adt, _) => {
                    let def = self.p.adt(*adt);
                    !ordered && def.is_enum() && def.variants().iter().all(|v| v.fields.is_empty())
                }
                TyKind::Param(_) | TyKind::Tuple(_) | TyKind::Array(..) | TyKind::ArrayN(..) => {
                    false
                }
                _ => true,
            };
            if builtin || self.infer.unify(&self.p.types, ta, tb).is_err() {
                return Err(Box::new((a, b)));
            }
            let r = TraitRef { trait_: tr, args: Vec::new() };
            let name = self.p.trait_(tr).name.clone();
            let shown = self.display(ta);
            if !self.p.types.has_vars(ta) && !crate::traits::implements(self.p, ta, &r) {
                let what = if ordered { "ordered" } else { "compared with `==`" };
                let mut d = Diagnostic::new(
                    codes::E0305,
                    span,
                    format!("`{shown}` can't be {what}: it doesn't declare `{name}`"),
                );
                if let TyKind::Adt(adt, _) = self.kind(ta) {
                    let def = self.p.adt(*adt);
                    d = d.with_help(format!(
                        "declare it where `{}` is declared: `{} {}: {name} {{ ... }}`",
                        def.name,
                        if def.is_enum() { "enum" } else { "struct" },
                        def.name
                    ));
                }
                self.err(d);
                return Ok(self.error_expr(span));
            }
            self.obligation(ta, r, span, format!("`{}`", op.text()));
            ta
        };
        let method_name = if ordered { "cmp" } else { "eq" };
        let Some(method) = crate::traits::trait_method(self.p, tr, method_name) else {
            return Err(Box::new((a, b)));
        };
        let bool_ty = self.p.types.bool;
        let ret = if ordered {
            match self.p.lang_adt(Lang::Ordering) {
                Some(o) => self.p.types.intern(TyKind::Adt(o, Vec::new())),
                None => return Err(Box::new((a, b))),
            }
        } else {
            bool_ty
        };
        let call = Expr {
            ty: ret,
            span,
            kind: ExprKind::Call(Call {
                callee: Callee::TraitMethod {
                    method,
                    self_ty,
                    trait_args: Vec::new(),
                    method_args: Vec::new(),
                },
                args: vec![a, b],
                modes: vec![Mode::Borrow, Mode::Borrow],
                order: vec![0, 1],
                receiver: true,
                ret_mode: RetMode::Owned,
            }),
        };
        if !ordered {
            return Ok(if op == BinOp::Eq {
                call
            } else {
                Expr { ty: bool_ty, span, kind: ExprKind::Unary(UnOp::Not, Box::new(call)) }
            });
        }
        // `<` is `== Less`, `<=` is `!= Greater`, `>` is `== Greater`, `>=` is `!= Less`.
        let (variant, test) = match op {
            BinOp::Lt => (0, BinOp::Eq),
            BinOp::Le => (2, BinOp::Ne),
            BinOp::Gt => (2, BinOp::Eq),
            _ => (0, BinOp::Ne),
        };
        let TyKind::Adt(ordering, _) = *self.kind(ret) else {
            return Err(Box::new((call, self.error_expr(span))));
        };
        let k = Expr {
            ty: ret,
            span,
            kind: ExprKind::Adt {
                adt: ordering,
                args: Vec::new(),
                variant: Some(variant),
                fields: Vec::new(),
                order: Vec::new(),
                base: None,
            },
        };
        Ok(Expr { ty: bool_ty, span, kind: ExprKind::Binary(test, Box::new(call), Box::new(k)) })
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
                    TyKind::Bool | TyKind::Vec(..) => !ordered,
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
                if self.is_int_vec(a) {
                    return Some(a);
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
                // An integer vector shifts each component, by one amount or by a `u32` each.
                if let &TyKind::Vec(e, n) = self.kind(a)
                    && !e.is_float()
                {
                    let each = self.p.types.vec_of(VecElem::U32, n);
                    if !self.try_unify(b, u) && !self.try_unify(b, each) {
                        return fail(self);
                    }
                    return Some(a);
                }
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
                    TyKind::Float(_) => Some(a),
                    TyKind::Vec(e, _) if e.is_float() => Some(a),
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
                // Vector and scalar (its component's type), matrix and vector.
                let scalar =
                    |k: &TyKind| matches!(k, TyKind::Float(_) | TyKind::Int(_) | TyKind::Var(_));
                match (&ka, &kb) {
                    (TyKind::Vec(..) | TyKind::Mat(..), _)
                        if scalar(kb)
                            && (!matches!(kb, TyKind::Var(_)) || self.is_number_var(b)) =>
                    {
                        let c = crate::builtins::scalar_of(&self.p.types, self.shallow(a));
                        if matches!(kb, TyKind::Var(_)) {
                            let _ = self.infer.unify(&self.p.types, b, c);
                        }
                        if self.shallow(b) != c {
                            return fail(self);
                        }
                        if matches!(ka, TyKind::Mat(..)) && !matches!(op, BinOp::Mul) {
                            return fail(self);
                        }
                        return Some(a);
                    }
                    (_, TyKind::Vec(..) | TyKind::Mat(..))
                        if scalar(ka)
                            && (!matches!(ka, TyKind::Var(_)) || self.is_number_var(a)) =>
                    {
                        let c = crate::builtins::scalar_of(&self.p.types, self.shallow(b));
                        if matches!(ka, TyKind::Var(_)) {
                            let _ = self.infer.unify(&self.p.types, a, c);
                        }
                        if self.shallow(a) != c
                            || matches!(op, BinOp::Div | BinOp::Rem)
                                && matches!(kb, TyKind::Mat(..))
                        {
                            return fail(self);
                        }
                        if matches!(kb, TyKind::Mat(..)) && !matches!(op, BinOp::Mul) {
                            return fail(self);
                        }
                        return Some(b);
                    }
                    // `M * v`: C components in, R out; `v * M`: R in, C out (WGSL's).
                    (&TyKind::Mat(c, r), &TyKind::Vec(VecElem::F32, m)) if op == BinOp::Mul => {
                        if c != m {
                            return fail(self);
                        }
                        return Some(self.p.types.vec(*r));
                    }
                    (&TyKind::Vec(VecElem::F32, m), &TyKind::Mat(c, r)) if op == BinOp::Mul => {
                        if r != m {
                            return fail(self);
                        }
                        return Some(self.p.types.vec(*c));
                    }
                    // `A * B`: A's columns are B's rows; B's columns of A's rows.
                    (&TyKind::Mat(c1, r1), &TyKind::Mat(c2, r2)) if op == BinOp::Mul => {
                        if c1 != r2 {
                            return fail(self);
                        }
                        return Some(self.p.types.intern(TyKind::Mat(*c2, *r1)));
                    }
                    _ => {}
                }
                if self.infer.unify(&self.p.types, a, b).is_err() {
                    return fail(self);
                }
                match self.kind(a) {
                    TyKind::Int(_) | TyKind::Float(_) | TyKind::Vec(..) => Some(a),
                    TyKind::Mat(..) if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) => {
                        Some(a)
                    }
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
        self.field_of_checked(b, name, span)
    }

    /// Field `name` of `b`, whose expression is checked already.
    pub(crate) fn field_of_checked(&mut self, b: Expr, name: &ast::FieldName, span: Span) -> Expr {
        // Resolved through: a field `T::Out` normalizes only once `T` is known.
        let bt = self.infer.resolve(&self.p.types, b.ty);
        // A field of the value a `GpuField` names, where it is: a `GpuField` of the field (§12).
        if let (&TyKind::Adt(a, ref args), ast::FieldName::Ident(n)) = (self.kind(bt), name)
            && self.p.is_lang_adt(a, Lang::GpuField)
            && args.len() == 2
        {
            let (outer, inner) = (args[0], self.infer.resolve(&self.p.types, args[1]));
            return self.gpu_field(b, outer, inner, n, span);
        }
        // A `Packed` struct's field: its bits of the word (§3).
        if let (&TyKind::Adt(a, _), ast::FieldName::Ident(n)) = (self.kind(bt), name)
            && self.p.packed.contains_key(&a)
        {
            return match self.packed_field(a, n) {
                Some(f) => self.packed_read(b, &f, span),
                None => self.error_expr(span),
            };
        }
        match (self.kind(bt), name) {
            // Its error is reported.
            (TyKind::Error, _) | (_, ast::FieldName::BadIndex(_)) => self.error_expr(span),
            (&TyKind::Adt(a, ref args), ast::FieldName::Ident(n)) if !self.p.adt(a).is_enum() => {
                let fields = self.p.adt(a).fields();
                match fields.iter().position(|f| f.name == n.name) {
                    Some(i) => {
                        let f = &fields[i];
                        let adt_mod = self.p.adt(a).module;
                        if self.field_hidden(adt_mod, f) {
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
            (&TyKind::Vec(e, ref n), ast::FieldName::Ident(id)) => {
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
                            self.p.types.elem(e)
                        } else {
                            self.p.types.vec_of(e, cs.len() as u8)
                        };
                        Expr { ty, span, kind: ExprKind::Swizzle(Box::new(b), cs) }
                    }
                    _ => {
                        self.err(
                            Diagnostic::new(
                                codes::E0317,
                                id.span,
                                format!(
                                    "`.{}` isn't a component of a `{}`",
                                    id.name,
                                    self.display(b.ty)
                                ),
                            )
                            .with_note(format!(
                                "a `{}`'s components are {}",
                                self.display(b.ty),
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

    /// `f.name`, `f` a `GpuField<S, T>` and `T` a struct: its field's `GpuField`, through
    /// `std::gpu::field_at`, which adds the field's offset in `T`'s layout.
    fn gpu_field(&mut self, f: Expr, outer: TyId, inner: TyId, n: &ast::Ident, span: Span) -> Expr {
        let TyKind::Adt(a, targs) = self.kind(inner).clone() else {
            let shown = self.display(inner);
            self.err(Diagnostic::new(
                codes::E0206,
                n.span,
                format!("a `GpuField` of `{shown}` has no fields to name"),
            ));
            return self.error_expr(span);
        };
        let fields = self.p.adt(a).fields();
        let Some(i) =
            fields.iter().position(|f| f.name == n.name).filter(|_| !self.p.adt(a).is_enum())
        else {
            let name = self.p.adt(a).name.clone();
            self.err(Diagnostic::new(
                codes::E0206,
                n.span,
                format!("`{name}` has no field `{}`", n.name),
            ));
            return self.error_expr(span);
        };
        if self.field_hidden(self.p.adt(a).module, &fields[i]) {
            let name = self.p.adt(a).name.clone();
            self.err(Diagnostic::new(
                codes::E0210,
                n.span,
                format!("the field `{}` of `{name}` is private", n.name),
            ));
        }
        let ft = self.p.field_ty(a, &targs, None, i).unwrap_or(self.p.types.error);
        let (Some(at), Some(gf)) =
            (self.p.lang_fn(Lang::GpuFieldAt), self.p.lang_adt(Lang::GpuField))
        else {
            return self.error_expr(span);
        };
        let ty = self.p.types.adt(gf, vec![outer, ft]);
        // The field's index is a type argument, so lowering knows it as it does the types.
        let k = self.p.types.intern(TyKind::ConstU32(i as u32));
        Expr {
            ty,
            span,
            kind: ExprKind::Call(Call {
                callee: Callee::Fn { func: at, args: vec![outer, inner, ft, k] },
                args: vec![f],
                modes: vec![Mode::Borrow],
                order: vec![0],
                receiver: false,
                ret_mode: RetMode::Owned,
            }),
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
        // `arena[h]`: the value a `Handle<T>` names (§6.8).
        if let TyKind::Adt(a, args) = &k
            && self.p.is_lang_adt(*a, Lang::Arena)
        {
            let elem = args[0];
            let handle = self.p.lang_adt(Lang::Handle).map(|h| self.p.types.adt(h, vec![elem]));
            let i = self.check_expr(index, handle);
            if let Some(h) = handle
                && !self.try_unify(i.ty, h)
                && !matches!(self.kind(i.ty), TyKind::Error)
            {
                let shown = self.display(i.ty);
                let want = self.display(h);
                self.err(
                    Diagnostic::new(
                        codes::E0312,
                        i.span,
                        format!("an arena is indexed by a `{want}`, not `{shown}`"),
                    )
                    .with_note("an arena's values are named by their handles (§6.8)"),
                );
            }
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
                TyKind::Vec(..) => d.with_help("index with one of its components: `[v.x]`"),
                _ => d,
            });
        }
        // A vector's or matrix's components are fixed: a literal index past them is caught here
        // (a computed one traps at run time, like an array's).
        if let (TyKind::Vec(_, n) | TyKind::Mat(n, _), ExprKind::Lit(Lit::Int(v))) = (&k, &i.kind)
            && *v >= i128::from(*n)
        {
            let shown = self.display(b.ty);
            let what = if matches!(k, TyKind::Vec(..)) { "components" } else { "columns" };
            self.err(Diagnostic::new(
                codes::E0312,
                i.span,
                format!("index {v} is out of range: a `{shown}` has {n} {what}"),
            ));
        }
        let elem = match k {
            TyKind::Array(t, _) | TyKind::ArrayN(t, _) | TyKind::Slice(t) => *t,
            &TyKind::Vec(e, _) => self.p.types.elem(e),
            // A column.
            TyKind::Mat(_, r) => self.p.types.vec(*r),
            TyKind::Error => self.p.types.error,
            _ if let Some(t) = self.vec_elem(b.ty) => t,
            TyKind::Adt(a, args) if self.p.is_lang_adt(*a, Lang::Bounded) => args[0],
            // A buffer's elements, in GPU code (§12; CPU code can't read them, E0607).
            TyKind::Adt(a, args) if self.p.is_lang_adt(*a, Lang::GpuSpan) => args[0],
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

    /// A borrow struct's field `d` given `value` (§6.6): a projection field takes a place, and
    /// a `mut` one is marked `mut place`, as at a call site.
    fn borrow_field(&mut self, d: &FieldDef, value: &Expr) {
        let marked = matches!(value.kind, ExprKind::MutArg(_));
        match d.mode {
            RetMode::Mut if !marked && !matches!(value.kind, ExprKind::Error) => {
                self.err(
                    Diagnostic::new(
                        codes::E0503,
                        value.span,
                        format!("`{}` is a `mut` projection, so it's given as `mut place`", d.name),
                    )
                    .with_fix(
                        "add `mut`",
                        value.span.shrink_to_start(),
                        "mut ",
                    ),
                );
            }
            RetMode::Borrow | RetMode::Owned if marked => {
                let ExprKind::MutArg(inner) = &value.kind else { return };
                self.err(
                    Diagnostic::new(
                        codes::E0504,
                        value.span,
                        format!(
                            "`{}` isn't a `mut` projection, so it can't take `mut ...`",
                            d.name
                        ),
                    )
                    .with_fix(
                        "remove `mut`",
                        Span::new(value.span.file, value.span.start, inner.span.start),
                        "",
                    ),
                );
            }
            _ => {}
        }
        if d.mode != RetMode::Owned {
            let inner = match &value.kind {
                ExprKind::MutArg(x) => x.as_ref(),
                _ => value,
            };
            if !inner.is_place() && !matches!(inner.kind, ExprKind::Error) {
                self.err(
                    Diagnostic::new(
                        codes::E0508,
                        value.span,
                        format!(
                            "`{}` is a projection, so it's given a place, and this is a temporary",
                            d.name
                        ),
                    )
                    .with_help("bind the value to a name first, and give the name"),
                );
            }
        }
    }

    /// `Packed` struct `a`'s field `n` (§3), or E0206 (E0210 for a private one elsewhere).
    pub(crate) fn packed_field(&mut self, a: AdtId, n: &ast::Ident) -> Option<PackedField> {
        let fields = self.p.packed.get(&a)?;
        let adt = self.p.adt(a);
        match fields.iter().find(|f| f.name == n.name) {
            Some(f) => {
                if !f.public && adt.module != self.scope.module {
                    self.err(
                        Diagnostic::new(
                            codes::E0210,
                            n.span,
                            format!("the field `{}` of `{}` is private", n.name, adt.name),
                        )
                        .with_secondary(f.span, "declared here without `pub`"),
                    );
                }
                Some(f.clone())
            }
            None => {
                let names: Vec<String> = fields.iter().map(|f| format!("`{}`", f.name)).collect();
                self.err(
                    Diagnostic::new(
                        codes::E0206,
                        n.span,
                        format!("`{}` has no field `{}`", adt.name, n.name),
                    )
                    .with_note(format!("its fields: {}", names.join(", "))),
                );
                None
            }
        }
    }

    /// Field `f` of `base`, a `Packed` struct (§3): its bits of the word, as a `u32`.
    pub(crate) fn packed_read(&self, base: Expr, f: &PackedField, span: Span) -> Expr {
        let u = self.p.types.u32;
        let word = Expr { ty: u, span: base.span, kind: ExprKind::Field(Box::new(base), 0) };
        let lit = |v: u64| Expr { ty: u, span, kind: ExprKind::Lit(Lit::Int(i128::from(v))) };
        let shifted = if f.shift == 0 {
            word
        } else {
            let k = lit(u64::from(f.shift));
            Expr {
                ty: u,
                span,
                kind: ExprKind::Binary(ast::BinOp::Shr, Box::new(word), Box::new(k)),
            }
        };
        if f.width == 32 {
            return shifted;
        }
        let mask = lit((1u64 << f.width) - 1);
        Expr {
            ty: u,
            span,
            kind: ExprKind::Binary(ast::BinOp::BitAnd, Box::new(shifted), Box::new(mask)),
        }
    }

    /// `v`, a `u32`, fitted to `width` bits of a `Packed` field and moved to `shift` (§3).
    pub(crate) fn packed_bits(&self, v: Expr, width: u32, shift: u32, span: Span) -> Expr {
        let u = self.p.types.u32;
        let lit = |v: u64| Expr { ty: u, span, kind: ExprKind::Lit(Lit::Int(i128::from(v))) };
        let fit = match self.p.lang_fn(Lang::PackedFit) {
            Some(func) if width < 32 => Expr {
                ty: u,
                span,
                kind: ExprKind::Call(Call {
                    callee: Callee::Fn { func, args: Vec::new() },
                    args: vec![v, lit(u64::from(width))],
                    modes: vec![Mode::Borrow, Mode::Borrow],
                    receiver: false,
                    order: vec![0, 1],
                    ret_mode: RetMode::Owned,
                }),
            },
            _ => v,
        };
        if shift == 0 {
            return fit;
        }
        let k = lit(u64::from(shift));
        Expr { ty: u, span, kind: ExprKind::Binary(ast::BinOp::Shl, Box::new(fit), Box::new(k)) }
    }

    /// A `Packed` struct's literal (§3): its word, each field's bits in it, in the order written.
    /// With `..base`, the fields not given are the base's.
    fn packed_lit(
        &mut self,
        adt: AdtId,
        args: Vec<TyId>,
        fields: &[ast::FieldInit],
        base: Option<&ast::Expr>,
        span: Span,
    ) -> Expr {
        let u = self.p.types.u32;
        let ty = self.p.types.adt(adt, args.clone());
        let views = self.p.packed.get(&adt).cloned().unwrap_or_default();
        let mut word: Option<Expr> = None;
        let mut given = Vec::new();
        for f in fields {
            let Some(view) = self.packed_field(adt, &f.name) else {
                if let Some(v) = &f.value {
                    self.check_expr(v, None);
                }
                continue;
            };
            if given.contains(&view.name) {
                self.err(Diagnostic::new(
                    codes::E0308,
                    f.name.span,
                    format!("the field `{}` is given twice", view.name),
                ));
                continue;
            }
            given.push(view.name.clone());
            let v = match &f.value {
                Some(v) => self.check_expect(v, u),
                // `S { x }`: the local of the field's name.
                None => {
                    let path = ast::Path {
                        segments: vec![ast::PathSegment { ident: f.name.clone(), generics: None }],
                        span: f.name.span,
                    };
                    let e = ast::Expr { kind: ast::ExprKind::Path(path), span: f.name.span };
                    self.check_expect(&e, u)
                }
            };
            let bits = self.packed_bits(v, view.width, view.shift, f.span);
            word = Some(match word {
                None => bits,
                Some(w) => Expr {
                    ty: u,
                    span,
                    kind: ExprKind::Binary(ast::BinOp::BitOr, Box::new(w), Box::new(bits)),
                },
            });
        }
        let lit = |v: u64| Expr { ty: u, span, kind: ExprKind::Lit(Lit::Int(i128::from(v))) };
        let missing: Vec<&PackedField> =
            views.iter().filter(|v| !given.contains(&v.name)).collect();
        let word = match base {
            Some(b) => {
                // The base's word, with the given fields' bits cleared.
                let b = self.check_expect(b, ty);
                let kept: u64 = missing
                    .iter()
                    .map(|v| (((1u64 << v.width) - 1) << v.shift) & 0xffff_ffff)
                    .fold(0, |a, m| a | m);
                let bw = Expr { ty: u, span: b.span, kind: ExprKind::Field(Box::new(b), 0) };
                let base_bits = Expr {
                    ty: u,
                    span,
                    kind: ExprKind::Binary(ast::BinOp::BitAnd, Box::new(bw), Box::new(lit(kept))),
                };
                match word {
                    None => base_bits,
                    Some(w) => Expr {
                        ty: u,
                        span,
                        kind: ExprKind::Binary(ast::BinOp::BitOr, Box::new(w), Box::new(base_bits)),
                    },
                }
            }
            None => {
                if !missing.is_empty() {
                    let names: Vec<String> =
                        missing.iter().map(|v| format!("`{}`", v.name)).collect();
                    self.err(Diagnostic::new(
                        codes::E0307,
                        span,
                        format!("this `{}` is missing {}", self.p.adt(adt).name, names.join(", ")),
                    ));
                }
                word.unwrap_or_else(|| lit(0))
            }
        };
        let kind = ExprKind::Adt {
            adt,
            args,
            variant: None,
            fields: vec![word],
            order: vec![0],
            base: None,
        };
        Expr { ty, span, kind }
    }

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
        if self.p.packed.contains_key(&adt) {
            return self.packed_lit(adt, args, fields, base, span);
        }
        if self.p.is_lang_adt(adt, Lang::GlobalId) || self.p.is_lang_adt(adt, Lang::LocalId) {
            let name = &self.p.adt(adt).name;
            self.err(
                Diagnostic::new(codes::E0601, path.span, format!("a `{name}` can't be built"))
                    .with_note("each invocation gets its own from the GPU, and `Slots` and workgroup memory's chunks are indexed by it, so each invocation writes only its own (§6.13)")
                    .with_help(format!("take it as a kernel parameter: `id: {name}`")),
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
            if self.field_hidden(adt_mod, &decls[i]) && variant.is_none() {
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
            if self.p.adt(adt).borrow {
                self.borrow_field(&decls[i], &value);
            }
            // A run field takes what passes as a run: an array, a `Vec`, a string.
            if !self.run_coerces(value.ty, fty) {
                self.expect(value.ty, fty, value.span);
            }
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
                        out.push(self.check_default(d, field_tys[i], adt_mod, true));
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
            TyKind::Vec(..) => format!("{}()", self.display(t)),
            _ => return None,
        })
    }

    /// A parameter's or field's default where a call or literal leaves it out. It's checked
    /// as where it's declared, in `module`: its names are that module's, never the caller's.
    /// A constant is a place nothing moves out of (§10), so where the value is `owned` (a
    /// field, a `take` parameter) a constant that isn't `Copy` is cloned.
    pub(crate) fn check_default(
        &mut self,
        d: &ast::Expr,
        ty: TyId,
        module: ModuleId,
        owned: bool,
    ) -> Expr {
        let scope = std::mem::replace(&mut self.scope, resolve::Scope::new(module));
        let env = std::mem::take(&mut self.env);
        let e = self.check_expect(d, ty);
        self.env = env;
        self.scope = scope;
        if owned
            && matches!(e.kind, ExprKind::Const(_))
            && !crate::traits::implements_builtin(self.p, ty, Lang::Copy)
        {
            let span = e.span;
            return self.clone_of(e, ty, span);
        }
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
        let (else_e, ty) =
            self.check_else(&t, else_, expected, span, "if", "the condition is false");
        let else_ = else_e.map(Box::new);
        Expr { ty, span, kind: ExprKind::If { cond: Box::new(c), then: t, else_ } }
    }

    /// The `else` of an `if` or an `if let` (`what`), after its branch `t`: the checked `else`,
    /// and the type of the whole. With no `else`, the branch can't have a value: `when` says
    /// when the missing `else` would run.
    fn check_else(
        &mut self,
        t: &Block,
        else_: Option<&ast::Expr>,
        expected: Option<TyId>,
        span: Span,
        what: &str,
        when: &str,
    ) -> (Option<Expr>, TyId) {
        let unit = self.p.types.unit;
        let Some(e) = else_ else {
            if let Some(tail) = &t.tail
                && !matches!(self.kind(tail.ty), TyKind::Never | TyKind::Error | TyKind::Tuple(_))
                && expected.is_some_and(|e| self.shallow(e) != unit)
            {
                let shown = self.display(tail.ty);
                self.err(
                    Diagnostic::new(codes::E0300, span, format!("this `{what}` has no `else`, so it has no value when {when}, but its branch is a `{shown}`"))
                        .with_help("add an `else` branch"),
                );
            }
            return (None, unit);
        };
        let ex = self.check_expr(e, expected.or(Some(t.ty)));
        if matches!(self.kind(t.ty), TyKind::Never) {
            let ty = ex.ty;
            return (Some(ex), ty);
        }
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
                format!("the branches of this `{what}` have different types: `{a}` and `{b}`"),
            )
            .with_secondary(then_at, format!("this is `{a}`"));
            self.err(self.branch_mismatch(d, t.ty, ex.ty));
        }
        (Some(ex), t.ty)
    }

    /// `f"..."`: a `String` built a part at a time: each text with `push_str`, and each
    /// `{value:spec}` with std's `Format` (§4).
    fn check_fstring(&mut self, parts: &[ast::FPart], span: Span) -> Expr {
        let p = self.p;
        let (Some(string), Some(format), Some(spec_adt)) =
            (p.lang_adt(Lang::String), p.lang_trait(Lang::Format), p.lang_adt(Lang::FormatSpec))
        else {
            return self.error_expr(span);
        };
        let method =
            |name: &str| {
                p.inherent_impls.get(&string).into_iter().flatten().find_map(|&i| {
                    p.impl_(i).methods.iter().copied().find(|&f| p.func(f).name == name)
                })
            };
        let (Some(new_fn), Some(push_fn), Some(format_fn)) =
            (method("new"), method("push_str"), crate::traits::trait_method(p, format, "format"))
        else {
            return self.error_expr(span);
        };
        let string_ty = p.types.adt(string, Vec::new());
        let unit = p.types.unit;
        let s = self.declare_unnamed(string_ty, LocalKind::Owned { mutable: true }, span);
        self.locals[s.index()].name = "f-string".into();
        let local = |ty| Expr { ty, span, kind: ExprKind::Local(s) };
        let mut_s =
            Expr { ty: string_ty, span, kind: ExprKind::MutArg(Box::new(local(string_ty))) };
        let call = |callee, args: Vec<Expr>, modes: Vec<Mode>, receiver, ty| {
            let n = args.len();
            Expr {
                ty,
                span,
                kind: ExprKind::Call(Call {
                    callee,
                    args,
                    modes,
                    order: (0..n).collect(),
                    receiver,
                    ret_mode: RetMode::Owned,
                }),
            }
        };
        let init = call(
            Callee::Fn { func: new_fn, args: Vec::new() },
            Vec::new(),
            Vec::new(),
            false,
            string_ty,
        );
        let mut stmts = vec![Stmt {
            kind: StmtKind::Bind {
                pat: Pat { ty: string_ty, kind: PatKind::Bind(s), span },
                init,
                else_: None,
            },
            span,
        }];
        for part in parts {
            match part {
                ast::FPart::Text(t) if t.is_empty() => {}
                ast::FPart::Text(t) => {
                    let text = self.text_expr(t.clone(), span);
                    let e = call(
                        Callee::Fn { func: push_fn, args: Vec::new() },
                        vec![mut_s.clone(), text],
                        vec![Mode::Mut, Mode::Borrow],
                        true,
                        unit,
                    );
                    stmts.push(Stmt { kind: StmtKind::Expr(e), span });
                }
                ast::FPart::Hole { expr, spec, span: hole } => {
                    let x = self.check_expr(expr, None);
                    let r = TraitRef { trait_: format, args: Vec::new() };
                    let shown = self.display(x.ty);
                    if !self.p.types.has_vars(self.infer.resolve(&self.p.types, x.ty))
                        && !crate::traits::implements(
                            self.p,
                            self.infer.resolve(&self.p.types, x.ty),
                            &r,
                        )
                        && !matches!(self.kind(x.ty), TyKind::Error)
                    {
                        self.err(
                            Diagnostic::new(
                                codes::E0400,
                                x.span,
                                format!("`{shown}` can't go in an f-string: it doesn't implement `Format`"),
                            )
                            .with_note("numbers, `bool`, vectors and text implement it (§4)"),
                        );
                        continue;
                    }
                    self.obligation(x.ty, r, x.span, "`{...}` in an f-string".into());
                    let fields = match parse_spec(spec.as_deref().unwrap_or("")) {
                        Ok(f) => f,
                        Err(why) => {
                            self.err(
                                Diagnostic::new(codes::E0217, *hole, format!("this format spec isn't valid: {why}"))
                                    .with_note("a spec is `[<^>][0][width][.precision][x]`, as in `{x:>8.2}` (§4)"),
                            );
                            continue;
                        }
                    };
                    let spec_ty = p.types.adt(spec_adt, Vec::new());
                    let u32_lit = |v: u32| Expr {
                        ty: p.types.u32,
                        span: *hole,
                        kind: ExprKind::Lit(Lit::Int(i128::from(v))),
                    };
                    let bool_lit = |b: bool| Expr {
                        ty: p.types.bool,
                        span: *hole,
                        kind: ExprKind::Lit(Lit::Bool(b)),
                    };
                    let spec_e = Expr {
                        ty: spec_ty,
                        span: *hole,
                        kind: ExprKind::Adt {
                            adt: spec_adt,
                            args: Vec::new(),
                            variant: None,
                            fields: vec![
                                u32_lit(fields.width),
                                u32_lit(fields.precision.unwrap_or(0)),
                                bool_lit(fields.precision.is_some()),
                                u32_lit(fields.align),
                                bool_lit(fields.zero),
                                bool_lit(fields.hex),
                            ],
                            order: (0..6).collect(),
                            base: None,
                        },
                    };
                    let callee = Callee::TraitMethod {
                        method: format_fn,
                        self_ty: x.ty,
                        trait_args: Vec::new(),
                        method_args: Vec::new(),
                    };
                    let e = call(
                        callee,
                        vec![x, mut_s.clone(), spec_e],
                        vec![Mode::Borrow, Mode::Mut, Mode::Borrow],
                        true,
                        unit,
                    );
                    stmts.push(Stmt { kind: StmtKind::Expr(e), span: *hole });
                }
            }
        }
        let tail = Expr { ty: string_ty, span, kind: ExprKind::Take(Box::new(local(string_ty))) };
        Expr {
            ty: string_ty,
            span,
            kind: ExprKind::Block(Block { stmts, tail: Some(Box::new(tail)), ty: string_ty, span }),
        }
    }

    /// `if let pat = cond { then } else { else_ }`: a `match` with the pattern's arm and a
    /// `_` arm.
    fn check_if_let(
        &mut self,
        pat: &ast::Pat,
        scrutinee: &ast::Expr,
        then: &ast::Block,
        else_: Option<&ast::Expr>,
        expected: Option<TyId>,
        span: Span,
    ) -> Expr {
        let s = self.check_expr(scrutinee, None);
        let env_len = self.env.len();
        let p = self.check_pat(pat, s.ty, s.is_place());
        self.bound_once(&p);
        let t = self.check_block(then, expected);
        self.env.truncate(env_len);
        let (else_e, ty) =
            self.check_else(&t, else_, expected, span, "if let", "the pattern doesn't match");
        let unit = self.p.types.unit;
        let else_e = else_e.unwrap_or_else(|| {
            let empty = Block { stmts: Vec::new(), tail: None, ty: unit, span };
            Expr { ty: unit, span, kind: ExprKind::Block(empty) }
        });
        let then_e = Expr { ty: t.ty, span: t.span, kind: ExprKind::Block(t) };
        let wild = Pat { ty: s.ty, kind: PatKind::Wild, span };
        let arms = vec![
            Arm { pat: p, guard: None, body: then_e, span: pat.span },
            Arm { pat: wild, guard: None, body: else_e, span },
        ];
        Expr { ty, span, kind: ExprKind::Match { scrutinee: Box::new(s), arms, mutable: false } }
    }

    /// `x?` (§15): `x`'s value if it's `Some` or `Ok`; otherwise the function returns `None`, or
    /// the `Err`. A `match` with two arms.
    fn check_try(&mut self, inner: &ast::Expr, span: Span) -> Expr {
        let x = self.check_expr(inner, None);
        let (Some(option), Some(result)) =
            (self.p.lang_adt(Lang::Option), self.p.lang_adt(Lang::Result))
        else {
            return self.error_expr(span);
        };
        let target = self.closure_ret().unwrap_or(self.ret_ty);
        let (adt, args) = match self.kind(x.ty) {
            TyKind::Adt(a, args) if *a == option || *a == result => (*a, args.clone()),
            TyKind::Error => return self.error_expr(span),
            _ => {
                let shown = self.display(x.ty);
                self.err(
                    Diagnostic::new(
                        codes::E0300,
                        x.span,
                        format!("`?` takes an `Option` or a `Result`, not `{shown}`"),
                    )
                    .with_note("`x?` is `x`'s value, or a return of its `None` or `Err` (§15)"),
                );
                return self.error_expr(span);
            }
        };
        let is_result = adt == result;
        // The function returns the same kind, with the same error type.
        let ret_args: Vec<TyId> = if is_result {
            vec![self.new_var(VarKind::General, span), args[1]]
        } else {
            vec![self.new_var(VarKind::General, span)]
        };
        let ret_ty = self.p.types.adt(adt, ret_args.clone());
        if !matches!(self.kind(target), TyKind::Error) && !self.try_unify(target, ret_ty) {
            let (want, has) = (self.display(ret_ty), self.display(target));
            let what = if is_result { "a `Result` with this error type" } else { "an `Option`" };
            let mut d = Diagnostic::new(
                codes::E0300,
                span,
                format!(
                    "`?` returns its {} from the function, which returns `{has}`",
                    if is_result { "`Err`" } else { "`None`" }
                ),
            )
            .with_note(format!("so the function must return {what}: `{want}`"));
            if is_result
                && let TyKind::Adt(a, targs) = self.kind(target)
                && *a == result
            {
                let theirs = self.display(targs[1]);
                let ours = self.display(args[1]);
                d = d.with_help(format!(
                    "convert the error first: `x.map_err(..)?` turns a `{ours}` into a `{theirs}`"
                ));
            }
            self.err(d);
        }
        let value_ty = args[0];
        let from_place = x.is_place();
        let declare = |c: &mut Self, name: &str, ty: TyId| {
            let kind =
                super::pat::binding_kind_of(c.p, c.infer.resolve(&c.p.types, ty), from_place);
            let id = c.declare_unnamed(ty, kind, x.span);
            c.locals[id.index()].name = name.into();
            id
        };
        let v = declare(self, "value", value_ty);
        // `Option`: `None`, `Some(T)`; `Result`: `Ok(T)`, `Err(E)`.
        let (ok_variant, err_variant) = if is_result { (0u32, 1u32) } else { (1, 0) };
        let ok_pat = Pat {
            ty: x.ty,
            kind: PatKind::Adt {
                adt,
                args: args.clone(),
                variant: Some(ok_variant),
                fields: vec![(0, Pat { ty: value_ty, kind: PatKind::Bind(v), span: x.span })],
            },
            span: x.span,
        };
        // A temporary's value is owned by its bindings, and moved on from them.
        let owned = |e: Expr| {
            if from_place {
                e
            } else {
                Expr { ty: e.ty, span: e.span, kind: ExprKind::Take(Box::new(e)) }
            }
        };
        let ok_body = owned(Expr { ty: value_ty, span: x.span, kind: ExprKind::Local(v) });
        let (err_fields, ret_fields) = if is_result {
            let e = declare(self, "error", args[1]);
            let pat = vec![(0, Pat { ty: args[1], kind: PatKind::Bind(e), span: x.span })];
            let val = vec![owned(Expr { ty: args[1], span: x.span, kind: ExprKind::Local(e) })];
            (pat, val)
        } else {
            (Vec::new(), Vec::new())
        };
        let err_pat = Pat {
            ty: x.ty,
            kind: PatKind::Adt {
                adt,
                args: args.clone(),
                variant: Some(err_variant),
                fields: err_fields,
            },
            span: x.span,
        };
        let order = (0..ret_fields.len() as u32).collect();
        let ret_value = Expr {
            ty: ret_ty,
            span,
            kind: ExprKind::Adt {
                adt,
                args: ret_args,
                variant: Some(err_variant),
                fields: ret_fields,
                order,
                base: None,
            },
        };
        let never = self.p.types.never;
        let ret = Expr { ty: never, span, kind: ExprKind::Return(Some(Box::new(ret_value))) };
        let arms = vec![
            Arm { pat: ok_pat, guard: None, body: ok_body, span },
            Arm { pat: err_pat, guard: None, body: ret, span },
        ];
        Expr {
            ty: value_ty,
            span,
            kind: ExprKind::Match { scrutinee: Box::new(x), arms, mutable: false },
        }
    }

    fn check_return(&mut self, value: Option<&ast::Expr>, span: Span) -> Expr {
        let target = self.closure_ret().unwrap_or(self.ret_ty);
        let v = match value {
            Some(v) if matches!(self.kind(target), TyKind::Slice(_) | TyKind::Str) => {
                // A run result: an array, a `Vec` or a string passes as one.
                let e = self.check_expr(v, None);
                if !self.run_coerces(e.ty, target) {
                    self.expect(e.ty, target, e.span);
                }
                Some(Box::new(e))
            }
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
            Some(TyKind::FnPtr(ps, r, flags)) if ps.len() == params.len() => {
                Some((ps.clone(), *r, *flags))
            }
            Some(TyKind::FnPtr(ps, _, _)) => {
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
                Some((vec![e; params.len()], e, FnFlags::default()))
            }
            _ => None,
        };
        let ret_ty = match ret {
            Some(t) => self.resolve_type(t),
            None => match &exp {
                Some((_, r, _)) => *r,
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
                    if let Some((ps, _, _)) = &exp {
                        self.expect(ps[i], t, p.name.span);
                    }
                    t
                }
                None => match &exp {
                    Some((ps, _, _)) => ps[i],
                    None => self.new_var(VarKind::General, p.name.span),
                },
            };
            // A closure's parameters take their modes from the function type it's passed as
            // (§6.7): `fn(mut Ui)` makes `|u| ...`'s `u` a mutable projection.
            let mode = exp.as_ref().map_or(Mode::Borrow, |(_, _, f)| f.mode(i));
            // `_` names nothing, so it can be every unused parameter.
            if let Some(first) =
                params[..i].iter().find(|q| q.name.name == p.name.name && p.name.name != "_")
            {
                self.err(
                    Diagnostic::new(
                        codes::E0201,
                        p.name.span,
                        format!("`{}` is a parameter of this closure twice", p.name.name),
                    )
                    .with_secondary(first.name.span, "first here"),
                );
            }
            locals.push(self.declare_closure_param(&p.name, ty, mode));
        }
        let b = self.without_loops(|this| this.check_expect(body, ret_ty));
        self.env.truncate(env_len);
        let id2 = self.end_closure(locals, ret_ty, b, span);
        debug_assert_eq!(id, id2);
        let env = self.scope_args();
        let ty = self.p.types.intern(TyKind::Closure(ClosureRef { owner: self.fn_id, id }, env));
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

/// How one side of `a + b` goes in an f-string: a string literal's text (its braces doubled),
/// or `{name}` for a name or a field. `None` for anything else.
fn f_part(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        ast::ExprKind::Lit(l) if l.kind == ast::LitKind::Str && !l.text.starts_with('f') => {
            let inner = l.text.strip_prefix('"')?.strip_suffix('"')?;
            Some(inner.replace('{', "{{").replace('}', "}}"))
        }
        _ => Some(format!("{{{}}}", path_text(e)?)),
    }
}

/// A name or a chain of fields, as written: `p.name`.
fn path_text(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        ast::ExprKind::Path(p) if p.segments.iter().all(|s| s.generics.is_none()) => {
            Some(p.segments.iter().map(|s| s.ident.name.as_str()).collect::<Vec<_>>().join("::"))
        }
        ast::ExprKind::Field { base, name: ast::FieldName::Ident(n) } => {
            Some(format!("{}.{}", path_text(base)?, n.name))
        }
        _ => None,
    }
}

/// Two decimal numbers' product, exactly, as decimal text (`15` and `0.01` give `15e-2`), for
/// a unit suffix's fold: `None` if either isn't a plain decimal (digits, `_`, one `.`, an
/// exponent) or the product's digits don't fit.
fn decimal_product(a: &str, b: &str) -> Option<String> {
    let (ma, ea) = decimal(a)?;
    let (mb, eb) = decimal(b)?;
    Some(format!("{}e{}", ma.checked_mul(mb)?, ea.checked_add(eb)?))
}

/// A decimal's digits and power of ten: `1.25e3` is (125, 1).
fn decimal(text: &str) -> Option<(u128, i32)> {
    let clean: String = text.chars().filter(|c| *c != '_').collect();
    let (body, exp) = match clean.split_once(['e', 'E']) {
        Some((body, exp)) => (body, exp.parse::<i32>().ok()?),
        None => (clean.as_str(), 0),
    };
    let (whole, frac) = body.split_once('.').unwrap_or((body, ""));
    if whole.is_empty() && frac.is_empty() {
        return None;
    }
    let mut digits: u128 = 0;
    for c in whole.chars().chain(frac.chars()) {
        digits = digits.checked_mul(10)?.checked_add(u128::from(c.to_digit(10)?))?;
    }
    Some((digits, exp.checked_sub(i32::try_from(frac.len()).ok()?)?))
}
