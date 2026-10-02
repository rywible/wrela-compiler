//! Types: from the checker's to the IR's, and instance signatures.

use crate::instance::{Callable, InstanceKey};
use crate::{Cx, ModuleBuilder};
use wrela_diag::{Diagnostic, Span, codes};
use wrela_ir as ir;
use wrela_sema::defs::{Mode, RetMode};
use wrela_sema::thir;
use wrela_sema::ty::*;

/// How a parameter of type `ty` and `mode` is passed: `(by_ref, mutable)`. `mut` is by
/// reference; on the CPU a borrowed aggregate is too (no copy); everything else is by value.
pub(crate) fn param_passing(
    target: ir::Target,
    types: &ir::Types,
    ty: ir::TypeId,
    mode: Mode,
) -> (bool, bool) {
    match mode {
        Mode::Mut => (true, true),
        Mode::Borrow => (
            target == ir::Target::Cpu
                && types.is_aggregate(ty)
                && !matches!(types.get(ty), ir::TypeDef::Run(_)),
            false,
        ),
        Mode::Take => (false, false),
    }
}

/// How a capture of a closure crosses into the closure's lifted function.
pub(crate) fn capture_passing(
    target: ir::Target,
    types: &ir::Types,
    ty: ir::TypeId,
    written: bool,
) -> (bool, bool) {
    if written {
        (true, true)
    } else if target == ir::Target::Cpu && types.is_aggregate(ty) {
        (true, false)
    } else {
        (false, false)
    }
}

impl<'a> Cx<'a> {
    /// `t` with `subst` applied, projections normalized and return-position trait types
    /// replaced by the types their functions return.
    pub fn concrete(&mut self, t: TyId, subst: &Subst) -> TyId {
        let p = &self.checked.program;
        let t = p.types.subst(t, subst);
        let t = wrela_sema::traits::normalize(p, t, None);
        self.reveal(t)
    }

    /// Replaces `Opaque(f, args)` by `f`'s hidden return type, instantiated.
    pub fn reveal(&mut self, t: TyId) -> TyId {
        if !self.checked.program.types.any(t, &mut |k| matches!(k, TyKind::Opaque(..))) {
            return t;
        }
        let k = self.checked.program.types.kind(t).clone();
        match k {
            TyKind::Opaque(f, args) => {
                let Some(hidden) = self.checked.bodies.get(&f).and_then(|b| b.hidden_ret) else {
                    return self.checked.program.types.error;
                };
                let generics = self.checked.program.fn_all_generics(f);
                let subst = Subst::from_pairs(&generics, &args);
                self.concrete(hidden, &subst)
            }
            TyKind::Tuple(ts) => {
                let ts = ts.iter().map(|&x| self.reveal(x)).collect();
                self.checked.program.types.intern(TyKind::Tuple(ts))
            }
            TyKind::Adt(a, ts) => {
                let ts = ts.iter().map(|&x| self.reveal(x)).collect();
                self.checked.program.types.intern(TyKind::Adt(a, ts))
            }
            TyKind::Array(e, n) => {
                let e = self.reveal(e);
                self.checked.program.types.intern(TyKind::Array(e, n))
            }
            TyKind::Slice(e) => {
                let e = self.reveal(e);
                self.checked.program.types.intern(TyKind::Slice(e))
            }
            _ => t,
        }
    }

    /// The substitution of an instance's source function's generics.
    pub fn instance_subst(&self, key: &InstanceKey) -> Subst {
        match key.source_fn() {
            Some(f) => Subst::from_pairs(&self.checked.program.fn_all_generics(f), key.substs()),
            None => Subst::new(),
        }
    }

    /// The IR type of a concrete type; `None` for types with no runtime value (the unit tuple,
    /// `!`, closures and functions).
    pub fn lower_ty(&mut self, mb: &mut ModuleBuilder, t: TyId, span: Span) -> Option<ir::TypeId> {
        if let Some(&c) = mb.type_cache.get(&t) {
            return c;
        }
        let k = self.checked.program.types.kind(t).clone();
        let gpu = mb.target == ir::Target::Gpu;
        let out = match k {
            TyKind::Bool => Some(mb.m.types.bool()),
            TyKind::Int(i) => {
                let s = match i {
                    IntTy::I8 => ir::Scalar::I8,
                    IntTy::U8 => ir::Scalar::U8,
                    IntTy::I16 => ir::Scalar::I16,
                    IntTy::U16 => ir::Scalar::U16,
                    IntTy::I32 => ir::Scalar::I32,
                    IntTy::U32 => ir::Scalar::U32,
                    IntTy::I64 => ir::Scalar::I64,
                    IntTy::U64 => ir::Scalar::U64,
                };
                if gpu && !s.on_gpu() {
                    self.cpu_only(mb, s.name(), span);
                }
                Some(mb.m.types.scalar(s))
            }
            TyKind::Float(f) => {
                let s = if f == FloatTy::F32 { ir::Scalar::F32 } else { ir::Scalar::F64 };
                if gpu && !s.on_gpu() {
                    self.cpu_only(mb, s.name(), span);
                }
                Some(mb.m.types.scalar(s))
            }
            TyKind::Vec(n) => Some(mb.m.types.vector(n)),
            TyKind::Mat(n) => Some(mb.m.types.intern(ir::TypeDef::Matrix(n))),
            TyKind::Tuple(ts) => {
                if ts.is_empty() {
                    None
                } else {
                    let mut fields = Vec::new();
                    for (i, &e) in ts.iter().enumerate() {
                        if let Some(f) = self.lower_ty(mb, e, span) {
                            fields.push((format!("_{i}"), f));
                        }
                    }
                    let name = self.checked.program.display_ty(t);
                    Some(mb.m.types.intern(ir::TypeDef::Struct { name, fields }))
                }
            }
            TyKind::Array(e, n) => {
                self.lower_ty(mb, e, span).map(|e| mb.m.types.intern(ir::TypeDef::Array(e, n)))
            }
            TyKind::Slice(e) => {
                let e = self.lower_ty(mb, e, span)?;
                Some(mb.m.types.intern(if gpu {
                    ir::TypeDef::RuntimeArray(e)
                } else {
                    ir::TypeDef::Run(e)
                }))
            }
            TyKind::Adt(a, args) => {
                let name = self.checked.program.display_ty(t);
                if self.checked.program.adt(a).is_enum() {
                    let u32_ty = mb.m.types.u32();
                    let mut fields = vec![("tag".to_string(), u32_ty)];
                    let nvar = self.checked.program.adt(a).variants().len();
                    for v in 0..nvar {
                        let vfields = self.checked.program.variant_fields(a, &args, v);
                        let vname = self.checked.program.adt(a).variants()[v].name.clone();
                        let mut payload = Vec::new();
                        for (n, ft) in vfields {
                            if let Some(f) = self.lower_ty(mb, ft, span) {
                                payload.push((n, f));
                            }
                        }
                        if !payload.is_empty() {
                            let pt = mb.m.types.intern(ir::TypeDef::Struct {
                                name: format!("{name}::{vname}"),
                                fields: payload,
                            });
                            fields.push((vname, pt));
                        }
                    }
                    Some(mb.m.types.intern(ir::TypeDef::Struct { name, fields }))
                } else {
                    let fs = self.checked.program.struct_fields(a, &args);
                    let mut fields = Vec::new();
                    for (n, ft) in fs {
                        let ft = self.reveal(ft);
                        if let Some(f) = self.lower_ty(mb, ft, span) {
                            fields.push((n, f));
                        }
                    }
                    if fields.is_empty() {
                        None
                    } else {
                        Some(mb.m.types.intern(ir::TypeDef::Struct { name, fields }))
                    }
                }
            }
            TyKind::Opaque(..) => {
                let r = self.reveal(t);
                self.lower_ty(mb, r, span)
            }
            TyKind::Never
            | TyKind::Closure(..)
            | TyKind::FnPtr(..)
            | TyKind::FnDef(..)
            | TyKind::Error => None,
            TyKind::Param(_) | TyKind::Projection { .. } | TyKind::Var(_) => {
                let shown = self.checked.program.display_ty(t);
                self.err(Diagnostic::new(
                    codes::E0702,
                    span,
                    format!("internal: a type that should be concrete reached lowering: `{shown}`"),
                ));
                None
            }
        };
        mb.type_cache.insert(t, out);
        out
    }

    fn cpu_only(&mut self, mb: &ModuleBuilder, name: &str, span: Span) {
        let what = mb.gpu.as_ref().map(|g| g.describe()).unwrap_or_else(|| "GPU code".into());
        self.err(
            Diagnostic::new(codes::E0326, span, format!("`{name}` is CPU-only, and this is {what}"))
                .with_note("WGSL has `bool`, `i32`, `u32` and `f32`; 64-bit and 8/16-bit types exist only on the CPU (language.md §4)"),
        );
    }

    /// For each THIR field of a struct (or variant), its IR field index; `None` when the field
    /// has no runtime value.
    pub fn field_map(
        &mut self,
        mb: &mut ModuleBuilder,
        t: TyId,
        variant: Option<u32>,
        span: Span,
    ) -> Vec<Option<u32>> {
        let k = self.checked.program.types.kind(t).clone();
        let fields: Vec<TyId> = match (&k, variant) {
            (TyKind::Adt(a, args), None) => {
                self.checked.program.struct_fields(*a, args).into_iter().map(|(_, t)| t).collect()
            }
            (TyKind::Adt(a, args), Some(v)) => self
                .checked
                .program
                .variant_fields(*a, args, v as usize)
                .into_iter()
                .map(|(_, t)| t)
                .collect(),
            (TyKind::Tuple(ts), _) => ts.clone(),
            _ => Vec::new(),
        };
        let mut out = Vec::new();
        let mut next = 0;
        for ft in fields {
            let ft = self.reveal(ft);
            if self.lower_ty(mb, ft, span).is_some() {
                out.push(Some(next));
                next += 1;
            } else {
                out.push(None);
            }
        }
        out
    }

    /// The IR field of an enum's variant payload, if it has one.
    pub fn payload_field(
        &mut self,
        mb: &mut ModuleBuilder,
        t: TyId,
        variant: u32,
        span: Span,
    ) -> Option<u32> {
        let TyKind::Adt(a, args) = self.checked.program.types.kind(t).clone() else { return None };
        let mut field = 1;
        for v in 0..variant {
            let vfields = self.checked.program.variant_fields(a, &args, v as usize);
            let any = vfields.into_iter().any(|(_, ft)| self.lower_ty(mb, ft, span).is_some());
            if any {
                field += 1;
            }
        }
        let vfields = self.checked.program.variant_fields(a, &args, variant as usize);
        let any = vfields.into_iter().any(|(_, ft)| self.lower_ty(mb, ft, span).is_some());
        any.then_some(field)
    }

    /// The parameters a callable adds to a function it's passed to: its captures.
    pub fn callable_params(
        &mut self,
        mb: &mut ModuleBuilder,
        c: &Callable,
        span: Span,
    ) -> Vec<ir::Param> {
        match c {
            Callable::Func { .. } => Vec::new(),
            Callable::Closure { owner, id } => {
                let Some(f) = owner.source_fn() else { return Vec::new() };
                let Some(body) = self.checked.bodies.get(&f).cloned() else { return Vec::new() };
                let subst = self.instance_subst(owner);
                let def = body.closures[id.0 as usize].clone();
                let mut out = Vec::new();
                for (l, written) in def.captures {
                    let decl = body.local(l).clone();
                    let t = self.concrete(decl.ty, &subst);
                    if matches!(
                        self.checked.program.types.kind(t),
                        TyKind::Closure(..) | TyKind::FnPtr(..)
                    ) {
                        continue; // a captured closure travels as its own instance
                    }
                    let Some(ty) = self.lower_ty(mb, t, span) else { continue };
                    let (by_ref, mutable) = capture_passing(mb.target, &mb.m.types, ty, written);
                    out.push(ir::Param { name: decl.name.clone(), ty, by_ref, mutable });
                }
                out
            }
        }
    }

    /// An instance's IR signature (an empty body, filled in later).
    pub fn signature(&mut self, mb: &mut ModuleBuilder, key: &InstanceKey) -> ir::Function {
        match key {
            InstanceKey::Fn { func, callables, resources, .. } => {
                let def = self.checked.program.func(*func).clone();
                let subst = self.instance_subst(key);
                let mut params = Vec::new();
                for (i, p) in def.params.iter().enumerate() {
                    let t = self.concrete(p.ty, &subst);
                    if let TyKind::FnPtr(..) = self.checked.program.types.kind(t) {
                        if let Some(Some(c)) = callables.get(i) {
                            let c = c.clone();
                            params.extend(self.callable_params(mb, &c, p.span));
                        }
                        continue;
                    }
                    if resources.get(i).is_some_and(|r| r.is_some()) {
                        continue;
                    }
                    let Some(ty) = self.lower_ty(mb, t, p.span) else { continue };
                    let (by_ref, mutable) = param_passing(mb.target, &mb.m.types, ty, p.mode);
                    params.push(ir::Param { name: p.name.clone(), ty, by_ref, mutable });
                }
                let ret_t = self.concrete(def.ret, &subst);
                let ret = self.lower_ty(mb, ret_t, def.sig_span);
                let mut f = ir::Function::new(self.instance_name(mb, key), params, ret);
                if def.ret_mode != RetMode::Owned {
                    if mb.target == ir::Target::Gpu {
                        self.err(
                            Diagnostic::new(codes::E0702, def.sig_span, format!("`{}` returns a projection, which GPU code can't do yet", def.name))
                                .with_note("WGSL functions can't return pointers; milestone 1 supports projections on the CPU only"),
                        );
                    }
                    f.ret_ref = true;
                }
                f
            }
            InstanceKey::Closure { owner, id } => {
                let c = Callable::Closure { owner: owner.clone(), id: *id };
                let mut params =
                    self.callable_params(mb, &c, Span::new(wrela_diag::FileId(0), 0, 0));
                let Some(f) = owner.source_fn() else {
                    return ir::Function::new("closure", params, None);
                };
                let Some(body) = self.checked.bodies.get(&f).cloned() else {
                    return ir::Function::new("closure", params, None);
                };
                let subst = self.instance_subst(owner);
                let def = body.closures[id.0 as usize].clone();
                for &l in &def.params {
                    let decl = body.local(l).clone();
                    let t = self.concrete(decl.ty, &subst);
                    if let Some(ty) = self.lower_ty(mb, t, decl.span) {
                        params.push(ir::Param {
                            name: decl.name,
                            ty,
                            by_ref: false,
                            mutable: false,
                        });
                    }
                }
                let rt = self.concrete(def.ret, &subst);
                let ret = self.lower_ty(mb, rt, def.span);
                let name = format!("{}_closure{}", self.checked.program.func(f).name, id.0);
                ir::Function::new(name, params, ret)
            }
            InstanceKey::Derived { .. } => crate::body::derived_signature(self, mb, key),
        }
    }

    fn instance_name(&self, mb: &ModuleBuilder, key: &InstanceKey) -> String {
        let n = mb.m.functions.len();
        match key.source_fn() {
            Some(f) => {
                let p = &self.checked.program;
                let name = p.fn_display_name(f);
                // Identifier-safe: `Sphere::distance` becomes `Sphere_distance`.
                let clean: String =
                    name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
                format!("{clean}_{n}")
            }
            None => format!("fn_{n}"),
        }
    }
}

/// The body of an instance's source function (or its owner, for a closure).
pub(crate) fn source_body(cx: &Cx, key: &InstanceKey) -> Option<thir::Body> {
    key.source_fn().and_then(|f| cx.checked.bodies.get(&f).cloned())
}
