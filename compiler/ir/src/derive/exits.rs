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
//!     if d(p) < e { break }       =>         skip = false
//!     p += step                              if i >= n { break }
//! } continuing { i += 1 }                    if d(p) < e { done = true }
//!                                            if !(done || skip) { p += step }
//!                                        } continuing { i += 1 }
//! ```
//!
//! A pass's later statements are wrapped in an `if`; a value one of them makes that the
//! `continuing` block reads would then be unset on the other side, so such a loop is left as it
//! is (and is E0701, as before). So is a loop whose own exit test comes after an exit on the
//! input: wrapped, it would be under a condition on the input itself.

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
    let changed = rewrite(&mut g, &mut body, bool_ty, active);
    g.body = body;
    changed.then_some(g)
}

/// Rewrites the loops in `block` and the blocks nested in it.
fn rewrite(
    g: &mut Function,
    block: &mut Block,
    bool_ty: TypeId,
    active: &dyn Fn(ValueId) -> bool,
) -> bool {
    let mut changed = false;
    let mut out = Vec::with_capacity(block.len());
    for mut s in std::mem::take(block) {
        // Loops inside first: each takes its own `break` and `continue`.
        for inner in s.blocks_mut() {
            changed |= rewrite(g, inner, bool_ty, active);
        }
        if let Stmt::Loop { body, continuing } = &mut s
            && let Some((done, skip, new_body)) = flag_loop(g, body, continuing, bool_ty, active)
        {
            let f = g.let_(&mut out, bool_ty, Expr::Const(Const::Bool(false)));
            out.push(Stmt::Store(Place::local(done), f));
            let _ = skip;
            *body = new_body;
            changed = true;
        }
        out.push(s);
    }
    *block = out;
    changed
}

/// The loop's body with its exits under conditions on the input made flags, and the flags'
/// locals; `None` when it has none, or when a value the wrapped statements make is read outside
/// them.
fn flag_loop(
    g: &mut Function,
    body: &Block,
    continuing: &Block,
    bool_ty: TypeId,
    active: &dyn Fn(ValueId) -> bool,
) -> Option<(LocalId, LocalId, Block)> {
    if !has_active_exit(body, false, active) {
        return None;
    }
    let done = g.new_local("exit_done", bool_ty);
    let skip = g.new_local("exit_skip", bool_ty);
    let mut new = Vec::new();
    let f = g.let_(&mut new, bool_ty, Expr::Const(Const::Bool(false)));
    new.push(Stmt::Store(Place::local(skip), f));
    let mut guarded = HashSet::new();
    let (flagged, _) = flag(g, body.clone(), false, done, skip, bool_ty, active, &mut guarded);
    new.extend(flagged);
    // The loop must still end by itself: its own count, a `break` not on the input. A `while`
    // on the input has none left, and stays E0701.
    if !breaks(&new) {
        return None;
    }
    // Each value the wrapped statements make must be read only inside them.
    let mut read_outside = false;
    visit::walk(continuing, &mut |s| {
        s.for_each_value(&mut |v| read_outside |= guarded.contains(&v))
    });
    if read_outside {
        return None;
    }
    Some((done, skip, new))
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
    guarded: &mut HashSet<ValueId>,
) -> (Block, bool) {
    let mut out = Vec::new();
    let mut sets = false;
    let mut rest = block.into_iter();
    while let Some(s) = rest.next() {
        match s {
            Stmt::Break if under => {
                let t = g.let_(&mut out, bool_ty, Expr::Const(Const::Bool(true)));
                out.push(Stmt::Store(Place::local(done), t));
                sets = true;
            }
            Stmt::Continue if under => {
                let t = g.let_(&mut out, bool_ty, Expr::Const(Const::Bool(true)));
                out.push(Stmt::Store(Place::local(skip), t));
                sets = true;
            }
            Stmt::If { cond, then, else_ } => {
                let u = under || active(cond);
                let (then, a) = flag(g, then, u, done, skip, bool_ty, active, guarded);
                let (else_, b) = flag(g, else_, u, done, skip, bool_ty, active, guarded);
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
            let (inner, _) = flag(g, remaining, under, done, skip, bool_ty, active, guarded);
            visit::walk(&inner, &mut |s| {
                if let Stmt::Let(v, _) = s {
                    guarded.insert(*v);
                }
            });
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
