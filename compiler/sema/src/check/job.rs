//! Jobs (language.md §16.3): a `@job fn`'s `yield`, `f.start(...)` and `job.resume(...)`.
//! Starting and resuming are calls (`Callee::JobStart`, `Callee::JobResume`), so the memory
//! checker sees their arguments as it sees any call's; lowering builds the job's value and its
//! resume function from the job's MIR.

use super::Checker;
use super::call::CallParam;
use crate::defs::{Lang, Mode, RetMode};
use crate::thir::{Call, Callee, Expr, ExprKind};
use crate::ty::{FnId, TyId, TyKind};
use wrela_diag::{Diagnostic, Span, codes};
use wrela_syntax::ast;

impl<'p> Checker<'p> {
    /// `yield`: in a job's own body (not a closure's), where its work for this frame ends.
    pub(crate) fn check_yield(&mut self, span: Span) -> Expr {
        let in_job = self.fn_id.is_some_and(|f| self.p.func(f).attrs.job.is_some());
        if !in_job || !self.closure_stack.is_empty() {
            let what = if in_job { "a closure in a job" } else { "a function that isn't a job" };
            self.err(
                Diagnostic::new(codes::E0335, span, format!("`yield` in {what}"))
                    .with_help("make the function a `@job fn`, or return instead")
                    .with_note(
                        "`yield` ends a `@job fn`'s work for this frame, in its own body (§16.3)",
                    ),
            );
            return self.error_expr(span);
        }
        Expr { ty: self.p.types.unit, span, kind: ExprKind::Yield }
    }

    /// E0336 for a job named as a function: called, or used as a value.
    pub(crate) fn job_not_a_function(&mut self, f: FnId, span: Span) {
        let name = &self.p.func(f).name;
        self.err(
            Diagnostic::new(
                codes::E0336,
                span,
                format!("`{name}` is a job, which runs over several frames: it's started, not called"),
            )
            .with_help(format!(
                "make a `Job<{name}>` with `{name}.start(...)`, and run it to each `yield` with its `resume(...)`"
            )),
        );
    }

    /// `Job<f>`, its generics `args`.
    pub(crate) fn job_ty(&self, f: FnId, args: Vec<TyId>) -> TyId {
        let Some(job) = self.p.lang_adt(Lang::Job) else { return self.p.types.error };
        let def = self.p.types.intern(TyKind::FnDef(f, args));
        self.p.types.adt(job, vec![def])
    }

    /// The job a value of type `t` is of (`Job<f>`'s `f`), and its generics.
    pub(crate) fn job_of(&self, t: TyId) -> Option<(FnId, Vec<TyId>)> {
        let TyKind::Adt(a, args) = self.kind(t) else { return None };
        if !self.p.is_lang_adt(*a, Lang::Job) {
            return None;
        }
        match self.kind(*args.first()?) {
            TyKind::FnDef(f, args) => Some((*f, args.clone())),
            _ => None,
        }
    }

    /// `f.start(...)`: its owned (`take`) parameters, matched as a call's arguments are; a
    /// `Job<f>`. A generic job's types are given (`f::<T>.start(...)`) or inferred.
    pub(crate) fn job_start(
        &mut self,
        f: FnId,
        path: &ast::Path,
        args: &[ast::Arg],
        span: Span,
    ) -> Expr {
        let gen_args = self.fresh_fn_args(f, path, path.span);
        self.fn_obligations(f, &gen_args, span);
        let (params, _, _) = self.fn_params(f, &gen_args);
        let owned: Vec<CallParam> = params.into_iter().filter(|c| c.mode == Mode::Take).collect();
        let name = format!("{}.start", self.p.func(f).name);
        let (args, modes, order) = self.match_args(&name, &owned, None, args, span, f);
        let call = Call {
            callee: Callee::JobStart { func: f, args: gen_args.clone() },
            args,
            modes,
            receiver: false,
            order,
            ret_mode: RetMode::Owned,
        };
        Expr { ty: self.job_ty(f, gen_args), span, kind: ExprKind::Call(call) }
    }

    /// `job.resume(...)`: the job (`mut`, as a receiver), then the job's `borrow` and `mut`
    /// parameters, matched as a call's arguments are; a `Step` of the job's result.
    pub(crate) fn job_resume(
        &mut self,
        f: FnId,
        gen_args: Vec<TyId>,
        job: Expr,
        args: &[ast::Arg],
        span: Span,
    ) -> Expr {
        let (params, ret, _) = self.fn_params(f, &gen_args);
        let module = self.p.func(f).module;
        let receiver = CallParam {
            name: "self",
            mode: Mode::Mut,
            ty: job.ty,
            default: None,
            module,
            fn_bound: None,
        };
        let lent: Vec<CallParam> = std::iter::once(receiver)
            .chain(params.into_iter().filter(|c| c.mode != Mode::Take))
            .collect();
        let name = format!("{}.resume", self.p.func(f).name);
        let (args, modes, order) = self.match_args(&name, &lent, Some(job), args, span, f);
        let call = Call {
            callee: Callee::JobResume { func: f, args: gen_args },
            args,
            modes,
            receiver: true,
            order,
            ret_mode: RetMode::Owned,
        };
        let ty = match self.p.lang_adt(Lang::Step) {
            Some(step) => self.p.types.adt(step, vec![ret]),
            None => self.p.types.error,
        };
        Expr { ty, span, kind: ExprKind::Call(call) }
    }
}

/// E0336 for each `@job fn` that can't be one: a job is a free function (not a method, no GPU
/// or test attributes) that returns a value it owns.
pub fn check_jobs(p: &crate::Program) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for def in &p.fns {
        let Some(at) = def.attrs.job else { continue };
        let a = &def.attrs;
        let other = [
            (a.entry.map(|e| e.1), "a GPU entry point"),
            (a.gpu, "`@gpu`"),
            (a.test, "a test"),
            (a.testing, "`@testing`"),
            (a.audio, "`@audio`"),
            (a.thread_entry, "a thread's entry"),
        ];
        let why = if let Some((_, what)) = other.iter().find(|(s, _)| s.is_some()) {
            Some(format!("it's {what} too"))
        } else if !matches!(def.owner, crate::defs::FnOwner::Free) {
            Some("it's a method: a job is a free function".to_string())
        } else if def.has_self() {
            Some("it takes `self`".to_string())
        } else if def.ret_mode != RetMode::Owned {
            Some("it returns a projection, which a stopped job couldn't hold".to_string())
        } else {
            None
        };
        if let Some(why) = why {
            out.push(
                Diagnostic::new(codes::E0336, at, format!("`{}` can't be a job: {why}", def.name)).with_help("a job is a free `@job fn` whose result is owned: make it one, or call this code from one")
                    .with_secondary(def.sig_span, "declared here"),
            );
        }
    }
    out
}

impl<'p> Checker<'p> {
    /// `k.run(input, params, mut out)` (§18.14): a `@deterministic` kernel `fn k(input: [A],
    /// params: P, out: mut Slots<B>, id: GlobalId)` run on the CPU over `out`'s places, `out` a
    /// `Vec<B>`. Its shape is checked where it's lowered (E0713).
    pub(crate) fn exact_run(
        &mut self,
        f: FnId,
        path: &ast::Path,
        args: &[ast::Arg],
        span: Span,
    ) -> Expr {
        let gen_args = self.fresh_fn_args(f, path, path.span);
        self.fn_obligations(f, &gen_args, span);
        let (params, _, _) = self.fn_params(f, &gen_args);
        let out_elem = params.get(2).and_then(|c| match self.p.types.kind(c.ty) {
            TyKind::Adt(a, targs) if self.p.adt(*a).lang == Some(Lang::Slots) => {
                targs.first().copied()
            }
            _ => None,
        });
        let (Some(out_elem), Some(vec)) = (out_elem, self.p.lang_adt(Lang::Vec)) else {
            self.err(
                Diagnostic::new(
                    codes::E0713,
                    span,
                    format!("`{}.run` needs the kernel to be `fn k(input: [A], params: P, out: mut Slots<B>, id: GlobalId)`", self.p.func(f).name),
                )
                .with_help("give the kernel that shape (§18.14)"),
            );
            return Expr { ty: self.p.types.unit, span, kind: ExprKind::Error };
        };
        let out_ty = self.p.types.intern(TyKind::Adt(vec, vec![out_elem]));
        let module = self.p.func(f).module;
        let run_params = vec![
            CallParam {
                name: "input",
                mode: Mode::Borrow,
                ty: params[0].ty,
                default: None,
                module,
                fn_bound: None,
            },
            CallParam {
                name: "params",
                mode: Mode::Borrow,
                ty: params[1].ty,
                default: None,
                module,
                fn_bound: None,
            },
            CallParam {
                name: "out",
                mode: Mode::Mut,
                ty: out_ty,
                default: None,
                module,
                fn_bound: None,
            },
        ];
        let name = format!("{}.run", self.p.func(f).name);
        let (args, modes, order) = self.match_args(&name, &run_params, None, args, span, f);
        let call = Call {
            callee: Callee::ExactRun { func: f, args: gen_args },
            args,
            modes,
            receiver: false,
            order,
            ret_mode: RetMode::Owned,
        };
        Expr { ty: self.p.types.unit, span, kind: ExprKind::Call(call) }
    }
}
