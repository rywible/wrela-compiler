//! The body checker: types every expression of a function body and builds its typed tree
//! ([`crate::thir`]). Inference is local to the body; signatures say everything else.

mod call;
mod expr;
pub mod infer;
mod pat;
mod units;
mod zonk;

pub use zonk::opaque_cycles;

use crate::defs::*;
use crate::graph::{Visit, dfs_cycles};
use crate::mir::build::Consts;
use crate::program::Program;
use crate::resolve::Scope;
use crate::thir::*;
use crate::ty::*;
use infer::Infer;
use std::collections::HashMap;
use wrela_diag::{Diagnostic, Edit, Span, codes};
use wrela_syntax::ast;

/// A trait bound to check once the body's types are known.
struct Obligation {
    ty: TyId,
    trait_ref: TraitRef,
    span: Span,
    /// The code reported if it isn't met: E0400, or a more specific one.
    code: wrela_diag::Code,
    /// What needed it, for the message: "the argument `field` of `sample`".
    why: String,
}

/// The local names in scope, innermost last: a stack that block ends cut back, with an index by
/// name, so a lookup doesn't walk every shadowed binding.
#[derive(Default)]
struct Env {
    names: Vec<String>,
    by_name: HashMap<String, Vec<LocalId>>,
}

impl Env {
    fn push(&mut self, name: &str, id: LocalId) {
        self.names.push(name.into());
        self.by_name.entry(name.into()).or_default().push(id);
    }

    fn len(&self) -> usize {
        self.names.len()
    }

    /// Forgets the names declared after the first `len`.
    fn truncate(&mut self, len: usize) {
        for name in self.names.drain(len..) {
            if let Some(ids) = self.by_name.get_mut(&name) {
                ids.pop();
                if ids.is_empty() {
                    self.by_name.remove(&name);
                }
            }
        }
    }

    /// The innermost local named `name`.
    fn lookup(&self, name: &str) -> Option<LocalId> {
        self.by_name.get(name).and_then(|ids| ids.last()).copied()
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }
}

struct ClosureFrame {
    id: ClosureId,
    /// Locals declared before the closure started; uses of these are captures.
    outer_locals: usize,
    captures: Vec<(LocalId, bool)>,
    ret: TyId,
}

pub(crate) struct Checker<'p> {
    pub(crate) p: &'p Program,
    /// The types of the constants checked so far (all of them, once bodies are checked).
    pub(crate) const_tys: &'p ConstTypes,
    pub(crate) diags: Vec<Diagnostic>,
    pub(crate) infer: Infer,
    pub(crate) fn_id: Option<FnId>,
    pub(crate) scope: Scope,
    pub(crate) locals: Vec<LocalDecl>,
    closures: Vec<Option<ClosureDef>>,
    /// Names in scope, innermost last.
    env: Env,
    ret_ty: TyId,
    loop_depth: u32,
    closure_stack: Vec<ClosureFrame>,
    obligations: Vec<Obligation>,
    /// Integer literals to range-check once their types are known.
    int_literals: Vec<(TyId, u64, bool, Span)>,
    /// Float literals to range-check once their types are known.
    float_literals: Vec<(TyId, f64, f32, Span)>,
    /// Negations of integers whose type wasn't known yet: they must turn out signed.
    negated_ints: Vec<(TyId, Span)>,
    /// `base ** exp` types, to check once the base's type is known: an integer's exponent is
    /// a `u32`, a float's a float.
    pows: Vec<(TyId, TyId, Span)>,
    /// Built-in calls whose argument types held literal types not settled yet: checked again
    /// once they are.
    builtin_calls: Vec<(crate::builtins::BuiltinFn, Vec<TyId>, Span)>,
    /// How many `unsafe` blocks enclose the code being checked.
    pub(crate) unsafe_depth: u32,
    /// Projections in a call's signature on types not inferred yet (`T::Out` while `T` is a
    /// variable), each with the variable that stands for it until it can be normalized.
    projections: Vec<(TyId, TyId, Span)>,
    /// Operands of integer-only operators (`<<`, `&`, `!`, ...) whose literal type wasn't
    /// settled yet: they must not turn out floats.
    int_ops: Vec<(TyId, &'static str, Span)>,
    /// Indexes whose literal type wasn't settled yet: a `u32` or an `i32` will do, so they're
    /// checked once it is.
    indexes: Vec<(TyId, Span)>,
    /// Pattern bindings declared before their types were known, and whether each binds into a
    /// place: their kinds are decided at the end ([`Checker::binding_kinds`]).
    pending_bindings: Vec<(LocalId, bool)>,
    /// `let x = place`: an error unless `x`'s type turns out `Copy` (E0518). The local, the
    /// place's span, and the `let` keyword's.
    pub(crate) owned_lets: Vec<(LocalId, Span, Span)>,
    /// In a `match mut`'s patterns: bindings into the place project it mutably.
    pub(crate) binding_mut: bool,
    /// A syntax error's node has been checked. Names that don't resolve after it aren't
    /// reported (the broken code may have declared them), nor is a missing return value.
    pub(crate) saw_syntax_error: bool,
    /// E0329 has been reported: a type that grew too large once grows again from the error.
    pub(crate) too_large: bool,
}

impl<'p> Checker<'p> {
    fn new(
        p: &'p Program,
        const_tys: &'p ConstTypes,
        scope: Scope,
        fn_id: Option<FnId>,
    ) -> Checker<'p> {
        let unit = p.types.unit;
        Checker {
            p,
            const_tys,
            diags: Vec::new(),
            infer: Infer::default(),
            fn_id,
            scope,
            locals: Vec::new(),
            closures: Vec::new(),
            env: Env::default(),
            ret_ty: unit,
            loop_depth: 0,
            closure_stack: Vec::new(),
            obligations: Vec::new(),
            int_literals: Vec::new(),
            float_literals: Vec::new(),
            negated_ints: Vec::new(),
            pows: Vec::new(),
            builtin_calls: Vec::new(),
            unsafe_depth: 0,
            projections: Vec::new(),
            int_ops: Vec::new(),
            indexes: Vec::new(),
            pending_bindings: Vec::new(),
            owned_lets: Vec::new(),
            binding_mut: false,
            saw_syntax_error: false,
            too_large: false,
        }
    }

    pub(crate) fn err(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    pub(crate) fn display(&self, t: TyId) -> String {
        let t = self.infer.resolve(&self.p.types, t);
        // A literal whose type isn't known yet.
        match self.infer.var_kind(&self.p.types, t) {
            Some(VarKind::Int) => "{integer}".into(),
            Some(VarKind::Float) => "{float}".into(),
            _ => self.p.display_ty(t),
        }
    }

    pub(crate) fn shallow(&self, t: TyId) -> TyId {
        self.infer.shallow(&self.p.types, t)
    }

    /// `t` with its variables resolved and its projections on concrete types replaced.
    pub(crate) fn normalized(&self, t: TyId) -> TyId {
        let r = self.infer.resolve(&self.p.types, t);
        crate::traits::normalize(self.p, r, None)
    }

    /// Unifies `a` and `b` if they fit, leaving no trace if they don't.
    pub(crate) fn try_unify(&mut self, a: TyId, b: TyId) -> bool {
        self.infer.try_unify(&self.p.types, a, b)
    }

    /// Whether `a` and `b` would unify, leaving no trace either way.
    pub(crate) fn fits(&mut self, a: TyId, b: TyId) -> bool {
        self.infer.fits(&self.p.types, a, b)
    }

    pub(crate) fn new_var(&mut self, kind: VarKind, span: Span) -> TyId {
        self.infer.new_var(&self.p.types, kind, span)
    }

    /// What `t` is, through the variables bound so far.
    pub(crate) fn kind(&self, t: TyId) -> &'p TyKind {
        self.p.types.kind(self.shallow(t))
    }

    /// Whether `t` is an integer type, or an integer literal's type not settled yet.
    pub(crate) fn is_intlike(&self, t: TyId) -> bool {
        matches!(self.kind(t), TyKind::Int(_))
            || self.infer.var_kind(&self.p.types, t) == Some(VarKind::Int)
    }

    /// Whether `t` is an integer vector: `vecNi` or `vecNu`.
    pub(crate) fn is_int_vec(&self, t: TyId) -> bool {
        matches!(self.kind(t), TyKind::Vec(VecElem::I32 | VecElem::U32, _))
    }

    /// Whether `t` is a number literal's type not settled yet.
    pub(crate) fn is_number_var(&self, t: TyId) -> bool {
        matches!(self.infer.var_kind(&self.p.types, t), Some(VarKind::Int | VarKind::Float))
    }

    /// Whether a part of `t` (a resolved type) is the error type.
    fn has_error(&self, t: TyId) -> bool {
        self.p.types.any(t, &mut |k| matches!(k, TyKind::Error))
    }

    /// The type of constant `c`: its annotation, or the type its value was checked to have.
    fn const_ty(&self, c: ConstId) -> TyId {
        match self.p.const_(c).ty {
            Some(t) => t,
            None => self.const_tys.get(&c).copied().unwrap_or(self.p.types.error),
        }
    }

    fn declare_local(&mut self, name: &str, ty: TyId, kind: LocalKind, span: Span) -> LocalId {
        let id = self.declare_unnamed(ty, kind, span);
        self.locals[id.index()].name = name.into();
        self.env.push(name, id);
        id
    }

    /// A local no name refers to: the counter of a `for _` loop.
    fn declare_unnamed(&mut self, ty: TyId, kind: LocalKind, span: Span) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        let closure = self.closure_stack.last().map(|c| c.id);
        let name = "_".into();
        self.locals.push(LocalDecl {
            name,
            ty,
            kind,
            span,
            keyword: None,
            shorthand: false,
            closure,
        });
        id
    }

    /// The loop variable of a `for`: the name, or none for `_`.
    fn declare_binder(
        &mut self,
        name: Option<&ast::Ident>,
        ty: TyId,
        kind: LocalKind,
        span: Span,
    ) -> LocalId {
        match name {
            Some(n) => self.declare_local(&n.name, ty, kind, n.span),
            None => self.declare_unnamed(ty, kind, span),
        }
    }

    /// Finds a local by name, recording a capture if it's from outside the current closure.
    pub(crate) fn lookup_local(&mut self, name: &str) -> Option<LocalId> {
        let id = self.env.lookup(name)?;
        self.note_use(id, false);
        Some(id)
    }

    /// Records that `id` is used (and written, if `write`) by the code being checked.
    pub(crate) fn note_use(&mut self, id: LocalId, write: bool) {
        for frame in self.closure_stack.iter_mut() {
            if id.index() < frame.outer_locals {
                match frame.captures.iter_mut().find(|(l, _)| *l == id) {
                    Some(c) => c.1 |= write,
                    None => frame.captures.push((id, write)),
                }
            }
        }
    }

    /// `d`, a mismatch between two branches typed `a` and `b`, with why when both are closures
    /// or functions: each is its own type. (Two `fn(..)` parameters differ in their signatures.)
    pub(crate) fn branch_mismatch(&self, d: Diagnostic, a: TyId, b: TyId) -> Diagnostic {
        let callable = |t| crate::mir::is_callable(&self.p.types, self.shallow(t));
        let fn_ptr = |t| matches!(self.kind(t), TyKind::FnPtr(..));
        // Two structs: one value of either kind is an enum's (a field's structure is its type).
        let is_struct = |t| matches!(self.kind(t), TyKind::Adt(x, _) if !self.p.adt(*x).is_enum());
        if is_struct(a) && is_struct(b) {
            return d
                .with_note("an `if` gives one type; each struct is its own type (a field's structure is its type, §17)")
                .with_help("to choose between them at run time, make an enum with a variant for each, and `match` on it where it's used");
        }
        if !(callable(a) && callable(b)) || (fn_ptr(a) && fn_ptr(b)) {
            return d;
        }
        d.with_note("each closure and function has its own type; choosing between them at run time isn't supported yet")
            .with_help("call each one in its own branch")
    }

    /// Unifies `actual` with `expected`, or reports E0300 at `span`.
    pub(crate) fn expect(&mut self, actual: TyId, expected: TyId, span: Span) -> bool {
        if matches!(self.kind(actual), TyKind::Never) {
            return true;
        }
        if self.try_unify(actual, expected) {
            return true;
        }
        // A projection on a type that's known by now (`X::Box` with `X = vec3`).
        let (na, ne) = (self.normalized(actual), self.normalized(expected));
        if (na != actual || ne != expected) && self.try_unify(na, ne) {
            return true;
        }
        // A type made from one that already produced an error isn't reported again.
        if self.has_error(na) || self.has_error(ne) {
            return false;
        }
        let (a, e) = (self.display(actual), self.display(expected));
        let mut d = Diagnostic::new(codes::E0300, span, format!("expected `{e}`, found `{a}`"));
        let (ka, ke) = (self.kind(actual), self.kind(expected));
        match (&ka, &ke) {
            (TyKind::Int(_) | TyKind::Float(_), TyKind::Int(_) | TyKind::Float(_)) => {
                d = d.with_help(format!("convert it explicitly: `{e}(...)`")).with_fix_edits(
                    format!("convert it: `{e}(...)`"),
                    vec![
                        wrela_diag::Edit {
                            span: span.shrink_to_start(),
                            replacement: format!("{e}("),
                        },
                        wrela_diag::Edit { span: span.shrink_to_end(), replacement: ")".into() },
                    ],
                );
            }
            (TyKind::Float(_), TyKind::Vec(..)) | (TyKind::Int(_), TyKind::Vec(..)) => {
                d = d.with_help(format!("make a vector from it with `{e}(...)`"));
            }
            (TyKind::Vec(..), TyKind::Vec(..)) => {
                d = d.with_help(format!("convert it, each component: `{e}(...)`"));
            }
            (TyKind::Vec(..), TyKind::Float(_) | TyKind::Int(_)) => {
                d = d.with_help(format!(
                    "a `{a}` isn't a number; take a component (`.x`) or its `length()`"
                ));
            }
            (TyKind::Tuple(t), _) if t.is_empty() => {
                d = d.with_note("a block's value is its last line when that's an expression; a statement has no value");
            }
            // An alias that names traits: one function decides its type (§4).
            (_, TyKind::Opaque(f, args))
                if args.is_empty() && self.p.aliases.iter().any(|al| al.defined_by == Some(*f)) =>
            {
                let name = &self.p.func(*f).name;
                d = d
                    .with_note(format!(
                        "`{e}` is the type `{name}` returns, seen only through its traits"
                    ))
                    .with_help(format!("use a value of it, such as `{name}(...)`'s"));
            }
            _ => {}
        }
        self.err(d);
        false
    }

    // ---- statements and blocks -------------------------------------------------------------

    pub(crate) fn check_block(&mut self, b: &ast::Block, expected: Option<TyId>) -> Block {
        let env_len = self.env.len();
        let mut stmts = Vec::new();
        let mut tail = None;
        let n = b.stmts.len();
        for (i, s) in b.stmts.iter().enumerate() {
            let last = i + 1 == n;
            if last && let ast::StmtKind::Expr(e) = &s.kind {
                let ex = self.check_expr(e, expected);
                tail = Some(Box::new(ex));
                continue;
            }
            if let Some(st) = self.check_stmt(s) {
                stmts.push(st);
            }
        }
        self.env.truncate(env_len);
        let ty = match &tail {
            Some(t) => t.ty,
            None if stmts.last().is_some_and(|s| self.diverges(s)) => self.p.types.never,
            None => self.p.types.unit,
        };
        Block { stmts, tail, ty, span: b.span }
    }

    /// Whether a statement never finishes normally: `return`, or a `loop` with no `break`.
    fn diverges(&self, s: &Stmt) -> bool {
        match &s.kind {
            StmtKind::Expr(e) => matches!(self.kind(e.ty), TyKind::Never),
            StmtKind::Loop { body } => !has_break(body),
            _ => false,
        }
    }

    /// A binding's value, against its annotation if it has one; and the binding's type.
    fn check_init(&mut self, annot: Option<&ast::TypeExpr>, init: &ast::Expr) -> (TyId, Expr) {
        let annot = annot.map(|t| self.resolve_type(t));
        let init = self.check_expr(init, annot);
        if let Some(a) = annot {
            self.expect(init.ty, a, init.span);
        }
        (annot.unwrap_or(init.ty), init)
    }

    fn check_stmt(&mut self, s: &ast::Stmt) -> Option<Stmt> {
        let kind = match &s.kind {
            ast::StmtKind::Let { pat, ty, init, else_ } => {
                let (ty, init) = self.check_init(ty.as_ref(), init);
                let keyword = Span::new(s.span.file, s.span.start, s.span.start + 3);
                let Some(else_) = else_ else {
                    let p = self.check_let_pattern(pat, ty, &init);
                    if let PatKind::Bind(id) = p.kind {
                        self.locals[id.index()].keyword = Some(keyword);
                        // `let` owns its value (§6.3): a place it would share is an error,
                        // unless its type is `Copy` (decided once its type is known).
                        if init.is_place() {
                            self.owned_lets.push((id, init.span, keyword));
                        }
                    }
                    return Some(Stmt {
                        kind: StmtKind::Bind { pat: p, init, else_: None },
                        span: s.span,
                    });
                };
                // `let pat = init else { ... }`: the block runs when the pattern doesn't match,
                // and must leave the scope. The pattern's names are bound after it.
                let from_place = init.is_place();
                let env_len = self.env.len();
                let p = self.check_pat(pat, ty, from_place);
                self.bound_once(&p);
                // The `else` block can't see the pattern's names.
                let bound: Vec<(String, LocalId)> = self
                    .env
                    .names()
                    .skip(env_len)
                    .map(String::from)
                    .zip(self.env_ids_after(env_len))
                    .collect();
                self.env.truncate(env_len);
                let eb = self.check_block(else_, None);
                if !matches!(self.kind(eb.ty), TyKind::Never | TyKind::Error) {
                    let mut d = Diagnostic::new(
                        codes::E0330,
                        else_.span,
                        "the `else` of a `let … else` must leave the scope",
                    )
                    .with_note("it runs when the pattern doesn't match, so the names it binds have no value after it")
                    .with_help("end it with `return`, `break`, `continue` or a panic");
                    // `else { 0 }`: the value was meant to be returned.
                    if let Some(last) = else_.stmts.last()
                        && let ast::StmtKind::Expr(e) = &last.kind
                        && !matches!(
                            e.kind,
                            ast::ExprKind::Block(_)
                                | ast::ExprKind::If { .. }
                                | ast::ExprKind::Match { .. }
                        )
                    {
                        let at = Span::new(e.span.file, e.span.start, e.span.start);
                        d = d.with_fix("return it", at, "return ");
                    }
                    self.err(d);
                }
                for (name, id) in bound {
                    self.env.push(&name, id);
                }
                StmtKind::Bind { pat: p, init, else_: Some(eb) }
            }
            ast::StmtKind::Var { kind, name, ty, init } => {
                // `borrow x = if c { a.p } else { a.q }`: a projection names one place, and an
                // `if` or a `match` gives a value, which would move out of the place it picks.
                let chooses = matches!(kind, ast::VarKind::Borrow | ast::VarKind::Mut)
                    && matches!(init.kind, ast::ExprKind::If { .. } | ast::ExprKind::Match { .. });
                let (ty, checked) = self.check_init(ty.as_ref(), init);
                let copy = crate::traits::implements_builtin(
                    self.p,
                    self.infer.resolve(&self.p.types, ty),
                    Lang::Copy,
                );
                if chooses && !copy && !self.has_error(ty) {
                    let kw = if *kind == ast::VarKind::Mut { "mut" } else { "borrow" };
                    self.err(
                        Diagnostic::new(
                            codes::E0502,
                            init.span,
                            format!("a `{kw}` binding names one place, and this chooses between places"),
                        )
                        .with_note("an `if` or a `match` gives a value, which would move out of the place it picks")
                        .with_help(format!("choose in a function that returns `-> {kw} T`, whose `if` picks the place it returns, and bind its result: `{kw} x = pick(...)`")),
                    );
                    let err = self.p.types.error;
                    let id = self.declare_local(
                        &name.name,
                        err,
                        LocalKind::Owned { mutable: false },
                        name.span,
                    );
                    let pat = Pat { ty: err, kind: PatKind::Bind(id), span: name.span };
                    let init = self.error_expr(init.span);
                    return Some(Stmt {
                        kind: StmtKind::Bind { pat, init, else_: None },
                        span: s.span,
                    });
                }
                let init = checked;
                let lk = match kind {
                    ast::VarKind::Var => LocalKind::Owned { mutable: true },
                    ast::VarKind::Mut => {
                        // Writes through it write the place it projects.
                        self.mark_written(&init);
                        LocalKind::Projection { mutable: true }
                    }
                    ast::VarKind::Borrow => LocalKind::Projection { mutable: false },
                };
                let id = self.declare_local(&name.name, ty, lk, name.span);
                if *kind == ast::VarKind::Var {
                    let kw = Span::new(s.span.file, s.span.start, s.span.start + 3);
                    self.locals[id.index()].keyword = Some(kw);
                }
                let pat = Pat { ty, kind: PatKind::Bind(id), span: name.span };
                StmtKind::Bind { pat, init, else_: None }
            }
            ast::StmtKind::Assign { target, op, value } => {
                return Some(self.check_assign(target, *op, value, s.span));
            }
            ast::StmtKind::Expr(e) => {
                let ex = self.check_expr(e, None);
                StmtKind::Expr(ex)
            }
            ast::StmtKind::While { cond, body } => {
                let bool_ty = self.p.types.bool;
                let c = self.check_expr(cond, Some(bool_ty));
                self.expect_cond(&c);
                let b = self.check_loop_body(body);
                StmtKind::While { cond: c, body: b }
            }
            ast::StmtKind::WhileLet { pat, init, body } => {
                // `loop { let pat = init else { break }; body }` (`stmt.while-let`).
                let env_len = self.env.len();
                let (ty, init) = self.check_init(None, init);
                let p = self.check_pat(pat, ty, init.is_place());
                self.bound_once(&p);
                let never = self.p.types.never;
                let leave = Expr { ty: never, span: pat.span, kind: ExprKind::Break };
                let leave = Block {
                    stmts: Vec::new(),
                    tail: Some(Box::new(leave)),
                    ty: never,
                    span: pat.span,
                };
                let mut b = self.check_loop_body(body);
                self.env.truncate(env_len);
                let bind = StmtKind::Bind { pat: p, init, else_: Some(leave) };
                b.stmts.insert(0, Stmt { kind: bind, span: pat.span });
                StmtKind::Loop { body: b }
            }
            ast::StmtKind::Loop { body } => StmtKind::Loop { body: self.check_loop_body(body) },
            ast::StmtKind::For { mutable, pat, iter, body } => {
                return self.check_for(*mutable, pat, iter, body, s.span);
            }
        };
        Some(Stmt { kind, span: s.span })
    }

    /// `target op value`, a statement (or a match arm's body).
    pub(crate) fn check_assign(
        &mut self,
        target: &ast::Expr,
        op: ast::AssignOp,
        value: &ast::Expr,
        span: Span,
    ) -> Stmt {
        let place = self.check_expr(target, None);
        self.check_assign_target(&place);
        self.mark_written(&place);
        let bin = op.binop();
        let value = match bin {
            None => self.check_expect(value, place.ty),
            Some(b) => {
                // As the right operand of `place op value`.
                let hint = self.right_hint(b, place.ty);
                let v = self.check_expr(value, Some(hint));
                let result = self.binary_result(b, place.ty, v.ty, span);
                if let Some(r) = result {
                    self.expect(r, place.ty, span);
                }
                v
            }
        };
        Stmt { kind: StmtKind::Assign { place, op: bin, value }, span }
    }

    /// The locals the environment has bound after its first `len` names, in order.
    fn env_ids_after(&self, len: usize) -> Vec<LocalId> {
        let names: Vec<&str> = self.env.names().skip(len).collect();
        // Each name's innermost binding is the one declared last: walk them in reverse.
        let mut seen: HashMap<&str, usize> = HashMap::new();
        let mut out = Vec::new();
        for n in names.iter().rev() {
            let depth = seen.entry(n).or_insert(0);
            let ids = &self.env.by_name[*n];
            out.push(ids[ids.len() - 1 - *depth]);
            *depth += 1;
        }
        out.reverse();
        out
    }

    /// An assignment writes a place: a local, a field, component or element of one, or what a
    /// `mut` projection-returning call returned. Whether that place may be written is the
    /// memory checker's question; whether it's a place at all is this one.
    fn check_assign_target(&mut self, place: &Expr) {
        let d = match &place.place_root().kind {
            ExprKind::Local(_) | ExprKind::Error => return,
            ExprKind::Call(c) if c.ret_mode != RetMode::Owned => return,
            ExprKind::Call(c) => {
                let name = match &c.callee {
                    Callee::Fn { func, .. } | Callee::TraitMethod { method: func, .. } => {
                        format!("`{}`", self.p.func(*func).name)
                    }
                    _ => "the call".into(),
                };
                Diagnostic::new(
                    codes::E0505,
                    place.span,
                    "can't assign to part of a temporary value",
                )
                .with_note(format!("{name} returns a value, not a projection, so the change would be lost"))
            }
            ExprKind::Const(c) => Diagnostic::new(
                codes::E0505,
                place.span,
                format!("can't assign to the constant `{}`", self.p.const_(*c).name),
            )
            .with_help("copy it into a `var` to change it"),
            _ => Diagnostic::new(codes::E0505, place.span, "can't assign to a temporary value")
                .with_note("an assignment's target is a place: a variable, or a field, component or element of one"),
        };
        self.err(d);
    }

    /// A loop's body: `break` and `continue` are allowed in it, and it has no value.
    fn check_loop_body(&mut self, body: &ast::Block) -> Block {
        self.loop_depth += 1;
        let b = self.check_block(body, None);
        self.loop_depth -= 1;
        self.expect_unit_block(&b);
        b
    }

    fn expect_unit_block(&mut self, b: &Block) {
        if let Some(t) = &b.tail {
            let unit = self.p.types.unit;
            if !matches!(self.kind(t.ty), TyKind::Never | TyKind::Tuple(_) | TyKind::Error) {
                let shown = self.display(t.ty);
                self.err(
                    Diagnostic::new(
                        codes::E0300,
                        t.span,
                        format!("a loop's body has no value, but this is a `{shown}`"),
                    )
                    .with_help("assign it to a `var` declared before the loop"),
                );
            } else {
                self.expect(t.ty, unit, t.span);
            }
        }
    }

    pub(crate) fn expect_cond(&mut self, c: &Expr) {
        if matches!(self.kind(c.ty), TyKind::Bool | TyKind::Never | TyKind::Error) {
            return;
        }
        if let TyKind::Var(_) = self.kind(c.ty) {
            let b = self.p.types.bool;
            if self.infer.unify(&self.p.types, c.ty, b).is_ok() {
                return;
            }
        }
        let shown = self.display(c.ty);
        let mut d = Diagnostic::new(
            codes::E0313,
            c.span,
            format!("a condition must be `bool`, not `{shown}`"),
        )
        .with_help("compare it, as in `x != 0`");
        // A number: test it against zero.
        let zero = match self.kind(c.ty) {
            TyKind::Int(_) => Some("0"),
            TyKind::Float(_) => Some("0.0"),
            _ => None,
        };
        if let Some(z) = zero {
            let end = c.span.shrink_to_end();
            d = d.with_fix(format!("compare it with `!= {z}`"), end, format!(" != {z}"));
        }
        self.err(d);
    }

    /// `xs.iter()`, `xs.iter_mut()` or `xs.into_iter()` as a `for` loop's sequence, where `xs`
    /// is one a loop goes over itself: E0207, fixed by looping over `xs` (`for mut x` for
    /// `iter_mut`). Returns `xs` checked, and whether its elements change.
    fn rust_iteration(
        &mut self,
        e: &ast::Expr,
        pat: &ast::Pat,
        mutable: bool,
    ) -> Option<(Expr, bool)> {
        let ast::ExprKind::MethodCall { receiver, name, args, generics: None, .. } = &e.kind else {
            return None;
        };
        if !args.is_empty() || !matches!(name.name.as_str(), "iter" | "iter_mut" | "into_iter") {
            return None;
        }
        let arr = self.check_expr(receiver, None);
        let rt = self.infer.resolve(&self.p.types, arr.ty);
        let sequence =
            matches!(self.kind(rt), TyKind::Array(..) | TyKind::ArrayN(..) | TyKind::Slice(_))
                || self.walked_elem(rt).is_some();
        if !sequence {
            // Reported as an unknown method, with the note on iterators.
            let _ = self.check_expr(e, None);
            return None;
        }
        let changes = name.name == "iter_mut";
        let call = Span::new(receiver.span.file, receiver.span.end, e.span.end);
        let mut edits = vec![Edit { span: call, replacement: String::new() }];
        let fix = if changes && !mutable {
            edits.push(Edit {
                span: Span::new(pat.span.file, pat.span.start, pat.span.start),
                replacement: "mut ".into(),
            });
            "loop over it with `for mut`"
        } else {
            "loop over it"
        };
        self.err(
            Diagnostic::new(codes::E0207, name.span, format!("`.{}()` isn't needed: wrela has no iterators", name.name))
                .with_note("a `for` loop goes over a range, an array, a run, a `Vec` or an arena itself; `for mut x in xs` changes each element")
                .with_fix_edits(fix, edits),
        );
        Some((arr, changes))
    }

    fn check_for(
        &mut self,
        mutable: bool,
        pat: &ast::Pat,
        iter: &ast::ForIter,
        body: &ast::Block,
        span: Span,
    ) -> Option<Stmt> {
        let env_len = self.env.len();
        // One name, or `_` for a loop that doesn't use it; over a sequence, a pattern that
        // destructures each element (`for (a, b) in pairs`).
        let (name, parts) = match &pat.kind {
            ast::PatKind::Ident(name) => (Some(name), None),
            ast::PatKind::Wild => (None, None),
            _ if matches!(iter, ast::ForIter::Expr(_)) => (None, Some(pat)),
            _ => {
                self.err(
                    Diagnostic::new(codes::E0100, pat.span, "a range's index is one name")
                        .with_help("name it, or `_` for a loop that doesn't use it"),
                );
                return None;
            }
        };
        let kind = match iter {
            ast::ForIter::Range { start, end, inclusive } => {
                if mutable {
                    self.err(
                        Diagnostic::new(codes::E0505, pat.span, "a range's index can't be `mut`")
                            .with_help("copy it into a `var` inside the loop"),
                    );
                }
                let s = self.check_expr(start, None);
                let e = self.check_expect(end, s.ty);
                if !self.is_intlike(s.ty)
                    && !matches!(self.kind(s.ty), TyKind::Error | TyKind::Never)
                {
                    let shown = self.display(s.ty);
                    self.err(Diagnostic::new(
                        codes::E0305,
                        s.span,
                        format!("a range's bounds must be integers, not `{shown}`"),
                    ));
                } else {
                    // Literal bounds may still become floats from a later use (`let c: f32 = b`):
                    // checked again once the body's types settle.
                    let op = if *inclusive { "..=" } else { ".." };
                    self.int_ops.push((s.ty, op, s.span));
                }
                let var =
                    self.declare_binder(name, s.ty, LocalKind::Owned { mutable: false }, pat.span);
                let b = self.check_loop_body(body);
                StmtKind::ForRange { var, start: s, end: e, inclusive: *inclusive, body: b }
            }
            ast::ForIter::Expr(e) => {
                // `for x in xs.iter()` (or `.iter_mut()`), as in Rust: the loop goes over `xs`.
                let mut mutable = mutable;
                let arr = match self.rust_iteration(e, pat, mutable) {
                    Some((arr, m)) => {
                        mutable |= m;
                        arr
                    }
                    None => self.check_expr(e, None),
                };
                let elem = match self.kind(arr.ty) {
                    TyKind::Array(t, _) | TyKind::ArrayN(t, _) | TyKind::Slice(t) => *t,
                    TyKind::Error => self.p.types.error,
                    _ if let Some(t) = self.walked_elem(arr.ty) => t,
                    _ => {
                        let shown = self.display(arr.ty);
                        self.err(
                            Diagnostic::new(
                                codes::E0305,
                                arr.span,
                                format!("`for` loops over a range or an array, not `{shown}`"),
                            )
                            .with_note("a `for` loop goes over a range, an array, a run, a `Vec` or an arena: wrela has no iterators"),
                        );
                        self.p.types.error
                    }
                };
                if mutable {
                    self.mark_written(&arr);
                }
                let elem = self.infer.resolve(&self.p.types, elem);
                let copy = !mutable && crate::traits::implements_builtin(self.p, elem, Lang::Copy);
                let kind = if copy {
                    LocalKind::Owned { mutable: false }
                } else {
                    LocalKind::Projection { mutable }
                };
                let var = self.declare_binder(name, elem, kind, pat.span);
                // The pattern destructures the element, as `let` does a place: its names copy
                // `Copy` parts and project the rest, mutably in a `for mut` (§6.3).
                let destructure = parts.map(|pat| {
                    let init = Expr { ty: elem, span: pat.span, kind: ExprKind::Local(var) };
                    let outer = std::mem::replace(&mut self.binding_mut, mutable);
                    let p = self.check_pat(pat, elem, true);
                    self.binding_mut = outer;
                    self.bound_once(&p);
                    if !self.irrefutable(&p) {
                        self.err(
                            Diagnostic::new(codes::E0309, pat.span, "this pattern might not match, so a `for` loop can't use it")
                                .with_help("loop over each element and `match` it, or skip the others with `let ... else { continue }`"),
                        );
                    }
                    Stmt { kind: StmtKind::Bind { pat: p, init, else_: None }, span: pat.span }
                });
                let mut b = self.check_loop_body(body);
                if let Some(d) = destructure {
                    b.stmts.insert(0, d);
                }
                StmtKind::ForEach { var, array: arr, mutable, body: b }
            }
        };
        self.env.truncate(env_len);
        Some(Stmt { kind, span })
    }

    /// Whether field `f` of a type declared in module `adt_mod` is hidden from this code: it
    /// isn't `pub` and this is another module, or it's `pub(package)` and this is another
    /// package (§3).
    pub(crate) fn field_hidden(&self, adt_mod: ModuleId, f: &crate::defs::FieldDef) -> bool {
        (!f.public && adt_mod != self.scope.module)
            || (f.package_only && !self.p.same_package(adt_mod, self.scope.module))
    }

    /// Whether a value of type `t` passes as a `str`: a `str`, a `Text` or a `String`.
    pub(crate) fn is_stringy(&self, t: TyId) -> bool {
        let t = self.infer.resolve(&self.p.types, t);
        match self.p.types.kind(t) {
            TyKind::Str => true,
            TyKind::Adt(a, _) => {
                let l = self.p.adt(*a).lang;
                l == Some(Lang::Text) || l == Some(Lang::String)
            }
            _ => false,
        }
    }

    /// The element type of a `Vec<T>`: `T`.
    pub(crate) fn vec_elem(&self, t: TyId) -> Option<TyId> {
        let t = self.infer.resolve(&self.p.types, t);
        match self.p.types.kind(t) {
            TyKind::Adt(a, args) if self.p.adt(*a).lang == Some(Lang::Vec) => args.first().copied(),
            _ => None,
        }
    }

    /// Whether a value of type `from` passes as the run `to`: an array, a `Vec` or a string
    /// as a run (a run result, a run field).
    pub(crate) fn run_coerces(&mut self, from: TyId, to: TyId) -> bool {
        match *self.kind(to) {
            TyKind::Str => self.is_stringy(from),
            TyKind::Slice(e) => match *self.kind(from) {
                TyKind::Array(a, _) | TyKind::ArrayN(a, _) => self.try_unify(a, e),
                // `Bytes` passes as a `[u8]`.
                TyKind::Adt(a, _) if self.p.adt(a).lang == Some(Lang::Bytes) => {
                    let u8 = self.p.types.int(IntTy::U8);
                    self.try_unify(u8, e)
                }
                _ => self.vec_elem(from).is_some_and(|a| self.try_unify(a, e)),
            },
            _ => false,
        }
    }

    /// The element type of what a `for` loop walks besides arrays and runs: a `Vec<T>`'s, a
    /// `Bounded<T, N>`'s or an `Arena<T>`'s (§6.6).
    pub(crate) fn walked_elem(&self, t: TyId) -> Option<TyId> {
        let t = self.infer.resolve(&self.p.types, t);
        match self.p.types.kind(t) {
            TyKind::Adt(a, args)
                if matches!(self.p.adt(*a).lang, Some(Lang::Vec | Lang::Bounded | Lang::Arena)) =>
            {
                args.first().copied()
            }
            _ => None,
        }
    }

    /// Notes that an assignment or `mut` use writes through `place`'s root local.
    pub(crate) fn mark_written(&mut self, place: &Expr) {
        if let Some(l) = root_local(place) {
            self.note_use(l, true);
        }
        // A `GlobalId` is the invocation's own (`Slots` are indexed by it): it can't change.
        let mut e = place;
        while let ExprKind::Field(b, _) | ExprKind::Swizzle(b, _) | ExprKind::Index(b, _) = &e.kind
        {
            // `v.xx` reads well, but written its two components are one place: one write would
            // be lost, or two added.
            if let ExprKind::Swizzle(_, cs) = &e.kind
                && cs.iter().enumerate().any(|(i, c)| cs[..i].contains(c))
            {
                self.err(
                    Diagnostic::new(
                        codes::E0317,
                        e.span,
                        "a swizzle that names a component twice can't be written",
                    )
                    .with_help("write each component once"),
                );
                break;
            }
            if matches!(e.kind, ExprKind::Field(..)) && self.is_global_id(b.ty) {
                self.err(
                    Diagnostic::new(codes::E0601, e.span, format!("a `{}` can't be changed", self.display(b.ty)))
                        .with_note("a `Slots` is indexed by the invocation's own `GlobalId`, and workgroup memory's chunks by its `LocalId`, so each invocation writes only its own (§6.13)")
                        .with_help("compute an index of your own from its fields: `let i = id.x + 1`"),
                );
                break;
            }
            e = b;
        }
    }

    /// Whether `t` is `std::gpu::GlobalId` or `LocalId`: the invocation's own, which `Slots`
    /// and workgroup memory's chunks are indexed by, so code can't build or change one.
    pub(crate) fn is_global_id(&self, t: TyId) -> bool {
        matches!(self.kind(t), &TyKind::Adt(a, _)
            if self.p.is_lang_adt(a, Lang::GlobalId) || self.p.is_lang_adt(a, Lang::LocalId))
    }

    pub(crate) fn resolve_type(&mut self, t: &ast::TypeExpr) -> TyId {
        let ty = crate::resolve::resolve_type(
            self.p,
            &mut self.diags,
            &self.scope,
            t,
            crate::resolve::TyPos::Local,
        );
        adt_bounds(self.p, ty, t.span, &mut self.diags);
        ty
    }

    pub(crate) fn obligation(&mut self, ty: TyId, trait_ref: TraitRef, span: Span, why: String) {
        self.obligation_coded(codes::E0400, ty, trait_ref, span, why);
    }

    /// An obligation reported with its own code when it isn't met.
    pub(crate) fn obligation_coded(
        &mut self,
        code: wrela_diag::Code,
        ty: TyId,
        trait_ref: TraitRef,
        span: Span,
        why: String,
    ) {
        self.obligations.push(Obligation { ty, trait_ref, span, code, why });
    }

    /// A parameter's or field's default where it's declared. Any other than a literal value
    /// is a constant by now (`collect`), unless its type is generic.
    fn check_literal_default(&mut self, d: &ast::Expr, ty: TyId, owner: &str) {
        let e = self.check_expect(d, ty);
        if let Some(bad) = zonk::non_literal(&e) {
            self.err(
                Diagnostic::new(
                    codes::E0324,
                    bad.span,
                    format!("a {owner}'s default of a generic type must be a literal value"),
                )
                .with_note("a constant expression has one type, which a generic type isn't (§10)"),
            );
        }
    }

    // ---- closures --------------------------------------------------------------------------

    pub(crate) fn begin_closure(&mut self, ret: TyId) -> ClosureId {
        let id = ClosureId(self.closures.len() as u32);
        self.closures.push(None);
        self.closure_stack.push(ClosureFrame {
            id,
            outer_locals: self.locals.len(),
            captures: Vec::new(),
            ret,
        });
        id
    }

    pub(crate) fn declare_closure_param(
        &mut self,
        name: &ast::Ident,
        ty: TyId,
        mode: Mode,
    ) -> LocalId {
        let kind = match mode {
            Mode::Borrow => LocalKind::ClosureParam,
            m => LocalKind::Param(m),
        };
        self.declare_local(&name.name, ty, kind, name.span)
    }

    pub(crate) fn end_closure(
        &mut self,
        params: Vec<LocalId>,
        ret: TyId,
        body: Expr,
        span: Span,
    ) -> ClosureId {
        let Some(frame) = self.closure_stack.pop() else {
            unreachable!("end_closure without begin_closure")
        };
        let id = frame.id;
        let sig: Vec<TyId> = params
            .iter()
            .map(|p| self.infer.resolve(&self.p.types, self.locals[p.index()].ty))
            .collect();
        let r = self.infer.resolve(&self.p.types, ret);
        self.p.closure_sigs.borrow_mut().insert(ClosureRef { owner: self.fn_id, id }, (sig, r));
        self.closures[id.0 as usize] =
            Some(ClosureDef { params, ret, body, captures: frame.captures, span });
        id
    }

    pub(crate) fn closure_ret(&self) -> Option<TyId> {
        self.closure_stack.last().map(|c| c.ret)
    }

    pub(crate) fn closure_def(&self, id: ClosureId) -> Option<&ClosureDef> {
        self.closures.get(id.0 as usize).and_then(|c| c.as_ref())
    }

    pub(crate) fn in_loop(&self) -> bool {
        self.loop_depth > 0
    }

    /// Runs `f` with loops reset (a closure body can't `break` out of the enclosing loop).
    pub(crate) fn without_loops<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = std::mem::replace(&mut self.loop_depth, 0);
        let r = f(self);
        self.loop_depth = saved;
        r
    }
}

/// Whether a loop body can `break` out of its loop: a `break` anywhere in it, outside nested
/// loops (whose `break`s are their own) and closures (which can't `break`; a closure's body
/// isn't inside the closure expression).
fn has_break(b: &Block) -> bool {
    fn in_expr(e: &Expr) -> bool {
        if let ExprKind::Break = e.kind {
            return true;
        }
        let mut found = false;
        e.for_each_child(&mut |c| {
            found = found
                || match c {
                    Child::Expr(x) => in_expr(x),
                    Child::Block(b) => has_break(b),
                }
        });
        found
    }
    b.stmts.iter().any(|s| match &s.kind {
        StmtKind::Expr(e) => in_expr(e),
        StmtKind::Bind { init, else_, .. } => {
            in_expr(init) || else_.as_ref().is_some_and(has_break)
        }
        StmtKind::Assign { place, value, .. } => in_expr(place) || in_expr(value),
        // A nested loop's `break`s end it (a `while` condition is part of the loop); a `for`
        // loop's range or array is evaluated before it starts.
        StmtKind::While { .. } | StmtKind::Loop { .. } => false,
        StmtKind::ForRange { start, end, .. } => in_expr(start) || in_expr(end),
        StmtKind::ForEach { array, .. } => in_expr(array),
    }) || b.tail.as_ref().is_some_and(|t| in_expr(t))
}

/// The local a place expression is rooted at, through `mut` and `take` markers.
pub fn root_local(e: &Expr) -> Option<LocalId> {
    match &e.place_root().kind {
        ExprKind::Local(l) => Some(*l),
        ExprKind::MutArg(b) | ExprKind::Take(b) => root_local(b),
        _ => None,
    }
}

/// What checking a function's body found.
pub struct FnCheck {
    pub body: Option<Body>,
    pub diags: Vec<Diagnostic>,
    /// The body holds a syntax error's node, so it may be missing parts.
    pub incomplete: bool,
}

/// Checks one function's body. `body` is `None` when there's no body to check (a required
/// trait method, or a std intrinsic).
/// E0400 for each struct or enum in `ty` whose arguments don't have its generics' bounds
/// (`S<i32>` where `struct S<T: Tr>`): no such type can exist, and its fields' types (`T::K`)
/// can't be found. Arguments not inferred yet are left to the obligations of the code that
/// builds the value ([`Checker::adt_args`]).
pub(crate) fn adt_bounds(p: &Program, ty: TyId, span: Span, out: &mut Vec<Diagnostic>) {
    let mut adts = Vec::new();
    p.types.any(ty, &mut |k| {
        if let TyKind::Adt(a, args) = k {
            adts.push((*a, args.clone()));
        }
        false
    });
    for (a, args) in adts {
        let adt = p.adt(a);
        let subst = Subst::from_pairs(&adt.generics, &args);
        for (&g, &arg) in adt.generics.iter().zip(&args) {
            if p.types.has_vars(arg) {
                continue;
            }
            let param = p.param(g);
            for b in &param.bounds {
                let r = b.subst(&p.types, &subst);
                if r.args.iter().any(|&t| p.types.has_vars(t))
                    || crate::traits::implements(p, arg, &r)
                {
                    continue;
                }
                let (t, tr) = (p.display_ty(arg), p.display_trait_ref(&r));
                let default = format!(
                    "`{t}` doesn't implement `{tr}`, which `{}` in `{}` needs",
                    param.name, adt.name
                );
                out.push(match p.custom_message(arg, &r) {
                    Some(msg) => Diagnostic::new(codes::E0400, span, msg).with_note(default),
                    None => Diagnostic::new(codes::E0400, span, default),
                });
            }
        }
    }
}

/// [`adt_bounds`] for the types every signature, field and impl header names.
pub fn check_declared_types(p: &Program) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for f in &p.fns {
        for ps in &f.params {
            adt_bounds(p, ps.ty, ps.span, &mut out);
        }
        adt_bounds(p, f.ret, f.sig_span, &mut out);
    }
    for adt in &p.adts {
        for field in adt.all_fields() {
            adt_bounds(p, field.ty, field.span, &mut out);
        }
    }
    for imp in &p.impls {
        adt_bounds(p, imp.self_ty, imp.span, &mut out);
    }
    out.extend(check_borrow_structs(p));
    out
}

/// Borrow structs (§6.6) hold projections, runs, `Copy` values and other borrow structs, and
/// are projections themselves: never a field of an ordinary type, nor a type argument.
fn check_borrow_structs(p: &Program) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let holds_borrow =
        |t: TyId| p.types.any(t, &mut |k| matches!(k, TyKind::Adt(a, _) if p.adt(*a).borrow));
    for adt in &p.adts {
        for f in adt.all_fields() {
            if adt.borrow {
                let ok = f.mode != RetMode::Owned
                    || matches!(p.types.kind(f.ty), TyKind::Slice(_) | TyKind::Str | TyKind::Error)
                    || p.is_borrow_struct(f.ty)
                    || crate::traits::implements_builtin(p, f.ty, Lang::Copy);
                if !ok {
                    out.push(
                        Diagnostic::new(
                            codes::E0519,
                            f.span,
                            format!(
                                "a borrow struct's field `{}` holds a `{}`, which isn't `Copy`",
                                f.name,
                                p.display_ty(f.ty)
                            ),
                        )
                        .with_note("a borrow struct's fields are projections, runs, `Copy` values and other borrow structs (§6.6)")
                        .with_help(format!("make it a projection: `{}: borrow {}`", f.name, p.display_ty(f.ty))),
                    );
                }
            } else if holds_borrow(f.ty) {
                out.push(
                    Diagnostic::new(
                        codes::E0327,
                        f.span,
                        format!("`{}` is a borrow struct, a projection, so it can't be a field of an ordinary type", p.display_ty(f.ty)),
                    )
                    .with_note("a borrow struct lives only as long as the places it borrows (§6.6)"),
                );
            }
        }
        if adt.borrow && !adt.opt_in.is_empty() {
            out.push(
                Diagnostic::new(
                    codes::E0519,
                    adt.opt_in[0].1,
                    format!(
                        "the borrow struct `{}` can't declare traits: it's a projection",
                        adt.name
                    ),
                )
                .with_help("pass it down; to copy what it holds, copy its fields"),
            );
        }
    }
    // As a type argument: `Vec<Ctx>`, `Option<Ctx>`.
    let mut seen = std::collections::HashSet::new();
    let mut check_ty = |t: TyId, span: Span, out: &mut Vec<Diagnostic>| {
        p.types.any(t, &mut |k| {
            if let TyKind::Adt(_, args) = k
                && args.iter().any(|&a| holds_borrow(a))
                && seen.insert(span)
            {
                out.push(
                    Diagnostic::new(
                        codes::E0327,
                        span,
                        "a borrow struct can't be a type argument: it's a projection",
                    )
                    .with_note("a projection lives only as long as the places it borrows (§6.6)"),
                );
            }
            false
        });
    };
    for f in &p.fns {
        for ps in &f.params {
            check_ty(ps.ty, ps.span, &mut out);
        }
        check_ty(f.ret, f.sig_span, &mut out);
    }
    out
}

pub fn check_fn(p: &Program, consts: &ConstTypes, f: FnId) -> FnCheck {
    let def = p.func(f);
    if def.derived.is_some() {
        let (body, diags) = crate::fieldwise::derive(p, f);
        return FnCheck { body, diags, incomplete: false };
    }
    let none = FnCheck { body: None, diags: Vec::new(), incomplete: false };
    let Some(body) = &def.body else { return none };
    if def.attrs.intrinsic {
        return none;
    }
    let mut c = Checker::new(p, consts, Scope::of_fn(p, f), Some(f));
    // A syntax error inside the body: it may be missing parts from the start.
    c.saw_syntax_error = p.syntax_errors.iter().any(|e| {
        e.file == body.span.file && body.span.start <= e.start && e.start <= body.span.end
    });
    let mut params = Vec::new();
    for ps in &def.params {
        let id = c.declare_local(&ps.name, ps.ty, LocalKind::Param(ps.mode), ps.span);
        params.push(id);
    }
    // Defaults are checked here, where they're declared, as well as where they're used.
    for ps in &def.params {
        if let Some(d) = &ps.default {
            c.check_literal_default(d, ps.ty, "parameter");
        }
    }
    let hidden =
        if def.opaque.is_some() { Some(c.new_var(VarKind::General, def.sig_span)) } else { None };
    c.ret_ty = hidden.unwrap_or(def.ret);
    let ret = c.ret_ty;
    let block = c.check_block(body, Some(ret));
    if !matches!(c.kind(block.ty), TyKind::Never) {
        let unit = c.p.types.unit;
        if block.tail.is_none()
            && c.shallow(ret) != unit
            && !matches!(c.kind(ret), TyKind::Var(_))
            && !c.saw_syntax_error
        {
            let shown = c.display(ret);
            let at = body.span.shrink_to_end();
            c.err(
                Diagnostic::new(
                    codes::E0314,
                    def.sig_span,
                    format!(
                        "`{}` returns `{shown}`, but its body can end without a value",
                        def.name
                    ),
                )
                .with_secondary(at, "the body ends here")
                .with_help("end the body with an expression of that type, or `return` one"),
            );
        } else if !c.run_coerces(block.ty, ret) {
            c.expect(block.ty, ret, block.tail.as_ref().map_or(block.span, |t| t.span));
        }
    }
    let value = Expr { ty: block.ty, kind: ExprKind::Block(block), span: body.span };
    let incomplete = c.saw_syntax_error;
    let (body, mut diags) = zonk::finish(c, params, value, hidden);
    zonk::check_opaque(p, f, body.hidden_ret, &mut diags);
    zonk::stray_markers(p, &body, def.ret_mode == RetMode::Mut, &mut diags);
    FnCheck { body: Some(body), diags, incomplete }
}

/// Checks a struct's field defaults where they're declared: literal values of the field's type.
pub fn check_field_defaults(p: &Program, consts: &ConstTypes, a: AdtId) -> Vec<Diagnostic> {
    let def = p.adt(a);
    let mut out = Vec::new();
    for f in def.fields() {
        let Some(d) = &f.default else { continue };
        let mut c = Checker::new(p, consts, Scope::new(def.module), None);
        c.check_literal_default(d, f.ty, "field");
        zonk::finish_common(&mut c);
        out.extend(c.diags);
    }
    out
}

/// The types of the program's constants, by id.
pub type ConstTypes = HashMap<ConstId, TyId>;

/// The program's checked constants.
pub struct CheckedConsts {
    pub tys: ConstTypes,
    pub values: Consts,
    /// The typed body of each constant the build computes: one whose value isn't literal
    /// (`crate::mir::build::is_literal`'s parts, other constants aside).
    pub bodies: Vec<(ConstId, Body)>,
}

/// Checks every `const` item, each after the constants its value names, so an unannotated
/// constant's type is known wherever it's used. A constant whose value refers back to itself,
/// directly or through others, is an error (E0328), and so is a constant naming one.
pub fn check_consts(p: &Program, diags: &mut Vec<Diagnostic>) -> CheckedConsts {
    let n = p.consts.len();
    let deps: Vec<Vec<ConstId>> = (0..n).map(|i| const_deps(p, ConstId(i as u32))).collect();
    // Depth-first, so each constant comes after its dependencies; a back edge is a cycle.
    let mut bad = vec![false; n];
    let mut order = Vec::new();
    dfs_cycles(
        &deps,
        |d| d.index(),
        |v| match v {
            Visit::Cycle(cycle) => {
                let cycle: Vec<usize> = cycle.iter().map(|&(x, _)| x).collect();
                report_const_cycle(p, &cycle, diags);
                for x in cycle {
                    bad[x] = true;
                }
            }
            Visit::Done(c) => {
                if deps[c].iter().any(|d| bad[d.index()]) {
                    bad[c] = true;
                }
                order.push(c);
            }
        },
    );
    let mut tys = ConstTypes::new();
    let mut values = Consts::new();
    let mut bodies = Vec::new();
    for c in order {
        let id = ConstId(c as u32);
        if bad[c] {
            tys.insert(id, p.types.error);
            continue;
        }
        let (ty, body, d) = check_const(p, &tys, id);
        let ok = !wrela_diag::has_errors(&d);
        diags.extend(d);
        tys.insert(id, ty);
        values.insert(id, (ty, body.value.clone()));
        if ok && zonk::non_literal(&body.value).is_some() {
            bodies.push((id, body));
        }
    }
    CheckedConsts { tys, values, bodies }
}

/// The constants a constant's value names, resolved where it's defined.
fn const_deps(p: &Program, c: ConstId) -> Vec<ConstId> {
    let def = p.const_(c);
    let mut out = Vec::new();
    def.value.walk(&mut |e| {
        if let ast::ExprKind::Path(path) = &e.kind
            && let Some(crate::defs::Res::Const(d)) =
                crate::resolve::resolve_value_item(p, def.module, path)
            && !out.contains(&d)
        {
            out.push(d);
        }
    });
    out
}

fn report_const_cycle(p: &Program, cycle: &[usize], diags: &mut Vec<Diagnostic>) {
    let first = p.const_(ConstId(cycle[0] as u32));
    let msg = if cycle.len() == 1 {
        format!("the constant `{}`'s value refers to itself", first.name)
    } else {
        format!(
            "the constant `{}`'s value refers back to itself through other constants",
            first.name
        )
    };
    let mut d = Diagnostic::new(codes::E0328, first.span, msg);
    for &c in &cycle[1..] {
        let def = p.const_(ConstId(c as u32));
        d = d.with_secondary(def.span, format!("`{}` is part of the cycle", def.name));
    }
    diags.push(d.with_help("give one of them a literal value"));
}

/// Checks a `const` item's value: any expression, checked as the body of the function that
/// computes it (§10), which a `return` in it returns from.
fn check_const(p: &Program, consts: &ConstTypes, id: ConstId) -> (TyId, Body, Vec<Diagnostic>) {
    let def = p.const_(id);
    let scope = Scope::new(def.module);
    let mut c = Checker::new(p, consts, scope, Some(def.eval));
    c.ret_ty = def.ty.unwrap_or_else(|| c.new_var(VarKind::General, def.span));
    let ret = c.ret_ty;
    let e = c.check_expr(&def.value, def.ty);
    if !matches!(c.kind(e.ty), TyKind::Never) {
        c.expect(e.ty, ret, e.span);
    }
    zonk::finish_const(c, e, ret)
}
