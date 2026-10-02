//! Derived interpretations of IR functions (language.md §13): forward-mode gradients ([`ad`])
//! and sound interval bounds ([`interval`]). Nothing here knows what a field is (D-056); any
//! function of floats qualifies, so long as it records no GPU work and has no loop whose exit
//! depends on its input (for intervals).
//!
//! Both transforms are driven by an **activity analysis**: which values and locals depend on the
//! input being derived for. Inactive code is copied as is, so a field's data (radii, offsets,
//! counts) costs nothing extra; only the arithmetic on the input's path is transformed.

pub mod ad;
pub mod interval;
pub(crate) mod single_exit;

use crate::*;
use std::collections::HashMap;

/// Derived functions already built in a module, so each is built once.
#[derive(Debug, Default)]
pub struct DeriveCache {
    ad: HashMap<(FuncId, Vec<bool>, u8), FuncId>,
    ad_returns: HashMap<(FuncId, Vec<bool>), bool>,
    interval: HashMap<(FuncId, Vec<bool>), FuncId>,
    interval_returns: HashMap<(FuncId, Vec<bool>), bool>,
}

/// A function of `f`'s captures and `x` (its last parameter) returning `(f32, X)`: `f(x)` and
/// its gradient with respect to `x`.
pub fn value_and_gradient(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    ncap: u32,
) -> Result<FuncId, String> {
    ad::value_and_gradient(m, cache, f, ncap)
}

/// A function of `f`'s captures and a box (`box_ty`, with fields `lo` and `hi`) returning an
/// interval (`interval_ty`, likewise) that contains `f(x)`, as the target computes it, for every
/// `x` in the box. On the GPU each operation's result is widened by its WGSL error bound
/// (D-075); on the CPU by its rounding.
pub fn interval(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    ncap: u32,
    target: Target,
    box_ty: TypeId,
    interval_ty: TypeId,
) -> Result<FuncId, String> {
    interval::interval(m, cache, f, ncap, target, box_ty, interval_ty)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Only floats carry derivatives; control flow doesn't.
    Ad,
    /// Anything can carry a range, and a branch on a range makes what it assigns a range too.
    Interval,
}

/// Which values, locals and parameters depend on the active input.
#[derive(Clone, Debug)]
pub(crate) struct Activity {
    pub values: Vec<bool>,
    pub locals: Vec<bool>,
    pub params: Vec<bool>,
    pub returns: bool,
    /// Interval mode: a loop whose exit depends on the input (unsupported).
    pub active_loop_exit: bool,
}

pub(crate) fn place_root_active(a: &Activity, p: &Place) -> bool {
    let root = match &p.root {
        PlaceRoot::Local(l) => a.locals[l.index()],
        PlaceRoot::Param(i) => a.params[*i as usize],
        PlaceRoot::Ptr(v) => a.values[v.index()],
        PlaceRoot::Resource(_) => false,
    };
    root || p.path.iter().any(|x| matches!(x, Proj::Index(v) if a.values[v.index()]))
}

/// Whether a call's argument is active.
pub(crate) fn arg_active(a: &Activity, arg: &Arg) -> bool {
    match arg {
        Arg::Value(v) => a.values[v.index()],
        Arg::Place(p) => place_root_active(a, p),
    }
}

/// The activity analysis: a fixpoint over the whole body (flow-insensitive per local, which is
/// sound and simple). `callee_returns` answers whether a call's result is active given which of
/// its arguments are.
pub(crate) fn activity(
    m: &Module,
    f: &Function,
    params: &[bool],
    mode: Mode,
    callee_returns: &mut dyn FnMut(FuncId, Vec<bool>) -> Result<bool, String>,
) -> Result<Activity, String> {
    let mut a = Activity {
        values: vec![false; f.values.len()],
        locals: vec![false; f.locals.len()],
        params: params.to_vec(),
        returns: false,
        active_loop_exit: false,
    };
    loop {
        let before = (a.values.clone(), a.locals.clone(), a.returns);
        walk(m, f, &f.body, &mut a, mode, false, callee_returns)?;
        if before == (a.values.clone(), a.locals.clone(), a.returns) {
            break;
        }
    }
    Ok(a)
}

fn carries(m: &Module, t: TypeId, mode: Mode) -> bool {
    match mode {
        Mode::Ad => m.types.has_float(t),
        Mode::Interval => !matches!(m.types.get(t), TypeDef::Ptr(_)),
    }
}

#[allow(clippy::too_many_arguments)]
fn walk(
    m: &Module,
    f: &Function,
    b: &Block,
    a: &mut Activity,
    mode: Mode,
    ctrl: bool,
    callee_returns: &mut dyn FnMut(FuncId, Vec<bool>) -> Result<bool, String>,
) -> Result<(), String> {
    for s in b {
        match s {
            Stmt::Let(v, e) => {
                if let Expr::Call(g, args) = e {
                    call_writes(m, a, mode, ctrl, *g, args);
                }
                let active = expr_active(e, a, callee_returns)? && carries(m, f.value_ty(*v), mode);
                if active {
                    a.values[v.index()] = true;
                }
            }
            Stmt::Eval(Expr::Call(g, args)) => call_writes(m, a, mode, ctrl, *g, args),
            Stmt::Eval(_) => {}
            Stmt::Store(p, v) => {
                if a.values[v.index()] || (mode == Mode::Interval && ctrl) {
                    mark_place(a, p);
                }
            }
            Stmt::If { cond, then, else_ } => {
                let c = ctrl || (mode == Mode::Interval && a.values[cond.index()]);
                walk(m, f, then, a, mode, c, callee_returns)?;
                walk(m, f, else_, a, mode, c, callee_returns)?;
            }
            Stmt::Loop { body, continuing } => {
                walk(m, f, body, a, mode, ctrl, callee_returns)?;
                walk(m, f, continuing, a, mode, ctrl, callee_returns)?;
                if mode == Mode::Interval && loop_exit_active(body, a) {
                    a.active_loop_exit = true;
                }
            }
            Stmt::Return(Some(v)) => {
                if a.values[v.index()] || (mode == Mode::Interval && ctrl) {
                    a.returns = true;
                }
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::Trap => {}
        }
    }
    Ok(())
}

/// Whether a loop's `break`, `continue` (or `return`) sits under a condition that depends on
/// the input.
fn loop_exit_active(b: &Block, a: &Activity) -> bool {
    fn exits(b: &Block) -> bool {
        b.iter().any(|s| match s {
            Stmt::Break | Stmt::Continue | Stmt::Return(_) => true,
            Stmt::If { then, else_, .. } => exits(then) || exits(else_),
            _ => false,
        })
    }
    b.iter().any(|s| match s {
        Stmt::If { cond, then, else_ } => {
            (a.values[cond.index()] && (exits(then) || exits(else_)))
                || loop_exit_active(then, a)
                || loop_exit_active(else_, a)
        }
        _ => false,
    })
}

/// A callee given active arguments may write active values through its `mut` parameters, and
/// in interval mode, so may one that runs under a condition on the input.
fn call_writes(m: &Module, a: &mut Activity, mode: Mode, ctrl: bool, g: FuncId, args: &[Arg]) {
    if args.iter().any(|x| arg_active(a, x)) || (mode == Mode::Interval && ctrl) {
        for (arg, p) in args.iter().zip(&m.functions[g.index()].params) {
            if let (Arg::Place(pl), true) = (arg, p.mutable) {
                mark_place(a, pl);
            }
        }
    }
}

fn mark_place(a: &mut Activity, p: &Place) {
    match &p.root {
        PlaceRoot::Local(l) => a.locals[l.index()] = true,
        PlaceRoot::Param(i) => a.params[*i as usize] = true,
        _ => {}
    }
}

fn expr_active(
    e: &Expr,
    a: &Activity,
    callee_returns: &mut dyn FnMut(FuncId, Vec<bool>) -> Result<bool, String>,
) -> Result<bool, String> {
    let v = |x: &ValueId| a.values[x.index()];
    Ok(match e {
        Expr::Const(_) | Expr::Zero(_) | Expr::EntryInput(_) => false,
        Expr::Param(i) => a.params[*i as usize],
        Expr::Load(p) | Expr::Run(p) | Expr::Addr(p) => place_root_active(a, p),
        Expr::Unary(_, x)
        | Expr::Extract(x, _)
        | Expr::Splat(x, _)
        | Expr::Swizzle(x, _)
        | Expr::Convert(x, _)
        | Expr::Bitcast(x, _) => v(x),
        Expr::Binary(_, x, y) | Expr::ExtractDyn(x, y) => v(x) || v(y),
        Expr::Builtin(_, xs) | Expr::Construct(_, xs) | Expr::Host(_, xs) => xs.iter().any(v),
        Expr::Select { cond, if_true, if_false } => v(cond) || v(if_true) || v(if_false),
        Expr::Call(g, args) => {
            let mask: Vec<bool> = args.iter().map(|x| arg_active(a, x)).collect();
            if !mask.iter().any(|b| *b) {
                false
            } else {
                // Arguments map to the callee's parameters one to one.
                callee_returns(*g, mask)?
            }
        }
    })
}

/// Every statement's block, recursively: for `Let`s, gather their types.
pub(crate) fn has_host_or_ptr(b: &Block) -> Option<&'static str> {
    for s in b {
        match s {
            Stmt::Let(_, Expr::Host(..)) | Stmt::Eval(Expr::Host(..)) => {
                return Some("records GPU work");
            }
            Stmt::Let(_, Expr::Addr(_)) => return Some("returns a projection"),
            Stmt::If { then, else_, .. } => {
                if let Some(r) = has_host_or_ptr(then).or_else(|| has_host_or_ptr(else_)) {
                    return Some(r);
                }
            }
            Stmt::Loop { body, continuing } => {
                if let Some(r) = has_host_or_ptr(body).or_else(|| has_host_or_ptr(continuing)) {
                    return Some(r);
                }
            }
            _ => {}
        }
    }
    None
}

/// The type of a place, walking its projections.
pub(crate) fn place_type(m: &Module, f: &Function, p: &Place) -> TypeId {
    let mut t = match &p.root {
        PlaceRoot::Local(l) => f.locals[l.index()].ty,
        PlaceRoot::Param(i) => f.params[*i as usize].ty,
        PlaceRoot::Resource(r) => m.resources[r.index()].ty,
        PlaceRoot::Ptr(v) => match m.types.get(f.value_ty(*v)) {
            TypeDef::Ptr(t) => *t,
            _ => f.value_ty(*v),
        },
    };
    for proj in &p.path {
        t = match (m.types.get(t), proj) {
            (TypeDef::Struct { fields, .. }, Proj::Field(k)) => fields[*k as usize].1,
            (TypeDef::Vector(_), _) => find(m, &TypeDef::Scalar(Scalar::F32)),
            (TypeDef::Matrix(n), _) => find(m, &TypeDef::Vector(*n)),
            (TypeDef::Array(e, _) | TypeDef::RuntimeArray(e) | TypeDef::Run(e), _) => *e,
            _ => t,
        };
    }
    t
}

pub(crate) fn find(m: &Module, d: &TypeDef) -> TypeId {
    m.types.iter().find(|(_, x)| *x == d).map_or(TypeId(0), |(t, _)| t)
}
