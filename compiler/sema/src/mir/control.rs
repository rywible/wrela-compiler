//! Which blocks of a function run only on some of its paths: control dependence, from
//! post-dominators. `wrela pipelines` marks a dispatch or a draw that runs only under a
//! condition, since its pipeline is in the build however rarely the condition holds (§18.9).
//!
//! Block `b` is under a condition when it is control dependent on a branch `x`: some successor
//! of `x` is post-dominated by `b`, and `b` doesn't strictly post-dominate `x`. So code after an
//! `if`/`else` joins again isn't under it, code after an early `return` is, and a conditional
//! draw inside a loop is too (the other arm reaches it only on a later pass). Branches that are
//! no condition of the program's are left out: one on a literal (`if true`), and a loop's own
//! test of whether to go on, whose one arm only leaves the loop.

use super::{BlockId, FnBody, OperandKind, TerminatorKind};

/// For each block of `f`, whether it runs only under a condition.
pub fn under_condition(f: &FnBody) -> Vec<bool> {
    let n = f.blocks.len();
    // Post-dominators, as sets over the blocks and a virtual exit (index `n`) that every block
    // with no successor goes to. Iterated to the fixed point from "everything".
    let exit = n;
    let succ: Vec<Vec<usize>> = (0..n)
        .map(|b| {
            let s: Vec<usize> = f.successors(BlockId(b as u32)).map(|t| t.index()).collect();
            if s.is_empty() { vec![exit] } else { s }
        })
        .collect();
    let mut pdom: Vec<Vec<bool>> = vec![vec![true; n + 1]; n + 1];
    pdom[exit] = vec![false; n + 1];
    pdom[exit][exit] = true;
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..n).rev() {
            let mut next = vec![true; n + 1];
            for &s in &succ[b] {
                for (i, v) in next.iter_mut().enumerate() {
                    *v &= pdom[s][i];
                }
            }
            next[b] = true;
            if next != pdom[b] {
                pdom[b] = next;
                changed = true;
            }
        }
    }
    let mut under = vec![false; n];
    for x in 0..n {
        if !is_condition(f, BlockId(x as u32)) {
            continue;
        }
        for &s in &succ[x] {
            for (b, flag) in under.iter_mut().enumerate() {
                if s < n && pdom[s][b] && !(b != x && pdom[x][b]) {
                    *flag = true;
                }
            }
        }
    }
    under
}

/// Whether block `x` ends in a branch that is a condition of the program's: an `if` on a value
/// that isn't a literal, other than a loop's test of whether to go on.
fn is_condition(f: &FnBody, x: BlockId) -> bool {
    let TerminatorKind::If { cond, then, else_, .. } = &f.block(x).term.kind else { return false };
    if matches!(cond.kind, OperandKind::Const(_)) {
        return false;
    }
    let only_leaves = |b: BlockId| {
        let d = f.block(b);
        d.stmts.iter().all(|s| matches!(s.kind, super::StatementKind::Dead(_)))
            && matches!(d.term.kind, TerminatorKind::Break(_))
    };
    !(only_leaves(*then) || only_leaves(*else_))
}
