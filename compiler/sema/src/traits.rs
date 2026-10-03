//! Trait solving: does a type implement a trait, by which impl, and what are its associated
//! types. Generic code is checked against its bounds; concrete code against impls.

use crate::defs::*;
use crate::program::Program;
use crate::resolve::{add_with_supertraits, param_bounds_closure};
use crate::ty::*;
use std::collections::HashMap;

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
            a.iter().zip(b).all(|(&x, &y)| match_ty(p, x, y, vars, subst))
        }
        (TyKind::Array(a, n), TyKind::Array(b, m)) if n == m => match_ty(p, *a, *b, vars, subst),
        (TyKind::Slice(a), TyKind::Slice(b)) => match_ty(p, *a, *b, vars, subst),
        _ => false,
    }
}

/// Whether two types could be the same type for some choice of their generic parameters and
/// inference variables (used to find overlapping impls).
pub fn could_unify(p: &Program, a: TyId, b: TyId) -> bool {
    if a == b {
        return true;
    }
    match (p.types.kind(a), p.types.kind(b)) {
        (TyKind::Param(_), _) | (_, TyKind::Param(_)) | (TyKind::Error, _) | (_, TyKind::Error) => {
            true
        }
        // A variable not inferred yet (method lookup on `W<{float}>`).
        (TyKind::Var(_), _) | (_, TyKind::Var(_)) => true,
        (TyKind::Projection { .. }, _) | (_, TyKind::Projection { .. }) => true,
        (TyKind::Adt(x, xs), TyKind::Adt(y, ys)) => x == y && could_unify_all(p, xs, ys),
        (TyKind::Tuple(xs), TyKind::Tuple(ys)) => could_unify_all(p, xs, ys),
        (TyKind::Array(x, n), TyKind::Array(y, m)) => n == m && could_unify(p, *x, *y),
        (TyKind::Slice(x), TyKind::Slice(y)) => could_unify(p, *x, *y),
        _ => false,
    }
}

/// Whether the types `a` and the types `b`, each of two impls' parameters standing for one
/// type throughout, could be the same: `(T, T)` can't be `(i32, f32)`. Projections and
/// variables could be anything.
pub fn impls_could_meet(p: &Program, a: &[TyId], b: &[TyId]) -> bool {
    let mut bound = HashMap::new();
    a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| meet(p, x, y, &mut bound, 0))
}

fn meet(p: &Program, a: TyId, b: TyId, bound: &mut HashMap<ParamId, TyId>, depth: u32) -> bool {
    if a == b || depth > 64 {
        return true;
    }
    match (p.types.kind(a), p.types.kind(b)) {
        (&TyKind::Param(x), _) => match bound.get(&x) {
            Some(&t) => meet(p, t, b, bound, depth + 1),
            None => {
                bound.insert(x, b);
                true
            }
        },
        (_, &TyKind::Param(y)) => match bound.get(&y) {
            Some(&t) => meet(p, a, t, bound, depth + 1),
            None => {
                bound.insert(y, a);
                true
            }
        },
        (TyKind::Error, _) | (_, TyKind::Error) => true,
        (TyKind::Var(_), _) | (_, TyKind::Var(_)) => true,
        (TyKind::Projection { .. }, _) | (_, TyKind::Projection { .. }) => true,
        (TyKind::Adt(x, xs), TyKind::Adt(y, ys)) => {
            x == y
                && xs.len() == ys.len()
                && xs.iter().zip(ys).all(|(&s, &t)| meet(p, s, t, bound, depth + 1))
        }
        (TyKind::Tuple(xs), TyKind::Tuple(ys)) => {
            xs.len() == ys.len()
                && xs.iter().zip(ys).all(|(&s, &t)| meet(p, s, t, bound, depth + 1))
        }
        (TyKind::Array(x, n), TyKind::Array(y, m)) => n == m && meet(p, *x, *y, bound, depth + 1),
        (TyKind::Slice(x), TyKind::Slice(y)) => meet(p, *x, *y, bound, depth + 1),
        _ => false,
    }
}

pub fn could_unify_all(p: &Program, a: &[TyId], b: &[TyId]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| could_unify(p, x, y))
}

/// The traits a type is known to have from its declaration rather than an impl: a generic
/// parameter's bounds, an opaque return type's traits, an associated type's bounds.
pub(crate) fn declared_bounds(p: &Program, ty: TyId) -> Option<Vec<TraitRef>> {
    match p.types.kind(ty) {
        TyKind::Param(id) => Some(param_bounds_closure(p, *id)),
        TyKind::Opaque(f, args) => {
            let generics = p.fn_all_generics(*f);
            let subst = Subst::from_pairs(&generics, args);
            let mut out = Vec::new();
            for r in p.func(*f).opaque.iter().flatten() {
                add_with_supertraits(p, ty, r.subst(&p.types, &subst), &mut out);
            }
            Some(out)
        }
        TyKind::Projection { self_ty, trait_, trait_args, name } => {
            let tr = p.trait_(*trait_);
            let at = tr.assoc_types.iter().find(|a| a.name == *name)?;
            let mut subst = Subst::from_pairs(&tr.generics, trait_args);
            subst.insert(tr.self_param, *self_ty);
            let mut out = Vec::new();
            for r in &at.bounds {
                add_with_supertraits(p, ty, r.subst(&p.types, &subst), &mut out);
            }
            Some(out)
        }
        _ => None,
    }
}

/// Finds the impl of `r` for `ty`, with the impl's parameters solved. `None` if there's none
/// (or `ty` is generic: its bounds answer instead).
pub fn find_impl(p: &Program, ty: TyId, r: &TraitRef) -> Option<(ImplId, Subst)> {
    p.impls_of(r.trait_).iter().find_map(|&i| impl_for(p, i, ty, r).map(|s| (i, s)))
}

/// Whether impl `i` is an impl of `r` for `ty`: its parameters solved, if so.
fn impl_for(p: &Program, i: ImplId, ty: TyId, r: &TraitRef) -> Option<Subst> {
    let imp = p.impl_(i);
    let mut subst = Subst::new();
    if !match_ty(p, imp.self_ty, ty, &imp.generics, &mut subst) {
        return None;
    }
    let tr = imp.trait_ref.as_ref()?;
    let ok_args = tr.args.len() == r.args.len()
        && tr.args.iter().zip(&r.args).all(|(&a, &b)| match_ty(p, a, b, &imp.generics, &mut subst));
    if !ok_args {
        return None;
    }
    // Parameters the impl's types don't mention can't be solved: such an impl is useless.
    if imp.generics.iter().any(|g| subst.get(*g).is_none()) {
        return None;
    }
    let bounds_ok = imp.generics.iter().all(|&g| {
        let arg = subst.get(g).unwrap_or(p.types.error);
        p.param(g).bounds.iter().all(|b| implements(p, arg, &b.subst(&p.types, &subst)))
    });
    // An opted-in builtin trait is conditional on the fields.
    let structural_ok = if imp.from_opt_in
        && let Some(l) = p.trait_(r.trait_).lang
    {
        implements_builtin(p, ty, l)
    } else {
        true
    };
    (bounds_ok && structural_ok).then_some(subst)
}

/// The arguments with which `ty` implements trait `t`: one list for each impl of `t` whose
/// self type matches `ty` and that applies (or whose arguments another impl gives). `ty` is
/// concrete: it has no declared bounds.
pub fn impl_args(p: &Program, ty: TyId, t: TraitId) -> Vec<Vec<TyId>> {
    let structural = p.trait_(t).lang.is_some_and(|l| l.is_structural());
    let mut out: Vec<Vec<TyId>> = Vec::new();
    for &i in p.impls_of(t) {
        let imp = p.impl_(i);
        let mut subst = Subst::new();
        if !match_ty(p, imp.self_ty, ty, &imp.generics, &mut subst) {
            continue;
        }
        let args =
            imp.trait_ref.as_ref().map(|r| r.subst(&p.types, &subst).args).unwrap_or_default();
        let r = TraitRef { trait_: t, args };
        // This impl usually answers by itself; when it doesn't, another impl may.
        if !out.contains(&r.args)
            && ((!structural && impl_for(p, i, ty, &r).is_some()) || implements(p, ty, &r))
        {
            out.push(r.args);
        }
    }
    out
}

/// How much deeper than its first goal's size the solver goes through impls' bounds before it
/// gives up: a goal that only grows (`impl<T: Tr<W<U>>, U> Tr<U> for T`) never ends otherwise.
/// A bound on a type's part (`Union<A, B>` needs `A: Surface`) needs a level per level of the
/// type, so the first goal's size counts too.
const SOLVE_DEPTH_SLACK: usize = 64;

/// Whether `ty` implements `r`.
pub fn implements(p: &Program, ty: TyId, r: &TraitRef) -> bool {
    match p.types.kind(ty) {
        TyKind::Error | TyKind::Never => return true,
        TyKind::Var(_) => return false,
        _ => {}
    }
    if let Some(l) = p.trait_(r.trait_).lang
        && l.is_structural()
    {
        return implements_builtin(p, ty, l);
    }
    if let Some(bounds) = declared_bounds(p, ty) {
        return bounds.iter().any(|b| b == r);
    }
    // A goal that an impl's bounds lead back to isn't met through that impl: alone,
    // `impl<T: Tr> Tr for T` implements `Tr` for nothing.
    let goal = (ty, r.clone());
    {
        let (goals, first) = &mut *p.solving.borrow_mut();
        if goals.is_empty() {
            let size =
                r.args.iter().fold(p.types.size(ty), |n, &a| n.saturating_add(p.types.size(a)));
            *first = size.min(MAX_TYPE_SIZE);
        }
        if goals.len() >= *first as usize + SOLVE_DEPTH_SLACK || !goals.insert(goal.clone()) {
            return false;
        }
    }
    let found = find_impl(p, ty, r).is_some();
    p.solving.borrow_mut().0.remove(&goal);
    found
}

/// `Copy`, `Clone` and `GpuData`, which are structural.
pub fn implements_builtin(p: &Program, ty: TyId, lang: Lang) -> bool {
    if let Some(&known) = p.builtin_impls.borrow().get(&(ty, lang)) {
        return known;
    }
    // Assumed while it's found: a type that holds itself (E0318, reported) doesn't send this
    // round its fields forever.
    p.builtin_impls.borrow_mut().insert((ty, lang), true);
    let r = implements_builtin_uncached(p, ty, lang);
    p.builtin_impls.borrow_mut().insert((ty, lang), r);
    r
}

fn implements_builtin_uncached(p: &Program, ty: TyId, lang: Lang) -> bool {
    match p.types.kind(ty) {
        TyKind::Error | TyKind::Never => true,
        TyKind::Bool => lang != Lang::GpuData,
        TyKind::Int(i) => lang != Lang::GpuData || i.on_gpu(),
        TyKind::Float(f) => lang != Lang::GpuData || *f == FloatTy::F32,
        TyKind::Vec(_) | TyKind::Mat(_) => true,
        TyKind::Tuple(ts) => ts.iter().all(|&t| implements_builtin(p, t, lang)),
        // WGSL has no empty arrays.
        TyKind::Array(e, n) => (lang != Lang::GpuData || *n > 0) && implements_builtin(p, *e, lang),
        TyKind::Adt(a, args) => {
            let Some(want) = p.lang_trait(lang) else { return false };
            let adt = p.adt(*a);
            if !adt.opt_in.iter().any(|(r, _)| r.trait_ == want) {
                return false;
            }
            let subst = Subst::from_pairs(&adt.generics, args);
            // A field `T::Out` is, with `T` known, the type the impl gives it.
            adt.all_fields().all(|f| {
                implements_builtin(p, normalize(p, p.types.subst(f.ty, &subst), None), lang)
            })
        }
        TyKind::Projection { .. } if normalize(p, ty, None) != ty => {
            implements_builtin(p, normalize(p, ty, None), lang)
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
    if !p.types.has_projections(ty) {
        return ty;
    }
    let ty = match in_impl {
        Some(imp) => p.types.map(ty, &mut |types, t| {
            let TyKind::Projection { self_ty, trait_, name, .. } = types.kind(t) else {
                return None;
            };
            if imp.self_ty == *self_ty
                && imp.trait_ref.as_ref().is_some_and(|r| r.trait_ == *trait_)
            {
                return imp.assoc_types.get(name).copied();
            }
            None
        }),
        None => ty,
    };
    // Then impls found by solving (needs the whole program, so a second pass).
    resolve_projections(p, ty)
}

fn resolve_projections(p: &Program, ty: TyId) -> TyId {
    resolve_projections_in(p, ty, &mut Vec::new())
}

/// How many projections deep resolving one may go: `type A = W<Self>::A` makes a new one at
/// each step.
const MAX_PROJECTION_CHAIN: usize = 64;

/// `active`: the projections being resolved. One met again is defined in terms of itself
/// (`type A = Self::A`), and a chain past [`MAX_PROJECTION_CHAIN`] grows without end: either is
/// left a projection, which [`cyclic_projection`] finds for the impl that declares it.
fn resolve_projections_in(p: &Program, ty: TyId, active: &mut Vec<TyId>) -> TyId {
    p.types.map(ty, &mut |types, t| match types.kind(t) {
        TyKind::Projection { self_ty, trait_, trait_args, name } => {
            let self_ty = resolve_projections_in(p, *self_ty, active);
            let r = TraitRef { trait_: *trait_, args: trait_args.clone() };
            let this = types.intern(TyKind::Projection {
                self_ty,
                trait_: r.trait_,
                trait_args: r.args.clone(),
                name: name.clone(),
            });
            if active.contains(&this) || active.len() >= MAX_PROJECTION_CHAIN {
                return Some(this);
            }
            if !types.has_params(self_ty)
                && !matches!(types.kind(self_ty), TyKind::Opaque(..) | TyKind::Var(_))
                && let Some((i, subst)) = find_impl(p, self_ty, &r)
                && let Some(&a) = p.impl_(i).assoc_types.get(name)
            {
                active.push(this);
                let resolved = resolve_projections_in(p, types.subst(a, &subst), active);
                active.pop();
                return Some(resolved);
            }
            Some(this)
        }
        // Left as they are, arguments and all.
        TyKind::Opaque(..) | TyKind::Closure(..) | TyKind::FnDef(..) => Some(t),
        _ => None,
    })
}

/// A projection in `ty`, normalized, that an impl defines but resolving couldn't end: an
/// associated type defined in terms of itself.
pub fn cyclic_projection(p: &Program, ty: TyId) -> Option<TyId> {
    let mut found = None;
    p.types.any(ty, &mut |k| {
        if let TyKind::Projection { self_ty, trait_, trait_args, name } = k
            && !p.types.has_params(*self_ty)
            && !p.types.has_vars(*self_ty)
            && !matches!(p.types.kind(*self_ty), TyKind::Opaque(..) | TyKind::Error)
        {
            let r = TraitRef { trait_: *trait_, args: trait_args.clone() };
            if find_impl(p, *self_ty, &r)
                .is_some_and(|(i, _)| p.impl_(i).assoc_types.contains_key(name))
            {
                found = Some(p.types.intern(k.clone()));
                return true;
            }
        }
        false
    });
    found
}

/// The method `name` declared in trait `t` itself, if it has one. Not its supertraits': the
/// bounds that method lookup goes through already include them.
pub fn trait_method(p: &Program, t: TraitId, name: &str) -> Option<FnId> {
    p.trait_(t).methods.iter().copied().find(|&f| p.func(f).name == name)
}

/// The methods `name` of `r` or of its supertraits (`Sub::base(x)`), each with the trait that
/// declares it, with its arguments for `Self = self_ty`: `r`'s own alone if it has one, else
/// every supertrait's (more than one is ambiguous).
pub fn trait_method_via(
    p: &Program,
    self_ty: TyId,
    r: TraitRef,
    name: &str,
) -> Vec<(FnId, TraitRef)> {
    if let Some(m) = trait_method(p, r.trait_, name) {
        return vec![(m, r)];
    }
    let mut all = Vec::new();
    add_with_supertraits(p, self_ty, r, &mut all);
    let mut found: Vec<(FnId, TraitRef)> = Vec::new();
    for r in all {
        if let Some(m) = trait_method(p, r.trait_, name)
            && !found.iter().any(|(f, _)| *f == m)
        {
            found.push((m, r));
        }
    }
    found
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
            for (&g, &a) in p.func(f).generics.iter().zip(method_args) {
                subst.insert(g, a);
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
