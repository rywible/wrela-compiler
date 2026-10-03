//! The body checker: types every expression of a function body and builds its typed tree
//! ([`crate::thir`]). Inference is local to the body; signatures say everything else.

mod call;
mod expr;
pub mod infer;
mod pat;
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
use wrela_diag::{Diagnostic, Span, codes};
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
    names: Vec<(String, LocalId)>,
    by_name: HashMap<String, Vec<LocalId>>,
}

impl Env {
    fn push(&mut self, name: &str, id: LocalId) {
        self.names.push((name.into(), id));
        self.by_name.entry(name.into()).or_default().push(id);
    }

    fn len(&self) -> usize {
        self.names.len()
    }

    /// Forgets the names declared after the first `len`.
    fn truncate(&mut self, len: usize) {
        while self.names.len() > len {
            let Some((name, _)) = self.names.pop() else { break };
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
        self.names.iter().map(|(n, _)| n.as_str())
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
    float_literals: Vec<(TyId, f64, Span)>,
    /// Negations of integers whose type wasn't known yet: they must turn out signed.
    negated_ints: Vec<(TyId, Span)>,
    /// `base ** exp` types, to check once the base's type is known: an integer's exponent is
    /// a `u32`, a float's a float.
    pows: Vec<(TyId, TyId, Span)>,
    /// Built-in calls whose argument types held literal types not settled yet: checked again
    /// once they are.
    builtin_calls: Vec<(crate::builtins::BuiltinFn, Vec<TyId>, Span)>,
    /// Projections in a call's signature on types not inferred yet (`T::Out` while `T` is a
    /// variable), each with the variable that stands for it until it can be normalized.
    projections: Vec<(TyId, TyId, Span)>,
    /// Operands of integer-only operators (`<<`, `&`, `!`, ...) whose literal type wasn't
    /// settled yet: they must not turn out floats.
    int_ops: Vec<(TyId, &'static str, Span)>,
    /// Pattern bindings declared before their types were known, and whether each binds into a
    /// place: their kinds are decided at the end ([`Checker::binding_kinds`]).
    pending_bindings: Vec<(LocalId, bool)>,
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
            projections: Vec::new(),
            int_ops: Vec::new(),
            pending_bindings: Vec::new(),
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

    /// Whether `t` is a number literal's type not settled yet.
    pub(crate) fn is_number_var(&self, t: TyId) -> bool {
        matches!(self.infer.var_kind(&self.p.types, t), Some(VarKind::Int | VarKind::Float))
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
        self.locals.push(LocalDecl { name, ty, kind, span, keyword: None, closure });
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
        let is_error = |k: &TyKind| matches!(k, TyKind::Error);
        if self.p.types.any(na, &mut { is_error }) || self.p.types.any(ne, &mut { is_error }) {
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
            (TyKind::Float(_), TyKind::Vec(n)) | (TyKind::Int(_), TyKind::Vec(n)) => {
                d = d.with_help(format!("make a vector from it with `vec{n}(...)`"));
            }
            (TyKind::Vec(n), TyKind::Float(_)) => {
                d = d.with_help(format!(
                    "a `vec{n}` isn't a number; take a component (`.x`) or its `length()`"
                ));
            }
            (TyKind::Tuple(t), _) if t.is_empty() => {
                d = d.with_note("a block's value is its last line when that's an expression; a statement has no value");
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
            StmtKind::Expr(e) => matches!(self.p.types.kind(self.shallow(e.ty)), TyKind::Never),
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
            ast::StmtKind::Let { pat, ty, init } => {
                let (ty, init) = self.check_init(ty.as_ref(), init);
                let p = self.check_let_pattern(pat, ty, &init);
                if let PatKind::Bind(id) = p.kind {
                    let k = Span::new(s.span.file, s.span.start, s.span.start + 3);
                    self.locals[id.index()].keyword = Some(k);
                }
                StmtKind::Bind { pat: p, init }
            }
            ast::StmtKind::Var { kind, name, ty, init } => {
                let (ty, init) = self.check_init(ty.as_ref(), init);
                let lk = match kind {
                    ast::VarKind::Var => LocalKind::Owned { mutable: true },
                    ast::VarKind::Mut => {
                        // Writes through it write the place it projects.
                        self.mark_written(&init);
                        LocalKind::Projection { mutable: true }
                    }
                };
                let id = self.declare_local(&name.name, ty, lk, name.span);
                StmtKind::Bind { pat: Pat { ty, kind: PatKind::Bind(id), span: name.span }, init }
            }
            ast::StmtKind::Assign { target, op, value } => {
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
                        let result = self.binary_result(b, place.ty, v.ty, s.span);
                        if let Some(r) = result {
                            self.expect(r, place.ty, s.span);
                        }
                        v
                    }
                };
                StmtKind::Assign { place, op: bin, value }
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
            ast::StmtKind::Loop { body } => StmtKind::Loop { body: self.check_loop_body(body) },
            ast::StmtKind::For { mutable, pat, iter, body } => {
                return self.check_for(*mutable, pat, iter, body, s.span);
            }
        };
        Some(Stmt { kind, span: s.span })
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

    fn check_for(
        &mut self,
        mutable: bool,
        pat: &ast::Pat,
        iter: &ast::ForIter,
        body: &ast::Block,
        span: Span,
    ) -> Option<Stmt> {
        let env_len = self.env.len();
        // One name, or `_` for a loop that doesn't use it.
        let name = match &pat.kind {
            ast::PatKind::Ident(name) => Some(name),
            ast::PatKind::Wild => None,
            _ => {
                self.err(Diagnostic::new(codes::E0100, pat.span, "a `for` loop binds one name"));
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
                }
                let var =
                    self.declare_binder(name, s.ty, LocalKind::Owned { mutable: false }, pat.span);
                let b = self.check_loop_body(body);
                StmtKind::ForRange { var, start: s, end: e, inclusive: *inclusive, body: b }
            }
            ast::ForIter::Expr(e) => {
                let arr = self.check_expr(e, None);
                let elem = match self.kind(arr.ty) {
                    TyKind::Array(t, _) | TyKind::Slice(t) => *t,
                    TyKind::Error => self.p.types.error,
                    _ => {
                        let shown = self.display(arr.ty);
                        self.err(
                            Diagnostic::new(
                                codes::E0305,
                                arr.span,
                                format!("`for` loops over a range or an array, not `{shown}`"),
                            )
                            .with_note("iterators are tier 1"),
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
                let b = self.check_loop_body(body);
                StmtKind::ForEach { var, array: arr, mutable, body: b }
            }
        };
        self.env.truncate(env_len);
        Some(Stmt { kind, span })
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
            if matches!(e.kind, ExprKind::Field(..)) && self.is_global_id(b.ty) {
                self.err(
                    Diagnostic::new(codes::E0601, e.span, "a `GlobalId` can't be changed")
                        .with_note("a `Slots` is indexed by the invocation's own `GlobalId`, so each invocation writes only its own slot (§6.13)")
                        .with_help("compute an index of your own from its fields: `let i = id.x + 1`"),
                );
                break;
            }
            e = b;
        }
    }

    /// Whether `t` is `std::gpu::GlobalId`.
    pub(crate) fn is_global_id(&self, t: TyId) -> bool {
        matches!(self.kind(t), &TyKind::Adt(a, _) if self.p.is_lang_adt(a, Lang::GlobalId))
    }

    pub(crate) fn resolve_type(&mut self, t: &ast::TypeExpr) -> TyId {
        crate::resolve::resolve_type(
            self.p,
            &mut self.diags,
            &self.scope,
            t,
            crate::resolve::TyPos::Normal,
        )
    }

    pub(crate) fn obligation(&mut self, ty: TyId, trait_ref: TraitRef, span: Span, why: String) {
        self.obligations.push(Obligation { ty, trait_ref, span, code: codes::E0400, why });
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

    /// A parameter's or field's default where it's declared: a literal value of its type.
    fn check_literal_default(&mut self, d: &ast::Expr, ty: TyId, owner: &str) {
        let e = self.check_expect(d, ty);
        if let Some(bad) = zonk::non_literal(&e) {
            self.err(
                Diagnostic::new(
                    codes::E0324,
                    bad.span,
                    format!("a {owner}'s default must be a literal value"),
                )
                .with_note("evaluating calls and arithmetic at compile time is tier 1 (D-073)"),
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

    pub(crate) fn declare_closure_param(&mut self, name: &ast::Ident, ty: TyId) -> LocalId {
        self.declare_local(&name.name, ty, LocalKind::ClosureParam, name.span)
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
        StmtKind::Expr(e) | StmtKind::Bind { init: e, .. } => in_expr(e),
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

/// The scope a function's signature and body see.
fn fn_scope(p: &Program, f: FnId) -> Scope {
    let def = p.func(f);
    let mut scope = Scope::new(def.module);
    match def.owner {
        FnOwner::Free => {}
        FnOwner::Impl(i) => {
            scope.self_ty = Some(p.impl_(i).self_ty);
            scope.impl_ = Some(i);
            scope.push_params(p, &p.impl_(i).generics);
        }
        FnOwner::Trait(t) => {
            let tr = p.trait_(t);
            scope.self_ty = Some(p.types.param(tr.self_param));
            scope.push_params(p, &tr.generics);
        }
    }
    scope.push_params(p, &def.generics);
    scope
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
pub fn check_fn(p: &Program, consts: &ConstTypes, f: FnId) -> FnCheck {
    let def = p.func(f);
    let none = FnCheck { body: None, diags: Vec::new(), incomplete: false };
    let Some(body) = &def.body else { return none };
    if def.attrs.intrinsic {
        return none;
    }
    let mut c = Checker::new(p, consts, fn_scope(p, f), Some(f));
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
        } else {
            c.expect(block.ty, ret, block.tail.as_ref().map_or(block.span, |t| t.span));
        }
    }
    let value = Expr { ty: block.ty, kind: ExprKind::Block(block), span: body.span };
    let incomplete = c.saw_syntax_error;
    let (body, mut diags) = zonk::finish(c, params, value, hidden);
    zonk::check_opaque(p, f, body.hidden_ret, &mut diags);
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

/// Checks every `const` item, each after the constants its value names, so an unannotated
/// constant's type is known wherever it's used. A constant whose value refers back to itself,
/// directly or through others, is an error (E0328), and so is a constant naming one.
pub fn check_consts(p: &Program, diags: &mut Vec<Diagnostic>) -> (ConstTypes, Consts) {
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
    let mut out = Consts::new();
    for c in order {
        let id = ConstId(c as u32);
        if bad[c] {
            tys.insert(id, p.types.error);
            continue;
        }
        let ((ty, value), d) = check_const(p, &tys, id);
        diags.extend(d);
        tys.insert(id, ty);
        out.insert(id, (ty, value));
    }
    (tys, out)
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

/// Checks a `const` item's value: tier 0 allows literal values only (D-073).
fn check_const(p: &Program, consts: &ConstTypes, id: ConstId) -> ((TyId, Expr), Vec<Diagnostic>) {
    let def = p.const_(id);
    let scope = Scope::new(def.module);
    let mut c = Checker::new(p, consts, scope, None);
    if let Some(bad) = non_constant(&def.value) {
        c.err(
            Diagnostic::new(codes::E0906, bad.span, "a constant's value must be a literal value in tier 0")
                .with_note("literals, struct and array literals, vector constructors and other constants are allowed; evaluating calls and arithmetic at compile time is tier 1 (D-073)"),
        );
    }
    let e = c.check_expr(&def.value, def.ty);
    if let Some(t) = def.ty {
        c.expect(e.ty, t, e.span);
    }
    zonk::finish_const(c, e)
}

/// The first part of `e` that isn't a literal value (D-073's tier-0 constants), if any.
pub fn non_constant(e: &ast::Expr) -> Option<&ast::Expr> {
    match &e.kind {
        ast::ExprKind::Lit(_) | ast::ExprKind::Path(_) => None,
        ast::ExprKind::Unary(ast::UnOp::Neg, inner)
            if matches!(inner.kind, ast::ExprKind::Lit(_)) =>
        {
            None
        }
        ast::ExprKind::Paren(inner) => non_constant(inner),
        ast::ExprKind::Tuple(xs) | ast::ExprKind::Array(xs) => xs.iter().find_map(non_constant),
        ast::ExprKind::ArrayRepeat { value, count } => {
            non_constant(value).or_else(|| non_constant(count))
        }
        ast::ExprKind::StructLit { fields, base, .. } => fields
            .iter()
            .filter_map(|f| f.value.as_ref())
            .find_map(non_constant)
            .or_else(|| base.as_deref().and_then(non_constant)),
        // A constructor: a built-in vector type, or an enum's tuple variant.
        ast::ExprKind::Call { callee, args, .. }
            if matches!(callee.kind, ast::ExprKind::Path(_)) =>
        {
            args.iter().find_map(|a| non_constant(&a.value))
        }
        _ => Some(e),
    }
}
