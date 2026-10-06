//! GPU pipelines: what CPU code records (`dispatch`, `draw`), and each pipeline's module, lowered
//! from its entry points with its parameters bound to resources (D-102). Effects are checked
//! here, per instantiation (§8).

use crate::body::{Fl, Repr};
use crate::instance::InstanceKey;
use crate::{Cx, ModuleBuilder};
use std::rc::Rc;
use wrela_abi::manifest::{BindingKind, ResourceBinding};
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
    /// A render pipeline's cull mode and depth bias.
    pub state: wrela_sema::thir::RenderState,
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
    /// A `Texture` or a `DepthTexture`, bound by its handle.
    Texture {
        depth: bool,
    },
    /// A `Sampler` or a `ComparisonSampler`, bound by its handle.
    Sampler {
        comparison: bool,
    },
    /// The vertex shader's output, read by the fragment shader.
    Varyings,
    Uniform,
}

impl ParamClass {
    /// Whether it's bound to a resource: a buffer, a texture or a sampler.
    pub fn is_bound(&self) -> bool {
        matches!(
            self,
            ParamClass::Buffer { .. }
                | ParamClass::Append { .. }
                | ParamClass::Texture { .. }
                | ParamClass::Sampler { .. }
        )
    }
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
    match p.lang_of_ty(ty) {
        Some(Lang::Texture) => return ParamClass::Texture { depth: false },
        Some(Lang::DepthTexture) => return ParamClass::Texture { depth: true },
        Some(Lang::Sampler) => return ParamClass::Sampler { comparison: false },
        Some(Lang::ComparisonSampler) => return ParamClass::Sampler { comparison: true },
        _ => {}
    }
    match p.types.kind(ty) {
        TyKind::Adt(_, args) if p.lang_of_ty(ty) == Some(Lang::Slots) => {
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
            let mode = p.func(f).params[param].mode;
            ParamClass::Buffer { elem: *e, read_write: mode == Mode::Mut, atomic: false }
        }
        _ => ParamClass::Uniform,
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

/// How a pipeline's entry points receive their parameters. CPU code that records the pipeline
/// and the pipeline's module both read it, so the buffer handles a command passes are the
/// buffers the module binds, in order, and the uniform values it packs are the module's
/// uniform block.
#[derive(Debug, Default)]
pub(crate) struct Interface {
    /// The entry points: a kernel, or a vertex shader then a fragment shader.
    pub entries: Vec<EntryParams>,
    /// The parameters bound to resources (buffers, textures, samplers) in binding order, as
    /// (entry point, parameter) indices. Each shader's parameters are its own (§12).
    pub bound: Vec<(usize, usize)>,
    /// The uniform block's fields: each uniform parameter, as (entry point, parameter), with its
    /// field's name (the parameter's, or the entry point's and the parameter's when both
    /// shaders have one of that name) and type.
    pub uniforms: Vec<(usize, usize, String, TyId)>,
    /// A render pipeline's vertex output: what the fragment shader's varyings are.
    pub vertex_ret: Option<TyId>,
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
                match class {
                    ParamClass::Buffer { .. }
                    | ParamClass::Append { .. }
                    | ParamClass::Texture { .. }
                    | ParamClass::Sampler { .. } => iface.bound.push((e, i)),
                    ParamClass::Uniform => {
                        let name = cx.checked.program.func(func).params[i].name.clone();
                        iface.uniforms.push((e, i, name, t));
                    }
                    ParamClass::Builtin(_)
                    | ParamClass::Varyings
                    | ParamClass::Workgroup { .. } => {}
                }
                params.push((t, class));
            }
            iface.entries.push(EntryParams { func, substs: substs.to_vec(), params });
        }
        // A field name both shaders have is told apart by the shader's.
        let program = &cx.checked.program;
        let names: Vec<String> = iface.uniforms.iter().map(|u| u.2.clone()).collect();
        for u in &mut iface.uniforms {
            if names.iter().filter(|n| **n == u.2).count() > 1 {
                u.2 = format!("{}_{}", ir::ident(&program.func(iface.entries[u.0].func).name), u.2);
            }
        }
        iface
    }
}

/// A pipeline's interface.
fn interface(cx: &mut Cx, key: &PipelineKey) -> Interface {
    match key {
        PipelineKey::Compute { kernel, substs } => {
            Interface::of(cx, &[(*kernel, substs.as_slice())], None, None)
        }
        PipelineKey::Render { vertex, fragment, .. } => {
            let vret = ret_type(cx, vertex.0, &vertex.1);
            let entries = [(vertex.0, vertex.1.as_slice()), (fragment.0, fragment.1.as_slice())];
            Interface::of(cx, &entries, Some(vret), Some(vret))
        }
    }
}

/// Whether a parameter type is bound to a resource on the GPU (rather than passed).
pub(crate) fn is_resource_param(fl: &Fl, t: TyId) -> bool {
    let p = &fl.cx.checked.program;
    matches!(p.types.kind(t), TyKind::Slice(_))
        || p.lang_of_ty(t).is_some_and(|l| l.is_invocation_safe() || l.is_texture_or_sampler())
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
    let u32_ty = fl.cx.checked.program.types.u32;
    let [handle, offset, _] = buffer_binding(fl, place, u32_ty, span)?;
    Some([handle, offset])
}

pub(crate) fn lower_dispatch(fl: &mut Fl, d: &mir::Dispatch, span: Span) {
    if fl.is_gpu() {
        host_effect(fl, "`dispatch`", span);
        return;
    }
    let substs: Vec<TyId> = d.kernel_args.iter().map(|&a| fl.concrete(a)).collect();
    let key = PipelineKey::Compute { kernel: d.kernel, substs };
    let (pindex, iface) = pipeline(fl.cx, key, span);
    let mut groups = Vec::new();
    match &d.indirect {
        Some((place, at)) => {
            let Some(b) = indirect_binding(fl, place, *at) else { return };
            groups.extend(b);
        }
        None => {
            for g in &d.groups {
                match fl.operand(g) {
                    Some(v) => groups.push(v),
                    None => return,
                }
            }
        }
    }
    let checked = fl.cx.checked;
    let params = &checked.program.func(d.kernel).params;
    let mut handles = Vec::new();
    let mut uniform_vals = Vec::new();
    let mut uniform_fields = Vec::new();
    for (i, (pt, class)) in iface.entries[0].params.iter().enumerate() {
        let Some((_, arg, arg_span)) = d.args.iter().find(|(j, ..)| *j == i) else { continue };
        match class {
            ParamClass::Builtin(_) | ParamClass::Varyings | ParamClass::Workgroup { .. } => {}
            ParamClass::Buffer { .. }
            | ParamClass::Append { .. }
            | ParamClass::Texture { .. }
            | ParamClass::Sampler { .. } => {
                let Some(b) = bindings_for(fl, *pt, class, arg, *arg_span) else { return };
                handles.extend(b);
            }
            ParamClass::Uniform => {
                // A value with no runtime value isn't in the block (`bind_uniform`).
                let Some(t) = fl.cx.lower_ty(fl.mb, *pt, *arg_span) else { continue };
                let Some(v) = fl.read(arg) else { return };
                let field =
                    iface.uniforms.iter().find(|u| u.1 == i).map_or(&params[i].name, |u| &u.2);
                uniform_fields.push((field.clone(), t));
                uniform_vals.push(v);
            }
        }
    }
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
    if fl.is_gpu() {
        host_effect(fl, "`draw`", span);
        return;
    }
    let vs = (d.vertex.0, d.vertex.1.iter().map(|&a| fl.concrete(a)).collect::<Vec<_>>());
    let fs = (d.fragment.0, d.fragment.1.iter().map(|&a| fl.concrete(a)).collect::<Vec<_>>());
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
        let Some(b) = buffer_binding(fl, place, elem, *at) else { return };
        counts.extend(b);
    }
    // The counts, or where their buffer holds them.
    match &d.indirect {
        Some((place, at)) => {
            let Some([handle, offset]) = indirect_binding(fl, place, *at) else { return };
            counts.extend([handle, offset]);
        }
        None => {
            let Some(vertices) = fl.operand(&d.vertices) else { return };
            let Some(instances) = fl.operand(&d.instances) else { return };
            counts.extend([vertices, instances]);
        }
    };
    let arg = |e: usize, i: usize| d.args.iter().find(|a| a.0 == e && a.1 == i);
    let mut fields = Vec::new();
    let mut vals = Vec::new();
    for (e, i, name, t) in &iface.uniforms {
        let Some((.., place, arg_span)) = arg(*e, *i) else { continue };
        // A value with no runtime value isn't in the block (`bind_uniform`).
        let Some(it) = fl.cx.lower_ty(fl.mb, *t, *arg_span) else { continue };
        let Some(v) = fl.read(place) else { return };
        fields.push((name.clone(), it));
        vals.push(v);
    }
    // Buffers, textures and samplers, in the order the pipeline binds them.
    let mut handles = Vec::new();
    for &(e, i) in &iface.bound {
        let Some((.., place, arg_span)) = arg(e, i) else { return };
        let (pt, class) = &iface.entries[e].params[i];
        let Some(b) = bindings_for(fl, *pt, class, place, *arg_span) else { return };
        handles.extend(b);
    }
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
            | Lang::DestroyTexture
            | Lang::CreateSampler
            | Lang::DestroySampler
            | Lang::BeginPass
            | Lang::EndPass),
        ) => {
            // (opcode, payload words, whether the first is a new handle)
            let (opcode, words, handle) = match l {
                Lang::CreateTexture => (Opcode::CreateTexture, 4, true),
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
            | Lang::InputTake),
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
            | Lang::PostCommand),
        ) => {
            // The request number and (a readback) the buffer's range, then the runs: a path or
            // URL, the bytes to store, a line to print.
            let (opcode, words) = match l {
                Lang::ReadBufferCommand => (Opcode::ReadBuffer, 4),
                Lang::StorageReadCommand => (Opcode::StorageRead, 1),
                Lang::StorageWriteCommand => (Opcode::StorageWrite, 1),
                Lang::PrintCommand => (Opcode::Log, 0),
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
            let u8_ = fl.mb.m.types.intern(ir::TypeDef::Scalar(ir::Scalar::U8));
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
            | Lang::AppendPush),
        ) => shared_op(fl, l, c, ty),
        Some(
            l @ (Lang::TextureSample
            | Lang::TextureSampleLevel
            | Lang::TextureSampleCompare
            | Lang::TextureSampleCompareLevel
            | Lang::TextureLoad
            | Lang::DepthLoad
            | Lang::TextureWidth
            | Lang::TextureHeight),
        ) => texture_read(fl, l, c, ty),
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
        Lang::TextureSampleLevel => T::SampleLevel,
        Lang::TextureSampleCompare => T::SampleCompare,
        Lang::TextureSampleCompareLevel => T::SampleCompareLevel,
        Lang::TextureLoad | Lang::DepthLoad => T::Load,
        Lang::TextureWidth => T::Width,
        _ => T::Height,
    };
    if !fl.is_gpu() {
        if matches!(op, T::Width | T::Height) {
            // `Texture` and `DepthTexture` hold their handle, width and height first.
            let v = fl.arg_value(&c.args[0])?;
            return Some(fl.value(u, ir::Expr::Extract(v, if op == T::Width { 1 } else { 2 })));
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
    let sampled = !matches!(op, T::Load | T::Width | T::Height);
    let sampler = if sampled { Some(resource_arg(fl, c, 1, msg)?) } else { None };
    let mut vals = Vec::new();
    for a in &c.args[1 + usize::from(sampled)..] {
        vals.push(fl.arg_value(a)?);
    }
    let t = fl.ty(ty?, span)?;
    let v = fl.value(t, ir::Expr::Texture(op, texture, sampler, vals));
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

/// An operation that compares neighbouring pixels, defining `v`, at `span`: in a fragment
/// shader, noted for E0608; elsewhere, the error `err` makes from what the code is.
fn fragment_only(
    fl: &mut Fl,
    v: Option<ir::ValueId>,
    span: Span,
    err: impl FnOnce(&str) -> Diagnostic,
) {
    // A `@gpu` function checked on its own has no stage: its callers' decide, and each
    // pipeline that calls it checks it there.
    if fl.mb.gpu.as_ref().is_some_and(|g| g.stage.is_none()) {
        return;
    }
    if fl.mb.gpu.as_ref().is_some_and(|g| g.stage == Some(Entry::Fragment)) {
        if let Some(v) = v {
            fl.mb.derivatives.insert((fl.id, v), span);
        }
        return;
    }
    let d = err(fl.mb.gpu.as_ref().map_or("CPU code", |g| g.what.as_str()));
    fl.cx.err(with_call_chain(fl, d));
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
    if let PipelineKey::Render { state, .. } = key {
        out.state = *state;
    }
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
    let mut resources: Vec<Vec<Option<ir::ResourceId>>> =
        iface.entries.iter().map(|e| vec![None; e.params.len()]).collect();
    let mut bindings = Vec::new();
    for &(e, i) in &iface.bound {
        let entry = &iface.entries[e];
        let param = &program.func(entry.func).params[i];
        let binding = 1 + bindings.len() as u32;
        let (kind, ty, bk) = match entry.params[i].1 {
            ParamClass::Append { elem } => {
                // The items, then the count: two bindings, the count's resource just after.
                let Some(ty) = cx.lower_ty(&mut mb, elem, param.span) else {
                    cx.holds_nothing("buffer", elem, param.span);
                    continue;
                };
                let kind = ir::ResourceKind::StorageReadWrite;
                let res = ir::Resource { name: param.name.clone(), binding, kind, ty };
                let id = mb.m.add_resource(res);
                let count = mb.m.types.intern(ir::TypeDef::Atomic(ir::Scalar::U32));
                let name = format!("{}_count", param.name);
                mb.m.add_resource(ir::Resource { name, binding: binding + 1, kind, ty: count });
                bindings.push(ResourceBinding { binding, kind: BindingKind::ReadWrite });
                bindings
                    .push(ResourceBinding { binding: binding + 1, kind: BindingKind::ReadWrite });
                resources[e][i] = Some(id);
                continue;
            }
            ParamClass::Buffer { elem, read_write, atomic } => {
                let Some(ty) = cx.lower_ty(&mut mb, elem, param.span) else {
                    cx.holds_nothing("buffer", elem, param.span);
                    continue;
                };
                let ty = if atomic {
                    match mb.m.types.get(ty) {
                        &ir::TypeDef::Scalar(s @ (ir::Scalar::U32 | ir::Scalar::I32)) => {
                            mb.m.types.intern(ir::TypeDef::Atomic(s))
                        }
                        _ => {
                            cx.err(Diagnostic::new(
                                codes::E0602,
                                param.span,
                                "atomics are `u32`s or `i32`s",
                            ));
                            continue;
                        }
                    }
                } else {
                    ty
                };
                if read_write {
                    (ir::ResourceKind::StorageReadWrite, ty, BindingKind::ReadWrite)
                } else {
                    (ir::ResourceKind::StorageRead, ty, BindingKind::Read)
                }
            }
            ParamClass::Texture { depth } => {
                let bk = if depth { BindingKind::DepthTexture } else { BindingKind::Texture };
                (ir::ResourceKind::Texture { depth }, mb.m.types.u32(), bk)
            }
            ParamClass::Sampler { comparison } => {
                let bk =
                    if comparison { BindingKind::ComparisonSampler } else { BindingKind::Sampler };
                (ir::ResourceKind::Sampler { comparison }, mb.m.types.u32(), bk)
            }
            _ => continue,
        };
        let id = mb.m.add_resource(ir::Resource { name: param.name.clone(), binding, kind, ty });
        bindings.push(ResourceBinding { binding, kind: bk });
        // Every entry point's parameter of this name is this binding.
        for (e2, other) in iface.entries.iter().enumerate() {
            for (i2, _) in other.params.iter().enumerate() {
                if program.func(other.func).params[i2].name == param.name
                    && other.params[i2].1.is_bound()
                {
                    resources[e2][i2] = Some(id);
                }
            }
        }
    }
    // A lifted build's literals: a buffer every pipeline binds, after the others (§22).
    let literals = crate::lift::len(cx);
    if literals > 0 {
        let binding = 1 + bindings.len() as u32;
        let ty = mb.m.types.f32();
        let kind = ir::ResourceKind::StorageRead;
        let id = mb.m.add_resource(ir::Resource { name: "literals".into(), binding, kind, ty });
        bindings.push(ResourceBinding { binding, kind: BindingKind::Read });
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
            resources[e][i] = Some(mb.m.add_resource(res));
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

/// The uniform block resource (binding 0), from the interface's uniforms: those with a runtime
/// value, as CPU code packs them (`uniform_block`).
fn bind_uniform(cx: &mut Cx, mb: &mut ModuleBuilder, iface: &Interface) -> Option<UniformOut> {
    let checked = cx.checked;
    let program = &checked.program;
    let mut fields = Vec::new();
    let mut map = Vec::new();
    for (e, i, name, t) in &iface.uniforms {
        let span = program.func(iface.entries[*e].func).params[*i].span;
        if !wrela_sema::traits::implements_builtin(program, *t, Lang::GpuData) {
            cx.err(wrela_sema::gpu::not_gpu_data(program, name, *t, span));
            return None;
        }
        match cx.lower_ty(mb, *t, span) {
            Some(it) => {
                map.push(Some(fields.len() as u32));
                fields.push((name.clone(), it));
            }
            None => map.push(None),
        }
    }
    if let Some(g) = mb.gpu.as_mut() {
        g.uniform_fields = map;
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
    let id = mb.m.add_resource(ir::Resource {
        name: "uniforms".into(),
        binding: 0,
        kind: ir::ResourceKind::Uniform { storage },
        ty,
    });
    if let Some(g) = mb.gpu.as_mut() {
        g.uniform = Some(id);
    }
    Some(UniformOut { binding: 0, size, storage })
}

/// E0602 when a pipeline binds more storage buffers than a WebGPU stage may: its buffers, and
/// its uniform block when that's in storage space.
fn check_storage_buffers(cx: &mut Cx, iface: &Interface, uniform: Option<&UniformOut>) {
    let max = wrela_abi::manifest::MAX_STORAGE_BUFFERS_PER_STAGE;
    let in_storage = uniform.is_some_and(|u| u.storage);
    // An `Append` is two buffers.
    let buffers: Vec<(usize, usize)> = iface
        .bound
        .iter()
        .copied()
        .flat_map(|(e, i)| match iface.entries[e].params[i].1 {
            ParamClass::Buffer { .. } => vec![(e, i)],
            ParamClass::Append { .. } => vec![(e, i), (e, i)],
            _ => Vec::new(),
        })
        .collect();
    let n = buffers.len() + usize::from(in_storage) + usize::from(crate::lift::len(cx) > 0);
    if n <= max {
        return;
    }
    let checked = cx.checked;
    let program = &checked.program;
    let param = |(e, i): (usize, usize)| &program.func(iface.entries[e].func).params[i];
    // Where it goes over: the first buffer past the limit, else the uniform block's first value.
    let span = match buffers.get(max) {
        Some(&at) => param(at).span,
        None => iface.uniforms.first().map_or(program.func(iface.entries[0].func).sig_span, |u| {
            program.func(iface.entries[u.0].func).params[u.1].span
        }),
    };
    let names: Vec<String> =
        iface.entries.iter().map(|e| format!("`{}`", program.func(e.func).name)).collect();
    let mut d = Diagnostic::new(
        codes::E0602,
        span,
        format!(
            "{} binds {n} storage buffers, and a WebGPU stage can have at most {max}",
            names.join(" with ")
        ),
    )
    .with_note("each `[T]` and `Slots<T>` parameter is a storage buffer");
    if in_storage {
        d = d.with_note(
            "the uniform values take one more: their layout doesn't meet WGSL's uniform rules, \
             or they're over 64 KiB",
        );
    }
    cx.err(d.with_help("pass fewer buffers: put data that's read together in one buffer"));
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
    check_workgroup(cx, &mb, entry, &entry_inputs(&iface.entries[0], true));
    mb.m.entry_points.push(ir::EntryPoint {
        name: ir::ident(name),
        stage: ir::Stage::Compute { workgroup_size: wg },
        function: entry,
        inputs: entry_inputs(&iface.entries[0], true),
    });
    verify(cx, &mut mb, name)?;
    let instances = mb.fn_instances();
    Some(PipelineOut {
        name: name.clone(),
        module: mb.m,
        kind: PipelineKind::Compute { workgroup_size: wg },
        uniform,
        bindings,
        key: PipelineKey::Compute { kernel: FnId(0), substs: Vec::new() },
        sites: Vec::new(),
        debug_flag: None,
        blend: false,
        state: Default::default(),
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
        let v4 = mb.m.types.intern(ir::TypeDef::Vector(4));
        let f = &mut mb.m.functions[fentry.index()];
        unwrap_returns(f, v4);
    }
    check_recursion(cx, &mb);
    check_uniformity(cx, &mb, fentry, &entry_inputs(fragment, false));
    let (position_field, flat) = varying_layout(cx, &mut mb, vret, vdef.sig_span)?;
    mb.m.entry_points.push(ir::EntryPoint {
        name: ir::ident(&vdef.name),
        stage: ir::Stage::Vertex { position_field, flat: flat.clone() },
        function: ventry,
        inputs: entry_inputs(vertex, false),
    });
    mb.m.entry_points.push(ir::EntryPoint {
        name: ir::ident(&fdef.name),
        stage: ir::Stage::Fragment {
            varyings: takes_varyings.then_some(vret_ir),
            position_field: takes_varyings.then_some(position_field),
            flat,
        },
        function: fentry,
        inputs: entry_inputs(fragment, false),
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
        key: PipelineKey::Compute { kernel: FnId(0), substs: Vec::new() },
        sites: Vec::new(),
        debug_flag: None,
        blend,
        state: Default::default(),
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

/// Which field of the vertex output is the clip position, and which fields are `Flat`. E0602
/// for a field that can't pass to the fragment shader: one of a generic type, which the
/// vertex shader's own check couldn't see.
fn varying_layout(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    vret: TyId,
    span: Span,
) -> Option<(u32, Vec<bool>)> {
    if is_clip_position(cx, vret) {
        return Some((0, vec![false]));
    }
    let checked = cx.checked;
    let p = &checked.program;
    let TyKind::Adt(a, _) = p.types.kind(vret) else { return None };
    let decls = p.adt(*a).fields();
    let mut position = None;
    let mut flat = Vec::new();
    for ((k, ft), decl) in cx.field_map(mb, vret, None, span).into_iter().zip(decls) {
        let Some(k) = k else { continue };
        if is_clip_position(cx, ft) {
            if position.is_some() {
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
            position = Some(k);
        } else if !wrela_sema::gpu::is_varying(p, ft) {
            cx.err(wrela_sema::gpu::not_varying(p, &decl.name, ft, decl.span));
            return None;
        }
        flat.push(p.lang_of_ty(ft) == Some(Lang::Flat));
    }
    position.map(|p| (p, flat))
}

/// The builtins an entry point reads: its builtin parameters' inputs in order, each once (WGSL
/// takes each once; two parameters of one type both read it), then (compute) `num_workgroups`.
fn entry_inputs(entry: &EntryParams, compute: bool) -> Vec<ir::BuiltinInput> {
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
    let inputs = entry_inputs(&iface.entries[e], matches!(entry, Entry::Compute(_)));
    let input = |b: ir::BuiltinInput| inputs.iter().position(|&x| x == b).unwrap_or(0) as u32;
    for (i, (t, class)) in iface.entries[e].params.iter().enumerate() {
        let local = fl.code.params[i];
        let p = &params[i];
        fl.locals[local.index()] = match class {
            ParamClass::Builtin(b) => {
                let Some(it) = fl.cx.lower_ty(fl.mb, *t, p.span) else { continue };
                Repr::Var(fl.bind_local(&p.name, it, ir::Expr::EntryInput(input(*b))))
            }
            ParamClass::Buffer { .. }
            | ParamClass::Append { .. }
            | ParamClass::Workgroup { .. }
            | ParamClass::Texture { .. }
            | ParamClass::Sampler { .. } => match fl.key.resources().get(i).copied().flatten() {
                Some(r) => Repr::Place(ir::Place::root(ir::PlaceRoot::Resource(r))),
                None => Repr::Erased,
            },
            ParamClass::Uniform => {
                let k = iface.uniforms.iter().position(|u| u.0 == e && u.1 == i);
                match (uniform, k.and_then(|k| fields.get(k).copied().flatten())) {
                    (Some(u), Some(field)) => Repr::Place(ir::Place {
                        root: ir::PlaceRoot::Resource(u),
                        path: vec![ir::Proj::Field(field)],
                    }),
                    // No runtime value.
                    _ => Repr::Erased,
                }
            }
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

/// Checks a pipeline's module and, if it'll be emitted, flattens it (`ir::opt::flatten_gpu`).
fn verify(cx: &mut Cx, mb: &mut ModuleBuilder, name: &str) -> Option<()> {
    if wrela_diag::has_errors(&cx.diags) {
        return None;
    }
    let mut checked = ir::verify(&mb.m);
    if cx.emit {
        checked =
            checked.and_then(|()| ir::opt::flatten_gpu(&mut mb.m)).and_then(|()| ir::verify(&mb.m));
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
            fragment =
                (stage == Entry::Fragment).then(|| (id, entry_inputs(&iface.entries[0], false)));
            mb
        }
        None => {
            // `@gpu`: an ordinary function, lowered for the GPU.
            let what = format!("the `@gpu` function `{}`", def.name);
            let mut mb = ModuleBuilder::gpu(GpuCx::new(what, Rc::default(), None));
            cx.instance(&mut mb, InstanceKey::plain(f, substs.to_vec()), None);
            mb
        }
    };
    cx.drain(&mut mb);
    check_recursion(cx, &mb);
    if let Some((id, inputs)) = fragment {
        check_uniformity(cx, &mb, id, &inputs);
    }
}
