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
        let mut walk = Walk { an: self, func: f, at: None, ret: vec![0; st.len()] };
        let mut s = st.clone();
        let body = &walk.an.m.functions[f.index()].body;
        walk.block(body, &mut s);
        let mut end = walk.ret;
        join(&mut end, &s);
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
}

impl Walk<'_, '_> {
    fn block(&mut self, b: &[Stmt], st: &mut State) {
        for s in b {
            self.stmt(s, st);
        }
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

    fn stmt(&mut self, s: &Stmt, st: &mut State) {
        match s {
            Stmt::Let(_, e) | Stmt::Eval(e) => self.expr(e, st),
            Stmt::Store(p, _) => {
                if let Some(r) = self.resource(p) {
                    self.access(r, true, st);
                }
            }
            Stmt::If { then, else_, .. } => {
                let mut b = st.clone();
                self.block(then, st);
                self.block(else_, &mut b);
                join(st, &b);
            }
            Stmt::Loop { body, continuing } => {
                // Until what the loop's head may have seen stops growing; it leaves with all of
                // it (a `break` leaves from part way).
                let mut head = st.clone();
                let mut seen = st.clone();
                loop {
                    let mut s = head.clone();
                    self.block(body, &mut s);
                    join(&mut seen, &s);
                    self.block(continuing, &mut s);
                    join(&mut seen, &s);
                    let mut next = head.clone();
                    join(&mut next, &s);
                    if next == head {
                        break;
                    }
                    head = next;
                }
                *st = seen;
            }
            Stmt::Return(_) => join(&mut self.ret, st),
            Stmt::At(span) => self.at = Some(*span),
            Stmt::Break | Stmt::Continue | Stmt::Trap => {}
        }
    }
}
