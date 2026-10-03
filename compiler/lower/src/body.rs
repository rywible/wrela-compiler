//! Function bodies: one instance's MIR, lowered to IR.
//!
//! The MIR's structured terminators become the IR's structured statements: an `If` lowers its
//! branches up to their merge block, a `Loop` its body up to the continuing block and the
//! continuing block up to its back edge, a `Match` each arm (the second and later inside
//! `if !matched`). A merge block nothing reaches isn't entered: both branches left.
//!
//! Locals become IR values (a temporary assigned once), IR locals (a variable, or a value
//! assigned on several paths), places (a projection aliases its place; lowering substitutes
//! it), or callables (closures and functions, inlined per instance).

use crate::instance::{Callable, InstanceKey};
use crate::{Cx, ModuleBuilder, rc};
use std::collections::HashMap;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_ir as ir;
use wrela_sema::builtins::BuiltinFn;
use wrela_sema::defs::{Lang, RetMode};
use wrela_sema::mir::{
    self, BlockId, Local, LocalKind, OperandKind, Rvalue, StatementKind, TerminatorKind,
};
use wrela_sema::thir::Lit;
use wrela_sema::ty::*;
use wrela_syntax::ast::{BinOp, UnOp};

/// How a MIR local exists in the IR function.
#[derive(Clone, Debug)]
pub(crate) enum Repr {
    /// Not given a value yet.
    Unbound,
    /// A temporary's value.
    Value(ir::ValueId),
    /// An IR local holding the value.
    Var(ir::LocalId),
    /// An alias of a place: a projection, a by-reference parameter, a resource.
    Place(ir::Place),
    /// An alias of some components of a vector place, in this order.
    Swizzle(ir::Place, Vec<u8>),
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
    /// The source function's MIR, and the code of this instance (the function, or a closure).
    pub mir: &'a mir::Body,
    pub code: &'a mir::FnBody,
    pub subst: Subst,
    pub locals: Vec<Repr>,
    /// The block being emitted is the last; enclosing ones before it.
    blocks: Vec<(ir::Block, bool)>,
    /// How many reachable blocks go to each block.
    preds: Vec<u32>,
    /// Each `Match` block's flag: whether an arm has matched.
    matched: HashMap<BlockId, ir::LocalId>,
}

pub(crate) fn lower_body(cx: &mut Cx, mb: &mut ModuleBuilder, key: &InstanceKey, id: ir::FuncId) {
    if let InstanceKey::Derived { .. } = key {
        crate::gpu::lower_derived(cx, mb, key, id);
        return;
    }
    let checked = cx.checked;
    let Some(mir) = key.source_fn().and_then(|f| checked.mir.get(&f)) else { return };
    let code = match key {
        InstanceKey::Closure { id: cid, .. } => mir.closure_fn(*cid),
        _ => &mir.fns[0],
    };
    let subst = cx.instance_subst(key);
    let f =
        std::mem::replace(&mut mb.m.functions[id.index()], ir::Function::new("", Vec::new(), None));
    let mut fl = Fl {
        cx,
        mb,
        key: key.clone(),
        id,
        f,
        mir,
        code,
        subst,
        locals: vec![Repr::Unbound; mir.locals.len()],
        blocks: vec![(Vec::new(), false)],
        preds: reachable_preds(code),
        matched: HashMap::new(),
    };
    match key {
        InstanceKey::Fn { .. } => fl.bind_fn_params(),
        InstanceKey::Closure { id: cid, .. } => fl.bind_closure_params(*cid),
        InstanceKey::Derived { .. } => {}
    }
    fl.region(mir::FnBody::ENTRY, None);
    if !fl.terminated() {
        // Off the end: only a function with no result gets here.
        let s = if fl.f.ret.is_some() { ir::Stmt::Trap } else { ir::Stmt::Return(None) };
        fl.emit(s);
    }
    fl.finish();
}

/// For each block, how many reachable blocks lead to it.
fn reachable_preds(code: &mir::FnBody) -> Vec<u32> {
    let n = code.blocks.len();
    let mut seen = vec![false; n];
    let mut work = vec![mir::FnBody::ENTRY];
    let mut preds = vec![0u32; n];
    while let Some(b) = work.pop() {
        if std::mem::replace(&mut seen[b.index()], true) {
            continue;
        }
        for s in code.successors(b) {
            preds[s.index()] += 1;
            work.push(s);
        }
    }
    preds
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

    /// Marks where the code that follows comes from (`ir::Stmt::At`): a marker that nothing
    /// followed yet is replaced.
    fn at(&mut self, span: Span) {
        let (b, term) = self.blocks.last_mut().expect("a block is open");
        if *term {
            return;
        }
        match b.last_mut() {
            Some(ir::Stmt::At(s)) => *s = span,
            _ => b.push(ir::Stmt::At(span)),
        }
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
        let c = self.concrete(t);
        self.cx.lower_ty(self.mb, c, span)
    }

    pub fn concrete(&mut self, t: TyId) -> TyId {
        let subst = self.subst.clone();
        self.cx.concrete(t, &subst)
    }

    pub fn types(&self) -> &'a Types {
        &self.cx.checked.program.types
    }

    pub fn is_gpu(&self) -> bool {
        self.mb.target == ir::Target::Gpu
    }

    fn local_ty(&self, l: Local) -> TyId {
        self.mir.local(l).ty
    }

    // ---- parameters ------------------------------------------------------------------------

    fn bind_fn_params(&mut self) {
        let InstanceKey::Fn { func, callables, resources, .. } = self.key.clone() else { return };
        let program = &self.cx.checked.program;
        let def = program.func(func);
        if let Some(entry) = self.cx.entry_of(func)
            && self.is_gpu()
            && self.mb.gpu.as_ref().is_some_and(|g| g.entry_fns.contains(&func))
        {
            crate::gpu::bind_entry_params(self, func, entry);
            return;
        }
        let mut k = 0u32;
        for (i, p) in def.params.iter().enumerate() {
            let local = self.code.params[i];
            let t = self.concrete(p.ty);
            if let TyKind::FnPtr(..) = self.types().kind(t) {
                match callables.get(i).cloned().flatten() {
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
        let info = &self.mir.closures[cid.0 as usize];
        let mut k = 0u32;
        // Captures first, in the order `callable_params` declared them.
        for &(l, _written) in &info.captures {
            let decl = self.mir.local(l);
            let t = self.concrete(decl.ty);
            if matches!(self.types().kind(t), TyKind::Closure(..) | TyKind::FnPtr(..)) {
                // A captured closure would need its own captures threaded through.
                self.cx.err(
                    Diagnostic::new(
                        codes::E0702,
                        decl.span,
                        "a closure that uses another closure from its surroundings isn't supported yet",
                    )
                    .with_help("pass the inner closure to the function directly"),
                );
                self.locals[l.index()] = Repr::Erased;
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
        for &l in &info.params {
            let decl = self.mir.local(l);
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

    // ---- control flow ----------------------------------------------------------------------

    /// Lowers the code from `start` until control reaches `stop` (or leaves).
    fn region(&mut self, start: BlockId, stop: Option<BlockId>) {
        let mut b = start;
        loop {
            if Some(b) == stop {
                return;
            }
            let block = self.code.block(b);
            for s in &block.stmts {
                self.statement(s);
            }
            self.at(block.term.span);
            match &block.term.kind {
                TerminatorKind::Goto(t) => b = *t,
                TerminatorKind::If { cond, then, else_, merge } => {
                    let Some(c) = self.operand(cond) else { return };
                    self.push_block();
                    self.region(*then, Some(*merge));
                    let tb = self.pop_block();
                    self.push_block();
                    self.region(*else_, Some(*merge));
                    let eb = self.pop_block();
                    self.emit(ir::Stmt::If { cond: c, then: tb, else_: eb });
                    if self.preds[merge.index()] == 0 {
                        return;
                    }
                    b = *merge;
                }
                TerminatorKind::Loop { body, continuing, merge } => {
                    self.push_block();
                    self.region(*body, Some(*continuing));
                    let lb = self.pop_block();
                    self.push_block();
                    self.region(*continuing, None);
                    let cb = self.pop_block();
                    self.emit(ir::Stmt::Loop { body: lb, continuing: cb });
                    if self.preds[merge.index()] == 0 {
                        return;
                    }
                    b = *merge;
                }
                TerminatorKind::LoopBack(_) => return,
                TerminatorKind::Break(_) => {
                    self.emit(ir::Stmt::Break);
                    return;
                }
                TerminatorKind::Continue(_) => {
                    self.emit(ir::Stmt::Continue);
                    return;
                }
                TerminatorKind::Match { arms, merge } => {
                    // `matched` is set when an arm's body finishes; later arms run only while
                    // it's false.
                    let bool_ty = self.mb.m.types.bool();
                    let flag = self.new_local("matched", bool_ty);
                    self.matched.insert(b, flag);
                    let f = self.konst(ir::Const::Bool(false));
                    self.emit(ir::Stmt::Store(ir::Place::local(flag), f));
                    for (i, &arm) in arms.iter().enumerate() {
                        self.push_block();
                        self.region(arm, None);
                        let body = self.pop_block();
                        if i == 0 {
                            for s in body {
                                self.emit(s);
                            }
                            continue;
                        }
                        let m = self.load(ir::Place::local(flag), bool_ty);
                        let not_m = self.value(bool_ty, ir::Expr::Unary(ir::UnOp::Not, m));
                        self.emit(ir::Stmt::If { cond: not_m, then: body, else_: Vec::new() });
                    }
                    if self.preds[merge.index()] == 0 {
                        return;
                    }
                    b = *merge;
                }
                TerminatorKind::ArmMatched(m) => {
                    if let Some(&flag) = self.matched.get(m) {
                        let t = self.konst(ir::Const::Bool(true));
                        self.emit(ir::Stmt::Store(ir::Place::local(flag), t));
                    }
                    return;
                }
                TerminatorKind::ArmFailed { .. } => return,
                TerminatorKind::Return(o) => {
                    let v = o.as_ref().and_then(|o| self.operand(o));
                    match (self.f.ret, v) {
                        (None, _) => self.emit(ir::Stmt::Return(None)),
                        (Some(_), Some(v)) => self.emit(ir::Stmt::Return(Some(v))),
                        (Some(_), None) => self.emit(ir::Stmt::Trap),
                    }
                    return;
                }
                TerminatorKind::ReturnPlace(p) => {
                    let pt = self.f.ret.map(|t| self.mb.m.types.intern(ir::TypeDef::Ptr(t)));
                    match (self.place(p), pt) {
                        (Some(p), Some(pt)) => {
                            let a = self.value(pt, ir::Expr::Addr(p));
                            self.emit(ir::Stmt::Return(Some(a)));
                        }
                        _ => self.emit(ir::Stmt::Trap),
                    }
                    return;
                }
                TerminatorKind::Unreachable => {
                    self.emit(ir::Stmt::Trap);
                    return;
                }
            }
        }
    }

    // ---- statements ------------------------------------------------------------------------

    fn statement(&mut self, s: &mir::Statement) {
        if self.terminated() {
            return;
        }
        if !matches!(s.kind, StatementKind::Live(_) | StatementKind::Dead(_)) {
            self.at(s.span);
        }
        match &s.kind {
            StatementKind::Assign(place, r) => self.assign(place, r, s.span),
            StatementKind::Eval(r) => {
                let _ = self.rvalue(r, None, s.span);
            }
            StatementKind::Bind { local, place, .. } => {
                let repr = match &self.locals[place.local.index()] {
                    r @ Repr::Callable(..) if place.proj.is_empty() => r.clone(),
                    _ => match self.place_parts(place) {
                        Some((p, _, None)) => Repr::Place(p),
                        Some((p, _, Some(cs))) => Repr::Swizzle(p, cs),
                        None => Repr::Erased,
                    },
                };
                self.locals[local.index()] = repr;
            }
            StatementKind::Live(_) | StatementKind::Dead(_) => {}
        }
    }

    fn assign(&mut self, place: &mir::Place, r: &Rvalue, span: Span) {
        let l = place.local;
        if place.proj.is_empty() {
            // A callable: no IR value; the local stands for it.
            if let Some(c) = self.callable_rvalue(r) {
                self.locals[l.index()] = c;
                return;
            }
            let kind = self.mir.local(l).kind;
            let through_alias = matches!(self.locals[l.index()], Repr::Place(_))
                && !matches!(r, Rvalue::Call(c) if c.ret_mode != RetMode::Owned);
            if !through_alias {
                let ty = self.local_ty(l);
                let v = self.rvalue(r, Some(ty), span);
                match kind {
                    LocalKind::Temp => {
                        self.locals[l.index()] = v.map_or(Repr::Erased, Repr::Value);
                    }
                    LocalKind::TempProjection { .. } => {
                        // What a projection-returning call returned: a pointer to its place.
                        self.locals[l.index()] = match v {
                            Some(p) => Repr::Place(ir::Place {
                                root: ir::PlaceRoot::Ptr(p),
                                path: Vec::new(),
                            }),
                            None => Repr::Erased,
                        };
                    }
                    _ => {
                        let Some(v) = v else {
                            if matches!(self.locals[l.index()], Repr::Unbound) {
                                self.locals[l.index()] = Repr::Erased;
                            }
                            return;
                        };
                        let var = match self.locals[l.index()] {
                            Repr::Var(var) => var,
                            _ => {
                                let ty = self.f.value_ty(v);
                                let name = self.mir.local(l).name.clone();
                                let var = self.new_local(&name, ty);
                                self.locals[l.index()] = Repr::Var(var);
                                var
                            }
                        };
                        self.emit(ir::Stmt::Store(ir::Place::local(var), v));
                    }
                }
                return;
            }
        }
        // A store into a place (or through a projection).
        let ty = mir::place_ty(&self.cx.checked.program, &self.mir.locals, place);
        let Some(v) = self.rvalue(r, Some(ty), span) else { return };
        self.store(place, v);
    }

    /// The callable an rvalue makes: a closure, a named function, or a copy of a local's.
    fn callable_rvalue(&mut self, r: &Rvalue) -> Option<Repr> {
        match r {
            Rvalue::Closure(id) => {
                let owner = rc(self.owner_key());
                let c = Callable::Closure { owner, id: *id };
                let srcs = self.capture_sources(*id);
                Some(Repr::Callable(c, srcs))
            }
            Rvalue::FnRef(f, args) => {
                let substs = args.iter().map(|&a| self.concrete(a)).collect();
                Some(Repr::Callable(Callable::Func { func: *f, substs }, Vec::new()))
            }
            Rvalue::Use(mir::Operand { kind: OperandKind::Copy(p), .. }) if p.proj.is_empty() => {
                match &self.locals[p.local.index()] {
                    r @ Repr::Callable(..) => Some(r.clone()),
                    _ => None,
                }
            }
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
        let mut out = Vec::new();
        for &(l, written) in &self.mir.closures[id.0 as usize].captures {
            let decl = self.mir.local(l);
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
                Repr::Value(v) => self.spill(l, v),
                Repr::Swizzle(..) if written => {
                    self.cx.err(
                        Diagnostic::new(
                            codes::E0702,
                            decl.span,
                            "a closure can't change a projection of several vector components yet",
                        )
                        .with_help("change the components through the vector itself"),
                    );
                    continue;
                }
                Repr::Swizzle(..) => {
                    // Components alias no single place; the closure only reads them.
                    let Some(v) = self.read(&mir::Place::local(l)) else { continue };
                    let ty = self.f.value_ty(v);
                    let copy = self.new_local("tmp", ty);
                    self.emit(ir::Stmt::Store(ir::Place::local(copy), v));
                    ir::Place::local(copy)
                }
                _ => continue,
            };
            out.push(CaptureSrc { place, by_ref });
        }
        out
    }

    // ---- places ----------------------------------------------------------------------------

    /// Moves a temporary's value into an IR local, so it has a place.
    fn spill(&mut self, l: Local, v: ir::ValueId) -> ir::Place {
        let ty = self.f.value_ty(v);
        let var = self.new_local("tmp", ty);
        self.emit(ir::Stmt::Store(ir::Place::local(var), v));
        self.locals[l.index()] = Repr::Var(var);
        ir::Place::local(var)
    }

    /// The IR place of a MIR place, its type, and a trailing swizzle (which no IR place can
    /// name). `None` when it has no runtime value.
    fn place_parts(&mut self, p: &mir::Place) -> Option<(ir::Place, TyId, Option<Vec<u8>>)> {
        let mut ty = self.concrete(self.local_ty(p.local));
        let mut swizzle = None;
        let mut place = match self.locals[p.local.index()].clone() {
            Repr::Var(v) => ir::Place::local(v),
            Repr::Place(ip) => ip,
            Repr::Swizzle(ip, cs) => {
                swizzle = Some(cs);
                ip
            }
            Repr::Value(v) => self.spill(p.local, v),
            Repr::Unbound | Repr::Callable(..) | Repr::Erased => return None,
        };
        let mut variant: Option<u32> = None;
        for proj in &p.proj {
            if let Some(cs) = &swizzle {
                // Through an alias of components: they pick from its vector.
                match proj {
                    mir::Proj::Comp(c) => {
                        place = place.with(ir::Proj::Comp(cs[*c as usize]));
                        swizzle = None;
                        ty = self.types().f32;
                    }
                    mir::Proj::Swizzle(more) => {
                        let picked: Vec<u8> = more.iter().map(|&c| cs[c as usize]).collect();
                        ty = self.types().vec(picked.len() as u8);
                        swizzle = Some(picked);
                    }
                    _ => return None,
                }
                continue;
            }
            match proj {
                mir::Proj::Downcast(v) => {
                    let payload = self.cx.payload_field(self.mb, ty, *v, self.code.span)?;
                    place = place.with(ir::Proj::Field(payload));
                    variant = Some(*v);
                }
                mir::Proj::Field(i) => {
                    let map = self.cx.field_map(self.mb, ty, variant, self.code.span);
                    let fi = map.get(*i as usize).copied().flatten()?;
                    ty = self.field_ty(ty, variant, *i);
                    variant = None;
                    place = place.with(ir::Proj::Field(fi));
                }
                mir::Proj::Comp(c) => {
                    place = place.with(ir::Proj::Comp(*c));
                    ty = self.types().f32;
                }
                mir::Proj::Swizzle(cs) => {
                    swizzle = Some(cs.clone());
                    ty = self.types().vec(cs.len() as u8);
                }
                mir::Proj::Index(t) => {
                    let iv = self.local_value(*t)?;
                    let k = self.types().kind(ty);
                    if let TyKind::Adt(a, args) = &k
                        && self.cx.checked.program.is_lang_adt(*a, Lang::Slots)
                    {
                        let idx = crate::gpu::slot_index(self, iv)?;
                        place = place.with(ir::Proj::Index(idx));
                        ty = args[0];
                        continue;
                    }
                    place = place.with(ir::Proj::Index(iv));
                    ty = match k {
                        TyKind::Array(e, _) | TyKind::Slice(e) => *e,
                        TyKind::Vec(_) => self.types().f32,
                        TyKind::Mat(n) => self.types().vec(*n),
                        _ => self.types().error,
                    };
                }
            }
        }
        Some((place, ty, swizzle))
    }

    fn field_ty(&self, ty: TyId, variant: Option<u32>, i: u32) -> TyId {
        let p = &self.cx.checked.program;
        match p.types.kind(ty) {
            TyKind::Adt(a, args) => {
                let fields = match variant {
                    Some(v) => p.variant_fields(*a, args, v as usize),
                    None => p.struct_fields(*a, args),
                };
                fields.get(i as usize).map_or(p.types.error, |f| f.1)
            }
            TyKind::Tuple(ts) => ts.get(i as usize).copied().unwrap_or(p.types.error),
            _ => p.types.error,
        }
    }

    /// The IR place of a MIR place without a trailing swizzle.
    pub fn place(&mut self, p: &mir::Place) -> Option<ir::Place> {
        let (place, _, swizzle) = self.place_parts(p)?;
        swizzle.is_none().then_some(place)
    }

    /// A place holding a MIR place's value, for passing by reference: the place itself, or a
    /// temporary copy of components (which no single place holds; they're read-only there).
    fn place_or_copy(&mut self, p: &mir::Place) -> Option<ir::Place> {
        if let Some(place) = self.place(p) {
            return Some(place);
        }
        let v = self.read(p)?;
        let ty = self.f.value_ty(v);
        let l = self.new_local("tmp", ty);
        self.emit(ir::Stmt::Store(ir::Place::local(l), v));
        Some(ir::Place::local(l))
    }

    /// A local's value: a temporary's, or a load of a variable.
    fn local_value(&mut self, l: Local) -> Option<ir::ValueId> {
        match self.locals[l.index()].clone() {
            Repr::Value(v) => Some(v),
            Repr::Var(_) | Repr::Place(_) | Repr::Swizzle(..) => self.read(&mir::Place::local(l)),
            _ => None,
        }
    }

    /// Reads a place's value. A temporary's fields and components come from its value.
    pub fn read(&mut self, p: &mir::Place) -> Option<ir::ValueId> {
        if let Repr::Value(v) = self.locals[p.local.index()].clone() {
            if p.proj.is_empty() {
                return Some(v);
            }
            let direct = p.proj.iter().all(|x| {
                matches!(x, mir::Proj::Field(_) | mir::Proj::Comp(_) | mir::Proj::Swizzle(_))
            });
            if direct {
                return self.extract_path(v, p);
            }
        }
        let (place, ty, swizzle) = self.place_parts(p)?;
        match swizzle {
            Some(cs) => {
                // Load the vector the components are of, then pick them.
                let vt = self.place_ty(&place)?;
                let v = self.load(place, vt);
                let st = self.cx.lower_ty(self.mb, ty, self.code.span)?;
                Some(self.value(st, ir::Expr::Swizzle(v, cs)))
            }
            None => {
                let it = self.cx.lower_ty(self.mb, ty, self.code.span)?;
                Some(self.load(place, it))
            }
        }
    }

    /// Fields and components of a temporary's value.
    fn extract_path(&mut self, mut v: ir::ValueId, p: &mir::Place) -> Option<ir::ValueId> {
        let mut ty = self.concrete(self.local_ty(p.local));
        for proj in &p.proj {
            match proj {
                mir::Proj::Field(i) => {
                    let map = self.cx.field_map(self.mb, ty, None, self.code.span);
                    let fi = map.get(*i as usize).copied().flatten()?;
                    ty = self.field_ty(ty, None, *i);
                    let it = self.cx.lower_ty(self.mb, ty, self.code.span)?;
                    v = self.value(it, ir::Expr::Extract(v, fi));
                }
                mir::Proj::Comp(c) => {
                    ty = self.types().f32;
                    let f32 = self.mb.m.types.f32();
                    v = self.value(f32, ir::Expr::Extract(v, *c as u32));
                }
                mir::Proj::Swizzle(cs) => {
                    ty = self.types().vec(cs.len() as u8);
                    let vt = self.mb.m.types.vector(cs.len() as u8);
                    v = self.value(vt, ir::Expr::Swizzle(v, cs.clone()));
                }
                _ => return None,
            }
        }
        Some(v)
    }

    /// Writes `v` to a place; a swizzle's components one by one.
    fn store(&mut self, p: &mir::Place, v: ir::ValueId) {
        let Some((place, _, swizzle)) = self.place_parts(p) else { return };
        match swizzle {
            Some(cs) => {
                let f32 = self.mb.m.types.f32();
                for (k, c) in cs.into_iter().enumerate() {
                    let comp = self.value(f32, ir::Expr::Extract(v, k as u32));
                    self.emit(ir::Stmt::Store(place.with(ir::Proj::Comp(c)), comp));
                }
            }
            None => self.emit(ir::Stmt::Store(place, v)),
        }
    }

    pub fn load(&mut self, p: ir::Place, ty: ir::TypeId) -> ir::ValueId {
        self.value(ty, ir::Expr::Load(p))
    }

    /// An IR place's type in the function being lowered.
    pub fn place_ty(&mut self, p: &ir::Place) -> Option<ir::TypeId> {
        let t = self.mb.m.place_ty(&self.f, p);
        if t.is_none() {
            self.cx.err(Diagnostic::internal(format!("a place with no type in lowering: {p:?}")));
        }
        t
    }

    // ---- operands and rvalues --------------------------------------------------------------

    pub fn operand(&mut self, o: &mir::Operand) -> Option<ir::ValueId> {
        match &o.kind {
            OperandKind::Const(l) => {
                let ty = self.ty(o.ty, o.span)?;
                let c = self.lit(*l, ty)?;
                Some(self.value(ty, ir::Expr::Const(c)))
            }
            OperandKind::Copy(p) | OperandKind::Move(p, _) => self.read(p),
        }
    }

    fn lit(&mut self, l: Lit, ty: ir::TypeId) -> Option<ir::Const> {
        let s = self.mb.m.types.as_scalar(ty)?;
        Some(match (l, s) {
            (Lit::Bool(b), _) => ir::Const::Bool(b),
            (Lit::Int(v), ir::Scalar::I32) => ir::Const::I32(v as i32),
            (Lit::Int(v), ir::Scalar::U32) => ir::Const::U32(v as u32),
            (Lit::Int(v), ir::Scalar::I64) => ir::Const::I64(v as i64),
            (Lit::Int(v), ir::Scalar::U64) => ir::Const::U64(v as u64),
            (Lit::Int(v), ir::Scalar::F32) => ir::Const::F32(v as f32),
            (Lit::Int(v), ir::Scalar::F64) => ir::Const::F64(v as f64),
            (Lit::Int(v), s) => ir::Const::Small(s, v as i64),
            (Lit::Float(v), ir::Scalar::F32) => ir::Const::F32(v as f32),
            (Lit::Float(v), _) => ir::Const::F64(v),
        })
    }

    /// An rvalue's value; `ty` is its type, when it has one.
    fn rvalue(&mut self, r: &Rvalue, ty: Option<TyId>, span: Span) -> Option<ir::ValueId> {
        match r {
            Rvalue::Use(o) => self.operand(o),
            Rvalue::Unary(op, x) => {
                let v = self.operand(x)?;
                let t = self.ty(x.ty, x.span)?;
                let op = match op {
                    UnOp::Neg => ir::UnOp::Neg,
                    UnOp::Not => ir::UnOp::Not,
                };
                Some(self.value(t, ir::Expr::Unary(op, v)))
            }
            Rvalue::Binary(op, a, b) => self.binary(*op, a, b, ty?, span),
            Rvalue::Adt { variant, fields, .. } => self.adt(*variant, fields, ty?, span),
            Rvalue::Tuple(xs) | Rvalue::Array(xs) => {
                let t = self.ty(ty?, span)?;
                let vs: Vec<ir::ValueId> = xs.iter().filter_map(|x| self.operand(x)).collect();
                Some(self.value(t, ir::Expr::Construct(t, vs)))
            }
            Rvalue::ArrayRepeat(x, n) => self.array_repeat(x, *n, ty?, span),
            Rvalue::Construct(xs) => self.construct(xs, ty?, span),
            Rvalue::Convert(x) => {
                let v = self.operand(x)?;
                let t = self.ty(ty?, span)?;
                let s = self.mb.m.types.as_scalar(t)?;
                Some(self.value(t, ir::Expr::Convert(v, s)))
            }
            Rvalue::Discriminant(p) => {
                let place = self.place(p)?;
                let u = self.mb.m.types.u32();
                Some(self.load(place.with(ir::Proj::Field(0)), u))
            }
            Rvalue::Len(p) => self.len(p),
            Rvalue::Call(c) => self.call(c, ty),
            Rvalue::Closure(_) | Rvalue::FnRef(..) => None,
            Rvalue::Dispatch(d) => {
                crate::gpu::lower_dispatch(self, d, span);
                None
            }
            Rvalue::Draw(d) => {
                crate::gpu::lower_draw(self, d, span);
                None
            }
        }
    }

    fn binary(
        &mut self,
        op: BinOp,
        a: &mir::Operand,
        b: &mir::Operand,
        ty: TyId,
        span: Span,
    ) -> Option<ir::ValueId> {
        let rty = self.ty(ty, span)?;
        let at = self.concrete(a.ty);
        let av = self.operand(a)?;
        let bv = self.operand(b)?;
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
            return self.math(ir::Builtin::Pow, vec![av, bv], rty, span, "**");
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
            // Both operands are already evaluated (a pattern's tests).
            BinOp::And => ir::BinOp::And,
            BinOp::Or => ir::BinOp::Or,
            BinOp::Pow => unreachable!("handled above"),
        };
        // A scalar with a vector is splatted to the vector's size: the IR's elementwise
        // operations take operands of one type.
        let (av, bv) = (self.splat_to(av, bv), self.splat_to(bv, av));
        Some(self.value(rty, ir::Expr::Binary(iop, av, bv)))
    }

    /// `x` splatted to the type of `like`, if `x` is a scalar `f32` and `like` a vector.
    fn splat_to(&mut self, x: ir::ValueId, like: ir::ValueId) -> ir::ValueId {
        let types = &self.mb.m.types;
        match (types.get(self.f.value_ty(x)), types.get(self.f.value_ty(like))) {
            (ir::TypeDef::Scalar(ir::Scalar::F32), &ir::TypeDef::Vector(n)) => {
                let t = self.f.value_ty(like);
                self.value(t, ir::Expr::Splat(x, n))
            }
            _ => x,
        }
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

    fn construct(&mut self, xs: &[mir::Operand], ty: TyId, span: Span) -> Option<ir::ValueId> {
        let t = self.ty(ty, span)?;
        let n = match self.mb.m.types.get(t) {
            ir::TypeDef::Vector(n) => *n,
            ir::TypeDef::Matrix(_) => {
                let cols: Vec<ir::ValueId> = xs.iter().filter_map(|x| self.operand(x)).collect();
                return Some(self.value(t, ir::Expr::Construct(t, cols)));
            }
            _ => return None,
        };
        let vals: Vec<(ir::ValueId, TyId)> =
            xs.iter().filter_map(|x| self.operand(x).map(|v| (v, x.ty))).collect();
        let first_is_scalar = vals.len() == 1 && {
            let st = self.concrete(vals[0].1);
            matches!(self.types().kind(st), TyKind::Float(_))
        };
        if first_is_scalar && n > 1 {
            return Some(self.value(t, ir::Expr::Splat(vals[0].0, n)));
        }
        // Flatten vectors into components.
        let f32 = self.mb.m.types.f32();
        let mut comps = Vec::new();
        for (v, vt) in vals {
            let vt = self.concrete(vt);
            match self.types().kind(vt) {
                &TyKind::Vec(m) => {
                    for c in 0..m {
                        comps.push(self.value(f32, ir::Expr::Extract(v, c as u32)));
                    }
                }
                _ => comps.push(v),
            }
        }
        Some(self.value(t, ir::Expr::Construct(t, comps)))
    }

    fn adt(
        &mut self,
        variant: Option<u32>,
        fields: &[mir::Operand],
        ty: TyId,
        span: Span,
    ) -> Option<ir::ValueId> {
        let it = self.ty(ty, span);
        let ct = self.concrete(ty);
        let map = self.cx.field_map(self.mb, ct, variant, span);
        let mut vals = Vec::new();
        for (i, x) in fields.iter().enumerate() {
            let v = self.operand(x);
            if map.get(i).copied().flatten().is_some()
                && let Some(v) = v
            {
                vals.push(v);
            }
        }
        let it = it?;
        match variant {
            None => Some(self.value(it, ir::Expr::Construct(it, vals))),
            Some(v) => {
                let payload = self
                    .mb
                    .m
                    .types
                    .field(it, 1 + v)
                    .map(|pt| self.value(pt, ir::Expr::Construct(pt, vals)));
                Some(self.value(it, ir::Expr::Variant(it, v, payload)))
            }
        }
    }

    fn array_repeat(
        &mut self,
        x: &mir::Operand,
        n: u32,
        ty: TyId,
        span: Span,
    ) -> Option<ir::ValueId> {
        let t = self.ty(ty, span)?;
        let zero = match &x.kind {
            OperandKind::Const(Lit::Int(0) | Lit::Bool(false)) => true,
            OperandKind::Const(Lit::Float(f)) => *f == 0.0 && f.is_sign_positive(),
            _ => false,
        };
        if zero {
            return Some(self.value(t, ir::Expr::Zero(t)));
        }
        let v = self.operand(x)?;
        if n <= 64 {
            return Some(self.value(t, ir::Expr::Construct(t, vec![v; n as usize])));
        }
        // A long repeat: fill a local in a loop.
        let l = self.new_local("repeat", t);
        self.counted_loop(n, |fl, i| {
            fl.emit(ir::Stmt::Store(ir::Place::local(l).with(ir::Proj::Index(i)), v))
        });
        Some(self.load(ir::Place::local(l), t))
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
        self.push_block();
        let v = self.load(ir::Place::local(i), u);
        let one = self.u32c(1);
        let next = self.value(u, ir::Expr::Binary(ir::BinOp::Add, v, one));
        self.emit(ir::Stmt::Store(ir::Place::local(i), next));
        let continuing = self.pop_block();
        self.emit(ir::Stmt::Loop { body: body_b, continuing });
    }

    /// `xs.len()`: an array's length is its type's; a run's is in the run (CPU) or the
    /// buffer's (GPU).
    fn len(&mut self, p: &mir::Place) -> Option<ir::ValueId> {
        let u = self.mb.m.types.u32();
        let t = mir::place_ty(&self.cx.checked.program, &self.mir.locals, p);
        let t = self.concrete(t);
        if let TyKind::Array(_, n) = self.types().kind(t) {
            let n = *n;
            return Some(self.value(u, ir::Expr::Const(ir::Const::U32(n))));
        }
        if self.is_gpu() {
            let place = self.place(p)?;
            return Some(self.value(u, ir::Expr::ArrayLength(place)));
        }
        let run = self.read(p)?;
        Some(self.value(u, ir::Expr::Extract(run, 1)))
    }

    // ---- calls -----------------------------------------------------------------------------

    /// An argument's value: a place's (loaded) or an operand's.
    pub fn arg_value(&mut self, a: &mir::Arg) -> Option<ir::ValueId> {
        match a {
            mir::Arg::Borrow(p, _) | mir::Arg::Mut(p, _) => self.read(p),
            mir::Arg::Take(o) => self.operand(o),
        }
    }

    /// The callable an argument passes: a closure or function a local holds.
    pub fn callable_arg(&mut self, a: &mir::Arg) -> Option<Repr> {
        let p = a.place()?;
        match &self.locals[p.local.index()] {
            r @ Repr::Callable(..) if p.proj.is_empty() => Some(r.clone()),
            _ => None,
        }
    }

    fn call(&mut self, c: &mir::Call, ty: Option<TyId>) -> Option<ir::ValueId> {
        let span = c.span;
        match &c.callee {
            mir::Callee::Builtin(b) => {
                let args: Vec<ir::ValueId> =
                    c.args.iter().filter_map(|a| self.arg_value(a)).collect();

                let t = self.ty(ty?, span)?;
                self.builtin(*b, args, t, span)
            }
            mir::Callee::Clone => self.arg_value(c.args.first()?),
            mir::Callee::Local(l) => {
                let Repr::Callable(callable, srcs) = self.locals[l.index()].clone() else {
                    return None;
                };
                self.call_callable(&callable, &srcs, &c.args, span)
            }
            mir::Callee::Fn { func, args } => {
                let substs: Vec<TyId> = args.iter().map(|&a| self.concrete(a)).collect();
                if self.cx.checked.program.func(*func).attrs.intrinsic {
                    return crate::gpu::intrinsic(self, *func, &substs, c, ty);
                }
                self.call_fn(*func, substs, c)
            }
            mir::Callee::TraitMethod { method, self_ty, trait_args, method_args } => {
                let st = self.concrete(*self_ty);
                let ta: Vec<TyId> = trait_args.iter().map(|&a| self.concrete(a)).collect();
                let ma: Vec<TyId> = method_args.iter().map(|&a| self.concrete(a)).collect();
                let program = &self.cx.checked.program;
                let Some((func, subst)) =
                    wrela_sema::traits::resolve_trait_method(program, *method, st, &ta, &ma)
                else {
                    let shown = program.display_ty(st);
                    self.cx.err(Diagnostic::internal(format!(
                        "no implementation of a trait method for `{shown}` at lowering"
                    )));
                    return None;
                };
                let generics = program.fn_all_generics(func);
                let substs: Vec<TyId> =
                    generics.iter().map(|g| subst.get(*g).unwrap_or(program.types.error)).collect();
                let substs: Vec<TyId> = substs.into_iter().map(|t| self.cx.reveal(t)).collect();
                if self.cx.checked.program.func(func).attrs.intrinsic {
                    return crate::gpu::intrinsic(self, func, &substs, c, ty);
                }
                self.call_fn(func, substs, c)
            }
        }
    }

    /// A callee's result: its type, and whether it's a pointer to a place. The function being
    /// lowered is out of the module while it is, so a call to itself reads its own.
    fn callee_ret(&self, callee: ir::FuncId) -> (Option<ir::TypeId>, bool) {
        if callee == self.id {
            (self.f.ret, self.f.ret_ref)
        } else {
            let f = &self.mb.m.functions[callee.index()];
            (f.ret, f.ret_ref)
        }
    }

    /// A call to a monomorphized function: arguments by its parameter modes, callables and
    /// resources becoming part of the instance.
    fn call_fn(&mut self, func: FnId, substs: Vec<TyId>, c: &mir::Call) -> Option<ir::ValueId> {
        let program = &self.cx.checked.program;
        let def = program.func(func);
        let generics = program.fn_all_generics(func);
        let subst = Subst::from_pairs(&generics, &substs);
        let mut callables = Vec::new();
        let mut resources = Vec::new();
        let mut out: Vec<ir::Arg> = Vec::new();
        // `mut` arguments passed through a copy (components of a vector), written back after.
        let mut write_back: Vec<(&mir::Place, ir::Place)> = Vec::new();
        for (p, a) in def.params.iter().zip(&c.args) {
            let pt = self.cx.concrete(p.ty, &subst);
            if let TyKind::FnPtr(..) = self.types().kind(pt) {
                match self.callable_arg(a) {
                    Some(Repr::Callable(callable, srcs)) => {
                        for s in &srcs {
                            out.push(self.capture_arg(s)?);
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
                let r = a.place().and_then(|pl| self.place(pl)).and_then(|pl| {
                    match (pl.root, pl.path.is_empty()) {
                        (ir::PlaceRoot::Resource(r), true) => Some(r),
                        _ => None,
                    }
                });
                if r.is_none() {
                    self.cx.err(Diagnostic::new(
                        codes::E0702,
                        a.span(),
                        "GPU code can pass a buffer on only as the kernel received it",
                    ));
                }
                resources.push(r);
                continue;
            }
            resources.push(None);
            let Some(ty) = self.cx.lower_ty(self.mb, pt, p.span) else { continue };
            let (by_ref, _) =
                crate::ty::param_passing(self.mb.target, &self.mb.m.types, ty, p.mode);
            if by_ref {
                let place = match a {
                    mir::Arg::Mut(pl, _) => {
                        let own = self.place(pl);
                        if own.is_none()
                            && let Some(copy) = self.place_or_copy(pl)
                        {
                            write_back.push((pl, copy.clone()));
                            Some(copy)
                        } else {
                            own
                        }
                    }
                    mir::Arg::Borrow(pl, _) => self.place_or_copy(pl),
                    mir::Arg::Take(o) => {
                        let v = self.operand(o)?;
                        let vt = self.f.value_ty(v);
                        let l = self.new_local("tmp", vt);
                        self.emit(ir::Stmt::Store(ir::Place::local(l), v));
                        Some(ir::Place::local(l))
                    }
                };
                out.push(ir::Arg::Place(place?));
                continue;
            }
            let array_for_run = matches!(self.mb.m.types.get(ty), ir::TypeDef::Run(_))
                && a.place().is_some_and(|pl| {
                    let at = mir::place_ty(&self.cx.checked.program, &self.mir.locals, pl);
                    let at = self.concrete(at);
                    matches!(self.types().kind(at), TyKind::Array(..))
                });
            if array_for_run {
                // A fixed array passed for a run.
                let place = self.place_or_copy(a.place()?)?;
                out.push(ir::Arg::Value(self.value(ty, ir::Expr::Run(place))));
            } else {
                out.push(ir::Arg::Value(self.arg_value(a)?));
            }
        }
        let key = InstanceKey::Fn { func, substs, callables, resources };
        let callee = self.cx.instance(self.mb, key, Some((self.id, c.span)));
        let result = match self.callee_ret(callee) {
            (Some(t), true) => {
                let pt = self.mb.m.types.intern(ir::TypeDef::Ptr(t));
                Some(self.value(pt, ir::Expr::Call(callee, out)))
            }
            (Some(t), false) => Some(self.value(t, ir::Expr::Call(callee, out))),
            (None, _) => {
                self.emit(ir::Stmt::Eval(ir::Expr::Call(callee, out)));
                None
            }
        };
        for (original, copy) in write_back {
            let Some(ty) = self.place_ty(&copy) else { continue };
            let v = self.load(copy, ty);
            self.store(original, v);
        }
        result
    }

    pub fn capture_arg(&mut self, s: &CaptureSrc) -> Option<ir::Arg> {
        if s.by_ref {
            Some(ir::Arg::Place(s.place.clone()))
        } else {
            let ty = self.place_ty(&s.place)?;
            Some(ir::Arg::Value(self.load(s.place.clone(), ty)))
        }
    }

    /// Calls a closure or function value with `args`.
    pub fn call_callable(
        &mut self,
        callable: &Callable,
        srcs: &[CaptureSrc],
        args: &[mir::Arg],
        span: Span,
    ) -> Option<ir::ValueId> {
        let mut out: Vec<ir::Arg> =
            srcs.iter().map(|s| self.capture_arg(s)).collect::<Option<_>>()?;
        let key = match callable {
            Callable::Closure { owner, id } => {
                InstanceKey::Closure { owner: owner.clone(), id: *id }
            }
            Callable::Func { func, substs } => InstanceKey::plain(*func, substs.clone()),
        };
        for a in args {
            if let Some(v) = self.arg_value(a) {
                out.push(ir::Arg::Value(v));
            }
        }
        let callee = self.cx.instance(self.mb, key, Some((self.id, span)));
        match self.callee_ret(callee).0 {
            Some(t) => Some(self.value(t, ir::Expr::Call(callee, out))),
            None => {
                self.emit(ir::Stmt::Eval(ir::Expr::Call(callee, out)));
                None
            }
        }
    }

    // ---- built-ins -------------------------------------------------------------------------

    fn builtin(
        &mut self,
        b: BuiltinFn,
        args: Vec<ir::ValueId>,
        ty: ir::TypeId,
        span: Span,
    ) -> Option<ir::ValueId> {
        let e = match builtin_ir(b) {
            BuiltinIr::Math(ib) => {
                if matches!(ib, ir::Builtin::Dpdx | ir::Builtin::Dpdy | ir::Builtin::Fwidth) {
                    crate::gpu::check_derivative(self, span);
                }
                return self.math(ib, args, ty, span, b.name());
            }
            BuiltinIr::Select => {
                ir::Expr::Select { cond: args[2], if_true: args[1], if_false: args[0] }
            }
            BuiltinIr::Bitcast(s) => ir::Expr::Bitcast(args[0], s),
            BuiltinIr::Wrapping(op) => ir::Expr::Binary(op, args[0], args[1]),
            BuiltinIr::Len => unreachable!("`len` is `Rvalue::Len`"),
        };
        Some(self.value(ty, e))
    }

    /// A math builtin (`what` as the source wrote it). On the CPU the transcendentals become
    /// calls to std's wrela functions (§11), but only after derived interpretations are built
    /// (`rewrite_cpu_math`), so gradients and intervals see the builtins themselves.
    pub fn math(
        &mut self,
        b: ir::Builtin,
        args: Vec<ir::ValueId>,
        ty: ir::TypeId,
        span: Span,
        what: &str,
    ) -> Option<ir::ValueId> {
        if !self.is_gpu()
            && crate::cpu_math_lang(b).is_some()
            && self.mb.m.types.element_scalar(ty) == Some(ir::Scalar::F64)
        {
            self.cx.err(
                Diagnostic::new(
                    codes::E0702,
                    span,
                    format!("`{what}` of an `f64` isn't supported yet"),
                )
                .with_note("std's CPU math works in `f32`; convert with `f32(x)`"),
            );
            return None;
        }
        // An elementwise builtin's scalar arguments next to vectors are splatted.
        let args = match args.iter().find(|&&a| self.mb.m.types.is_vector(self.f.value_ty(a))) {
            Some(&like) if b.is_elementwise() => {
                args.iter().map(|&a| self.splat_to(a, like)).collect()
            }
            _ => args,
        };
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

/// What a built-in function lowers to.
pub(crate) enum BuiltinIr {
    Math(ir::Builtin),
    Select,
    Bitcast(ir::Scalar),
    Wrapping(ir::BinOp),
    Len,
}

pub(crate) fn builtin_ir(b: BuiltinFn) -> BuiltinIr {
    use BuiltinIr::Math;
    use ir::Builtin as I;
    match b {
        BuiltinFn::Sqrt => Math(I::Sqrt),
        BuiltinFn::InverseSqrt => Math(I::InverseSqrt),
        BuiltinFn::Sin => Math(I::Sin),
        BuiltinFn::Cos => Math(I::Cos),
        BuiltinFn::Tan => Math(I::Tan),
        BuiltinFn::Asin => Math(I::Asin),
        BuiltinFn::Acos => Math(I::Acos),
        BuiltinFn::Atan => Math(I::Atan),
        BuiltinFn::Atan2 => Math(I::Atan2),
        BuiltinFn::Exp => Math(I::Exp),
        BuiltinFn::Exp2 => Math(I::Exp2),
        BuiltinFn::Log => Math(I::Log),
        BuiltinFn::Log2 => Math(I::Log2),
        BuiltinFn::Pow => Math(I::Pow),
        BuiltinFn::Floor => Math(I::Floor),
        BuiltinFn::Ceil => Math(I::Ceil),
        BuiltinFn::Round => Math(I::Round),
        BuiltinFn::Trunc => Math(I::Trunc),
        BuiltinFn::Fract => Math(I::Fract),
        BuiltinFn::Saturate => Math(I::Saturate),
        BuiltinFn::Step => Math(I::Step),
        BuiltinFn::Abs => Math(I::Abs),
        BuiltinFn::Sign => Math(I::Sign),
        BuiltinFn::Min => Math(I::Min),
        BuiltinFn::Max => Math(I::Max),
        BuiltinFn::Clamp => Math(I::Clamp),
        BuiltinFn::Mix => Math(I::Mix),
        BuiltinFn::Smoothstep => Math(I::Smoothstep),
        BuiltinFn::Length => Math(I::Length),
        BuiltinFn::Distance => Math(I::Distance),
        BuiltinFn::Dot => Math(I::Dot),
        BuiltinFn::Cross => Math(I::Cross),
        BuiltinFn::Normalize => Math(I::Normalize),
        BuiltinFn::Dpdx => Math(I::Dpdx),
        BuiltinFn::Dpdy => Math(I::Dpdy),
        BuiltinFn::Fwidth => Math(I::Fwidth),
        BuiltinFn::Select => BuiltinIr::Select,
        BuiltinFn::BitcastU32 => BuiltinIr::Bitcast(ir::Scalar::U32),
        BuiltinFn::BitcastI32 => BuiltinIr::Bitcast(ir::Scalar::I32),
        BuiltinFn::BitcastF32 => BuiltinIr::Bitcast(ir::Scalar::F32),
        BuiltinFn::BitcastU64 => BuiltinIr::Bitcast(ir::Scalar::U64),
        BuiltinFn::BitcastF64 => BuiltinIr::Bitcast(ir::Scalar::F64),
        BuiltinFn::WrappingAdd => BuiltinIr::Wrapping(ir::BinOp::WrappingAdd),
        BuiltinFn::WrappingSub => BuiltinIr::Wrapping(ir::BinOp::WrappingSub),
        BuiltinFn::WrappingMul => BuiltinIr::Wrapping(ir::BinOp::WrappingMul),
        BuiltinFn::Len => BuiltinIr::Len,
    }
}
