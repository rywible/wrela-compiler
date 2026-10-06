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
//! named place needs `take` (E0501, E0502), `var` can't share a place (E0517), `mut` needs a
//! place, not a temporary, a constant or a closure (E0512), and a `mut` projection is returned
//! as `mut place` (E0503). The rest, which depend on control flow, are the borrow checker's.
//!
//! **Drops** (§6.1) are placed here too. A value whose type may need dropping is dropped where
//! its owner's scope ends, in reverse order: a local at the end of its block (and on every
//! `return`, `break` or `continue` that leaves the block), a temporary at the end of its
//! statement. A temporary whose value something consumes (a field of a new value, a `take`
//! argument, a binding, an assignment, a return) is passed on whole ([`MoveKind::Temp`]) and
//! not dropped. Moving out of a place leaves zeros there, which drop as nothing, so a value
//! moved on some paths needs no flag: its drop finds zeros.

use super::*;
use crate::defs::{Lang, Mode, RetMode};
use crate::program::Program;
use crate::thir::{self, ExprKind, PatKind, StmtKind};
use crate::traits;
use std::collections::{BTreeMap, HashMap};
use wrela_diag::{Diagnostic, codes};

/// The checked constants: their types and values.
pub type Consts = BTreeMap<ConstId, (TyId, thir::Expr)>;

/// A constant with more numbers than this is a table ([`Builder::is_table`]).
const TABLE_SCALARS: u64 = 16;

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
            shorthand: d.shorthand,
            closure: d.closure,
        })
        .collect();
    let mut b = Builder {
        p,
        consts,
        locals,
        diags: Vec::new(),
        fs: FnState::new(def.ret_mode, None, body.value.span),
        chained: None,
        discarded: Vec::new(),
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
    let discarded = std::mem::take(&mut b.discarded);
    let mir = Body { locals: b.locals, fns, closures, hidden_ret: body.hidden_ret, discarded };
    (mir, b.diags)
}

/// Whether a constant's value is literal all the way down: literals, text, and struct, enum,
/// tuple, array and vector literals of them, and other such constants. Lowering folds it; any
/// other constant is computed by the build (§10).
pub fn is_literal(consts: &Consts, e: &thir::Expr) -> bool {
    match &e.kind {
        ExprKind::Lit(_) | ExprKind::Text(_) | ExprKind::Embed(_) => true,
        ExprKind::Unary(UnOp::Neg, x) => matches!(x.kind, ExprKind::Lit(_)),
        ExprKind::Const(c) => consts.get(c).is_some_and(|(_, v)| is_literal(consts, v)),
        ExprKind::Adt { fields, base: None, .. } => fields.iter().all(|f| is_literal(consts, f)),
        ExprKind::Tuple(xs) | ExprKind::Array(xs) | ExprKind::Construct(xs) => {
            xs.iter().all(|x| is_literal(consts, x))
        }
        ExprKind::ArrayRepeat(x, _) => is_literal(consts, x),
        _ => false,
    }
}

fn constant(l: Lit, ty: TyId, span: Span) -> Operand {
    Operand { kind: OperandKind::Const(l), ty, span }
}

/// A copy of `l`'s value.
fn copy_of(l: Local, ty: TyId, span: Span) -> Operand {
    Operand { kind: OperandKind::Copy(Place::local(l)), ty, span }
}

/// The function being built.
struct FnState {
    blocks: Vec<BlockData>,
    cur: BlockId,
    /// Enclosing loops' `Loop` blocks, innermost last.
    loops: Vec<BlockId>,
    /// The scopes that own values to drop, innermost last.
    scopes: Vec<Scope>,
    ret_mode: RetMode,
    closure: Option<ClosureId>,
    span: Span,
    /// The local that names each constant this code uses (`LocalKind::Const`).
    consts: HashMap<ConstId, Local>,
}

impl FnState {
    fn new(ret_mode: RetMode, closure: Option<ClosureId>, span: Span) -> FnState {
        FnState {
            blocks: Vec::new(),
            cur: BlockId(0),
            loops: Vec::new(),
            scopes: Vec::new(),
            ret_mode,
            closure,
            span,
            consts: HashMap::new(),
        }
    }
}

/// A scope that owns values, dropped when it ends.
struct Scope {
    kind: ScopeKind,
    /// The locals and temporaries it owns whose types may need dropping, in the order they got
    /// their values.
    owned: Vec<Local>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    /// A block (or a function's body, or a `match` arm): its locals.
    Block,
    /// A statement, or a condition: its temporaries.
    Temps,
    /// A loop, by its `Loop` block: `break` and `continue` drop what's inside it.
    Loop(BlockId),
}

struct Builder<'a> {
    p: &'a Program,
    consts: &'a Consts,
    locals: Vec<LocalDecl>,
    diags: Vec<Diagnostic>,
    fs: FnState,
    /// A call that is the base of a field, index or swizzle, or another call's receiver: a
    /// `take` written before it needs parentheses (`(take b.finish()).n`).
    chained: Option<Span>,
    /// Calls whose values statements discard ([`Body::discarded`]).
    discarded: Vec<(Callee, bool, TyId, Span)>,
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
        self.open_scope(ScopeKind::Block);
        // A `take` parameter is the function's to drop.
        for &l in params {
            if self.locals[l.index()].kind == LocalKind::User(thir::LocalKind::Param(Mode::Take)) {
                self.own_local(l);
            }
        }
        if ret_mode == RetMode::Owned {
            let want = if closure.is_some() { Want::Move } else { Want::Return };
            let v = self.value(value, want).map(|v| self.consume(v));
            self.drop_all();
            self.terminate(TerminatorKind::Return(v), value.span);
        } else {
            self.place_tail(value);
        }
        self.fs.scopes.clear();
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

    /// `l = v`.
    fn set(&mut self, l: Local, v: Operand, span: Span) {
        self.push(StatementKind::Assign(Place::local(l), Rvalue::Use(v)), span);
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

    /// `if cond { then } else { else_ }`, each branch going on to the block where they join;
    /// `cur` is that block after.
    fn diamond(
        &mut self,
        cond: Operand,
        span: Span,
        then: impl FnOnce(&mut Self),
        else_: impl FnOnce(&mut Self),
    ) {
        let (t, el, m) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(TerminatorKind::If { cond, then: t, else_: el, merge: m }, span);
        self.fs.cur = t;
        then(self);
        self.goto(m, span);
        self.fs.cur = el;
        else_(self);
        self.goto(m, span);
        self.fs.cur = m;
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
            shorthand: false,
            closure: self.fs.closure,
        });
        id
    }

    fn is_copy(&self, t: TyId) -> bool {
        traits::implements_builtin(self.p, t, Lang::Copy)
    }

    /// Whether a value of this type is nothing at run time: `()` or `!`.
    fn is_unit(&self, t: TyId) -> bool {
        self.p.types.is_unit(t) || matches!(self.p.types.kind(t), TyKind::Never | TyKind::Error)
    }

    /// Whether `l` is outside the closure being built (a capture).
    fn is_capture(&self, l: Local) -> bool {
        is_capture(&self.locals, self.fs.closure, l)
    }

    // ---- drops -----------------------------------------------------------------------------

    /// Whether dropping a value of type `t` may do something.
    fn needs_drop(&self, t: TyId) -> bool {
        traits::may_need_drop(self.p, t)
    }

    fn open_scope(&mut self, kind: ScopeKind) {
        self.fs.scopes.push(Scope { kind, owned: Vec::new() });
    }

    /// Ends the innermost scope: drops what it owns, in reverse order. A block's temporaries
    /// (its value's) go on to the scope around it, which drops them with its own.
    fn close_scope(&mut self) {
        let Some(scope) = self.fs.scopes.pop() else { return };
        if scope.kind == ScopeKind::Block {
            let (locals, temps): (Vec<Local>, Vec<Local>) = scope
                .owned
                .into_iter()
                .partition(|&l| matches!(self.locals[l.index()].kind, LocalKind::User(_)));
            self.drop_locals(locals.into_iter().rev());
            if let Some(outer) = self.fs.scopes.last_mut() {
                outer.owned.extend(temps);
            }
            return;
        }
        self.drop_locals(scope.owned.into_iter().rev());
    }

    /// Drops each of `locals`, in order, each at where its value was bound.
    fn drop_locals(&mut self, locals: impl IntoIterator<Item = Local>) {
        for l in locals {
            let at = self.locals[l.index()].span;
            self.push(StatementKind::Drop(Place::local(l)), at);
        }
    }

    /// Drops what the scopes from the innermost out to `until` (not included) own, leaving
    /// them open: for a `break` or `continue`, or with `None`, a `return`.
    fn drop_until(&mut self, until: Option<usize>) {
        let stop = until.map_or(0, |i| i + 1);
        let mut drops = Vec::new();
        for scope in self.fs.scopes[stop..].iter().rev() {
            drops.extend(scope.owned.iter().rev().copied());
        }
        self.drop_locals(drops);
    }

    /// Drops everything the function owns: before it returns.
    fn drop_all(&mut self) {
        self.drop_until(None);
    }

    /// `l`, an owned local, holds a value its block drops.
    fn own_local(&mut self, l: Local) {
        if !self.needs_drop(self.locals[l.index()].ty) {
            return;
        }
        if let Some(s) = self.fs.scopes.iter_mut().rev().find(|s| s.kind == ScopeKind::Block) {
            s.owned.push(l);
        }
    }

    /// `t`, a temporary, holds a value its statement drops unless something consumes it.
    fn own_temp(&mut self, t: Local) {
        if !self.needs_drop(self.locals[t.index()].ty) {
            return;
        }
        if let Some(s) = self.fs.scopes.last_mut() {
            s.owned.push(t);
        }
    }

    /// Whether temporary `l` owns a value that will be dropped.
    fn owning_temp(&self, l: Local) -> bool {
        !matches!(self.locals[l.index()].kind, LocalKind::User(_))
            && self.fs.scopes.iter().any(|s| s.owned.contains(&l))
    }

    /// An operand whose value is consumed: a temporary's is passed on to its new owner
    /// ([`MoveKind::Temp`]), and its scope no longer drops it. (Part of a temporary is moved
    /// out of it, which leaves zeros there.)
    fn consume(&mut self, op: Operand) -> Operand {
        let OperandKind::Copy(p) = &op.kind else { return op };
        if !self.needs_drop(op.ty) || !self.owning_temp(p.local) {
            return op;
        }
        if p.proj.is_empty() {
            for s in &mut self.fs.scopes {
                s.owned.retain(|&x| x != p.local);
            }
        }
        Operand { kind: OperandKind::Move(p.clone(), MoveKind::Temp), ..op }
    }

    /// After a pattern bound owned names to parts of a temporary (`match make() { Some(v) =>
    /// .. }`), those parts are the bindings' now: they're moved out of the temporary, which
    /// leaves zeros there for its drop. Done once the arm is chosen, after its guard.
    fn commit_moves(&mut self, pat: &thir::Pat, place: &Place) {
        if !self.owning_temp(place.local) {
            return;
        }
        match &pat.kind {
            PatKind::Bind(l) => {
                if matches!(
                    self.locals[l.index()].kind,
                    LocalKind::User(thir::LocalKind::Owned { .. })
                ) && self.needs_drop(pat.ty)
                {
                    let op = Operand {
                        kind: OperandKind::Move(place.clone(), MoveKind::Temp),
                        ty: pat.ty,
                        span: pat.span,
                    };
                    self.push(StatementKind::Eval(Rvalue::Use(op)), pat.span);
                    self.own_local(*l);
                }
            }
            PatKind::Tuple(ps) => {
                for (i, sp) in ps.iter().enumerate() {
                    self.commit_moves(sp, &place.with(Proj::Field(i as u32)));
                }
            }
            PatKind::Adt { variant, fields, .. } => {
                let base = match variant {
                    Some(v) => place.with(Proj::Downcast(*v)),
                    None => place.clone(),
                };
                for (i, sp) in fields {
                    self.commit_moves(sp, &base.with(Proj::Field(*i)));
                }
            }
            PatKind::Wild | PatKind::Lit(_) | PatKind::Text(_) | PatKind::Or(_) => {}
        }
    }

    fn describe(&self, place: &Place) -> String {
        describe(self.p, &self.locals, place)
    }

    /// `t = value`, returning a copy of `t`. `None` for a value of `!` (or after an error).
    fn temp_of(&mut self, rv: Rvalue, ty: TyId, span: Span) -> Option<Operand> {
        if matches!(self.p.types.kind(ty), TyKind::Never | TyKind::Error) {
            self.push(StatementKind::Eval(rv), span);
            return None;
        }
        // A call's result is named after the call: a borrow struct's loans are reported by it.
        let name = match &rv {
            Rvalue::Call(Call {
                callee: Callee::Fn { func, .. } | Callee::TraitMethod { method: func, .. },
                ..
            }) => format!("{}(..)", self.p.func(*func).name),
            _ => "temporary".into(),
        };
        let t = self.temp(ty, LocalKind::Temp, name, span);
        let owns = match &rv {
            Rvalue::Call(c) => c.ret_mode == RetMode::Owned,
            Rvalue::Adt { .. } | Rvalue::Tuple(_) | Rvalue::Array(_) | Rvalue::ArrayRepeat(..) => {
                true
            }
            Rvalue::Use(o) => matches!(o.kind, OperandKind::Move(..)),
            _ => false,
        };
        self.push(StatementKind::Assign(Place::local(t), rv), span);
        if owns {
            self.own_temp(t);
        }
        Some(copy_of(t, ty, span))
    }

    /// An operand read where it's evaluated: copied into a temporary now. A move's temporary
    /// owns what it moved.
    fn materialize(&mut self, kind: OperandKind, ty: TyId, span: Span) -> Operand {
        let t = self.temp(ty, LocalKind::Temp, "temporary", span);
        let moved = matches!(kind, OperandKind::Move(..));
        self.set(t, Operand { kind, ty, span }, span);
        if moved {
            self.own_temp(t);
        }
        copy_of(t, ty, span)
    }

    /// A local holding an operand's value (for an index).
    fn operand_local(&mut self, op: Operand) -> Local {
        if let OperandKind::Copy(p) = &op.kind
            && p.proj.is_empty()
            && self.locals[p.local.index()].kind == LocalKind::Temp
        {
            return p.local;
        }
        let span = op.span;
        let t = self.temp(op.ty, LocalKind::Temp, "index", span);
        self.set(t, op, span);
        t
    }

    /// A place holding an operand's value: the temporary's own, if it's one that owns.
    fn operand_place(&mut self, op: Operand) -> Place {
        if let OperandKind::Copy(p) = &op.kind
            && p.proj.is_empty()
            && self.owning_temp(p.local)
        {
            return p.clone();
        }
        Place::local(self.operand_local(op))
    }

    // ---- expressions -----------------------------------------------------------------------

    /// Whether `e` names a place the memory rules see: rooted at a local, or what a
    /// projection-returning call returned.
    fn is_named_place(&self, e: &thir::Expr) -> bool {
        match &e.place_root().kind {
            // A constant is a place that lives as long as the program (§10).
            ExprKind::Local(_) | ExprKind::Const(_) => true,
            ExprKind::Call(c) => c.ret_mode != RetMode::Owned,
            _ => false,
        }
    }

    /// Evaluates `e` for its value. `None` when it has none: `!`, or after an error. A `()` is
    /// a value too (nothing at run time), so a tuple, struct or call it's part of is whole.
    fn value(&mut self, e: &thir::Expr, want: Want) -> Option<Operand> {
        let v = self.eval(e, want);
        if v.is_none() && self.p.types.is_unit(e.ty) {
            return self.temp_of(Rvalue::Tuple(Vec::new()), e.ty, e.span);
        }
        v
    }

    /// [`Self::value`], except that a `()` may have no operand.
    fn eval(&mut self, e: &thir::Expr, want: Want) -> Option<Operand> {
        let span = e.span;
        match &e.kind {
            ExprKind::Lit(l) => Some(constant(*l, e.ty, span)),
            ExprKind::Text(t) => self.temp_of(Rvalue::Text(t.clone()), e.ty, span),
            ExprKind::Embed(path) => self.temp_of(Rvalue::Embed(path.clone()), e.ty, span),
            ExprKind::ConstParam(g) => self.temp_of(Rvalue::ConstParam(*g), e.ty, span),
            ExprKind::Discriminant(x) => {
                let p = self.scrutinee(x)?;
                self.temp_of(Rvalue::Discriminant(p), e.ty, span)
            }
            ExprKind::Unary(UnOp::Neg, x)
                if matches!(x.kind, ExprKind::Lit(Lit::Int(_) | Lit::Float(..))) =>
            {
                // A negative literal is one constant: `-2147483648` is an i32 as written, not
                // the negation of a number too large for one.
                let l = match x.kind {
                    ExprKind::Lit(Lit::Int(v)) if self.p.types.is_int(e.ty) => Lit::Int(-v),
                    ExprKind::Lit(Lit::Int(v)) => Lit::Float(-(v as f64), -(v as f32)),
                    ExprKind::Lit(Lit::Float(d, f)) => Lit::Float(-d, -f),
                    _ => unreachable!("matched above"),
                };
                Some(constant(l, e.ty, span))
            }
            ExprKind::Local(_)
            | ExprKind::Field(..)
            | ExprKind::Swizzle(..)
            | ExprKind::Index(..)
                if self.is_named_place(e) =>
            {
                self.place_value(e, want)
            }
            ExprKind::Field(..) | ExprKind::Swizzle(..) | ExprKind::Index(..) => {
                // Part of a temporary: moved out of it if it's consumed.
                let p = self.place(e)?;
                let op = Operand { kind: OperandKind::Copy(p), ty: e.ty, span };
                let op = if want == Want::Read { op } else { self.consume(op) };
                Some(self.materialize(op.kind, e.ty, span))
            }
            ExprKind::Const(c) => {
                let (_, v) = self.consts.get(c)?;
                if !self.inlined(*c) {
                    return self.place_value(e, want);
                }
                // A lifted package's table is built here from its literals, so each is lifted.
                let lifted = self.p.package_of(self.p.const_(*c).module).lifted;
                if self.is_table(v) && !lifted {
                    return self.temp_of(Rvalue::Const(*c), e.ty, span);
                }
                self.value(v, Want::Read)
            }
            ExprKind::Take(inner) => self.take(inner),
            ExprKind::MutArg(inner) => self.value(inner, want),
            ExprKind::Block(b) => self.block(b, want),
            ExprKind::If { cond, then, else_ } => self.if_(cond, then, else_.as_deref(), e, want),
            ExprKind::Match { scrutinee, arms, mutable } => {
                self.match_(scrutinee, arms, *mutable, e, want)
            }
            ExprKind::Binary(op @ (BinOp::And | BinOp::Or), a, b) => {
                self.short_circuit(*op, a, b, e)
            }
            ExprKind::Return(v) => {
                self.return_(v.as_deref(), span);
                None
            }
            ExprKind::Break => {
                if let Some(&l) = self.fs.loops.last() {
                    self.drop_loop(l);
                    self.terminate(TerminatorKind::Break(l), span);
                }
                None
            }
            ExprKind::Continue => {
                if let Some(&l) = self.fs.loops.last() {
                    self.drop_loop(l);
                    self.terminate(TerminatorKind::Continue(l), span);
                }
                None
            }
            ExprKind::Call(c) => self.call_value(c, e, false, want),
            ExprKind::Error | ExprKind::FromBase => None,
            _ => {
                let rv = self.rvalue(e, want)?;
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
                let handle = self.arena_handle(e);
                self.unmarked_move(&place, e.ty, e.span, handle.as_deref());
                OperandKind::Move(place, MoveKind::Unmarked)
            }
        };
        Some(self.materialize(kind, e.ty, e.span))
    }

    /// Whether the function owns `l` outright (so it can move out of it).
    fn owned(&self, l: Local) -> bool {
        self.locals[l.index()].kind.owns_value() && !self.is_capture(l)
    }

    /// E0501 or E0502: a non-`Copy` named place, of type `ty`, used at `span` where its value is
    /// consumed, unmarked.
    /// With `handle`, the place is an arena's element, `arena[handle]`.
    fn unmarked_move(&mut self, place: &Place, ty: TyId, span: Span, handle: Option<&str>) {
        let arena = handle.map(|h| {
            let mut base = place.clone();
            base.proj.pop();
            (self.describe(&base), h)
        });
        let what = match &arena {
            Some((a, h)) => format!("{a}[{h}]"),
            None => self.describe(place),
        };
        let shown = self.p.display_ty(ty);
        let indexed = place.proj.iter().any(|p| matches!(p, Proj::Index(_)));
        let d = if self.owned(place.local) && !indexed {
            Diagnostic::new(
                codes::E0501,
                span,
                format!("moving `{what}` out of a named place is written `take {what}`"),
            )
            .with_note(format!("`{shown}` isn't `Copy`, so using it here moves it (§6.1)"))
            .with_fix("move it: `take`", span.shrink_to_start(), "take ")
        } else {
            let why = match &arena {
                Some(_) => "it's an element of an arena".to_string(),
                None if indexed => "it's an element of an array".to_string(),
                None => self.why_not_owned(place.local),
            };
            let mut d =
                Diagnostic::new(codes::E0502, span, format!("can't move `{what}` here: {why}"))
                    .with_note(format!("`{shown}` isn't `Copy`, so using it here would move it"));
            if let Some((a, h)) = &arena {
                d = d.with_help(format!("`{a}.remove({h})` takes it out of the arena"));
            }
            if traits::implements_builtin(self.p, ty, Lang::Clone) {
                d.with_fix("copy it: `.clone()`", span.shrink_to_end(), ".clone()")
            } else {
                d
            }
        };
        self.diags.push(d);
    }

    /// For `arena[h]`, an element of an arena: how its handle is shown.
    fn arena_handle(&self, e: &thir::Expr) -> Option<String> {
        let ExprKind::Index(base, index) = &e.kind else { return None };
        if self.p.lang_of_ty(base.ty) != Some(Lang::Arena) {
            return None;
        }
        Some(match &index.kind {
            ExprKind::Local(l) => self.locals[l.index()].name.clone(),
            _ => "h".into(),
        })
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
            LocalKind::Const(_) => {
                format!("`{}` is a constant, which lives as long as the program", d.name)
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
        if !self.is_named_place(inner) {
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
            ExprKind::Const(c) => Some(Place::local(self.const_local(*c, e.ty, e.span))),
            ExprKind::Field(b, i) => {
                let p = self.chained_place(b)?;
                Some(p.with(Proj::Field(*i)))
            }
            ExprKind::Swizzle(b, comps) => {
                let p = self.chained_place(b)?;
                Some(self.swizzle(p, comps))
            }
            ExprKind::Index(b, i) => {
                let p = self.chained_place(b)?;
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

    /// [`Self::place`] of what a field, index or swizzle is taken of.
    fn chained_place(&mut self, b: &thir::Expr) -> Option<Place> {
        self.chained = matches!(b.kind, ExprKind::Call(_)).then_some(b.span);
        self.place(b)
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

    /// An rvalue's parts. A value being returned returns its parts: `Ok(v)` moves a local
    /// `v` out without `take`, as returning `v` would (§6.1).
    fn rvalue(&mut self, e: &thir::Expr, want: Want) -> Option<Rvalue> {
        let part = if want == Want::Return { Want::Return } else { Want::Move };
        Some(match &e.kind {
            ExprKind::Unary(op, x) => Rvalue::Unary(*op, self.value(x, Want::Read)?),
            ExprKind::Binary(op, a, b) => {
                let a = self.value(a, Want::Read)?;
                let b = self.value(b, Want::Read)?;
                Rvalue::Binary(*op, a, b)
            }
            ExprKind::Adt { adt, args, variant: None, fields, order, base: None }
                if self.p.adt(*adt).borrow =>
            {
                // Each field's place where it's written, or its value.
                let decls = self.p.adt_fields(*adt, None);
                let mut out: Vec<Option<Arg>> = vec![None; fields.len()];
                for &i in order {
                    let f = &fields[i as usize];
                    let d = &decls[i as usize];
                    // The declared type decides: a run field may be given an array.
                    let projected = d.mode != RetMode::Owned
                        || matches!(self.p.types.kind(d.ty), TyKind::Slice(_) | TyKind::Str)
                        || self.p.is_borrow_struct(d.ty);
                    let arg = if !projected {
                        Arg::Take(self.value(f, Want::Read)?)
                    } else {
                        let inner = match &f.kind {
                            ExprKind::MutArg(x) => x.as_ref(),
                            _ => f,
                        };
                        let p = self.place(inner)?;
                        self.check_indices(&p, inner.span);
                        if d.mode == RetMode::Mut {
                            Arg::Mut(p, f.span)
                        } else {
                            Arg::Borrow(p, f.span)
                        }
                    };
                    out[i as usize] = Some(arg);
                }
                let fields = out.into_iter().collect::<Option<Vec<Arg>>>()?;
                Rvalue::BorrowStruct { adt: *adt, args: args.clone(), fields }
            }
            ExprKind::Adt { adt, args, variant, fields, order, base } => {
                let mut vals: Vec<Option<Operand>> = vec![None; fields.len()];
                for &i in order {
                    let v = self.value(&fields[i as usize], part)?;
                    vals[i as usize] = Some(self.consume(v));
                }
                let base_place = match base {
                    Some(b) => Some((self.scrutinee(b)?, self.is_named_place(b))),
                    None => None,
                };
                let mut out = Vec::new();
                for (i, (v, f)) in vals.into_iter().zip(fields).enumerate() {
                    match (v, &f.kind, &base_place) {
                        (Some(v), ..) => out.push(v),
                        (None, ExprKind::FromBase, Some((bp, named))) => {
                            let fp = bp.with(Proj::Field(i as u32));
                            let kind = if *named && !self.is_copy(f.ty) {
                                self.unmarked_move(&fp, f.ty, f.span, None);
                                OperandKind::Move(fp, MoveKind::Unmarked)
                            } else {
                                OperandKind::Copy(fp)
                            };
                            out.push(self.materialize(kind, f.ty, f.span));
                        }
                        (None, ..) => {
                            let v = self.value(f, part)?;
                            out.push(self.consume(v));
                        }
                    }
                }
                Rvalue::Adt { adt: *adt, args: args.clone(), variant: *variant, fields: out }
            }
            ExprKind::Tuple(xs) => {
                let vs = self.values(xs, part)?;
                Rvalue::Tuple(vs.into_iter().map(|v| self.consume(v)).collect())
            }
            ExprKind::Array(xs) => {
                let vs = self.values(xs, part)?;
                Rvalue::Array(vs.into_iter().map(|v| self.consume(v)).collect())
            }
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

    /// Whether a use of constant `c`'s whole value is its literal value, built where it's used
    /// (or, for a table, read in place): a literal value of a `Copy` type. Any other constant
    /// is read as a place.
    fn inlined(&self, c: ConstId) -> bool {
        self.consts.get(&c).is_some_and(|(t, v)| is_literal(self.consts, v) && self.is_copy(*t))
    }

    /// The local naming constant `c` in the code being built.
    fn const_local(&mut self, c: ConstId, ty: TyId, span: Span) -> Local {
        if let Some(&l) = self.fs.consts.get(&c) {
            return l;
        }
        let name = self.p.const_(c).name.clone();
        let l = self.temp(ty, LocalKind::Const(c), name, span);
        self.fs.consts.insert(c, l);
        l
    }

    /// Whether a constant's value is a table: more than [`TABLE_SCALARS`] numbers, in parts
    /// that are all literal values (no conversions). It's kept in one place, not built at each
    /// use.
    fn is_table(&self, v: &thir::Expr) -> bool {
        self.scalars(v.ty) > TABLE_SCALARS && is_literal(self.consts, v)
    }

    /// How many numbers a value of type `t` holds (an enum: its tag and largest payload).
    fn scalars(&self, t: TyId) -> u64 {
        let p = self.p;
        let sum = |ts: &mut dyn Iterator<Item = TyId>| {
            ts.map(|t| self.scalars(t)).fold(0u64, u64::saturating_add)
        };
        match p.types.kind(t) {
            TyKind::Vec(n) => u64::from(*n),
            TyKind::Mat(n) => u64::from(*n) * u64::from(*n),
            TyKind::Array(e, n) => self.scalars(*e).saturating_mul(u64::from(*n)),
            TyKind::Tuple(ts) => sum(&mut ts.iter().copied()),
            TyKind::Adt(a, args) if p.adt(*a).is_enum() => {
                let n = p.adt(*a).variants().len() as u32;
                let payload =
                    (0..n).map(|v| sum(&mut p.fields_of(*a, args, Some(v)).into_iter())).max();
                1 + payload.unwrap_or(0)
            }
            TyKind::Adt(a, args) => sum(&mut p.fields_of(*a, args, None).into_iter()),
            _ => 1,
        }
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
        self.set(r, av.clone(), e.span);
        // `a && b` evaluates `b` when `a`; `a || b` when not `a`.
        let cond = match op {
            BinOp::And => av,
            _ => self.temp_of(Rvalue::Unary(UnOp::Not, av), e.ty, e.span)?,
        };
        self.diamond(
            cond,
            e.span,
            |this| {
                // `b`'s temporaries end in the branch that makes them.
                this.open_scope(ScopeKind::Temps);
                if let Some(bv) = this.value(b, Want::Read) {
                    this.set(r, bv, e.span);
                }
                this.close_scope();
            },
            |_| {},
        );
        Some(copy_of(r, e.ty, e.span))
    }

    // ---- calls -----------------------------------------------------------------------------

    /// A call's arguments, evaluated in the order written, and its callee.
    fn call(&mut self, c: &thir::Call, e: &thir::Expr, receiver_taken: bool) -> Option<Call> {
        let chained = self.chained.take() == Some(e.span);
        if c.receiver
            && c.modes.first() == Some(&Mode::Take)
            && !receiver_taken
            && let Some(recv) = c.args.first()
            && self.is_named_place(recv)
            && !self.is_copy(recv.ty)
        {
            // A consuming method on a named place must be marked `take x.m()`.
            let what = self.receiver_text(recv);
            let mut edits = vec![wrela_diag::Edit {
                span: e.span.shrink_to_start(),
                replacement: "take ".into(),
            }];
            if chained {
                // `take` marks everything after it up to an operator: the call alone, here.
                edits[0].replacement = "(take ".into();
                edits.push(wrela_diag::Edit {
                    span: e.span.shrink_to_end(),
                    replacement: ")".into(),
                });
            }
            self.diags.push(
                Diagnostic::new(codes::E0501, e.span, format!("this method takes `{what}`, so the call is written `take {what}...`"))
                    .with_note("a consuming method moves its receiver; moving out of a named place is marked (§6.2)")
                    .with_fix_edits("mark the move", edits),
            );
        }
        // A stored closure or function is read into a local, which the call calls.
        let callee = match &c.callee {
            thir::Callee::Value(v) => {
                let op = self.value(v, Want::Read)?;
                Callee::Local(self.operand_local(op))
            }
            other => other.clone(),
        };
        // The locals the call's `mut` arguments are places of: a `Copy` argument read from one
        // is copied where it's evaluated, before the call's `mut` access begins (§6.5).
        let mut_roots: Vec<Local> = c
            .args
            .iter()
            .zip(&c.modes)
            .filter(|(_, m)| **m == Mode::Mut)
            .filter_map(|(a, _)| match &a.place_root().kind {
                ExprKind::Local(l) => Some(*l),
                ExprKind::MutArg(x) => match &x.place_root().kind {
                    ExprKind::Local(l) => Some(*l),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let mut args: Vec<Option<Arg>> = vec![None; c.args.len()];
        for &i in &c.order {
            let a = &c.args[i];
            let mode = c.modes.get(i).copied().unwrap_or(Mode::Borrow);
            let is_recv = c.receiver && i == 0;
            if is_recv && matches!(a.kind, ExprKind::Call(_)) {
                self.chained = Some(a.span);
            }
            let arg = match mode {
                Mode::Take => {
                    let v = if is_recv && self.is_named_place(a) {
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
                    Arg::Take(self.consume(v))
                }
                Mode::Mut => {
                    let inner = match &a.kind {
                        ExprKind::MutArg(x) => x.as_ref(),
                        _ => a,
                    };
                    let p = self.place(inner)?;
                    Arg::Mut(self.arg_local(p, true, inner.span), a.span)
                }
                Mode::Borrow
                    if is_callable(&self.p.types, a.ty) || self.p.is_borrow_struct(a.ty) =>
                {
                    // A closure or function, or a borrow struct: a value whose loans are its
                    // captures' or its fields'.
                    let p = match &a.kind {
                        ExprKind::Local(l) => Place::local(*l),
                        _ if self.is_named_place(a) && self.p.is_borrow_struct(a.ty) => {
                            self.place(a)?
                        }
                        _ => {
                            let v = self.value(a, Want::Read)?;
                            self.operand_place(v)
                        }
                    };
                    Arg::Borrow(p, a.span)
                }
                Mode::Borrow
                    if c.ret_mode != RetMode::Borrow
                        && self.is_copy(a.ty)
                        && self.is_named_place(a)
                        && matches!(&a.place_root().kind, ExprKind::Local(l) if mut_roots.contains(l)) =>
                {
                    let v = self.value(a, Want::Read)?;
                    Arg::Borrow(self.operand_place(v), a.span)
                }
                Mode::Borrow => Arg::Borrow(self.borrowed_place(a)?, a.span),
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
                    parts.push(format!(".{}", field_name(self.p, b.ty, None, *i)));
                    e = b;
                }
                ExprKind::Index(b, _) => {
                    parts.push("[_]".into());
                    e = b;
                }
                ExprKind::Swizzle(b, cs) => {
                    let s: String = cs.iter().map(|&c| comp_char(c)).collect();
                    parts.push(format!(".{s}"));
                    e = b;
                }
                _ => break,
            }
        }
        parts.reverse();
        parts.concat()
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
                    self.unmarked_move(&p, e.ty, e.span, None);
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
        let (name, role) = match &call.callee {
            Callee::Fn { func, .. } | Callee::TraitMethod { method: func, .. } => {
                (format!("{}(..)", self.p.func(*func).name), TempRole::CallResult)
            }
            _ => ("the call's result".into(), TempRole::Other),
        };
        let mutable = c.ret_mode == RetMode::Mut;
        let t = self.temp(e.ty, LocalKind::TempProjection { mutable, role }, name, e.span);
        self.push(StatementKind::Assign(Place::local(t), Rvalue::Call(call)), e.span);
        Some(Place::local(t))
    }

    fn dispatch(&mut self, d: &thir::Dispatch) -> Option<Rvalue> {
        // In the order written: the arguments before `groups`, `groups`, the rest.
        let mut args = Vec::new();
        for (i, a) in &d.args[..d.groups_at] {
            let p = self.gpu_arg(a)?;
            args.push((*i, p, a.span));
        }
        let u32_ty = self.p.types.u32;
        let mut indirect = None;
        let groups = if d.indirect {
            // The counts are in a buffer, which the dispatch reads.
            let p = self.borrowed_place(&d.groups)?;
            indirect = Some((p, d.groups.span));
            let zero = constant(Lit::Int(0), u32_ty, d.groups.span);
            [zero.clone(), zero.clone(), zero]
        } else if matches!(self.p.types.kind(d.groups.ty), TyKind::Tuple(_)) {
            let g = self.value(&d.groups, Want::Read)?;
            let gp = self.operand_place(g);
            let f = |k: u32, b: &mut Self| {
                b.materialize(OperandKind::Copy(gp.with(Proj::Field(k))), u32_ty, d.groups.span)
            };
            [f(0, self), f(1, self), f(2, self)]
        } else {
            let g = self.value(&d.groups, Want::Read)?;
            let one = constant(Lit::Int(1), u32_ty, d.groups.span);
            [g, one.clone(), one]
        };
        for (i, a) in &d.args[d.groups_at..] {
            let p = self.gpu_arg(a)?;
            args.push((*i, p, a.span));
        }
        Some(Rvalue::Dispatch(Box::new(Dispatch {
            kernel: d.kernel,
            kernel_args: d.kernel_args.clone(),
            groups,
            args,
            indirect,
        })))
    }

    fn draw(&mut self, d: &thir::Draw) -> Option<Rvalue> {
        // In the order written: the shaders' bound arguments, then the counts.
        let mut args = Vec::new();
        for (e, i, a) in &d.args {
            let p = self.gpu_arg(a)?;
            args.push((*e, *i, p, a.span));
        }
        let (mut vertices, mut instances, mut indirect, mut indices) = (None, None, None, None);
        for c in &d.counts {
            match c {
                thir::DrawCount::Vertices => vertices = Some(self.value(&d.vertices, Want::Read)?),
                thir::DrawCount::Instances => {
                    instances = Some(self.value(&d.instances, Want::Read)?);
                }
                thir::DrawCount::Indirect => {
                    if let Some(ind) = &d.indirect {
                        indirect = Some((self.borrowed_place(ind)?, ind.span));
                    }
                }
                thir::DrawCount::Indices => {
                    if let Some(ind) = &d.indices {
                        indices = Some((self.borrowed_place(ind)?, ind.span));
                    }
                }
            }
        }
        // Defaults, which aren't written: literals.
        if vertices.is_none() {
            vertices = Some(self.value(&d.vertices, Want::Read)?);
        }
        if instances.is_none() {
            instances = Some(self.value(&d.instances, Want::Read)?);
        }
        Some(Rvalue::Draw(Box::new(Draw {
            vertex: d.vertex.clone(),
            fragment: d.fragment.clone(),
            vertices: vertices?,
            instances: instances?,
            args,
            indirect,
            indices,
            state: d.state,
        })))
    }

    /// A dispatch's or draw's argument: a buffer passed `mut buf` is bound mutably, so while
    /// the GPU writes it nothing else reaches it (§6.13); anything else is borrowed.
    fn gpu_arg(&mut self, a: &thir::Expr) -> Option<Place> {
        match &a.kind {
            ExprKind::MutArg(x) => {
                let p = self.place(x)?;
                Some(self.arg_local(p, true, x.span))
            }
            _ => self.borrowed_place(a),
        }
    }

    /// A place borrowed for a call, dispatch or draw: bound where it's evaluated.
    fn borrowed_place(&mut self, a: &thir::Expr) -> Option<Place> {
        if self.is_named_place(a) {
            let p = self.place(a)?;
            Some(self.arg_local(p, false, a.span))
        } else {
            let v = self.value(a, Want::Read)?;
            Some(self.operand_place(v))
        }
    }

    // ---- control flow ----------------------------------------------------------------------

    fn block(&mut self, b: &thir::Block, want: Want) -> Option<Operand> {
        self.open_scope(ScopeKind::Block);
        let mut declared = Vec::new();
        for s in &b.stmts {
            self.stmt(s, &mut declared);
        }
        let v = b.tail.as_ref().and_then(|t| self.value(t, want));
        self.close_scope();
        for l in declared.into_iter().rev() {
            self.push(StatementKind::Dead(l), b.span);
        }
        v
    }

    /// Drops what the scopes inside loop `lp` own: before a `break` or `continue` leaves them.
    fn drop_loop(&mut self, lp: BlockId) {
        let at = self.fs.scopes.iter().rposition(|s| s.kind == ScopeKind::Loop(lp));
        if at.is_some() {
            self.drop_until(at);
        }
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
        self.diamond(
            c,
            e.span,
            // Each branch's temporaries end in the branch: its value is moved out first.
            |this| {
                this.open_scope(ScopeKind::Temps);
                let v = this.block(then, want);
                if let (Some(r), Some(v)) = (r, v) {
                    let v = this.consume(v);
                    this.set(r, v, then.span);
                }
                this.close_scope();
            },
            |this| {
                if let Some(x) = else_ {
                    this.open_scope(ScopeKind::Temps);
                    let v = this.value(x, want);
                    if let (Some(r), Some(v)) = (r, v) {
                        let v = this.consume(v);
                        this.set(r, v, x.span);
                    }
                    this.close_scope();
                }
            },
        );
        r.map(|r| {
            self.own_temp(r);
            copy_of(r, e.ty, e.span)
        })
    }

    /// The scrutinee's place: its own if it's a named place, else a temporary.
    /// Checks `place`'s indices where it's evaluated, if it has any ([`StatementKind::Check`]).
    fn check_indices(&mut self, place: &Place, span: Span) {
        if place.proj.iter().any(|p| matches!(p, Proj::Index(_))) {
            self.push(StatementKind::Check(place.clone()), span);
        }
    }

    fn scrutinee(&mut self, s: &thir::Expr) -> Option<Place> {
        if self.is_named_place(s) {
            // Its patterns may read nothing (`_`): its indices are checked anyway.
            let p = self.place(s)?;
            self.check_indices(&p, s.span);
            Some(p)
        } else {
            let v = self.value(s, Want::Move)?;
            Some(self.operand_place(v))
        }
    }

    /// A `match`'s scrutinee: a named place is borrowed until an arm is chosen, by a
    /// projection the arms' tests read through, so a guard can't change what a later arm
    /// tests (E0507). A `match mut` borrows it mutably, for its bindings.
    fn match_scrutinee(&mut self, s: &thir::Expr, mutable: bool) -> Option<Place> {
        let s = match &s.kind {
            ExprKind::MutArg(x) => x.as_ref(),
            _ => s,
        };
        let p = self.scrutinee(s)?;
        if !self.is_named_place(s) {
            return Some(p);
        }
        let ty = place_ty(self.p, &self.locals, &p);
        let kind = LocalKind::TempProjection { mutable, role: TempRole::Match };
        let h = self.temp(ty, kind, "match", s.span);
        self.push(StatementKind::Bind { local: h, place: p, mutable }, s.span);
        Some(Place::local(h))
    }

    fn match_(
        &mut self,
        scrutinee: &thir::Expr,
        arms: &[thir::Arm],
        mutable: bool,
        e: &thir::Expr,
        want: Want,
    ) -> Option<Operand> {
        let sp = self.match_scrutinee(scrutinee, mutable)?;
        let r = (!self.is_unit(e.ty)).then(|| self.temp(e.ty, LocalKind::TempVar, "match", e.span));
        self.arms(&sp, arms, e.span, |b, arm| {
            // The arm's temporaries end in the arm: its value is moved out first.
            b.open_scope(ScopeKind::Temps);
            let v = b.value(&arm.body, want);
            if let (Some(r), Some(v)) = (r, v) {
                let v = b.consume(v);
                b.set(r, v, arm.body.span);
            }
            b.close_scope();
            true
        });
        r.map(|r| {
            self.own_temp(r);
            copy_of(r, e.ty, e.span)
        })
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
            self.open_scope(ScopeKind::Block);
            self.bind_pat(&arm.pat, sp, &mut bound);
            if let Some(g) = &arm.guard {
                self.open_scope(ScopeKind::Temps);
                let gv = self.value(g, Want::Read);
                self.close_scope();
                if let Some(gv) = gv {
                    self.branch_or(gv, fail.clone(), g.span);
                }
            }
            self.commit_moves(&arm.pat, sp);
            let falls_through = body(self, arm);
            self.close_scope();
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
                let k = constant(*l, pat.ty, pat.span);
                self.temp_of(Rvalue::Binary(BinOp::Eq, v, k), bool_ty, pat.span)
            }
            PatKind::Text(t) => {
                // `str`'s `==`: the place and the text, both read as runs of UTF-8.
                let text_ty = self.p.types.adt(self.p.lang_adt(Lang::Text)?, Vec::new());
                let k = self.temp_of(Rvalue::Text(t.clone()), text_ty, pat.span)?;
                let kp = self.operand_place(k);
                let eq = self.p.lang_trait(Lang::Eq)?;
                let method = traits::trait_method(self.p, eq, "eq")?;
                let call = Call {
                    callee: Callee::TraitMethod {
                        method,
                        self_ty: self.p.types.intern(TyKind::Str),
                        trait_args: Vec::new(),
                        method_args: Vec::new(),
                    },
                    args: vec![Arg::Borrow(place.clone(), pat.span), Arg::Borrow(kp, pat.span)],
                    receiver: true,
                    ret_mode: RetMode::Owned,
                    span: pat.span,
                };
                self.temp_of(Rvalue::Call(call), bool_ty, pat.span)
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
                        let k = constant(Lit::Int(i128::from(*v)), u32_ty, pat.span);
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
        it.try_fold(first, |acc, t| self.temp_of(Rvalue::Binary(op, acc, t), bool_ty, span))
    }

    /// Binds a pattern's names to the parts of `place`: a projection aliases its part, any
    /// other binding copies it.
    fn bind_pat(&mut self, pat: &thir::Pat, place: &Place, bound: &mut Vec<Local>) {
        match &pat.kind {
            PatKind::Wild | PatKind::Lit(_) | PatKind::Text(_) | PatKind::Or(_) => {}
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
                        self.set(*l, op, pat.span);
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
        let o = v.and_then(|v| self.value(v, want)).map(|o| self.consume(o));
        self.drop_all();
        self.terminate(TerminatorKind::Return(o), span);
    }

    /// The value of a projection-returning function (or what it `return`s): a place, chosen
    /// by control flow if need be. Each path ends returning its place.
    fn place_tail(&mut self, e: &thir::Expr) {
        match &e.kind {
            ExprKind::Block(b) => self.place_tail_block(b),
            ExprKind::If { cond, then, else_ } => {
                let Some(c) = self.value(cond, Want::Read) else { return };
                let (t, el, m) = (self.new_block(), self.new_block(), self.new_block());
                self.terminate(
                    TerminatorKind::If { cond: c, then: t, else_: el, merge: m },
                    e.span,
                );
                self.fs.cur = t;
                self.place_tail_block(then);
                self.fs.cur = el;
                match else_ {
                    Some(x) => self.place_tail(x),
                    None => self.terminate(TerminatorKind::Unreachable, e.span),
                }
                self.fs.cur = m;
                self.terminate(TerminatorKind::Unreachable, e.span);
            }
            ExprKind::Match { scrutinee, arms, mutable } => {
                let Some(sp) = self.match_scrutinee(scrutinee, *mutable) else { return };
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
                if !self.is_named_place(inner) {
                    let d = Diagnostic::new(
                        codes::E0508,
                        inner.span,
                        "a projection must come from a `borrow` or `mut` parameter, and this is a temporary",
                    );
                    self.diags.push(d.with_help("return an owned value instead: `-> T`"));
                    let _ = self.value(inner, Want::Read);
                    self.terminate(TerminatorKind::Unreachable, e.span);
                    return;
                }
                match self.place(inner) {
                    Some(p) => {
                        self.drop_all();
                        self.terminate(TerminatorKind::ReturnPlace(p), inner.span)
                    }
                    None => self.terminate(TerminatorKind::Unreachable, e.span),
                }
            }
        }
    }

    /// [`Self::place_tail`] of a block: its statements, then its value's place.
    fn place_tail_block(&mut self, b: &thir::Block) {
        self.open_scope(ScopeKind::Block);
        let mut declared = Vec::new();
        for s in &b.stmts {
            self.stmt(s, &mut declared);
        }
        match &b.tail {
            Some(t) => self.place_tail(t),
            None => self.terminate(TerminatorKind::Unreachable, b.span),
        }
        // Every path left above, dropping what it owned.
        self.fs.scopes.pop();
    }

    // ---- statements ------------------------------------------------------------------------

    fn stmt(&mut self, s: &thir::Stmt, declared: &mut Vec<Local>) {
        self.open_scope(ScopeKind::Temps);
        self.stmt_inner(s, declared);
        self.close_scope();
    }

    fn stmt_inner(&mut self, s: &thir::Stmt, declared: &mut Vec<Local>) {
        match &s.kind {
            StmtKind::Bind { pat, init, else_: None } => self.bind(pat, init, s.span, declared),
            StmtKind::Bind { pat, init, else_: Some(eb) } => {
                // Tested as a match arm is: when it doesn't match, the `else` block runs and
                // leaves the scope.
                let Some(p) = self.scrutinee(init) else { return };
                if let Some(t) = self.pat_test(pat, &p) {
                    let (yes, no, after) = (self.new_block(), self.new_block(), self.new_block());
                    self.terminate(
                        TerminatorKind::If { cond: t, then: yes, else_: no, merge: after },
                        pat.span,
                    );
                    self.fs.cur = no;
                    // The `else` block's temporaries (an `f"…"` message's string) are its own,
                    // dropped on its path, not after the statement where the paths join.
                    self.open_scope(ScopeKind::Temps);
                    let _ = self.block(eb, Want::Read);
                    self.close_scope();
                    self.terminate(TerminatorKind::Unreachable, eb.span);
                    self.fs.cur = yes;
                    self.goto(after, pat.span);
                    self.fs.cur = after;
                }
                self.bind_pat(pat, &p, declared);
                self.commit_moves(pat, &p);
            }
            StmtKind::Assign { place, op, value } => self.assign(place, *op, value, s.span),
            StmtKind::Expr(e) => {
                if let ExprKind::Call(c) = &e.kind {
                    // A borrow struct with a `mut` field lends that field `mut`, in any mode.
                    let lends_mut = c.modes.contains(&Mode::Mut)
                        || c.args.iter().any(|a| {
                            self.p.is_borrow_struct(a.ty)
                                && crate::borrowck::has_mut_field(self.p, a.ty)
                        });
                    self.discarded.push((c.callee.clone(), lends_mut, e.ty, e.span));
                }
                if self.is_named_place(e) && !matches!(e.kind, ExprKind::Call(_)) {
                    // A place on its own: evaluate and check its indices, read nothing.
                    if let Some(p) = self.place(e) {
                        self.check_indices(&p, e.span);
                    }
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
            self.commit_moves(pat, &p);
            return;
        };
        declared.push(l);
        let kind = self.locals[l.index()].kind;
        if is_callable(&self.p.types, init.ty) {
            // A closure or function bound to a name: the local holds it (and its loans).
            if kind == LocalKind::User(thir::LocalKind::Projection { mutable: true }) {
                let name = &self.locals[l.index()].name;
                self.diags.push(
                    Diagnostic::new(
                        codes::E0512,
                        init.span,
                        "a closure or function can't be projected with `mut`",
                    )
                    .with_note(
                        "calling it doesn't change it; it borrows mutably what it changes (§6.7)",
                    )
                    .with_help(format!("bind it with `let {name} = ...`")),
                );
            }
            let rv = match &init.kind {
                ExprKind::Local(src) => Rvalue::Use(Operand {
                    kind: OperandKind::Copy(Place::local(*src)),
                    ty: init.ty,
                    span: init.span,
                }),
                ExprKind::Closure(_) | ExprKind::FnRef(..) => match self.rvalue(init, Want::Read) {
                    Some(rv) => rv,
                    None => return,
                },
                // Chosen by `if`, `match` or a block.
                _ => match self.value(init, Want::Read) {
                    Some(v) => Rvalue::Use(v),
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
                let p = if self.is_named_place(inner) {
                    self.place(inner)
                } else {
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
                    self.value(init, Want::Move).map(|v| self.operand_place(v))
                };
                let Some(p) = p else { return };
                self.check_indices(&p, init.span);
                self.push(StatementKind::Live(l), span);
                self.push(StatementKind::Bind { local: l, place: p, mutable }, init.span);
            }
            LocalKind::User(thir::LocalKind::Owned { mutable: true })
                if self.is_named_place(init) && !self.is_copy(init.ty) =>
            {
                // `var` owns its value: from a temporary, `take place`, or `.clone()`.
                let Some(p) = self.place(init) else { return };
                let what = self.describe(&p);
                let d = Diagnostic::new(
                    codes::E0517,
                    init.span,
                    format!("`var` owns its value, so it can't share `{what}`"),
                );
                // A constant can't be moved out of: only copied.
                let constant = matches!(self.locals[p.local.index()].kind, LocalKind::Const(_));
                let clone = traits::implements_builtin(self.p, init.ty, Lang::Clone);
                self.diags.push(if clone && constant {
                    d.with_help(format!(
                        "copy it with `{what}.clone()`: a constant can't be moved out of"
                    ))
                    .with_fix(
                        format!("copy it: `{what}.clone()`"),
                        init.span.shrink_to_end(),
                        ".clone()",
                    )
                } else if constant {
                    d.with_note("a constant can't be moved out of, and its type isn't `Clone`")
                } else if clone {
                    d.with_help(format!(
                        "move it with `take {what}`, or copy it with `{what}.clone()`"
                    ))
                    .with_fix(
                        format!("copy it: `{what}.clone()`"),
                        init.span.shrink_to_end(),
                        ".clone()",
                    )
                } else {
                    d.with_fix(
                        format!("move it: `take {what}`"),
                        init.span.shrink_to_start(),
                        "take ",
                    )
                });
                let v = self.materialize(OperandKind::Copy(p), init.ty, init.span);
                self.push(StatementKind::Live(l), span);
                self.set(l, v, span);
            }
            _ => {
                let v = self.value(init, Want::Move).map(|v| self.consume(v));
                self.push(StatementKind::Live(l), span);
                if let Some(v) = v {
                    self.set(l, v, span);
                    self.own_local(l);
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
            None => {
                // The old value is dropped first: it's overwritten.
                if self.needs_drop(place.ty) {
                    self.push(StatementKind::Drop(p.clone()), place.span);
                }
                self.consume(v)
            }
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
        self.open_scope(ScopeKind::Loop(lp));
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
        self.fs.scopes.pop();
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
            // The condition's temporaries are dropped on each pass.
            let bool_ty = self.p.types.bool;
            self.open_scope(ScopeKind::Temps);
            let nc = self
                .value(c, Want::Read)
                .and_then(|cv| self.temp_of(Rvalue::Unary(UnOp::Not, cv), bool_ty, c.span));
            self.close_scope();
            if let Some(nc) = nc {
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
        self.set(counter, s.clone(), span);
        let copy = |b: &mut Self| b.materialize(OperandKind::Copy(Place::local(counter)), ty, span);
        // `a..=b` stops after `b` without computing `b + 1`, which could overflow.
        let done = inclusive.then(|| {
            let d = self.temp(bool_ty, LocalKind::TempVar, "done", span);
            if let Some(gt) = self.temp_of(Rvalue::Binary(BinOp::Gt, s, e.clone()), bool_ty, span) {
                self.set(d, gt, span);
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
        self.set(var, c, span);
        let _ = self.block(body, Want::Read);
        self.push(StatementKind::Dead(var), span);
        self.end_loop(lp, cont, exit, span, |b| {
            let c = copy(b);
            match done {
                None => b.set_next(counter, c, span),
                Some(d) => {
                    let Some(last) =
                        b.temp_of(Rvalue::Binary(BinOp::Eq, c.clone(), e), bool_ty, span)
                    else {
                        return;
                    };
                    let yes = constant(Lit::Bool(true), bool_ty, span);
                    b.diamond(
                        last,
                        span,
                        |b| b.set(d, yes, span),
                        |b| b.set_next(counter, c, span),
                    );
                }
            }
        });
    }

    /// `l = v + 1`.
    fn set_next(&mut self, l: Local, v: Operand, span: Span) {
        let ty = v.ty;
        let one = constant(Lit::Int(1), ty, span);
        if let Some(n) = self.temp_of(Rvalue::Binary(BinOp::Add, v, one), ty, span) {
            self.set(l, n, span);
        }
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
            let kind = LocalKind::TempProjection { mutable, role: TempRole::Other };
            let w = self.temp(array.ty, kind, name, array.span);
            self.push(StatementKind::Bind { local: w, place: ap, mutable }, array.span);
            Place::local(w)
        } else {
            ap
        };
        let len = match self.p.types.kind(array.ty) {
            TyKind::Array(_, n) => constant(Lit::Int(i128::from(*n)), u32_ty, array.span),
            _ => match self.temp_of(Rvalue::Len(walked.clone()), u32_ty, array.span) {
                Some(v) => v,
                None => return,
            },
        };
        let index = self.temp(u32_ty, LocalKind::TempVar, "index", span);
        self.set(index, constant(Lit::Int(0), u32_ty, span), span);
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
            self.set(var, v, span);
        }
        let _ = self.block(body, Want::Read);
        self.push(StatementKind::Dead(var), span);
        self.end_loop(lp, cont, exit, span, |b| {
            let i = b.materialize(OperandKind::Copy(Place::local(index)), u32_ty, span);
            b.set_next(index, i, span);
        });
    }
}
