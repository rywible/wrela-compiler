//! Type inference within one body: variables, unification, and resolving what's left.

use crate::ty::*;
use std::collections::HashSet;
use wrela_diag::Span;

#[derive(Clone, Debug)]
struct VarInfo {
    kind: VarKind,
    bound: Option<TyId>,
    origin: Span,
}

/// Two types that can't be made the same; the caller reports it with context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mismatch;

#[derive(Clone, Debug, Default)]
pub struct Infer {
    vars: Vec<VarInfo>,
}

impl Infer {
    pub fn new_var(&mut self, types: &Types, kind: VarKind, origin: Span) -> TyId {
        self.vars.push(VarInfo { kind, bound: None, origin });
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

    /// Substitutes every bound variable, deeply. Unbound ones stay.
    pub fn resolve(&self, types: &Types, t: TyId) -> TyId {
        types.map(t, &mut |types, x| match types.kind(x) {
            TyKind::Var(v) => {
                let b = self.vars[v.index()].bound?;
                Some(self.resolve(types, b))
            }
            _ => None,
        })
    }

    /// Gives literal variables their default types: `i32` for integers, `f32` for floats.
    /// Returns the origins of general variables nothing determined.
    pub fn apply_defaults(&mut self, types: &Types) -> Vec<Span> {
        let mut unknown = Vec::new();
        for i in 0..self.vars.len() {
            if self.vars[i].bound.is_some() {
                continue;
            }
            match self.vars[i].kind {
                VarKind::Int => self.vars[i].bound = Some(types.i32),
                VarKind::Float => self.vars[i].bound = Some(types.f32),
                VarKind::General => {
                    unknown.push(self.vars[i].origin);
                    self.vars[i].bound = Some(types.error);
                }
            }
        }
        unknown
    }

    /// Makes `a` and `b` the same type, or reports that they can't be.
    pub fn unify(&mut self, types: &Types, a: TyId, b: TyId) -> Result<(), Mismatch> {
        self.unify_in(types, a, b, &mut HashSet::new())
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
        let (ka, kb) = (types.kind(a).clone(), types.kind(b).clone());
        match (&ka, &kb) {
            (TyKind::Error, _) | (_, TyKind::Error) => Ok(()),
            (TyKind::Var(x), TyKind::Var(y)) => {
                let (kx, ky) = (self.vars[x.index()].kind, self.vars[y.index()].kind);
                // Keep the more specific kind: Float beats Int beats General.
                let rank = |k: VarKind| match k {
                    VarKind::General => 0,
                    VarKind::Int => 1,
                    VarKind::Float => 2,
                };
                if rank(kx) >= rank(ky) {
                    self.vars[y.index()].bound = Some(a);
                } else {
                    self.vars[x.index()].bound = Some(b);
                }
                Ok(())
            }
            (TyKind::Var(x), _) => self.bind(types, *x, b),
            (_, TyKind::Var(y)) => self.bind(types, *y, a),
            (TyKind::Tuple(xs), TyKind::Tuple(ys)) if xs.len() == ys.len() => {
                for (&x, &y) in xs.iter().zip(ys) {
                    self.unify_in(types, x, y, done)?;
                }
                Ok(())
            }
            (TyKind::Adt(x, xs), TyKind::Adt(y, ys)) if x == y && xs.len() == ys.len() => {
                for (&x, &y) in xs.iter().zip(ys) {
                    self.unify_in(types, x, y, done)?;
                }
                Ok(())
            }
            (TyKind::Opaque(x, xs), TyKind::Opaque(y, ys))
            | (TyKind::FnDef(x, xs), TyKind::FnDef(y, ys))
                if x == y && xs.len() == ys.len() =>
            {
                for (&x, &y) in xs.iter().zip(ys) {
                    self.unify_in(types, x, y, done)?;
                }
                Ok(())
            }
            (TyKind::Closure(x, xs), TyKind::Closure(y, ys)) if x == y => {
                for (&x, &y) in xs.iter().zip(ys) {
                    self.unify_in(types, x, y, done)?;
                }
                Ok(())
            }
            (TyKind::Array(x, n), TyKind::Array(y, m)) if n == m => {
                self.unify_in(types, *x, *y, done)
            }
            (TyKind::Slice(x), TyKind::Slice(y)) => self.unify_in(types, *x, *y, done),
            (TyKind::FnPtr(xp, xr), TyKind::FnPtr(yp, yr)) if xp.len() == yp.len() => {
                for (&x, &y) in xp.iter().zip(yp) {
                    self.unify_in(types, x, y, done)?;
                }
                self.unify_in(types, *xr, *yr, done)
            }
            (
                TyKind::Projection { self_ty: s1, trait_: t1, trait_args: a1, name: n1 },
                TyKind::Projection { self_ty: s2, trait_: t2, trait_args: a2, name: n2 },
            ) if t1 == t2 && n1 == n2 && a1.len() == a2.len() => {
                self.unify_in(types, *s1, *s2, done)?;
                for (&x, &y) in a1.iter().zip(a2) {
                    self.unify_in(types, x, y, done)?;
                }
                Ok(())
            }
            _ => Err(Mismatch),
        }
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
        let resolved = self.resolve(types, t);
        if types.any(resolved, &mut |k| matches!(k, TyKind::Var(x) if *x == v)) {
            return Err(Mismatch);
        }
        self.vars[v.index()].bound = Some(t);
        Ok(())
    }
}
