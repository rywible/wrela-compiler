//! GPU pipelines: what CPU code records (`dispatch`, `draw`), and each pipeline's module, lowered
//! from its entry points with its parameters bound to resources (D-102). Effects are checked
//! here, per instantiation (§8).

use crate::body::{Fl, Repr};
use crate::instance::InstanceKey;
use crate::{Cx, ModuleBuilder};
use std::rc::Rc;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_ir as ir;
use wrela_sema::defs::{Entry, Lang, Mode};
use wrela_sema::gpu::BuiltinInput;
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
    /// (binding, read-write) for each buffer, in the order commands list their handles.
    pub buffers: Vec<(u32, bool)>,
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
    /// A `[T]` (read) or `Slots<T>` (read-write) parameter, bound to a buffer of `T`.
    Buffer {
        elem: TyId,
        read_write: bool,
    },
    /// The vertex shader's output, read by the fragment shader.
    Varyings,
    Uniform,
}

/// How parameter `param` of `f`, of type `ty`, is supplied. `varyings` is the vertex output a
/// fragment shader may take.
fn classify(cx: &Cx, f: FnId, param: usize, ty: TyId, varyings: Option<TyId>) -> ParamClass {
    let p = &cx.checked.program;
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
    match p.types.kind(ty) {
        TyKind::Adt(_, args) if p.lang_of_ty(ty) == Some(Lang::Slots) => {
            ParamClass::Buffer { elem: args[0], read_write: true }
        }
        TyKind::Adt(..) if Some(ty) == varyings => ParamClass::Varyings,
        TyKind::Slice(e) => {
            let mode = p.func(f).params[param].mode;
            ParamClass::Buffer { elem: *e, read_write: mode == Mode::Mut }
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
    /// The buffer parameters in binding order, as (entry point, parameter) indices.
    pub buffers: Vec<(usize, usize)>,
    /// The uniform block's fields: each uniform parameter's name and type. Entry points share
    /// a uniform by name.
    pub uniforms: Vec<(String, TyId)>,
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
                    ParamClass::Buffer { .. } => iface.buffers.push((e, i)),
                    ParamClass::Uniform => {
                        let name = &cx.checked.program.func(func).params[i].name;
                        if !iface.uniforms.iter().any(|(n, _)| n == name) {
                            iface.uniforms.push((name.clone(), t));
                        }
                    }
                    ParamClass::Builtin(_) | ParamClass::Varyings => {}
                }
                params.push((t, class));
            }
            iface.entries.push(EntryParams { func, substs: substs.to_vec(), params });
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
        PipelineKey::Render { vertex, fragment } => {
            let vret = ret_type(cx, vertex.0, &vertex.1);
            let entries = [(vertex.0, vertex.1.as_slice()), (fragment.0, fragment.1.as_slice())];
            Interface::of(cx, &entries, Some(vret), Some(vret))
        }
    }
}

/// Whether a parameter type is bound to a resource on the GPU (rather than passed).
pub(crate) fn is_resource_param(fl: &Fl, t: TyId) -> bool {
    let p = &fl.cx.checked.program;
    matches!(p.types.kind(t), TyKind::Slice(_)) || p.lang_of_ty(t) == Some(Lang::Slots)
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
fn pipeline(cx: &mut Cx, key: PipelineKey) -> (u32, Rc<Interface>) {
    if let Some(i) = cx.pipelines.iter().position(|(k, _)| *k == key) {
        return (i as u32, cx.pipelines[i].1.clone());
    }
    let iface = Rc::new(interface(cx, &key));
    cx.pipelines.push((key, iface.clone()));
    (cx.pipelines.len() as u32 - 1, iface)
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
    let (pindex, iface) = pipeline(fl.cx, PipelineKey::Compute { kernel: d.kernel, substs });
    let mut groups = Vec::new();
    for g in &d.groups {
        match fl.operand(g) {
            Some(v) => groups.push(v),
            None => return,
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
            ParamClass::Builtin(_) | ParamClass::Varyings => {}
            ParamClass::Buffer { .. } => {
                let Some(v) = fl.read(arg) else { return };
                handles.push(handle(fl, v));
            }
            ParamClass::Uniform => {
                // A value with no runtime value isn't in the block (`bind_uniform`).
                let Some(t) = fl.cx.lower_ty(fl.mb, *pt, *arg_span) else { continue };
                let Some(v) = fl.read(arg) else { return };
                uniform_fields.push((params[i].name.clone(), t));
                uniform_vals.push(v);
            }
        }
    }
    let buffers = handles.len() as u32;
    let mut args = groups;
    args.extend(handles);
    let uniform = uniform_block(fl, &uniform_fields, &uniform_vals, &mut args, "dispatch", span);
    let op = ir::HostOp::Dispatch { pipeline: pindex, buffers, uniform };
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
    let (pindex, iface) = pipeline(fl.cx, PipelineKey::Render { vertex: vs, fragment: fs });
    let Some(vertices) = fl.operand(&d.vertices) else { return };
    let Some(instances) = fl.operand(&d.instances) else { return };
    let arg = |name: &str| d.args.iter().find(|(n, ..)| n == name);
    let mut fields = Vec::new();
    let mut vals = Vec::new();
    for (name, t) in &iface.uniforms {
        let Some((_, place, arg_span)) = arg(name) else { continue };
        // A value with no runtime value isn't in the block (`bind_uniform`).
        let Some(it) = fl.cx.lower_ty(fl.mb, *t, *arg_span) else { continue };
        let Some(v) = fl.read(place) else { return };
        fields.push((name.clone(), it));
        vals.push(v);
    }
    // Buffers, in the order the pipeline binds them, each by its parameter's name.
    let checked = fl.cx.checked;
    let mut handles = Vec::new();
    for &(e, i) in &iface.buffers {
        let name = &checked.program.func(iface.entries[e].func).params[i].name;
        let Some((_, place, _)) = arg(name) else { return };
        let Some(v) = fl.read(place) else { return };
        handles.push(handle(fl, v));
    }
    let buffers = handles.len() as u32;
    let mut args = vec![vertices, instances];
    args.extend(handles);
    let uniform = uniform_block(fl, &fields, &vals, &mut args, "draw", span);
    let op = ir::HostOp::Draw { pipeline: pindex, buffers, uniform };
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
        Some(l @ (Lang::Gradient | Lang::ValueAndGradient | Lang::IntervalOf)) => {
            return crate::derive::call(fl, l, substs, c, ty);
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
            let run = fl.run_arg(c.args[2].place()?, run_t)?;
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
    let ctx = fl.mb.gpu.as_ref().map_or("GPU code", |g| g.what.as_str());
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
        return Vec::new();
    }
    names.reverse();
    let chain: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
    vec![format!("called through {}", chain.join(" → "))]
}

/// E0607 for a derivative outside a fragment shader. In one, where it is (`v`), for E0608.
pub(crate) fn check_derivative(fl: &mut Fl, v: Option<ir::ValueId>, span: Span) {
    let ok = fl.is_gpu() && fl.mb.gpu.as_ref().is_some_and(|g| g.stage == Some(Entry::Fragment));
    if ok && let Some(v) = v {
        fl.mb.derivatives.insert((fl.id, v), span);
    }
    if !ok {
        let ctx = fl.mb.gpu.as_ref().map_or("CPU code", |g| g.what.as_str());
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
    for n in ir::uniformity::derivatives_in_non_uniform_flow(&mb.m, entry, inputs) {
        let at = mb.derivatives.get(&(n.func, n.value)).copied().or(n.at);
        let Some(span) = at else { continue };
        let name = match n.builtin {
            ir::Builtin::Dpdx => "dpdx",
            ir::Builtin::Dpdy => "dpdy",
            _ => "fwidth",
        };
        let mut d = Diagnostic::new(
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
        if !n.path.is_empty() {
            let chain: Vec<String> = n
                .path
                .iter()
                .chain([&n.func])
                .map(|&f| format!("`{}`", mb.display_name(f)))
                .collect();
            d = d.with_note(format!("called through {}", chain.join(" → ")));
        }
        cx.err(d.with_help(
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
) -> Option<PipelineOut> {
    match key {
        PipelineKey::Compute { .. } => lower_compute(cx, iface),
        PipelineKey::Render { .. } => lower_render(cx, iface),
    }
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
    /// Each entry point's instance key, with the buffer bound to each parameter.
    keys: Vec<InstanceKey>,
    /// (binding, read-write) for each buffer.
    buffers: Vec<(u32, bool)>,
    uniform: Option<UniformOut>,
}

/// A GPU module for the entry points of `iface`, the first of them a `stage`: its buffers
/// bound (bindings from 1, in the interface's order), its uniform block (binding 0) and, for a
/// kernel, `num_workgroups`.
fn entry_module(cx: &mut Cx, iface: Rc<Interface>, stage: Entry) -> EntryModule {
    let checked = cx.checked;
    let program = &checked.program;
    let what = describe_entry(stage, &program.func(iface.entries[0].func).name);
    let mut mb = ModuleBuilder::gpu(GpuCx::new(what, iface.clone(), Some(stage)));
    let mut resources: Vec<Vec<Option<ir::ResourceId>>> =
        iface.entries.iter().map(|e| vec![None; e.params.len()]).collect();
    let mut buffers = Vec::new();
    for &(e, i) in &iface.buffers {
        let entry = &iface.entries[e];
        let ParamClass::Buffer { elem, read_write } = entry.params[i].1 else { continue };
        let param = &program.func(entry.func).params[i];
        let Some(ty) = cx.lower_ty(&mut mb, elem, param.span) else { continue };
        let binding = 1 + buffers.len() as u32;
        let kind = if read_write {
            ir::ResourceKind::StorageReadWrite
        } else {
            ir::ResourceKind::StorageRead
        };
        let id = mb.m.add_resource(ir::Resource { name: param.name.clone(), binding, kind, ty });
        buffers.push((binding, read_write));
        resources[e][i] = Some(id);
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
    EntryModule { mb, keys, buffers, uniform }
}

/// The uniform block resource (binding 0), from the interface's uniforms: those with a runtime
/// value, as CPU code packs them (`uniform_block`).
fn bind_uniform(cx: &mut Cx, mb: &mut ModuleBuilder, iface: &Interface) -> Option<UniformOut> {
    let checked = cx.checked;
    let program = &checked.program;
    let mut fields = Vec::new();
    let mut map = Vec::new();
    for (name, t) in &iface.uniforms {
        // The parameter, in the first entry point that has it.
        let span = iface
            .entries
            .iter()
            .find_map(|e| program.func(e.func).params.iter().find(|p| p.name == *name))
            .map(|p| p.span)?;
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
    let n = iface.buffers.len() + usize::from(in_storage);
    if n <= max {
        return;
    }
    let checked = cx.checked;
    let program = &checked.program;
    let param = |(e, i): (usize, usize)| &program.func(iface.entries[e].func).params[i];
    // Where it goes over: the first buffer past the limit, else the uniform block's first value.
    let span = match iface.buffers.get(max) {
        Some(&at) => param(at).span,
        None => iface
            .entries
            .iter()
            .find_map(|e| {
                let f = program.func(e.func);
                f.params.iter().find(|p| p.name == iface.uniforms[0].0)
            })
            .map_or(program.func(iface.entries[0].func).sig_span, |p| p.span),
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
    let EntryModule { mut mb, keys, buffers, uniform } =
        entry_module(cx, iface.clone(), Entry::Compute(wg));
    let entry = entry_function(&mut mb, &keys[0], name, None, None);
    cx.drain(&mut mb);
    check_recursion(cx, &mb);
    mb.m.entry_points.push(ir::EntryPoint {
        name: ir::ident(name),
        stage: ir::Stage::Compute { workgroup_size: wg },
        function: entry,
        inputs: entry_inputs(&iface.entries[0], true),
    });
    verify(cx, &mut mb, name)?;
    Some(PipelineOut {
        name: name.clone(),
        module: mb.m,
        kind: PipelineKind::Compute { workgroup_size: wg },
        uniform,
        buffers,
    })
}

fn lower_render(cx: &mut Cx, iface: Rc<Interface>) -> Option<PipelineOut> {
    let checked = cx.checked;
    let [vertex, fragment] = &iface.entries[..] else { return None };
    let vret = iface.vertex_ret?;
    let vdef = checked.program.func(vertex.func);
    let fdef = checked.program.func(fragment.func);
    let EntryModule { mut mb, keys, buffers, uniform } =
        entry_module(cx, iface.clone(), Entry::Vertex);
    let vret_ir = cx.lower_ty(&mut mb, vret, vdef.sig_span)?;
    let ventry = entry_function(&mut mb, &keys[0], &vdef.name, Some(vret_ir), None);
    cx.drain(&mut mb);
    if let Some(g) = mb.gpu.as_mut() {
        g.stage = Some(Entry::Fragment);
        g.what = describe_entry(Entry::Fragment, &fdef.name);
    }
    let fret_t = ret_type(cx, fragment.func, &fragment.substs);
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
    Some(PipelineOut {
        name: format!("{}+{}", vdef.name, fdef.name),
        module: mb.m,
        kind: PipelineKind::Render,
        uniform,
        buffers,
    })
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
            ParamClass::Buffer { .. } => match fl.key.resources().get(i).copied().flatten() {
                Some(r) => Repr::Place(ir::Place::root(ir::PlaceRoot::Resource(r))),
                None => Repr::Erased,
            },
            ParamClass::Uniform => {
                let k = iface.uniforms.iter().position(|(n, _)| *n == p.name);
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
