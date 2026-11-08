//! Constant folding (GPU, language.md §18.13): a value computed only from constants is computed
//! once, as the shader is built, not by every invocation each time it's run.
//!
//! Flattening leaves values that a function computed from its constant arguments, and the
//! elements of constant arrays at indices unrolling made constant (`normalize(CUTS[i].xyz)`).
//! Each invocation computed them again, to the same result. Folded here, they become constants
//! in the WGSL.
//!
//! What's folded: `bool`s, `i32`s, `u32`s and `f32`s, and vectors of them, through arithmetic,
//! comparisons, conversions, selects and the math builtins; parts of known vectors, arrays and
//! structs; and what a local (or a private variable) holds, where only its own stores write it.
//! A float is computed as WGSL computes it, in `f32`, a transcendental through `f64` (within
//! WGSL's error bounds for it). Where WGSL leaves a result undefined (`sqrt` of a negative, a
//! `smoothstep` whose edges meet, an integer divided by zero) or it isn't finite, the value is
//! left to the GPU.

use crate::*;
use std::collections::HashMap;

/// A value known as the shader is built: a scalar, or a vector's, array's or struct's parts.
#[derive(Clone, Debug, PartialEq)]
enum K {
    S(Const),
    P(Vec<K>),
}

/// Folds what `f` computes from constants (see the module docs). Whether it folded anything.
pub(crate) fn fold_constants(m: &mut Module, f: &mut Function) -> bool {
    let taken = crate::opt::address_taken(f);
    let private = m
        .resources
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r.kind, ResourceKind::Private))
        .map(|(i, _)| ResourceId(i as u32))
        .collect();
    let body = std::mem::take(&mut f.body);
    let mut fx = Folder { types: &mut m.types, private, known: HashMap::new(), changed: false };
    let mut st = Places::default();
    f.body = fx.block(f, body, &mut st, &taken);
    fx.changed
}

/// What locals and private variables hold where it's known.
#[derive(Clone, Default, PartialEq)]
struct Places {
    locals: HashMap<LocalId, K>,
    private: HashMap<ResourceId, K>,
}

impl Places {
    fn join(&mut self, other: &Places) {
        self.locals.retain(|l, k| other.locals.get(l) == Some(k));
        self.private.retain(|r, k| other.private.get(r) == Some(k));
    }
}

struct Folder<'t> {
    types: &'t mut Types,
    /// The private variables (one per invocation), which only the invocation's own stores write.
    private: std::collections::HashSet<ResourceId>,
    known: HashMap<ValueId, K>,
    changed: bool,
}

impl Folder<'_> {
    fn block(&mut self, f: &mut Function, b: Block, st: &mut Places, taken: &[bool]) -> Block {
        let mut out = Vec::with_capacity(b.len());
        for s in b {
            match s {
                Stmt::Let(v, e) => {
                    let k = self.expr(f, &e, st);
                    if matches!(e, Expr::Call(..)) {
                        // A call might write a private variable.
                        st.private.clear();
                    }
                    match k {
                        Some(k) => {
                            let e = self.rewrite(f, v, e, &k, &mut out);
                            self.known.insert(v, k);
                            out.push(Stmt::Let(v, e));
                        }
                        None => out.push(Stmt::Let(v, e)),
                    }
                }
                Stmt::Eval(e) => {
                    if matches!(e, Expr::Call(..)) {
                        st.private.clear();
                    }
                    out.push(Stmt::Eval(e));
                }
                Stmt::Store(p, v) => {
                    self.store(&p, v, st, taken);
                    out.push(Stmt::Store(p, v));
                }
                Stmt::If { cond, then, else_ } => {
                    let mut on_else = st.clone();
                    let then = self.block(f, then, st, taken);
                    let else_ = self.block(f, else_, &mut on_else, taken);
                    st.join(&on_else);
                    out.push(Stmt::If { cond, then, else_ });
                }
                Stmt::Loop { body, continuing } => {
                    // A loop of only what's known runs now: what it leaves in its locals.
                    if let Some(left) = self.run_loop(f, &body, &continuing, st, taken) {
                        self.changed = true;
                        for (l, k) in left {
                            let t = f.locals[l.index()].ty;
                            let Some(v) = self.emit(f, t, &k, &mut out) else { unreachable!() };
                            out.push(Stmt::Store(Place::local(l), v));
                            st.locals.insert(l, k);
                        }
                        continue;
                    }
                    // What the loop stores holds what the pass before left: unknown in it and
                    // after it.
                    let (mut locals, mut private, mut calls) = (Vec::new(), Vec::new(), false);
                    for b in [&body, &continuing] {
                        visit::walk(b, &mut |s| {
                            if let Stmt::Store(p, _) = s {
                                match p.root {
                                    PlaceRoot::Local(l) => locals.push(l),
                                    PlaceRoot::Resource(r) => private.push(r),
                                    _ => {}
                                }
                            }
                            calls |= s.expr().is_some_and(|e| matches!(e, Expr::Call(..)));
                        });
                    }
                    let forget = |st: &mut Places| {
                        locals.iter().for_each(|l| {
                            st.locals.remove(l);
                        });
                        private.iter().for_each(|r| {
                            st.private.remove(r);
                        });
                        if calls {
                            st.private.clear();
                        }
                    };
                    forget(st);
                    let body = self.block(f, body, st, taken);
                    forget(st);
                    let continuing = self.block(f, continuing, st, taken);
                    forget(st);
                    out.push(Stmt::Loop { body, continuing });
                }
                s => out.push(s),
            }
        }
        out
    }

    /// The most passes a loop is run for as the shader is built.
    const MOST_PASSES: u32 = 4096;

    /// Runs a loop whose every value is known, from what `st` knows: the locals it stores, with
    /// what each holds after it, in a kind `emit` writes. `None` (the loop left as it is) where
    /// a value isn't known, it has an effect beyond its locals' stores, or it runs past
    /// [`Self::MOST_PASSES`].
    fn run_loop(
        &mut self,
        f: &Function,
        body: &[Stmt],
        continuing: &[Stmt],
        st: &Places,
        taken: &[bool],
    ) -> Option<Vec<(LocalId, K)>> {
        let mut pl = Places { locals: st.locals.clone(), private: HashMap::new() };
        let mut stored: Vec<LocalId> = Vec::new();
        // The loop's own values, known while it runs, forgotten after.
        let mut defined: Vec<ValueId> = Vec::new();
        let mut steps = 0u32;
        let mut left = false;
        let mut run = |me: &mut Self| -> Option<()> {
            for _ in 0..Self::MOST_PASSES {
                let mut cx = RunCx {
                    pl: &mut pl,
                    stored: &mut stored,
                    defined: &mut defined,
                    steps: &mut steps,
                    taken,
                };
                match me.run(f, body, &mut cx)? {
                    Run::Break => {
                        left = true;
                        return Some(());
                    }
                    Run::Normal | Run::Continue => {}
                }
                if me.run(f, continuing, &mut cx)? != Run::Normal {
                    return None;
                }
            }
            None
        };
        let ran = run(self);
        for v in &defined {
            self.known.remove(v);
        }
        ran?;
        if !left {
            return None;
        }
        stored.sort_by_key(|l| l.0);
        stored.dedup();
        stored
            .into_iter()
            .map(|l| {
                let k = pl.locals.get(&l)?.clone();
                writable(self.types, f.locals[l.index()].ty, &k).then_some((l, k))
            })
            .collect()
    }

    /// Runs `b` for `run_loop`: how it ended.
    fn run(&mut self, f: &Function, b: &[Stmt], cx: &mut RunCx) -> Option<Run> {
        for s in b {
            *cx.steps += 1;
            if *cx.steps > 1 << 20 {
                return None;
            }
            match s {
                Stmt::At(_) => {}
                Stmt::Let(v, e) => {
                    if matches!(e, Expr::Call(..)) {
                        return None;
                    }
                    let k = self.expr(f, e, cx.pl)?;
                    self.known.insert(*v, k);
                    cx.defined.push(*v);
                }
                Stmt::Store(p, v) => {
                    let l = p.root_local()?;
                    if cx.taken[l.index()] {
                        return None;
                    }
                    let value = self.known.get(v)?.clone();
                    let path = self.path(p)?;
                    if !path.is_empty() && !cx.pl.locals.contains_key(&l) {
                        return None;
                    }
                    put(&mut cx.pl.locals, l, Some(value), Some(path));
                    if !cx.pl.locals.contains_key(&l) {
                        return None;
                    }
                    cx.stored.push(l);
                }
                Stmt::If { cond, then, else_ } => {
                    let c = match self.known.get(cond)? {
                        K::S(Const::Bool(c)) => *c,
                        _ => return None,
                    };
                    let r = self.run(f, if c { then } else { else_ }, cx)?;
                    if r != Run::Normal {
                        return Some(r);
                    }
                }
                Stmt::Break => return Some(Run::Break),
                Stmt::Continue => return Some(Run::Continue),
                // A nested loop, a return, an effect: not run here.
                _ => return None,
            }
        }
        Some(Run::Normal)
    }

    /// Writes `k`, of type `t`, as constants: the value that holds it.
    fn emit(&mut self, f: &mut Function, t: TypeId, k: &K, out: &mut Block) -> Option<ValueId> {
        match (self.types.get(t).clone(), k) {
            (TypeDef::Scalar(_), K::S(c)) => {
                let v = f.let_(out, t, Expr::Const(c.clone()));
                self.known.insert(v, k.clone());
                Some(v)
            }
            (TypeDef::Vector(s, _), K::P(parts)) => {
                let st = self.types.intern(TypeDef::Scalar(s));
                let mut comps = Vec::new();
                for p in parts {
                    comps.push(self.emit(f, st, p, out)?);
                }
                let v = f.let_(out, t, Expr::Construct(t, comps));
                self.known.insert(v, k.clone());
                Some(v)
            }
            _ => None,
        }
    }

    /// A store's effect on what's known of its place.
    fn store(&mut self, p: &Place, v: ValueId, st: &mut Places, taken: &[bool]) {
        let value = self.known.get(&v).cloned();
        let path = self.path(p);
        match p.root {
            PlaceRoot::Local(l) if !taken[l.index()] => put(&mut st.locals, l, value, path),
            PlaceRoot::Local(l) => {
                st.locals.remove(&l);
            }
            PlaceRoot::Resource(r) if self.private.contains(&r) => {
                put(&mut st.private, r, value, path)
            }
            _ => {}
        }
    }

    /// The parts a place's path picks, where its indices are known.
    fn path(&self, p: &Place) -> Option<Vec<usize>> {
        p.path
            .iter()
            .map(|x| match x {
                Proj::Field(i) => Some(*i as usize),
                Proj::Comp(i) => Some(*i as usize),
                Proj::Index(v) => match self.known.get(v) {
                    Some(K::S(Const::U32(i))) => Some(*i as usize),
                    Some(K::S(Const::I32(i))) if *i >= 0 => Some(*i as usize),
                    _ => None,
                },
            })
            .collect()
    }

    fn read(&self, p: &Place, st: &Places) -> Option<K> {
        let mut at = match p.root {
            PlaceRoot::Local(l) => st.locals.get(&l)?,
            PlaceRoot::Resource(r) if self.private.contains(&r) => st.private.get(&r)?,
            _ => return None,
        };
        for i in self.path(p)? {
            match at {
                K::P(parts) => at = parts.get(i)?,
                K::S(_) => return None,
            }
        }
        Some(at.clone())
    }

    /// The expression `v` is given in place of `e`, now that it's known to be `k`: a constant,
    /// or a vector of constants. Other values (arrays, structs) keep their expression.
    fn rewrite(&mut self, f: &mut Function, v: ValueId, e: Expr, k: &K, out: &mut Block) -> Expr {
        match (k, &e) {
            (_, Expr::Const(_)) => e,
            (K::S(c), _) => {
                self.changed = true;
                Expr::Const(c.clone())
            }
            (K::P(parts), _) => {
                let t = f.value_ty(v);
                let TypeDef::Vector(s, _) = *self.types.get(t) else { return e };
                // Already a vector of constants: as it is.
                if let Expr::Construct(_, xs) = &e
                    && xs.iter().all(|x| matches!(self.known.get(x), Some(K::S(_))))
                {
                    return e;
                }
                let st = self.types.intern(TypeDef::Scalar(s));
                let mut comps = Vec::with_capacity(parts.len());
                for p in parts {
                    let K::S(c) = p else { return e };
                    let x = f.let_(out, st, Expr::Const(c.clone()));
                    self.known.insert(x, p.clone());
                    comps.push(x);
                }
                self.changed = true;
                Expr::Construct(t, comps)
            }
        }
    }

    /// `e`'s value, if it's known.
    fn expr(&self, f: &Function, e: &Expr, st: &Places) -> Option<K> {
        let k = |v: &ValueId| self.known.get(v).cloned();
        match e {
            Expr::Const(c) => Some(K::S(c.clone())),
            Expr::Zero(t) => zero(self.types, *t),
            Expr::Load(p) => self.read(p, st),
            Expr::Construct(t, xs) => {
                let parts = xs.iter().map(k).collect::<Option<Vec<_>>>()?;
                // A matrix's columns, an enum: not folded.
                match self.types.get(*t) {
                    TypeDef::Vector(..) | TypeDef::Array(..) | TypeDef::Struct { .. } => {
                        Some(K::P(parts))
                    }
                    _ => None,
                }
            }
            Expr::Extract(x, i) => match k(x)? {
                K::P(parts) => parts.get(*i as usize).cloned(),
                K::S(_) => None,
            },
            Expr::ExtractDyn(x, i) => {
                let K::P(parts) = k(x)? else { return None };
                let i = match k(i)? {
                    K::S(Const::U32(i)) => i as usize,
                    K::S(Const::I32(i)) if i >= 0 => i as usize,
                    _ => return None,
                };
                parts.get(i).cloned()
            }
            Expr::Splat(x, n) => {
                let K::S(c) = k(x)? else { return None };
                Some(K::P(vec![K::S(c); *n as usize]))
            }
            Expr::Swizzle(x, comps) => {
                let K::P(parts) = k(x)? else { return None };
                let picked =
                    comps.iter().map(|&c| parts.get(c as usize).cloned()).collect::<Option<_>>()?;
                Some(K::P(picked))
            }
            Expr::Unary(op, x) => each1(k(x)?, &|c| unary(*op, c)),
            Expr::Binary(op, a, b) => {
                // A matrix's product isn't folded.
                let ta = self.types.get(f.value_ty(*a));
                let tb = self.types.get(f.value_ty(*b));
                if !matches!(ta, TypeDef::Scalar(_) | TypeDef::Vector(..))
                    || !matches!(tb, TypeDef::Scalar(_) | TypeDef::Vector(..))
                {
                    return None;
                }
                each2(k(a)?, k(b)?, &|x, y| binary(*op, x, y))
            }
            Expr::Convert(x, s) => each1(k(x)?, &|c| convert(c, *s)),
            Expr::Select { cond, if_true, if_false } => {
                let (c, t, e) = (k(cond)?, k(if_true)?, k(if_false)?);
                select(c, t, e)
            }
            Expr::Builtin(b, args) => {
                let args = args.iter().map(k).collect::<Option<Vec<_>>>()?;
                builtin(*b, &args)
            }
            _ => None,
        }
    }
}

/// What `run_loop` runs a loop with: what's known of the locals, the locals stored, the values
/// defined, the statements run, and the locals whose addresses are taken.
struct RunCx<'a> {
    pl: &'a mut Places,
    stored: &'a mut Vec<LocalId>,
    defined: &'a mut Vec<ValueId>,
    steps: &'a mut u32,
    taken: &'a [bool],
}

/// How a block `run_loop` runs ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Run {
    Normal,
    Break,
    Continue,
}

/// Whether `emit` writes a `k` of type `t`: a scalar or a vector of known constants.
fn writable(types: &Types, t: TypeId, k: &K) -> bool {
    match (types.get(t), k) {
        (TypeDef::Scalar(_), K::S(_)) => true,
        (TypeDef::Vector(_, n), K::P(parts)) => {
            parts.len() == *n as usize && parts.iter().all(|p| matches!(p, K::S(_)))
        }
        _ => false,
    }
}

/// What's known of `key`'s place once `value` is stored at `path` in it (`None`: not known).
fn put<Key: std::hash::Hash + Eq>(
    map: &mut HashMap<Key, K>,
    key: Key,
    value: Option<K>,
    path: Option<Vec<usize>>,
) {
    let (Some(value), Some(path)) = (value, path) else {
        map.remove(&key);
        return;
    };
    if path.is_empty() {
        map.insert(key, value);
        return;
    }
    let Some(mut at) = map.get_mut(&key) else { return };
    for &i in &path {
        match at {
            K::P(parts) if i < parts.len() => at = &mut parts[i],
            _ => {
                map.remove(&key);
                return;
            }
        }
    }
    *at = value;
}

/// A zero of type `t`, where it's a scalar or a vector of one folded here.
fn zero(types: &Types, t: TypeId) -> Option<K> {
    let z = |s: Scalar| match s {
        Scalar::Bool => Some(Const::Bool(false)),
        Scalar::I32 => Some(Const::I32(0)),
        Scalar::U32 => Some(Const::U32(0)),
        Scalar::F32 => Some(Const::F32(0.0)),
        _ => None,
    };
    match types.get(t) {
        TypeDef::Scalar(s) => z(*s).map(K::S),
        TypeDef::Vector(s, n) => Some(K::P(vec![K::S(z(*s)?); *n as usize])),
        _ => None,
    }
}

fn each1(x: K, op: &dyn Fn(&Const) -> Option<Const>) -> Option<K> {
    match x {
        K::S(c) => op(&c).map(K::S),
        K::P(xs) => xs.into_iter().map(|x| each1(x, op)).collect::<Option<Vec<_>>>().map(K::P),
    }
}

fn each2(a: K, b: K, op: &dyn Fn(&Const, &Const) -> Option<Const>) -> Option<K> {
    match (a, b) {
        (K::S(x), K::S(y)) => op(&x, &y).map(K::S),
        (K::P(xs), K::P(ys)) if xs.len() == ys.len() => xs
            .into_iter()
            .zip(ys)
            .map(|(x, y)| each2(x, y, op))
            .collect::<Option<Vec<_>>>()
            .map(K::P),
        (K::S(x), K::P(ys)) => {
            ys.into_iter().map(|y| each2(K::S(x.clone()), y, op)).collect::<Option<_>>().map(K::P)
        }
        (K::P(xs), K::S(y)) => {
            xs.into_iter().map(|x| each2(x, K::S(y.clone()), op)).collect::<Option<_>>().map(K::P)
        }
        _ => None,
    }
}

fn each3(a: K, b: K, c: K, op: &dyn Fn(&Const, &Const, &Const) -> Option<Const>) -> Option<K> {
    match (a, b, c) {
        (K::S(x), K::S(y), K::S(z)) => op(&x, &y, &z).map(K::S),
        (a, b, c) => {
            let n = [&a, &b, &c].iter().find_map(|k| match k {
                K::P(p) => Some(p.len()),
                K::S(_) => None,
            })?;
            let part = |k: &K, i: usize| match k {
                K::P(p) => p.get(i).cloned(),
                K::S(_) => Some(k.clone()),
            };
            (0..n)
                .map(|i| each3(part(&a, i)?, part(&b, i)?, part(&c, i)?, op))
                .collect::<Option<Vec<_>>>()
                .map(K::P)
        }
    }
}

/// A float result, where it's finite.
fn float(x: f32) -> Option<Const> {
    x.is_finite().then_some(Const::F32(x))
}

fn unary(op: UnOp, c: &Const) -> Option<Const> {
    Some(match (op, c) {
        (UnOp::Neg, Const::F32(x)) => Const::F32(-x),
        (UnOp::Neg, Const::I32(x)) => Const::I32(x.wrapping_neg()),
        (UnOp::Not, Const::Bool(b)) => Const::Bool(!b),
        (UnOp::Not, Const::I32(x)) => Const::I32(!x),
        (UnOp::Not, Const::U32(x)) => Const::U32(!x),
        _ => return None,
    })
}

fn binary(op: BinOp, a: &Const, b: &Const) -> Option<Const> {
    use BinOp as B;
    Some(match (a, b) {
        (Const::F32(x), Const::F32(y)) => {
            let (x, y) = (*x, *y);
            match op {
                B::Add => float(x + y)?,
                B::Sub => float(x - y)?,
                B::Mul => float(x * y)?,
                B::Div => float(x / y)?,
                // WGSL's (and the GPU's `frem`): `x - y * trunc(x / y)`, in f32, not an exact
                // `fmod`; they part where `x / y` rounds to a whole number.
                B::Rem => float(x - y * (x / y).trunc())?,
                B::Eq => Const::Bool(x == y),
                B::Ne => Const::Bool(x != y),
                B::Lt => Const::Bool(x < y),
                B::Le => Const::Bool(x <= y),
                B::Gt => Const::Bool(x > y),
                B::Ge => Const::Bool(x >= y),
                _ => return None,
            }
        }
        (Const::I32(x), Const::I32(y)) => {
            let (x, y) = (*x, *y);
            match op {
                B::Add | B::WrappingAdd => Const::I32(x.wrapping_add(y)),
                B::Sub | B::WrappingSub => Const::I32(x.wrapping_sub(y)),
                B::Mul | B::WrappingMul => Const::I32(x.wrapping_mul(y)),
                B::Div if y != 0 => Const::I32(x.wrapping_div(y)),
                B::Rem if y != 0 => Const::I32(x.wrapping_rem(y)),
                B::BitAnd => Const::I32(x & y),
                B::BitOr => Const::I32(x | y),
                B::BitXor => Const::I32(x ^ y),
                B::Eq => Const::Bool(x == y),
                B::Ne => Const::Bool(x != y),
                B::Lt => Const::Bool(x < y),
                B::Le => Const::Bool(x <= y),
                B::Gt => Const::Bool(x > y),
                B::Ge => Const::Bool(x >= y),
                _ => return None,
            }
        }
        (Const::U32(x), Const::U32(y)) => {
            let (x, y) = (*x, *y);
            match op {
                B::Add | B::WrappingAdd => Const::U32(x.wrapping_add(y)),
                B::Sub | B::WrappingSub => Const::U32(x.wrapping_sub(y)),
                B::Mul | B::WrappingMul => Const::U32(x.wrapping_mul(y)),
                B::Div if y != 0 => Const::U32(x / y),
                B::Rem if y != 0 => Const::U32(x % y),
                B::BitAnd => Const::U32(x & y),
                B::BitOr => Const::U32(x | y),
                B::BitXor => Const::U32(x ^ y),
                B::Shl if y < 32 => Const::U32(x << y),
                B::Shr if y < 32 => Const::U32(x >> y),
                B::Eq => Const::Bool(x == y),
                B::Ne => Const::Bool(x != y),
                B::Lt => Const::Bool(x < y),
                B::Le => Const::Bool(x <= y),
                B::Gt => Const::Bool(x > y),
                B::Ge => Const::Bool(x >= y),
                _ => return None,
            }
        }
        (Const::I32(x), Const::U32(y)) => match op {
            B::Shl if *y < 32 => Const::I32(x.wrapping_shl(*y)),
            B::Shr if *y < 32 => Const::I32(x >> y),
            _ => return None,
        },
        (Const::Bool(x), Const::Bool(y)) => match op {
            B::And => Const::Bool(*x && *y),
            B::Or => Const::Bool(*x || *y),
            B::Eq => Const::Bool(x == y),
            B::Ne => Const::Bool(x != y),
            _ => return None,
        },
        _ => return None,
    })
}

fn convert(c: &Const, to: Scalar) -> Option<Const> {
    Some(match (c, to) {
        (Const::F32(x), Scalar::F32) => Const::F32(*x),
        (Const::I32(x), Scalar::F32) => Const::F32(*x as f32),
        (Const::U32(x), Scalar::F32) => Const::F32(*x as f32),
        // Only a float in the integer's range: past it, backends clamp to the largest float
        // the integer holds, not the integer's own bound (Rust's `as`); NaN isn't folded.
        (Const::F32(x), Scalar::I32) if (-2147483648.0..2147483648.0).contains(x) => {
            Const::I32(*x as i32)
        }
        (Const::F32(x), Scalar::U32) if *x > -1.0 && *x < 4294967296.0 => Const::U32(*x as u32),
        (Const::I32(x), Scalar::U32) => Const::U32(*x as u32),
        (Const::U32(x), Scalar::I32) => Const::I32(*x as i32),
        (Const::I32(x), Scalar::I32) => Const::I32(*x),
        (Const::U32(x), Scalar::U32) => Const::U32(*x),
        (Const::Bool(b), Scalar::F32) => Const::F32(f32::from(u8::from(*b))),
        (Const::Bool(b), Scalar::I32) => Const::I32(i32::from(*b)),
        (Const::Bool(b), Scalar::U32) => Const::U32(u32::from(*b)),
        (Const::Bool(b), Scalar::Bool) => Const::Bool(*b),
        _ => return None,
    })
}

fn select(c: K, t: K, e: K) -> Option<K> {
    match c {
        K::S(Const::Bool(b)) => Some(if b { t } else { e }),
        // A vector condition picks each component.
        K::P(cs) => {
            let (K::P(ts), K::P(es)) = (t, e) else { return None };
            cs.into_iter()
                .zip(ts.into_iter().zip(es))
                .map(|(c, (t, e))| select(c, t, e))
                .collect::<Option<Vec<_>>>()
                .map(K::P)
        }
        K::S(_) => None,
    }
}

fn f(c: &Const) -> Option<f32> {
    match c {
        Const::F32(x) => Some(*x),
        _ => None,
    }
}

/// A float's components, a vector's or a scalar's.
fn floats(k: &K) -> Option<Vec<f32>> {
    match k {
        K::S(c) => Some(vec![f(c)?]),
        K::P(ps) => ps
            .iter()
            .map(|p| match p {
                K::S(c) => f(c),
                K::P(_) => None,
            })
            .collect(),
    }
}

fn builtin(b: Builtin, args: &[K]) -> Option<K> {
    use Builtin as B;
    // Through `f64`, rounded once: within WGSL's error bound for each.
    let wide = |g: fn(f64) -> f64| move |c: &Const| float(g(f64::from(f(c)?)) as f32);
    match b {
        B::Sqrt => each1(args[0].clone(), &|c| {
            let x = f(c)?;
            if x < 0.0 { None } else { float(x.sqrt()) }
        }),
        B::InverseSqrt => each1(args[0].clone(), &|c| {
            let x = f(c)?;
            if x <= 0.0 { None } else { float((1.0 / f64::from(x).sqrt()) as f32) }
        }),
        B::Sin => each1(args[0].clone(), &wide(f64::sin)),
        B::Cos => each1(args[0].clone(), &wide(f64::cos)),
        B::Tan => each1(args[0].clone(), &wide(f64::tan)),
        B::Asin | B::Acos => each1(args[0].clone(), &|c| {
            let x = f64::from(f(c)?);
            if !(-1.0..=1.0).contains(&x) {
                return None;
            }
            float(if b == B::Asin { x.asin() } else { x.acos() } as f32)
        }),
        B::Atan => each1(args[0].clone(), &wide(f64::atan)),
        B::Atan2 => each2(args[0].clone(), args[1].clone(), &|y, x| {
            float(f64::from(f(y)?).atan2(f64::from(f(x)?)) as f32)
        }),
        B::Exp => each1(args[0].clone(), &wide(f64::exp)),
        B::Exp2 => each1(args[0].clone(), &wide(f64::exp2)),
        B::Log | B::Log2 => each1(args[0].clone(), &|c| {
            let x = f64::from(f(c)?);
            if x <= 0.0 {
                return None;
            }
            float(if b == B::Log { x.ln() } else { x.log2() } as f32)
        }),
        B::Sinh => each1(args[0].clone(), &wide(f64::sinh)),
        B::Cosh => each1(args[0].clone(), &wide(f64::cosh)),
        B::Tanh => each1(args[0].clone(), &wide(f64::tanh)),
        B::Pow => each2(args[0].clone(), args[1].clone(), &|x, y| {
            let (x, y) = (f64::from(f(x)?), f64::from(f(y)?));
            // WGSL's `pow` is `exp2(y * log2(x))`: undefined for x < 0, and x = 0 with y <= 0.
            if x < 0.0 || (x == 0.0 && y <= 0.0) {
                return None;
            }
            float(x.powf(y) as f32)
        }),
        B::Floor => each1(args[0].clone(), &|c| float(f(c)?.floor())),
        B::Ceil => each1(args[0].clone(), &|c| float(f(c)?.ceil())),
        B::Round => each1(args[0].clone(), &|c| float(f(c)?.round_ties_even())),
        B::Trunc => each1(args[0].clone(), &|c| float(f(c)?.trunc())),
        B::Fract => each1(args[0].clone(), &|c| {
            let x = f(c)?;
            float(x - x.floor())
        }),
        B::Abs => each1(args[0].clone(), &|c| match c {
            Const::F32(x) => float(x.abs()),
            Const::I32(x) => Some(Const::I32(x.wrapping_abs())),
            Const::U32(x) => Some(Const::U32(*x)),
            _ => None,
        }),
        B::Sign => each1(args[0].clone(), &|c| match c {
            Const::F32(x) => float(if *x > 0.0 {
                1.0
            } else if *x < 0.0 {
                -1.0
            } else {
                0.0
            }),
            Const::I32(x) => Some(Const::I32(x.signum())),
            _ => None,
        }),
        B::Min | B::Max => each2(args[0].clone(), args[1].clone(), &|x, y| {
            let max = b == B::Max;
            match (x, y) {
                (Const::F32(x), Const::F32(y)) => float(if max { x.max(*y) } else { x.min(*y) }),
                (Const::I32(x), Const::I32(y)) => {
                    Some(Const::I32(if max { *x.max(y) } else { *x.min(y) }))
                }
                (Const::U32(x), Const::U32(y)) => {
                    Some(Const::U32(if max { *x.max(y) } else { *x.min(y) }))
                }
                _ => None,
            }
        }),
        B::Clamp => {
            each3(args[0].clone(), args[1].clone(), args[2].clone(), &|x, lo, hi| match (x, lo, hi)
            {
                (Const::F32(x), Const::F32(lo), Const::F32(hi)) if lo <= hi => {
                    float(x.max(*lo).min(*hi))
                }
                (Const::I32(x), Const::I32(lo), Const::I32(hi)) if lo <= hi => {
                    Some(Const::I32(*x.max(lo).min(hi)))
                }
                (Const::U32(x), Const::U32(lo), Const::U32(hi)) if lo <= hi => {
                    Some(Const::U32(*x.max(lo).min(hi)))
                }
                _ => None,
            })
        }
        B::Saturate => each1(args[0].clone(), &|c| float(f(c)?.clamp(0.0, 1.0))),
        B::Mix => each3(args[0].clone(), args[1].clone(), args[2].clone(), &|a, b, t| {
            let (a, b, t) = (f(a)?, f(b)?, f(t)?);
            float(a * (1.0 - t) + b * t)
        }),
        B::Step => each2(args[0].clone(), args[1].clone(), &|edge, x| {
            float(if f(x)? >= f(edge)? { 1.0 } else { 0.0 })
        }),
        B::Smoothstep => each3(args[0].clone(), args[1].clone(), args[2].clone(), &|lo, hi, x| {
            let (lo, hi, x) = (f(lo)?, f(hi)?, f(x)?);
            if lo == hi {
                return None;
            }
            let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
            float(t * t * (3.0 - 2.0 * t))
        }),
        B::Length => {
            let v = floats(&args[0])?;
            float(v.iter().map(|x| x * x).sum::<f32>().sqrt()).map(K::S)
        }
        B::Distance => {
            let (a, b) = (floats(&args[0])?, floats(&args[1])?);
            (a.len() == b.len()).then_some(())?;
            float(a.iter().zip(&b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()).map(K::S)
        }
        B::Dot => {
            let (a, b) = (floats(&args[0])?, floats(&args[1])?);
            (a.len() == b.len()).then_some(())?;
            float(a.iter().zip(&b).map(|(x, y)| x * y).sum()).map(K::S)
        }
        B::Cross => {
            let (a, b) = (floats(&args[0])?, floats(&args[1])?);
            (a.len() == 3 && b.len() == 3).then_some(())?;
            let c =
                [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
            c.iter().map(|x| float(*x).map(K::S)).collect::<Option<Vec<_>>>().map(K::P)
        }
        B::Normalize => {
            let v = floats(&args[0])?;
            let l = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if l == 0.0 || v.len() < 2 {
                return None;
            }
            v.iter().map(|x| float(x / l).map(K::S)).collect::<Option<Vec<_>>>().map(K::P)
        }
        _ => None,
    }
}
