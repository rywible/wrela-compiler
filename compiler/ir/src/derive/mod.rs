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
pub mod literal;

pub use literal::literal_gradient;

use crate::*;
use std::collections::HashMap;

/// Derived functions already built in a module, so each is built once.
#[derive(Debug, Default)]
pub struct DeriveCache {
    ad: HashMap<(FuncId, Vec<Bits>, u8), FuncId>,
    interval: HashMap<(FuncId, Vec<Bits>), FuncId>,
    /// What a call does with active arguments, by derivation and active parameters: `None`
    /// while the callee is still being analyzed.
    effects: HashMap<(Mode, FuncId, Vec<Bits>), Option<Effect>>,
    /// The activity [`effect`] found, kept for deriving the function when it's final.
    activity: HashMap<(Mode, FuncId, Vec<Bits>), Activity>,
    /// The intervals being built, outermost first, and whether the call that asked for each
    /// runs under a branch on the input.
    building: Vec<(FuncId, Vec<Bits>, bool)>,
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

/// Activity by part: a bit for each **leaf** of a value's type, in field order. A leaf is a
/// scalar, vector, matrix, enum or array, or a struct that holds an enum or a pointer, or one
/// with more than 64 leaves. A struct built from an active value and a constant is then active
/// only in the first: fbm's `octaves` in a displacement of an animated body stays a constant,
/// and a loop over it runs the same number of times for every point.
pub(crate) type Bits = u64;

fn span(off: u32, n: u32) -> Bits {
    (if n >= 64 { !0 } else { (1u64 << n) - 1 }) << off
}

/// Whether a struct is split into leaves: not when it holds an enum (whose range is a struct
/// of its own) or a pointer (followed by [`Pointers`] as a whole).
fn splits(types: &Types, t: TypeId) -> bool {
    fn holds(types: &Types, t: TypeId) -> bool {
        match types.get(t) {
            TypeDef::Enum { .. } | TypeDef::Ptr(_) => true,
            TypeDef::Struct { fields, .. } => fields.iter().any(|(_, f)| holds(types, *f)),
            TypeDef::Array(e, _) | TypeDef::RuntimeArray(e) | TypeDef::Run(e) => holds(types, *e),
            _ => false,
        }
    }
    matches!(types.get(t), TypeDef::Struct { .. }) && !holds(types, t)
}

/// A type's leaves, before the limit of 64.
fn flat(types: &Types, t: TypeId) -> u32 {
    match types.get(t) {
        TypeDef::Struct { fields, .. } if splits(types, t) => {
            fields.iter().fold(0u32, |n, (_, f)| n.saturating_add(flat(types, *f))).max(1)
        }
        _ => 1,
    }
}

/// How many leaves a value of type `t` has.
pub(crate) fn leaves(types: &Types, t: TypeId) -> u32 {
    let n = flat(types, t);
    if n <= 64 { n } else { 1 }
}

/// Every leaf of `t`.
pub(crate) fn all_bits(types: &Types, t: TypeId) -> Bits {
    span(0, leaves(types, t))
}

/// The leaves of `t` that hold a float: those that can carry a derivative.
fn float_bits(types: &Types, t: TypeId) -> Bits {
    if leaves(types, t) == 1 {
        return types.has_float(t) as Bits;
    }
    let TypeDef::Struct { fields, .. } = types.get(t) else { return 0 };
    let mut bits = 0;
    let mut off = 0;
    for (_, f) in fields {
        bits |= float_bits(types, *f) << off;
        off += flat(types, *f);
    }
    bits
}

/// Where field `i` of `t`'s leaves are within `t`'s: an offset and a count. All of `t` when it
/// is one leaf.
fn field_bits(types: &Types, t: TypeId, i: u32) -> (u32, u32) {
    if leaves(types, t) == 1 {
        return (0, 1);
    }
    let TypeDef::Struct { fields, .. } = types.get(t) else { return (0, 1) };
    let off = fields[..i as usize].iter().map(|(_, f)| flat(types, *f)).sum();
    (off, flat(types, fields[i as usize].1))
}

/// The leaves of `whole` (a value of type `t`) that its field `i` holds, as that field's own.
fn field_of(types: &Types, t: TypeId, whole: Bits, i: u32, field_ty: TypeId) -> Bits {
    let (off, n) = field_bits(types, t, i);
    let bits = (whole >> off) & span(0, n);
    if bits != 0 && n != leaves(types, field_ty) { all_bits(types, field_ty) } else { bits }
}

/// Which values, locals and parameters depend on the active input, by leaf.
#[derive(Clone, Debug)]
pub(crate) struct Activity {
    pub values: Vec<Bits>,
    pub locals: Vec<Bits>,
    pub params: Vec<Bits>,
    pub returns: Bits,
    /// Interval mode: a loop whose exit depends on the input (unsupported), and where its
    /// exit's condition is in the source, if known.
    pub active_loop_exit: Option<Option<wrela_diag::Span>>,
    /// The original function's local and parameter types, which the leaves are of.
    local_tys: Vec<TypeId>,
    param_tys: Vec<TypeId>,
}

impl Activity {
    pub(crate) fn value(&self, v: ValueId) -> bool {
        self.values[v.index()] != 0
    }

    pub(crate) fn local(&self, l: LocalId) -> bool {
        self.locals[l.index()] != 0
    }

    pub(crate) fn param(&self, i: u32) -> bool {
        self.params[i as usize] != 0
    }

    /// Whether a place's root, a local or a parameter, is active in any leaf.
    pub(crate) fn root(&self, r: &PlaceRoot) -> bool {
        match r {
            PlaceRoot::Local(l) => self.local(*l),
            PlaceRoot::Param(i) => self.param(*i),
            PlaceRoot::Ptr(v) => self.value(*v),
            PlaceRoot::Resource(_) | PlaceRoot::Data(_) => false,
        }
    }

    pub(crate) fn index_active(&self, p: &Place) -> bool {
        p.path.iter().any(|x| matches!(x, Proj::Index(v) if self.value(*v)))
    }

    /// A place's root type and the leaves its path covers within it: an offset and a count.
    /// The path stops splitting at an array index or a vector component.
    fn region(&self, types: &Types, p: &Place) -> Option<(TypeId, u32, u32)> {
        let t = match &p.root {
            PlaceRoot::Local(l) => self.local_tys[l.index()],
            PlaceRoot::Param(i) => self.param_tys[*i as usize],
            _ => return None,
        };
        let (mut at, mut off, mut n) = (t, 0, leaves(types, t));
        for proj in &p.path {
            match (proj, types.get(at)) {
                (Proj::Field(i), TypeDef::Struct { fields, .. }) if n > 1 => {
                    let (o, k) = field_bits(types, at, *i);
                    off += o;
                    n = k;
                    at = fields[*i as usize].1;
                }
                _ => break,
            }
        }
        Some((at, off, n))
    }

    /// The active leaves of what a place holds (of type `ty`), as a value of it has them.
    pub(crate) fn place_bits(&self, types: &Types, p: &Place, ty: TypeId) -> Bits {
        if self.index_active(p) {
            return all_bits(types, ty);
        }
        if let PlaceRoot::Ptr(v) = p.root {
            return if self.value(v) { all_bits(types, ty) } else { 0 };
        }
        let Some((_, off, n)) = self.region(types, p) else { return 0 };
        let root = match &p.root {
            PlaceRoot::Local(l) => self.locals[l.index()],
            PlaceRoot::Param(i) => self.params[*i as usize],
            _ => 0,
        };
        let bits = (root >> off) & span(0, n);
        if bits != 0 && n != leaves(types, ty) { all_bits(types, ty) } else { bits }
    }

    /// Whether what a place holds is active in any leaf (`place_type` gives its type).
    pub(crate) fn place(&self, m: &Module, f: &Function, p: &Place) -> bool {
        match m.place_ty(f, p) {
            Some(ty) => self.place_bits(&m.types, p, ty) != 0,
            None => self.index_active(p) || self.root(&p.root),
        }
    }

    /// Marks the leaves `bits` (of the place's type `ty`) of a local or parameter active; all
    /// of the place's leaves when its path isn't split that finely.
    fn mark_bits(&mut self, types: &Types, p: &Place, ty: TypeId, bits: Bits) {
        let Some((_, off, n)) = self.region(types, p) else { return };
        let add = if n == leaves(types, ty) && !self.index_active(p) {
            (bits & span(0, n)) << off
        } else {
            span(off, n)
        };
        match &p.root {
            PlaceRoot::Local(l) => self.locals[l.index()] |= add,
            PlaceRoot::Param(i) => self.params[*i as usize] |= add,
            _ => {}
        }
    }
}

/// What a call does, given which of its arguments are active.
#[derive(Clone, Debug)]
pub(crate) struct Effect {
    /// Which leaves of its result are active.
    pub returns: Bits,
    /// By parameter: whether it may write an active value through it (a `mut` one).
    pub writes: Vec<bool>,
}

/// How the activity analysis learns what a call does.
pub(crate) type Callee<'c> = dyn FnMut(FuncId, Vec<Bits>) -> Effect + 'c;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Mode {
    /// Only floats carry derivatives; control flow doesn't.
    Ad,
    /// Anything can carry a range, and a branch on a range makes what it assigns a range too.
    Interval,
}

/// The active leaves of a call's argument, as its parameter has them.
pub(crate) fn arg_bits(m: &Module, f: &Function, a: &Activity, arg: &Arg) -> Bits {
    match arg {
        Arg::Value(v) => a.values[v.index()],
        Arg::Place(p) => match m.place_ty(f, p) {
            Some(ty) => a.place_bits(&m.types, p, ty),
            None => (a.index_active(p) || a.root(&p.root)) as Bits,
        },
    }
}

/// The activity analysis: a fixpoint over the whole body (flow-insensitive per local, which is
/// sound and simple). `callee` answers what a call does given which of its arguments are
/// active.
pub(crate) fn activity(
    m: &Module,
    f: &Function,
    params: &[Bits],
    mode: Mode,
    callee: &mut Callee<'_>,
) -> Activity {
    let mut a = Activity {
        values: vec![0; f.values.len()],
        locals: vec![0; f.locals.len()],
        params: params.to_vec(),
        returns: 0,
        active_loop_exit: None,
        local_tys: f.locals.iter().map(|l| l.ty).collect(),
        param_tys: f.params.iter().map(|p| p.ty).collect(),
    };
    let mut uses = vec![0u32; f.locals.len()];
    visit::count_local_mentions(&f.body, &mut uses);
    let cx = Walk { m, f, mode, uses, ptrs: Pointers::of(m, f) };
    // Bits only turn on, so a round changed something when it turned more on.
    let on = |a: &Activity| {
        let ones = |xs: &[Bits]| xs.iter().map(|x| x.count_ones() as usize).sum::<usize>();
        ones(&a.values) + ones(&a.locals) + ones(&a.params) + a.returns.count_ones() as usize
    };
    loop {
        let before = on(&a);
        walk(&cx, &f.body, &mut a, None, callee);
        if on(&a) == before {
            return a;
        }
    }
}

/// The leaves of a value of type `t` that can carry what `mode` derives.
fn carried(m: &Module, t: TypeId, mode: Mode) -> Bits {
    match mode {
        Mode::Ad => float_bits(&m.types, t),
        Mode::Interval => all_bits(&m.types, t),
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
    /// Marks the leaves `bits` of a written place active, or all of a place's leaves (`None`);
    /// for a pointer root, every root it may point into (every local and parameter, if that
    /// isn't known).
    fn mark(&self, a: &mut Activity, p: &Place, bits: Option<Bits>) {
        let PlaceRoot::Ptr(v) = p.root else {
            return match self.m.place_ty(self.f, p) {
                Some(ty) => a.mark_bits(&self.m.types, p, ty, bits.unwrap_or(!0)),
                None => mark_root(a, &p.root),
            };
        };
        let mut roots = Vec::new();
        if self.ptrs.roots(v, &mut roots).is_none() {
            a.locals.iter_mut().chain(a.params.iter_mut()).for_each(|x| *x = !0);
        }
        for r in roots {
            mark_root(a, &r);
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
/// may run, and what they wrote is joined after the branch). `ctrl` then counts, by local, the
/// places inside the innermost such branch that mention it. A local that only those places
/// mention never reaches the join, so a store to it doesn't.
fn walk(cx: &Walk<'_>, b: &Block, a: &mut Activity, ctrl: Option<&[u32]>, callee: &mut Callee<'_>) {
    let (m, f, mode) = (cx.m, cx.f, cx.mode);
    let joined = |p: &Place| match (ctrl, &p.root) {
        (None, _) => false,
        (Some(inside), PlaceRoot::Local(l)) => inside[l.index()] != cx.uses[l.index()],
        (Some(_), _) => true,
    };
    let mut at = None;
    for s in b {
        match s {
            Stmt::Let(v, e) => {
                let ty = f.value_ty(*v);
                let bits = match e {
                    Expr::Call(g, args) => call(cx, a, *g, args, &joined, callee),
                    e => expr_bits(cx, e, a, ty),
                };
                a.values[v.index()] |= bits & carried(m, ty, mode);
            }
            Stmt::Eval(Expr::Call(g, args)) => {
                call(cx, a, *g, args, &joined, callee);
            }
            Stmt::Eval(_) => {}
            Stmt::Store(p, v) => {
                if joined(p) {
                    cx.mark(a, p, None);
                } else if a.value(*v) {
                    cx.mark(a, p, Some(a.values[v.index()]));
                }
            }
            Stmt::If { cond, then, else_ } => {
                if mode == Mode::Interval && a.value(*cond) {
                    let mut inside = vec![0u32; f.locals.len()];
                    visit::count_local_mentions(then, &mut inside);
                    visit::count_local_mentions(else_, &mut inside);
                    walk(cx, then, a, Some(&inside), callee);
                    walk(cx, else_, a, Some(&inside), callee);
                } else {
                    walk(cx, then, a, ctrl, callee);
                    walk(cx, else_, a, ctrl, callee);
                }
            }
            Stmt::Loop { body, continuing } => {
                walk(cx, body, a, ctrl, callee);
                walk(cx, continuing, a, ctrl, callee);
                if mode == Mode::Interval
                    && a.active_loop_exit.is_none()
                    && let Some(exit) = loop_exit(body, a, at)
                {
                    a.active_loop_exit = Some(exit);
                }
            }
            Stmt::Return(Some(v)) => {
                a.returns |= if ctrl.is_some() { !0 } else { a.values[v.index()] };
            }
            Stmt::At(span) => at = Some(*span),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::Trap => {}
        }
    }
}

/// Whether a block can `break` or `continue` a loop around it (or `return`). A loop inside it
/// takes its own `break` and `continue`.
fn leaves_loop(b: &[Stmt]) -> bool {
    b.iter().any(|s| match s {
        Stmt::Break | Stmt::Continue | Stmt::Return(_) => true,
        Stmt::If { then, else_, .. } => leaves_loop(then) || leaves_loop(else_),
        _ => false,
    })
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
    for s in b {
        match s {
            Stmt::At(span) => at = Some(*span),
            Stmt::If { cond, then, else_ } => {
                if a.value(*cond) && (leaves_loop(then) || leaves_loop(else_)) {
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

/// A call: the active leaves of its result. A callee given active arguments may write active
/// values through its `mut` parameters (those its own activity marks), and in interval mode,
/// so may one that runs under a condition on the input (`joined`).
fn call(
    cx: &Walk<'_>,
    a: &mut Activity,
    g: FuncId,
    args: &[Arg],
    joined: &dyn Fn(&Place) -> bool,
    callee: &mut Callee<'_>,
) -> Bits {
    let mask: Vec<Bits> = args.iter().map(|x| arg_bits(cx.m, cx.f, a, x)).collect();
    // Arguments map to the callee's parameters one to one.
    let effect = mask.iter().any(|b| *b != 0).then(|| callee(g, mask));
    for (i, (arg, p)) in args.iter().zip(&cx.m.functions[g.index()].params).enumerate() {
        let writes = effect.as_ref().is_some_and(|e| e.writes[i]);
        if let (Arg::Place(pl), true) = (arg, p.mutable)
            && (writes || joined(pl))
        {
            cx.mark(a, pl, None);
        }
    }
    effect.map_or(0, |e| e.returns)
}

/// Marks every leaf of a local or parameter active.
fn mark_root(a: &mut Activity, r: &PlaceRoot) {
    match r {
        PlaceRoot::Local(l) => a.locals[l.index()] = !0,
        PlaceRoot::Param(i) => a.params[*i as usize] = !0,
        _ => {}
    }
}

/// The active leaves of `e`'s value (of type `ty`). Most expressions are active as a whole when
/// any operand is; a struct's construction, a field's extraction, a load and a select keep the
/// leaves apart.
fn expr_bits(cx: &Walk<'_>, e: &Expr, a: &Activity, ty: TypeId) -> Bits {
    let types = &cx.m.types;
    let all = all_bits(types, ty);
    let any = |b: bool| if b { all } else { 0 };
    let v = |x: &ValueId| a.value(*x);
    match e {
        Expr::Const(_) | Expr::Zero(_) | Expr::EntryInput(_) => 0,
        Expr::Texture(_, _, _, xs) => any(xs.iter().any(v)),
        Expr::Atomic(..) | Expr::Barrier => all,
        Expr::Param(i) => a.params[*i as usize],
        Expr::Load(p) => a.place_bits(types, p, ty),
        Expr::ArrayLength(p) => any(a.place(cx.m, cx.f, p)),
        // A pointer or a view: active when any of its root is (writes through it are followed
        // as the root's).
        Expr::Run(p) | Expr::Addr(p) => any(a.index_active(p) || a.root(&p.root)),
        Expr::Extract(x, i) => {
            let xt = cx.f.value_ty(*x);
            match types.get(xt) {
                TypeDef::Struct { .. } => field_of(types, xt, a.values[x.index()], *i, ty),
                _ => any(v(x)),
            }
        }
        Expr::Construct(t, xs) if leaves(types, *t) > 1 => {
            let mut bits = 0;
            for (i, x) in xs.iter().enumerate() {
                let (off, n) = field_bits(types, *t, i as u32);
                bits |= (a.values[x.index()] & span(0, n)) << off;
            }
            bits
        }
        Expr::Select { cond, if_true, if_false } => {
            if v(cond) {
                all
            } else {
                a.values[if_true.index()] | a.values[if_false.index()]
            }
        }
        Expr::Unary(_, x)
        | Expr::Splat(x, _)
        | Expr::Swizzle(x, _)
        | Expr::Convert(x, _)
        | Expr::Bitcast(x, _)
        | Expr::Variant(_, _, Some(x)) => any(v(x)),
        Expr::Variant(_, _, None) => 0,
        Expr::Binary(_, x, y) | Expr::ExtractDyn(x, y) => any(v(x) || v(y)),
        Expr::Builtin(_, xs) | Expr::Construct(_, xs) | Expr::Host(_, xs) | Expr::Mem(_, xs) => {
            any(xs.iter().any(v))
        }
        Expr::Call(..) => unreachable!("calls are handled in `call`"),
    }
}

/// The active leaves of `f`'s result with these active parameters: a derivative (for
/// [`Mode::Ad`], of a float result) or a range. Zero when none.
pub(super) fn returns_active(
    m: &Module,
    cache: &mut DeriveCache,
    f: FuncId,
    mask: Vec<Bits>,
    mode: Mode,
) -> Bits {
    effect(m, cache, f, mask, mode).returns
}

/// What a call of `f` with these active parameters does. Memoized; a recursive call still being
/// analyzed is assumed to return an active value and to write through every `mut` parameter.
pub(super) fn effect(
    m: &Module,
    cache: &mut DeriveCache,
    f: FuncId,
    mask: Vec<Bits>,
    mode: Mode,
) -> Effect {
    let func = &m.functions[f.index()];
    let key = (mode, f, mask);
    let ret_bits = func.ret.map_or(0, |t| carried(m, t, mode));
    match cache.effects.get(&key) {
        Some(Some(e)) => return e.clone(),
        Some(None) => {
            let writes = func.params.iter().map(|p| p.mutable).collect();
            return Effect { returns: ret_bits, writes };
        }
        None => {}
    }
    cache.effects.insert(key.clone(), None);
    let mut assumed = false;
    let a = activity(m, func, &key.2, mode, &mut |g, mk| {
        assumed |= matches!(cache.effects.get(&(mode, g, mk.clone())), Some(None));
        effect(m, cache, g, mk, mode)
    });
    let returns = a.returns & ret_bits;
    let writes = func.params.iter().zip(&a.params).map(|(p, w)| p.mutable && *w != 0).collect();
    let e = Effect { returns, writes };
    cache.effects.insert(key.clone(), Some(e.clone()));
    // An activity that took no answer from a function still being analyzed is final: deriving
    // `f` would find it again.
    if !assumed {
        cache.activity.insert(key, a);
    }
    e
}

/// The activity of `f`'s body with active parameters `mask`: the one [`effect`] kept, if it
/// did.
pub(super) fn body_activity(
    m: &Module,
    cache: &mut DeriveCache,
    f: FuncId,
    mask: &[Bits],
    mode: Mode,
) -> Activity {
    if let Some(a) = cache.activity.get(&(mode, f, mask.to_vec())) {
        return a.clone();
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

/// `v = e`, or `e` evaluated for its effect when there's no `v`.
pub(super) fn let_or_eval(v: Option<ValueId>, e: Expr) -> Stmt {
    match v {
        Some(v) => Stmt::Let(v, e),
        None => Stmt::Eval(e),
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

/// Whether a raw memory operation can run in a derived function as it is: it moves no float
/// (which memory would carry without its derivative) and changes nothing. A text literal's
/// address, a panic, the heap's bounds, and reading an integer (comparing text) can.
fn harmless(op: &MemOp) -> bool {
    match op {
        MemOp::Addr | MemOp::Panic | MemOp::HeapBase | MemOp::Pages => true,
        MemOp::Load(s) => !matches!(s, Scalar::F32 | Scalar::F64),
        _ => false,
    }
}

/// An error if a function's body records GPU work (a host operation) or moves memory other than
/// harmlessly, which a derived function can't do.
pub(super) fn no_gpu_work(body: &Block) -> Result<()> {
    // Where: the last `At` before it (`at` before the block).
    fn find(b: &Block, mut at: Option<wrela_diag::Span>) -> Option<Option<wrela_diag::Span>> {
        for s in b {
            match s {
                Stmt::At(span) => at = Some(*span),
                Stmt::Let(_, Expr::Mem(op, _)) | Stmt::Eval(Expr::Mem(op, _)) if harmless(op) => {}
                Stmt::Let(_, Expr::Host(..) | Expr::Mem(..))
                | Stmt::Eval(Expr::Host(..) | Expr::Mem(..)) => return Some(at),
                _ => {
                    if let Some(r) = s.blocks().find_map(|inner| find(inner, at)) {
                        return Some(r);
                    }
                }
            }
        }
        None
    }
    match find(body, None) {
        Some(at) => {
            Err(Error::not_derivable("this records GPU work, which a derived function can't do")
                .at(at))
        }
        None => Ok(()),
    }
}

/// An error if deriving `func` would write `what` (a derivative, a range) through a `mut`
/// parameter that isn't active itself. The caller's statement (the call) places it, or the
/// call of the derivation does.
pub(super) fn no_write_through_mut(
    func: &Function,
    act: &Activity,
    mask: &[Bits],
    what: &str,
) -> Result<()> {
    for ((p, written), active) in func.params.iter().zip(&act.params).zip(mask) {
        if *written != 0 && *active == 0 && p.mutable {
            return Err(Error::not_derivable(format!(
                "this would write {what} through the `mut` parameter `{}`",
                p.name
            )));
        }
    }
    Ok(())
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
