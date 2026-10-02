//! GPU pipelines: what CPU code records (`dispatch`, `draw`), and each pipeline's module, lowered
//! from its entry points with its parameters bound to resources (D-102). Effects are checked
//! here, per instantiation (§8).

use crate::body::{Fl, Repr};
use crate::instance::{Callable, DeriveKind, InstanceKey};
use crate::{Cx, ModuleBuilder};
use wrela_diag::{Diagnostic, Span, codes};
use wrela_ir as ir;
use wrela_sema::defs::{Entry, Lang, Mode};
use wrela_sema::mir;
use wrela_sema::ty::*;

/// A pipeline as CPU code names it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PipelineKey {
    Compute { kernel: FnId, substs: Vec<TyId> },
    Render { vertex: (FnId, Vec<TyId>), fragment: (FnId, Vec<TyId>) },
}

impl PipelineKey {
    pub fn mentions(&self, f: FnId) -> bool {
        match self {
            PipelineKey::Compute { kernel, .. } => *kernel == f,
            PipelineKey::Render { vertex, fragment } => vertex.0 == f || fragment.0 == f,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PipelineKind {
    Compute { entry: String, workgroup_size: [u32; 3] },
    Render { vertex_entry: String, fragment_entry: String },
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
    /// (binding, read-write) for each buffer, in the order commands list their handles.
    pub buffers: Vec<(u32, bool)>,
}

/// What a GPU module is for, while it's lowered.
#[derive(Clone, Debug)]
pub struct GpuCx {
    pub what: String,
    pub entry_fns: Vec<FnId>,
    /// The stage being lowered now (a render pipeline lowers its vertex shader first).
    pub stage: Option<Entry>,
    pub workgroup_size: [u32; 3],
    /// The private variable holding `num_workgroups`, for `Slots` indexing.
    pub num_workgroups: Option<ir::ResourceId>,
    /// Each entry's uniform parameters: (function, param index) -> uniform field.
    pub uniform_fields: Vec<(FnId, usize, u32)>,
    pub uniform: Option<ir::ResourceId>,
    /// Each entry's buffer parameters: (function, param index) -> resource.
    pub buffer_params: Vec<(FnId, usize, ir::ResourceId)>,
}

impl GpuCx {
    pub fn describe(&self) -> String {
        self.what.clone()
    }
}

/// How an entry point's parameter is supplied.
#[derive(Clone, Debug, PartialEq)]
enum ParamClass {
    Builtin(ir::BuiltinInput),
    /// A `[T]` (read) or `Slots<T>` (read-write) parameter, bound to a buffer of `T`.
    Buffer {
        elem: TyId,
        read_write: bool,
    },
    /// The vertex shader's output, read by the fragment shader.
    Varyings,
    Uniform,
}

fn classify(cx: &Cx, f: FnId, param: usize, ty: TyId, vertex_ret: Option<TyId>) -> ParamClass {
    let p = &cx.checked.program;
    let mode = p.func(f).params[param].mode;
    match p.types.kind(ty) {
        TyKind::Adt(a, args) => {
            match p.adt(*a).lang {
                Some(Lang::GlobalId) => {
                    return ParamClass::Builtin(ir::BuiltinInput::GlobalInvocationId);
                }
                Some(Lang::LocalId) => {
                    return ParamClass::Builtin(ir::BuiltinInput::LocalInvocationId);
                }
                Some(Lang::WorkgroupId) => {
                    return ParamClass::Builtin(ir::BuiltinInput::WorkgroupId);
                }
                Some(Lang::VertexIndex) => {
                    return ParamClass::Builtin(ir::BuiltinInput::VertexIndex);
                }
                Some(Lang::InstanceIndex) => {
                    return ParamClass::Builtin(ir::BuiltinInput::InstanceIndex);
                }
                Some(Lang::FragCoord) => return ParamClass::Builtin(ir::BuiltinInput::Position),
                Some(Lang::Slots) => return ParamClass::Buffer { elem: args[0], read_write: true },
                _ => {}
            }
            if Some(ty) == vertex_ret {
                return ParamClass::Varyings;
            }
            let _ = mode;
            ParamClass::Uniform
        }
        TyKind::Slice(e) => ParamClass::Buffer { elem: *e, read_write: mode == Mode::Mut },
        _ => ParamClass::Uniform,
    }
}

/// Whether a parameter type is bound to a resource on the GPU (rather than passed).
pub(crate) fn is_resource_param(fl: &Fl, t: TyId) -> bool {
    match fl.cx.checked.program.types.kind(t) {
        TyKind::Slice(_) => true,
        TyKind::Adt(a, _) => fl.cx.checked.program.is_lang_adt(*a, Lang::Slots),
        _ => false,
    }
}

/// The concrete parameter types of an entry instance.
fn param_types(cx: &mut Cx, f: FnId, substs: &[TyId]) -> Vec<TyId> {
    let generics = cx.checked.program.fn_all_generics(f);
    let subst = Subst::from_pairs(&generics, substs);
    let tys: Vec<TyId> = cx.checked.program.func(f).params.iter().map(|p| p.ty).collect();
    tys.into_iter().map(|t| cx.concrete(t, &subst)).collect()
}

fn ret_type(cx: &mut Cx, f: FnId, substs: &[TyId]) -> TyId {
    let generics = cx.checked.program.fn_all_generics(f);
    let subst = Subst::from_pairs(&generics, substs);
    let r = cx.checked.program.func(f).ret;
    cx.concrete(r, &subst)
}

// ---- CPU side: recording ------------------------------------------------------------------------

fn pipeline_index(cx: &mut Cx, key: PipelineKey) -> u32 {
    match cx.pipelines.iter().position(|k| *k == key) {
        Some(i) => i as u32,
        None => {
            cx.pipelines.push(key);
            cx.pipelines.len() as u32 - 1
        }
    }
}

/// The handle inside a `GpuBuffer<T>` value.
fn handle(fl: &mut Fl, buf: ir::ValueId) -> ir::ValueId {
    let u = fl.mb.m.types.u32();
    fl.value(u, ir::Expr::Extract(buf, 0))
}

pub(crate) fn lower_dispatch(fl: &mut Fl, d: &mir::Dispatch, span: Span) {
    if fl.is_gpu() {
        host_effect(fl, "`dispatch`", span);
        return;
    }
    let substs: Vec<TyId> = d.kernel_args.iter().map(|&a| fl.concrete(a)).collect();
    let pindex =
        pipeline_index(fl.cx, PipelineKey::Compute { kernel: d.kernel, substs: substs.clone() });
    let ptys = param_types(fl.cx, d.kernel, &substs);
    let mut groups = Vec::new();
    for g in &d.groups {
        match fl.operand(g) {
            Some(v) => groups.push(v),
            None => return,
        }
    }
    let mut handles = Vec::new();
    let mut uniform_vals = Vec::new();
    let mut uniform_fields = Vec::new();
    for (i, &pt) in ptys.iter().enumerate() {
        let class = classify(fl.cx, d.kernel, i, pt, None);
        let Some((_, arg, arg_span)) = d.args.iter().find(|(j, ..)| *j == i) else { continue };
        match class {
            ParamClass::Builtin(_) | ParamClass::Varyings => {}
            ParamClass::Buffer { .. } => {
                let Some(v) = fl.read(arg) else { return };
                handles.push(handle(fl, v));
            }
            ParamClass::Uniform => {
                let Some(v) = fl.read(arg) else { continue };
                let Some(t) = fl.cx.lower_ty(fl.mb, pt, *arg_span) else { continue };
                let name = fl.cx.checked.program.func(d.kernel).params[i].name.clone();
                uniform_fields.push((name, t));
                uniform_vals.push(v);
            }
        }
    }
    let uniform = uniform_block(fl, &uniform_fields, &uniform_vals, "dispatch");
    let mut args = groups;
    let nbuf = handles.len() as u32;
    args.extend(handles);
    let uty = uniform.map(|(t, v)| {
        args.push(v);
        t
    });
    fl.emit(ir::Stmt::Eval(ir::Expr::Host(
        ir::HostOp::Dispatch { pipeline: pindex, buffers: nbuf, uniform: uty },
        args,
    )));
}

/// Packs uniform values into the block struct (same fields, same layout as the GPU side).
fn uniform_block(
    fl: &mut Fl,
    fields: &[(String, ir::TypeId)],
    vals: &[ir::ValueId],
    what: &str,
) -> Option<(ir::TypeId, ir::ValueId)> {
    if fields.is_empty() {
        return None;
    }
    let t = fl
        .mb
        .m
        .types
        .intern(ir::TypeDef::Struct { name: format!("Uniforms_{what}"), fields: fields.to_vec() });
    Some((t, fl.value(t, ir::Expr::Construct(t, vals.to_vec()))))
}

/// The uniform parameters of a render pipeline: the vertex shader's, then the fragment
/// shader's not already named (shared by name).
fn render_uniform_params(cx: &mut Cx, key: &PipelineKey) -> Vec<(String, TyId)> {
    let PipelineKey::Render { vertex, fragment } = key else { return Vec::new() };
    let vret = ret_type(cx, vertex.0, &vertex.1);
    let mut out: Vec<(String, TyId)> = Vec::new();
    for (f, substs) in [vertex, fragment] {
        let tys = param_types(cx, *f, substs);
        for (i, &t) in tys.iter().enumerate() {
            if classify(cx, *f, i, t, Some(vret)) != ParamClass::Uniform {
                continue;
            }
            let name = cx.checked.program.func(*f).params[i].name.clone();
            if !out.iter().any(|(n, _)| *n == name) {
                out.push((name, t));
            }
        }
    }
    out
}

pub(crate) fn lower_draw(fl: &mut Fl, d: &mir::Draw, span: Span) {
    if fl.is_gpu() {
        host_effect(fl, "`draw`", span);
        return;
    }
    let vs = (d.vertex.0, d.vertex.1.iter().map(|&a| fl.concrete(a)).collect::<Vec<_>>());
    let fs = (d.fragment.0, d.fragment.1.iter().map(|&a| fl.concrete(a)).collect::<Vec<_>>());
    let key = PipelineKey::Render { vertex: vs, fragment: fs };
    let pindex = pipeline_index(fl.cx, key.clone());
    let Some(vertices) = fl.operand(&d.vertices) else { return };
    let Some(instances) = fl.operand(&d.instances) else { return };
    let params = render_uniform_params(fl.cx, &key);
    let mut fields = Vec::new();
    let mut vals = Vec::new();
    for (name, t) in params {
        let Some((_, arg, arg_span)) = d.args.iter().find(|(n, ..)| *n == name) else { continue };
        let Some(v) = fl.read(arg) else { return };
        let Some(it) = fl.cx.lower_ty(fl.mb, t, *arg_span) else { continue };
        fields.push((name, it));
        vals.push(v);
    }
    // Buffers, in the order the pipeline binds them: the vertex shader's, then the fragment
    // shader's (`bind_buffers`), each by its parameter's name.
    let PipelineKey::Render { vertex, fragment } = &key else { return };
    let vret = ret_type(fl.cx, vertex.0, &vertex.1);
    let mut handles = Vec::new();
    for (f, substs) in [vertex, fragment] {
        let tys = param_types(fl.cx, *f, substs);
        for (i, &t) in tys.iter().enumerate() {
            if !matches!(classify(fl.cx, *f, i, t, Some(vret)), ParamClass::Buffer { .. }) {
                continue;
            }
            let name = fl.cx.checked.program.func(*f).params[i].name.clone();
            let Some((_, arg, _)) = d.args.iter().find(|(n, ..)| *n == name) else { return };
            let Some(v) = fl.read(arg) else { return };
            handles.push(handle(fl, v));
        }
    }
    let uniform = uniform_block(fl, &fields, &vals, "draw");
    let mut args = vec![vertices, instances];
    let nbuf = handles.len() as u32;
    args.extend(handles);
    let uty = uniform.map(|(t, v)| {
        args.push(v);
        t
    });
    fl.emit(ir::Stmt::Eval(ir::Expr::Host(
        ir::HostOp::Draw { pipeline: pindex, buffers: nbuf, uniform: uty },
        args,
    )));
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
        Some(Lang::Gradient | Lang::ValueAndGradient | Lang::IntervalOf) => {
            return derived_call(fl, lang, substs, c, ty);
        }
        Some(Lang::Buffer | Lang::Write | Lang::BeginScreenPass | Lang::Present) if fl.is_gpu() => {
            let name = fl.cx.checked.program.func(func).name.clone();
            host_effect(fl, &format!("`{name}`"), span);
            return None;
        }
        _ => {}
    }
    match lang {
        Some(Lang::Buffer) => {
            let elem = fl.cx.lower_ty(fl.mb, substs[0], span)?;
            let elem_size = ir::layout::array_stride(&fl.mb.m.types, elem);
            let count = fl.arg_value(&c.args[0])?;
            let u = fl.mb.m.types.u32();
            let h =
                fl.value(u, ir::Expr::Host(ir::HostOp::CreateBuffer { elem_size }, vec![count]));
            let bt = fl.ty(ty?, span)?;
            Some(fl.value(bt, ir::Expr::Construct(bt, vec![h])))
        }
        Some(Lang::Write) => {
            let elem = fl.cx.lower_ty(fl.mb, substs[0], span)?;
            let elem_size = ir::layout::array_stride(&fl.mb.m.types, elem);
            let buf = fl.arg_value(&c.args[0])?;
            let h = handle(fl, buf);
            let at = fl.arg_value(&c.args[1])?;
            let run_t = fl.mb.m.types.intern(ir::TypeDef::Run(elem));
            let values = c.args[2].place()?;
            let vt = mir::place_ty(&fl.cx.checked.program, &fl.mir.locals, values);
            let vt = fl.concrete(vt);
            let run = match fl.cx.checked.program.types.kind(vt).clone() {
                TyKind::Array(..) => {
                    let p = fl.place(values)?;
                    fl.value(run_t, ir::Expr::Run(p))
                }
                _ => fl.read(values)?,
            };
            fl.emit(ir::Stmt::Eval(ir::Expr::Host(
                ir::HostOp::WriteBuffer { elem_size },
                vec![h, at, run],
            )));
            None
        }
        Some(Lang::BeginScreenPass) => {
            let clear = fl.arg_value(&c.args[0])?;
            fl.emit(ir::Stmt::Eval(ir::Expr::Host(ir::HostOp::BeginScreenPass, vec![clear])));
            None
        }
        Some(Lang::Present) => {
            fl.emit(ir::Stmt::Eval(ir::Expr::Host(ir::HostOp::Present, Vec::new())));
            None
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
    let ctx = fl.mb.gpu.as_ref().map(|g| g.describe()).unwrap_or_else(|| "GPU code".into());
    let mut d = Diagnostic::new(
        codes::E0600,
        span,
        format!("{what} records GPU work, which {ctx} can't do"),
    )
    .with_note(
        "recording GPU work is the `host` effect; GPU entry points forbid it (language.md §8)",
    );
    for note in call_chain(fl) {
        d = d.with_note(note);
    }
    fl.cx.err(d);
}

/// The call chain from the entry point down to the current function, as one note
/// (`fill → helper → scratch`), when the current function isn't the entry point itself.
fn call_chain(fl: &Fl) -> Vec<String> {
    let mut names = vec![strip_suffix(&fl.f.name).to_string()];
    let mut at = fl.id;
    while let Some(&(caller, _)) = fl.mb.callers.get(&at) {
        let name = strip_suffix(&fl.mb.m.functions[caller.index()].name).to_string();
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
        return Vec::new();
    }
    names.reverse();
    let chain: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
    vec![format!("called through {}", chain.join(" → "))]
}

fn strip_suffix(name: &str) -> &str {
    match name.rfind('_') {
        Some(i) if name[i + 1..].bytes().all(|b| b.is_ascii_digit()) => &name[..i],
        _ => name,
    }
}

pub(crate) fn check_derivative(fl: &mut Fl, span: Span) {
    let ok = fl.is_gpu() && fl.mb.gpu.as_ref().is_some_and(|g| g.stage == Some(Entry::Fragment));
    if !ok {
        let ctx = match fl.mb.gpu.as_ref() {
            Some(g) => g.describe(),
            None => "CPU code".into(),
        };
        let mut d = Diagnostic::new(
            codes::E0607,
            span,
            format!("derivatives exist only in fragment shaders, and this is {ctx}"),
        )
        .with_note("`dpdx`, `dpdy` and `fwidth` compare neighbouring pixels");
        for note in call_chain(fl) {
            d = d.with_note(note);
        }
        fl.cx.err(d);
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
    let n = fl.load(ir::Place { root: ir::PlaceRoot::Resource(nwg), path: Vec::new() }, nt);
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

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// The IR struct the GPU builtin `b` arrives as (the std struct for it).
fn input_type(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    b: ir::BuiltinInput,
    std_ty: Option<TyId>,
    span: Span,
) -> Option<ir::TypeId> {
    match std_ty {
        Some(t) => cx.lower_ty(mb, t, span),
        None => {
            let u = mb.m.types.u32();
            Some(mb.m.types.intern(ir::TypeDef::Struct {
                name: format!("{b:?}"),
                fields: vec![("x".into(), u), ("y".into(), u), ("z".into(), u)],
            }))
        }
    }
}

pub(crate) fn lower_pipeline(cx: &mut Cx, index: u32, key: &PipelineKey) -> Option<PipelineOut> {
    match key {
        PipelineKey::Compute { kernel, substs } => lower_compute(cx, index, *kernel, substs),
        PipelineKey::Render { vertex, fragment } => lower_render(cx, index, vertex, fragment, key),
    }
}

/// Resources for an entry point's parameters: buffers (bindings from 1, in parameter order).
fn bind_buffers(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    f: FnId,
    substs: &[TyId],
    vret: Option<TyId>,
    buffers: &mut Vec<(u32, bool)>,
) -> Vec<Option<ir::ResourceId>> {
    let tys = param_types(cx, f, substs);
    let mut res = Vec::new();
    for (i, &t) in tys.iter().enumerate() {
        match classify(cx, f, i, t, vret) {
            ParamClass::Buffer { elem, read_write } => {
                let span = cx.checked.program.func(f).params[i].span;
                let Some(et) = cx.lower_ty(mb, elem, span) else {
                    res.push(None);
                    continue;
                };
                let binding = 1 + buffers.len() as u32;
                let name = cx.checked.program.func(f).params[i].name.clone();
                mb.m.resources.push(ir::Resource {
                    name,
                    binding,
                    kind: if read_write {
                        ir::ResourceKind::StorageReadWrite
                    } else {
                        ir::ResourceKind::StorageRead
                    },
                    ty: et,
                });
                let id = ir::ResourceId(mb.m.resources.len() as u32 - 1);
                buffers.push((binding, read_write));
                if let Some(g) = mb.gpu.as_mut() {
                    g.buffer_params.push((f, i, id));
                }
                res.push(Some(id));
            }
            _ => res.push(None),
        }
    }
    res
}

/// The uniform block resource (binding 0), from the uniform parameters.
fn bind_uniform(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    params: &[(String, TyId)],
    owners: &[(FnId, Vec<TyId>)],
    vret: Option<TyId>,
    what: &str,
) -> Option<UniformOut> {
    if params.is_empty() {
        return None;
    }
    let mut fields = Vec::new();
    for (name, t) in params {
        if !wrela_sema::traits::implements_builtin(&cx.checked.program, *t, Lang::GpuData) {
            let shown = cx.checked.program.display_ty(*t);
            let span = owners.first().map_or(Span::new(wrela_diag::FileId(0), 0, 0), |(f, _)| {
                cx.checked.program.func(*f).sig_span
            });
            cx.err(
                Diagnostic::new(codes::E0604, span, format!("the GPU parameter `{name}` is a `{shown}`, which isn't `GpuData`"))
                    .with_note("data crossing to the GPU must be `GpuData`, so its layout is the same on both sides (§6.13)")
                    .with_help("opt the type in: `struct Name: GpuData { ... }`"),
            );
            return None;
        }
        let it = cx.lower_ty(mb, *t, Span::new(wrela_diag::FileId(0), 0, 0))?;
        fields.push((name.clone(), it));
    }
    let ty = mb.m.types.intern(ir::TypeDef::Struct { name: format!("Uniforms_{what}"), fields });
    let storage = !ir::layout::uniform_compatible(&mb.m.types, ty);
    mb.m.resources.push(ir::Resource {
        name: "uniforms".into(),
        binding: 0,
        kind: ir::ResourceKind::Uniform { storage },
        ty,
    });
    let id = ir::ResourceId(mb.m.resources.len() as u32 - 1);
    let size = ir::layout::round_up(16, ir::layout::layout(&mb.m.types, ty).size);
    if let Some(g) = mb.gpu.as_mut() {
        g.uniform = Some(id);
        for (fi, (f, substs)) in owners.iter().enumerate() {
            let _ = fi;
            let tys = param_types(cx, *f, substs);
            for (i, &t) in tys.iter().enumerate() {
                if classify(cx, *f, i, t, vret) == ParamClass::Uniform {
                    let name = cx.checked.program.func(*f).params[i].name.clone();
                    if let Some(k) = params.iter().position(|(n, _)| *n == name) {
                        g.uniform_fields.push((*f, i, k as u32));
                    }
                }
            }
        }
    }
    Some(UniformOut { binding: 0, size, storage })
}

fn lower_compute(cx: &mut Cx, _index: u32, kernel: FnId, substs: &[TyId]) -> Option<PipelineOut> {
    let def = cx.checked.program.func(kernel).clone();
    let Some(Entry::Compute(wg)) = def.attrs.entry.map(|e| e.0) else { return None };
    let mut mb = ModuleBuilder::new(ir::Target::Gpu);
    mb.gpu = Some(GpuCx {
        what: format!("the `@compute` kernel `{}`", def.name),
        entry_fns: vec![kernel],
        stage: Some(Entry::Compute(wg)),
        workgroup_size: wg,
        num_workgroups: None,
        uniform_fields: Vec::new(),
        uniform: None,
        buffer_params: Vec::new(),
    });
    let mut buffers = Vec::new();
    let resources = bind_buffers(cx, &mut mb, kernel, substs, None, &mut buffers);
    let tys = param_types(cx, kernel, substs);
    let uparams: Vec<(String, TyId)> = tys
        .iter()
        .enumerate()
        .filter(|(i, t)| classify(cx, kernel, *i, **t, None) == ParamClass::Uniform)
        .map(|(i, t)| (def.params[i].name.clone(), *t))
        .collect();
    let uniform = bind_uniform(
        cx,
        &mut mb,
        &uparams,
        &[(kernel, substs.to_vec())],
        None,
        &sanitize(&def.name),
    );
    // num_workgroups, for `Slots` indexing anywhere in the module.
    let nwg_ty = input_type(cx, &mut mb, ir::BuiltinInput::NumWorkgroups, None, def.sig_span)?;
    mb.m.resources.push(ir::Resource {
        name: "num_workgroups".into(),
        binding: u32::MAX,
        kind: ir::ResourceKind::Private,
        ty: nwg_ty,
    });
    let nwg = ir::ResourceId(mb.m.resources.len() as u32 - 1);
    if let Some(g) = mb.gpu.as_mut() {
        g.num_workgroups = Some(nwg);
    }
    let key =
        InstanceKey::Fn { func: kernel, substs: substs.to_vec(), callables: Vec::new(), resources };
    let entry = entry_function(cx, &mut mb, &key, &sanitize(&def.name), None, None);
    cx.drain(&mut mb);
    check_recursion(cx, &mb);
    let inputs = entry_inputs(cx, &mut mb, kernel, substs, None, true);
    mb.m.entry_points.push(ir::EntryPoint {
        name: sanitize(&def.name),
        stage: ir::Stage::Compute { workgroup_size: wg },
        function: entry,
        inputs,
    });
    verify(cx, &mut mb, &def.name)?;
    Some(PipelineOut {
        name: def.name.clone(),
        module: mb.m,
        kind: PipelineKind::Compute { entry: sanitize(&def.name), workgroup_size: wg },
        uniform,
        buffers,
    })
}

fn lower_render(
    cx: &mut Cx,
    _index: u32,
    vertex: &(FnId, Vec<TyId>),
    fragment: &(FnId, Vec<TyId>),
    key: &PipelineKey,
) -> Option<PipelineOut> {
    let vdef = cx.checked.program.func(vertex.0).clone();
    let fdef = cx.checked.program.func(fragment.0).clone();
    let mut mb = ModuleBuilder::new(ir::Target::Gpu);
    mb.gpu = Some(GpuCx {
        what: format!("the vertex shader `{}`", vdef.name),
        entry_fns: vec![vertex.0, fragment.0],
        stage: Some(Entry::Vertex),
        workgroup_size: [1, 1, 1],
        num_workgroups: None,
        uniform_fields: Vec::new(),
        uniform: None,
        buffer_params: Vec::new(),
    });
    let vret = ret_type(cx, vertex.0, &vertex.1);
    let mut buffers = Vec::new();
    let vres = bind_buffers(cx, &mut mb, vertex.0, &vertex.1, Some(vret), &mut buffers);
    let fres = bind_buffers(cx, &mut mb, fragment.0, &fragment.1, Some(vret), &mut buffers);
    let uparams = render_uniform_params(cx, key);
    let name = format!("{}_{}", sanitize(&vdef.name), sanitize(&fdef.name));
    let uniform =
        bind_uniform(cx, &mut mb, &uparams, &[vertex.clone(), fragment.clone()], Some(vret), &name);
    let vkey = InstanceKey::Fn {
        func: vertex.0,
        substs: vertex.1.clone(),
        callables: Vec::new(),
        resources: vres,
    };
    let vret_ir = cx.lower_ty(&mut mb, vret, vdef.sig_span)?;
    let ventry = entry_function(cx, &mut mb, &vkey, &sanitize(&vdef.name), Some(vret_ir), None);
    cx.drain(&mut mb);
    if let Some(g) = mb.gpu.as_mut() {
        g.stage = Some(Entry::Fragment);
        g.what = format!("the fragment shader `{}`", fdef.name);
    }
    let fkey = InstanceKey::Fn {
        func: fragment.0,
        substs: fragment.1.clone(),
        callables: Vec::new(),
        resources: fres,
    };
    let fret_t = ret_type(cx, fragment.0, &fragment.1);
    let fret = cx.lower_ty(&mut mb, fret_t, fdef.sig_span);
    let ftys = param_types(cx, fragment.0, &fragment.1);
    let takes_varyings = ftys.contains(&vret) && !is_clip_position(cx, vret);
    let fentry = entry_function(
        cx,
        &mut mb,
        &fkey,
        &sanitize(&fdef.name),
        fret,
        if takes_varyings { Some(vret_ir) } else { None },
    );
    cx.drain(&mut mb);
    check_recursion(cx, &mb);
    let (position_field, flat) = varying_layout(cx, &mut mb, vret, vdef.sig_span)?;
    let vin = entry_inputs(cx, &mut mb, vertex.0, &vertex.1, Some(vret), false);
    let fin = entry_inputs(cx, &mut mb, fragment.0, &fragment.1, Some(vret), false);
    mb.m.entry_points.push(ir::EntryPoint {
        name: sanitize(&vdef.name),
        stage: ir::Stage::Vertex { position_field, flat: flat.clone() },
        function: ventry,
        inputs: vin,
    });
    mb.m.entry_points.push(ir::EntryPoint {
        name: sanitize(&fdef.name),
        stage: ir::Stage::Fragment {
            varyings: takes_varyings.then_some(vret_ir),
            position_field: takes_varyings.then_some(position_field),
            flat,
        },
        function: fentry,
        inputs: fin,
    });
    verify(cx, &mut mb, &name)?;
    Some(PipelineOut {
        name: format!("{}+{}", vdef.name, fdef.name),
        module: mb.m,
        kind: PipelineKind::Render {
            vertex_entry: sanitize(&vdef.name),
            fragment_entry: sanitize(&fdef.name),
        },
        uniform,
        buffers,
    })
}

fn is_clip_position(cx: &Cx, t: TyId) -> bool {
    matches!(cx.checked.program.types.kind(t), TyKind::Adt(a, _) if cx.checked.program.is_lang_adt(*a, Lang::ClipPosition))
}

/// Which field of the vertex output is the clip position, and which fields are `Flat`.
fn varying_layout(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    vret: TyId,
    span: Span,
) -> Option<(u32, Vec<bool>)> {
    if is_clip_position(cx, vret) {
        return Some((0, vec![false]));
    }
    let TyKind::Adt(a, args) = cx.checked.program.types.kind(vret).clone() else { return None };
    let fields = cx.checked.program.struct_fields(a, &args);
    let map = cx.field_map(mb, vret, None, span);
    let mut position = None;
    let mut flat = Vec::new();
    for (i, (_, ft)) in fields.iter().enumerate() {
        if map[i].is_none() {
            continue;
        }
        if is_clip_position(cx, *ft) {
            position = map[i];
        }
        let is_flat = matches!(cx.checked.program.types.kind(*ft), TyKind::Adt(b, _) if cx.checked.program.is_lang_adt(*b, Lang::Flat));
        flat.push(is_flat);
    }
    position.map(|p| (p, flat))
}

/// The builtins an entry point reads: its builtin parameters in order, then (compute)
/// `num_workgroups`.
fn entry_inputs(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    f: FnId,
    substs: &[TyId],
    vret: Option<TyId>,
    compute: bool,
) -> Vec<ir::BuiltinInput> {
    let tys = param_types(cx, f, substs);
    let mut out = Vec::new();
    for (i, &t) in tys.iter().enumerate() {
        if let ParamClass::Builtin(b) = classify(cx, f, i, t, vret) {
            out.push(b);
        }
    }
    if compute {
        out.push(ir::BuiltinInput::NumWorkgroups);
    }
    let _ = mb;
    out
}

/// Declares an entry point's function (no parameters but the varyings) and queues its body.
fn entry_function(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    key: &InstanceKey,
    name: &str,
    ret: Option<ir::TypeId>,
    varyings: Option<ir::TypeId>,
) -> ir::FuncId {
    let params = varyings
        .map(|t| vec![ir::Param { name: "varyings".into(), ty: t, by_ref: false, mutable: false }])
        .unwrap_or_default();
    let f = ir::Function::new(name, params, ret);
    let id = mb.m.add_function(f);
    mb.instances.insert(key.clone(), id);
    mb.queue.push_back((key.clone(), id));
    let _ = cx;
    id
}

/// Binds an entry point's parameters: builtins from its inputs, uniforms and buffers from
/// resources, the varyings from its parameter.
pub(crate) fn bind_entry_params(fl: &mut Fl, func: FnId, entry: Entry) {
    let substs = fl.key.substs().to_vec();
    let tys = param_types(fl.cx, func, &substs);
    let vret = match fl.mb.gpu.as_ref().map(|g| g.entry_fns.clone()) {
        Some(fs) if fs.len() == 2 => {
            Some(ret_type(fl.cx, fs[0], &fl.cx.instance_subst_args(fs[0])))
        }
        _ => None,
    };
    let vret = vret.filter(|_| entry == Entry::Fragment);
    let mut input = 0u32;
    for (i, &t) in tys.iter().enumerate() {
        let local = fl.code.params[i];
        let class = classify(fl.cx, func, i, t, vret);
        match class {
            ParamClass::Builtin(_) => {
                let span = fl.cx.checked.program.func(func).params[i].span;
                let Some(it) = fl.cx.lower_ty(fl.mb, t, span) else { continue };
                let v = fl.value(it, ir::Expr::EntryInput(input));
                input += 1;
                let l = fl.new_local(&fl.cx.checked.program.func(func).params[i].name.clone(), it);
                fl.emit(ir::Stmt::Store(ir::Place::local(l), v));
                fl.locals[local.index()] = Repr::Var(l);
            }
            ParamClass::Buffer { .. } => {
                let r = fl.mb.gpu.as_ref().and_then(|g| {
                    g.buffer_params.iter().find(|(f, p, _)| *f == func && *p == i).map(|x| x.2)
                });
                fl.locals[local.index()] = match r {
                    Some(r) => Repr::Place(ir::Place {
                        root: ir::PlaceRoot::Resource(r),
                        path: Vec::new(),
                    }),
                    None => Repr::Erased,
                };
            }
            ParamClass::Uniform => {
                let g = fl.mb.gpu.as_ref();
                let field = g.and_then(|g| {
                    g.uniform_fields.iter().find(|(f, p, _)| *f == func && *p == i).map(|x| x.2)
                });
                let u = g.and_then(|g| g.uniform);
                fl.locals[local.index()] = match (u, field) {
                    (Some(u), Some(k)) => Repr::Place(ir::Place {
                        root: ir::PlaceRoot::Resource(u),
                        path: vec![ir::Proj::Field(k)],
                    }),
                    _ => Repr::Erased,
                };
            }
            ParamClass::Varyings => {
                let span = fl.cx.checked.program.func(func).params[i].span;
                let Some(it) = fl.cx.lower_ty(fl.mb, t, span) else { continue };
                let v = fl.value(it, ir::Expr::Param(0));
                let l = fl.new_local("varyings", it);
                fl.emit(ir::Stmt::Store(ir::Place::local(l), v));
                fl.locals[local.index()] = Repr::Var(l);
            }
        }
    }
    // A compute entry fills in `num_workgroups` for `Slots` indexing.
    if let Entry::Compute(_) = entry
        && let Some(nwg) = fl.mb.gpu.as_ref().and_then(|g| g.num_workgroups)
    {
        let t = fl.mb.m.resources[nwg.index()].ty;
        let v = fl.value(t, ir::Expr::EntryInput(input));
        fl.emit(ir::Stmt::Store(
            ir::Place { root: ir::PlaceRoot::Resource(nwg), path: Vec::new() },
            v,
        ));
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
                let name = strip_suffix(&mb.m.functions[w].name).to_string();
                let ctx = mb.gpu.as_ref().map(|g| g.describe()).unwrap_or_default();
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

/// Checks a pipeline's module and flattens it (`ir::opt::flatten_gpu`).
fn verify(cx: &mut Cx, mb: &mut ModuleBuilder, name: &str) -> Option<()> {
    if cx.diags.iter().any(|d| d.is_error()) {
        return None;
    }
    if let Err(e) = ir::verify(&mb.m)
        .and_then(|()| ir::opt::flatten_gpu(&mut mb.m))
        .and_then(|()| ir::verify(&mb.m))
    {
        cx.err(Diagnostic::internal(format!("the IR for `{name}` is malformed: {e}")));
        return None;
    }
    Some(())
}

/// A GPU entry point or `@gpu` function that nothing dispatches: lowered for the GPU anyway, so
/// its effects are checked where it's defined.
pub(crate) fn check_standalone(cx: &mut Cx, f: FnId, entry: Option<Entry>) {
    match entry {
        Some(Entry::Compute(_)) => {
            let _ = lower_compute(cx, u32::MAX, f, &[]);
        }
        Some(stage) => {
            let def = cx.checked.program.func(f).clone();
            let mut mb = ModuleBuilder::new(ir::Target::Gpu);
            mb.gpu = Some(GpuCx {
                what: format!(
                    "the {} shader `{}`",
                    if stage == Entry::Vertex { "vertex" } else { "fragment" },
                    def.name
                ),
                entry_fns: vec![f],
                stage: Some(stage),
                workgroup_size: [1, 1, 1],
                num_workgroups: None,
                uniform_fields: Vec::new(),
                uniform: None,
                buffer_params: Vec::new(),
            });
            let mut buffers = Vec::new();
            let res = bind_buffers(cx, &mut mb, f, &[], None, &mut buffers);
            let tys = param_types(cx, f, &[]);
            let uparams: Vec<(String, TyId)> = tys
                .iter()
                .enumerate()
                .filter(|(i, t)| classify(cx, f, *i, **t, None) == ParamClass::Uniform)
                .map(|(i, t)| (def.params[i].name.clone(), *t))
                .collect();
            let _ = bind_uniform(cx, &mut mb, &uparams, &[(f, Vec::new())], None, "check");
            let key = InstanceKey::Fn {
                func: f,
                substs: Vec::new(),
                callables: Vec::new(),
                resources: res,
            };
            let rt = ret_type(cx, f, &[]);
            let ret = cx.lower_ty(&mut mb, rt, def.sig_span);
            entry_function(cx, &mut mb, &key, &sanitize(&def.name), ret, None);
            cx.drain(&mut mb);
            check_recursion(cx, &mb);
        }
        None => {
            // `@gpu`: an ordinary function, lowered for the GPU.
            let def = cx.checked.program.func(f).clone();
            let mut mb = ModuleBuilder::new(ir::Target::Gpu);
            mb.gpu = Some(GpuCx {
                what: format!("the `@gpu` function `{}`", def.name),
                entry_fns: Vec::new(),
                stage: None,
                workgroup_size: [1, 1, 1],
                num_workgroups: None,
                uniform_fields: Vec::new(),
                uniform: None,
                buffer_params: Vec::new(),
            });
            let key = InstanceKey::plain(f, Vec::new());
            cx.instance(&mut mb, key, None);
            cx.drain(&mut mb);
            check_recursion(cx, &mb);
        }
    }
}

impl<'a> Cx<'a> {
    /// The substitution arguments a pipeline's other entry point was instantiated with (render
    /// pipelines only; read back from the pipeline key being lowered).
    pub(crate) fn instance_subst_args(&self, f: FnId) -> Vec<TyId> {
        for k in &self.pipelines {
            if let PipelineKey::Render { vertex, fragment } = k {
                if vertex.0 == f {
                    return vertex.1.clone();
                }
                if fragment.0 == f {
                    return fragment.1.clone();
                }
            }
        }
        Vec::new()
    }
}

// ---- derived interpretations ------------------------------------------------------------------

fn derived_call(
    fl: &mut Fl,
    lang: Option<Lang>,
    substs: &[TyId],
    c: &mir::Call,
    ty: Option<TyId>,
) -> Option<ir::ValueId> {
    let kind = match lang {
        Some(Lang::Interval) | Some(Lang::IntervalOf) => DeriveKind::Interval,
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
    let mut args: Vec<ir::Arg> = Vec::new();
    for s in &srcs {
        args.push(fl.capture_arg(s)?);
    }
    let x = fl.arg_value(&c.args[1])?;
    args.push(ir::Arg::Value(x));
    let ret = fl.mb.m.functions[callee.index()].ret?;
    let v = fl.value(ret, ir::Expr::Call(callee, args));
    match lang {
        Some(Lang::Gradient) => {
            // value_and_gradient returns (f32, X); gradient wants the X.
            let t = fl.ty(ty?, c.span)?;
            Some(fl.value(t, ir::Expr::Extract(v, 1)))
        }
        _ => Some(v),
    }
}

/// A derived instance's signature: the callable's captures, then its input (a point, or a box),
/// returning `(f32, X)` or an `Interval`.
pub(crate) fn derived_signature(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    key: &InstanceKey,
) -> ir::Function {
    let InstanceKey::Derived { of, kind, input } = key else {
        return ir::Function::new("derived", Vec::new(), None);
    };
    let span = Span::new(wrela_diag::FileId(0), 0, 0);
    let mut params = cx.callable_params(mb, of, span);
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

/// Lowers a derived instance: the callable's own instance, then the IR transform.
pub(crate) fn lower_derived(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    key: &InstanceKey,
    id: ir::FuncId,
) {
    let InstanceKey::Derived { of, kind, .. } = key else { return };
    let inner_key = match of {
        Callable::Closure { owner, id } => InstanceKey::Closure { owner: owner.clone(), id: *id },
        Callable::Func { func, substs } => InstanceKey::plain(*func, substs.clone()),
    };
    let inner = cx.instance(mb, inner_key, Some((id, Span::new(wrela_diag::FileId(0), 0, 0))));
    // Everything the inner function calls must be lowered before it's transformed.
    cx.drain(mb);
    let ncap = mb.m.functions[id.index()].params.len() - 1;
    let target = mb.target;
    let result = match kind {
        DeriveKind::ValueAndGradient => {
            ir::derive::value_and_gradient(&mut mb.m, &mut mb.derived, inner, ncap as u32)
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
                let own = ir::Place { root: ir::PlaceRoot::Param(i as u32), path: Vec::new() };
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
            // Reported where the interpretation is asked for: the `gradient(..)` or
            // `interval(..)` call that made this instance.
            let Some(&(_, span)) = mb.callers.get(&id) else {
                cx.err(Diagnostic::internal(format!("a derived function with no caller: {e}")));
                return;
            };
            let code = match &e {
                ir::Error::NotDerivable(_) => codes::E0700,
                ir::Error::ActiveLoopExit(_) => codes::E0701,
                ir::Error::Internal(m) => {
                    cx.err(Diagnostic::internal(format!("deriving a function failed: {m}")));
                    return;
                }
            };
            cx.err(Diagnostic::new(code, span, format!("can't derive this function: {e}")));
        }
    }
}
