//! Tasks (`std::mem::task`, std's unsafe core): `task(run)` is the number of a function the
//! module calls by index (`run_task`, and the hosts' thread entries) with a context address and
//! a chunk. It calls `run(chunk, mut context)`, the context being the `C` at that address. A
//! parallel job's chunks are tasks too (`par`), made from their chunk function and arguments.

use crate::body::{Fl, Repr};
use crate::instance::Callable;
use wrela_diag::{Diagnostic, codes};
use wrela_ir as ir;
use wrela_sema::mir;
use wrela_sema::ty::TyId;

pub(crate) fn task(fl: &mut Fl, substs: &[TyId], c: &mir::Call) -> Option<ir::ValueId> {
    let span = c.span;
    if fl.is_gpu() {
        fl.cx.err(Diagnostic::new(codes::E0607, span, "a task is CPU code"));
        return None;
    }
    let ([run], [_context]) = (&c.args[..], substs) else {
        fl.cx.err(Diagnostic::internal("`task` takes a function, with its context's type"));
        return None;
    };
    // A named function: what runs on another thread can't have captures (they'd be this
    // thread's), so its state is the context.
    let callable = match fl.callable_arg(run) {
        Some(Repr::Callable(callable @ Callable::Func { .. }, srcs)) if srcs.is_empty() => callable,
        _ => {
            fl.cx.err(Diagnostic::internal("a task's function must be a named function"));
            return None;
        }
    };
    let callee = fl.cx.instance(fl.mb, callable.instance_key(), Some((fl.id, span)));
    let context = match &fl.mb.m.functions[callee.index()].params[..] {
        [chunk, context] if !chunk.by_ref && context.by_ref => context.ty,
        _ => {
            fl.cx.err(Diagnostic::internal(
                "a task's function takes a chunk, then `mut` its context",
            ));
            return None;
        }
    };
    let k = crate::par::add_task(fl, "task", callee, context, |_, _, p| {
        vec![ir::Arg::Place(ir::Place { root: ir::PlaceRoot::Ptr(p), path: Vec::new() })]
    });
    Some(fl.u32c(k))
}
