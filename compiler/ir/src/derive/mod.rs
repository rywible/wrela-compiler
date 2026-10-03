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

use crate::*;
use std::collections::{HashMap, HashSet};

/// Derived functions already built in a module, so each is built once.
#[derive(Debug, Default)]
pub struct DeriveCache {
    ad: HashMap<(FuncId, Vec<bool>, u8), FuncId>,
    interval: HashMap<(FuncId, Vec<bool>), FuncId>,
    /// What a call does with active arguments, by derivation and active parameters: `None`
    /// while the callee is still being analyzed.
    effects: HashMap<(Mode, FuncId, Vec<bool>), Option<Effect>>,
    /// The activity [`effect`] found, kept for deriving the function when it's final.
    activity: HashMap<(Mode, FuncId, Vec<bool>), Activity>,
    /// The intervals being built, outermost first, and whether the call that asked for each
    /// runs under a branch on the input.
    building: Vec<(FuncId, Vec<bool>, bool)>,
}

/// A function of `f`'s captures and `x` (its last parameter) returning `(f32, X)`: `f(x)` and
/// its gradient with respect to `x`, for `target`.
pub fn value_and_gradient(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    ncap: u32,
    target: Target,
) -> Result<FuncId> {
    ad::value_and_gradient(m, cache, f, ncap, target)
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

/// What a call does, given which of its arguments are active.
#[derive(Clone, Debug)]
pub(crate) struct Effect {
    /// Whether its result is active.
    pub returns: bool,
    /// By parameter: whether it may write an active value through it (a `mut` one).
    pub writes: Vec<bool>,
}

/// How the activity analysis learns what a call does.
pub(crate) type Callee<'c> = dyn FnMut(FuncId, Vec<bool>) -> Result<Effect> + 'c;

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
    /// Interval mode: a loop whose exit depends on the input (unsupported), and where its
    /// exit's condition is in the source, if known.
    pub active_loop_exit: Option<Option<wrela_diag::Span>>,
}

pub(crate) fn place_root_active(a: &Activity, p: &Place) -> bool {
    let root = match &p.root {
        PlaceRoot::Local(l) => a.locals[l.index()],
        PlaceRoot::Param(i) => a.params[*i as usize],
        PlaceRoot::Ptr(v) => a.values[v.index()],
        PlaceRoot::Resource(_) | PlaceRoot::Data(_) => false,
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
/// sound and simple). `callee` answers what a call does given which of its arguments are
/// active.
pub(crate) fn activity(
    m: &Module,
    f: &Function,
    params: &[bool],
    mode: Mode,
    callee: &mut Callee<'_>,
) -> Result<Activity> {
    let mut a = Activity {
        values: vec![false; f.values.len()],
        locals: vec![false; f.locals.len()],
        params: params.to_vec(),
        returns: false,
        active_loop_exit: None,
    };
    let mut uses = vec![0u32; f.locals.len()];
    visit::count_local_mentions(&f.body, &mut uses);
    let cx = Walk { m, f, mode, uses, ptrs: Pointers::of(m, f) };
    // Bits only turn on, so a round changed something when it turned more on.
    let on = |a: &Activity| {
        a.values.iter().chain(&a.locals).filter(|x| **x).count() + a.returns as usize
    };
    loop {
        let before = on(&a);
        walk(&cx, &f.body, &mut a, None, callee)?;
        if on(&a) == before {
            break;
        }
    }
    Ok(a)
}

fn carries(m: &Module, t: TypeId, mode: Mode) -> bool {
    match mode {
        Mode::Ad => m.types.has_float(t),
        Mode::Interval => true,
    }
}

struct Walk<'a> {
    m: &'a Module,
    f: &'a Function,
    mode: Mode,
    /// How many places mention each local, in the whole function.
    uses: Vec<u32>,
    ptrs: Pointers,
}

impl Walk<'_> {
    /// Marks a written place active: its root, or what a pointer root may point into (every
    /// local and parameter, if that isn't known).
    fn mark(&self, a: &mut Activity, p: &Place) {
        let PlaceRoot::Ptr(v) = p.root else {
            return mark_place(a, p);
        };
        let mut roots = Vec::new();
        if self.ptrs.roots(v, &mut roots).is_none() {
            a.locals.iter_mut().chain(a.params.iter_mut()).for_each(|x| *x = true);
        }
        for r in roots {
            mark_place(a, &Place::root(r));
        }
    }
}

/// Where a function's pointer values may point: into the place an address is of, or into a
/// place a projection's call gave it to write (a `mut` projection is of a `mut` parameter).
/// A derived projection returns its pointers in a struct, so the parts of a value are followed
/// too.
pub(crate) struct Pointers {
    into: HashMap<ValueId, Vec<Place>>,
}

impl Pointers {
    pub(crate) fn of(m: &Module, f: &Function) -> Pointers {
        let mut into: HashMap<ValueId, Vec<Place>> = HashMap::new();
        visit::walk(&f.body, &mut |s| {
            let Stmt::Let(v, e) = s else { return };
            if !has_ptr(&m.types, f.value_ty(*v)) {
                return;
            }
            let places = match e {
                Expr::Addr(p) => vec![p.clone()],
                Expr::Call(g, args) => {
                    let params = &m.functions[g.index()].params;
                    let places = args.iter().zip(params).filter_map(|(a, p)| match a {
                        Arg::Place(pl) if p.mutable => Some(pl.clone()),
                        _ => None,
                    });
                    places.collect()
                }
                Expr::Extract(..)
                | Expr::ExtractDyn(..)
                | Expr::Construct(..)
                | Expr::Select { .. } => {
                    let mut places = Vec::new();
                    let mut known = true;
                    e.for_each_value(&mut |x| match into.get(&x) {
                        Some(ps) => places.extend(ps.iter().cloned()),
                        None => known &= !has_ptr(&m.types, f.value_ty(x)),
                    });
                    if !known {
                        return;
                    }
                    places
                }
                _ => return,
            };
            into.insert(*v, places);
        });
        Pointers { into }
    }

    /// The locals and parameters the pointer `v` may point into. `None` when it can't tell.
    pub(crate) fn roots(&self, v: ValueId, into: &mut Vec<PlaceRoot>) -> Option<()> {
        for p in self.into.get(&v)? {
            match &p.root {
                PlaceRoot::Ptr(w) => self.roots(*w, into)?,
                r => into.push(r.clone()),
            }
        }
        Some(())
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
    callee: &mut Callee<'_>,
) -> Result<()> {
    let (m, f, mode) = (cx.m, cx.f, cx.mode);
    let joined = |p: &Place| match (ctrl, &p.root) {
        (None, _) => false,
        (Some(inside), PlaceRoot::Local(l)) => !inside.contains(l),
        (Some(_), _) => true,
    };
    let mut at = None;
    for s in b {
        match s {
            Stmt::Let(v, e) => {
                let active = match e {
                    Expr::Call(g, args) => call(cx, a, *g, args, &joined, callee)?,
                    e => expr_active(e, a),
                };
                if active && carries(m, f.value_ty(*v), mode) {
                    a.values[v.index()] = true;
                }
            }
            Stmt::Eval(Expr::Call(g, args)) => {
                call(cx, a, *g, args, &joined, callee)?;
            }
            Stmt::Eval(_) => {}
            Stmt::Store(p, v) => {
                if a.values[v.index()] || joined(p) {
                    cx.mark(a, p);
                }
            }
            Stmt::If { cond, then, else_ } => {
                if mode == Mode::Interval && a.values[cond.index()] {
                    let mut inside = vec![0u32; f.locals.len()];
                    visit::count_local_mentions(then, &mut inside);
                    visit::count_local_mentions(else_, &mut inside);
                    let internal: HashSet<LocalId> = (0..f.locals.len())
                        .filter(|&l| inside[l] > 0 && inside[l] == cx.uses[l])
                        .map(|l| LocalId(l as u32))
                        .collect();
                    walk(cx, then, a, Some(&internal), callee)?;
                    walk(cx, else_, a, Some(&internal), callee)?;
                } else {
                    walk(cx, then, a, ctrl, callee)?;
                    walk(cx, else_, a, ctrl, callee)?;
                }
            }
            Stmt::Loop { body, continuing } => {
                walk(cx, body, a, ctrl, callee)?;
                walk(cx, continuing, a, ctrl, callee)?;
                if mode == Mode::Interval
                    && a.active_loop_exit.is_none()
                    && let Some(exit) = loop_exit(body, a, at)
                {
                    a.active_loop_exit = Some(exit);
                }
            }
            Stmt::Return(Some(v)) => {
                if a.values[v.index()] || ctrl.is_some() {
                    a.returns = true;
                }
            }
            Stmt::At(span) => at = Some(*span),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::Trap => {}
        }
    }
    Ok(())
}

/// Whether a loop's `break`, `continue` (or `return`) sits under a condition that depends on
/// the input.
/// If so, where the condition is in the source, if known (the last `At` before it; `at` before
/// the block).
fn loop_exit(
    b: &Block,
    a: &Activity,
    mut at: Option<wrela_diag::Span>,
) -> Option<Option<wrela_diag::Span>> {
    fn exits(b: &Block) -> bool {
        b.iter().any(|s| match s {
            Stmt::Break | Stmt::Continue | Stmt::Return(_) => true,
            Stmt::If { then, else_, .. } => exits(then) || exits(else_),
            _ => false,
        })
    }
    for s in b {
        match s {
            Stmt::At(span) => at = Some(*span),
            Stmt::If { cond, then, else_ } => {
                if a.values[cond.index()] && (exits(then) || exits(else_)) {
                    return Some(at);
                }
                if let Some(e) = loop_exit(then, a, at).or_else(|| loop_exit(else_, a, at)) {
                    return Some(e);
                }
            }
            _ => {}
        }
    }
    None
}

/// A call: whether its result is active. A callee given active arguments may write active
/// values through its `mut` parameters (those its own activity marks), and in interval mode,
/// so may one that runs under a condition on the input (`joined`).
fn call(
    cx: &Walk<'_>,
    a: &mut Activity,
    g: FuncId,
    args: &[Arg],
    joined: &dyn Fn(&Place) -> bool,
    callee: &mut Callee<'_>,
) -> Result<bool> {
    let mask: Vec<bool> = args.iter().map(|x| arg_active(a, x)).collect();
    // Arguments map to the callee's parameters one to one.
    let effect = if mask.iter().any(|b| *b) { Some(callee(g, mask)?) } else { None };
    for (i, (arg, p)) in args.iter().zip(&cx.m.functions[g.index()].params).enumerate() {
        let writes = effect.as_ref().is_some_and(|e| e.writes[i]);
        if let (Arg::Place(pl), true) = (arg, p.mutable)
            && (writes || joined(pl))
        {
            cx.mark(a, pl);
        }
    }
    Ok(effect.is_some_and(|e| e.returns))
}

fn mark_place(a: &mut Activity, p: &Place) {
    match &p.root {
        PlaceRoot::Local(l) => a.locals[l.index()] = true,
        PlaceRoot::Param(i) => a.params[*i as usize] = true,
        _ => {}
    }
}

fn expr_active(e: &Expr, a: &Activity) -> bool {
    let v = |x: &ValueId| a.values[x.index()];
    match e {
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
        | Expr::Bitcast(x, _)
        | Expr::Variant(_, _, Some(x)) => v(x),
        Expr::Variant(_, _, None) => false,
        Expr::Binary(_, x, y) | Expr::ExtractDyn(x, y) => v(x) || v(y),
        Expr::Builtin(_, xs) | Expr::Construct(_, xs) | Expr::Host(_, xs) => xs.iter().any(v),
        Expr::Select { cond, if_true, if_false } => v(cond) || v(if_true) || v(if_false),
        Expr::Call(..) => unreachable!("calls are handled in `call`"),
    }
}

/// Whether `f` with these active parameters returns an active value: a derivative (for
/// [`Mode::Ad`], of a float result) or a range.
pub(super) fn returns_active(
    m: &Module,
    cache: &mut DeriveCache,
    f: FuncId,
    mask: Vec<bool>,
    mode: Mode,
) -> Result<bool> {
    Ok(effect(m, cache, f, mask, mode)?.returns)
}

/// What a call of `f` with these active parameters does. Memoized; a recursive call still being
/// analyzed is assumed to return an active value and to write through every `mut` parameter.
pub(super) fn effect(
    m: &Module,
    cache: &mut DeriveCache,
    f: FuncId,
    mask: Vec<bool>,
    mode: Mode,
) -> Result<Effect> {
    let func = &m.functions[f.index()];
    let key = (mode, f, mask);
    match cache.effects.get(&key) {
        Some(Some(e)) => return Ok(e.clone()),
        Some(None) => {
            let writes = func.params.iter().map(|p| p.mutable).collect();
            return Ok(Effect { returns: true, writes });
        }
        None => {}
    }
    cache.effects.insert(key.clone(), None);
    let mut assumed = false;
    let a = activity(m, func, &key.2, mode, &mut |g, mk| {
        assumed |= matches!(cache.effects.get(&(mode, g, mk.clone())), Some(None));
        effect(m, cache, g, mk, mode)
    })?;
    let returns =
        a.returns && func.ret.is_some_and(|t| mode == Mode::Interval || m.types.has_float(t));
    let writes = func.params.iter().zip(&a.params).map(|(p, w)| p.mutable && *w).collect();
    let e = Effect { returns, writes };
    cache.effects.insert(key.clone(), Some(e.clone()));
    // An activity that took no answer from a function still being analyzed is final: deriving
    // `f` would find it again.
    if !assumed {
        cache.activity.insert(key, a);
    }
    Ok(e)
}

/// The activity of `f`'s body with active parameters `mask`: the one [`effect`] kept, if it
/// did.
pub(super) fn body_activity(
    m: &Module,
    cache: &mut DeriveCache,
    f: FuncId,
    mask: &[bool],
    mode: Mode,
) -> Result<Activity> {
    if let Some(a) = cache.activity.get(&(mode, f, mask.to_vec())) {
        return Ok(a.clone());
    }
    activity(m, &m.functions[f.index()], mask, mode, &mut |g, mk| effect(m, cache, g, mk, mode))
}

/// What both derivations build code with. Each emits a value its own way; the rest is the same.
pub(super) trait Builder {
    /// A new value of type `ty`, defined as `e` in `out`.
    fn emit(&mut self, out: &mut Block, ty: TypeId, e: Expr) -> ValueId;

    fn types(&self) -> &Types;

    /// The type of a value of the function being built.
    fn ty(&self, v: ValueId) -> TypeId;

    /// `x` as an operand of type `to` (see [`splat_size`]).
    fn coerce(&mut self, out: &mut Block, x: ValueId, to: TypeId) -> ValueId {
        match splat_size(self.types(), self.ty(x), to) {
            Some(n) => self.emit(out, to, Expr::Splat(x, n)),
            None => x,
        }
    }

    /// `a op b` of type `ty`, a scalar operand splatted to a vector `ty` (the IR's elementwise
    /// operations take one type).
    fn bin(&mut self, out: &mut Block, op: BinOp, a: ValueId, b: ValueId, ty: TypeId) -> ValueId {
        let (a, b) = (self.coerce(out, a, ty), self.coerce(out, b, ty));
        self.emit(out, ty, Expr::Binary(op, a, b))
    }

    /// The builtin `b` of type `ty`; an elementwise one's scalar arguments are splatted to a
    /// vector `ty`.
    fn builtin(&mut self, out: &mut Block, b: Builtin, args: Vec<ValueId>, ty: TypeId) -> ValueId {
        let args = if b.is_elementwise() {
            args.into_iter().map(|x| self.coerce(out, x, ty)).collect()
        } else {
            args
        };
        self.emit(out, ty, Expr::Builtin(b, args))
    }
}

/// What both derivations keep track of from one statement to the next: where in the source the
/// statement is, and which loads are the same value. Two loads of a place that nothing wrote
/// between are the same value, so `x * x` and `dot(v, v)` can get the rules for squares; this is
/// tracked within a straight run of statements.
#[derive(Default)]
pub(super) struct Track {
    loads: Vec<(Place, ValueId)>,
    /// A load's earlier twin.
    same: HashMap<ValueId, ValueId>,
    /// Where in the source the statement being derived is (the last `Stmt::At`), for errors.
    at: Option<wrela_diag::Span>,
}

impl Track {
    /// Before deriving a statement: notes where it is and a load, and forgets the loads a write,
    /// a call or a branch may change.
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
            Stmt::At(span) => self.at = Some(*span),
            Stmt::Let(..) | Stmt::Break | Stmt::Continue | Stmt::Return(_) | Stmt::Trap => {}
        }
    }

    /// After deriving a statement, with result `r`: forgets the loads made in its blocks, and
    /// places an error at the statement.
    pub(super) fn after(&mut self, s: &Stmt, r: Result<()>) -> Result<()> {
        if s.blocks().any(|b| !b.is_empty()) {
            self.loads.clear();
        }
        r.map_err(|e| e.at(self.at))
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

/// Where a block records GPU work (a host operation), which a derived function can't do: the
/// last `At` before it (`at` before the block).
pub(crate) fn records_gpu_work(
    b: &Block,
    mut at: Option<wrela_diag::Span>,
) -> Option<Option<wrela_diag::Span>> {
    for s in b {
        match s {
            Stmt::At(span) => at = Some(*span),
            Stmt::Let(_, Expr::Host(..)) | Stmt::Eval(Expr::Host(..)) => return Some(at),
            Stmt::If { then, else_, .. } => {
                if let Some(r) = records_gpu_work(then, at).or_else(|| records_gpu_work(else_, at))
                {
                    return Some(r);
                }
            }
            Stmt::Loop { body, continuing } => {
                let r = records_gpu_work(body, at).or_else(|| records_gpu_work(continuing, at));
                if r.is_some() {
                    return r;
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether a value of type `t` holds a pointer.
fn has_ptr(types: &Types, t: TypeId) -> bool {
    match types.get(t) {
        TypeDef::Ptr(_) => true,
        TypeDef::Struct { fields, .. } => fields.iter().any(|(_, f)| has_ptr(types, *f)),
        TypeDef::Enum { variants, .. } => {
            variants.iter().any(|(_, p)| p.is_some_and(|p| has_ptr(types, p)))
        }
        TypeDef::Array(e, _) => has_ptr(types, *e),
        _ => false,
    }
}

/// The type of a place.
pub(crate) fn place_type(m: &Module, f: &Function, p: &Place) -> Result<TypeId> {
    m.place_ty(f, p).ok_or_else(|| Error::internal(format!("a place with no type: {p:?}")))
}
