//! The body checker: types every expression of a function body and builds its typed tree
//! ([`crate::thir`]). Inference is local to the body; signatures say everything else.

mod call;
mod expr;
pub mod infer;
mod pat;
mod zonk;

use crate::defs::*;
use crate::program::Program;
use crate::resolve::Scope;
use crate::thir::*;
use crate::ty::*;
use infer::Infer;
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

struct ClosureFrame {
    id: ClosureId,
    /// Locals declared before the closure started; uses of these are captures.
    outer_locals: usize,
    captures: Vec<(LocalId, bool)>,
    ret: TyId,
}

pub(crate) struct Checker<'p> {
    pub(crate) p: &'p mut Program,
    pub(crate) diags: Vec<Diagnostic>,
    pub(crate) infer: Infer,
    pub(crate) fn_id: Option<FnId>,
    pub(crate) scope: Scope,
    pub(crate) locals: Vec<LocalDecl>,
    closures: Vec<Option<ClosureDef>>,
    /// Names in scope, innermost last.
    env: Vec<(String, LocalId)>,
    ret_ty: TyId,
    ret_mode: RetMode,
    loop_depth: u32,
    closure_stack: Vec<ClosureFrame>,
    obligations: Vec<Obligation>,
    /// Integer literals to range-check once their types are known.
    int_literals: Vec<(TyId, u64, bool, Span)>,
    /// Negations of integers whose type wasn't known yet: they must turn out signed.
    negated_ints: Vec<(TyId, Span)>,
}

impl<'p> Checker<'p> {
    fn new(p: &'p mut Program, scope: Scope, fn_id: Option<FnId>) -> Checker<'p> {
        let unit = p.types.unit;
        Checker {
            p,
            diags: Vec::new(),
            infer: Infer::default(),
            fn_id,
            scope,
            locals: Vec::new(),
            closures: Vec::new(),
            env: Vec::new(),
            ret_ty: unit,
            ret_mode: RetMode::Owned,
            loop_depth: 0,
            closure_stack: Vec::new(),
            obligations: Vec::new(),
            int_literals: Vec::new(),
            negated_ints: Vec::new(),
        }
    }

    pub(crate) fn err(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    pub(crate) fn display(&mut self, t: TyId) -> String {
        let t = self.infer.resolve(&mut self.p.types, t);
        self.p.display_ty(t)
    }

    pub(crate) fn shallow(&self, t: TyId) -> TyId {
        self.infer.shallow(&self.p.types, t)
    }

    /// `t` with its variables resolved and its projections on concrete types replaced.
    pub(crate) fn normalized(&mut self, t: TyId) -> TyId {
        let r = self.infer.resolve(&mut self.p.types, t);
        crate::traits::normalize(self.p, r, None)
    }

    /// Unifies `a` and `b` if they fit, leaving no trace if they don't.
    pub(crate) fn try_unify(&mut self, a: TyId, b: TyId) -> bool {
        let saved = self.infer.clone();
        if self.infer.unify(&mut self.p.types, a, b).is_ok() {
            true
        } else {
            self.infer = saved;
            false
        }
    }

    pub(crate) fn new_var(&mut self, kind: VarKind, span: Span) -> TyId {
        self.infer.new_var(&mut self.p.types, kind, span)
    }

    pub(crate) fn kind(&self, t: TyId) -> TyKind {
        self.p.types.kind(self.shallow(t)).clone()
    }

    fn declare_local(&mut self, name: &str, ty: TyId, kind: LocalKind, span: Span) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        let closure = self.closure_stack.last().map(|c| c.id);
        self.locals.push(LocalDecl { name: name.into(), ty, kind, span, closure });
        if name != "_" {
            self.env.push((name.into(), id));
        }
        id
    }

    /// Finds a local by name, recording a capture if it's from outside the current closure.
    pub(crate) fn lookup_local(&mut self, name: &str) -> Option<LocalId> {
        let id = self.env.iter().rev().find(|(n, _)| n == name).map(|(_, l)| *l)?;
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
        let (a, e) = (self.display(actual), self.display(expected));
        let mut d = Diagnostic::new(codes::E0300, span, format!("expected `{e}`, found `{a}`"));
        let (ka, ke) = (self.kind(actual), self.kind(expected));
        match (&ka, &ke) {
            (TyKind::Int(_) | TyKind::Float(_), TyKind::Int(_) | TyKind::Float(_)) => {
                d = d.with_help(format!("convert it explicitly: `{e}(...)`"));
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

    fn check_stmt(&mut self, s: &ast::Stmt) -> Option<Stmt> {
        let kind = match &s.kind {
            ast::StmtKind::Bind { kind, pat, ty, init } => {
                let annot = ty.as_ref().map(|t| self.resolve_type(t));
                let init = self.check_expr(init, annot);
                if let Some(a) = annot {
                    self.expect(init.ty, a, init.span);
                }
                let ty = annot.unwrap_or(init.ty);
                let pat = match kind {
                    ast::BindKind::Let => self.check_let_pattern(pat, ty, &init),
                    ast::BindKind::Var | ast::BindKind::Mut => {
                        let ast::PatKind::Ident(name) = &pat.kind else {
                            self.err(Diagnostic::new(
                                codes::E0100,
                                pat.span,
                                "`var` and `mut` bind one name",
                            ));
                            return None;
                        };
                        let mutable = true;
                        let lk = if *kind == ast::BindKind::Var {
                            LocalKind::Owned { mutable }
                        } else {
                            LocalKind::Projection { mutable }
                        };
                        let id = self.declare_local(&name.name, ty, lk, name.span);
                        Pat { ty, kind: PatKind::Bind(id), span: pat.span }
                    }
                };
                StmtKind::Bind { pat, init }
            }
            ast::StmtKind::Assign { target, op, value } => {
                let place = self.check_expr(target, None);
                self.mark_written(&place);
                let bin = op.binop();
                let value = match bin {
                    None => {
                        let v = self.check_expr(value, Some(place.ty));
                        self.expect(v.ty, place.ty, v.span);
                        v
                    }
                    Some(b) => {
                        let v = self.check_expr(value, Some(place.ty));
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
                self.loop_depth += 1;
                let b = self.check_block(body, None);
                self.loop_depth -= 1;
                self.expect_unit_block(&b);
                StmtKind::While { cond: c, body: b }
            }
            ast::StmtKind::Loop { body } => {
                self.loop_depth += 1;
                let b = self.check_block(body, None);
                self.loop_depth -= 1;
                self.expect_unit_block(&b);
                StmtKind::Loop { body: b }
            }
            ast::StmtKind::For { mutable, pat, iter, body } => {
                return self.check_for(*mutable, pat, iter, body, s.span);
            }
        };
        Some(Stmt { kind, span: s.span })
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
            if self.infer.unify(&mut self.p.types, c.ty, b).is_ok() {
                return;
            }
        }
        let shown = self.display(c.ty);
        self.err(
            Diagnostic::new(
                codes::E0313,
                c.span,
                format!("a condition must be `bool`, not `{shown}`"),
            )
            .with_help("compare it, as in `x != 0`"),
        );
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
        let ast::PatKind::Ident(name) = &pat.kind else {
            self.err(Diagnostic::new(codes::E0100, pat.span, "a `for` loop binds one name"));
            return None;
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
                let e = self.check_expr(end, Some(s.ty));
                self.expect(e.ty, s.ty, e.span);
                if !matches!(self.kind(s.ty), TyKind::Int(_) | TyKind::Error | TyKind::Never)
                    && self.infer.var_kind(&self.p.types, s.ty) != Some(VarKind::Int)
                {
                    let shown = self.display(s.ty);
                    self.err(Diagnostic::new(
                        codes::E0305,
                        s.span,
                        format!("a range's bounds must be integers, not `{shown}`"),
                    ));
                }
                let var = self.declare_local(
                    &name.name,
                    s.ty,
                    LocalKind::Owned { mutable: false },
                    name.span,
                );
                self.loop_depth += 1;
                let b = self.check_block(body, None);
                self.loop_depth -= 1;
                self.expect_unit_block(&b);
                StmtKind::ForRange { var, start: s, end: e, inclusive: *inclusive, body: b }
            }
            ast::ForIter::Expr(e) => {
                let arr = self.check_expr(e, None);
                let elem = match self.kind(arr.ty) {
                    TyKind::Array(t, _) | TyKind::Slice(t) => t,
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
                let copy = !mutable && crate::traits::implements_builtin(self.p, elem, Lang::Copy);
                let kind = if copy {
                    LocalKind::Owned { mutable: false }
                } else {
                    LocalKind::Projection { mutable }
                };
                let var = self.declare_local(&name.name, elem, kind, name.span);
                self.loop_depth += 1;
                let b = self.check_block(body, None);
                self.loop_depth -= 1;
                self.expect_unit_block(&b);
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
    }

    pub(crate) fn resolve_type(&mut self, t: &ast::TypeExpr) -> TyId {
        crate::resolve::resolve_type(
            self.p,
            &mut self.diags,
            &self.scope.clone(),
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

    pub(crate) fn ret(&self) -> (TyId, RetMode) {
        (self.ret_ty, self.ret_mode)
    }

    /// Runs `f` with loops reset (a closure body can't `break` out of the enclosing loop).
    pub(crate) fn without_loops<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = std::mem::replace(&mut self.loop_depth, 0);
        let r = f(self);
        self.loop_depth = saved;
        r
    }
}

fn has_break(b: &Block) -> bool {
    fn in_expr(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Break => true,
            ExprKind::Block(b) => has_break(b),
            ExprKind::If { then, else_, .. } => {
                has_break(then) || else_.as_ref().is_some_and(|e| in_expr(e))
            }
            ExprKind::Match { arms, .. } => arms.iter().any(|a| in_expr(&a.body)),
            _ => false,
        }
    }
    b.stmts.iter().any(|s| match &s.kind {
        StmtKind::Expr(e) => in_expr(e),
        StmtKind::Bind { init, .. } => in_expr(init),
        _ => false, // a nested loop's `break` is its own
    }) || b.tail.as_ref().is_some_and(|t| in_expr(t))
}

/// The local a place expression is rooted at.
pub fn root_local(e: &Expr) -> Option<LocalId> {
    match &e.kind {
        ExprKind::Local(l) => Some(*l),
        ExprKind::Field(b, _) | ExprKind::Swizzle(b, _) | ExprKind::Index(b, _) => root_local(b),
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
            scope.push_params(p, &p.impl_(i).generics);
        }
        FnOwner::Trait(t) => {
            let tr = p.trait_(t);
            scope.self_ty = None;
            scope.push_params(p, &tr.generics);
        }
    }
    scope.push_params(p, &def.generics);
    scope
}

/// Checks one function's body. `None` when it has no body to check (a required trait method,
/// or a std intrinsic).
pub fn check_fn(p: &mut Program, f: FnId) -> (Option<Body>, Vec<Diagnostic>) {
    let def = p.func(f).clone();
    let Some(body) = def.body.clone() else { return (None, Vec::new()) };
    if def.attrs.intrinsic {
        return (None, Vec::new());
    }
    let mut scope = fn_scope(p, f);
    if let FnOwner::Trait(t) = def.owner {
        scope.self_ty = Some(p.types.param(p.trait_(t).self_param));
    }
    let mut c = Checker::new(p, scope, Some(f));
    let mut params = Vec::new();
    for ps in &def.params {
        let id = c.declare_local(&ps.name, ps.ty, LocalKind::Param(ps.mode), ps.span);
        params.push(id);
    }
    // Defaults are checked here, where they're declared, as well as where they're used.
    for ps in &def.params {
        if let Some(d) = &ps.default {
            let e = c.check_default(d, ps.ty);
            if let Some(bad) = zonk::non_literal(&e) {
                c.err(
                    Diagnostic::new(
                        codes::E0324,
                        bad.span,
                        "a parameter's default must be a literal value",
                    )
                    .with_note("evaluating calls and arithmetic at compile time is tier 1 (D-073)"),
                );
            }
        }
    }
    let hidden =
        if def.opaque.is_some() { Some(c.new_var(VarKind::General, def.sig_span)) } else { None };
    c.ret_ty = hidden.unwrap_or(def.ret);
    c.ret_mode = def.ret_mode;
    let ret = c.ret_ty;
    let block = c.check_block(&body, Some(ret));
    if !matches!(c.kind(block.ty), TyKind::Never) {
        let unit = c.p.types.unit;
        if block.tail.is_none() && c.shallow(ret) != unit && !matches!(c.kind(ret), TyKind::Var(_))
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
    let mut out = zonk::finish(c, params, value, hidden, f);
    if let Some(b) = &mut out.0 {
        crate::check::zonk::check_opaque(p, f, b, &mut out.1);
    }
    out
}

/// Checks a struct's field defaults where they're declared: literal values of the field's type.
pub fn check_field_defaults(p: &mut Program, a: AdtId) -> Vec<Diagnostic> {
    let def = p.adt(a).clone();
    let mut out = Vec::new();
    for f in def.fields() {
        let Some(d) = &f.default else { continue };
        let mut c = Checker::new(p, Scope::new(def.module), None);
        let e = c.check_default(d, f.ty);
        if let Some(bad) = zonk::non_literal(&e) {
            c.err(
                Diagnostic::new(
                    codes::E0324,
                    bad.span,
                    "a field's default must be a literal value",
                )
                .with_note("evaluating calls and arithmetic at compile time is tier 1 (D-073)"),
            );
        }
        zonk::finish_common(&mut c);
        out.extend(c.diags);
    }
    out
}

/// Checks a `const` item's value: tier 0 allows literal values only (D-073).
pub fn check_const(p: &mut Program, id: ConstId) -> (Option<(TyId, Expr)>, Vec<Diagnostic>) {
    let def = p.const_(id).clone();
    let scope = Scope::new(def.module);
    let mut c = Checker::new(p, scope, None);
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

/// The type of a `const` without an annotation, from its value (cached; a cycle is an error).
pub fn const_type(p: &mut Program, c: ConstId) -> TyId {
    if let Some(t) = p.const_tys.get(&c) {
        return *t;
    }
    if !p.consts_in_progress.insert(c) {
        return p.types.error;
    }
    let (r, _) = check_const(p, c);
    p.consts_in_progress.remove(&c);
    let t = r.map_or(p.types.error, |(t, _)| t);
    p.const_tys.insert(c, t);
    t
}
