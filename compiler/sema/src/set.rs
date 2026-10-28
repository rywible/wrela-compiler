//! A build-time override of a program's `pub const` (language.md §13: `wrela build --set
//! NAME=VALUE`): the root module's constant `NAME` takes `VALUE` in place of its initializer,
//! before anything reads it, so its uses fold with the new value as they would with one
//! written there. It's a constant of a scalar type, a `Text`, or an enum whose variants hold
//! nothing (`--set TIER=Medium` names a variant of the constant's enum type): what a program
//! names its tier or its style by.

use crate::collect::{PackageInfo, SourceUnit};
use crate::defs::PackageKind;
use crate::program::Program;
use crate::ty::ConstId;
use std::sync::Arc;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_syntax::ast;

/// E0224, about the command line rather than a place in the program.
fn unplaced(message: String) -> Diagnostic {
    let nowhere = Span::new(wrela_diag::FileId(0), 0, 0);
    Diagnostic { primary: None, ..Diagnostic::new(codes::E0224, nowhere, message) }
}

/// The root module of the program's package (its `main.wrela`), as `collect` finds `Program::
/// main`.
fn is_root(u: &SourceUnit, packages: &[PackageInfo]) -> bool {
    u.path == ["main"] && packages.get(u.package).is_some_and(|p| p.kind == PackageKind::Program)
}

/// Puts each of `sets`' values in place of its constant's initializer, in the root module of
/// the program's package: the names it can't set, each an error (E0224).
pub fn apply(
    units: &mut [SourceUnit],
    packages: &[PackageInfo],
    sets: &[(String, String)],
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if sets.is_empty() {
        return out;
    }
    let Some(root) = units.iter_mut().find(|u| is_root(u, packages)) else {
        out.push(unplaced(
            "`--set` overrides a constant of a program's `main.wrela`, and this package has none"
                .into(),
        ));
        return out;
    };
    let file = Arc::make_mut(&mut root.ast);
    for (name, value) in sets {
        let found = file.items.iter_mut().find_map(|item| match &mut item.kind {
            ast::ItemKind::Const(c) if c.name.name == *name => Some((item.vis.is_some(), c)),
            _ => None,
        });
        let Some((public, c)) = found else {
            out.push(
                unplaced(format!(
                    "`--set {name}=…`: the program's `main.wrela` has no constant `{name}`"
                ))
                .with_note("`--set` overrides a `pub const` of the program's root module"),
            );
            continue;
        };
        let at = c.name.span;
        let Some(ty) = &c.ty else {
            out.push(Diagnostic::new(
                codes::E0224,
                at,
                format!(
                    "`--set {name}=…` needs `{name}`'s type written: `pub const {name}: T = …`"
                ),
            ));
            continue;
        };
        if !settable_type(ty) {
            let shown = match &ty.kind {
                ast::TypeExprKind::Path(path) if path.is_single() => {
                    path.segments[0].ident.name.clone()
                }
                _ => "that type".to_string(),
            };
            out.push(
                Diagnostic::new(
                    codes::E0224,
                    at,
                    format!("`--set {name}=…`: `{name}` is a `{shown}`"),
                )
                .with_note(SETTABLE),
            );
            continue;
        }
        if !public {
            out.push(
                Diagnostic::new(
                    codes::E0224,
                    at,
                    format!("`--set {name}=…` overrides a `pub const`, and `{name}` isn't `pub`"),
                )
                .with_help(format!("make it `pub const {name}` to let a build set it")),
            );
            continue;
        }
        match value_expr(value, ty, c.value.span) {
            Some(e) => c.value = e,
            None => out.push(Diagnostic::new(
                codes::E0224,
                at,
                format!("`--set {name}={value}`: `{value}` isn't a number, `true`, `false`, a quoted text or a variant's name"),
            )),
        }
    }
    out
}

/// What a build sets.
const SETTABLE: &str =
    "a build sets a `bool`, `u32`, `i32`, `f32`, a `Text`, or an enum whose variants hold nothing";

/// Whether a constant written with type `ty` may be set: a path that isn't a built-in type
/// other than `bool`, `u32`, `i32` or `f32` (an enum's or `Text`'s is checked once types are
/// known, [`check_types`]).
fn settable_type(ty: &ast::TypeExpr) -> bool {
    let ast::TypeExprKind::Path(path) = &ty.kind else { return false };
    if !path.is_single() {
        return true;
    }
    let name = path.segments[0].ident.name.as_str();
    matches!(name, "bool" | "u32" | "i32" | "f32")
        || crate::builtins::BuiltinTy::lookup(name).is_none()
}

/// `value` as an expression of the constant's written type `ty`, at `span`: a literal, a
/// negated one, a quoted text, or a variant of the type (`Medium` is `Tier::Medium`).
fn value_expr(value: &str, ty: &ast::TypeExpr, span: Span) -> Option<ast::Expr> {
    let lit = |kind, text: &str| ast::Expr {
        kind: ast::ExprKind::Lit(ast::Lit { kind, text: text.to_string(), span }),
        span,
    };
    let v = value.trim();
    if let Some(rest) = v.strip_prefix('-') {
        let inner = value_expr(rest, ty, span)?;
        return matches!(&inner.kind, ast::ExprKind::Lit(_)).then(|| ast::Expr {
            kind: ast::ExprKind::Unary(ast::UnOp::Neg, Box::new(inner)),
            span,
        });
    }
    if v == "true" || v == "false" {
        return Some(lit(ast::LitKind::Bool(v == "true"), v));
    }
    if v.starts_with('"') && v.ends_with('"') && v.len() >= 2 {
        return Some(lit(ast::LitKind::Str, v));
    }
    if v.bytes().all(|b| b.is_ascii_digit()) && !v.is_empty() {
        return Some(lit(ast::LitKind::Int(ast::IntValue::Ok(v.parse().ok()?)), v));
    }
    if let Ok(f) = v.parse::<f64>()
        && v.bytes().next().is_some_and(|b| b.is_ascii_digit())
    {
        return Some(lit(ast::LitKind::Float(f), v));
    }
    let ast::TypeExprKind::Path(path) = &ty.kind else { return None };
    if !wrela_syntax::lexer::is_name(v) {
        return None;
    }
    let mut segments = path.segments.clone();
    segments
        .push(ast::PathSegment { ident: ast::Ident { name: v.to_string(), span }, generics: None });
    Some(ast::Expr { kind: ast::ExprKind::Path(ast::Path { segments, span }), span })
}

/// E0224 for each set constant whose type a build can't set: a scalar, a `Text`, or an enum
/// whose variants hold nothing. Checked once the constants' types are known.
pub fn check_types(
    p: &Program,
    tys: &crate::check::ConstTypes,
    sets: &[(String, String)],
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (name, _) in sets {
        let Some(i) =
            p.consts.iter().position(|c| !c.default && c.name == *name && Some(c.module) == p.main)
        else {
            continue;
        };
        let c = &p.consts[i];
        let Some(&t) = tys.get(&ConstId(i as u32)) else { continue };
        let ok = p.is_build_constant_ty(t);
        if !ok {
            out.push(
                Diagnostic::new(
                    codes::E0224,
                    c.span,
                    format!("`--set {name}=…`: `{name}` is a `{}`", p.display_ty(t)),
                )
                .with_note(SETTABLE),
            );
        }
    }
    out
}
