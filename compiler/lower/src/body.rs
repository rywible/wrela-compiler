//! Function bodies: the typed tree of one instance, lowered to IR.

use crate::instance::{Callable, InstanceKey};
use crate::ty::source_body;
use crate::{Cx, ModuleBuilder, rc};
use wrela_diag::{Diagnostic, Span, codes};
use wrela_ir as ir;
use wrela_sema::builtins::BuiltinFn;
use wrela_sema::defs::{Lang, RetMode};
use wrela_sema::thir::{self, Callee, ExprKind, LocalKind, PatKind, StmtKind};
use wrela_sema::ty::*;
use wrela_syntax::ast::{BinOp, UnOp};

/// How a source local exists in the IR function.
#[derive(Clone, Debug)]
pub(crate) enum Repr {
    /// Not yet bound (declared later in the body).
    Unbound,
    /// An IR local holding the value.
    Var(ir::LocalId),
    /// An alias of a place: a projection, a by-reference parameter, a resource.
    Place(ir::Place),
    /// A closure or function, with where its captures are in this function.
    Callable(Callable, Vec<CaptureSrc>),
    /// No runtime value.
    Erased,
}

/// Where a closure's capture lives in the function currently being lowered.
#[derive(Clone, Debug)]
pub(crate) struct CaptureSrc {
    pub place: ir::Place,
    pub by_ref: bool,
}

pub(crate) struct Fl<'c, 'a> {
    pub cx: &'c mut Cx<'a>,
    pub mb: &'c mut ModuleBuilder,
    pub key: InstanceKey,
    pub id: ir::FuncId,
    pub f: ir::Function,
    pub body: thir::Body,
    pub subst: Subst,
    pub locals: Vec<Repr>,
    /// The block being emitted is the last; enclosing ones before it.
    blocks: Vec<(ir::Block, bool)>,
}

pub(crate) fn lower_body(cx: &mut Cx, mb: &mut ModuleBuilder, key: &InstanceKey, id: ir::FuncId) {
    if let InstanceKey::Derived { .. } = key {
        crate::gpu::lower_derived(cx, mb, key, id);
        return;
    }
    let Some(body) = source_body(cx, key) else { return };
    let subst = cx.instance_subst(key);
    let f =
        std::mem::replace(&mut mb.m.functions[id.index()], ir::Function::new("", Vec::new(), None));
    let n = body.locals.len();
    let mut fl = Fl {
        cx,
        mb,
        key: key.clone(),
        id,
        f,
        body,
        subst,
        locals: vec![Repr::Unbound; n],
        blocks: vec![(Vec::new(), false)],
    };
    match key {
        InstanceKey::Fn { .. } => fl.bind_fn_params(),
        InstanceKey::Closure { id: cid, .. } => fl.bind_closure_params(*cid),
        InstanceKey::Derived { .. } => {}
    }
    let value = match key {
        InstanceKey::Closure { id: cid, .. } => fl.body.closures[cid.0 as usize].body.clone(),
        _ => fl.body.value.clone(),
    };
    fl.lower_fn_value(&value);
    fl.finish();
}

impl<'c, 'a> Fl<'c, 'a> {
    // ---- emission --------------------------------------------------------------------------

    pub fn emit(&mut self, s: ir::Stmt) {
        let (b, term) = self.blocks.last_mut().expect("a block is open");
        if *term {
            return;
        }
        let ends = matches!(
            s,
            ir::Stmt::Break | ir::Stmt::Continue | ir::Stmt::Return(_) | ir::Stmt::Trap
        );
        b.push(s);
        if ends {
            *term = true;
        }
    }

    pub fn terminated(&self) -> bool {
        self.blocks.last().is_some_and(|b| b.1)
    }

    pub fn push_block(&mut self) {
        self.blocks.push((Vec::new(), false));
    }

    pub fn pop_block(&mut self) -> ir::Block {
        self.blocks.pop().map(|b| b.0).unwrap_or_default()
    }

    pub fn value(&mut self, ty: ir::TypeId, e: ir::Expr) -> ir::ValueId {
        let v = self.f.new_value(ty);
        self.emit(ir::Stmt::Let(v, e));
        v
    }

    pub fn konst(&mut self, c: ir::Const) -> ir::ValueId {
        let ty = self.mb.m.types.scalar(c.scalar());
        self.value(ty, ir::Expr::Const(c))
    }

    pub fn u32c(&mut self, v: u32) -> ir::ValueId {
        self.konst(ir::Const::U32(v))
    }

    pub fn new_local(&mut self, name: &str, ty: ir::TypeId) -> ir::LocalId {
        self.f.new_local(name, ty)
    }

    fn finish(mut self) {
        let body = self.pop_block();
        self.f.body = body;
        self.mb.m.functions[self.id.index()] = self.f;
    }

    pub fn ty(&mut self, t: TyId, span: Span) -> Option<ir::TypeId> {
        let c = self.cx.concrete(t, &self.subst.clone());
        self.cx.lower_ty(self.mb, c, span)
    }

    pub fn concrete(&mut self, t: TyId) -> TyId {
        self.cx.concrete(t, &self.subst.clone())
    }

    pub fn types(&self) -> &Types {
        &self.cx.checked.program.types
    }

    pub fn is_gpu(&self) -> bool {
        self.mb.target == ir::Target::Gpu
    }

    // ---- parameters ------------------------------------------------------------------------

    fn bind_fn_params(&mut self) {
        let InstanceKey::Fn { func, callables, resources, .. } = self.key.clone() else { return };
        let def = self.cx.checked.program.func(func).clone();
        if let Some(entry) = self.cx.entry_of(func)
            && self.is_gpu()
            && self.mb.gpu.as_ref().is_some_and(|g| g.entry_fns.contains(&func))
        {
            crate::gpu::bind_entry_params(self, func, entry);
            return;
        }
        let mut k = 0u32;
        for (i, p) in def.params.iter().enumerate() {
            let local = self.body.params[i];
            let t = self.concrete(p.ty);
            if let TyKind::FnPtr(..) = self.types().kind(t) {
                let c = callables.get(i).cloned().flatten();
                match c {
                    Some(c) => {
                        let caps = self.cx.callable_params(self.mb, &c, p.span);
                        let mut srcs = Vec::new();
                        for cp in caps {
                            let place = if cp.by_ref {
                                ir::Place { root: ir::PlaceRoot::Param(k), path: Vec::new() }
                            } else {
                                let l = self.new_local(&cp.name, cp.ty);
                                let v = self.value(cp.ty, ir::Expr::Param(k));
                                self.emit(ir::Stmt::Store(ir::Place::local(l), v));
                                ir::Place::local(l)
                            };
                            srcs.push(CaptureSrc { place, by_ref: cp.by_ref });
                            k += 1;
                        }
                        self.locals[local.index()] = Repr::Callable(c, srcs);
                    }
                    None => self.locals[local.index()] = Repr::Erased,
                }
                continue;
            }
            if let Some(Some(r)) = resources.get(i) {
                self.locals[local.index()] =
                    Repr::Place(ir::Place { root: ir::PlaceRoot::Resource(*r), path: Vec::new() });
                continue;
            }
            let Some(ty) = self.cx.lower_ty(self.mb, t, p.span) else {
                self.locals[local.index()] = Repr::Erased;
                continue;
            };
            let param = self.f.params[k as usize].clone();
            if param.by_ref {
                self.locals[local.index()] =
                    Repr::Place(ir::Place { root: ir::PlaceRoot::Param(k), path: Vec::new() });
            } else {
                let l = self.new_local(&p.name, ty);
                let v = self.value(ty, ir::Expr::Param(k));
                self.emit(ir::Stmt::Store(ir::Place::local(l), v));
                self.locals[local.index()] = Repr::Var(l);
            }
            k += 1;
        }
    }

    fn bind_closure_params(&mut self, cid: ClosureId) {
        let def = self.body.closures[cid.0 as usize].clone();
        let mut k = 0u32;
        // Captures first, in the order `callable_params` declared them.
        for (l, _written) in &def.captures {
            let decl = self.body.local(*l).clone();
            let t = self.concrete(decl.ty);
            if matches!(self.types().kind(t), TyKind::Closure(..) | TyKind::FnPtr(..)) {
                // A captured closure: the owner's own callable, still reachable through the
                // owner's key.
                self.locals[l.index()] = self.owner_callable_repr(*l);
                continue;
            }
            let Some(ty) = self.cx.lower_ty(self.mb, t, decl.span) else {
                self.locals[l.index()] = Repr::Erased;
                continue;
            };
            let param = self.f.params[k as usize].clone();
            if param.by_ref {
                self.locals[l.index()] =
                    Repr::Place(ir::Place { root: ir::PlaceRoot::Param(k), path: Vec::new() });
            } else {
                let lv = self.new_local(&decl.name, ty);
                let v = self.value(ty, ir::Expr::Param(k));
                self.emit(ir::Stmt::Store(ir::Place::local(lv), v));
                self.locals[l.index()] = Repr::Var(lv);
            }
            k += 1;
        }
        for &l in &def.params {
            let decl = self.body.local(l).clone();
            let t = self.concrete(decl.ty);
            let Some(ty) = self.cx.lower_ty(self.mb, t, decl.span) else {
                self.locals[l.index()] = Repr::Erased;
                continue;
            };
            let lv = self.new_local(&decl.name, ty);
            let v = self.value(ty, ir::Expr::Param(k));
            self.emit(ir::Stmt::Store(ir::Place::local(lv), v));
            self.locals[l.index()] = Repr::Var(lv);
            k += 1;
        }
    }

    /// A closure captured by another closure: only closures passed in as the owner's function
    /// parameters can be (others would need their own captures threaded through).
    fn owner_callable_repr(&mut self, l: thir::LocalId) -> Repr {
        let span = self.body.local(l).span;
        self.cx.err(
            Diagnostic::new(
                codes::E0702,
                span,
                "a closure that uses another closure from its surroundings isn't supported yet",
            )
            .with_help("pass the inner closure to the function directly"),
        );
        Repr::Erased
    }

    // ---- the function's value --------------------------------------------------------------

    fn lower_fn_value(&mut self, value: &thir::Expr) {
        let ret_ref = self.f.ret_ref;
        if ret_ref {
            // A projection: return the place's address.
            match self.place_of_projection(value) {
                Some(p) => {
                    let pt = self.f.ret.map(|t| self.mb.m.types.intern(ir::TypeDef::Ptr(t)));
                    if let Some(pt) = pt {
                        let v = self.value(pt, ir::Expr::Addr(p));
                        self.emit(ir::Stmt::Return(Some(v)));
                    }
                }
                None => self.emit(ir::Stmt::Trap),
            }
            return;
        }
        let v = self.expr(value);
        if self.f.ret.is_some() {
            if let Some(v) = v {
                self.emit(ir::Stmt::Return(Some(v)));
            } else {
                self.emit(ir::Stmt::Trap);
            }
        } else {
            self.emit(ir::Stmt::Return(None));
        }
    }

    /// The place a projection-returning body (or `return`) designates.
    fn place_of_projection(&mut self, e: &thir::Expr) -> Option<ir::Place> {
        match &e.kind {
            ExprKind::Block(b) => {
                for s in &b.stmts {
                    self.stmt(s);
                }
                b.tail.as_ref().and_then(|t| self.place_of_projection(t))
            }
            ExprKind::MutArg(inner) => self.place(inner),
            _ => self.place(e),
        }
    }

    // ---- places ----------------------------------------------------------------------------

    /// The IR place of a place expression; `None` if it isn't one (or has no runtime value).
    pub fn place(&mut self, e: &thir::Expr) -> Option<ir::Place> {
        match &e.kind {
            ExprKind::Local(l) => match self.locals[l.index()].clone() {
                Repr::Var(v) => Some(ir::Place::local(v)),
                Repr::Place(p) => Some(p),
                _ => None,
            },
            ExprKind::Field(b, i) => {
                let bt = self.concrete(b.ty);
                let base = self.place(b)?;
                let map = self.cx.field_map(self.mb, bt, None, e.span);
                let fi = map.get(*i as usize).copied().flatten()?;
                Some(base.with(ir::Proj::Field(fi)))
            }
            ExprKind::Swizzle(b, comps) if comps.len() == 1 => {
                let base = self.place(b)?;
                Some(base.with(ir::Proj::Comp(comps[0])))
            }
            ExprKind::Index(b, i) => {
                let bt = self.concrete(b.ty);
                let base = self.place(b)?;
                if let TyKind::Adt(a, _) = self.types().kind(bt)
                    && self.cx.checked.program.is_lang_adt(*a, Lang::Slots)
                {
                    let idx = crate::gpu::slot_index(self, i)?;
                    return Some(base.with(ir::Proj::Index(idx)));
                }
                if let TyKind::Vec(_) = self.types().kind(bt)
                    && let ExprKind::Lit(thir::Lit::Int(c)) = i.kind
                {
                    return Some(base.with(ir::Proj::Comp(c as u8)));
                }
                let idx = self.expr(i)?;
                Some(base.with(ir::Proj::Index(idx)))
            }
            ExprKind::Call(c) if c.ret_mode != RetMode::Owned => {
                let ptr = self.call(c, e)?;
                Some(ir::Place { root: ir::PlaceRoot::Ptr(ptr), path: Vec::new() })
            }
            ExprKind::MutArg(inner) | ExprKind::Take(inner) => self.place(inner),
            _ => None,
        }
    }

    pub fn load(&mut self, p: ir::Place, ty: ir::TypeId) -> ir::ValueId {
        self.value(ty, ir::Expr::Load(p))
    }

    /// A place for `e`: its own if it's a place, else a temporary local holding its value.
    pub fn place_or_temp(&mut self, e: &thir::Expr) -> Option<ir::Place> {
        if e.is_place()
            && let Some(p) = self.place(e)
        {
            return Some(p);
        }
        let ty = self.ty(e.ty, e.span)?;
        let v = self.expr(e)?;
        let l = self.new_local("tmp", ty);
        self.emit(ir::Stmt::Store(ir::Place::local(l), v));
        Some(ir::Place::local(l))
    }

    // ---- expressions -----------------------------------------------------------------------

    /// Lowers an expression; `None` for no value (unit, never, or after an error).
    pub fn expr(&mut self, e: &thir::Expr) -> Option<ir::ValueId> {
        if self.terminated() {
            return None;
        }
        let span = e.span;
        match &e.kind {
            ExprKind::Lit(l) => {
                let ty = self.ty(e.ty, span)?;
                let c = self.lit(*l, ty)?;
                Some(self.value(ty, ir::Expr::Const(c)))
            }
            ExprKind::Local(_)
            | ExprKind::Field(..)
            | ExprKind::Index(..)
            | ExprKind::Swizzle(..) => {
                if e.is_place()
                    && let Some(p) = self.place(e)
                {
                    let ty = self.ty(e.ty, span)?;
                    // A multi-component swizzle of a place: load the vector, then swizzle.
                    if let ExprKind::Swizzle(_, comps) = &e.kind
                        && comps.len() > 1
                    {
                        let _ = p;
                    } else {
                        return Some(self.load(p, ty));
                    }
                }
                self.projection_value(e)
            }
            ExprKind::Const(c) => {
                let value = self.cx.checked.consts.get(c).map(|(_, v)| v.clone())?;
                self.expr(&value)
            }
            ExprKind::Unary(op, x) => {
                let v = self.expr(x)?;
                let ty = self.ty(e.ty, span)?;
                let op = match op {
                    UnOp::Neg => ir::UnOp::Neg,
                    UnOp::Not => ir::UnOp::Not,
                };
                Some(self.value(ty, ir::Expr::Unary(op, v)))
            }
            ExprKind::Binary(op, a, b) => self.binary(*op, a, b, e),
            ExprKind::Call(c) => self.call(c, e),
            ExprKind::Adt { adt, variant, fields, .. } => self.adt(*adt, *variant, fields, e),
            ExprKind::Tuple(xs) => {
                let ty = self.ty(e.ty, span)?;
                let vs: Vec<ir::ValueId> = xs.iter().filter_map(|x| self.expr(x)).collect();
                Some(self.value(ty, ir::Expr::Construct(ty, vs)))
            }
            ExprKind::Array(xs) => {
                let ty = self.ty(e.ty, span)?;
                let vs: Vec<ir::ValueId> = xs.iter().filter_map(|x| self.expr(x)).collect();
                Some(self.value(ty, ir::Expr::Construct(ty, vs)))
            }
            ExprKind::ArrayRepeat(x, n) => {
                let ty = self.ty(e.ty, span)?;
                let zero = match x.kind {
                    ExprKind::Lit(thir::Lit::Int(0) | thir::Lit::Bool(false)) => true,
                    ExprKind::Lit(thir::Lit::Float(f)) => f == 0.0 && f.is_sign_positive(),
                    _ => false,
                };
                if zero {
                    return Some(self.value(ty, ir::Expr::Zero(ty)));
                }
                let v = self.expr(x)?;
                if *n <= 64 {
                    return Some(self.value(ty, ir::Expr::Construct(ty, vec![v; *n as usize])));
                }
                // A long repeat: fill a local in a loop.
                let l = self.new_local("repeat", ty);
                let n = *n;
                self.counted_loop(n, |fl, i| {
                    fl.emit(ir::Stmt::Store(ir::Place::local(l).with(ir::Proj::Index(i)), v))
                });
                Some(self.load(ir::Place::local(l), ty))
            }
            ExprKind::Construct(xs) => self.construct(xs, e),
            ExprKind::Convert(x) => {
                let v = self.expr(x)?;
                let ty = self.ty(e.ty, span)?;
                let s = self.mb.m.types.as_scalar(ty)?;
                Some(self.value(ty, ir::Expr::Convert(v, s)))
            }
            ExprKind::Block(b) => self.block(b),
            ExprKind::If { cond, then, else_ } => self.if_(cond, then, else_.as_deref(), e),
            ExprKind::Match { scrutinee, arms } => self.match_(scrutinee, arms, e),
            ExprKind::Closure(_) | ExprKind::FnRef(..) => None,
            ExprKind::Take(x) | ExprKind::MutArg(x) => self.expr(x),
            ExprKind::Return(v) => {
                if self.f.ret_ref {
                    let p = v.as_ref().and_then(|v| self.place_of_projection(v));
                    let pt = self.f.ret.map(|t| self.mb.m.types.intern(ir::TypeDef::Ptr(t)));
                    match (p, pt) {
                        (Some(p), Some(pt)) => {
                            let a = self.value(pt, ir::Expr::Addr(p));
                            self.emit(ir::Stmt::Return(Some(a)));
                        }
                        _ => self.emit(ir::Stmt::Trap),
                    }
                    return None;
                }
                let v = v.as_ref().and_then(|v| self.expr(v));
                self.emit(ir::Stmt::Return(v.filter(|_| self.f.ret.is_some())));
                None
            }
            ExprKind::Break => {
                self.emit(ir::Stmt::Break);
                None
            }
            ExprKind::Continue => {
                self.emit(ir::Stmt::Continue);
                None
            }
            ExprKind::Dispatch(d) => {
                crate::gpu::lower_dispatch(self, d, span);
                None
            }
            ExprKind::Draw(d) => {
                crate::gpu::lower_draw(self, d, span);
                None
            }
            ExprKind::Error => None,
        }
    }

    /// A field, swizzle or element of a value that isn't a place.
    fn projection_value(&mut self, e: &thir::Expr) -> Option<ir::ValueId> {
        let ty = self.ty(e.ty, e.span)?;
        match &e.kind {
            ExprKind::Field(b, i) => {
                let bt = self.concrete(b.ty);
                let map = self.cx.field_map(self.mb, bt, None, e.span);
                let fi = map.get(*i as usize).copied().flatten()?;
                let bv = self.expr(b)?;
                Some(self.value(ty, ir::Expr::Extract(bv, fi)))
            }
            ExprKind::Swizzle(b, comps) => {
                let bv = self.expr(b)?;
                if comps.len() == 1 {
                    Some(self.value(ty, ir::Expr::Extract(bv, comps[0] as u32)))
                } else {
                    Some(self.value(ty, ir::Expr::Swizzle(bv, comps.clone())))
                }
            }
            ExprKind::Index(b, i) => {
                let bv = self.expr(b)?;
                let iv = self.expr(i)?;
                Some(self.value(ty, ir::Expr::ExtractDyn(bv, iv)))
            }
            _ => None,
        }
    }

    fn lit(&mut self, l: thir::Lit, ty: ir::TypeId) -> Option<ir::Const> {
        let s = self.mb.m.types.as_scalar(ty)?;
        Some(match (l, s) {
            (thir::Lit::Bool(b), _) => ir::Const::Bool(b),
            (thir::Lit::Int(v), ir::Scalar::I32) => ir::Const::I32(v as i64 as i32),
            (thir::Lit::Int(v), ir::Scalar::U32) => ir::Const::U32(v as u32),
            (thir::Lit::Int(v), ir::Scalar::I64) => ir::Const::I64(v as i64),
            (thir::Lit::Int(v), ir::Scalar::U64) => ir::Const::U64(v),
            (thir::Lit::Int(v), ir::Scalar::F32) => ir::Const::F32(v as f32),
            (thir::Lit::Int(v), ir::Scalar::F64) => ir::Const::F64(v as f64),
            (thir::Lit::Int(v), s) => ir::Const::Small(s, v as i64),
            (thir::Lit::Float(v), ir::Scalar::F32) => ir::Const::F32(v as f32),
            (thir::Lit::Float(v), _) => ir::Const::F64(v),
        })
    }

    fn binary(
        &mut self,
        op: BinOp,
        a: &thir::Expr,
        b: &thir::Expr,
        e: &thir::Expr,
    ) -> Option<ir::ValueId> {
        let rty = self.ty(e.ty, e.span)?;
        if matches!(op, BinOp::And | BinOp::Or) {
            // Short-circuit: `b` runs only when it decides the result.
            let av = self.expr(a)?;
            let l = self.new_local("logic", rty);
            self.emit(ir::Stmt::Store(ir::Place::local(l), av));
            let cond = if op == BinOp::And {
                av
            } else {
                self.value(rty, ir::Expr::Unary(ir::UnOp::Not, av))
            };
            self.push_block();
            if let Some(bv) = self.expr(b) {
                self.emit(ir::Stmt::Store(ir::Place::local(l), bv));
            }
            let then = self.pop_block();
            self.emit(ir::Stmt::If { cond, then, else_: Vec::new() });
            return Some(self.load(ir::Place::local(l), rty));
        }
        let at = self.concrete(a.ty);
        let av = self.expr(a)?;
        let bv = self.expr(b)?;
        // Enum equality compares tags.
        if let TyKind::Adt(adt, _) = self.types().kind(at)
            && self.cx.checked.program.adt(*adt).is_enum()
        {
            let u = self.mb.m.types.u32();
            let ta = self.value(u, ir::Expr::Extract(av, 0));
            let tb = self.value(u, ir::Expr::Extract(bv, 0));
            let iop = if op == BinOp::Eq { ir::BinOp::Eq } else { ir::BinOp::Ne };
            return Some(self.value(rty, ir::Expr::Binary(iop, ta, tb)));
        }
        if let TyKind::Vec(_) = self.types().kind(at)
            && matches!(op, BinOp::Eq | BinOp::Ne)
        {
            let eq = self.value(rty, ir::Expr::Builtin(ir::Builtin::AllEqual, vec![av, bv]));
            return Some(if op == BinOp::Eq {
                eq
            } else {
                self.value(rty, ir::Expr::Unary(ir::UnOp::Not, eq))
            });
        }
        if op == BinOp::Pow {
            if self.mb.m.types.as_scalar(rty).is_some_and(|s| s.is_int()) {
                return Some(self.int_pow(av, bv, rty));
            }
            return self.math(ir::Builtin::Pow, vec![av, bv], rty, e.span);
        }
        let iop = match op {
            BinOp::Add => ir::BinOp::Add,
            BinOp::Sub => ir::BinOp::Sub,
            BinOp::Mul => ir::BinOp::Mul,
            BinOp::Div => ir::BinOp::Div,
            BinOp::Rem => ir::BinOp::Rem,
            BinOp::BitAnd => ir::BinOp::BitAnd,
            BinOp::BitOr => ir::BinOp::BitOr,
            BinOp::BitXor => ir::BinOp::BitXor,
            BinOp::Shl => ir::BinOp::Shl,
            BinOp::Shr => ir::BinOp::Shr,
            BinOp::Eq => ir::BinOp::Eq,
            BinOp::Ne => ir::BinOp::Ne,
            BinOp::Lt => ir::BinOp::Lt,
            BinOp::Le => ir::BinOp::Le,
            BinOp::Gt => ir::BinOp::Gt,
            BinOp::Ge => ir::BinOp::Ge,
            BinOp::And | BinOp::Or | BinOp::Pow => unreachable!("handled above"),
        };
        Some(self.value(rty, ir::Expr::Binary(iop, av, bv)))
    }

    /// `base ** exp` for integers: repeated multiplication (checked on the CPU).
    fn int_pow(&mut self, base: ir::ValueId, exp: ir::ValueId, ty: ir::TypeId) -> ir::ValueId {
        let s = self.mb.m.types.as_scalar(ty).unwrap_or(ir::Scalar::I32);
        let one = match s {
            ir::Scalar::I32 => ir::Const::I32(1),
            ir::Scalar::U32 => ir::Const::U32(1),
            ir::Scalar::I64 => ir::Const::I64(1),
            ir::Scalar::U64 => ir::Const::U64(1),
            other => ir::Const::Small(other, 1),
        };
        let acc = self.new_local("pow", ty);
        let onev = self.konst(one);
        self.emit(ir::Stmt::Store(ir::Place::local(acc), onev));
        let u = self.mb.m.types.u32();
        let n = self.new_local("pow_n", u);
        self.emit(ir::Stmt::Store(ir::Place::local(n), exp));
        let bool_ty = self.mb.m.types.bool();
        self.push_block();
        let nv = self.load(ir::Place::local(n), u);
        let zero = self.u32c(0);
        let done = self.value(bool_ty, ir::Expr::Binary(ir::BinOp::Eq, nv, zero));
        self.emit(ir::Stmt::If { cond: done, then: vec![ir::Stmt::Break], else_: Vec::new() });
        let av = self.load(ir::Place::local(acc), ty);
        let prod = self.value(ty, ir::Expr::Binary(ir::BinOp::Mul, av, base));
        self.emit(ir::Stmt::Store(ir::Place::local(acc), prod));
        let one_u = self.u32c(1);
        let nv2 = self.load(ir::Place::local(n), u);
        let dec = self.value(u, ir::Expr::Binary(ir::BinOp::Sub, nv2, one_u));
        self.emit(ir::Stmt::Store(ir::Place::local(n), dec));
        let body = self.pop_block();
        self.emit(ir::Stmt::Loop { body, continuing: Vec::new() });
        self.load(ir::Place::local(acc), ty)
    }

    fn construct(&mut self, xs: &[thir::Expr], e: &thir::Expr) -> Option<ir::ValueId> {
        let ty = self.ty(e.ty, e.span)?;
        let n = match self.mb.m.types.get(ty) {
            ir::TypeDef::Vector(n) => *n,
            ir::TypeDef::Matrix(_) => {
                let cols: Vec<ir::ValueId> = xs.iter().filter_map(|x| self.expr(x)).collect();
                return Some(self.value(ty, ir::Expr::Construct(ty, cols)));
            }
            _ => return None,
        };
        let vals: Vec<(ir::ValueId, TyId)> =
            xs.iter().filter_map(|x| self.expr(x).map(|v| (v, x.ty))).collect();
        let first_is_scalar = vals.len() == 1 && {
            let t = self.concrete(vals[0].1);
            matches!(self.types().kind(t), TyKind::Float(_))
        };
        if first_is_scalar && n > 1 {
            return Some(self.value(ty, ir::Expr::Splat(vals[0].0, n)));
        }
        // Flatten vectors into components.
        let f32 = self.mb.m.types.f32();
        let mut comps = Vec::new();
        for (v, t) in vals {
            let t = self.concrete(t);
            match self.types().kind(t).clone() {
                TyKind::Vec(m) => {
                    for c in 0..m {
                        comps.push(self.value(f32, ir::Expr::Extract(v, c as u32)));
                    }
                }
                _ => comps.push(v),
            }
        }
        Some(self.value(ty, ir::Expr::Construct(ty, comps)))
    }

    fn adt(
        &mut self,
        adt: AdtId,
        variant: Option<u32>,
        fields: &[thir::Expr],
        e: &thir::Expr,
    ) -> Option<ir::ValueId> {
        let ty = self.ty(e.ty, e.span);
        let ct = self.concrete(e.ty);
        let mut vals = Vec::new();
        let map = self.cx.field_map(self.mb, ct, variant, e.span);
        for (i, x) in fields.iter().enumerate() {
            let v = self.expr(x);
            if map.get(i).copied().flatten().is_some()
                && let Some(v) = v
            {
                vals.push(v);
            }
        }
        let ty = ty?;
        match variant {
            None => Some(self.value(ty, ir::Expr::Construct(ty, vals))),
            Some(v) => {
                // { tag, payload of each variant that has one }: this variant's filled in.
                let ir::TypeDef::Struct { fields: ir_fields, .. } = self.mb.m.types.get(ty).clone()
                else {
                    return None;
                };
                let payload = self.cx.payload_field(self.mb, ct, v, e.span);
                let mut parts = vec![self.u32c(v)];
                for (fi, (_, ft)) in ir_fields.iter().enumerate().skip(1) {
                    if Some(fi as u32) == payload {
                        parts.push(self.value(*ft, ir::Expr::Construct(*ft, vals.clone())));
                    } else {
                        parts.push(self.value(*ft, ir::Expr::Zero(*ft)));
                    }
                }
                let _ = adt;
                Some(self.value(ty, ir::Expr::Construct(ty, parts)))
            }
        }
    }

    // ---- control flow ----------------------------------------------------------------------

    pub fn block(&mut self, b: &thir::Block) -> Option<ir::ValueId> {
        for s in &b.stmts {
            self.stmt(s);
        }
        b.tail.as_ref().and_then(|t| self.expr(t))
    }

    fn if_(
        &mut self,
        cond: &thir::Expr,
        then: &thir::Block,
        else_: Option<&thir::Expr>,
        e: &thir::Expr,
    ) -> Option<ir::ValueId> {
        let c = self.expr(cond)?;
        let ty = self.ty(e.ty, e.span);
        let result = ty.map(|t| self.new_local("if", t));
        self.push_block();
        if let Some(v) = self.block(then)
            && let Some(r) = result
        {
            self.emit(ir::Stmt::Store(ir::Place::local(r), v));
        }
        let tb = self.pop_block();
        self.push_block();
        if let Some(x) = else_
            && let Some(v) = self.expr(x)
            && let Some(r) = result
        {
            self.emit(ir::Stmt::Store(ir::Place::local(r), v));
        }
        let eb = self.pop_block();
        self.emit(ir::Stmt::If { cond: c, then: tb, else_: eb });
        match (result, ty) {
            (Some(r), Some(t)) if !self.terminated() => Some(self.load(ir::Place::local(r), t)),
            _ => None,
        }
    }

    fn match_(
        &mut self,
        scrutinee: &thir::Expr,
        arms: &[thir::Arm],
        e: &thir::Expr,
    ) -> Option<ir::ValueId> {
        let sp = self.place_or_temp(scrutinee);
        let st = self.concrete(scrutinee.ty);
        let ty = self.ty(e.ty, e.span);
        let result = ty.map(|t| self.new_local("match", t));
        let bool_ty = self.mb.m.types.bool();
        let matched = self.new_local("matched", bool_ty);
        let f = self.konst(ir::Const::Bool(false));
        self.emit(ir::Stmt::Store(ir::Place::local(matched), f));
        for arm in arms {
            let m = self.load(ir::Place::local(matched), bool_ty);
            let not_m = self.value(bool_ty, ir::Expr::Unary(ir::UnOp::Not, m));
            self.push_block();
            let test = match &sp {
                Some(p) => self.pat_test(&arm.pat, p.clone(), st),
                None => None,
            };
            self.push_block();
            if let Some(p) = &sp {
                self.pat_bind(&arm.pat, p.clone(), st);
            }
            let guard = arm.guard.as_ref().and_then(|g| self.expr(g));
            self.push_block();
            let t = self.konst(ir::Const::Bool(true));
            self.emit(ir::Stmt::Store(ir::Place::local(matched), t));
            if let Some(v) = self.expr(&arm.body)
                && let Some(r) = result
            {
                self.emit(ir::Stmt::Store(ir::Place::local(r), v));
            }
            let taken = self.pop_block();
            match guard {
                Some(g) => self.emit(ir::Stmt::If { cond: g, then: taken, else_: Vec::new() }),
                None => {
                    for s in taken {
                        self.emit(s);
                    }
                }
            }
            let inner = self.pop_block();
            match test {
                Some(c) => self.emit(ir::Stmt::If { cond: c, then: inner, else_: Vec::new() }),
                None => {
                    for s in inner {
                        self.emit(s);
                    }
                }
            }
            let outer = self.pop_block();
            self.emit(ir::Stmt::If { cond: not_m, then: outer, else_: Vec::new() });
        }
        match (result, ty) {
            (Some(r), Some(t)) => Some(self.load(ir::Place::local(r), t)),
            _ => None,
        }
    }

    /// The condition under which `pat` matches the value at `p`; `None` means always.
    fn pat_test(&mut self, pat: &thir::Pat, p: ir::Place, ty: TyId) -> Option<ir::ValueId> {
        let bool_ty = self.mb.m.types.bool();
        match &pat.kind {
            PatKind::Wild | PatKind::Bind(_) => None,
            PatKind::Lit(l) => {
                let t = self.ty(ty, pat.span)?;
                let c = self.lit(*l, t)?;
                let v = self.load(p, t);
                let k = self.value(t, ir::Expr::Const(c));
                Some(self.value(bool_ty, ir::Expr::Binary(ir::BinOp::Eq, v, k)))
            }
            PatKind::Tuple(ps) => {
                let map = self.cx.field_map(self.mb, ty, None, pat.span);
                let TyKind::Tuple(ts) = self.types().kind(ty).clone() else { return None };
                let mut conds = Vec::new();
                for (i, sp) in ps.iter().enumerate() {
                    if let Some(fi) = map.get(i).copied().flatten() {
                        let ft = self.concrete(ts[i]);
                        if let Some(c) = self.pat_test(sp, p.with(ir::Proj::Field(fi)), ft) {
                            conds.push(c);
                        }
                    }
                }
                self.all(conds)
            }
            PatKind::Adt { variant, fields, adt, .. } => {
                let mut conds = Vec::new();
                let TyKind::Adt(_, args) = self.types().kind(ty).clone() else { return None };
                let (field_tys, base) = match variant {
                    Some(v) => {
                        let u = self.mb.m.types.u32();
                        let tag = self.load(p.with(ir::Proj::Field(0)), u);
                        let k = self.u32c(*v);
                        conds.push(self.value(bool_ty, ir::Expr::Binary(ir::BinOp::Eq, tag, k)));
                        let payload = self.cx.payload_field(self.mb, ty, *v, pat.span);
                        let ftys = self.cx.checked.program.variant_fields(*adt, &args, *v as usize);
                        (ftys, payload.map(|pf| p.with(ir::Proj::Field(pf))))
                    }
                    None => (self.cx.checked.program.struct_fields(*adt, &args), Some(p.clone())),
                };
                let map = self.cx.field_map(self.mb, ty, *variant, pat.span);
                if let Some(base) = base {
                    for (i, sp) in fields {
                        if let Some(fi) = map.get(*i as usize).copied().flatten() {
                            let ft = self.concrete(field_tys[*i as usize].1);
                            if let Some(c) = self.pat_test(sp, base.with(ir::Proj::Field(fi)), ft) {
                                conds.push(c);
                            }
                        }
                    }
                }
                self.all(conds)
            }
            PatKind::Or(ps) => {
                let mut any: Option<ir::ValueId> = None;
                for sp in ps {
                    let c = self.pat_test(sp, p.clone(), ty)?;
                    any = Some(match any {
                        None => c,
                        Some(a) => self.value(bool_ty, ir::Expr::Binary(ir::BinOp::Or, a, c)),
                    });
                }
                any
            }
        }
    }

    fn all(&mut self, conds: Vec<ir::ValueId>) -> Option<ir::ValueId> {
        let bool_ty = self.mb.m.types.bool();
        let mut it = conds.into_iter();
        let first = it.next()?;
        Some(it.fold(first, |a, c| self.value(bool_ty, ir::Expr::Binary(ir::BinOp::And, a, c))))
    }

    /// Binds a pattern's names to the parts of the value at `p`.
    pub fn pat_bind(&mut self, pat: &thir::Pat, p: ir::Place, ty: TyId) {
        match &pat.kind {
            PatKind::Bind(l) => {
                let decl = self.body.local(*l).clone();
                match decl.kind {
                    LocalKind::Projection { .. } => self.locals[l.index()] = Repr::Place(p),
                    _ => {
                        let Some(t) = self.ty(ty, pat.span) else {
                            self.locals[l.index()] = Repr::Erased;
                            return;
                        };
                        let v = self.load(p, t);
                        let lv = self.new_local(&decl.name, t);
                        self.emit(ir::Stmt::Store(ir::Place::local(lv), v));
                        self.locals[l.index()] = Repr::Var(lv);
                    }
                }
            }
            PatKind::Tuple(ps) => {
                let map = self.cx.field_map(self.mb, ty, None, pat.span);
                let TyKind::Tuple(ts) = self.types().kind(ty).clone() else { return };
                for (i, sp) in ps.iter().enumerate() {
                    let ft = self.concrete(ts[i]);
                    match map.get(i).copied().flatten() {
                        Some(fi) => self.pat_bind(sp, p.with(ir::Proj::Field(fi)), ft),
                        None => self.erase_bindings(sp),
                    }
                }
            }
            PatKind::Adt { adt, variant, fields, .. } => {
                let TyKind::Adt(_, args) = self.types().kind(ty).clone() else { return };
                let (ftys, base) = match variant {
                    Some(v) => {
                        let payload = self.cx.payload_field(self.mb, ty, *v, pat.span);
                        (
                            self.cx.checked.program.variant_fields(*adt, &args, *v as usize),
                            payload.map(|pf| p.with(ir::Proj::Field(pf))),
                        )
                    }
                    None => (self.cx.checked.program.struct_fields(*adt, &args), Some(p.clone())),
                };
                let map = self.cx.field_map(self.mb, ty, *variant, pat.span);
                for (i, sp) in fields {
                    let ft = self.concrete(ftys[*i as usize].1);
                    match (map.get(*i as usize).copied().flatten(), &base) {
                        (Some(fi), Some(b)) => self.pat_bind(sp, b.with(ir::Proj::Field(fi)), ft),
                        _ => self.erase_bindings(sp),
                    }
                }
            }
            PatKind::Or(_) | PatKind::Wild | PatKind::Lit(_) => {}
        }
    }

    fn erase_bindings(&mut self, pat: &thir::Pat) {
        match &pat.kind {
            PatKind::Bind(l) => self.locals[l.index()] = Repr::Erased,
            PatKind::Tuple(ps) | PatKind::Or(ps) => ps.iter().for_each(|x| self.erase_bindings(x)),
            PatKind::Adt { fields, .. } => fields.iter().for_each(|(_, x)| self.erase_bindings(x)),
            _ => {}
        }
    }

    /// `for i in 0..n` with a compile-time `n`, for internal loops.
    pub fn counted_loop(&mut self, n: u32, mut body: impl FnMut(&mut Self, ir::ValueId)) {
        let u = self.mb.m.types.u32();
        let bool_ty = self.mb.m.types.bool();
        let i = self.new_local("i", u);
        let zero = self.u32c(0);
        self.emit(ir::Stmt::Store(ir::Place::local(i), zero));
        self.push_block();
        let iv = self.load(ir::Place::local(i), u);
        let nv = self.u32c(n);
        let lt = self.value(bool_ty, ir::Expr::Binary(ir::BinOp::Lt, iv, nv));
        let notlt = self.value(bool_ty, ir::Expr::Unary(ir::UnOp::Not, lt));
        self.emit(ir::Stmt::If { cond: notlt, then: vec![ir::Stmt::Break], else_: Vec::new() });
        body(self, iv);
        let body_b = self.pop_block();
        let continuing = self.increment(i);
        self.emit(ir::Stmt::Loop { body: body_b, continuing });
    }

    /// `i = i + 1` for a u32 or i32 local, as a block for a loop's `continuing`.
    fn increment(&mut self, i: ir::LocalId) -> ir::Block {
        let ty = self.f.locals[i.index()].ty;
        self.push_block();
        let v = self.load(ir::Place::local(i), ty);
        let one = match self.mb.m.types.as_scalar(ty) {
            Some(ir::Scalar::I32) => self.konst(ir::Const::I32(1)),
            Some(ir::Scalar::I64) => self.konst(ir::Const::I64(1)),
            Some(ir::Scalar::U64) => self.konst(ir::Const::U64(1)),
            Some(s @ (ir::Scalar::I8 | ir::Scalar::U8 | ir::Scalar::I16 | ir::Scalar::U16)) => {
                self.konst(ir::Const::Small(s, 1))
            }
            _ => self.u32c(1),
        };
        let n = self.value(ty, ir::Expr::Binary(ir::BinOp::Add, v, one));
        self.emit(ir::Stmt::Store(ir::Place::local(i), n));
        self.pop_block()
    }

    // ---- statements ------------------------------------------------------------------------

    pub fn stmt(&mut self, s: &thir::Stmt) {
        if self.terminated() {
            return;
        }
        match &s.kind {
            StmtKind::Bind { pat, init } => self.bind(pat, init),
            StmtKind::Assign { place, op, value } => {
                let v = self.expr(value);
                let Some(p) = self.place(place) else { return };
                let Some(ty) = self.ty(place.ty, place.span) else { return };
                let Some(v) = v else { return };
                let v = match op {
                    None => v,
                    Some(op) => {
                        let old = self.load(p.clone(), ty);
                        if *op == BinOp::Pow {
                            if self.mb.m.types.as_scalar(ty).is_some_and(|s| s.is_int()) {
                                self.int_pow(old, v, ty)
                            } else {
                                match self.math(ir::Builtin::Pow, vec![old, v], ty, s.span) {
                                    Some(x) => x,
                                    None => return,
                                }
                            }
                        } else {
                            let iop = match op {
                                BinOp::Add => ir::BinOp::Add,
                                BinOp::Sub => ir::BinOp::Sub,
                                BinOp::Mul => ir::BinOp::Mul,
                                BinOp::Div => ir::BinOp::Div,
                                BinOp::Rem => ir::BinOp::Rem,
                                BinOp::BitAnd => ir::BinOp::BitAnd,
                                BinOp::BitOr => ir::BinOp::BitOr,
                                BinOp::BitXor => ir::BinOp::BitXor,
                                BinOp::Shl => ir::BinOp::Shl,
                                _ => ir::BinOp::Shr,
                            };
                            self.value(ty, ir::Expr::Binary(iop, old, v))
                        }
                    }
                };
                self.emit(ir::Stmt::Store(p, v));
            }
            StmtKind::Expr(e) => {
                let _ = self.expr(e);
            }
            StmtKind::While { cond, body } => {
                let bool_ty = self.mb.m.types.bool();
                self.push_block();
                if let Some(c) = self.expr(cond) {
                    let nc = self.value(bool_ty, ir::Expr::Unary(ir::UnOp::Not, c));
                    self.emit(ir::Stmt::If {
                        cond: nc,
                        then: vec![ir::Stmt::Break],
                        else_: Vec::new(),
                    });
                }
                self.block(body);
                let b = self.pop_block();
                self.emit(ir::Stmt::Loop { body: b, continuing: Vec::new() });
            }
            StmtKind::Loop { body } => {
                self.push_block();
                self.block(body);
                let b = self.pop_block();
                self.emit(ir::Stmt::Loop { body: b, continuing: Vec::new() });
            }
            StmtKind::ForRange { var, start, end, inclusive, body } => {
                self.for_range(*var, start, end, *inclusive, body)
            }
            StmtKind::ForEach { var, array, body, .. } => self.for_each(*var, array, body),
        }
    }

    fn for_range(
        &mut self,
        var: thir::LocalId,
        start: &thir::Expr,
        end: &thir::Expr,
        inclusive: bool,
        body: &thir::Block,
    ) {
        let Some(ty) = self.ty(start.ty, start.span) else { return };
        let Some(s) = self.expr(start) else { return };
        let Some(e) = self.expr(end) else { return };
        let bool_ty = self.mb.m.types.bool();
        let decl = self.body.local(var).clone();
        let i = self.new_local(&decl.name, ty);
        self.emit(ir::Stmt::Store(ir::Place::local(i), s));
        self.locals[var.index()] = Repr::Var(i);
        if !inclusive {
            self.push_block();
            let iv = self.load(ir::Place::local(i), ty);
            let ge = self.value(bool_ty, ir::Expr::Binary(ir::BinOp::Ge, iv, e));
            self.emit(ir::Stmt::If { cond: ge, then: vec![ir::Stmt::Break], else_: Vec::new() });
            self.block(body);
            let b = self.pop_block();
            let continuing = self.increment(i);
            self.emit(ir::Stmt::Loop { body: b, continuing });
            return;
        }
        // `a..=b`: stop after `b` without computing `b + 1`, which could overflow.
        let done = self.new_local("done", bool_ty);
        let gt = self.value(bool_ty, ir::Expr::Binary(ir::BinOp::Gt, s, e));
        self.emit(ir::Stmt::Store(ir::Place::local(done), gt));
        self.push_block();
        let dv = self.load(ir::Place::local(done), bool_ty);
        self.emit(ir::Stmt::If { cond: dv, then: vec![ir::Stmt::Break], else_: Vec::new() });
        self.block(body);
        let b = self.pop_block();
        self.push_block();
        let iv = self.load(ir::Place::local(i), ty);
        let last = self.value(bool_ty, ir::Expr::Binary(ir::BinOp::Eq, iv, e));
        let t = self.konst(ir::Const::Bool(true));
        let inc = self.increment(i);
        self.emit(ir::Stmt::If {
            cond: last,
            then: vec![ir::Stmt::Store(ir::Place::local(done), t)],
            else_: inc,
        });
        let continuing = self.pop_block();
        self.emit(ir::Stmt::Loop { body: b, continuing });
    }

    fn for_each(&mut self, var: thir::LocalId, array: &thir::Expr, body: &thir::Block) {
        let at = self.concrete(array.ty);
        let Some(ap) = self.place_or_temp(array) else { return };
        let u = self.mb.m.types.u32();
        let bool_ty = self.mb.m.types.bool();
        let elem_t = match self.types().kind(at).clone() {
            TyKind::Array(e, _) | TyKind::Slice(e) => e,
            _ => return,
        };
        let n = match self.types().kind(at).clone() {
            TyKind::Array(_, n) => Some(n),
            _ => None,
        };
        let idx = self.new_local("index", u);
        let zero = self.u32c(0);
        self.emit(ir::Stmt::Store(ir::Place::local(idx), zero));
        self.push_block();
        let iv = self.load(ir::Place::local(idx), u);
        let len = match n {
            Some(n) => self.u32c(n),
            None => {
                // A run on the CPU: its length is its second word.
                let rt = self.ty(at, array.span);
                let Some(rt) = rt else { return };
                let run = self.load(ap.clone(), rt);
                self.value(u, ir::Expr::Extract(run, 1))
            }
        };
        let ge = self.value(bool_ty, ir::Expr::Binary(ir::BinOp::Ge, iv, len));
        self.emit(ir::Stmt::If { cond: ge, then: vec![ir::Stmt::Break], else_: Vec::new() });
        let elem_place = ap.with(ir::Proj::Index(iv));
        let decl = self.body.local(var).clone();
        match decl.kind {
            LocalKind::Projection { .. } => self.locals[var.index()] = Repr::Place(elem_place),
            _ => {
                if let Some(et) = self.ty(elem_t, decl.span) {
                    let v = self.load(elem_place, et);
                    let l = self.new_local(&decl.name, et);
                    self.emit(ir::Stmt::Store(ir::Place::local(l), v));
                    self.locals[var.index()] = Repr::Var(l);
                } else {
                    self.locals[var.index()] = Repr::Erased;
                }
            }
        }
        self.block(body);
        let b = self.pop_block();
        let continuing = self.increment(idx);
        self.emit(ir::Stmt::Loop { body: b, continuing });
    }

    fn bind(&mut self, pat: &thir::Pat, init: &thir::Expr) {
        if let PatKind::Bind(l) = pat.kind {
            let decl = self.body.local(l).clone();
            // A closure or function bound to a name: a callable.
            if let Some(c) = self.callable_of(init) {
                self.locals[l.index()] = c;
                return;
            }
            match decl.kind {
                LocalKind::Projection { .. } => {
                    match self.place(init) {
                        Some(p) => self.locals[l.index()] = Repr::Place(p),
                        None => {
                            // A temporary bound by `let`: it owns it after all.
                            self.owned_binding(l, &decl, init);
                        }
                    }
                }
                _ => self.owned_binding(l, &decl, init),
            }
            return;
        }
        let it = self.concrete(init.ty);
        let Some(p) = self.place_or_temp(init) else {
            self.erase_bindings(pat);
            return;
        };
        self.pat_bind(pat, p, it);
    }

    fn owned_binding(&mut self, l: thir::LocalId, decl: &thir::LocalDecl, init: &thir::Expr) {
        let v = self.expr(init);
        match (self.ty(decl.ty, decl.span), v) {
            (Some(t), Some(v)) => {
                let lv = self.new_local(&decl.name, t);
                self.emit(ir::Stmt::Store(ir::Place::local(lv), v));
                self.locals[l.index()] = Repr::Var(lv);
            }
            _ => self.locals[l.index()] = Repr::Erased,
        }
    }

    /// If `e` is a closure, a named function, or a local holding one: its callable here.
    pub fn callable_of(&mut self, e: &thir::Expr) -> Option<Repr> {
        match &e.kind {
            ExprKind::Closure(id) => {
                let owner = rc(self.owner_key());
                let c = Callable::Closure { owner, id: *id };
                let srcs = self.capture_sources(*id);
                Some(Repr::Callable(c, srcs))
            }
            ExprKind::FnRef(f, args) => {
                let substs = args.iter().map(|&a| self.concrete(a)).collect();
                Some(Repr::Callable(Callable::Func { func: *f, substs }, Vec::new()))
            }
            ExprKind::Local(l) => match &self.locals[l.index()] {
                r @ Repr::Callable(..) => Some(r.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// The instance that owns closures created in this body (a closure's own body creates
    /// closures owned by the same function instance).
    fn owner_key(&self) -> InstanceKey {
        match &self.key {
            InstanceKey::Closure { owner, .. } => (**owner).clone(),
            k => k.clone(),
        }
    }

    /// Where a new closure's captures are in this function.
    fn capture_sources(&mut self, id: ClosureId) -> Vec<CaptureSrc> {
        let def = self.body.closures[id.0 as usize].clone();
        let mut out = Vec::new();
        for (l, written) in def.captures {
            let decl = self.body.local(l).clone();
            let t = self.concrete(decl.ty);
            if matches!(self.types().kind(t), TyKind::Closure(..) | TyKind::FnPtr(..)) {
                continue;
            }
            let Some(ty) = self.cx.lower_ty(self.mb, t, decl.span) else { continue };
            let (by_ref, _) =
                crate::ty::capture_passing(self.mb.target, &self.mb.m.types, ty, written);
            let place = match self.locals[l.index()].clone() {
                Repr::Var(v) => ir::Place::local(v),
                Repr::Place(p) => p,
                _ => continue,
            };
            out.push(CaptureSrc { place, by_ref });
        }
        out
    }

    // ---- calls -----------------------------------------------------------------------------

    pub fn call(&mut self, c: &thir::Call, e: &thir::Expr) -> Option<ir::ValueId> {
        let span = e.span;
        match &c.callee {
            Callee::Builtin(BuiltinFn::Len) => self.len(&c.args[0], e),
            Callee::Builtin(b) => {
                let args: Vec<ir::ValueId> = c.args.iter().filter_map(|a| self.expr(a)).collect();
                let ty = self.ty(e.ty, span)?;
                self.builtin(*b, args, ty, span)
            }
            Callee::Clone => self.expr(&c.args[0]),
            Callee::Local(l) => {
                let repr = self.locals[l.index()].clone();
                let Repr::Callable(callable, srcs) = repr else { return None };
                self.call_callable(&callable, &srcs, &c.args, e)
            }
            Callee::Fn { func, args } => {
                let substs: Vec<TyId> = args.iter().map(|&a| self.concrete(a)).collect();
                if self.cx.checked.program.func(*func).attrs.intrinsic {
                    return crate::gpu::intrinsic(self, *func, &substs, c, e);
                }
                self.call_fn(*func, substs, c, e)
            }
            Callee::TraitMethod { method, self_ty, trait_args, method_args } => {
                let st = self.concrete(*self_ty);
                let ta: Vec<TyId> = trait_args.iter().map(|&a| self.concrete(a)).collect();
                let ma: Vec<TyId> = method_args.iter().map(|&a| self.concrete(a)).collect();
                let Some((func, subst)) = wrela_sema::traits::resolve_trait_method(
                    &mut self.cx.checked.program,
                    *method,
                    st,
                    &ta,
                    &ma,
                ) else {
                    let shown = self.cx.checked.program.display_ty(st);
                    self.cx.err(Diagnostic::new(
                        codes::E0702,
                        span,
                        format!("internal: no implementation of this method for `{shown}`"),
                    ));
                    return None;
                };
                let generics = self.cx.checked.program.fn_all_generics(func);
                let substs: Vec<TyId> = generics
                    .iter()
                    .map(|g| subst.get(*g).unwrap_or(self.cx.checked.program.types.error))
                    .collect();
                let substs: Vec<TyId> = substs.into_iter().map(|t| self.cx.reveal(t)).collect();
                if self.cx.checked.program.func(func).attrs.intrinsic {
                    return crate::gpu::intrinsic(self, func, &substs, c, e);
                }
                self.call_fn(func, substs, c, e)
            }
        }
    }

    /// A call to a monomorphized function: arguments by its parameter modes, callables and
    /// resources becoming part of the instance.
    fn call_fn(
        &mut self,
        func: FnId,
        substs: Vec<TyId>,
        c: &thir::Call,
        e: &thir::Expr,
    ) -> Option<ir::ValueId> {
        let def = self.cx.checked.program.func(func).clone();
        let generics = self.cx.checked.program.fn_all_generics(func);
        let subst = Subst::from_pairs(&generics, &substs);
        let mut callables = Vec::new();
        let mut resources = Vec::new();
        let mut out: Vec<ir::Arg> = Vec::new();
        for (p, a) in def.params.iter().zip(&c.args) {
            let pt = self.cx.concrete(p.ty, &subst);
            if let TyKind::FnPtr(..) = self.types().kind(pt) {
                match self.callable_of(a) {
                    Some(Repr::Callable(callable, srcs)) => {
                        for s in &srcs {
                            out.push(self.capture_arg(s));
                        }
                        callables.push(Some(callable));
                    }
                    _ => callables.push(None),
                }
                resources.push(None);
                continue;
            }
            callables.push(None);
            if self.is_gpu() && crate::gpu::is_resource_param(self, pt) {
                let r = self.place(a).and_then(|pl| match (pl.root, pl.path.is_empty()) {
                    (ir::PlaceRoot::Resource(r), true) => Some(r),
                    _ => None,
                });
                if r.is_none() {
                    self.cx.err(Diagnostic::new(
                        codes::E0702,
                        a.span,
                        "GPU code can pass a buffer on only as the kernel received it",
                    ));
                }
                resources.push(r);
                continue;
            }
            resources.push(None);
            let Some(ty) = self.cx.lower_ty(self.mb, pt, p.span) else {
                let _ = self.expr(a); // nothing to pass; evaluate it for its effects
                continue;
            };
            let (by_ref, _) =
                crate::ty::param_passing(self.mb.target, &self.mb.m.types, ty, p.mode);
            if by_ref {
                let inner = match &a.kind {
                    ExprKind::MutArg(x) | ExprKind::Take(x) => x.as_ref(),
                    _ => a,
                };
                out.push(ir::Arg::Place(self.place_or_temp(inner)?));
            } else if matches!(self.mb.m.types.get(ty), ir::TypeDef::Run(_)) && {
                let at = self.concrete(a.ty);
                matches!(self.types().kind(at), TyKind::Array(..))
            } {
                // A fixed array passed for a run.
                let pl = self.place_or_temp(a)?;
                out.push(ir::Arg::Value(self.value(ty, ir::Expr::Run(pl))));
            } else {
                out.push(ir::Arg::Value(self.expr(a)?));
            }
        }
        let key = InstanceKey::Fn { func, substs, callables, resources };
        let callee = self.cx.instance(self.mb, key, Some((self.id, e.span)));
        let ret = self.mb.m.functions[callee.index()].ret;
        let ret_ref = self.mb.m.functions[callee.index()].ret_ref;
        match ret {
            Some(t) if ret_ref => {
                let pt = self.mb.m.types.intern(ir::TypeDef::Ptr(t));
                Some(self.value(pt, ir::Expr::Call(callee, out)))
            }
            Some(t) => Some(self.value(t, ir::Expr::Call(callee, out))),
            None => {
                self.emit(ir::Stmt::Eval(ir::Expr::Call(callee, out)));
                None
            }
        }
    }

    fn capture_arg(&mut self, s: &CaptureSrc) -> ir::Arg {
        if s.by_ref {
            ir::Arg::Place(s.place.clone())
        } else {
            let ty = self.place_ty(&s.place);
            ir::Arg::Value(self.load(s.place.clone(), ty))
        }
    }

    pub fn place_ty(&mut self, p: &ir::Place) -> ir::TypeId {
        let mut t = match &p.root {
            ir::PlaceRoot::Local(l) => self.f.locals[l.index()].ty,
            ir::PlaceRoot::Param(i) => self.f.params[*i as usize].ty,
            ir::PlaceRoot::Resource(r) => self.mb.m.resources[r.index()].ty,
            ir::PlaceRoot::Ptr(v) => match self.mb.m.types.get(self.f.value_ty(*v)) {
                ir::TypeDef::Ptr(t) => *t,
                _ => self.f.value_ty(*v),
            },
        };
        if let ir::PlaceRoot::Resource(r) = &p.root
            && !matches!(self.mb.m.resources[r.index()].kind, ir::ResourceKind::Uniform { .. })
        {
            // A storage buffer's place is its array.
            t = self.mb.m.types.intern(ir::TypeDef::RuntimeArray(t));
        }
        for proj in &p.path {
            t = match (self.mb.m.types.get(t), proj) {
                (ir::TypeDef::Struct { fields, .. }, ir::Proj::Field(i)) => fields[*i as usize].1,
                (ir::TypeDef::Vector(_), _) => self.mb.m.types.f32(),
                (ir::TypeDef::Matrix(n), _) => {
                    let n = *n;
                    self.mb.m.types.vector(n)
                }
                (
                    ir::TypeDef::Array(e, _) | ir::TypeDef::RuntimeArray(e) | ir::TypeDef::Run(e),
                    _,
                ) => *e,
                _ => t,
            };
        }
        t
    }

    /// Calls a closure or function value with `args`.
    pub fn call_callable(
        &mut self,
        callable: &Callable,
        srcs: &[CaptureSrc],
        args: &[thir::Expr],
        e: &thir::Expr,
    ) -> Option<ir::ValueId> {
        let mut out: Vec<ir::Arg> = srcs.iter().map(|s| self.capture_arg(s)).collect();
        let key = match callable {
            Callable::Closure { owner, id } => {
                InstanceKey::Closure { owner: owner.clone(), id: *id }
            }
            Callable::Func { func, substs } => InstanceKey::plain(*func, substs.clone()),
        };
        for a in args {
            if let Some(v) = self.expr(a) {
                out.push(ir::Arg::Value(v));
            }
        }
        let callee = self.cx.instance(self.mb, key, Some((self.id, e.span)));
        match self.mb.m.functions[callee.index()].ret {
            Some(t) => Some(self.value(t, ir::Expr::Call(callee, out))),
            None => {
                self.emit(ir::Stmt::Eval(ir::Expr::Call(callee, out)));
                None
            }
        }
    }

    // ---- built-ins -------------------------------------------------------------------------

    /// `xs.len()`: an array's length is its type's; a run's is in the run (CPU) or the buffer's
    /// (GPU).
    fn len(&mut self, xs: &thir::Expr, e: &thir::Expr) -> Option<ir::ValueId> {
        let u = self.mb.m.types.u32();
        let t = self.concrete(xs.ty);
        if let TyKind::Array(_, n) = self.types().kind(t) {
            let n = *n;
            return Some(self.value(u, ir::Expr::Const(ir::Const::U32(n))));
        }
        if self.is_gpu() {
            let p = self.place(xs)?;
            return Some(self.value(u, ir::Expr::ArrayLength(p)));
        }
        let _ = e;
        let run = self.expr(xs)?;
        Some(self.value(u, ir::Expr::Extract(run, 1)))
    }

    fn builtin(
        &mut self,
        b: BuiltinFn,
        args: Vec<ir::ValueId>,
        ty: ir::TypeId,
        span: Span,
    ) -> Option<ir::ValueId> {
        use ir::Builtin as I;
        let ib = match b {
            BuiltinFn::Sqrt => I::Sqrt,
            BuiltinFn::InverseSqrt => I::InverseSqrt,
            BuiltinFn::Sin => I::Sin,
            BuiltinFn::Cos => I::Cos,
            BuiltinFn::Tan => I::Tan,
            BuiltinFn::Asin => I::Asin,
            BuiltinFn::Acos => I::Acos,
            BuiltinFn::Atan => I::Atan,
            BuiltinFn::Atan2 => I::Atan2,
            BuiltinFn::Exp => I::Exp,
            BuiltinFn::Exp2 => I::Exp2,
            BuiltinFn::Log => I::Log,
            BuiltinFn::Log2 => I::Log2,
            BuiltinFn::Pow => I::Pow,
            BuiltinFn::Floor => I::Floor,
            BuiltinFn::Ceil => I::Ceil,
            BuiltinFn::Round => I::Round,
            BuiltinFn::Trunc => I::Trunc,
            BuiltinFn::Fract => I::Fract,
            BuiltinFn::Saturate => I::Saturate,
            BuiltinFn::Step => I::Step,
            BuiltinFn::Abs => I::Abs,
            BuiltinFn::Sign => I::Sign,
            BuiltinFn::Min => I::Min,
            BuiltinFn::Max => I::Max,
            BuiltinFn::Clamp => I::Clamp,
            BuiltinFn::Mix => I::Mix,
            BuiltinFn::Smoothstep => I::Smoothstep,
            BuiltinFn::Length => I::Length,
            BuiltinFn::Distance => I::Distance,
            BuiltinFn::Dot => I::Dot,
            BuiltinFn::Cross => I::Cross,
            BuiltinFn::Normalize => I::Normalize,
            BuiltinFn::Dpdx => I::Dpdx,
            BuiltinFn::Dpdy => I::Dpdy,
            BuiltinFn::Fwidth => I::Fwidth,
            BuiltinFn::Select => {
                return Some(self.value(
                    ty,
                    ir::Expr::Select { cond: args[2], if_true: args[1], if_false: args[0] },
                ));
            }
            BuiltinFn::BitcastU32 => {
                return Some(self.value(ty, ir::Expr::Bitcast(args[0], ir::Scalar::U32)));
            }
            BuiltinFn::BitcastI32 => {
                return Some(self.value(ty, ir::Expr::Bitcast(args[0], ir::Scalar::I32)));
            }
            BuiltinFn::BitcastF32 => {
                return Some(self.value(ty, ir::Expr::Bitcast(args[0], ir::Scalar::F32)));
            }
            BuiltinFn::BitcastU64 => {
                return Some(self.value(ty, ir::Expr::Bitcast(args[0], ir::Scalar::U64)));
            }
            BuiltinFn::BitcastF64 => {
                return Some(self.value(ty, ir::Expr::Bitcast(args[0], ir::Scalar::F64)));
            }
            BuiltinFn::WrappingAdd => {
                return Some(
                    self.value(ty, ir::Expr::Binary(ir::BinOp::WrappingAdd, args[0], args[1])),
                );
            }
            BuiltinFn::WrappingSub => {
                return Some(
                    self.value(ty, ir::Expr::Binary(ir::BinOp::WrappingSub, args[0], args[1])),
                );
            }
            BuiltinFn::WrappingMul => {
                return Some(
                    self.value(ty, ir::Expr::Binary(ir::BinOp::WrappingMul, args[0], args[1])),
                );
            }
            BuiltinFn::Len => unreachable!("`len` is lowered by `Fl::len`"),
        };
        if matches!(ib, I::Dpdx | I::Dpdy | I::Fwidth) {
            crate::gpu::check_derivative(self, span);
        }
        self.math(ib, args, ty, span)
    }

    /// A math builtin. On the CPU the transcendentals are std's wrela functions (§11), applied
    /// per component for vectors.
    /// A math builtin. On the CPU the transcendentals become calls to std's wrela functions
    /// (§11), but only after derived interpretations are built (`rewrite_cpu_math`), so
    /// gradients and intervals see the functions themselves.
    pub fn math(
        &mut self,
        b: ir::Builtin,
        args: Vec<ir::ValueId>,
        ty: ir::TypeId,
        span: Span,
    ) -> Option<ir::ValueId> {
        if !self.is_gpu()
            && crate::cpu_math_lang(b).is_some()
            && self.mb.m.types.element_scalar(ty) == Some(ir::Scalar::F64)
        {
            self.cx.err(
                Diagnostic::new(
                    codes::E0702,
                    span,
                    format!("`{b:?}` of an `f64` isn't supported yet"),
                )
                .with_note("std's CPU math works in `f32`; convert with `f32(x)`"),
            );
            return None;
        }
        Some(self.value(ty, ir::Expr::Builtin(b, args)))
    }
}

/// The signature of a derived function (gradient or interval).
pub(crate) fn derived_signature(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    key: &InstanceKey,
) -> ir::Function {
    crate::gpu::derived_signature(cx, mb, key)
}
