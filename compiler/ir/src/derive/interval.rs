//! Interval arithmetic (language.md §13, D-075). An active value of type `T` becomes two values
//! of type `T`, `lo` and `hi`: componentwise bounds on every value it can take when the input
//! ranges over a box, **as the target computes it**. That last part is what makes the result
//! sound for the program that actually runs: each operation's bounds are widened outward by the
//! operation's error bound on the target, so a point evaluation, rounded however the target
//! rounds, still lands inside.
//!
//! - **CPU** (WASM: IEEE 754 binary32, round to nearest, subnormals kept). Each rounded result
//!   is widened by at least one ulp (`|x| 2⁻²³ + 2⁻¹⁴⁹`), more where the operation isn't
//!   correctly rounded (std's transcendentals, `mix`, `length`, `dot`).
//! - **GPU** (WGSL §15.7 accuracy, which allows flushing subnormals and assumes no infinities):
//!   each result is widened by twice its WGSL error bound, at least one ulp, plus `2⁻¹²⁶`; the
//!   unbounded end of a range is `±f32::MAX` rather than an infinity.
//!
//! Integers and bools carry ranges too. An integer operation is exact when its operands are
//! single values and gives the type's whole range otherwise (the hash in value noise is the
//! motivating case: exact within a lattice cell, `[0, 2³²)` across cells). A bool's range is
//! `[false, true]` when it could be either.
//!
//! A branch on a range that could go either way runs both sides, one after the other, and joins
//! what each wrote (the hull); `return`s are first turned into stores ([`single_exit`]) so both
//! sides reach the join. Loops must have exits that don't depend on the input.
//!
//! An enum's range is a struct ([`range_ty`]): its tag's range, and each variant's payload in
//! a field of its own, so the hull of two variants keeps both payloads (an enum's payloads
//! share their memory on the CPU).
//!
//! Known limits: NaN results (from `0 × ∞`, `∞ − ∞`, or a point outside a function's domain,
//! like `sqrt` of a negative) can't be bounded, and on the CPU a NaN bound is replaced by an
//! infinite one at the end; on the GPU, which may assume NaNs away, they aren't. WGSL bounds
//! `sin` and `cos` only on `[-π, π]`; outside it we assume the same absolute error. A branch
//! taken speculatively still runs its code: an integer overflow or out-of-range index there
//! traps on the CPU even if no point in the box would have taken it (a `Trap` statement itself
//! is skipped).

use super::*;
use crate::single_exit::single_exit;
use std::collections::HashMap;

type R<T> = Result<T>;
/// An active value's bounds.
type Iv = (ValueId, ValueId);

const F32_EPS: f64 = 1.0 / 8_388_608.0; // 2^-23: one ulp at 1
const F64_EPS: f64 = f64::EPSILON;

pub(super) fn interval(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    ncap: u32,
    target: Target,
    box_ty: TypeId,
    interval_ty: TypeId,
) -> R<FuncId> {
    let func = m.functions[f.index()].clone();
    let xi = ncap as usize;
    let x_ty = func.params.get(xi).ok_or_else(|| Error::internal("no input parameter"))?.ty;
    if !matches!(m.types.get(x_ty), TypeDef::Scalar(Scalar::F32) | TypeDef::Vector(Scalar::F32, _))
    {
        return Err(Error::not_derivable(format!(
            "an interval needs an `f32` or a float vector input, not `{}`",
            m.types.display(x_ty)
        )));
    }
    let f32 = m.types.f32();
    let ret_ok = func
        .ret
        .is_some_and(|r| r == f32 || matches!(m.types.get(r), TypeDef::Vector(Scalar::F32, _)));
    if !ret_ok || func.ret_ref {
        return Err(Error::not_derivable(
            "an interval needs a function that returns an `f32` or a float vector",
        ));
    }
    let out_ty = func.ret.unwrap_or(f32);
    let mut mask = vec![0; func.params.len()];
    mask[xi] = all_bits(&m.types, x_ty);
    let d = derive(m, cache, f, mask.clone(), target, false)?;
    let returns = returns_active(m, cache, f, mask, Mode::Interval) != 0;

    let mut params = func.params[..xi].to_vec();
    params.push(Param { name: "over".into(), ty: box_ty, by_ref: false, mutable: false });
    let mut g = Function::new(format!("{}_interval", func.name), params, Some(interval_ty));
    let mut body = Vec::new();
    let mut args: Vec<Arg> = (0..xi as u32).map(|i| g.param_arg(i, &mut body)).collect();
    let over = g.new_value(box_ty);
    body.push(Stmt::Let(over, Expr::Param(xi as u32)));
    let lo = g.new_value(x_ty);
    body.push(Stmt::Let(lo, Expr::Extract(over, 0)));
    let hi = g.new_value(x_ty);
    body.push(Stmt::Let(hi, Expr::Extract(over, 1)));
    // The input's `lo` takes its own slot; its `hi` is appended after the originals. Each is
    // passed the way `x` is (a borrowed vector by reference on the CPU).
    for (v, name) in [(lo, "lo"), (hi, "hi")] {
        if func.params[xi].by_ref {
            let l = g.new_local(name, x_ty);
            body.push(Stmt::Store(Place::local(l), v));
            args.push(Arg::Place(Place::local(l)));
        } else {
            args.push(Arg::Value(v));
        }
    }
    let rty = m.functions[d.index()]
        .ret
        .ok_or_else(|| Error::internal("a derived function returns nothing"))?;
    let r = g.new_value(rty);
    body.push(Stmt::Let(r, Expr::Call(d, args)));
    let (mut rlo, mut rhi) = (r, r);
    if returns {
        rlo = g.new_value(out_ty);
        body.push(Stmt::Let(rlo, Expr::Extract(r, 0)));
        rhi = g.new_value(out_ty);
        body.push(Stmt::Let(rhi, Expr::Extract(r, 1)));
    }
    if target == Target::Cpu {
        // A NaN bound means "unknown": make it infinite, a component at a time.
        let b = m.types.bool();
        let n = match m.types.get(out_ty) {
            TypeDef::Vector(_, n) => Some(*n as u32),
            _ => None,
        };
        for (v, inf) in [(&mut rlo, f32::NEG_INFINITY), (&mut rhi, f32::INFINITY)] {
            let mut parts = Vec::new();
            for c in 0..n.unwrap_or(1) {
                let x = match n {
                    Some(_) => {
                        let x = g.new_value(f32);
                        body.push(Stmt::Let(x, Expr::Extract(*v, c)));
                        x
                    }
                    None => *v,
                };
                let ok = g.new_value(b);
                body.push(Stmt::Let(ok, Expr::Binary(BinOp::Eq, x, x)));
                let k = g.new_value(f32);
                body.push(Stmt::Let(k, Expr::Const(Const::F32(inf))));
                let s = g.new_value(f32);
                body.push(Stmt::Let(s, Expr::Select { cond: ok, if_true: x, if_false: k }));
                parts.push(s);
            }
            *v = match n {
                Some(_) => {
                    let w = g.new_value(out_ty);
                    body.push(Stmt::Let(w, Expr::Construct(out_ty, parts)));
                    w
                }
                None => parts[0],
            };
        }
    }
    let out = g.new_value(interval_ty);
    body.push(Stmt::Let(out, Expr::Construct(interval_ty, vec![rlo, rhi])));
    body.push(Stmt::Return(Some(out)));
    g.body = body;
    Ok(m.add_function(g))
}

/// The interval version of `f` with active parameters `mask`. It takes `f`'s parameters, the
/// active ones as their `lo`, followed by a `hi` for each active one; it returns
/// `range<T> { lo, hi }` when its result is active. `speculative`: the call runs under a branch
/// on the input.
fn derive(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    mask: Vec<Bits>,
    target: Target,
    speculative: bool,
) -> R<FuncId> {
    if let Some(&d) = cache.interval.get(&(f, mask.clone())) {
        // A call back into one being built recurses. If a call on the way runs under a branch
        // on the input, so does the recursion, and running both sides may never end.
        let open = cache.building.iter().position(|(g, mk, _)| *g == f && *mk == mask);
        if let Some(i) = open
            && (speculative || cache.building[i + 1..].iter().any(|b| b.2))
        {
            return Err(Error::ActiveLoopExit(
                "this recursion runs under a branch on the input; an interval runs both sides \
                 of such a branch, so the recursion might not end"
                    .to_string(),
                None,
            ));
        }
        return Ok(d);
    }
    let orig = m.functions[f.index()].clone();
    no_gpu_work(&orig.body)?;
    let exit = single_exit(m, &orig);
    let returns = returns_active(m, cache, f, mask.clone(), Mode::Interval) != 0;
    let (func, result, act) = match exit {
        None => (orig, None, body_activity(m, cache, f, &mask, Mode::Interval)),
        Some((func, result)) => {
            let act = activity(m, &func, &mask, Mode::Interval, &mut |g, mk| {
                effect(m, cache, g, mk, Mode::Interval)
            });
            (func, result, act)
        }
    };
    if let Some(at) = act.active_loop_exit {
        return Err(Error::ActiveLoopExit(
            "a loop exits here depending on the input; an interval needs loops that run the \
             same number of times for every point"
                .to_string(),
            at,
        ));
    }
    no_write_through_mut(&func, &act, &mask, "a range")?;
    let mut nf = func.clone_signature();
    nf.name = format!("{}_iv", func.name);
    nf.interval = true;
    // A range's `lo` takes the original's slot, as a range type.
    let mut hparam = vec![None; func.params.len()];
    for (i, p) in func.params.iter().enumerate() {
        if mask[i] != 0 {
            let ty = range_ty(&mut m.types, p.ty);
            nf.params[i].ty = ty;
            hparam[i] = Some(nf.params.len() as u32);
            nf.params.push(Param { name: format!("{}_hi", p.name), ty, ..p.clone() });
        }
    }
    let mut hlocal = vec![None; func.locals.len()];
    for (l, decl) in func.locals.iter().enumerate() {
        if act.locals[l] != 0 {
            let ty = range_ty(&mut m.types, decl.ty);
            nf.locals[l].ty = ty;
            hlocal[l] = Some(nf.new_local(format!("{}_hi", decl.name), ty));
        }
    }
    if returns {
        let r = func.ret.ok_or_else(|| Error::internal("an active result of nothing"))?;
        let mut r = range_ty(&mut m.types, r);
        if func.ret_ref {
            // A projection's: pointers into the place's `lo` and its `hi`.
            r = m.types.intern(TypeDef::Ptr(r));
            nf.ret_ref = false;
        }
        nf.ret = Some(m.types.intern(TypeDef::Struct {
            name: format!("range<{}>", m.types.display(r)),
            fields: vec![("lo".into(), r), ("hi".into(), r)],
        }));
    }
    // With its signature, for a recursive call to read.
    let id = m.add_function(Function::new(nf.name.clone(), nf.params.clone(), nf.ret));
    // Cached before its body is built, for calls back into it; removed again if building
    // fails, so a later request doesn't find the bodiless placeholder.
    let key = (f, mask);
    cache.interval.insert(key.clone(), id);
    cache.building.push((f, key.1.clone(), speculative));
    let ptrs = Pointers::of(m, &func);
    let local_src = single_stores(m, &func);
    let mut b = Ivx {
        m,
        cache,
        f: &func,
        act,
        ptrs,
        target,
        nf,
        hparam,
        hlocal,
        iv: HashMap::new(),
        defs: HashMap::new(),
        local_src,
        branch_writes: Vec::new(),
        splitting: false,
        returns,
        speculative: 0,
        track: Track::default(),
    };
    let mut body = Vec::new();
    let built = (|| -> R<()> {
        if let Some(r) = result.filter(|r| b.act.local(*r)) {
            // The result starts empty, so a side that never returns adds nothing to the hull.
            // (A projection's pointers have no hull: every side that runs returns one.)
            let ty = b.nf.locals[r.index()].ty;
            if matches!(b.m.types.get(ty), TypeDef::Ptr(_)) {
                return b.block_into(&func.body, &mut body);
            }
            let e = b.empty(&mut body, ty)?;
            b.store_root(&mut body, &PlaceRoot::Local(r), e)?;
        }
        b.block_into(&func.body, &mut body)
    })();
    b.cache.building.pop();
    if let Err(e) = built {
        b.cache.interval.remove(&key);
        return Err(e);
    }
    let mut nf = b.nf;
    nf.body = body;
    m.functions[id.index()] = nf;
    Ok(id)
}

/// What narrowing changed for a branch, to put back after it.
enum Saved {
    /// A value's range, or that it had none of its own.
    Value(ValueId, Option<Iv>),
    /// A local's range as it was.
    Local(LocalId, Iv),
}

/// By local: the one value stored in it, when it's stored exactly once, whole, and nothing
/// else writes it (no projection's store, no `mut` argument): a `let` binding.
fn single_stores(m: &Module, f: &Function) -> Vec<Option<ValueId>> {
    let mut src: Vec<Option<ValueId>> = vec![None; f.locals.len()];
    let mut writes = vec![0u32; f.locals.len()];
    visit::walk(&f.body, &mut |s| match s {
        Stmt::Store(Place { root: PlaceRoot::Local(l), path }, v) => {
            writes[l.index()] += 1;
            if path.is_empty() {
                src[l.index()] = Some(*v);
            } else {
                writes[l.index()] += 1;
            }
        }
        Stmt::Let(_, Expr::Call(g, args)) | Stmt::Eval(Expr::Call(g, args)) => {
            for (a, p) in args.iter().zip(&m.functions[g.index()].params) {
                if let (Arg::Place(Place { root: PlaceRoot::Local(l), .. }), true) = (a, p.mutable)
                {
                    writes[l.index()] += 2;
                }
            }
        }
        Stmt::Let(_, Expr::Addr(Place { root: PlaceRoot::Local(l), .. })) => {
            writes[l.index()] += 2;
        }
        _ => {}
    });
    src.iter().zip(&writes).map(|(s, &w)| if w == 1 { *s } else { None }).collect()
}

/// What [`Ivx::leaves`] applies to each leaf.
type LeafFn<'f, 'a> = dyn FnMut(&mut Ivx<'a>, &mut Block, TypeId, &[ValueId]) -> R<ValueId> + 'f;

struct Ivx<'a> {
    m: &'a mut Module,
    cache: &'a mut DeriveCache,
    /// The function being derived, whose types the activity's leaves are of.
    f: &'a Function,
    act: Activity,
    /// What the function's pointers point into.
    ptrs: Pointers,
    target: Target,
    nf: Function,
    hparam: Vec<Option<u32>>,
    hlocal: Vec<Option<LocalId>>,
    iv: HashMap<ValueId, Iv>,
    /// Each active value's expression, in the original's values: what a branch's guard narrows
    /// through (`narrow`).
    defs: HashMap<ValueId, Expr>,
    /// By local: the one value stored in it, if it's stored once, whole, and written no other
    /// way (a `let` binding): what narrowing a load of it narrows next.
    local_src: Vec<Option<ValueId>>,
    /// The roots the branch being narrowed writes, which narrowing leaves alone.
    branch_writes: Vec<PlaceRoot>,
    /// Whether a `floor`'s case split is under way: one at a time, so the code grows by at
    /// most 2³ (`split_floor`).
    splitting: bool,
    returns: bool,
    /// How many branches deep we are that run whether or not a point would take them.
    speculative: u32,
    track: Track,
}

impl Builder for Ivx<'_> {
    fn emit(&mut self, out: &mut Block, ty: TypeId, e: Expr) -> ValueId {
        self.nf.let_(out, ty, e)
    }

    fn types(&self) -> &Types {
        &self.m.types
    }

    fn ty(&self, v: ValueId) -> TypeId {
        self.nf.value_ty(v)
    }
}

impl<'a> Ivx<'a> {
    // ---- emission helpers --------------------------------------------------------------------

    fn range(&self, v: ValueId) -> Iv {
        self.iv.get(&v).copied().unwrap_or((v, v))
    }

    /// [`range`](Self::range) as a range type: a value that isn't active, of a type with an
    /// enum in it, is rebuilt as its range type (see [`range_ty`]).
    fn lifted(&mut self, out: &mut Block, v: ValueId) -> Iv {
        if let Some(r) = self.iv.get(&v) {
            return *r;
        }
        let x = self.lift(out, v);
        (x, x)
    }

    /// A value as its range type: each enum's tag, and its payload in its variant's field
    /// (the other variants' fields zero).
    fn lift(&mut self, out: &mut Block, v: ValueId) -> ValueId {
        let ty = self.ty(v);
        let rt = range_ty(&mut self.m.types, ty);
        if rt == ty {
            return v;
        }
        let u32t = self.m.types.u32();
        let parts = match self.m.types.get(ty).clone() {
            TypeDef::Enum { variants, .. } => {
                let tag = self.emit(out, u32t, Expr::Extract(v, 0));
                let mut parts = vec![tag];
                for (k, (_, payload)) in variants.iter().enumerate() {
                    let Some(pt) = *payload else {
                        parts.push(self.emit(out, u32t, Expr::Const(Const::U32(0))));
                        continue;
                    };
                    let raw = self.emit(out, pt, Expr::Extract(v, 1 + k as u32));
                    let x = self.lift(out, raw);
                    let xt = self.ty(x);
                    let zero = self.emit(out, xt, Expr::Zero(xt));
                    let kc = self.emit(out, u32t, Expr::Const(Const::U32(k as u32)));
                    let is_k = self.cmp(out, BinOp::Eq, tag, kc);
                    parts.push(self.select(out, is_k, x, zero));
                }
                parts
            }
            TypeDef::Struct { fields, .. } => (0..fields.len() as u32)
                .map(|i| {
                    let x = self.emit(out, fields[i as usize].1, Expr::Extract(v, i));
                    self.lift(out, x)
                })
                .collect(),
            TypeDef::Array(e, n) => (0..n)
                .map(|i| {
                    let ic = self.emit(out, u32t, Expr::Const(Const::U32(i)));
                    let x = self.emit(out, e, Expr::ExtractDyn(v, ic));
                    self.lift(out, x)
                })
                .collect(),
            _ => return v,
        };
        self.emit(out, rt, Expr::Construct(rt, parts))
    }

    fn elem(&self, ty: TypeId) -> Scalar {
        self.m.types.element_scalar(ty).unwrap_or(Scalar::F32)
    }

    fn bool_ty(&mut self) -> TypeId {
        self.m.types.bool()
    }

    fn cmp(&mut self, out: &mut Block, op: BinOp, a: ValueId, b: ValueId) -> ValueId {
        let bt = self.bool_ty();
        self.emit(out, bt, Expr::Binary(op, a, b))
    }

    fn select(&mut self, out: &mut Block, c: ValueId, t: ValueId, f: ValueId) -> ValueId {
        let ty = self.ty(t);
        self.emit(out, ty, Expr::Select { cond: c, if_true: t, if_false: f })
    }

    fn not(&mut self, out: &mut Block, x: ValueId) -> ValueId {
        let bt = self.bool_ty();
        self.emit(out, bt, Expr::Unary(UnOp::Not, x))
    }

    /// A float constant of type `ty` (splatted for a vector).
    fn fc(&mut self, out: &mut Block, ty: TypeId, x: f64) -> ValueId {
        let s = self.elem(ty);
        let st = self.m.types.scalar(s);
        let c = if s == Scalar::F64 { Const::F64(x) } else { Const::F32(x as f32) };
        let v = self.emit(out, st, Expr::Const(c));
        self.coerce(out, v, ty)
    }

    /// The largest magnitude a float of this type takes here: infinity on the CPU, `MAX` on the
    /// GPU (which may assume infinities away).
    fn inf(&self) -> f64 {
        match self.target {
            Target::Gpu => f32::MAX as f64,
            Target::Cpu => f64::INFINITY,
        }
    }

    fn eps(&self, ty: TypeId) -> f64 {
        if self.elem(ty) == Scalar::F64 { F64_EPS } else { F32_EPS }
    }

    /// The smallest step a rounded result can be off by near zero.
    fn tiny(&self, ty: TypeId) -> f64 {
        match (self.target, self.elem(ty)) {
            (_, Scalar::F64) => f64::from_bits(1),
            (Target::Cpu, _) => f32::from_bits(1) as f64,
            (Target::Gpu, _) => f32::MIN_POSITIVE as f64,
        }
    }

    fn gpu(&self) -> bool {
        self.target == Target::Gpu
    }

    // ---- widening ----------------------------------------------------------------------------

    /// `[lo - (|lo| k ε + tiny + abs), hi + (|hi| k ε + tiny + abs)]`: outward by `ulps` ulps of
    /// each bound and `abs` absolutely.
    fn widen(&mut self, out: &mut Block, x: Iv, ty: TypeId, ulps: f64, abs: f64) -> Iv {
        let ml = self.builtin(out, Builtin::Abs, vec![x.0], ty);
        let mh = self.builtin(out, Builtin::Abs, vec![x.1], ty);
        self.widen_mag(out, x, ty, (ml, mh), ulps, abs)
    }

    /// Outward by `ulps` ulps of the magnitudes `mag` (for the low and high bound) and `abs`.
    fn widen_mag(
        &mut self,
        out: &mut Block,
        x: Iv,
        ty: TypeId,
        mag: (ValueId, ValueId),
        ulps: f64,
        abs: f64,
    ) -> Iv {
        let k = self.fc(out, ty, ulps * self.eps(ty));
        self.widen_k(out, x, ty, mag, k, abs)
    }

    /// Outward by `mag × k` (`k` a value, for error bounds that depend on the operand) plus a
    /// tiny and `abs`.
    fn widen_k(
        &mut self,
        out: &mut Block,
        x: Iv,
        ty: TypeId,
        mag: (ValueId, ValueId),
        k: ValueId,
        abs: f64,
    ) -> Iv {
        let t = self.tiny(ty) + abs;
        let t = self.fc(out, ty, t);
        let dl = self.bin(out, BinOp::Mul, mag.0, k, ty);
        let dl = self.bin(out, BinOp::Add, dl, t, ty);
        let lo = self.bin(out, BinOp::Sub, x.0, dl, ty);
        let dh = self.bin(out, BinOp::Mul, mag.1, k, ty);
        let dh = self.bin(out, BinOp::Add, dh, t, ty);
        let hi = self.bin(out, BinOp::Add, x.1, dh, ty);
        if self.gpu() {
            return (lo, hi);
        }
        // On the CPU a NaN bound (from ∞ − ∞, or 0 × ∞ at a corner) means "unknown": it becomes
        // infinite here, so that no comparison or rule after it reads it as a decided answer.
        let inf = self.inf();
        (self.unless_nan(out, lo, ty, -inf), self.unless_nan(out, hi, ty, inf))
    }

    /// `x`, or `to` in each component that's NaN.
    fn unless_nan(&mut self, out: &mut Block, x: ValueId, ty: TypeId, to: f64) -> ValueId {
        if let TypeDef::Vector(_, n) = *self.m.types.get(ty) {
            let f32t = self.m.types.f32();
            let comps = (0..n as u32)
                .map(|c| {
                    let xc = self.emit(out, f32t, Expr::Extract(x, c));
                    self.unless_nan(out, xc, f32t, to)
                })
                .collect();
            return self.emit(out, ty, Expr::Construct(ty, comps));
        }
        let ok = self.cmp(out, BinOp::Eq, x, x);
        let c = self.fc(out, ty, to);
        self.select(out, ok, x, c)
    }

    /// One correctly rounded operation (+, −, ×; WASM's ÷ and √): one ulp. Rounding to nearest
    /// is monotone, so this is more than a point evaluation needs; it also covers a GPU fusing
    /// a multiply into an add.
    fn widen1(&mut self, out: &mut Block, x: Iv, ty: TypeId) -> Iv {
        self.widen(out, x, ty, 1.0, 0.0)
    }

    // ---- types and aggregates ----------------------------------------------------------------

    /// Rebuilds values of type `ty` from `f` applied to each scalar or vector leaf of `vals`.
    fn leaves(
        &mut self,
        out: &mut Block,
        ty: TypeId,
        vals: &[ValueId],
        f: &mut LeafFn<'_, 'a>,
    ) -> R<ValueId> {
        // The parts' types. `vals` may be empty (an empty range is built from nothing).
        let parts: Vec<TypeId>;
        let mut arrays = false;
        match self.m.types.get(ty).clone() {
            TypeDef::Scalar(_) | TypeDef::Vector(..) => return f(self, out, ty, vals),
            TypeDef::Matrix(c, r) => {
                let col = self.m.types.vector(r);
                parts = vec![col; c as usize];
            }
            TypeDef::Struct { fields, .. } => {
                parts = fields.iter().map(|(_, t)| *t).collect();
            }
            TypeDef::Array(e, n) if n <= 256 => {
                arrays = true;
                parts = vec![e; n as usize];
            }
            TypeDef::Ptr(_) => {
                // Both sides of a branch on the input run: which place a store through the
                // join would write isn't one place.
                return Err(Error::not_derivable(
                    "an interval through a projection chosen by a branch on the input isn't \
                     supported",
                ));
            }
            _ => {
                return Err(Error::not_derivable(format!(
                    "an interval over a `{}` isn't supported",
                    self.m.types.display(ty)
                )));
            }
        }
        let u32t = self.m.types.u32();
        let mut built = Vec::new();
        for (k, pt) in parts.iter().enumerate() {
            let mut xs = Vec::new();
            for &v in vals {
                let e = if arrays {
                    let i = self.emit(out, u32t, Expr::Const(Const::U32(k as u32)));
                    Expr::ExtractDyn(v, i)
                } else {
                    Expr::Extract(v, k as u32)
                };
                xs.push(self.emit(out, *pt, e));
            }
            built.push(self.leaves(out, *pt, &xs, f)?);
        }
        Ok(self.emit(out, ty, Expr::Construct(ty, built)))
    }

    /// The smaller (`upper` false) or larger of two leaves.
    fn extreme(
        &mut self,
        out: &mut Block,
        ty: TypeId,
        a: ValueId,
        b: ValueId,
        upper: bool,
    ) -> ValueId {
        if self.m.types.as_scalar(ty) == Some(Scalar::Bool) {
            let op = if upper { BinOp::Or } else { BinOp::And };
            return self.bin(out, op, a, b, ty);
        }
        let op = if upper { Builtin::Max } else { Builtin::Min };
        self.builtin(out, op, vec![a, b], ty)
    }

    /// The join of two ranges of type `ty`, picked by a condition's range `c`: the first when
    /// it's surely true, the second when surely false, else the hull of both.
    fn merge(&mut self, out: &mut Block, ty: TypeId, c: Iv, t: Iv, e: Iv) -> R<Iv> {
        let pick = |upper: bool| {
            move |s: &mut Self, out: &mut Block, lt: TypeId, v: &[ValueId]| -> R<ValueId> {
                let h = s.extreme(out, lt, v[0], v[1], upper);
                let x = s.select(out, c.1, h, v[1]);
                Ok(s.select(out, c.0, v[0], x))
            }
        };
        let lo = self.leaves(out, ty, &[t.0, e.0], &mut pick(false))?;
        let hi = self.leaves(out, ty, &[t.1, e.1], &mut pick(true))?;
        Ok((lo, hi))
    }

    /// Bounds on one scalar type: its whole range.
    fn full(&mut self, out: &mut Block, ty: TypeId) -> Iv {
        let s = self.elem(ty);
        let (lo, hi) = match s {
            Scalar::Bool => (Const::Bool(false), Const::Bool(true)),
            Scalar::F32 | Scalar::F64 => {
                let inf = self.inf();
                let lo = self.fc(out, ty, -inf);
                let hi = self.fc(out, ty, inf);
                return (lo, hi);
            }
            Scalar::I32 => (Const::I32(i32::MIN), Const::I32(i32::MAX)),
            Scalar::U32 => (Const::U32(0), Const::U32(u32::MAX)),
            Scalar::I64 => (Const::I64(i64::MIN), Const::I64(i64::MAX)),
            Scalar::U64 => (Const::U64(0), Const::U64(u64::MAX)),
            Scalar::I8 => (Const::Small(s, i8::MIN as i64), Const::Small(s, i8::MAX as i64)),
            Scalar::U8 => (Const::Small(s, 0), Const::Small(s, u8::MAX as i64)),
            Scalar::I16 => (Const::Small(s, i16::MIN as i64), Const::Small(s, i16::MAX as i64)),
            Scalar::U16 => (Const::Small(s, 0), Const::Small(s, u16::MAX as i64)),
        };
        (self.emit(out, ty, Expr::Const(lo)), self.emit(out, ty, Expr::Const(hi)))
    }

    /// The empty range of type `ty` (`lo` above `hi`), the identity of the hull.
    fn empty(&mut self, out: &mut Block, ty: TypeId) -> R<Iv> {
        let leaf = |upper: bool| {
            move |s: &mut Self, out: &mut Block, lt: TypeId, _: &[ValueId]| -> R<ValueId> {
                let (lo, hi) = s.full(out, lt);
                let v = if upper { lo } else { hi };
                Ok(s.coerce(out, v, lt))
            }
        };
        let lo = self.leaves(out, ty, &[], &mut leaf(false))?;
        let hi = self.leaves(out, ty, &[], &mut leaf(true))?;
        Ok((lo, hi))
    }

    // ---- places ------------------------------------------------------------------------------

    fn hi_root(&self, r: &PlaceRoot) -> R<PlaceRoot> {
        Ok(match r {
            PlaceRoot::Local(l) => PlaceRoot::Local(
                self.hlocal[l.index()]
                    .ok_or_else(|| Error::internal("an inactive local's range"))?,
            ),
            PlaceRoot::Param(i) => PlaceRoot::Param(
                self.hparam[*i as usize]
                    .ok_or_else(|| Error::internal("an inactive parameter's range"))?,
            ),
            _ => return Err(Error::not_derivable("an interval can't go through this place")),
        })
    }

    /// The places holding a range's `lo` and `hi`.
    fn places(&self, p: &Place) -> R<(Place, Place)> {
        if self.act.index_active(p) {
            return Err(Error::not_derivable(
                "an interval through an array index that depends on the input isn't supported",
            ));
        }
        self.split(p)
    }

    /// The places holding the `lo` and `hi` of the range at `p`, whatever its indices are: in
    /// an inactive local or parameter, or in data, both are `p`.
    fn split(&self, p: &Place) -> R<(Place, Place)> {
        let at = |root: PlaceRoot| Place { root, path: p.path.clone() };
        Ok(match &p.root {
            // A projection's range: pointers into the place's `lo` and its `hi`.
            PlaceRoot::Ptr(v) => {
                let (lo, hi) = self.range(*v);
                (at(PlaceRoot::Ptr(lo)), at(PlaceRoot::Ptr(hi)))
            }
            r if self.act.root(r) => (p.clone(), at(self.hi_root(r)?)),
            _ => (p.clone(), p.clone()),
        })
    }

    /// A load through an array (or vector) index that depends on the input: the hull of the
    /// elements the index's range reaches, held to the array's bounds. A loop joins them, so the
    /// code doesn't grow with the array, and a range of a few indices (a table read near a
    /// point) gives a tight bound. One such index per place, into an array of known length, of
    /// elements with no enum in them.
    fn load_any(&mut self, out: &mut Block, p: &Place, ty: TypeId) -> R<Iv> {
        let mut active = p.path.iter().enumerate().filter_map(|(k, x)| match x {
            Proj::Index(v) if self.act.value(*v) => Some((k, *v)),
            _ => None,
        });
        let (at, index) = active.next().ok_or_else(|| Error::internal("no active index"))?;
        if active.next().is_some() {
            return Err(Error::not_derivable(
                "an interval through two array indexes that depend on the input isn't supported",
            ));
        }
        let prefix = Place { root: p.root.clone(), path: p.path[..at].to_vec() };
        let n = match self.m.place_ty(self.f, &prefix).map(|t| self.m.types.get(t).clone()) {
            Some(TypeDef::Array(_, n)) => n,
            Some(TypeDef::Vector(_, n)) => u32::from(n),
            _ => {
                return Err(Error::not_derivable(
                    "an interval through an index that depends on the input, into an array of \
                     unknown length, isn't supported",
                ));
            }
        };
        if self.m.place_ty(self.f, p) != Some(ty) {
            return Err(Error::not_derivable(
                "an interval through an index that depends on the input, into an array of \
                 enums, isn't supported",
            ));
        }
        // The index's range, held to 0..n − 1, as u32.
        let u32t = self.m.types.u32();
        let it = self.ty(index);
        let (il, ih) = self.range(index);
        let held = |me: &mut Self, out: &mut Block, x: ValueId| -> R<ValueId> {
            let last = match me.m.types.as_scalar(it) {
                Some(Scalar::U32) => Const::U32(n - 1),
                Some(Scalar::I32) => Const::I32(n as i32 - 1),
                _ => {
                    return Err(Error::not_derivable(
                        "an interval through an index that isn't u32 or i32",
                    ));
                }
            };
            let last = me.emit(out, it, Expr::Const(last));
            let over = me.cmp(out, BinOp::Gt, x, last);
            let x = me.select(out, over, last, x);
            if me.m.types.as_scalar(it) == Some(Scalar::I32) {
                let zero = me.emit(out, it, Expr::Const(Const::I32(0)));
                let under = me.cmp(out, BinOp::Lt, x, zero);
                let x = me.select(out, under, zero, x);
                return Ok(me.emit(out, u32t, Expr::Convert(x, Scalar::U32)));
            }
            Ok(x)
        };
        let first = held(self, out, il)?;
        let last = held(self, out, ih)?;
        // The element at `i`: its place's `lo` and `hi`, the index replaced (by a value of the
        // new function, which `places` can't check).
        let element = |me: &mut Self, out: &mut Block, i: ValueId| -> R<Iv> {
            let mut q = p.clone();
            q.path[at] = Proj::Index(i);
            let (lp, hp) = me.split(&q)?;
            Ok((me.emit(out, ty, Expr::Load(lp)), me.emit(out, ty, Expr::Load(hp))))
        };
        let k = self.nf.new_local("index", u32t);
        let lo = self.nf.new_local("hull_lo", ty);
        let hi = self.nf.new_local("hull_hi", ty);
        out.push(Stmt::Store(Place::local(k), first));
        let e = element(self, out, first)?;
        out.push(Stmt::Store(Place::local(lo), e.0));
        out.push(Stmt::Store(Place::local(hi), e.1));
        let mut body = Vec::new();
        let i = self.emit(&mut body, u32t, Expr::Load(Place::local(k)));
        let one = self.emit(&mut body, u32t, Expr::Const(Const::U32(1)));
        let next = self.emit(&mut body, u32t, Expr::Binary(BinOp::Add, i, one));
        let past = self.cmp(&mut body, BinOp::Gt, next, last);
        body.push(Stmt::If { cond: past, then: vec![Stmt::Break], else_: Vec::new() });
        body.push(Stmt::Store(Place::local(k), next));
        let e = element(self, &mut body, next)?;
        let acc = (
            self.emit(&mut body, ty, Expr::Load(Place::local(lo))),
            self.emit(&mut body, ty, Expr::Load(Place::local(hi))),
        );
        let joined = self.hull(&mut body, ty, acc, e)?;
        body.push(Stmt::Store(Place::local(lo), joined.0));
        body.push(Stmt::Store(Place::local(hi), joined.1));
        out.push(Stmt::Loop { body, continuing: Vec::new() });
        Ok((
            self.emit(out, ty, Expr::Load(Place::local(lo))),
            self.emit(out, ty, Expr::Load(Place::local(hi))),
        ))
    }

    /// The hull of two ranges of type `ty`.
    fn hull(&mut self, out: &mut Block, ty: TypeId, a: Iv, b: Iv) -> R<Iv> {
        let lo = self.leaves(out, ty, &[a.0, b.0], &mut |s, out, lt, v| {
            Ok(s.extreme(out, lt, v[0], v[1], false))
        })?;
        let hi = self.leaves(out, ty, &[a.1, b.1], &mut |s, out, lt, v| {
            Ok(s.extreme(out, lt, v[0], v[1], true))
        })?;
        Ok((lo, hi))
    }

    fn root_ty(&self, r: &PlaceRoot) -> R<TypeId> {
        match r {
            PlaceRoot::Local(l) => Ok(self.nf.locals[l.index()].ty),
            PlaceRoot::Param(i) => Ok(self.nf.params[*i as usize].ty),
            PlaceRoot::Resource(_) | PlaceRoot::Ptr(_) | PlaceRoot::Data(_) => {
                Err(Error::internal("the range of a resource, pointer or constant as a whole"))
            }
        }
    }

    fn load_root(&mut self, out: &mut Block, r: &PlaceRoot) -> R<Iv> {
        let ty = self.root_ty(r)?;
        let h = self.hi_root(r)?;
        let lo = self.emit(out, ty, Expr::Load(Place::root(r.clone())));
        let hi = self.emit(out, ty, Expr::Load(Place::root(h)));
        Ok((lo, hi))
    }

    fn store_root(&mut self, out: &mut Block, r: &PlaceRoot, x: Iv) -> R<()> {
        let h = self.hi_root(r)?;
        out.push(Stmt::Store(Place::root(r.clone()), x.0));
        out.push(Stmt::Store(Place::root(h), x.1));
        Ok(())
    }

    /// The active locals and parameters a block may write, through a projection too.
    fn written(&self, b: &Block, into: &mut Vec<PlaceRoot>) {
        let active = |r: &PlaceRoot| match r {
            PlaceRoot::Local(_) | PlaceRoot::Param(_) => self.act.root(r),
            _ => false,
        };
        let mut add = |r: &PlaceRoot| {
            let mut roots = vec![r.clone()];
            if let PlaceRoot::Ptr(v) = r {
                roots.clear();
                if self.ptrs.roots(*v, &mut roots).is_none() {
                    // Anywhere.
                    let n = (self.act.locals.len(), self.act.params.len());
                    roots.extend((0..n.0).map(|l| PlaceRoot::Local(LocalId(l as u32))));
                    roots.extend((0..n.1).map(|i| PlaceRoot::Param(i as u32)));
                }
            }
            for r in roots {
                if active(&r) && !into.contains(&r) {
                    into.push(r);
                }
            }
        };
        visit::walk(b, &mut |s| match s {
            Stmt::Store(p, _) => add(&p.root),
            Stmt::Let(_, Expr::Call(g, args)) | Stmt::Eval(Expr::Call(g, args)) => {
                for (a, p) in args.iter().zip(&self.m.functions[g.index()].params) {
                    if let (Arg::Place(pl), true) = (a, p.mutable) {
                        add(&pl.root);
                    }
                }
            }
            _ => {}
        });
    }

    // ---- statements --------------------------------------------------------------------------

    fn block_into(&mut self, b: &Block, out: &mut Block) -> R<()> {
        for (i, s) in b.iter().enumerate() {
            if let Stmt::Let(v, Expr::Builtin(Builtin::Floor, args)) = s
                && !self.splitting
                && self.act.value(*v)
                && self.splittable(args[0])
            {
                let mut rest = b[i + 1..].to_vec();
                let tail = matches!(rest.last(), Some(Stmt::Return(_))).then(|| rest.pop());
                // The cases run in a loop of their own, so a `break` or `continue` of the
                // loop around this block can't be in them.
                if !leaves_loop(&rest) {
                    self.stmt(s, out)?;
                    let tail = tail.as_ref().and_then(|t| t.as_ref());
                    self.split_floor(*v, args[0], &rest, tail, out)?;
                    if let Some(t) = tail {
                        self.stmt(t, out)?;
                    }
                    return Ok(());
                }
            }
            self.stmt(s, out)?;
        }
        Ok(())
    }

    /// Whether `x` is an `f32` or a float vector: what `split_floor` handles.
    fn splittable(&self, x: ValueId) -> bool {
        let ty = self.ty(x);
        matches!(
            self.m.types.get(ty),
            TypeDef::Scalar(Scalar::F32) | TypeDef::Vector(Scalar::F32, _)
        )
    }

    /// `v = floor(x)`, then `rest` (to the end of the block, but for a final `return`), derived
    /// once and run for each case of which integer each component's floor is, and joined. Where a
    /// component's range crosses exactly one integer plane, its floor isn't one integer, and
    /// what's computed from it (value noise's hashed lattice values) would get its whole range
    /// (spike 13); in each case it's exact, and `x - floor(x)` is as narrow as the box. A
    /// component that crosses no plane, or more than one, isn't split. Up to 2³ cases; each
    /// writes the same roots, and the join is their hull, of the cases whose components all
    /// cross where they're split. `tail`, the block's `return`, may return a value `rest`
    /// computes: that's joined too.
    fn split_floor(
        &mut self,
        v: ValueId,
        x: ValueId,
        rest: &Block,
        tail: Option<&Stmt>,
        out: &mut Block,
    ) -> R<()> {
        let ty = self.ty(x);
        let n = match *self.m.types.get(ty) {
            TypeDef::Vector(_, n) => n as u32,
            _ => 1,
        };
        let f32t = self.m.types.f32();
        let bt = self.bool_ty();
        let (xl, xh) = self.range(x);
        let comp = |me: &mut Self, out: &mut Block, w: ValueId, c: u32| {
            if n == 1 { w } else { me.emit(out, f32t, Expr::Extract(w, c)) }
        };
        // Each component's floors at its ends, and whether they're one integer apart.
        let mut fl = Vec::new();
        for c in 0..n {
            let (l, h) = (comp(self, out, xl, c), comp(self, out, xh, c));
            let fl_lo = self.builtin(out, Builtin::Floor, vec![l], f32t);
            let fl_hi = self.builtin(out, Builtin::Floor, vec![h], f32t);
            let one = self.fc(out, f32t, 1.0);
            let next = self.bin(out, BinOp::Add, fl_lo, one, f32t);
            let two = self.cmp(out, BinOp::Eq, fl_hi, next);
            fl.push((fl_lo, fl_hi, next, two));
        }
        let mut roots = Vec::new();
        self.written(rest, &mut roots);
        let mut before = Vec::new();
        for r in &roots {
            before.push(self.load_root(out, r)?);
        }
        let returned = match tail {
            Some(Stmt::Return(Some(r))) if self.act.value(*r) => Some(*r),
            _ => None,
        };
        let old_floor = self.iv.get(&v).copied();
        // The cases run in a loop, derived once: each sets the floor, skips itself if a
        // component it splits doesn't cross, and joins what it gives into accumulators.
        let u32t = self.m.types.u32();
        let case = self.nf.new_local("case", u32t);
        let first = self.nf.new_local("first_case", bt);
        let zero = self.emit(out, u32t, Expr::Const(Const::U32(0)));
        out.push(Stmt::Store(Place::local(case), zero));
        let yes = self.emit(out, bt, Expr::Const(Const::Bool(true)));
        out.push(Stmt::Store(Place::local(first), yes));
        let mut accs = Vec::new();
        for r in &roots {
            let ty = self.root_ty(r)?;
            accs.push((self.nf.new_local("acc_lo", ty), self.nf.new_local("acc_hi", ty), ty));
        }
        let ret_acc = match returned {
            Some(r) => {
                let t = self.ty(r);
                let ty = range_ty(&mut self.m.types, t);
                Some((self.nf.new_local("ret_lo", ty), self.nf.new_local("ret_hi", ty), ty))
            }
            None => None,
        };
        let mut body = Vec::new();
        let i = self.emit(&mut body, u32t, Expr::Load(Place::local(case)));
        let cases = self.emit(&mut body, u32t, Expr::Const(Const::U32(1 << n)));
        let done = self.cmp(&mut body, BinOp::Eq, i, cases);
        body.push(Stmt::If { cond: done, then: vec![Stmt::Break], else_: Vec::new() });
        // This case's floor: per component, its low integer (bit 0) or the next one (bit 1);
        // where the component isn't split, its whole range, and bit 1 doesn't hold.
        let (mut los, mut his) = (Vec::new(), Vec::new());
        let mut holds = self.emit(&mut body, bt, Expr::Const(Const::Bool(true)));
        let one_u = self.emit(&mut body, u32t, Expr::Const(Const::U32(1)));
        for (c, &(fl_lo, fl_hi, next, two)) in fl.iter().enumerate() {
            let cc = self.emit(&mut body, u32t, Expr::Const(Const::U32(c as u32)));
            let shifted = self.emit(&mut body, u32t, Expr::Binary(BinOp::Shr, i, cc));
            let b = self.emit(&mut body, u32t, Expr::Binary(BinOp::BitAnd, shifted, one_u));
            let bit = self.cmp(&mut body, BinOp::Eq, b, one_u);
            let whole = self.select(&mut body, two, fl_lo, fl_hi);
            los.push(self.select(&mut body, bit, next, fl_lo));
            his.push(self.select(&mut body, bit, next, whole));
            let nb = self.not(&mut body, bit);
            let ok = self.bin(&mut body, BinOp::Or, nb, two, bt);
            holds = self.bin(&mut body, BinOp::And, holds, ok, bt);
        }
        let floor = if n == 1 {
            (los[0], his[0])
        } else {
            (
                self.emit(&mut body, ty, Expr::Construct(ty, los)),
                self.emit(&mut body, ty, Expr::Construct(ty, his)),
            )
        };
        self.iv.insert(v, floor);
        let mut each = Vec::new();
        for (r, b) in roots.iter().zip(&before) {
            self.store_root(&mut each, r, *b)?;
        }
        self.splitting = true;
        self.speculative += 1;
        self.block_into(rest, &mut each)?;
        self.speculative -= 1;
        self.splitting = false;
        // Join: the first case's result as it is, each later one's hull with the accumulator.
        let mut got = Vec::new();
        for r in &roots {
            got.push(self.load_root(&mut each, r)?);
        }
        let mut ends: Vec<(LocalId, LocalId, TypeId, Iv)> =
            accs.iter().zip(got).map(|(&(l, h, t), g)| (l, h, t, g)).collect();
        if let (Some((l, h, t)), Some(r)) = (ret_acc, returned) {
            let g = self.lifted(&mut each, r);
            ends.push((l, h, t, g));
        }
        let no = self.emit(&mut each, bt, Expr::Const(Const::Bool(false)));
        let yes = self.emit(&mut each, bt, Expr::Const(Const::Bool(true)));
        let is_first = self.emit(&mut each, bt, Expr::Load(Place::local(first)));
        let mut first_stores = Vec::new();
        let mut later_stores = Vec::new();
        for (l, h, t, g) in ends {
            first_stores.push(Stmt::Store(Place::local(l), g.0));
            first_stores.push(Stmt::Store(Place::local(h), g.1));
            let acc = (
                self.emit(&mut later_stores, t, Expr::Load(Place::local(l))),
                self.emit(&mut later_stores, t, Expr::Load(Place::local(h))),
            );
            let j = self.merge(&mut later_stores, t, (no, yes), g, acc)?;
            later_stores.push(Stmt::Store(Place::local(l), j.0));
            later_stores.push(Stmt::Store(Place::local(h), j.1));
        }
        each.push(Stmt::If { cond: is_first, then: first_stores, else_: later_stores });
        each.push(Stmt::Store(Place::local(first), no));
        body.push(Stmt::If { cond: holds, then: each, else_: Vec::new() });
        let mut continuing = Vec::new();
        let i2 = self.emit(&mut continuing, u32t, Expr::Load(Place::local(case)));
        let one2 = self.emit(&mut continuing, u32t, Expr::Const(Const::U32(1)));
        let i3 = self.emit(&mut continuing, u32t, Expr::Binary(BinOp::Add, i2, one2));
        continuing.push(Stmt::Store(Place::local(case), i3));
        out.push(Stmt::Loop { body, continuing });
        match old_floor {
            Some(o) => self.iv.insert(v, o),
            None => self.iv.remove(&v),
        };
        for (r, &(l, h, t)) in roots.iter().zip(&accs) {
            let j = (
                self.emit(out, t, Expr::Load(Place::local(l))),
                self.emit(out, t, Expr::Load(Place::local(h))),
            );
            self.store_root(out, r, j)?;
        }
        if let (Some(r), Some((l, h, t))) = (returned, ret_acc) {
            let j = (
                self.emit(out, t, Expr::Load(Place::local(l))),
                self.emit(out, t, Expr::Load(Place::local(h))),
            );
            self.iv.insert(r, j);
        }
        Ok(())
    }

    fn block(&mut self, b: &Block) -> R<Block> {
        let mut out = Vec::new();
        self.block_into(b, &mut out)?;
        Ok(out)
    }

    fn stmt(&mut self, s: &Stmt, out: &mut Block) -> R<()> {
        self.track.before(s);
        let r = self.stmt_inner(s, out);
        self.track.after(s, r)
    }

    fn let_(&mut self, s: &Stmt, v: ValueId, e: &Expr, out: &mut Block) -> R<()> {
        if let Expr::Call(g, args) = e {
            return self.call_fn(Some(v), *g, args, out);
        }
        if self.act.value(v) {
            let r = self.expr(v, e, out)?;
            self.iv.insert(v, r);
            self.defs.insert(v, e.clone());
        } else if let Expr::Extract(x, i) = e
            && self.act.value(*x)
        {
            // An inactive field of a struct that's active in others: the same in both bounds.
            let (lo, _) = self.range(*x);
            out.push(Stmt::Let(v, Expr::Extract(lo, *i)));
        } else {
            out.push(s.clone());
        }
        Ok(())
    }

    fn stmt_inner(&mut self, s: &Stmt, out: &mut Block) -> R<()> {
        match s {
            Stmt::Eval(Expr::Call(g, args)) => self.call_fn(None, *g, args, out),
            Stmt::Let(v, e) => self.let_(s, *v, e, out),
            Stmt::Store(p, v) => {
                if self.act.place(self.m, self.f, p) {
                    let (lp, hp) = self.places(p)?;
                    let (lo, hi) = self.lifted(out, *v);
                    out.push(Stmt::Store(lp, lo));
                    out.push(Stmt::Store(hp, hi));
                } else if self.partly_active(p) {
                    // An inactive part of a range: both bounds hold it.
                    let (lp, hp) = self.places(p)?;
                    out.push(Stmt::Store(lp, *v));
                    out.push(Stmt::Store(hp, *v));
                } else {
                    out.push(s.clone());
                }
                Ok(())
            }
            Stmt::If { cond, then, else_ } if self.act.value(*cond) => {
                // Either side may run: run both, one after the other, from the same state, and
                // join what they wrote.
                let c = self.range(*cond);
                let mut roots = Vec::new();
                self.written(then, &mut roots);
                self.written(else_, &mut roots);
                let mut before = Vec::new();
                for r in &roots {
                    before.push(self.load_root(out, r)?);
                }
                // Each side knows its guard: `a < b` narrows `a` and `b`, and what they were
                // computed from, while that side is derived.
                self.speculative += 1;
                self.branch_writes = roots.clone();
                let saved = self.narrow_by(out, *cond, true)?;
                self.block_into(then, out)?;
                self.restore(out, saved)?;
                let mut after_then = Vec::new();
                for (r, b) in roots.iter().zip(&before) {
                    after_then.push(self.load_root(out, r)?);
                    self.store_root(out, r, *b)?;
                }
                self.branch_writes = roots.clone();
                let saved = self.narrow_by(out, *cond, false)?;
                self.block_into(else_, out)?;
                self.restore(out, saved)?;
                self.speculative -= 1;
                for (r, t) in roots.iter().zip(after_then) {
                    let e = self.load_root(out, r)?;
                    let ty = self.root_ty(r)?;
                    let j = self.merge(out, ty, c, t, e)?;
                    self.store_root(out, r, j)?;
                }
                Ok(())
            }
            Stmt::Return(Some(v)) if self.returns => {
                let (lo, hi) = self.lifted(out, *v);
                let rt = self.nf.ret.ok_or_else(|| Error::internal("no range type"))?;
                let r = self.emit(out, rt, Expr::Construct(rt, vec![lo, hi]));
                out.push(Stmt::Return(Some(r)));
                Ok(())
            }
            Stmt::Trap if self.speculative > 0 => Ok(()),
            other => pass_through(other, out, &mut |b| self.block(b)),
        }
    }

    fn call_fn(&mut self, v: Option<ValueId>, g: FuncId, args: &[Arg], out: &mut Block) -> R<()> {
        let mask: Vec<Bits> = args.iter().map(|a| arg_bits(self.m, self.f, &self.act, a)).collect();
        // A `mut` place that's an inactive part of a range: the call writes its `lo`, and the
        // `hi` copies it after.
        let mut sync = Vec::new();
        for ((a, bits), p) in args.iter().zip(&mask).zip(&self.m.functions[g.index()].params) {
            if let (Arg::Place(pl), 0, true) = (a, bits, p.mutable)
                && self.partly_active(pl)
            {
                sync.push(pl.clone());
            }
        }
        if mask.iter().all(|b| *b == 0) {
            out.push(let_or_eval(v, Expr::Call(g, args.to_vec())));
            return self.sync_hi(out, sync);
        }
        let dg = derive(self.m, self.cache, g, mask.clone(), self.target, self.speculative > 0)?;
        let returns = returns_active(self.m, self.cache, g, mask.clone(), Mode::Interval) != 0;
        let mut new_args = Vec::new();
        let mut his = Vec::new();
        for (a, active) in args.iter().zip(&mask) {
            match (a, *active != 0) {
                (_, false) => new_args.push(a.clone()),
                (Arg::Value(x), true) => {
                    let (lo, hi) = self.range(*x);
                    new_args.push(Arg::Value(lo));
                    his.push(Arg::Value(hi));
                }
                (Arg::Place(p), true) => {
                    let (lp, hp) = self.places(p)?;
                    new_args.push(Arg::Place(lp));
                    his.push(Arg::Place(hp));
                }
            }
        }
        new_args.extend(his);
        match (v, returns) {
            (Some(v), true) => {
                let rt = self.m.functions[dg.index()]
                    .ret
                    .ok_or_else(|| Error::internal("no range type"))?;
                let r = self.emit(out, rt, Expr::Call(dg, new_args));
                let ty = self.ty(v);
                let ty = range_ty(&mut self.m.types, ty);
                let lo = self.emit(out, ty, Expr::Extract(r, 0));
                let hi = self.emit(out, ty, Expr::Extract(r, 1));
                self.iv.insert(v, (lo, hi));
            }
            (v, _) => out.push(let_or_eval(v, Expr::Call(dg, new_args))),
        }
        self.sync_hi(out, sync)
    }

    /// Whether `p` is an inactive part of an active local or parameter.
    fn partly_active(&self, p: &Place) -> bool {
        matches!(p.root, PlaceRoot::Local(_) | PlaceRoot::Param(_))
            && self.act.root(&p.root)
            && !self.act.place(self.m, self.f, p)
    }

    /// Copies each place's `lo` to its `hi`.
    fn sync_hi(&mut self, out: &mut Block, places: Vec<Place>) -> R<()> {
        for p in places {
            let ty = place_type(self.m, &self.nf, &p)?;
            let (lp, hp) = self.places(&p)?;
            let x = self.emit(out, ty, Expr::Load(lp));
            out.push(Stmt::Store(hp, x));
        }
        Ok(())
    }

    // ---- narrowing by a branch's guard ---------------------------------------------------------

    /// The orderings `cond` implies between floats where it's `holds`: each `(a, b, strict)`
    /// says `a ≤ b` (`a < b` when `strict`). Through `!`, `&&` where it holds and `||` where it
    /// doesn't. (A NaN breaks the negated ones, but a NaN has no range here anyway.)
    fn facts(
        &self,
        cond: ValueId,
        holds: bool,
        out: &mut Vec<(ValueId, ValueId, bool)>,
        depth: u32,
    ) {
        if depth > 4 {
            return;
        }
        let Some(e) = self.defs.get(&cond) else { return };
        match e.clone() {
            Expr::Unary(UnOp::Not, c) => self.facts(c, !holds, out, depth + 1),
            Expr::Binary(BinOp::And, a, b) if holds => {
                self.facts(a, true, out, depth + 1);
                self.facts(b, true, out, depth + 1);
            }
            Expr::Binary(BinOp::Or, a, b) if !holds => {
                self.facts(a, false, out, depth + 1);
                self.facts(b, false, out, depth + 1);
            }
            Expr::Binary(op @ (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge), a, b) => {
                let float = |v: ValueId| {
                    matches!(self.m.types.as_scalar(self.ty(v)), Some(Scalar::F32 | Scalar::F64))
                };
                if !float(a) || !float(b) {
                    return;
                }
                out.push(match (op, holds) {
                    (BinOp::Lt, true) => (a, b, true),
                    (BinOp::Le, true) => (a, b, false),
                    (BinOp::Gt, true) => (b, a, true),
                    (BinOp::Ge, true) => (b, a, false),
                    (BinOp::Lt, false) => (b, a, false),
                    (BinOp::Le, false) => (b, a, true),
                    (BinOp::Gt, false) => (a, b, false),
                    _ => (a, b, true),
                });
            }
            _ => {}
        }
    }

    /// Narrows the ranges `cond`'s facts bound, for the side where it's `holds`: what to give
    /// back to `restore` after that side.
    fn narrow_by(&mut self, out: &mut Block, cond: ValueId, holds: bool) -> R<Vec<Saved>> {
        let mut facts = Vec::new();
        self.facts(cond, holds, &mut facts, 0);
        let mut saved = Vec::new();
        for (a, b, strict) in facts {
            let ty = self.ty(a);
            let (al, _) = self.range(a);
            let (_, bh) = self.range(b);
            // a ≤ b.hi, and b ≥ a.lo; strictly, a bound of 0 moves off it.
            let (ahi, blo) = if strict {
                (self.off_zero(out, bh, ty, false), self.off_zero(out, al, ty, true))
            } else {
                (bh, al)
            };
            self.narrow(out, a, None, Some(ahi), 0, &mut saved)?;
            self.narrow(out, b, Some(blo), None, 0, &mut saved)?;
        }
        Ok(saved)
    }

    /// `x`, or the least float above (`up`) or below 0 if it's 0: a strict bound on that side.
    fn off_zero(&mut self, out: &mut Block, x: ValueId, ty: TypeId, up: bool) -> ValueId {
        let t = self.tiny(ty);
        let tiny = self.fc(out, ty, if up { t } else { -t });
        let zero = self.fc(out, ty, 0.0);
        let is0 = self.cmp(out, BinOp::Eq, x, zero);
        self.select(out, is0, tiny, x)
    }

    fn restore(&mut self, out: &mut Block, saved: Vec<Saved>) -> R<()> {
        for s in saved.into_iter().rev() {
            match s {
                Saved::Value(v, Some(r)) => {
                    self.iv.insert(v, r);
                }
                Saved::Value(v, None) => {
                    self.iv.remove(&v);
                }
                Saved::Local(l, r) => self.store_root(out, &PlaceRoot::Local(l), r)?,
            }
        }
        Ok(())
    }

    /// Narrows active local `l`, which the branch doesn't write, to `[lo, hi]` (component
    /// `comp` of a vector's) while the branch is derived, then the value stored in it, if it's
    /// a `let` binding's.
    fn narrow_local(
        &mut self,
        out: &mut Block,
        l: LocalId,
        comp: Option<u32>,
        (lo, hi): Iv,
        depth: u32,
        saved: &mut Vec<Saved>,
    ) -> R<()> {
        let root = PlaceRoot::Local(l);
        if !self.act.local(l) || self.branch_writes.contains(&root) {
            return Ok(());
        }
        let ty = self.root_ty(&root)?;
        let old = self.load_root(out, &root)?;
        let new = match comp {
            None => (
                self.builtin(out, Builtin::Max, vec![old.0, lo], ty),
                self.builtin(out, Builtin::Min, vec![old.1, hi], ty),
            ),
            Some(i) => {
                let TypeDef::Vector(_, n) = *self.m.types.get(ty) else { return Ok(()) };
                let st = self.m.types.f32();
                let mut ends = [old.0, old.1];
                for (k, end) in ends.iter_mut().enumerate() {
                    let parts: Vec<ValueId> = (0..n as u32)
                        .map(|j| {
                            let c = self.emit(out, st, Expr::Extract(*end, j));
                            match (j == i, k) {
                                (true, 0) => self.builtin(out, Builtin::Max, vec![c, lo], st),
                                (true, _) => self.builtin(out, Builtin::Min, vec![c, hi], st),
                                _ => c,
                            }
                        })
                        .collect();
                    *end = self.emit(out, ty, Expr::Construct(ty, parts));
                }
                (ends[0], ends[1])
            }
        };
        saved.push(Saved::Local(l, old));
        self.store_root(out, &root, new)?;
        if let (None, Some(src)) = (comp, self.local_src[l.index()]) {
            self.narrow(out, src, Some(lo), Some(hi), depth + 1, saved)?;
        }
        Ok(())
    }

    /// Narrows active value `v`'s range to at least `lo` and at most `hi`, then what it was
    /// computed from, where that's exact enough to follow: `max`, `min`, a vector's component,
    /// `+`, `-` and negation. Each override goes on `saved`.
    fn narrow(
        &mut self,
        out: &mut Block,
        v: ValueId,
        lo: Option<ValueId>,
        hi: Option<ValueId>,
        depth: u32,
        saved: &mut Vec<Saved>,
    ) -> R<()> {
        if !self.act.value(v) {
            return Ok(());
        }
        let ty = self.ty(v);
        if !matches!(self.m.types.as_scalar(ty), Some(Scalar::F32 | Scalar::F64)) {
            return Ok(());
        }
        let (l, h) = self.range(v);
        let l2 = match lo {
            Some(x) => self.builtin(out, Builtin::Max, vec![l, x], ty),
            None => l,
        };
        let h2 = match hi {
            Some(x) => self.builtin(out, Builtin::Min, vec![h, x], ty),
            None => h,
        };
        saved.push(Saved::Value(v, self.iv.get(&v).copied()));
        self.iv.insert(v, (l2, h2));
        if depth >= 8 {
            return Ok(());
        }
        let Some(e) = self.defs.get(&v).cloned() else { return Ok(()) };
        match e {
            Expr::Load(Place { root: PlaceRoot::Local(l), path }) => match path[..] {
                [] => self.narrow_local(out, l, None, (l2, h2), depth, saved)?,
                [Proj::Comp(i)] => {
                    self.narrow_local(out, l, Some(u32::from(i)), (l2, h2), depth, saved)?
                }
                _ => {}
            },
            Expr::Builtin(Builtin::Max, args) if args.len() == 2 && self.ty(args[0]) == ty => {
                // Both are at most the max; one is at least its low end where the other can't be.
                let (p, q) = (args[0], args[1]);
                let ((pl, ph), (ql, qh)) = (self.range(p), self.range(q));
                let q_low = self.cmp(out, BinOp::Lt, ph, l2);
                let q_lo = self.select(out, q_low, l2, ql);
                let p_low = self.cmp(out, BinOp::Lt, qh, l2);
                let p_lo = self.select(out, p_low, l2, pl);
                self.narrow(out, p, Some(p_lo), Some(h2), depth + 1, saved)?;
                self.narrow(out, q, Some(q_lo), Some(h2), depth + 1, saved)?;
            }
            Expr::Builtin(Builtin::Min, args) if args.len() == 2 && self.ty(args[0]) == ty => {
                let (p, q) = (args[0], args[1]);
                let ((pl, ph), (ql, qh)) = (self.range(p), self.range(q));
                let q_high = self.cmp(out, BinOp::Gt, pl, h2);
                let q_hi = self.select(out, q_high, h2, qh);
                let p_high = self.cmp(out, BinOp::Gt, ql, h2);
                let p_hi = self.select(out, p_high, h2, ph);
                self.narrow(out, p, Some(l2), Some(p_hi), depth + 1, saved)?;
                self.narrow(out, q, Some(l2), Some(q_hi), depth + 1, saved)?;
            }
            Expr::Extract(x, i) if self.act.value(x) => {
                let xt = self.ty(x);
                let TypeDef::Vector(_, n) = *self.m.types.get(xt) else { return Ok(()) };
                let (xl, xh) = self.range(x);
                let mut ends = [xl, xh];
                for (k, end) in ends.iter_mut().enumerate() {
                    let parts: Vec<ValueId> = (0..n as u32)
                        .map(|j| match (j == i, k) {
                            (true, 0) => l2,
                            (true, _) => h2,
                            _ => self.emit(out, ty, Expr::Extract(*end, j)),
                        })
                        .collect();
                    *end = self.emit(out, xt, Expr::Construct(xt, parts));
                }
                saved.push(Saved::Value(x, self.iv.get(&x).copied()));
                self.iv.insert(x, (ends[0], ends[1]));
                // A component of a vector `let` binding: the binding too.
                if let Some(Expr::Load(Place { root: PlaceRoot::Local(l), path })) =
                    self.defs.get(&x).cloned()
                    && path.is_empty()
                {
                    self.narrow_local(out, l, Some(i), (l2, h2), depth, saved)?;
                }
            }
            Expr::Unary(UnOp::Neg, p) => {
                let nl = self.emit(out, ty, Expr::Unary(UnOp::Neg, h2));
                let nh = self.emit(out, ty, Expr::Unary(UnOp::Neg, l2));
                self.narrow(out, p, Some(nl), Some(nh), depth + 1, saved)?;
            }
            Expr::Binary(op @ (BinOp::Add | BinOp::Sub), p, q)
                if self.ty(p) == ty && self.ty(q) == ty =>
            {
                // v = fl(p ± q) is within an ulp of p ± q: so p is within v ∓ q, an ulp wider.
                let ((pl, ph), (ql, qh)) = (self.range(p), self.range(q));
                let (p_lo, p_hi, q_lo, q_hi) = if op == BinOp::Add {
                    (
                        self.bin(out, BinOp::Sub, l2, qh, ty),
                        self.bin(out, BinOp::Sub, h2, ql, ty),
                        self.bin(out, BinOp::Sub, l2, ph, ty),
                        self.bin(out, BinOp::Sub, h2, pl, ty),
                    )
                } else {
                    (
                        self.bin(out, BinOp::Add, l2, ql, ty),
                        self.bin(out, BinOp::Add, h2, qh, ty),
                        self.bin(out, BinOp::Sub, pl, h2, ty),
                        self.bin(out, BinOp::Sub, ph, l2, ty),
                    )
                };
                let pr = self.widen(out, (p_lo, p_hi), ty, 2.0, 0.0);
                let qr = self.widen(out, (q_lo, q_hi), ty, 2.0, 0.0);
                self.narrow(out, p, Some(pr.0), Some(pr.1), depth + 1, saved)?;
                self.narrow(out, q, Some(qr.0), Some(qr.1), depth + 1, saved)?;
            }
            _ => {}
        }
        Ok(())
    }

    // ---- expressions -------------------------------------------------------------------------

    /// The range of an active value `v = e`.
    fn expr(&mut self, v: ValueId, e: &Expr, out: &mut Block) -> R<Iv> {
        let ty = self.ty(v);
        let ty = range_ty(&mut self.m.types, ty);
        Ok(match e {
            Expr::Param(i) => {
                let h = self.hparam[*i as usize]
                    .ok_or_else(|| Error::internal("an inactive parameter's range"))?;
                (self.emit(out, ty, Expr::Param(*i)), self.emit(out, ty, Expr::Param(h)))
            }
            Expr::Load(p) if self.act.index_active(p) => self.load_any(out, p, ty)?,
            Expr::Load(p) => {
                let (lp, hp) = self.places(p)?;
                (self.emit(out, ty, Expr::Load(lp)), self.emit(out, ty, Expr::Load(hp)))
            }
            Expr::Unary(op, x) => self.unary(out, *op, *x, ty)?,
            Expr::Binary(op, a, b) => self.binary(out, *op, *a, *b, ty)?,
            Expr::Builtin(b, args) => self.builtin_range(out, *b, args, ty)?,
            Expr::ExtractDyn(x, i) if self.act.value(*i) => {
                // Any element: the hull of them all.
                let n = match self.m.types.get(self.ty(*x)) {
                    TypeDef::Array(_, n) => *n,
                    TypeDef::Vector(_, n) => *n as u32,
                    _ => return Err(Error::internal("a dynamic extract from a non-array")),
                };
                if !matches!(self.m.types.get(ty), TypeDef::Scalar(_) | TypeDef::Vector(..)) {
                    return Err(Error::not_derivable(
                        "an interval over an array of aggregates indexed by a value that \
                                depends on the input isn't supported",
                    ));
                }
                let (xl, xh) = self.range(*x);
                let u32t = self.m.types.u32();
                let mut acc: Option<Iv> = None;
                for k in 0..n {
                    let i = self.emit(out, u32t, Expr::Const(Const::U32(k)));
                    let l = self.emit(out, ty, Expr::ExtractDyn(xl, i));
                    let h = self.emit(out, ty, Expr::ExtractDyn(xh, i));
                    acc = Some(match acc {
                        None => (l, h),
                        Some((al, ah)) => (
                            self.extreme(out, ty, al, l, false),
                            self.extreme(out, ty, ah, h, true),
                        ),
                    });
                }
                acc.ok_or_else(|| Error::internal("an empty array"))?
            }
            Expr::Construct(..)
            | Expr::Variant(..)
            | Expr::Extract(..)
            | Expr::ExtractDyn(..)
            | Expr::Splat(..)
            | Expr::Swizzle(..) => self.structural(out, ty, e),
            Expr::Convert(x, s) => self.convert(out, *x, *s, ty)?,
            Expr::Bitcast(x, s) => {
                let s = *s;
                self.exact_or_full(out, ty, &[self.range(*x)], &mut |me, out, xs| {
                    me.emit(out, ty, Expr::Bitcast(xs[0], s))
                })
            }
            Expr::Select { cond, if_true, if_false } => {
                let (t, f) = (self.lifted(out, *if_true), self.lifted(out, *if_false));
                if self.act.value(*cond) {
                    let c = self.range(*cond);
                    self.merge(out, ty, c, t, f)?
                } else {
                    (self.select(out, *cond, t.0, f.0), self.select(out, *cond, t.1, f.1))
                }
            }
            Expr::Const(_) | Expr::Zero(_) | Expr::EntryInput(_) | Expr::ArrayLength(_) => {
                return Err(Error::internal("a constant marked active"));
            }
            Expr::Call(..) => unreachable!("calls are handled in `call_fn`"),
            Expr::Addr(p) => {
                let (lp, hp) = self.places(p)?;
                (self.emit(out, ty, Expr::Addr(lp)), self.emit(out, ty, Expr::Addr(hp)))
            }
            Expr::Run(_) => {
                return Err(Error::not_derivable(
                    "an interval can't go through a run (`[T]`) of values that depend on the input yet: pass the array itself (`[f32; N]`)",
                ));
            }
            Expr::Texture(..) | Expr::TextureStore(..) => {
                return Err(Error::not_derivable(
                    "an interval can't go through a texture read: its texels are data, not code",
                ));
            }
            Expr::Atomic(..) | Expr::Barrier | Expr::Discard => {
                return Err(Error::not_derivable(
                    "an interval can't go through an atomic or a barrier: derived code is pure",
                ));
            }
            Expr::Host(..) => {
                return Err(Error::not_derivable(
                    "an interval can't go through GPU work (a buffer, a dispatch or a draw)",
                ));
            }
            Expr::Mem(..) => {
                return Err(Error::not_derivable(
                    "an interval can't go through code that allocates or reads raw memory",
                ));
            }
        })
    }

    /// The range of an expression that moves parts about but computes nothing (a construct, a
    /// variant, an extract at an index that doesn't depend on the input, a splat or a swizzle):
    /// the same expression on the operands' `lo`s, and on their `hi`s.
    fn structural(&mut self, out: &mut Block, ty: TypeId, e: &Expr) -> Iv {
        if let &Expr::Variant(et, k, Some(payload)) = e {
            // Its range type's struct: the tag, and the payload in its variant's field.
            let TypeDef::Enum { variants, .. } = self.m.types.get(et).clone() else {
                return (payload, payload);
            };
            let u32t = self.m.types.u32();
            let p = self.range(payload);
            let mut ends = [p.0, p.1];
            for end in &mut ends {
                let tag = self.m.types.tag(et, k);
                let mut parts = vec![self.emit(out, u32t, Expr::Const(Const::U32(tag)))];
                for (j, (_, pt)) in variants.iter().enumerate() {
                    parts.push(match pt {
                        _ if j == k as usize => *end,
                        Some(pt) => {
                            let rt = range_ty(&mut self.m.types, *pt);
                            self.emit(out, rt, Expr::Zero(rt))
                        }
                        None => self.emit(out, u32t, Expr::Const(Const::U32(0))),
                    });
                }
                *end = self.emit(out, ty, Expr::Construct(ty, parts));
            }
            return (ends[0], ends[1]);
        }
        let mut ranges = HashMap::new();
        e.for_each_value(&mut |x| {
            ranges.entry(x).or_insert_with(|| self.lifted(out, x));
        });
        let mut ends = [e.clone(), e.clone()];
        for (i, end) in ends.iter_mut().enumerate() {
            end.for_each_value_mut(&mut |x| *x = if i == 0 { ranges[x].0 } else { ranges[x].1 });
            if let Expr::Construct(t, _) = end {
                *t = ty;
            }
        }
        let [lo, hi] = ends;
        (self.emit(out, ty, lo), self.emit(out, ty, hi))
    }

    /// For integers and bit patterns: exact when every operand is a single value, else the
    /// type's whole range. A float operand is single only when its bounds have the same bits
    /// (`-0.0` and `0.0` differ to a bitcast). `compute` gets the operands' `lo`s; on the CPU,
    /// where it could trap (`abs` of `i32::MIN`), it gets zeros when they aren't all single.
    fn exact_or_full(
        &mut self,
        out: &mut Block,
        ty: TypeId,
        xs: &[Iv],
        compute: &mut dyn FnMut(&mut Self, &mut Block, &[ValueId]) -> ValueId,
    ) -> Iv {
        let mut single: Option<ValueId> = None;
        for x in xs {
            let eq = match self.m.types.as_scalar(self.ty(x.0)) {
                Some(s @ (Scalar::F32 | Scalar::F64)) => {
                    let bits = if s == Scalar::F64 { Scalar::U64 } else { Scalar::U32 };
                    let bt = self.m.types.scalar(bits);
                    let l = self.emit(out, bt, Expr::Bitcast(x.0, bits));
                    let h = self.emit(out, bt, Expr::Bitcast(x.1, bits));
                    self.cmp(out, BinOp::Eq, l, h)
                }
                _ => self.cmp(out, BinOp::Eq, x.0, x.1),
            };
            single = Some(match single {
                None => eq,
                Some(s) => {
                    let bt = self.bool_ty();
                    self.bin(out, BinOp::And, s, eq, bt)
                }
            });
        }
        let mut los: Vec<ValueId> = xs.iter().map(|x| x.0).collect();
        if let Some(s) = single
            && !self.gpu()
        {
            for l in &mut los {
                let lt = self.ty(*l);
                let z = self.emit(out, lt, Expr::Zero(lt));
                *l = self.select(out, s, *l, z);
            }
        }
        let r = compute(self, out, &los);
        let (fl, fh) = self.full(out, ty);
        match single {
            Some(s) => (self.select(out, s, r, fl), self.select(out, s, r, fh)),
            None => (r, r),
        }
    }

    fn unary(&mut self, out: &mut Block, op: UnOp, x: ValueId, ty: TypeId) -> R<Iv> {
        let (lo, hi) = self.range(x);
        let s = self.elem(ty);
        Ok(match op {
            UnOp::Neg if !s.is_float() => {
                self.exact_or_full(out, ty, &[(lo, hi)], &mut |me, out, xs| {
                    let z = me.emit(out, ty, Expr::Zero(ty));
                    me.bin(out, BinOp::WrappingSub, z, xs[0], ty)
                })
            }
            // Float negation and bitwise not reverse the order exactly.
            UnOp::Neg | UnOp::Not => {
                (self.emit(out, ty, Expr::Unary(op, hi)), self.emit(out, ty, Expr::Unary(op, lo)))
            }
        })
    }

    fn binary(&mut self, out: &mut Block, op: BinOp, a: ValueId, b: ValueId, ty: TypeId) -> R<Iv> {
        let (ta, tb) = (self.ty(a), self.ty(b));
        if [ta, tb, ty].iter().any(|t| matches!(self.m.types.get(*t), TypeDef::Matrix(..))) {
            return Err(Error::not_derivable("an interval over matrix arithmetic isn't supported"));
        }
        let (ra, rb) = (self.range(a), self.range(b));
        let os = self.elem(ta);
        if op.is_comparison() {
            if self.m.types.is_vector(ta) {
                return Err(Error::internal("a vector comparison"));
            }
            return Ok(self.compare(out, op, ra, rb, os));
        }
        if os == Scalar::Bool {
            return Ok(match op {
                BinOp::And | BinOp::Or | BinOp::BitAnd | BinOp::BitOr => {
                    let o = if matches!(op, BinOp::And | BinOp::BitAnd) {
                        BinOp::And
                    } else {
                        BinOp::Or
                    };
                    (self.bin(out, o, ra.0, rb.0, ty), self.bin(out, o, ra.1, rb.1, ty))
                }
                _ => self.exact_or_full(out, ty, &[ra, rb], &mut |me, out, xs| {
                    me.bin(out, op, xs[0], xs[1], ty)
                }),
            });
        }
        if os.is_int() {
            let wrapping = match op {
                BinOp::Add | BinOp::WrappingAdd => Some(BinOp::WrappingAdd),
                BinOp::Sub | BinOp::WrappingSub => Some(BinOp::WrappingSub),
                BinOp::Mul | BinOp::WrappingMul => Some(BinOp::WrappingMul),
                BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => Some(op),
                BinOp::Shl | BinOp::Shr => Some(op),
                _ => None,
            };
            let Some(w) = wrapping else {
                // Division: the whole range (computing it could trap).
                return Ok(self.full(out, ty));
            };
            let bits = os.bits();
            return Ok(self.exact_or_full(out, ty, &[ra, rb], &mut |me, out, xs| {
                let mut amount = xs[1];
                if matches!(w, BinOp::Shl | BinOp::Shr) {
                    // An out-of-range shift traps on the CPU; mask it.
                    let st = me.ty(amount);
                    let mask = me.emit(out, st, Expr::Const(Const::U32(bits - 1)));
                    amount = me.bin(out, BinOp::BitAnd, amount, mask, st);
                }
                me.bin(out, w, xs[0], amount, ty)
            }));
        }
        // Floats.
        Ok(match op {
            BinOp::Add => {
                let r = (self.bin(out, op, ra.0, rb.0, ty), self.bin(out, op, ra.1, rb.1, ty));
                self.widen1(out, r, ty)
            }
            BinOp::Sub => {
                let r = (self.bin(out, op, ra.0, rb.1, ty), self.bin(out, op, ra.1, rb.0, ty));
                self.widen1(out, r, ty)
            }
            BinOp::Mul if self.track.same_value(a, b) => {
                let r = self.sqr(out, ra, ty);
                self.widen1(out, r, ty)
            }
            BinOp::Mul => {
                let r = self.mul(out, ra, rb, ty);
                self.widen1(out, r, ty)
            }
            BinOp::Div => self.per_comp(out, ty, &[ra, rb], Self::div)?,
            BinOp::Rem => self.per_comp(out, ty, &[ra, rb], Self::rem)?,
            _ => return Err(Error::internal(format!("{op:?} on floats"))),
        })
    }

    /// The four corner products' extremes (unwidened).
    fn mul(&mut self, out: &mut Block, a: Iv, b: Iv, ty: TypeId) -> Iv {
        self.corners(out, ty, a, b, |s, out, x, y| s.bin(out, BinOp::Mul, x, y, ty))
    }

    /// The extremes of `op` at the four corners of `a` × `b` (unwidened).
    fn corners(
        &mut self,
        out: &mut Block,
        ty: TypeId,
        a: Iv,
        b: Iv,
        mut op: impl FnMut(&mut Self, &mut Block, ValueId, ValueId) -> ValueId,
    ) -> Iv {
        let c = [
            op(self, out, a.0, b.0),
            op(self, out, a.0, b.1),
            op(self, out, a.1, b.0),
            op(self, out, a.1, b.1),
        ];
        self.extremes(out, &c, ty)
    }

    fn extremes(&mut self, out: &mut Block, p: &[ValueId], ty: TypeId) -> Iv {
        let mut lo = p[0];
        let mut hi = p[0];
        for &x in &p[1..] {
            lo = self.builtin(out, Builtin::Min, vec![lo, x], ty);
            hi = self.builtin(out, Builtin::Max, vec![hi, x], ty);
        }
        (lo, hi)
    }

    /// `x × x` (unwidened): `[0, ...]` when the range straddles zero.
    fn sqr(&mut self, out: &mut Block, x: Iv, ty: TypeId) -> Iv {
        let a = self.abs(out, x, ty);
        (self.bin(out, BinOp::Mul, a.0, a.0, ty), self.bin(out, BinOp::Mul, a.1, a.1, ty))
    }

    /// `|x|`, exactly: `[max(lo, -hi, 0), max(|lo|, |hi|)]`.
    fn abs(&mut self, out: &mut Block, x: Iv, ty: TypeId) -> Iv {
        let nh = self.emit(out, ty, Expr::Unary(UnOp::Neg, x.1));
        let m = self.builtin(out, Builtin::Max, vec![x.0, nh], ty);
        let z = self.fc(out, ty, 0.0);
        let lo = self.builtin(out, Builtin::Max, vec![m, z], ty);
        (lo, self.magnitude(out, x, ty))
    }

    /// The largest magnitude in a range: `max(|lo|, |hi|)`.
    fn magnitude(&mut self, out: &mut Block, x: Iv, ty: TypeId) -> ValueId {
        let l = self.builtin(out, Builtin::Abs, vec![x.0], ty);
        let h = self.builtin(out, Builtin::Abs, vec![x.1], ty);
        self.builtin(out, Builtin::Max, vec![l, h], ty)
    }

    /// Applies a bound rule for one scalar operation to each component of a vector (scalar
    /// operands are shared).
    fn per_comp(
        &mut self,
        out: &mut Block,
        ty: TypeId,
        args: &[Iv],
        mut rule: impl FnMut(&mut Self, &mut Block, TypeId, &[Iv]) -> R<Iv>,
    ) -> R<Iv> {
        let TypeDef::Vector(_, n) = *self.m.types.get(ty) else {
            return rule(self, out, ty, args);
        };
        let f32t = self.m.types.f32();
        let (mut los, mut his) = (Vec::new(), Vec::new());
        for c in 0..n as u32 {
            let mut comps = Vec::new();
            for a in args {
                comps.push(if self.m.types.is_vector(self.ty(a.0)) {
                    (
                        self.emit(out, f32t, Expr::Extract(a.0, c)),
                        self.emit(out, f32t, Expr::Extract(a.1, c)),
                    )
                } else {
                    *a
                });
            }
            let (l, h) = rule(self, out, f32t, &comps)?;
            los.push(l);
            his.push(h);
        }
        Ok((
            self.emit(out, ty, Expr::Construct(ty, los)),
            self.emit(out, ty, Expr::Construct(ty, his)),
        ))
    }

    fn div(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        let (a, b) = (x[0], x[1]);
        let r = self.corners(out, ty, a, b, |s, out, x, y| s.bin(out, BinOp::Div, x, y, ty));
        // WGSL: 2.5 ulp; WASM: correctly rounded.
        let ulps = if self.gpu() { 6.0 } else { 1.0 };
        let r = self.widen(out, r, ty, ulps, 0.0);
        // A divisor that could be zero: unbounded.
        let z = self.fc(out, ty, 0.0);
        let le = self.cmp(out, BinOp::Le, b.0, z);
        let ge = self.cmp(out, BinOp::Ge, b.1, z);
        let bt = self.bool_ty();
        let zero_in = self.bin(out, BinOp::And, le, ge, bt);
        let (fl, fh) = self.full(out, ty);
        Ok((self.select(out, zero_in, fl, r.0), self.select(out, zero_in, fh, r.1)))
    }

    /// `a - b trunc(a / b)`: the sign of `a`, smaller than `|b|`, and within rounding of that.
    /// A divisor that could be zero gives a NaN there (`a % 0`): unbounded, as `div`'s is.
    fn rem(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        let (a, b) = (x[0], x[1]);
        let bm = self.magnitude(out, b, ty);
        let nbm = self.emit(out, ty, Expr::Unary(UnOp::Neg, bm));
        let z = self.fc(out, ty, 0.0);
        let lo_any = self.builtin(out, Builtin::Max, vec![a.0, nbm], ty);
        let hi_any = self.builtin(out, Builtin::Min, vec![a.1, bm], ty);
        let nonneg = self.cmp(out, BinOp::Ge, a.0, z);
        let nonpos = self.cmp(out, BinOp::Le, a.1, z);
        let lo = self.select(out, nonneg, z, lo_any);
        let hi = self.select(out, nonpos, z, hi_any);
        // Rounding in `trunc(a / b)` can put the result just past either end, by about an ulp
        // of `a`.
        let am = self.magnitude(out, a, ty);
        let mag = self.bin(out, BinOp::Add, am, bm, ty);
        let ulps = if self.gpu() { 16.0 } else { 4.0 };
        let r = self.widen_mag(out, (lo, hi), ty, (mag, mag), ulps, 0.0);
        let le = self.cmp(out, BinOp::Le, b.0, z);
        let ge = self.cmp(out, BinOp::Ge, b.1, z);
        let bt = self.bool_ty();
        let zero_in = self.bin(out, BinOp::And, le, ge, bt);
        let (fl, fh) = self.full(out, ty);
        Ok((self.select(out, zero_in, fl, r.0), self.select(out, zero_in, fh, r.1)))
    }

    fn compare(&mut self, out: &mut Block, op: BinOp, a: Iv, b: Iv, s: Scalar) -> Iv {
        let ordered = s != Scalar::Bool;
        match op {
            BinOp::Lt | BinOp::Le => (self.cmp(out, op, a.1, b.0), self.cmp(out, op, a.0, b.1)),
            BinOp::Gt | BinOp::Ge => (self.cmp(out, op, a.0, b.1), self.cmp(out, op, a.1, b.0)),
            BinOp::Eq | BinOp::Ne => {
                let bt = self.bool_ty();
                let sa = self.cmp(out, BinOp::Eq, a.0, a.1);
                let sb = self.cmp(out, BinOp::Eq, b.0, b.1);
                let single = self.bin(out, BinOp::And, sa, sb, bt);
                let same = self.cmp(out, BinOp::Eq, a.0, b.0);
                let surely = self.bin(out, BinOp::And, single, same, bt);
                let maybe = if ordered {
                    let x = self.cmp(out, BinOp::Le, a.0, b.1);
                    let y = self.cmp(out, BinOp::Le, b.0, a.1);
                    self.bin(out, BinOp::And, x, y, bt)
                } else {
                    let differ = self.not(out, same);
                    let never = self.bin(out, BinOp::And, single, differ, bt);
                    self.not(out, never)
                };
                if op == BinOp::Eq {
                    (surely, maybe)
                } else {
                    (self.not(out, maybe), self.not(out, surely))
                }
            }
            _ => unreachable!("not a comparison"),
        }
    }

    fn convert(&mut self, out: &mut Block, x: ValueId, to: Scalar, ty: TypeId) -> R<Iv> {
        let r = self.range(x);
        let from = self.elem(self.ty(x));
        let conv =
            |me: &mut Self, out: &mut Block, v: ValueId| me.emit(out, ty, Expr::Convert(v, to));
        let monotone = (from.is_float() && to.is_float())
            || (!from.is_float() && to.is_float())
            || from == Scalar::Bool
            || (to.is_int()
                && from.is_int()
                && to.bits() >= from.bits()
                && (from.signed() == to.signed() || (!from.signed() && to.bits() > from.bits())));
        if monotone {
            return Ok((conv(self, out, r.0), conv(self, out, r.1)));
        }
        if from.is_float() && to.is_int() {
            // Truncation is monotone; clamp first so no bound is out of range (which traps on
            // the CPU). A point that far out traps anyway.
            let (lo, hi) = float_int_range(from, to);
            let xt = self.ty(x);
            let cl = self.fc(out, xt, lo);
            let ch = self.fc(out, xt, hi);
            let mut ends = Vec::new();
            for (v, nan_to) in [(r.0, cl), (r.1, ch)] {
                let ok = self.cmp(out, BinOp::Eq, v, v);
                let v = self.select(out, ok, v, nan_to);
                let v = self.builtin(out, Builtin::Clamp, vec![v, cl, ch], xt);
                ends.push(conv(self, out, v));
            }
            return Ok((ends[0], ends[1]));
        }
        Ok(self.exact_or_full(out, ty, &[r], &mut |me, out, xs| conv(me, out, xs[0])))
    }

    fn builtin_range(
        &mut self,
        out: &mut Block,
        b: Builtin,
        args: &[ValueId],
        ty: TypeId,
    ) -> R<Iv> {
        use Builtin as B;
        let r: Vec<Iv> = args.iter().map(|a| self.range(*a)).collect();
        let s = self.elem(ty);
        if s.is_int() {
            return Ok(match b {
                B::Min | B::Max | B::Clamp => self.monotone(out, b, &r, ty),
                _ => self.exact_or_full(out, ty, &r, &mut |me, out, xs| {
                    me.builtin(out, b, xs.to_vec(), ty)
                }),
            });
        }
        let gpu = self.gpu();
        Ok(match b {
            B::Floor | B::Ceil | B::Round | B::Trunc | B::Sign | B::Saturate => {
                self.monotone(out, b, &r, ty)
            }
            B::Min | B::Max | B::Clamp => self.monotone(out, b, &r, ty),
            B::Step => {
                // 1 where edge <= x: up in x, down in the edge.
                let lo = self.builtin(out, B::Step, vec![r[0].1, r[1].0], ty);
                let hi = self.builtin(out, B::Step, vec![r[0].0, r[1].1], ty);
                (lo, hi)
            }
            B::Abs => self.abs(out, r[0], ty),
            B::Sqrt => {
                let z = self.fc(out, ty, 0.0);
                let l = self.builtin(out, B::Max, vec![r[0].0, z], ty);
                let h = self.builtin(out, B::Max, vec![r[0].1, z], ty);
                let x = (
                    self.builtin(out, B::Sqrt, vec![l], ty),
                    self.builtin(out, B::Sqrt, vec![h], ty),
                );
                // WGSL: inherited from 1 / inverseSqrt (2 + 2.5 ulp).
                let ulps = if gpu { 10.0 } else { 1.0 };
                self.widen(out, x, ty, ulps, 0.0)
            }
            B::InverseSqrt => self.per_comp(out, ty, &r, Self::inverse_sqrt)?,
            B::Exp | B::Exp2 => {
                let x = self.monotone(out, b, &r, ty);
                if gpu {
                    let held = self.exp_held(out, r[0], ty);
                    self.exp_gpu_widen(out, x, held, ty)
                } else {
                    self.widen(out, x, ty, CPU_STD_ULPS, 0.0)
                }
            }
            // Increasing: the ends. The GPU's are "inherited" from `exp` (WGSL): their error is
            // relative where they're large, and near 0, where they're a difference of `exp`s
            // near 1, it's absolute, about an ulp of 1.
            B::Sinh | B::Tanh => {
                let x = self.monotone(out, b, &r, ty);
                match gpu {
                    true => self.widen(out, x, ty, HYPERBOLIC_GPU_ULPS, HYPERBOLIC_GPU_ABS),
                    false => self.widen(out, x, ty, CPU_STD_ULPS, 0.0),
                }
            }
            B::Cosh => {
                let x = self.per_comp(out, ty, &r, Self::cosh)?;
                self.widen(out, x, ty, if gpu { HYPERBOLIC_GPU_ULPS } else { CPU_STD_ULPS }, 0.0)
            }
            B::Log | B::Log2 => {
                self.per_comp(out, ty, &r, |s, out, ty, x| Ok(s.log_ends(out, b, x[0], ty)))?
            }
            B::Pow => self.per_comp(out, ty, &r, Self::pow)?,
            B::Sin => self.per_comp(out, ty, &r, Self::sin)?,
            B::Cos => self.per_comp(out, ty, &r, Self::cos)?,
            B::Tan => self.per_comp(out, ty, &r, Self::tan)?,
            B::Asin | B::Acos | B::Atan => {
                let one = self.fc(out, ty, 1.0);
                let neg = self.fc(out, ty, -1.0);
                let x = if b == B::Atan {
                    r[0]
                } else {
                    (
                        self.builtin(out, B::Clamp, vec![r[0].0, neg, one], ty),
                        self.builtin(out, B::Clamp, vec![r[0].1, neg, one], ty),
                    )
                };
                let fl = self.builtin(out, b, vec![x.0], ty);
                let fh = self.builtin(out, b, vec![x.1], ty);
                let x = if b == B::Acos { (fh, fl) } else { (fl, fh) };
                self.inverse_trig_widen(out, x, ty)
            }
            B::Atan2 => {
                let pi = std::f64::consts::PI;
                let x = (self.fc(out, ty, -pi), self.fc(out, ty, pi));
                self.inverse_trig_widen(out, x, ty)
            }
            B::Fract => self.per_comp(out, ty, &r, Self::fract)?,
            B::Mix => self.per_comp(out, ty, &r, Self::mix)?,
            B::Smoothstep => self.per_comp(out, ty, &r, Self::smoothstep)?,
            B::Length => {
                let at = self.ty(args[0]);
                self.length(out, r[0], at, ty)
            }
            B::Distance => {
                let at = self.ty(args[0]);
                let (a, bb) = (r[0], r[1]);
                let d = (
                    self.bin(out, BinOp::Sub, a.0, bb.1, at),
                    self.bin(out, BinOp::Sub, a.1, bb.0, at),
                );
                let d = self.widen1(out, d, at);
                self.length(out, d, at, ty)
            }
            B::Dot => {
                let at = self.ty(args[0]);
                let TypeDef::Vector(_, n) = *self.m.types.get(at) else {
                    return Err(Error::internal("a dot of non-vectors"));
                };
                // dot(v, v) is a sum of squares.
                let p = if self.track.same_value(args[0], args[1]) {
                    self.sqr(out, r[0], at)
                } else {
                    self.mul(out, r[0], r[1], at)
                };
                let lo = self.sum(out, p.0, n, ty);
                let hi = self.sum(out, p.1, n, ty);
                let m = self.magnitude(out, p, at);
                let mag = self.sum(out, m, n, ty);
                // n products and n - 1 sums, in whatever order the target picks.
                let ulps = if gpu { 4.0 * n as f64 } else { 2.0 * n as f64 };
                self.widen_mag(out, (lo, hi), ty, (mag, mag), ulps, 0.0)
            }
            B::Cross => {
                // a.yzx b.zxy - a.zxy b.yzx
                let sw = |me: &mut Self, out: &mut Block, x: Iv, c: [u8; 3]| -> Iv {
                    (
                        me.emit(out, ty, Expr::Swizzle(x.0, c.to_vec())),
                        me.emit(out, ty, Expr::Swizzle(x.1, c.to_vec())),
                    )
                };
                let (yzx, zxy) = ([1, 2, 0], [2, 0, 1]);
                let a1 = sw(self, out, r[0], yzx);
                let b1 = sw(self, out, r[1], zxy);
                let a2 = sw(self, out, r[0], zxy);
                let b2 = sw(self, out, r[1], yzx);
                let p = self.mul(out, a1, b1, ty);
                let p = self.widen1(out, p, ty);
                let q = self.mul(out, a2, b2, ty);
                let q = self.widen1(out, q, ty);
                let d = (
                    self.bin(out, BinOp::Sub, p.0, q.1, ty),
                    self.bin(out, BinOp::Sub, p.1, q.0, ty),
                );
                self.widen1(out, d, ty)
            }
            B::Normalize => self.normalize(out, r[0], ty)?,
            B::AllEqual => {
                let bt = self.bool_ty();
                let (a, bb) = (r[0], r[1]);
                let sa = self.builtin(out, B::AllEqual, vec![a.0, a.1], bt);
                let sb = self.builtin(out, B::AllEqual, vec![bb.0, bb.1], bt);
                let same = self.builtin(out, B::AllEqual, vec![a.0, bb.0], bt);
                let x = self.bin(out, BinOp::And, sa, sb, bt);
                let lo = self.bin(out, BinOp::And, x, same, bt);
                let hi = self.emit(out, bt, Expr::Const(Const::Bool(true)));
                (lo, hi)
            }
            B::Dpdx | B::Dpdy | B::Fwidth => {
                return Err(Error::not_derivable(
                    "an interval of a screen-space derivative isn't supported",
                ));
            }
            // Their results are integers, which take the branch above.
            B::CountOnes | B::LeadingZeros | B::TrailingZeros => self.full(out, ty),
        })
    }

    /// A builtin that's non-decreasing in every argument and exact: applied to the `lo`s and
    /// to the `hi`s.
    fn monotone(&mut self, out: &mut Block, b: Builtin, r: &[Iv], ty: TypeId) -> Iv {
        let lo = self.builtin(out, b, r.iter().map(|x| x.0).collect(), ty);
        let hi = self.builtin(out, b, r.iter().map(|x| x.1).collect(), ty);
        (lo, hi)
    }

    /// The sum of a vector's components.
    fn sum(&mut self, out: &mut Block, v: ValueId, n: u8, ty: TypeId) -> ValueId {
        let mut acc = self.emit(out, ty, Expr::Extract(v, 0));
        for c in 1..n as u32 {
            let x = self.emit(out, ty, Expr::Extract(v, c));
            acc = self.bin(out, BinOp::Add, acc, x, ty);
        }
        acc
    }

    /// `length` of a vector (or `|x|` of a scalar), `at` the argument's type.
    fn length(&mut self, out: &mut Block, x: Iv, at: TypeId, ty: TypeId) -> Iv {
        let TypeDef::Vector(_, n) = *self.m.types.get(at) else {
            return self.abs(out, x, ty);
        };
        let sq = self.sqr(out, x, at);
        let lo = self.sum(out, sq.0, n, ty);
        let hi = self.sum(out, sq.1, n, ty);
        let r = (
            self.builtin(out, Builtin::Sqrt, vec![lo], ty),
            self.builtin(out, Builtin::Sqrt, vec![hi], ty),
        );
        // Sums of squares don't cancel; the error is relative. WGSL: sqrt(dot(e, e)).
        let ulps = if self.gpu() { 16.0 } else { 4.0 };
        let (l, h) = self.widen(out, r, ty, ulps, 0.0);
        // A vector is at least as long as its largest component's least magnitude, also where
        // the squares underflow: one that can't be 0 has a length that can't be either.
        let least = self.abs(out, x, at).0;
        let mut biggest = self.emit(out, ty, Expr::Extract(least, 0));
        for c in 1..n as u32 {
            let e = self.emit(out, ty, Expr::Extract(least, c));
            biggest = self.builtin(out, Builtin::Max, vec![biggest, e], ty);
        }
        // The length as computed rounds down by at most the widening's ulps.
        let k = self.fc(out, ty, 1.0 - (ulps + 1.0) * self.eps(ty));
        let floor = self.bin(out, BinOp::Mul, biggest, k, ty);
        (self.builtin(out, Builtin::Max, vec![l, floor], ty), h)
    }

    /// `normalize(x)`, a component at a time: `x_i / sqrt(x_i² + R²)`, where `R²` is the
    /// others' squares summed, rises with `x_i` and, for each sign of `x_i`, shrinks in
    /// magnitude as `R²` grows. So its ends are at the ends of `x_i` and `R²`: exact, also where
    /// the other components are exactly 0, where dividing `x` by its length as intervals would
    /// lose that `x_i / |x_i|` is ±1.
    fn normalize(&mut self, out: &mut Block, x: Iv, ty: TypeId) -> R<Iv> {
        let TypeDef::Vector(_, n) = *self.m.types.get(ty) else {
            return Err(Error::internal("a normalize of a non-vector"));
        };
        let f32t = self.m.types.f32();
        let sq = self.sqr(out, x, ty);
        let (mut los, mut his) = (Vec::new(), Vec::new());
        for i in 0..n as u32 {
            // R² over the others: its least and greatest.
            let mut r = None;
            for j in (0..n as u32).filter(|&j| j != i) {
                let l = self.emit(out, f32t, Expr::Extract(sq.0, j));
                let h = self.emit(out, f32t, Expr::Extract(sq.1, j));
                r = Some(match r {
                    None => (l, h),
                    Some((al, ah)) => (
                        self.bin(out, BinOp::Add, al, l, f32t),
                        self.bin(out, BinOp::Add, ah, h, f32t),
                    ),
                });
            }
            let (rl, rh) = r.ok_or_else(|| Error::internal("a one-component vector"))?;
            let a = self.emit(out, f32t, Expr::Extract(x.0, i));
            let b = self.emit(out, f32t, Expr::Extract(x.1, i));
            let zero = self.fc(out, f32t, 0.0);
            // lo: at a, with the R² that makes it least; hi: at b, with the R² that makes it most.
            let a_pos = self.cmp(out, BinOp::Ge, a, zero);
            let ra = self.select(out, a_pos, rh, rl);
            let lo = self.unit_part(out, a, ra, f32t);
            let b_pos = self.cmp(out, BinOp::Ge, b, zero);
            let rb = self.select(out, b_pos, rl, rh);
            let hi = self.unit_part(out, b, rb, f32t);
            // normalize rounds: a few ulps on the CPU (a division by a square root), more on
            // the GPU (an inverse square root), and never past ±1 by more than that.
            let ulps = if self.gpu() { 8.0 } else { 4.0 };
            let (lo, hi) = self.widen(out, (lo, hi), f32t, ulps, 0.0);
            los.push(lo);
            his.push(hi);
        }
        Ok((
            self.emit(out, ty, Expr::Construct(ty, los)),
            self.emit(out, ty, Expr::Construct(ty, his)),
        ))
    }

    /// `x / sqrt(x² + r2)` without underflow: ±1 where `r2` is 0 (`x` ≠ 0), 0 where `x` is,
    /// and `sign(x) / sqrt(1 + r2 / x²)` otherwise; ±1 is also the answer for `x = r2 = 0`,
    /// where the direction is any: the caller's range then spans [-1, 1].
    fn unit_part(&mut self, out: &mut Block, x: ValueId, r2: ValueId, ty: TypeId) -> ValueId {
        let one = self.fc(out, ty, 1.0);
        let neg = self.fc(out, ty, -1.0);
        let zero = self.fc(out, ty, 0.0);
        let pos = self.cmp(out, BinOp::Ge, x, zero);
        let s = self.select(out, pos, one, neg);
        let xx = self.bin(out, BinOp::Mul, x, x, ty);
        let q = self.bin(out, BinOp::Div, r2, xx, ty);
        let q1 = self.bin(out, BinOp::Add, one, q, ty);
        let root = self.builtin(out, Builtin::Sqrt, vec![q1], ty);
        let f = self.bin(out, BinOp::Div, s, root, ty);
        // r2 = 0: exactly ±1 (also for x = 0, the range's end where any direction is possible).
        let no_r = self.cmp(out, BinOp::Eq, r2, zero);
        let f = self.select(out, no_r, s, f);
        // x = 0 with r2 > 0: 0 (r2 / 0 is infinite, so f is ±0 already; NaN only for 0 / 0).
        let x0 = self.cmp(out, BinOp::Eq, x, zero);
        let not_r = self.not(out, no_r);
        let bt = self.bool_ty();
        let x0r = self.bin(out, BinOp::And, x0, not_r, bt);
        self.select(out, x0r, zero, f)
    }

    fn inverse_sqrt(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        let x = x[0];
        let z = self.fc(out, ty, 0.0);
        let l = self.builtin(out, Builtin::Max, vec![x.0, z], ty);
        let h = self.builtin(out, Builtin::Max, vec![x.1, z], ty);
        let lo = self.builtin(out, Builtin::InverseSqrt, vec![h], ty);
        let hi = self.builtin(out, Builtin::InverseSqrt, vec![l], ty);
        // At zero it's unbounded both ways: +∞ at +0, and -∞ at -0 (whose square root is -0).
        let pos = self.cmp(out, BinOp::Gt, x.0, z);
        let (fl, fh) = self.full(out, ty);
        let lo = self.select(out, pos, lo, fl);
        let hi = self.select(out, pos, hi, fh);
        let ulps = if self.gpu() { 4.0 } else { 2.0 };
        Ok(self.widen(out, (lo, hi), ty, ulps, 0.0))
    }

    fn pow(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        let (a, b) = (x[0], x[1]);
        let z = self.fc(out, ty, 0.0);
        // The corners bound a base above zero, and one that reaches ±0 under an exponent that
        // isn't negative. A base at ±0 under a negative exponent gives +∞ (at +0) and, for an
        // odd one, -∞ (at -0), with every value between them at the bases between.
        let above = self.cmp(out, BinOp::Gt, a.0, z);
        let at_zero = self.cmp(out, BinOp::Eq, a.0, z);
        let exp_up = self.cmp(out, BinOp::Ge, b.0, z);
        let bt = self.bool_ty();
        let zero_up = self.bin(out, BinOp::And, at_zero, exp_up, bt);
        let pos = self.bin(out, BinOp::Or, above, zero_up, bt);
        let r = if self.gpu() {
            // WGSL: inherited from exp2(y log2(x)); bound each step.
            let l = self.log_ends(out, Builtin::Log2, a, ty);
            let p = self.mul(out, b, l, ty);
            let p = self.widen1(out, p, ty);
            let p = self.exp_held(out, p, ty);
            let e = (
                self.builtin(out, Builtin::Exp2, vec![p.0], ty),
                self.builtin(out, Builtin::Exp2, vec![p.1], ty),
            );
            self.exp_gpu_widen(out, e, p, ty)
        } else {
            // `y log x` is bilinear in (y, log x), so the extremes are at the corners. A base
            // below zero gives the full range, so the corners take it held at zero: `pow` of
            // a negative base is NaN, which a debug build traps even where it's not chosen.
            let held = (
                self.builtin(out, Builtin::Max, vec![a.0, z], ty),
                self.builtin(out, Builtin::Max, vec![a.1, z], ty),
            );
            let e = self.corners(out, ty, held, b, |s, out, x, y| {
                s.builtin(out, Builtin::Pow, vec![x, y], ty)
            });
            self.widen(out, e, ty, CPU_STD_ULPS, 0.0)
        };
        let (fl, fh) = self.full(out, ty);
        Ok((self.select(out, pos, r.0, fl), self.select(out, pos, r.1, fh)))
    }

    /// `log`/`log2` at both ends, widened: `-∞` at or below zero.
    fn log_ends(&mut self, out: &mut Block, b: Builtin, x: Iv, ty: TypeId) -> Iv {
        let z = self.fc(out, ty, 0.0);
        let (fl, _) = self.full(out, ty);
        let mut ends = Vec::new();
        for v in [x.0, x.1] {
            let pos = self.cmp(out, BinOp::Gt, v, z);
            // (The log of the end held at zero: of a negative end it's NaN, unchosen.)
            let held = self.builtin(out, Builtin::Max, vec![v, z], ty);
            let l = self.builtin(out, b, vec![held], ty);
            ends.push(self.select(out, pos, l, fl));
        }
        let r = (ends[0], ends[1]);
        if self.gpu() {
            // WGSL: 3 ulp outside [0.5, 2], 2⁻²¹ absolute inside.
            self.widen(out, r, ty, 6.0, 2.0 * (2f64).powi(-21))
        } else {
            self.widen(out, r, ty, CPU_STD_ULPS, 0.0)
        }
    }

    /// `x` held to ±200, where `exp` and `exp2` are already 0 and +∞ in f32: the range
    /// [`Self::exp_gpu_widen`] widens by. Unheld, an end at ±∞ (`pow`'s base at zero, whose log
    /// is -∞) makes the widening's tolerance ∞, and ∞ times the result's end there (0) is NaN, an
    /// interval holding nothing.
    fn exp_held(&mut self, out: &mut Block, x: Iv, ty: TypeId) -> Iv {
        let (lim_lo, lim_hi) = (self.fc(out, ty, -200.0), self.fc(out, ty, 200.0));
        let mut clamp = |v: ValueId| {
            let v = self.builtin(out, Builtin::Max, vec![v, lim_lo], ty);
            self.builtin(out, Builtin::Min, vec![v, lim_hi], ty)
        };
        (clamp(x.0), clamp(x.1))
    }

    /// Widening for `exp` and `exp2` on the GPU (WGSL: 3 + 2|x| ulp), `x` the argument's range,
    /// [held](Self::exp_held).
    fn exp_gpu_widen(&mut self, out: &mut Block, e: Iv, x: Iv, ty: TypeId) -> Iv {
        let m = self.magnitude(out, x, ty);
        let four = self.fc(out, ty, 4.0 * F32_EPS);
        let six = self.fc(out, ty, 6.0 * F32_EPS);
        let k = self.bin(out, BinOp::Mul, m, four, ty);
        let k = self.bin(out, BinOp::Add, k, six, ty);
        let ml = self.builtin(out, Builtin::Abs, vec![e.0], ty);
        let mh = self.builtin(out, Builtin::Abs, vec![e.1], ty);
        self.widen_k(out, e, ty, (ml, mh), k, 0.0)
    }

    /// `cosh` over `x`: least at 0, so 1 where `x` holds 0, and its larger end's at most.
    fn cosh(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        let (lo, hi) = x[0];
        let (cl, ch) = (
            self.builtin(out, Builtin::Cosh, vec![lo], ty),
            self.builtin(out, Builtin::Cosh, vec![hi], ty),
        );
        let near = self.builtin(out, Builtin::Min, vec![cl, ch], ty);
        let far = self.builtin(out, Builtin::Max, vec![cl, ch], ty);
        let zero = self.fc(out, ty, 0.0);
        let one = self.fc(out, ty, 1.0);
        let bt = self.m.types.bool();
        let below = self.emit(out, bt, Expr::Binary(BinOp::Le, lo, zero));
        let above = self.emit(out, bt, Expr::Binary(BinOp::Ge, hi, zero));
        let holds = self.emit(out, bt, Expr::Binary(BinOp::And, below, above));
        let low = self.emit(out, ty, Expr::Select { cond: holds, if_true: one, if_false: near });
        Ok((low, far))
    }

    fn sin(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        // Peaks at π/2 + 2πk, troughs at -π/2 + 2πk.
        let h = std::f64::consts::FRAC_PI_2;
        self.periodic(out, ty, x[0], Builtin::Sin, h, -h)
    }

    fn cos(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        self.periodic(out, ty, x[0], Builtin::Cos, 0.0, std::f64::consts::PI)
    }

    /// `sin` or `cos` over `x`: the ends, or ±1 where a peak or trough is (or might be, within
    /// rounding of the test) inside.
    fn periodic(
        &mut self,
        out: &mut Block,
        ty: TypeId,
        x: Iv,
        b: Builtin,
        peak: f64,
        trough: f64,
    ) -> R<Iv> {
        let tau = std::f64::consts::TAU;
        let bt = self.bool_ty();
        let e0 = self.builtin(out, b, vec![x.0], ty);
        let e1 = self.builtin(out, b, vec![x.1], ty);
        let (lo, hi) = self.extremes(out, &[e0, e1], ty);
        // A near miss counts as inside (sound: ±1 is a bound).
        let (m, near) = self.with_slack(out, x, ty);
        let hits = [peak, trough].map(|at| self.lattice_hit(out, ty, near, at, tau));
        // Far out, or a whole period wide: everything.
        let wide = self.bin(out, BinOp::Sub, x.1, x.0, ty);
        let tc = self.fc(out, ty, tau);
        let wide = self.cmp(out, BinOp::Ge, wide, tc);
        let far_c = self.fc(out, ty, (2f64).powi(19));
        let far = self.cmp(out, BinOp::Gt, m, far_c);
        let all = self.bin(out, BinOp::Or, wide, far, bt);
        let peak_in = self.bin(out, BinOp::Or, hits[0], all, bt);
        let trough_in = self.bin(out, BinOp::Or, hits[1], all, bt);
        let one = self.fc(out, ty, 1.0);
        let neg = self.fc(out, ty, -1.0);
        let hi = self.select(out, peak_in, one, hi);
        let lo = self.select(out, trough_in, neg, lo);
        Ok(if self.gpu() {
            // WGSL: 2⁻¹¹ absolute (on [-π, π]).
            self.widen(out, (lo, hi), ty, 1.0, 2.0 * (2f64).powi(-11))
        } else {
            self.widen(out, (lo, hi), ty, CPU_STD_ULPS, 0.0)
        })
    }

    /// The largest magnitude in `x`, and `x` widened by slack for a test that rounds in f32
    /// ([`lattice_hit`](Self::lattice_hit)), so that a near miss counts as a hit.
    fn with_slack(&mut self, out: &mut Block, x: Iv, ty: TypeId) -> (ValueId, Iv) {
        let m = self.magnitude(out, x, ty);
        let k = self.fc(out, ty, (2f64).powi(-18));
        let c = self.fc(out, ty, (2f64).powi(-10));
        let slack = self.bin(out, BinOp::Mul, m, k, ty);
        let slack = self.bin(out, BinOp::Add, slack, c, ty);
        let xl = self.bin(out, BinOp::Sub, x.0, slack, ty);
        let xh = self.bin(out, BinOp::Add, x.1, slack, ty);
        (m, (xl, xh))
    }

    /// Whether there's an integer `n` with `x.0 <= at + period n <= x.1`.
    fn lattice_hit(&mut self, out: &mut Block, ty: TypeId, x: Iv, at: f64, period: f64) -> ValueId {
        let a = self.fc(out, ty, at);
        let inv = self.fc(out, ty, 1.0 / period);
        let dl = self.bin(out, BinOp::Sub, x.0, a, ty);
        let dl = self.bin(out, BinOp::Mul, dl, inv, ty);
        let nl = self.builtin(out, Builtin::Ceil, vec![dl], ty);
        let dh = self.bin(out, BinOp::Sub, x.1, a, ty);
        let dh = self.bin(out, BinOp::Mul, dh, inv, ty);
        let nh = self.builtin(out, Builtin::Floor, vec![dh], ty);
        self.cmp(out, BinOp::Le, nl, nh)
    }

    fn tan(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        let x = x[0];
        if self.gpu() {
            // WGSL: inherited from sin / cos.
            let s = self.sin(out, ty, &[x])?;
            let c = self.cos(out, ty, &[x])?;
            return self.div(out, ty, &[s, c]);
        }
        // Increasing between asymptotes at π/2 + πk.
        let pi = std::f64::consts::PI;
        let bt = self.bool_ty();
        let e = (
            self.builtin(out, Builtin::Tan, vec![x.0], ty),
            self.builtin(out, Builtin::Tan, vec![x.1], ty),
        );
        let e = self.widen(out, e, ty, CPU_STD_ULPS, 0.0);
        // A near miss counts as a pole (as in `periodic`); far out, everything.
        let (m, near) = self.with_slack(out, x, ty);
        let pole = self.lattice_hit(out, ty, near, pi / 2.0, pi);
        let far_c = self.fc(out, ty, (2f64).powi(19));
        let far = self.cmp(out, BinOp::Gt, m, far_c);
        // Ends out of order can only straddle a pole.
        let inverted = self.cmp(out, BinOp::Gt, e.0, e.1);
        let pole = self.bin(out, BinOp::Or, pole, far, bt);
        let pole = self.bin(out, BinOp::Or, pole, inverted, bt);
        let (fl, fh) = self.full(out, ty);
        Ok((self.select(out, pole, fl, e.0), self.select(out, pole, fh, e.1)))
    }

    /// asin, acos, atan, atan2. WGSL: 4096 ulp (atan, atan2; asin and acos inherit from
    /// atan2).
    fn inverse_trig_widen(&mut self, out: &mut Block, x: Iv, ty: TypeId) -> Iv {
        if self.gpu() {
            self.widen(out, x, ty, 8192.0, (2f64).powi(-20))
        } else {
            self.widen(out, x, ty, CPU_STD_ULPS, 0.0)
        }
    }

    fn fract(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        // Within one unit cell, `x - floor(x)` is increasing; across cells, anything in [0, 1].
        let x = x[0];
        let fl = self.builtin(out, Builtin::Floor, vec![x.0], ty);
        let fh = self.builtin(out, Builtin::Floor, vec![x.1], ty);
        let same = self.cmp(out, BinOp::Eq, fl, fh);
        let l = self.bin(out, BinOp::Sub, x.0, fl, ty);
        let h = self.bin(out, BinOp::Sub, x.1, fh, ty);
        let z = self.fc(out, ty, 0.0);
        let one = self.fc(out, ty, 1.0);
        let lo = self.select(out, same, l, z);
        let hi = self.select(out, same, h, one);
        Ok(self.widen1(out, (lo, hi), ty))
    }

    fn mix(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        let (a, b, t) = (x[0], x[1], x[2]);
        let bt = self.bool_ty();
        // For t in [0, 1], mix is non-decreasing in a and b and linear in t: the extremes are
        // at (a.lo, b.lo) and (a.hi, b.hi), each at one end of t.
        let c = [
            self.builtin(out, Builtin::Mix, vec![a.0, b.0, t.0], ty),
            self.builtin(out, Builtin::Mix, vec![a.0, b.0, t.1], ty),
            self.builtin(out, Builtin::Mix, vec![a.1, b.1, t.0], ty),
            self.builtin(out, Builtin::Mix, vec![a.1, b.1, t.1], ty),
        ];
        let lo = self.builtin(out, Builtin::Min, vec![c[0], c[1]], ty);
        let hi = self.builtin(out, Builtin::Max, vec![c[2], c[3]], ty);
        // a (1 - t) + b t rounds three times, relative to the terms it adds; a GPU may compute
        // a + t (b - a) instead, whose error also grows with |b - a| t. So the magnitude is
        // |a| (1 - t) + (|a| + |b|) t: just |a| at t = 0, where `smin` against a huge value
        // starts its fold.
        let ma = self.magnitude(out, a, ty);
        let mb = self.magnitude(out, b, ty);
        let one = self.fc(out, ty, 1.0);
        let omt = self.bin(out, BinOp::Sub, one, t.0, ty);
        let x = self.bin(out, BinOp::Mul, ma, omt, ty);
        let ab = self.bin(out, BinOp::Add, ma, mb, ty);
        let y = self.bin(out, BinOp::Mul, ab, t.1, ty);
        let mag = self.bin(out, BinOp::Add, x, y, ty);
        let ulps = if self.gpu() { 8.0 } else { 4.0 };
        let convex = self.widen_mag(out, (lo, hi), ty, (mag, mag), ulps, 0.0);
        // Otherwise, its formula, bounded step by step.
        let omt =
            (self.bin(out, BinOp::Sub, one, t.1, ty), self.bin(out, BinOp::Sub, one, t.0, ty));
        let omt = self.widen1(out, omt, ty);
        let p = self.mul(out, a, omt, ty);
        let p = self.widen1(out, p, ty);
        let q = self.mul(out, b, t, ty);
        let q = self.widen1(out, q, ty);
        let s = (self.bin(out, BinOp::Add, p.0, q.0, ty), self.bin(out, BinOp::Add, p.1, q.1, ty));
        let general = self.widen1(out, s, ty);
        let z = self.fc(out, ty, 0.0);
        let ge = self.cmp(out, BinOp::Ge, t.0, z);
        let le = self.cmp(out, BinOp::Le, t.1, one);
        let inside = self.bin(out, BinOp::And, ge, le, bt);
        Ok((
            self.select(out, inside, convex.0, general.0),
            self.select(out, inside, convex.1, general.1),
        ))
    }

    fn smoothstep(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        // With e0 < e1 it's non-decreasing in x and non-increasing in both edges.
        let (e0, e1, v) = (x[0], x[1], x[2]);
        let lo = self.builtin(out, Builtin::Smoothstep, vec![e0.1, e1.1, v.0], ty);
        let hi = self.builtin(out, Builtin::Smoothstep, vec![e0.0, e1.0, v.1], ty);
        let ok = self.cmp(out, BinOp::Lt, e0.1, e1.0);
        // Unless the widest e1 - e0 overflows: at ∞ its t is 0, so the result grows with e0
        // there, and the corners don't bound it.
        let w = self.bin(out, BinOp::Sub, e1.1, e0.0, ty);
        let max = self.fc(out, ty, f32::MAX as f64);
        let finite = self.cmp(out, BinOp::Le, w, max);
        let bt = self.bool_ty();
        let ok = self.bin(out, BinOp::And, ok, finite, bt);
        let z = self.fc(out, ty, 0.0);
        let one = self.fc(out, ty, 1.0);
        let lo = self.select(out, ok, lo, z);
        let hi = self.select(out, ok, hi, one);
        // The result is in [0, 1] and its error relative to 1 is a few roundings (WGSL: inherited
        // from t² (3 - 2t), with t from a division).
        let abs = if self.gpu() { (2f64).powi(-19) } else { (2f64).powi(-20) };
        Ok(self.widen(out, (lo, hi), ty, 1.0, abs))
    }
}

/// std's CPU transcendentals compute in f64 and round once to f32: within an ulp. Twice that,
/// for the point and the bound, and some margin.
const CPU_STD_ULPS: f64 = 4.0;

/// How far the GPU's `sinh`, `cosh` and `tanh` may be from the truth: ulps of the result's
/// magnitude (WGSL inherits their accuracy from `exp`'s, 3 + 2|x| ulps), and, for `sinh` and
/// `tanh` near 0, an absolute error. The GPU interval tests hold them to these.
const HYPERBOLIC_GPU_ULPS: f64 = 1024.0;
const HYPERBOLIC_GPU_ABS: f64 = 1.0 / 1048576.0;

/// The float range whose truncation fits in an integer type (exactly representable ends).
fn float_int_range(from: Scalar, to: Scalar) -> (f64, f64) {
    let f64_ = from == Scalar::F64;
    match to {
        Scalar::I32 if f64_ => (-2147483648.0, 2147483647.0),
        Scalar::I32 => (-2147483648.0, 2147483520.0),
        Scalar::U32 if f64_ => (0.0, 4294967295.0),
        Scalar::U32 => (0.0, 4294967040.0),
        Scalar::I64 if f64_ => (-9223372036854775808.0, 9223372036854774784.0),
        Scalar::I64 => (-9223372036854775808.0, 9223371487098961920.0),
        Scalar::U64 if f64_ => (0.0, 18446744073709549568.0),
        Scalar::U64 => (0.0, 18446742974197923840.0),
        Scalar::I8 => (-128.0, 127.0),
        Scalar::U8 => (0.0, 255.0),
        Scalar::I16 => (-32768.0, 32767.0),
        Scalar::U16 => (0.0, 65535.0),
        Scalar::Bool | Scalar::F32 | Scalar::F64 => (0.0, 0.0),
    }
}

/// The type of a range's bounds: `ty`, with each enum in it a struct of its tag and a field for
/// each variant's payload (a `u32` zero for none), in the enum's field order.
fn range_ty(types: &mut Types, ty: TypeId) -> TypeId {
    match types.get(ty).clone() {
        TypeDef::Enum { name, variants, .. } => {
            let u32t = types.u32();
            let mut fields = vec![("tag".to_string(), u32t)];
            for (v, p) in variants {
                fields.push((v, p.map_or(u32t, |p| range_ty(types, p))));
            }
            types.intern(TypeDef::Struct { name: format!("range<{name}>"), fields })
        }
        TypeDef::Struct { name, fields } => {
            let ranged: Vec<(String, TypeId)> =
                fields.iter().map(|(n, t)| (n.clone(), range_ty(types, *t))).collect();
            if ranged == fields {
                return ty;
            }
            types.intern(TypeDef::Struct { name: format!("range<{name}>"), fields: ranged })
        }
        TypeDef::Array(e, n) => {
            let r = range_ty(types, e);
            if r == e { ty } else { types.intern(TypeDef::Array(r, n)) }
        }
        TypeDef::Ptr(e) => {
            let r = range_ty(types, e);
            if r == e { ty } else { types.intern(TypeDef::Ptr(r)) }
        }
        _ => ty,
    }
}
