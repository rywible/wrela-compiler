//! Patterns: `let` destructuring and `match`, with exhaustiveness and unreachable arms.

use super::Checker;
use crate::defs::*;
use crate::resolve::{self, PathLookup};
use crate::thir::*;
use crate::ty::*;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_syntax::ast;

impl<'p> Checker<'p> {
    /// How a pattern binding of type `ty` holds its value: a `Copy` value is copied; anything
    /// else projects into the scrutinee when it's a place, and owns it when it's a temporary.
    fn binding_kind(&mut self, ty: TyId, from_place: bool) -> LocalKind {
        if crate::traits::implements_builtin(self.p, ty, Lang::Copy)
            || matches!(self.kind(ty), TyKind::Var(_))
        {
            LocalKind::Owned { mutable: false }
        } else if from_place {
            LocalKind::Projection { mutable: false }
        } else {
            LocalKind::Owned { mutable: false }
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
            ast::PatKind::Ident(name) => {
                // A unit variant or constant in scope is matched, not bound (`None`).
                match resolve::lookup_name(self.p, self.scope.module, &name.name) {
                    Some(Res::Variant(a, v))
                        if self.p.adt(a).variants()[v as usize].shape == VariantShape::Unit =>
                    {
                        return self.variant_pat(a, v, &[], span, ty);
                    }
                    _ => {
                        let kind = self.binding_kind(ty, from_place);
                        let id = self.declare_pattern_local(&name.name, ty, kind, name.span);
                        PatKind::Bind(id)
                    }
                }
            }
            ast::PatKind::Lit { neg, lit } => {
                let e = self.check_lit_pattern(*neg, lit, ty);
                match e {
                    Some(l) => PatKind::Lit(l),
                    None => PatKind::Wild,
                }
            }
            ast::PatKind::Tuple(ps) => {
                let tys = match self.kind(ty) {
                    TyKind::Tuple(ts) if ts.len() == ps.len() => ts,
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
                        return Pat { ty, kind: PatKind::Wild, span };
                    }
                };
                PatKind::Tuple(
                    ps.iter().zip(tys).map(|(p, t)| self.check_pat(p, t, from_place)).collect(),
                )
            }
            ast::PatKind::Path(path)
            | ast::PatKind::TupleStruct(path, _)
            | ast::PatKind::Struct { path, .. } => {
                let idents: Vec<ast::Ident> =
                    path.segments.iter().map(|s| s.ident.clone()).collect();
                let res =
                    match resolve::resolve_module_path_in(self.p, self.scope.module, &idents, true)
                    {
                        PathLookup::Found(r) => r,
                        PathLookup::Error(d) => {
                            self.err(*d);
                            return Pat { ty, kind: PatKind::Wild, span };
                        }
                        PathLookup::NotYet => return Pat { ty, kind: PatKind::Wild, span },
                    };
                match (&pat.kind, res) {
                    (ast::PatKind::Path(_), Res::Variant(a, v)) => {
                        return self.variant_pat(a, v, &[], span, ty);
                    }
                    (ast::PatKind::TupleStruct(_, subs), Res::Variant(a, v)) => {
                        let subs: Vec<(u32, &ast::Pat)> =
                            subs.iter().enumerate().map(|(i, p)| (i as u32, p)).collect();
                        return self.variant_pat_checked(
                            a,
                            v,
                            &subs,
                            subs.len(),
                            span,
                            ty,
                            from_place,
                            VariantShape::Tuple,
                        );
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
                        PatKind::Wild
                    }
                }
            }
        };
        Pat { ty, kind, span }
    }

    fn declare_pattern_local(
        &mut self,
        name: &str,
        ty: TyId,
        kind: LocalKind,
        span: Span,
    ) -> LocalId {
        self.declare_local(name, ty, kind, span)
    }

    fn check_lit_pattern(&mut self, neg: bool, lit: &ast::Lit, ty: TyId) -> Option<Lit> {
        match lit.kind {
            ast::LitKind::Bool(b) => {
                let bt = self.p.types.bool;
                self.expect(bt, ty, lit.span);
                Some(Lit::Bool(b))
            }
            ast::LitKind::Int => {
                let v = wrela_syntax::lexer::int_value(&lit.text)?;
                let var = self.new_var(VarKind::Int, lit.span);
                self.expect(var, ty, lit.span);
                self.int_literals.push((ty, v, neg, lit.span));
                if neg {
                    // Matched as the negated value; stored as two's complement of u64.
                    Some(Lit::Int(v.wrapping_neg()))
                } else {
                    Some(Lit::Int(v))
                }
            }
            ast::LitKind::Float => {
                let v = wrela_syntax::lexer::float_value(&lit.text);
                let var = self.new_var(VarKind::Float, lit.span);
                self.expect(var, ty, lit.span);
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

    fn variant_pat(&mut self, a: AdtId, v: u32, _subs: &[ast::Pat], span: Span, ty: TyId) -> Pat {
        let variant = self.p.adt(a).variants()[v as usize].clone();
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

    #[allow(clippy::too_many_arguments)]
    fn variant_pat_checked(
        &mut self,
        a: AdtId,
        v: u32,
        subs: &[(u32, &ast::Pat)],
        n: usize,
        span: Span,
        ty: TyId,
        from_place: bool,
        shape: VariantShape,
    ) -> Pat {
        let variant = self.p.adt(a).variants()[v as usize].clone();
        let Some(args) = self.pat_adt_args(a, ty, span) else {
            return Pat { ty, kind: PatKind::Wild, span };
        };
        if variant.shape != shape {
            self.err(Diagnostic::new(
                codes::E0320,
                span,
                format!("`{}` isn't a tuple variant", variant.name),
            ));
            return Pat { ty, kind: PatKind::Wild, span };
        }
        if n != variant.fields.len() {
            self.err(Diagnostic::new(
                codes::E0320,
                span,
                format!(
                    "`{}` holds {} value{}, but this pattern has {n}",
                    variant.name,
                    variant.fields.len(),
                    if variant.fields.len() == 1 { "" } else { "s" }
                ),
            ));
            return Pat { ty, kind: PatKind::Wild, span };
        }
        let ftys = self.p.variant_fields(a, &args, v as usize);
        let fields = subs
            .iter()
            .map(|(i, p)| (*i, self.check_pat(p, ftys[*i as usize].1, from_place)))
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
            return Pat { ty, kind: PatKind::Wild, span };
        };
        let (decls, ftys) = match v {
            Some(v) => (
                self.p.adt(a).variants()[v as usize].fields.clone(),
                self.p.variant_fields(a, &args, v as usize),
            ),
            None => (self.p.adt(a).fields().to_vec(), self.p.struct_fields(a, &args)),
        };
        let mut out = Vec::new();
        for f in fields {
            let Some(i) = decls.iter().position(|d| d.name == f.name.name) else {
                self.err(Diagnostic::new(
                    codes::E0206,
                    f.name.span,
                    format!("`{}` has no field `{}`", self.p.adt(a).name, f.name.name),
                ));
                continue;
            };
            let sub = match &f.pat {
                Some(p) => self.check_pat(p, ftys[i].1, from_place),
                None => {
                    let kind = self.binding_kind(ftys[i].1, from_place);
                    let id = self.declare_pattern_local(&f.name.name, ftys[i].1, kind, f.name.span);
                    Pat { ty: ftys[i].1, kind: PatKind::Bind(id), span: f.name.span }
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
        let mut result = expected;
        let mut out = Vec::new();
        for arm in arms {
            let env_len = self.env_len();
            let pats: Vec<Pat> =
                arm.pats.iter().map(|p| self.check_pat(p, s.ty, from_place)).collect();
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
            let body = self.check_expr(&arm.body, result);
            match result {
                Some(r) => {
                    if !matches!(self.kind(body.ty), TyKind::Never)
                        && self.infer.unify(&self.p.types, body.ty, r).is_err()
                    {
                        let (a, b) = (self.display(r), self.display(body.ty));
                        self.err(Diagnostic::new(
                            codes::E0300,
                            body.span,
                            format!("this arm is a `{b}`, but the match is a `{a}`"),
                        ));
                    }
                }
                None => {
                    if !matches!(self.kind(body.ty), TyKind::Never) {
                        result = Some(body.ty);
                    }
                }
            }
            self.truncate_env(env_len);
            out.push(Arm { pat, guard, body, span: arm.span });
        }
        let ty = result.unwrap_or(self.p.types.never);
        self.check_exhaustive(&s, &out, span);
        Expr { ty, span, kind: ExprKind::Match { scrutinee: Box::new(s), arms: out } }
    }

    fn check_exhaustive(&mut self, s: &Expr, arms: &[Arm], span: Span) {
        let ty = self.infer.resolve(&self.p.types, s.ty);
        let mut rows: Vec<Vec<DPat>> = Vec::new();
        for arm in arms {
            let d = self.dpat(&arm.pat);
            if !useful(self, &rows, std::slice::from_ref(&d), &[ty]) {
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
        if useful(self, &rows, &[DPat::Wild], &[ty]) {
            let missing = witness(self, &rows, ty).unwrap_or_else(|| "_".into());
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
            PatKind::Adt { adt, variant, fields, args } => {
                let n = match variant {
                    Some(v) => self.p.adt(*adt).variants()[*v as usize].fields.len(),
                    None => self.p.adt(*adt).fields().len(),
                };
                let _ = args;
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

    pub(crate) fn env_len(&self) -> usize {
        self.env.len()
    }

    pub(crate) fn truncate_env(&mut self, n: usize) {
        self.env.truncate(n);
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

/// The constructors of a type, with their field types; `None` for infinite types.
fn ctors(c: &mut Checker, ty: TyId) -> Option<Vec<(Ctor, Vec<TyId>)>> {
    match c.p.types.kind(ty).clone() {
        TyKind::Bool => Some(vec![(Ctor::Variant(0), Vec::new()), (Ctor::Variant(1), Vec::new())]),
        TyKind::Tuple(ts) => Some(vec![(Ctor::Single, ts)]),
        TyKind::Adt(a, args) => {
            if c.p.adt(a).is_enum() {
                let n = c.p.adt(a).variants().len();
                Some(
                    (0..n)
                        .map(|v| {
                            (
                                Ctor::Variant(v as u32),
                                c.p.variant_fields(a, &args, v)
                                    .into_iter()
                                    .map(|(_, t)| t)
                                    .collect(),
                            )
                        })
                        .collect(),
                )
            } else {
                Some(vec![(
                    Ctor::Single,
                    c.p.struct_fields(a, &args).into_iter().map(|(_, t)| t).collect(),
                )])
            }
        }
        _ => None,
    }
}

/// Expands or-patterns in the first column.
fn expand(rows: &[Vec<DPat>]) -> Vec<Vec<DPat>> {
    let mut out = Vec::new();
    for r in rows {
        match r.first() {
            Some(DPat::Or(alts)) => {
                for a in alts {
                    let mut nr = vec![a.clone()];
                    nr.extend_from_slice(&r[1..]);
                    out.extend(expand(&[nr]));
                }
            }
            _ => out.push(r.clone()),
        }
    }
    out
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
            DPat::Ctor(c, subs) if c == k => {
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
fn useful(c: &mut Checker, rows: &[Vec<DPat>], row: &[DPat], tys: &[TyId]) -> bool {
    if row.is_empty() {
        return rows.is_empty();
    }
    let rows = expand(rows);
    if let DPat::Or(alts) = &row[0] {
        return alts.iter().any(|a| {
            let mut r = vec![a.clone()];
            r.extend_from_slice(&row[1..]);
            useful(c, &rows, &r, tys)
        });
    }
    let ty = tys[0];
    match &row[0] {
        DPat::Ctor(k, subs) => {
            let arity = subs.len();
            let field_tys = ctors(c, ty)
                .and_then(|cs| cs.into_iter().find(|(x, _)| x == k))
                .map(|(_, f)| f)
                .unwrap_or_default();
            let mut ntys = field_tys;
            ntys.resize(arity, c.p.types.error);
            ntys.extend_from_slice(&tys[1..]);
            let mut nrow = subs.clone();
            nrow.extend_from_slice(&row[1..]);
            useful(c, &specialize(&rows, k, arity), &nrow, &ntys)
        }
        DPat::Lit(l) => {
            let l = l.clone();
            let matching: Vec<Vec<DPat>> = rows
                .iter()
                .filter(|r| matches!(&r[0], DPat::Wild) || matches!(&r[0], DPat::Lit(x) if *x == l))
                .map(|r| r[1..].to_vec())
                .collect();
            useful(c, &matching, &row[1..], &tys[1..])
        }
        DPat::Wild => {
            let used: Vec<Ctor> = rows
                .iter()
                .filter_map(|r| if let DPat::Ctor(k, _) = &r[0] { Some(k.clone()) } else { None })
                .collect();
            match ctors(c, ty) {
                Some(all) if all.iter().all(|(k, _)| used.contains(k)) => {
                    all.into_iter().any(|(k, ftys)| {
                        let arity = ftys.len();
                        let mut ntys = ftys;
                        ntys.extend_from_slice(&tys[1..]);
                        let mut nrow = vec![DPat::Wild; arity];
                        nrow.extend_from_slice(&row[1..]);
                        useful(c, &specialize(&rows, &k, arity), &nrow, &ntys)
                    })
                }
                _ => useful(c, &default_rows(&rows), &row[1..], &tys[1..]),
            }
        }
        DPat::Or(_) => unreachable!("or-patterns are expanded above"),
    }
}

/// An example of a value the rows miss, for the message (one level deep).
fn witness(c: &mut Checker, rows: &[Vec<DPat>], ty: TyId) -> Option<String> {
    let rows = expand(rows);
    let all = ctors(c, ty)?;
    for (k, ftys) in all {
        let arity = ftys.len();
        let spec = specialize(&rows, &k, arity);
        let mut tys = ftys.clone();
        tys.resize(arity, c.p.types.error);
        if useful(c, &spec, &vec![DPat::Wild; arity], &tys) {
            let text = match (&k, c.p.types.kind(ty).clone()) {
                (Ctor::Variant(v), TyKind::Bool) => {
                    (if *v == 1 { "true" } else { "false" }).to_string()
                }
                (Ctor::Variant(v), TyKind::Adt(a, _)) => {
                    let var = &c.p.adt(a).variants()[*v as usize];
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
