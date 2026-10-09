//! Jobs (language.md §6.18): a `@job fn`'s `yield`, `f.start(...)` and `job.resume(...)`.
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
            self.err(Diagnostic::new(codes::E0335, span, format!("`yield` in {what}")).with_note(
                "`yield` ends a `@job fn`'s work for this frame, in its own body (§6.18)",
            ));
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

    /// `Job<f>`.
    pub(crate) fn job_ty(&self, f: FnId) -> TyId {
        let Some(job) = self.p.lang_adt(Lang::Job) else { return self.p.types.error };
        let def = self.p.types.intern(TyKind::FnDef(f, Vec::new()));
        self.p.types.adt(job, vec![def])
    }

    /// The job a value of type `t` is of (`Job<f>`'s `f`).
    pub(crate) fn job_of(&self, t: TyId) -> Option<FnId> {
        match self.kind(t) {
            TyKind::Adt(a, args) if self.p.is_lang_adt(*a, Lang::Job) => match args.first() {
                Some(&f) => match self.kind(f) {
                    TyKind::FnDef(f, _) => Some(*f),
                    _ => None,
                },
                None => None,
            },
            _ => None,
        }
    }

    /// `f.start(...)`: its owned (`take`) parameters, matched as a call's arguments are; a
    /// `Job<f>`.
    pub(crate) fn job_start(&mut self, f: FnId, args: &[ast::Arg], span: Span) -> Expr {
        let (params, _, _) = self.fn_params(f, &[]);
        let owned: Vec<CallParam> = params.into_iter().filter(|c| c.mode == Mode::Take).collect();
        let name = format!("{}.start", self.p.func(f).name);
        let (args, modes, order) = self.match_args(&name, &owned, None, args, span, f);
        let call = Call {
            callee: Callee::JobStart(f),
            args,
            modes,
            receiver: false,
            order,
            ret_mode: RetMode::Owned,
        };
        Expr { ty: self.job_ty(f), span, kind: ExprKind::Call(call) }
    }

    /// `job.resume(...)`: the job (`mut`, as a receiver), then the job's `borrow` and `mut`
    /// parameters, matched as a call's arguments are; a `Step` of the job's result.
    pub(crate) fn job_resume(&mut self, f: FnId, job: Expr, args: &[ast::Arg], span: Span) -> Expr {
        let (params, ret, _) = self.fn_params(f, &[]);
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
            callee: Callee::JobResume(f),
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

/// E0336 for each `@job fn` that can't be one: a job is a plain function (no generics, no
/// `self`, no GPU or test attributes) that returns a value it owns.
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
        } else if !def.generics.is_empty() || !matches!(def.owner, crate::defs::FnOwner::Free) {
            Some("it's generic or a method: a job is a plain function, for now".to_string())
        } else if def.has_self() {
            Some("it takes `self`".to_string())
        } else if def.ret_mode != RetMode::Owned {
            Some("it returns a projection, which a stopped job couldn't hold".to_string())
        } else {
            None
        };
        if let Some(why) = why {
            out.push(
                Diagnostic::new(codes::E0336, at, format!("`{}` can't be a job: {why}", def.name))
                    .with_secondary(def.sig_span, "declared here"),
            );
        }
    }
    out
}
