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

/// Clamps every index into a buffer or an array to its last element, in each function of a
/// `@deterministic` kernel's module (§18.14): an index past the end reads the last element on
/// every GPU and on the CPU, where WebGPU's robust access would give any value from inside the
/// buffer, or zero. A buffer of no elements still has none to give: there, a read is the GPU's
/// robust access, and the CPU's form doesn't run (its loop is over the output's places).
pub fn clamp_indexes(m: &mut Module) {
    for i in 0..m.functions.len() {
        let mut f = std::mem::take(&mut m.functions[i]);
        let body = std::mem::take(&mut f.body);
        f.body = clamp_block(m, &mut f, body);
        m.functions[i] = f;
    }
}

fn clamp_block(m: &mut Module, f: &mut crate::Function, b: crate::Block) -> crate::Block {
    let mut out = Vec::new();
    for s in b {
        match s {
            Stmt::If { cond, then, else_ } => {
                let then = clamp_block(m, f, then);
                let else_ = clamp_block(m, f, else_);
                out.push(Stmt::If { cond, then, else_ });
            }
            Stmt::Loop { body, continuing } => {
                let body = clamp_block(m, f, body);
                let continuing = clamp_block(m, f, continuing);
                out.push(Stmt::Loop { body, continuing });
            }
            Stmt::Store(p, v) => {
                let p = clamp_place(m, f, p, &mut out);
                out.push(Stmt::Store(p, v));
            }
            Stmt::Let(v, Expr::Load(p)) => {
                let p = clamp_place(m, f, p, &mut out);
                out.push(Stmt::Let(v, Expr::Load(p)));
            }
            Stmt::Let(v, Expr::ExtractDyn(a, i)) => {
                let n = match m.types.get(f.value_ty(a)) {
                    TypeDef::Array(_, n) => Some(*n),
                    _ => None,
                };
                let i = match n {
                    Some(n) if n > 0 => {
                        let u32t = m.types.u32();
                        let last = f.let_(&mut out, u32t, Expr::Const(crate::Const::U32(n - 1)));
                        clamped(m, f, i, last, &mut out)
                    }
                    _ => i,
                };
                out.push(Stmt::Let(v, Expr::ExtractDyn(a, i)));
            }
            other => out.push(other),
        }
    }
    out
}

/// `p` with each index clamped, the clamps' values defined at the end of `out`.
fn clamp_place(
    m: &mut Module,
    f: &mut crate::Function,
    p: crate::Place,
    out: &mut crate::Block,
) -> crate::Place {
    use crate::{BinOp, Const, Place, Proj, Scalar};
    let u32t = m.types.u32();
    let mut ty = match &p.root {
        PlaceRoot::Local(l) => Some(f.locals[l.index()].ty),
        PlaceRoot::Resource(r) => {
            let res = &m.resources[r.index()];
            match res.kind {
                // A buffer: a runtime array of its element type.
                ResourceKind::StorageRead | ResourceKind::StorageReadWrite => {
                    Some(m.types.intern(TypeDef::RuntimeArray(res.ty)))
                }
                _ => Some(res.ty),
            }
        }
        _ => None,
    };
    let mut path = Vec::new();
    for (k, proj) in p.path.iter().enumerate() {
        let here = ty.map(|t| m.types.get(t).clone());
        let next = match (&here, proj) {
            (Some(TypeDef::Struct { fields, .. }), Proj::Field(i)) => {
                fields.get(*i as usize).map(|f| f.1)
            }
            (Some(TypeDef::Vector(s, _)), Proj::Comp(_)) => {
                Some(m.types.intern(TypeDef::Scalar(*s)))
            }
            (Some(TypeDef::Array(e, _) | TypeDef::RuntimeArray(e)), Proj::Index(_)) => Some(*e),
            _ => None,
        };
        let proj = match (&here, proj) {
            (Some(TypeDef::Array(_, n)), Proj::Index(i)) if *n > 0 => {
                let last = f.let_(out, u32t, Expr::Const(Const::U32(n - 1)));
                Proj::Index(clamped(m, f, *i, last, out))
            }
            (Some(TypeDef::RuntimeArray(_)), Proj::Index(i)) => {
                let whole = Place { root: p.root.clone(), path: p.path[..k].to_vec() };
                let len = f.let_(out, u32t, Expr::ArrayLength(whole));
                let one = f.let_(out, u32t, Expr::Const(Const::U32(1)));
                let last = f.let_(out, u32t, Expr::Binary(BinOp::WrappingSub, len, one));
                Proj::Index(clamped(m, f, *i, last, out))
            }
            (_, other) => other.clone(),
        };
        path.push(proj);
        ty = next;
        let _ = Scalar::U32;
    }
    Place { root: p.root, path }
}

/// Index `i` (a `u32`, or an `i32`, below 0 taken as 0) clamped to `last`: a `u32`.
fn clamped(
    m: &mut Module,
    f: &mut crate::Function,
    i: ValueId,
    last: ValueId,
    out: &mut crate::Block,
) -> ValueId {
    use crate::{Const, Scalar};
    let u32t = m.types.u32();
    let i = if m.types.as_scalar(f.value_ty(i)) == Some(Scalar::I32) {
        let i32t = f.value_ty(i);
        let zero = f.let_(out, i32t, Expr::Const(Const::I32(0)));
        let pos = f.let_(out, i32t, Expr::Builtin(Builtin::Max, vec![i, zero]));
        f.let_(out, u32t, Expr::Convert(pos, Scalar::U32))
    } else {
        i
    };
    f.let_(out, u32t, Expr::Builtin(Builtin::Min, vec![i, last]))
}
