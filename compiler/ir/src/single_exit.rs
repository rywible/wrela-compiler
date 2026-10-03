//! Single-exit form: every `return` becomes a store to a result local and a `done` flag, and the
//! code after it runs under `if !done`. Two transforms need one exit:
//!
//! - **Intervals** ([`derive::interval`](crate::derive::interval)): an interval evaluation may
//!   have to take both sides of a branch that depends on the input, which a real `return` would
//!   cut short; with one exit, both sides' results meet in the result local and are hulled there.
//! - **Inlining** ([`opt`](crate::opt)): an inlined body ends where the caller's code goes on.

use crate::*;

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
    let mut cx = Cx { f: &mut nf, done, ret, bool_ty, loops: Vec::new() };
    let no = cx.f.new_value(bool_ty);
    let mut out =
        vec![Stmt::Let(no, Expr::Const(Const::Bool(false))), Stmt::Store(Place::local(done), no)];
    out.extend(cx.block(&f.body));
    match ret {
        Some(r) => {
            let t = ret_ty.unwrap_or(bool_ty);
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

struct Cx<'a> {
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
    fn block(&mut self, b: &[Stmt]) -> Block {
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
        let rest = self.block(rest);
        out.push(Stmt::If { cond: d, then: Vec::new(), else_: rest });
    }
}
