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
//! Known limits: NaN results (from `0 × ∞`, `∞ − ∞`, or a point outside a function's domain,
//! like `sqrt` of a negative) can't be bounded, and on the CPU a NaN bound is replaced by an
//! infinite one at the end; on the GPU, which may assume NaNs away, they aren't. WGSL bounds
//! `sin` and `cos` only on `[-π, π]`; outside it we assume the same absolute error. A branch
//! taken speculatively still runs its code: an integer overflow or out-of-range index there
//! traps on the CPU even if no point in the box would have taken it (a `Trap` statement itself
//! is skipped).

use super::single_exit::single_exit;
use super::*;
use std::collections::HashMap;

type R<T> = Result<T, String>;
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
    let x_ty = func.params.get(xi).ok_or("internal: no input parameter")?.ty;
    if !matches!(m.types.get(x_ty), TypeDef::Scalar(Scalar::F32) | TypeDef::Vector(_)) {
        return Err(format!(
            "an interval needs an `f32` or a float vector input, not `{}`",
            m.types.display(x_ty)
        ));
    }
    let f32 = m.types.f32();
    if func.ret != Some(f32) {
        return Err("an interval needs a function that returns an `f32`".into());
    }
    let mut mask = vec![false; func.params.len()];
    mask[xi] = true;
    let d = derive(m, cache, f, mask.clone(), target)?;
    let returns = returns_active(m, cache, f, mask)?;

    let mut params = func.params[..xi].to_vec();
    params.push(Param { name: "over".into(), ty: box_ty, by_ref: false, mutable: false });
    let mut g = Function::new(format!("{}_interval", func.name), params, Some(interval_ty));
    let mut body = Vec::new();
    let mut args = Vec::new();
    for (i, p) in func.params[..xi].iter().enumerate() {
        if p.by_ref {
            args.push(Arg::Place(Place { root: PlaceRoot::Param(i as u32), path: Vec::new() }));
        } else {
            let v = g.new_value(p.ty);
            body.push(Stmt::Let(v, Expr::Param(i as u32)));
            args.push(Arg::Value(v));
        }
    }
    let over = g.new_value(box_ty);
    body.push(Stmt::Let(over, Expr::Param(xi as u32)));
    let lo = g.new_value(x_ty);
    body.push(Stmt::Let(lo, Expr::Extract(over, 0)));
    let hi = g.new_value(x_ty);
    body.push(Stmt::Let(hi, Expr::Extract(over, 1)));
    // The input's `lo` takes its own slot; its `hi` is appended after the originals.
    args.push(Arg::Value(lo));
    args.push(Arg::Value(hi));
    let rty = m.functions[d.index()].ret.ok_or("internal: a derived function returns nothing")?;
    let r = g.new_value(rty);
    body.push(Stmt::Let(r, Expr::Call(d, args)));
    let (mut rlo, mut rhi) = (r, r);
    if returns {
        rlo = g.new_value(f32);
        body.push(Stmt::Let(rlo, Expr::Extract(r, 0)));
        rhi = g.new_value(f32);
        body.push(Stmt::Let(rhi, Expr::Extract(r, 1)));
    }
    if target == Target::Cpu {
        // A NaN bound means "unknown": make it infinite.
        let b = m.types.bool();
        for (v, inf) in [(&mut rlo, f32::NEG_INFINITY), (&mut rhi, f32::INFINITY)] {
            let ok = g.new_value(b);
            body.push(Stmt::Let(ok, Expr::Binary(BinOp::Eq, *v, *v)));
            let c = g.new_value(f32);
            body.push(Stmt::Let(c, Expr::Const(Const::F32(inf))));
            let s = g.new_value(f32);
            body.push(Stmt::Let(s, Expr::Select { cond: ok, if_true: *v, if_false: c }));
            *v = s;
        }
    }
    let out = g.new_value(interval_ty);
    body.push(Stmt::Let(out, Expr::Construct(interval_ty, vec![rlo, rhi])));
    body.push(Stmt::Return(Some(out)));
    g.body = body;
    Ok(m.add_function(g))
}

/// Whether `f` with these active parameters returns a range rather than a single value.
pub(super) fn returns_active(
    m: &Module,
    cache: &mut DeriveCache,
    f: FuncId,
    mask: Vec<bool>,
) -> R<bool> {
    if let Some(&r) = cache.interval_returns.get(&(f, mask.clone())) {
        return Ok(r);
    }
    cache.interval_returns.insert((f, mask.clone()), true);
    let func = &m.functions[f.index()];
    let a = activity(m, func, &mask, Mode::Interval, &mut |g, mk| returns_active(m, cache, g, mk))?;
    let r = a.returns && func.ret.is_some();
    cache.interval_returns.insert((f, mask), r);
    Ok(r)
}

/// The interval version of `f` with active parameters `mask`. It takes `f`'s parameters, the
/// active ones as their `lo`, followed by a `hi` for each active one; it returns
/// `range<T> { lo, hi }` when its result is active.
fn derive(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    mask: Vec<bool>,
    target: Target,
) -> R<FuncId> {
    if let Some(&d) = cache.interval.get(&(f, mask.clone())) {
        return Ok(d);
    }
    let orig = m.functions[f.index()].clone();
    if let Some(why) = has_host_or_ptr(&orig.body) {
        return Err(format!("`{}` {why}", orig.name));
    }
    let (func, result) = match single_exit(m, &orig) {
        Some((nf, r)) => (nf, r),
        None => (orig, None),
    };
    let returns = returns_active(m, cache, f, mask.clone())?;
    let act = {
        let mm: &Module = m;
        activity(mm, &func, &mask, Mode::Interval, &mut |g, mk| returns_active(mm, cache, g, mk))?
    };
    if act.active_loop_exit {
        return Err(format!(
            "`{}` has a loop whose exit depends on the input; an interval needs loops that run \
             the same number of times for every point",
            func.name
        ));
    }
    for (i, p) in func.params.iter().enumerate() {
        if act.params[i] && !mask[i] && p.mutable {
            return Err(format!(
                "internal: `{}` writes a range through its `mut` parameter `{}`",
                func.name, p.name
            ));
        }
    }
    let mut nf = func.clone();
    nf.name = format!("{}_iv", func.name);
    let mut hparam = vec![None; func.params.len()];
    for (i, p) in func.params.iter().enumerate() {
        if mask[i] {
            hparam[i] = Some(nf.params.len() as u32);
            nf.params.push(Param { name: format!("{}_hi", p.name), ..p.clone() });
        }
    }
    let mut hlocal = vec![None; func.locals.len()];
    for (l, decl) in func.locals.iter().enumerate() {
        if act.locals[l] {
            hlocal[l] = Some(nf.new_local(format!("{}_hi", decl.name), decl.ty));
        }
    }
    if returns {
        let r = func.ret.ok_or("internal: an active result of nothing")?;
        nf.ret = Some(m.types.intern(TypeDef::Struct {
            name: format!("range<{}>", m.types.display(r)),
            fields: vec![("lo".into(), r), ("hi".into(), r)],
        }));
    }
    nf.body = Vec::new();
    let id = m.add_function(Function::new(nf.name.clone(), Vec::new(), None));
    cache.interval.insert((f, mask), id);
    let mut b = Ivx {
        m,
        cache,
        act,
        target,
        nf,
        hparam,
        hlocal,
        iv: HashMap::new(),
        returns,
        speculative: 0,
        loads: Vec::new(),
        same: HashMap::new(),
    };
    let mut body = Vec::new();
    if let Some(r) = result.filter(|r| b.act.locals[r.index()]) {
        // The result starts empty, so a side that never returns adds nothing to the hull.
        let ty = b.nf.locals[r.index()].ty;
        let e = b.empty(&mut body, ty)?;
        b.store_root(&mut body, &PlaceRoot::Local(r), e)?;
    }
    b.block_into(&func.body, &mut body)?;
    let mut nf = b.nf;
    nf.body = body;
    m.functions[id.index()] = nf;
    Ok(id)
}

/// What [`Ivx::leaves`] applies to each leaf.
type LeafFn<'f, 'a> = dyn FnMut(&mut Ivx<'a>, &mut Block, TypeId, &[ValueId]) -> R<ValueId> + 'f;

/// A bound rule for one scalar operation (or one vector operation, where it works
/// componentwise).
type Rule<'a> = fn(&mut Ivx<'a>, &mut Block, TypeId, &[Iv]) -> R<Iv>;

struct Ivx<'a> {
    m: &'a mut Module,
    cache: &'a mut DeriveCache,
    act: Activity,
    target: Target,
    nf: Function,
    hparam: Vec<Option<u32>>,
    hlocal: Vec<Option<LocalId>>,
    iv: HashMap<ValueId, Iv>,
    returns: bool,
    /// How many branches deep we are that run whether or not a point would take them.
    speculative: u32,
    /// Loads since the last write or block boundary, by place: two loads of the same place
    /// are the same value, so `x * x` and `dot(v, v)` get the rule for squares.
    loads: Vec<(Place, ValueId)>,
    /// A load's earlier twin.
    same: HashMap<ValueId, ValueId>,
}

impl<'a> Ivx<'a> {
    // ---- emission helpers --------------------------------------------------------------------

    fn emit(&mut self, out: &mut Block, ty: TypeId, e: Expr) -> ValueId {
        let v = self.nf.new_value(ty);
        out.push(Stmt::Let(v, e));
        v
    }

    fn ty(&self, v: ValueId) -> TypeId {
        self.nf.value_ty(v)
    }

    /// Whether two values are surely equal: the same value, or loads of an unchanged place.
    fn same_value(&self, a: ValueId, b: ValueId) -> bool {
        let c = |v: ValueId| self.same.get(&v).copied().unwrap_or(v);
        c(a) == c(b)
    }

    fn range(&self, v: ValueId) -> Iv {
        self.iv.get(&v).copied().unwrap_or((v, v))
    }

    fn elem(&self, ty: TypeId) -> Scalar {
        self.m.types.element_scalar(ty).unwrap_or(Scalar::F32)
    }

    fn is_vector(&self, ty: TypeId) -> bool {
        matches!(self.m.types.get(ty), TypeDef::Vector(_))
    }

    fn bool_ty(&mut self) -> TypeId {
        self.m.types.bool()
    }

    fn bin(&mut self, out: &mut Block, op: BinOp, a: ValueId, b: ValueId, ty: TypeId) -> ValueId {
        self.emit(out, ty, Expr::Binary(op, a, b))
    }

    fn cmp(&mut self, out: &mut Block, op: BinOp, a: ValueId, b: ValueId) -> ValueId {
        let bt = self.bool_ty();
        self.emit(out, bt, Expr::Binary(op, a, b))
    }

    fn call(&mut self, out: &mut Block, b: Builtin, args: Vec<ValueId>, ty: TypeId) -> ValueId {
        self.emit(out, ty, Expr::Builtin(b, args))
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

    /// A scalar as type `ty`: splatted to a vector.
    fn coerce(&mut self, out: &mut Block, x: ValueId, ty: TypeId) -> ValueId {
        match (self.m.types.get(self.ty(x)).clone(), self.m.types.get(ty).clone()) {
            (TypeDef::Scalar(_), TypeDef::Vector(n)) => self.emit(out, ty, Expr::Splat(x, n)),
            _ => x,
        }
    }

    fn coerce_iv(&mut self, out: &mut Block, x: Iv, ty: TypeId) -> Iv {
        (self.coerce(out, x.0, ty), self.coerce(out, x.1, ty))
    }

    /// The largest magnitude a float of this type takes here: infinity on the CPU, `MAX` on the
    /// GPU (which may assume infinities away).
    fn inf(&self, ty: TypeId) -> f64 {
        match (self.target, self.elem(ty)) {
            (Target::Gpu, _) => f32::MAX as f64,
            (Target::Cpu, _) => f64::INFINITY,
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
        let ml = self.call(out, Builtin::Abs, vec![x.0], ty);
        let mh = self.call(out, Builtin::Abs, vec![x.1], ty);
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
        (lo, hi)
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
        let parts: Vec<(TypeId, Expr)>;
        let mut arrays = false;
        match self.m.types.get(ty).clone() {
            TypeDef::Scalar(_) | TypeDef::Vector(_) => return f(self, out, ty, vals),
            TypeDef::Matrix(n) => {
                let col = self.m.types.vector(n);
                parts = (0..n as u32).map(|c| (col, Expr::Extract(vals[0], c))).collect();
            }
            TypeDef::Struct { fields, .. } => {
                parts = fields
                    .iter()
                    .enumerate()
                    .map(|(k, (_, t))| (*t, Expr::Extract(vals[0], k as u32)))
                    .collect();
            }
            TypeDef::Array(e, n) if n <= 256 => {
                arrays = true;
                parts = (0..n).map(|k| (e, Expr::Extract(vals[0], k))).collect();
            }
            _ => {
                return Err(format!(
                    "an interval over a `{}` isn't supported",
                    self.m.types.display(ty)
                ));
            }
        }
        let u32t = self.m.types.u32();
        let mut built = Vec::new();
        for (k, (pt, _)) in parts.iter().enumerate() {
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
        self.call(out, op, vec![a, b], ty)
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
                let inf = self.inf(ty);
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
                self.hlocal[l.index()].ok_or("internal: an inactive local's range")?,
            ),
            PlaceRoot::Param(i) => PlaceRoot::Param(
                self.hparam[*i as usize].ok_or("internal: an inactive parameter's range")?,
            ),
            _ => return Err("an interval can't go through this place".into()),
        })
    }

    /// The places holding a range's `lo` and `hi`.
    fn places(&self, p: &Place) -> R<(Place, Place)> {
        if p.path.iter().any(|x| matches!(x, Proj::Index(v) if self.act.values[v.index()])) {
            return Err(
                "an interval through an array index that depends on the input isn't supported"
                    .into(),
            );
        }
        let hi = Place { root: self.hi_root(&p.root)?, path: p.path.clone() };
        Ok((p.clone(), hi))
    }

    fn root_ty(&self, r: &PlaceRoot) -> TypeId {
        match r {
            PlaceRoot::Local(l) => self.nf.locals[l.index()].ty,
            PlaceRoot::Param(i) => self.nf.params[*i as usize].ty,
            _ => TypeId(0),
        }
    }

    fn load_root(&mut self, out: &mut Block, r: &PlaceRoot) -> R<Iv> {
        let ty = self.root_ty(r);
        let h = self.hi_root(r)?;
        let lo = self.emit(out, ty, Expr::Load(Place { root: r.clone(), path: Vec::new() }));
        let hi = self.emit(out, ty, Expr::Load(Place { root: h, path: Vec::new() }));
        Ok((lo, hi))
    }

    fn store_root(&mut self, out: &mut Block, r: &PlaceRoot, x: Iv) -> R<()> {
        let h = self.hi_root(r)?;
        out.push(Stmt::Store(Place { root: r.clone(), path: Vec::new() }, x.0));
        out.push(Stmt::Store(Place { root: h, path: Vec::new() }, x.1));
        Ok(())
    }

    /// The active locals and parameters a block may write.
    fn written(&self, b: &Block, into: &mut Vec<PlaceRoot>) {
        let add = |r: &PlaceRoot, into: &mut Vec<PlaceRoot>| {
            let active = match r {
                PlaceRoot::Local(l) => self.act.locals[l.index()],
                PlaceRoot::Param(i) => self.act.params[*i as usize],
                _ => false,
            };
            if active && !into.contains(r) {
                into.push(r.clone());
            }
        };
        for s in b {
            match s {
                Stmt::Store(p, _) => add(&p.root, into),
                Stmt::Let(_, Expr::Call(g, args)) | Stmt::Eval(Expr::Call(g, args)) => {
                    for (a, p) in args.iter().zip(&self.m.functions[g.index()].params) {
                        if let (Arg::Place(pl), true) = (a, p.mutable) {
                            add(&pl.root, into);
                        }
                    }
                }
                Stmt::If { then, else_, .. } => {
                    self.written(then, into);
                    self.written(else_, into);
                }
                Stmt::Loop { body, continuing } => {
                    self.written(body, into);
                    self.written(continuing, into);
                }
                _ => {}
            }
        }
    }

    // ---- statements --------------------------------------------------------------------------

    fn block_into(&mut self, b: &Block, out: &mut Block) -> R<()> {
        for s in b {
            self.stmt(s, out)?;
        }
        Ok(())
    }

    fn block(&mut self, b: &Block) -> R<Block> {
        let mut out = Vec::new();
        self.block_into(b, &mut out)?;
        Ok(out)
    }

    fn stmt(&mut self, s: &Stmt, out: &mut Block) -> R<()> {
        match s {
            Stmt::Let(v, Expr::Load(p)) => {
                if let Some(&(_, w)) = self.loads.iter().find(|(q, _)| q == p) {
                    self.same.insert(*v, w);
                } else {
                    self.loads.push((p.clone(), *v));
                }
                self.let_(s, *v, &Expr::Load(p.clone()), out)
            }
            Stmt::Store(..) | Stmt::If { .. } | Stmt::Loop { .. } | Stmt::Eval(Expr::Call(..)) => {
                self.loads.clear();
                self.stmt_inner(s, out)?;
                self.loads.clear();
                Ok(())
            }
            Stmt::Let(v, e @ Expr::Call(..)) => {
                self.loads.clear();
                self.let_(s, *v, e, out)
            }
            Stmt::Let(v, e) => self.let_(s, *v, e, out),
            _ => self.stmt_inner(s, out),
        }
    }

    fn let_(&mut self, s: &Stmt, v: ValueId, e: &Expr, out: &mut Block) -> R<()> {
        if let Expr::Call(g, args) = e {
            return self.call_fn(Some(v), *g, args, out);
        }
        if self.act.values[v.index()] {
            let r = self.expr(v, e, out)?;
            self.iv.insert(v, r);
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
                if place_root_active(&self.act, p) {
                    let (lp, hp) = self.places(p)?;
                    let (lo, hi) = self.range(*v);
                    out.push(Stmt::Store(lp, lo));
                    out.push(Stmt::Store(hp, hi));
                } else {
                    out.push(s.clone());
                }
                Ok(())
            }
            Stmt::If { cond, then, else_ } if self.act.values[cond.index()] => {
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
                self.speculative += 1;
                self.block_into(then, out)?;
                let mut after_then = Vec::new();
                for (r, b) in roots.iter().zip(&before) {
                    after_then.push(self.load_root(out, r)?);
                    self.store_root(out, r, *b)?;
                }
                self.block_into(else_, out)?;
                self.speculative -= 1;
                for (r, t) in roots.iter().zip(after_then) {
                    let e = self.load_root(out, r)?;
                    let ty = self.root_ty(r);
                    let j = self.merge(out, ty, c, t, e)?;
                    self.store_root(out, r, j)?;
                }
                Ok(())
            }
            Stmt::If { cond, then, else_ } => {
                let then = self.block(then)?;
                let else_ = self.block(else_)?;
                out.push(Stmt::If { cond: *cond, then, else_ });
                Ok(())
            }
            Stmt::Loop { body, continuing } => {
                let body = self.block(body)?;
                let continuing = self.block(continuing)?;
                out.push(Stmt::Loop { body, continuing });
                Ok(())
            }
            Stmt::Return(Some(v)) if self.returns => {
                let (lo, hi) = self.range(*v);
                let rt = self.nf.ret.ok_or("internal: no range type")?;
                let r = self.emit(out, rt, Expr::Construct(rt, vec![lo, hi]));
                out.push(Stmt::Return(Some(r)));
                Ok(())
            }
            Stmt::Trap if self.speculative > 0 => Ok(()),
            other => {
                out.push(other.clone());
                Ok(())
            }
        }
    }

    fn call_fn(&mut self, v: Option<ValueId>, g: FuncId, args: &[Arg], out: &mut Block) -> R<()> {
        let mask: Vec<bool> = args.iter().map(|a| arg_active(&self.act, a)).collect();
        if !mask.iter().any(|b| *b) {
            out.push(match v {
                Some(v) => Stmt::Let(v, Expr::Call(g, args.to_vec())),
                None => Stmt::Eval(Expr::Call(g, args.to_vec())),
            });
            return Ok(());
        }
        let dg = derive(self.m, self.cache, g, mask.clone(), self.target)?;
        let returns = returns_active(self.m, self.cache, g, mask.clone())?;
        let mut new_args = Vec::new();
        let mut his = Vec::new();
        for (a, active) in args.iter().zip(&mask) {
            match (a, active) {
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
                let rt = self.m.functions[dg.index()].ret.ok_or("internal: no range type")?;
                let r = self.emit(out, rt, Expr::Call(dg, new_args));
                let ty = self.ty(v);
                let lo = self.emit(out, ty, Expr::Extract(r, 0));
                let hi = self.emit(out, ty, Expr::Extract(r, 1));
                self.iv.insert(v, (lo, hi));
            }
            (Some(v), false) => out.push(Stmt::Let(v, Expr::Call(dg, new_args))),
            (None, _) => out.push(Stmt::Eval(Expr::Call(dg, new_args))),
        }
        Ok(())
    }

    // ---- expressions -------------------------------------------------------------------------

    /// The range of an active value `v = e`.
    fn expr(&mut self, v: ValueId, e: &Expr, out: &mut Block) -> R<Iv> {
        let ty = self.ty(v);
        Ok(match e {
            Expr::Param(i) => {
                let h =
                    self.hparam[*i as usize].ok_or("internal: an inactive parameter's range")?;
                (self.emit(out, ty, Expr::Param(*i)), self.emit(out, ty, Expr::Param(h)))
            }
            Expr::Load(p) => {
                let (lp, hp) = self.places(p)?;
                (self.emit(out, ty, Expr::Load(lp)), self.emit(out, ty, Expr::Load(hp)))
            }
            Expr::Unary(op, x) => self.unary(out, *op, *x, ty)?,
            Expr::Binary(op, a, b) => self.binary(out, *op, *a, *b, ty)?,
            Expr::Builtin(b, args) => self.builtin(out, *b, args, ty)?,
            Expr::Construct(t, parts) => {
                let rs: Vec<Iv> = parts.iter().map(|p| self.range(*p)).collect();
                let lo = self.emit(out, *t, Expr::Construct(*t, rs.iter().map(|r| r.0).collect()));
                let hi = self.emit(out, *t, Expr::Construct(*t, rs.iter().map(|r| r.1).collect()));
                (lo, hi)
            }
            Expr::Extract(x, i) => {
                let (lo, hi) = self.range(*x);
                (
                    self.emit(out, ty, Expr::Extract(lo, *i)),
                    self.emit(out, ty, Expr::Extract(hi, *i)),
                )
            }
            Expr::ExtractDyn(x, i) if !self.act.values[i.index()] => {
                let (lo, hi) = self.range(*x);
                (
                    self.emit(out, ty, Expr::ExtractDyn(lo, *i)),
                    self.emit(out, ty, Expr::ExtractDyn(hi, *i)),
                )
            }
            Expr::ExtractDyn(x, _) => {
                // Any element: the hull of them all.
                let n = match self.m.types.get(self.ty(*x)) {
                    TypeDef::Array(_, n) => *n,
                    TypeDef::Vector(n) => *n as u32,
                    _ => return Err("internal: a dynamic extract from a non-array".into()),
                };
                if !matches!(self.m.types.get(ty), TypeDef::Scalar(_) | TypeDef::Vector(_)) {
                    return Err("an interval over an array of aggregates indexed by a value that \
                                depends on the input isn't supported"
                        .into());
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
                acc.ok_or("internal: an empty array")?
            }
            Expr::Splat(x, n) => {
                let (lo, hi) = self.range(*x);
                (self.emit(out, ty, Expr::Splat(lo, *n)), self.emit(out, ty, Expr::Splat(hi, *n)))
            }
            Expr::Swizzle(x, c) => {
                let (lo, hi) = self.range(*x);
                (
                    self.emit(out, ty, Expr::Swizzle(lo, c.clone())),
                    self.emit(out, ty, Expr::Swizzle(hi, c.clone())),
                )
            }
            Expr::Convert(x, s) => self.convert(out, *x, *s, ty)?,
            Expr::Bitcast(x, s) => {
                let s = *s;
                self.exact_or_full(out, ty, &[self.range(*x)], &mut |me, out, xs| {
                    me.emit(out, ty, Expr::Bitcast(xs[0], s))
                })
            }
            Expr::Select { cond, if_true, if_false } => {
                let (t, f) = (self.range(*if_true), self.range(*if_false));
                if self.act.values[cond.index()] {
                    let c = self.range(*cond);
                    self.merge(out, ty, c, t, f)?
                } else {
                    (self.select(out, *cond, t.0, f.0), self.select(out, *cond, t.1, f.1))
                }
            }
            Expr::Const(_) | Expr::Zero(_) | Expr::EntryInput(_) | Expr::ArrayLength(_) => {
                return Err("internal: a constant marked active".into());
            }
            Expr::Call(..) => unreachable!("calls are handled in `call_fn`"),
            Expr::Host(..) | Expr::Run(_) | Expr::Addr(_) => {
                return Err("an interval can't go through this".into());
            }
        })
    }

    /// For integers and bit patterns: exact when every operand is a single value, else the
    /// type's whole range. `compute` gets the operands' `lo`s and must not trap.
    fn exact_or_full(
        &mut self,
        out: &mut Block,
        ty: TypeId,
        xs: &[Iv],
        compute: &mut dyn FnMut(&mut Self, &mut Block, &[ValueId]) -> ValueId,
    ) -> Iv {
        let mut single: Option<ValueId> = None;
        for x in xs {
            let eq = self.cmp(out, BinOp::Eq, x.0, x.1);
            single = Some(match single {
                None => eq,
                Some(s) => {
                    let bt = self.bool_ty();
                    self.bin(out, BinOp::And, s, eq, bt)
                }
            });
        }
        let los: Vec<ValueId> = xs.iter().map(|x| x.0).collect();
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
            // Negation and bitwise not reverse the order exactly.
            UnOp::Neg if s.is_float() => {
                (self.emit(out, ty, Expr::Unary(op, hi)), self.emit(out, ty, Expr::Unary(op, lo)))
            }
            UnOp::Not => {
                (self.emit(out, ty, Expr::Unary(op, hi)), self.emit(out, ty, Expr::Unary(op, lo)))
            }
            UnOp::Neg => self.exact_or_full(out, ty, &[(lo, hi)], &mut |me, out, xs| {
                let z = me.emit(out, ty, Expr::Zero(ty));
                me.bin(out, BinOp::WrappingSub, z, xs[0], ty)
            }),
        })
    }

    fn binary(&mut self, out: &mut Block, op: BinOp, a: ValueId, b: ValueId, ty: TypeId) -> R<Iv> {
        let (ta, tb) = (self.ty(a), self.ty(b));
        if [ta, tb, ty].iter().any(|t| matches!(self.m.types.get(*t), TypeDef::Matrix(_))) {
            return Err("an interval over matrix arithmetic isn't supported".into());
        }
        let (ra, rb) = (self.range(a), self.range(b));
        let os = self.elem(ta);
        if op.is_comparison() {
            if self.is_vector(ta) {
                return Err("internal: a vector comparison".into());
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
        let ra = self.coerce_iv(out, ra, ty);
        let rb = self.coerce_iv(out, rb, ty);
        Ok(match op {
            BinOp::Add => {
                let r = (self.bin(out, op, ra.0, rb.0, ty), self.bin(out, op, ra.1, rb.1, ty));
                self.widen1(out, r, ty)
            }
            BinOp::Sub => {
                let r = (self.bin(out, op, ra.0, rb.1, ty), self.bin(out, op, ra.1, rb.0, ty));
                self.widen1(out, r, ty)
            }
            BinOp::Mul if self.same_value(a, b) => {
                let r = self.sqr(out, ra, ty);
                self.widen1(out, r, ty)
            }
            BinOp::Mul => {
                let r = self.mul(out, ra, rb, ty);
                self.widen1(out, r, ty)
            }
            BinOp::Div => self.per_comp(out, ty, &[ra, rb], Self::div)?,
            BinOp::Rem => self.per_comp(out, ty, &[ra, rb], Self::rem)?,
            _ => return Err(format!("internal: {op:?} on floats")),
        })
    }

    /// The four corner products' extremes (unwidened).
    fn mul(&mut self, out: &mut Block, a: Iv, b: Iv, ty: TypeId) -> Iv {
        let p = [
            self.bin(out, BinOp::Mul, a.0, b.0, ty),
            self.bin(out, BinOp::Mul, a.0, b.1, ty),
            self.bin(out, BinOp::Mul, a.1, b.0, ty),
            self.bin(out, BinOp::Mul, a.1, b.1, ty),
        ];
        self.extremes(out, &p, ty)
    }

    fn extremes(&mut self, out: &mut Block, p: &[ValueId], ty: TypeId) -> Iv {
        let mut lo = p[0];
        let mut hi = p[0];
        for &x in &p[1..] {
            lo = self.call(out, Builtin::Min, vec![lo, x], ty);
            hi = self.call(out, Builtin::Max, vec![hi, x], ty);
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
        let m = self.call(out, Builtin::Max, vec![x.0, nh], ty);
        let z = self.fc(out, ty, 0.0);
        let lo = self.call(out, Builtin::Max, vec![m, z], ty);
        let al = self.call(out, Builtin::Abs, vec![x.0], ty);
        let ah = self.call(out, Builtin::Abs, vec![x.1], ty);
        let hi = self.call(out, Builtin::Max, vec![al, ah], ty);
        (lo, hi)
    }

    /// Applies a scalar rule to each component of a vector (scalar operands are shared).
    fn per_comp(&mut self, out: &mut Block, ty: TypeId, args: &[Iv], rule: Rule<'a>) -> R<Iv> {
        let TypeDef::Vector(n) = *self.m.types.get(ty) else {
            return rule(self, out, ty, args);
        };
        let f32t = self.m.types.f32();
        let (mut los, mut his) = (Vec::new(), Vec::new());
        for c in 0..n as u32 {
            let mut comps = Vec::new();
            for a in args {
                comps.push(if self.is_vector(self.ty(a.0)) {
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
        let q = [
            self.bin(out, BinOp::Div, a.0, b.0, ty),
            self.bin(out, BinOp::Div, a.0, b.1, ty),
            self.bin(out, BinOp::Div, a.1, b.0, ty),
            self.bin(out, BinOp::Div, a.1, b.1, ty),
        ];
        let r = self.extremes(out, &q, ty);
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
    fn rem(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        let (a, b) = (x[0], x[1]);
        let bl = self.call(out, Builtin::Abs, vec![b.0], ty);
        let bh = self.call(out, Builtin::Abs, vec![b.1], ty);
        let bm = self.call(out, Builtin::Max, vec![bl, bh], ty);
        let nbm = self.emit(out, ty, Expr::Unary(UnOp::Neg, bm));
        let z = self.fc(out, ty, 0.0);
        let lo_any = self.call(out, Builtin::Max, vec![a.0, nbm], ty);
        let hi_any = self.call(out, Builtin::Min, vec![a.1, bm], ty);
        let nonneg = self.cmp(out, BinOp::Ge, a.0, z);
        let nonpos = self.cmp(out, BinOp::Le, a.1, z);
        let lo = self.select(out, nonneg, z, lo_any);
        let hi = self.select(out, nonpos, z, hi_any);
        // Rounding in `trunc(a / b)` can put the result just past either end, by about an ulp
        // of `a`.
        let al = self.call(out, Builtin::Abs, vec![a.0], ty);
        let ah = self.call(out, Builtin::Abs, vec![a.1], ty);
        let am = self.call(out, Builtin::Max, vec![al, ah], ty);
        let mag = self.bin(out, BinOp::Add, am, bm, ty);
        let ulps = if self.gpu() { 16.0 } else { 4.0 };
        Ok(self.widen_mag(out, (lo, hi), ty, (mag, mag), ulps, 0.0))
    }

    fn compare(&mut self, out: &mut Block, op: BinOp, a: Iv, b: Iv, s: Scalar) -> Iv {
        let ordered = s != Scalar::Bool;
        match op {
            BinOp::Lt => (self.cmp(out, op, a.1, b.0), self.cmp(out, op, a.0, b.1)),
            BinOp::Le => (self.cmp(out, op, a.1, b.0), self.cmp(out, op, a.0, b.1)),
            BinOp::Gt => (self.cmp(out, op, a.0, b.1), self.cmp(out, op, a.1, b.0)),
            BinOp::Ge => (self.cmp(out, op, a.0, b.1), self.cmp(out, op, a.1, b.0)),
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
                let v = self.call(out, Builtin::Clamp, vec![v, cl, ch], xt);
                ends.push(conv(self, out, v));
            }
            return Ok((ends[0], ends[1]));
        }
        Ok(self.exact_or_full(out, ty, &[r], &mut |me, out, xs| conv(me, out, xs[0])))
    }

    fn builtin(&mut self, out: &mut Block, b: Builtin, args: &[ValueId], ty: TypeId) -> R<Iv> {
        use Builtin as B;
        let r: Vec<Iv> = args.iter().map(|a| self.range(*a)).collect();
        let s = self.elem(ty);
        if s.is_int() {
            return Ok(match b {
                B::Min | B::Max | B::Clamp => self.monotone(out, b, &r, ty),
                _ => {
                    let args = r.clone();
                    self.exact_or_full(out, ty, &args, &mut |me, out, xs| {
                        me.call(out, b, xs.to_vec(), ty)
                    })
                }
            });
        }
        let gpu = self.gpu();
        Ok(match b {
            B::Floor | B::Ceil | B::Round | B::Trunc | B::Sign | B::Saturate => {
                self.monotone(out, b, &r, ty)
            }
            B::Min | B::Max | B::Clamp => {
                let r: Vec<Iv> = r.iter().map(|x| self.coerce_iv(out, *x, ty)).collect();
                self.monotone(out, b, &r, ty)
            }
            B::Step => {
                // 1 where edge <= x: up in x, down in the edge.
                let lo = self.call(out, B::Step, vec![r[0].1, r[1].0], ty);
                let hi = self.call(out, B::Step, vec![r[0].0, r[1].1], ty);
                (lo, hi)
            }
            B::Abs => self.abs(out, r[0], ty),
            B::Sqrt => {
                let z = self.fc(out, ty, 0.0);
                let l = self.call(out, B::Max, vec![r[0].0, z], ty);
                let h = self.call(out, B::Max, vec![r[0].1, z], ty);
                let x =
                    (self.call(out, B::Sqrt, vec![l], ty), self.call(out, B::Sqrt, vec![h], ty));
                // WGSL: inherited from 1 / inverseSqrt (2 + 2.5 ulp).
                let ulps = if gpu { 10.0 } else { 1.0 };
                self.widen(out, x, ty, ulps, 0.0)
            }
            B::InverseSqrt => self.per_comp(out, ty, &r, Self::inverse_sqrt)?,
            B::Exp | B::Exp2 => {
                let x = self.monotone(out, b, &r, ty);
                if gpu {
                    self.exp_gpu_widen(out, x, r[0], ty)
                } else {
                    self.widen(out, x, ty, CPU_STD_ULPS, 0.0)
                }
            }
            B::Log => self.per_comp(out, ty, &r, Self::log)?,
            B::Log2 => self.per_comp(out, ty, &r, Self::log2)?,
            B::Pow => self.per_comp(out, ty, &r, Self::pow)?,
            B::Sin | B::Cos => self.per_comp(out, ty, &r, Self::sin_cos_rule(b))?,
            B::Tan => self.per_comp(out, ty, &r, Self::tan)?,
            B::Asin | B::Acos | B::Atan => {
                let one = self.fc(out, ty, 1.0);
                let neg = self.fc(out, ty, -1.0);
                let x = if b == B::Atan {
                    r[0]
                } else {
                    (
                        self.call(out, B::Clamp, vec![r[0].0, neg, one], ty),
                        self.call(out, B::Clamp, vec![r[0].1, neg, one], ty),
                    )
                };
                let fl = self.call(out, b, vec![x.0], ty);
                let fh = self.call(out, b, vec![x.1], ty);
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
                let a = self.coerce_iv(out, r[0], at);
                let bb = self.coerce_iv(out, r[1], at);
                let d = (
                    self.bin(out, BinOp::Sub, a.0, bb.1, at),
                    self.bin(out, BinOp::Sub, a.1, bb.0, at),
                );
                let d = self.widen1(out, d, at);
                self.length(out, d, at, ty)
            }
            B::Dot => {
                let at = self.ty(args[0]);
                let TypeDef::Vector(n) = *self.m.types.get(at) else {
                    return Err("internal: a dot of non-vectors".into());
                };
                // dot(v, v) is a sum of squares.
                let p = if self.same_value(args[0], args[1]) {
                    self.sqr(out, r[0], at)
                } else {
                    self.mul(out, r[0], r[1], at)
                };
                let lo = self.sum(out, p.0, n, ty);
                let hi = self.sum(out, p.1, n, ty);
                let ml = self.call(out, B::Abs, vec![p.0], at);
                let mh = self.call(out, B::Abs, vec![p.1], at);
                let m = self.call(out, B::Max, vec![ml, mh], at);
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
            B::Normalize => {
                let f32t = self.m.types.f32();
                let len = self.length(out, r[0], ty, f32t);
                let len = self.coerce_iv(out, len, ty);
                self.per_comp(out, ty, &[r[0], len], Self::div)?
            }
            B::AllEqual => {
                let bt = self.bool_ty();
                let (a, bb) = (r[0], r[1]);
                let sa = self.call(out, B::AllEqual, vec![a.0, a.1], bt);
                let sb = self.call(out, B::AllEqual, vec![bb.0, bb.1], bt);
                let same = self.call(out, B::AllEqual, vec![a.0, bb.0], bt);
                let x = self.bin(out, BinOp::And, sa, sb, bt);
                let lo = self.bin(out, BinOp::And, x, same, bt);
                let hi = self.emit(out, bt, Expr::Const(Const::Bool(true)));
                (lo, hi)
            }
            B::Dpdx | B::Dpdy | B::Fwidth => {
                return Err("an interval of a screen-space derivative isn't supported".into());
            }
        })
    }

    /// A builtin that's non-decreasing in every argument and exact: applied to the `lo`s and
    /// to the `hi`s.
    fn monotone(&mut self, out: &mut Block, b: Builtin, r: &[Iv], ty: TypeId) -> Iv {
        let lo = self.call(out, b, r.iter().map(|x| x.0).collect(), ty);
        let hi = self.call(out, b, r.iter().map(|x| x.1).collect(), ty);
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
        let TypeDef::Vector(n) = *self.m.types.get(at) else {
            return self.abs(out, x, ty);
        };
        let sq = self.sqr(out, x, at);
        let lo = self.sum(out, sq.0, n, ty);
        let hi = self.sum(out, sq.1, n, ty);
        let r = (
            self.call(out, Builtin::Sqrt, vec![lo], ty),
            self.call(out, Builtin::Sqrt, vec![hi], ty),
        );
        // Sums of squares don't cancel; the error is relative. WGSL: sqrt(dot(e, e)).
        let ulps = if self.gpu() { 16.0 } else { 4.0 };
        self.widen(out, r, ty, ulps, 0.0)
    }

    fn inverse_sqrt(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        let x = x[0];
        let z = self.fc(out, ty, 0.0);
        let l = self.call(out, Builtin::Max, vec![x.0, z], ty);
        let h = self.call(out, Builtin::Max, vec![x.1, z], ty);
        let lo = self.call(out, Builtin::InverseSqrt, vec![h], ty);
        let hi = self.call(out, Builtin::InverseSqrt, vec![l], ty);
        // At zero it's unbounded.
        let pos = self.cmp(out, BinOp::Gt, x.0, z);
        let (_, fh) = self.full(out, ty);
        let hi = self.select(out, pos, hi, fh);
        let ulps = if self.gpu() { 4.0 } else { 2.0 };
        Ok(self.widen(out, (lo, hi), ty, ulps, 0.0))
    }

    fn log(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        Ok(self.log_ends(out, Builtin::Log, x[0], ty))
    }

    fn log2(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        Ok(self.log_ends(out, Builtin::Log2, x[0], ty))
    }

    fn pow(&mut self, out: &mut Block, ty: TypeId, x: &[Iv]) -> R<Iv> {
        let (a, b) = (x[0], x[1]);
        let z = self.fc(out, ty, 0.0);
        let pos = self.cmp(out, BinOp::Ge, a.0, z);
        let r = if self.gpu() {
            // WGSL: inherited from exp2(y log2(x)); bound each step.
            let l = self.log_ends(out, Builtin::Log2, a, ty);
            let p = self.mul(out, b, l, ty);
            let p = self.widen1(out, p, ty);
            let e = (
                self.call(out, Builtin::Exp2, vec![p.0], ty),
                self.call(out, Builtin::Exp2, vec![p.1], ty),
            );
            self.exp_gpu_widen(out, e, p, ty)
        } else {
            // `y log x` is bilinear in (y, log x), so the extremes are at the corners.
            let c = [
                self.call(out, Builtin::Pow, vec![a.0, b.0], ty),
                self.call(out, Builtin::Pow, vec![a.0, b.1], ty),
                self.call(out, Builtin::Pow, vec![a.1, b.0], ty),
                self.call(out, Builtin::Pow, vec![a.1, b.1], ty),
            ];
            let e = self.extremes(out, &c, ty);
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
            let l = self.call(out, b, vec![v], ty);
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

    /// Widening for `exp` and `exp2` on the GPU (WGSL: 3 + 2|x| ulp), `x` the argument's range.
    fn exp_gpu_widen(&mut self, out: &mut Block, e: Iv, x: Iv, ty: TypeId) -> Iv {
        let al = self.call(out, Builtin::Abs, vec![x.0], ty);
        let ah = self.call(out, Builtin::Abs, vec![x.1], ty);
        let m = self.call(out, Builtin::Max, vec![al, ah], ty);
        let four = self.fc(out, ty, 4.0 * F32_EPS);
        let six = self.fc(out, ty, 6.0 * F32_EPS);
        let k = self.bin(out, BinOp::Mul, m, four, ty);
        let k = self.bin(out, BinOp::Add, k, six, ty);
        let ml = self.call(out, Builtin::Abs, vec![e.0], ty);
        let mh = self.call(out, Builtin::Abs, vec![e.1], ty);
        self.widen_k(out, e, ty, (ml, mh), k, 0.0)
    }

    fn sin_cos_rule(b: Builtin) -> Rule<'a> {
        if b == Builtin::Sin { Self::sin } else { Self::cos }
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
        let e0 = self.call(out, b, vec![x.0], ty);
        let e1 = self.call(out, b, vec![x.1], ty);
        let (lo, hi) = self.extremes(out, &[e0, e1], ty);
        // Slack for the test, so a near miss counts as inside (sound: ±1 is a bound).
        let al = self.call(out, Builtin::Abs, vec![x.0], ty);
        let ah = self.call(out, Builtin::Abs, vec![x.1], ty);
        let m = self.call(out, Builtin::Max, vec![al, ah], ty);
        let k = self.fc(out, ty, (2f64).powi(-18));
        let c = self.fc(out, ty, (2f64).powi(-10));
        let slack = self.bin(out, BinOp::Mul, m, k, ty);
        let slack = self.bin(out, BinOp::Add, slack, c, ty);
        let xl = self.bin(out, BinOp::Sub, x.0, slack, ty);
        let xh = self.bin(out, BinOp::Add, x.1, slack, ty);
        let mut hits = Vec::new();
        for at in [peak, trough] {
            // Is there an integer n with xl <= at + τn <= xh?
            let a = self.fc(out, ty, at);
            let inv = self.fc(out, ty, 1.0 / tau);
            let dl = self.bin(out, BinOp::Sub, xl, a, ty);
            let dl = self.bin(out, BinOp::Mul, dl, inv, ty);
            let nl = self.call(out, Builtin::Ceil, vec![dl], ty);
            let dh = self.bin(out, BinOp::Sub, xh, a, ty);
            let dh = self.bin(out, BinOp::Mul, dh, inv, ty);
            let nh = self.call(out, Builtin::Floor, vec![dh], ty);
            hits.push(self.cmp(out, BinOp::Le, nl, nh));
        }
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
        let e = (
            self.call(out, Builtin::Tan, vec![x.0], ty),
            self.call(out, Builtin::Tan, vec![x.1], ty),
        );
        let e = self.widen(out, e, ty, CPU_STD_ULPS, 0.0);
        let k = self.fc(out, ty, (2f64).powi(-10));
        let xl = self.bin(out, BinOp::Sub, x.0, k, ty);
        let xh = self.bin(out, BinOp::Add, x.1, k, ty);
        let a = self.fc(out, ty, pi / 2.0);
        let inv = self.fc(out, ty, 1.0 / pi);
        let dl = self.bin(out, BinOp::Sub, xl, a, ty);
        let dl = self.bin(out, BinOp::Mul, dl, inv, ty);
        let nl = self.call(out, Builtin::Ceil, vec![dl], ty);
        let dh = self.bin(out, BinOp::Sub, xh, a, ty);
        let dh = self.bin(out, BinOp::Mul, dh, inv, ty);
        let nh = self.call(out, Builtin::Floor, vec![dh], ty);
        let pole = self.cmp(out, BinOp::Le, nl, nh);
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
        let fl = self.call(out, Builtin::Floor, vec![x.0], ty);
        let fh = self.call(out, Builtin::Floor, vec![x.1], ty);
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
            self.call(out, Builtin::Mix, vec![a.0, b.0, t.0], ty),
            self.call(out, Builtin::Mix, vec![a.0, b.0, t.1], ty),
            self.call(out, Builtin::Mix, vec![a.1, b.1, t.0], ty),
            self.call(out, Builtin::Mix, vec![a.1, b.1, t.1], ty),
        ];
        let lo = self.call(out, Builtin::Min, vec![c[0], c[1]], ty);
        let hi = self.call(out, Builtin::Max, vec![c[2], c[3]], ty);
        // a (1 - t) + b t rounds three times, relative to the terms it adds; a GPU may compute
        // a + t (b - a) instead, whose error also grows with |b - a| t. So the magnitude is
        // |a| (1 - t) + (|a| + |b|) t: just |a| at t = 0, where `smin` against a huge value
        // starts its fold.
        let m = [a.0, a.1, b.0, b.1].map(|v| self.call(out, Builtin::Abs, vec![v], ty));
        let ma = self.call(out, Builtin::Max, vec![m[0], m[1]], ty);
        let mb = self.call(out, Builtin::Max, vec![m[2], m[3]], ty);
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
        let lo = self.call(out, Builtin::Smoothstep, vec![e0.1, e1.1, v.0], ty);
        let hi = self.call(out, Builtin::Smoothstep, vec![e0.0, e1.0, v.1], ty);
        let ok = self.cmp(out, BinOp::Lt, e0.1, e1.0);
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
