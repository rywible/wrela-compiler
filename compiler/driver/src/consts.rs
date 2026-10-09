//! Build-time constants (language.md §10): the build runs the program's own code to compute
//! each constant whose value isn't literal. Lowering makes a module that computes them
//! (`wrela_lower::lower_consts`); this runs it in wasmtime, with a fuel limit, reads each value
//! back, and makes a panic, a trap or a computation that runs away a compile error with the
//! call chain.
//!
//! Constants are computed in rounds: those whose code reads no constant still to be computed
//! first, in one module, then those that read them, and so on. Constants that read each other
//! are an error (E0328).

use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use wrela_diag::{Diagnostic, SourceMap, Span, codes};
use wrela_lower::{BuildData, ConstLowering, ConstModule, Embeds, Memory};
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

/// Computes the program's computed constants into `data` (whose files `embed` reads are
/// already there): the errors computing them.
pub fn compute(checked: &Checked, sources: &SourceMap, data: &mut BuildData) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut pending: Vec<ConstId> = (0..checked.program.consts.len())
        .map(|i| ConstId(i as u32))
        .filter(|&c| wrela_lower::is_computed(checked, c))
        .collect();
    let mut failed = BTreeSet::new();
    while !pending.is_empty() {
        let (lowering, d) = wrela_lower::lower_consts(checked, &pending, data);
        let (module, d, rest) = match lowering {
            ConstLowering::Ready(m) => (m, d, Vec::new()),
            ConstLowering::Needs(_) => {
                // Which can be computed now: those that need no constant still to compute.
                let mut ready = Vec::new();
                let mut rest = Vec::new();
                for &c in &pending {
                    match wrela_lower::lower_consts(checked, &[c], data).0 {
                        ConstLowering::Ready(_) => ready.push(c),
                        // One that needs a constant the build couldn't compute: that one's error
                        // says why.
                        ConstLowering::Needs(n) if n.iter().any(|x| failed.contains(x)) => {
                            failed.insert(c);
                        }
                        ConstLowering::Needs(n) => rest.push((c, n)),
                    }
                }
                if ready.is_empty() {
                    if let Some(d) = cycle(checked, &rest) {
                        diags.push(d);
                    }
                    break;
                }
                let (lowering, d) = wrela_lower::lower_consts(checked, &ready, data);
                let ConstLowering::Ready(m) = lowering else {
                    diags.push(Diagnostic::internal("a constant ready to compute needs another"));
                    break;
                };
                (m, d, rest.into_iter().map(|(c, _)| c).collect())
            }
        };
        let errors = wrela_diag::has_errors(&d);
        diags.extend(d);
        if errors {
            break;
        }
        for (c, result) in run(engine(), checked, sources, &module, !data.no_simd) {
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
        pending = rest;
    }
    diags
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

/// A module of the build's code, running in wasmtime on memory of its own, with the host's
/// functions refusing: what computes constants and runs tests.
struct Running {
    store: wasmtime::Store<()>,
    instance: wasmtime::Instance,
    memory: wasmtime::Memory,
}

/// The build's code, compiled for wasmtime: started once to compute constants, and once for
/// each test.
struct Compiled {
    module: wasmtime::Module,
    /// Where each WASM offset is in the source, for call chains.
    lines: Vec<(u32, Option<Span>)>,
}

fn compile(
    engine: &wasmtime::Engine,
    m: &wrela_ir::Module,
    simd: bool,
) -> Result<Compiled, String> {
    // Its own memory, unshared: it runs on this one thread.
    let options = wrela_wasm::Options { shared_memory: false, simd };
    let out = wrela_wasm::emit_with(m, options)
        .map_err(|e| format!("the WASM back end failed on the build's code: {e}"))?;
    let module = wasmtime::Module::new(engine, &out.wasm)
        .map_err(|e| format!("wasmtime refused the build's code: {e:#}"))?;
    Ok(Compiled { module, lines: out.lines })
}

impl Compiled {
    /// An instance on new memory.
    fn start(&self, engine: &wasmtime::Engine) -> Result<Running, String> {
        let mut linker = wasmtime::Linker::new(engine);
        // Effects keep constants and tests from recording GPU work and making requests (§8):
        // nothing calls these. Only a program that starts a voice imports `audio`, and they
        // can't.
        let host = wrela_abi::HOST_FUNCTIONS.iter().filter(|f| f.name != wrela_abi::IMPORT_AUDIO);
        for f in host {
            let why = if f.results == 0 {
                "build-time code can't call the host"
            } else {
                "build-time code can't make requests"
            };
            let i32s = |n| std::iter::repeat_n(wasmtime::ValType::I32, n);
            let ty = wasmtime::FuncType::new(engine, i32s(f.params), i32s(f.results));
            linker
                .func_new(wrela_abi::IMPORT_MODULE, f.name, ty, move |_, _, _| {
                    Err(wasmtime::format_err!("{why}"))
                })
                .map_err(|e| format!("{e:#}"))?;
        }
        let mut store = wasmtime::Store::new(engine, ());
        store.set_fuel(FUEL).map_err(|e| format!("{e:#}"))?;
        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|e| format!("the build's code didn't start: {e:#}"))?;
        let memory = instance
            .get_memory(&mut store, wrela_abi::EXPORT_MEMORY)
            .ok_or_else(|| "the build's code has no memory".to_string())?;
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
            let panic = take_panic_message(self.memory.data_mut(&mut self.store));
            CallError::Failed(e, panic)
        })
    }
}

enum CallError {
    Internal(wasmtime::Error),
    Failed(wasmtime::Error, Option<String>),
}

fn run(
    engine: &wasmtime::Engine,
    checked: &Checked,
    sources: &SourceMap,
    m: &ConstModule,
    simd: bool,
) -> Vec<(ConstId, Result<wrela_lower::Value, Diagnostic>)> {
    let started = compile(engine, &m.module, simd).and_then(|c| Ok((c.start(engine)?, c)));
    let (mut running, compiled) = match started {
        Ok(r) => r,
        Err(msg) => {
            return m
                .exports
                .iter()
                .map(|&(c, _)| (c, Err(Diagnostic::internal(msg.clone()))))
                .collect();
        }
    };
    let mut results = Vec::new();
    for (c, name) in &m.exports {
        let result = match running.call(name, FUEL) {
            Ok(addr) => {
                let mem = Memory { bytes: running.memory.data(&running.store) };
                wrela_lower::read_value(checked, *c, &mem, addr).map_err(|e| {
                    Diagnostic::internal(format!(
                        "reading the value of `{}`: {e}",
                        checked.program.const_(*c).name
                    ))
                })
            }
            Err(CallError::Internal(e)) => Err(Diagnostic::internal(format!("{e:#}"))),
            Err(CallError::Failed(e, panic)) => {
                let fault = Fault::of(&e, panic);
                Err(failure(checked, sources, &compiled.lines, Item::Const(*c), &fault, None))
            }
        };
        results.push((*c, result));
    }
    results
}

/// Runs the program package's `@test` functions (§10), after its constants are computed into
/// `data`: each test's name and, if it failed, why (E0706, E0705), with the errors lowering
/// them found. Each test runs on new memory, so one test can't change what another sees.
pub fn run_tests(
    checked: &Checked,
    sources: &SourceMap,
    data: &BuildData,
    tests: &[wrela_sema::ty::FnId],
    fuel: u64,
) -> (Vec<(wrela_sema::ty::FnId, Option<Diagnostic>)>, Vec<Diagnostic>) {
    let (module, diags) = wrela_lower::lower_tests(checked, tests, data);
    let Some(module) = module.filter(|_| !wrela_diag::has_errors(&diags)) else {
        return (Vec::new(), diags);
    };
    let compiled = match compile(engine(), &module.module, !data.no_simd) {
        Ok(c) => c,
        Err(msg) => return (Vec::new(), vec![Diagnostic::internal(msg)]),
    };
    let names: Vec<&str> = module.exports.iter().map(|(_, name)| name.as_str()).collect();
    let ran = each_at_once(&names, |name| compiled.start(engine()).map(|mut r| r.call(name, fuel)));
    let results = module
        .exports
        .iter()
        .zip(ran)
        .map(|((f, _), ran)| {
            let failed = match ran {
                Ok(Ok(_)) => None,
                Err(msg) => Some(Diagnostic::internal(msg)),
                Ok(Err(CallError::Internal(e))) => Some(Diagnostic::internal(format!("{e:#}"))),
                Ok(Err(CallError::Failed(e, panic))) => {
                    let fault = Fault::of(&e, panic);
                    let item = Item::Test(*f, fuel);
                    Some(failure(checked, sources, &compiled.lines, item, &fault, None))
                }
            };
            (*f, failed)
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

/// What failed: computing a constant, or a test (with the fuel it ran with).
#[derive(Clone, Copy)]
pub(crate) enum Item {
    Const(ConstId),
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
            Item::Const(c) => p.const_(c).span,
            Item::Test(f, _) => p.func(f).span,
        }
    }

    /// "computing the constant `X`", "the test `t`".
    fn what(self, p: &wrela_sema::program::Program) -> String {
        match self {
            Item::Const(c) => format!("computing {}", p.const_(c).shown()),
            Item::Test(f, _) => format!("the test `{}`", p.func(f).name),
        }
    }

    /// The label where the code that failed was called from the item.
    fn within(self, p: &wrela_sema::program::Program) -> String {
        match self {
            Item::Const(c) => format!("while the build computes {}", p.const_(c).short()),
            Item::Test(f, _) => format!("in the test `{}`", p.func(f).name),
        }
    }
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
            let limit = if matches!(item, Item::Test(..)) { "a test's" } else { "the build's" };
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
                Item::Const(_) => format!(
                    "the build stops a constant after about {} WASM instructions, so a computation that doesn't end is an error rather than a hang (§10)",
                    instructions(FUEL)
                ),
                Item::Test(_, fuel) => format!(
                    "`wrela test` stops a test after about {} WASM instructions, so a test that doesn't end fails rather than hangs (§10)",
                    instructions(fuel)
                ),
            })
            .with_help(match item {
                Item::Const(_) => "check that its loops end, or compute the value at load instead",
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
