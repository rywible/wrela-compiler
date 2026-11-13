//! Range proofs for loops (plan stage L.1; the question of Bodík, Gupta and Sarkar's "ABCD:
//! eliminating array bounds checks on demand", 2000, answered for one shape only).
//!
//! The shape: a loop whose body begins with the counter's load `i`, `i >= bound` and a `break`,
//! and whose `continuing` adds one to the counter, with no other store to the counter. Inside
//! its body, `i < bound`. So:
//! - a check `if i >= n { panic }` (what lowering's `check_below` makes) is removed when `n` is
//!   `bound` itself, or both are loads of one place that nothing in the loop can write: no store
//!   to it, no call lent a place, no memory operation that writes, and, where the place is
//!   reached through a pointer (or its root's address is taken), no store through a pointer;
//! - the counter's `+ 1` becomes `WrappingAdd`: it is below `bound`, so it can't overflow.
//!
//! Nothing else. Run in `inline_cpu` after inlining; each removal is counted.

use crate::{BinOp, Block, Expr, MemOp, Place, PlaceRoot, Stmt, ValueId};
use std::collections::{HashMap, HashSet};

/// Proves what it can in each function: how many checks it removed.
pub fn prove(m: &mut crate::Module) -> u32 {
    let mut removed = 0;
    for f in &mut m.functions {
        let mut defs = HashMap::new();
        collect_defs(&f.body, &mut defs);
        let mut addressed = HashSet::new();
        collect_addressed(&f.body, &mut addressed);
        let body = std::mem::take(&mut f.body);
        let cx = Cx { defs: &defs, addressed: &addressed };
        f.body = cx.block(body, &mut removed);
    }
    removed
}

/// Each value's definition, for telling what a bound is a load of.
fn collect_defs(b: &Block, out: &mut HashMap<ValueId, Expr>) {
    for s in b {
        match s {
            Stmt::Let(v, e) => {
                out.insert(*v, e.clone());
            }
            Stmt::If { then, else_, .. } => {
                collect_defs(then, out);
                collect_defs(else_, out);
            }
            Stmt::Loop { body, continuing } => {
                collect_defs(body, out);
                collect_defs(continuing, out);
            }
            _ => {}
        }
    }
}

/// The roots whose places have their address taken somewhere in the function.
fn collect_addressed(b: &Block, out: &mut HashSet<PlaceRoot>) {
    for s in b {
        match s {
            Stmt::Let(_, Expr::Addr(p)) | Stmt::Eval(Expr::Addr(p)) => {
                out.insert(p.root.clone());
            }
            Stmt::If { then, else_, .. } => {
                collect_addressed(then, out);
                collect_addressed(else_, out);
            }
            Stmt::Loop { body, continuing } => {
                collect_addressed(body, out);
                collect_addressed(continuing, out);
            }
            _ => {}
        }
    }
}

/// What a loop may write.
#[derive(Default)]
struct Writes {
    places: Vec<Place>,
    through_pointers: bool,
    anything: bool,
}

fn collect_writes(b: &Block, w: &mut Writes) {
    for s in b {
        match s {
            Stmt::Store(p, _) => {
                if matches!(p.root, PlaceRoot::Ptr(_)) {
                    w.through_pointers = true;
                }
                w.places.push(p.clone());
            }
            Stmt::Let(_, e) | Stmt::Eval(e) => match e {
                Expr::Call(_, args) => {
                    if args.iter().any(|a| matches!(a, crate::Arg::Place(_))) {
                        w.anything = true;
                    }
                }
                Expr::Mem(op, _) => {
                    if matches!(op, MemOp::Store(_) | MemOp::Copy | MemOp::Fill | MemOp::Cas) {
                        w.anything = true;
                    }
                }
                _ => {}
            },
            Stmt::If { then, else_, .. } => {
                collect_writes(then, w);
                collect_writes(else_, w);
            }
            Stmt::Loop { body, continuing } => {
                collect_writes(body, w);
                collect_writes(continuing, w);
            }
            _ => {}
        }
    }
}

/// Whether one place is within the other (a write to either changes the other).
fn overlaps(a: &Place, b: &Place) -> bool {
    a.root == b.root && a.path.iter().zip(&b.path).all(|(x, y)| x == y)
}

struct Cx<'a> {
    defs: &'a HashMap<ValueId, Expr>,
    addressed: &'a HashSet<PlaceRoot>,
}

/// A loop of the shape: the counter's local, the body's first load of it, and the bound.
struct Shape {
    counter: crate::LocalId,
    i: ValueId,
    bound: ValueId,
}

impl Cx<'_> {
    fn block(&self, b: Block, removed: &mut u32) -> Block {
        let mut out = Vec::new();
        for s in b {
            match s {
                Stmt::If { cond, then, else_ } => {
                    let then = self.block(then, removed);
                    let else_ = self.block(else_, removed);
                    out.push(Stmt::If { cond, then, else_ });
                }
                Stmt::Loop { body, continuing } => {
                    let (body, continuing) = match self.shape(&body, &continuing) {
                        Some(shape) => self.prove_loop(&shape, body, continuing, removed),
                        None => (body, continuing),
                    };
                    let body = self.block(body, removed);
                    let continuing = self.block(continuing, removed);
                    out.push(Stmt::Loop { body, continuing });
                }
                other => out.push(other),
            }
        }
        out
    }

    /// The loop's counter, its load and its bound, if the loop has the shape.
    fn shape(&self, body: &Block, continuing: &Block) -> Option<Shape> {
        let mut it = body.iter().filter(|s| !matches!(s, Stmt::At(_)));
        let Some(Stmt::Let(i, Expr::Load(p))) = it.next() else { return None };
        let PlaceRoot::Local(counter) = p.root else { return None };
        if !p.path.is_empty() {
            return None;
        }
        let Some(Stmt::Let(ge, Expr::Binary(BinOp::Ge, a, bound))) = it.next() else {
            return None;
        };
        if a != i {
            return None;
        }
        let Some(Stmt::If { cond, then, else_ }) = it.next() else { return None };
        let only_break = then.iter().filter(|s| !matches!(s, Stmt::At(_))).eq([Stmt::Break].iter());
        if cond != ge || !only_break || else_.iter().any(|s| !matches!(s, Stmt::At(_))) {
            return None;
        }
        // The continuing adds one: `x = load c; one = 1; y = x + 1; c = y`, and nothing else
        // stores to the counter.
        let cont: Vec<&Stmt> = continuing.iter().filter(|s| !matches!(s, Stmt::At(_))).collect();
        let [
            Stmt::Let(x, Expr::Load(cp)),
            Stmt::Let(one, Expr::Const(crate::Const::U32(1))),
            Stmt::Let(y, Expr::Binary(BinOp::Add, xa, ob)),
            Stmt::Store(sp, sv),
        ] = cont.as_slice()
        else {
            return None;
        };
        if cp.root != PlaceRoot::Local(counter)
            || !cp.path.is_empty()
            || xa != x
            || ob != one
            || sp.root != PlaceRoot::Local(counter)
            || sv != y
        {
            return None;
        }
        let mut w = Writes::default();
        collect_writes(body, &mut w);
        if w.places.iter().any(|p| p.root == PlaceRoot::Local(counter)) {
            return None;
        }
        Some(Shape { counter, i: *i, bound: *bound })
    }

    /// Whether `n` is the loop's bound: itself, or a load of the place the bound is a load of,
    /// which nothing in the loop can write.
    fn same_bound(&self, s: &Shape, n: ValueId, w: &Writes) -> bool {
        if n == s.bound {
            return true;
        }
        let (Some(Expr::Load(pn)), Some(Expr::Load(pb))) =
            (self.defs.get(&n), self.defs.get(&s.bound))
        else {
            return false;
        };
        if pn != pb || w.anything {
            return false;
        }
        if w.places.iter().any(|q| overlaps(q, pn)) {
            return false;
        }
        let through = matches!(pn.root, PlaceRoot::Ptr(_)) || self.addressed.contains(&pn.root);
        !(through && w.through_pointers)
    }

    fn prove_loop(
        &self,
        s: &Shape,
        body: Block,
        continuing: Block,
        removed: &mut u32,
    ) -> (Block, Block) {
        let mut w = Writes::default();
        collect_writes(&body, &mut w);
        collect_writes(&continuing, &mut w);
        // The body's top level: a check of the counter's value against the bound.
        let mut out = Vec::new();
        let mut ges: HashMap<ValueId, ValueId> = HashMap::new();
        for st in body {
            match &st {
                Stmt::Let(v, Expr::Binary(BinOp::Ge, a, n)) if *a == s.i => {
                    ges.insert(*v, *n);
                    out.push(st);
                }
                Stmt::If { cond, then, else_ }
                    if else_.iter().all(|x| matches!(x, Stmt::At(_)))
                        && ges.get(cond).is_some_and(|n| self.same_bound(s, *n, &w))
                        && then
                            .iter()
                            .any(|x| matches!(x, Stmt::Eval(Expr::Mem(MemOp::Panic, _)))) =>
                {
                    *removed += 1;
                }
                _ => out.push(st),
            }
        }
        // The counter's `+ 1` can't overflow: it's below the bound.
        let continuing = continuing
            .into_iter()
            .map(|st| match st {
                Stmt::Let(y, Expr::Binary(BinOp::Add, a, b)) => {
                    Stmt::Let(y, Expr::Binary(BinOp::WrappingAdd, a, b))
                }
                other => other,
            })
            .collect();
        let _ = s.counter;
        (out, continuing)
    }
}
