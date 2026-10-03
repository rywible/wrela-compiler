//! Type inference within one body: variables, unification, and resolving what's left.

use crate::ty::*;
use std::collections::{HashMap, HashSet};
use wrela_diag::Span;

#[derive(Clone, Debug)]
struct VarInfo {
    kind: VarKind,
    bound: Option<TyId>,
    origin: Span,
    /// How many variables are bound, through others, to this one (itself included): when two
    /// unbound variables unify, the smaller set joins the larger, so chains stay short.
    size: u32,
}

/// Two types that can't be made the same; the caller reports it with context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mismatch;

#[derive(Clone, Debug, Default)]
pub struct Infer {
    vars: Vec<VarInfo>,
    /// While a snapshot is open: the variables bound since it was taken, so a rollback can
    /// unbind them. Unification only binds variables; it never makes new ones.
    trail: Option<Vec<usize>>,
}

impl Infer {
    pub fn new_var(&mut self, types: &Types, kind: VarKind, origin: Span) -> TyId {
        self.vars.push(VarInfo { kind, bound: None, origin, size: 1 });
        types.var(VarId(self.vars.len() as u32 - 1))
    }

    /// Follows bound variables until a type that isn't one.
    pub fn shallow(&self, types: &Types, mut t: TyId) -> TyId {
        while let TyKind::Var(v) = types.kind(t) {
            match self.vars[v.index()].bound {
                Some(b) => t = b,
                None => break,
            }
        }
        t
    }

    pub fn var_kind(&self, types: &Types, t: TyId) -> Option<VarKind> {
        match types.kind(self.shallow(types, t)) {
            TyKind::Var(v) => Some(self.vars[v.index()].kind),
            _ => None,
        }
    }

    /// The type `t` takes when it's a number literal's type not settled yet: `i32` for an
    /// integer, `f32` for a float (as [`Infer::apply_defaults`] gives them).
    pub fn literal_default(&self, types: &Types, t: TyId) -> Option<TyId> {
        match self.var_kind(types, t)? {
            VarKind::Int => Some(types.i32),
            VarKind::Float => Some(types.f32),
            VarKind::General => None,
        }
    }

    /// Substitutes every bound variable, deeply. Unbound ones stay.
    pub fn resolve(&self, types: &Types, t: TyId) -> TyId {
        if !types.has_vars(t) {
            return t;
        }
        self.resolve_in(types, t, &mut HashMap::new())
    }

    /// [`Infer::resolve`], each variable resolved once: a type whose variables share parts
    /// (`(a, a)` where `a` is `(b, b)`, …) is a small graph, however large its tree is.
    fn resolve_in(&self, types: &Types, t: TyId, done: &mut HashMap<VarId, TyId>) -> TyId {
        types.map(t, &mut |types, x| match types.kind(x) {
            TyKind::Var(v) => {
                let b = self.vars[v.index()].bound?;
                if let Some(&r) = done.get(v) {
                    return Some(r);
                }
                let r = self.resolve_in(types, b, done);
                done.insert(*v, r);
                Some(r)
            }
            _ => None,
        })
    }

    /// Gives literal variables their default types: `i32` for integers, `f32` for floats.
    /// Returns the origins of general variables nothing determined.
    pub fn apply_defaults(&mut self, types: &Types) -> Vec<Span> {
        let mut unknown = Vec::new();
        for v in self.vars.iter_mut().filter(|v| v.bound.is_none()) {
            v.bound = Some(match v.kind {
                VarKind::Int => types.i32,
                VarKind::Float => types.f32,
                VarKind::General => {
                    unknown.push(v.origin);
                    types.error
                }
            });
        }
        unknown
    }

    /// Makes `a` and `b` the same type, or reports that they can't be. On a mismatch, the
    /// parts unified before it stay unified.
    pub fn unify(&mut self, types: &Types, a: TyId, b: TyId) -> Result<(), Mismatch> {
        self.unify_in(types, a, b, &mut HashSet::new())
    }

    /// Unifies `a` and `b` if they fit, leaving every variable as it was if they don't.
    pub fn try_unify(&mut self, types: &Types, a: TyId, b: TyId) -> bool {
        self.snapshot();
        let ok = self.unify(types, a, b).is_ok();
        if ok {
            self.commit();
        } else {
            self.rollback();
        }
        ok
    }

    /// Whether `a` and `b` would unify, leaving every variable as it was either way.
    pub fn fits(&mut self, types: &Types, a: TyId, b: TyId) -> bool {
        self.snapshot();
        let ok = self.unify(types, a, b).is_ok();
        self.rollback();
        ok
    }

    /// Starts recording bindings, for [`Infer::rollback`]. Snapshots don't nest.
    fn snapshot(&mut self) {
        debug_assert!(self.trail.is_none(), "a snapshot is already open");
        self.trail = Some(Vec::new());
    }

    /// Keeps the bindings made since the snapshot.
    fn commit(&mut self) {
        self.trail = None;
    }

    /// Unbinds the variables bound since the snapshot.
    fn rollback(&mut self) {
        for i in self.trail.take().unwrap_or_default() {
            self.vars[i].bound = None;
        }
    }

    /// Binds the unbound variable `v` to `t`.
    fn set(&mut self, v: VarId, t: TyId) {
        debug_assert!(self.vars[v.index()].bound.is_none());
        if let Some(trail) = &mut self.trail {
            trail.push(v.index());
        }
        self.vars[v.index()].bound = Some(t);
    }

    /// `done` holds the pairs of large types already made the same: types share parts, and
    /// unifying a shared part again would only repeat the work.
    fn unify_in(
        &mut self,
        types: &Types,
        a: TyId,
        b: TyId,
        done: &mut HashSet<(TyId, TyId)>,
    ) -> Result<(), Mismatch> {
        let a = self.shallow(types, a);
        let b = self.shallow(types, b);
        if a == b {
            return Ok(());
        }
        let large = types.size(a).max(types.size(b)) > 64;
        if large && done.contains(&(a, b)) {
            return Ok(());
        }
        self.unify_parts(types, a, b, done)?;
        if large {
            done.insert((a, b));
        }
        Ok(())
    }

    fn unify_parts(
        &mut self,
        types: &Types,
        a: TyId,
        b: TyId,
        done: &mut HashSet<(TyId, TyId)>,
    ) -> Result<(), Mismatch> {
        let (ka, kb) = (types.kind(a), types.kind(b));
        match (&ka, &kb) {
            (TyKind::Error, _) | (_, TyKind::Error) => Ok(()),
            (TyKind::Var(x), TyKind::Var(y)) => {
                let (kx, ky) = (self.vars[x.index()].kind, self.vars[y.index()].kind);
                // Keep the more specific kind: Float beats Int beats General. Between equals, the
                // smaller set joins the larger (each element of `[0.0, 1.0, ...]` would
                // otherwise make one chain longer).
                let rank = |k: VarKind| match k {
                    VarKind::General => 0,
                    VarKind::Int => 1,
                    VarKind::Float => 2,
                };
                let (sx, sy) = (self.vars[x.index()].size, self.vars[y.index()].size);
                let y_joins_x = if rank(kx) != rank(ky) { rank(kx) > rank(ky) } else { sx >= sy };
                let (from, to, to_ty) = if y_joins_x { (*y, *x, a) } else { (*x, *y, b) };
                let joined = self.vars[from.index()].size;
                self.vars[to.index()].size = self.vars[to.index()].size.saturating_add(joined);
                self.set(from, to_ty);
                Ok(())
            }
            (TyKind::Var(x), _) => self.bind(types, *x, b),
            (_, TyKind::Var(y)) => self.bind(types, *y, a),
            (TyKind::Tuple(xs), TyKind::Tuple(ys)) => self.unify_all(types, xs, ys, done),
            (TyKind::Adt(x, xs), TyKind::Adt(y, ys)) if x == y => {
                self.unify_all(types, xs, ys, done)
            }
            (TyKind::Opaque(x, xs), TyKind::Opaque(y, ys))
            | (TyKind::FnDef(x, xs), TyKind::FnDef(y, ys))
                if x == y =>
            {
                self.unify_all(types, xs, ys, done)
            }
            (TyKind::Closure(x, xs), TyKind::Closure(y, ys)) if x == y => {
                self.unify_all(types, xs, ys, done)
            }
            (TyKind::Array(x, n), TyKind::Array(y, m)) if n == m => {
                self.unify_in(types, *x, *y, done)
            }
            (TyKind::Slice(x), TyKind::Slice(y)) => self.unify_in(types, *x, *y, done),
            (TyKind::FnPtr(xp, xr), TyKind::FnPtr(yp, yr)) => {
                self.unify_all(types, xp, yp, done)?;
                self.unify_in(types, *xr, *yr, done)
            }
            (
                TyKind::Projection { self_ty: s1, trait_: t1, trait_args: a1, name: n1 },
                TyKind::Projection { self_ty: s2, trait_: t2, trait_args: a2, name: n2 },
            ) if t1 == t2 && n1 == n2 && a1.len() == a2.len() => {
                self.unify_in(types, *s1, *s2, done)?;
                self.unify_all(types, a1, a2, done)
            }
            _ => Err(Mismatch),
        }
    }

    /// Unifies two lists pairwise, in order; lists of different lengths don't unify.
    fn unify_all(
        &mut self,
        types: &Types,
        xs: &[TyId],
        ys: &[TyId],
        done: &mut HashSet<(TyId, TyId)>,
    ) -> Result<(), Mismatch> {
        if xs.len() != ys.len() {
            return Err(Mismatch);
        }
        for (&x, &y) in xs.iter().zip(ys) {
            self.unify_in(types, x, y, done)?;
        }
        Ok(())
    }

    fn bind(&mut self, types: &Types, v: VarId, t: TyId) -> Result<(), Mismatch> {
        let ok = match self.vars[v.index()].kind {
            VarKind::General => true,
            VarKind::Int => matches!(types.kind(t), TyKind::Int(_) | TyKind::Float(_)),
            VarKind::Float => matches!(types.kind(t), TyKind::Float(_)),
        };
        if !ok {
            return Err(Mismatch);
        }
        // The occurs check: a type can't contain itself.
        if self.occurs(types, v, t) {
            return Err(Mismatch);
        }
        self.set(v, t);
        Ok(())
    }

    /// Whether the variable `v` is part of `t`, through the variables bound so far.
    fn occurs(&self, types: &Types, v: VarId, t: TyId) -> bool {
        match types.kind(self.shallow(types, t)) {
            TyKind::Var(x) => *x == v,
            k => {
                let mut found = false;
                children(k, &mut |c| found = found || self.occurs(types, v, c));
                found
            }
        }
    }
}
