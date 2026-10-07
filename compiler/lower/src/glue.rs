//! Glue the compiler writes for each concrete type (language.md §6.1, §6.11): dropping a value
//! (its destructor, then its fields', in order, leaving zeros), and cloning one, field by field,
//! through std's hand-written leaves (`Vec`'s and `Box`'s `Clone`). Each is a function of its
//! own, so a type that holds itself through a `Box` or a `Vec` has finite glue.

use crate::instance::InstanceKey;
use crate::{Cx, ModuleBuilder};
use wrela_diag::{Diagnostic, Span};
use wrela_ir as ir;
use wrela_sema::defs::Lang;
use wrela_sema::traits;
use wrela_sema::ty::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlueKind {
    /// `drop(mut value)`: runs the destructors, then leaves zeros.
    Drop,
    /// `clone(value) -> T`.
    Clone,
    /// `clone_into(value, mut into)`: reuses what `into` owns.
    CloneInto,
}

impl<'a> Cx<'a> {
    /// Whether dropping a value of the concrete type `t` does anything.
    pub fn needs_drop(&self, t: TyId) -> bool {
        traits::may_need_drop(&self.checked.program, t)
    }

    /// Whether `t` is `Copy`: cloning it is a copy.
    pub fn is_copy(&self, t: TyId) -> bool {
        traits::implements_builtin(&self.checked.program, t, Lang::Copy)
    }

    /// The function trait method `method` runs for the concrete `self_ty` (an impl's, or the
    /// trait's default), with all its type arguments, given the trait's (`trait_args`) and the
    /// method's own (`method_args`). `None` if nothing implements it.
    pub fn resolve_impl(
        &self,
        method: FnId,
        self_ty: TyId,
        trait_args: &[TyId],
        method_args: &[TyId],
    ) -> Option<(FnId, Vec<TyId>)> {
        let program = &self.checked.program;
        let (func, subst) =
            traits::resolve_trait_method(program, method, self_ty, trait_args, method_args)?;
        let substs = program
            .fn_all_generics(func)
            .iter()
            .map(|g| self.checked.reveal(subst.get(*g).unwrap_or(program.types.error)))
            .collect();
        Some((func, substs))
    }

    /// The instance a trait method `name` of lang trait `lang` runs for `self_ty`, if an impl
    /// (or the trait's default) has one: `Drop::drop`, `Clone::clone_into`.
    pub fn lang_method(&mut self, lang: Lang, name: &str, self_ty: TyId) -> Option<InstanceKey> {
        let program = &self.checked.program;
        let tr = program.lang_trait(lang)?;
        let method = traits::trait_method(program, tr, name)?;
        let (func, substs) = self.resolve_impl(method, self_ty, &[], &[])?;
        if program.func(func).attrs.intrinsic || !self.checked.mir.contains_key(&func) {
            return None;
        }
        Some(InstanceKey::plain(func, substs))
    }

    /// The destructor std's core wrote for `t` (`Vec`'s, `Box`'s), if it has one.
    fn drop_impl(&mut self, t: TyId) -> Option<InstanceKey> {
        let program = &self.checked.program;
        let TyKind::Adt(a, _) = program.types.kind(t) else { return None };
        traits::explicit_drop(program, *a)?;
        self.lang_method(Lang::Drop, "drop", t)
    }

    /// Whether `t`'s `Clone` is written by hand in std's core (`Vec`, `Box`).
    fn explicit_clone(&self, t: TyId) -> bool {
        let program = &self.checked.program;
        match program.types.kind(t) {
            TyKind::Adt(a, _) => traits::explicit_clone(program, *a).is_some(),
            _ => false,
        }
    }

    /// Where a type is declared, for glue's diagnostics (which name it).
    pub fn type_span(&self, t: TyId) -> Span {
        let program = &self.checked.program;
        match program.types.kind(t) {
            TyKind::Adt(a, _) => program.adt(*a).span,
            _ => Span::new(wrela_diag::FileId(0), 0, 0),
        }
    }

    pub fn glue_signature(
        &mut self,
        mb: &mut ModuleBuilder,
        kind: GlueKind,
        t: TyId,
    ) -> ir::Function {
        let n = mb.m.functions.len();
        let span = self.type_span(t);
        let Some(ty) = self.lower_ty(mb, t, span) else {
            return ir::Function::new(format!("glue_{n}"), Vec::new(), None);
        };
        let param =
            |name: &str, mutable| ir::Param { name: name.into(), ty, by_ref: true, mutable };
        let shown = ir::ident(&self.checked.program.display_ty(t));
        match kind {
            GlueKind::Drop => {
                ir::Function::new(format!("drop_{shown}_{n}"), vec![param("value", true)], None)
            }
            GlueKind::Clone => ir::Function::new(
                format!("clone_{shown}_{n}"),
                vec![param("value", false)],
                Some(ty),
            ),
            GlueKind::CloneInto => ir::Function::new(
                format!("clone_into_{shown}_{n}"),
                vec![param("value", false), param("into", true)],
                None,
            ),
        }
    }
}

/// Builds a glue instance's body.
pub(crate) fn lower(cx: &mut Cx, mb: &mut ModuleBuilder, kind: GlueKind, t: TyId, id: ir::FuncId) {
    let f = std::mem::take(&mut mb.m.functions[id.index()]);
    let span = mb.callers.get(&id).map_or_else(|| cx.type_span(t), |c| c.1);
    let mut g = G { cx, mb, f, id, blocks: vec![Vec::new()], span };
    let Some(ty) = g.cx.lower_ty(g.mb, t, span) else {
        g.finish();
        return;
    };
    let p0 = ir::Place::root(ir::PlaceRoot::Param(0));
    match kind {
        GlueKind::Drop => {
            g.drop_body(t, ty, &p0);
            g.emit(ir::Stmt::Return(None));
        }
        GlueKind::Clone => {
            let v = g.clone_body(t, ty, &p0);
            g.emit(ir::Stmt::Return(v));
        }
        GlueKind::CloneInto => {
            let p1 = ir::Place::root(ir::PlaceRoot::Param(1));
            g.clone_into_body(t, ty, &p0, &p1);
            g.emit(ir::Stmt::Return(None));
        }
    }
    g.finish();
}

/// A glue function being built.
struct G<'c, 'a> {
    cx: &'c mut Cx<'a>,
    mb: &'c mut ModuleBuilder,
    f: ir::Function,
    id: ir::FuncId,
    /// The block being emitted is the last.
    blocks: Vec<ir::Block>,
    /// Where the glue was first needed.
    span: Span,
}

/// The parts of a value: each field's place and type, for a struct, a tuple or a variant.
type Parts = Vec<(ir::Place, TyId)>;

impl G<'_, '_> {
    fn finish(mut self) {
        self.f.body = self.blocks.pop().unwrap_or_default();
        self.mb.m.functions[self.id.index()] = self.f;
    }

    fn emit(&mut self, s: ir::Stmt) {
        self.blocks.last_mut().expect("a block is open").push(s);
    }

    fn value(&mut self, ty: ir::TypeId, e: ir::Expr) -> ir::ValueId {
        let v = self.f.new_value(ty);
        self.emit(ir::Stmt::Let(v, e));
        v
    }

    fn u32c(&mut self, v: u32) -> ir::ValueId {
        let u = self.mb.m.types.u32();
        self.value(u, ir::Expr::Const(ir::Const::U32(v)))
    }

    fn load(&mut self, p: &ir::Place, ty: ir::TypeId) -> ir::ValueId {
        self.value(ty, ir::Expr::Load(p.clone()))
    }

    /// `if cond { body }`.
    fn when(&mut self, cond: ir::ValueId, body: impl FnOnce(&mut Self)) {
        self.blocks.push(Vec::new());
        body(self);
        let then = self.blocks.pop().unwrap_or_default();
        self.emit(ir::Stmt::If { cond, then, else_: Vec::new() });
    }

    /// `if cond { a } else { b }`.
    fn branch(&mut self, cond: ir::ValueId, a: impl FnOnce(&mut Self), b: impl FnOnce(&mut Self)) {
        self.blocks.push(Vec::new());
        a(self);
        let then = self.blocks.pop().unwrap_or_default();
        self.blocks.push(Vec::new());
        b(self);
        let else_ = self.blocks.pop().unwrap_or_default();
        self.emit(ir::Stmt::If { cond, then, else_ });
    }

    /// `for i in 0..n { body(i) }`.
    fn counted(&mut self, n: u32, body: impl FnOnce(&mut Self, ir::ValueId)) {
        let u = self.mb.m.types.u32();
        let b = self.mb.m.types.bool();
        let i = self.f.new_local("i", u);
        let zero = self.u32c(0);
        self.emit(ir::Stmt::Store(ir::Place::local(i), zero));
        self.blocks.push(Vec::new());
        let iv = self.load(&ir::Place::local(i), u);
        let end = self.u32c(n);
        let done = self.value(b, ir::Expr::Binary(ir::BinOp::Ge, iv, end));
        self.emit(ir::Stmt::If { cond: done, then: vec![ir::Stmt::Break], else_: Vec::new() });
        body(self, iv);
        let lb = self.blocks.pop().unwrap_or_default();
        self.blocks.push(Vec::new());
        let iv = self.load(&ir::Place::local(i), u);
        let one = self.u32c(1);
        let next = self.value(u, ir::Expr::Binary(ir::BinOp::Add, iv, one));
        self.emit(ir::Stmt::Store(ir::Place::local(i), next));
        let cb = self.blocks.pop().unwrap_or_default();
        self.emit(ir::Stmt::Loop { body: lb, continuing: cb });
    }

    /// Calls instance `key` with `args` (places for by-reference parameters): its result.
    fn call(&mut self, key: InstanceKey, args: Vec<ir::Place>) -> Option<ir::ValueId> {
        let callee = self.cx.instance(self.mb, key, Some((self.id, self.span)));
        self.call_id(callee, args)
    }

    fn call_id(&mut self, callee: ir::FuncId, args: Vec<ir::Place>) -> Option<ir::ValueId> {
        let (params, ret) = if callee == self.id {
            (self.f.params.clone(), self.f.ret)
        } else {
            let f = &self.mb.m.functions[callee.index()];
            (f.params.clone(), f.ret)
        };
        let mut out = Vec::new();
        for (p, a) in params.iter().zip(args) {
            out.push(if p.by_ref {
                ir::Arg::Place(a)
            } else {
                ir::Arg::Value(self.load(&a, p.ty))
            });
        }
        let call = ir::Expr::Call(callee, out);
        match ret {
            Some(t) => Some(self.value(t, call)),
            None => {
                self.emit(ir::Stmt::Eval(call));
                None
            }
        }
    }

    /// The glue of `kind` for `t` called on `args`.
    fn glue(&mut self, kind: GlueKind, t: TyId, args: Vec<ir::Place>) -> Option<ir::ValueId> {
        self.call(InstanceKey::Glue { kind, ty: t }, args)
    }

    /// The fields of the value at `p`, a `t`: of a struct or tuple, or of variant `v` of an
    /// enum.
    fn parts(&mut self, t: TyId, p: &ir::Place, variant: Option<u32>) -> Parts {
        let span = self.span;
        let base = match variant {
            Some(v) => match self.cx.payload_field(self.mb, t, v, span) {
                Some(k) => p.with(ir::Proj::Field(k)),
                None => return Vec::new(),
            },
            None => p.clone(),
        };
        self.cx
            .field_map(self.mb, t, variant, span)
            .into_iter()
            .filter_map(|(k, ft)| {
                let ft = self.cx.checked.reveal(ft);
                k.map(|k| (base.with(ir::Proj::Field(k)), ft))
            })
            .collect()
    }

    /// The variants of enum `t` (empty for any other type).
    fn variants(&self, t: TyId) -> u32 {
        let program = &self.cx.checked.program;
        match program.types.kind(t) {
            TyKind::Adt(a, _) if program.adt(*a).is_enum() => {
                program.adt(*a).variants().len() as u32
            }
            _ => 0,
        }
    }

    fn is_enum(&self, t: TyId) -> bool {
        let program = &self.cx.checked.program;
        matches!(program.types.kind(t), TyKind::Adt(a, _) if program.adt(*a).is_enum())
    }

    /// Runs `body` for the variant the enum at `p` holds: `if tag == v { body(v) }` for each.
    fn each_variant(&mut self, t: TyId, p: &ir::Place, mut body: impl FnMut(&mut Self, u32)) {
        let u = self.mb.m.types.u32();
        let b = self.mb.m.types.bool();
        let tag = self.load(&p.with(ir::Proj::Field(0)), u);
        for v in 0..self.variants(t) {
            let k = self.u32c(v);
            let is = self.value(b, ir::Expr::Binary(ir::BinOp::Eq, tag, k));
            self.when(is, |g| body(g, v));
        }
    }

    // ---- drop ----------------------------------------------------------------------------

    fn drop_body(&mut self, t: TyId, ty: ir::TypeId, p: &ir::Place) {
        if let Some(key) = self.cx.drop_impl(t) {
            self.call(key, vec![p.clone()]);
        }
        let program = &self.cx.checked.program;
        match program.types.kind(t).clone() {
            TyKind::Array(e, n) => {
                if self.cx.needs_drop(e) && self.cx.lower_ty(self.mb, e, self.span).is_some() {
                    self.counted(n, |g, i| {
                        g.glue(GlueKind::Drop, e, vec![p.with(ir::Proj::Index(i))]);
                    });
                }
            }
            TyKind::Adt(..) if self.is_enum(t) => {
                self.each_variant(t, p, |g, v| {
                    for (fp, ft) in g.parts(t, p, Some(v)) {
                        if g.cx.needs_drop(ft) {
                            g.glue(GlueKind::Drop, ft, vec![fp]);
                        }
                    }
                });
            }
            TyKind::Adt(..) | TyKind::Tuple(_) => {
                for (fp, ft) in self.parts(t, p, None) {
                    if self.cx.needs_drop(ft) {
                        self.glue(GlueKind::Drop, ft, vec![fp]);
                    }
                }
            }
            _ => {}
        }
        // Zeros: dropped again, it's nothing (a moved-from value is zeros too).
        let z = self.value(ty, ir::Expr::Zero(ty));
        self.emit(ir::Stmt::Store(p.clone(), z));
    }

    // ---- clone ---------------------------------------------------------------------------

    /// A clone of the value at `p`, a `t`.
    fn clone_of(&mut self, t: TyId, ty: ir::TypeId, p: &ir::Place) -> Option<ir::ValueId> {
        if self.cx.is_copy(t) {
            return Some(self.load(p, ty));
        }
        self.glue(GlueKind::Clone, t, vec![p.clone()])
    }

    fn clone_body(&mut self, t: TyId, ty: ir::TypeId, p: &ir::Place) -> Option<ir::ValueId> {
        if self.cx.is_copy(t) {
            return Some(self.load(p, ty));
        }
        if self.cx.explicit_clone(t) {
            let key = self.cx.lang_method(Lang::Clone, "clone", t)?;
            return self.call(key, vec![p.clone()]);
        }
        let program = &self.cx.checked.program;
        match program.types.kind(t).clone() {
            TyKind::Array(e, n) => {
                let et = self.cx.lower_ty(self.mb, e, self.span)?;
                let out = self.f.new_local("clone", ty);
                let z = self.value(ty, ir::Expr::Zero(ty));
                self.emit(ir::Stmt::Store(ir::Place::local(out), z));
                self.counted(n, |g, i| {
                    if let Some(v) = g.clone_of(e, et, &p.with(ir::Proj::Index(i))) {
                        g.emit(ir::Stmt::Store(ir::Place::local(out).with(ir::Proj::Index(i)), v));
                    }
                });
                Some(self.load(&ir::Place::local(out), ty))
            }
            TyKind::Adt(..) if self.is_enum(t) => {
                let out = self.f.new_local("clone", ty);
                let z = self.value(ty, ir::Expr::Zero(ty));
                self.emit(ir::Stmt::Store(ir::Place::local(out), z));
                self.each_variant(t, p, |g, v| {
                    let mut vals = Vec::new();
                    for (fp, ft) in g.parts(t, p, Some(v)) {
                        let Some(fty) = g.cx.lower_ty(g.mb, ft, g.span) else { continue };
                        if let Some(x) = g.clone_of(ft, fty, &fp) {
                            vals.push(x);
                        }
                    }
                    let payload =
                        g.mb.m
                            .types
                            .field(ty, 1 + v)
                            .map(|pt| g.value(pt, ir::Expr::Construct(pt, vals)));
                    let x = g.value(ty, ir::Expr::Variant(ty, v, payload));
                    g.emit(ir::Stmt::Store(ir::Place::local(out), x));
                });
                Some(self.load(&ir::Place::local(out), ty))
            }
            TyKind::Adt(..) | TyKind::Tuple(_) => {
                let mut vals = Vec::new();
                for (fp, ft) in self.parts(t, p, None) {
                    let fty = self.cx.lower_ty(self.mb, ft, self.span)?;
                    vals.push(self.clone_of(ft, fty, &fp)?);
                }
                Some(self.value(ty, ir::Expr::Construct(ty, vals)))
            }
            _ => {
                let shown = self.cx.checked.program.display_ty(t);
                self.cx.err(Diagnostic::internal(format!("no clone for `{shown}` at lowering")));
                None
            }
        }
    }

    /// Clones the value at `src` into `dst`, both `t`s.
    fn clone_into_of(&mut self, t: TyId, ty: ir::TypeId, src: &ir::Place, dst: &ir::Place) {
        if self.cx.is_copy(t) {
            let v = self.load(src, ty);
            self.emit(ir::Stmt::Store(dst.clone(), v));
            return;
        }
        self.glue(GlueKind::CloneInto, t, vec![src.clone(), dst.clone()]);
    }

    fn clone_into_body(&mut self, t: TyId, ty: ir::TypeId, src: &ir::Place, dst: &ir::Place) {
        if self.cx.is_copy(t) {
            let v = self.load(src, ty);
            self.emit(ir::Stmt::Store(dst.clone(), v));
            return;
        }
        if self.cx.explicit_clone(t) {
            if let Some(key) = self.cx.lang_method(Lang::Clone, "clone_into", t) {
                self.call(key, vec![src.clone(), dst.clone()]);
            }
            return;
        }
        let program = &self.cx.checked.program;
        match program.types.kind(t).clone() {
            TyKind::Array(e, n) => {
                let Some(et) = self.cx.lower_ty(self.mb, e, self.span) else { return };
                self.counted(n, |g, i| {
                    let (s, d) = (src.with(ir::Proj::Index(i)), dst.with(ir::Proj::Index(i)));
                    g.clone_into_of(e, et, &s, &d);
                });
            }
            TyKind::Adt(..) if self.is_enum(t) => {
                // The same variant: field by field, reusing what `dst` owns. Another: `dst` is
                // dropped and replaced by a clone.
                let u = self.mb.m.types.u32();
                let b = self.mb.m.types.bool();
                let ts = self.load(&src.with(ir::Proj::Field(0)), u);
                let td = self.load(&dst.with(ir::Proj::Field(0)), u);
                let same = self.value(b, ir::Expr::Binary(ir::BinOp::Eq, ts, td));
                self.branch(
                    same,
                    |g| {
                        g.each_variant(t, src, |g, v| {
                            let s = g.parts(t, src, Some(v));
                            let d = g.parts(t, dst, Some(v));
                            for ((sp, ft), (dp, _)) in s.into_iter().zip(d) {
                                let Some(fty) = g.cx.lower_ty(g.mb, ft, g.span) else {
                                    continue;
                                };
                                g.clone_into_of(ft, fty, &sp, &dp);
                            }
                        });
                    },
                    |g| {
                        if g.cx.needs_drop(t) {
                            g.glue(GlueKind::Drop, t, vec![dst.clone()]);
                        }
                        if let Some(v) = g.glue(GlueKind::Clone, t, vec![src.clone()]) {
                            g.emit(ir::Stmt::Store(dst.clone(), v));
                        }
                    },
                );
            }
            TyKind::Adt(..) | TyKind::Tuple(_) => {
                let s = self.parts(t, src, None);
                let d = self.parts(t, dst, None);
                for ((sp, ft), (dp, _)) in s.into_iter().zip(d) {
                    let Some(fty) = self.cx.lower_ty(self.mb, ft, self.span) else { continue };
                    self.clone_into_of(ft, fty, &sp, &dp);
                }
            }
            _ => {
                if let Some(v) = self.clone_body(t, ty, src) {
                    self.emit(ir::Stmt::Store(dst.clone(), v));
                }
            }
        }
    }
}
