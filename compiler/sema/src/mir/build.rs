//! Building a function's MIR from its typed tree.
//!
//! The builder fixes evaluation order: operands left to right, a call's arguments in the order
//! they're written (then defaults), a struct literal's fields as written, then its `..base`.
//! Every value an operand reads is copied into a temporary where it's evaluated, so later
//! operands can't change what an earlier one read. A call's `borrow` and `mut` arguments are
//! bound to argument locals ([`LocalKind::Arg`]) where they're evaluated: their loans then last
//! until the call, across anything evaluated in between.
//!
//! The memory rules that depend only on how code is written are checked here: a move out of a
//! named place needs `take` (E0501, E0502), `var` can't share a place (E0517), `mut x = ...`
//! needs a place (E0512), and a `mut` projection is returned as `mut place` (E0503). The rest,
//! which depend on control flow, are the borrow checker's.

use super::*;
use crate::defs::{Lang, Mode, RetMode};
use crate::program::Program;
use crate::thir::{self, ExprKind, PatKind, StmtKind};
use crate::traits;
use std::collections::BTreeMap;
use wrela_diag::{Diagnostic, codes};

/// The checked constants: their types and values.
pub type Consts = BTreeMap<ConstId, (TyId, thir::Expr)>;

/// How an expression's value is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Want {
    /// Read: a `Copy` value is copied; anything else is only looked at.
    Read,
    /// Consumed: a non-`Copy` place has to be `take`n.
    Move,
    /// Returned from the function: a whole owned local is moved out implicitly.
    Return,
}

/// Builds `f`'s MIR, with the diagnostics of the rules checked while building.
pub fn build(p: &Program, consts: &Consts, f: FnId, body: &thir::Body) -> (Body, Vec<Diagnostic>) {
    let def = p.func(f);
    let locals = body
        .locals
        .iter()
        .map(|d| LocalDecl {
            name: d.name.clone(),
            ty: d.ty,
            kind: LocalKind::User(d.kind),
            span: d.span,
            keyword: d.keyword,
            closure: d.closure,
        })
        .collect();
    let mut b = Builder {
        p,
        consts,
        thir: body,
        locals,
        diags: Vec::new(),
        fs: FnState::new(def.ret_mode, None, body.value.span),
    };
    let mut fns = vec![b.build_fn(&body.params, &body.value, def.ret_mode, None, body.value.span)];
    for (i, c) in body.closures.iter().enumerate() {
        let id = ClosureId(i as u32);
        fns.push(b.build_fn(&c.params, &c.body, RetMode::Owned, Some(id), c.span));
    }
    let closures = body
        .closures
        .iter()
        .map(|c| ClosureInfo {
            params: c.params.clone(),
            ret: c.ret,
            captures: c.captures.clone(),
            span: c.span,
        })
        .collect();
    let mir = Body { locals: b.locals, fns, closures, hidden_ret: body.hidden_ret };
    (mir, b.diags)
}

/// The function being built.
struct FnState {
    blocks: Vec<BlockData>,
    cur: BlockId,
    /// Enclosing loops' `Loop` blocks, innermost last.
    loops: Vec<BlockId>,
    ret_mode: RetMode,
    closure: Option<ClosureId>,
    span: Span,
}

impl FnState {
    fn new(ret_mode: RetMode, closure: Option<ClosureId>, span: Span) -> FnState {
        FnState { blocks: Vec::new(), cur: BlockId(0), loops: Vec::new(), ret_mode, closure, span }
    }
}

struct Builder<'a> {
    p: &'a Program,
    consts: &'a Consts,
    thir: &'a thir::Body,
    locals: Vec<LocalDecl>,
    diags: Vec<Diagnostic>,
    fs: FnState,
}

impl<'a> Builder<'a> {
    fn build_fn(
        &mut self,
        params: &[Local],
        value: &thir::Expr,
        ret_mode: RetMode,
        closure: Option<ClosureId>,
        span: Span,
    ) -> FnBody {
        self.fs = FnState::new(ret_mode, closure, span);
        let entry = self.new_block();
        self.fs.cur = entry;
        if ret_mode == RetMode::Owned {
            let want = if closure.is_some() { Want::Move } else { Want::Return };
            let v = self.value(value, want);
            self.terminate(TerminatorKind::Return(v), value.span);
        } else {
            self.place_tail(value);
        }
        FnBody {
            params: params.to_vec(),
            blocks: std::mem::take(&mut self.fs.blocks),
            ret_mode,
            span,
        }
    }

    // ---- blocks ----------------------------------------------------------------------------

    fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.fs.blocks.len() as u32);
        let term = Terminator { kind: TerminatorKind::Unreachable, span: self.fs.span };
        self.fs.blocks.push(BlockData { stmts: Vec::new(), term });
        id
    }

    fn push(&mut self, kind: StatementKind, span: Span) {
        let b = self.fs.cur.index();
        self.fs.blocks[b].stmts.push(Statement { kind, span });
    }

    /// Ends the current block; what follows goes in a new block nothing reaches yet.
    fn terminate(&mut self, kind: TerminatorKind, span: Span) {
        let b = self.fs.cur.index();
        self.fs.blocks[b].term = Terminator { kind, span };
        self.fs.cur = self.new_block();
    }

    fn goto(&mut self, target: BlockId, span: Span) {
        self.terminate(TerminatorKind::Goto(target), span);
    }

    // ---- locals ----------------------------------------------------------------------------

    fn temp(&mut self, ty: TyId, kind: LocalKind, name: impl Into<String>, span: Span) -> Local {
        let id = Local(self.locals.len() as u32);
        self.locals.push(LocalDecl {
            name: name.into(),
            ty,
            kind,
            span,
            keyword: None,
            closure: self.fs.closure,
        });
        id
    }

    fn is_copy(&self, t: TyId) -> bool {
        traits::implements_builtin(self.p, t, Lang::Copy)
    }

    /// Whether a value of this type is nothing at run time: `()` or `!`.
    fn is_unit(&self, t: TyId) -> bool {
        matches!(self.p.types.kind(t), TyKind::Never | TyKind::Error)
            || matches!(self.p.types.kind(t), TyKind::Tuple(ts) if ts.is_empty())
    }

    /// Whether `l` is outside the closure being built (a capture).
    fn is_capture(&self, l: Local) -> bool {
        match self.fs.closure {
            Some(c) => {
                self.locals[l.index()].closure != Some(c)
                    && !self.thir.closures[c.0 as usize].params.contains(&l)
            }
            None => false,
        }
    }

    fn describe(&self, place: &Place) -> String {
        describe(self.p, &self.locals, place)
    }

    /// `t = value`, returning a copy of `t`. `None` for a value that is nothing.
    fn temp_of(&mut self, rv: Rvalue, ty: TyId, span: Span) -> Option<Operand> {
        if self.is_unit(ty) {
            self.push(StatementKind::Eval(rv), span);
            return None;
        }
        let t = self.temp(ty, LocalKind::Temp, "temporary", span);
        self.push(StatementKind::Assign(Place::local(t), rv), span);
        Some(Operand { kind: OperandKind::Copy(Place::local(t)), ty, span })
    }

    /// An operand read where it's evaluated: copied into a temporary now.
    fn materialize(&mut self, kind: OperandKind, ty: TyId, span: Span) -> Operand {
        let op = Operand { kind, ty, span };
        let t = self.temp(ty, LocalKind::Temp, "temporary", span);
        self.push(StatementKind::Assign(Place::local(t), Rvalue::Use(op)), span);
        Operand { kind: OperandKind::Copy(Place::local(t)), ty, span }
    }

    /// A local holding an operand's value (for an index).
    fn operand_local(&mut self, op: Operand) -> Local {
        if let OperandKind::Copy(p) = &op.kind
            && p.proj.is_empty()
            && self.locals[p.local.index()].kind == LocalKind::Temp
        {
            return p.local;
        }
        let (ty, span) = (op.ty, op.span);
        let t = self.temp(ty, LocalKind::Temp, "index", span);
        self.push(StatementKind::Assign(Place::local(t), Rvalue::Use(op)), span);
        t
    }

    /// A place holding an operand's value.
    fn operand_place(&mut self, op: Operand) -> Place {
        Place::local(self.operand_local(op))
    }

    // ---- expressions -----------------------------------------------------------------------

    /// Whether `e` names a place the memory rules see: rooted at a local, or what a
    /// projection-returning call returned.
    fn is_named_place(e: &thir::Expr) -> bool {
        match &e.kind {
            ExprKind::Local(_) => true,
            ExprKind::Field(b, _) | ExprKind::Swizzle(b, _) | ExprKind::Index(b, _) => {
                Self::is_named_place(b)
            }
            ExprKind::Call(c) => c.ret_mode != RetMode::Owned,
            _ => false,
        }
    }

    /// Evaluates `e` for its value. `None` when it has none: `()`, `!`, or after an error.
    fn value(&mut self, e: &thir::Expr, want: Want) -> Option<Operand> {
        let span = e.span;
        match &e.kind {
            ExprKind::Lit(l) => Some(Operand { kind: OperandKind::Const(*l), ty: e.ty, span }),
            ExprKind::Unary(UnOp::Neg, x)
                if matches!(x.kind, ExprKind::Lit(Lit::Int(_) | Lit::Float(_))) =>
            {
                // A negative literal is one constant: `-2147483648` is an i32 as written, not
                // the negation of a number too large for one.
                let l = match x.kind {
                    ExprKind::Lit(Lit::Int(v)) if self.p.types.is_int(e.ty) => Lit::Int(-v),
                    ExprKind::Lit(Lit::Int(v)) => Lit::Float(-(v as f64)),
                    ExprKind::Lit(Lit::Float(f)) => Lit::Float(-f),
                    _ => unreachable!("matched above"),
                };
                Some(Operand { kind: OperandKind::Const(l), ty: e.ty, span })
            }
            ExprKind::Local(_)
            | ExprKind::Field(..)
            | ExprKind::Swizzle(..)
            | ExprKind::Index(..)
                if Self::is_named_place(e) =>
            {
                self.place_value(e, want)
            }
            ExprKind::Field(..) | ExprKind::Swizzle(..) | ExprKind::Index(..) => {
                // Part of a temporary.
                let p = self.place(e)?;
                Some(self.materialize(OperandKind::Copy(p), e.ty, span))
            }
            ExprKind::Const(c) => {
                let (_, v) = self.consts.get(c)?;
                self.value(v, Want::Read)
            }
            ExprKind::Take(inner) => self.take(inner),
            ExprKind::MutArg(inner) => self.value(inner, want),
            ExprKind::Block(b) => self.block(b, want),
            ExprKind::If { cond, then, else_ } => self.if_(cond, then, else_.as_deref(), e, want),
            ExprKind::Match { scrutinee, arms } => self.match_(scrutinee, arms, e, want),
            ExprKind::Binary(op @ (BinOp::And | BinOp::Or), a, b) => {
                self.short_circuit(*op, a, b, e)
            }
            ExprKind::Return(v) => {
                self.return_(v.as_deref(), span);
                None
            }
            ExprKind::Break => {
                if let Some(&l) = self.fs.loops.last() {
                    self.terminate(TerminatorKind::Break(l), span);
                }
                None
            }
            ExprKind::Continue => {
                if let Some(&l) = self.fs.loops.last() {
                    self.terminate(TerminatorKind::Continue(l), span);
                }
                None
            }
            ExprKind::Call(c) => self.call_value(c, e, false, want),
            ExprKind::Error | ExprKind::FromBase => None,
            _ => {
                let rv = self.rvalue(e)?;
                self.temp_of(rv, e.ty, span)
            }
        }
    }

    /// The value of a named place, as `want` uses it.
    fn place_value(&mut self, e: &thir::Expr, want: Want) -> Option<Operand> {
        let place = self.place(e)?;
        let kind = match want {
            Want::Read => OperandKind::Copy(place),
            Want::Move | Want::Return if self.is_copy(e.ty) => OperandKind::Copy(place),
            Want::Return if place.proj.is_empty() && self.owned(place.local) => {
                OperandKind::Move(place, MoveKind::Return)
            }
            Want::Move | Want::Return => {
                self.unmarked_move(&place, e);
                OperandKind::Move(place, MoveKind::Unmarked)
            }
        };
        Some(self.materialize(kind, e.ty, e.span))
    }

    /// Whether the function owns `l` outright (so it can move out of it).
    fn owned(&self, l: Local) -> bool {
        matches!(
            self.locals[l.index()].kind,
            LocalKind::User(thir::LocalKind::Owned { .. } | thir::LocalKind::Param(Mode::Take))
                | LocalKind::Temp
                | LocalKind::TempVar
        ) && !self.is_capture(l)
    }

    /// E0501 or E0502: a non-`Copy` named place used where its value is consumed, unmarked.
    fn unmarked_move(&mut self, place: &Place, e: &thir::Expr) {
        let what = self.describe(place);
        let shown = self.p.display_ty(e.ty);
        let indexed = place.proj.iter().any(|p| matches!(p, Proj::Index(_)));
        let d = if self.owned(place.local) && !indexed {
            Diagnostic::new(
                codes::E0501,
                e.span,
                format!("moving `{what}` out of a named place is written `take {what}`"),
            )
            .with_note(format!("`{shown}` isn't `Copy`, so using it here moves it (§6.1)"))
            .with_fix(
                format!("move it: `take {what}`"),
                e.span.shrink_to_start(),
                "take ",
            )
        } else {
            let why = if indexed {
                "it's an element of an array".to_string()
            } else {
                self.why_not_owned(place.local)
            };
            Diagnostic::new(codes::E0502, e.span, format!("can't move `{what}` here: {why}"))
                .with_note(format!("`{shown}` isn't `Copy`, so using it here would move it"))
                .with_fix(format!("copy it: `{what}.clone()`"), e.span.shrink_to_end(), ".clone()")
        };
        self.diags.push(d);
    }

    fn why_not_owned(&self, l: Local) -> String {
        let d = &self.locals[l.index()];
        if self.is_capture(l) {
            return format!("the closure borrows `{}` from its surroundings", d.name);
        }
        match d.kind {
            LocalKind::User(thir::LocalKind::Param(_)) => {
                format!("`{}` is borrowed by this function, not owned", d.name)
            }
            LocalKind::User(thir::LocalKind::Projection { .. }) => {
                format!("`{}` is a projection of another place", d.name)
            }
            LocalKind::User(thir::LocalKind::ClosureParam) => {
                format!("`{}` is a closure parameter", d.name)
            }
            LocalKind::TempProjection { .. } | LocalKind::Arg { .. } => {
                format!("`{}` is a projection", d.name)
            }
            _ => String::new(),
        }
    }

    /// `take place`, or `take recv.method()` for a consuming method.
    fn take(&mut self, inner: &thir::Expr) -> Option<Operand> {
        if let ExprKind::Call(c) = &inner.kind
            && c.receiver
            && c.modes.first() == Some(&Mode::Take)
        {
            return self.call_value(c, inner, true, Want::Move);
        }
        if !Self::is_named_place(inner) {
            return self.value(inner, Want::Move);
        }
        let place = self.place(inner)?;
        let kind = if self.is_copy(inner.ty) {
            OperandKind::Copy(place)
        } else {
            OperandKind::Move(place, MoveKind::Take)
        };
        Some(self.materialize(kind, inner.ty, inner.span))
    }

    /// The place an expression names; a temporary holding its value if it isn't one.
    fn place(&mut self, e: &thir::Expr) -> Option<Place> {
        match &e.kind {
            ExprKind::Local(l) => Some(Place::local(*l)),
            ExprKind::Field(b, i) => {
                let p = self.place(b)?;
                Some(p.with(Proj::Field(*i)))
            }
            ExprKind::Swizzle(b, comps) => {
                let p = self.place(b)?;
                Some(self.swizzle(p, comps))
            }
            ExprKind::Index(b, i) => {
                let p = self.place(b)?;
                if let (TyKind::Vec(_), ExprKind::Lit(Lit::Int(c))) =
                    (self.p.types.kind(b.ty), &i.kind)
                {
                    // A literal index into a vector is a component (checked in range).
                    return Some(self.swizzle(p, &[*c as u8]));
                }
                let iv = self.value(i, Want::Read)?;
                let t = self.operand_local(iv);
                Some(p.with(Proj::Index(t)))
            }
            ExprKind::Call(c) if c.ret_mode != RetMode::Owned => self.call_place(c, e, false),
            ExprKind::MutArg(x) => self.place(x),
            _ => {
                let v = self.value(e, Want::Move)?;
                Some(self.operand_place(v))
            }
        }
    }

    /// `p.xyz`: components of a vector place. A swizzle of a swizzle is one swizzle.
    fn swizzle(&mut self, p: Place, comps: &[u8]) -> Place {
        let comps: Vec<u8> = match p.proj.last() {
            Some(Proj::Swizzle(prev)) => comps.iter().map(|&c| prev[c as usize]).collect(),
            Some(Proj::Comp(_)) => {
                // A component's swizzle isn't a vector; the checker rejects it.
                return p;
            }
            _ => comps.to_vec(),
        };
        let mut base = p;
        if matches!(base.proj.last(), Some(Proj::Swizzle(_))) {
            base.proj.pop();
        }
        if comps.len() == 1 {
            base.with(Proj::Comp(comps[0]))
        } else {
            base.with(Proj::Swizzle(comps))
        }
    }

    fn rvalue(&mut self, e: &thir::Expr) -> Option<Rvalue> {
        Some(match &e.kind {
            ExprKind::Unary(op, x) => Rvalue::Unary(*op, self.value(x, Want::Read)?),
            ExprKind::Binary(op, a, b) => {
                let a = self.value(a, Want::Read)?;
                let b = self.value(b, Want::Read)?;
                Rvalue::Binary(*op, a, b)
            }
            ExprKind::Adt { adt, args, variant, fields, order, base } => {
                let mut vals: Vec<Option<Operand>> = vec![None; fields.len()];
                for &i in order {
                    vals[i as usize] = Some(self.value(&fields[i as usize], Want::Move)?);
                }
                let base_place = match base {
                    Some(b) if Self::is_named_place(b) => Some(self.place(b)?),
                    Some(b) => {
                        let v = self.value(b, Want::Move)?;
                        Some(self.operand_place(v))
                    }
                    None => None,
                };
                let mut out = Vec::new();
                for (i, (v, f)) in vals.into_iter().zip(fields).enumerate() {
                    match (v, &f.kind, &base_place) {
                        (Some(v), ..) => out.push(v),
                        (None, ExprKind::FromBase, Some(bp)) => {
                            let fp = bp.with(Proj::Field(i as u32));
                            let base_expr = base.as_deref().expect("a base");
                            let named = Self::is_named_place(base_expr);
                            let kind = if named && !self.is_copy(f.ty) {
                                let probe =
                                    thir::Expr { ty: f.ty, kind: ExprKind::Error, span: f.span };
                                self.unmarked_move(&fp, &probe);
                                OperandKind::Move(fp, MoveKind::Unmarked)
                            } else {
                                OperandKind::Copy(fp)
                            };
                            out.push(self.materialize(kind, f.ty, f.span));
                        }
                        (None, ..) => out.push(self.value(f, Want::Move)?),
                    }
                }
                Rvalue::Adt { adt: *adt, args: args.clone(), variant: *variant, fields: out }
            }
            ExprKind::Tuple(xs) => Rvalue::Tuple(self.values(xs, Want::Move)?),
            ExprKind::Array(xs) => Rvalue::Array(self.values(xs, Want::Move)?),
            ExprKind::Construct(xs) => Rvalue::Construct(self.values(xs, Want::Read)?),
            ExprKind::ArrayRepeat(x, n) => Rvalue::ArrayRepeat(self.value(x, Want::Read)?, *n),
            ExprKind::Convert(x) => Rvalue::Convert(self.value(x, Want::Read)?),
            ExprKind::Closure(id) => Rvalue::Closure(*id),
            ExprKind::FnRef(f, args) => Rvalue::FnRef(*f, args.clone()),
            ExprKind::Dispatch(d) => self.dispatch(d)?,
            ExprKind::Draw(d) => self.draw(d)?,
            _ => return None,
        })
    }

    fn values(&mut self, xs: &[thir::Expr], want: Want) -> Option<Vec<Operand>> {
        xs.iter().map(|x| self.value(x, want)).collect()
    }

    fn short_circuit(
        &mut self,
        op: BinOp,
        a: &thir::Expr,
        b: &thir::Expr,
        e: &thir::Expr,
    ) -> Option<Operand> {
        let av = self.value(a, Want::Read)?;
        let r = self.temp(e.ty, LocalKind::TempVar, "logic", e.span);
        self.push(StatementKind::Assign(Place::local(r), Rvalue::Use(av.clone())), e.span);
        // `a && b` evaluates `b` when `a`; `a || b` when not `a`.
        let cond = match op {
            BinOp::And => av,
            _ => self.temp_of(Rvalue::Unary(UnOp::Not, av), e.ty, e.span)?,
        };
        let (t, el, m) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(TerminatorKind::If { cond, then: t, else_: el, merge: m }, e.span);
        self.fs.cur = t;
        if let Some(bv) = self.value(b, Want::Read) {
            self.push(StatementKind::Assign(Place::local(r), Rvalue::Use(bv)), e.span);
        }
        self.goto(m, e.span);
        self.fs.cur = el;
        self.goto(m, e.span);
        self.fs.cur = m;
        Some(Operand { kind: OperandKind::Copy(Place::local(r)), ty: e.ty, span: e.span })
    }

    // ---- calls -----------------------------------------------------------------------------

    /// A call's arguments, evaluated in the order written, and its callee.
    fn call(&mut self, c: &thir::Call, e: &thir::Expr, receiver_taken: bool) -> Option<Call> {
        if c.receiver
            && c.modes.first() == Some(&Mode::Take)
            && !receiver_taken
            && let Some(recv) = c.args.first()
            && Self::is_named_place(recv)
            && !self.is_copy(recv.ty)
        {
            // A consuming method on a named place must be marked `take x.m()`.
            let what = self.receiver_text(recv);
            self.diags.push(
                Diagnostic::new(codes::E0501, e.span, format!("this method takes `{what}`, so the call is written `take {what}...`"))
                    .with_note("a consuming method moves its receiver; moving out of a named place is marked (§6.2)")
                    .with_fix("mark the move", e.span.shrink_to_start(), "take "),
            );
        }
        let callee = match &c.callee {
            thir::Callee::Fn { func, args } => Callee::Fn { func: *func, args: args.clone() },
            thir::Callee::TraitMethod { method, self_ty, trait_args, method_args } => {
                Callee::TraitMethod {
                    method: *method,
                    self_ty: *self_ty,
                    trait_args: trait_args.clone(),
                    method_args: method_args.clone(),
                }
            }
            thir::Callee::Builtin(b) => Callee::Builtin(*b),
            thir::Callee::Local(l) => Callee::Local(*l),
            thir::Callee::Clone => Callee::Clone,
        };
        let mut args: Vec<Option<Arg>> = vec![None; c.args.len()];
        for &i in &c.order {
            let a = &c.args[i];
            let mode = c.modes.get(i).copied().unwrap_or(Mode::Borrow);
            let is_recv = c.receiver && i == 0;
            let arg = match mode {
                Mode::Take => {
                    let v = if is_recv && Self::is_named_place(a) {
                        let place = self.place(a)?;
                        let kind = if self.is_copy(a.ty) {
                            OperandKind::Copy(place)
                        } else if receiver_taken {
                            OperandKind::Move(place, MoveKind::Take)
                        } else {
                            // Reported above; checked as a read so later uses still are.
                            OperandKind::Copy(place)
                        };
                        self.materialize(kind, a.ty, a.span)
                    } else {
                        self.value(a, Want::Move)?
                    };
                    Arg::Take(v)
                }
                Mode::Mut => {
                    let inner = match &a.kind {
                        ExprKind::MutArg(x) => x.as_ref(),
                        _ => a,
                    };
                    let p = self.place(inner)?;
                    Arg::Mut(self.arg_local(p, true, inner.span), a.span)
                }
                Mode::Borrow => {
                    if self.is_callable(a.ty) {
                        // A closure or function: a value whose captures are its loans.
                        let p = match &a.kind {
                            ExprKind::Local(l) => Place::local(*l),
                            _ => {
                                let rv = self.rvalue(a)?;
                                let t = self.temp(a.ty, LocalKind::Temp, "closure", a.span);
                                self.push(StatementKind::Assign(Place::local(t), rv), a.span);
                                Place::local(t)
                            }
                        };
                        Arg::Borrow(p, a.span)
                    } else if Self::is_named_place(a) {
                        let p = self.place(a)?;
                        Arg::Borrow(self.arg_local(p, false, a.span), a.span)
                    } else {
                        let v = self.value(a, Want::Read)?;
                        Arg::Borrow(self.operand_place(v), a.span)
                    }
                }
            };
            args[i] = Some(arg);
        }
        let args = args.into_iter().collect::<Option<Vec<Arg>>>()?;
        Some(Call { callee, args, receiver: c.receiver, ret_mode: c.ret_mode, span: e.span })
    }

    fn receiver_text(&mut self, recv: &thir::Expr) -> String {
        let mut e = recv;
        while let ExprKind::MutArg(x) = &e.kind {
            e = x;
        }
        // Describe without evaluating: walk the expression as a path.
        let mut parts = Vec::new();
        loop {
            match &e.kind {
                ExprKind::Local(l) => {
                    parts.push(self.locals[l.index()].name.clone());
                    break;
                }
                ExprKind::Field(b, i) => {
                    let name = match self.p.types.kind(b.ty).clone() {
                        TyKind::Adt(a, args) => self
                            .p
                            .struct_fields(a, &args)
                            .get(*i as usize)
                            .map_or_else(|| i.to_string(), |f| f.0.clone()),
                        _ => i.to_string(),
                    };
                    parts.push(format!(".{name}"));
                    e = b;
                }
                ExprKind::Index(b, _) => {
                    parts.push("[_]".into());
                    e = b;
                }
                ExprKind::Swizzle(b, cs) => {
                    let s: String =
                        cs.iter().map(|&c| ['x', 'y', 'z', 'w'][c as usize % 4]).collect();
                    parts.push(format!(".{s}"));
                    e = b;
                }
                _ => break,
            }
        }
        parts.reverse();
        parts.concat()
    }

    fn is_callable(&self, t: TyId) -> bool {
        matches!(self.p.types.kind(t), TyKind::Closure(..) | TyKind::FnPtr(..) | TyKind::FnDef(..))
    }

    /// Binds a call's `borrow` or `mut` argument where it's evaluated. A temporary needs no
    /// binding: nothing else can reach it.
    fn arg_local(&mut self, p: Place, mutable: bool, span: Span) -> Place {
        if p.proj.is_empty() && matches!(self.locals[p.local.index()].kind, LocalKind::Temp) {
            return p;
        }
        let ty = place_ty(self.p, &self.locals, &p);
        let name = self.describe(&p);
        let a = self.temp(ty, LocalKind::Arg { mutable }, name, span);
        self.push(StatementKind::Bind { local: a, place: p, mutable }, span);
        Place::local(a)
    }

    fn call_value(
        &mut self,
        c: &thir::Call,
        e: &thir::Expr,
        receiver_taken: bool,
        want: Want,
    ) -> Option<Operand> {
        if c.ret_mode != RetMode::Owned {
            let p = self.call_place(c, e, receiver_taken)?;
            let kind = match want {
                Want::Move | Want::Return if !self.is_copy(e.ty) => {
                    let probe = thir::Expr { ty: e.ty, kind: ExprKind::Error, span: e.span };
                    self.unmarked_move(&p, &probe);
                    OperandKind::Move(p, MoveKind::Unmarked)
                }
                _ => OperandKind::Copy(p),
            };
            return Some(self.materialize(kind, e.ty, e.span));
        }
        if let thir::Callee::Builtin(BuiltinFn::Len) = c.callee {
            let a = c.args.first()?;
            let p = self.place(a)?;
            return self.temp_of(Rvalue::Len(p), e.ty, e.span);
        }
        let call = self.call(c, e, receiver_taken)?;
        self.temp_of(Rvalue::Call(call), e.ty, e.span)
    }

    /// A projection-returning call: a local holding the place it returned.
    fn call_place(
        &mut self,
        c: &thir::Call,
        e: &thir::Expr,
        receiver_taken: bool,
    ) -> Option<Place> {
        let call = self.call(c, e, receiver_taken)?;
        let name = match &call.callee {
            Callee::Fn { func, .. } | Callee::TraitMethod { method: func, .. } => {
                format!("{}(..)", self.p.func(*func).name)
            }
            _ => "the call's result".into(),
        };
        let mutable = c.ret_mode == RetMode::Mut;
        let t = self.temp(e.ty, LocalKind::TempProjection { mutable }, name, e.span);
        self.push(StatementKind::Assign(Place::local(t), Rvalue::Call(call)), e.span);
        Some(Place::local(t))
    }

    fn dispatch(&mut self, d: &thir::Dispatch) -> Option<Rvalue> {
        let g = self.value(&d.groups, Want::Read)?;
        let u32_ty = self.p.types.u32;
        let groups = if matches!(self.p.types.kind(d.groups.ty), TyKind::Tuple(_)) {
            let gp = self.operand_place(g);
            let f = |k: u32, b: &mut Self| {
                b.materialize(OperandKind::Copy(gp.with(Proj::Field(k))), u32_ty, d.groups.span)
            };
            [f(0, self), f(1, self), f(2, self)]
        } else {
            let one =
                Operand { kind: OperandKind::Const(Lit::Int(1)), ty: u32_ty, span: d.groups.span };
            [g, one.clone(), one]
        };
        let mut args = Vec::new();
        for (i, a) in &d.args {
            let p = self.borrowed_place(a)?;
            args.push((*i, p, a.span));
        }
        Some(Rvalue::Dispatch(Box::new(Dispatch {
            kernel: d.kernel,
            kernel_args: d.kernel_args.clone(),
            groups,
            args,
        })))
    }

    fn draw(&mut self, d: &thir::Draw) -> Option<Rvalue> {
        let vertices = self.value(&d.vertices, Want::Read)?;
        let instances = self.value(&d.instances, Want::Read)?;
        let mut args = Vec::new();
        for (n, a) in &d.args {
            let p = self.borrowed_place(a)?;
            args.push((n.clone(), p, a.span));
        }
        Some(Rvalue::Draw(Box::new(Draw {
            vertex: d.vertex.clone(),
            fragment: d.fragment.clone(),
            vertices,
            instances,
            args,
        })))
    }

    /// A place borrowed for a dispatch or draw: bound where it's evaluated, like a call's.
    fn borrowed_place(&mut self, a: &thir::Expr) -> Option<Place> {
        if Self::is_named_place(a) {
            let p = self.place(a)?;
            Some(self.arg_local(p, false, a.span))
        } else {
            let v = self.value(a, Want::Read)?;
            Some(self.operand_place(v))
        }
    }

    // ---- control flow ----------------------------------------------------------------------

    fn block(&mut self, b: &thir::Block, want: Want) -> Option<Operand> {
        let mut declared = Vec::new();
        for s in &b.stmts {
            self.stmt(s, &mut declared);
        }
        let v = b.tail.as_ref().and_then(|t| self.value(t, want));
        for l in declared.into_iter().rev() {
            self.push(StatementKind::Dead(l), b.span);
        }
        v
    }

    fn if_(
        &mut self,
        cond: &thir::Expr,
        then: &thir::Block,
        else_: Option<&thir::Expr>,
        e: &thir::Expr,
        want: Want,
    ) -> Option<Operand> {
        let c = self.value(cond, Want::Read)?;
        let r = (!self.is_unit(e.ty)).then(|| self.temp(e.ty, LocalKind::TempVar, "if", e.span));
        let (t, el, m) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(TerminatorKind::If { cond: c, then: t, else_: el, merge: m }, e.span);
        self.fs.cur = t;
        let v = self.block(then, want);
        if let (Some(r), Some(v)) = (r, v) {
            self.push(StatementKind::Assign(Place::local(r), Rvalue::Use(v)), then.span);
        }
        self.goto(m, e.span);
        self.fs.cur = el;
        if let Some(x) = else_ {
            let v = self.value(x, want);
            if let (Some(r), Some(v)) = (r, v) {
                self.push(StatementKind::Assign(Place::local(r), Rvalue::Use(v)), x.span);
            }
        }
        self.goto(m, e.span);
        self.fs.cur = m;
        r.map(|r| Operand { kind: OperandKind::Copy(Place::local(r)), ty: e.ty, span: e.span })
    }

    /// The scrutinee's place: its own if it's a named place, else a temporary.
    fn scrutinee(&mut self, s: &thir::Expr) -> Option<Place> {
        if Self::is_named_place(s) {
            self.place(s)
        } else {
            let v = self.value(s, Want::Move)?;
            Some(self.operand_place(v))
        }
    }

    fn match_(
        &mut self,
        scrutinee: &thir::Expr,
        arms: &[thir::Arm],
        e: &thir::Expr,
        want: Want,
    ) -> Option<Operand> {
        let sp = self.scrutinee(scrutinee)?;
        let r = (!self.is_unit(e.ty)).then(|| self.temp(e.ty, LocalKind::TempVar, "match", e.span));
        self.arms(&sp, arms, e.span, |b, arm| {
            let v = b.value(&arm.body, want);
            if let (Some(r), Some(v)) = (r, v) {
                b.push(StatementKind::Assign(Place::local(r), Rvalue::Use(v)), arm.body.span);
            }
            true
        });
        r.map(|r| Operand { kind: OperandKind::Copy(Place::local(r)), ty: e.ty, span: e.span })
    }

    /// A `Match`: each arm tests its pattern and guard, binds, runs `body` (which returns
    /// whether the arm then ends normally), and ends in `ArmMatched`. Leaves `cur` at the merge.
    fn arms(
        &mut self,
        sp: &Place,
        arms: &[thir::Arm],
        span: Span,
        mut body: impl FnMut(&mut Self, &thir::Arm) -> bool,
    ) {
        let m = self.fs.cur;
        let merge = self.new_block();
        let entries: Vec<BlockId> = arms.iter().map(|_| self.new_block()).collect();
        // After the last arm: no arm matched, which exhaustiveness rules out.
        let none = self.new_block();
        self.terminate(TerminatorKind::Match { arms: entries.clone(), merge }, span);
        for (i, arm) in arms.iter().enumerate() {
            self.fs.cur = entries[i];
            let next = entries.get(i + 1).copied().unwrap_or(none);
            let fail = TerminatorKind::ArmFailed { match_: m, next };
            if let Some(t) = self.pat_test(&arm.pat, sp) {
                self.branch_or(t, fail.clone(), arm.pat.span);
            }
            let mut bound = Vec::new();
            self.bind_pat(&arm.pat, sp, &mut bound);
            if let Some(g) = &arm.guard
                && let Some(gv) = self.value(g, Want::Read)
            {
                self.branch_or(gv, fail.clone(), g.span);
            }
            let falls_through = body(self, arm);
            for l in bound.into_iter().rev() {
                self.push(StatementKind::Dead(l), arm.span);
            }
            if falls_through {
                self.terminate(TerminatorKind::ArmMatched(m), arm.span);
            } else {
                self.terminate(TerminatorKind::Unreachable, arm.span);
            }
        }
        self.fs.cur = merge;
    }

    /// Continues in a new block when `cond` holds; otherwise ends with `otherwise`.
    fn branch_or(&mut self, cond: Operand, otherwise: TerminatorKind, span: Span) {
        let (yes, no, after) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(TerminatorKind::If { cond, then: yes, else_: no, merge: after }, span);
        self.fs.cur = no;
        self.terminate(otherwise, span);
        self.fs.cur = yes;
    }

    /// The condition under which `pat` matches the value at `place`; `None` means always.
    fn pat_test(&mut self, pat: &thir::Pat, place: &Place) -> Option<Operand> {
        let bool_ty = self.p.types.bool;
        match &pat.kind {
            PatKind::Wild | PatKind::Bind(_) => None,
            PatKind::Lit(l) => {
                let v = self.materialize(OperandKind::Copy(place.clone()), pat.ty, pat.span);
                let k = Operand { kind: OperandKind::Const(*l), ty: pat.ty, span: pat.span };
                self.temp_of(Rvalue::Binary(BinOp::Eq, v, k), bool_ty, pat.span)
            }
            PatKind::Tuple(ps) => {
                let tests: Vec<Operand> = ps
                    .iter()
                    .enumerate()
                    .filter_map(|(i, sp)| self.pat_test(sp, &place.with(Proj::Field(i as u32))))
                    .collect();
                self.all(tests, BinOp::And, pat.span)
            }
            PatKind::Adt { variant, fields, .. } => {
                let mut tests = Vec::new();
                let base = match variant {
                    Some(v) => {
                        let u32_ty = self.p.types.u32;
                        let d =
                            self.temp_of(Rvalue::Discriminant(place.clone()), u32_ty, pat.span)?;
                        let k = Operand {
                            kind: OperandKind::Const(Lit::Int(i128::from(*v))),
                            ty: u32_ty,
                            span: pat.span,
                        };
                        tests.push(self.temp_of(
                            Rvalue::Binary(BinOp::Eq, d, k),
                            bool_ty,
                            pat.span,
                        )?);
                        place.with(Proj::Downcast(*v))
                    }
                    None => place.clone(),
                };
                for (i, sp) in fields {
                    if let Some(t) = self.pat_test(sp, &base.with(Proj::Field(*i))) {
                        tests.push(t);
                    }
                }
                self.all(tests, BinOp::And, pat.span)
            }
            PatKind::Or(ps) => {
                let mut tests = Vec::new();
                for sp in ps {
                    // An alternative that always matches makes the whole pattern match.
                    tests.push(self.pat_test(sp, place)?);
                }
                self.all(tests, BinOp::Or, pat.span)
            }
        }
    }

    fn all(&mut self, tests: Vec<Operand>, op: BinOp, span: Span) -> Option<Operand> {
        let bool_ty = self.p.types.bool;
        let mut it = tests.into_iter();
        let first = it.next()?;
        let mut acc = first;
        for t in it {
            acc = self.temp_of(Rvalue::Binary(op, acc, t), bool_ty, span)?;
        }
        Some(acc)
    }

    /// Binds a pattern's names to the parts of `place`: a projection aliases its part, any
    /// other binding copies it.
    fn bind_pat(&mut self, pat: &thir::Pat, place: &Place, bound: &mut Vec<Local>) {
        match &pat.kind {
            PatKind::Wild | PatKind::Lit(_) | PatKind::Or(_) => {}
            PatKind::Bind(l) => {
                bound.push(*l);
                self.push(StatementKind::Live(*l), pat.span);
                match self.locals[l.index()].kind {
                    LocalKind::User(thir::LocalKind::Projection { mutable }) => self.push(
                        StatementKind::Bind { local: *l, place: place.clone(), mutable },
                        pat.span,
                    ),
                    _ => {
                        let op = Operand {
                            kind: OperandKind::Copy(place.clone()),
                            ty: pat.ty,
                            span: pat.span,
                        };
                        self.push(
                            StatementKind::Assign(Place::local(*l), Rvalue::Use(op)),
                            pat.span,
                        );
                    }
                }
            }
            PatKind::Tuple(ps) => {
                for (i, sp) in ps.iter().enumerate() {
                    self.bind_pat(sp, &place.with(Proj::Field(i as u32)), bound);
                }
            }
            PatKind::Adt { variant, fields, .. } => {
                let base = match variant {
                    Some(v) => place.with(Proj::Downcast(*v)),
                    None => place.clone(),
                };
                for (i, sp) in fields {
                    self.bind_pat(sp, &base.with(Proj::Field(*i)), bound);
                }
            }
        }
    }

    fn return_(&mut self, v: Option<&thir::Expr>, span: Span) {
        if self.fs.closure.is_none() && self.fs.ret_mode != RetMode::Owned {
            match v {
                Some(v) => self.place_tail(v),
                None => self.terminate(TerminatorKind::Unreachable, span),
            }
            return;
        }
        let want = if self.fs.closure.is_some() { Want::Move } else { Want::Return };
        let o = v.and_then(|v| self.value(v, want));
        self.terminate(TerminatorKind::Return(o), span);
    }

    /// The value of a projection-returning function (or what it `return`s): a place, chosen
    /// by control flow if need be. Each path ends returning its place.
    fn place_tail(&mut self, e: &thir::Expr) {
        match &e.kind {
            ExprKind::Block(b) => {
                let mut declared = Vec::new();
                for s in &b.stmts {
                    self.stmt(s, &mut declared);
                }
                match &b.tail {
                    Some(t) => self.place_tail(t),
                    None => self.terminate(TerminatorKind::Unreachable, b.span),
                }
            }
            ExprKind::If { cond, then, else_ } => {
                let Some(c) = self.value(cond, Want::Read) else { return };
                let (t, el, m) = (self.new_block(), self.new_block(), self.new_block());
                self.terminate(
                    TerminatorKind::If { cond: c, then: t, else_: el, merge: m },
                    e.span,
                );
                self.fs.cur = t;
                self.place_tail(&thir::Expr {
                    ty: then.ty,
                    kind: ExprKind::Block(then.clone()),
                    span: then.span,
                });
                self.fs.cur = el;
                match else_ {
                    Some(x) => self.place_tail(x),
                    None => self.terminate(TerminatorKind::Unreachable, e.span),
                }
                self.fs.cur = m;
                self.terminate(TerminatorKind::Unreachable, e.span);
            }
            ExprKind::Match { scrutinee, arms } => {
                let Some(sp) = self.scrutinee(scrutinee) else { return };
                self.arms(&sp, arms, e.span, |b, arm| {
                    b.place_tail(&arm.body);
                    false
                });
                self.terminate(TerminatorKind::Unreachable, e.span);
            }
            ExprKind::Return(Some(v)) => self.place_tail(v),
            ExprKind::Return(None) | ExprKind::Error => {
                self.terminate(TerminatorKind::Unreachable, e.span)
            }
            _ if matches!(self.p.types.kind(e.ty), TyKind::Never) => {
                // `loop {}`, `break`, ...: no place, and nothing after it.
                let _ = self.value(e, Want::Read);
                self.terminate(TerminatorKind::Unreachable, e.span);
            }
            _ => {
                let (inner, marked_mut) = match &e.kind {
                    ExprKind::MutArg(x) => (x.as_ref(), true),
                    _ => (e, false),
                };
                if self.fs.ret_mode == RetMode::Mut && !marked_mut {
                    self.diags.push(
                        Diagnostic::new(
                            codes::E0503,
                            e.span,
                            "a `mut` projection is returned as `mut place`",
                        )
                        .with_fix(
                            "add `mut`",
                            e.span.shrink_to_start(),
                            "mut ",
                        ),
                    );
                }
                if !Self::is_named_place(inner) {
                    self.diags.push(
                        Diagnostic::new(codes::E0508, inner.span, "a projection must come from a `borrow` or `mut` parameter, and this is a temporary")
                            .with_help("return an owned value instead: `-> T`"),
                    );
                    let _ = self.value(inner, Want::Read);
                    self.terminate(TerminatorKind::Unreachable, e.span);
                    return;
                }
                match self.place(inner) {
                    Some(p) => self.terminate(TerminatorKind::ReturnPlace(p), inner.span),
                    None => self.terminate(TerminatorKind::Unreachable, e.span),
                }
            }
        }
    }

    // ---- statements ------------------------------------------------------------------------

    fn stmt(&mut self, s: &thir::Stmt, declared: &mut Vec<Local>) {
        match &s.kind {
            StmtKind::Bind { pat, init } => self.bind(pat, init, s.span, declared),
            StmtKind::Assign { place, op, value } => self.assign(place, *op, value, s.span),
            StmtKind::Expr(e) => {
                if Self::is_named_place(e) && !matches!(e.kind, ExprKind::Call(_)) {
                    // A place on its own: evaluate its indices, read nothing.
                    let _ = self.place(e);
                } else {
                    let _ = self.value(e, Want::Read);
                }
            }
            StmtKind::While { cond, body } => self.loop_(Some(cond), body, s.span),
            StmtKind::Loop { body } => self.loop_(None, body, s.span),
            StmtKind::ForRange { var, start, end, inclusive, body } => {
                self.for_range(*var, start, end, *inclusive, body, s.span)
            }
            StmtKind::ForEach { var, array, mutable, body } => {
                self.for_each(*var, array, *mutable, body, s.span)
            }
        }
    }

    fn bind(&mut self, pat: &thir::Pat, init: &thir::Expr, span: Span, declared: &mut Vec<Local>) {
        let PatKind::Bind(l) = pat.kind else {
            // Destructuring: into a place, or a temporary.
            let Some(p) = self.scrutinee(init) else { return };
            self.bind_pat(pat, &p, declared);
            return;
        };
        declared.push(l);
        let kind = self.locals[l.index()].kind;
        if self.is_callable(init.ty) {
            // A closure or function bound to a name: the local holds it (and its loans).
            let rv = match &init.kind {
                ExprKind::Local(src) => Rvalue::Use(Operand {
                    kind: OperandKind::Copy(Place::local(*src)),
                    ty: init.ty,
                    span: init.span,
                }),
                _ => match self.rvalue(init) {
                    Some(rv) => rv,
                    None => return,
                },
            };
            self.push(StatementKind::Live(l), span);
            self.push(StatementKind::Assign(Place::local(l), rv), span);
            return;
        }
        match kind {
            LocalKind::User(thir::LocalKind::Projection { mutable }) => {
                let inner = match &init.kind {
                    ExprKind::MutArg(x) => x.as_ref(),
                    _ => init,
                };
                if Self::is_named_place(inner) {
                    let Some(p) = self.place(inner) else { return };
                    self.push(StatementKind::Live(l), span);
                    self.push(StatementKind::Bind { local: l, place: p, mutable }, init.span);
                    return;
                }
                if matches!(inner.kind, ExprKind::Const(_)) {
                    // A constant is a value: the projection aliases a copy of it.
                    let Some(v) = self.value(inner, Want::Read) else { return };
                    let p = self.operand_place(v);
                    self.push(StatementKind::Live(l), span);
                    self.push(StatementKind::Bind { local: l, place: p, mutable }, init.span);
                    return;
                }
                if mutable {
                    self.diags.push(
                        Diagnostic::new(
                            codes::E0512,
                            init.span,
                            "`mut x = ...` projects a place, and this is a temporary",
                        )
                        .with_help("to own a new value you can change, write `var x = ...`"),
                    );
                }
                let Some(v) = self.value(init, Want::Move) else { return };
                let p = self.operand_place(v);
                self.push(StatementKind::Live(l), span);
                self.push(StatementKind::Bind { local: l, place: p, mutable }, init.span);
            }
            LocalKind::User(thir::LocalKind::Owned { mutable: true })
                if Self::is_named_place(init) && !self.is_copy(init.ty) =>
            {
                // `var` owns its value: from a temporary, `take place`, or `.clone()`.
                let Some(p) = self.place(init) else { return };
                let what = self.describe(&p);
                self.diags.push(
                    Diagnostic::new(
                        codes::E0517,
                        init.span,
                        format!("`var` owns its value, so it can't share `{what}`"),
                    )
                    .with_help(format!(
                        "move it with `take {what}`, or copy it with `{what}.clone()`"
                    ))
                    .with_fix(
                        format!("copy it: `{what}.clone()`"),
                        init.span.shrink_to_end(),
                        ".clone()",
                    ),
                );
                let v = self.materialize(OperandKind::Copy(p), init.ty, init.span);
                self.push(StatementKind::Live(l), span);
                self.push(StatementKind::Assign(Place::local(l), Rvalue::Use(v)), span);
            }
            _ => {
                let v = self.value(init, Want::Move);
                self.push(StatementKind::Live(l), span);
                if let Some(v) = v {
                    self.push(StatementKind::Assign(Place::local(l), Rvalue::Use(v)), span);
                }
            }
        }
    }

    fn assign(&mut self, place: &thir::Expr, op: Option<BinOp>, value: &thir::Expr, span: Span) {
        // The value first, then the place's indices, then the old value for `op=`.
        let v = self.value(value, if op.is_some() { Want::Read } else { Want::Move });
        let Some(p) = self.place(place) else { return };
        let Some(v) = v else { return };
        let v = match op {
            None => v,
            Some(op) => {
                let old = self.materialize(OperandKind::Copy(p.clone()), place.ty, place.span);
                match self.temp_of(Rvalue::Binary(op, old, v), place.ty, span) {
                    Some(v) => v,
                    None => return,
                }
            }
        };
        self.push(StatementKind::Assign(p, Rvalue::Use(v)), place.span);
    }

    /// Starts a loop at the current block; returns (loop block, body, continuing, exit), with
    /// `cur` at the body.
    fn begin_loop(&mut self, span: Span) -> (BlockId, BlockId, BlockId, BlockId) {
        let lp = self.fs.cur;
        let (body, cont, exit) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(TerminatorKind::Loop { body, continuing: cont, merge: exit }, span);
        self.fs.loops.push(lp);
        self.fs.cur = body;
        (lp, body, cont, exit)
    }

    /// Ends a loop's body (on to the continuing block), runs `continuing` there, then goes on
    /// after the loop.
    fn end_loop(
        &mut self,
        lp: BlockId,
        cont: BlockId,
        exit: BlockId,
        span: Span,
        continuing: impl FnOnce(&mut Self),
    ) {
        self.goto(cont, span);
        self.fs.cur = cont;
        continuing(self);
        self.terminate(TerminatorKind::LoopBack(lp), span);
        self.fs.loops.pop();
        self.fs.cur = exit;
    }

    /// `if cond { break }`.
    fn break_if(&mut self, cond: Operand, lp: BlockId, span: Span) {
        let (t, e, m) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(TerminatorKind::If { cond, then: t, else_: e, merge: m }, span);
        self.fs.cur = t;
        self.terminate(TerminatorKind::Break(lp), span);
        self.fs.cur = e;
        self.goto(m, span);
        self.fs.cur = m;
    }

    fn loop_(&mut self, cond: Option<&thir::Expr>, body: &thir::Block, span: Span) {
        let (lp, _, cont, exit) = self.begin_loop(span);
        if let Some(c) = cond {
            let bool_ty = self.p.types.bool;
            if let Some(cv) = self.value(c, Want::Read)
                && let Some(nc) = self.temp_of(Rvalue::Unary(UnOp::Not, cv), bool_ty, c.span)
            {
                self.break_if(nc, lp, c.span);
            }
        }
        let _ = self.block(body, Want::Read);
        self.end_loop(lp, cont, exit, span, |_| {});
    }

    fn for_range(
        &mut self,
        var: Local,
        start: &thir::Expr,
        end: &thir::Expr,
        inclusive: bool,
        body: &thir::Block,
        span: Span,
    ) {
        let Some(s) = self.value(start, Want::Read) else { return };
        let Some(e) = self.value(end, Want::Read) else { return };
        let ty = start.ty;
        let bool_ty = self.p.types.bool;
        let counter = self.temp(ty, LocalKind::TempVar, "counter", span);
        let cplace = Place::local(counter);
        self.push(StatementKind::Assign(cplace.clone(), Rvalue::Use(s.clone())), span);
        let copy = |b: &mut Self| b.materialize(OperandKind::Copy(cplace.clone()), ty, span);
        let one = Operand { kind: OperandKind::Const(Lit::Int(1)), ty, span };
        // `a..=b` stops after `b` without computing `b + 1`, which could overflow.
        let done = inclusive.then(|| {
            let d = self.temp(bool_ty, LocalKind::TempVar, "done", span);
            if let Some(gt) = self.temp_of(Rvalue::Binary(BinOp::Gt, s, e.clone()), bool_ty, span) {
                self.push(StatementKind::Assign(Place::local(d), Rvalue::Use(gt)), span);
            }
            d
        });
        let (lp, _, cont, exit) = self.begin_loop(span);
        let stop = match done {
            Some(d) => Some(self.materialize(OperandKind::Copy(Place::local(d)), bool_ty, span)),
            None => {
                let c = copy(self);
                self.temp_of(Rvalue::Binary(BinOp::Ge, c, e.clone()), bool_ty, span)
            }
        };
        if let Some(stop) = stop {
            self.break_if(stop, lp, span);
        }
        self.push(StatementKind::Live(var), span);
        let c = copy(self);
        self.push(StatementKind::Assign(Place::local(var), Rvalue::Use(c)), span);
        let _ = self.block(body, Want::Read);
        self.push(StatementKind::Dead(var), span);
        self.end_loop(lp, cont, exit, span, |b| {
            let c = copy(b);
            match done {
                None => {
                    if let Some(n) = b.temp_of(Rvalue::Binary(BinOp::Add, c, one), ty, span) {
                        b.push(StatementKind::Assign(cplace.clone(), Rvalue::Use(n)), span);
                    }
                }
                Some(d) => {
                    let Some(last) =
                        b.temp_of(Rvalue::Binary(BinOp::Eq, c.clone(), e), bool_ty, span)
                    else {
                        return;
                    };
                    let (t, el, m) = (b.new_block(), b.new_block(), b.new_block());
                    b.terminate(
                        TerminatorKind::If { cond: last, then: t, else_: el, merge: m },
                        span,
                    );
                    b.fs.cur = t;
                    let yes =
                        Operand { kind: OperandKind::Const(Lit::Bool(true)), ty: bool_ty, span };
                    b.push(StatementKind::Assign(Place::local(d), Rvalue::Use(yes)), span);
                    b.goto(m, span);
                    b.fs.cur = el;
                    if let Some(n) = b.temp_of(Rvalue::Binary(BinOp::Add, c, one), ty, span) {
                        b.push(StatementKind::Assign(cplace.clone(), Rvalue::Use(n)), span);
                    }
                    b.goto(m, span);
                    b.fs.cur = m;
                }
            }
        });
    }

    fn for_each(
        &mut self,
        var: Local,
        array: &thir::Expr,
        mutable: bool,
        body: &thir::Block,
        span: Span,
    ) {
        let Some(ap) = self.scrutinee(array) else { return };
        let u32_ty = self.p.types.u32;
        let bool_ty = self.p.types.bool;
        let projection = self.locals[var.index()].kind.is_projection();
        // A projecting loop holds the array for the whole loop: through `walked`, which every
        // pass uses (§6.5). Diagnostics call it by the loop variable's name.
        let walked = if projection {
            let name = self.locals[var.index()].name.clone();
            let w = self.temp(array.ty, LocalKind::TempProjection { mutable }, name, array.span);
            self.push(StatementKind::Bind { local: w, place: ap, mutable }, array.span);
            Place::local(w)
        } else {
            ap
        };
        let len = match self.p.types.kind(array.ty) {
            TyKind::Array(_, n) => Operand {
                kind: OperandKind::Const(Lit::Int(i128::from(*n))),
                ty: u32_ty,
                span: array.span,
            },
            _ => match self.temp_of(Rvalue::Len(walked.clone()), u32_ty, array.span) {
                Some(v) => v,
                None => return,
            },
        };
        let index = self.temp(u32_ty, LocalKind::TempVar, "index", span);
        let zero = Operand { kind: OperandKind::Const(Lit::Int(0)), ty: u32_ty, span };
        self.push(StatementKind::Assign(Place::local(index), Rvalue::Use(zero)), span);
        let (lp, _, cont, exit) = self.begin_loop(span);
        let i = self.materialize(OperandKind::Copy(Place::local(index)), u32_ty, span);
        let Some(stop) = self.temp_of(Rvalue::Binary(BinOp::Ge, i.clone(), len), bool_ty, span)
        else {
            return;
        };
        self.break_if(stop, lp, span);
        let elem = walked.with(Proj::Index(self.operand_local(i.clone())));
        self.push(StatementKind::Live(var), span);
        if projection {
            self.push(StatementKind::Bind { local: var, place: elem, mutable }, span);
        } else {
            let et = self.locals[var.index()].ty;
            let v = self.materialize(OperandKind::Copy(elem), et, span);
            self.push(StatementKind::Assign(Place::local(var), Rvalue::Use(v)), span);
        }
        let _ = self.block(body, Want::Read);
        self.push(StatementKind::Dead(var), span);
        self.end_loop(lp, cont, exit, span, |b| {
            let i = b.materialize(OperandKind::Copy(Place::local(index)), u32_ty, span);
            let one = Operand { kind: OperandKind::Const(Lit::Int(1)), ty: u32_ty, span };
            if let Some(n) = b.temp_of(Rvalue::Binary(BinOp::Add, i, one), u32_ty, span) {
                b.push(StatementKind::Assign(Place::local(index), Rvalue::Use(n)), span);
            }
        });
    }
}
