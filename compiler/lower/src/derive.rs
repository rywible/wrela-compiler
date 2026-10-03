//! Derived interpretations (language.md §13): calls of `gradient`, `value_and_gradient` and
//! `interval`, and the instances that build them with `ir::derive`, on the CPU and on the GPU.

use crate::body::{Fl, Repr};
use crate::instance::{Callable, DeriveKind, InstanceKey};
use crate::{Cx, ModuleBuilder};
use wrela_diag::{Diagnostic, Span, codes};
use wrela_ir as ir;
use wrela_sema::defs::Lang;
use wrela_sema::mir;
use wrela_sema::ty::TyId;

/// A call of the intrinsic `lang` (`gradient`, `value_and_gradient` or `interval`): a call of
/// the derived instance of its callable.
pub(crate) fn call(
    fl: &mut Fl,
    lang: Lang,
    substs: &[TyId],
    c: &mir::Call,
    ty: Option<TyId>,
) -> Option<ir::ValueId> {
    let kind = match lang {
        Lang::IntervalOf => DeriveKind::Interval,
        _ => DeriveKind::ValueAndGradient,
    };
    let Some(Repr::Callable(callable, srcs)) = fl.callable_arg(&c.args[0]) else {
        fl.cx.err(Diagnostic::new(
            codes::E0700,
            c.args[0].span(),
            "a derived interpretation needs a closure or a named function",
        ));
        return None;
    };
    let input = substs[0];
    let key = InstanceKey::Derived { of: callable, kind, input };
    let callee = fl.cx.instance(fl.mb, key, Some((fl.id, c.span)));
    let mut args = fl.capture_args(&srcs)?;
    let x = fl.arg_value(&c.args[1])?;
    args.push(ir::Arg::Value(x));
    let ret = fl.mb.m.functions[callee.index()].ret?;
    let v = fl.value(ret, ir::Expr::Call(callee, args));
    match lang {
        Lang::Gradient => {
            // value_and_gradient returns (f32, X); gradient wants the X.
            let t = fl.ty(ty?, c.span)?;
            Some(fl.value(t, ir::Expr::Extract(v, 1)))
        }
        _ => Some(v),
    }
}

/// Where a callable is defined: a closure's expression, or a function's signature.
fn callable_span(cx: &Cx, c: &Callable) -> Span {
    let program = &cx.checked.program;
    match c {
        Callable::Func { func, .. } => program.func(*func).sig_span,
        Callable::Closure { owner, id } => {
            let f = owner.source_fn().expect("a closure's owner is a function");
            match cx.checked.mir.get(&f) {
                Some(b) => b.closures[id.0 as usize].span,
                None => program.func(f).sig_span,
            }
        }
    }
}

/// A derived instance's signature: the callable's captures, then its input (a point, or a box),
/// returning `(f32, X)` or an `Interval`.
pub(crate) fn signature(cx: &mut Cx, mb: &mut ModuleBuilder, key: &InstanceKey) -> ir::Function {
    let InstanceKey::Derived { of, kind, input } = key else {
        return ir::Function::new("derived", Vec::new(), None);
    };
    let span = callable_span(cx, of);
    let mut params = cx.callable_params(mb, of);
    let x = cx.lower_ty(mb, *input, span);
    let f32 = mb.m.types.f32();
    match kind {
        DeriveKind::ValueAndGradient => {
            if let Some(x) = x {
                params.push(ir::Param { name: "x".into(), ty: x, by_ref: false, mutable: false });
                let tuple = mb.m.types.intern(ir::TypeDef::Struct {
                    name: format!("(f32, {})", mb.m.types.display(x)),
                    fields: vec![("_0".into(), f32), ("_1".into(), x)],
                });
                return ir::Function::new("value_and_gradient", params, Some(tuple));
            }
            ir::Function::new("value_and_gradient", params, None)
        }
        DeriveKind::Interval => {
            let interval = cx
                .checked
                .program
                .lang_adt(Lang::Interval)
                .map(|a| cx.checked.program.types.adt(a, Vec::new()));
            let box_ty = box_type(cx, *input);
            let bt = box_ty.and_then(|b| cx.lower_ty(mb, b, span));
            let it = interval.and_then(|i| cx.lower_ty(mb, i, span));
            if let Some(bt) = bt {
                params.push(ir::Param {
                    name: "over".into(),
                    ty: bt,
                    by_ref: false,
                    mutable: false,
                });
            }
            ir::Function::new("interval", params, it)
        }
    }
}

/// `X::Box` for a domain type `X`.
fn box_type(cx: &mut Cx, x: TyId) -> Option<TyId> {
    let domain = cx.checked.program.lang_trait(Lang::Domain)?;
    let r = wrela_sema::defs::TraitRef { trait_: domain, args: Vec::new() };
    let (i, subst) = wrela_sema::traits::find_impl(&cx.checked.program, x, &r)?;
    let b = *cx.checked.program.impl_(i).assoc_types.get("Box")?;
    Some(cx.checked.program.types.subst(b, &subst))
}

/// A derived instance whose callable's instance is declared, waiting to be built (see
/// [`build_next`]).
pub(crate) struct Pending {
    key: InstanceKey,
    id: ir::FuncId,
    inner: ir::FuncId,
}

/// Lowers a derived instance: the callable's own instance now, and the IR transform once every
/// function that instance reaches has its body ([`build_next`]).
pub(crate) fn lower(cx: &mut Cx, mb: &mut ModuleBuilder, key: &InstanceKey, id: ir::FuncId) {
    let InstanceKey::Derived { of, .. } = key else { return };
    let at = mb.callers.get(&id).map_or_else(|| callable_span(cx, of), |c| c.1);
    let inner = cx.instance(mb, of.instance_key(), Some((id, at)));
    mb.deriving.push(Pending { key: key.clone(), id, inner });
}

/// Builds one waiting derived instance, when nothing is left to lower: one whose callable
/// reaches no other waiting one, so the derivation reads finished bodies (an empty one would
/// read as a function that returns nothing active, so its derivative would be zero). One that
/// reaches itself would need its own derivative to build it: an error.
pub(crate) fn build_next(cx: &mut Cx, mb: &mut ModuleBuilder) {
    let waiting: Vec<ir::FuncId> = mb.deriving.iter().map(|p| p.id).collect();
    let reach: Vec<Vec<ir::FuncId>> =
        mb.deriving.iter().map(|p| reached(&mb.m, p.inner, &waiting)).collect();
    if let Some(i) = reach.iter().position(Vec::is_empty) {
        let p = mb.deriving.remove(i);
        return build(cx, mb, &p.key, p.id, p.inner);
    }
    // Every one waits on another: drop those in a cycle, with an error, and the rest can go on.
    for (p, r) in std::mem::take(&mut mb.deriving).into_iter().zip(reach) {
        if !r.contains(&p.id) {
            mb.deriving.push(p);
            continue;
        }
        mb.m.functions[p.id.index()].body = vec![ir::Stmt::Trap];
        let at = mb.callers.get(&p.id).map(|c| c.1);
        let what = match &p.key {
            InstanceKey::Derived { kind: DeriveKind::Interval, .. } => "its interval",
            _ => "its gradient",
        };
        let msg = format!(
            "can't derive this function: it uses {what}, so deriving it would need its \
             derivative's derivative, without end"
        );
        cx.err(match at {
            Some(at) => Diagnostic::new(codes::E0700, at, msg),
            None => Diagnostic::internal(msg),
        });
    }
}

/// The waiting derived instances among the functions `from` reaches through calls.
fn reached(m: &ir::Module, from: ir::FuncId, waiting: &[ir::FuncId]) -> Vec<ir::FuncId> {
    let mut seen = vec![false; m.functions.len()];
    let mut stack = vec![from];
    let mut found = Vec::new();
    while let Some(f) = stack.pop() {
        if std::mem::replace(&mut seen[f.index()], true) {
            continue;
        }
        if waiting.contains(&f) {
            found.push(f);
            continue;
        }
        stack.extend(ir::visit::calls(&m.functions[f.index()].body));
    }
    found
}

/// The IR transform of a derived instance, `inner` its callable's instance.
fn build(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    key: &InstanceKey,
    id: ir::FuncId,
    inner: ir::FuncId,
) {
    let InstanceKey::Derived { kind, .. } = key else { return };
    let ncap = mb.m.functions[id.index()].params.len() - 1;
    let target = mb.target();
    let result = match kind {
        DeriveKind::ValueAndGradient => {
            ir::derive::value_and_gradient(&mut mb.m, &mut mb.derived, inner, ncap as u32, target)
        }
        DeriveKind::Interval => {
            let sig = &mb.m.functions[id.index()];
            match (sig.params.last().map(|p| p.ty), sig.ret) {
                (Some(box_ty), Some(interval_ty)) => ir::derive::interval(
                    &mut mb.m,
                    &mut mb.derived,
                    inner,
                    ncap as u32,
                    target,
                    box_ty,
                    interval_ty,
                ),
                _ => Err(ir::Error::internal("an interval's signature has no box or no result")),
            }
        }
    };
    match result {
        Ok(derived) => {
            // The instance forwards to the derived function, passing each argument the way
            // the derived function takes it (which follows the source function's parameters:
            // a borrowed aggregate by reference on the CPU).
            let takes: Vec<bool> =
                mb.m.functions[derived.index()].params.iter().map(|p| p.by_ref).collect();
            let f = &mut mb.m.functions[id.index()];
            let ret = f.ret;
            let nparams = f.params.len();
            let mut args = Vec::new();
            for (i, &by_ref) in takes.iter().enumerate().take(nparams) {
                let p = f.params[i].clone();
                let own = ir::Place::root(ir::PlaceRoot::Param(i as u32));
                let arg = match (p.by_ref, by_ref) {
                    (true, true) => ir::Arg::Place(own),
                    (false, false) => {
                        let v = f.new_value(p.ty);
                        f.body.push(ir::Stmt::Let(v, ir::Expr::Param(i as u32)));
                        ir::Arg::Value(v)
                    }
                    (false, true) => {
                        let v = f.new_value(p.ty);
                        f.body.push(ir::Stmt::Let(v, ir::Expr::Param(i as u32)));
                        let l = f.new_local(&p.name, p.ty);
                        f.body.push(ir::Stmt::Store(ir::Place::local(l), v));
                        ir::Arg::Place(ir::Place::local(l))
                    }
                    (true, false) => {
                        let v = f.new_value(p.ty);
                        f.body.push(ir::Stmt::Let(v, ir::Expr::Load(own)));
                        ir::Arg::Value(v)
                    }
                };
                args.push(arg);
            }
            match ret {
                Some(t) => {
                    let v = f.new_value(t);
                    f.body.push(ir::Stmt::Let(v, ir::Expr::Call(derived, args)));
                    f.body.push(ir::Stmt::Return(Some(v)));
                }
                None => f.body.push(ir::Stmt::Return(None)),
            }
        }
        Err(e) => {
            // Reported at the code that can't be derived, if the IR says where, with the
            // `gradient(..)` or `interval(..)` call that asked for it.
            let Some(&(_, call)) = mb.callers.get(&id) else {
                cx.err(Diagnostic::internal(format!("a derived function with no caller: {e}")));
                return;
            };
            let code = match &e {
                ir::Error::NotDerivable(..) => codes::E0700,
                ir::Error::ActiveLoopExit(..) => codes::E0701,
                ir::Error::Internal(m) => {
                    cx.err(Diagnostic::internal(format!("deriving a function failed: {m}")));
                    return;
                }
            };
            let d = match e.span() {
                Some(at) if at != call => {
                    Diagnostic::new(code, at, e.to_string()).with_secondary(call, "derived here")
                }
                _ => Diagnostic::new(code, call, format!("can't derive this function: {e}")),
            };
            cx.err(d);
        }
    }
}
