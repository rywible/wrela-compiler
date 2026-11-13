//! What keeps a kernel from giving the same bits on every GPU (language.md §18.14): a
//! `@deterministic` kernel's module, walked through every function it reaches, for float
//! arithmetic (a float may be loaded, stored, selected and bit-cast: its bits are data), and for
//! what makes a result depend on how invocations interleave or group (atomics, workgroup memory,
//! subgroup operations, barriers, textures, derivatives). Each is E0713, naming what and where.

use crate::{Builtin, Expr, Module, PlaceRoot, ResourceKind, Stmt, TypeDef, ValueId};
use wrela_diag::Span;

/// What a deterministic kernel does that isn't the same on every GPU, and where.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub what: String,
    pub at: Option<Span>,
}

/// Every problem in `m`'s functions (a kernel's module holds the kernel and what it reaches).
pub fn problems(m: &Module) -> Vec<Problem> {
    let mut out = Vec::new();
    for f in &m.functions {
        let mut at = None;
        walk(m, f, &f.body, &mut at, &mut out);
    }
    out.dedup_by(|a, b| a.what == b.what && a.at == b.at);
    out
}

fn is_float(m: &Module, f: &crate::Function, v: ValueId) -> bool {
    match m.types.get(f.value_ty(v)) {
        TypeDef::Scalar(s) | TypeDef::Vector(s, _) => s.is_float(),
        _ => false,
    }
}

fn place_problem(m: &Module, p: &crate::Place) -> Option<&'static str> {
    match p.root {
        PlaceRoot::Resource(r) => match m.resources[r.index()].kind {
            ResourceKind::Workgroup => Some("workgroup memory"),
            _ => None,
        },
        _ => None,
    }
}

fn walk(
    m: &Module,
    f: &crate::Function,
    stmts: &[Stmt],
    at: &mut Option<Span>,
    out: &mut Vec<Problem>,
) {
    let push = |what: String, at: Option<Span>, out: &mut Vec<Problem>| {
        out.push(Problem { what, at });
    };
    for s in stmts {
        match s {
            Stmt::At(sp) => *at = Some(*sp),
            Stmt::Let(_, e) | Stmt::Eval(e) => {
                if let Some(what) = expr_problem(m, f, e) {
                    push(what, *at, out);
                }
            }
            Stmt::Store(p, _) => {
                if let Some(what) = place_problem(m, p) {
                    push(format!("a store to {what}"), *at, out);
                }
            }
            Stmt::If { then, else_, .. } => {
                walk(m, f, then, at, out);
                walk(m, f, else_, at, out);
            }
            Stmt::Loop { body, continuing } => {
                walk(m, f, body, at, out);
                walk(m, f, continuing, at, out);
            }
            _ => {}
        }
    }
}

/// What an expression does that a GPU may do its own way, if anything.
fn expr_problem(m: &Module, f: &crate::Function, e: &Expr) -> Option<String> {
    let float = |x: ValueId| is_float(m, f, x);
    match e {
        Expr::Unary(op, a) if float(*a) => Some(format!("float arithmetic (`{op:?}`)")),
        Expr::Binary(op, a, b) if float(*a) || float(*b) => {
            if op.is_comparison() {
                Some(format!("a float comparison (`{op:?}`)"))
            } else {
                Some(format!("float arithmetic (`{op:?}`)"))
            }
        }
        Expr::Builtin(b, xs) => match b {
            Builtin::Dpdx | Builtin::Dpdy | Builtin::Fwidth => Some("a derivative".into()),
            Builtin::CountOnes | Builtin::LeadingZeros | Builtin::TrailingZeros => None,
            Builtin::Min | Builtin::Max | Builtin::Clamp | Builtin::Abs | Builtin::Sign
                if !xs.iter().any(|x| float(*x)) =>
            {
                None
            }
            Builtin::AllEqual if !xs.iter().any(|x| float(*x)) => None,
            other => Some(format!("a float builtin (`{other:?}`)")),
        },
        Expr::Convert(a, to) if float(*a) != to.is_float() => {
            Some("a conversion between a float and an integer".into())
        }
        Expr::Convert(a, to) if float(*a) && to.is_float() => {
            Some("a conversion between float widths".into())
        }
        Expr::Atomic(..) => Some("an atomic".into()),
        Expr::Subgroup(..) => Some("a subgroup operation".into()),
        Expr::Barrier => Some("a barrier".into()),
        Expr::Texture(..) | Expr::TextureStore(..) => Some("a texture".into()),
        Expr::Load(p) => place_problem(m, p).map(|w| format!("a read of {w}")),
        _ => None,
    }
}
