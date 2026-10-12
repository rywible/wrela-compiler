//! Workgroup memory's phases (language.md §6.13). Between barriers, each invocation writes only
//! its own chunk and may read any element, so reading what another invocation may still be
//! writing, or writing what another may still be reading, needs a barrier in between. This finds
//! each read or write of workgroup memory reached, on some path through the kernel (its calls
//! included), with no barrier since an access of the other kind.
//!
//! It doesn't look at indexes: an invocation that reads back an element of its own chunk needs
//! a barrier too. That's stricter than it must be, never looser.

use crate::*;
use std::collections::HashMap;
use wrela_diag::Span;

/// A read or write of workgroup memory with no barrier since an access of the other kind.
#[derive(Clone, Debug, PartialEq)]
pub struct Hazard {
    pub resource: ResourceId,
    /// Whether this access is the write (after a read); else it's the read (after a write).
    pub write: bool,
    pub func: FuncId,
    /// Where it is: its function's last `Stmt::At` before it.
    pub at: Option<Span>,
}

/// What has happened to each workgroup resource since the last barrier.
const WRITTEN: u8 = 1;
const READ: u8 = 2;
type State = Vec<u8>;

fn join(a: &mut State, b: &State) {
    for (x, y) in a.iter_mut().zip(b) {
        *x |= *y;
    }
}

fn join_exit(a: &mut Option<State>, b: &State) {
    match a {
        Some(a) => join(a, b),
        None => *a = Some(b.clone()),
    }
}

#[derive(Default)]
struct LoopExits {
    breaks: Option<State>,
    continues: Option<State>,
}

/// The hazards of the kernel whose entry point's function is `entry`.
pub fn hazards(m: &Module, entry: FuncId) -> Vec<Hazard> {
    let wg: Vec<bool> = m.resources.iter().map(|r| r.kind == ResourceKind::Workgroup).collect();
    if !wg.contains(&true) {
        return Vec::new();
    }
    let mut an = Analysis { m, wg, memo: HashMap::new(), out: Vec::new() };
    let start = vec![0; m.resources.len()];
    an.func(entry, start);
    an.out
}

struct Analysis<'m> {
    m: &'m Module,
    wg: Vec<bool>,
    /// Each function's end state, by the state it starts in.
    memo: HashMap<(FuncId, State), State>,
    out: Vec<Hazard>,
}

impl Analysis<'_> {
    fn func(&mut self, f: FuncId, st: State) -> State {
        if let Some(end) = self.memo.get(&(f, st.clone())) {
            return end.clone();
        }
        // On the GPU there's no recursion; this keeps a malformed module from looping.
        self.memo.insert((f, st.clone()), st.clone());
        // What it may have done by its `return`s, joined with what it does by its end.
        let mut walk =
            Walk { an: self, func: f, at: None, ret: vec![0; st.len()], loops: Vec::new() };
        let mut s = st.clone();
        let body = &walk.an.m.functions[f.index()].body;
        let falls_through = walk.block(body, &mut s);
        let mut end = walk.ret;
        if falls_through {
            join(&mut end, &s);
        }
        self.memo.insert((f, st), end.clone());
        end
    }

    fn report(&mut self, h: Hazard) {
        if !self.out.iter().any(|o| o.func == h.func && o.at == h.at && o.resource == h.resource) {
            self.out.push(h);
        }
    }
}

struct Walk<'a, 'm> {
    an: &'a mut Analysis<'m>,
    func: FuncId,
    at: Option<Span>,
    /// What the function may have done by any `return`.
    ret: State,
    loops: Vec<LoopExits>,
}

impl Walk<'_, '_> {
    /// Whether this path reaches the end of the block. Exits keep the state at the exit,
    /// before any barrier in the skipped part of the block.
    fn block(&mut self, b: &[Stmt], st: &mut State) -> bool {
        for s in b {
            if !self.stmt(s, st) {
                return false;
            }
        }
        true
    }

    /// A workgroup resource a place is in, if it's in one.
    fn resource(&self, p: &Place) -> Option<ResourceId> {
        match p.root {
            PlaceRoot::Resource(r) if self.an.wg[r.index()] => Some(r),
            _ => None,
        }
    }

    fn access(&mut self, r: ResourceId, write: bool, st: &mut State) {
        let (mine, theirs) = if write { (WRITTEN, READ) } else { (READ, WRITTEN) };
        if st[r.index()] & theirs != 0 {
            let h = Hazard { resource: r, write, func: self.func, at: self.at };
            self.an.report(h);
        }
        st[r.index()] |= mine;
    }

    fn expr(&mut self, e: &Expr, st: &mut State) {
        match e {
            Expr::Barrier => st.iter_mut().for_each(|x| *x = 0),
            Expr::Call(g, _) => {
                let end = self.an.func(*g, st.clone());
                *st = end;
            }
            _ => {
                let mut read = Vec::new();
                e.for_each_place(&mut |p| read.extend(self.resource(p)));
                for r in read {
                    self.access(r, false, st);
                }
            }
        }
    }

    fn stmt(&mut self, s: &Stmt, st: &mut State) -> bool {
        match s {
            Stmt::Let(_, e) | Stmt::Eval(e) => self.expr(e, st),
            Stmt::Store(p, _) => {
                if let Some(r) = self.resource(p) {
                    self.access(r, true, st);
                }
            }
            Stmt::If { then, else_, .. } => {
                let mut b = st.clone();
                let a_next = self.block(then, st);
                let b_next = self.block(else_, &mut b);
                match (a_next, b_next) {
                    (true, true) => join(st, &b),
                    (false, true) => *st = b,
                    _ => {}
                }
                return a_next || b_next;
            }
            Stmt::Loop { body, continuing } => {
                // A continue reaches the continuing block; a break skips it and everything
                // after the break. Keep each exit separately until the loop head stops growing.
                self.loops.push(LoopExits::default());
                let mut head = st.clone();
                loop {
                    let mut s = head.clone();
                    let falls_through = self.block(body, &mut s);
                    let exits = self.loops.last().expect("in a loop");
                    let mut back = exits.continues.clone();
                    if falls_through {
                        join_exit(&mut back, &s);
                    }
                    let mut next = head.clone();
                    if let Some(mut s) = back
                        && self.block(continuing, &mut s)
                    {
                        join(&mut next, &s);
                    }
                    if next == head {
                        break;
                    }
                    head = next;
                }
                let exits = self.loops.pop().unwrap_or_default();
                if let Some(breaks) = exits.breaks {
                    *st = breaks;
                    return true;
                }
                return false;
            }
            Stmt::Return(_) => {
                join(&mut self.ret, st);
                return false;
            }
            Stmt::At(span) => self.at = Some(*span),
            Stmt::Break | Stmt::Continue => {
                if let Some(exits) = self.loops.last_mut() {
                    let into = if matches!(s, Stmt::Break) {
                        &mut exits.breaks
                    } else {
                        &mut exits.continues
                    };
                    join_exit(into, st);
                }
                return false;
            }
            // The WGSL back end emits nothing for a trap: valid GPU code never reaches it.
            Stmt::Trap => {}
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_exits_keep_accesses_before_skipped_barriers() {
        for (continue_, barrier_before_exit) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let mut m = Module::default();
            let u = m.types.u32();
            let b = m.types.bool();
            let array = m.types.intern(TypeDef::Array(u, 1));
            let shared = m.add_resource(Resource {
                name: "shared".into(),
                binding: 0,
                kind: ResourceKind::Workgroup,
                ty: array,
            });
            let mut f = Function::new("kernel", Vec::new(), None);
            let zero = f.new_value(u);
            let cond = f.new_value(b);
            let at = Place::root(PlaceRoot::Resource(shared)).with(Proj::Index(zero));
            let read = f.new_value(u);
            let mut body = Vec::new();
            if continue_ {
                body.push(Stmt::Let(read, Expr::Load(at.clone())));
                body.push(Stmt::Eval(Expr::Barrier));
            }
            body.push(Stmt::Store(at.clone(), zero));
            if barrier_before_exit {
                body.push(Stmt::Eval(Expr::Barrier));
            }
            body.extend([
                Stmt::If {
                    cond,
                    then: vec![if continue_ { Stmt::Continue } else { Stmt::Break }],
                    else_: Vec::new(),
                },
                Stmt::Eval(Expr::Barrier),
            ]);
            f.body = vec![
                Stmt::Let(zero, Expr::Const(Const::U32(0))),
                Stmt::Let(cond, Expr::Const(Const::Bool(true))),
                Stmt::Loop { body, continuing: Vec::new() },
            ];
            if !continue_ {
                f.body.push(Stmt::Let(read, Expr::Load(at)));
            }
            f.body.push(Stmt::Return(None));
            let entry = m.add_function(f);
            crate::verify(&m).expect("valid IR");
            assert_eq!(
                hazards(&m, entry).iter().any(|h| h.resource == shared && !h.write),
                !barrier_before_exit,
                "a {} must retain only accesses after the last barrier it reached",
                if continue_ { "continue" } else { "break" },
            );
        }
    }
}
