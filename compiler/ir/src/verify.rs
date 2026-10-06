//! Checks a module's invariants. A failure is a bug in the compiler (lowering or a transform),
//! never in the user's program.
//!
//! - **Scoping:** values are defined once and used only where their definition is visible
//!   (earlier in the same block or an enclosing one); `break` and `continue` are inside loops;
//!   locals, parameters, resources and callees exist.
//! - **Types:** each value's declared type is the one its expression makes; a store writes a
//!   value of its place's type; a call passes what its callee's parameters take, each the way
//!   the parameter is passed; a return returns the function's type; a condition is a `bool`.

use crate::*;

pub fn verify(m: &Module) -> Result<()> {
    for (i, f) in m.functions.iter().enumerate() {
        let mut v = Verifier {
            m,
            f,
            defined: vec![false; f.values.len()],
            visible: vec![false; f.values.len()],
            scopes: Vec::new(),
            loops: 0,
        };
        v.block(&f.body)
            .map_err(|e| Error::internal(format!("fn{i} `{}`: {}", f.name, e.message())))?;
    }
    for e in &m.entry_points {
        if e.function.index() >= m.functions.len() {
            return Err(Error::internal(format!("entry {} names a missing function", e.name)));
        }
    }
    Ok(())
}

struct Verifier<'a> {
    m: &'a Module,
    f: &'a Function,
    defined: Vec<bool>,
    /// Whether each value's definition is in scope here.
    visible: Vec<bool>,
    /// The values each open block defined, to put out of scope when it closes.
    scopes: Vec<Vec<ValueId>>,
    loops: u32,
}

fn bug(message: impl Into<String>) -> Error {
    Error::internal(message)
}

impl Verifier<'_> {
    fn show(&self, t: TypeId) -> String {
        self.m.types.display(t)
    }

    fn visible(&self, v: ValueId) -> Result<()> {
        if v.index() >= self.f.values.len() {
            return Err(bug(format!("v{} doesn't exist", v.0)));
        }
        if !self.visible[v.index()] {
            return Err(bug(format!("v{} is used where its definition isn't visible", v.0)));
        }
        Ok(())
    }

    /// Checks that each value `each` passes on is visible here.
    fn all_visible(&self, each: impl FnOnce(&mut dyn FnMut(ValueId))) -> Result<()> {
        let mut err = Ok(());
        each(&mut |v| {
            if err.is_ok() {
                err = self.visible(v);
            }
        });
        err
    }

    fn ty(&self, v: ValueId) -> TypeId {
        self.f.value_ty(v)
    }

    fn def(&self, t: TypeId) -> &TypeDef {
        self.m.types.get(t)
    }

    fn scalar(&self, t: TypeId) -> Option<Scalar> {
        self.m.types.as_scalar(t)
    }

    fn is_f32(&self, t: TypeId) -> bool {
        self.scalar(t) == Some(Scalar::F32)
    }

    fn is_bool(&self, t: TypeId) -> bool {
        self.scalar(t) == Some(Scalar::Bool)
    }

    fn expect(&self, what: &str, want: TypeId, got: TypeId) -> Result<()> {
        if want == got {
            Ok(())
        } else {
            Err(bug(format!("{what} is a `{}`, not a `{}`", self.show(got), self.show(want))))
        }
    }

    /// The type of a place (whose values the caller has checked are visible).
    /// Whether a place is in constant data: everything but the program's state.
    fn constant(&self, p: &Place) -> bool {
        matches!(p.root, PlaceRoot::Data(d) if self.m.state != Some(d) && !self.m.writable.contains(&d))
    }

    fn place(&self, p: &Place) -> Result<TypeId> {
        match &p.root {
            PlaceRoot::Local(l) if l.index() >= self.f.locals.len() => {
                return Err(bug(format!("l{} doesn't exist", l.0)));
            }
            PlaceRoot::Param(i) if !self.f.params.get(*i as usize).is_some_and(|p| p.by_ref) => {
                return Err(bug(format!("p{i} isn't a by-reference parameter")));
            }
            PlaceRoot::Resource(r) if r.index() >= self.m.resources.len() => {
                return Err(bug(format!("r{} doesn't exist", r.0)));
            }
            PlaceRoot::Data(d) if d.index() >= self.m.data.len() => {
                return Err(bug(format!("d{} doesn't exist", d.0)));
            }
            _ => {}
        }
        for proj in &p.path {
            if let Proj::Index(i) = proj
                && !self.scalar(self.ty(*i)).is_some_and(|s| s.is_int())
            {
                return Err(bug(format!("an index v{} that isn't an integer", i.0)));
            }
        }
        self.m.place_ty(self.f, p).ok_or_else(|| bug(format!("a place with no type: {p:?}")))
    }

    /// The type `e` makes; `None` for an expression evaluated only for its effect that makes
    /// nothing (a call to a function with no result, a host op with none).
    fn expr(&self, e: &Expr) -> Result<Option<TypeId>> {
        self.all_visible(|f| e.for_each_value(&mut |v| f(v)))?;
        let types = &self.m.types;
        let t = match e {
            Expr::Const(c) => types.lookup(&TypeDef::Scalar(c.scalar())),
            Expr::Zero(t) => Some(*t),
            Expr::Param(i) => match self.f.params.get(*i as usize) {
                Some(p) if !p.by_ref => Some(p.ty),
                _ => return Err(bug(format!("param {i} isn't a by-value parameter"))),
            },
            Expr::EntryInput(_) => return Ok(None),
            Expr::Load(p) => Some(self.place(p)?),
            Expr::Run(p) => match self.def(self.place(p)?) {
                TypeDef::Array(e, _) => types.lookup(&TypeDef::Run(*e)),
                _ => return Err(bug("a run of something that isn't an array")),
            },
            Expr::Addr(p) => types.lookup(&TypeDef::Ptr(self.place(p)?)),
            Expr::Barrier => return Ok(None),
            Expr::Atomic(op, p, xs) => {
                let t = self.place(p)?;
                let TypeDef::Atomic(s) = *self.def(t) else {
                    return Err(bug("an atomic operation on something that isn't atomic"));
                };
                let n = match op {
                    AtomicOp::Load => 0,
                    AtomicOp::CompareExchange => 2,
                    _ => 1,
                };
                if xs.len() != n || xs.iter().any(|x| self.scalar(self.ty(*x)) != Some(s)) {
                    return Err(bug(format!("{op:?} with the wrong arguments")));
                }
                match op {
                    // A struct of the old value and whether it stored.
                    AtomicOp::CompareExchange => return Ok(None),
                    AtomicOp::Store => return Ok(None),
                    _ => types.lookup(&TypeDef::Scalar(s)),
                }
            }
            Expr::Texture(op, t, s, xs) => {
                let kind = |r: &ResourceId| self.m.resources.get(r.index()).map(|r| r.kind);
                let Some(ResourceKind::Texture { depth }) = kind(t) else {
                    return Err(bug("a texture read of something that isn't a texture"));
                };
                let comparison =
                    matches!(op, TextureOp::SampleCompare | TextureOp::SampleCompareLevel);
                let sampled = !matches!(op, TextureOp::Load | TextureOp::Width | TextureOp::Height);
                match (sampled, s.as_ref().map(kind)) {
                    (false, None) => {}
                    (true, Some(Some(ResourceKind::Sampler { comparison: c })))
                        if c == comparison => {}
                    _ => return Err(bug(format!("{op:?} with the wrong sampler"))),
                }
                if comparison != (depth && sampled) {
                    return Err(bug(format!("{op:?} of a texture that's depth: {depth}")));
                }
                let (f32_, u32_) = (TypeDef::Scalar(Scalar::F32), TypeDef::Scalar(Scalar::U32));
                let want: Vec<TypeDef> = match op {
                    TextureOp::Sample => vec![TypeDef::Vector(2)],
                    // The level, or the reference depth.
                    TextureOp::SampleLevel
                    | TextureOp::SampleCompare
                    | TextureOp::SampleCompareLevel => vec![TypeDef::Vector(2), f32_],
                    TextureOp::Load => vec![u32_.clone(), u32_],
                    TextureOp::Width | TextureOp::Height => Vec::new(),
                };
                if xs.len() != want.len()
                    || xs.iter().zip(&want).any(|(x, w)| self.def(self.ty(*x)) != w)
                {
                    return Err(bug(format!("{op:?} with the wrong arguments")));
                }
                let out = match op {
                    TextureOp::Width | TextureOp::Height => TypeDef::Scalar(Scalar::U32),
                    TextureOp::SampleCompare | TextureOp::SampleCompareLevel => {
                        TypeDef::Scalar(Scalar::F32)
                    }
                    TextureOp::Load if depth => TypeDef::Scalar(Scalar::F32),
                    _ => TypeDef::Vector(4),
                };
                types.lookup(&out)
            }
            Expr::ArrayLength(p) => {
                let storage = matches!(&p.root, PlaceRoot::Resource(r)
                    if matches!(self.m.resources.get(r.index()).map(|r| r.kind),
                        Some(ResourceKind::StorageRead | ResourceKind::StorageReadWrite)));
                if !storage || !p.path.is_empty() {
                    return Err(bug("the length of something that isn't a storage buffer"));
                }
                types.lookup(&TypeDef::Scalar(Scalar::U32))
            }
            Expr::Unary(op, x) => {
                let t = self.ty(*x);
                let ok = match op {
                    UnOp::Neg => types.element_scalar(t).is_some_and(|s| s != Scalar::Bool),
                    UnOp::Not => self.is_bool(t) || self.scalar(t).is_some_and(|s| s.is_int()),
                };
                if !ok {
                    return Err(bug(format!("{op:?} of a `{}`", self.show(t))));
                }
                Some(t)
            }
            Expr::Binary(op, a, b) => Some(self.binary(*op, *a, *b)?),
            Expr::Call(g, args) => return self.call(*g, args),
            Expr::Builtin(b, args) => {
                // Elementwise builtins make their arguments' type, which is one type; the
                // reductions a scalar. The arguments are scalars or vectors.
                for a in args {
                    let t = self.ty(*a);
                    if !matches!(self.def(t), TypeDef::Scalar(_) | TypeDef::Vector(_)) {
                        return Err(bug(format!("a builtin given a `{}`", self.show(t))));
                    }
                }
                if b.is_elementwise()
                    && let Some(first) = args.first()
                    && let Some(odd) = args.iter().find(|a| self.ty(**a) != self.ty(*first))
                {
                    let (x, y) = (self.show(self.ty(*first)), self.show(self.ty(*odd)));
                    return Err(bug(format!("{b:?} of `{x}` and `{y}`")));
                }
                return Ok(None);
            }
            Expr::Construct(t, parts) => {
                self.construct(*t, parts)?;
                Some(*t)
            }
            Expr::Variant(t, k, payload) => {
                let TypeDef::Enum { variants, .. } = self.def(*t) else {
                    return Err(bug(format!("a variant of `{}`", self.show(*t))));
                };
                let Some((name, want)) = variants.get(*k as usize) else {
                    return Err(bug(format!("variant {k} of `{}`", self.show(*t))));
                };
                match (want, payload) {
                    (Some(w), Some(p)) => {
                        self.expect(&format!("`{name}`'s payload"), *w, self.ty(*p))?
                    }
                    (None, None) => {}
                    _ => return Err(bug(format!("`{name}` built with the wrong payload"))),
                }
                Some(*t)
            }
            Expr::Extract(x, k) => Some(self.extract(self.ty(*x), *k)?),
            Expr::ExtractDyn(x, i) => {
                if !self.scalar(self.ty(*i)).is_some_and(|s| s.is_int()) {
                    return Err(bug("a dynamic extract at an index that isn't an integer"));
                }
                match self.def(self.ty(*x)) {
                    TypeDef::Array(e, _) => Some(*e),
                    TypeDef::Vector(_) => types.lookup(&TypeDef::Scalar(Scalar::F32)),
                    TypeDef::Matrix(n) => types.lookup(&TypeDef::Vector(*n)),
                    d => return Err(bug(format!("a dynamic extract from a `{d:?}`"))),
                }
            }
            Expr::Splat(x, n) => {
                if !self.is_f32(self.ty(*x)) {
                    return Err(bug("a splat of something that isn't an `f32`"));
                }
                types.lookup(&TypeDef::Vector(*n))
            }
            Expr::Swizzle(x, comps) => {
                let TypeDef::Vector(n) = self.def(self.ty(*x)) else {
                    return Err(bug("a swizzle of something that isn't a vector"));
                };
                if comps.iter().any(|c| c >= n) || comps.len() < 2 || comps.len() > 4 {
                    return Err(bug(format!("a swizzle {comps:?} of a vec{n}")));
                }
                types.lookup(&TypeDef::Vector(comps.len() as u8))
            }
            Expr::Convert(x, s) => {
                if self.scalar(self.ty(*x)).is_none() {
                    return Err(bug("a conversion of something that isn't a scalar"));
                }
                types.lookup(&TypeDef::Scalar(*s))
            }
            Expr::Bitcast(x, s) => {
                let from = self.scalar(self.ty(*x));
                if from.is_none_or(|f| f.bits() != s.bits()) {
                    return Err(bug(format!(
                        "a bitcast of a `{}` to {s:?}",
                        self.show(self.ty(*x))
                    )));
                }
                types.lookup(&TypeDef::Scalar(*s))
            }
            Expr::Select { cond, if_true, if_false } => {
                if !self.is_bool(self.ty(*cond)) {
                    return Err(bug("a select whose condition isn't a `bool`"));
                }
                self.expect("a select's second branch", self.ty(*if_true), self.ty(*if_false))?;
                Some(self.ty(*if_true))
            }
            Expr::Host(_, _) | Expr::Mem(..) => return Ok(None),
        };
        match t {
            Some(t) => Ok(Some(t)),
            None => Err(bug(format!("an expression whose type was never interned: {e:?}"))),
        }
    }

    fn binary(&self, op: BinOp, a: ValueId, b: ValueId) -> Result<TypeId> {
        let (ta, tb) = (self.ty(a), self.ty(b));
        let types = &self.m.types;
        let bool_t = || types.lookup(&TypeDef::Scalar(Scalar::Bool));
        if op.is_comparison() {
            if ta != tb || self.scalar(ta).is_none() {
                return Err(bug(format!("{op:?} of `{}` and `{}`", self.show(ta), self.show(tb))));
            }
            return bool_t().ok_or_else(|| bug("a comparison with no `bool`"));
        }
        if matches!(op, BinOp::And | BinOp::Or) {
            if !self.is_bool(ta) || !self.is_bool(tb) {
                return Err(bug(format!("{op:?} of `{}` and `{}`", self.show(ta), self.show(tb))));
            }
            return Ok(ta);
        }
        if matches!(op, BinOp::Shl | BinOp::Shr) {
            if !self.scalar(ta).is_some_and(|s| s.is_int()) || self.scalar(tb) != Some(Scalar::U32)
            {
                return Err(bug(format!("a shift of `{}` by `{}`", self.show(ta), self.show(tb))));
            }
            return Ok(ta);
        }
        if ta == tb {
            return match self.def(ta) {
                TypeDef::Scalar(Scalar::Bool) => {
                    if matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) {
                        Ok(ta)
                    } else {
                        Err(bug(format!("{op:?} of bools")))
                    }
                }
                TypeDef::Scalar(_) | TypeDef::Vector(_) => Ok(ta),
                TypeDef::Matrix(_) if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) => Ok(ta),
                d => Err(bug(format!("{op:?} of `{d:?}`"))),
            };
        }
        // A matrix with a scalar (each element) or a vector. A vector with a scalar is splatted
        // first.
        match (self.def(ta), self.def(tb)) {
            (TypeDef::Matrix(_), TypeDef::Scalar(Scalar::F32)) => Ok(ta),
            (TypeDef::Scalar(Scalar::F32), TypeDef::Matrix(_)) => Ok(tb),
            (TypeDef::Matrix(n), TypeDef::Vector(k)) | (TypeDef::Vector(k), TypeDef::Matrix(n))
                if op == BinOp::Mul && n == k =>
            {
                Ok(if matches!(self.def(ta), TypeDef::Matrix(_)) { tb } else { ta })
            }
            _ => Err(bug(format!("{op:?} of `{}` and `{}`", self.show(ta), self.show(tb)))),
        }
    }

    fn call(&self, g: FuncId, args: &[Arg]) -> Result<Option<TypeId>> {
        let Some(callee) = self.m.functions.get(g.index()) else {
            return Err(bug(format!("a call to missing fn{}", g.0)));
        };
        if callee.params.len() != args.len() {
            return Err(bug(format!(
                "a call to `{}` with {} arguments, not {}",
                callee.name,
                args.len(),
                callee.params.len()
            )));
        }
        for (a, p) in args.iter().zip(&callee.params) {
            match (a, p.by_ref) {
                (Arg::Value(v), false) => self.expect(
                    &format!("`{}`'s argument `{}`", callee.name, p.name),
                    p.ty,
                    self.ty(*v),
                )?,
                (Arg::Place(pl), true) => {
                    if p.mutable && self.constant(pl) {
                        return Err(bug(format!(
                            "constant data passed for `{}`, which may be written",
                            p.name
                        )));
                    }
                    let t = self.place(pl)?;
                    self.expect(&format!("`{}`'s argument `{}`", callee.name, p.name), p.ty, t)?
                }
                _ => {
                    return Err(bug(format!(
                        "argument `{}` of `{}` passed the wrong way",
                        p.name, callee.name
                    )));
                }
            }
        }
        Ok(match (callee.ret, callee.ret_ref) {
            (Some(t), true) => Some(
                self.m
                    .types
                    .lookup(&TypeDef::Ptr(t))
                    .ok_or_else(|| bug("a projection call with no pointer type"))?,
            ),
            (r, _) => r,
        })
    }

    fn construct(&self, t: TypeId, parts: &[ValueId]) -> Result<()> {
        match self.def(t) {
            TypeDef::Struct { fields, .. } => {
                if fields.len() != parts.len() {
                    return Err(bug(format!(
                        "a `{}` built from {} parts",
                        self.show(t),
                        parts.len()
                    )));
                }
                for ((name, ft), p) in fields.iter().zip(parts) {
                    self.expect(
                        &format!("field `{name}` of `{}`", self.show(t)),
                        *ft,
                        self.ty(*p),
                    )?;
                }
            }
            TypeDef::Vector(n) => {
                if parts.len() != *n as usize || !parts.iter().all(|p| self.is_f32(self.ty(*p))) {
                    return Err(bug(format!("a vec{n} built from {} parts", parts.len())));
                }
            }
            TypeDef::Matrix(n) => {
                let col = self.m.types.lookup(&TypeDef::Vector(*n));
                if parts.len() != *n as usize || !parts.iter().all(|p| Some(self.ty(*p)) == col) {
                    return Err(bug(format!("a mat{n} built from the wrong columns")));
                }
            }
            TypeDef::Array(e, n) => {
                if parts.len() != *n as usize {
                    return Err(bug(format!("an array of {n} built from {} parts", parts.len())));
                }
                for p in parts {
                    self.expect("an array element", *e, self.ty(*p))?;
                }
            }
            // A run from its first element's address and its length, both `u32`s (CPU only).
            TypeDef::Run(_) => {
                let u = self.m.types.lookup(&TypeDef::Scalar(Scalar::U32));
                if parts.len() != 2 || !parts.iter().all(|p| Some(self.ty(*p)) == u) {
                    return Err(bug("a run built from something but an address and a length"));
                }
            }
            d => return Err(bug(format!("a construct of `{d:?}`"))),
        }
        Ok(())
    }

    fn extract(&self, t: TypeId, k: u32) -> Result<TypeId> {
        let types = &self.m.types;
        let r = match self.def(t) {
            TypeDef::Struct { .. } | TypeDef::Enum { .. } => types.field(t, k),
            TypeDef::Vector(n) if k < u32::from(*n) => types.lookup(&TypeDef::Scalar(Scalar::F32)),
            TypeDef::Matrix(n) if k < u32::from(*n) => types.lookup(&TypeDef::Vector(*n)),
            TypeDef::Array(e, n) if k < *n => Some(*e),
            TypeDef::Run(_) if k < 2 => types.lookup(&TypeDef::Scalar(Scalar::U32)),
            _ => None,
        };
        r.ok_or_else(|| bug(format!("part {k} of a `{}`", self.show(t))))
    }

    fn block(&mut self, b: &Block) -> Result<()> {
        self.scopes.push(Vec::new());
        for s in b {
            match s {
                Stmt::Let(v, e) => {
                    if let Some(t) = self.expr(e)? {
                        self.expect(&format!("v{}", v.0), self.ty(*v), t)?;
                    }
                    if self.defined[v.index()] {
                        return Err(bug(format!("v{} is defined twice", v.0)));
                    }
                    self.defined[v.index()] = true;
                    self.visible[v.index()] = true;
                    if let Some(scope) = self.scopes.last_mut() {
                        scope.push(*v);
                    }
                }
                Stmt::Eval(e) => {
                    self.expr(e)?;
                }
                Stmt::Store(p, v) => {
                    if self.constant(p) {
                        return Err(bug("a store into constant data"));
                    }
                    self.all_visible(|f| p.for_each_value(&mut |v| f(v)))?;
                    let t = self.place(p)?;
                    self.visible(*v)?;
                    self.expect("a stored value", t, self.ty(*v))?;
                }
                Stmt::If { cond, then, else_ } => {
                    self.visible(*cond)?;
                    if !self.is_bool(self.ty(*cond)) {
                        return Err(bug("an `if` whose condition isn't a `bool`"));
                    }
                    self.block(then)?;
                    self.block(else_)?;
                }
                Stmt::Loop { body, continuing } => {
                    self.loops += 1;
                    self.block(body)?;
                    self.loops -= 1;
                    // `continuing` sees the body's values in WGSL only if they're defined at
                    // the body's top level; keep it simple: it gets its own scope.
                    let saved = self.loops;
                    self.loops = 0;
                    self.block(continuing)?;
                    self.loops = saved;
                }
                Stmt::Break | Stmt::Continue if self.loops == 0 => {
                    return Err(bug("break or continue outside a loop"));
                }
                Stmt::Break | Stmt::Continue | Stmt::Trap | Stmt::At(_) => {}
                Stmt::Return(v) => match (v, self.f.ret) {
                    (Some(v), Some(r)) => {
                        self.visible(*v)?;
                        let want = if self.f.ret_ref {
                            self.m.types.lookup(&TypeDef::Ptr(r)).unwrap_or(r)
                        } else {
                            r
                        };
                        self.expect("the returned value", want, self.ty(*v))?;
                    }
                    (None, None) => {}
                    (Some(_), None) => {
                        return Err(bug("a value returned from a function that returns nothing"));
                    }
                    (None, Some(_)) => {
                        return Err(bug("nothing returned from a function that returns a value"));
                    }
                },
            }
        }
        for v in self.scopes.pop().unwrap_or_default() {
            self.visible[v.index()] = false;
        }
        Ok(())
    }
}
