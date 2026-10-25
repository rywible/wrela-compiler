//! Loops that leave early depending on the input, made intervals can bound (language.md
//! §14.2). An interval runs both sides of a branch on the input and joins them, but a `break`
//! on the input would make the loop run a number of times that differs from point to point.
//! So each such `break` (and `continue`) becomes a flag: the loop runs its own count, which
//! must not depend on the input (its own exit test stays a `break`), and the rest of each pass
//! runs only while neither flag is set. Where the flag is set on some points of the box and not
//! others, its interval holds both, and the branches it guards are run both ways and joined, as
//! any branch on the input is: the bound holds every point's value.
//!
//! ```text
//! loop {                                 done = false
//!     if i >= n { break }                loop {
//!     k += 1                                 skip = false
//!     if d(p) < e { break }       =>         if i >= n { break }
//!     p += step                              if !done {
//! } continuing { i += 1 }                        k += 1
//!                                                if d(p) < e { done = true }
//!                                                if !(done || skip) { p += step }
//!                                            }
//!                                        } continuing { i += 1 }
//! ```
//!
//! A pass is its head, up to its own exit test, which runs on every pass, and the rest, which
//! runs only until an exit on the input is taken. The head may only compute values and test
//! them: whatever it changed would go on changing after the exit. The rest is wrapped in an
//! `if`, so a value it makes that the `continuing` block reads would be unset on the other side.
//! A loop that breaks either rule is left as it is (and is E0701, as before), as is one whose own
//! exit test comes after an exit on the input: wrapped, it would be under a condition on the
//! input itself.

use crate::*;
use std::collections::HashSet;

/// `f`, with every loop whose `break` or `continue` sits under a condition `active` holds
/// (one that depends on the input) rewritten with flags; `None` if no loop changed.
pub(crate) fn flag_exits(
    f: &Function,
    types: &mut Types,
    active: &dyn Fn(ValueId) -> bool,
) -> Option<Function> {
    let mut g = f.clone();
    let bool_ty = types.bool();
    let mut body = std::mem::take(&mut g.body);
    let mut changed = false;
    // Loops inside first: each takes its own `break` and `continue`.
    visit::expand(&mut body, &mut |mut s, out| {
        if let Stmt::Loop { body, continuing } = &mut s
            && let Some((done, new_body)) = flag_loop(&mut g, body, continuing, bool_ty, active)
        {
            let f = g.let_(out, bool_ty, Expr::Const(Const::Bool(false)));
            out.push(Stmt::Store(Place::local(done), f));
            *body = new_body;
            changed = true;
        }
        out.push(s);
    });
    g.body = body;
    changed.then_some(g)
}

/// The loop's body with its exits under conditions on the input made flags, and the local of a
/// `break`'s flag, which must start false before the loop; `None` when it has none, or when its
/// head or its rest breaks a rule above.
fn flag_loop(
    g: &mut Function,
    body: &Block,
    continuing: &Block,
    bool_ty: TypeId,
    active: &dyn Fn(ValueId) -> bool,
) -> Option<(LocalId, Block)> {
    // The head: up to the loop's own exit test, the last `break` before an exit on the input.
    let first_exit =
        body.iter().position(|s| has_active_exit(std::slice::from_ref(s), false, active))?;
    let head = body[..first_exit].iter().rposition(|s| breaks(std::slice::from_ref(s)))? + 1;
    if !body[..head].iter().all(computes_only) {
        return None;
    }
    let done = g.new_local("exit_done", bool_ty);
    let skip = g.new_local("exit_skip", bool_ty);
    let mut new = Vec::new();
    let f = g.let_(&mut new, bool_ty, Expr::Const(Const::Bool(false)));
    new.push(Stmt::Store(Place::local(skip), f));
    new.extend(body[..head].iter().cloned());
    let (rest, _) = flag(g, body[head..].to_vec(), false, done, skip, bool_ty, active);
    // The rest runs while no exit on the input has been taken; the loop ends by its head's.
    if breaks(&rest) {
        return None;
    }
    let mut guarded = HashSet::new();
    visit::walk(&rest, &mut |s| {
        if let Stmt::Let(v, _) = s {
            guarded.insert(*v);
        }
    });
    let mut read_outside = false;
    visit::walk(continuing, &mut |s| {
        s.for_each_value(&mut |v| read_outside |= guarded.contains(&v))
    });
    if read_outside {
        return None;
    }
    let d = g.let_(&mut new, bool_ty, Expr::Load(Place::local(done)));
    let go = g.let_(&mut new, bool_ty, Expr::Unary(UnOp::Not, d));
    new.push(Stmt::If { cond: go, then: rest, else_: Vec::new() });
    Some((done, new))
}

/// Whether `s`, in a loop's head, only computes values and tests them: no call (which may change
/// what it's given), no store, no exit but a `break`.
fn computes_only(s: &Stmt) -> bool {
    match s {
        Stmt::Let(_, Expr::Call(..)) => false,
        Stmt::Let(..) | Stmt::At(_) | Stmt::Break => true,
        Stmt::If { then, else_, .. } => {
            then.iter().all(computes_only) && else_.iter().all(computes_only)
        }
        _ => false,
    }
}

/// Whether `b` has a `break` of the loop around it (one in a loop inside it is that loop's).
fn breaks(b: &[Stmt]) -> bool {
    b.iter().any(|s| match s {
        Stmt::Break => true,
        Stmt::If { then, else_, .. } => breaks(then) || breaks(else_),
        _ => false,
    })
}

/// Whether `b` has a `break` or `continue` of the loop around it under a condition on the
/// input (`under`: an enclosing one is).
fn has_active_exit(b: &[Stmt], under: bool, active: &dyn Fn(ValueId) -> bool) -> bool {
    b.iter().any(|s| match s {
        Stmt::Break | Stmt::Continue => under,
        Stmt::If { cond, then, else_ } => {
            let u = under || active(*cond);
            has_active_exit(then, u, active) || has_active_exit(else_, u, active)
        }
        _ => false,
    })
}

/// `block` with its exits under a condition on the input made flags (`under`: an enclosing one
/// is), and the statements after one that may set a flag wrapped to run only while neither is
/// set; and whether it may set one.
#[allow(clippy::too_many_arguments)]
fn flag(
    g: &mut Function,
    block: Block,
    under: bool,
    done: LocalId,
    skip: LocalId,
    bool_ty: TypeId,
    active: &dyn Fn(ValueId) -> bool,
) -> (Block, bool) {
    let mut out = Vec::new();
    let mut sets = false;
    let mut rest = block.into_iter();
    while let Some(s) = rest.next() {
        match s {
            Stmt::Break | Stmt::Continue if under => {
                let flag = if matches!(s, Stmt::Break) { done } else { skip };
                let t = g.let_(&mut out, bool_ty, Expr::Const(Const::Bool(true)));
                out.push(Stmt::Store(Place::local(flag), t));
                sets = true;
            }
            Stmt::If { cond, then, else_ } => {
                let u = under || active(cond);
                let (then, a) = flag(g, then, u, done, skip, bool_ty, active);
                let (else_, b) = flag(g, else_, u, done, skip, bool_ty, active);
                out.push(Stmt::If { cond, then, else_ });
                sets |= a || b;
            }
            other => out.push(other),
        }
        if sets {
            // The rest of the pass runs only while no flag is set.
            let remaining: Block = rest.collect();
            if remaining.is_empty() {
                break;
            }
            let (inner, _) = flag(g, remaining, under, done, skip, bool_ty, active);
            let d = g.let_(&mut out, bool_ty, Expr::Load(Place::local(done)));
            let k = g.let_(&mut out, bool_ty, Expr::Load(Place::local(skip)));
            let either = g.let_(&mut out, bool_ty, Expr::Binary(BinOp::Or, d, k));
            let go = g.let_(&mut out, bool_ty, Expr::Unary(UnOp::Not, either));
            out.push(Stmt::If { cond: go, then: inner, else_: Vec::new() });
            break;
        }
    }
    (out, sets)
}
