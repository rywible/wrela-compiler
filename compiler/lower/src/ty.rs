//! Types: from the checker's to the IR's, and instance signatures.

use crate::instance::{Callable, InstanceKey};
use crate::{Cx, ModuleBuilder};
use wrela_diag::{Diagnostic, Span, codes};
use wrela_ir as ir;
use wrela_sema::defs::{Mode, RetMode};
use wrela_sema::mir::Local;
use wrela_sema::ty::*;

/// How a parameter of type `ty` and `mode` is passed, in a function returning `ret_mode`:
/// `(by_ref, mutable)`. `mut` is by reference; on the CPU a borrowed aggregate is too (no
/// copy), and so is every borrowed parameter of a function returning a projection, which may
/// point into it after the function's frame is gone; everything else is by value.
pub(crate) fn param_passing(
    target: ir::Target,
    types: &ir::Types,
    ty: ir::TypeId,
    mode: Mode,
    ret_mode: RetMode,
) -> (bool, bool) {
    let cpu = target == ir::Target::Cpu;
    match mode {
        Mode::Mut => (true, true),
        Mode::Borrow if cpu && ret_mode != RetMode::Owned => (true, false),
        Mode::Borrow => {
            (cpu && types.is_aggregate(ty) && !matches!(types.get(ty), ir::TypeDef::Run(_)), false)
        }
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
        let k = self.checked.program.types.kind(t);
        match k {
            TyKind::Opaque(f, args) => {
                let Some(hidden) = self.checked.mir.get(f).and_then(|b| b.hidden_ret) else {
                    return self.checked.program.types.error;
                };
                let generics = self.checked.program.fn_all_generics(*f);
                let subst = Subst::from_pairs(&generics, args);
                self.concrete(hidden, &subst)
            }
            TyKind::Tuple(ts) => {
                let ts = ts.iter().map(|&x| self.reveal(x)).collect();
                self.checked.program.types.intern(TyKind::Tuple(ts))
            }
            TyKind::Adt(a, ts) => {
                let ts = ts.iter().map(|&x| self.reveal(x)).collect();
                self.checked.program.types.intern(TyKind::Adt(*a, ts))
            }
            TyKind::Array(e, n) => {
                let e = self.reveal(*e);
                self.checked.program.types.intern(TyKind::Array(e, *n))
            }
            TyKind::Slice(e) => {
                let e = self.reveal(*e);
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
        let k = self.checked.program.types.kind(t);
        let gpu = mb.target() == ir::Target::Gpu;
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
                let s = if *f == FloatTy::F32 { ir::Scalar::F32 } else { ir::Scalar::F64 };
                if gpu && !s.on_gpu() {
                    self.cpu_only(mb, s.name(), span);
                }
                Some(mb.m.types.scalar(s))
            }
            TyKind::Vec(n) => Some(mb.m.types.vector(*n)),
            TyKind::Mat(n) => Some(mb.m.types.intern(ir::TypeDef::Matrix(*n))),
            TyKind::Tuple(ts) => {
                let mut fields = Vec::new();
                for (i, &e) in ts.iter().enumerate() {
                    if let Some(f) = self.lower_ty(mb, e, span) {
                        fields.push((format!("_{i}"), f));
                    }
                }
                // Like a struct, a tuple none of whose elements has a value (`()`, `((), ())`)
                // has none: WGSL has no empty structs.
                if fields.is_empty() {
                    return None;
                }
                let name = self.checked.program.display_ty(t);
                Some(mb.m.types.intern(ir::TypeDef::Struct { name, fields }))
            }
            TyKind::Array(e, n) => {
                if gpu && *n == 0 {
                    let shown = self.checked.program.display_ty(t);
                    let note = "WGSL has no empty arrays: on the GPU an array has at least one element (language.md §4)";
                    self.not_on_gpu(mb, &shown, note, span);
                }
                self.lower_ty(mb, *e, span).map(|e| mb.m.types.intern(ir::TypeDef::Array(e, *n)))
            }
            TyKind::Slice(e) => {
                let Some(e) = self.lower_ty(mb, *e, span) else {
                    self.holds_nothing("run", *e, span);
                    return None;
                };
                Some(mb.m.types.intern(if gpu {
                    ir::TypeDef::RuntimeArray(e)
                } else {
                    ir::TypeDef::Run(e)
                }))
            }
            TyKind::Adt(a, args) => {
                let p = &self.checked.program;
                let name = p.display_ty(t);
                if p.adt(*a).is_enum() {
                    // The tag is a `u32`, which field 0 has.
                    mb.m.types.u32();
                    let mut variants = Vec::new();
                    for (v, variant) in p.adt(*a).variants().iter().enumerate() {
                        let v = Some(v as u32);
                        let mut payload = Vec::new();
                        for (d, ft) in p.adt_fields(*a, v).iter().zip(p.fields_of(*a, args, v)) {
                            if let Some(f) = self.lower_ty(mb, ft, span) {
                                payload.push((d.name.clone(), f));
                            }
                        }
                        let vname = variant.name.clone();
                        let pt = (!payload.is_empty()).then(|| {
                            mb.m.types.intern(ir::TypeDef::Struct {
                                name: format!("{name}::{vname}"),
                                fields: payload,
                            })
                        });
                        variants.push((vname, pt));
                    }
                    Some(mb.m.types.intern(ir::TypeDef::Enum { name, variants }))
                } else {
                    let mut fields = Vec::new();
                    for (d, ft) in p.adt_fields(*a, None).iter().zip(p.fields_of(*a, args, None)) {
                        let ft = self.reveal(ft);
                        if let Some(f) = self.lower_ty(mb, ft, span) {
                            fields.push((d.name.clone(), f));
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
                self.err(Diagnostic::internal(format!(
                    "a type that should be concrete reached lowering: `{shown}`"
                )));
                None
            }
        };
        if let Some(o) = out
            && gpu
        {
            self.check_gpu_size(mb, t, o, span);
        }
        mb.type_cache.insert(t, out);
        out
    }

    /// E0702: a type too large for WGSL, reported for the smallest type that is (its parts
    /// fit), so a struct around a large array isn't reported again.
    fn check_gpu_size(&mut self, mb: &ModuleBuilder, t: TyId, o: ir::TypeId, span: Span) {
        let types = &mb.m.types;
        let too_large = |x| ir::layout::wgsl_layout(types, x).size > ir::layout::WGSL_MAX_TYPE_SIZE;
        if !too_large(o) {
            return;
        }
        let fields = |x| match types.get(x) {
            ir::TypeDef::Struct { fields, .. } => fields.iter().map(|f| f.1).collect(),
            _ => Vec::new(),
        };
        let parts: Vec<ir::TypeId> = match types.get(o) {
            ir::TypeDef::Array(e, _) => vec![*e],
            ir::TypeDef::Struct { .. } => fields(o),
            ir::TypeDef::Enum { variants, .. } => {
                variants.iter().filter_map(|v| v.1).flat_map(fields).collect()
            }
            _ => Vec::new(),
        };
        if parts.into_iter().any(too_large) {
            return;
        }
        let shown = self.checked.program.display_ty(t);
        let what = mb.gpu.as_ref().map_or("GPU code", |g| g.what.as_str());
        let max = ir::layout::WGSL_MAX_TYPE_SIZE;
        self.err(
            Diagnostic::new(codes::E0702, span, format!("`{shown}` is too large for {what}"))
                .with_note(format!("a WGSL type is at most {max} bytes")),
        );
    }

    fn cpu_only(&mut self, mb: &ModuleBuilder, name: &str, span: Span) {
        let note = "WGSL has `bool`, `i32`, `u32` and `f32`; 64-bit and 8/16-bit types exist only on the CPU (language.md §4)";
        self.not_on_gpu(mb, name, note, span);
    }

    /// E0326: a type GPU code can't hold.
    /// E0702: a run or a buffer of `elem`, a type whose values hold nothing (`()`, a struct
    /// with no fields). It would have a length and no bytes, which neither back end has a form
    /// for yet; dropping it would lose the length (and a dispatch that takes one).
    pub(crate) fn holds_nothing(&mut self, what: &str, elem: TyId, span: Span) {
        let shown = self.checked.program.display_ty(elem);
        self.err(
            Diagnostic::new(
                codes::E0702,
                span,
                format!("a {what} of `{shown}`, whose values hold nothing, isn't supported yet"),
            )
            .with_help("give the type a field, or pass a count instead"),
        );
    }

    fn not_on_gpu(&mut self, mb: &ModuleBuilder, name: &str, note: &str, span: Span) {
        let what = mb.gpu.as_ref().map_or("GPU code", |g| g.what.as_str());
        self.err(
            Diagnostic::new(
                codes::E0326,
                span,
                format!("`{name}` is CPU-only, and this is {what}"),
            )
            .with_note(note),
        );
    }

    /// For each field of a struct, a tuple or (with `variant`) an enum variant: its IR field
    /// index (`None` when the field has no runtime value) and its type.
    pub fn field_map(
        &mut self,
        mb: &mut ModuleBuilder,
        t: TyId,
        variant: Option<u32>,
        span: Span,
    ) -> Vec<(Option<u32>, TyId)> {
        let p = &self.checked.program;
        let fields: Vec<TyId> = match p.types.kind(t) {
            TyKind::Adt(a, args) => p.fields_of(*a, args, variant),
            TyKind::Tuple(ts) => ts.clone(),
            _ => Vec::new(),
        };
        let mut out = Vec::new();
        let mut next = 0;
        for ft in fields {
            let revealed = self.reveal(ft);
            if self.lower_ty(mb, revealed, span).is_some() {
                out.push((Some(next), ft));
                next += 1;
            } else {
                out.push((None, ft));
            }
        }
        out
    }

    /// Field `i` of a struct, a tuple or (with `variant`) an enum variant: its IR field index
    /// and its type. `None` when it has no runtime value.
    pub fn field(
        &mut self,
        mb: &mut ModuleBuilder,
        t: TyId,
        variant: Option<u32>,
        i: u32,
        span: Span,
    ) -> Option<(u32, TyId)> {
        let (k, ft) = *self.field_map(mb, t, variant, span).get(i as usize)?;
        Some((k?, ft))
    }

    /// The IR field of an enum's variant payload, if it has one.
    pub fn payload_field(
        &mut self,
        mb: &mut ModuleBuilder,
        t: TyId,
        variant: u32,
        span: Span,
    ) -> Option<u32> {
        let it = self.lower_ty(mb, t, span)?;
        // Field 0 is the tag; field 1 + v variant v's payload (ir::TypeDef::Enum).
        mb.m.types.field(it, 1 + variant).map(|_| 1 + variant)
    }

    /// The captures of closure `id` in `owner`'s body that its lifted function takes as
    /// parameters, in order: each one's local, IR type, and whether the closure writes it. A
    /// captured closure travels as its own instance, and a capture with no runtime value isn't
    /// passed.
    pub fn lowered_captures(
        &mut self,
        mb: &mut ModuleBuilder,
        owner: &InstanceKey,
        id: ClosureId,
    ) -> Vec<(Local, ir::TypeId, bool)> {
        let checked = self.checked;
        let Some(body) = owner.source_fn().and_then(|f| checked.mir.get(&f)) else {
            return Vec::new();
        };
        let subst = self.instance_subst(owner);
        let mut out = Vec::new();
        for &(l, written) in &body.closures[id.0 as usize].captures {
            if self.capture_resource(owner, l).is_some() {
                continue;
            }
            let decl = body.local(l);
            let t = self.concrete(decl.ty, &subst);
            if matches!(self.checked.program.types.kind(t), TyKind::Closure(..) | TyKind::FnPtr(..))
            {
                continue;
            }
            let Some(ty) = self.lower_ty(mb, t, decl.span) else { continue };
            out.push((l, ty, written));
        }
        out
    }

    /// The resource a closure's capture `l` is, on the GPU: a buffer parameter of `owner`, which
    /// the closure's instance names as its owner's does, rather than taking it.
    pub fn capture_resource(&self, owner: &InstanceKey, l: Local) -> Option<ir::ResourceId> {
        let body = self.checked.mir.get(&owner.source_fn()?)?;
        let i = body.fns[0].params.iter().position(|&p| p == l)?;
        owner.resources().get(i).copied().flatten()
    }

    /// The parameters a callable adds to a function it's passed to: its captures.
    pub fn callable_params(&mut self, mb: &mut ModuleBuilder, c: &Callable) -> Vec<ir::Param> {
        let Callable::Closure { owner, id } = c else { return Vec::new() };
        let checked = self.checked;
        let Some(body) = owner.source_fn().and_then(|f| checked.mir.get(&f)) else {
            return Vec::new();
        };
        self.lowered_captures(mb, owner, *id)
            .into_iter()
            .map(|(l, ty, written)| {
                let (by_ref, mutable) = capture_passing(mb.target(), &mb.m.types, ty, written);
                ir::Param { name: body.local(l).name.clone(), ty, by_ref, mutable }
            })
            .collect()
    }

    /// An instance's IR signature (an empty body, filled in later).
    pub fn signature(&mut self, mb: &mut ModuleBuilder, key: &InstanceKey) -> ir::Function {
        match key {
            InstanceKey::Fn { func, callables, resources, .. } => {
                let def = self.checked.program.func(*func);
                let subst = self.instance_subst(key);
                let mut params = Vec::new();
                for (i, p) in def.params.iter().enumerate() {
                    let t = self.concrete(p.ty, &subst);
                    if let TyKind::FnPtr(..) = self.checked.program.types.kind(t) {
                        if let Some(Some(c)) = callables.get(i) {
                            params.extend(self.callable_params(mb, c));
                        }
                        continue;
                    }
                    if resources.get(i).is_some_and(|r| r.is_some()) {
                        continue;
                    }
                    let Some(ty) = self.lower_ty(mb, t, p.span) else { continue };
                    let (by_ref, mutable) =
                        param_passing(mb.target(), &mb.m.types, ty, p.mode, def.ret_mode);
                    params.push(ir::Param { name: p.name.clone(), ty, by_ref, mutable });
                }
                let ret_t = self.concrete(def.ret, &subst);
                let ret = self.lower_ty(mb, ret_t, def.sig_span);
                let mut f = ir::Function::new(self.instance_name(mb, key), params, ret);
                if def.ret_mode != RetMode::Owned {
                    if mb.target() == ir::Target::Gpu {
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
                let checked = self.checked;
                let Some(f) = owner.source_fn() else {
                    return ir::Function::new("closure", Vec::new(), None);
                };
                let Some(body) = checked.mir.get(&f) else {
                    return ir::Function::new("closure", Vec::new(), None);
                };
                let info = &body.closures[id.0 as usize];
                let c = Callable::Closure { owner: owner.clone(), id: *id };
                let mut params = self.callable_params(mb, &c);
                let subst = self.instance_subst(owner);
                for &l in &info.params {
                    let decl = body.local(l);
                    let t = self.concrete(decl.ty, &subst);
                    if let Some(ty) = self.lower_ty(mb, t, decl.span) {
                        params.push(ir::Param {
                            name: decl.name.clone(),
                            ty,
                            by_ref: false,
                            mutable: false,
                        });
                    }
                }
                let rt = self.concrete(info.ret, &subst);
                let ret = self.lower_ty(mb, rt, info.span);
                let name = format!("{}_closure{}", self.checked.program.func(f).name, id.0);
                ir::Function::new(name, params, ret)
            }
            InstanceKey::Derived { .. } => crate::derive::signature(self, mb, key),
        }
    }

    fn instance_name(&self, mb: &ModuleBuilder, key: &InstanceKey) -> String {
        let n = mb.m.functions.len();
        match key.source_fn() {
            Some(f) => {
                let p = &self.checked.program;
                let name = p.fn_display_name(f);
                // Identifier-safe: `Sphere::distance` becomes `Sphere_distance`.
                format!("{}_{n}", ir::ident(&name))
            }
            None => format!("fn_{n}"),
        }
    }
}
