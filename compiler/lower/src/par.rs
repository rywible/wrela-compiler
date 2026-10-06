//! Parallel jobs (`std::par`, language.md §6.12). `par_each(chunks, args...)` (and
//! `par_map_reduce`) runs its chunk function, `par_each_chunk(chunk, chunks, args...)`, for each
//! chunk on this thread and on the program's workers. The job becomes a task: a function of a
//! context address and a chunk, called by index from the module's table. The context holds the
//! chunk function's other arguments as its instance takes them (values, or a place's address,
//! with a closure argument's captures in its place); it's on this thread's stack, in the memory
//! the workers share, and `run_chunks` returns only when every chunk is done, so it outlives
//! the job. The helpers that run the other chunks enter through std's `@thread_entry` worker.

use crate::body::{Fl, Repr};
use crate::instance::InstanceKey;
use wrela_diag::{Diagnostic, codes};
use wrela_ir as ir;
use wrela_sema::defs::Lang;
use wrela_sema::mir;
use wrela_sema::ty::TyId;

pub(crate) fn par_job(
    fl: &mut Fl,
    lang: Lang,
    substs: &[TyId],
    c: &mir::Call,
) -> Option<ir::ValueId> {
    let span = c.span;
    if fl.is_gpu() {
        fl.cx.err(Diagnostic::new(codes::E0607, span, "a parallel job is CPU code"));
        return None;
    }
    let program = &fl.cx.checked.program;
    let chunk_fn = program.lang_fn(match lang {
        Lang::ParEach => Lang::ParEachChunk,
        _ => Lang::ParMapReduceChunk,
    });
    let (Some(chunk_fn), Some(run)) = (chunk_fn, program.lang_fn(Lang::RunChunks)) else {
        fl.cx.err(Diagnostic::internal("std::par's scheduler is missing"));
        return None;
    };
    // The chunk function's instance: its callable arguments are part of it, as a call makes it.
    let mut callables = vec![None];
    let mut captures = Vec::new();
    for a in &c.args {
        match fl.callable_arg(a) {
            Some(Repr::Callable(callable, srcs)) => {
                callables.push(Some(callable));
                captures.push(Some(srcs));
            }
            _ => {
                callables.push(None);
                captures.push(None);
            }
        }
    }
    let substs: Vec<TyId> = substs.to_vec();
    let key = InstanceKey::Fn { func: chunk_fn, substs, callables, resources: Vec::new() };
    let callee = fl.cx.instance(fl.mb, key, Some((fl.id, span)));
    // Its IR parameters after the chunk: each argument's, a closure's captures in its place.
    let by_ref: Vec<bool> =
        fl.mb.m.functions[callee.index()].params.iter().map(|p| p.by_ref).collect();
    let mut args = Vec::new();
    for (a, caps) in c.args.iter().zip(&captures) {
        match caps {
            Some(srcs) => args.extend(fl.capture_args(srcs)?),
            None if by_ref.get(1 + args.len()).copied().unwrap_or(false) => {
                let pl = fl.place(a.place()?)?;
                args.push(ir::Arg::Place(pl));
            }
            None => {
                if let Some(v) = fl.arg_value(a) {
                    args.push(ir::Arg::Value(v));
                }
            }
        }
    }
    if 1 + args.len() != by_ref.len() {
        fl.cx
            .err(Diagnostic::internal("a parallel job's arguments don't match its chunk function"));
        return None;
    }
    // The context: each argument's value, or its place's address.
    let u = fl.mb.m.types.u32();
    let mut fields = Vec::new();
    let mut vals = Vec::new();
    let mut is_place = Vec::new();
    for (i, a) in args.iter().enumerate() {
        let (t, v) = match a {
            ir::Arg::Place(pl) => {
                let t = fl.place_ty(pl)?;
                let pt = fl.mb.m.types.intern(ir::TypeDef::Ptr(t));
                (pt, fl.value(pt, ir::Expr::Addr(pl.clone())))
            }
            ir::Arg::Value(v) => (fl.f.value_ty(*v), *v),
        };
        fields.push((format!("arg{i}"), t));
        vals.push(v);
        is_place.push(matches!(a, ir::Arg::Place(_)));
    }
    let field_tys: Vec<ir::TypeId> = fields.iter().map(|&(_, t)| t).collect();
    let ct = fl.mb.m.types.intern(ir::TypeDef::Struct { name: "TaskContext".into(), fields });
    let v = fl.value(ct, ir::Expr::Construct(ct, vals));
    let l = fl.f.new_local("task_context", ct);
    let place = ir::Place::root(ir::PlaceRoot::Local(l));
    fl.emit(ir::Stmt::Store(place.clone(), v));
    let pt = fl.mb.m.types.intern(ir::TypeDef::Ptr(ct));
    let p = fl.value(pt, ir::Expr::Addr(place));
    let ctx = fl.value(u, ir::Expr::Mem(ir::MemOp::Addr, vec![p]));
    // The task passes the chunk function the context's fields, each a value or, where
    // `is_place`, a place's address.
    let k = add_task(fl, "task", callee, ct, |f, body, p| {
        let mut args = Vec::new();
        for (i, (t, place)) in field_tys.into_iter().zip(is_place).enumerate() {
            let at =
                ir::Place { root: ir::PlaceRoot::Ptr(p), path: vec![ir::Proj::Field(i as u32)] };
            let v = f.new_value(t);
            body.push(ir::Stmt::Let(v, ir::Expr::Load(at)));
            args.push(if place {
                ir::Arg::Place(ir::Place { root: ir::PlaceRoot::Ptr(v), path: Vec::new() })
            } else {
                ir::Arg::Value(v)
            });
        }
        args
    });
    let rc = fl.cx.instance(fl.mb, InstanceKey::plain(run, Vec::new()), Some((fl.id, span)));
    let n = fl.arg_value(c.args.first()?)?;
    let kv = fl.u32c(k);
    let call = vec![ir::Arg::Value(kv), ir::Arg::Value(ctx), ir::Arg::Value(n)];
    fl.emit(ir::Stmt::Eval(ir::Expr::Call(rc, call)));
    None
}

/// Adds the module's next task, `k`, and returns `k`: a function `(context, chunk)` named
/// `{name}{k}`, which calls `callee` with the chunk and the arguments `args` makes (in the
/// function, before the call) from a pointer to the context, of type `context`.
pub(crate) fn add_task(
    fl: &mut Fl,
    name: &str,
    callee: ir::FuncId,
    context: ir::TypeId,
    args: impl FnOnce(&mut ir::Function, &mut ir::Block, ir::ValueId) -> Vec<ir::Arg>,
) -> u32 {
    let k = fl.mb.m.tasks.len() as u32;
    let types = &mut fl.mb.m.types;
    let u = types.u32();
    let pt = types.intern(ir::TypeDef::Ptr(context));
    let param = |name: &str| ir::Param { name: name.into(), ty: u, by_ref: false, mutable: false };
    let mut f = ir::Function::new(format!("{name}{k}"), vec![param("ctx"), param("chunk")], None);
    let ctx = f.new_value(u);
    let chunk = f.new_value(u);
    let p = f.new_value(pt);
    let mut body = vec![
        ir::Stmt::Let(ctx, ir::Expr::Param(0)),
        ir::Stmt::Let(chunk, ir::Expr::Param(1)),
        ir::Stmt::Let(p, ir::Expr::Mem(ir::MemOp::Ptr, vec![ctx])),
    ];
    let mut call = vec![ir::Arg::Value(chunk)];
    call.extend(args(&mut f, &mut body, p));
    body.push(ir::Stmt::Eval(ir::Expr::Call(callee, call)));
    body.push(ir::Stmt::Return(None));
    f.body = body;
    let task = fl.mb.m.add_function(f);
    fl.mb.m.tasks.push(task);
    k
}
