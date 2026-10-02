//! Single-exit form, for intervals: every `return` becomes a store to a result local and a
//! `done` flag, and the code after it runs under `if !done`. An interval evaluation may have to
//! take both sides of a branch that depends on the input, which a real `return` would cut short;
//! with one exit, both sides' results meet in the result local and are hulled there.

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
    let mut nf = f.clone();
    let done = nf.new_local("done", bool_ty);
    let ret = f.ret.map(|t| nf.new_local("result", t));
    let body = std::mem::take(&mut nf.body);
    let mut cx = Cx { f: &mut nf, done, ret, bool_ty };
    let no = cx.f.new_value(bool_ty);
    let mut out =
        vec![Stmt::Let(no, Expr::Const(Const::Bool(false))), Stmt::Store(Place::local(done), no)];
    out.extend(cx.block(&body, false));
    match ret {
        Some(r) => {
            let t = f.ret.unwrap_or(bool_ty);
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
    b.iter().any(|s| match s {
        Stmt::Return(_) => true,
        Stmt::If { then, else_, .. } => returns(then) || returns(else_),
        Stmt::Loop { body, continuing } => returns(body) || returns(continuing),
        _ => false,
    })
}

struct Cx<'a> {
    f: &'a mut Function,
    done: LocalId,
    ret: Option<LocalId>,
    bool_ty: TypeId,
}

impl Cx<'_> {
    fn block(&mut self, b: &[Stmt], in_loop: bool) -> Block {
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
                    if in_loop {
                        out.push(Stmt::Break);
                    }
                    // What follows a return is dead.
                    return out;
                }
                Stmt::If { cond, then, else_ } if returns(std::slice::from_ref(s)) => {
                    let then = self.block(then, in_loop);
                    let else_ = self.block(else_, in_loop);
                    out.push(Stmt::If { cond: *cond, then, else_ });
                    self.rest(&b[i + 1..], in_loop, &mut out);
                    return out;
                }
                Stmt::Loop { body, continuing } if returns(body) => {
                    let body = self.block(body, true);
                    out.push(Stmt::Loop { body, continuing: continuing.clone() });
                    self.rest(&b[i + 1..], in_loop, &mut out);
                    return out;
                }
                other => out.push(other.clone()),
            }
        }
        out
    }

    /// The statements after one that may have returned: in a loop, leave it if it did; then run
    /// the rest only if it didn't.
    fn rest(&mut self, rest: &[Stmt], in_loop: bool, out: &mut Block) {
        let d = self.f.new_value(self.bool_ty);
        out.push(Stmt::Let(d, Expr::Load(Place::local(self.done))));
        if in_loop {
            out.push(Stmt::If { cond: d, then: vec![Stmt::Break], else_: Vec::new() });
        }
        if rest.is_empty() {
            return;
        }
        let rest = self.block(rest, in_loop);
        if in_loop {
            out.extend(rest);
        } else {
            out.push(Stmt::If { cond: d, then: Vec::new(), else_: rest });
        }
    }
}
