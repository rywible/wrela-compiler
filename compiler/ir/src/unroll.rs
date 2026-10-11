//! Unrolling (GPU): a loop that counts a local from a constant by a constant step and leaves
//! only when the count reaches a constant bound, a few times, is written out once per pass.
//!
//! Why: wgpu bounds every WGSL loop it translates for Metal (a counter that breaks out after
//! 2⁶⁴ passes, so a loop can't hang the GPU), and Metal's compiler then keeps the loop: the
//! temporal resolve's 2 × 2 reconstruction as a loop took 0.88 ms at 1080p, written out 0.53.
//! Written out here, each pass's index is a constant the shader compiler folds.
//!
//! A loop is unrolled when:
//! - its body starts with pure computations of its counter and constants, then
//!   `if cond { break }`, and has no other `break` or `continue` of its own;
//! - its counter is a local, stored a constant just before the loop, written in the loop only
//!   by one store in its continuing block (a pure function of the counter and constants), and
//!   never lent;
//! - it runs at most `MOST_PASSES` times, and its copies come to at most `MOST_STATEMENTS`.
//!
//! The copies keep the counter's stores, so the counter holds what it would after the loop;
//! inner loops are unrolled first.

use crate::*;
use std::collections::HashMap;

/// The most passes a loop is unrolled for, and the most statements its copies may come to.
const MOST_PASSES: u32 = 16;
const MOST_STATEMENTS: usize = 2000;

/// Unrolls `f`'s loops that qualify (see the module docs).
pub(crate) fn unroll(f: &mut Function) {
    let mut consts = HashMap::new();
    visit::walk(&f.body, &mut |s| {
        if let Stmt::Let(v, Expr::Const(c)) = s {
            consts.insert(*v, c.clone());
        }
    });
    let body = std::mem::take(&mut f.body);
    f.body = block(f, body, &consts);
}

fn block(f: &mut Function, b: Block, consts: &HashMap<ValueId, Const>) -> Block {
    let mut out: Block = Vec::with_capacity(b.len());
    for s in b {
        match s {
            Stmt::If { cond, then, else_ } => {
                let then = block(f, then, consts);
                let else_ = block(f, else_, consts);
                out.push(Stmt::If { cond, then, else_ });
            }
            Stmt::Loop { body, continuing } => {
                let body = block(f, body, consts);
                let continuing = block(f, continuing, consts);
                match plan(&out, &body, &continuing, consts) {
                    Some(passes) => {
                        for _ in 0..passes {
                            let mut names = HashMap::new();
                            for s in body.iter().filter(|s| !is_exit_test(s)) {
                                out.push(copy(f, s, &mut names));
                            }
                            for s in &continuing {
                                out.push(copy(f, s, &mut names));
                            }
                        }
                    }
                    None => out.push(Stmt::Loop { body, continuing }),
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// How many times the loop runs, if it qualifies; `before` is the code before it in its block.
fn plan(
    before: &[Stmt],
    body: &Block,
    continuing: &Block,
    consts: &HashMap<ValueId, Const>,
) -> Option<u32> {
    // The exit test: pure statements, then `if cond { break }`.
    let test = body.iter().position(is_exit_test)?;
    let Stmt::If { cond, .. } = &body[test] else { return None };
    let prefix = &body[..test];
    if !prefix.iter().all(|s| matches!(s, Stmt::At(_)) || matches!(s, Stmt::Let(_, e) if pure(e))) {
        return None;
    }
    // The counter: the one local the test reads.
    let mut counter = None;
    for s in prefix {
        if let Stmt::Let(_, Expr::Load(p)) = s {
            let l = whole_local(p)?;
            if counter.is_some_and(|c| c != l) {
                return None;
            }
            counter = Some(l);
        }
    }
    let counter = counter?;
    // No other exit of its own, nothing else writes or lends the counter, and the continuing
    // block's one store of it is the step.
    let rest = &body[test + 1..];
    if exits(rest) || exits(continuing) {
        return None;
    }
    let mut step = None;
    for s in continuing {
        if let Stmt::Store(p, v) = s
            && whole_local(p) == Some(counter)
        {
            if step.is_some() {
                return None;
            }
            step = Some(*v);
        }
    }
    let step = step?;
    let others_write = |b: &[Stmt], allow_step: bool| {
        let mut bad = false;
        visit::walk(b, &mut |s| {
            match s {
                Stmt::Store(p, v) if p.root_local() == Some(counter) => {
                    bad |= !(allow_step && *v == step && p.path.is_empty());
                }
                _ => {}
            }
            if let Some(e) = s.expr() {
                e.for_each_place(&mut |p| {
                    let lent = matches!(e, Expr::Addr(_) | Expr::Call(..) | Expr::Atomic(..));
                    bad |= lent && p.root_local() == Some(counter);
                });
            }
        });
        bad
    };
    if others_write(rest, false) || others_write(continuing, true) {
        return None;
    }
    // The counter's start: the last store of it before the loop, with nothing between that
    // mentions it.
    let mut start = None;
    for s in before.iter().rev() {
        if let Stmt::Store(p, v) = s
            && whole_local(p) == Some(counter)
        {
            start = Some(consts.get(v)?.clone());
            break;
        }
        if mentions(s, counter) {
            return None;
        }
    }
    let mut k = start?;
    // Run the test and the step on the counter alone.
    let defs: HashMap<ValueId, &Expr> = prefix
        .iter()
        .chain(continuing.iter())
        .filter_map(|s| match s {
            Stmt::Let(v, e) => Some((*v, e)),
            _ => None,
        })
        .collect();
    let size = crate::opt::size_of(body) + crate::opt::size_of(continuing);
    let mut passes = 0;
    loop {
        match eval(*cond, &k, counter, &defs, consts)? {
            Const::Bool(true) => return Some(passes),
            Const::Bool(false) => {}
            _ => return None,
        }
        passes += 1;
        if passes > MOST_PASSES || passes as usize * size > MOST_STATEMENTS {
            return None;
        }
        k = eval(step, &k, counter, &defs, consts)?;
    }
}

/// `if cond { break }`, with nothing else in either branch but source locations.
fn is_exit_test(s: &Stmt) -> bool {
    let Stmt::If { then, else_, .. } = s else { return false };
    let only = |b: &Block, want: bool| {
        let mut breaks = 0;
        for s in b {
            match s {
                Stmt::At(_) => {}
                Stmt::Break => breaks += 1,
                _ => return false,
            }
        }
        breaks == usize::from(want)
    };
    only(then, true) && only(else_, false)
}

/// Whether code leaves the loop it's in by its own `break`, `continue` (not an inner loop's).
fn exits(b: &[Stmt]) -> bool {
    b.iter().any(|s| match s {
        Stmt::Break | Stmt::Continue => true,
        Stmt::If { then, else_, .. } => exits(then) || exits(else_),
        _ => false,
    })
}

fn whole_local(p: &Place) -> Option<LocalId> {
    if p.path.is_empty() { p.root_local() } else { None }
}

fn mentions(s: &Stmt, l: LocalId) -> bool {
    visit::any(std::slice::from_ref(s), &mut |s| {
        let mut hit = false;
        s.for_each_place(&mut |p| hit |= p.root_local() == Some(l));
        hit
    })
}

/// What the exit test and the step may compute: loads, constants, arithmetic, comparisons.
fn pure(e: &Expr) -> bool {
    matches!(e, Expr::Load(_) | Expr::Const(_) | Expr::Binary(..) | Expr::Unary(..))
}

/// Value `v` when the counter holds `k`.
fn eval(
    v: ValueId,
    k: &Const,
    counter: LocalId,
    defs: &HashMap<ValueId, &Expr>,
    consts: &HashMap<ValueId, Const>,
) -> Option<Const> {
    if let Some(c) = consts.get(&v) {
        return Some(c.clone());
    }
    let go = |x: ValueId| eval(x, k, counter, defs, consts);
    match defs.get(&v)? {
        Expr::Load(p) if whole_local(p) == Some(counter) => Some(k.clone()),
        Expr::Unary(UnOp::Not, x) => match go(*x)? {
            Const::Bool(b) => Some(Const::Bool(!b)),
            _ => None,
        },
        Expr::Binary(op, a, b) => binary(*op, go(*a)?, go(*b)?),
        _ => None,
    }
}

/// An integer step or test, on the counter's type (`i32` or `u32`), as the GPU computes it.
fn binary(op: BinOp, a: Const, b: Const) -> Option<Const> {
    use std::cmp::Ordering;
    let (ord, sum, diff): (Ordering, Const, Const) = match (a, b) {
        (Const::I32(x), Const::I32(y)) => {
            (x.cmp(&y), Const::I32(x.wrapping_add(y)), Const::I32(x.wrapping_sub(y)))
        }
        (Const::U32(x), Const::U32(y)) => {
            (x.cmp(&y), Const::U32(x.wrapping_add(y)), Const::U32(x.wrapping_sub(y)))
        }
        (Const::Bool(x), Const::Bool(y)) => {
            return match op {
                BinOp::And => Some(Const::Bool(x && y)),
                BinOp::Or => Some(Const::Bool(x || y)),
                BinOp::Eq => Some(Const::Bool(x == y)),
                BinOp::Ne => Some(Const::Bool(x != y)),
                _ => None,
            };
        }
        _ => return None,
    };
    Some(match op {
        BinOp::Add | BinOp::WrappingAdd => sum,
        BinOp::Sub | BinOp::WrappingSub => diff,
        BinOp::Eq => Const::Bool(ord == Ordering::Equal),
        BinOp::Ne => Const::Bool(ord != Ordering::Equal),
        BinOp::Lt => Const::Bool(ord == Ordering::Less),
        BinOp::Le => Const::Bool(ord != Ordering::Greater),
        BinOp::Gt => Const::Bool(ord == Ordering::Greater),
        BinOp::Ge => Const::Bool(ord != Ordering::Less),
        _ => return None,
    })
}

/// A copy of `s` whose values are new: each value it defines gets a new name, in `names`, and
/// the values it uses go by theirs.
fn copy(f: &mut Function, s: &Stmt, names: &mut HashMap<ValueId, ValueId>) -> Stmt {
    // Copy control flow's children once. Cloning the whole statement before copying each
    // child again would copy every nested block once for each block around it.
    let mut out = match s {
        Stmt::If { cond, .. } => Stmt::If { cond: *cond, then: Vec::new(), else_: Vec::new() },
        Stmt::Loop { .. } => Stmt::Loop { body: Vec::new(), continuing: Vec::new() },
        other => other.clone(),
    };
    out.for_each_value_mut(&mut |x| {
        if let Some(n) = names.get(x) {
            *x = *n;
        }
    });
    match (s, &mut out) {
        (_, Stmt::Let(v, _)) => {
            let n = f.new_value(f.value_ty(*v));
            names.insert(*v, n);
            *v = n;
        }
        (Stmt::If { then, else_, .. }, Stmt::If { then: out_then, else_: out_else, .. }) => {
            *out_then = then.iter().map(|s| copy(f, s, names)).collect();
            *out_else = else_.iter().map(|s| copy(f, s, names)).collect();
        }
        (Stmt::Loop { body, continuing }, Stmt::Loop { body: out_body, continuing: out_cont }) => {
            *out_body = body.iter().map(|s| copy(f, s, names)).collect();
            *out_cont = continuing.iter().map(|s| copy(f, s, names)).collect();
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_copy(
        f: &mut Function,
        s: &Stmt,
        names: &mut HashMap<ValueId, ValueId>,
        cloned: &mut usize,
    ) -> Stmt {
        visit::walk(std::slice::from_ref(s), &mut |_| *cloned += 1);
        let mut s = s.clone();
        s.for_each_value_mut(&mut |x| {
            if let Some(n) = names.get(x) {
                *x = *n;
            }
        });
        match &mut s {
            Stmt::Let(v, _) => {
                let n = f.new_value(f.value_ty(*v));
                names.insert(*v, n);
                *v = n;
            }
            Stmt::If { then, else_, .. } => {
                *then = then.iter().map(|s| old_copy(f, s, names, cloned)).collect();
                *else_ = else_.iter().map(|s| old_copy(f, s, names, cloned)).collect();
            }
            Stmt::Loop { body, continuing } => {
                *body = body.iter().map(|s| old_copy(f, s, names, cloned)).collect();
                *continuing = continuing.iter().map(|s| old_copy(f, s, names, cloned)).collect();
            }
            _ => {}
        }
        s
    }

    #[test]
    fn codegen_perf_unroll_copies_nested_control_flow_once() {
        let mut types = Types::default();
        let (u, b) = (types.u32(), types.bool());
        let mut f = Function::new("nested", Vec::new(), None);
        let local = f.new_local("counter", u);
        let (cond, value) = (f.new_value(b), f.new_value(u));
        let mut nested = Stmt::Store(Place::local(local), value);
        for _ in 0..12 {
            let next = f.new_value(u);
            nested = Stmt::If {
                cond,
                then: vec![Stmt::Let(next, Expr::Binary(BinOp::Add, value, value)), nested],
                else_: vec![Stmt::Store(Place::local(local), value)],
            };
        }
        let source = Stmt::Loop {
            body: vec![
                Stmt::Let(cond, Expr::Const(Const::Bool(true))),
                Stmt::Let(value, Expr::Const(Const::U32(3))),
                nested,
            ],
            continuing: vec![Stmt::Store(Place::local(local), value)],
        };
        let mut expected_f = f.clone();
        let (mut names, mut expected_names) = (HashMap::new(), HashMap::new());
        let mut cloned = 0;
        let expected = old_copy(&mut expected_f, &source, &mut expected_names, &mut cloned);
        let actual = copy(&mut f, &source, &mut names);
        assert_eq!(actual, expected);
        assert_eq!(f, expected_f);
        assert_eq!(names, expected_names);
        let mut statements = 0;
        visit::walk(std::slice::from_ref(&source), &mut |_| statements += 1);
        assert_eq!(statements, 41);
        assert_eq!(cloned, 315);
    }
}
