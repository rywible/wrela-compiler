//! Call merging (GPU): calls of one function, one after another in a block, become one call in
//! a loop over their arguments.
//!
//! Why: the calls left after flattening are to big functions (`opt::outlined`), and a browser's
//! shader compiler writes a function out at each place it's called, so its compile time grows
//! with the places (language.md §18.11's weight). A normal taken by four taps of a field
//! (`a * f(p + a * e) + b * f(p + b * e) + ...`) is four copies of the field; as a loop over the
//! four taps, one. The loop costs a few statements a pass, against a field's hundreds.
//!
//! Calls of a function `g` are merged when they're in one block, two or more, with nothing
//! between them but `let`s: each later call's arguments don't depend on an earlier one's result,
//! and a `let` between them that does (`f(..).x` before the next tap) moves after the loop, with
//! what depends on it. A block's other statements (stores, other effects, control flow) end a
//! run. The functions called are pure (`outlined` keeps only those), so calling them in another
//! order, all before what reads their results, computes the same.

use crate::*;
use std::collections::HashSet;

/// Merges runs of calls in `f`'s blocks (see the module docs).
pub(crate) fn merge_calls(m: &mut Module, f: &mut Function) {
    let body = std::mem::take(&mut f.body);
    f.body = block(m, f, body);
}

fn block(m: &mut Module, f: &mut Function, b: Block) -> Block {
    // Inner blocks first.
    let b: Block = b
        .into_iter()
        .map(|s| match s {
            Stmt::If { cond, then, else_ } => {
                Stmt::If { cond, then: block(m, f, then), else_: block(m, f, else_) }
            }
            Stmt::Loop { body, continuing } => {
                Stmt::Loop { body: block(m, f, body), continuing: block(m, f, continuing) }
            }
            s => s,
        })
        .collect();
    let mut out: Block = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match run_at(f, &b, i).filter(|r| big(m, r.callee)) {
            Some(run) => {
                emit(m, f, &b, &run, &mut out);
                i = run.end;
            }
            None => {
                out.push(b[i].clone());
                i += 1;
            }
        }
    }
    out
}

/// The weight (`work::weight`) of a function worth merging calls of: a loop's few statements a
/// pass, and its arrays of arguments and results, cost less than another copy of it.
const MERGE_WEIGHT: u64 = 400;

/// Whether calls of `g` are worth merging.
fn big(m: &Module, g: FuncId) -> bool {
    crate::work::weight(m, g) >= MERGE_WEIGHT
}

/// A run of calls to merge: the calls' indices in the block, the `let`s between them that read
/// their results (moved after the loop), and the index after the run.
struct Run {
    callee: FuncId,
    calls: Vec<usize>,
    moved: Vec<usize>,
    end: usize,
}

/// The call `s` makes, if it's `let v = g(values...)`.
fn call(s: &Stmt) -> Option<(ValueId, FuncId, Vec<ValueId>)> {
    let Stmt::Let(v, Expr::Call(g, args)) = s else { return None };
    let args = args
        .iter()
        .map(|a| match a {
            Arg::Value(x) => Some(*x),
            Arg::Place(_) => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some((*v, *g, args))
}

/// Whether `e` only computes a value: moving it later changes nothing but when it's computed.
/// (A run holds no store, so a load reads the same before and after.)
fn computes(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Const(_)
            | Expr::Zero(_)
            | Expr::Load(_)
            | Expr::Unary(..)
            | Expr::Binary(..)
            | Expr::Builtin(..)
            | Expr::Construct(..)
            | Expr::Variant(..)
            | Expr::Extract(..)
            | Expr::ExtractDyn(..)
            | Expr::Splat(..)
            | Expr::Swizzle(..)
            | Expr::Convert(..)
            | Expr::Bitcast(..)
            | Expr::Select { .. }
            | Expr::Param(_)
    )
}

/// The run of calls that starts at `b[i]`, if there's one of two calls or more.
fn run_at(f: &Function, b: &Block, i: usize) -> Option<Run> {
    let (v, g, _) = call(&b[i])?;
    // A function returning nothing, or one that takes a pointer, isn't merged.
    f.values.get(v.index())?;
    let mut calls = vec![i];
    // The values the run's results reach: its calls' results, and the moved `let`s'.
    let mut tainted: HashSet<ValueId> = HashSet::from([v]);
    let mut moved = Vec::new();
    let mut j = i + 1;
    while j < b.len() {
        let s = &b[j];
        let reads_tainted = |s: &Stmt| {
            let mut hit = false;
            s.for_each_value(&mut |x| hit |= tainted.contains(&x));
            hit
        };
        if let Some((w, h, _)) = call(s)
            && h == g
        {
            if reads_tainted(s) {
                break;
            }
            calls.push(j);
            tainted.insert(w);
        } else {
            match s {
                Stmt::At(_) => {}
                Stmt::Let(w, e) if computes(e) || matches!(e, Expr::Call(..)) => {
                    // Another pure call (all kept functions are pure) stays where it is, or
                    // moves after the loop with what it reads.
                    if matches!(e, Expr::Call(..)) && call(s).is_none() {
                        break;
                    }
                    if reads_tainted(s) {
                        moved.push(j);
                        tainted.insert(*w);
                    }
                }
                _ => break,
            }
        }
        j += 1;
    }
    // The run ends at its last call: what's after it stays where it is.
    let last = *calls.last()?;
    moved.retain(|&k| k < last);
    (calls.len() >= 2).then_some(Run { callee: g, calls, moved, end: last + 1 })
}

/// Writes `run` out: what stays before the loop, the loop, each call's result, then what moved.
fn emit(m: &mut Module, f: &mut Function, b: &Block, run: &Run, out: &mut Block) {
    let start = run.calls[0];
    for (k, s) in b.iter().enumerate().take(run.end).skip(start) {
        if !run.calls.contains(&k) && !run.moved.contains(&k) {
            out.push(s.clone());
        }
    }
    let calls: Vec<(ValueId, Vec<ValueId>)> =
        run.calls.iter().filter_map(|&k| call(&b[k]).map(|(v, _, a)| (v, a))).collect();
    let n = calls.len() as u32;
    let u = m.types.u32();
    let bool_ = m.types.bool();
    let ret = f.values[calls[0].0.index()];
    // Each parameter's arguments, as an array: or the one value, where every call passes it.
    let mut params = Vec::new();
    for q in 0..calls[0].1.len() {
        let t = f.values[calls[0].1[q].index()];
        let values: Vec<ValueId> = calls.iter().map(|(_, a)| a[q]).collect();
        if values.iter().all(|&v| v == values[0]) {
            params.push(Err(values[0]));
            continue;
        }
        let at = m.types.intern(TypeDef::Array(t, n));
        let array = f.let_(out, at, Expr::Construct(at, values));
        let local = f.new_local(format!("merged_arg{q}"), at);
        out.push(Stmt::Store(Place::local(local), array));
        params.push(Ok((local, t)));
    }
    let results_ty = m.types.intern(TypeDef::Array(ret, n));
    let results = f.new_local("merged_results", results_ty);
    let counter = f.new_local("merged_i", u);
    let zero = f.let_(out, u, Expr::Const(Const::U32(0)));
    out.push(Stmt::Store(Place::local(counter), zero));
    // The loop: `i` from 0 while `i < n`, each pass one call.
    let mut body = Vec::new();
    let i = f.let_(&mut body, u, Expr::Load(Place::local(counter)));
    let count = f.let_(&mut body, u, Expr::Const(Const::U32(n)));
    let done = f.let_(&mut body, bool_, Expr::Binary(BinOp::Ge, i, count));
    body.push(Stmt::If { cond: done, then: vec![Stmt::Break], else_: Vec::new() });
    let mut args = Vec::new();
    for p in &params {
        match *p {
            Ok((local, t)) => {
                let at = Place { root: PlaceRoot::Local(local), path: vec![Proj::Index(i)] };
                args.push(Arg::Value(f.let_(&mut body, t, Expr::Load(at))));
            }
            Err(same) => args.push(Arg::Value(same)),
        }
    }
    let r = f.let_(&mut body, ret, Expr::Call(run.callee, args));
    body.push(Stmt::Store(
        Place { root: PlaceRoot::Local(results), path: vec![Proj::Index(i)] },
        r,
    ));
    let mut continuing = Vec::new();
    let now = f.let_(&mut continuing, u, Expr::Load(Place::local(counter)));
    let one = f.let_(&mut continuing, u, Expr::Const(Const::U32(1)));
    let next = f.let_(&mut continuing, u, Expr::Binary(BinOp::Add, now, one));
    continuing.push(Stmt::Store(Place::local(counter), next));
    out.push(Stmt::Loop { body, continuing });
    // Each call's result, under its own name, then what read them.
    for (k, (v, _)) in calls.iter().enumerate() {
        let index = f.let_(out, u, Expr::Const(Const::U32(k as u32)));
        let at = Place { root: PlaceRoot::Local(results), path: vec![Proj::Index(index)] };
        out.push(Stmt::Let(*v, Expr::Load(at)));
    }
    for &k in &run.moved {
        out.push(b[k].clone());
    }
}
