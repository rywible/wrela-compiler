//! GPU pipelines: what CPU code records (`dispatch`, `draw`), and each pipeline's module, lowered
//! from its entry points with its parameters bound to resources (D-102). Effects are checked
//! here, per instantiation (§8).

use crate::body::{Fl, Repr};
use crate::instance::{Bound, InstanceKey};
use crate::{Cx, ModuleBuilder};
use std::rc::Rc;
use wrela_abi::manifest::{BindingKind, BindingStage, ResourceBinding};
use wrela_abi::stream::Opcode;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_ir as ir;
use wrela_ir::uniformity::{Collective, NonUniform};
use wrela_sema::defs::{Entry, Lang, Mode};
use wrela_sema::gpu::BuiltinInput;
use wrela_sema::mir;
use wrela_sema::ty::*;

/// A pipeline as CPU code names it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PipelineKey {
    Compute {
        kernel: FnId,
        substs: Vec<TyId>,
        /// Dispatched over a domain (`over:`): its uniform block holds the domain, and
        /// invocations past it return at once.
        over: bool,
    },
    Render {
        vertex: (FnId, Vec<TyId>),
        fragment: (FnId, Vec<TyId>),
        /// Its cull mode and depth bias, written where it's drawn: one pipeline each.
        state: wrela_sema::thir::RenderState,
    },
}

impl PipelineKey {
    pub fn mentions(&self, f: FnId) -> bool {
        match self {
            PipelineKey::Compute { kernel, .. } => *kernel == f,
            PipelineKey::Render { vertex, fragment, .. } => vertex.0 == f || fragment.0 == f,
        }
    }
}

/// A pipeline's kind. Its entry points are the module's, in order: a kernel, or a vertex shader
/// then a fragment shader.
#[derive(Clone, Debug, PartialEq)]
pub enum PipelineKind {
    Compute { workgroup_size: [u32; 3] },
    Render,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UniformOut {
    pub binding: u32,
    pub size: u32,
    /// The layout doesn't meet the uniform rules, so it's a read-only storage block.
    pub storage: bool,
}

/// A lowered pipeline: its module and what the manifest says about it.
#[derive(Clone, Debug)]
pub struct PipelineOut {
    pub name: String,
    pub module: ir::Module,
    pub kind: PipelineKind,
    pub uniform: Option<UniformOut>,
    /// Each buffer, texture and sampler binding, in the order commands list them.
    pub bindings: Vec<ResourceBinding>,
    /// Which instantiation of its entry points it is.
    pub key: PipelineKey,
    /// Where CPU code dispatches or draws it, first seen first (the pipeline-count query, §7).
    pub sites: Vec<Span>,
    /// A debug build's: the binding of the flag its bounds checks set (`ir::bounds`).
    pub debug_flag: Option<u32>,
    /// A render pipeline whose fragment shader returns `Over`: its colour is blended over the
    /// target's.
    pub blend: bool,
    /// A render pipeline whose fragment shader gives its depth (`WithDepth`).
    pub writes_depth: bool,
    /// A render pipeline whose fragment shader returns a `u32`: drawn into an `R32Uint`
    /// texture.
    pub uint: bool,
    /// The functions its module instantiates, with their type arguments (`wrela query`).
    pub instances: Vec<(FnId, Vec<TyId>)>,
}

/// What a GPU module is for, while it's lowered.
#[derive(Clone, Debug)]
pub(crate) struct GpuCx {
    pub what: String,
    /// How the module's entry points receive their parameters (none for a `@gpu` function).
    pub iface: Rc<Interface>,
    /// The stage being lowered now (a render pipeline lowers its vertex shader first).
    pub stage: Option<Entry>,
    pub workgroup_size: [u32; 3],
    /// The private variable holding `num_workgroups`, for `Slots` indexing.
    pub num_workgroups: Option<ir::ResourceId>,
    /// The uniform block, whose fields are the interface's uniforms that have a runtime value.
    pub uniform: Option<ir::ResourceId>,
    /// For each of the interface's uniforms, its field in the block (`None`: no runtime value).
    pub uniform_fields: Vec<Option<u32>>,
    /// Over a domain: the block's field holding the domain's size.
    pub domain_field: Option<u32>,
    /// A lifted build's literal table (§22): the storage buffer every pipeline binds last.
    pub literals: Option<ir::ResourceId>,
}

impl GpuCx {
    /// A module for `what` (`the @compute kernel k`), lowered from the entry points of `iface`,
    /// starting with `stage`'s.
    pub fn new(what: String, iface: Rc<Interface>, stage: Option<Entry>) -> GpuCx {
        let workgroup_size = match stage {
            Some(Entry::Compute(wg)) => wg,
            _ => [1, 1, 1],
        };
        GpuCx {
            what,
            iface,
            stage,
            workgroup_size,
            num_workgroups: None,
            uniform: None,
            uniform_fields: Vec::new(),
            domain_field: None,
            literals: None,
        }
    }

    /// Which of the module's entry points `f` is.
    pub fn entry_index(&self, f: FnId) -> Option<usize> {
        self.iface.entries.iter().position(|e| e.func == f)
    }
}

/// How an entry point's parameter is supplied.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ParamClass {
    Builtin(ir::BuiltinInput),
    /// A `[T]` (read) or `Slots<T>` (read-write) parameter, bound to a buffer of `T`; or an
    /// `Atomics<T>` or `AtomicMap` (`atomic`), bound to a buffer of `atomic<T>`.
    Buffer {
        elem: TyId,
        read_write: bool,
        atomic: bool,
    },
    /// An `Append<T>`: bound to two buffers, the items and the count (an atomic).
    Append {
        elem: TyId,
    },
    /// A `Shared<T, N>`: workgroup memory, not bound.
    Workgroup {
        elem: TyId,
        len: u32,
    },
    /// A `Texture`, a `DepthTexture` or a `Texture3d` (`three`), bound by its handle; or a
    /// kernel's `Texels` or `Texels3d` (`storage`), bound by its texture's. Its texels' format
    /// (a depth texture's is `R32Float`).
    Texture {
        depth: bool,
        storage: bool,
        three: bool,
        format: ir::TexFormat,
    },
    /// A `Sampler` or a `ComparisonSampler`, bound by its handle.
    Sampler {
        comparison: bool,
    },
    /// The vertex shader's output, read by the fragment shader.
    Varyings,
    Uniform,
    /// A group (a borrow struct, §12): its fields are the interface's leaves.
    Group,
}

/// How parameter `param` of `f`, of type `ty`, is supplied. `varyings` is the vertex output a
/// fragment shader may take.
fn classify(cx: &Cx, f: FnId, param: usize, ty: TyId, varyings: Option<TyId>) -> ParamClass {
    let p = &cx.checked.program;
    // A parameter of a generic type is the caller's data whatever the type is, as the checker
    // decided from the declaration: `k<T>(x: T)` given a `LocalId` for `x` takes that value, not
    // the invocation's id.
    if let TyKind::Param(_) = p.types.kind(p.func(f).params[param].ty) {
        return ParamClass::Uniform;
    }
    if let Some(b) = BuiltinInput::of(p, ty) {
        return ParamClass::Builtin(match b {
            BuiltinInput::GlobalId => ir::BuiltinInput::GlobalInvocationId,
            BuiltinInput::LocalId => ir::BuiltinInput::LocalInvocationId,
            BuiltinInput::WorkgroupId => ir::BuiltinInput::WorkgroupId,
            BuiltinInput::VertexIndex => ir::BuiltinInput::VertexIndex,
            BuiltinInput::InstanceIndex => ir::BuiltinInput::InstanceIndex,
            BuiltinInput::FragCoord => ir::BuiltinInput::Position,
        });
    }
    class_of(cx, ty, p.func(f).params[param].mode, varyings)
}

/// How a parameter of type `ty` (not a builtin), taken in `mode`, is supplied.
fn class_of(cx: &Cx, ty: TyId, mode: Mode, varyings: Option<TyId>) -> ParamClass {
    let p = &cx.checked.program;
    if let Some(class) = resource_class(cx, ty) {
        return class;
    }
    // A kernel's texels to write (the textures it reads are `resource_class`'s).
    if let Some(l @ (Lang::Texels | Lang::Texels3d)) = p.lang_of_ty(ty) {
        let (three, format) = (l == Lang::Texels3d, texture_format(cx, ty));
        return ParamClass::Texture { depth: false, storage: true, three, format };
    }
    match p.types.kind(ty) {
        TyKind::Adt(_, args) if matches!(p.lang_of_ty(ty), Some(Lang::Slots | Lang::One)) => {
            ParamClass::Buffer { elem: args[0], read_write: true, atomic: false }
        }
        TyKind::Adt(_, args) if p.lang_of_ty(ty) == Some(Lang::Atomics) => {
            ParamClass::Buffer { elem: args[0], read_write: true, atomic: true }
        }
        TyKind::Adt(..) if p.lang_of_ty(ty) == Some(Lang::AtomicMap) => {
            ParamClass::Buffer { elem: p.types.u32, read_write: true, atomic: true }
        }
        TyKind::Adt(_, args) if p.lang_of_ty(ty) == Some(Lang::Append) => {
            ParamClass::Append { elem: args[0] }
        }
        TyKind::Adt(_, args) if p.lang_of_ty(ty) == Some(Lang::Shared) => {
            let len = match p.types.kind(args[1]) {
                TyKind::ConstU32(n) => *n,
                _ => 0,
            };
            ParamClass::Workgroup { elem: args[0], len }
        }
        TyKind::Adt(..) if Some(ty) == varyings => ParamClass::Varyings,
        TyKind::Slice(e) => {
            ParamClass::Buffer { elem: *e, read_write: mode == Mode::Mut, atomic: false }
        }
        _ if wrela_sema::gpu::is_group(p, ty) => ParamClass::Group,
        _ => ParamClass::Uniform,
    }
}

/// How a group's field of type `ty`, or a parameter of it, is bound when it's read-only: a
/// texture, sampler or span. `None` for the rest.
fn resource_class(cx: &Cx, ty: TyId) -> Option<ParamClass> {
    let p = &cx.checked.program;
    Some(match (p.lang_of_ty(ty)?, p.types.kind(ty)) {
        (Lang::Texture, _) => ParamClass::Texture {
            depth: false,
            storage: false,
            three: false,
            format: texture_format(cx, ty),
        },
        (Lang::DepthTexture, _) => ParamClass::Texture {
            depth: true,
            storage: false,
            three: false,
            format: ir::TexFormat::R32Float,
        },
        (Lang::Texture3d, _) => ParamClass::Texture {
            depth: false,
            storage: false,
            three: true,
            format: texture_format(cx, ty),
        },
        (Lang::Sampler, _) => ParamClass::Sampler { comparison: false },
        (Lang::ComparisonSampler, _) => ParamClass::Sampler { comparison: true },
        (Lang::GpuSpan, TyKind::Adt(_, args)) => {
            ParamClass::Buffer { elem: args[0], read_write: false, atomic: false }
        }
        _ => return None,
    })
}

/// The format of a texture type's texels (`Texture<F>`, `Texels<F>`, ...): its first type
/// argument's; a depth texture's, `R32Float`.
pub(crate) fn texture_format(cx: &Cx, ty: TyId) -> ir::TexFormat {
    let p = &cx.checked.program;
    let TyKind::Adt(_, args) = p.types.kind(ty) else { return ir::TexFormat::R32Float };
    match args.first().and_then(|&f| p.lang_of_ty(f)) {
        Some(Lang::Rgba8) => ir::TexFormat::Rgba8Unorm,
        Some(Lang::Rgba16Float) => ir::TexFormat::Rgba16Float,
        Some(Lang::R16Float) => ir::TexFormat::R16Float,
        Some(Lang::Rg16Float) => ir::TexFormat::Rg16Float,
        Some(Lang::R32Uint) => ir::TexFormat::R32Uint,
        _ => ir::TexFormat::R32Float,
    }
}

/// The stream's and the manifest's format for an IR one.
pub(crate) fn stream_format(f: ir::TexFormat) -> wrela_abi::stream::TextureFormat {
    use wrela_abi::stream::TextureFormat as T;
    match f {
        ir::TexFormat::Rgba8Unorm => T::Rgba8,
        ir::TexFormat::Rgba16Float => T::Rgba16Float,
        ir::TexFormat::R16Float => T::R16Float,
        ir::TexFormat::Rg16Float => T::Rg16Float,
        ir::TexFormat::R32Float => T::R32Float,
        ir::TexFormat::R32Uint => T::R32Uint,
    }
}

/// How a group's field of type `ty` is passed: bound, a group of its own, or a value in the
/// uniform block (§12).
fn field_class(cx: &Cx, ty: TyId) -> ParamClass {
    match resource_class(cx, ty) {
        Some(class) => class,
        None if wrela_sema::gpu::is_group(&cx.checked.program, ty) => ParamClass::Group,
        None => ParamClass::Uniform,
    }
}

/// The concrete types of a group's fields.
pub(crate) fn group_fields(cx: &mut Cx, ty: TyId) -> Vec<TyId> {
    let checked = cx.checked;
    let p = &checked.program;
    let TyKind::Adt(a, args) = p.types.kind(ty) else { return Vec::new() };
    let fields = p.fields_of(*a, args, None);
    fields.into_iter().map(|t| cx.concrete(t, &Subst::default())).collect()
}

/// A group's fields' names.
pub(crate) fn group_field_names(cx: &Cx, ty: TyId) -> Vec<String> {
    let p = &cx.checked.program;
    let TyKind::Adt(a, _) = p.types.kind(ty) else { return Vec::new() };
    p.adt_fields(*a, None).iter().map(|f| f.name.clone()).collect()
}

/// What a group's fields are bound to before any is bound: a group of nothing, with a group
/// in each of its group fields.
fn group_skeleton(cx: &mut Cx, ty: TyId) -> Bound {
    let fields = group_fields(cx, ty);
    Bound::Group(
        fields
            .into_iter()
            .map(|t| match field_class(cx, t) {
                ParamClass::Group => Some(group_skeleton(cx, t)),
                _ => None,
            })
            .collect(),
    )
}

/// Binds resource `r` at `path` in a group's binding.
fn bind_at(b: &mut Bound, path: &[u32], r: ir::ResourceId) {
    let Bound::Group(fields) = b else { return };
    let Some((&first, rest)) = path.split_first() else { return };
    let Some(slot) = fields.get_mut(first as usize) else { return };
    match (rest.is_empty(), slot) {
        (true, slot) => *slot = Some(Bound::One(r)),
        (false, Some(inner)) => bind_at(inner, rest, r),
        (false, None) => {}
    }
}

/// One entry point of a pipeline.
#[derive(Debug)]
pub(crate) struct EntryParams {
    pub func: FnId,
    /// Concrete types for the function's generics.
    pub substs: Vec<TyId>,
    /// Each parameter's concrete type, and how it's supplied.
    pub params: Vec<(TyId, ParamClass)>,
}

/// A parameter an entry point is passed, or a field of a group it's passed (§12).
#[derive(Clone, Debug)]
pub(crate) struct Leaf {
    /// Its entry point and parameter, as indices.
    pub entry: usize,
    pub param: usize,
    /// The fields from the parameter to it, as indices: none for the parameter itself.
    pub path: Vec<u32>,
    pub ty: TyId,
    pub class: ParamClass,
    /// The parameter's name, then each field's (`lit.probes`).
    pub name: String,
}

impl Leaf {
    /// Whether it's (in) parameter `param` of entry point `entry`, at `path`.
    pub fn is(&self, entry: usize, param: usize, path: &[u32]) -> bool {
        self.entry == entry && self.param == param && self.path == path
    }

    /// Its name as a WGSL identifier: `lit.probes` is `lit_probes`.
    pub fn field_name(&self) -> String {
        ir::ident(&self.name.replace('.', "_"))
    }

    /// The place of its argument, given its parameter's argument `arg`: through the group's
    /// fields, on the CPU.
    fn arg_place(&self, arg: &mir::Place) -> mir::Place {
        let mut p = arg.clone();
        p.proj.extend(self.path.iter().map(|&k| mir::Proj::Field(k)));
        p
    }
}

/// How a pipeline's entry points receive their parameters. CPU code that records the pipeline
/// and the pipeline's module both read it, so the buffer handles a command passes are the
/// buffers the module binds, in order, and the uniform values it packs are the module's
/// uniform block.
#[derive(Debug, Default)]
pub(crate) struct Interface {
    /// The entry points: a kernel, or a vertex shader then a fragment shader.
    pub entries: Vec<EntryParams>,
    /// What's bound to resources (buffers, textures, samplers), in binding order: parameters,
    /// and the fields of groups. Each shader's are its own (§12).
    pub bound: Vec<Leaf>,
    /// The uniform block's fields, each a uniform parameter or a group's value. Its `name` is
    /// its field's (`lit_cam`; with the entry point's first when both shaders have one).
    pub uniforms: Vec<Leaf>,
    /// A render pipeline's vertex output: what the fragment shader's varyings are.
    pub vertex_ret: Option<TyId>,
    /// A kernel dispatched over a domain (`over:`): its uniform block ends with the domain's
    /// size (`DOMAIN`), and invocations past it return at once.
    pub over: bool,
}

impl Interface {
    /// The interface of entry points, each with its generic arguments. A fragment shader among
    /// them may take `varyings`.
    fn of(
        cx: &mut Cx,
        entries: &[(FnId, &[TyId])],
        vertex_ret: Option<TyId>,
        varyings: Option<TyId>,
    ) -> Interface {
        let mut iface = Interface { vertex_ret, ..Interface::default() };
        for (e, &(func, substs)) in entries.iter().enumerate() {
            let mut params = Vec::new();
            let varyings = varyings.filter(|_| cx.entry_of(func) == Some(Entry::Fragment));
            for (i, t) in param_types(cx, func, substs).into_iter().enumerate() {
                let class = classify(cx, func, i, t, varyings);
                let name = cx.checked.program.func(func).params[i].name.clone();
                let leaf = Leaf { entry: e, param: i, path: Vec::new(), ty: t, class, name };
                iface.add(cx, leaf.clone());
                params.push((t, leaf.class));
            }
            iface.entries.push(EntryParams { func, substs: substs.to_vec(), params });
        }
        // A field name both shaders have is told apart by the shader's.
        let program = &cx.checked.program;
        let names: Vec<String> = iface.uniforms.iter().map(|u| u.field_name()).collect();
        for (k, u) in iface.uniforms.iter_mut().enumerate() {
            if names.iter().filter(|n| **n == names[k]).count() > 1 {
                let shader = ir::ident(&program.func(entries[u.entry].0).name);
                u.name = format!("{shader}.{}", u.name);
            }
        }
        iface
    }

    /// Adds what `leaf` is passed as: a binding, a uniform, or (a group) each of its fields.
    fn add(&mut self, cx: &mut Cx, leaf: Leaf) {
        match leaf.class {
            ParamClass::Buffer { .. }
            | ParamClass::Append { .. }
            | ParamClass::Texture { .. }
            | ParamClass::Sampler { .. } => self.bound.push(leaf),
            ParamClass::Uniform => self.uniforms.push(leaf),
            ParamClass::Group => {
                let names = group_field_names(cx, leaf.ty);
                for (k, ft) in group_fields(cx, leaf.ty).into_iter().enumerate() {
                    let mut path = leaf.path.clone();
                    path.push(k as u32);
                    let name = format!("{}.{}", leaf.name, names[k]);
                    let class = field_class(cx, ft);
                    self.add(cx, Leaf { path, ty: ft, class, name, ..leaf.clone() });
                }
            }
            ParamClass::Builtin(_) | ParamClass::Varyings | ParamClass::Workgroup { .. } => {}
        }
    }
}

/// A pipeline's interface.
fn interface(cx: &mut Cx, key: &PipelineKey) -> Interface {
    match key {
        PipelineKey::Compute { kernel, substs, over } => Interface {
            over: *over,
            ..Interface::of(cx, &[(*kernel, substs.as_slice())], None, None)
        },
        PipelineKey::Render { vertex, fragment, .. } => {
            let vret = ret_type(cx, vertex.0, &vertex.1);
            let entries = [(vertex.0, vertex.1.as_slice()), (fragment.0, fragment.1.as_slice())];
            Interface::of(cx, &entries, Some(vret), Some(vret))
        }
    }
}

/// Whether a parameter or group field of type `t` is bound to a resource on the GPU: a run, a
/// span, a texture or sampler, or what invocations share.
pub(crate) fn is_resource_ty(p: &wrela_sema::program::Program, t: TyId) -> bool {
    matches!(p.types.kind(t), TyKind::Slice(_))
        || p.lang_of_ty(t).is_some_and(|l| {
            l.is_invocation_safe() || l.is_texture_or_sampler() || l == Lang::GpuSpan
        })
}

/// The bindings of a buffer-like argument for a parameter of type `pt`: a `GpuBuffer` or span
/// for `[T]`, `Slots<T>` and `Atomics<T>`; an `AppendBuffer<T>`'s items and count for an
/// `Append<T>`; an `AtomicMapBuffer`'s entries for an `AtomicMap`.
fn resource_bindings(
    fl: &mut Fl,
    pt: TyId,
    arg: &mir::Place,
    span: Span,
) -> Option<Vec<ir::ValueId>> {
    let p = &fl.cx.checked.program;
    let u32_ty = p.types.u32;
    let field = |k: u32| {
        let mut f = arg.clone();
        f.proj.push(mir::Proj::Field(k));
        f
    };
    Some(match (p.lang_of_ty(pt), p.types.kind(pt)) {
        (Some(Lang::Append), TyKind::Adt(_, args)) => {
            let elem = args[0];
            let mut b = buffer_binding(fl, &field(0), elem, span)?.to_vec();
            b.extend(buffer_binding(fl, &field(1), u32_ty, span)?);
            b
        }
        (Some(Lang::AtomicMap), _) => buffer_binding(fl, &field(0), u32_ty, span)?.to_vec(),
        (_, TyKind::Slice(e)) => buffer_binding(fl, arg, *e, span)?.to_vec(),
        (_, TyKind::Adt(_, args)) => buffer_binding(fl, arg, args[0], span)?.to_vec(),
        _ => return None,
    })
}

/// The binding of a texture or sampler argument: its handle, and no range.
fn handle_binding(fl: &mut Fl, arg: &mir::Place) -> Option<[ir::ValueId; 3]> {
    let u = fl.mb.m.types.u32();
    let v = fl.read(arg)?;
    let handle = fl.value(u, ir::Expr::Extract(v, 0));
    let zero = fl.u32c(0);
    Some([handle, zero, zero])
}

/// The bindings of argument `arg` for an entry point's parameter of type `pt` that `class`
/// binds to a resource: a buffer's, or a texture's or sampler's.
fn bindings_for(
    fl: &mut Fl,
    pt: TyId,
    class: &ParamClass,
    arg: &mir::Place,
    span: Span,
) -> Option<Vec<ir::ValueId>> {
    match class {
        ParamClass::Buffer { .. } | ParamClass::Append { .. } => {
            resource_bindings(fl, pt, arg, span)
        }
        _ => handle_binding(fl, arg).map(Vec::from),
    }
}

/// The concrete parameter types of an entry instance.
fn param_types(cx: &mut Cx, f: FnId, substs: &[TyId]) -> Vec<TyId> {
    let checked = cx.checked;
    let subst = Subst::from_pairs(&checked.program.fn_all_generics(f), substs);
    checked.program.func(f).params.iter().map(|p| cx.concrete(p.ty, &subst)).collect()
}

fn ret_type(cx: &mut Cx, f: FnId, substs: &[TyId]) -> TyId {
    let generics = cx.checked.program.fn_all_generics(f);
    let subst = Subst::from_pairs(&generics, substs);
    let r = cx.checked.program.func(f).ret;
    cx.concrete(r, &subst)
}

// ---- CPU side: recording ------------------------------------------------------------------------

/// A pipeline CPU code records: its index, and its interface (worked out the first time).
fn pipeline(cx: &mut Cx, key: PipelineKey, site: Span) -> (u32, Rc<Interface>) {
    if let Some(i) = cx.pipelines.iter().position(|(k, _)| *k == key) {
        if !cx.pipeline_sites[i].contains(&site) {
            cx.pipeline_sites[i].push(site);
        }
        return (i as u32, cx.pipelines[i].1.clone());
    }
    let iface = Rc::new(interface(cx, &key));
    cx.pipelines.push((key, iface.clone()));
    cx.pipeline_sites.push(vec![site]);
    (cx.pipelines.len() as u32 - 1, iface)
}

/// A buffer binding of a dispatch or draw (stream v3): the handle, and the offset and size in
/// bytes, of the `GpuBuffer<T>`, `GpuSpan<T>` or `GpuSpanMut<T>` at `arg`, whose elements are
/// `elem`s. A whole buffer of no elements still binds one, as it has room for one.
fn buffer_binding(
    fl: &mut Fl,
    arg: &mir::Place,
    elem: TyId,
    span: Span,
) -> Option<[ir::ValueId; 3]> {
    let elem = fl.concrete(elem);
    let et = fl.cx.lower_ty(fl.mb, elem, span)?;
    let stride = ir::layout::array_stride(&fl.mb.m.types, et);
    let t = fl.place_src_ty(arg);
    let lang = fl.cx.checked.program.lang_of_ty(t);
    let u = fl.mb.m.types.u32();
    let v = fl.read(arg)?;
    let stride_v = fl.u32c(stride);
    let mul =
        |fl: &mut Fl, a: ir::ValueId| fl.value(u, ir::Expr::Binary(ir::BinOp::Mul, a, stride_v));
    if lang == Some(Lang::GpuField) {
        // One element (or a field of one): its buffer's handle, its offset, its size.
        let pt = fl.mb.m.types.field(fl.f.value_ty(v), 0)?;
        let ptr = fl.value(pt, ir::Expr::Extract(v, 0));
        let at = ir::Place { root: ir::PlaceRoot::Ptr(ptr), path: vec![ir::Proj::Field(0)] };
        let handle = fl.value(u, ir::Expr::Load(at));
        let offset = fl.value(u, ir::Expr::Extract(v, 1));
        return Some([handle, offset, stride_v]);
    }
    if matches!(lang, Some(Lang::GpuSpan | Lang::GpuSpanMut)) {
        // Through the span's projection of its buffer.
        let pt = fl.mb.m.types.field(fl.f.value_ty(v), 0)?;
        let ptr = fl.value(pt, ir::Expr::Extract(v, 0));
        let at = ir::Place { root: ir::PlaceRoot::Ptr(ptr), path: vec![ir::Proj::Field(0)] };
        let handle = fl.value(u, ir::Expr::Load(at));
        let start = fl.value(u, ir::Expr::Extract(v, 1));
        let count = fl.value(u, ir::Expr::Extract(v, 2));
        let offset = mul(fl, start);
        let size = mul(fl, count);
        return Some([handle, offset, size]);
    }
    let handle = fl.value(u, ir::Expr::Extract(v, 0));
    let count = fl.value(u, ir::Expr::Extract(v, 1));
    let size = mul(fl, count);
    let zero = fl.u32c(0);
    let b = fl.mb.m.types.bool();
    let empty = fl.value(b, ir::Expr::Binary(ir::BinOp::Eq, count, zero));
    let size = fl.value(u, ir::Expr::Select { cond: empty, if_true: stride_v, if_false: size });
    Some([handle, zero, size])
}

/// An indirect dispatch's or draw's counts: their buffer's handle, and the byte offset where
/// they start.
fn indirect_binding(fl: &mut Fl, place: &mir::Place, span: Span) -> Option<[ir::ValueId; 2]> {
    // Its elements: `u32` words, or a command's typed arguments (`DrawArgs`, ...).
    let t = fl.place_src_ty(place);
    let elem = match fl.cx.checked.program.types.kind(t) {
        TyKind::Adt(_, args) if !args.is_empty() => args[0],
        _ => fl.cx.checked.program.types.u32,
    };
    let [handle, offset, _] = buffer_binding(fl, place, elem, span)?;
    Some([handle, offset])
}

pub(crate) fn lower_dispatch(fl: &mut Fl, d: &mir::Dispatch, span: Span) {
    recorded(fl, span, "dispatch", |fl| record_dispatch(fl, d, span));
}

/// Records a command with `record`, which is `None` when it couldn't: then, if the build has
/// no error that explains it, an internal error says so, rather than the command going missing
/// without a word.
fn recorded(fl: &mut Fl, span: Span, what: &str, record: impl FnOnce(&mut Fl) -> Option<()>) {
    if record(fl).is_none() && !wrela_diag::has_errors(&fl.cx.diags) {
        fl.cx.err(Diagnostic::internal(format!(
            "a {what} couldn't be recorded, at {}..{}",
            span.start, span.end
        )));
    }
}

/// A command's entry point, as this instance records it: the function, its generic arguments,
/// and where each parameter's argument is.
struct Recorded {
    func: FnId,
    substs: Vec<TyId>,
    /// A bound entry point's value, and the parameter each of its fields binds; `None` for an
    /// entry point named where it's recorded, whose arguments are the command's.
    value: Option<(mir::Place, Span, Vec<usize>)>,
}

impl Recorded {
    /// The place of parameter `param`'s argument: a field of the value, or one of the
    /// command's `args`.
    fn arg<'d>(
        &self,
        param: usize,
        args: impl IntoIterator<Item = (usize, &'d mir::Place, Span)>,
    ) -> Option<(mir::Place, Span)> {
        match &self.value {
            Some((place, at, params)) => {
                let k = params.iter().position(|&p| p == param)?;
                Some((place.with(mir::Proj::Field(k as u32)), *at))
            }
            None => args.into_iter().find(|a| a.0 == param).map(|(_, p, s)| (p.clone(), s)),
        }
    }
}

/// What `s` records in this instance: a bound entry point's value is of its entry point's
/// bound type here, whose fields hold the arguments (`gpu::bound_fields`).
fn recorded_shader(fl: &mut Fl, s: &mir::Shader) -> Option<Recorded> {
    match s {
        mir::Shader::Named(f, args) => {
            let substs = args.iter().map(|&a| fl.concrete(a)).collect();
            Some(Recorded { func: *f, substs, value: None })
        }
        mir::Shader::Value(place, at) => {
            let t = fl.place_src_ty(place);
            let program = &fl.cx.checked.program;
            let TyKind::Adt(a, args) = program.types.kind(t) else { return None };
            let func = program.adt(*a).entry?;
            let params =
                wrela_sema::gpu::bound_fields(program, func).iter().map(|b| b.param).collect();
            Some(Recorded { func, substs: args.clone(), value: Some((place.clone(), *at, params)) })
        }
    }
}

fn record_dispatch(fl: &mut Fl, d: &mir::Dispatch, span: Span) -> Option<()> {
    if fl.is_gpu() {
        host_effect(fl, "`dispatch`", span);
        return None;
    }
    let kernel = recorded_shader(fl, &d.kernel)?;
    // A kernel a generic function dispatches over a domain is known here (§12, E0603).
    if d.over
        && kernel.value.is_some()
        && wrela_sema::gpu::shares_memory(&fl.cx.checked.program, kernel.func)
    {
        fl.cx.err(wrela_sema::gpu::shared_over_domain(&fl.cx.checked.program, kernel.func, span));
        return None;
    }
    let key =
        PipelineKey::Compute { kernel: kernel.func, substs: kernel.substs.clone(), over: d.over };
    let (pindex, iface) = pipeline(fl.cx, key, span);
    let mut groups = Vec::new();
    match &d.indirect {
        Some((place, at)) => {
            let b = indirect_binding(fl, place, *at)?;
            groups.extend(b);
        }
        None => {
            for g in &d.groups {
                groups.push(fl.operand(g)?);
            }
        }
    }
    // Over a domain: `groups` is its size, which the groups cover, each a workgroup's.
    let domain = d.over.then(|| groups.clone());
    if let Some(size) = &domain {
        let wg = match fl.cx.entry_of(kernel.func) {
            Some(Entry::Compute(wg)) => wg,
            _ => [1, 1, 1],
        };
        let u = fl.mb.m.types.u32();
        groups = size
            .iter()
            .zip(wg)
            .map(|(&n, w)| {
                let (wv, w1) = (fl.u32c(w), fl.u32c(w - 1));
                let up = fl.value(u, ir::Expr::Binary(ir::BinOp::WrappingAdd, n, w1));
                fl.value(u, ir::Expr::Binary(ir::BinOp::Div, up, wv))
            })
            .collect();
    }
    let arg = |i: usize| kernel.arg(i, d.args.iter().map(|(i, p, s)| (*i, p, *s)));
    let (mut uniform_fields, mut uniform_vals) = uniforms(fl, &iface, |_, i| arg(i))?;
    if let Some(size) = domain {
        let v3 = fl.mb.m.types.vector_of(ir::Scalar::U32, 3);
        uniform_fields.push((DOMAIN.to_string(), v3));
        uniform_vals.push(fl.value(v3, ir::Expr::Construct(v3, size)));
    }
    let mut handles = bound(fl, &iface, |_, i| arg(i))?;
    // A lifted build's literal table (only a build with literals has one).
    handles.extend(fl.literal_binding(true).into_iter().flatten());
    let bindings = handles.len() as u32 / 3;
    let mut args = groups;
    args.extend(handles);
    let uniform = uniform_block(fl, &uniform_fields, &uniform_vals, &mut args, "dispatch", span);
    let op = ir::HostOp::Dispatch {
        pipeline: pindex,
        bindings,
        uniform,
        indirect: d.indirect.is_some(),
    };
    fl.emit(ir::Stmt::Eval(ir::Expr::Host(op, args)));
    Some(())
}

/// A uniform block's fields: each one's name and type.
type UniformFields = Vec<(String, ir::TypeId)>;

/// The uniform block's last field, of a kernel dispatched over a domain: the domain's size.
const DOMAIN: &str = "domain";

/// The uniform block's fields and their values: each uniform leaf whose argument (`arg` of an
/// entry point's parameter) has a runtime value, as `bind_uniform` makes the block. `None` if
/// one can't be read.
fn uniforms(
    fl: &mut Fl,
    iface: &Interface,
    arg: impl Fn(usize, usize) -> Option<(mir::Place, Span)>,
) -> Option<(UniformFields, Vec<ir::ValueId>)> {
    let mut fields = Vec::new();
    let mut vals = Vec::new();
    for u in &iface.uniforms {
        let Some((place, at)) = arg(u.entry, u.param) else { continue };
        // A value with no runtime value isn't in the block (`bind_uniform`).
        let Some(t) = fl.cx.lower_ty(fl.mb, u.ty, at) else { continue };
        vals.push(fl.read(&u.arg_place(&place))?);
        fields.push((u.field_name(), t));
    }
    Some((fields, vals))
}

/// The bindings of the interface's resources, in its order, from each entry point's
/// parameter's argument (`arg`). `None` if one can't be read.
fn bound(
    fl: &mut Fl,
    iface: &Interface,
    arg: impl Fn(usize, usize) -> Option<(mir::Place, Span)>,
) -> Option<Vec<ir::ValueId>> {
    let mut handles = Vec::new();
    for b in &iface.bound {
        let (place, at) = arg(b.entry, b.param)?;
        handles.extend(bindings_for(fl, b.ty, &b.class, &b.arg_place(&place), at)?);
    }
    Some(handles)
}

/// Packs uniform values into the block struct (same fields, same layout as the GPU side), last
/// in `args`: the block's type, if it has fields. E0702 if the block is too large to travel in
/// its command.
fn uniform_block(
    fl: &mut Fl,
    fields: &[(String, ir::TypeId)],
    vals: &[ir::ValueId],
    args: &mut Vec<ir::ValueId>,
    what: &str,
    span: Span,
) -> Option<ir::TypeId> {
    if fields.is_empty() {
        return None;
    }
    let t = fl
        .mb
        .m
        .types
        .intern(ir::TypeDef::Struct { name: format!("Uniforms_{what}"), fields: fields.to_vec() });
    let size = ir::layout::layout(&fl.mb.m.types, t).size;
    if size > ir::MAX_UNIFORM_BYTES {
        let max = ir::MAX_UNIFORM_BYTES;
        fl.cx.err(
            Diagnostic::new(
                codes::E0702,
                span,
                format!("this {what}'s `GpuData` arguments take {size} bytes; at most {max} fit in one command"),
            )
            .with_note("a dispatch's or draw's `GpuData` arguments travel inside its command, which the CPU program's 1 MiB command buffer holds")
            .with_help("put large data in a `GpuBuffer` (`buffer` and `write`) and pass that"),
        );
    }
    args.push(fl.value(t, ir::Expr::Construct(t, vals.to_vec())));
    Some(t)
}

pub(crate) fn lower_draw(fl: &mut Fl, d: &mir::Draw, span: Span) {
    recorded(fl, span, "draw", |fl| record_draw(fl, d, span));
}

fn record_draw(fl: &mut Fl, d: &mir::Draw, span: Span) -> Option<()> {
    if fl.is_gpu() {
        host_effect(fl, "`draw`", span);
        return None;
    }
    let shaders = [recorded_shader(fl, &d.vertex)?, recorded_shader(fl, &d.fragment)?];
    let vs = (shaders[0].func, shaders[0].substs.clone());
    let fs = (shaders[1].func, shaders[1].substs.clone());
    let key = PipelineKey::Render { vertex: vs, fragment: fs, state: d.state };
    let (pindex, iface) = pipeline(fl.cx, key, span);
    // An indexed draw's indices: their buffer's handle, and the byte offset and size, by its
    // elements' stride (`u32`s, or structs of them).
    let mut counts = Vec::new();
    if let Some((place, at)) = &d.indices {
        let t = fl.place_src_ty(place);
        let elem = match fl.cx.checked.program.types.kind(t) {
            TyKind::Adt(_, args) if !args.is_empty() => args[0],
            _ => fl.cx.checked.program.types.u32,
        };
        let b = buffer_binding(fl, place, elem, *at)?;
        counts.extend(b);
    }
    // The counts, or where their buffer holds them.
    match &d.indirect {
        Some((place, at)) => {
            let [handle, offset] = indirect_binding(fl, place, *at)?;
            counts.extend([handle, offset]);
        }
        None => {
            let vertices = fl.operand(&d.vertices)?;
            let instances = fl.operand(&d.instances)?;
            counts.extend([vertices, instances]);
        }
    };
    let arg = |e: usize, i: usize| {
        let args = d.args.iter().filter(|a| a.0 == e).map(|(_, i, p, s)| (*i, p, *s));
        shaders[e].arg(i, args)
    };
    let (fields, vals) = uniforms(fl, &iface, arg)?;
    // Buffers, textures and samplers, in the order the pipeline binds them.
    let mut handles = bound(fl, &iface, arg)?;
    handles.extend(fl.literal_binding(false).into_iter().flatten());
    let bindings = handles.len() as u32 / 3;
    let mut args = counts;
    args.extend(handles);
    let uniform = uniform_block(fl, &fields, &vals, &mut args, "draw", span);
    let op = ir::HostOp::Draw {
        pipeline: pindex,
        bindings,
        uniform,
        indirect: d.indirect.is_some(),
        indexed: d.indices.is_some(),
    };
    fl.emit(ir::Stmt::Eval(ir::Expr::Host(op, args)));
    Some(())
}

/// `std::gpu`'s and `std::derive`'s intrinsics.
pub(crate) fn intrinsic(
    fl: &mut Fl,
    func: FnId,
    substs: &[TyId],
    c: &mir::Call,
    ty: Option<TyId>,
) -> Option<ir::ValueId> {
    let lang = fl.cx.checked.program.func(func).lang;
    let span = c.span;
    match lang {
        Some(
            l @ (Lang::Gradient
            | Lang::ValueAndGradient
            | Lang::ValueGradientWith
            | Lang::IntervalOf),
        ) => {
            return crate::derive::call(fl, l, substs, c, ty);
        }
        Some(
            l @ (Lang::LiftGradient
            | Lang::LiftReads
            | Lang::LiftCount
            | Lang::LiftValue
            | Lang::LiftBuiltValue
            | Lang::LiftSet
            | Lang::LiftGeneration
            | Lang::LiftSource
            | Lang::LiftFiles
            | Lang::LiftFile),
        ) => {
            if fl.is_gpu() {
                let name = fl.cx.checked.program.func(func).name.clone();
                fl.cx.err(Diagnostic::new(
                    codes::E0600,
                    span,
                    format!("`{name}` reads a lifted build's table on the CPU; GPU code reads literals as written"),
                ));
                return None;
            }
            if matches!(l, Lang::LiftGradient | Lang::LiftReads) {
                return crate::derive::call(fl, l, substs, c, ty);
            }
            return crate::lift::intrinsic(fl, l, c, ty);
        }
        Some(l) if l.records_gpu_work() && fl.is_gpu() => {
            let name = fl.cx.checked.program.func(func).name.clone();
            host_effect(fl, &format!("`{name}`"), span);
            return None;
        }
        _ => {}
    }
    match lang {
        Some(Lang::Buffer) => {
            let Some(elem) = fl.cx.lower_ty(fl.mb, substs[0], span) else {
                fl.cx.holds_nothing("GPU buffer", substs[0], span);
                return None;
            };
            let elem_size = ir::layout::array_stride(&fl.mb.m.types, elem);
            let count = fl.arg_value(&c.args[0])?;
            let u = fl.mb.m.types.u32();
            let h =
                fl.value(u, ir::Expr::Host(ir::HostOp::CreateBuffer { elem_size }, vec![count]));
            let bt = fl.ty(ty?, span)?;
            Some(fl.value(bt, ir::Expr::Construct(bt, vec![h, count])))
        }
        Some(Lang::Write) => {
            let elem = fl.cx.lower_ty(fl.mb, substs[0], span)?;
            let elem_size = ir::layout::array_stride(&fl.mb.m.types, elem);
            let h = fl.arg_value(&c.args[0])?;
            let at = fl.arg_value(&c.args[1])?;
            let run_t = fl.mb.m.types.intern(ir::TypeDef::Run(elem));
            let run = fl.run_arg(c.args[2].place()?, run_t)?;
            fl.emit(ir::Stmt::Eval(ir::Expr::Host(
                ir::HostOp::WriteBuffer { elem, elem_size },
                vec![h, at, run],
            )));
            None
        }
        Some(Lang::DestroyBuffer) => {
            let h = fl.arg_value(&c.args[0])?;
            fl.emit(ir::Stmt::Eval(ir::Expr::Host(ir::HostOp::DestroyBuffer, vec![h])));
            None
        }
        Some(Lang::CopyBuffer) => {
            let mut args = Vec::new();
            for a in &c.args {
                args.push(fl.arg_value(a)?);
            }
            fl.emit(ir::Stmt::Eval(ir::Expr::Host(ir::HostOp::CopyBuffer, args)));
            None
        }
        Some(Lang::BeginScreenPass) => {
            let clear = fl.arg_value(&c.args[0])?;
            // A lifted build's literals reach the GPU before the pass: a pass holds only draws.
            fl.upload_literals();
            fl.emit(ir::Stmt::Eval(ir::Expr::Host(ir::HostOp::BeginScreenPass, vec![clear])));
            None
        }
        Some(Lang::Present) => {
            fl.emit(ir::Stmt::Eval(ir::Expr::Host(ir::HostOp::Present, Vec::new())));
            None
        }
        Some(
            l @ (Lang::CreateTexture
            | Lang::CreateTexture3d
            | Lang::DestroyTexture
            | Lang::CreateSampler
            | Lang::DestroySampler
            | Lang::BeginPass
            | Lang::EndPass),
        ) => {
            // (opcode, payload words, whether the first is a new handle)
            let (opcode, words, handle) = match l {
                Lang::CreateTexture => (Opcode::CreateTexture, 4, true),
                Lang::CreateTexture3d => (Opcode::CreateTexture, 5, true),
                Lang::DestroyTexture => (Opcode::DestroyTexture, 1, false),
                Lang::CreateSampler => (Opcode::CreateSampler, 4, true),
                Lang::DestroySampler => (Opcode::DestroySampler, 1, false),
                Lang::BeginPass => (Opcode::BeginPass, 9, false),
                _ => (Opcode::EndPass, 0, false),
            };
            let mut args = Vec::new();
            for a in &c.args {
                args.push(fl.arg_value(a)?);
            }
            if l == Lang::BeginPass {
                fl.upload_literals();
            }
            let op = ir::HostOp::Command { opcode: opcode as u32, words, runs: 0, handle };
            if handle {
                let u = fl.mb.m.types.u32();
                return Some(fl.value(u, ir::Expr::Host(op, args)));
            }
            fl.emit(ir::Stmt::Eval(ir::Expr::Host(op, args)));
            None
        }
        Some(
            l @ (Lang::NextRequest
            | Lang::RequestStatus
            | Lang::RequestTake
            | Lang::Limit
            | Lang::InputTake
            | Lang::Keep
            | Lang::Kept),
        ) if fl.is_gpu() => {
            let name = fl.cx.checked.program.func(func).name.clone();
            fl.cx.err(Diagnostic::new(
                codes::E0600,
                span,
                format!("`{name}` asks the host, and GPU code can't ({l:?})"),
            ));
            None
        }
        Some(Lang::Limit) if !fl.is_gpu() => {
            let i = fl.arg_value(&c.args[0])?;
            let u = fl.mb.m.types.u32();
            Some(fl.value(u, ir::Expr::Host(ir::HostOp::Limit, vec![i])))
        }
        Some(Lang::NextRequest) => {
            let u = fl.mb.m.types.u32();
            Some(fl.value(u, ir::Expr::Host(ir::HostOp::NextRequest, Vec::new())))
        }
        Some(Lang::RequestStatus) => {
            let r = fl.arg_value(&c.args[0])?;
            let i = fl.mb.m.types.scalar(ir::Scalar::I32);
            Some(fl.value(i, ir::Expr::Host(ir::HostOp::RequestStatus, vec![r])))
        }
        Some(Lang::Keep) => {
            let run = fl.arg_value(&c.args[0])?;
            let u = fl.mb.m.types.u32();
            let at = fl.value(u, ir::Expr::Extract(run, 0));
            let len = fl.value(u, ir::Expr::Extract(run, 1));
            fl.mb.m.reload = true;
            fl.emit(ir::Stmt::Eval(ir::Expr::Host(ir::HostOp::Keep, vec![at, len])));
            None
        }
        Some(Lang::Kept) => {
            let at = fl.arg_value(&c.args[0])?;
            let cap = fl.arg_value(&c.args[1])?;
            fl.mb.m.reload = true;
            let u = fl.mb.m.types.u32();
            Some(fl.value(u, ir::Expr::Host(ir::HostOp::Kept, vec![at, cap])))
        }
        Some(Lang::InputTake) => {
            let at = fl.arg_value(&c.args[0])?;
            let cap = fl.arg_value(&c.args[1])?;
            fl.mb.m.input = true;
            let u = fl.mb.m.types.u32();
            Some(fl.value(u, ir::Expr::Host(ir::HostOp::Input, vec![at, cap])))
        }
        Some(Lang::RequestTake) => {
            let r = fl.arg_value(&c.args[0])?;
            let at = fl.arg_value(&c.args[1])?;
            fl.emit(ir::Stmt::Eval(ir::Expr::Host(ir::HostOp::RequestTake, vec![r, at])));
            None
        }
        Some(
            l @ (Lang::ReadBufferCommand
            | Lang::StorageReadCommand
            | Lang::StorageWriteCommand
            | Lang::FetchCommand
            | Lang::PrintCommand
            | Lang::LabelCommand
            | Lang::PostCommand),
        ) => {
            // The request number and (a readback) the buffer's range, then the runs: a path or
            // URL, the bytes to store, a line to print, a label.
            let (opcode, words) = match l {
                Lang::ReadBufferCommand => (Opcode::ReadBuffer, 4),
                Lang::StorageReadCommand => (Opcode::StorageRead, 1),
                Lang::StorageWriteCommand => (Opcode::StorageWrite, 1),
                Lang::PrintCommand => (Opcode::Log, 0),
                Lang::LabelCommand => (Opcode::Label, 0),
                Lang::PostCommand => (Opcode::Post, 1),
                _ => (Opcode::Fetch, 1),
            };
            let mut args = Vec::new();
            for a in &c.args[..words as usize] {
                args.push(fl.arg_value(a)?);
            }
            for a in &c.args[words as usize..] {
                args.push(fl.str_arg(a)?);
            }
            let runs = (c.args.len() - words as usize) as u32;
            let op = ir::HostOp::Command { opcode: opcode as u32, words, runs, handle: false };
            fl.emit(ir::Stmt::Eval(ir::Expr::Host(op, args)));
            None
        }
        Some(Lang::WriteTexture) => {
            // handle, x, y, width, height, then bytes `from..from + count` of the run.
            let mut args = Vec::new();
            for a in &c.args[..5] {
                args.push(fl.arg_value(a)?);
            }
            let u8_ = fl.mb.m.types.scalar(ir::Scalar::U8);
            let run_t = fl.mb.m.types.intern(ir::TypeDef::Run(u8_));
            let run = fl.run_arg(c.args[5].place()?, run_t)?;
            let from = fl.arg_value(&c.args[6])?;
            let count = fl.arg_value(&c.args[7])?;
            let u = fl.mb.m.types.u32();
            let addr = fl.value(u, ir::Expr::Extract(run, 0));
            let start = fl.value(u, ir::Expr::Binary(ir::BinOp::Add, addr, from));
            args.push(fl.value(run_t, ir::Expr::Construct(run_t, vec![start, count])));
            let op = ir::HostOp::Command {
                opcode: Opcode::WriteTexture as u32,
                words: 5,
                runs: 1,
                handle: false,
            };
            fl.emit(ir::Stmt::Eval(ir::Expr::Host(op, args)));
            None
        }
        Some(
            l @ (Lang::LocalIndex
            | Lang::WorkgroupInvocations
            | Lang::SharedGet
            | Lang::SharedSet
            | Lang::WorkgroupBarrier
            | Lang::AtomicLen
            | Lang::AtomicLoad
            | Lang::AtomicStore
            | Lang::AtomicAdd
            | Lang::AtomicSub
            | Lang::AtomicMin
            | Lang::AtomicMax
            | Lang::AtomicAnd
            | Lang::AtomicOr
            | Lang::AtomicXor
            | Lang::AtomicExchange
            | Lang::AtomicCompareExchange
            | Lang::AppendPush
            | Lang::TexelsStore
            | Lang::Texels3dStore
            | Lang::OneGet
            | Lang::OneSet),
        ) => shared_op(fl, l, c, ty),
        Some(Lang::Discard) => {
            let at = user_span(fl, c.span);
            let ok = fragment_only(fl, None, at, |what| {
                Diagnostic::new(
                    codes::E0607,
                    at,
                    format!("`discard` is for fragment shaders, and this is {what}"),
                )
                .with_note("`discard` drops the fragment a fragment shader is shading; a kernel, a vertex shader and CPU code have no fragment to drop")
                .with_help("make the test in the fragment shader, passing it what it needs from the vertex shader")
            });
            if ok {
                fl.emit(ir::Stmt::Eval(ir::Expr::Discard));
            }
            None
        }
        Some(
            l @ (Lang::TextureSample
            | Lang::TextureSampleLevel
            | Lang::TextureSampleCompare
            | Lang::TextureSampleCompareLevel
            | Lang::TextureLoad
            | Lang::DepthLoad
            | Lang::Texture3dSampleLevel
            | Lang::Texture3dLoad
            | Lang::TextureWidth
            | Lang::TextureHeight
            | Lang::TextureDepth),
        ) => texture_read(fl, l, c, ty),
        // A field of what a `GpuField` names: its offset moved by the field's in the struct's
        // layout. CPU code only (GPU code doesn't name buffers).
        Some(Lang::GpuFieldAt) => {
            if fl.is_gpu() {
                let at = user_span(fl, c.span);
                fl.cx.err(Diagnostic::new(
                    codes::E0607,
                    at,
                    "a `GpuField` names a value in a buffer for CPU code's commands, and this is GPU code",
                ));
                return None;
            }
            let u = fl.mb.m.types.u32();
            let f = fl.arg_value(&c.args[0])?;
            let k = match fl.cx.checked.program.types.kind(substs[3]) {
                TyKind::ConstU32(k) => *k as usize,
                _ => return None,
            };
            let inner = fl.cx.lower_ty(fl.mb, substs[1], c.span)?;
            let off = ir::layout::field_offsets(&fl.mb.m.types, inner).get(k).copied()?;
            let off = fl.u32c(off);
            let base = fl.value(u, ir::Expr::Extract(f, 1));
            let moved = fl.value(u, ir::Expr::Binary(ir::BinOp::Add, base, off));
            let t = fl.ty(ty?, c.span)?;
            let ptr = fl.value(fl.mb.m.types.field(t, 0)?, ir::Expr::Extract(f, 0));
            Some(fl.value(t, ir::Expr::Construct(t, vec![ptr, moved])))
        }
        // A span's length: its count on the CPU, its binding's on the GPU (bound to its range).
        Some(Lang::SpanLen) => {
            let u = fl.mb.m.types.u32();
            let p = c.args[0].place()?;
            if fl.is_gpu() {
                let place = fl.place(p)?;
                return Some(fl.value(u, ir::Expr::ArrayLength(place)));
            }
            let v = fl.read(p)?;
            Some(fl.value(u, ir::Expr::Extract(v, 2)))
        }
        _ => {
            let name = fl.cx.checked.program.func(func).name.clone();
            fl.cx.err(Diagnostic::internal(format!("the intrinsic `{name}` has no lowering")));
            None
        }
    }
}

/// GPU code recorded GPU work: the `host` effect, which every GPU context forbids (§8).
fn host_effect(fl: &mut Fl, what: &str, span: Span) {
    let ctx = fl.mb.gpu.as_ref().map_or("GPU code", |g| g.what.as_str());
    let d = Diagnostic::new(
        codes::E0600,
        span,
        format!("{what} records GPU work, which {ctx} can't do"),
    )
    .with_note(
        "recording GPU work is the `host` effect; GPU entry points forbid it (language.md §8)",
    );
    fl.cx.err(with_call_chain(fl, d));
}

/// `d`, with a note naming the call chain from the entry point down to the current function
/// (`fill → helper → scratch`) when the current function isn't the entry point itself.
fn with_call_chain(fl: &Fl, d: Diagnostic) -> Diagnostic {
    let mut names = vec![fl.mb.display_name(fl.id)];
    let mut at = fl.id;
    while let Some(&(caller, _)) = fl.mb.callers.get(&at) {
        let name = fl.mb.display_name(caller);
        // An entry point's wrapper has the entry point's name.
        if names.last() != Some(&name) {
            names.push(name);
        }
        at = caller;
        if names.len() > 32 {
            break;
        }
    }
    if names.len() < 2 {
        return d;
    }
    names.reverse();
    d.with_note(called_through(&names))
}

/// `d`, with a note naming the calls from the entry point down to the function of `n`, when
/// that isn't the entry point itself.
fn with_path(mb: &ModuleBuilder, n: &NonUniform, d: Diagnostic) -> Diagnostic {
    if n.path.is_empty() {
        return d;
    }
    let names: Vec<String> = n.path.iter().chain([&n.func]).map(|&f| mb.display_name(f)).collect();
    d.with_note(called_through(&names))
}

/// The note that names a call chain, callers first.
fn called_through(names: &[String]) -> String {
    let chain: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
    format!("called through {}", chain.join(" → "))
}

/// The resource GPU code passes as call `c`'s argument `i`: a parameter bound to one, passed
/// on as it was received (E0702 with `msg` otherwise).
fn resource_arg(fl: &mut Fl, c: &mir::Call, i: usize, msg: &str) -> Option<ir::ResourceId> {
    let place = c.args.get(i)?.place()?;
    let pl = fl.place(place)?;
    match (pl.root, pl.path.is_empty()) {
        (ir::PlaceRoot::Resource(r), true) => Some(r),
        _ => {
            fl.cx.err(Diagnostic::new(codes::E0702, c.args[i].span(), msg));
            None
        }
    }
}

/// Workgroup memory, barriers, atomics and appends (`std::gpu`'s kernel `mut` parameters):
/// GPU code only (E0607).
fn shared_op(fl: &mut Fl, l: Lang, c: &mir::Call, ty: Option<TyId>) -> Option<ir::ValueId> {
    let span = c.span;
    if !fl.is_gpu() {
        let name = fl.cx.checked.program.func(match &c.callee {
            mir::Callee::Fn { func, .. } => *func,
            _ => return None,
        });
        let d = Diagnostic::new(
            codes::E0607,
            user_span(fl, span),
            format!("`{}` is for kernels, and this is CPU code", name.name),
        )
        .with_note("workgroup memory, atomics and appends are a kernel's `mut` parameters");
        fl.cx.err(with_call_chain(fl, d));
        return None;
    }
    let msg = "GPU code can pass a resource on only as the kernel received it";
    let u = fl.mb.m.types.u32();
    match l {
        Lang::LocalIndex | Lang::WorkgroupInvocations => {
            let g = fl.mb.gpu.as_ref()?;
            let [wx, wy, wz] = g.workgroup_size;
            if l == Lang::WorkgroupInvocations {
                return Some(fl.u32c(wx * wy * wz));
            }
            let lid = fl.arg_value(&c.args[0])?;
            let x = fl.value(u, ir::Expr::Extract(lid, 0));
            let y = fl.value(u, ir::Expr::Extract(lid, 1));
            let z = fl.value(u, ir::Expr::Extract(lid, 2));
            let (cx_, cxy) = (fl.u32c(wx), fl.u32c(wx * wy));
            let yx = fl.value(u, ir::Expr::Binary(ir::BinOp::Mul, y, cx_));
            let zxy = fl.value(u, ir::Expr::Binary(ir::BinOp::Mul, z, cxy));
            let s = fl.value(u, ir::Expr::Binary(ir::BinOp::Add, x, yx));
            Some(fl.value(u, ir::Expr::Binary(ir::BinOp::Add, s, zxy)))
        }
        Lang::WorkgroupBarrier => {
            resource_arg(fl, c, 0, msg)?;
            fl.emit(ir::Stmt::Eval(ir::Expr::Barrier));
            None
        }
        Lang::TexelsStore | Lang::Texels3dStore => {
            let r = resource_arg(fl, c, 0, msg)?;
            let mut vals = Vec::new();
            for a in &c.args[1..] {
                vals.push(fl.arg_value(a)?);
            }
            // The texel, written as four of its format's scalar (the channels it lacks, 0).
            if let ir::ResourceKind::StorageTexture { format, .. } =
                fl.mb.m.resources[r.index()].kind
                && let Some(last) = vals.pop()
            {
                vals.push(widen(fl, last, format.scalar()));
            }
            fl.emit(ir::Stmt::Eval(ir::Expr::TextureStore(r, vals)));
            None
        }
        Lang::SharedGet | Lang::SharedSet | Lang::AtomicLoad | Lang::AtomicStore => {
            let r = resource_arg(fl, c, 0, msg)?;
            let i = fl.arg_value(&c.args[1])?;
            let place =
                ir::Place { root: ir::PlaceRoot::Resource(r), path: vec![ir::Proj::Index(i)] };
            match l {
                Lang::SharedGet => {
                    let t = fl.ty(ty?, span)?;
                    Some(fl.value(t, ir::Expr::Load(place)))
                }
                Lang::SharedSet => {
                    let v = fl.arg_value(&c.args[2])?;
                    fl.emit(ir::Stmt::Store(place, v));
                    None
                }
                Lang::AtomicLoad => {
                    let t = fl.ty(ty?, span)?;
                    Some(fl.value(t, ir::Expr::Atomic(ir::AtomicOp::Load, place, Vec::new())))
                }
                _ => {
                    let v = fl.arg_value(&c.args[2])?;
                    let e = ir::Expr::Atomic(ir::AtomicOp::Store, place, vec![v]);
                    fl.emit(ir::Stmt::Eval(e));
                    None
                }
            }
        }
        // A `One<T>`'s value: its buffer's first element (std guards who sets it).
        Lang::OneGet | Lang::OneSet => {
            let r = resource_arg(fl, c, 0, msg)?;
            let zero = fl.u32c(0);
            let place =
                ir::Place { root: ir::PlaceRoot::Resource(r), path: vec![ir::Proj::Index(zero)] };
            if l == Lang::OneGet {
                let t = fl.ty(ty?, span)?;
                return Some(fl.value(t, ir::Expr::Load(place)));
            }
            let v = fl.arg_value(&c.args[1])?;
            fl.emit(ir::Stmt::Store(place, v));
            None
        }
        Lang::AtomicLen => {
            let r = resource_arg(fl, c, 0, msg)?;
            Some(fl.value(u, ir::Expr::ArrayLength(ir::Place::root(ir::PlaceRoot::Resource(r)))))
        }
        Lang::AppendPush => {
            // count[0] += 1; if the old count is below the capacity, the item goes there.
            let r = resource_arg(fl, c, 0, msg)?;
            let count = ir::ResourceId(r.0 + 1);
            let v = fl.arg_value(&c.args[1])?;
            let zero = fl.u32c(0);
            let one = fl.u32c(1);
            let at = ir::Place {
                root: ir::PlaceRoot::Resource(count),
                path: vec![ir::Proj::Index(zero)],
            };
            let old = fl.value(u, ir::Expr::Atomic(ir::AtomicOp::Add, at, vec![one]));
            let cap =
                fl.value(u, ir::Expr::ArrayLength(ir::Place::root(ir::PlaceRoot::Resource(r))));
            let b = fl.mb.m.types.bool();
            let room = fl.value(b, ir::Expr::Binary(ir::BinOp::Lt, old, cap));
            let slot =
                ir::Place { root: ir::PlaceRoot::Resource(r), path: vec![ir::Proj::Index(old)] };
            fl.emit(ir::Stmt::If {
                cond: room,
                then: vec![ir::Stmt::Store(slot, v)],
                else_: Vec::new(),
            });
            Some(old)
        }
        _ => {
            let op = match l {
                Lang::AtomicAdd => ir::AtomicOp::Add,
                Lang::AtomicSub => ir::AtomicOp::Sub,
                Lang::AtomicMin => ir::AtomicOp::Min,
                Lang::AtomicMax => ir::AtomicOp::Max,
                Lang::AtomicAnd => ir::AtomicOp::And,
                Lang::AtomicOr => ir::AtomicOp::Or,
                Lang::AtomicXor => ir::AtomicOp::Xor,
                Lang::AtomicExchange => ir::AtomicOp::Exchange,
                _ => ir::AtomicOp::CompareExchange,
            };
            let r = resource_arg(fl, c, 0, msg)?;
            let i = fl.arg_value(&c.args[1])?;
            let mut vals = Vec::new();
            for a in &c.args[2..] {
                vals.push(fl.arg_value(a)?);
            }
            let place =
                ir::Place { root: ir::PlaceRoot::Resource(r), path: vec![ir::Proj::Index(i)] };
            let t = fl.ty(ty?, span)?;
            Some(fl.value(t, ir::Expr::Atomic(op, place, vals)))
        }
    }
}

/// Where to report an error found at `span` in the function being lowered: there, or, when
/// that's std's code (a texture method's body), at the first call to it from outside std.
fn user_span(fl: &Fl, span: Span) -> Span {
    user_span_in(fl.cx, fl.mb, fl.id, span)
}

/// [`user_span`] for a span in function `id` of `mb`.
fn user_span_in(cx: &Cx, mb: &ModuleBuilder, id: ir::FuncId, span: Span) -> Span {
    let p = &cx.checked.program;
    let in_std = |id: ir::FuncId| {
        let key = mb.instances.iter().find(|(_, v)| **v == id).map(|(k, _)| k);
        key.and_then(InstanceKey::source_fn).is_some_and(|f| p.is_std(p.func(f).module))
    };
    let (mut at, mut span) = (id, span);
    while in_std(at) {
        let Some(&(caller, s)) = mb.callers.get(&at) else { break };
        (at, span) = (caller, s);
    }
    span
}

/// A texture read (`Texture::sample` and the rest), in GPU code. On the CPU a texture's size
/// is its fields; its texels are E0607.
fn texture_read(fl: &mut Fl, l: Lang, c: &mir::Call, ty: Option<TyId>) -> Option<ir::ValueId> {
    use ir::TextureOp as T;
    let span = c.span;
    let u = fl.mb.m.types.u32();
    let op = match l {
        Lang::TextureSample => T::Sample,
        Lang::TextureSampleLevel | Lang::Texture3dSampleLevel => T::SampleLevel,
        Lang::TextureSampleCompare => T::SampleCompare,
        Lang::TextureSampleCompareLevel => T::SampleCompareLevel,
        Lang::TextureLoad | Lang::DepthLoad | Lang::Texture3dLoad => T::Load,
        Lang::TextureWidth => T::Width,
        Lang::TextureDepth => T::Depth,
        _ => T::Height,
    };
    if !fl.is_gpu() {
        if matches!(op, T::Width | T::Height | T::Depth) {
            // `Texture`, `DepthTexture` and `Texture3d` hold their handle, width and height
            // first (and a `Texture3d` its depth next).
            let field = match op {
                T::Width => 1,
                T::Height => 2,
                _ => 3,
            };
            let v = fl.arg_value(&c.args[0])?;
            return Some(fl.value(u, ir::Expr::Extract(v, field)));
        }
        let d = Diagnostic::new(
            codes::E0607,
            user_span(fl, span),
            "a texture's texels are read in GPU code, and this is CPU code",
        )
        .with_note("CPU code can write a texture and pass it to shaders, but not read it");
        fl.cx.err(with_call_chain(fl, d));
        return None;
    }
    let msg = "GPU code can pass a texture or sampler on only as the shader received it";
    let texture = resource_arg(fl, c, 0, msg)?;
    let sampled = !matches!(op, T::Load | T::Width | T::Height | T::Depth);
    let sampler = if sampled { Some(resource_arg(fl, c, 1, msg)?) } else { None };
    let mut vals = Vec::new();
    for a in &c.args[1 + usize::from(sampled)..] {
        vals.push(fl.arg_value(a)?);
    }
    let t = fl.ty(ty?, span)?;
    // A colour texel is read as four of its format's scalar: the format's texel is their first.
    let colour = match fl.mb.m.resources[texture.index()].kind {
        ir::ResourceKind::Texture { depth: false, format, .. } => Some(format),
        _ => None,
    };
    let texel = !matches!(op, T::Width | T::Height | T::Depth);
    let read = match colour {
        Some(f) if texel => fl.mb.m.types.vector_of(f.scalar(), 4),
        _ => t,
    };
    let v = fl.value(read, ir::Expr::Texture(op, texture, sampler, vals));
    let v = narrow(fl, v, t);
    if op.uses_derivatives() {
        // `sample` and `sample_compare` pick a texture's detail from neighbouring pixels, as
        // derivatives do.
        let at = user_span(fl, span);
        fragment_only(fl, Some(v), at, |ctx| {
            Diagnostic::new(
                codes::E0607,
                at,
                format!("sampling picks its detail from neighbouring pixels, so it's in fragment shaders only, and this is {ctx}"),
            )
            .with_help("use `sample_level` (or `sample_compare_level`), which any GPU code can call")
        });
    }
    Some(v)
}

/// `v`, a vector of four, as a value of type `t`: its first component, or first components, if
/// `t` has fewer (a format's texel, read as four of its scalar).
fn narrow(fl: &mut Fl, v: ir::ValueId, t: ir::TypeId) -> ir::ValueId {
    if fl.f.value_ty(v) == t {
        return v;
    }
    match *fl.mb.m.types.get(t) {
        ir::TypeDef::Scalar(_) => fl.value(t, ir::Expr::Extract(v, 0)),
        ir::TypeDef::Vector(_, n) => fl.value(t, ir::Expr::Swizzle(v, (0..n).collect())),
        _ => v,
    }
}

/// `v`, a format's texel (a scalar or a vector of `s`), as four of `s`: the components it lacks
/// 0, as a texture's write takes it.
fn widen(fl: &mut Fl, v: ir::ValueId, s: ir::Scalar) -> ir::ValueId {
    let four = fl.mb.m.types.vector_of(s, 4);
    let vt = fl.f.value_ty(v);
    if vt == four {
        return v;
    }
    let st = fl.mb.m.types.scalar(s);
    let zero = fl.value(st, ir::Expr::Const(crate::eval::zero_scalar(s)));
    let mut parts = match *fl.mb.m.types.get(vt) {
        ir::TypeDef::Vector(_, n) => {
            (0..u32::from(n)).map(|k| fl.value(st, ir::Expr::Extract(v, k))).collect()
        }
        _ => vec![v],
    };
    parts.resize(4, zero);
    fl.value(four, ir::Expr::Construct(four, parts))
}

/// E0607 for a derivative outside a fragment shader. In one, where it is (`v`), for E0608.
pub(crate) fn check_derivative(fl: &mut Fl, v: Option<ir::ValueId>, span: Span) {
    fragment_only(fl, v, span, |ctx| {
        Diagnostic::new(
            codes::E0607,
            span,
            format!("derivatives exist only in fragment shaders, and this is {ctx}"),
        )
        .with_note("`dpdx`, `dpdy` and `fwidth` compare neighbouring pixels")
    });
}

/// An operation for fragment shaders only (one that compares neighbouring pixels, defining `v`,
/// at `span`): in a fragment shader, `v` is noted for E0608; elsewhere, the error `err` makes
/// from what the code is. Whether the code may do it.
fn fragment_only(
    fl: &mut Fl,
    v: Option<ir::ValueId>,
    span: Span,
    err: impl FnOnce(&str) -> Diagnostic,
) -> bool {
    // A `@gpu` function checked on its own has no stage: its callers' decide, and each
    // pipeline that calls it checks it there.
    if fl.mb.gpu.as_ref().is_some_and(|g| g.stage.is_none()) {
        return true;
    }
    if fl.mb.gpu.as_ref().is_some_and(|g| g.stage == Some(Entry::Fragment)) {
        if let Some(v) = v {
            fl.mb.derivatives.insert((fl.id, v), span);
        }
        return true;
    }
    let d = err(fl.mb.gpu.as_ref().map_or("CPU code", |g| g.what.as_str()));
    fl.cx.err(with_call_chain(fl, d));
    false
}

/// A kernel's workgroup memory: E0610 for a barrier that its workgroup's invocations may not
/// all reach together, E0609 for a read or write of workgroup memory with no barrier since an
/// access of the other kind (§6.13).
fn check_workgroup(
    cx: &mut Cx,
    mb: &ModuleBuilder,
    entry: ir::FuncId,
    inputs: &[ir::BuiltinInput],
) {
    let found = ir::uniformity::collectives_in_non_uniform_flow(&mb.m, entry, inputs);
    for n in found.into_iter().filter(|n| n.op == Collective::Barrier) {
        let Some(at) = n.at else { continue };
        let d = Diagnostic::new(
            codes::E0610,
            user_span_in(cx, mb, n.func, at),
            "`barrier` is called where the workgroup's invocations may have taken different branches",
        )
        .with_note("a barrier waits for every invocation of the workgroup, so all of them must reach it together (WGSL's uniformity rule)");
        cx.err(with_path(mb, &n, d).with_help(
            "call it outside the branch, or make the branch's condition the same for the whole workgroup (from `WorkgroupId` or uniforms)",
        ));
    }
    for h in ir::workgroup::hazards(&mb.m, entry) {
        let Some(at) = h.at else { continue };
        let name = mb.m.resources[h.resource.index()].name.clone();
        let (what, other) = if h.write {
            ("written", "another invocation may still be reading it")
        } else {
            ("read", "another invocation may still be writing it")
        };
        cx.err(
            Diagnostic::new(
                codes::E0609,
                user_span_in(cx, mb, h.func, at),
                format!("`{name}` is {what} here, and {other}: there's no barrier since"),
            )
            .with_note("between barriers, workgroup memory is written (each invocation its own chunk) or read, not both (§6.13)")
            .with_help(format!("call `barrier(mut {name})` between the writes and the reads")),
        );
    }
}

/// E0608: the derivatives the fragment shader `entry` reaches in non-uniform control flow,
/// which WGSL rejects (language.md §11).
fn check_uniformity(
    cx: &mut Cx,
    mb: &ModuleBuilder,
    entry: ir::FuncId,
    inputs: &[ir::BuiltinInput],
) {
    if mb.derivatives.is_empty() {
        return;
    }
    for n in ir::uniformity::collectives_in_non_uniform_flow(&mb.m, entry, inputs) {
        let at = mb.derivatives.get(&(n.func, n.value)).copied().or(n.at);
        let Some(span) = at else { continue };
        let span = user_span_in(cx, mb, n.func, span);
        // The texture methods by their names in the language.
        let name = match n.op {
            Collective::Sample => "sample",
            Collective::SampleCompare => "sample_compare",
            op => op.wgsl_name(),
        };
        let d = Diagnostic::new(
            codes::E0608,
            span,
            format!(
                "`{name}` is called where neighbouring pixels may have taken different branches"
            ),
        )
        .with_note(
            "a derivative compares a pixel with its neighbours, so WGSL requires all of them to \
             reach it together (language.md §11)",
        );
        cx.err(with_path(mb, &n, d).with_help(
            "compute it before the branch or the early `return`, and use the result after it",
        ));
    }
}

/// The index of `out[id]` for a `Slots` write: the invocation's position in the whole dispatch,
/// row by row.
pub(crate) fn slot_index(fl: &mut Fl, idv: ir::ValueId) -> Option<ir::ValueId> {
    let u = fl.mb.m.types.u32();
    let x = fl.value(u, ir::Expr::Extract(idv, 0));
    let y = fl.value(u, ir::Expr::Extract(idv, 1));
    let z = fl.value(u, ir::Expr::Extract(idv, 2));
    let (nwg, wg) = match fl.mb.gpu.as_ref() {
        Some(g) => (g.num_workgroups, g.workgroup_size),
        None => return Some(x),
    };
    let Some(nwg) = nwg else { return Some(x) };
    let nt = fl.mb.m.resources[nwg.index()].ty;
    let n = fl.load(ir::Place::root(ir::PlaceRoot::Resource(nwg)), nt);
    let nx = fl.value(u, ir::Expr::Extract(n, 0));
    let ny = fl.value(u, ir::Expr::Extract(n, 1));
    let wx = fl.u32c(wg[0]);
    let wy = fl.u32c(wg[1]);
    let w = fl.value(u, ir::Expr::Binary(ir::BinOp::Mul, nx, wx));
    let h = fl.value(u, ir::Expr::Binary(ir::BinOp::Mul, ny, wy));
    let row = fl.value(u, ir::Expr::Binary(ir::BinOp::Mul, y, w));
    let wh = fl.value(u, ir::Expr::Binary(ir::BinOp::Mul, w, h));
    let plane = fl.value(u, ir::Expr::Binary(ir::BinOp::Mul, z, wh));
    let a = fl.value(u, ir::Expr::Binary(ir::BinOp::Add, x, row));
    Some(fl.value(u, ir::Expr::Binary(ir::BinOp::Add, a, plane)))
}

// ---- GPU side: pipelines ------------------------------------------------------------------------

pub(crate) fn lower_pipeline(
    cx: &mut Cx,
    key: &PipelineKey,
    iface: Rc<Interface>,
    sites: Vec<Span>,
) -> Option<PipelineOut> {
    let mut out = match key {
        PipelineKey::Compute { .. } => lower_compute(cx, iface),
        PipelineKey::Render { .. } => lower_render(cx, iface),
    }?;
    out.key = key.clone();
    out.sites = sites;
    Some(out)
}

/// How diagnostics name the entry point `name` of a module: "the `@compute` kernel `fill`".
fn describe_entry(stage: Entry, name: &str) -> String {
    match stage {
        Entry::Compute(_) => format!("the `@compute` kernel `{name}`"),
        Entry::Vertex => format!("the vertex shader `{name}`"),
        Entry::Fragment => format!("the fragment shader `{name}`"),
    }
}

/// A GPU module for some entry points, before their functions are lowered.
struct EntryModule {
    mb: ModuleBuilder,
    /// Each entry point's instance key, with the resource bound to each parameter.
    keys: Vec<InstanceKey>,
    bindings: Vec<ResourceBinding>,
    uniform: Option<UniformOut>,
}

/// A GPU module for the entry points of `iface`, the first of them a `stage`: its buffers
/// bound (bindings from 1, in the interface's order), its uniform block (binding 0) and, for a
/// kernel, `num_workgroups`.
fn entry_module(cx: &mut Cx, iface: Rc<Interface>, stage: Entry) -> EntryModule {
    let checked = cx.checked;
    let program = &checked.program;
    let what = describe_entry(stage, &program.func(iface.entries[0].func).name);
    let mut mb = ModuleBuilder::gpu(GpuCx::new(what.clone(), iface.clone(), Some(stage)));
    // What each parameter is bound to: a group's fields as they're bound below.
    let mut resources: Vec<Vec<Option<Bound>>> = Vec::new();
    for entry in &iface.entries {
        let mut params = Vec::new();
        for &(t, ref class) in &entry.params {
            params.push((*class == ParamClass::Group).then(|| group_skeleton(cx, t)));
        }
        resources.push(params);
    }
    let bind = |resources: &mut Vec<Vec<Option<Bound>>>, leaf: &Leaf, r: ir::ResourceId| {
        let slot = &mut resources[leaf.entry][leaf.param];
        match slot {
            Some(group) if !leaf.path.is_empty() => bind_at(group, &leaf.path, r),
            _ => *slot = Some(Bound::One(r)),
        }
    };
    let mut bindings = Vec::new();
    for leaf in &iface.bound {
        let (e, i) = (leaf.entry, leaf.param);
        let param = &program.func(iface.entries[e].func).params[i];
        let name = leaf.field_name();
        let binding = 1 + bindings.len() as u32;
        // A render pipeline's binding is its own shader's (`Interface::bound`).
        let stage = match stage {
            Entry::Compute(_) => BindingStage::Both,
            _ if e == 0 => BindingStage::Vertex,
            _ => BindingStage::Fragment,
        };
        let Some((id, kinds)) = add_bound(cx, &mut mb, &leaf.class, &name, binding, param.span)
        else {
            continue;
        };
        for (k, (kind, format)) in kinds.into_iter().enumerate() {
            bindings.push(ResourceBinding { binding: binding + k as u32, kind, stage, format });
        }
        bind(&mut resources, leaf, id);
    }
    // A lifted build's literals: a buffer every pipeline binds, after the others (§22).
    let literals = crate::lift::len(cx);
    if literals > 0 {
        let binding = 1 + bindings.len() as u32;
        let ty = mb.m.types.f32();
        let kind = ir::ResourceKind::StorageRead;
        let id = mb.m.add_resource(ir::Resource { name: "literals".into(), binding, kind, ty });
        bindings.push(ResourceBinding {
            binding,
            kind: BindingKind::Read,
            stage: BindingStage::Both,
            format: None,
        });
        if let Some(g) = mb.gpu.as_mut() {
            g.literals = Some(id);
        }
    }
    // Workgroup memory: one array per `Shared` parameter.
    let mut workgroup_bytes = 0u32;
    for (e, entry) in iface.entries.iter().enumerate() {
        for (i, (_, class)) in entry.params.iter().enumerate() {
            let ParamClass::Workgroup { elem, len } = *class else { continue };
            let param = &program.func(entry.func).params[i];
            let Some(et) = cx.lower_ty(&mut mb, elem, param.span) else {
                cx.holds_nothing("workgroup memory", elem, param.span);
                continue;
            };
            if len == 0 {
                let d =
                    Diagnostic::new(codes::E0602, param.span, "workgroup memory of no elements");
                cx.err(d);
                continue;
            }
            let ty = mb.m.types.intern(ir::TypeDef::Array(et, len));
            workgroup_bytes += ir::layout::layout(&mb.m.types, ty).size;
            let kind = ir::ResourceKind::Workgroup;
            let res = ir::Resource { name: param.name.clone(), binding: u32::MAX, kind, ty };
            resources[e][i] = Some(Bound::One(mb.m.add_resource(res)));
        }
    }
    let max = wrela_abi::Limits::DEFAULT.max_workgroup_storage_size;
    if workgroup_bytes > max {
        let at = program.func(iface.entries[0].func).sig_span;
        cx.err(
            Diagnostic::new(
                codes::E0602,
                at,
                format!(
                    "{what} has {workgroup_bytes} bytes of workgroup memory; WebGPU's default limit is {max}"
                ),
            )
            .with_help("share fewer elements, or smaller ones"),
        );
    }
    let uniform = bind_uniform(cx, &mut mb, &iface);
    check_storage_buffers(cx, &iface, uniform.as_ref());
    check_stage_limits(cx, &iface);
    if let Entry::Compute(_) = stage {
        // num_workgroups, for `Slots` indexing anywhere in the module.
        let u = mb.m.types.u32();
        let ty = mb.m.types.intern(ir::TypeDef::Struct {
            name: "NumWorkgroups".into(),
            fields: vec![("x".into(), u), ("y".into(), u), ("z".into(), u)],
        });
        let nwg = mb.m.add_resource(ir::Resource {
            name: "num_workgroups".into(),
            binding: u32::MAX,
            kind: ir::ResourceKind::Private,
            ty,
        });
        if let Some(g) = mb.gpu.as_mut() {
            g.num_workgroups = Some(nwg);
        }
    }
    let keys = iface
        .entries
        .iter()
        .zip(resources)
        .map(|(e, resources)| InstanceKey::Fn {
            func: e.func,
            substs: e.substs.clone(),
            callables: Vec::new(),
            resources,
        })
        .collect();
    EntryModule { mb, keys, bindings, uniform }
}

/// What a resource binding is in the manifest: its kind, and a colour texture's format.
type Binds = Vec<(BindingKind, Option<wrela_abi::stream::TextureFormat>)>;

/// Adds the resource what's of `class` (a parameter, or a group's field) is bound to, at
/// `binding`, named `name`: its id, and the manifest's kind (and format) of each binding it
/// takes (an `Append` takes two: its items, then its count). `None` for a class that isn't
/// bound, or a buffer of what has no runtime value (reported).
fn add_bound(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    class: &ParamClass,
    name: &str,
    binding: u32,
    span: Span,
) -> Option<(ir::ResourceId, Binds)> {
    let (kind, ty, bk, format) = match *class {
        ParamClass::Append { elem } => {
            let Some(ty) = cx.lower_ty(mb, elem, span) else {
                cx.holds_nothing("buffer", elem, span);
                return None;
            };
            let kind = ir::ResourceKind::StorageReadWrite;
            let id = mb.m.add_resource(ir::Resource { name: name.to_string(), binding, kind, ty });
            let count = mb.m.types.intern(ir::TypeDef::Atomic(ir::Scalar::U32));
            let name = format!("{name}_count");
            mb.m.add_resource(ir::Resource { name, binding: binding + 1, kind, ty: count });
            return Some((
                id,
                vec![(BindingKind::ReadWrite, None), (BindingKind::ReadWrite, None)],
            ));
        }
        ParamClass::Buffer { elem, read_write, atomic } => {
            let Some(ty) = cx.lower_ty(mb, elem, span) else {
                cx.holds_nothing("buffer", elem, span);
                return None;
            };
            let ty = if atomic {
                match mb.m.types.get(ty) {
                    &ir::TypeDef::Scalar(s @ (ir::Scalar::U32 | ir::Scalar::I32)) => {
                        mb.m.types.intern(ir::TypeDef::Atomic(s))
                    }
                    _ => {
                        cx.err(Diagnostic::new(codes::E0602, span, "atomics are `u32`s or `i32`s"));
                        return None;
                    }
                }
            } else {
                ty
            };
            if read_write {
                (ir::ResourceKind::StorageReadWrite, ty, BindingKind::ReadWrite, None)
            } else {
                (ir::ResourceKind::StorageRead, ty, BindingKind::Read, None)
            }
        }
        ParamClass::Texture { storage: true, three, format, .. } => {
            let bk =
                if three { BindingKind::StorageTexture3d } else { BindingKind::StorageTexture };
            let kind = ir::ResourceKind::StorageTexture { three, format };
            (kind, mb.m.types.u32(), bk, Some(stream_format(format)))
        }
        ParamClass::Texture { depth, three, format, .. } => {
            let bk = match (depth, three) {
                (true, _) => BindingKind::DepthTexture,
                (false, true) => BindingKind::Texture3d,
                (false, false) => BindingKind::Texture,
            };
            let kind = ir::ResourceKind::Texture { depth, three, format };
            (kind, mb.m.types.u32(), bk, (!depth).then(|| stream_format(format)))
        }
        ParamClass::Sampler { comparison } => {
            let bk = if comparison { BindingKind::ComparisonSampler } else { BindingKind::Sampler };
            (ir::ResourceKind::Sampler { comparison }, mb.m.types.u32(), bk, None)
        }
        _ => return None,
    };
    let id = mb.m.add_resource(ir::Resource { name: name.to_string(), binding, kind, ty });
    Some((id, vec![(bk, format)]))
}

/// The uniform block resource (binding 0), from the interface's uniforms: those with a runtime
/// value, as CPU code packs them (`uniform_block`).
fn bind_uniform(cx: &mut Cx, mb: &mut ModuleBuilder, iface: &Interface) -> Option<UniformOut> {
    let checked = cx.checked;
    let program = &checked.program;
    let mut fields = Vec::new();
    let mut map = Vec::new();
    for u in &iface.uniforms {
        let span = program.func(iface.entries[u.entry].func).params[u.param].span;
        if !wrela_sema::traits::implements_builtin(program, u.ty, Lang::GpuData) {
            cx.err(wrela_sema::gpu::not_gpu_data(program, &u.name, u.ty, span));
            return None;
        }
        match cx.lower_ty(mb, u.ty, span) {
            Some(it) => {
                map.push(Some(fields.len() as u32));
                fields.push((u.field_name(), it));
            }
            None => map.push(None),
        }
    }
    // Over a domain, its size last.
    let domain = iface.over.then(|| {
        let v3 = mb.m.types.vector_of(ir::Scalar::U32, 3);
        fields.push((DOMAIN.to_string(), v3));
        fields.len() as u32 - 1
    });
    if let Some(g) = mb.gpu.as_mut() {
        g.uniform_fields = map;
        g.domain_field = domain;
    }
    if fields.is_empty() {
        return None;
    }
    let names: Vec<String> =
        iface.entries.iter().map(|e| ir::ident(&program.func(e.func).name)).collect();
    let ty =
        mb.m.types
            .intern(ir::TypeDef::Struct { name: format!("Uniforms_{}", names.join("_")), fields });
    let size = ir::layout::round_up(16, ir::layout::layout(&mb.m.types, ty).size);
    // A uniform binding has stricter layout rules, and is at most 64 KiB; otherwise the same
    // bytes are a read-only storage binding.
    let storage = !ir::layout::uniform_compatible(&mb.m.types, ty)
        || size > wrela_abi::manifest::MAX_UNIFORM_BUFFER_BINDING_SIZE;
    // Named `u`: a creature's shader reads it a thousand times or more, each written out in the
    // WGSL with its whole path.
    let id = mb.m.add_resource(ir::Resource {
        name: "u".into(),
        binding: 0,
        kind: ir::ResourceKind::Uniform { storage },
        ty,
    });
    if let Some(g) = mb.gpu.as_mut() {
        g.uniform = Some(id);
    }
    Some(UniformOut { binding: 0, size, storage })
}

/// E0602 when a shader of a pipeline binds more storage buffers than a WebGPU stage may: its
/// own buffers (each shader's are its own bindings, which the other doesn't see), and the
/// uniform block when that's in storage space and a lifted build's literals, which both see.
fn check_storage_buffers(cx: &mut Cx, iface: &Interface, uniform: Option<&UniformOut>) {
    let max = wrela_abi::manifest::MAX_STORAGE_BUFFERS_PER_STAGE;
    let in_storage = uniform.is_some_and(|u| u.storage);
    let shared = usize::from(in_storage) + usize::from(crate::lift::len(cx) > 0);
    for e in 0..iface.entries.len() {
        // An `Append` is two buffers.
        let buffers: Vec<&Leaf> = iface
            .bound
            .iter()
            .filter(|l| l.entry == e)
            .flat_map(|l| match l.class {
                ParamClass::Buffer { .. } => vec![l],
                ParamClass::Append { .. } => vec![l, l],
                _ => Vec::new(),
            })
            .collect();
        let n = buffers.len() + shared;
        if n <= max {
            continue;
        }
        let checked = cx.checked;
        let program = &checked.program;
        let func = program.func(iface.entries[e].func);
        // Where it goes over: the first buffer past the limit, else the uniform block's first
        // value.
        let (span, past) = match buffers.get(max) {
            Some(l) => (leaf_span(cx, iface, l), Some(l.name.clone())),
            None => {
                (iface.uniforms.first().map_or(func.sig_span, |u| leaf_span(cx, iface, u)), None)
            }
        };
        let what = stage_name(cx, iface, e);
        let mut d = Diagnostic::new(
            codes::E0602,
            span,
            format!("{what} binds {n} storage buffers, and a WebGPU stage can have at most {max}"),
        )
        .with_note(
            "each `[T]`, `GpuSpan<T>` and `Slots<T>` it takes is a storage buffer, in a group too",
        );
        if let Some(name) = past {
            d = d.with_note(format!("`{name}` is the first past the limit"));
        }
        if in_storage {
            d = d.with_note(
                "the uniform values take one more: their layout doesn't meet WGSL's uniform \
                 rules, or they're over 64 KiB",
            );
        }
        cx.err(d.with_help("pass fewer buffers: put data that's read together in one buffer"));
    }
}

/// E0602 when a shader of a pipeline binds more textures, samplers or storage textures than a
/// WebGPU stage may, groups' fields counted (§12): named by the first past the limit.
fn check_stage_limits(cx: &mut Cx, iface: &Interface) {
    use wrela_abi::manifest::{
        MAX_SAMPLED_TEXTURES_PER_STAGE, MAX_SAMPLERS_PER_STAGE, MAX_STORAGE_TEXTURES_PER_STAGE,
    };
    type Is = fn(&ParamClass) -> bool;
    let kinds: [(&str, usize, Is); 3] = [
        ("textures", MAX_SAMPLED_TEXTURES_PER_STAGE, |c| {
            matches!(c, ParamClass::Texture { storage: false, .. })
        }),
        ("samplers", MAX_SAMPLERS_PER_STAGE, |c| matches!(c, ParamClass::Sampler { .. })),
        ("storage textures (`Texels`)", MAX_STORAGE_TEXTURES_PER_STAGE, |c| {
            matches!(c, ParamClass::Texture { storage: true, .. })
        }),
    ];
    for e in 0..iface.entries.len() {
        for (what, max, is) in kinds {
            let of: Vec<&Leaf> =
                iface.bound.iter().filter(|l| l.entry == e && is(&l.class)).collect();
            if of.len() <= max {
                continue;
            }
            let past = of[max];
            let stage = stage_name(cx, iface, e);
            let d = Diagnostic::new(
                codes::E0602,
                leaf_span(cx, iface, past),
                format!(
                    "{stage} binds {} {what}, and a WebGPU stage can have at most {max}",
                    of.len()
                ),
            )
            .with_note(format!(
                "`{}` is the first past the limit, its groups' fields counted",
                past.name
            ));
            cx.err(d);
        }
    }
}

/// Where a leaf's parameter is declared.
fn leaf_span(cx: &Cx, iface: &Interface, l: &Leaf) -> Span {
    cx.checked.program.func(iface.entries[l.entry].func).params[l.param].span
}

/// Entry point `e` of a pipeline, as a diagnostic names it.
fn stage_name(cx: &Cx, iface: &Interface, e: usize) -> String {
    let func = cx.checked.program.func(iface.entries[e].func);
    match cx.entry_of(iface.entries[e].func) {
        Some(stage) => describe_entry(stage, &func.name),
        None => format!("`{}`", func.name),
    }
}

fn lower_compute(cx: &mut Cx, iface: Rc<Interface>) -> Option<PipelineOut> {
    let kernel = iface.entries[0].func;
    let Some(Entry::Compute(wg)) = cx.entry_of(kernel) else { return None };
    let checked = cx.checked;
    let name = &checked.program.func(kernel).name;
    let EntryModule { mut mb, keys, bindings, uniform } =
        entry_module(cx, iface.clone(), Entry::Compute(wg));
    let entry = entry_function(&mut mb, &keys[0], name, None, None);
    cx.drain(&mut mb);
    check_recursion(cx, &mb);
    check_workgroup(cx, &mb, entry, &entry_inputs(&iface.entries[0], true, iface.over));
    mb.m.entry_points.push(ir::EntryPoint {
        name: ir::ident(name),
        stage: ir::Stage::Compute { workgroup_size: wg },
        function: entry,
        inputs: entry_inputs(&iface.entries[0], true, iface.over),
    });
    verify(cx, &mut mb, name)?;
    let instances = mb.fn_instances();
    Some(PipelineOut {
        name: name.clone(),
        module: mb.m,
        kind: PipelineKind::Compute { workgroup_size: wg },
        uniform,
        bindings,
        key: PipelineKey::Compute { kernel: FnId(0), substs: Vec::new(), over: false },
        sites: Vec::new(),
        debug_flag: None,
        blend: false,
        writes_depth: false,
        uint: false,
        instances,
    })
}

fn lower_render(cx: &mut Cx, iface: Rc<Interface>) -> Option<PipelineOut> {
    let checked = cx.checked;
    let [vertex, fragment] = &iface.entries[..] else { return None };
    let vret = iface.vertex_ret?;
    let vdef = checked.program.func(vertex.func);
    let fdef = checked.program.func(fragment.func);
    let EntryModule { mut mb, keys, bindings, uniform } =
        entry_module(cx, iface.clone(), Entry::Vertex);
    let vret_ir = cx.lower_ty(&mut mb, vret, vdef.sig_span)?;
    let ventry = entry_function(&mut mb, &keys[0], &vdef.name, Some(vret_ir), None);
    cx.drain(&mut mb);
    if let Some(g) = mb.gpu.as_mut() {
        g.stage = Some(Entry::Fragment);
        g.what = describe_entry(Entry::Fragment, &fdef.name);
    }
    let fret_t = ret_type(cx, fragment.func, &fragment.substs);
    let blend = checked.program.lang_of_ty(fret_t) == Some(Lang::Over);
    let writes_depth = checked.program.lang_of_ty(fret_t) == Some(Lang::WithDepth);
    let uint = fret_t == checked.program.types.u32;
    let fret = cx.lower_ty(&mut mb, fret_t, fdef.sig_span);
    let takes_varyings =
        fragment.params.iter().any(|&(t, _)| t == vret) && !is_clip_position(cx, vret);
    let fentry = entry_function(
        &mut mb,
        &keys[1],
        &fdef.name,
        fret,
        if takes_varyings { Some(vret_ir) } else { None },
    );
    cx.drain(&mut mb);
    if blend {
        // An `Over` is its colour: the entry point returns the `vec4` WGSL blends.
        let v4 = mb.m.types.vector(4);
        let f = &mut mb.m.functions[fentry.index()];
        unwrap_returns(f, v4);
    }
    check_recursion(cx, &mb);
    check_uniformity(cx, &mb, fentry, &entry_inputs(fragment, false, false));
    let varyings = varying_layout(cx, &mut mb, vret, vdef.sig_span)?;
    mb.m.entry_points.push(ir::EntryPoint {
        name: ir::ident(&vdef.name),
        stage: ir::Stage::Vertex { varyings: varyings.clone() },
        function: ventry,
        inputs: entry_inputs(vertex, false, false),
    });
    mb.m.entry_points.push(ir::EntryPoint {
        name: ir::ident(&fdef.name),
        stage: ir::Stage::Fragment {
            varyings: takes_varyings.then_some((vret_ir, varyings)),
            depth: writes_depth,
            uint,
        },
        function: fentry,
        inputs: entry_inputs(fragment, false, false),
    });
    let name = format!("{}_{}", ir::ident(&vdef.name), ir::ident(&fdef.name));
    verify(cx, &mut mb, &name)?;
    let instances = mb.fn_instances();
    Some(PipelineOut {
        name: format!("{}+{}", vdef.name, fdef.name),
        module: mb.m,
        kind: PipelineKind::Render,
        uniform,
        bindings,
        key: PipelineKey::Compute { kernel: FnId(0), substs: Vec::new(), over: false },
        sites: Vec::new(),
        debug_flag: None,
        blend,
        writes_depth,
        uint,
        instances,
    })
}

/// Makes `f` return field 0 of what it returned (an `Over`'s colour), of type `v4`.
fn unwrap_returns(f: &mut ir::Function, v4: ir::TypeId) {
    fn walk(f: &mut ir::Function, block: ir::Block, v4: ir::TypeId) -> ir::Block {
        let mut out = Vec::with_capacity(block.len());
        for s in block {
            match s {
                ir::Stmt::Return(Some(v)) => {
                    let c = f.let_(&mut out, v4, ir::Expr::Extract(v, 0));
                    out.push(ir::Stmt::Return(Some(c)));
                }
                ir::Stmt::If { cond, then, else_ } => {
                    let then = walk(f, then, v4);
                    let else_ = walk(f, else_, v4);
                    out.push(ir::Stmt::If { cond, then, else_ });
                }
                ir::Stmt::Loop { body, continuing } => {
                    let body = walk(f, body, v4);
                    let continuing = walk(f, continuing, v4);
                    out.push(ir::Stmt::Loop { body, continuing });
                }
                other => out.push(other),
            }
        }
        out
    }
    let body = std::mem::take(&mut f.body);
    f.body = walk(f, body, v4);
    f.ret = Some(v4);
}

fn is_clip_position(cx: &Cx, t: TyId) -> bool {
    cx.checked.program.lang_of_ty(t) == Some(Lang::ClipPosition)
}

/// The members of the struct WGSL passes from the vertex shader to the fragment shader: the
/// clip position, and each value the vertex output's other fields hold (a matrix's columns, a
/// struct's or a tuple's fields, a `Flat<T>`'s value, not interpolated). E0602 for a field that
/// can't pass: one of a generic type, which the vertex shader's own check couldn't see.
fn varying_layout(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    vret: TyId,
    span: Span,
) -> Option<Vec<ir::Varying>> {
    let position = |path| ir::Varying { path, position: true, flat: false };
    if is_clip_position(cx, vret) {
        return Some(vec![position(vec![0])]);
    }
    let checked = cx.checked;
    let p = &checked.program;
    let TyKind::Adt(a, _) = p.types.kind(vret) else { return None };
    let decls = p.adt(*a).fields();
    let mut found = false;
    let mut out = Vec::new();
    for ((k, ft), decl) in cx.field_map(mb, vret, None, span).into_iter().zip(decls) {
        let Some(k) = k else { continue };
        if is_clip_position(cx, ft) {
            if found {
                // A generic field made a `ClipPosition` (the checker counts the declared ones).
                cx.err(
                    Diagnostic::new(
                        codes::E0602,
                        decl.span,
                        format!("`{}` is a second `ClipPosition` in the vertex output", decl.name),
                    )
                    .with_help("a vertex shader returns one `ClipPosition`, and the values to pass to the fragment shader"),
                );
                return None;
            }
            found = true;
            out.push(position(vec![k, 0]));
        } else if !wrela_sema::gpu::is_varying(p, ft) {
            cx.err(wrela_sema::gpu::not_varying(p, &decl.name, ft, decl.span));
            return None;
        } else {
            varying_leaves(cx, mb, ft, vec![k], false, span, &mut out);
        }
    }
    found.then_some(out)
}

/// The values a vertex output's part of type `t`, at `path`, passes on: see `varying_layout`.
fn varying_leaves(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    t: TyId,
    path: Vec<u32>,
    flat: bool,
    span: Span,
    out: &mut Vec<ir::Varying>,
) {
    let p = &cx.checked.program;
    let leaf = |path, flat| ir::Varying { path, position: false, flat };
    match p.types.kind(t) {
        // A column a location.
        TyKind::Mat(cols, _) => {
            for c in 0..u32::from(*cols) {
                out.push(leaf([path.clone(), vec![c]].concat(), flat));
            }
        }
        TyKind::Adt(..) | TyKind::Tuple(_) => {
            let flat = flat || p.lang_of_ty(t) == Some(Lang::Flat);
            for (k, ft) in cx.field_map(mb, t, None, span) {
                if let Some(k) = k {
                    varying_leaves(cx, mb, ft, [path.clone(), vec![k]].concat(), flat, span, out);
                }
            }
        }
        // An integer isn't interpolated (WGSL's rule).
        TyKind::Int(_) | TyKind::Vec(VecElem::I32 | VecElem::U32, _) => out.push(leaf(path, true)),
        _ => out.push(leaf(path, flat)),
    }
}

/// The builtins an entry point reads: its builtin parameters' inputs in order, each once (WGSL
/// takes each once; two parameters of one type both read it), then (compute) `num_workgroups`,
/// and over a domain (`over`) the invocation's id, if no parameter reads it.
fn entry_inputs(entry: &EntryParams, compute: bool, over: bool) -> Vec<ir::BuiltinInput> {
    let mut out: Vec<ir::BuiltinInput> = Vec::new();
    for (_, class) in &entry.params {
        if let ParamClass::Builtin(b) = class
            && !out.contains(b)
        {
            out.push(*b);
        }
    }
    if compute {
        out.push(ir::BuiltinInput::NumWorkgroups);
    }
    if over && !out.contains(&ir::BuiltinInput::GlobalInvocationId) {
        out.push(ir::BuiltinInput::GlobalInvocationId);
    }
    out
}

/// Declares an entry point's function (no parameters but the varyings) and queues its body.
fn entry_function(
    mb: &mut ModuleBuilder,
    key: &InstanceKey,
    name: &str,
    ret: Option<ir::TypeId>,
    varyings: Option<ir::TypeId>,
) -> ir::FuncId {
    let params = varyings
        .map(|t| vec![ir::Param { name: "varyings".into(), ty: t, by_ref: false, mutable: false }])
        .unwrap_or_default();
    let f = ir::Function::new(ir::ident(name), params, ret);
    let id = mb.m.add_function(f);
    mb.instances.insert(key.clone(), id);
    mb.source_names.insert(id, name.to_string());
    mb.queue.push_back((key.clone(), id));
    id
}

/// Binds an entry point's parameters, as the module's interface supplies them: builtins from
/// its inputs, uniforms and buffers from resources, the varyings from its parameter.
pub(crate) fn bind_entry_params(fl: &mut Fl, func: FnId, entry: Entry) {
    let Some(g) = fl.mb.gpu.as_ref() else { return };
    let Some(e) = g.entry_index(func) else { return };
    let (iface, uniform, fields) = (g.iface.clone(), g.uniform, g.uniform_fields.clone());
    let checked = fl.cx.checked;
    let params = &checked.program.func(func).params;
    let inputs = entry_inputs(&iface.entries[e], matches!(entry, Entry::Compute(_)), iface.over);
    let input = |b: ir::BuiltinInput| inputs.iter().position(|&x| x == b).unwrap_or(0) as u32;
    for (i, (t, class)) in iface.entries[e].params.iter().enumerate() {
        let local = fl.code.params[i];
        let p = &params[i];
        let at = GroupAt { iface: &iface, e, i, uniform, fields: &fields };
        fl.locals[local.index()] = match class {
            ParamClass::Builtin(b) => {
                let Some(it) = fl.cx.lower_ty(fl.mb, *t, p.span) else { continue };
                Repr::Var(fl.bind_local(&p.name, it, ir::Expr::EntryInput(input(*b))))
            }
            ParamClass::Buffer { .. }
            | ParamClass::Append { .. }
            | ParamClass::Workgroup { .. }
            | ParamClass::Texture { .. }
            | ParamClass::Sampler { .. } => {
                resource_repr(fl.key.resources().get(i).and_then(Option::as_ref))
            }
            ParamClass::Group => {
                let b = fl.key.resources().get(i).cloned().flatten();
                entry_group(fl, &at, &[], *t, b.as_ref())
            }
            ParamClass::Uniform => at.uniform_repr(&[]),
            ParamClass::Varyings if entry == Entry::Fragment => {
                let Some(it) = fl.cx.lower_ty(fl.mb, *t, p.span) else { continue };
                Repr::Var(fl.bind_local("varyings", it, ir::Expr::Param(0)))
            }
            // A vertex shader's parameter of its own output type: nothing supplies it.
            ParamClass::Varyings => Repr::Erased,
        };
    }
    // A compute entry fills in `num_workgroups` for `Slots` indexing.
    if let Entry::Compute(_) = entry
        && let Some(nwg) = fl.mb.gpu.as_ref().and_then(|g| g.num_workgroups)
    {
        let t = fl.mb.m.resources[nwg.index()].ty;
        let v = fl.value(t, ir::Expr::EntryInput(input(ir::BuiltinInput::NumWorkgroups)));
        fl.emit(ir::Stmt::Store(ir::Place::root(ir::PlaceRoot::Resource(nwg)), v));
    }
    // Over a domain: an invocation past it returns at once.
    if let Some(g) = fl.mb.gpu.as_ref()
        && let (Some(u), Some(field)) = (g.uniform, g.domain_field)
    {
        let types = &mut fl.mb.m.types;
        let (b, u32_ty) = (types.bool(), types.u32());
        let v3 = types.vector_of(ir::Scalar::U32, 3);
        let id = fl.value(v3, ir::Expr::EntryInput(input(ir::BuiltinInput::GlobalInvocationId)));
        let at = ir::Place { root: ir::PlaceRoot::Resource(u), path: vec![ir::Proj::Field(field)] };
        let size = fl.load(at, v3);
        let mut past = None;
        for k in 0..3 {
            let i = fl.value(u32_ty, ir::Expr::Extract(id, k));
            let n = fl.value(u32_ty, ir::Expr::Extract(size, k));
            let out = fl.value(b, ir::Expr::Binary(ir::BinOp::Ge, i, n));
            past = Some(match past {
                None => out,
                Some(p) => fl.value(b, ir::Expr::Binary(ir::BinOp::Or, p, out)),
            });
        }
        if let Some(cond) = past {
            fl.emit(ir::Stmt::If { cond, then: vec![ir::Stmt::Return(None)], else_: Vec::new() });
        }
    }
}

/// Where an entry point's group parameter's fields are: its interface, the parameter, and the
/// uniform block with its fields (`GpuCx::uniform_fields`).
struct GroupAt<'i> {
    iface: &'i Interface,
    e: usize,
    i: usize,
    uniform: Option<ir::ResourceId>,
    fields: &'i [Option<u32>],
}

/// A group an entry point takes (§12), at `path` in its parameter: each field bound to its
/// resource, its place in the uniform block, or a group of its own, as `b` binds it.
fn entry_group(fl: &mut Fl, at: &GroupAt, path: &[u32], ty: TyId, b: Option<&Bound>) -> Repr {
    let Some(Bound::Group(bs)) = b else { return Repr::Erased };
    let mut out = Vec::new();
    for (k, ft) in group_fields(fl.cx, ty).into_iter().enumerate() {
        let mut here = path.to_vec();
        here.push(k as u32);
        let inner = bs.get(k).and_then(Option::as_ref);
        out.push(match field_class(fl.cx, ft) {
            ParamClass::Group => entry_group(fl, at, &here, ft, inner),
            ParamClass::Uniform => at.uniform_repr(&here),
            _ => resource_repr(inner),
        });
    }
    Repr::Group(out)
}

impl GroupAt<'_> {
    /// The value at `path` in the parameter, in the uniform block: its field there, or
    /// `Erased` if it has no runtime value.
    fn uniform_repr(&self, path: &[u32]) -> Repr {
        let u = self.iface.uniforms.iter().position(|u| u.is(self.e, self.i, path));
        match (self.uniform, u.and_then(|u| self.fields.get(u).copied().flatten())) {
            (Some(u), Some(f)) => Repr::Place(ir::Place {
                root: ir::PlaceRoot::Resource(u),
                path: vec![ir::Proj::Field(f)],
            }),
            _ => Repr::Erased,
        }
    }
}

/// What a parameter (or a group's field) bound to one resource, `b`, is: the resource; or
/// `Erased` if it's bound to none.
fn resource_repr(b: Option<&Bound>) -> Repr {
    match b.and_then(Bound::one) {
        Some(r) => Repr::Place(ir::Place::root(ir::PlaceRoot::Resource(r))),
        None => Repr::Erased,
    }
}

/// Recursion on the GPU: WGSL has none (§8: GPU code forbids the `recursion` effect).
fn check_recursion(cx: &mut Cx, mb: &ModuleBuilder) {
    let n = mb.m.functions.len();
    let mut adj: Vec<Vec<(usize, Span)>> = vec![Vec::new(); n];
    for &(a, b, s) in &mb.edges {
        adj[a.index()].push((b.index(), s));
    }
    // 0 = unvisited, 1 = on the stack, 2 = done.
    let mut state = vec![0u8; n];
    fn dfs(
        v: usize,
        adj: &[Vec<(usize, Span)>],
        state: &mut [u8],
        found: &mut Option<(usize, Span)>,
    ) {
        state[v] = 1;
        for &(w, s) in &adj[v] {
            if found.is_some() {
                return;
            }
            if state[w] == 1 {
                *found = Some((w, s));
                return;
            }
            if state[w] == 0 {
                dfs(w, adj, state, found);
            }
        }
        state[v] = 2;
    }
    for v in 0..n {
        if state[v] == 0 {
            let mut found = None;
            dfs(v, &adj, &mut state, &mut found);
            if let Some((w, span)) = found {
                let name = mb.display_name(ir::FuncId(w as u32));
                let ctx = mb.gpu.as_ref().map(|g| g.what.as_str()).unwrap_or_default();
                cx.err(
                    Diagnostic::new(codes::E0600, span, format!("`{name}` calls itself, and {ctx} can't recurse"))
                        .with_note("GPU code forbids the `recursion` effect: WGSL has no call stack (language.md §8)")
                        .with_help("rewrite it as a loop"),
                );
                return;
            }
        }
    }
}

/// A debug build's bounds checks (`ir::bounds`), whose flag is pipeline number `code` (from
/// 1). A pipeline that already has WebGPU's default limit of storage buffers isn't checked.
pub(crate) fn debug_checks(cx: &mut Cx, out: &mut PipelineOut, code: u32) {
    let storage = out.bindings.iter().filter(|b| b.kind.is_buffer()).count()
        + usize::from(out.uniform.as_ref().is_some_and(|u| u.storage));
    if storage >= wrela_abi::manifest::MAX_STORAGE_BUFFERS_PER_STAGE {
        return;
    }
    let used =
        out.bindings.iter().map(|b| b.binding).chain(out.uniform.as_ref().map(|u| u.binding));
    let binding = used.max().map_or(0, |b| b + 1);
    if ir::bounds::add_checks(&mut out.module, binding, code).is_none() {
        return;
    }
    match ir::verify(&out.module) {
        Ok(()) => out.debug_flag = Some(binding),
        Err(e) => cx.err(Diagnostic::internal(format!(
            "the bounds checks for `{}` made malformed IR: {e}",
            out.name
        ))),
    }
}

/// Checks a pipeline's module and, if it'll be emitted, flattens it (`ir::opt::flatten_gpu`)
/// and lays out its memory as the CPU's (`ir::gpu_memory`).
fn verify(cx: &mut Cx, mb: &mut ModuleBuilder, name: &str) -> Option<()> {
    if wrela_diag::has_errors(&cx.diags) {
        return None;
    }
    let mut checked = ir::verify(&mb.m);
    // For the compiler's own debugging: `WRELA_DUMP_IR=gpu:<pipeline>` prints that pipeline's
    // module before it's flattened.
    if std::env::var("WRELA_DUMP_IR").is_ok_and(|d| d.strip_prefix("gpu:") == Some(name)) {
        eprintln!("{}", ir::print::print(&mb.m));
    }
    if cx.emit {
        checked = checked
            .and_then(|()| ir::opt::flatten_gpu(&mut mb.m))
            .and_then(|()| ir::gpu_memory::lay_out_gpu_memory(&mut mb.m))
            .and_then(|()| ir::verify(&mb.m));
    }
    if let Err(e) = checked {
        cx.err(Diagnostic::internal(format!("the IR for `{name}` is malformed: {e}")));
        return None;
    }
    Some(())
}

/// A GPU entry point or `@gpu` function that nothing dispatches: lowered for the GPU anyway
/// (a generic `@gpu` function with `substs`, stand-ins for its generics), so its effects are
/// checked where it's defined. Only to check: the module isn't emitted, so it isn't flattened.
/// What a `@gpu` function's parameter of type `t` is bound to for its check: a resource of its
/// own for a buffer, texture or sampler, and for each of a group's (`check_standalone`); `None`
/// for a value. `next` is the next binding.
#[allow(clippy::too_many_arguments)]
fn stand_in(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    t: TyId,
    mode: Mode,
    name: &str,
    span: Span,
    next: &mut u32,
) -> Option<Bound> {
    if wrela_sema::gpu::is_group(&cx.checked.program, t) {
        let names = group_field_names(cx, t);
        let fields = group_fields(cx, t);
        let mut bs = Vec::new();
        for (ft, n) in fields.into_iter().zip(names) {
            let field = format!("{name}_{n}");
            bs.push(stand_in(cx, mb, ft, Mode::Borrow, &field, span, next));
        }
        return Some(Bound::Group(bs));
    }
    if !is_resource_ty(&cx.checked.program, t) {
        return None;
    }
    let class = class_of(cx, t, mode, None);
    if let ParamClass::Workgroup { elem, len } = class {
        let et = cx.lower_ty(mb, elem, span)?;
        let ty = mb.m.types.intern(ir::TypeDef::Array(et, len.max(1)));
        let kind = ir::ResourceKind::Workgroup;
        let res = ir::Resource { name: name.to_string(), binding: u32::MAX, kind, ty };
        return Some(Bound::One(mb.m.add_resource(res)));
    }
    let (id, kinds) = add_bound(cx, mb, &class, name, *next, span)?;
    *next += kinds.len() as u32;
    Some(Bound::One(id))
}

pub(crate) fn check_standalone(cx: &mut Cx, f: FnId, entry: Option<Entry>, substs: &[TyId]) {
    let checked = cx.checked;
    let def = checked.program.func(f);
    let mut fragment = None;
    let mut mb = match entry {
        Some(stage) => {
            // A fragment shader may take a vertex output, whichever vertex shader it's drawn with.
            let varyings = def
                .params
                .iter()
                .map(|p| p.ty)
                .find(|&t| wrela_sema::gpu::is_vertex_output(&checked.program, t))
                .filter(|_| stage == Entry::Fragment);
            let iface = Rc::new(Interface::of(cx, &[(f, &[])], None, varyings));
            let EntryModule { mut mb, keys, .. } = entry_module(cx, iface.clone(), stage);
            let ret = match stage {
                Entry::Compute(_) => None,
                Entry::Vertex | Entry::Fragment => {
                    let rt = ret_type(cx, f, &[]);
                    cx.lower_ty(&mut mb, rt, def.sig_span)
                }
            };
            let varyings = varyings.and_then(|t| cx.lower_ty(&mut mb, t, def.sig_span));
            let id = entry_function(&mut mb, &keys[0], &def.name, ret, varyings);
            fragment = (stage == Entry::Fragment)
                .then(|| (id, entry_inputs(&iface.entries[0], false, false)));
            mb
        }
        None => {
            // `@gpu`: an ordinary function, lowered for the GPU, its buffers, textures,
            // samplers and groups bound to resources of their own, as a shader's would be.
            let what = format!("the `@gpu` function `{}`", def.name);
            let mut mb = ModuleBuilder::gpu(GpuCx::new(what, Rc::default(), None));
            let mut next = 1;
            let generics = checked.program.fn_all_generics(f);
            let subst = Subst::from_pairs(&generics, substs);
            let mut resources = Vec::new();
            for ps in &def.params {
                let t = cx.concrete(ps.ty, &subst);
                resources.push(stand_in(cx, &mut mb, t, ps.mode, &ps.name, ps.span, &mut next));
            }
            if resources.iter().all(Option::is_none) {
                resources.clear();
            }
            let key = InstanceKey::Fn {
                func: f,
                substs: substs.to_vec(),
                callables: Vec::new(),
                resources,
            };
            cx.instance(&mut mb, key, None);
            mb
        }
    };
    cx.drain(&mut mb);
    check_recursion(cx, &mb);
    if let Some((id, inputs)) = fragment {
        check_uniformity(cx, &mb, id, &inputs);
    }
}
