//! The program's voice (`std::audio`, language.md §6.13). `start_voice(voice, render)` makes
//! `render_quantum(quantum, mut voice, render)` a task, as a parallel job's chunk function is
//! one (`par`): a function of a context and a chunk, called by index. Its context is the
//! voice's address: `play` put the voice on the heap, where it stays. The call then tells the
//! host (`wrela.audio`), whose audio thread calls `__audio(task, context)` for each quantum.

use crate::body::{Fl, Repr};
use crate::instance::InstanceKey;
use wrela_diag::{Diagnostic, codes};
use wrela_ir as ir;
use wrela_sema::defs::Lang;
use wrela_sema::mir;
use wrela_sema::ty::TyId;

pub(crate) fn start_voice(fl: &mut Fl, substs: &[TyId], c: &mir::Call) -> Option<ir::ValueId> {
    let span = c.span;
    if fl.is_gpu() {
        fl.cx.err(Diagnostic::new(codes::E0607, span, "a voice is CPU code"));
        return None;
    }
    let Some(quantum_fn) = fl.cx.checked.program.lang_fn(Lang::RenderQuantum) else {
        fl.cx.err(Diagnostic::internal("std::audio's render_quantum is missing"));
        return None;
    };
    let [voice_arg, render_arg] = &c.args[..] else {
        fl.cx.err(Diagnostic::internal("start_voice takes a voice and its render function"));
        return None;
    };
    // `render` is part of the quantum function's instance, as a call makes it; it captures
    // nothing (the checker requires it), so the instance takes only the quantum and the voice.
    let callable = match fl.callable_arg(render_arg) {
        Some(Repr::Callable(callable, srcs)) if srcs.is_empty() => callable,
        _ => {
            fl.cx.err(Diagnostic::internal("a voice's render function that captures"));
            return None;
        }
    };
    let key = InstanceKey::Fn {
        func: quantum_fn,
        substs: substs.to_vec(),
        callables: vec![None, None, Some(callable)],
        resources: Vec::new(),
    };
    let callee = fl.cx.instance(fl.mb, key, Some((fl.id, span)));
    let params = &fl.mb.m.functions[callee.index()].params;
    let voice_ty = match &params[..] {
        [quantum, voice] if !quantum.by_ref && voice.by_ref => voice.ty,
        _ => {
            fl.cx.err(Diagnostic::internal("render_quantum's instance has unexpected parameters"));
            return None;
        }
    };
    // The task passes the quantum function the voice the context points to.
    let k = crate::par::add_task(fl, "voice", callee, voice_ty, |_, _, p| {
        vec![ir::Arg::Place(ir::Place { root: ir::PlaceRoot::Ptr(p), path: Vec::new() })]
    });
    fl.mb.m.audio = true;
    let voice = fl.arg_value(voice_arg)?;
    let kv = fl.u32c(k);
    fl.emit(ir::Stmt::Eval(ir::Expr::Host(ir::HostOp::Audio, vec![kv, voice])));
    None
}
