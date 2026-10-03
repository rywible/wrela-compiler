//! Forward-mode differentiation (D-012). An active value of type `T` carries `n` tangents, also
//! of type `T`: its derivative along each of the input's `n` components. A derived function takes
//! a tangent for each active parameter (appended after the originals, `n` per parameter) and,
//! when its result is active, returns `{value, d0, ..., d(n-1)}`.

use super::*;
use std::collections::HashMap;

fn dims(m: &Module, t: TypeId) -> Result<u8> {
    match m.types.get(t) {
        TypeDef::Scalar(Scalar::F32) => Ok(1),
        TypeDef::Vector(n) => Ok(*n),
        _ => Err(Error::not_derivable(format!(
            "a gradient needs an `f32` or a float vector, not `{}`",
            m.types.display(t)
        ))),
    }
}

pub(super) fn value_and_gradient(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    ncap: u32,
    target: Target,
) -> Result<FuncId> {
    let func = m.functions[f.index()].clone();
    let xi = ncap as usize;
    let x_ty = func.params.get(xi).ok_or_else(|| Error::internal("no input parameter"))?.ty;
    let n = dims(m, x_ty)?;
    if func.ret != Some(m.types.f32()) || func.ret_ref {
        return Err(Error::not_derivable("a gradient needs a function that returns an `f32`"));
    }
    let mut mask = vec![false; func.params.len()];
    mask[xi] = true;
    let d = derive(m, cache, f, mask.clone(), n, target)?;
    let returns = returns_active(m, cache, f, mask, Mode::Ad)?;
    let f32 = m.types.f32();
    let tuple = m.types.intern(TypeDef::Struct {
        name: format!("(f32, {})", m.types.display(x_ty)),
        fields: vec![("_0".into(), f32), ("_1".into(), x_ty)],
    });
    let mut g = Function::new(
        format!("{}_value_and_gradient", func.name),
        func.params.clone(),
        Some(tuple),
    );
    let mut body = Vec::new();
    let mut args: Vec<Arg> =
        (0..func.params.len() as u32).map(|i| g.param_arg(i, &mut body)).collect();
    // Seeds: the unit vectors along each component of x, passed the way `x` is (a tangent
    // parameter is passed like its original).
    let x_by_ref = func.params[xi].by_ref;
    for k in 0..n {
        let one = g.new_value(f32);
        body.push(Stmt::Let(one, Expr::Const(Const::F32(1.0))));
        let seed = if n == 1 {
            one
        } else {
            let zero = g.new_value(f32);
            body.push(Stmt::Let(zero, Expr::Const(Const::F32(0.0))));
            let comps = (0..n).map(|c| if c == k { one } else { zero }).collect();
            let e = g.new_value(x_ty);
            body.push(Stmt::Let(e, Expr::Construct(x_ty, comps)));
            e
        };
        if x_by_ref {
            let l = g.new_local(format!("seed{k}"), x_ty);
            body.push(Stmt::Store(Place::local(l), seed));
            args.push(Arg::Place(Place::local(l)));
        } else {
            args.push(Arg::Value(seed));
        }
    }
    let dual_ty = m.functions[d.index()]
        .ret
        .ok_or_else(|| Error::internal("derived function returns nothing"))?;
    let r = g.new_value(dual_ty);
    body.push(Stmt::Let(r, Expr::Call(d, args)));
    let (value, parts) = if returns {
        let v = g.new_value(f32);
        body.push(Stmt::Let(v, Expr::Extract(r, 0)));
        let mut parts = Vec::new();
        for k in 0..n {
            let p = g.new_value(f32);
            body.push(Stmt::Let(p, Expr::Extract(r, 1 + k as u32)));
            parts.push(p);
        }
        (v, parts)
    } else {
        // The function doesn't depend on its input: the gradient is zero.
        let z = g.new_value(f32);
        body.push(Stmt::Let(z, Expr::Const(Const::F32(0.0))));
        (r, vec![z; n as usize])
    };
    let grad = if n == 1 {
        parts[0]
    } else {
        let gv = g.new_value(x_ty);
        body.push(Stmt::Let(gv, Expr::Construct(x_ty, parts)));
        gv
    };
    let out = g.new_value(tuple);
    body.push(Stmt::Let(out, Expr::Construct(tuple, vec![value, grad])));
    body.push(Stmt::Return(Some(out)));
    g.body = body;
    Ok(m.add_function(g))
}

/// The derivative of `f` with active parameters `mask` and `n` directions.
pub(super) fn derive(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    mask: Vec<bool>,
    n: u8,
    target: Target,
) -> Result<FuncId> {
    if let Some(&d) = cache.ad.get(&(f, mask.clone(), n)) {
        return Ok(d);
    }
    let func = m.functions[f.index()].clone();
    if let Some(at) = records_gpu_work(&func.body, None) {
        return Err(Error::not_derivable(
            "this records GPU work, which a derived function can't do",
        )
        .at(at));
    }
    let returns = returns_active(m, cache, f, mask.clone(), Mode::Ad)?;
    let act = body_activity(m, cache, f, &mask, Mode::Ad)?;
    for (i, p) in func.params.iter().enumerate() {
        if act.params[i] && !mask[i] && p.mutable {
            // Placed by the caller's statement (the call), or at the `gradient(..)` call.
            return Err(Error::not_derivable(format!(
                "this would write a derivative through the `mut` parameter `{}`",
                p.name
            )));
        }
    }
    let mut nf = func.clone_signature();
    nf.name = format!("{}_d{}", func.name, n);
    // Tangent parameters, appended: n per active parameter.
    let mut tparam: Vec<Vec<u32>> = vec![Vec::new(); func.params.len()];
    for (i, p) in func.params.iter().enumerate() {
        if mask[i] {
            for k in 0..n {
                tparam[i].push(nf.params.len() as u32);
                nf.params.push(Param {
                    name: format!("{}_d{k}", p.name),
                    ty: p.ty,
                    by_ref: p.by_ref,
                    mutable: p.mutable,
                });
            }
        }
    }
    let mut tlocal: Vec<Vec<LocalId>> = vec![Vec::new(); func.locals.len()];
    for (l, decl) in func.locals.iter().enumerate() {
        if act.locals[l] {
            for k in 0..n {
                tlocal[l].push(nf.new_local(format!("{}_d{k}", decl.name), decl.ty));
            }
        }
    }
    if returns {
        let r = func.ret.ok_or_else(|| Error::internal("a missing result type"))?;
        // A projection's: a pointer into the place, and one into each tangent's.
        let r = if func.ret_ref { m.types.intern(TypeDef::Ptr(r)) } else { r };
        nf.ret_ref = false;
        let mut fields = vec![("v".to_string(), r)];
        for k in 0..n {
            fields.push((format!("d{k}"), r));
        }
        nf.ret =
            Some(m.types.intern(TypeDef::Struct {
                name: format!("dual{n}<{}>", m.types.display(r)),
                fields,
            }));
    }
    // With its signature, for a recursive call to read.
    let id = m.add_function(Function::new(nf.name.clone(), nf.params.clone(), nf.ret));
    // Cached before its body is built, for calls back into it; removed again if building
    // fails, so a later request doesn't find the bodiless placeholder.
    let key = (f, mask, n);
    cache.ad.insert(key.clone(), id);
    let mut b = Ad {
        m,
        cache,
        f: &func,
        nf,
        act,
        n,
        target,
        tparam,
        tlocal,
        tv: HashMap::new(),
        returns,
        recips: HashMap::new(),
        made: Vec::new(),
        scaled: HashMap::new(),
        track: Track::default(),
    };
    let body = match b.block(&func.body) {
        Ok(body) => body,
        Err(e) => {
            b.cache.ad.remove(&key);
            return Err(e);
        }
    };
    let mut nf = b.nf;
    nf.body = body;
    m.functions[id.index()] = nf;
    Ok(id)
}

struct Ad<'a> {
    m: &'a mut Module,
    cache: &'a mut DeriveCache,
    f: &'a Function,
    nf: Function,
    act: Activity,
    n: u8,
    target: Target,
    tparam: Vec<Vec<u32>>,
    tlocal: Vec<Vec<LocalId>>,
    /// Each active value's tangents (`None`: structurally zero).
    tv: HashMap<ValueId, Vec<Option<ValueId>>>,
    returns: bool,
    /// Reciprocals made while differentiating one statement, shared by its `n` directions:
    /// one division and `n` multiplications instead of `n` divisions.
    recips: HashMap<ValueId, ValueId>,
    /// The expressions made while differentiating one statement, so what its directions share
    /// (`2v` for a square root, a clamp's masks) is made once.
    made: Vec<(TypeId, Expr, ValueId)>,
    /// Tangents made as another tangent times a factor every direction shares (see `scale`).
    scaled: HashMap<ValueId, (ValueId, ValueId)>,
    track: Track,
}

impl Builder for Ad<'_> {
    fn emit(&mut self, out: &mut Block, ty: TypeId, e: Expr) -> ValueId {
        if let Some((_, _, v)) = self.made.iter().find(|(t, x, _)| *t == ty && *x == e) {
            return *v;
        }
        let v = self.nf.new_value(ty);
        out.push(Stmt::Let(v, e.clone()));
        if !matches!(e, Expr::Call(..) | Expr::Host(..)) {
            self.made.push((ty, e, v));
        }
        v
    }

    fn types(&self) -> &Types {
        &self.m.types
    }

    fn ty(&self, v: ValueId) -> TypeId {
        self.nf.value_ty(v)
    }
}

impl Ad<'_> {
    /// A float constant of `ty`'s element type (`f32` or `f64`), as a scalar: operations on
    /// `ty` splat it.
    fn fc(&mut self, out: &mut Block, ty: TypeId, x: f64) -> ValueId {
        let (t, c) = match self.m.types.element_scalar(ty) {
            Some(Scalar::F64) => (self.m.types.scalar(Scalar::F64), Const::F64(x)),
            _ => (self.m.types.f32(), Const::F32(x as f32)),
        };
        self.emit(out, t, Expr::Const(c))
    }

    fn t(&self, v: ValueId, k: usize) -> Option<ValueId> {
        self.tv.get(&v).and_then(|ts| ts[k])
    }

    /// The tangent `t` as an operand of type `ty`, or a zero for a structurally zero one.
    fn tangent_or_zero(&mut self, out: &mut Block, t: Option<ValueId>, ty: TypeId) -> ValueId {
        match t {
            Some(t) => self.coerce(out, t, ty),
            None => self.zero(out, ty),
        }
    }

    /// The tangent `t` times `f`, a factor every direction shares. A tangent made that way,
    /// `b × g`, becomes `b × (g f)`, with `g f` made once for all directions: a chain of
    /// factors (`y y l2`, `sqrt(s) / l2`) costs one multiplication per direction, not one each.
    fn scale(&mut self, out: &mut Block, t: ValueId, f: ValueId, ty: TypeId) -> ValueId {
        let matrix = |me: &Self, v: ValueId| matches!(me.m.types.get(me.ty(v)), TypeDef::Matrix(_));
        if matrix(self, t) || matrix(self, f) {
            return self.bin(out, BinOp::Mul, t, f, ty);
        }
        if let Some(&(b, g)) = self.scaled.get(&t)
            && self.ty(g) == self.ty(f)
        {
            let gt = self.ty(g);
            let gf = self.bin(out, BinOp::Mul, g, f, gt);
            let r = self.bin(out, BinOp::Mul, b, gf, ty);
            self.scaled.insert(r, (b, gf));
            return r;
        }
        let r = self.bin(out, BinOp::Mul, t, f, ty);
        self.scaled.insert(r, (t, f));
        r
    }

    /// `x / d` as `x × (1 / d)`, with `1 / d` made once per statement.
    fn div(&mut self, out: &mut Block, x: ValueId, d: ValueId, ty: TypeId) -> ValueId {
        let r = match self.recips.get(&d) {
            Some(&r) => r,
            None => {
                let dt = self.ty(d);
                let one = self.fc(out, dt, 1.0);
                let one = self.coerce(out, one, dt);
                let r = self.bin(out, BinOp::Div, one, d, dt);
                self.recips.insert(d, r);
                r
            }
        };
        self.scale(out, x, r, ty)
    }

    /// `x / d` for a vector's length `d`, with `d` raised to at least 10⁻³⁰. Where a length is
    /// zero, `x` (its vector dotted with a tangent) is zero too: inside a box,
    /// `length(max(q, 0))` is zero and moves in no direction, and its tangent is then zero, not
    /// 0/0. One `max` per statement, shared by its directions. (`sqrt` keeps a plain division:
    /// guarding it measurably slowed the grazer's gradient kernel, and its 0/0 happens only at
    /// isolated points, such as a round cone's axis.)
    fn div_from_zero(&mut self, out: &mut Block, x: ValueId, d: ValueId, ty: TypeId) -> ValueId {
        let dt = self.ty(d);
        let floor = self.fc(out, dt, 1e-30);
        let d = self.builtin(out, Builtin::Max, vec![d, floor], dt);
        self.div(out, x, d, ty)
    }

    /// `x + y` where either may be zero.
    fn add(
        &mut self,
        out: &mut Block,
        x: Option<ValueId>,
        y: Option<ValueId>,
        ty: TypeId,
    ) -> Option<ValueId> {
        match (x, y) {
            (None, None) => None,
            (Some(a), None) | (None, Some(a)) => Some(a),
            (Some(a), Some(b)) => Some(self.bin(out, BinOp::Add, a, b, ty)),
        }
    }

    fn sub(
        &mut self,
        out: &mut Block,
        x: Option<ValueId>,
        y: Option<ValueId>,
        ty: TypeId,
    ) -> Option<ValueId> {
        match (x, y) {
            (None, None) => None,
            (Some(a), None) => Some(a),
            (None, Some(b)) => {
                let b = self.coerce(out, b, ty);
                Some(self.emit(out, ty, Expr::Unary(UnOp::Neg, b)))
            }
            (Some(a), Some(b)) => Some(self.bin(out, BinOp::Sub, a, b, ty)),
        }
    }

    /// `x * y` where `x` may be zero.
    fn mul(
        &mut self,
        out: &mut Block,
        x: Option<ValueId>,
        y: ValueId,
        ty: TypeId,
    ) -> Option<ValueId> {
        x.map(|x| self.scale(out, x, y, ty))
    }

    fn place(&self, p: &Place, k: usize) -> Result<Place> {
        let root = match &p.root {
            PlaceRoot::Local(l) => PlaceRoot::Local(
                *self.tlocal[l.index()]
                    .get(k)
                    .ok_or_else(|| Error::internal("an inactive local's tangent"))?,
            ),
            PlaceRoot::Param(i) => PlaceRoot::Param(
                *self.tparam[*i as usize]
                    .get(k)
                    .ok_or_else(|| Error::internal("an inactive parameter's tangent"))?,
            ),
            // A projection's tangent points into the place's tangent.
            PlaceRoot::Ptr(v) => PlaceRoot::Ptr(
                self.t(*v, k).ok_or_else(|| Error::internal("an inactive pointer's tangent"))?,
            ),
            _ => return Err(Error::not_derivable("can't differentiate through this place")),
        };
        Ok(Place { root, path: p.path.clone() })
    }

    fn zero(&mut self, out: &mut Block, ty: TypeId) -> ValueId {
        self.emit(out, ty, Expr::Zero(ty))
    }

    fn block(&mut self, b: &Block) -> Result<Block> {
        let mut out = Vec::new();
        for s in b {
            self.stmt(s, &mut out)?;
        }
        Ok(out)
    }

    fn stmt(&mut self, s: &Stmt, out: &mut Block) -> Result<()> {
        // What one statement shares among its directions stays within it.
        self.made.clear();
        self.recips.clear();
        self.track.before(s);
        let r = self.stmt_inner(s, out);
        self.track.after(s, r)
    }

    fn stmt_inner(&mut self, s: &Stmt, out: &mut Block) -> Result<()> {
        match s {
            Stmt::Let(v, Expr::Call(g, args)) => self.call(Some(*v), *g, args, out),
            Stmt::Eval(Expr::Call(g, args)) => self.call(None, *g, args, out),
            Stmt::Let(v, e) => {
                out.push(s.clone());
                if self.act.values[v.index()] {
                    let mut ts = Vec::new();
                    for k in 0..self.n as usize {
                        ts.push(self.tangent(*v, e, k, out)?);
                    }
                    self.tv.insert(*v, ts);
                }
                Ok(())
            }
            Stmt::Store(p, v) => {
                out.push(s.clone());
                if place_root_active(&self.act, p) && !matches!(p.root, PlaceRoot::Resource(_)) {
                    let ty = place_type(self.m, self.f, p)?;
                    for k in 0..self.n as usize {
                        let tp = self.place(p, k)?;
                        let tv = self.tangent_or_zero(out, self.t(*v, k), ty);
                        out.push(Stmt::Store(tp, tv));
                    }
                }
                Ok(())
            }
            Stmt::Return(Some(v)) if self.returns => {
                let r = self.ty(*v);
                let mut parts = vec![*v];
                for k in 0..self.n as usize {
                    let t = self.tangent_or_zero(out, self.t(*v, k), r);
                    parts.push(t);
                }
                let dual = self.nf.ret.ok_or_else(|| Error::internal("a missing result type"))?;
                let d = self.emit(out, dual, Expr::Construct(dual, parts));
                out.push(Stmt::Return(Some(d)));
                Ok(())
            }
            other => pass_through(other, out, &mut |b| self.block(b)),
        }
    }

    fn call(&mut self, v: Option<ValueId>, g: FuncId, args: &[Arg], out: &mut Block) -> Result<()> {
        let mask: Vec<bool> = args.iter().map(|a| arg_active(&self.act, a)).collect();
        if !mask.iter().any(|b| *b) {
            out.push(match v {
                Some(v) => Stmt::Let(v, Expr::Call(g, args.to_vec())),
                None => Stmt::Eval(Expr::Call(g, args.to_vec())),
            });
            return Ok(());
        }
        let dg = derive(self.m, self.cache, g, mask.clone(), self.n, self.target)?;
        let returns = returns_active(self.m, self.cache, g, mask.clone(), Mode::Ad)?;
        let mut new_args = args.to_vec();
        let callee_params = self.m.functions[g.index()].params.clone();
        for (i, a) in args.iter().enumerate() {
            if !mask[i] {
                continue;
            }
            for k in 0..self.n as usize {
                new_args.push(match a {
                    Arg::Value(x) => {
                        let ty = callee_params[i].ty;
                        Arg::Value(self.tangent_or_zero(out, self.t(*x, k), ty))
                    }
                    Arg::Place(p) => Arg::Place(self.place(p, k)?),
                });
            }
        }
        match (v, returns) {
            (Some(v), true) => {
                let dual = self.m.functions[dg.index()]
                    .ret
                    .ok_or_else(|| Error::internal("a missing result type"))?;
                let r = self.emit(out, dual, Expr::Call(dg, new_args));
                out.push(Stmt::Let(v, Expr::Extract(r, 0)));
                let ty = self.ty(v);
                let mut ts = Vec::new();
                for k in 0..self.n as usize {
                    ts.push(Some(self.emit(out, ty, Expr::Extract(r, 1 + k as u32))));
                }
                self.tv.insert(v, ts);
            }
            (Some(v), false) => out.push(Stmt::Let(v, Expr::Call(dg, new_args))),
            (None, _) => out.push(Stmt::Eval(Expr::Call(dg, new_args))),
        }
        Ok(())
    }

    /// The `k`th tangent of `v = e`.
    fn tangent(
        &mut self,
        v: ValueId,
        e: &Expr,
        k: usize,
        out: &mut Block,
    ) -> Result<Option<ValueId>> {
        let ty = self.ty(v);
        if !self.m.types.has_float(ty) {
            return Ok(None);
        }
        let t = |s: &Self, x: &ValueId| s.t(*x, k);
        Ok(match e {
            Expr::Const(_)
            | Expr::Zero(_)
            | Expr::EntryInput(_)
            | Expr::Bitcast(..)
            | Expr::ArrayLength(_) => None,
            Expr::Param(i) => {
                self.tparam[*i as usize].get(k).copied().map(|p| self.emit(out, ty, Expr::Param(p)))
            }
            Expr::Load(p) => {
                if place_root_active(&self.act, p) && !matches!(p.root, PlaceRoot::Resource(_)) {
                    let tp = self.place(p, k)?;
                    Some(self.emit(out, ty, Expr::Load(tp)))
                } else {
                    None
                }
            }
            Expr::Unary(UnOp::Neg, x) => {
                t(self, x).map(|dx| self.emit(out, ty, Expr::Unary(UnOp::Neg, dx)))
            }
            Expr::Unary(UnOp::Not, _) => None,
            Expr::Binary(op, a, b) => {
                let (da, db) = (t(self, a), t(self, b));
                match op {
                    BinOp::Add => self.add(out, da, db, ty),
                    BinOp::Sub => self.sub(out, da, db, ty),
                    BinOp::Mul
                        if self.track.same_value(*a, *b)
                            && !matches!(self.m.types.get(self.ty(*a)), TypeDef::Matrix(_)) =>
                    {
                        // d(a²) = 2 a da (a matrix product doesn't commute: dM M + M dM)
                        da.map(|da| {
                            let two = self.fc(out, ty, 2.0);
                            let t = self.bin(out, BinOp::Mul, *a, two, ty);
                            self.scale(out, da, t, ty)
                        })
                    }
                    BinOp::Mul => {
                        let x = self.mul(out, da, *b, ty);
                        // A matrix product keeps its order (`scale` sees to that).
                        let y = db.map(|db| {
                            if matches!(self.m.types.get(self.ty(*a)), TypeDef::Matrix(_)) {
                                self.bin(out, BinOp::Mul, *a, db, ty)
                            } else {
                                self.scale(out, db, *a, ty)
                            }
                        });
                        self.add(out, x, y, ty)
                    }
                    BinOp::Div => {
                        // (da - v db) / b
                        let vdb = db.map(|db| self.scale(out, db, v, ty));
                        let num = self.sub(out, da, vdb, ty);
                        num.map(|n| self.div(out, n, *b, ty))
                    }
                    BinOp::Rem => {
                        // a - b trunc(a / b): da - db trunc(a / b)
                        let q = self.bin(out, BinOp::Div, *a, *b, ty);
                        let q = self.builtin(out, Builtin::Trunc, vec![q], ty);
                        let dbq = db.map(|db| self.scale(out, db, q, ty));
                        self.sub(out, da, dbq, ty)
                    }
                    _ => None,
                }
            }
            Expr::Builtin(b, args) => self.builtin_tangent(v, *b, args, k, ty, out)?,
            Expr::Construct(cty, parts) => {
                let ts: Vec<Option<ValueId>> = parts.iter().map(|p| t(self, p)).collect();
                if ts.iter().all(Option::is_none) {
                    None
                } else {
                    let mut comps = Vec::new();
                    for (p, tp) in parts.iter().zip(ts) {
                        let pty = self.ty(*p);
                        comps.push(self.tangent_or_zero(out, tp, pty));
                    }
                    Some(self.emit(out, *cty, Expr::Construct(*cty, comps)))
                }
            }
            // The payload's tangent, in the same variant.
            Expr::Variant(et, k, payload) => payload
                .and_then(|p| t(self, &p))
                .map(|dp| self.emit(out, *et, Expr::Variant(*et, *k, Some(dp)))),
            Expr::Extract(x, i) => t(self, x).map(|dx| self.emit(out, ty, Expr::Extract(dx, *i))),
            Expr::ExtractDyn(x, i) => {
                t(self, x).map(|dx| self.emit(out, ty, Expr::ExtractDyn(dx, *i)))
            }
            Expr::Splat(x, n) => t(self, x).map(|dx| self.emit(out, ty, Expr::Splat(dx, *n))),
            Expr::Swizzle(x, c) => {
                t(self, x).map(|dx| self.emit(out, ty, Expr::Swizzle(dx, c.clone())))
            }
            Expr::Convert(x, s) => {
                if s.is_float()
                    && self.m.types.element_scalar(self.ty(*x)).is_some_and(|s| s.is_float())
                {
                    t(self, x).map(|dx| self.emit(out, ty, Expr::Convert(dx, *s)))
                } else {
                    None
                }
            }
            Expr::Select { cond, if_true, if_false } => {
                let (dt, df) = (t(self, if_true), t(self, if_false));
                if dt.is_none() && df.is_none() {
                    None
                } else {
                    let dt = self.tangent_or_zero(out, dt, ty);
                    let df = self.tangent_or_zero(out, df, ty);
                    Some(self.emit(
                        out,
                        ty,
                        Expr::Select { cond: *cond, if_true: dt, if_false: df },
                    ))
                }
            }
            Expr::Call(..) => unreachable!("calls are handled in `call`"),
            Expr::Addr(p) => {
                if place_root_active(&self.act, p) {
                    let tp = self.place(p, k)?;
                    Some(self.emit(out, ty, Expr::Addr(tp)))
                } else {
                    None
                }
            }
            Expr::Host(..) | Expr::Run(_) => {
                return Err(Error::not_derivable("can't differentiate this"));
            }
        })
    }

    fn builtin_tangent(
        &mut self,
        v: ValueId,
        b: Builtin,
        args: &[ValueId],
        k: usize,
        ty: TypeId,
        out: &mut Block,
    ) -> Result<Option<ValueId>> {
        use Builtin as B;
        let d: Vec<Option<ValueId>> = args.iter().map(|a| self.t(*a, k)).collect();
        let a0 = args[0];
        let a0ty = self.ty(a0);
        let ln2 = std::f64::consts::LN_2;
        Ok(match b {
            B::Floor | B::Ceil | B::Round | B::Trunc | B::Sign | B::Step | B::AllEqual => None,
            B::Dpdx | B::Dpdy | B::Fwidth => {
                return Err(Error::not_derivable(
                    "derivatives of screen-space derivatives aren't supported",
                ));
            }
            B::Fract => d[0],
            B::Sqrt => d[0].map(|da| {
                let two = self.fc(out, ty, 2.0);
                let den = self.bin(out, BinOp::Mul, v, two, ty);
                self.div(out, da, den, ty)
            }),
            B::InverseSqrt => d[0].map(|da| {
                let h = self.fc(out, ty, -0.5);
                let r2 = self.bin(out, BinOp::Mul, v, v, ty);
                let r3 = self.bin(out, BinOp::Mul, r2, v, ty);
                let f = self.bin(out, BinOp::Mul, r3, h, ty);
                self.scale(out, da, f, ty)
            }),
            B::Sin => d[0].map(|da| {
                let c = self.builtin(out, B::Cos, vec![a0], ty);
                self.scale(out, da, c, ty)
            }),
            B::Cos => d[0].map(|da| {
                let s = self.builtin(out, B::Sin, vec![a0], ty);
                let ns = self.emit(out, ty, Expr::Unary(UnOp::Neg, s));
                self.scale(out, da, ns, ty)
            }),
            B::Tan => d[0].map(|da| {
                let one = self.fc(out, ty, 1.0);
                let r2 = self.bin(out, BinOp::Mul, v, v, ty);
                let f = self.bin(out, BinOp::Add, r2, one, ty);
                self.scale(out, da, f, ty)
            }),
            B::Asin | B::Acos => d[0].map(|da| {
                let one = self.fc(out, ty, 1.0);
                let a2 = self.bin(out, BinOp::Mul, a0, a0, ty);
                let s = self.bin(out, BinOp::Sub, one, a2, ty);
                let r = self.builtin(out, B::Sqrt, vec![s], ty);
                let q = self.div(out, da, r, ty);
                if b == B::Acos { self.emit(out, ty, Expr::Unary(UnOp::Neg, q)) } else { q }
            }),
            B::Atan => d[0].map(|da| {
                let one = self.fc(out, ty, 1.0);
                let a2 = self.bin(out, BinOp::Mul, a0, a0, ty);
                let s = self.bin(out, BinOp::Add, a2, one, ty);
                self.div(out, da, s, ty)
            }),
            B::Atan2 => {
                // (x dy - y dx) / (x² + y²), with y = args[0], x = args[1].
                let (y, x) = (args[0], args[1]);
                let xdy = d[0].map(|dy| self.scale(out, dy, x, ty));
                let ydx = d[1].map(|dx| self.scale(out, dx, y, ty));
                let num = self.sub(out, xdy, ydx, ty);
                num.map(|n| {
                    let x2 = self.bin(out, BinOp::Mul, x, x, ty);
                    let y2 = self.bin(out, BinOp::Mul, y, y, ty);
                    let den = self.bin(out, BinOp::Add, x2, y2, ty);
                    self.div(out, n, den, ty)
                })
            }
            B::Exp => d[0].map(|da| self.scale(out, da, v, ty)),
            B::Exp2 => d[0].map(|da| {
                let c = self.fc(out, ty, ln2);
                let f = self.bin(out, BinOp::Mul, v, c, ty);
                self.scale(out, da, f, ty)
            }),
            B::Log => d[0].map(|da| self.div(out, da, a0, ty)),
            B::Log2 => d[0].map(|da| {
                let c = self.fc(out, ty, ln2);
                let den = self.bin(out, BinOp::Mul, a0, c, ty);
                self.div(out, da, den, ty)
            }),
            B::Pow => {
                // b a^(b-1) da + a^b log(a) db. Each factor is made finite first: where a is 0
                // and doesn't move (da = 0, as in `pow(max(x, 0), 0.5)`), the term is 0, not
                // ∞ × 0.
                let (a, bb) = (args[0], args[1]);
                let big = self.fc(out, ty, f32::MAX as f64);
                let small = self.fc(out, ty, -f32::MAX as f64);
                let x = d[0].map(|da| {
                    let one = self.fc(out, ty, 1.0);
                    let bm1 = self.bin(out, BinOp::Sub, bb, one, ty);
                    let p = self.builtin(out, B::Pow, vec![a, bm1], ty);
                    let f = self.bin(out, BinOp::Mul, bb, p, ty);
                    let f = self.builtin(out, B::Clamp, vec![f, small, big], ty);
                    self.scale(out, da, f, ty)
                });
                let y = d[1].map(|db| {
                    let l = self.builtin(out, B::Log, vec![a], ty);
                    let l = self.builtin(out, B::Clamp, vec![l, small, big], ty);
                    let f = self.bin(out, BinOp::Mul, l, v, ty);
                    self.scale(out, db, f, ty)
                });
                self.add(out, x, y, ty)
            }
            B::Abs => d[0].map(|da| {
                let s = self.builtin(out, B::Sign, vec![a0], ty);
                self.scale(out, da, s, ty)
            }),
            // The taken argument's tangent; where the arguments are equal, the average of
            // both. That's the symmetric choice (as `abs`'s, zero at zero, is), and the
            // derivative of a smooth function written with kinks that cancel: std's `smin`,
            // `min(a, b) - f(|a - b|)`, has 1/2 in each on its seam.
            B::Min | B::Max if self.m.types.as_scalar(ty).is_some() => {
                if d[0].is_none() && d[1].is_none() {
                    return Ok(None);
                }
                let (a, bb) = (args[0], args[1]);
                let bt = self.m.types.bool();
                let op = if b == B::Min { BinOp::Lt } else { BinOp::Gt };
                let first = self.emit(out, bt, Expr::Binary(op, a, bb));
                let tie = self.emit(out, bt, Expr::Binary(BinOp::Eq, a, bb));
                let half = self.fc(out, ty, 0.5);
                if self.target == Target::Gpu {
                    // A weight w (1, 1/2 or 0), shared by the directions: one `mix` each. A
                    // `select` and an average each measured over the grazer's 1.25× (WGSL may
                    // assume the tangent not taken is finite, which `mix` needs).
                    let one = self.fc(out, ty, 1.0);
                    let zero = self.fc(out, ty, 0.0);
                    let w = self.emit(
                        out,
                        ty,
                        Expr::Select { cond: first, if_true: one, if_false: zero },
                    );
                    let w =
                        self.emit(out, ty, Expr::Select { cond: tie, if_true: half, if_false: w });
                    return Ok(Some(match (d[0], d[1]) {
                        (Some(x), Some(y)) => self.builtin(out, B::Mix, vec![y, x, w], ty),
                        (Some(x), None) => self.scale(out, x, w, ty),
                        (None, y) => {
                            let y = self.tangent_or_zero(out, y, ty);
                            let nw = self.bin(out, BinOp::Sub, one, w, ty);
                            self.scale(out, y, nw, ty)
                        }
                    }));
                }
                // On the CPU, selects: a tangent not taken may be infinite (a square root's at 0).
                let x = self.tangent_or_zero(out, d[0], ty);
                let y = self.tangent_or_zero(out, d[1], ty);
                let pick =
                    self.emit(out, ty, Expr::Select { cond: first, if_true: x, if_false: y });
                let sum = self.add(out, d[0], d[1], ty).unwrap_or(x);
                let avg = self.bin(out, BinOp::Mul, sum, half, ty);
                Some(self.emit(out, ty, Expr::Select { cond: tie, if_true: avg, if_false: pick }))
            }
            B::Min | B::Max => {
                // m is 1 where the first argument is taken, 0 where the second is, and 1/2 on a
                // tie (as for a scalar): (step(a, b) - step(b, a) + 1) / 2 for `min`.
                let (a, bb) = (args[0], args[1]);
                let (p, q) = if b == B::Min { (a, bb) } else { (bb, a) };
                let s = self.builtin(out, B::Step, vec![p, q], ty);
                let t = self.builtin(out, B::Step, vec![q, p], ty);
                let diff = self.bin(out, BinOp::Sub, s, t, ty);
                let half = self.fc(out, ty, 0.5);
                let h = self.bin(out, BinOp::Mul, diff, half, ty);
                let m = self.bin(out, BinOp::Add, h, half, ty);
                let one = self.fc(out, ty, 1.0);
                let not_m = self.bin(out, BinOp::Sub, one, m, ty);
                let x = self.mul(out, d[0], m, ty);
                let y = self.mul(out, d[1], not_m, ty);
                self.add(out, x, y, ty)
            }
            B::Clamp => {
                // WGSL: min(max(x, lo), hi).
                let (x, lo, hi) = (args[0], args[1], args[2]);
                let above_lo = self.builtin(out, B::Step, vec![lo, x], ty);
                let below_hi = self.builtin(out, B::Step, vec![x, hi], ty);
                let inside = self.bin(out, BinOp::Mul, above_lo, below_hi, ty);
                let one = self.fc(out, ty, 1.0);
                let not_lo = self.bin(out, BinOp::Sub, one, above_lo, ty);
                let not_hi = self.bin(out, BinOp::Sub, one, below_hi, ty);
                let t1 = self.mul(out, d[0], inside, ty);
                let t2 = self.mul(out, d[1], not_lo, ty);
                let t3 = self.mul(out, d[2], not_hi, ty);
                let s = self.add(out, t1, t2, ty);
                self.add(out, s, t3, ty)
            }
            B::Saturate => d[0].map(|dx| {
                let zero = self.zero(out, ty);
                let one = self.fc(out, ty, 1.0);
                let onev = self.coerce(out, one, ty);
                let a = self.builtin(out, B::Step, vec![zero, a0], ty);
                let bb = self.builtin(out, B::Step, vec![a0, onev], ty);
                let inside = self.bin(out, BinOp::Mul, a, bb, ty);
                self.scale(out, dx, inside, ty)
            }),
            B::Mix => {
                // da (1 - t) + db t + (b - a) dt
                let (a, bb, tt) = (args[0], args[1], args[2]);
                let ttype = self.ty(tt);
                let one = self.fc(out, ttype, 1.0);
                let omt = self.bin(out, BinOp::Sub, one, tt, ttype);
                let x = self.mul(out, d[0], omt, ty);
                let y = self.mul(out, d[1], tt, ty);
                let z = d[2].map(|dt| {
                    let ba = self.bin(out, BinOp::Sub, bb, a, ty);
                    self.scale(out, dt, ba, ty)
                });
                let s = self.add(out, x, y, ty);
                self.add(out, s, z, ty)
            }
            B::Smoothstep => {
                // u = (x - e0) / w, t = clamp(u, 0, 1): dr = 6 t (1 - t) du, where
                // du = (dx - de0 - u (de1 - de0)) / w. (6t(1-t) is 0 where the clamp acts.)
                let (e0, e1, x) = (args[0], args[1], args[2]);
                let w = self.bin(out, BinOp::Sub, e1, e0, ty);
                let xe = self.bin(out, BinOp::Sub, x, e0, ty);
                let u = self.bin(out, BinOp::Div, xe, w, ty);
                let de = self.sub(out, d[1], d[0], ty);
                let ude = de.map(|de| self.scale(out, de, u, ty));
                let n1 = self.sub(out, d[2], d[0], ty);
                let num = self.sub(out, n1, ude, ty);
                num.map(|num| {
                    let du = self.div(out, num, w, ty);
                    let zero = self.zero(out, ty);
                    let one = self.fc(out, ty, 1.0);
                    let onev = self.coerce(out, one, ty);
                    let t = self.builtin(out, B::Clamp, vec![u, zero, onev], ty);
                    let omt = self.bin(out, BinOp::Sub, onev, t, ty);
                    let six = self.fc(out, ty, 6.0);
                    let tt = self.bin(out, BinOp::Mul, t, omt, ty);
                    let f = self.bin(out, BinOp::Mul, tt, six, ty);
                    self.scale(out, du, f, ty)
                })
            }
            B::Length => d[0].map(|dx| {
                if self.m.types.is_vector(a0ty) {
                    let dot = self.builtin(out, B::Dot, vec![a0, dx], ty);
                    self.div_from_zero(out, dot, v, ty)
                } else {
                    let s = self.builtin(out, B::Sign, vec![a0], ty);
                    self.scale(out, dx, s, ty)
                }
            }),
            B::Distance => {
                let dd = self.sub(out, d[0], d[1], a0ty);
                dd.map(|dd| {
                    let diff = self.bin(out, BinOp::Sub, args[0], args[1], a0ty);
                    if self.m.types.is_vector(a0ty) {
                        let dot = self.builtin(out, B::Dot, vec![diff, dd], ty);
                        self.div_from_zero(out, dot, v, ty)
                    } else {
                        let s = self.builtin(out, B::Sign, vec![diff], ty);
                        self.scale(out, dd, s, ty)
                    }
                })
            }
            B::Dot if self.track.same_value(args[0], args[1]) => d[0].map(|da| {
                // d(v·v) = 2 v·dv
                let dot = self.builtin(out, B::Dot, vec![a0, da], ty);
                let two = self.fc(out, ty, 2.0);
                self.scale(out, dot, two, ty)
            }),
            B::Dot => {
                let x = d[0].map(|da| self.builtin(out, B::Dot, vec![da, args[1]], ty));
                let y = d[1].map(|db| self.builtin(out, B::Dot, vec![args[0], db], ty));
                self.add(out, x, y, ty)
            }
            B::Cross => {
                let x = d[0].map(|da| self.builtin(out, B::Cross, vec![da, args[1]], ty));
                let y = d[1].map(|db| self.builtin(out, B::Cross, vec![args[0], db], ty));
                self.add(out, x, y, ty)
            }
            B::Normalize => d[0].map(|dx| {
                // (dx - n dot(n, dx)) / |x|
                let f32 = self.m.types.f32();
                let len = self.builtin(out, B::Length, vec![a0], f32);
                let ndx = self.builtin(out, B::Dot, vec![v, dx], f32);
                let proj = self.bin(out, BinOp::Mul, v, ndx, ty);
                let num = self.bin(out, BinOp::Sub, dx, proj, ty);
                self.div(out, num, len, ty)
            }),
        })
    }
}
