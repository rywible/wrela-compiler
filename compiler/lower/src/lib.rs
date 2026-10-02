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

mod body;
mod gpu;
mod instance;
mod ty;

pub use gpu::{PipelineKind, PipelineOut};
pub use instance::{Callable, InstanceKey};

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use wrela_diag::{Diagnostic, Span};
use wrela_ir as ir;
use wrela_sema::Checked;
use wrela_sema::builtins::BuiltinFn;
use wrela_sema::defs::{Entry, Lang};
use wrela_sema::ty::{FnId, TyId, TyKind};

/// The program, lowered: the CPU module and one module per GPU pipeline.
#[derive(Debug)]
pub struct Lowered {
    pub cpu: ir::Module,
    pub pipelines: Vec<PipelineOut>,
}

/// What to lower.
pub struct Roots {
    /// Functions exported from the CPU module: the entry module's `pub fn`s.
    pub exports: Vec<FnId>,
    /// Non-generic functions to check on the GPU even if nothing dispatches them: entry points
    /// and `@gpu` functions (so their effects are checked where they're defined).
    pub gpu_checks: Vec<FnId>,
}

impl Roots {
    /// The default roots: the entry module's public functions; every non-generic entry point
    /// and `@gpu` function in the package.
    pub fn of(checked: &Checked) -> Roots {
        let p = &checked.program;
        let mut exports = Vec::new();
        let mut gpu_checks = Vec::new();
        for (i, f) in p.fns.iter().enumerate() {
            let id = FnId(i as u32);
            if p.is_std(f.module) || !checked.mir.contains_key(&id) {
                continue;
            }
            if Some(f.module) == p.main
                && f.public
                && f.owner == wrela_sema::defs::FnOwner::Free
                && f.attrs.entry.is_none()
                && p.fn_all_generics(id).is_empty()
            {
                exports.push(id);
            }
            if (f.attrs.entry.is_some() || f.attrs.gpu.is_some())
                && p.fn_all_generics(id).is_empty()
            {
                gpu_checks.push(id);
            }
        }
        Roots { exports, gpu_checks }
    }
}

/// Shared state while lowering every module of one program.
pub(crate) struct Cx<'a> {
    pub checked: &'a Checked,
    pub diags: Vec<Diagnostic>,
    /// Pipelines discovered by CPU code, deduplicated, in discovery order.
    pub pipelines: Vec<gpu::PipelineKey>,
}

/// One target module being built.
pub(crate) struct ModuleBuilder {
    pub m: ir::Module,
    pub target: ir::Target,
    pub instances: HashMap<InstanceKey, ir::FuncId>,
    pub queue: VecDeque<(InstanceKey, ir::FuncId)>,
    pub type_cache: HashMap<TyId, Option<ir::TypeId>>,
    /// GPU: what the module is for, to check effects with context.
    pub gpu: Option<gpu::GpuCx>,
    /// The first caller of each instance, for call chains in diagnostics.
    pub callers: HashMap<ir::FuncId, (ir::FuncId, Span)>,
    /// How many instantiations each instance is from a root (an export or entry point).
    pub depth: HashMap<ir::FuncId, u32>,
    /// Whether the instantiation limit has been reported in this module.
    pub too_deep: bool,
    /// Instances being lowered right now.
    pub active: Vec<ir::FuncId>,
    /// Every call between instances, for finding recursion on the GPU.
    pub edges: Vec<(ir::FuncId, ir::FuncId, Span)>,
    /// Derived functions built in this module.
    pub derived: ir::derive::DeriveCache,
}

impl ModuleBuilder {
    pub fn new(target: ir::Target) -> ModuleBuilder {
        ModuleBuilder {
            m: ir::Module::default(),
            target,
            instances: HashMap::new(),
            queue: VecDeque::new(),
            type_cache: HashMap::new(),
            gpu: None,
            callers: HashMap::new(),
            depth: HashMap::new(),
            too_deep: false,
            active: Vec::new(),
            edges: Vec::new(),
            derived: ir::derive::DeriveCache::default(),
        }
    }
}

/// Lowers a checked program (with no errors) to IR.
pub fn lower(checked: &Checked, roots: &Roots) -> (Lowered, Vec<Diagnostic>) {
    let mut cx = Cx { checked, diags: Vec::new(), pipelines: Vec::new() };
    let mut cpu = ModuleBuilder::new(ir::Target::Cpu);
    for &f in &roots.exports {
        let key = InstanceKey::plain(f, Vec::new());
        let id = cx.instance(&mut cpu, key, None);
        let name = cx.checked.program.func(f).name.clone();
        if cx.check_export(f) {
            cpu.m.exports.push((name, id));
        }
    }
    cx.drain(&mut cpu);
    rewrite_cpu_math(&mut cx, &mut cpu);
    cx.drain(&mut cpu);
    if !cx.diags.iter().any(|d| d.is_error())
        && let Err(e) = ir::verify(&cpu.m)
    {
        cx.err(Diagnostic::internal(format!("the CPU module's IR is malformed: {e}")));
    }
    // Pipelines: those CPU code records, then the standalone GPU checks.
    let mut pipelines = Vec::new();
    let mut i = 0;
    while i < cx.pipelines.len() {
        let key = cx.pipelines[i].clone();
        if let Some(out) = gpu::lower_pipeline(&mut cx, i as u32, &key) {
            pipelines.push(out);
        }
        i += 1;
    }
    for &f in &roots.gpu_checks {
        let entry = cx.checked.program.func(f).attrs.entry.map(|e| e.0);
        let already = cx.pipelines.iter().any(|k| k.mentions(f));
        if !already {
            gpu::check_standalone(&mut cx, f, entry);
        }
    }
    let lowered = Lowered { cpu: cpu.m, pipelines };
    let mut diags = cx.diags;
    wrela_diag::sort_and_dedup(&mut diags);
    (lowered, diags)
}

impl<'a> Cx<'a> {
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
        let depth = caller.map_or(0, |(from, _)| mb.depth.get(&from).copied().unwrap_or(0) + 1);
        let f = self.signature(mb, &key);
        let id = mb.m.add_function(f);
        mb.instances.insert(key.clone(), id);
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
                self.report_too_deep(&key, caller.map(|c| c.1));
            }
            mb.m.functions[id.index()].body = vec![ir::Stmt::Trap];
            return id;
        }
        mb.queue.push_back((key, id));
        id
    }

    fn report_too_deep(&mut self, key: &InstanceKey, at: Option<Span>) {
        let Some(func) = key.source_fn() else { return };
        let name = self.checked.program.fn_display_name(func);
        let args: Vec<String> =
            key.substs().iter().map(|&t| self.checked.program.display_ty(t)).collect();
        let span = at.unwrap_or(self.checked.program.func(func).sig_span);
        self.err(
            Diagnostic::new(
                wrela_diag::codes::E0412,
                span,
                format!("instantiating `{name}` never ends: each instance calls one with a larger type"),
            )
            .with_note(format!(
                "after {MAX_INSTANCE_DEPTH} levels it's instantiated with <{}>",
                args.join(", ")
            ))
            .with_note("generics are monomorphized (§7), so a generic function can't call itself with an ever-growing type"),
        );
    }

    /// Lowers queued instance bodies until none are left.
    pub fn drain(&mut self, mb: &mut ModuleBuilder) {
        while let Some((key, id)) = mb.queue.pop_front() {
            mb.active.push(id);
            body::lower_body(self, mb, &key, id);
            mb.active.pop();
        }
    }

    /// Whether an exported function's signature can cross the WASM boundary: scalars in, and a
    /// scalar, a vector or nothing out.
    fn check_export(&mut self, f: FnId) -> bool {
        let def = self.checked.program.func(f).clone();
        let mut ok = true;
        let exportable = |t: &TyKind, ret: bool| match t {
            TyKind::Bool | TyKind::Int(_) | TyKind::Float(_) => true,
            TyKind::Vec(_) => ret,
            TyKind::Tuple(ts) => ret && ts.is_empty(),
            _ => false,
        };
        for p in &def.params {
            let k = self.checked.program.types.kind(p.ty).clone();
            if !exportable(&k, false) || p.mode != wrela_sema::defs::Mode::Borrow {
                ok = false;
                let shown = self.checked.program.display_ty(p.ty);
                self.err(
                    Diagnostic::new(wrela_diag::codes::E0703, p.span, format!("`{}` is exported from the program, so its parameters are numbers or bools; `{}` is a `{shown}`", def.name, p.name))
                        .with_note("the entry module's `pub fn`s are the program's exports, called by the host")
                        .with_help("make it private (drop `pub`) if the host doesn't call it"),
                );
            }
        }
        let rk = self.checked.program.types.kind(def.ret).clone();
        if !exportable(&rk, true) || def.ret_mode != wrela_sema::defs::RetMode::Owned {
            ok = false;
            let shown = self.checked.program.display_ty(def.ret);
            self.err(
                Diagnostic::new(wrela_diag::codes::E0703, def.sig_span, format!("`{}` is exported, so it returns a number, a vector or nothing, not `{shown}`", def.name))
                    .with_help("make it private (drop `pub`) if the host doesn't call it"),
            );
        }
        ok
    }

    /// The lang item function `l`.
    pub fn lang_fn(&self, l: Lang) -> Option<FnId> {
        self.checked.program.lang_fn(l)
    }

    pub fn entry_of(&self, f: FnId) -> Option<Entry> {
        self.checked.program.func(f).attrs.entry.map(|e| e.0)
    }
}

/// How many instantiations deep a call chain may go from an export or entry point. Real
/// programs are a few levels deep; only polymorphic recursion gets near.
const MAX_INSTANCE_DEPTH: u32 = 256;

/// Rc helper so instance keys can nest.
pub(crate) fn rc<T>(t: T) -> Rc<T> {
    Rc::new(t)
}

/// The std function implementing a transcendental builtin on the CPU (language.md §11): the
/// one the built-in function that lowers to it names.
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
        let mut f =
            std::mem::replace(&mut mb.m.functions[i], ir::Function::new("", Vec::new(), None));
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
    for s in b {
        match s {
            ir::Stmt::Let(v, ir::Expr::Builtin(op, args)) if cpu_math_lang(op).is_some() => {
                let Some(func) = cpu_math_lang(op).and_then(|l| cx.lang_fn(l)) else {
                    out.push(ir::Stmt::Let(v, ir::Expr::Builtin(op, args)));
                    continue;
                };
                let callee = cx.instance(mb, InstanceKey::plain(func, Vec::new()), None);
                let ty = f.value_ty(v);
                let f32 = mb.m.types.f32();
                match mb.m.types.get(ty).clone() {
                    ir::TypeDef::Vector(n) => {
                        let mut comps = Vec::new();
                        for c in 0..n as u32 {
                            let mut parts = Vec::new();
                            for &a in &args {
                                let x = f.new_value(f32);
                                out.push(ir::Stmt::Let(x, ir::Expr::Extract(a, c)));
                                parts.push(ir::Arg::Value(x));
                            }
                            let r = f.new_value(f32);
                            out.push(ir::Stmt::Let(r, ir::Expr::Call(callee, parts)));
                            comps.push(r);
                        }
                        out.push(ir::Stmt::Let(v, ir::Expr::Construct(ty, comps)));
                    }
                    _ => out.push(ir::Stmt::Let(
                        v,
                        ir::Expr::Call(callee, args.into_iter().map(ir::Arg::Value).collect()),
                    )),
                }
            }
            ir::Stmt::If { cond, then, else_ } => {
                let then = rewrite_block(cx, mb, f, then);
                let else_ = rewrite_block(cx, mb, f, else_);
                out.push(ir::Stmt::If { cond, then, else_ });
            }
            ir::Stmt::Loop { body, continuing } => {
                let body = rewrite_block(cx, mb, f, body);
                let continuing = rewrite_block(cx, mb, f, continuing);
                out.push(ir::Stmt::Loop { body, continuing });
            }
            other => out.push(other),
        }
    }
    out
}
