//! Build-time constants (language.md §10): the build runs the program's own code to compute
//! each constant whose value isn't literal. Lowering makes a module that computes them
//! (`wrela_lower::lower_consts`); this runs it in wasmtime, with a fuel limit, reads each value
//! back, and makes a panic, a trap or a computation that runs away a compile error with the
//! call chain.
//!
//! Constants are computed in rounds: those whose code reads no constant still to be computed
//! first, in one module, then those that read them, and so on. Constants that read each other
//! are an error (E0328).

use crate::const_cache;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use wrela_diag::{Diagnostic, SourceMap, Span, codes};
use wrela_lower::{BuildData, ConstLowering, Embeds, Memory};
use wrela_sema::Checked;
use wrela_sema::ty::ConstId;

/// How much work computing one constant may do: wasmtime's fuel, about one unit per WASM
/// instruction. A computation that goes past it is a compile error, not a hang. About 17 billion
/// instructions: under a second on a 2024 laptop.
pub const FUEL: u64 = 1 << 34;

/// How much work one test may do (§10): eight times a constant's, since a test runs only when
/// `wrela test` asks, and a simulation stepped for some seconds is a test worth writing. About
/// 137 billion instructions: a few seconds.
pub const TEST_FUEL: u64 = 1 << 37;

/// Reads the files `embed` names, relative to the package's root at `root` (§10): a file that
/// can't be read, or whose path leaves the package, is E0218.
pub fn read_embeds(checked: &Checked, dirs: &[std::path::PathBuf]) -> (Embeds, Vec<Diagnostic>) {
    let mut files = Embeds::new();
    let mut diags = Vec::new();
    let mut seen = BTreeSet::new();
    for (key, span) in wrela_sema::embeds(checked) {
        if !seen.insert(key.clone()) {
            continue;
        }
        // Read from the directory of the package that embeds it.
        let Some((package, path)) = wrela_sema::split_embed_key(&key) else { continue };
        let Some(root) = dirs.get(package) else { continue };
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let read = match root.join(path).canonicalize() {
            // A symbolic link may lead out of the package.
            Ok(full) if !full.starts_with(&root) => Err("it's outside the package".to_string()),
            Ok(full) if !full.is_file() => Err("it isn't a file".to_string()),
            Ok(full) => std::fs::read(full).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err("there's no such file in the package".to_string())
            }
            Err(e) => Err(e.to_string()),
        };
        match read {
            Ok(bytes) => {
                files.insert(key, Arc::from(bytes));
            }
            Err(why) => diags.push(
                Diagnostic::new(codes::E0218, span, format!("`embed` can't read `{path}`: {why}"))
                    .with_note("a path is relative to the package's root: the directory with `main.wrela` (§10)"),
            ),
        }
    }
    (files, diags)
}

/// What computing the constants did: how many the build computed, and how many it read from
/// the cache (§10).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConstStats {
    pub computed: u32,
    pub cached: u32,
    /// How many chunks of the computed constants' parallel jobs the build's helpers ran.
    pub helped: u64,
    /// What each computed constant that printed or timed a phase did (§8's `trace`), by name:
    /// its lines were shown as they were made.
    pub traced: Vec<(String, wrela_host::Traced)>,
}

/// Computes the program's computed constants into `data` (whose files `embed` reads are
/// already there): the errors computing them. With `cache`, a directory, a constant whose inputs
/// are unchanged since a build computed it is read from there ([`crate::const_cache`]).
pub fn compute(
    checked: &Checked,
    sources: &SourceMap,
    data: &mut BuildData,
    cache: Option<&std::path::Path>,
) -> (Vec<Diagnostic>, ConstStats) {
    let mut diags = Vec::new();
    let mut stats = ConstStats::default();
    let mut pending: Vec<ConstId> = (0..checked.program.consts.len())
        .map(|i| ConstId(i as u32))
        .filter(|&c| wrela_lower::is_computed(checked, c))
        .collect();
    let mut failed = BTreeSet::new();
    let simd = !data.no_simd;
    while !pending.is_empty() {
        // Each constant in a module of its own, which holds only what it reads: its key. Those
        // that need a constant still to compute wait for the next round.
        let mut ready = Vec::new();
        let mut rest = Vec::new();
        for &c in &pending {
            match wrela_lower::lower_consts(checked, &[c], data) {
                (ConstLowering::Ready(m), d) => ready.push((c, m, d)),
                // One that needs a constant the build couldn't compute: that one's error says
                // why.
                (ConstLowering::Needs(n), _) if n.iter().any(|x| failed.contains(x)) => {
                    failed.insert(c);
                }
                (ConstLowering::Needs(n), _) => rest.push((c, n)),
            }
        }
        if ready.is_empty() {
            if let Some(d) = cycle(checked, &rest) {
                diags.push(d);
            }
            break;
        }
        let mut errors = false;
        for (_, _, d) in &mut ready {
            errors |= wrela_diag::has_errors(d);
            diags.append(d);
        }
        if errors {
            break;
        }
        for (c, module, _) in ready {
            let fuel = checked.program.const_(c).fuel.unwrap_or(FUEL);
            // The key is the module's WASM, so the cache is read before wasmtime compiles it:
            // a constant read from the cache isn't compiled at all.
            let result = match emit(&module.module, simd, true) {
                Err(msg) => Err(Diagnostic::internal(msg)),
                Ok(out) => {
                    let key = const_cache::key(&out.wasm, fuel);
                    match cache.and_then(|dir| const_cache::get(dir, &key)) {
                        Some(v) => {
                            stats.cached += 1;
                            Ok(v)
                        }
                        None => {
                            stats.computed += 1;
                            let export = &module.exports[0].1;
                            let name = checked.program.const_(c).name.clone();
                            let trace = wrela_host::Trace::new(Some(format!("{name}: ")));
                            let r = Compiled::of(engine(), out)
                                .map_err(|msg| Box::new(Diagnostic::internal(msg)))
                                .and_then(|compiled| {
                                    run_one(checked, sources, &compiled, export, c, fuel, &trace)
                                })
                                .map(|(v, helped)| {
                                    stats.helped += u64::from(helped);
                                    v
                                })
                                .map_err(|d| *d);
                            let traced = trace.take();
                            if !traced.phases.is_empty() {
                                eprintln!("{name}'s phases: {}", traced.phases_line());
                            }
                            if !traced.is_empty() {
                                stats.traced.push((name, traced));
                            }
                            if let (Ok(v), Some(dir)) = (&r, cache) {
                                const_cache::put(dir, &key, v);
                            }
                            r
                        }
                    }
                }
            };
            match result {
                Ok(v) => {
                    data.values.insert(c, Rc::new(v));
                }
                Err(d) => {
                    diags.push(d);
                    failed.insert(c);
                }
            }
        }
        pending = rest.into_iter().map(|(c, _)| c).collect();
    }
    if let Some(dir) = cache {
        const_cache::clean(dir);
    }
    (diags, stats)
}

/// The engine for the code builds run (constants and tests): the native host's that counts
/// fuel, one for the process, as wasmtime intends (an engine is costly to make, and cheap to
/// share).
fn engine() -> &'static wasmtime::Engine {
    wrela_host::metered_engine()
}

/// E0328 for constants that read each other, through the functions that compute them: one
/// cycle among `blocked` (each with the constants it needs).
fn cycle(checked: &Checked, blocked: &[(ConstId, BTreeSet<ConstId>)]) -> Option<Diagnostic> {
    let needs = |c: ConstId| blocked.iter().find(|(x, _)| *x == c).map(|(_, n)| n);
    let mut path = vec![blocked.first()?.0];
    loop {
        let last = *path.last()?;
        let next = *needs(last)?.iter().find(|n| needs(**n).is_some())?;
        if let Some(i) = path.iter().position(|&x| x == next) {
            path.drain(..i);
            break;
        }
        path.push(next);
    }
    let p = &checked.program;
    let first = p.const_(path[0]);
    let names: Vec<String> = path.iter().map(|&c| p.const_(c).short()).collect();
    let msg = if path.len() == 1 {
        format!("computing {} needs its own value", first.shown())
    } else {
        format!(
            "computing {} needs its own value: {} → {}",
            first.shown(),
            names.join(" → "),
            first.short()
        )
    };
    let mut d = Diagnostic::new(codes::E0328, first.span, msg)
        .with_note("the code that computes a constant reads it, through the functions it calls");
    for &c in &path[1..] {
        let def = p.const_(c);
        d = d.with_secondary(def.span, format!("`{}` is part of the cycle", def.name));
    }
    Some(d.with_help("compute one of them without reading the others"))
}

/// A module of the build's code, running in wasmtime, with the host's functions refusing: what
/// computes constants and runs tests. A constant's runs on memory its helpers share (§6.12).
struct Running {
    store: wasmtime::Store<()>,
    instance: wasmtime::Instance,
    memory: Mem,
}

/// The memory build-time code runs on: its own (a test's), or shared with helpers (a
/// constant's).
enum Mem {
    Own(wasmtime::Memory),
    Shared(wasmtime::SharedMemory),
}

/// The build's code, compiled for wasmtime: started once to compute constants, and once for
/// each test.
struct Compiled {
    module: wasmtime::Module,
    /// Where each WASM offset is in the source, for call chains.
    lines: Vec<(u32, Option<Span>)>,
}

/// The build's code as WASM, with where each offset is in the source. With `shared`, its memory
/// is shared and imported, so helpers can run its parallel jobs.
fn emit(m: &wrela_ir::Module, simd: bool, shared: bool) -> Result<wrela_wasm::Emitted, String> {
    let options = wrela_wasm::Options { shared_memory: shared, simd };
    wrela_wasm::emit_with(m, options)
        .map_err(|e| format!("the WASM back end failed on the build's code: {e}"))
}

/// The build's code, compiled for wasmtime ([`emit`]'s options).
fn compile(
    engine: &wasmtime::Engine,
    m: &wrela_ir::Module,
    simd: bool,
    shared: bool,
) -> Result<Compiled, String> {
    Compiled::of(engine, emit(m, simd, shared)?)
}

fn refusal(results: u32) -> &'static str {
    if results == 0 {
        "build-time code can't call the host"
    } else {
        "build-time code can't make requests"
    }
}

impl Compiled {
    /// `out`'s WASM, compiled for wasmtime: the costly step, which a constant read from the
    /// cache skips.
    fn of(engine: &wasmtime::Engine, out: wrela_wasm::Emitted) -> Result<Compiled, String> {
        let module = wasmtime::Module::new(engine, &out.wasm)
            .map_err(|e| format!("wasmtime refused the build's code: {e:#}"))?;
        Ok(Compiled { module, lines: out.lines })
    }

    /// An instance on new memory: its own, or (`shared`) memory it shares with helpers. What
    /// it prints and times goes to `trace`.
    fn start(
        &self,
        engine: &wasmtime::Engine,
        shared: bool,
        trace: &wrela_host::Trace,
    ) -> Result<Running, String> {
        let mut linker = wasmtime::Linker::new(engine);
        let mut store = wasmtime::Store::new(engine, ());
        store.set_fuel(FUEL).map_err(|e| format!("{e:#}"))?;
        let mut shared_memory = None;
        if shared {
            let ty = self
                .module
                .imports()
                .find_map(|i| i.ty().memory().cloned())
                .ok_or_else(|| "the build's code imports no memory".to_string())?;
            let m = wasmtime::SharedMemory::new(engine, ty).map_err(|e| format!("{e:#}"))?;
            linker
                .define(&store, wrela_abi::IMPORT_MODULE, wrela_abi::IMPORT_MEMORY, m.clone())
                .map_err(|e| format!("{e:#}"))?;
            shared_memory = Some(m);
        }
        // Effects keep constants and tests from recording GPU work and making requests (§8):
        // nothing calls these. Only a program that starts a voice imports `audio`, and they
        // can't. A line printed and a phase timed go to the trace.
        wrela_host::refuse_host_functions(
            &mut linker,
            engine,
            refusal,
            trace,
            shared_memory.as_ref(),
        )
        .map_err(|e| format!("{e:#}"))?;
        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|e| format!("the build's code didn't start: {e:#}"))?;
        let memory = match shared_memory {
            Some(m) => Mem::Shared(m),
            None => Mem::Own(
                instance
                    .get_memory(&mut store, wrela_abi::EXPORT_MEMORY)
                    .ok_or_else(|| "the build's code has no memory".to_string())?,
            ),
        };
        Ok(Running { store, instance, memory })
    }
}

impl Running {
    /// Calls export `name`, with `fuel`: the address it returns, or why it failed (with the
    /// panic's message, if it panicked).
    fn call(&mut self, name: &str, fuel: u64) -> Result<u32, CallError> {
        self.store.set_fuel(fuel).map_err(CallError::Internal)?;
        let f = self
            .instance
            .get_typed_func::<(), u32>(&mut self.store, name)
            .map_err(CallError::Internal)?;
        f.call(&mut self.store, ()).map_err(|e| {
            let panic = match &self.memory {
                Mem::Own(m) => take_panic_message(m.data_mut(&mut self.store)),
                // A constant's memory isn't used again: its message is read, not cleared.
                Mem::Shared(_) => self.read(read_panic_message),
            };
            CallError::Failed(e, panic)
        })
    }

    /// The fuel the last call used of the `fuel` it was given.
    fn used(&self, fuel: u64) -> u64 {
        fuel - self.store.get_fuel().unwrap_or(0)
    }

    /// `f` of the memory's bytes, read in place.
    fn read<T>(&mut self, f: impl FnOnce(&[u8]) -> T) -> T {
        match &self.memory {
            Mem::Own(m) => f(m.data(&self.store)),
            Mem::Shared(m) => {
                f(wrela_host::build_memory::slice(m, 0, m.data().len()).unwrap_or(&[]))
            }
        }
    }
}

enum CallError {
    Internal(wasmtime::Error),
    Failed(wasmtime::Error, Option<String>),
}

/// How many threads compute a constant: this one and helpers, one a core.
fn const_threads() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
}

/// Computes constant `c` with `compiled` (its module, built with shared memory), on this thread
/// and helpers, with `fuel` for their work together: its value, or why it failed.
fn run_one(
    checked: &Checked,
    sources: &SourceMap,
    compiled: &Compiled,
    export: &str,
    c: ConstId,
    fuel: u64,
    trace: &wrela_host::Trace,
) -> Result<(wrela_lower::Value, u32), Box<Diagnostic>> {
    let mut running =
        compiled.start(engine(), true, trace).map_err(|m| Box::new(Diagnostic::internal(m)))?;
    let Mem::Shared(shared) = &running.memory else {
        return Err(Box::new(Diagnostic::internal("a constant's code runs on shared memory")));
    };
    let helpers =
        wrela_host::BuildHelpers::start(&compiled.module, shared, const_threads() - 1, fuel, trace);
    let result = running.call(export, fuel);
    let main_used = running.used(fuel);
    let ran = helpers.finish();
    let helped = running.read(|bytes| {
        let at = wrela_abi::memory::PAR_HELPED as usize;
        bytes.get(at..at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    });
    let used: u64 = main_used + ran.iter().map(|r| r.used).sum::<u64>();
    let helper_out_of_fuel = ran.iter().any(|r| r.out_of_fuel);
    let out_of_fuel = |frames| Fault {
        frames,
        trap: Some(wasmtime::Trap::OutOfFuel),
        panic: None,
        text: "out of fuel".into(),
    };
    match result {
        Ok(addr) if used <= fuel => running.read(|bytes| {
            let mem = Memory { bytes };
            wrela_lower::read_value(checked, c, &mem, addr).map(|v| (v, helped)).map_err(|e| {
                Box::new(Diagnostic::internal(format!(
                    "reading the value of `{}`: {e}",
                    checked.program.const_(c).name
                )))
            })
        }),
        // Its threads' work together was past its fuel.
        Ok(_) => {
            let fault = out_of_fuel(Vec::new());
            Err(Box::new(failure(
                checked,
                sources,
                &compiled.lines,
                Item::Const(c, fuel),
                &fault,
                None,
            )))
        }
        Err(CallError::Internal(e)) => Err(Box::new(Diagnostic::internal(format!("{e:#}")))),
        Err(CallError::Failed(e, panic)) => {
            let mut fault = Fault::of(&e, panic);
            // A helper that ran out of fuel stops the job it ran, and its join traps: the
            // constant ran out of fuel.
            if helper_out_of_fuel {
                fault = out_of_fuel(fault.frames);
            }
            Err(Box::new(failure(
                checked,
                sources,
                &compiled.lines,
                Item::Const(c, fuel),
                &fault,
                None,
            )))
        }
    }
}

/// Runs the program package's `@test` functions (§10), after its constants are computed into
/// A test that ran: which, why it failed if it did, and what it printed and timed.
pub type Ran = (wrela_sema::ty::FnId, Option<Diagnostic>, wrela_host::Traced);

/// `data`: each test that ran (what it printed shown as it was made), with the errors lowering
/// them found. A failure is E0706 or E0705. Each test runs on new memory, so one test can't
/// change what another sees.
pub fn run_tests(
    checked: &Checked,
    sources: &SourceMap,
    data: &BuildData,
    tests: &[wrela_sema::ty::FnId],
    fuel: u64,
) -> (Vec<Ran>, Vec<Diagnostic>) {
    let (module, diags) = wrela_lower::lower_tests(checked, tests, data);
    let Some(module) = module.filter(|_| !wrela_diag::has_errors(&diags)) else {
        return (Vec::new(), diags);
    };
    // Its own memory, unshared: each test runs on one thread.
    let compiled = match compile(engine(), &module.module, !data.no_simd, false) {
        Ok(c) => c,
        Err(msg) => return (Vec::new(), vec![Diagnostic::internal(msg)]),
    };
    // Each test's fuel: its own (`@fuel`), or a test's.
    let fuel_of =
        |f: wrela_sema::ty::FnId| checked.program.func(f).attrs.fuel.map_or(fuel, |(v, _)| v);
    let exports: Vec<(&str, String, u64)> = module
        .exports
        .iter()
        .map(|(f, name)| (name.as_str(), checked.program.func(*f).name.clone(), fuel_of(*f)))
        .collect();
    let ran = each_at_once(&exports, |(export, test, fuel)| {
        let trace = wrela_host::Trace::new(Some(format!("{test}: ")));
        let ran = compiled.start(engine(), false, &trace).map(|mut r| r.call(export, *fuel));
        (ran, trace.take())
    });
    let results = module
        .exports
        .iter()
        .zip(ran)
        .map(|((f, _), (ran, traced))| {
            let failed = match ran {
                Ok(Ok(_)) => None,
                Err(msg) => Some(Diagnostic::internal(msg)),
                Ok(Err(CallError::Internal(e))) => Some(Diagnostic::internal(format!("{e:#}"))),
                Ok(Err(CallError::Failed(e, panic))) => {
                    let fault = Fault::of(&e, panic);
                    let item = Item::Test(*f, fuel_of(*f));
                    Some(failure(checked, sources, &compiled.lines, item, &fault, None))
                }
            };
            (*f, failed, traced)
        })
        .collect();
    (results, diags)
}

/// `run` of each of `items`, on as many threads as the machine has cores: the results in the
/// items' order. Each test runs in an instance of its own, so one doesn't wait on another.
fn each_at_once<T: Sync, R: Send>(items: &[T], run: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(items.len());
    let next = AtomicUsize::new(0);
    let done = Mutex::new(Vec::with_capacity(items.len()));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let r = run(item);
                    done.lock().unwrap_or_else(|p| p.into_inner()).push((i, r));
                }
            });
        }
    });
    let mut done = done.into_inner().unwrap_or_else(|p| p.into_inner());
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

/// What failed: computing a constant, or a test (each with the fuel it ran with).
#[derive(Clone, Copy)]
pub(crate) enum Item {
    Const(ConstId, u64),
    Test(wrela_sema::ty::FnId, u64),
}

/// A number of instructions, roughly: "17 billion", "16 million".
fn instructions(n: u64) -> String {
    match n {
        1_000_000_000.. => format!("{} billion", n / 1_000_000_000),
        1_000_000.. => format!("{} million", n / 1_000_000),
        _ => n.to_string(),
    }
}

impl Item {
    fn span(self, p: &wrela_sema::program::Program) -> Span {
        match self {
            Item::Const(c, _) => p.const_(c).span,
            Item::Test(f, _) => p.func(f).span,
        }
    }

    /// "computing the constant `X`", "the test `t`".
    fn what(self, p: &wrela_sema::program::Program) -> String {
        match self {
            Item::Const(c, _) => format!("computing {}", p.const_(c).shown()),
            Item::Test(f, _) => format!("the test `{}`", p.func(f).name),
        }
    }

    /// The label where the code that failed was called from the item.
    fn within(self, p: &wrela_sema::program::Program) -> String {
        match self {
            Item::Const(c, _) => format!("while the build computes {}", p.const_(c).short()),
            Item::Test(f, _) => format!("in the test `{}`", p.func(f).name),
        }
    }
}

fn read_panic_message(data: &[u8]) -> Option<String> {
    let at = wrela_abi::memory::panic_at(wrela_abi::memory::THREAD_MAIN) as usize;
    wrela_abi::memory::panic_message(data.get(at..)?)
}

fn take_panic_message(data: &mut [u8]) -> Option<String> {
    let at = wrela_abi::memory::panic_at(wrela_abi::memory::THREAD_MAIN) as usize;
    let msg = wrela_abi::memory::panic_message(data.get(at..)?)?;
    data[at..at + 4].fill(0);
    Some(msg)
}

/// E0704, E0705 or E0706: why computing a constant or a test failed, where, and the calls that
/// led there.
/// How a call into the build's code failed: the WASM offset of each frame, innermost first;
/// the trap, if it was one; the message a panic left; and the error as text.
pub(crate) struct Fault {
    pub frames: Vec<u32>,
    pub trap: Option<wasmtime::Trap>,
    pub panic: Option<String>,
    pub text: String,
}

impl Fault {
    fn of(err: &wasmtime::Error, panic: Option<String>) -> Fault {
        let frames = err
            .downcast_ref::<wasmtime::WasmBacktrace>()
            .map(|bt| {
                bt.frames().iter().filter_map(|f| f.module_offset()).map(|o| o as u32).collect()
            })
            .unwrap_or_default();
        let trap = err.downcast_ref::<wasmtime::Trap>().copied();
        Fault { frames, trap, panic, text: format!("{err:#}") }
    }
}

pub(crate) fn failure(
    checked: &Checked,
    sources: &SourceMap,
    lines: &[(u32, Option<Span>)],
    item: Item,
    fault: &Fault,
    phase: Option<&str>,
) -> Diagnostic {
    let p = &checked.program;
    let item_span = item.span(p);
    // Where each frame is in the source, innermost first; frames with no location are skipped.
    let mut chain: Vec<Span> = Vec::new();
    for &off in &fault.frames {
        let i = lines.partition_point(|&(o, _)| o <= off);
        if let Some(&(_, Some(span))) = i.checked_sub(1).and_then(|i| lines.get(i))
            && chain.last() != Some(&span)
        {
            chain.push(span);
        }
    }
    // The function that runs the item's code marks the whole item: not a call. std's helpers
    // that explain a failed `assert` are how it panics, not calls the program made.
    use wrela_sema::defs::Lang;
    let helpers: Vec<Span> = [Lang::AssertFailed1, Lang::AssertFailed2]
        .into_iter()
        .filter_map(|l| p.lang_fn(l))
        .map(|f| p.func(f).span)
        .collect();
    let inside = |s: &Span, h: &Span| s.file == h.file && h.start <= s.start && s.end <= h.end;
    chain.retain(|s| *s != item_span && !helpers.iter().any(|h| inside(s, h)));
    // std's frames aren't the program's own code, unless the item is std's (its tests).
    let is_std = |s: &Span| sources.file(s.file).name.starts_with('<');
    let item_in_std = is_std(&item_span);
    let in_std = |s: &Span| is_std(s) && !item_in_std;
    let in_item =
        |s: &Span| s.file == item_span.file && item_span.start <= s.start && s.end <= item_span.end;
    let failed = if matches!(item, Item::Test(..)) { codes::E0706 } else { codes::E0704 };
    let (code, verb, detail) = match (fault.trap, &fault.panic) {
        (Some(wasmtime::Trap::OutOfFuel), _) => {
            let limit = match item {
                Item::Test(..) => "a test's",
                Item::Const(c, _) if p.const_(c).fuel.is_some() => "its",
                Item::Const(..) => "the build's",
            };
            (codes::E0705, format!("ran past {limit} fuel limit"), None)
        }
        (_, Some(msg)) => (failed, "panicked".to_string(), Some(msg.clone())),
        (Some(t), None) => (failed, "trapped".to_string(), Some(trap_words(t))),
        (None, None) => (failed, "failed".to_string(), Some(fault.text.clone())),
    };
    let phase = phase.map_or(String::new(), |ph| format!(" {ph}"));
    let detail = detail.map_or(String::new(), |d| format!(": {d}"));
    // The error is where it happened in the program's own code: the innermost frame outside
    // std, or the item itself.
    let primary = chain.iter().copied().find(|s| !in_std(s)).unwrap_or(item_span);
    let mut d = Diagnostic::new(code, primary, format!("{} {verb}{phase}{detail}", item.what(p)));
    for s in chain.iter().filter(|s| !in_std(s) && **s != primary) {
        let label = if in_item(s) { item.within(p) } else { "called from here".to_string() };
        d = d.with_secondary(*s, label);
    }
    if !in_item(&primary) && !chain.iter().any(in_item) {
        d = d.with_secondary(item_span, item.within(p));
    }
    let names: Vec<String> = chain.iter().rev().filter_map(|&s| enclosing_fn(checked, s)).fold(
        Vec::new(),
        |mut v, n| {
            if v.last() != Some(&n) {
                v.push(n);
            }
            v
        },
    );
    if names.len() > 1 {
        d = d.with_note(format!("the call chain: {}", names.join(" → ")));
    }
    if code == codes::E0705 {
        d = d
            .with_note(match item {
                Item::Const(c, fuel) if p.const_(c).fuel.is_some() => format!(
                    "its `@fuel` stops it after about {} WASM instructions, its threads' together (§10)",
                    instructions(fuel)
                ),
                Item::Const(..) => format!(
                    "the build stops a constant after about {} WASM instructions, so a computation that doesn't end is an error rather than a hang (§10)",
                    instructions(FUEL)
                ),
                Item::Test(_, fuel) => format!(
                    "`wrela test` stops a test after about {} WASM instructions, so a test that doesn't end fails rather than hangs (§10)",
                    instructions(fuel)
                ),
            })
            .with_help(match item {
                Item::Const(c, _) if p.const_(c).fuel.is_some() => {
                    "check that its loops end, or give it more `@fuel`"
                }
                Item::Const(..) => {
                    "check that its loops end, give it more with `@fuel(n)`, or compute the value at load instead"
                }
                Item::Test(..) => "check that its loops end, or test less at once",
            });
    } else if code == codes::E0706 {
        let framed = matches!(item, Item::Test(f, _) if p.func(f).attrs.test_run.is_some());
        d = d.with_note(if framed {
            "a frame test runs `init` and the program's frames (or a tick test its ticks), then the test: it passes unless one of them panics, and a failed `assert` panics (§10)"
        } else {
            "a test passes unless it panics, and a failed `assert` panics (§10)"
        });
    } else {
        d = d.with_note(
            "the build runs the program's own code to compute a constant, so a panic or a failed `assert` in it fails the build (§10)",
        );
    }
    d
}

/// What a trap means in the program's terms (§11's table).
fn trap_words(t: wasmtime::Trap) -> String {
    use wasmtime::Trap as T;
    match t {
        // The checks the compiler emits all end in `unreachable`.
        T::UnreachableCodeReached => {
            "an index out of range, an integer overflow, or a value too large for the stack"
                .to_string()
        }
        T::IntegerDivisionByZero => "an integer division by zero".to_string(),
        T::IntegerOverflow => "an integer overflow (`MIN / -1`)".to_string(),
        T::BadConversionToInteger => "a float out of range for the integer type".to_string(),
        T::StackOverflow => "the stack overflowed".to_string(),
        other => format!("{other}"),
    }
}

/// The name of the function whose source holds `s`: the innermost one, for diagnostics.
fn enclosing_fn(checked: &Checked, s: Span) -> Option<String> {
    let p = &checked.program;
    let mut best: Option<(u32, String)> = None;
    for (i, f) in p.fns.iter().enumerate() {
        let fs = f.span;
        if fs.file == s.file && fs.start <= s.start && s.end <= fs.end {
            let size = fs.end - fs.start;
            if best.as_ref().is_none_or(|(b, _)| size < *b) {
                let id = wrela_sema::ty::FnId(i as u32);
                let name = match f.owner {
                    wrela_sema::defs::FnOwner::Const(c) if p.const_(c).default => {
                        p.const_(c).short()
                    }
                    wrela_sema::defs::FnOwner::Const(_) => format!("const {}", f.name),
                    _ => p.fn_display_name(id),
                };
                best = Some((size, name));
            }
        }
    }
    best.map(|(_, n)| n)
}
