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
use crate::{Cx, ModuleBuilder};
use std::collections::HashMap;
use std::rc::Rc;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_ir as ir;
use wrela_sema::builtins::BuiltinFn;
use wrela_sema::defs::{FnOwner, Lang, RetMode};
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
    blocks: Vec<Open>,
    /// The open block each value was defined in, by [`Open::id`].
    defined: HashMap<ir::ValueId, u32>,
    /// The id of the next block opened.
    next_block: u32,
    /// How many reachable blocks go to each block.
    preds: Vec<u32>,
    /// Each `Match` block's flag: whether an arm has matched.
    matched: HashMap<BlockId, ir::LocalId>,
    /// Each `Match` block whose last arm can fail: the block after it (no arm matched).
    unmatched: HashMap<BlockId, BlockId>,
}

/// A block being emitted.
struct Open {
    stmts: ir::Block,
    /// It ended (a `return`, `break`, `continue` or trap): what follows is dead.
    term: bool,
    id: u32,
    /// Stores of values into locals (see [`Fl::spill`]): each goes after its value's `Let`
    /// once the block is done.
    spills: Vec<(ir::ValueId, ir::Stmt)>,
}

impl Open {
    fn new(id: u32) -> Open {
        Open { stmts: Vec::new(), term: false, id, spills: Vec::new() }
    }

    /// The block's statements, each spill after its value's `Let`.
    fn finish(self) -> ir::Block {
        if self.spills.is_empty() {
            return self.stmts;
        }
        let mut after: HashMap<ir::ValueId, Vec<ir::Stmt>> = HashMap::new();
        for (v, s) in self.spills.into_iter().rev() {
            after.entry(v).or_default().push(s);
        }
        let mut out = Vec::with_capacity(self.stmts.len() + after.len());
        for s in self.stmts {
            let spills = match &s {
                ir::Stmt::Let(v, _) => after.remove(v),
                _ => None,
            };
            out.push(s);
            out.extend(spills.into_iter().flatten());
        }
        out
    }
}

pub(crate) fn lower_body(cx: &mut Cx, mb: &mut ModuleBuilder, key: &InstanceKey, id: ir::FuncId) {
    match key {
        InstanceKey::Derived { .. } => {
            crate::derive::lower(cx, mb, key, id);
            return;
        }
        InstanceKey::Glue { kind, ty } => {
            crate::glue::lower(cx, mb, *kind, *ty, id);
            return;
        }
        _ => {}
    }
    let checked = cx.checked;
    let Some(mir) = key.source_fn().and_then(|f| checked.mir.get(&f)) else { return };
    let code = match key {
        InstanceKey::Closure { id: cid, .. } => mir.closure_fn(*cid),
        _ => &mir.fns[0],
    };
    let subst = cx.instance_subst(key);
    let f = std::mem::take(&mut mb.m.functions[id.index()]);
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
        blocks: vec![Open::new(0)],
        defined: HashMap::new(),
        next_block: 1,
        preds: reachable_preds(code),
        matched: HashMap::new(),
        unmatched: HashMap::new(),
    };
    fl.unmatched = unmatched(code, &fl.preds);
    // The constants this code names: places that live as long as the program (§10).
    let closure = match key {
        InstanceKey::Closure { id, .. } => Some(*id),
        _ => None,
    };
    for (i, d) in mir.locals.iter().enumerate() {
        if let LocalKind::Const(c) = d.kind
            && d.closure == closure
        {
            fl.locals[i] = fl.table(c);
        }
    }
    match key {
        InstanceKey::Fn { .. } => fl.bind_fn_params(),
        InstanceKey::Closure { id: cid, .. } => fl.bind_closure_params(*cid),
        InstanceKey::Derived { .. } | InstanceKey::Glue { .. } => {}
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

/// For each `Match` whose last arm can fail (reachably), the block control goes to then.
fn unmatched(code: &mir::FnBody, preds: &[u32]) -> HashMap<BlockId, BlockId> {
    let mut out = HashMap::new();
    for (i, b) in code.blocks.iter().enumerate() {
        let reachable = i == mir::FnBody::ENTRY.index() || preds[i] > 0;
        if let (true, TerminatorKind::ArmFailed { match_, next }) = (reachable, &b.term.kind)
            && let TerminatorKind::Match { arms, .. } = &code.block(*match_).term.kind
            && !arms.contains(next)
        {
            out.insert(*match_, *next);
        }
    }
    out
}

impl<'c, 'a> Fl<'c, 'a> {
    // ---- emission --------------------------------------------------------------------------

    pub fn emit(&mut self, s: ir::Stmt) {
        let b = self.blocks.last_mut().expect("a block is open");
        if b.term {
            return;
        }
        let ends = matches!(
            s,
            ir::Stmt::Break | ir::Stmt::Continue | ir::Stmt::Return(_) | ir::Stmt::Trap
        );
        if let ir::Stmt::Let(v, _) = s {
            self.defined.insert(v, b.id);
        }
        b.stmts.push(s);
        if ends {
            b.term = true;
        }
    }

    pub fn terminated(&self) -> bool {
        self.blocks.last().is_some_and(|b| b.term)
    }

    /// Marks where the code that follows comes from (`ir::Stmt::At`): a marker that nothing
    /// followed yet is replaced.
    fn at(&mut self, span: Span) {
        let b = self.blocks.last_mut().expect("a block is open");
        if b.term {
            return;
        }
        match b.stmts.last_mut() {
            Some(ir::Stmt::At(s)) => *s = span,
            _ => b.stmts.push(ir::Stmt::At(span)),
        }
    }

    pub fn push_block(&mut self) {
        self.blocks.push(Open::new(self.next_block));
        self.next_block += 1;
    }

    pub fn pop_block(&mut self) -> ir::Block {
        self.blocks.pop().map(Open::finish).unwrap_or_default()
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

    /// A new IR local named `name`, holding the value of `e`.
    pub fn bind_local(&mut self, name: &str, ty: ir::TypeId, e: ir::Expr) -> ir::LocalId {
        let v = self.value(ty, e);
        self.local_of(name, v)
    }

    /// A new IR local holding `v`.
    fn local_of(&mut self, name: &str, v: ir::ValueId) -> ir::LocalId {
        let l = self.new_local(name, self.f.value_ty(v));
        self.emit(ir::Stmt::Store(ir::Place::local(l), v));
        l
    }

    /// A temporary copy of `v`, so it has a place.
    pub(crate) fn temp_place(&mut self, v: ir::ValueId) -> ir::Place {
        ir::Place::local(self.local_of("tmp", v))
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
        self.cx.concrete(t, &self.subst)
    }

    pub fn types(&self) -> &'a Types {
        &self.cx.checked.program.types
    }

    pub fn is_gpu(&self) -> bool {
        self.mb.target() == ir::Target::Gpu
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
            && self.mb.gpu.as_ref().is_some_and(|g| g.entry_index(func).is_some())
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
                        // The callable's captures, in the order `callable_params` declares them.
                        let mut srcs = Vec::new();
                        for _ in 0..self.cx.callable_params(self.mb, &c).len() {
                            let by_ref = self.f.params[k as usize].by_ref;
                            let place = if by_ref {
                                ir::Place::root(ir::PlaceRoot::Param(k))
                            } else {
                                ir::Place::local(self.param_local(k))
                            };
                            srcs.push(CaptureSrc { place, by_ref });
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
                    Repr::Place(ir::Place::root(ir::PlaceRoot::Resource(*r)));
                continue;
            }
            if self.cx.lower_ty(self.mb, t, p.span).is_none() {
                self.locals[local.index()] = Repr::Erased;
                continue;
            }
            self.bind_param(local, k);
            k += 1;
        }
    }

    fn bind_closure_params(&mut self, cid: ClosureId) {
        let info = &self.mir.closures[cid.0 as usize];
        let owner = self.owner_key();
        for &(l, _) in &info.captures {
            let decl = self.mir.local(l);
            let t = self.concrete(decl.ty);
            // A closure captures another closure as its value, copies of its captures (§6.7):
            // one that captures projections has none, and a function-typed parameter is its
            // caller's. Checked here, on concrete types: a capture of a generic type may be a
            // closure in one instance and a named function, which isn't passed, in another.
            match self.types().kind(t).clone() {
                TyKind::Closure(c, env) if self.cx.closure_fields(self.mb, c, &env).is_none() => {
                    self.cx.err(
                        Diagnostic::new(
                            codes::E0702,
                            decl.span,
                            "a closure can capture another closure only if that one captures copies, not projections",
                        )
                        .with_help("pass the inner closure to the function directly, or copy what it captures first"),
                    );
                }
                TyKind::FnPtr(..) => {
                    self.cx.err(
                        Diagnostic::new(
                            codes::E0702,
                            decl.span,
                            "a closure can't capture a function-typed parameter yet",
                        )
                        .with_help("pass the function to what the closure calls, as a parameter of its own"),
                    );
                }
                _ => {}
            }
            // Bound below if the closure takes it as a parameter; a buffer is its resource; a
            // named function is its type, so it isn't passed.
            self.locals[l.index()] =
                match (self.types().kind(t).clone(), self.cx.capture_resource(&owner, l)) {
                    (TyKind::FnDef(func, substs), _) => {
                        Repr::Callable(self.func_callable(func, substs), Vec::new())
                    }
                    (_, Some(r)) => Repr::Place(ir::Place::root(ir::PlaceRoot::Resource(r))),
                    (_, None) => Repr::Erased,
                };
        }
        // The captures it takes first, then the closure's own parameters.
        let mut k = 0u32;
        for (l, ..) in self.cx.lowered_captures(self.mb, &owner, cid) {
            self.bind_param(l, k);
            k += 1;
        }
        for &l in &info.params {
            let decl = self.mir.local(l);
            let t = self.concrete(decl.ty);
            if self.cx.lower_ty(self.mb, t, decl.span).is_none() {
                self.locals[l.index()] = Repr::Erased;
                continue;
            }
            self.bind_param(l, k);
            k += 1;
        }
    }

    /// Binds MIR local `l` to IR parameter `k`: a by-reference parameter is its place; a value
    /// is copied into a new local, which can be assigned.
    fn bind_param(&mut self, l: Local, k: u32) {
        self.locals[l.index()] = if self.f.params[k as usize].by_ref {
            Repr::Place(ir::Place::root(ir::PlaceRoot::Param(k)))
        } else {
            Repr::Var(self.param_local(k))
        };
    }

    /// A new local holding the value of IR parameter `k`.
    fn param_local(&mut self, k: u32) -> ir::LocalId {
        let p = &self.f.params[k as usize];
        let (name, ty) = (p.name.clone(), p.ty);
        self.bind_local(&name, ty, ir::Expr::Param(k))
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
                    // No arm matched (unreachable: the match is exhaustive, and a guard can't
                    // change what the arms test).
                    let none = self.unmatched.get(&b).copied();
                    for (i, arm) in arms.iter().copied().chain(none).enumerate() {
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
                TerminatorKind::ReturnPlace(p)
                    if self
                        .f
                        .ret
                        .is_some_and(|t| matches!(self.mb.m.types.get(t), ir::TypeDef::Run(_))) =>
                {
                    // A run result is returned as the run, not as a pointer to one.
                    let rt = self.f.ret.expect("checked above");
                    match self.run_of(p, rt) {
                        Some(v) => self.emit(ir::Stmt::Return(Some(v))),
                        None => self.emit(ir::Stmt::Trap),
                    }
                    return;
                }
                TerminatorKind::ReturnPlace(p) => {
                    let pt = self.f.ret.map(|t| self.mb.m.types.intern(ir::TypeDef::Ptr(t)));
                    let place = match self.place_parts(p) {
                        Some((_, _, Some(_))) => {
                            // A pointer is to one place, and the components are several.
                            self.cx.err(
                                Diagnostic::new(
                                    codes::E0702,
                                    block.term.span,
                                    "a function can't return a projection of several vector components yet",
                                )
                                .with_help("return a projection of the whole vector, and pick the components at the call site"),
                            );
                            None
                        }
                        parts => parts.map(|(place, ..)| place),
                    };
                    match (place, pt) {
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
                let repr = match self.callable_of(place) {
                    Some(r) => r,
                    None => match self.place_parts(place) {
                        Some((p, _, None)) => Repr::Place(p),
                        Some((p, _, Some(cs))) => Repr::Swizzle(p, cs),
                        None => Repr::Erased,
                    },
                };
                self.locals[local.index()] = repr;
            }
            // On the CPU an index is checked where its address is made: a read makes it. (GPU
            // code doesn't trap; its indexing is robust.)
            StatementKind::Check(p) => {
                if !self.is_gpu() {
                    let _ = self.read(p);
                }
            }
            StatementKind::Live(_) | StatementKind::Dead(_) => {}
            StatementKind::Drop(p) => self.drop_place(p),
        }
    }

    fn assign(&mut self, place: &mir::Place, r: &Rvalue, span: Span) {
        let l = place.local;
        if let (true, Rvalue::Const(c)) = (place.proj.is_empty(), r) {
            self.locals[l.index()] = self.table(*c);
            return;
        }
        if place.proj.is_empty() {
            // A callable: no IR value; the local stands for it.
            if let Some(c) = self.callable_rvalue(r) {
                if let (Repr::Callable(old, _), Repr::Callable(new, _)) =
                    (&self.locals[l.index()], &c)
                    && old != new
                {
                    // Branches chose different functions: each is its own instance here.
                    self.cx.err(
                        Diagnostic::new(
                            codes::E0702,
                            span,
                            "choosing between closures or functions at run time isn't supported yet",
                        )
                        .with_help("call each one in its own branch"),
                    );
                }
                self.locals[l.index()] = c;
                return;
            }
            let kind = self.mir.local(l).kind;
            let through_alias =
                matches!(self.locals[l.index()], Repr::Place(_) | Repr::Swizzle(..))
                    && !matches!(r, Rvalue::Call(c) if c.ret_mode != RetMode::Owned);
            if !through_alias {
                let ty = self.local_ty(l);
                let v = self.rvalue(r, Some(ty), span);
                match kind {
                    LocalKind::Temp => {
                        self.locals[l.index()] = v.map_or(Repr::Erased, Repr::Value);
                    }
                    LocalKind::TempProjection { .. } => {
                        // What a projection-returning call returned: a pointer to its place,
                        // or a run (a view already).
                        self.locals[l.index()] = match v {
                            Some(v) if self.is_run(v) => Repr::Value(v),
                            Some(p) => Repr::Place(ir::Place::root(ir::PlaceRoot::Ptr(p))),
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

    /// A table's value (`mir::Rvalue::Const`): on the CPU its constant data, read in place; GPU
    /// code builds it.
    fn table(&mut self, c: ConstId) -> Repr {
        // A lifted build's constant of a lifted package is built from its literals (§22).
        if let Some(v) = self.lifted_const(c) {
            return Repr::Value(v);
        }
        if !self.is_gpu() {
            return match self.cx.const_data(self.mb, c) {
                Some(d) => Repr::Place(ir::Place::root(ir::PlaceRoot::Data(d))),
                None => Repr::Erased,
            };
        }
        let built = self.cx.const_value(self.mb, c).and_then(|(t, v)| self.build_const(t, &v));
        built.map_or(Repr::Erased, Repr::Value)
    }

    /// What a named function used as a value calls: the function, or for a trait's (`W::work`,
    /// §7), the implementation for the `Self` it now has.
    fn func_callable(&mut self, func: FnId, substs: Vec<TyId>) -> Callable {
        let program = &self.cx.checked.program;
        let FnOwner::Trait(t) = program.func(func).owner else {
            return Callable::Func { func, substs };
        };
        let n = 1 + program.trait_(t).generics.len();
        let substs: Vec<TyId> = substs.iter().map(|&a| self.concrete(a)).collect();
        let program = &self.cx.checked.program;
        match wrela_sema::traits::resolve_trait_method(
            program,
            func,
            substs[0],
            &substs[1..n],
            &substs[n..],
        ) {
            Some((f, subst)) => {
                let substs = program
                    .fn_all_generics(f)
                    .iter()
                    .map(|g| self.cx.checked.reveal(subst.get(*g).unwrap_or(program.types.error)))
                    .collect();
                Callable::Func { func: f, substs }
            }
            None => {
                let shown = program.display_ty(substs[0]);
                self.cx.err(Diagnostic::internal(format!(
                    "no implementation of a trait's function for `{shown}` at lowering"
                )));
                Callable::Func { func, substs }
            }
        }
    }

    /// Whether `v` is a run.
    pub fn is_run(&self, v: ir::ValueId) -> bool {
        matches!(self.mb.m.types.get(self.f.value_ty(v)), ir::TypeDef::Run(_))
    }

    /// The closure or function a local holds as a value (one stored and read back): its
    /// callable, from its concrete type, and its captures, the value's fields.
    fn stored_callable(&mut self, l: Local) -> Option<(Callable, Vec<CaptureSrc>)> {
        let t = self.concrete(self.local_ty(l));
        match self.types().kind(t).clone() {
            TyKind::FnDef(func, substs) => Some((self.func_callable(func, substs), Vec::new())),
            TyKind::Closure(c, env) => {
                let owner = self.cx.stored_closure_owner(c, &env)?;
                let fields = self.cx.closure_fields(self.mb, c, &env)?;
                let mut srcs = Vec::new();
                if !fields.is_empty() {
                    let base = self.place(&mir::Place::local(l))?;
                    for (k, (_, ty)) in fields.into_iter().enumerate() {
                        let (by_ref, _) = crate::ty::capture_passing(
                            self.mb.target(),
                            &self.mb.m.types,
                            ty,
                            false,
                        );
                        srcs.push(CaptureSrc {
                            place: base.with(ir::Proj::Field(k as u32)),
                            by_ref,
                        });
                    }
                }
                Some((Callable::Closure { owner: Rc::new(owner), id: c.id }, srcs))
            }
            _ => None,
        }
    }

    /// A closure's value, built from where its captures are: copies of them (§6.7).
    fn closure_value(&mut self, l: Local, srcs: &[CaptureSrc]) -> Option<ir::ValueId> {
        let t = self.local_ty(l);
        let it = self.ty(t, self.code.span)?;
        let mut vals = Vec::new();
        for s in srcs {
            let ty = self.place_ty(&s.place)?;
            vals.push(self.load(s.place.clone(), ty));
        }
        Some(self.value(it, ir::Expr::Construct(it, vals)))
    }

    /// The callable an rvalue makes: a closure, a named function, or a local's, copied or moved
    /// (`take f`).
    fn callable_rvalue(&mut self, r: &Rvalue) -> Option<Repr> {
        match r {
            Rvalue::Closure(id) => {
                let owner = self.owner_key();
                let srcs = self.capture_sources(&owner, *id);
                Some(Repr::Callable(Callable::Closure { owner: Rc::new(owner), id: *id }, srcs))
            }
            Rvalue::FnRef(f, args) => {
                let substs = args.iter().map(|&a| self.concrete(a)).collect();
                Some(Repr::Callable(self.func_callable(*f, substs), Vec::new()))
            }
            Rvalue::Use(mir::Operand {
                kind: OperandKind::Copy(p) | OperandKind::Move(p, _),
                ..
            }) => self.callable_of(p),
            _ => None,
        }
    }

    /// The callable a place holds: a closure or function in a local.
    fn callable_of(&self, p: &mir::Place) -> Option<Repr> {
        match &self.locals[p.local.index()] {
            r @ Repr::Callable(..) if p.proj.is_empty() => Some(r.clone()),
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

    /// Where the captures of a new closure, `id` of `owner`, are in this function.
    fn capture_sources(&mut self, owner: &InstanceKey, id: ClosureId) -> Vec<CaptureSrc> {
        let mut out = Vec::new();
        for (l, ty, written) in self.cx.lowered_captures(self.mb, owner, id) {
            let (by_ref, _) =
                crate::ty::capture_passing(self.mb.target(), &self.mb.m.types, ty, written);
            // A closure in a local here is a callable with no value yet: its value is copies
            // of its captures.
            if let Repr::Callable(_, srcs) = self.locals[l.index()].clone() {
                let Some(v) = self.closure_value(l, &srcs) else { continue };
                let copy = self.local_of("capture", v);
                out.push(CaptureSrc { place: ir::Place::local(copy), by_ref });
                continue;
            }
            // A `Copy` value the closure only reads is copied when it's made (§6.7).
            let decl_ty = self.concrete(self.mir.local(l).ty);
            if !written
                && wrela_sema::traits::implements_builtin(
                    &self.cx.checked.program,
                    decl_ty,
                    Lang::Copy,
                )
                && let Some(v) = self.read(&mir::Place::local(l))
            {
                let copy = self.local_of("capture", v);
                out.push(CaptureSrc { place: ir::Place::local(copy), by_ref });
                continue;
            }
            let place = match self.locals[l.index()].clone() {
                Repr::Var(v) => ir::Place::local(v),
                Repr::Place(p) => p,
                Repr::Value(v) => self.spill(l, v),
                Repr::Swizzle(..) if written => {
                    self.cx.err(
                        Diagnostic::new(
                            codes::E0702,
                            self.mir.local(l).span,
                            "a closure can't change a projection of several vector components yet",
                        )
                        .with_help("change the components through the vector itself"),
                    );
                    continue;
                }
                // Components alias no single place; the closure only reads them.
                Repr::Swizzle(..) => {
                    let Some(p) = self.place_or_copy(&mir::Place::local(l)) else { continue };
                    p
                }
                _ => continue,
            };
            out.push(CaptureSrc { place, by_ref });
        }
        out
    }

    // ---- places ----------------------------------------------------------------------------

    /// Moves a temporary's value into an IR local, so it has a place. The local is stored
    /// where the value is defined, which encloses every use of the temporary: the first use
    /// that needs a place may be in a branch (a `match` arm), and later uses outside it.
    fn spill(&mut self, l: Local, v: ir::ValueId) -> ir::Place {
        let var = self.new_local("tmp", self.f.value_ty(v));
        let store = ir::Stmt::Store(ir::Place::local(var), v);
        let open = self.defined.get(&v).and_then(|id| self.blocks.iter_mut().find(|b| b.id == *id));
        match open {
            Some(b) => b.spills.push((v, store)),
            None => self.emit(store),
        }
        self.locals[l.index()] = Repr::Var(var);
        ir::Place::local(var)
    }

    /// The IR place of a MIR place, its type, and a trailing swizzle (which no IR place can
    /// name). `None` when it has no runtime value.
    fn place_parts(&mut self, p: &mir::Place) -> Option<(ir::Place, TyId, Option<Vec<u8>>)> {
        let mut ty = self.concrete(self.local_ty(p.local));
        let mut swizzle = None;
        let ir::Place { mut root, mut path } = match self.locals[p.local.index()].clone() {
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
                        path.push(ir::Proj::Comp(cs[*c as usize]));
                        swizzle = None;
                        ty = self.types().f32;
                    }
                    mir::Proj::Swizzle(more) => {
                        let picked: Vec<u8> = more.iter().map(|&c| cs[c as usize]).collect();
                        ty = self.types().vec(picked.len() as u8);
                        swizzle = Some(picked);
                    }
                    mir::Proj::Index(t) => {
                        let iv = self.local_value(*t)?;
                        path.push(ir::Proj::Index(self.swizzle_index(cs, iv)));
                        swizzle = None;
                        ty = self.types().f32;
                    }
                    _ => return None,
                }
                continue;
            }
            match proj {
                mir::Proj::Downcast(v) => {
                    let payload = self.cx.payload_field(self.mb, ty, *v, self.code.span)?;
                    path.push(ir::Proj::Field(payload));
                    variant = Some(*v);
                }
                mir::Proj::Field(i) => {
                    let projected = self.borrow_projection(ty, *i);
                    let (fi, ft) = self.cx.field(self.mb, ty, variant, *i, self.code.span)?;
                    ty = ft;
                    variant = None;
                    path.push(ir::Proj::Field(fi));
                    if projected {
                        // A borrow struct's projection field: the place it points at.
                        let it = self.cx.lower_ty(self.mb, ft, self.code.span)?;
                        let pt = self.mb.m.types.intern(ir::TypeDef::Ptr(it));
                        let here =
                            ir::Place { root: root.clone(), path: std::mem::take(&mut path) };
                        let ptr = self.load(here, pt);
                        root = ir::PlaceRoot::Ptr(ptr);
                    }
                }
                mir::Proj::Comp(c) => {
                    path.push(ir::Proj::Comp(*c));
                    ty = self.types().f32;
                }
                mir::Proj::Swizzle(cs) => {
                    swizzle = Some(cs.clone());
                    ty = self.types().vec(cs.len() as u8);
                }
                mir::Proj::Index(t) => {
                    let iv = self.local_value(*t)?;
                    let k = self.types().kind(ty);
                    if let TyKind::Adt(_, args) = k
                        && self.cx.checked.program.lang_of_ty(ty) == Some(Lang::Slots)
                    {
                        let idx = crate::gpu::slot_index(self, iv)?;
                        path.push(ir::Proj::Index(idx));
                        ty = args[0];
                        continue;
                    }
                    if let TyKind::Adt(_, args) = k
                        && self.cx.checked.program.lang_of_ty(ty) == Some(Lang::Arena)
                    {
                        // A value in an arena: by a handle (checked), or by position (a loop).
                        let elem = args[0];
                        let arena =
                            ir::Place { root: root.clone(), path: std::mem::take(&mut path) };
                        let index_ty = self.concrete(self.mir.local(*t).ty);
                        let at = self.arena_elem(arena, ty, elem, iv, index_ty)?;
                        root = at.root;
                        path = at.path;
                        ty = elem;
                        continue;
                    }
                    if let TyKind::Adt(_, args) = k
                        && self.cx.checked.program.lang_of_ty(ty) == Some(Lang::Bounded)
                    {
                        // An element in use of the array it holds in place.
                        let elem = args[0];
                        let b = ir::Place { root: root.clone(), path: std::mem::take(&mut path) };
                        let iv = self.index_u32(iv);
                        let at = self.bounded_elem(b, ty, iv)?;
                        root = at.root;
                        path = at.path;
                        ty = elem;
                        continue;
                    }
                    if let TyKind::Adt(_, args) = k
                        && self.cx.checked.program.lang_of_ty(ty) == Some(Lang::Vec)
                    {
                        // An element on the heap: a place at its address.
                        let elem = args[0];
                        let vec = ir::Place { root: root.clone(), path: std::mem::take(&mut path) };
                        let iv = self.index_u32(iv);
                        let at = self.vec_elem(vec, ty, elem, iv)?;
                        root = at.root;
                        path = at.path;
                        ty = elem;
                        continue;
                    }
                    path.push(ir::Proj::Index(iv));
                    ty = match k {
                        TyKind::Array(e, _) | TyKind::Slice(e) => *e,
                        TyKind::Vec(_) => self.types().f32,
                        TyKind::Mat(n) => self.types().vec(*n),
                        _ => self.types().error,
                    };
                }
            }
        }
        Some((ir::Place { root, path }, ty, swizzle))
    }

    /// Whether field `i` of type `t` is a borrow struct's projection field: a pointer.
    fn borrow_projection(&self, t: TyId, i: u32) -> bool {
        let p = &self.cx.checked.program;
        match p.types.kind(t) {
            TyKind::Adt(a, _) if p.adt(*a).borrow => {
                p.adt_fields(*a, None).get(i as usize).is_some_and(|f| f.mode != RetMode::Owned)
            }
            _ => false,
        }
    }

    /// A borrow struct's value: a pointer to each projection field's place, and each other
    /// field's value (a run's, or another borrow struct's).
    fn borrow_struct(&mut self, fields: &[mir::Arg], ty: TyId, span: Span) -> Option<ir::ValueId> {
        let t = self.ty(ty, span)?;
        let ct = self.concrete(ty);
        let map = self.cx.field_map(self.mb, ct, None, span);
        let mut vals = Vec::new();
        for (k, a) in fields.iter().enumerate() {
            let Some(&(Some(_), _)) = map.get(k) else { continue };
            let projected = self.borrow_projection(ct, k as u32);
            let v = match a {
                mir::Arg::Borrow(p, _) | mir::Arg::Mut(p, _) if projected => {
                    let place = self.place_or_copy(p)?;
                    let ft = self.place_ty(&place)?;
                    let pt = self.mb.m.types.intern(ir::TypeDef::Ptr(ft));
                    self.value(pt, ir::Expr::Addr(place))
                }
                mir::Arg::Borrow(p, _) | mir::Arg::Mut(p, _) => {
                    // A run field, or a borrow struct's value.
                    let ft = self.mb.m.types.field(t, vals.len() as u32)?;
                    if matches!(self.mb.m.types.get(ft), ir::TypeDef::Run(_)) {
                        self.run_of(p, ft)?
                    } else {
                        self.read(p)?
                    }
                }
                mir::Arg::Take(o) => self.operand(o)?,
            };
            vals.push(v);
        }
        Some(self.value(t, ir::Expr::Construct(t, vals)))
    }

    /// An index as a `u32` (an `i32` index converts: a negative one is past any end).
    fn index_u32(&mut self, i: ir::ValueId) -> ir::ValueId {
        let u = self.mb.m.types.u32();
        if self.f.value_ty(i) == u {
            return i;
        }
        self.value(u, ir::Expr::Bitcast(i, ir::Scalar::U32))
    }

    /// The component of a vector that element `i` of its swizzle `cs` is: `cs[i]`, read from a
    /// table, so an index past the swizzle is out of range as any other is.
    fn swizzle_index(&mut self, cs: &[u8], i: ir::ValueId) -> ir::ValueId {
        let u = self.mb.m.types.u32();
        let t = self.mb.m.types.intern(ir::TypeDef::Array(u, cs.len() as u32));
        let comps = cs.iter().map(|&c| self.u32c(u32::from(c))).collect();
        let table = self.bind_local("swizzle", t, ir::Expr::Construct(t, comps));
        self.load(ir::Place::local(table).with(ir::Proj::Index(i)), u)
    }

    /// The IR place of a MIR place without a trailing swizzle.
    pub fn place(&mut self, p: &mir::Place) -> Option<ir::Place> {
        let (place, _, swizzle) = self.place_parts(p)?;
        swizzle.is_none().then_some(place)
    }

    /// A place holding a MIR place's value, for passing by reference: the place itself, or a
    /// temporary copy of components (which no single place holds; they're read-only there).
    pub fn place_or_copy(&mut self, p: &mir::Place) -> Option<ir::Place> {
        if let Some(place) = self.place(p) {
            return Some(place);
        }
        let v = self.read(p)?;
        Some(self.temp_place(v))
    }

    /// A MIR place's type, concrete.
    pub fn place_src_ty(&mut self, p: &mir::Place) -> TyId {
        let t = mir::place_ty(&self.cx.checked.program, &self.mir.locals, p);
        self.concrete(t)
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
        // A closure read as a value (stored, passed on, returned): copies of its captures.
        if let Repr::Callable(_, srcs) = self.locals[p.local.index()].clone()
            && p.proj.is_empty()
        {
            return self.closure_value(p.local, &srcs);
        }
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
                    let (fi, ft) = self.cx.field(self.mb, ty, None, *i, self.code.span)?;
                    ty = ft;
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
    pub fn store_mir(&mut self, p: &mir::Place, v: ir::ValueId) {
        self.store(p, v)
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
                if let Some(i) = self.lifted_index(o) {
                    return self.lifted(i);
                }
                let ty = self.ty(o.ty, o.span)?;
                let c = self.lit(*l, ty)?;
                Some(self.value(ty, ir::Expr::Const(c)))
            }
            OperandKind::Copy(p) => self.read(p),
            OperandKind::Move(p, kind) => {
                let v = self.read(p);
                self.moved(p, *kind);
                v
            }
        }
    }

    /// The index of the lifted literal an operand is, in a lifted build (§22): an `f32`
    /// literal written in a lifted package.
    fn lifted_index(&self, o: &mir::Operand) -> Option<u32> {
        let lift = self.cx.lift?;
        let OperandKind::Const(_) = o.kind else { return None };
        let f32_ty = self.cx.checked.program.types.f32;
        if o.ty != f32_ty {
            return None;
        }
        lift.of(o.span)
    }

    fn lit(&mut self, l: Lit, ty: ir::TypeId) -> Option<ir::Const> {
        lit_const(&self.mb.m.types, l, ty)
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
            Rvalue::ArrayRepeat(x, _) => {
                // Its length is its type's: a `const` parameter's is known only here.
                let t = self.concrete(ty?);
                let TyKind::Array(_, n) = *self.types().kind(t) else { return None };
                self.array_repeat(x, n, t, span)
            }
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
            Rvalue::Const(c) => match self.table(*c) {
                Repr::Value(v) => Some(v),
                Repr::Place(p) => {
                    let t = self.ty(ty?, span)?;
                    Some(self.load(p, t))
                }
                _ => None,
            },
            Rvalue::Text(t) => self.text(t, ty?, span),
            Rvalue::Embed(path) => self.embedded(path, ty?, span),
            Rvalue::BorrowStruct { fields, .. } => self.borrow_struct(fields, ty?, span),
            Rvalue::ConstParam(g) => {
                let t = self.concrete(self.types().param(*g));
                match *self.types().kind(t) {
                    TyKind::ConstU32(n) => Some(self.u32c(n)),
                    _ => {
                        self.cx.err(Diagnostic::internal(
                            "a `const` parameter with no value at lowering",
                        ));
                        None
                    }
                }
            }
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
                return self.int_pow(av, bv, rty);
            }
            return self.math(ir::Builtin::Pow, vec![av, bv], rty);
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

    /// `base ** exp` for integers, by squaring: a multiplication for each bit of `exp`, and a
    /// squaring for each but the highest (checked on the CPU). Every product is a power of
    /// `base` no higher than the result, so one overflows only when the result does.
    fn int_pow(
        &mut self,
        base: ir::ValueId,
        exp: ir::ValueId,
        ty: ir::TypeId,
    ) -> Option<ir::ValueId> {
        let u = self.mb.m.types.u32();
        let bool_ty = self.mb.m.types.bool();
        let one = self.lit(Lit::Int(1), ty)?;
        let acc = self.bind_local("pow", ty, ir::Expr::Const(one));
        let b = self.new_local("pow_base", ty);
        self.emit(ir::Stmt::Store(ir::Place::local(b), base));
        let e = self.new_local("pow_exp", u);
        self.emit(ir::Stmt::Store(ir::Place::local(e), exp));
        let (zero, bit) = (self.u32c(0), self.u32c(1));
        self.push_block();
        // No bits left: done.
        let ev = self.load(ir::Place::local(e), u);
        let none = self.value(bool_ty, ir::Expr::Binary(ir::BinOp::Eq, ev, zero));
        self.emit(ir::Stmt::If { cond: none, then: vec![ir::Stmt::Break], else_: Vec::new() });
        // The lowest bit set: acc *= b.
        let low = self.value(u, ir::Expr::Binary(ir::BinOp::BitAnd, ev, bit));
        let set = self.value(bool_ty, ir::Expr::Binary(ir::BinOp::Ne, low, zero));
        self.push_block();
        let av = self.load(ir::Place::local(acc), ty);
        let bv = self.load(ir::Place::local(b), ty);
        let prod = self.value(ty, ir::Expr::Binary(ir::BinOp::Mul, av, bv));
        self.emit(ir::Stmt::Store(ir::Place::local(acc), prod));
        let then = self.pop_block();
        self.emit(ir::Stmt::If { cond: set, then, else_: Vec::new() });
        // The next bit; b *= b only if there is one.
        let rest = self.value(u, ir::Expr::Binary(ir::BinOp::Shr, ev, bit));
        self.emit(ir::Stmt::Store(ir::Place::local(e), rest));
        let last = self.value(bool_ty, ir::Expr::Binary(ir::BinOp::Eq, rest, zero));
        self.emit(ir::Stmt::If { cond: last, then: vec![ir::Stmt::Break], else_: Vec::new() });
        let bv = self.load(ir::Place::local(b), ty);
        let square = self.value(ty, ir::Expr::Binary(ir::BinOp::Mul, bv, bv));
        self.emit(ir::Stmt::Store(ir::Place::local(b), square));
        let body = self.pop_block();
        self.emit(ir::Stmt::Loop { body, continuing: Vec::new() });
        Some(self.load(ir::Place::local(acc), ty))
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
            if map.get(i).is_some_and(|f| f.0.is_some())
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
        let zero = self.lifted_index(x).is_none()
            && match &x.kind {
                OperandKind::Const(Lit::Int(0) | Lit::Bool(false)) => true,
                OperandKind::Const(Lit::Float(f, _)) => *f == 0.0 && f.is_sign_positive(),
                _ => false,
            };
        if zero {
            return Some(self.value(t, ir::Expr::Zero(t)));
        }
        let v = self.operand(x)?;
        if n <= 64 {
            return Some(self.value(t, ir::Expr::Construct(t, vec![v; n as usize])));
        }
        // A long repeat: fill a local in a loop, zeroed first, so its padding (a `vec3`'s
        // fourth float) is zeros, not what the stack held (§11: a value's bytes are a function
        // of its fields).
        let l = self.new_local("repeat", t);
        let z = self.value(t, ir::Expr::Zero(t));
        self.emit(ir::Stmt::Store(ir::Place::local(l), z));
        let n = self.u32c(n);
        self.counted_loop(n, |fl, i| {
            fl.emit(ir::Stmt::Store(ir::Place::local(l).with(ir::Proj::Index(i)), v))
        });
        Some(self.load(ir::Place::local(l), t))
    }

    /// `for i in 0..n` (`n` a `u32`), for internal loops.
    pub fn counted_loop(&mut self, n: ir::ValueId, mut body: impl FnMut(&mut Self, ir::ValueId)) {
        let u = self.mb.m.types.u32();
        let bool_ty = self.mb.m.types.bool();
        let i = self.new_local("i", u);
        let zero = self.u32c(0);
        self.emit(ir::Stmt::Store(ir::Place::local(i), zero));
        self.push_block();
        let iv = self.load(ir::Place::local(i), u);
        let lt = self.value(bool_ty, ir::Expr::Binary(ir::BinOp::Lt, iv, n));
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
        let t = self.place_src_ty(p);
        if let TyKind::Array(_, n) = self.types().kind(t) {
            let n = *n;
            return Some(self.value(u, ir::Expr::Const(ir::Const::U32(n))));
        }
        if let Some(n) = self.container_len(p) {
            return n;
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
    pub fn callable_arg(&self, a: &mir::Arg) -> Option<Repr> {
        self.callable_of(a.place()?)
    }

    /// A place passed for a `[T]` parameter (`run_t`): a run of it if it's a fixed array, else
    /// its value (a run already).
    pub fn run_arg(&mut self, p: &mir::Place, run_t: ir::TypeId) -> Option<ir::ValueId> {
        self.run_of(p, run_t)
    }

    fn call(&mut self, c: &mir::Call, ty: Option<TyId>) -> Option<ir::ValueId> {
        let span = c.span;
        match &c.callee {
            mir::Callee::Builtin(BuiltinFn::Panic) => {
                self.panic_call(c.args.first()?);
                None
            }
            mir::Callee::Builtin(BuiltinFn::Assert) => {
                self.assert_call(c.args.first()?, c.args.get(1)?, &c.args[2..], span);
                None
            }
            mir::Callee::Builtin(b) => {
                let args: Option<Vec<ir::ValueId>> =
                    c.args.iter().map(|a| self.arg_value(a)).collect();
                let args = args?;
                let t = self.ty(ty?, span)?;
                self.builtin(*b, args, t, span)
            }
            mir::Callee::Clone => self.clone_arg(c.args.first()?),
            mir::Callee::Value(_) => {
                self.cx.err(Diagnostic::internal("a call of a stored value reached lowering"));
                None
            }
            mir::Callee::Local(l) => {
                if !matches!(self.locals[l.index()], Repr::Callable(..))
                    && let Some((callable, srcs)) = self.stored_callable(*l)
                {
                    return self.call_callable(&callable, &srcs, &c.args, span);
                }
                let Repr::Callable(callable, srcs) = self.locals[l.index()].clone() else {
                    // Its callable was given up on with an error (E0702); without one,
                    // dropping the call would be a silent miscompile.
                    if !wrela_diag::has_errors(&self.cx.diags) {
                        let decl = self.mir.local(*l);
                        let t = self.concrete(decl.ty);
                        self.cx.err(Diagnostic::internal(format!(
                            "a call through a local that holds no known closure or function: `{}`, of type `{}`, as {:?}",
                            decl.name,
                            self.cx.checked.program.display_ty(t),
                            self.locals[l.index()]
                        )));
                    }
                    return None;
                };
                self.call_callable(&callable, &srcs, &c.args, span)
            }
            mir::Callee::Fn { func, args } => {
                let substs: Vec<TyId> = args.iter().map(|&a| self.concrete(a)).collect();
                if self.cx.checked.program.func(*func).attrs.intrinsic {
                    if let Some(l) = self.cx.checked.program.func(*func).lang
                        && let Some(v) = self.mem_intrinsic(l, &substs, c, ty)
                    {
                        return v;
                    }
                    if let Some(l @ (Lang::ParEach | Lang::ParMapReduce)) =
                        self.cx.checked.program.func(*func).lang
                    {
                        return crate::par::par_job(self, l, &substs, c);
                    }
                    return crate::gpu::intrinsic(self, *func, &substs, c, ty);
                }
                self.call_fn(*func, substs, &c.args, span)
            }
            mir::Callee::TraitMethod { method, self_ty, trait_args, method_args } => {
                let st = self.concrete(*self_ty);
                // `Clone`'s methods are the compiler's glue, which calls std's hand-written
                // leaves where there are any.
                let program = &self.cx.checked.program;
                if let FnOwner::Trait(tr) = program.func(*method).owner
                    && program.is_lang_trait(tr, Lang::Clone)
                {
                    return match program.func(*method).name.as_str() {
                        "clone" => self.clone_arg(c.args.first()?),
                        _ => {
                            self.clone_into_args(c.args.first()?, c.args.get(1)?);
                            None
                        }
                    };
                }
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
                let substs: Vec<TyId> = program
                    .fn_all_generics(func)
                    .iter()
                    .map(|g| self.cx.checked.reveal(subst.get(*g).unwrap_or(program.types.error)))
                    .collect();
                if program.func(func).attrs.intrinsic {
                    return crate::gpu::intrinsic(self, func, &substs, c, ty);
                }
                self.call_fn(func, substs, &c.args, span)
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
    pub(crate) fn call_fn(
        &mut self,
        func: FnId,
        substs: Vec<TyId>,
        args: &[mir::Arg],
        span: Span,
    ) -> Option<ir::ValueId> {
        let program = &self.cx.checked.program;
        let def = program.func(func);
        let generics = program.fn_all_generics(func);
        let subst = Subst::from_pairs(&generics, &substs);
        let mut callables = Vec::new();
        let mut resources = Vec::new();
        let mut out: Vec<ir::Arg> = Vec::new();
        // `mut` arguments passed through a copy (components of a vector), written back after.
        let mut write_back: Vec<(&mir::Place, ir::Place)> = Vec::new();
        for (p, a) in def.params.iter().zip(args) {
            let pt = self.cx.concrete(p.ty, &subst);
            if let TyKind::FnPtr(..) = self.types().kind(pt) {
                match self.callable_arg(a) {
                    Some(Repr::Callable(callable, srcs)) => {
                        out.extend(self.capture_args(&srcs)?);
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
            let (by_ref, _) = crate::ty::param_passing(
                self.mb.target(),
                &self.mb.m.types,
                ty,
                p.mode,
                def.ret_mode,
            );
            let run = matches!(self.mb.m.types.get(ty), ir::TypeDef::Run(_));
            if by_ref {
                let place = match a {
                    // An array passed for a run: a run of it, in a place of its own.
                    mir::Arg::Mut(pl, _) | mir::Arg::Borrow(pl, _)
                        if run
                            && matches!(
                                self.types().kind(self.place_src_ty(pl)),
                                TyKind::Array(..) | TyKind::Adt(..)
                            ) =>
                    {
                        let v = self.run_arg(pl, ty)?;
                        Some(self.temp_place(v))
                    }
                    mir::Arg::Mut(pl, _) => {
                        let own = self.place(pl);
                        if own.is_none()
                            && let Some(copy) = self.place_or_copy(pl)
                        {
                            if def.ret_mode == RetMode::Mut {
                                // The result may point into the copy, and writes through it
                                // would never reach the components.
                                self.cx.err(
                                    Diagnostic::new(
                                        codes::E0702,
                                        a.span(),
                                        format!("`{}` returns a `mut` projection, so it can't take several vector components as a `mut` argument yet", def.name),
                                    )
                                    .with_help("pass the whole vector, and pick the components in the function"),
                                );
                            }
                            write_back.push((pl, copy.clone()));
                            Some(copy)
                        } else {
                            own
                        }
                    }
                    mir::Arg::Borrow(pl, _) => self.place_or_copy(pl),
                    mir::Arg::Take(o) => {
                        let v = self.operand(o)?;
                        Some(self.temp_place(v))
                    }
                };
                out.push(ir::Arg::Place(place?));
                continue;
            }
            let v = match a.place() {
                Some(pl) if run => self.run_arg(pl, ty)?,
                _ => self.arg_value(a)?,
            };
            out.push(ir::Arg::Value(v));
        }
        // With no callables or resources, the instance is the function's plain one (shared with
        // its export, and with calls through a `fn` value).
        if callables.iter().all(Option::is_none) && resources.iter().all(Option::is_none) {
            callables.clear();
            resources.clear();
        }
        let key = InstanceKey::Fn { func, substs, callables, resources };
        let callee = self.cx.instance(self.mb, key, Some((self.id, span)));
        let result = self.emit_call(callee, out);
        for (original, copy) in write_back {
            let Some(ty) = self.place_ty(&copy) else { continue };
            let v = self.load(copy, ty);
            self.store(original, v);
        }
        result
    }

    /// Calls `callee`: its result (a pointer to a place, if it returns a projection), or `None`
    /// if it returns nothing.
    pub(crate) fn emit_call(
        &mut self,
        callee: ir::FuncId,
        args: Vec<ir::Arg>,
    ) -> Option<ir::ValueId> {
        let call = ir::Expr::Call(callee, args);
        match self.callee_ret(callee) {
            (Some(t), true) => {
                let pt = self.mb.m.types.intern(ir::TypeDef::Ptr(t));
                Some(self.value(pt, call))
            }
            (Some(t), false) => Some(self.value(t, call)),
            (None, _) => {
                self.emit(ir::Stmt::Eval(call));
                None
            }
        }
    }

    /// The arguments passing a callable's captures: the place by reference, else its value.
    pub fn capture_args(&mut self, srcs: &[CaptureSrc]) -> Option<Vec<ir::Arg>> {
        let mut out = Vec::new();
        for s in srcs {
            out.push(if s.by_ref {
                ir::Arg::Place(s.place.clone())
            } else {
                let ty = self.place_ty(&s.place)?;
                ir::Arg::Value(self.load(s.place.clone(), ty))
            });
        }
        Some(out)
    }

    /// Calls a closure or function value with `args`.
    pub fn call_callable(
        &mut self,
        callable: &Callable,
        srcs: &[CaptureSrc],
        args: &[mir::Arg],
        span: Span,
    ) -> Option<ir::ValueId> {
        if let Callable::Func { func, substs } = callable {
            // Its parameters are passed as a direct call passes them (borrowed aggregates by
            // reference on the CPU).
            return self.call_fn(*func, substs.clone(), args, span);
        }
        // A closure takes its `mut` parameters as places, the others by value.
        let mut out = self.capture_args(srcs)?;
        for a in args {
            if let mir::Arg::Mut(pl, _) = a {
                out.push(ir::Arg::Place(self.place_or_copy(pl)?));
            } else if let Some(v) = self.arg_value(a) {
                out.push(ir::Arg::Value(v));
            }
        }
        let callee = self.cx.instance(self.mb, callable.instance_key(), Some((self.id, span)));
        self.emit_call(callee, out)
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
                let v = self.math(ib, args, ty);
                if matches!(ib, ir::Builtin::Dpdx | ir::Builtin::Dpdy | ir::Builtin::Fwidth) {
                    crate::gpu::check_derivative(self, v, span);
                }
                return v;
            }
            BuiltinIr::Select => {
                ir::Expr::Select { cond: args[2], if_true: args[1], if_false: args[0] }
            }
            BuiltinIr::Bitcast(s) => ir::Expr::Bitcast(args[0], s),
            BuiltinIr::Wrapping(op) => ir::Expr::Binary(op, args[0], args[1]),
            BuiltinIr::Len => unreachable!("`len` is `Rvalue::Len`"),
            BuiltinIr::Panic => unreachable!("`panic` and `assert` are lowered at their calls"),
            BuiltinIr::Embed => unreachable!("the checker makes `embed` an expression of its own"),
        };
        Some(self.value(ty, e))
    }

    /// A math builtin. On the CPU the transcendentals become calls to std's wrela functions
    /// (§11), but only after derived interpretations are built (`rewrite_cpu_math`), so
    /// gradients and intervals see the builtins themselves. (Of an `f64`, they're E0702 in the
    /// checker: std's CPU math is `f32`.)
    pub fn math(
        &mut self,
        b: ir::Builtin,
        args: Vec<ir::ValueId>,
        ty: ir::TypeId,
    ) -> Option<ir::ValueId> {
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

/// A literal as a constant of the IR type `ty` (a scalar).
pub(crate) fn lit_const(types: &ir::Types, l: Lit, ty: ir::TypeId) -> Option<ir::Const> {
    let s = types.as_scalar(ty)?;
    Some(match (l, s) {
        (Lit::Bool(b), _) => ir::Const::Bool(b),
        (Lit::Int(v), ir::Scalar::I32) => ir::Const::I32(v as i32),
        (Lit::Int(v), ir::Scalar::U32) => ir::Const::U32(v as u32),
        (Lit::Int(v), ir::Scalar::I64) => ir::Const::I64(v as i64),
        (Lit::Int(v), ir::Scalar::U64) => ir::Const::U64(v as u64),
        (Lit::Int(v), ir::Scalar::F32) => ir::Const::F32(v as f32),
        (Lit::Int(v), ir::Scalar::F64) => ir::Const::F64(v as f64),
        (Lit::Int(v), s) => ir::Const::Small(s, v as i64),
        (Lit::Float(_, f), ir::Scalar::F32) => ir::Const::F32(f),
        (Lit::Float(v, _), _) => ir::Const::F64(v),
    })
}

/// What a built-in function lowers to.
pub(crate) enum BuiltinIr {
    Math(ir::Builtin),
    Select,
    Bitcast(ir::Scalar),
    Wrapping(ir::BinOp),
    Len,
    /// `panic` and `assert`: lowered where they're called.
    Panic,
    /// `embed`, which is never called: the checker makes it an expression of its own.
    Embed,
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
        BuiltinFn::Sinh => Math(I::Sinh),
        BuiltinFn::Cosh => Math(I::Cosh),
        BuiltinFn::Tanh => Math(I::Tanh),
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
        BuiltinFn::CountOnes => Math(I::CountOnes),
        BuiltinFn::LeadingZeros => Math(I::LeadingZeros),
        BuiltinFn::TrailingZeros => Math(I::TrailingZeros),
        BuiltinFn::Len => BuiltinIr::Len,
        BuiltinFn::Panic | BuiltinFn::Assert => BuiltinIr::Panic,
        BuiltinFn::Embed => BuiltinIr::Embed,
    }
}
