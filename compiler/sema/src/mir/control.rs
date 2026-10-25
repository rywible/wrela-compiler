//! Which blocks of a function run only on some of its paths: control dependence, from
//! post-dominators. `wrela pipelines` marks a dispatch or a draw that runs only under a
//! condition, since its pipeline is in the build however rarely the condition holds (§18.9).
//!
//! Block `b` is under a condition when it is control dependent on a branch `x`: some successor
//! of `x` is post-dominated by `b`, and `b` doesn't strictly post-dominate `x`. So code after an
//! `if`/`else` joins again isn't under it, code after an early `return` is, and a conditional
//! draw inside a loop is too (the other arm reaches it only on a later pass), as is code after
//! an `if c { break }`. Branches that are no condition of the program's are left out: one on a
//! literal (`if true`), and a loop's own test of whether to go on (`FnBody::loop_tests`). What
//! depends on such a branch is under whatever the branch itself is under: a loop in an `if`
//! runs only when the `if`'s condition holds.
//!
//! A loop that nothing leaves (`loop { ... }` without a `break` or a `return`) never reaches the
//! function's end, so nothing would post-dominate its blocks. Its back edge goes to the end too,
//! as if each pass were a run of its own.

use super::{BlockId, FnBody, OperandKind, TerminatorKind};

/// For each block of `f`, whether it runs only under a condition.
pub fn under_condition(f: &FnBody) -> Vec<bool> {
    let n = f.blocks.len();
    // Post-dominators, as sets over the blocks and a virtual exit (index `n`) that every block
    // with no successor goes to. Iterated to the fixed point from "everything".
    let exit = n;
    let mut succ: Vec<Vec<usize>> = (0..n)
        .map(|b| {
            let s: Vec<usize> = f.successors(BlockId(b as u32)).map(|t| t.index()).collect();
            if s.is_empty() { vec![exit] } else { s }
        })
        .collect();
    // The blocks that reach the exit; a loop's back edge that doesn't goes there as well.
    let mut preds = vec![Vec::new(); n + 1];
    for (b, ss) in succ.iter().enumerate() {
        for &s in ss {
            preds[s].push(b);
        }
    }
    let mut reaches = vec![false; n + 1];
    reaches[exit] = true;
    let mut work = vec![exit];
    while let Some(b) = work.pop() {
        for &x in &preds[b] {
            if !reaches[x] {
                reaches[x] = true;
                work.push(x);
            }
        }
    }
    for (b, ss) in succ.iter_mut().enumerate() {
        if !reaches[b] && f.is_back_edge(BlockId(b as u32)) {
            ss.push(exit);
        }
    }
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
    // The blocks control dependent on each branch that is no condition: under it when it is.
    let mut inherits: Vec<Vec<usize>> = vec![Vec::new(); n];
    for x in 0..n {
        if !matches!(f.block(BlockId(x as u32)).term.kind, TerminatorKind::If { .. }) {
            continue;
        }
        let condition = is_condition(f, BlockId(x as u32));
        for &s in &succ[x] {
            for b in 0..n {
                if pdom[s][b] && !(b != x && pdom[x][b]) {
                    if condition {
                        under[b] = true;
                    } else {
                        inherits[x].push(b);
                    }
                }
            }
        }
    }
    let mut work: Vec<usize> = (0..n).filter(|&x| under[x]).collect();
    while let Some(x) = work.pop() {
        for &b in &inherits[x] {
            if !under[b] {
                under[b] = true;
                work.push(b);
            }
        }
    }
    under
}

/// Whether block `x` ends in a branch that is a condition of the program's: an `if` on a value
/// that isn't a literal, other than a loop's own test of whether to go on. An `if c { break }`
/// the code wrote is one: what comes after it runs only while `c` doesn't hold.
fn is_condition(f: &FnBody, x: BlockId) -> bool {
    let TerminatorKind::If { cond, .. } = &f.block(x).term.kind else { return false };
    !matches!(cond.kind, OperandKind::Const(_)) && !f.loop_tests.contains(&x)
}

#[cfg(test)]
mod tests {
    /// Whether the one draw of a `frame` whose body ends with `body` is under a condition.
    fn conditional(body: &str) -> bool {
        let source = format!(
            "use std::gpu::{{ClipPosition, VertexIndex, begin_screen_pass, draw}}\n\
             @vertex\n\
             fn cover(v: VertexIndex) -> ClipPosition {{ ClipPosition {{ position: vec4(0.0) }} }}\n\
             @fragment\n\
             fn colour() -> vec4 {{ vec4(1.0) }}\n\
             pub fn frame(time: f32, width: u32, height: u32) {{\n\
                 let screen = begin_screen_pass(clear: vec4(0.0))\n\
                 let shown = time > 1.0\n\
                 {body}\n\
             }}\n"
        );
        let mut diagnostics = Vec::new();
        let checked = crate::check_program(crate::test_units(&source), &mut diagnostics);
        assert!(!wrela_diag::has_errors(&diagnostics), "{body}: {diagnostics:#?}");
        let (_, frame) = checked
            .mir
            .iter()
            .find(|(f, _)| checked.program.func(**f).name == "frame")
            .expect("frame");
        let f = &frame.fns[0];
        let under = super::under_condition(f);
        let draws: Vec<bool> = (f.blocks.iter().zip(under))
            .filter(|(b, _)| {
                b.stmts.iter().any(|s| {
                    use crate::mir::{Rvalue::Draw, StatementKind::*};
                    matches!(&s.kind, Eval(Draw(_)) | Assign(_, Draw(_)))
                })
            })
            .map(|(_, u)| u)
            .collect();
        assert_eq!(draws.len(), 1, "{body}");
        draws[0]
    }

    const DRAW: &str = "draw(screen, cover, colour, vertices: 3)";

    /// A branch that is no condition (a loop's test, `if true`) passes on what it is under.
    #[test]
    fn a_loop_under_a_condition_is_under_it() {
        for (body, want) in [
            (format!("if shown {{ for i in 0..4 {{ {DRAW} }} }}\nscreen.present()"), true),
            (format!("if shown {{ if true {{ {DRAW} }} }}\nscreen.present()"), true),
            (format!("for i in 0..4 {{ {DRAW} }}\nscreen.present()"), false),
            (format!("if true {{ {DRAW} }}\nscreen.present()"), false),
        ] {
            assert_eq!(conditional(&body), want, "{body}");
        }
    }

    /// A loop that nothing leaves still has conditions in it, and before it.
    #[test]
    fn an_endless_loop_keeps_its_conditions() {
        for (body, want) in [
            (format!("loop {{ if shown {{ {DRAW} }} }}"), true),
            (format!("loop {{ {DRAW} }}"), false),
            (format!("if shown {{ loop {{}} }}\n{DRAW}\nscreen.present()"), true),
        ] {
            assert_eq!(conditional(&body), want, "{body}");
        }
    }
}
