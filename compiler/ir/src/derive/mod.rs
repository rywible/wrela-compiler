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
use std::collections::{HashMap, HashSet};

/// Derived functions already built in a module, so each is built once.
#[derive(Debug, Default)]
pub struct DeriveCache {
    ad: HashMap<(FuncId, Vec<bool>, u8), FuncId>,
    interval: HashMap<(FuncId, Vec<bool>), FuncId>,
    /// Whether a function returns an active value, by derivation and active parameters.
    returns: HashMap<(Mode, FuncId, Vec<bool>), bool>,
}

/// A function of `f`'s captures and `x` (its last parameter) returning `(f32, X)`: `f(x)` and
/// its gradient with respect to `x`.
pub fn value_and_gradient(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    ncap: u32,
) -> Result<FuncId> {
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
) -> Result<FuncId> {
    interval::interval(m, cache, f, ncap, target, box_ty, interval_ty)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    callee_returns: &mut dyn FnMut(FuncId, Vec<bool>) -> Result<bool>,
) -> Result<Activity> {
    let mut a = Activity {
        values: vec![false; f.values.len()],
        locals: vec![false; f.locals.len()],
        params: params.to_vec(),
        returns: false,
        active_loop_exit: false,
    };
    let mut uses = vec![0u32; f.locals.len()];
    count_local_uses(&f.body, &mut uses);
    let cx = Walk { m, f, mode, uses };
    loop {
        let before = (a.values.clone(), a.locals.clone(), a.returns);
        walk(&cx, &f.body, &mut a, None, callee_returns)?;
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

struct Walk<'a> {
    m: &'a Module,
    f: &'a Function,
    mode: Mode,
    /// How many places mention each local, in the whole function.
    uses: Vec<u32>,
}

/// Counts the places that mention each local.
fn count_local_uses(b: &Block, uses: &mut [u32]) {
    let mut place = |p: &Place| {
        if let PlaceRoot::Local(l) = p.root {
            uses[l.index()] += 1;
        }
    };
    let mut stack = vec![b];
    while let Some(b) = stack.pop() {
        for s in b {
            match s {
                Stmt::Let(_, e) | Stmt::Eval(e) => match e {
                    Expr::Load(p) | Expr::Run(p) | Expr::Addr(p) | Expr::ArrayLength(p) => place(p),
                    Expr::Call(_, args) => {
                        for a in args {
                            if let Arg::Place(p) = a {
                                place(p);
                            }
                        }
                    }
                    _ => {}
                },
                Stmt::Store(p, _) => place(p),
                Stmt::If { then, else_, .. } => {
                    stack.push(then);
                    stack.push(else_);
                }
                Stmt::Loop { body, continuing } => {
                    stack.push(body);
                    stack.push(continuing);
                }
                _ => {}
            }
        }
    }
}

/// In interval mode, under a branch on the input, a store makes its place a range (both sides
/// may run, and what they wrote is joined after the branch). `ctrl` is then the set of locals
/// mentioned only inside the innermost such branch: their values never reach the join, so
/// storing to them doesn't.
fn walk(
    cx: &Walk<'_>,
    b: &Block,
    a: &mut Activity,
    ctrl: Option<&HashSet<LocalId>>,
    callee_returns: &mut dyn FnMut(FuncId, Vec<bool>) -> Result<bool>,
) -> Result<()> {
    let (m, f, mode) = (cx.m, cx.f, cx.mode);
    let joined = |p: &Place| match (ctrl, &p.root) {
        (None, _) => false,
        (Some(inside), PlaceRoot::Local(l)) => !inside.contains(l),
        (Some(_), _) => true,
    };
    for s in b {
        match s {
            Stmt::Let(v, e) => {
                if let Expr::Call(g, args) = e {
                    call_writes(m, a, *g, args, &joined);
                }
                let active = expr_active(e, a, callee_returns)? && carries(m, f.value_ty(*v), mode);
                if active {
                    a.values[v.index()] = true;
                }
            }
            Stmt::Eval(Expr::Call(g, args)) => call_writes(m, a, *g, args, &joined),
            Stmt::Eval(_) => {}
            Stmt::Store(p, v) => {
                if a.values[v.index()] || joined(p) {
                    mark_place(a, p);
                }
            }
            Stmt::If { cond, then, else_ } => {
                if mode == Mode::Interval && a.values[cond.index()] {
                    let mut inside = vec![0u32; f.locals.len()];
                    count_local_uses(then, &mut inside);
                    count_local_uses(else_, &mut inside);
                    let internal: HashSet<LocalId> = (0..f.locals.len())
                        .filter(|&l| inside[l] > 0 && inside[l] == cx.uses[l])
                        .map(|l| LocalId(l as u32))
                        .collect();
                    walk(cx, then, a, Some(&internal), callee_returns)?;
                    walk(cx, else_, a, Some(&internal), callee_returns)?;
                } else {
                    walk(cx, then, a, ctrl, callee_returns)?;
                    walk(cx, else_, a, ctrl, callee_returns)?;
                }
            }
            Stmt::Loop { body, continuing } => {
                walk(cx, body, a, ctrl, callee_returns)?;
                walk(cx, continuing, a, ctrl, callee_returns)?;
                if mode == Mode::Interval && loop_exit_active(body, a) {
                    a.active_loop_exit = true;
                }
            }
            Stmt::Return(Some(v)) => {
                if a.values[v.index()] || ctrl.is_some() {
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
/// in interval mode, so may one that runs under a condition on the input (`joined`).
fn call_writes(
    m: &Module,
    a: &mut Activity,
    g: FuncId,
    args: &[Arg],
    joined: &dyn Fn(&Place) -> bool,
) {
    let any = args.iter().any(|x| arg_active(a, x));
    for (arg, p) in args.iter().zip(&m.functions[g.index()].params) {
        if let (Arg::Place(pl), true) = (arg, p.mutable)
            && (any || joined(pl))
        {
            mark_place(a, pl);
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
    callee_returns: &mut dyn FnMut(FuncId, Vec<bool>) -> Result<bool>,
) -> Result<bool> {
    let v = |x: &ValueId| a.values[x.index()];
    Ok(match e {
        Expr::Const(_) | Expr::Zero(_) | Expr::EntryInput(_) => false,
        Expr::Param(i) => a.params[*i as usize],
        Expr::Load(p) | Expr::Run(p) | Expr::Addr(p) | Expr::ArrayLength(p) => {
            place_root_active(a, p)
        }
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
/// Whether `f` with these active parameters returns an active value: a derivative (for
/// [`Mode::Ad`], of a float result) or a range. Memoized; a recursive call still being analyzed
/// is assumed active.
pub(super) fn returns_active(
    m: &Module,
    cache: &mut DeriveCache,
    f: FuncId,
    mask: Vec<bool>,
    mode: Mode,
) -> Result<bool> {
    let key = (mode, f, mask);
    if let Some(&r) = cache.returns.get(&key) {
        return Ok(r);
    }
    cache.returns.insert(key.clone(), true);
    let func = &m.functions[f.index()];
    let a = activity(m, func, &key.2, mode, &mut |g, mk| returns_active(m, cache, g, mk, mode))?;
    let r = a.returns && func.ret.is_some_and(|t| mode == Mode::Interval || m.types.has_float(t));
    cache.returns.insert(key, r);
    Ok(r)
}

/// Loads of a place that nothing wrote between are the same value, so `x * x` and `dot(v, v)`
/// can get the rules for squares. Tracked within a straight run of statements.
#[derive(Default)]
pub(super) struct LoadTwins {
    loads: Vec<(Place, ValueId)>,
    /// A load's earlier twin.
    same: HashMap<ValueId, ValueId>,
}

impl LoadTwins {
    /// Before a statement: notes a load, and forgets the loads a write, a call or a branch may
    /// change.
    pub(super) fn before(&mut self, s: &Stmt) {
        match s {
            Stmt::Let(v, Expr::Load(p)) => match self.loads.iter().find(|(q, _)| q == p) {
                Some(&(_, w)) => {
                    self.same.insert(*v, w);
                }
                None => self.loads.push((p.clone(), *v)),
            },
            Stmt::Let(_, Expr::Call(..))
            | Stmt::Eval(_)
            | Stmt::Store(..)
            | Stmt::If { .. }
            | Stmt::Loop { .. } => self.loads.clear(),
            Stmt::Let(..) | Stmt::Break | Stmt::Continue | Stmt::Return(_) | Stmt::Trap => {}
        }
    }

    /// After a statement: forgets the loads made in its blocks.
    pub(super) fn after(&mut self, s: &Stmt) {
        if s.blocks().iter().any(|b| !b.is_empty()) {
            self.loads.clear();
        }
    }

    /// Whether two values are surely equal: the same value, or loads of an unchanged place.
    pub(super) fn same_value(&self, a: ValueId, b: ValueId) -> bool {
        let c = |v: ValueId| self.same.get(&v).copied().unwrap_or(v);
        c(a) == c(b)
    }
}

/// The size of the splat that makes a value of type `from` an operand of type `to`, if it
/// takes one: a scalar next to a vector, in the derivations' own operations (the IR wants the
/// operands of an elementwise operation of one type).
pub(super) fn splat_size(types: &Types, from: TypeId, to: TypeId) -> Option<u8> {
    match (types.get(from), types.get(to)) {
        (TypeDef::Scalar(_), &TypeDef::Vector(n)) => Some(n),
        _ => None,
    }
}

/// What both derivations do with a statement they leave as it is: rebuild an `if` or a loop
/// from their versions of its blocks (`block`), and copy anything else.
pub(super) fn pass_through(
    s: &Stmt,
    out: &mut Block,
    block: &mut impl FnMut(&Block) -> Result<Block>,
) -> Result<()> {
    out.push(match s {
        Stmt::If { cond, then, else_ } => {
            Stmt::If { cond: *cond, then: block(then)?, else_: block(else_)? }
        }
        Stmt::Loop { body, continuing } => {
            Stmt::Loop { body: block(body)?, continuing: block(continuing)? }
        }
        other => other.clone(),
    });
    Ok(())
}

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

/// The type of a place.
pub(crate) fn place_type(m: &Module, f: &Function, p: &Place) -> Result<TypeId> {
    m.place_ty(f, p).ok_or_else(|| Error::internal(format!("a place with no type: {p:?}")))
}
