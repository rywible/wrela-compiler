//! Jobs (language.md §6.18): a `@job fn`'s value, its start and its `resume`.
//!
//! A job's value (`Job<f>`) is a struct: where it stopped (`at`: the MIR block it goes on at,
//! `0` before it starts, [`DONE`] after its end), then each owned local of its body (its `take`
//! parameters among them), a field each. `f.start(...)` builds it with the parameters in their
//! fields and zeros in the rest. `resume` is an instance of its own: it moves each field into its
//! local, and runs the body from `at` to its next `yield` (where it moves the locals back into
//! the value and gives `Step::Yielded`) or to its end (`Step::Done`, with the result). The body
//! isn't nested as the MIR's structure is, which can't be entered in the middle: it's a loop
//! round a choice of the block to run (`crate::body`'s job mode). A generic job's value and
//! `resume` are laid out for each of its instantiations.

use crate::body::Fl;
use crate::instance::InstanceKey;
use crate::{Cx, ModuleBuilder};
use wrela_diag::Span;
use wrela_ir as ir;
use wrela_sema::defs::{Lang, Mode, RetMode};
use wrela_sema::mir::{self, Local};
use wrela_sema::ty::*;

/// `at` once a job has run to its end.
pub(crate) const DONE: u32 = u32::MAX;

/// A job's value as the IR lays it out.
#[derive(Clone, Debug)]
pub(crate) struct JobLayout {
    pub ty: ir::TypeId,
    /// Each local the value holds, and its field (field 0 is `at`).
    pub fields: Vec<(Local, u32)>,
}

impl Cx<'_> {
    /// The locals a job's value holds: the owned locals of its body (not of its closures').
    fn job_locals(&self, f: FnId) -> Vec<Local> {
        let Some(body) = self.checked.mir.get(&f) else { return Vec::new() };
        let held = |d: &mir::LocalDecl| d.closure.is_none() && d.kind.owns_value();
        (0..body.locals.len() as u32).map(Local).filter(|&l| held(body.local(l))).collect()
    }

    /// The substitution of job `f`'s generics by `substs`.
    pub(crate) fn job_subst(&self, f: FnId, substs: &[TyId]) -> Subst {
        Subst::from_pairs(&self.checked.program.fn_all_generics(f), substs)
    }

    /// `Job<f>`'s layout, its generics `substs` (concrete).
    pub(crate) fn job_layout(
        &mut self,
        mb: &mut ModuleBuilder,
        f: FnId,
        substs: &[TyId],
    ) -> Option<JobLayout> {
        // One layout whatever names its types: `Grown`, or the type it names.
        let substs: Vec<TyId> = substs.iter().map(|&t| self.checked.reveal(t)).collect();
        let substs = &substs[..];
        let key = (f, substs.to_vec());
        if let Some(l) = mb.jobs.get(&key) {
            return Some(l.clone());
        }
        let checked = self.checked;
        let body = checked.mir.get(&f)?;
        let subst = self.job_subst(f, substs);
        let u = mb.m.types.u32();
        let mut fields = vec![("at".to_string(), u)];
        let mut held = Vec::new();
        for l in self.job_locals(f) {
            let d = body.local(l);
            let t = self.concrete(d.ty, &subst);
            if let Some(it) = self.lower_ty(mb, t, d.span) {
                held.push((l, fields.len() as u32));
                fields.push((format!("{}_{}", ir::ident(&d.name), l.index()), it));
            }
        }
        let name = checked.program.display_ty(self.job_ty(f, substs));
        let ty = mb.m.types.intern(ir::TypeDef::Struct { name, fields });
        let layout = JobLayout { ty, fields: held };
        mb.jobs.insert(key, layout.clone());
        Some(layout)
    }

    /// `Job<f>`, its generics `substs`.
    fn job_ty(&self, f: FnId, substs: &[TyId]) -> TyId {
        let p = &self.checked.program;
        match p.lang_adt(Lang::Job) {
            Some(job) => p.types.adt(job, vec![p.types.intern(TyKind::FnDef(f, substs.to_vec()))]),
            None => p.types.error,
        }
    }

    /// The fields of `Job<f>`, as `field_map` gives a struct's: `at`, then each local it holds.
    pub(crate) fn job_fields(
        &mut self,
        mb: &mut ModuleBuilder,
        f: FnId,
        substs: &[TyId],
    ) -> Vec<(Option<u32>, TyId)> {
        let Some(layout) = self.job_layout(mb, f, substs) else { return Vec::new() };
        let checked = self.checked;
        let Some(body) = checked.mir.get(&f) else { return Vec::new() };
        let subst = self.job_subst(f, substs);
        let mut out = vec![(Some(0), checked.program.types.u32)];
        for &(l, k) in &layout.fields {
            out.push((Some(k), self.concrete(body.local(l).ty, &subst)));
        }
        out
    }

    /// The job `t` is a value of (`Job<f>`'s `f`), and its generics.
    pub(crate) fn job_of(&self, t: TyId) -> Option<(FnId, Vec<TyId>)> {
        let p = &self.checked.program;
        match p.types.kind(t) {
            TyKind::Adt(a, args) if p.is_lang_adt(*a, Lang::Job) => match args.first() {
                Some(&f) => match p.types.kind(f) {
                    TyKind::FnDef(f, args) => Some((*f, args.clone())),
                    _ => None,
                },
                None => None,
            },
            _ => None,
        }
    }

    /// `Step<R>`, for job `f`'s result `R`, its generics `substs`.
    pub(crate) fn step_ty(&mut self, f: FnId, substs: &[TyId]) -> TyId {
        let subst = self.job_subst(f, substs);
        let ret = self.concrete(self.checked.program.func(f).ret, &subst);
        let p = &self.checked.program;
        match p.lang_adt(Lang::Step) {
            Some(step) => p.types.adt(step, vec![ret]),
            None => p.types.error,
        }
    }

    /// The signature of `f`'s `resume`: the job (by reference), then the job's `borrow` and
    /// `mut` parameters as a call passes them; a `Step` of its result.
    pub(crate) fn job_signature(
        &mut self,
        mb: &mut ModuleBuilder,
        f: FnId,
        substs: &[TyId],
    ) -> ir::Function {
        let fname = ir::ident(&self.checked.program.func(f).name);
        let name = format!("{fname}_resume_{}", mb.m.functions.len());
        let Some(layout) = self.job_layout(mb, f, substs) else {
            return ir::Function::new(name, Vec::new(), None);
        };
        let mut params =
            vec![ir::Param { name: "job".into(), ty: layout.ty, by_ref: true, mutable: true }];
        let subst = self.job_subst(f, substs);
        let def = self.checked.program.func(f);
        for p in def.params.iter().filter(|p| p.mode != Mode::Take) {
            let t = self.concrete(p.ty, &subst);
            let Some(ty) = self.lower_ty(mb, t, p.span) else { continue };
            let (by_ref, mutable) =
                crate::ty::param_passing(mb.target(), &mb.m.types, ty, p.mode, RetMode::Owned);
            params.push(ir::Param { name: p.name.clone(), ty, by_ref, mutable });
        }
        let step = self.step_ty(f, substs);
        let ret = self.lower_ty(mb, step, def.sig_span);
        ir::Function::new(name, params, ret)
    }
}

/// `f.start(...)`: `Job<f>`'s value, its `take` parameters in their fields, zeros in the rest.
pub(crate) fn start(
    fl: &mut Fl,
    f: FnId,
    substs: &[TyId],
    args: &[mir::Arg],
) -> Option<ir::ValueId> {
    let layout = fl.cx.job_layout(fl.mb, f, substs)?;
    let checked = fl.cx.checked;
    let body = checked.mir.get(&f)?;
    let def = checked.program.func(f);
    let job = fl.new_local("job", layout.ty);
    let zero = fl.value(layout.ty, ir::Expr::Zero(layout.ty));
    fl.emit(ir::Stmt::Store(ir::Place::local(job), zero));
    let owned = def.params.iter().zip(&body.fns[0].params).filter(|(p, _)| p.mode == Mode::Take);
    for ((_, &local), a) in owned.zip(args) {
        let v = fl.arg_value(a);
        if let (Some(v), Some(&(_, k))) = (v, layout.fields.iter().find(|(l, _)| *l == local)) {
            fl.emit(ir::Stmt::Store(ir::Place::local(job).with(ir::Proj::Field(k)), v));
        }
    }
    Some(fl.load(ir::Place::local(job), layout.ty))
}

/// `job.resume(...)`: a call of `f`'s `resume` instance, with the job's place and the job's
/// `borrow` and `mut` arguments.
pub(crate) fn resume(
    fl: &mut Fl,
    f: FnId,
    substs: Vec<TyId>,
    args: &[mir::Arg],
    span: Span,
) -> Option<ir::ValueId> {
    let def = fl.cx.checked.program.func(f);
    let lent: Vec<(Mode, TyId)> =
        def.params.iter().filter(|p| p.mode != Mode::Take).map(|p| (p.mode, p.ty)).collect();
    let subst = fl.cx.job_subst(f, &substs);
    let (job, rest) = args.split_first()?;
    let mut out = vec![ir::Arg::Place(fl.place(job.place()?)?)];
    for ((mode, t), a) in lent.into_iter().zip(rest) {
        let t = fl.cx.concrete(t, &subst);
        let Some(ty) = fl.cx.lower_ty(fl.mb, t, span) else { continue };
        let (by_ref, _) =
            crate::ty::param_passing(fl.mb.target(), &fl.mb.m.types, ty, mode, RetMode::Owned);
        let run = matches!(fl.mb.m.types.get(ty), ir::TypeDef::Run(_));
        if by_ref {
            let place = match a {
                mir::Arg::Mut(pl, _) => fl.place(pl),
                mir::Arg::Borrow(pl, _) => fl.place_or_copy(pl),
                mir::Arg::Take(o) => {
                    let v = fl.operand(o)?;
                    Some(fl.temp_place(v))
                }
            };
            out.push(ir::Arg::Place(place?));
        } else {
            let v = match a.place() {
                Some(pl) if run => fl.run_arg(pl, ty)?,
                _ => fl.arg_value(a)?,
            };
            out.push(ir::Arg::Value(v));
        }
    }
    let substs = substs.iter().map(|&t| fl.cx.checked.reveal(t)).collect();
    let key = InstanceKey::JobResume { func: f, substs };
    let callee = fl.cx.instance(fl.mb, key, Some((fl.id, span)));
    fl.emit_call(callee, out)
}
