//! Warnings for mistakes an agent makes often, which the language allows but which never mean
//! what they look like: W0012, a request polled in the call that made it; and W0013, a counted
//! `while` over a run, which a `for` loop says better. (W0010, a copy only read, is borrowck's,
//! which knows the moves; W0011, a test that can't fail, is effects'.) Std's own code isn't
//! linted.

use crate::defs::FnOwner;
use crate::mir::{
    Aliases, Arg, Body, Callee, Local, LocalKind, OperandKind, Rvalue, StatementKind,
};
use crate::program::Program;
use crate::thir;
use crate::ty::{FnId, TyKind};
use std::collections::{HashMap, HashSet};
use wrela_diag::{Diagnostic, Span, codes};
use wrela_syntax::ast::{self, AssignOp, BinOp, ExprKind, StmtKind};

/// Every lint of function `f`, whose memory IR is `body`.
pub fn lint_fn(p: &Program, f: FnId, body: &Body) -> Vec<Diagnostic> {
    let def = p.func(f);
    if p.is_std(def.module) {
        return Vec::new();
    }
    let mut out = Vec::new();
    // A job's locals live across its `yield`s, so it polls a request on a later frame.
    if def.attrs.job.is_none() {
        out.extend(polled_at_once(p, body));
    }
    if let Some(b) = &def.body {
        b.for_each_block(&mut |block| out.extend(counted_whiles(p, block)));
    }
    out
}

// ---- W0012 --------------------------------------------------------------------------------

/// Whether `t` is std's `Pending<T>` (`std::io`), a request's handle.
fn is_pending(p: &Program, t: crate::ty::TyId) -> bool {
    matches!(p.types.kind(t), TyKind::Adt(a, _)
        if p.adt(*a).name == "Pending" && p.is_std(p.adt(*a).module))
}

/// Whether `f` is `Pending::poll`.
fn is_poll(p: &Program, f: FnId) -> bool {
    let d = p.func(f);
    d.name == "poll"
        && p.is_std(d.module)
        && matches!(d.owner, FnOwner::Impl(i) if is_pending(p, p.impl_(i).self_ty))
}

/// W0012: `let r = load(path)` then `r.poll()` in the same call. The host answers a request no
/// sooner than the next call (§17.1), so that poll is `None` every time, and code that waits for
/// its answer never runs. The handle belongs in the state, polled on a later frame or tick.
///
/// Only a local whose every value is a request made here is reported: one given another handle
/// (`r = old`), lent `mut` to anything but its poll, or written through a projection, may hold
/// a request made earlier.
fn polled_at_once(p: &Program, body: &Body) -> Vec<Diagnostic> {
    // What each projection local aliases, and which temporaries hold a call's result.
    let mut alias = Aliases::default();
    let mut from_call: Vec<Local> = Vec::new();
    // Each `let` or `var` of a handle: where it's first given a request made here, and whether
    // anything else could have given it its value.
    let mut made: HashMap<Local, Span> = HashMap::new();
    let mut other = HashSet::new();
    let handle = |x: Local| is_pending(p, body.local(x).ty) && is_user_owned(body, x);
    // The poll a call is, and its receiver's local.
    let polled = |alias: &Aliases, c: &crate::mir::Call| match (&c.callee, c.args.first()) {
        (Callee::Fn { func, .. }, Some(Arg::Mut(recv, _) | Arg::Borrow(recv, _)))
            if is_poll(p, *func) =>
        {
            Some(alias.root(recv.local))
        }
        _ => None,
    };
    // A request std makes (`load`, `std::gpu::read`): a function of std's declared to return a
    // `Pending`, which makes one. Not one declared to return a `T` that's a `Pending` here
    // (`replace`, `remove`, `unwrap`), which gives back one made earlier; nor one of the
    // program's, which may give back one it kept.
    let makes_request = |c: &crate::mir::Call| match &c.callee {
        Callee::Fn { func, .. } => {
            let d = p.func(*func);
            p.is_std(d.module) && is_pending(p, d.ret)
        }
        _ => false,
    };
    let mut polls = Vec::new();
    for f in &body.fns {
        for b in &f.blocks {
            for s in &b.stmts {
                let call = match &s.kind {
                    StatementKind::Assign(_, Rvalue::Call(c))
                    | StatementKind::Eval(Rvalue::Call(c)) => Some(c),
                    _ => None,
                };
                if let Some(c) = call {
                    let poll = polled(&alias, c);
                    if let Some(r) = poll {
                        polls.push((r, c.span));
                    }
                    for a in &c.args {
                        if let Arg::Mut(pl, _) = a
                            && poll != Some(alias.root(pl.local))
                        {
                            other.insert(alias.root(pl.local));
                        }
                    }
                }
                match &s.kind {
                    StatementKind::Bind { local, place, .. } => alias.bind(*local, place),
                    // Through a `mut` projection: what it aliases is given another value.
                    StatementKind::Assign(place, _) if alias.contains(place.local) => {
                        other.insert(alias.root(place.local));
                    }
                    StatementKind::Assign(place, r) if place.proj.is_empty() => {
                        let x = place.local;
                        let fresh = match r {
                            Rvalue::Call(c) if makes_request(c) => {
                                from_call.push(x);
                                true
                            }
                            // A `Pending` is `Copy`: its value comes through a copy.
                            Rvalue::Use(o) => matches!(&o.kind,
                                OperandKind::Move(tmp, _) | OperandKind::Copy(tmp)
                                    if tmp.proj.is_empty() && from_call.contains(&tmp.local)),
                            _ => false,
                        };
                        if handle(x) {
                            if fresh {
                                made.entry(x).or_insert(s.span);
                            } else {
                                other.insert(x);
                            }
                        }
                    }
                    StatementKind::Assign(place, _) => {
                        other.insert(alias.root(place.local));
                    }
                    _ => {}
                }
            }
        }
    }
    polls
        .into_iter()
        .filter(|(r, _)| !other.contains(r))
        .filter_map(|(r, at)| made.get(&r).map(|&m| poll_warning(body, r, m, at)))
        .collect()
}

/// Whether `x` is a `let` or `var` of the source that owns its value.
fn is_user_owned(body: &Body, x: Local) -> bool {
    matches!(body.local(x).kind, LocalKind::User(thir::LocalKind::Owned { .. }))
}

fn poll_warning(body: &Body, x: Local, made: Span, poll: Span) -> Diagnostic {
    let name = &body.local(x).name;
    Diagnostic::new(
        codes::W0012,
        poll,
        format!("`{name}` is polled in the call that made its request, so it's `None` here"),
    )
    .with_secondary(made, "the request is made here")
    .with_note("the host answers a request no sooner than the next call (§17.1)")
    .with_help("keep the `Pending` in the program's state, and poll it in a later frame or tick")
}

// ---- W0013 --------------------------------------------------------------------------------

/// Every expression in statement `s`, nested ones included.
fn each_expr(s: &ast::Stmt, f: &mut impl FnMut(&ast::Expr)) {
    s.for_each_expr(&mut |e| e.walk(&mut |x| f(x)));
}

/// W0013 in block `b`: `var i = 0`, then `while i < xs.len() { ...xs[i]... i += 1 }`, where `i`
/// is used only to index `xs`, and nowhere after the loop, and nothing in the body leaves it
/// early or names `xs` but to index it. That's `for x in xs`, which can't index past the end,
/// count wrong or loop forever.
fn counted_whiles(p: &Program, b: &ast::Block) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (k, s) in b.stmts.iter().enumerate() {
        let StmtKind::While { pat: None, cond, body } = &s.kind else { continue };
        let ExprKind::Binary(BinOp::Lt, i, len) = &cond.kind else { continue };
        let Some(name) = i.single_name() else { continue };
        let ExprKind::MethodCall { receiver: xs, name: m, args, .. } = &len.kind else { continue };
        if m.name != "len" || !args.is_empty() {
            continue;
        }
        let Some(seq) = p.text(xs.span) else { continue };
        // Counted from 0: the last statement before the loop that names `i` is `var i = 0`.
        let starts_at_zero = b.stmts[..k].iter().rev().find_map(|s| match &s.kind {
            StmtKind::Var { name: n, init, .. } if n.name == name => {
                Some(matches!(&init.kind, ExprKind::Lit(l) if p.text(l.span) == Some("0")))
            }
            StmtKind::Assign { target, .. } if target.single_name() == Some(name) => Some(false),
            _ => None,
        });
        if starts_at_zero != Some(true) {
            continue;
        }
        // Stepped by one, last.
        let Some(last) = body.stmts.last() else { continue };
        let stepped = matches!(&last.kind, StmtKind::Assign { target, op: AssignOp::Add, value }
            if target.single_name() == Some(name) && p.text(value.span) == Some("1"));
        if !stepped {
            continue;
        }
        // Every other use of `i` indexes `xs`, `xs` is named only to be indexed by `i`, and
        // nothing leaves the loop early (a `break` keeps `i`'s value for after).
        // The local the sequence is reached through (`w` of `w.slots`): a body that names it
        // otherwise may change it (`w.drop(t)`), which a `for` over `w.slots` can't allow.
        let root = seq.split(['.', '[']).next().unwrap_or(seq).to_string();
        let mut root_named = 0;
        let (mut uses, mut indexing, mut seq_named, mut seq_indexed) = (0, 0, 0, 0);
        let mut leaves = false;
        let mut writes = false;
        let mut assigns_i = 0;
        let rest = &body.stmts[..body.stmts.len() - 1];
        let mut count = |e: &ast::Expr| match &e.kind {
            ExprKind::Path(_) if e.single_name() == Some(name) => uses += 1,
            ExprKind::Path(_) if e.single_name() == Some(root.as_str()) => root_named += 1,
            ExprKind::Index { base, index }
                if index.single_name() == Some(name) && p.text(base.span) == Some(seq) =>
            {
                indexing += 1;
                seq_indexed += 1;
            }
            ExprKind::Break | ExprKind::Continue | ExprKind::Return(_) | ExprKind::Try(_) => {
                leaves = true
            }
            _ if p.text(e.span) == Some(seq) => seq_named += 1,
            _ => {}
        };
        rest.iter().for_each(|s| each_expr(s, &mut count));
        let mut assigns = |stmts: &[ast::Stmt]| {
            for s in stmts {
                if let StmtKind::Assign { target, .. } = &s.kind {
                    if target.single_name() == Some(name) {
                        assigns_i += 1;
                    }
                    if matches!(&target.kind, ExprKind::Index { base, index }
                        if index.single_name() == Some(name) && p.text(base.span) == Some(seq))
                    {
                        writes = true;
                    }
                }
            }
        };
        assigns(rest);
        rest.iter().for_each(|s| s.for_each_block(&mut |blk| assigns(&blk.stmts)));
        // `xs[i]` also counts as a use of `xs` and of `i` among the expressions walked.
        let named_after = b.stmts[k + 1..].iter().any(|s| {
            let mut found = false;
            each_expr(s, &mut |e| found |= e.single_name() == Some(name));
            found
        });
        // Each `seq[i]` names the root once; any other naming of it is a use the loop has.
        // (A sequence that is a name alone is its own root, counted as the root.)
        let named_otherwise =
            root_named != seq_indexed || (root != seq && seq_named != seq_indexed);
        if leaves
            || assigns_i > 0
            || named_after
            || indexing == 0
            || uses != indexing
            || named_otherwise
        {
            continue;
        }
        let (form, what) = if writes {
            ("for mut x in", "changes each element")
        } else {
            ("for x in", "reads each element")
        };
        out.push(
            Diagnostic::new(
                codes::W0013,
                s.span.to(cond.span),
                format!("this `while` counts `{name}` over `{seq}` only to index it"),
            )
            .with_note(format!("a `for` loop walks a run, an array or a `Vec` itself, and {what} without an index (§2.2)"))
            .with_help(format!("walk it: `{form} {seq} {{ ... }}`, with `x` where the body has `{seq}[{name}]`")),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    fn warnings(source: &str) -> Vec<(String, String)> {
        let mut diagnostics = Vec::new();
        crate::check_program(crate::test_units(source), &mut diagnostics);
        diagnostics
            .into_iter()
            .filter(|d| matches!(d.code.as_str(), "W0012" | "W0013"))
            .map(|d| (d.code.as_str().to_string(), d.message))
            .collect()
    }

    #[test]
    fn a_counted_while_over_a_run_is_reported() {
        let w = warnings(
            "pub fn sum(xs: [f32]) -> f32 {
    var total = 0.0
    var i = 0
    while i < xs.len() {
        total += xs[i]
        i += 1
    }
    total
}
",
        );
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(w[0].0, "W0013");
    }

    #[test]
    fn a_while_that_needs_its_index_is_not_reported() {
        for body in [
            // The index is the result.
            "var i = 0\n    while i < xs.len() {\n        if xs[i] < 0.0 { break }\n        i += 1\n    }\n    f32(i)",
            // The index is used for more than indexing.
            "var t = 0.0\n    var i = 0\n    while i < xs.len() {\n        t += xs[i] * f32(i)\n        i += 1\n    }\n    t",
            // It doesn't start at the beginning.
            "var t = 0.0\n    var i = 1\n    while i < xs.len() {\n        t += xs[i]\n        i += 1\n    }\n    t",
            // The body changes what holds the sequence, which a `for` over it couldn't.
            "var t = 0.0\n    var i = 0\n    while i < w.xs.len() {\n        t += w.xs[i]\n        w.n += 1\n        i += 1\n    }\n    t",
            // It steps by two.
            "var t = 0.0\n    var i = 0\n    while i < xs.len() {\n        t += xs[i]\n        i += 2\n    }\n    t",
        ] {
            let w = warnings(&format!(
                "pub struct W {{ pub xs: Vec<f32>, pub n: u32 }}\npub fn f(xs: [f32], w: mut W) -> f32 {{\n    {body}\n}}\n"
            ));
            assert!(w.is_empty(), "{body}: {w:?}");
        }
    }
}
