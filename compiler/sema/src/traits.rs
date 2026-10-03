//! Trait solving: does a type implement a trait, by which impl, and what are its associated
//! types. Generic code is checked against its bounds; concrete code against impls.

use crate::defs::*;
use crate::program::Program;
use crate::resolve::{add_with_supertraits, param_bounds_closure};
use crate::ty::*;

/// Matches `pattern` (which may mention `vars`) against `ty`, binding `vars` in `subst`.
pub fn match_ty(p: &Program, pattern: TyId, ty: TyId, vars: &[ParamId], subst: &mut Subst) -> bool {
    if pattern == ty {
        return true;
    }
    let (pk, tk) = (p.types.kind(pattern), p.types.kind(ty));
    if let TyKind::Param(v) = pk
        && vars.contains(v)
    {
        return match subst.get(*v) {
            Some(bound) => bound == ty,
            None => {
                subst.insert(*v, ty);
                true
            }
        };
    }
    match (pk, tk) {
        (TyKind::Error, _) | (_, TyKind::Error) => true,
        (TyKind::Tuple(a), TyKind::Tuple(b)) | (TyKind::Adt(_, a), TyKind::Adt(_, b))
            if a.len() == b.len() =>
        {
            if let (TyKind::Adt(x, _), TyKind::Adt(y, _)) = (pk, tk)
                && x != y
            {
                return false;
            }
            let (a, b) = (a.clone(), b.clone());
            a.iter().zip(&b).all(|(&x, &y)| match_ty(p, x, y, vars, subst))
        }
        (TyKind::Array(a, n), TyKind::Array(b, m)) if n == m => match_ty(p, *a, *b, vars, subst),
        (TyKind::Slice(a), TyKind::Slice(b)) => match_ty(p, *a, *b, vars, subst),
        _ => false,
    }
}

/// Whether two types could be the same type for some choice of their generic parameters
/// (used to find overlapping impls).
pub fn could_unify(p: &Program, a: TyId, b: TyId) -> bool {
    if a == b {
        return true;
    }
    match (p.types.kind(a), p.types.kind(b)) {
        (TyKind::Param(_), _) | (_, TyKind::Param(_)) | (TyKind::Error, _) | (_, TyKind::Error) => {
            true
        }
        (TyKind::Projection { .. }, _) | (_, TyKind::Projection { .. }) => true,
        (TyKind::Adt(x, xs), TyKind::Adt(y, ys)) => x == y && could_unify_all(p, xs, ys),
        (TyKind::Tuple(xs), TyKind::Tuple(ys)) => could_unify_all(p, xs, ys),
        (TyKind::Array(x, n), TyKind::Array(y, m)) => n == m && could_unify(p, *x, *y),
        (TyKind::Slice(x), TyKind::Slice(y)) => could_unify(p, *x, *y),
        _ => false,
    }
}

pub fn could_unify_all(p: &Program, a: &[TyId], b: &[TyId]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| could_unify(p, x, y))
}

/// The traits a type is known to have from its declaration rather than an impl: a generic
/// parameter's bounds, an opaque return type's traits, an associated type's bounds.
fn declared_bounds(p: &Program, ty: TyId) -> Option<Vec<TraitRef>> {
    match p.types.kind(ty).clone() {
        TyKind::Param(id) => Some(param_bounds_closure(p, id)),
        TyKind::Opaque(f, args) => {
            let traits = p.func(f).opaque.clone().unwrap_or_default();
            let generics = p.fn_all_generics(f);
            let subst = Subst::from_pairs(&generics, &args);
            let mut out = Vec::new();
            for r in traits {
                let args = r.args.iter().map(|&a| p.types.subst(a, &subst)).collect();
                add_with_supertraits(p, TraitRef { trait_: r.trait_, args }, &mut out);
            }
            Some(out)
        }
        TyKind::Projection { self_ty, trait_, trait_args, name } => {
            let tr = p.trait_(trait_).clone();
            let at = tr.assoc_types.iter().find(|a| a.name == name)?.clone();
            let mut subst = Subst::from_pairs(&tr.generics, &trait_args);
            subst.insert(tr.self_param, self_ty);
            let mut out = Vec::new();
            for r in at.bounds {
                let args = r.args.iter().map(|&a| p.types.subst(a, &subst)).collect();
                add_with_supertraits(p, TraitRef { trait_: r.trait_, args }, &mut out);
            }
            Some(out)
        }
        _ => None,
    }
}

/// Finds the impl of `r` for `ty`, with the impl's parameters solved. `None` if there's none
/// (or `ty` is generic: its bounds answer instead).
pub fn find_impl(p: &Program, ty: TyId, r: &TraitRef) -> Option<(ImplId, Subst)> {
    let impls = p.impls_of_trait.get(&r.trait_).cloned().unwrap_or_default();
    for i in impls {
        let imp = p.impl_(i);
        let mut subst = Subst::new();
        if !match_ty(p, imp.self_ty, ty, &imp.generics, &mut subst) {
            continue;
        }
        let Some(tr) = &imp.trait_ref else { continue };
        let ok_args = tr.args.len() == r.args.len()
            && tr
                .args
                .iter()
                .zip(&r.args)
                .all(|(&a, &b)| match_ty(p, a, b, &imp.generics, &mut subst));
        if !ok_args {
            continue;
        }
        // Parameters the impl's types don't mention can't be solved: such an impl is useless.
        if imp.generics.iter().any(|g| subst.get(*g).is_none()) {
            continue;
        }
        let bounds_ok = imp.generics.iter().all(|&g| {
            let bounds = p.param(g).bounds.clone();
            let arg = subst.get(g).unwrap_or(p.types.error);
            bounds.iter().all(|b| {
                let b = TraitRef {
                    trait_: b.trait_,
                    args: b.args.iter().map(|&a| p.types.subst(a, &subst)).collect(),
                };
                implements(p, arg, &b)
            })
        });
        // An opted-in builtin trait is conditional on the fields.
        let structural_ok = if imp.from_opt_in
            && let Some(l) = p.trait_(r.trait_).lang
        {
            implements_builtin(p, ty, l)
        } else {
            true
        };
        if bounds_ok && structural_ok {
            return Some((i, subst));
        }
    }
    None
}

/// Whether `ty` implements `r`.
pub fn implements(p: &Program, ty: TyId, r: &TraitRef) -> bool {
    match p.types.kind(ty) {
        TyKind::Error | TyKind::Never => return true,
        TyKind::Var(_) => return false,
        _ => {}
    }
    if let Some(l) = p.trait_(r.trait_).lang
        && matches!(l, Lang::Copy | Lang::Clone | Lang::GpuData)
    {
        return implements_builtin(p, ty, l);
    }
    if let Some(bounds) = declared_bounds(p, ty) {
        return bounds.iter().any(|b| b == r);
    }
    find_impl(p, ty, r).is_some()
}

/// `Copy`, `Clone` and `GpuData`, which are structural.
pub fn implements_builtin(p: &Program, ty: TyId, lang: Lang) -> bool {
    if let Some(&known) = p.builtin_impls.borrow().get(&(ty, lang)) {
        return known;
    }
    let r = implements_builtin_uncached(p, ty, lang);
    p.builtin_impls.borrow_mut().insert((ty, lang), r);
    r
}

fn implements_builtin_uncached(p: &Program, ty: TyId, lang: Lang) -> bool {
    let k = p.types.kind(ty).clone();
    match k {
        TyKind::Error | TyKind::Never => true,
        TyKind::Bool => lang != Lang::GpuData,
        TyKind::Int(i) => lang != Lang::GpuData || i.on_gpu(),
        TyKind::Float(f) => lang != Lang::GpuData || f == FloatTy::F32,
        TyKind::Vec(_) | TyKind::Mat(_) => true,
        TyKind::Tuple(ts) => ts.iter().all(|&t| implements_builtin(p, t, lang)),
        TyKind::Array(e, _) => implements_builtin(p, e, lang),
        TyKind::Adt(a, args) => {
            let Some(want) = p.lang_trait(lang) else { return false };
            let opted = p.adt(a).opt_in.iter().any(|(r, _)| r.trait_ == want);
            if !opted {
                return false;
            }
            let adt = p.adt(a);
            let subst = Subst::from_pairs(&adt.generics, &args);
            let mut field_tys: Vec<TyId> = adt.fields().iter().map(|f| f.ty).collect();
            for v in adt.variants() {
                field_tys.extend(v.fields.iter().map(|f| f.ty));
            }
            field_tys.into_iter().all(|t| {
                let t = p.types.subst(t, &subst);
                implements_builtin(p, t, lang)
            })
        }
        TyKind::Param(_) | TyKind::Opaque(..) | TyKind::Projection { .. } => {
            let Some(want) = p.lang_trait(lang) else { return false };
            declared_bounds(p, ty).is_some_and(|bs| bs.iter().any(|b| b.trait_ == want))
        }
        TyKind::Slice(_)
        | TyKind::FnPtr(..)
        | TyKind::Closure(..)
        | TyKind::FnDef(..)
        | TyKind::Var(_) => false,
    }
}

/// Replaces projections on concrete types by the impl's associated type. `in_impl` resolves
/// `Self::Name` inside that impl.
pub fn normalize(p: &Program, ty: TyId, in_impl: Option<&ImplDef>) -> TyId {
    if !p.types.any(ty, &mut |k| matches!(k, TyKind::Projection { .. })) {
        return ty;
    }
    let ty = match in_impl {
        Some(imp) => {
            let imp = imp.clone();
            p.types.map(ty, &mut |types, t| {
                let TyKind::Projection { self_ty, trait_, name, .. } = types.kind(t) else {
                    return None;
                };
                if imp.self_ty == *self_ty
                    && imp.trait_ref.as_ref().is_some_and(|r| r.trait_ == *trait_)
                {
                    return imp.assoc_types.get(name).copied();
                }
                None
            })
        }
        None => ty,
    };
    // Then impls found by solving (needs the whole program, so a second pass).
    resolve_projections(p, ty)
}

fn resolve_projections(p: &Program, ty: TyId) -> TyId {
    let k = p.types.kind(ty).clone();
    match k {
        TyKind::Projection { self_ty, trait_, trait_args, name } => {
            let self_ty = resolve_projections(p, self_ty);
            let r = TraitRef { trait_, args: trait_args.clone() };
            if !p.types.has_params(self_ty)
                && !matches!(p.types.kind(self_ty), TyKind::Opaque(..) | TyKind::Var(_))
                && let Some((i, subst)) = find_impl(p, self_ty, &r)
                && let Some(&a) = p.impl_(i).assoc_types.get(&name)
            {
                let a = p.types.subst(a, &subst);
                return resolve_projections(p, a);
            }
            p.types.intern(TyKind::Projection { self_ty, trait_, trait_args, name })
        }
        TyKind::Tuple(ts) => {
            let ts = ts.iter().map(|&t| resolve_projections(p, t)).collect();
            p.types.intern(TyKind::Tuple(ts))
        }
        TyKind::Adt(a, ts) => {
            let ts = ts.iter().map(|&t| resolve_projections(p, t)).collect();
            p.types.intern(TyKind::Adt(a, ts))
        }
        TyKind::Array(e, n) => {
            let e = resolve_projections(p, e);
            p.types.intern(TyKind::Array(e, n))
        }
        TyKind::Slice(e) => {
            let e = resolve_projections(p, e);
            p.types.intern(TyKind::Slice(e))
        }
        _ => ty,
    }
}

/// The method `name` declared in trait `t` itself, if it has one. Not its supertraits': the
/// bounds that method lookup goes through already include them.
pub fn trait_method(p: &Program, t: TraitId, name: &str) -> Option<FnId> {
    p.trait_(t).methods.iter().copied().find(|&f| p.func(f).name == name)
}

/// The function that implements trait method `method` for the concrete type `self_ty`, and the
/// substitution for its generics (impl's, then the method's own `method_args`). Falls back to
/// the trait's default body.
pub fn resolve_trait_method(
    p: &Program,
    method: FnId,
    self_ty: TyId,
    trait_args: &[TyId],
    method_args: &[TyId],
) -> Option<(FnId, Subst)> {
    let FnOwner::Trait(t) = p.func(method).owner else { return None };
    let tr = p.trait_(t);
    let r = TraitRef { trait_: t, args: trait_args.to_vec() };
    let mdef = p.func(method);
    if let Some((i, mut subst)) = find_impl(p, self_ty, &r) {
        let imp = p.impl_(i);
        if let Some(f) = imp.methods.iter().copied().find(|&f| p.func(f).name == mdef.name) {
            let own = p.func(f).generics.clone();
            for (g, &a) in own.iter().zip(method_args) {
                subst.insert(*g, a);
            }
            return Some((f, subst));
        }
    }
    // The default body: generic over the trait's `Self` and parameters.
    mdef.body.as_ref()?;
    let mut subst = Subst::from_pairs(&tr.generics, trait_args);
    subst.insert(tr.self_param, self_ty);
    for (g, &a) in mdef.generics.iter().zip(method_args) {
        subst.insert(*g, a);
    }
    Some((method, subst))
}
