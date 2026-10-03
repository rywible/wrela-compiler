//! Patterns: `let` destructuring and `match`, with exhaustiveness and unreachable arms.

use super::Checker;
use crate::defs::*;
use crate::program::Program;
use crate::resolve;
use crate::thir::*;
use crate::ty::*;
use std::borrow::Cow;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_syntax::ast;

impl<'p> Checker<'p> {
    /// Declares a pattern binding of type `ty`. A `Copy` value is copied; anything else
    /// projects into the scrutinee when it's a place, and owns it when it's a temporary. While
    /// `ty` isn't known, that's decided once it is ([`Checker::binding_kinds`]).
    fn declare_binding(&mut self, name: &ast::Ident, ty: TyId, from_place: bool) -> LocalId {
        // Through any inference variables bound so far: `[In { .. }; 2]`'s elements are `Copy`
        // even before the array's type is written anywhere.
        let resolved = self.infer.resolve(&self.p.types, ty);
        let kind = if self.p.types.has_vars(resolved) {
            None
        } else {
            Some(binding_kind(self.p, resolved, from_place))
        };
        let owned = LocalKind::Owned { mutable: false };
        let id = self.declare_local(&name.name, ty, kind.unwrap_or(owned), name.span);
        if kind.is_none() {
            self.pending_bindings.push((id, from_place));
        }
        id
    }

    /// Decides the kinds of the pattern bindings whose types weren't known when they were
    /// declared; `locals` have their types resolved.
    pub(crate) fn binding_kinds(&mut self) {
        for (id, from_place) in std::mem::take(&mut self.pending_bindings) {
            let ty = self.infer.resolve(&self.p.types, self.locals[id.index()].ty);
            self.locals[id.index()].kind = binding_kind(self.p, ty, from_place);
        }
    }

    pub(crate) fn check_let_pattern(&mut self, pat: &ast::Pat, ty: TyId, init: &Expr) -> Pat {
        let from_place = init.is_place();
        let p = self.check_pat(pat, ty, from_place);
        if !self.irrefutable(&p) {
            self.err(
                Diagnostic::new(
                    codes::E0309,
                    pat.span,
                    "this pattern might not match, so `let` can't use it",
                )
                .with_help("use `match` to handle the other cases"),
            );
        }
        p
    }

    fn irrefutable(&self, p: &Pat) -> bool {
        match &p.kind {
            PatKind::Wild | PatKind::Bind(_) => true,
            PatKind::Lit(_) => false,
            PatKind::Tuple(ps) => ps.iter().all(|x| self.irrefutable(x)),
            PatKind::Adt { adt, variant, fields, .. } => {
                (variant.is_none() || self.p.adt(*adt).variants().len() == 1)
                    && fields.iter().all(|(_, x)| self.irrefutable(x))
            }
            PatKind::Or(ps) => ps.iter().any(|x| self.irrefutable(x)),
        }
    }

    pub(crate) fn check_pat(&mut self, pat: &ast::Pat, ty: TyId, from_place: bool) -> Pat {
        let span = pat.span;
        let kind = match &pat.kind {
            ast::PatKind::Wild => PatKind::Wild,
            // Its syntax error is reported; it matches anything, so the match isn't reported as
            // missing what it was meant to cover.
            ast::PatKind::Error => {
                self.saw_syntax_error = true;
                PatKind::Wild
            }
            ast::PatKind::Ident(name) => {
                // A unit variant or constant in scope is matched, not bound (`None`, `MAX`).
                match resolve::lookup_name(self.p, self.scope.module, &name.name) {
                    Some(Res::Variant(a, v))
                        if self.p.adt(a).variants()[v as usize].shape == VariantShape::Unit =>
                    {
                        return self.variant_pat(a, v, span, ty);
                    }
                    Some(Res::Const(c)) => {
                        self.const_pat(c, ty, name.span).map_or(PatKind::Wild, PatKind::Lit)
                    }
                    _ => PatKind::Bind(self.declare_binding(name, ty, from_place)),
                }
            }
            ast::PatKind::Lit { neg, lit } => {
                self.check_lit_pattern(*neg, lit, ty).map_or(PatKind::Wild, PatKind::Lit)
            }
            ast::PatKind::Tuple(ps) => {
                let tys = match self.kind(ty) {
                    TyKind::Tuple(ts) if ts.len() == ps.len() => ts.clone(),
                    TyKind::Var(_) => {
                        let ts: Vec<TyId> =
                            ps.iter().map(|p| self.new_var(VarKind::General, p.span)).collect();
                        let tt = self.p.types.tuple(ts.clone());
                        self.expect(tt, ty, span);
                        ts
                    }
                    _ => {
                        let shown = self.display(ty);
                        self.err(Diagnostic::new(
                            codes::E0320,
                            span,
                            format!("a tuple of {} doesn't match `{shown}`", ps.len()),
                        ));
                        return self.unmatched(pat, ty);
                    }
                };
                PatKind::Tuple(
                    ps.iter().zip(tys).map(|(p, t)| self.check_pat(p, t, from_place)).collect(),
                )
            }
            ast::PatKind::Path(path)
            | ast::PatKind::TupleStruct(path, _)
            | ast::PatKind::Struct { path, .. } => {
                let module = self.scope.module;
                let Some(res) =
                    resolve::lookup_or_report(self.p, &mut self.diags, module, &path.segments)
                else {
                    return self.unmatched(pat, ty);
                };
                match (&pat.kind, res) {
                    (ast::PatKind::Path(_), Res::Variant(a, v)) => {
                        return self.variant_pat(a, v, span, ty);
                    }
                    (ast::PatKind::TupleStruct(_, subs), Res::Variant(a, v)) => {
                        return self.tuple_variant_pat(a, v, subs, span, ty, from_place);
                    }
                    (ast::PatKind::Struct { fields, rest, .. }, Res::Variant(a, v)) => {
                        return self.struct_pat(a, Some(v), fields, *rest, span, ty, from_place);
                    }
                    (ast::PatKind::Struct { fields, rest, .. }, Res::Adt(a))
                        if !self.p.adt(a).is_enum() =>
                    {
                        return self.struct_pat(a, None, fields, *rest, span, ty, from_place);
                    }
                    _ => {
                        self.err(Diagnostic::new(
                            codes::E0320,
                            span,
                            format!("`{}` can't be matched here", path.last().name),
                        ));
                        return self.unmatched(pat, ty);
                    }
                }
            }
        };
        Pat { ty, kind, span }
    }

    /// A pattern that can't match (its error is reported): it matches anything, and the names
    /// it binds are declared, with the error type, so their uses aren't reported too.
    fn unmatched(&mut self, pat: &ast::Pat, ty: TyId) -> Pat {
        self.declare_unmatched(pat);
        Pat { ty, kind: PatKind::Wild, span: pat.span }
    }

    fn declare_unmatched(&mut self, pat: &ast::Pat) {
        let error = self.p.types.error;
        let owned = LocalKind::Owned { mutable: false };
        match &pat.kind {
            ast::PatKind::Ident(name) => {
                if !matches!(
                    resolve::lookup_name(self.p, self.scope.module, &name.name),
                    Some(Res::Variant(..) | Res::Const(_))
                ) {
                    self.declare_local(&name.name, error, owned, name.span);
                }
            }
            ast::PatKind::TupleStruct(_, ps) | ast::PatKind::Tuple(ps) => {
                for p in ps {
                    self.declare_unmatched(p);
                }
            }
            ast::PatKind::Struct { fields, .. } => {
                for f in fields {
                    self.declare_unmatched_field(f);
                }
            }
            ast::PatKind::Wild
            | ast::PatKind::Lit { .. }
            | ast::PatKind::Path(_)
            | ast::PatKind::Error => {}
        }
    }

    fn declare_unmatched_field(&mut self, f: &ast::FieldPat) {
        match &f.pat {
            Some(p) => self.declare_unmatched(p),
            None => {
                let owned = LocalKind::Owned { mutable: false };
                self.declare_local(&f.name.name, self.p.types.error, owned, f.name.span);
            }
        }
    }

    fn unmatched_subs(&mut self, subs: &[ast::Pat], ty: TyId, span: Span) -> Pat {
        for p in subs {
            self.declare_unmatched(p);
        }
        Pat { ty, kind: PatKind::Wild, span }
    }

    /// A constant used as a pattern: its value, which must be a number or a `bool`.
    fn const_pat(&mut self, c: ConstId, ty: TyId, span: Span) -> Option<Lit> {
        let ct = self.const_ty(c);
        self.expect(ct, ty, span);
        let p = self.p;
        let not_a_number = || {
            let name = &p.const_(c).name;
            let msg = format!("`{name}` can't be a pattern: only a number or `bool` constant can");
            Diagnostic::new(codes::E0320, span, msg)
        };
        // Follow constants naming constants (cycles are reported where they're defined).
        let mut seen = vec![c];
        let mut cur = c;
        let (neg, lit) = loop {
            let def = self.p.const_(cur);
            let mut e = &def.value;
            let mut neg = false;
            loop {
                match &e.kind {
                    ast::ExprKind::Paren(x) => e = x,
                    ast::ExprKind::Unary(ast::UnOp::Neg, x) => {
                        neg = !neg;
                        e = x;
                    }
                    _ => break,
                }
            }
            match &e.kind {
                ast::ExprKind::Lit(l) => break (neg, l.clone()),
                ast::ExprKind::Path(path) if !neg => {
                    match resolve::resolve_value_item(self.p, def.module, path) {
                        Some(Res::Const(d)) if !seen.contains(&d) => {
                            seen.push(d);
                            cur = d;
                        }
                        // A unit variant (`const NOTHING: Option<i32> = None`) isn't a number.
                        Some(Res::Variant(..)) => {
                            let names: Vec<&str> =
                                path.segments.iter().map(|s| s.ident.name.as_str()).collect();
                            let help = format!("match the variant itself: `{}`", names.join("::"));
                            self.err(not_a_number().with_help(help));
                            return None;
                        }
                        // Its error is reported where the constant is defined.
                        _ => return None,
                    }
                }
                _ => {
                    let help = "match its parts, or compare with `==` in a guard";
                    self.err(not_a_number().with_help(help));
                    return None;
                }
            }
        };
        let resolved = self.infer.resolve(&self.p.types, ty);
        match lit.kind {
            ast::LitKind::Bool(b) => Some(Lit::Bool(b)),
            ast::LitKind::Int(v) if self.p.types.is_float(resolved) => {
                let v = v.ok()? as f64;
                Some(Lit::Float(if neg { -v } else { v }))
            }
            ast::LitKind::Int(v) => {
                let v = v.ok()?;
                Some(Lit::Int(if neg { -i128::from(v) } else { i128::from(v) }))
            }
            ast::LitKind::Float(v) => Some(Lit::Float(if neg { -v } else { v })),
            _ => None,
        }
    }

    fn check_lit_pattern(&mut self, neg: bool, lit: &ast::Lit, ty: TyId) -> Option<Lit> {
        match lit.kind {
            ast::LitKind::Bool(b) => {
                let bt = self.p.types.bool;
                self.expect(bt, ty, lit.span);
                Some(Lit::Bool(b))
            }
            ast::LitKind::Int(v) => {
                let v = match v {
                    ast::IntValue::Ok(v) => v,
                    ast::IntValue::Malformed => return None,
                    ast::IntValue::TooLarge => {
                        self.err(Diagnostic::new(
                            codes::E0006,
                            lit.span,
                            "this integer is too large for any integer type",
                        ));
                        return None;
                    }
                };
                let var = self.new_var(VarKind::Int, lit.span);
                self.expect(var, ty, lit.span);
                self.int_literals.push((ty, v, neg, lit.span));
                if neg {
                    // As for `-x`: an unsigned value is never negative.
                    match self.kind(ty) {
                        TyKind::Int(it) if !it.signed() => {
                            let shown = self.display(ty);
                            self.err(
                                Diagnostic::new(
                                    codes::E0305,
                                    lit.span,
                                    format!("`-` doesn't apply to `{shown}`"),
                                )
                                .with_note("unsigned integers can't be negative"),
                            );
                            return None;
                        }
                        _ => self.negated_ints.push((ty, lit.span)),
                    }
                }
                Some(Lit::Int(if neg { -i128::from(v) } else { i128::from(v) }))
            }
            ast::LitKind::Float(v) => {
                let var = self.new_var(VarKind::Float, lit.span);
                self.expect(var, ty, lit.span);
                self.float_literals.push((ty, v, lit.span));
                Some(Lit::Float(if neg { -v } else { v }))
            }
            _ => {
                self.err(Diagnostic::new(
                    codes::E0320,
                    lit.span,
                    "this literal can't be a pattern",
                ));
                None
            }
        }
    }

    /// The enum's arguments for a pattern of type `ty`.
    fn pat_adt_args(&mut self, a: AdtId, ty: TyId, span: Span) -> Option<Vec<TyId>> {
        let n = self.p.adt(a).generics.len();
        let args: Vec<TyId> = (0..n).map(|_| self.new_var(VarKind::General, span)).collect();
        let at = self.p.types.adt(a, args.clone());
        if self.infer.unify(&self.p.types, at, ty).is_err() {
            let (x, y) = (self.display(at), self.display(ty));
            self.err(Diagnostic::new(
                codes::E0320,
                span,
                format!("a `{x}` pattern can't match a `{y}`"),
            ));
            return None;
        }
        Some(args.iter().map(|&t| self.shallow(t)).collect())
    }

    fn variant_pat(&mut self, a: AdtId, v: u32, span: Span, ty: TyId) -> Pat {
        let variant = &self.p.adt(a).variants()[v as usize];
        let Some(args) = self.pat_adt_args(a, ty, span) else {
            return Pat { ty, kind: PatKind::Wild, span };
        };
        if variant.shape != VariantShape::Unit {
            let how = if variant.shape == VariantShape::Tuple {
                format!("`{}(..)`", variant.name)
            } else {
                format!("`{} {{ .. }}`", variant.name)
            };
            self.err(Diagnostic::new(
                codes::E0320,
                span,
                format!("`{}` holds values; match it with {how}", variant.name),
            ));
        }
        Pat { ty, kind: PatKind::Adt { adt: a, args, variant: Some(v), fields: Vec::new() }, span }
    }

    /// `Variant(p, q, ...)`.
    fn tuple_variant_pat(
        &mut self,
        a: AdtId,
        v: u32,
        subs: &[ast::Pat],
        span: Span,
        ty: TyId,
        from_place: bool,
    ) -> Pat {
        let variant = &self.p.adt(a).variants()[v as usize];
        let Some(args) = self.pat_adt_args(a, ty, span) else {
            return self.unmatched_subs(subs, ty, span);
        };
        if variant.shape != VariantShape::Tuple {
            self.err(Diagnostic::new(
                codes::E0320,
                span,
                format!("`{}` isn't a tuple variant", variant.name),
            ));
            return self.unmatched_subs(subs, ty, span);
        }
        let n = subs.len();
        if n != variant.fields.len() {
            self.err(Diagnostic::new(
                codes::E0320,
                span,
                format!(
                    "`{}` holds {} value{}, but this pattern has {n}",
                    variant.name,
                    variant.fields.len(),
                    wrela_diag::plural(variant.fields.len())
                ),
            ));
            return self.unmatched_subs(subs, ty, span);
        }
        let ftys = self.p.fields_of(a, &args, Some(v));
        let fields = subs
            .iter()
            .zip(ftys)
            .enumerate()
            .map(|(i, (p, t))| (i as u32, self.check_pat(p, t, from_place)))
            .collect();
        Pat { ty, kind: PatKind::Adt { adt: a, args, variant: Some(v), fields }, span }
    }

    #[allow(clippy::too_many_arguments)]
    fn struct_pat(
        &mut self,
        a: AdtId,
        v: Option<u32>,
        fields: &[ast::FieldPat],
        rest: bool,
        span: Span,
        ty: TyId,
        from_place: bool,
    ) -> Pat {
        let Some(args) = self.pat_adt_args(a, ty, span) else {
            for f in fields {
                self.declare_unmatched_field(f);
            }
            return Pat { ty, kind: PatKind::Wild, span };
        };
        let (decls, ftys) = (self.p.adt_fields(a, v), self.p.fields_of(a, &args, v));
        let mut out = Vec::new();
        for f in fields {
            let Some(i) = decls.iter().position(|d| d.name == f.name.name) else {
                self.err(Diagnostic::new(
                    codes::E0206,
                    f.name.span,
                    format!("`{}` has no field `{}`", self.p.adt(a).name, f.name.name),
                ));
                self.declare_unmatched_field(f);
                continue;
            };
            // Exhaustiveness sees one pattern per field.
            if out.iter().any(|(j, _)| *j as usize == i) {
                self.err(Diagnostic::new(
                    codes::E0308,
                    f.name.span,
                    format!("the field `{}` is matched twice", f.name.name),
                ));
                self.declare_unmatched_field(f);
                continue;
            }
            if v.is_none() && !decls[i].public && self.p.adt(a).module != self.scope.module {
                self.err(
                    Diagnostic::new(
                        codes::E0210,
                        f.name.span,
                        format!(
                            "the field `{}` of `{}` is private",
                            f.name.name,
                            self.p.adt(a).name
                        ),
                    )
                    .with_secondary(decls[i].span, "declared here without `pub`")
                    .with_help("leave it out with `..`"),
                );
            }
            let sub = match &f.pat {
                Some(p) => self.check_pat(p, ftys[i], from_place),
                None => {
                    let id = self.declare_binding(&f.name, ftys[i], from_place);
                    Pat { ty: ftys[i], kind: PatKind::Bind(id), span: f.name.span }
                }
            };
            out.push((i as u32, sub));
        }
        if !rest && out.len() < decls.len() {
            let missing: Vec<String> = decls
                .iter()
                .enumerate()
                .filter(|(i, _)| !out.iter().any(|(j, _)| *j as usize == *i))
                .map(|(_, d)| format!("`{}`", d.name))
                .collect();
            self.err(
                Diagnostic::new(
                    codes::E0307,
                    span,
                    format!("this pattern doesn't mention {}", missing.join(", ")),
                )
                .with_help("add them, or end the pattern with `..`"),
            );
        }
        Pat { ty, kind: PatKind::Adt { adt: a, args, variant: v, fields: out }, span }
    }

    // ---- match -----------------------------------------------------------------------------

    pub(crate) fn check_match(
        &mut self,
        scrutinee: &ast::Expr,
        arms: &[ast::Arm],
        expected: Option<TyId>,
        span: Span,
    ) -> Expr {
        let s = self.check_expr(scrutinee, None);
        let from_place = s.is_place();
        // The arms agree with each other; `expected` only guides them, as with `if`, so a
        // match of arrays can still pass as a run.
        let mut result = None;
        let mut out = Vec::new();
        // A pattern with an error (reported) could have been meant to cover anything, or
        // nothing: what the arms cover isn't known.
        let mut bad_pattern = false;
        for arm in arms {
            let env_len = self.env.len();
            let errors = self.diags.len();
            let pats: Vec<Pat> =
                arm.pats.iter().map(|p| self.check_pat(p, s.ty, from_place)).collect();
            bad_pattern |= wrela_diag::has_errors(&self.diags[errors..]);
            if pats.len() > 1 && pats.iter().any(has_binding) {
                self.err(
                    Diagnostic::new(
                        codes::E0320,
                        arm.span,
                        "alternatives with `|` can't bind names",
                    )
                    .with_help("write one arm per alternative"),
                );
            }
            let pat = if pats.len() == 1 {
                pats.into_iter().next().unwrap_or(Pat { ty: s.ty, kind: PatKind::Wild, span })
            } else {
                Pat { ty: s.ty, kind: PatKind::Or(pats), span: arm.span }
            };
            let guard = arm.guard.as_ref().map(|g| {
                let bt = self.p.types.bool;
                let e = self.check_expr(g, Some(bt));
                self.expect_cond(&e);
                e
            });
            let body = self.check_expr(&arm.body, result.or(expected));
            match result {
                Some(r) => {
                    if !matches!(self.kind(body.ty), TyKind::Never)
                        && self.infer.unify(&self.p.types, body.ty, r).is_err()
                    {
                        let (a, b) = (self.display(r), self.display(body.ty));
                        let d = Diagnostic::new(
                            codes::E0300,
                            body.span,
                            format!("this arm is a `{b}`, but the match is a `{a}`"),
                        );
                        self.err(self.branch_mismatch(d, r, body.ty));
                    }
                }
                None => {
                    if !matches!(self.kind(body.ty), TyKind::Never) {
                        result = Some(body.ty);
                    }
                }
            }
            self.env.truncate(env_len);
            out.push(Arm { pat, guard, body, span: arm.span });
        }
        let ty = result.unwrap_or(self.p.types.never);
        // An arm that failed to parse could have covered anything, too.
        let scrutinee_error = matches!(self.kind(s.ty), TyKind::Error);
        if !bad_pattern
            && !scrutinee_error
            && !arms.iter().any(|a| a.pats.iter().any(has_error_pat))
        {
            self.check_exhaustive(&s, &out, span);
        }
        Expr { ty, span, kind: ExprKind::Match { scrutinee: Box::new(s), arms: out } }
    }

    fn check_exhaustive(&mut self, s: &Expr, arms: &[Arm], span: Span) {
        let ty = self.infer.resolve(&self.p.types, s.ty);
        let mut rows: Vec<Vec<DPat>> = Vec::new();
        for arm in arms {
            let d = self.dpat(&arm.pat);
            if !useful(self.p, &rows, std::slice::from_ref(&d), &[ty]) {
                self.err(
                    Diagnostic::new(
                        codes::W0002,
                        arm.pat.span,
                        "this arm can never match: the arms before it cover everything it does",
                    )
                    .with_help("remove it, or move it before the arm that covers it"),
                );
            }
            if arm.guard.is_none() {
                rows.push(vec![d]);
            }
        }
        if useful(self.p, &rows, &[DPat::Wild], &[ty]) {
            let missing = witness(self.p, &rows, ty).unwrap_or_else(|| "_".into());
            self.err(
                Diagnostic::new(
                    codes::E0309,
                    span,
                    format!("this `match` doesn't cover every value: `{missing}` isn't matched"),
                )
                .with_help(format!(
                    "add an arm for `{missing}`, or `_ => ...` for everything else"
                )),
            );
        }
    }

    fn dpat(&self, p: &Pat) -> DPat {
        match &p.kind {
            PatKind::Wild | PatKind::Bind(_) => DPat::Wild,
            PatKind::Lit(Lit::Bool(b)) => DPat::Ctor(Ctor::Variant(u32::from(*b)), Vec::new()),
            PatKind::Lit(l) => DPat::Lit(format!("{l:?}")),
            PatKind::Tuple(ps) => {
                DPat::Ctor(Ctor::Single, ps.iter().map(|x| self.dpat(x)).collect())
            }
            PatKind::Adt { adt, variant, fields, .. } => {
                let n = match variant {
                    Some(v) => self.p.adt(*adt).variants()[*v as usize].fields.len(),
                    None => self.p.adt(*adt).fields().len(),
                };
                let mut subs = vec![DPat::Wild; n];
                for (i, f) in fields {
                    subs[*i as usize] = self.dpat(f);
                }
                let ctor = match variant {
                    Some(v) => Ctor::Variant(*v),
                    None => Ctor::Single,
                };
                DPat::Ctor(ctor, subs)
            }
            PatKind::Or(ps) => DPat::Or(ps.iter().map(|x| self.dpat(x)).collect()),
        }
    }
}

/// How a pattern binding of the known type `ty` holds its value.
fn binding_kind(p: &Program, ty: TyId, from_place: bool) -> LocalKind {
    if from_place && !crate::traits::implements_builtin(p, ty, Lang::Copy) {
        LocalKind::Projection { mutable: false }
    } else {
        LocalKind::Owned { mutable: false }
    }
}

fn has_binding(p: &Pat) -> bool {
    match &p.kind {
        PatKind::Bind(_) => true,
        PatKind::Tuple(ps) | PatKind::Or(ps) => ps.iter().any(has_binding),
        PatKind::Adt { fields, .. } => fields.iter().any(|(_, x)| has_binding(x)),
        _ => false,
    }
}

// ---- usefulness (Maranget, "Warnings for pattern matching", 2007) ------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Ctor {
    /// A struct or tuple: the type's only constructor.
    Single,
    /// An enum variant, or `false`/`true` (0/1).
    Variant(u32),
}

#[derive(Clone, Debug)]
enum DPat {
    Wild,
    Ctor(Ctor, Vec<DPat>),
    /// A number literal: numbers have too many values to cover without a wildcard.
    Lit(String),
    Or(Vec<DPat>),
}

/// The constructors of a type; `None` for infinite types.
fn ctors(p: &Program, ty: TyId) -> Option<Vec<Ctor>> {
    match p.types.kind(ty) {
        TyKind::Bool => Some(vec![Ctor::Variant(0), Ctor::Variant(1)]),
        TyKind::Tuple(_) => Some(vec![Ctor::Single]),
        TyKind::Adt(a, _) if p.adt(*a).is_enum() => {
            Some((0..p.adt(*a).variants().len() as u32).map(Ctor::Variant).collect())
        }
        TyKind::Adt(..) => Some(vec![Ctor::Single]),
        _ => None,
    }
}

/// The field types of constructor `k` of `ty`; none when `ty` has no such constructor.
fn ctor_fields(p: &Program, ty: TyId, k: &Ctor) -> Vec<TyId> {
    match (p.types.kind(ty), k) {
        (TyKind::Tuple(ts), Ctor::Single) => ts.clone(),
        (TyKind::Adt(a, args), Ctor::Single) if !p.adt(*a).is_enum() => p.fields_of(*a, args, None),
        (TyKind::Adt(a, args), Ctor::Variant(v)) if (*v as usize) < p.adt(*a).variants().len() => {
            p.fields_of(*a, args, Some(*v))
        }
        _ => Vec::new(),
    }
}

/// Expands or-patterns in the first column (the rows themselves when there are none).
fn expand(rows: &[Vec<DPat>]) -> Cow<'_, [Vec<DPat>]> {
    if !rows.iter().any(|r| matches!(r.first(), Some(DPat::Or(_)))) {
        return Cow::Borrowed(rows);
    }
    let mut out = Vec::new();
    for r in rows {
        push_expanded(r.clone(), &mut out);
    }
    Cow::Owned(out)
}

/// Pushes `row`, or a row for each alternative of its first column's or-pattern, expanded.
fn push_expanded(row: Vec<DPat>, out: &mut Vec<Vec<DPat>>) {
    match row.first() {
        Some(DPat::Or(alts)) => {
            for a in alts {
                let mut nr = vec![a.clone()];
                nr.extend_from_slice(&row[1..]);
                push_expanded(nr, out);
            }
        }
        _ => out.push(row),
    }
}

/// The rows that match constructor `k` (of `arity`), with its fields spliced in.
fn specialize(rows: &[Vec<DPat>], k: &Ctor, arity: usize) -> Vec<Vec<DPat>> {
    let mut out = Vec::new();
    for r in rows {
        match &r[0] {
            DPat::Wild => {
                let mut nr = vec![DPat::Wild; arity];
                nr.extend_from_slice(&r[1..]);
                out.push(nr);
            }
            // A pattern of another type (its error is reported) matches none of this one's.
            DPat::Ctor(c, subs) if c == k && subs.len() == arity => {
                let mut nr = subs.clone();
                nr.extend_from_slice(&r[1..]);
                out.push(nr);
            }
            _ => {}
        }
    }
    out
}

fn default_rows(rows: &[Vec<DPat>]) -> Vec<Vec<DPat>> {
    rows.iter().filter(|r| matches!(r[0], DPat::Wild)).map(|r| r[1..].to_vec()).collect()
}

/// Whether `row` matches some value no row of `rows` matches.
fn useful(p: &Program, rows: &[Vec<DPat>], row: &[DPat], tys: &[TyId]) -> bool {
    if row.is_empty() {
        return rows.is_empty();
    }
    let rows = expand(rows);
    if let DPat::Or(alts) = &row[0] {
        return alts.iter().any(|a| {
            let mut r = vec![a.clone()];
            r.extend_from_slice(&row[1..]);
            useful(p, &rows, &r, tys)
        });
    }
    let ty = tys[0];
    match &row[0] {
        DPat::Ctor(k, subs) => {
            let arity = subs.len();
            let mut ntys = ctor_fields(p, ty, k);
            ntys.resize(arity, p.types.error);
            ntys.extend_from_slice(&tys[1..]);
            let mut nrow = subs.clone();
            nrow.extend_from_slice(&row[1..]);
            useful(p, &specialize(&rows, k, arity), &nrow, &ntys)
        }
        DPat::Lit(l) => {
            let matching: Vec<Vec<DPat>> = rows
                .iter()
                .filter(|r| matches!(&r[0], DPat::Wild) || matches!(&r[0], DPat::Lit(x) if x == l))
                .map(|r| r[1..].to_vec())
                .collect();
            useful(p, &matching, &row[1..], &tys[1..])
        }
        DPat::Wild => {
            let used: Vec<&Ctor> = rows
                .iter()
                .filter_map(|r| if let DPat::Ctor(k, _) = &r[0] { Some(k) } else { None })
                .collect();
            match ctors(p, ty) {
                Some(all) if all.iter().all(|k| used.contains(&k)) => all.iter().any(|k| {
                    let mut ntys = ctor_fields(p, ty, k);
                    let arity = ntys.len();
                    ntys.extend_from_slice(&tys[1..]);
                    let mut nrow = vec![DPat::Wild; arity];
                    nrow.extend_from_slice(&row[1..]);
                    useful(p, &specialize(&rows, k, arity), &nrow, &ntys)
                }),
                _ => useful(p, &default_rows(&rows), &row[1..], &tys[1..]),
            }
        }
        DPat::Or(_) => unreachable!("or-patterns are expanded above"),
    }
}

/// An example of a value the rows miss, for the message (one level deep).
fn witness(p: &Program, rows: &[Vec<DPat>], ty: TyId) -> Option<String> {
    let rows = expand(rows);
    for k in ctors(p, ty)? {
        let tys = ctor_fields(p, ty, &k);
        let arity = tys.len();
        let spec = specialize(&rows, &k, arity);
        if useful(p, &spec, &vec![DPat::Wild; arity], &tys) {
            let text = match (&k, p.types.kind(ty)) {
                (Ctor::Variant(v), TyKind::Bool) => {
                    (if *v == 1 { "true" } else { "false" }).to_string()
                }
                (Ctor::Variant(v), TyKind::Adt(a, _)) => {
                    let var = &p.adt(*a).variants()[*v as usize];
                    match var.shape {
                        VariantShape::Unit => var.name.clone(),
                        VariantShape::Tuple => {
                            format!("{}({})", var.name, vec!["_"; arity].join(", "))
                        }
                        VariantShape::Struct => format!("{} {{ .. }}", var.name),
                    }
                }
                _ => "_".into(),
            };
            return Some(text);
        }
    }
    None
}

/// Whether a pattern holds a syntax error's node.
fn has_error_pat(p: &ast::Pat) -> bool {
    match &p.kind {
        ast::PatKind::Error => true,
        ast::PatKind::TupleStruct(_, ps) | ast::PatKind::Tuple(ps) => ps.iter().any(has_error_pat),
        ast::PatKind::Struct { fields, .. } => {
            fields.iter().any(|f| f.pat.as_ref().is_some_and(has_error_pat))
        }
        ast::PatKind::Wild
        | ast::PatKind::Ident(_)
        | ast::PatKind::Lit { .. }
        | ast::PatKind::Path(_) => false,
    }
}
