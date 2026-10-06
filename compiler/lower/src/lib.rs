//! Lowering: checked wrela to IR, monomorphized (language.md §7: generics are always
//! monomorphized).
//!
//! - **Instances.** A function is lowered once per [`InstanceKey`]: its generic arguments, the
//!   closures or functions passed for its `fn(...)` parameters (each closure is its own
//!   instance, so closures are inlined away), and on the GPU the resources bound to its run and
//!   `Slots` parameters.
//! - **Targets.** The CPU program is one IR module; each GPU pipeline (a `dispatch`ed kernel, or
//!   a `draw`'s vertex and fragment shaders) is its own module, lowered from its entry points.
//! - **Effects per instantiation** (§8): GPU code can't record GPU work (`host`), recurse, or use
//!   CPU-only types; derivatives only exist in fragment shaders. These are found while lowering
//!   each GPU pipeline and reported with the call chain.

mod audio;
mod body;
mod consts;
mod derive;
mod eval;
mod glue;
mod gpu;
mod instance;
mod lift;
mod mem;
mod par;
mod ty;

pub use eval::{
    BuildData, ConstLowering, ConstModule, Embeds, Memory, TestModule, Value, Values, is_computed,
    lower_consts, lower_tests, read_value,
};
pub use gpu::{PipelineKey, PipelineKind, PipelineOut};
pub use instance::{Callable, InstanceKey};
pub use lift::{LiftTable, LiftedFile, LiftedLiteral, candidates};

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use wrela_diag::{Diagnostic, Span};
use wrela_ir as ir;
use wrela_sema::Checked;
use wrela_sema::builtins::BuiltinFn;
use wrela_sema::defs::{Entry, Lang, TraitRef};
use wrela_sema::program::Program;
use wrela_sema::ty::{FnId, TyId, TyKind};

/// The program, lowered: the CPU module and one module per GPU pipeline.
#[derive(Debug)]
pub struct Lowered {
    pub cpu: ir::Module,
    pub pipelines: Vec<PipelineOut>,
    /// The functions the CPU module instantiates, each with its type arguments (its owner's,
    /// then its own): for tools (`wrela query`). A pipeline's are its own.
    pub instances: Vec<(FnId, Vec<TyId>)>,
}

/// What to lower.
pub struct Roots {
    /// Functions exported from the CPU module: the entry module's `pub fn`s.
    pub exports: Vec<FnId>,
    /// Functions to check on the GPU even if nothing dispatches them, so their effects are
    /// checked where they're defined: non-generic entry points, and `@gpu` functions, each
    /// with the type arguments to check it with (a generic one's are [`stand_ins`]).
    pub gpu_checks: Vec<(FnId, Vec<TyId>)>,
    /// Functions lowered for the CPU though nothing exports them: for tests, so code that
    /// nothing calls still goes through lowering and the back ends.
    pub also: Vec<FnId>,
    /// Frame tests (§10), each exported as `test.<index>`, with the program's state when it
    /// takes it: a name no wrela function has.
    pub tests: Vec<FnId>,
}

impl Roots {
    /// The default roots: the entry module's public functions; every non-generic entry point
    /// and every `@gpu` function in the package.
    pub fn of(checked: &Checked) -> Roots {
        let p = &checked.program;
        let mut exports = Vec::new();
        let mut gpu_checks = Vec::new();
        for (i, f) in p.fns.iter().enumerate() {
            let id = FnId(i as u32);
            if p.is_std(f.module) || !checked.mir.contains_key(&id) {
                continue;
            }
            let generic = !p.fn_all_generics(id).is_empty();
            // A generic one too: it's reported (the host can't choose its types).
            if Some(f.module) == p.main
                && f.public
                && f.owner == wrela_sema::defs::FnOwner::Free
                && f.attrs.entry.is_none()
            {
                exports.push(id);
            }
            if f.attrs.entry.is_some() && !generic {
                gpu_checks.push((id, Vec::new()));
            } else if f.attrs.gpu.is_some()
                && let Some(substs) = stand_ins(p, id)
            {
                gpu_checks.push((id, substs));
            }
        }
        Roots { exports, gpu_checks, also: Vec::new(), tests: Vec::new() }
    }

    /// [`Roots::of`], and every other function of the package that can be lowered on its own:
    /// one that isn't generic, isn't GPU code, and takes no closure or function (lowered for
    /// the one each call passes).
    pub fn every_fn(checked: &Checked) -> Roots {
        let p = &checked.program;
        let mut roots = Roots::of(checked);
        for (i, f) in p.fns.iter().enumerate() {
            let id = FnId(i as u32);
            if !p.is_std(f.module)
                && checked.mir.contains_key(&id)
                && !matches!(f.owner, wrela_sema::defs::FnOwner::Const(_))
                && p.fn_all_generics(id).is_empty()
                && f.attrs.entry.is_none()
                && f.attrs.gpu.is_none()
                && !f.attrs.intrinsic
                && !f.params.iter().any(|ps| wrela_sema::mir::is_callable(&p.types, ps.ty))
                && !roots.exports.contains(&id)
            {
                roots.also.push(id);
            }
        }
        roots
    }
}

/// Type arguments to check a `@gpu` function with: for each generic parameter, a type GPU code
/// can hold that has the parameter's bounds. `f32` has every structural trait (`Copy`, `Clone`,
/// `GpuData`); a parameter bounded by a declared trait (a trait method's `Self`, say) takes
/// the first type the program implements it for. `None` when some parameter has none: then no
/// GPU code can call the function either.
fn stand_ins(p: &Program, f: FnId) -> Option<Vec<TyId>> {
    let mut out = Vec::new();
    for g in p.fn_all_generics(f) {
        let bounds = wrela_sema::resolve::param_bounds_closure(p, g);
        if bounds.iter().any(|b| b.args.iter().any(|&a| p.types.has_params(a))) {
            return None;
        }
        let structural = |b: &&TraitRef| p.trait_(b.trait_).lang.is_some_and(Lang::is_structural);
        let candidates: Vec<TyId> = match bounds.iter().find(|b| !structural(b)) {
            None => vec![p.types.f32],
            Some(b) => p
                .impls_of(b.trait_)
                .iter()
                .map(|&i| p.impl_(i))
                .filter(|imp| imp.generics.is_empty())
                .map(|imp| imp.self_ty)
                .collect(),
        };
        let fits = |t: TyId| {
            on_gpu(p, t) && bounds.iter().all(|b| wrela_sema::traits::implements(p, t, b))
        };
        out.push(candidates.into_iter().find(|&t| fits(t))?);
    }
    Some(out)
}

/// Whether GPU code can hold a value of type `t`: no CPU-only scalars, runs or functions.
fn on_gpu(p: &Program, t: TyId) -> bool {
    on_gpu_in(p, t, &mut HashMap::new())
}

/// [`on_gpu`], each part a type shares decided once (`known`).
fn on_gpu_in(p: &Program, t: TyId, known: &mut HashMap<TyId, bool>) -> bool {
    if let Some(&k) = known.get(&t) {
        return k;
    }
    let ok = match p.types.kind(t) {
        TyKind::Bool | TyKind::Vec(_) | TyKind::Mat(_) => true,
        TyKind::Int(i) => i.on_gpu(),
        TyKind::Float(f) => *f == wrela_sema::ty::FloatTy::F32,
        TyKind::Array(e, n) => *n > 0 && on_gpu_in(p, *e, known),
        TyKind::Tuple(ts) => ts.iter().all(|&e| on_gpu_in(p, e, known)),
        TyKind::Adt(a, args) => {
            // A struct's fields, or each variant's; an enum without variants has no values.
            let variants = p.field_lists(*a);
            !variants.is_empty()
                && variants
                    .into_iter()
                    .all(|v| p.fields_of(*a, args, v).into_iter().all(|ft| on_gpu_in(p, ft, known)))
        }
        _ => false,
    };
    known.insert(t, ok);
    ok
}

/// What a debug build's NaN check panics with (§11).
pub(crate) const NAN_MESSAGE: &str = "debug build: a float operation created a NaN";

/// Shared state while lowering every module of one program.
pub(crate) struct Cx<'a> {
    pub checked: &'a Checked,
    /// The values of the constants the build has computed, and the files `embed` reads.
    pub data: &'a BuildData,
    /// Whether a failure is explained: a failed `assert` shows its operands' values. In a
    /// debug build, and in the modules that compute constants and run tests (§10).
    pub explain: bool,
    /// Computed constants whose values were needed but aren't in `values` yet.
    pub missing: std::collections::BTreeSet<wrela_sema::ty::ConstId>,
    /// Whether the modules will be emitted; only then are GPU modules flattened.
    pub emit: bool,
    pub diags: Vec<Diagnostic>,
    /// Pipelines discovered by CPU code, deduplicated, in discovery order, with their
    /// interfaces.
    pub pipelines: Vec<(gpu::PipelineKey, Rc<gpu::Interface>)>,
    /// Where CPU code dispatches or draws each of `pipelines`.
    pub pipeline_sites: Vec<Vec<Span>>,
    /// A lifted build's literals (§22): only the program's own modules read them, never the
    /// modules that compute constants or run tests.
    pub lift: Option<&'a LiftTable>,
}

/// One target module being built: the CPU module by default.
#[derive(Default)]
pub(crate) struct ModuleBuilder {
    pub m: ir::Module,
    pub instances: HashMap<InstanceKey, ir::FuncId>,
    pub queue: VecDeque<(InstanceKey, ir::FuncId)>,
    pub type_cache: HashMap<TyId, Option<ir::TypeId>>,
    /// A GPU module: what it's for, to check effects with context.
    pub gpu: Option<gpu::GpuCx>,
    /// The first caller of each instance, for call chains in diagnostics.
    pub callers: HashMap<ir::FuncId, (ir::FuncId, Span)>,
    /// Each instance's source name (`Sphere::distance`, `main`'s closure), for diagnostics.
    pub source_names: HashMap<ir::FuncId, String>,
    /// How many instantiations each instance is from a root (an export or entry point),
    /// counting only those that specialize a function ([`InstanceKey::specializes`]).
    pub depth: HashMap<ir::FuncId, u32>,
    /// Whether the instantiation limit has been reported in this module.
    pub too_deep: bool,
    /// Every call between instances, for finding recursion on the GPU.
    pub edges: Vec<(ir::FuncId, ir::FuncId, Span)>,
    /// Derived functions built in this module.
    pub derived: ir::derive::DeriveCache,
    /// Where each derivative (`dpdx`, `dpdy`, `fwidth`) is in the source, by the value it
    /// defines, for E0608.
    pub derivatives: HashMap<(ir::FuncId, ir::ValueId), Span>,
    /// The constant data holding each table (CPU only).
    pub data: HashMap<wrela_sema::ty::ConstId, Option<ir::DataId>>,
    /// Derived instances waiting for what they derive from to be lowered.
    pub deriving: Vec<derive::Pending>,
    /// The constant data holding each text (CPU only).
    pub texts: HashMap<String, ir::DataId>,
    /// A lifted build's table of literals, and what keeps its GPU copy (CPU only).
    pub lift: Option<lift::CpuTable>,
}

impl ModuleBuilder {
    /// A GPU module, for `g`.
    pub fn gpu(g: gpu::GpuCx) -> ModuleBuilder {
        ModuleBuilder { gpu: Some(g), ..ModuleBuilder::default() }
    }

    pub fn target(&self) -> ir::Target {
        if self.gpu.is_some() { ir::Target::Gpu } else { ir::Target::Cpu }
    }

    /// The functions this module instantiates, each with its type arguments, in order.
    pub fn fn_instances(&self) -> Vec<(FnId, Vec<TyId>)> {
        let mut out: Vec<(FnId, Vec<TyId>)> = self
            .instances
            .keys()
            .filter_map(|k| match k {
                InstanceKey::Fn { func, substs, .. } => Some((*func, substs.clone())),
                _ => None,
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// How diagnostics name an instance: its source name, else its IR name.
    pub fn display_name(&self, id: ir::FuncId) -> String {
        match self.source_names.get(&id) {
            Some(name) => name.clone(),
            None => self.m.functions[id.index()].name.clone(),
        }
    }
}

/// Lowers a checked program (with no errors) to IR. With `emit`, its modules are ready for
/// the back ends; without, lowering only checks (the GPU modules aren't flattened, which only
/// the WGSL back end needs and which finds nothing wrong with a program).
pub fn lower(
    checked: &Checked,
    roots: &Roots,
    data: &BuildData,
    emit: bool,
) -> (Lowered, Vec<Diagnostic>) {
    let mut cx = Cx::new(checked, data, emit);
    cx.lift = data.lift.as_ref().filter(|t| !t.literals.is_empty());
    let mut cpu = ModuleBuilder::default();
    cx.make_table(&mut cpu);
    let mut has_frame = false;
    for &f in &roots.also {
        cx.instance(&mut cpu, InstanceKey::plain(f, Vec::new()), None);
    }
    // The program's state (§12): what `init` returns, which the other exports take first.
    let state = state_type(checked);
    if let Some((init, s)) = state {
        let span = checked.program.func(init).sig_span;
        if let Some(t) = cx.lower_ty(&mut cpu, s, span) {
            let value = eval::zero(&cpu.m.types, t);
            cpu.m.state = Some(cpu.m.add_data(ir::Data { name: "state".into(), ty: t, value }));
        }
    }
    for &f in &roots.exports {
        if !checked.program.fn_all_generics(f).is_empty() {
            // Reported; it has no types to be lowered with.
            cx.check_export(f, state.map(|s| s.1));
            continue;
        }
        let key = InstanceKey::plain(f, Vec::new());
        let id = cx.instance(&mut cpu, key, None);
        let name = cx.checked.program.func(f).name.clone();
        has_frame |= name == wrela_abi::EXPORT_FRAME;
        if cx.check_export(f, state.map(|s| s.1)) {
            let id = match (cpu.m.state, state) {
                (Some(d), Some((init, _))) if f == init => state_wrapper(&mut cpu, id, d, true),
                (Some(d), Some((_, s)))
                    if checked.program.func(f).params.first().is_some_and(|p| p.ty == s) =>
                {
                    state_wrapper(&mut cpu, id, d, false)
                }
                _ => id,
            };
            cpu.m.exports.push((name, id));
        }
    }
    for (i, &f) in roots.tests.iter().enumerate() {
        let id = cx.instance(&mut cpu, InstanceKey::plain(f, Vec::new()), None);
        let id = match (cpu.m.state, checked.program.func(f).params.is_empty()) {
            (Some(d), false) => state_wrapper(&mut cpu, id, d, false),
            _ => id,
        };
        cpu.m.exports.push((format!("test.{i}"), id));
    }
    if !has_frame {
        cx.no_frame();
    }
    cx.drain(&mut cpu);
    rewrite_cpu_math(&mut cx, &mut cpu);
    cx.drain(&mut cpu);
    mark_counted(&mut cx, &mut cpu);
    if data.debug {
        // Checked where floats are computed, by the back end.
        cpu.m.nan_message = Some(cx.text_data(&mut cpu, NAN_MESSAGE));
    }
    if !wrela_diag::has_errors(&cx.diags)
        && let Err(e) = ir::opt::inline_cpu(&mut cpu.m)
    {
        cx.err(Diagnostic::internal(format!("inlining the CPU module: {e}")));
    }
    // For the compiler's own debugging: `WRELA_DUMP_IR=cpu` prints the CPU module; set to
    // anything else, it's printed when it's malformed.
    let dump = std::env::var("WRELA_DUMP_IR").ok();
    if dump.as_deref() == Some("cpu") {
        eprintln!("{}", ir::print::print(&cpu.m));
    }
    if !wrela_diag::has_errors(&cx.diags)
        && let Err(e) = ir::verify(&cpu.m)
    {
        if dump.is_some_and(|d| d != "cpu") {
            eprintln!("{}", ir::print::print(&cpu.m));
        }
        cx.err(Diagnostic::internal(format!("the CPU module's IR is malformed: {e}")));
    }
    // Pipelines: those CPU code records, then the standalone GPU checks.
    let mut pipelines = Vec::new();
    let mut i = 0;
    while i < cx.pipelines.len() {
        let (key, iface) = cx.pipelines[i].clone();
        let sites = cx.pipeline_sites[i].clone();
        if let Some(mut out) = gpu::lower_pipeline(&mut cx, &key, iface, sites) {
            if data.debug && cx.emit {
                gpu::debug_checks(&mut cx, &mut out, pipelines.len() as u32 + 1);
            }
            pipelines.push(out);
        }
        i += 1;
    }
    for (f, substs) in &roots.gpu_checks {
        let already = cx.pipelines.iter().any(|(k, _)| k.mentions(*f));
        if already {
            continue;
        }
        let entry = cx.entry_of(*f);
        let before = cx.diags.len();
        gpu::check_standalone(&mut cx, *f, entry, substs);
        if !substs.is_empty() {
            // With stand-in types, only what the function's own code does is its error.
            let own = checked.program.func(*f).span;
            let found = cx.diags.split_off(before);
            let inside = |s: Span| s.file == own.file && s.start >= own.start && s.end <= own.end;
            cx.diags.extend(found.into_iter().filter(|d| d.span().is_some_and(inside)));
        }
    }
    let instances = cpu.fn_instances();
    let lowered = Lowered { cpu: cpu.m, pipelines, instances };
    let mut diags = cx.diags;
    wrela_diag::sort_and_dedup(&mut diags);
    (lowered, diags)
}

impl<'a> Cx<'a> {
    pub fn new(checked: &'a Checked, data: &'a BuildData, emit: bool) -> Cx<'a> {
        Cx {
            checked,
            data,
            explain: data.debug,
            missing: Default::default(),
            emit,
            diags: Vec::new(),
            pipelines: Vec::new(),
            pipeline_sites: Vec::new(),
            lift: None,
        }
    }

    pub fn err(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    /// The function for an instance, declaring it (and queueing its body) the first time.
    pub fn instance(
        &mut self,
        mb: &mut ModuleBuilder,
        key: InstanceKey,
        caller: Option<(ir::FuncId, Span)>,
    ) -> ir::FuncId {
        if let Some(&id) = mb.instances.get(&key) {
            if let Some((from, span)) = caller {
                mb.edges.push((from, id, span));
            }
            return id;
        }
        // Only an instance that specializes its function is a level deeper than its caller: a
        // chain of plain calls can't go on without end.
        let step = u32::from(key.specializes());
        let depth = caller.map_or(0, |(from, _)| mb.depth.get(&from).copied().unwrap_or(0) + step);
        let types = &self.checked.program.types;
        let huge = key.substs().iter().any(|&t| types.size(t) > wrela_sema::ty::MAX_TYPE_SIZE);
        if huge {
            // Its signature isn't made either: the types are too large to lower.
            let id = mb.m.add_function(ir::Function::new("too_large", Vec::new(), None));
            mb.m.functions[id.index()].body = vec![ir::Stmt::Trap];
            mb.instances.insert(key.clone(), id);
            if !mb.too_deep {
                mb.too_deep = true;
                self.report_too_deep(&key, caller.map(|c| c.1), true);
            }
            return id;
        }
        let f = self.signature(mb, &key);
        let id = mb.m.add_function(f);
        mb.instances.insert(key.clone(), id);
        if let Some(name) = self.source_name(&key) {
            mb.source_names.insert(id, name);
        }
        mb.depth.insert(id, depth);
        if let Some(c) = caller {
            mb.callers.insert(id, c);
            mb.edges.push((c.0, id, c.1));
        }
        if depth > MAX_INSTANCE_DEPTH {
            // Polymorphic recursion: each instance calls one with a bigger type. Its body is
            // never lowered, so the queue ends; the build stops on the error.
            if !mb.too_deep {
                mb.too_deep = true;
                self.report_too_deep(&key, caller.map(|c| c.1), false);
            }
            mb.m.functions[id.index()].body = vec![ir::Stmt::Trap];
            return id;
        }
        mb.queue.push_back((key, id));
        id
    }

    /// E0412, after [`MAX_INSTANCE_DEPTH`] levels or, if `huge`, at type arguments past
    /// `MAX_TYPE_SIZE`.
    fn report_too_deep(&mut self, key: &InstanceKey, at: Option<Span>, huge: bool) {
        let Some(func) = key.source_fn() else { return };
        let name = self.checked.program.fn_display_name(func);
        let args: Vec<String> =
            key.substs().iter().map(|&t| self.checked.program.display_ty(t)).collect();
        let span = at.unwrap_or(self.checked.program.func(func).sig_span);
        let when = if huge {
            format!(
                "its type arguments grew past {} parts: <{}>",
                wrela_sema::ty::MAX_TYPE_SIZE,
                args.join(", ")
            )
        } else {
            format!(
                "after {MAX_INSTANCE_DEPTH} levels it's instantiated with <{}>",
                args.join(", ")
            )
        };
        self.err(
            Diagnostic::new(
                wrela_diag::codes::E0412,
                span,
                format!("instantiating `{name}` never ends: each instance calls one with a larger type"),
            )
            .with_note(when)
            .with_note("generics are monomorphized (§7), so a generic function can't call itself with an ever-growing type"),
        );
    }

    /// Lowers queued instance bodies until none are left. Derived instances are built when
    /// nothing else is, one at a time ([`derive::build_next`]).
    pub fn drain(&mut self, mb: &mut ModuleBuilder) {
        loop {
            while let Some((key, id)) = mb.queue.pop_front() {
                body::lower_body(self, mb, &key, id);
            }
            if mb.deriving.is_empty() {
                return;
            }
            derive::build_next(self, mb);
        }
    }

    /// Whether an exported function's signature can cross the WASM boundary: scalars and
    /// vectors in, and a scalar, a vector or nothing out. `frame` has the one signature the
    /// host calls, and no export takes the name of the program's memory.
    fn check_export(&mut self, f: FnId, state: Option<TyId>) -> bool {
        let def = self.checked.program.func(f);
        if def.name == wrela_abi::EXPORT_MEMORY {
            self.err(
                Diagnostic::new(
                    wrela_diag::codes::E0703,
                    def.name_span,
                    format!(
                        "`{}` can't be exported: the program exports its memory under that name",
                        def.name
                    ),
                )
                .with_help(
                    "rename it, or make it private (drop `pub`) if the host doesn't call it",
                ),
            );
            return false;
        }
        use wrela_sema::defs::{Mode, RetMode};
        if def.name == wrela_abi::EXPORT_INIT {
            let t = &self.checked.program.types;
            let ok = def.params.is_empty()
                && !t.is_unit(def.ret)
                && def.ret_mode == RetMode::Owned
                && self.checked.program.fn_all_generics(f).is_empty();
            if !ok {
                self.err(
                    Diagnostic::new(wrela_diag::codes::E0703, def.sig_span, "`init`'s signature is `init() -> State`: it makes the program's state, which the host keeps")
                        .with_note("the host calls `init` once, then passes its result to `frame` and the other exports that take the state first (§12)"),
                );
            }
            return ok;
        }
        // An export may take the program's state first, as `mut` or borrowed.
        let takes_state = state.is_some_and(|s| def.params.first().is_some_and(|p| p.ty == s));
        if def.name == wrela_abi::EXPORT_FRAME {
            let t = &self.checked.program.types;
            let want = [t.f32, t.u32, t.u32];
            let rest = if takes_state { &def.params[1..] } else { &def.params[..] };
            let params_ok = rest.len() == want.len()
                && rest.iter().zip(want).all(|(p, w)| p.ty == w && p.mode == Mode::Borrow)
                && (takes_state || state.is_none())
                && (!takes_state || def.params[0].mode == Mode::Mut);
            if params_ok && def.ret == t.unit && def.ret_mode == RetMode::Owned {
                return true;
            }
            let msg = match state {
                Some(s) => format!(
                    "`frame`'s signature is `frame(state: mut {}, time: f32, width: u32, height: u32)`, with no result: the host calls it each frame",
                    self.checked.program.display_ty(s)
                ),
                None => "`frame`'s signature is `frame(time: f32, width: u32, height: u32)`, with no result: the host calls it each frame".into(),
            };
            self.err(
                Diagnostic::new(wrela_diag::codes::E0703, def.sig_span, msg)
                    .with_note("a host program exports `frame`: the time in seconds, and the screen's size in pixels; a program with `init` gets its state first (§12)"),
            );
            return false;
        }
        if !self.checked.program.fn_all_generics(f).is_empty() {
            self.err(
                Diagnostic::new(wrela_diag::codes::E0703, def.sig_span, format!("`{}` is exported, so it can't be generic: the host calls it with numbers, and can't choose its types", def.name))
                    .with_note("the entry module's `pub fn`s are the program's exports, called by the host")
                    .with_help("make it private (drop `pub`), and export a function that calls it with the types the host needs"),
            );
            return false;
        }
        let mut ok = true;
        let exportable = |t: &TyKind, ret: bool| match t {
            TyKind::Bool | TyKind::Int(_) | TyKind::Float(_) => true,
            TyKind::Vec(_) => true,
            TyKind::Tuple(ts) => ret && ts.is_empty(),
            _ => false,
        };
        let skip = usize::from(takes_state);
        for p in &def.params[skip..] {
            let k = self.checked.program.types.kind(p.ty);
            if !exportable(k, false) {
                ok = false;
                let shown = self.checked.program.display_ty(p.ty);
                self.err(
                    Diagnostic::new(wrela_diag::codes::E0703, p.span, format!("`{}` is exported from the program, so its parameters are numbers, bools or vectors; `{}` is a `{shown}`", def.name, p.name))
                        .with_note("the entry module's `pub fn`s are the program's exports, called by the host")
                        .with_help("make it private (drop `pub`) if the host doesn't call it"),
                );
            } else if p.mode != wrela_sema::defs::Mode::Borrow {
                ok = false;
                let mode = p.mode.keyword();
                self.err(
                    Diagnostic::new(wrela_diag::codes::E0703, p.span, format!("`{}` is exported from the program, so its parameters are borrowed; `{}` is `{mode}`", def.name, p.name))
                        .with_note("the host passes each argument as a value, which has no place to change or to move from")
                        .with_help(format!("drop `{mode}`, or make the function private (drop `pub`) if the host doesn't call it")),
                );
            }
        }
        let rk = self.checked.program.types.kind(def.ret);
        if !exportable(rk, true) || def.ret_mode != wrela_sema::defs::RetMode::Owned {
            ok = false;
            let mode = match def.ret_mode {
                wrela_sema::defs::RetMode::Owned => "",
                wrela_sema::defs::RetMode::Borrow => "borrow ",
                wrela_sema::defs::RetMode::Mut => "mut ",
            };
            let shown = format!("{mode}{}", self.checked.program.display_ty(def.ret));
            let mut d = Diagnostic::new(
                wrela_diag::codes::E0703,
                def.sig_span,
                format!(
                    "`{}` is exported, so it returns a number, a `bool`, a vector or nothing, not `{shown}`",
                    def.name
                ),
            );
            if !mode.is_empty() {
                d = d.with_note(
                    "a projection points into the program's memory, which the host can't hold",
                );
            }
            self.err(d.with_help("make it private (drop `pub`) if the host doesn't call it"));
        }
        ok
    }

    /// E0703 for a program that doesn't export `frame`, which every host calls: at a private
    /// `frame`, else at the start of main.wrela.
    fn no_frame(&mut self) {
        let p = &self.checked.program;
        let Some(main) = p.main else { return };
        let private = p.fns.iter().find(|f| {
            f.module == main
                && f.owner == wrela_sema::defs::FnOwner::Free
                && f.name == wrela_abi::EXPORT_FRAME
        });
        let main_file = p.main_file.map(|f| Span::new(f, 0, 0));
        let (span, help) = match (private, main_file) {
            (Some(f), _) if !f.public => (f.name_span, "make it public: `pub fn frame`"),
            (Some(f), _) => (
                f.name_span,
                "the host calls it directly, so it can't be generic or a GPU entry point",
            ),
            (None, Some(s)) => {
                (s, "add `pub fn frame(time: f32, width: u32, height: u32) {}` to main.wrela")
            }
            (None, None) => return,
        };
        self.err(
            Diagnostic::new(
                wrela_diag::codes::E0703,
                span,
                "the program doesn't export `frame`, which the host calls each frame",
            )
            .with_help(help),
        );
    }

    /// An instance's function as the source names it: `Sphere::distance`, or `a closure in
    /// main`. `None` for a derived interpretation, which has no source of its own.
    pub fn source_name(&self, key: &InstanceKey) -> Option<String> {
        let p = &self.checked.program;
        match key {
            InstanceKey::Fn { func, .. } => Some(p.fn_display_name(*func)),
            InstanceKey::Closure { owner, .. } => {
                owner.source_fn().map(|f| format!("a closure in {}", p.fn_display_name(f)))
            }
            InstanceKey::Derived { .. } | InstanceKey::Glue { .. } => None,
        }
    }

    pub fn entry_of(&self, f: FnId) -> Option<Entry> {
        self.checked.program.func(f).attrs.entry.map(|e| e.0)
    }
}

/// How many instantiations deep a call chain may go from an export or entry point, counting
/// those with type arguments or callables. Real programs are a few levels deep; only
/// polymorphic recursion gets near.
const MAX_INSTANCE_DEPTH: u32 = 256;

/// The std function implementing a transcendental builtin on the CPU (language.md §11): the
/// one the built-in function that lowers to it names.
/// The program's `init` (§12): `pub fn init() -> S` in main.wrela, and its state type `S`.
fn state_type(checked: &Checked) -> Option<(FnId, TyId)> {
    let p = &checked.program;
    let main = p.main?;
    p.fns.iter().enumerate().find_map(|(i, f)| {
        (f.module == main
            && f.public
            && f.owner == wrela_sema::defs::FnOwner::Free
            && f.name == wrela_abi::EXPORT_INIT
            && f.params.is_empty()
            && f.generics.is_empty()
            && !p.types.is_unit(f.ret))
        .then_some((FnId(i as u32), f.ret))
    })
}

/// An export that runs `inner` on the program's state, in data `state`: with `init`, it stores
/// what `inner` returns there; otherwise it passes the state for `inner`'s first parameter.
fn state_wrapper(
    mb: &mut ModuleBuilder,
    inner: ir::FuncId,
    state: ir::DataId,
    init: bool,
) -> ir::FuncId {
    let f = &mb.m.functions[inner.index()];
    let name = format!("{}_export", f.name);
    let at = ir::Place::root(ir::PlaceRoot::Data(state));
    if init {
        let ret = f.ret;
        let mut w = ir::Function::new(name, Vec::new(), None);
        if let Some(t) = ret {
            let v = w.new_value(t);
            w.body = vec![
                ir::Stmt::Let(v, ir::Expr::Call(inner, Vec::new())),
                ir::Stmt::Store(at, v),
                ir::Stmt::Return(None),
            ];
        }
        return mb.m.add_function(w);
    }
    let (params, ret, first) = (f.params[1..].to_vec(), f.ret, f.params[0].clone());
    let mut w = ir::Function::new(name, params.clone(), ret);
    w.ret_ref = f.ret_ref;
    let mut body = Vec::new();
    let mut args = Vec::new();
    if first.by_ref {
        args.push(ir::Arg::Place(at));
    } else {
        let v = w.new_value(first.ty);
        body.push(ir::Stmt::Let(v, ir::Expr::Load(at)));
        args.push(ir::Arg::Value(v));
    }
    for i in 0..params.len() {
        args.push(w.param_arg(i as u32, &mut body));
    }
    match ret {
        Some(t) => {
            let r = w.new_value(t);
            body.push(ir::Stmt::Let(r, ir::Expr::Call(inner, args)));
            body.push(ir::Stmt::Return(Some(r)));
        }
        None => {
            body.push(ir::Stmt::Eval(ir::Expr::Call(inner, args)));
            body.push(ir::Stmt::Return(None));
        }
    }
    w.body = body;
    mb.m.add_function(w)
}

/// Marks the recursive functions `@deterministic` code reaches (language.md §8): their depth is
/// counted, and a call past [`ir::RECURSION_LIMIT`] of them in progress panics, the same way on
/// every engine, before engines' different stack limits could matter. A function in a cycle of
/// calls is counted wherever it's called from. Drop and clone glue isn't: it follows the data.
fn mark_counted(cx: &mut Cx, mb: &mut ModuleBuilder) {
    let p = &cx.checked.program;
    let n = mb.m.functions.len();
    let mut adj = vec![Vec::new(); n];
    for &(a, b, _) in &mb.edges {
        adj[a.index()].push(b.index());
    }
    let mut glue = vec![false; n];
    let mut work = Vec::new();
    for (key, id) in &mb.instances {
        match key {
            InstanceKey::Fn { func, .. } if p.func(*func).attrs.deterministic.is_some() => {
                work.push(id.index());
            }
            InstanceKey::Glue { .. } => glue[id.index()] = true,
            _ => {}
        }
    }
    let mut reached = vec![false; n];
    while let Some(i) = work.pop() {
        if !std::mem::replace(&mut reached[i], true) {
            work.extend(&adj[i]);
        }
    }
    let mut any = false;
    for comp in wrela_sema::effects::sccs(&adj) {
        let cyclic = comp.len() > 1 || adj[comp[0]].contains(&comp[0]);
        for &f in comp.iter().filter(|&&f| cyclic && reached[f] && !glue[f]) {
            mb.m.functions[f].counted = true;
            any = true;
        }
    }
    if any {
        let msg = format!("recursion deeper than {} calls", ir::RECURSION_LIMIT);
        let (d, len) = cx.text_data(mb, &msg);
        mb.m.depth_message = Some((d, len));
    }
}

pub(crate) fn cpu_math_lang(b: ir::Builtin) -> Option<Lang> {
    BuiltinFn::ALL.iter().find_map(|&f| match body::builtin_ir(f) {
        body::BuiltinIr::Math(m) if m == b => f.cpu_impl(),
        _ => None,
    })
}

/// Replaces the CPU module's transcendental builtins by calls to std's wrela implementations,
/// per component for vectors. Runs after derived interpretations, which read the builtins.
fn rewrite_cpu_math(cx: &mut Cx, mb: &mut ModuleBuilder) {
    let mut i = 0;
    while i < mb.m.functions.len() {
        let mut f = std::mem::take(&mut mb.m.functions[i]);
        let body = std::mem::take(&mut f.body);
        f.body = rewrite_block(cx, mb, &mut f, body);
        mb.m.functions[i] = f;
        i += 1;
    }
}

fn rewrite_block(
    cx: &mut Cx,
    mb: &mut ModuleBuilder,
    f: &mut ir::Function,
    b: ir::Block,
) -> ir::Block {
    let mut out = Vec::with_capacity(b.len());
    for mut s in b {
        if let ir::Stmt::Let(v, ir::Expr::Builtin(op, args)) = &s
            && let Some(func) = cpu_math_lang(*op).and_then(|l| cx.checked.program.lang_fn(l))
        {
            let v = *v;
            let callee = cx.instance(mb, InstanceKey::plain(func, Vec::new()), None);
            let ty = f.value_ty(v);
            let f32 = mb.m.types.f32();
            if let &ir::TypeDef::Vector(n) = mb.m.types.get(ty) {
                let mut comps = Vec::new();
                for c in 0..n as u32 {
                    let mut parts = Vec::new();
                    for &a in args {
                        let x = f.new_value(f32);
                        out.push(ir::Stmt::Let(x, ir::Expr::Extract(a, c)));
                        parts.push(ir::Arg::Value(x));
                    }
                    let r = f.new_value(f32);
                    out.push(ir::Stmt::Let(r, ir::Expr::Call(callee, parts)));
                    comps.push(r);
                }
                out.push(ir::Stmt::Let(v, ir::Expr::Construct(ty, comps)));
            } else {
                let args = args.iter().map(|&a| ir::Arg::Value(a)).collect();
                out.push(ir::Stmt::Let(v, ir::Expr::Call(callee, args)));
            }
            continue;
        }
        for inner in s.blocks_mut() {
            let b = std::mem::take(inner);
            *inner = rewrite_block(cx, mb, f, b);
        }
        out.push(s);
    }
    out
}
