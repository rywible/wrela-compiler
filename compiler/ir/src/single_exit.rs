//! Single-exit form: every `return` becomes a store to a result local and a `done` flag, and the
//! code after it runs under `if !done`. Two transforms need one exit:
//!
//! - **Intervals** ([`derive::interval`](crate::derive::interval)): an interval evaluation may
//!   have to take both sides of a branch that depends on the input, which a real `return` would
//!   cut short; with one exit, both sides' results meet in the result local and are hulled there.
//! - **Inlining** ([`opt`](crate::opt)): an inlined body ends where the caller's code goes on.

use crate::*;
use std::collections::HashMap;

/// The transformed function, and its result local (when it returns a value). `None` when the
/// function already has one exit: its last statement.
pub(crate) fn single_exit(m: &mut Module, f: &Function) -> Option<(Function, Option<LocalId>)> {
    let tail = f.body.len().saturating_sub(1);
    let early = f.body.iter().enumerate().any(|(i, s)| match s {
        Stmt::Return(_) => i != tail,
        _ => returns(std::slice::from_ref(s)),
    });
    if !early {
        return None;
    }
    let bool_ty = m.types.bool();
    let mut nf = f.clone_signature();
    let done = nf.new_local("done", bool_ty);
    // A projection returns a pointer.
    let ret_ty = f.ret.map(|t| if f.ret_ref { m.types.intern(TypeDef::Ptr(t)) } else { t });
    let ret = ret_ty.map(|t| nf.new_local("result", t));
    let mut cx = Cx { types: &m.types, f: &mut nf, done, ret, bool_ty, loops: Vec::new() };
    let no = cx.f.new_value(bool_ty);
    let mut out =
        vec![Stmt::Let(no, Expr::Const(Const::Bool(false))), Stmt::Store(Place::local(done), no)];
    out.extend(cx.block(&f.body));
    match ret.zip(ret_ty) {
        Some((r, t)) => {
            let v = cx.f.new_value(t);
            out.push(Stmt::Let(v, Expr::Load(Place::local(r))));
            out.push(Stmt::Return(Some(v)));
        }
        None => out.push(Stmt::Return(None)),
    }
    nf.body = out;
    Some((nf, ret))
}

/// Whether a block contains a `return`, at any depth.
fn returns(b: &[Stmt]) -> bool {
    visit::any(b, &mut |s| matches!(s, Stmt::Return(_)))
}

/// How many statements that may return a run of statements takes before the rest of it starts
/// again at the run's level: each one puts the rest under one more `if !done`, and WGSL allows
/// 127 levels of braces in all (each level also indents every line under it).
const MAX_DEPTH: usize = 16;

struct Cx<'a> {
    types: &'a Types,
    f: &'a mut Function,
    done: LocalId,
    ret: Option<LocalId>,
    bool_ty: TypeId,
    /// A flag for each loop the code is in, innermost last: set by a `return` that leaves it.
    /// A loop's exit then reads only its own flag, which nothing outside the loop writes (an
    /// interval's activity would otherwise see the exit depend on whatever `done` does).
    loops: Vec<LocalId>,
}

impl Cx<'_> {
    /// `b` in single-exit form. Out of a loop, the statements go in parts of [`MAX_DEPTH`]
    /// statements that may return, each part after the first under its own `if !done`; a value
    /// that a later part uses goes there through a local.
    fn block(&mut self, b: &[Stmt]) -> Block {
        if !self.loops.is_empty() {
            return self.chain(b);
        }
        // What follows a return is dead.
        let b = match b.iter().position(|s| matches!(s, Stmt::Return(_))) {
            Some(i) => &b[..=i],
            None => b,
        };
        let may_return: Vec<usize> = (0..b.len())
            .filter(|&i| !matches!(b[i], Stmt::Return(_)) && returns(&b[i..=i]))
            .collect();
        if may_return.len() <= MAX_DEPTH {
            return self.chain(b);
        }
        // Each value `b` defines: where, and the last statement that uses it.
        let mut def = HashMap::new();
        let mut last_use = HashMap::new();
        for (i, s) in b.iter().enumerate() {
            let mut used = |v| {
                last_use.insert(v, i);
            };
            s.for_each_value(&mut used);
            for inner in s.blocks() {
                visit::walk(inner, &mut |t| t.for_each_value(&mut used));
            }
            if let Stmt::Let(v, _) = s {
                def.insert(*v, i);
            }
        }
        // The values defined before `at` and used from there on, in order.
        let crossing = |at: usize| {
            let mut vs: Vec<ValueId> = def
                .iter()
                .filter(|&(v, &d)| d < at && last_use.get(v).is_some_and(|&u| u >= at))
                .map(|(v, _)| *v)
                .collect();
            vs.sort_by_key(|v| v.0);
            vs
        };
        // Where each part after the first starts: after every `MAX_DEPTH` statements that may
        // return, or later, if a value that crosses there can't be held in a local.
        let mut starts = Vec::new();
        let mut since = 0;
        for &i in &may_return {
            since += 1;
            if since >= MAX_DEPTH
                && i + 1 < b.len()
                && crossing(i + 1).iter().all(|v| self.holdable(self.f.value_ty(*v)))
            {
                starts.push(i + 1);
                since = 0;
            }
        }
        if starts.is_empty() {
            return self.chain(b);
        }
        // The values that go through a local, in order, and the local each goes through.
        let mut held: Vec<(ValueId, LocalId)> = Vec::new();
        for &at in &starts {
            for v in crossing(at) {
                if !held.iter().any(|(h, _)| *h == v) {
                    held.push((v, self.f.new_local("held", self.f.value_ty(v))));
                }
            }
        }
        held.sort_by_key(|(v, _)| v.0);
        let holder: HashMap<ValueId, LocalId> = held.iter().copied().collect();
        let mut out = Vec::new();
        let bounds: Vec<usize> = std::iter::once(0).chain(starts).chain([b.len()]).collect();
        for w in bounds.windows(2) {
            let (from, to) = (w[0], w[1]);
            let mut part = Vec::new();
            // The held values defined before this part that it uses, each by a new name.
            let mut rename = HashMap::new();
            for &(v, l) in &held {
                if def[&v] < from && last_use[&v] >= from {
                    let nv = self.f.new_value(self.f.value_ty(v));
                    part.push(Stmt::Let(nv, Expr::Load(Place::local(l))));
                    rename.insert(v, nv);
                }
            }
            for s in &b[from..to] {
                let mut s = s.clone();
                let mut renamed = |v: &mut ValueId| {
                    if let Some(nv) = rename.get(v) {
                        *v = *nv;
                    }
                };
                s.for_each_value_mut(&mut renamed);
                for inner in s.blocks_mut() {
                    visit::walk_mut(inner, &mut |t| t.for_each_value_mut(&mut renamed));
                }
                let hold = match &s {
                    Stmt::Let(v, _) => holder.get(v).map(|&l| Stmt::Store(Place::local(l), *v)),
                    _ => None,
                };
                part.push(s);
                part.extend(hold);
            }
            let part = self.chain(&part);
            if from == 0 {
                out.extend(part);
            } else {
                let d = self.f.new_value(self.bool_ty);
                out.push(Stmt::Let(d, Expr::Load(Place::local(self.done))));
                out.push(Stmt::If { cond: d, then: Vec::new(), else_: part });
            }
        }
        out
    }

    /// Whether a local can hold a value of type `t`: plain data, not a pointer or a run.
    fn holdable(&self, t: TypeId) -> bool {
        match self.types.get(t) {
            TypeDef::Scalar(_) | TypeDef::Vector(_) | TypeDef::Matrix(_) => true,
            TypeDef::Struct { fields, .. } => fields.iter().all(|(_, f)| self.holdable(*f)),
            TypeDef::Enum { variants, .. } => {
                variants.iter().all(|(_, p)| p.is_none_or(|p| self.holdable(p)))
            }
            TypeDef::Array(e, _) => self.holdable(*e),
            TypeDef::RuntimeArray(_) | TypeDef::Run(_) | TypeDef::Ptr(_) | TypeDef::Atomic(_) => {
                false
            }
        }
    }

    /// `b` in single-exit form, the statements after each one that may return under it.
    fn chain(&mut self, b: &[Stmt]) -> Block {
        let mut out = Vec::new();
        for (i, s) in b.iter().enumerate() {
            match s {
                Stmt::Return(v) => {
                    if let (Some(v), Some(r)) = (v, self.ret) {
                        out.push(Stmt::Store(Place::local(r), *v));
                    }
                    let yes = self.f.new_value(self.bool_ty);
                    out.push(Stmt::Let(yes, Expr::Const(Const::Bool(true))));
                    out.push(Stmt::Store(Place::local(self.done), yes));
                    for l in &self.loops {
                        out.push(Stmt::Store(Place::local(*l), yes));
                    }
                    if !self.loops.is_empty() {
                        out.push(Stmt::Break);
                    }
                    // What follows a return is dead.
                    return out;
                }
                Stmt::If { cond, then, else_ } if returns(std::slice::from_ref(s)) => {
                    let then = self.block(then);
                    let else_ = self.block(else_);
                    out.push(Stmt::If { cond: *cond, then, else_ });
                    // In a loop, a `return` in it has already left the loop.
                    let flag = self.loops.is_empty().then_some(self.done);
                    self.rest(&b[i + 1..], flag, &mut out);
                    return out;
                }
                Stmt::Loop { body, continuing } if returns(body) => {
                    let left = self.f.new_local("left", self.bool_ty);
                    let no = self.f.new_value(self.bool_ty);
                    out.push(Stmt::Let(no, Expr::Const(Const::Bool(false))));
                    out.push(Stmt::Store(Place::local(left), no));
                    self.loops.push(left);
                    let body = self.block(body);
                    self.loops.pop();
                    out.push(Stmt::Loop { body, continuing: continuing.clone() });
                    self.rest(&b[i + 1..], Some(left), &mut out);
                    return out;
                }
                other => out.push(other.clone()),
            }
        }
        out
    }

    /// The statements after one that may have returned, which `flag` says (`None`: in a loop,
    /// where the return left it). In a loop, leave it too if it did; then run the rest only if
    /// it didn't.
    fn rest(&mut self, rest: &[Stmt], flag: Option<LocalId>, out: &mut Block) {
        let in_loop = !self.loops.is_empty();
        if in_loop {
            if let Some(l) = flag {
                let d = self.f.new_value(self.bool_ty);
                out.push(Stmt::Let(d, Expr::Load(Place::local(l))));
                out.push(Stmt::If { cond: d, then: vec![Stmt::Break], else_: Vec::new() });
            }
            out.extend(self.block(rest));
            return;
        }
        if rest.is_empty() {
            return;
        }
        let d = self.f.new_value(self.bool_ty);
        out.push(Stmt::Let(d, Expr::Load(Place::local(flag.unwrap_or(self.done)))));
        let rest = self.chain(rest);
        out.push(Stmt::If { cond: d, then: Vec::new(), else_: rest });
    }
}
