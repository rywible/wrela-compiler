//! The error walk (plan stage I): the interpreter's second way to run a function. Where the
//! concrete run holds a number, this one holds, for a box of inputs, a range and an error,
//! `(lo, hi, err)`: over the box the original's value lies in `[lo, hi]`, and a twin's value is
//! within `err` of the original's at the same input. A twin differs from the original at its
//! sites, each a value the twin computes differently, within its own bound `δ`; the walk adds
//! `δ` to that value's `err` and carries it to the result.
//!
//! Each rule is a line of Lipschitz reasoning over the real numbers (the `proven` of a twin
//! covers real arithmetic over the box; each target's rounding is §14.2's). Arithmetic is in
//! `f64`, each range's ends rounded outward by 2⁻⁴⁰ of their size. With `|a|` the largest size
//! in `a`'s range and `⌊a⌋` the least:
//!
//! - `a ± b`: `err_a + err_b` (the triangle inequality).
//! - `a · b`: `|a| err_b + |b| err_a + err_a err_b` (`a'b' - ab = a(b' - b) + b(a' - a) +
//!   (a' - a)(b' - b)`).
//! - `a / b`: `(err_a + |a / b| err_b) / (⌊b⌋ - err_b)`, refused unless `⌊b⌋ > err_b` (`a'/b' -
//!   a/b = ((a' - a) - (a/b)(b' - b)) / b'`, and `|b'| >= ⌊b⌋ - err_b`).
//! - `-a`, `abs`, `min`, `max`, `clamp`, `saturate`: the largest operand's (each is 1-Lipschitz
//!   in each operand, and moves by at most the largest move).
//! - `sin`, `cos`, `atan`, `tanh`: `err_a` (their derivatives are at most 1).
//! - `exp`: `exp(hi_a + err_a) · err_a` (the mean value theorem, the derivative at its largest).
//! - `log`: `err_a / (lo_a - err_a)`, refused unless `lo_a > err_a`.
//! - `sqrt`: `err_a / (2 sqrt(lo_a - err_a))`, refused unless `lo_a > err_a`.
//! - `pow(a, c)`, `c` exact: `|c| · sup |a|^(c - 1) · err_a` over `a`'s range widened by `err_a`.
//! - `mix(a, b, t)` = `a + (b - a) t`: `err_a + err_b + |b - a| err_t` (with `t` in [0, 1]; else
//!   by the product rule).
//! - `smoothstep(e0, e1, x)`, edges exact: `1.5 err_x / ⌊e1 - e0⌋` (its slope is at most 1.5 /
//!   (e1 - e0)).
//! - `length`, `distance`: the sum of the components' errors (1-Lipschitz in each).
//! - `dot`, `cross`, `normalize`: the rules above a component at a time (`normalize` as
//!   `v / length(v)`).
//! - `floor`, `ceil`, `round`, `trunc`, `fract`, `step`, `sign`, a comparison, a conversion to an
//!   integer, an index: fine where every operand's `err` is 0; else refused, unless the ranges
//!   widened by their errors settle a comparison (a changed value would otherwise decide a
//!   jump).
//! - A branch or `select` whose condition the ranges don't settle: both sides, joined (the hull,
//!   the larger `err`). A loop of a count the ranges settle: each pass in turn.
//! - Anything else is refused, naming it. A refusal means no proof: the rewrite isn't made.

use crate::const_fold::K;
use crate::{
    Arg, BinOp, Builtin, Const, Expr, FuncId, Module, Place, PlaceRoot, Proj, ResourceId, Scalar,
    Stmt, TypeDef, TypeId, UnOp, ValueId,
};
use std::collections::HashMap;
use wrela_diag::Span;

/// Whether `a > b`: false where either isn't a number.
fn above(a: f64, b: f64) -> bool {
    a > b
}

/// A float's range and error.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct R {
    pub lo: f64,
    pub hi: f64,
    pub err: f64,
}

impl R {
    pub fn exact(x: f64) -> R {
        R { lo: x, hi: x, err: 0.0 }
    }

    /// The largest size in the range.
    pub fn size(&self) -> f64 {
        self.lo.abs().max(self.hi.abs())
    }

    /// The least size in the range: 0 if it holds 0.
    pub fn least(&self) -> f64 {
        if self.lo <= 0.0 && self.hi >= 0.0 { 0.0 } else { self.lo.abs().min(self.hi.abs()) }
    }

    fn join(&self, o: &R) -> R {
        R { lo: self.lo.min(o.lo), hi: self.hi.max(o.hi), err: self.err.max(o.err) }
    }
}

/// A value of the walk.
#[derive(Clone, Debug, PartialEq)]
pub enum A {
    /// A float.
    F(R),
    /// An integer's range: exact (no error), one value or several.
    I(i128, i128),
    /// A boolean, `None` where the ranges don't settle it.
    B(Option<bool>),
    /// A vector's lanes, a struct's fields, an array's elements.
    P(Vec<A>),
}

/// Why the walk proves nothing: what it met, and where.
#[derive(Clone, Debug, PartialEq)]
pub struct Refused {
    pub what: String,
    pub at: Option<Span>,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.what)
    }
}

type Result<T> = std::result::Result<T, Refused>;

/// 2⁻⁴⁰: how much each range's ends move outward.
const OUTWARD: f64 = 1.0 / 1_099_511_627_776.0;

/// `[lo, hi]` rounded outward.
fn out(lo: f64, hi: f64, err: f64) -> R {
    let pad = |x: f64| x.abs() * OUTWARD + f64::MIN_POSITIVE;
    R { lo: lo - pad(lo), hi: hi + pad(hi), err: err + err * OUTWARD }
}

/// What a walk saw: each value's joined range and error, by function and value, and the place
/// in the source it was made at.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub values: HashMap<(FuncId, ValueId), (A, Option<Span>)>,
}

/// The walk of one function: its module, the sites' bounds, and what it has seen.
pub struct Walk<'a> {
    pub m: &'a Module,
    /// Each site's own bound `δ`, by function and value.
    pub sites: &'a HashMap<(FuncId, ValueId), f64>,
    pub seen: Seen,
    /// Each read buffer's every element, as a box: a kernel's inputs.
    pub resources: HashMap<ResourceId, A>,
    /// What the run wrote to buffers, joined: a kernel's result.
    pub written: Option<A>,
    /// Each private variable's value (`var<private>`: the kernel fills it in).
    private: HashMap<ResourceId, A>,
    frames: Vec<Frame>,
    at: Option<Span>,
}

struct Frame {
    func: FuncId,
    vals: Vec<Option<A>>,
    locals: Vec<A>,
    args: Vec<A>,
}

enum Flow {
    Normal,
    Break,
    Continue,
    Return(Option<A>),
}

/// The most passes of one loop the walk takes.
const MOST_PASSES: u32 = 1 << 16;

impl<'a> Walk<'a> {
    pub fn new(m: &'a Module, sites: &'a HashMap<(FuncId, ValueId), f64>) -> Walk<'a> {
        Walk {
            m,
            sites,
            seen: Seen::default(),
            resources: HashMap::new(),
            written: None,
            private: HashMap::new(),
            frames: Vec::new(),
            at: None,
        }
    }

    /// Walks function `f` over `args` (each a box, as ranges with errors): its result's range
    /// and error, or why there is no proof.
    pub fn run(&mut self, f: FuncId, args: Vec<A>) -> Result<Option<A>> {
        self.call(f, args)
    }

    fn refuse<T>(&self, what: impl Into<String>) -> Result<T> {
        Err(Refused { what: what.into(), at: self.at })
    }

    fn call(&mut self, f: FuncId, args: Vec<A>) -> Result<Option<A>> {
        let func = &self.m.functions[f.index()];
        if self.frames.len() > 64 {
            return self.refuse("calls nested more than 64 deep");
        }
        let mut locals = Vec::new();
        for l in &func.locals {
            locals.push(self.zero(l.ty)?);
        }
        self.frames.push(Frame { func: f, vals: vec![None; func.values.len()], locals, args });
        let r = self.block(&func.body);
        self.frames.pop();
        match r? {
            Flow::Return(v) => Ok(v),
            _ => Ok(None),
        }
    }

    fn zero(&self, t: TypeId) -> Result<A> {
        Ok(lift(&super::zero(&self.m.types, t)))
    }

    fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("a frame")
    }

    fn val(&self, v: ValueId) -> Result<A> {
        match self.frames.last().and_then(|f| f.vals.get(v.index()).cloned().flatten()) {
            Some(a) => Ok(a),
            None => self.refuse(format!("value {v:?} read before it's made")),
        }
    }

    fn block(&mut self, b: &[Stmt]) -> Result<Flow> {
        for s in b {
            match self.stmt(s)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn stmt(&mut self, s: &Stmt) -> Result<Flow> {
        match s {
            Stmt::At(span) => self.at = Some(*span),
            Stmt::Let(v, e) => {
                let mut x = match e {
                    // A builtin input (an invocation's id): exact, its zero value, since a
                    // kernel's result here doesn't depend on which invocation it is.
                    Expr::EntryInput(_) => {
                        let f = self.frames.last().expect("a frame").func;
                        Some(self.zero(self.m.functions[f.index()].value_ty(*v))?)
                    }
                    e => self.expr(e)?,
                };
                let f = self.frames.last().expect("a frame").func;
                if let Some(a) = &mut x {
                    if let Some(&d) = self.sites.get(&(f, *v)) {
                        add_err(a, d);
                    }
                    let entry = self.seen.values.entry((f, *v)).or_insert((a.clone(), self.at));
                    entry.0 = join(&entry.0, a);
                }
                self.frame().vals[v.index()] = x;
            }
            Stmt::Eval(e) => {
                self.expr(e)?;
            }
            Stmt::Store(p, v) => {
                let x = self.val(*v)?;
                self.store(p, x)?;
            }
            Stmt::If { cond, then, else_ } => {
                return match self.val(*cond)? {
                    A::B(Some(c)) => self.block(if c { then } else { else_ }),
                    A::B(None) => self.both(then, else_),
                    _ => self.refuse("a branch on a value that isn't a boolean"),
                };
            }
            Stmt::Loop { body, continuing } => {
                let mut n = 0;
                loop {
                    n += 1;
                    if n > MOST_PASSES {
                        return self.refuse(format!("a loop of more than {MOST_PASSES} passes"));
                    }
                    match self.block(body)? {
                        Flow::Break => break,
                        f @ Flow::Return(_) => return Ok(f),
                        Flow::Normal | Flow::Continue => {}
                    }
                    if let f @ Flow::Return(_) = self.block(continuing)? {
                        return Ok(f);
                    }
                }
            }
            Stmt::Break => return Ok(Flow::Break),
            Stmt::Continue => return Ok(Flow::Continue),
            Stmt::Return(v) => {
                let v = v.map(|v| self.val(v)).transpose()?;
                return Ok(Flow::Return(v));
            }
            Stmt::Trap => return self.refuse("a trap"),
        }
        Ok(Flow::Normal)
    }

    /// Both sides of a branch the ranges don't settle, joined: each must end normally.
    fn both(&mut self, then: &[Stmt], else_: &[Stmt]) -> Result<Flow> {
        let (vals, locals) = {
            let f = self.frames.last().expect("a frame");
            (f.vals.clone(), f.locals.clone())
        };
        let private = self.private.clone();
        let a = self.block(then)?;
        let (vals_a, locals_a) = {
            let f = self.frame();
            (std::mem::replace(&mut f.vals, vals), std::mem::replace(&mut f.locals, locals))
        };
        let private_a = std::mem::replace(&mut self.private, private);
        let b = self.block(else_)?;
        for (k, v) in private_a {
            let joined = match self.private.get(&k) {
                Some(w) => join(w, &v),
                None => v,
            };
            self.private.insert(k, joined);
        }
        let (Flow::Normal, Flow::Normal) = (&a, &b) else {
            return self.refuse("a branch the ranges don't settle that leaves its block");
        };
        let f = self.frame();
        for (x, y) in f.locals.iter_mut().zip(&locals_a) {
            *x = join(x, y);
        }
        for (x, y) in f.vals.iter_mut().zip(vals_a) {
            *x = match (x.take(), y) {
                (Some(p), Some(q)) => Some(join(&p, &q)),
                (p, q) => p.or(q),
            };
        }
        Ok(Flow::Normal)
    }

    fn store(&mut self, p: &Place, x: A) -> Result<()> {
        let path = self.path(&p.path)?;
        if let PlaceRoot::Resource(r) = p.root
            && self.m.resources[r.index()].kind == crate::ResourceKind::Private
        {
            let ty = self.m.resources[r.index()].ty;
            if !self.private.contains_key(&r) {
                let z = self.zero(ty)?;
                self.private.insert(r, z);
            }
            let target = self.private.get_mut(&r).expect("made");
            if !put(target, &path, x) {
                return self.refuse("a store through a projection");
            }
            return Ok(());
        }
        if let PlaceRoot::Resource(_) = p.root {
            // A buffer's element, or a part of one: the kernel's result.
            self.written = Some(match &self.written {
                Some(w) => join(w, &x),
                None => x,
            });
            return Ok(());
        }
        let target = match &p.root {
            PlaceRoot::Local(l) => &mut self.frame().locals[l.index()],
            _ => return self.refuse("a store outside the function's locals"),
        };
        if !put(target, &path, x) {
            return Err(Refused { what: "a store through a projection".into(), at: self.at });
        }
        Ok(())
    }

    /// A place's projections as indexes: each array index exact.
    fn path(&self, path: &[Proj]) -> Result<Vec<usize>> {
        let mut out = Vec::new();
        for p in path {
            out.push(match p {
                Proj::Field(i) => *i as usize,
                Proj::Comp(c) => *c as usize,
                Proj::Index(v) => match self.val(*v)? {
                    A::I(lo, hi) if lo == hi && lo >= 0 => lo as usize,
                    _ => return self.refuse("an index the ranges don't settle"),
                },
            });
        }
        Ok(out)
    }

    fn load(&mut self, p: &Place) -> Result<A> {
        // An exact index over a range reads each element it reaches, joined.
        if let Some(k) = p.path.iter().position(
            |q| matches!(q, Proj::Index(v) if matches!(self.val(*v), Ok(A::I(lo, hi)) if lo != hi)),
        ) {
            let Proj::Index(v) = p.path[k] else { unreachable!() };
            let Ok(A::I(lo, hi)) = self.val(v) else { unreachable!() };
            if lo < 0 || hi - lo > 4096 {
                return self.refuse("an index over a range too wide to read");
            }
            let mut joined: Option<A> = None;
            for i in lo..=hi {
                let mut q = p.clone();
                q.path[k] = Proj::Field(i as u32);
                match self.load(&q) {
                    Ok(x) => joined = Some(joined.map_or(x.clone(), |j| join(&j, &x))),
                    // Past the array's end: an index the program clamps reaches no further.
                    Err(_) if joined.is_some() => break,
                    Err(e) => return Err(e),
                }
            }
            return Ok(joined.expect("an element"));
        }
        let path = self.path(&p.path)?;
        let mut v = match &p.root {
            PlaceRoot::Local(l) => self.frames.last().expect("a frame").locals[l.index()].clone(),
            PlaceRoot::Param(i) => match self.frames.last().expect("a frame").args.get(*i as usize)
            {
                Some(a) => a.clone(),
                None => return self.refuse("a parameter that isn't given"),
            },
            PlaceRoot::Resource(r)
                if self.m.resources[r.index()].kind == crate::ResourceKind::Private =>
            {
                match self.private.get(r) {
                    Some(a) => a.clone(),
                    None => self.zero(self.m.resources[r.index()].ty)?,
                }
            }
            PlaceRoot::Resource(r) => match self.resources.get(r) {
                // Any element of a buffer: the first projection is its index.
                Some(a) => {
                    let mut v = a.clone();
                    for k in path.into_iter().skip(1) {
                        v = match v {
                            A::P(mut ps) if k < ps.len() => ps.swap_remove(k),
                            _ => return self.refuse("a projection out of a buffer's element"),
                        };
                    }
                    return Ok(v);
                }
                None => return self.refuse("a read of a buffer the walk isn't given"),
            },
            _ => return self.refuse("a read of memory other than a local or a parameter"),
        };
        for k in path {
            v = match v {
                A::P(mut ps) if k < ps.len() => ps.swap_remove(k),
                _ => return self.refuse("a projection out of its value"),
            };
        }
        Ok(v)
    }

    fn expr(&mut self, e: &Expr) -> Result<Option<A>> {
        Ok(Some(match e {
            Expr::Const(c) => lift(&K::S(c.clone())),
            Expr::Zero(t) => self.zero(*t)?,
            Expr::Load(p) => self.load(p)?,
            Expr::Param(i) => match self.frames.last().expect("a frame").args.get(*i as usize) {
                Some(a) => a.clone(),
                None => return self.refuse("a parameter that isn't given"),
            },
            Expr::Unary(op, v) => {
                let a = self.val(*v)?;
                self.unary(*op, a)?
            }
            Expr::Binary(op, a, b) => {
                let (a, b) = (self.val(*a)?, self.val(*b)?);
                self.binary(*op, a, b)?
            }
            Expr::Builtin(b, args) => {
                let mut xs = Vec::new();
                for a in args {
                    xs.push(self.val(*a)?);
                }
                self.builtin(*b, xs)?
            }
            Expr::Construct(t, parts) => {
                let mut xs = Vec::new();
                for p in parts {
                    xs.push(self.val(*p)?);
                }
                // A vector built of smaller vectors and scalars: their lanes in order.
                if let TypeDef::Vector(..) = self.m.types.get(*t) {
                    let mut lanes = Vec::new();
                    for x in xs {
                        match x {
                            A::P(ls) => lanes.extend(ls),
                            s => lanes.push(s),
                        }
                    }
                    A::P(lanes)
                } else {
                    A::P(xs)
                }
            }
            Expr::Extract(v, i) => match self.val(*v)? {
                A::P(mut ps) if (*i as usize) < ps.len() => ps.swap_remove(*i as usize),
                _ => return self.refuse("an extract out of its value"),
            },
            Expr::ExtractDyn(v, i) => {
                let (v, i) = (self.val(*v)?, self.val(*i)?);
                match (v, i) {
                    (A::P(ps), A::I(lo, hi)) if lo >= 0 && (hi as usize) < ps.len() => {
                        let mut r = ps[lo as usize].clone();
                        for p in &ps[lo as usize..=hi as usize] {
                            r = join(&r, p);
                        }
                        r
                    }
                    _ => return self.refuse("an index the ranges don't settle"),
                }
            }
            Expr::Splat(v, n) => A::P(vec![self.val(*v)?; *n as usize]),
            Expr::Swizzle(v, lanes) => match self.val(*v)? {
                A::P(ps) => {
                    let mut out = Vec::new();
                    for &l in lanes {
                        match ps.get(l as usize) {
                            Some(p) => out.push(p.clone()),
                            None => return self.refuse("a swizzle out of its vector"),
                        }
                    }
                    if out.len() == 1 { out.pop().expect("a lane") } else { A::P(out) }
                }
                _ => return self.refuse("a swizzle of a scalar"),
            },
            Expr::Convert(v, to) => {
                let a = self.val(*v)?;
                map1(&a, &mut |x| convert(x, *to)).map_err(|w| self.why(w))?
            }
            Expr::Select { cond, if_true, if_false } => {
                let (c, t, f) = (self.val(*cond)?, self.val(*if_true)?, self.val(*if_false)?);
                select(&c, &t, &f).map_err(|w| self.why(w))?
            }
            Expr::Call(g, args) => {
                let mut xs = Vec::new();
                for a in args {
                    match a {
                        Arg::Value(v) => xs.push(self.val(*v)?),
                        Arg::Place(_) => return self.refuse("a call that's lent a place"),
                    }
                }
                return self.call(*g, xs);
            }
            other => return self.refuse(describe(other)),
        }))
    }

    fn why(&self, what: String) -> Refused {
        Refused { what, at: self.at }
    }

    fn unary(&self, op: UnOp, a: A) -> Result<A> {
        map1(&a, &mut |x| match (op, x) {
            (UnOp::Neg, A::F(r)) => Ok(A::F(R { lo: -r.hi, hi: -r.lo, err: r.err })),
            (UnOp::Neg, A::I(lo, hi)) => Ok(A::I(-hi, -lo)),
            (UnOp::Not, A::B(b)) => Ok(A::B(b.map(|b| !b))),
            (op, _) => Err(format!("`{op:?}` of this value")),
        })
        .map_err(|w| self.why(w))
    }

    fn binary(&self, op: BinOp, a: A, b: A) -> Result<A> {
        map2(&a, &b, &mut |x, y| binary(op, x, y)).map_err(|w| self.why(w))
    }

    fn builtin(&self, b: Builtin, xs: Vec<A>) -> Result<A> {
        builtin(b, &xs).map_err(|w| self.why(w))
    }
}

/// What a walk refuses an expression it doesn't run as.
fn describe(e: &Expr) -> &'static str {
    match e {
        Expr::Texture(..) => "a texture read",
        Expr::TextureStore(..) => "a texture write",
        Expr::Atomic(..) => "an atomic",
        Expr::Subgroup(..) => "a subgroup operation",
        Expr::Barrier => "a barrier",
        Expr::Discard => "a discard",
        Expr::Bitcast(..) => "a bitcast",
        Expr::Variant(..) => "an enum",
        Expr::EntryInput(_) => "an entry point's input",
        _ => "an operation the error walk doesn't run",
    }
}

/// A constant value as the walk's: exact.
pub fn lift(k: &K) -> A {
    match k {
        K::S(c) => match *c {
            Const::Bool(b) => A::B(Some(b)),
            Const::I32(x) => A::I(x as i128, x as i128),
            Const::U32(x) => A::I(x as i128, x as i128),
            Const::I64(x) => A::I(x as i128, x as i128),
            Const::U64(x) => A::I(x as i128, x as i128),
            Const::Small(_, x) => A::I(x as i128, x as i128),
            Const::F32(x) | Const::F16(x) => A::F(R::exact(f64::from(x))),
            Const::F64(x) => A::F(R::exact(x)),
        },
        K::P(ps) => A::P(ps.iter().map(lift).collect()),
    }
}

/// `a` and `b` joined: the hull of each float's range, the larger error.
pub fn join(a: &A, b: &A) -> A {
    match (a, b) {
        (A::F(x), A::F(y)) => A::F(x.join(y)),
        (A::I(a, b), A::I(c, d)) => A::I(*a.min(c), *b.max(d)),
        (A::B(x), A::B(y)) => A::B(if x == y { *x } else { None }),
        (A::P(xs), A::P(ys)) if xs.len() == ys.len() => {
            A::P(xs.iter().zip(ys).map(|(x, y)| join(x, y)).collect())
        }
        (x, _) => x.clone(),
    }
}

fn add_err(a: &mut A, d: f64) {
    match a {
        A::F(r) => r.err += d,
        A::P(ps) => ps.iter_mut().for_each(|p| add_err(p, d)),
        _ => {}
    }
}

fn put(v: &mut A, path: &[usize], x: A) -> bool {
    match path.split_first() {
        None => {
            *v = x;
            true
        }
        Some((&k, rest)) => match v {
            A::P(ps) if k < ps.len() => put(&mut ps[k], rest, x),
            _ => false,
        },
    }
}

type Rule<'f> = dyn FnMut(&A) -> std::result::Result<A, String> + 'f;

/// `f` lane by lane.
fn map1(a: &A, f: &mut Rule<'_>) -> std::result::Result<A, String> {
    match a {
        A::P(ps) => Ok(A::P(ps.iter().map(|p| map1(p, f)).collect::<std::result::Result<_, _>>()?)),
        x => f(x),
    }
}

/// `f` lane by lane over two values, a scalar against a vector's every lane.
fn map2(
    a: &A,
    b: &A,
    f: &mut dyn FnMut(&A, &A) -> std::result::Result<A, String>,
) -> std::result::Result<A, String> {
    match (a, b) {
        (A::P(xs), A::P(ys)) if xs.len() == ys.len() => Ok(A::P(
            xs.iter().zip(ys).map(|(x, y)| map2(x, y, f)).collect::<std::result::Result<_, _>>()?,
        )),
        (A::P(xs), y) => {
            Ok(A::P(xs.iter().map(|x| map2(x, y, f)).collect::<std::result::Result<_, _>>()?))
        }
        (x, A::P(ys)) => {
            Ok(A::P(ys.iter().map(|y| map2(x, y, f)).collect::<std::result::Result<_, _>>()?))
        }
        (x, y) => f(x, y),
    }
}

/// `f` lane by lane over several values.
fn map_n(
    xs: &[A],
    f: &mut dyn FnMut(&[R]) -> std::result::Result<R, String>,
) -> std::result::Result<A, String> {
    let lanes = xs.iter().filter_map(|x| if let A::P(p) = x { Some(p.len()) } else { None }).max();
    let lane = |x: &A, i: usize| -> std::result::Result<R, String> {
        match x {
            A::F(r) => Ok(*r),
            A::P(p) => match p.get(i) {
                Some(A::F(r)) => Ok(*r),
                _ => Err("a lane that isn't a float".into()),
            },
            _ => Err("an operand that isn't a float".into()),
        }
    };
    match lanes {
        None => Ok(A::F(f(&xs
            .iter()
            .map(|x| lane(x, 0))
            .collect::<std::result::Result<Vec<_>, _>>()?)?)),
        Some(n) => {
            let mut out = Vec::new();
            for i in 0..n {
                let rs =
                    xs.iter().map(|x| lane(x, i)).collect::<std::result::Result<Vec<_>, _>>()?;
                out.push(A::F(f(&rs)?));
            }
            Ok(A::P(out))
        }
    }
}

/// The lanes of a float vector (or a float alone).
fn lanes(a: &A) -> std::result::Result<Vec<R>, String> {
    match a {
        A::F(r) => Ok(vec![*r]),
        A::P(ps) => ps
            .iter()
            .map(|p| if let A::F(r) = p { Ok(*r) } else { Err("a lane that isn't a float".into()) })
            .collect(),
        _ => Err("an operand that isn't a float".into()),
    }
}

fn add(a: R, b: R) -> R {
    out(a.lo + b.lo, a.hi + b.hi, a.err + b.err)
}

fn sub(a: R, b: R) -> R {
    out(a.lo - b.hi, a.hi - b.lo, a.err + b.err)
}

fn mul(a: R, b: R) -> R {
    let ps = [a.lo * b.lo, a.lo * b.hi, a.hi * b.lo, a.hi * b.hi];
    let (lo, hi) =
        ps.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &p| (l.min(p), h.max(p)));
    out(lo, hi, a.size() * b.err + b.size() * a.err + a.err * b.err)
}

fn div(a: R, b: R) -> std::result::Result<R, String> {
    if b.least() == 0.0 {
        return Err("a division by a range that holds 0".into());
    }
    if b.least() <= b.err {
        return Err("a division whose divisor's error reaches 0".into());
    }
    let ps = [a.lo / b.lo, a.lo / b.hi, a.hi / b.lo, a.hi / b.hi];
    let (lo, hi) =
        ps.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &p| (l.min(p), h.max(p)));
    let q = lo.abs().max(hi.abs());
    Ok(out(lo, hi, (a.err + q * b.err) / (b.least() - b.err)))
}

fn binary(op: BinOp, x: &A, y: &A) -> std::result::Result<A, String> {
    use BinOp::*;
    match (x, y) {
        (A::F(a), A::F(b)) => match op {
            Add => Ok(A::F(add(*a, *b))),
            Sub => Ok(A::F(sub(*a, *b))),
            Mul => Ok(A::F(mul(*a, *b))),
            Div => Ok(A::F(div(*a, *b)?)),
            Eq | Ne | Lt | Le | Gt | Ge => compare(op, *a, *b),
            _ => Err(format!("`{op:?}` of floats")),
        },
        (A::I(a, b), A::I(c, d)) => {
            let exact = a == b && c == d;
            match op {
                Add | WrappingAdd => Ok(A::I(a + c, b + d)),
                Sub | WrappingSub => Ok(A::I(a - d, b - c)),
                Mul | WrappingMul => {
                    let ps = [a * c, a * d, b * c, b * d];
                    Ok(A::I(*ps.iter().min().expect("4"), *ps.iter().max().expect("4")))
                }
                Div if exact && *c != 0 => Ok(A::I(a / c, a / c)),
                Rem if exact && *c != 0 => Ok(A::I(a % c, a % c)),
                Shl if exact && (0..64).contains(c) => Ok(A::I(a << c, a << c)),
                Shr if exact && (0..64).contains(c) => Ok(A::I(a >> c, a >> c)),
                BitAnd if exact => Ok(A::I(a & c, a & c)),
                BitOr if exact => Ok(A::I(a | c, a | c)),
                BitXor if exact => Ok(A::I(a ^ c, a ^ c)),
                Eq | Ne | Lt | Le | Gt | Ge => {
                    let r = |l: i128, h: i128| R { lo: l as f64, hi: h as f64, err: 0.0 };
                    compare(op, r(*a, *b), r(*c, *d))
                }
                _ => Err(format!("`{op:?}` of integer ranges")),
            }
        }
        (A::B(a), A::B(b)) => match op {
            And => Ok(A::B(match (a, b) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            })),
            Or => Ok(A::B(match (a, b) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            })),
            Eq => Ok(A::B(a.zip(*b).map(|(a, b)| a == b))),
            Ne => Ok(A::B(a.zip(*b).map(|(a, b)| a != b))),
            _ => Err(format!("`{op:?}` of booleans")),
        },
        _ => Err(format!("`{op:?}` of these values")),
    }
}

/// A comparison: settled where the ranges widened by their errors settle it (for the original and
/// the twin alike); else unknown where neither has an error, and refused where one has.
fn compare(op: BinOp, a: R, b: R) -> std::result::Result<A, String> {
    use BinOp::*;
    let (alo, ahi, blo, bhi) = (a.lo - a.err, a.hi + a.err, b.lo - b.err, b.hi + b.err);
    let settled = match op {
        Lt => (ahi < blo).then_some(true).or((alo >= bhi).then_some(false)),
        Le => (ahi <= blo).then_some(true).or((alo > bhi).then_some(false)),
        Gt => (alo > bhi).then_some(true).or((ahi <= blo).then_some(false)),
        Ge => (alo >= bhi).then_some(true).or((ahi < blo).then_some(false)),
        Eq => (ahi < blo || alo > bhi).then_some(false),
        Ne => (ahi < blo || alo > bhi).then_some(true),
        _ => None,
    };
    match settled {
        Some(s) => Ok(A::B(Some(s))),
        None if a.err == 0.0 && b.err == 0.0 => Ok(A::B(None)),
        None => Err("a comparison a changed value could decide".into()),
    }
}

fn select(c: &A, t: &A, f: &A) -> std::result::Result<A, String> {
    match c {
        A::B(Some(true)) => Ok(t.clone()),
        A::B(Some(false)) => Ok(f.clone()),
        A::B(None) => Ok(join(t, f)),
        A::P(cs) => match (t, f) {
            (A::P(ts), A::P(fs)) if ts.len() == cs.len() && fs.len() == cs.len() => Ok(A::P(
                cs.iter()
                    .zip(ts.iter().zip(fs))
                    .map(|(c, (t, f))| select(c, t, f))
                    .collect::<std::result::Result<_, _>>()?,
            )),
            _ => Err("a select of mismatched lanes".into()),
        },
        _ => Err("a select on a value that isn't a boolean".into()),
    }
}

fn convert(x: &A, to: Scalar) -> std::result::Result<A, String> {
    let float = matches!(to, Scalar::F32 | Scalar::F64 | Scalar::F16);
    match x {
        A::F(r) if float => Ok(A::F(*r)),
        A::I(lo, hi) if float => Ok(A::F(R { lo: *lo as f64, hi: *hi as f64, err: 0.0 })),
        A::F(r) if r.err == 0.0 && r.lo.is_finite() && r.hi.is_finite() => {
            Ok(A::I(r.lo.trunc() as i128, r.hi.trunc() as i128))
        }
        A::F(_) => Err("a conversion to an integer of a value with an error".into()),
        A::I(lo, hi) => Ok(A::I(*lo, *hi)),
        A::B(b) => Ok(match b {
            Some(b) if float => A::F(R::exact(f64::from(u8::from(*b)))),
            Some(b) => A::I(i128::from(*b), i128::from(*b)),
            None if float => A::F(R { lo: 0.0, hi: 1.0, err: 0.0 }),
            None => A::I(0, 1),
        }),
        _ => Err("a conversion of an aggregate".into()),
    }
}

/// A monotone function's range over `[lo, hi]`: increasing, or decreasing.
fn mono(r: R, f: impl Fn(f64) -> f64, increasing: bool, err: f64) -> R {
    let (a, b) = (f(r.lo), f(r.hi));
    if increasing { out(a, b, err) } else { out(b, a, err) }
}

/// sin's range over `[lo, hi]`, and cos's (sin shifted).
fn sin_range(lo: f64, hi: f64) -> (f64, f64) {
    use std::f64::consts::{FRAC_PI_2, PI, TAU};
    if !above(TAU, hi - lo) {
        return (-1.0, 1.0);
    }
    let (mut l, mut h) = (lo.sin().min(hi.sin()), lo.sin().max(hi.sin()));
    // Each extreme π/2 + kπ inside the range.
    let mut k = ((lo - FRAC_PI_2) / PI).ceil();
    while FRAC_PI_2 + k * PI <= hi {
        let v = (FRAC_PI_2 + k * PI).sin();
        l = l.min(v);
        h = h.max(v);
        k += 1.0;
    }
    (l, h)
}

/// The builtins' ranges and errors, the rules of this file's comment.
fn builtin(b: Builtin, xs: &[A]) -> std::result::Result<A, String> {
    use Builtin as B;
    let exact_only = |name: &str, xs: &[R]| -> std::result::Result<(), String> {
        if xs.iter().any(|x| x.err > 0.0) {
            Err(format!("`{name}` of a value with an error: a changed value could decide a jump"))
        } else {
            Ok(())
        }
    };
    match b {
        B::Sin | B::Cos => map_n(xs, &mut |r| {
            let x = r[0];
            let (lo, hi) = if b == B::Sin {
                sin_range(x.lo, x.hi)
            } else {
                sin_range(x.lo + std::f64::consts::FRAC_PI_2, x.hi + std::f64::consts::FRAC_PI_2)
            };
            Ok(out(lo, hi, x.err))
        }),
        B::Atan => map_n(xs, &mut |r| Ok(mono(r[0], f64::atan, true, r[0].err))),
        B::Tanh => map_n(xs, &mut |r| Ok(mono(r[0], f64::tanh, true, r[0].err))),
        B::Exp => map_n(xs, &mut |r| {
            let x = r[0];
            Ok(mono(x, f64::exp, true, (x.hi + x.err).exp() * x.err))
        }),
        B::Exp2 => map_n(xs, &mut |r| {
            let x = r[0];
            let ln2 = std::f64::consts::LN_2;
            Ok(mono(x, f64::exp2, true, ln2 * (x.hi + x.err).exp2() * x.err))
        }),
        B::Log | B::Log2 => map_n(xs, &mut |r| {
            let x = r[0];
            if !above(x.lo, x.err) {
                return Err("a log whose argument's range, widened by its error, reaches 0".into());
            }
            let s = if b == B::Log { 1.0 } else { std::f64::consts::LOG2_E };
            let f = if b == B::Log { f64::ln } else { f64::log2 };
            Ok(mono(x, f, true, s * x.err / (x.lo - x.err)))
        }),
        B::Sqrt => map_n(xs, &mut |r| {
            let x = r[0];
            if !above(x.lo, x.err) {
                if x.err == 0.0 && x.lo >= 0.0 {
                    return Ok(mono(x, f64::sqrt, true, 0.0));
                }
                return Err("a sqrt whose argument's range, widened by its error, reaches 0".into());
            }
            Ok(mono(x, f64::sqrt, true, x.err / (2.0 * (x.lo - x.err).sqrt())))
        }),
        B::InverseSqrt => map_n(xs, &mut |r| {
            let x = r[0];
            if !above(x.lo, x.err) {
                return Err("an inverse sqrt whose argument reaches 0".into());
            }
            let w = x.lo - x.err;
            Ok(mono(x, |v| 1.0 / v.sqrt(), false, 0.5 * w.powf(-1.5) * x.err))
        }),
        B::Pow => map_n(xs, &mut |r| {
            let (a, c) = (r[0], r[1]);
            if c.lo != c.hi || c.err != 0.0 {
                return general_pow(a, c);
            }
            let c = c.lo;
            let (wlo, whi) = (a.lo - a.err, a.hi + a.err);
            if !above(wlo, 0.0) && c.fract() != 0.0 {
                return Err("a power of a range that reaches 0 or below".into());
            }
            if !above(wlo, 0.0) && c < 1.0 && c != 0.0 {
                return Err("a power below 1 of a range that reaches 0".into());
            }
            let p = |v: f64| v.powf(c);
            let ends = [p(a.lo), p(a.hi)];
            let (mut lo, mut hi) = (ends[0].min(ends[1]), ends[0].max(ends[1]));
            if a.lo < 0.0 && a.hi > 0.0 && c > 0.0 {
                lo = lo.min(0.0);
                hi = hi.max(0.0);
            }
            // sup |v|^(c - 1) over the widened range.
            let e = c - 1.0;
            let sup = if e >= 0.0 {
                wlo.abs().max(whi.abs()).powf(e)
            } else {
                let least = if wlo <= 0.0 && whi >= 0.0 { 0.0 } else { wlo.abs().min(whi.abs()) };
                least.powf(e)
            };
            Ok(out(lo, hi, c.abs() * sup * a.err))
        }),
        B::Atan2 => map_n(xs, &mut |r| {
            // atan2(y, x) moves by at most |Δ(y, x)| / r, r the least distance from the origin
            // the moved point can have: its gradient is (x, -y) / r².
            let (y, x) = (r[0], r[1]);
            let near = (y.least().powi(2) + x.least().powi(2)).sqrt() - (y.err + x.err);
            if !above(near, 0.0) {
                return Err("an atan2 whose point, moved by its error, reaches the origin".into());
            }
            let range = if x.lo > 0.0 {
                let ends = [y.lo.atan2(x.lo), y.lo.atan2(x.hi), y.hi.atan2(x.lo), y.hi.atan2(x.hi)];
                (
                    ends.iter().copied().fold(f64::INFINITY, f64::min),
                    ends.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                )
            } else {
                (-std::f64::consts::PI, std::f64::consts::PI)
            };
            if !above(x.lo, x.err) && y.lo - y.err <= 0.0 && y.hi + y.err >= 0.0 {
                return Err("an atan2 whose moved point could cross its cut".into());
            }
            Ok(out(range.0, range.1, (y.err + x.err) / near))
        }),
        B::Abs => map_n(xs, &mut |r| {
            let x = r[0];
            let (lo, hi) = if x.lo >= 0.0 {
                (x.lo, x.hi)
            } else if x.hi <= 0.0 {
                (-x.hi, -x.lo)
            } else {
                (0.0, x.size())
            };
            Ok(out(lo, hi, x.err))
        }),
        B::Min => map_n(xs, &mut |r| {
            Ok(out(r[0].lo.min(r[1].lo), r[0].hi.min(r[1].hi), r[0].err.max(r[1].err)))
        }),
        B::Max => map_n(xs, &mut |r| {
            Ok(out(r[0].lo.max(r[1].lo), r[0].hi.max(r[1].hi), r[0].err.max(r[1].err)))
        }),
        B::Clamp => map_n(xs, &mut |r| {
            let (x, l, h) = (r[0], r[1], r[2]);
            // min(max(x, l), h): increasing in each, so its ends are its operands' ends.
            Ok(out(x.lo.max(l.lo).min(h.lo), x.hi.max(l.hi).min(h.hi), x.err.max(l.err).max(h.err)))
        }),
        B::Saturate => {
            map_n(xs, &mut |r| Ok(out(r[0].lo.clamp(0.0, 1.0), r[0].hi.clamp(0.0, 1.0), r[0].err)))
        }
        B::Mix => map_n(xs, &mut |r| {
            let (a, c, t) = (r[0], r[1], r[2]);
            let d = sub(c, a);
            let v = add(a, mul(d, t));
            let err = if t.lo >= 0.0 && t.hi <= 1.0 && t.err == 0.0 {
                a.err.max(c.err) + d.size() * t.err
            } else if t.lo >= 0.0 && t.hi <= 1.0 {
                a.err + c.err + d.size() * t.err
            } else {
                v.err
            };
            Ok(R { lo: v.lo, hi: v.hi, err })
        }),
        B::Smoothstep => map_n(xs, &mut |r| {
            let (e0, e1, x) = (r[0], r[1], r[2]);
            if e0.err != 0.0 || e1.err != 0.0 {
                return Err("a smoothstep whose edges have an error".into());
            }
            let gap = e1.lo - e0.hi;
            if !above(gap, 0.0) {
                return Err("a smoothstep whose edges' ranges meet".into());
            }
            let s = |v: f64, e0: f64, e1: f64| {
                let t = ((v - e0) / (e1 - e0)).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            };
            // Increasing in x, decreasing in each edge.
            Ok(out(s(x.lo, e0.hi, e1.hi), s(x.hi, e0.lo, e1.lo), 1.5 * x.err / gap))
        }),
        B::Floor | B::Ceil | B::Round | B::Trunc => map_n(xs, &mut |r| {
            exact_only("floor", r)?;
            let f = match b {
                B::Floor => f64::floor,
                B::Ceil => f64::ceil,
                B::Round => f64::round,
                _ => f64::trunc,
            };
            Ok(R { lo: f(r[0].lo), hi: f(r[0].hi), err: 0.0 })
        }),
        B::Fract => map_n(xs, &mut |r| {
            exact_only("fract", r)?;
            let x = r[0];
            if x.lo.floor() == x.hi.floor() {
                Ok(out(x.lo - x.lo.floor(), x.hi - x.hi.floor(), 0.0))
            } else {
                Ok(R { lo: 0.0, hi: 1.0, err: 0.0 })
            }
        }),
        B::Step => map_n(xs, &mut |r| {
            let (e, x) = (r[0], r[1]);
            match compare(BinOp::Ge, x, e)? {
                A::B(Some(true)) => Ok(R::exact(1.0)),
                A::B(Some(false)) => Ok(R::exact(0.0)),
                _ => Ok(R { lo: 0.0, hi: 1.0, err: 0.0 }),
            }
        }),
        B::Sign => map_n(xs, &mut |r| {
            let x = r[0];
            let (lo, hi) = (x.lo - x.err, x.hi + x.err);
            if lo > 0.0 {
                Ok(R::exact(1.0))
            } else if hi < 0.0 {
                Ok(R::exact(-1.0))
            } else if x.err == 0.0 {
                Ok(R {
                    lo: if x.lo < 0.0 { -1.0 } else { 0.0 },
                    hi: if x.hi > 0.0 { 1.0 } else { 0.0 },
                    err: 0.0,
                })
            } else {
                Err("a sign a changed value could decide".into())
            }
        }),
        B::Dot => {
            let (a, c) = (lanes(&xs[0])?, lanes(&xs[1])?);
            let mut s = R::exact(0.0);
            for (x, y) in a.iter().zip(&c) {
                s = add(s, mul(*x, *y));
            }
            Ok(A::F(s))
        }
        B::Length => Ok(A::F(length(&lanes(&xs[0])?)?)),
        B::Distance => {
            let (a, c) = (lanes(&xs[0])?, lanes(&xs[1])?);
            let d: Vec<R> = a.iter().zip(&c).map(|(x, y)| sub(*x, *y)).collect();
            Ok(A::F(length(&d)?))
        }
        B::Normalize => {
            let v = lanes(&xs[0])?;
            let l = length(&v)?;
            Ok(A::P(v.iter().map(|x| div(*x, l).map(A::F)).collect::<std::result::Result<_, _>>()?))
        }
        B::Cross => {
            let (a, c) = (lanes(&xs[0])?, lanes(&xs[1])?);
            if a.len() != 3 || c.len() != 3 {
                return Err("a cross product of vectors that aren't 3D".into());
            }
            let comp = |i: usize, j: usize| sub(mul(a[i], c[j]), mul(a[j], c[i]));
            Ok(A::P(vec![A::F(comp(1, 2)), A::F(comp(2, 0)), A::F(comp(0, 1))]))
        }
        other => Err(format!("`{other:?}`")),
    }
}

/// `a^c` where the exponent varies too: as `exp(c ln a)`, with `a` above 0. Its move is at most
/// `sup |c a^(c-1)| err_a + sup |a^c ln a| err_c` over the ranges widened by their errors (the
/// mean value theorem in each argument in turn).
fn general_pow(a: R, c: R) -> std::result::Result<R, String> {
    let (alo, ahi) = (a.lo - a.err, a.hi + a.err);
    if !above(alo, 0.0) {
        return Err("a power of a range that reaches 0 or below".into());
    }
    let (clo, chi) = (c.lo - c.err, c.hi + c.err);
    // a^c is monotone in each argument, so its extremes are at the corners.
    let corners = |l1: f64, h1: f64, l2: f64, h2: f64| {
        let v = [l1.powf(l2), l1.powf(h2), h1.powf(l2), h1.powf(h2)];
        (
            v.iter().copied().fold(f64::INFINITY, f64::min),
            v.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        )
    };
    let (lo, hi) = corners(a.lo, a.hi, c.lo, c.hi);
    let (_, wide) = corners(alo, ahi, clo, chi);
    // |c a^(c-1)| at most max|c| · max a^(c-1), the latter at a corner too.
    let (_, d1) = corners(alo, ahi, clo - 1.0, chi - 1.0);
    let cmax = clo.abs().max(chi.abs());
    let lnmax = alo.ln().abs().max(ahi.ln().abs());
    Ok(out(lo, hi, cmax * d1 * a.err + wide * lnmax * c.err))
}

/// A vector's length: its range from the components' squares, its error the sum of theirs.
fn length(v: &[R]) -> std::result::Result<R, String> {
    let (mut lo2, mut hi2, mut err) = (0.0, 0.0, 0.0);
    for x in v {
        let l = x.least();
        lo2 += l * l;
        hi2 += x.size() * x.size();
        err += x.err;
    }
    Ok(out(f64::sqrt(lo2), f64::sqrt(hi2), err))
}

/// Walks kernel function `f` in `m` with each read buffer's elements the boxes `resources`
/// gives, with `sites`' bounds: what it wrote, joined, and what it saw.
pub fn walk_kernel(
    m: &Module,
    f: FuncId,
    resources: HashMap<ResourceId, A>,
    sites: &HashMap<(FuncId, ValueId), f64>,
) -> std::result::Result<(Option<A>, Seen), Refused> {
    let mut w = Walk::new(m, sites);
    w.resources = resources;
    w.run(f, Vec::new())?;
    Ok((w.written, w.seen))
}

/// Walks `f` in `m` over `args`, with `sites`' bounds: its result and what it saw.
pub fn walk(
    m: &Module,
    f: FuncId,
    args: Vec<A>,
    sites: &HashMap<(FuncId, ValueId), f64>,
) -> std::result::Result<(Option<A>, Seen), Refused> {
    let mut w = Walk::new(m, sites);
    let r = w.run(f, args)?;
    Ok((r, w.seen))
}

/// The largest error of a result.
pub fn largest_err(a: &A) -> f64 {
    match a {
        A::F(r) => r.err,
        A::P(ps) => ps.iter().map(largest_err).fold(0.0, f64::max),
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each rule against sampled perturbations: over a box, moving the inputs by at most their
    /// errors never moves the result by more than the rule's error.
    fn check(
        name: &str,
        f: impl Fn(&[f64]) -> f64,
        rule: impl Fn(&[R]) -> R,
        boxes: &[(f64, f64, f64)],
    ) {
        let rs: Vec<R> = boxes.iter().map(|&(lo, hi, err)| R { lo, hi, err }).collect();
        let r = rule(&rs);
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        for _ in 0..20000 {
            let x: Vec<f64> = rs.iter().map(|b| b.lo + (b.hi - b.lo) * next()).collect();
            let y: Vec<f64> =
                rs.iter().zip(&x).map(|(b, x)| x + b.err * (2.0 * next() - 1.0)).collect();
            let (fx, fy) = (f(&x), f(&y));
            assert!(fx >= r.lo && fx <= r.hi, "{name}: {fx} outside [{}, {}] at {x:?}", r.lo, r.hi);
            assert!(
                (fy - fx).abs() <= r.err * (1.0 + 1e-12),
                "{name}: moved {} over {} at {x:?}",
                (fy - fx).abs(),
                r.err
            );
        }
    }

    fn one(b: Builtin, rs: &[R]) -> R {
        let xs: Vec<A> = rs.iter().map(|r| A::F(*r)).collect();
        match builtin(b, &xs).expect("a rule") {
            A::F(r) => r,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn each_rule_bounds_sampled_perturbations() {
        check("add", |x| x[0] + x[1], |r| add(r[0], r[1]), &[(-1.0, 2.0, 0.01), (3.0, 4.0, 0.02)]);
        check("sub", |x| x[0] - x[1], |r| sub(r[0], r[1]), &[(-1.0, 2.0, 0.01), (3.0, 4.0, 0.02)]);
        check("mul", |x| x[0] * x[1], |r| mul(r[0], r[1]), &[(-1.0, 2.0, 0.01), (-3.0, 4.0, 0.02)]);
        check(
            "div",
            |x| x[0] / x[1],
            |r| div(r[0], r[1]).unwrap(),
            &[(-1.0, 2.0, 0.01), (0.5, 4.0, 0.02)],
        );
        check("sin", |x| x[0].sin(), |r| one(Builtin::Sin, r), &[(-1.0, 5.0, 0.05)]);
        check("cos", |x| x[0].cos(), |r| one(Builtin::Cos, r), &[(-1.0, 5.0, 0.05)]);
        check("atan", |x| x[0].atan(), |r| one(Builtin::Atan, r), &[(-3.0, 5.0, 0.05)]);
        check("tanh", |x| x[0].tanh(), |r| one(Builtin::Tanh, r), &[(-3.0, 5.0, 0.05)]);
        check("exp", |x| x[0].exp(), |r| one(Builtin::Exp, r), &[(-3.0, 2.0, 0.05)]);
        check("log", |x| x[0].ln(), |r| one(Builtin::Log, r), &[(0.3, 5.0, 0.05)]);
        check("sqrt", |x| x[0].sqrt(), |r| one(Builtin::Sqrt, r), &[(0.3, 5.0, 0.05)]);
        let c = R::exact(2.5);
        check("pow", |x| x[0].powf(2.5), |r| one(Builtin::Pow, &[r[0], c]), &[(0.3, 2.0, 0.05)]);
        let c = R::exact(-1.5);
        check(
            "pow below 0",
            |x| x[0].powf(-1.5),
            |r| one(Builtin::Pow, &[r[0], c]),
            &[(0.3, 2.0, 0.05)],
        );
        check(
            "mix",
            |x| x[0] + (x[1] - x[0]) * x[2],
            |r| one(Builtin::Mix, r),
            &[(-1.0, 2.0, 0.01), (3.0, 4.0, 0.02), (0.0, 1.0, 0.0)],
        );
        check(
            "mix, t moved",
            |x| x[0] + (x[1] - x[0]) * x[2],
            |r| one(Builtin::Mix, r),
            &[(-1.0, 2.0, 0.01), (3.0, 4.0, 0.02), (0.1, 0.9, 0.05)],
        );
        let (e0, e1) = (R::exact(0.2), R::exact(0.7));
        let ss = |v: f64| {
            let t = ((v - 0.2) / 0.5f64).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        check(
            "smoothstep",
            |x| ss(x[0]),
            |r| one(Builtin::Smoothstep, &[e0, e1, r[0]]),
            &[(-1.0, 2.0, 0.03)],
        );
        check("abs", |x| x[0].abs(), |r| one(Builtin::Abs, r), &[(-1.0, 2.0, 0.03)]);
        check(
            "pow, both moving",
            |x| x[0].powf(x[1]),
            |r| one(Builtin::Pow, r),
            &[(0.3, 2.0, 0.05), (-2.0, 2.0, 0.04)],
        );
        check(
            "atan2",
            |x| x[0].atan2(x[1]),
            |r| one(Builtin::Atan2, r),
            &[(-1.0, 2.0, 0.03), (0.5, 3.0, 0.02)],
        );
        check(
            "min",
            |x| x[0].min(x[1]),
            |r| one(Builtin::Min, r),
            &[(-1.0, 2.0, 0.01), (0.0, 3.0, 0.02)],
        );
        check(
            "clamp",
            |x| x[0].clamp(0.0, 1.0),
            |r| one(Builtin::Clamp, &[r[0], R::exact(0.0), R::exact(1.0)]),
            &[(-1.0, 2.0, 0.03)],
        );
        check(
            "clamp, its bounds moving",
            |x| x[0].max(x[1]).min(x[2]),
            |r| one(Builtin::Clamp, r),
            &[(-1.0, 0.1, 0.03), (-0.5, 0.3, 0.02), (0.6, 1.5, 0.01)],
        );
        check(
            "clamp, above its moving bounds",
            |x| x[0].max(x[1]).min(x[2]),
            |r| one(Builtin::Clamp, r),
            &[(1.6, 2.0, 0.03), (-0.5, 0.3, 0.02), (0.6, 1.5, 0.01)],
        );
        let lane = |rs: &[R]| A::P(rs.iter().map(|r| A::F(*r)).collect());
        check(
            "length",
            |x| (x[0] * x[0] + x[1] * x[1] + x[2] * x[2]).sqrt(),
            |r| match builtin(Builtin::Length, &[lane(r)]).unwrap() {
                A::F(r) => r,
                o => panic!("{o:?}"),
            },
            &[(-1.0, 2.0, 0.01), (0.5, 3.0, 0.02), (-2.0, -1.0, 0.03)],
        );
        check(
            "dot",
            |x| x[0] * x[2] + x[1] * x[3],
            |r| match builtin(Builtin::Dot, &[lane(&r[..2]), lane(&r[2..])]).unwrap() {
                A::F(r) => r,
                o => panic!("{o:?}"),
            },
            &[(-1.0, 2.0, 0.01), (0.5, 3.0, 0.02), (-2.0, -1.0, 0.03), (1.0, 2.0, 0.0)],
        );
    }

    #[test]
    fn a_jump_a_changed_value_could_decide_is_refused() {
        let a = R { lo: 0.0, hi: 1.0, err: 0.01 };
        assert!(compare(BinOp::Lt, a, R::exact(0.5)).is_err());
        // Settled even with the error: no refusal.
        assert_eq!(compare(BinOp::Lt, a, R::exact(2.0)), Ok(A::B(Some(true))));
        assert!(builtin(Builtin::Floor, &[A::F(a)]).is_err());
        assert!(builtin(Builtin::Floor, &[A::F(R { err: 0.0, ..a })]).is_ok());
        assert!(div(R::exact(1.0), R { lo: 0.01, hi: 1.0, err: 0.02 }).is_err());
    }
}
