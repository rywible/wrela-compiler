//! The CPU side: a program's WASM under wasmtime, its one import, and the batches it submits.
//!
//! [`Program`] is generic over the [`Executor`] that carries out decoded commands, so the ABI
//! rules (imports, exports, bounds, decoding, sequencing, the state hash) are tested here
//! without a GPU; [`crate::gpu::Gpu`] is the real executor.

use crate::error::{Error, Result};
use crate::shared;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use wasmtime::SharedMemory;
use wasmtime::{
    Engine, ExternType, Func, FuncType, Instance, Linker, Module, Store, TypedFunc, Val, ValType,
};
use wrela_abi::check::{Checker, CommandError};
use wrela_abi::hash::StateHash;
use wrela_abi::input::{EVENT_SIZE, Event, Queue};
use wrela_abi::lines::{self, Lines};
use wrela_abi::memory::MAX_WORKERS;
use wrela_abi::stream::{self, Command, Sequencer};
use wrela_abi::{
    AUDIO_QUANTUM, EXPORT_AUDIO, EXPORT_FRAME, EXPORT_INIT, EXPORT_MEMORY, EXPORT_WORKER,
    HOST_FUNCTIONS, IMPORT_AUDIO, IMPORT_INPUT, IMPORT_LIMIT, IMPORT_MEMORY, IMPORT_MODULE,
    IMPORT_REQUEST_STATUS, IMPORT_REQUEST_TAKE, IMPORT_SUBMIT, Manifest, REQUEST_FAILED,
    REQUEST_PENDING,
};

/// The process's one wasmtime engine: every program compiles and runs under it, as wasmtime
/// intends (an engine is costly to make, and cheap to share).
static ENGINE: LazyLock<Engine> = LazyLock::new(|| engine(false));

/// [`ENGINE`], counting fuel: for running a program's frames in a test, where a call that
/// does too much work fails rather than hangs, after the same work on every machine.
static METERED: LazyLock<Engine> = LazyLock::new(|| engine(true));

fn engine(fuel: bool) -> Engine {
    let mut config = wasmtime::Config::new();
    // A program's memory is shared with its workers (wrela_abi::memory). Wasmtime's threads are
    // a tier 2 feature: no security updates for old releases.
    config.wasm_threads(true).shared_memory(true).consume_fuel(fuel);
    Engine::new(&config).expect("wasmtime's configuration is valid")
}

/// Why the program's last call failed, for a caller that reports it against the source: the
/// WASM offset of each frame, innermost first; the trap, if it was one; and the message a
/// panic left.
#[derive(Clone, Debug)]
pub struct Failure {
    pub frames: Vec<u32>,
    pub trap: Option<wasmtime::Trap>,
    pub panic: Option<String>,
}

/// Carries out commands that have already been decoded, sequenced and checked.
pub(crate) trait Executor: 'static {
    fn execute(&mut self, cmd: &Command<'_>) -> Result<()>;
    /// Called after each call of `frame` returns: finish (submit) the frame's work.
    fn end_frame(&mut self) -> Result<()>;
    /// A buffer's bytes `offset..offset + size`, for a `ReadBuffer` request: the GPU's work
    /// recorded so far, done.
    fn read_back(&mut self, handle: u32, offset: u32, size: u32) -> Result<Vec<u8>>;
    /// The device's limits, which commands are checked against and `wrela.limit` gives.
    fn limits(&self) -> wrela_abi::Limits {
        wrela_abi::Limits::DEFAULT
    }
}

/// A numeric WASM value, for calling a program's other exports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl Value {
    fn to_val(self) -> Val {
        match self {
            Value::I32(v) => Val::I32(v),
            Value::I64(v) => Val::I64(v),
            Value::F32(v) => Val::F32(v.to_bits()),
            Value::F64(v) => Val::F64(v.to_bits()),
        }
    }

    fn from_val(v: &Val) -> Option<Value> {
        Some(match *v {
            Val::I32(v) => Value::I32(v),
            Val::I64(v) => Value::I64(v),
            Val::F32(bits) => Value::F32(f32::from_bits(bits)),
            Val::F64(bits) => Value::F64(f64::from_bits(bits)),
            _ => return None,
        })
    }

    fn ty(self) -> ValType {
        match self {
            Value::I32(_) => ValType::I32,
            Value::I64(_) => ValType::I64,
            Value::F32(_) => ValType::F32,
            Value::F64(_) => ValType::F64,
        }
    }
}

/// A WASM type's name, as in a signature: `i32`, `f64`, ... (`other` for a non-numeric type).
fn type_name(ty: ValType) -> &'static str {
    match ty {
        ValType::I32 => "i32",
        ValType::I64 => "i64",
        ValType::F32 => "f32",
        ValType::F64 => "f64",
        _ => "other",
    }
}

/// The store's data: what `wrela.submit` needs.
struct State<E> {
    /// The program's memory, which its workers share.
    memory: SharedMemory,
    checker: Checker,
    executor: E,
    hash: StateHash,
    sequencer: Sequencer,
    /// The typed error behind the most recent trap raised by `submit`, so the caller gets it
    /// rather than wasmtime's flattened message.
    failure: Option<Error>,
    /// Copies of the submitted batches, when recording.
    batches: Option<Vec<Vec<u8>>>,
    /// The first error the executor gave: what it holds isn't known after it, so every later
    /// command fails. (A command the sequencer or the checker rejects leaves no trace.)
    failed: Option<String>,
    requests: Requests,
    /// Whether the instance has started: its start function, which runs as it's
    /// instantiated, may not call the host.
    started: bool,
    /// The voice `wrela.audio` started: its task and context.
    voice: Option<(u32, u32)>,
    /// The lines the program printed (`std::io::print`), each also shown on stderr unless quiet.
    logs: Vec<String>,
    /// Input events not yet read (`wrela.input`).
    input: Queue,
}

/// What a start function that calls the host gets.
const CALLED_WHILE_STARTING: &str =
    "its start function called the host; a wrela program calls the host only from an export";

/// Where a program's requests read and write (language.md §6.15): its build, for `Fetch`, its
/// storage directory, for `StorageRead` and `StorageWrite`, and what answers its `Post`s.
#[derive(Clone, Debug, Default)]
pub(crate) struct Io {
    pub build: Option<PathBuf>,
    pub storage: Option<PathBuf>,
    pub post: Option<PostHandler>,
    /// Don't show printed lines on stderr: the caller shows them ([`crate::Options::quiet`]).
    pub quiet: bool,
}

/// A post's answer, from the URL and the body.
type Answer = dyn Fn(&str, &[u8]) -> std::result::Result<Vec<u8>, String> + Send + Sync;

/// What answers a program's posts (`std::io::post`): given the URL (relative to the build) and
/// the body, the reply's bytes, or why there's none. The host that serves the build answers
/// them; a native host has none unless its user gives one.
#[derive(Clone)]
pub struct PostHandler(pub Arc<Answer>);

impl PostHandler {
    pub fn new(
        f: impl Fn(&str, &[u8]) -> std::result::Result<Vec<u8>, String> + Send + Sync + 'static,
    ) -> PostHandler {
        PostHandler(Arc::new(f))
    }
}

impl std::fmt::Debug for PostHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PostHandler")
    }
}

/// The program's requests: answered ones it hasn't taken, and ones answered during the current
/// call, which it sees from the next call on.
#[derive(Default)]
struct Requests {
    io: Io,
    answered: HashMap<u32, std::result::Result<Vec<u8>, String>>,
    arriving: Vec<(u32, std::result::Result<Vec<u8>, String>)>,
}

impl Requests {
    /// A request's number, checked to be free.
    fn start(&self, request: u32, op: stream::Opcode) -> Result<()> {
        if self.answered.contains_key(&request) || self.arriving.iter().any(|a| a.0 == request) {
            return Err(Error::Command(CommandError {
                opcode: op,
                why: format!("request {request} is already in use"),
            }));
        }
        Ok(())
    }

    /// The file a storage path or a URL names, under `dir`.
    fn file(dir: &Option<PathBuf>, path: &str, what: &str) -> std::result::Result<PathBuf, String> {
        match dir {
            Some(d) => Ok(d.join(path)),
            None => Err(format!("this host has no {what}")),
        }
    }

    /// Carries out an IO request now; its answer arrives at the end of the call.
    fn io(&mut self, cmd: &Command<'_>) -> Result<()> {
        let (request, answer) = match cmd {
            Command::StorageRead { request, path } => {
                self.start(*request, cmd.opcode())?;
                let a = Self::file(&self.io.storage, path, "storage")
                    .and_then(|f| std::fs::read(f).map_err(|e| e.to_string()));
                (*request, a)
            }
            Command::StorageWrite { request, path, data } => {
                self.start(*request, cmd.opcode())?;
                let a = Self::file(&self.io.storage, path, "storage").and_then(|f| {
                    if let Some(parent) = f.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    std::fs::write(f, data).map(|()| Vec::new()).map_err(|e| e.to_string())
                });
                (*request, a)
            }
            Command::Fetch { request, url } => {
                self.start(*request, cmd.opcode())?;
                let a = Self::file(&self.io.build, url, "build to fetch from")
                    .and_then(|f| std::fs::read(f).map_err(|e| e.to_string()));
                (*request, a)
            }
            Command::Post { request, url, body } => {
                self.start(*request, cmd.opcode())?;
                let a = match &self.io.post {
                    Some(h) => (h.0)(url, body),
                    None => Err("this host has nothing that answers posts".to_string()),
                };
                (*request, a)
            }
            _ => return Ok(()),
        };
        self.arriving.push((request, answer));
        Ok(())
    }

    /// The end of a call: what arrived during it is answered now.
    fn settle(&mut self) {
        for (r, a) in self.arriving.drain(..) {
            self.answered.insert(r, a);
        }
    }
}

impl<E: Executor> State<E> {
    fn submit(&mut self, batch: &[u8]) -> Result<()> {
        // The state hash covers every submitted byte, valid or not.
        self.hash.update(batch);
        if let Some(b) = &mut self.batches {
            b.push(batch.to_vec());
        }
        for cmd in stream::decode(batch)? {
            if let Some(first) = &self.failed {
                return Err(Error::Command(CommandError {
                    opcode: cmd.opcode(),
                    why: format!("the host failed to carry out an earlier command ({first})"),
                }));
            }
            let before = self.sequencer.clone();
            self.sequencer.step(&cmd)?;
            if let Err(e) = self.checker.check(&cmd) {
                self.sequencer = before;
                return Err(e.into());
            }
            if let Err(e) = self.executor.execute(&cmd) {
                self.failed = Some(e.to_string());
                return Err(e);
            }
            match &cmd {
                Command::ReadBuffer { request, handle, offset, size } => {
                    self.requests.start(*request, cmd.opcode())?;
                    let answer =
                        self.executor.read_back(*handle, *offset, *size).map_err(|e| e.to_string());
                    self.requests.arriving.push((*request, answer));
                }
                Command::StorageRead { .. }
                | Command::StorageWrite { .. }
                | Command::Fetch { .. }
                | Command::Post { .. } => self.requests.io(&cmd)?,
                Command::Log { text } => {
                    if !self.requests.io.quiet {
                        eprintln!("wrela: {text}");
                    }
                    self.logs.push(text.to_string());
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// The audio thread's instance of the program, and its `__audio(task, context)`.
type AudioThread = (Store<()>, TypedFunc<(u32, u32), ()>);

pub(crate) struct Program<E: 'static> {
    store: Store<State<E>>,
    /// The worker threads, which return when the program is dropped.
    threads: Vec<std::thread::JoinHandle<()>>,
    memory: SharedMemory,
    module: Module,
    /// The audio thread's instance, once [`Program::render_audio`] has made it.
    audio: Option<AudioThread>,
    instance: Instance,
    frame: TypedFunc<(f32, u32, u32), ()>,
    /// Where the code came from, for trap messages (`wrela_abi::lines`).
    lines: Option<Arc<Lines>>,
    /// The fuel each call gets, on a metered engine ([`METERED`]).
    fuel: Option<u64>,
    /// Why the last call failed, if it did.
    last_failure: Option<Failure>,
}

/// Checks a module against the program ABI before instantiating it: it imports its memory,
/// shared, as `wrela.memory`, may import only the host's functions besides, and must export
/// `memory` and `frame(f32, i32, i32)`. The memory's type.
pub(crate) fn check_abi(module: &Module) -> Result<wasmtime::MemoryType> {
    let mut memory = None;
    for import in module.imports() {
        let (m, n) = (import.module(), import.name());
        if (m, n) == (IMPORT_MODULE, IMPORT_MEMORY) {
            match import.ty() {
                ExternType::Memory(t) if t.is_shared() => memory = Some(t),
                ty => {
                    return Err(Error::Program(format!(
                        "its import `{m}.{n}` must be a shared memory, not {}",
                        describe(&ty)
                    )));
                }
            }
            continue;
        }
        let want = HOST_FUNCTIONS.iter().find(|f| m == IMPORT_MODULE && f.name == n);
        let Some(want) = want else {
            let all: Vec<String> = std::iter::once(IMPORT_MEMORY)
                .chain(HOST_FUNCTIONS.iter().map(|f| f.name))
                .map(|name| format!("`{IMPORT_MODULE}.{name}`"))
                .collect();
            return Err(Error::Program(format!(
                "it imports `{m}.{n}`, which isn't one of the host's: {}",
                all.join(", ")
            )));
        };
        // Its type as both hosts word it.
        let shown = want.signature();
        if !matches!(import.ty(), ExternType::Func(ref f) if signature(f) == shown) {
            return Err(Error::Program(format!(
                "its import `{m}.{n}` must be a function {shown}, not {}",
                describe(&import.ty())
            )));
        }
    }
    let Some(memory) = memory else {
        return Err(Error::Program(format!(
            "it doesn't import its memory, shared, as `{IMPORT_MODULE}.{IMPORT_MEMORY}`"
        )));
    };
    if !matches!(module.get_export(EXPORT_MEMORY), Some(ExternType::Memory(_))) {
        return Err(Error::Program(format!("it doesn't export its memory as `{EXPORT_MEMORY}`")));
    }
    if !module.get_export(EXPORT_FRAME).is_some_and(|t| is_func(&t, &["f32", "i32", "i32"])) {
        return Err(Error::Program(format!(
            "it must export `{EXPORT_FRAME}(time: f32, width: i32, height: i32)` with no results"
        )));
    }
    if module.get_export(EXPORT_INIT).is_some_and(|t| !is_func(&t, &[])) {
        return Err(Error::Program(format!(
            "its `{EXPORT_INIT}` export must be a function with no parameters and no results"
        )));
    }
    Ok(memory)
}

/// Whether `ty` is a function with these parameter types (by [`type_name`]) and no results.
fn is_func(ty: &ExternType, params: &[&str]) -> bool {
    let ExternType::Func(f) = ty else { return false };
    f.params().map(type_name).eq(params.iter().copied()) && f.results().len() == 0
}

/// A function's type, as both hosts word it: `(i32, i32) -> ()`.
fn signature(f: &FuncType) -> String {
    let list = |types: &mut dyn Iterator<Item = ValType>| {
        types.map(|t| t.to_string()).collect::<Vec<_>>().join(", ")
    };
    format!("({}) -> ({})", list(&mut f.params()), list(&mut f.results()))
}

/// What an import is, as both hosts word it: `a function (i32) -> ()`, `a memory`, ...
fn describe(ty: &ExternType) -> String {
    match ty {
        ExternType::Func(f) => format!("a function {}", signature(f)),
        ExternType::Global(_) => "a global".into(),
        ExternType::Table(_) => "a table".into(),
        ExternType::Memory(_) => "a memory".into(),
        ExternType::Tag(_) => "a tag".into(),
    }
}

/// A module that satisfies the program ABI, ready to instantiate (any number of times).
pub(crate) struct Compiled {
    module: Module,
    /// The fuel each call gets, when compiled for [`METERED`].
    fuel: Option<u64>,
    /// Its memory's type: shared.
    memory: wasmtime::MemoryType,
    lines: Option<Arc<Lines>>,
}

/// Compiles a program and checks it against the ABI ([`check_abi`]); with `fuel`, for the
/// metered engine, where each call gets that much fuel.
pub(crate) fn compile_with(wasm: &[u8], fuel: Option<u64>) -> Result<Compiled> {
    let engine = if fuel.is_some() { &*METERED } else { &*ENGINE };
    let module = Module::new(engine, wasm).map_err(|e| Error::Program(format!("{e:#}")))?;
    compiled(module, wasm, fuel)
}

/// [`compile_with`] with no fuel, the compiled code kept beside the build in `dir`
/// (`crate::cache`).
pub(crate) fn compile_cached(wasm: &[u8], dir: &std::path::Path) -> Result<Compiled> {
    compiled(crate::cache::module(&ENGINE, wasm, dir)?, wasm, None)
}

fn compiled(module: Module, wasm: &[u8], fuel: Option<u64>) -> Result<Compiled> {
    let memory = check_abi(&module)?;
    let lines = match lines::custom_section(wasm, lines::SECTION) {
        Some(payload) => Some(Arc::new(Lines::decode(payload).map_err(|e| {
            Error::Program(format!("its `{}` section is malformed: {e}", lines::SECTION))
        })?)),
        None => None,
    };
    Ok(Compiled { module, fuel, memory, lines })
}

impl<E: Executor> Program<E> {
    /// Instantiates a compiled program whose commands are checked against `manifest` and
    /// carried out by `executor`.
    /// `workers` is how many threads run a parallel job's chunks, this one included: 1 runs
    /// them all here.
    #[cfg(any(test, feature = "gpu"))]
    pub(crate) fn instantiate(
        compiled: &Compiled,
        manifest: &Manifest,
        executor: E,
        io: Io,
        record: bool,
        workers: u32,
    ) -> Result<Program<E>> {
        Program::instantiate_with(compiled, manifest, executor, io, record, workers, true)
    }

    /// [`Program::instantiate`], running `init` only with `run_init`: otherwise the caller
    /// runs it ([`Program::run_init`]), and learns why it failed if it does.
    pub(crate) fn instantiate_with(
        compiled: &Compiled,
        manifest: &Manifest,
        executor: E,
        io: Io,
        record: bool,
        workers: u32,
        run_init: bool,
    ) -> Result<Program<E>> {
        let engine = compiled.module.engine();
        let mut linker = Linker::new(engine);
        let wrap = |e: wasmtime::Error| Error::Program(format!("{e:#}"));
        let memory = SharedMemory::new(engine, compiled.memory.clone()).map_err(wrap)?;
        linker.func_wrap(IMPORT_MODULE, IMPORT_SUBMIT, submit::<E>).map_err(wrap)?;
        linker
            .func_wrap(IMPORT_MODULE, IMPORT_REQUEST_STATUS, request_status::<E>)
            .map_err(wrap)?;
        linker.func_wrap(IMPORT_MODULE, IMPORT_REQUEST_TAKE, request_take::<E>).map_err(wrap)?;
        linker.func_wrap(IMPORT_MODULE, IMPORT_LIMIT, limit::<E>).map_err(wrap)?;
        linker.func_wrap(IMPORT_MODULE, IMPORT_AUDIO, audio::<E>).map_err(wrap)?;
        linker.func_wrap(IMPORT_MODULE, IMPORT_INPUT, input::<E>).map_err(wrap)?;
        let state = State {
            memory: memory.clone(),
            checker: Checker::with_limits(manifest, executor.limits()),
            executor,
            hash: StateHash::new(),
            sequencer: Sequencer::new(),
            failure: None,
            // From the start, so `init`'s batches are kept too.
            batches: record.then(Vec::new),
            failed: None,
            requests: Requests { io, ..Requests::default() },
            started: false,
            voice: None,
            logs: Vec::new(),
            input: Queue::default(),
        };
        let mut store = Store::new(engine, state);
        if let Some(fuel) = compiled.fuel {
            store.set_fuel(fuel).map_err(wrap)?;
        }
        linker.define(&store, IMPORT_MODULE, IMPORT_MEMORY, memory.clone()).map_err(wrap)?;
        let instance = linker.instantiate(&mut store, &compiled.module).map_err(|e| {
            let failure = store.data_mut().failure.take();
            failure.unwrap_or_else(|| Error::Program(format!("{e:#}")))
        })?;
        store.data_mut().started = true;
        // The workers: each its own instance on its own thread, sharing the memory.
        let has_worker = compiled.module.get_export(EXPORT_WORKER).is_some();
        let n = if has_worker { workers.clamp(1, MAX_WORKERS + 1) - 1 } else { 0 };
        let threads =
            (0..n).map(|i| spawn_worker(compiled.module.clone(), memory.clone(), i)).collect();
        let frame = instance
            .get_typed_func::<(f32, u32, u32), ()>(&mut store, EXPORT_FRAME)
            .map_err(|e| Error::Program(format!("{e:#}")))?;
        let mut program = Program {
            store,
            instance,
            frame,
            lines: compiled.lines.clone(),
            threads,
            memory,
            module: compiled.module.clone(),
            audio: None,
            fuel: compiled.fuel,
            last_failure: None,
        };
        if run_init {
            program.init()?;
        }
        Ok(program)
    }

    /// Runs `init`, for a program [`Program::instantiate_with`] made without running it.
    pub(crate) fn run_init(&mut self) -> Result<()> {
        self.init()
    }

    /// A program with state makes it once, before anything else runs (language.md §12).
    fn init(&mut self) -> Result<()> {
        let Ok(init) = self.instance.get_typed_func::<(), ()>(&mut self.store, EXPORT_INIT) else {
            return Ok(());
        };
        self.refuel()?;
        let result = init.call(&mut self.store, ());
        self.settle(result)
    }

    /// A metered program's fuel, filled for the next call.
    fn refuel(&mut self) -> Result<()> {
        match self.fuel {
            Some(fuel) => self.store.set_fuel(fuel).map_err(|e| Error::Program(format!("{e:#}"))),
            None => Ok(()),
        }
    }

    /// Why the last call failed, if it did: for reporting it against the source.
    pub(crate) fn last_failure(&self) -> Option<&Failure> {
        self.last_failure.as_ref()
    }

    /// Calls `frame(time, width, height)`, then ends the frame.
    pub(crate) fn frame(&mut self, time: f32, width: u32, height: u32) -> Result<()> {
        self.refuel()?;
        let result = self.frame.call(&mut self.store, (time, width, height));
        self.settle(result)?;
        let state = self.store.data_mut();
        state.sequencer.end_frame()?;
        state.executor.end_frame()
    }

    /// Calls another export with numeric arguments.
    pub(crate) fn call(&mut self, name: &str, args: &[Value]) -> Result<Vec<Value>> {
        let func: Func = self
            .instance
            .get_func(&mut self.store, name)
            .ok_or_else(|| Error::Program(format!("it has no exported function `{name}`")))?;
        let ty = func.ty(&self.store);
        let arg_types = || args.iter().map(|a| type_name(a.ty()));
        if !ty.params().map(type_name).eq(arg_types()) {
            let given: Vec<&str> = arg_types().collect();
            return Err(Error::Program(format!(
                "`{name}` has type {ty}; it can't take ({})",
                given.join(", ")
            )));
        }
        let args: Vec<Val> = args.iter().map(|a| a.to_val()).collect();
        let mut results: Vec<Val> = ty.results().map(|_| Val::I32(0)).collect();
        self.refuel()?;
        let result = func.call(&mut self.store, &args, &mut results);
        self.settle(result)?;
        results
            .iter()
            .map(|v| {
                Value::from_val(v)
                    .ok_or_else(|| Error::Program(format!("`{name}` returns a non-numeric value")))
            })
            .collect()
    }

    /// The exported functions' names and WASM signatures.
    pub(crate) fn exports(&self) -> Vec<(String, Vec<&'static str>, Vec<&'static str>)> {
        self.instance
            .module(&self.store)
            .exports()
            .filter_map(|e| match e.ty() {
                ExternType::Func(f) => Some((
                    e.name().to_string(),
                    f.params().map(type_name).collect(),
                    f.results().map(type_name).collect(),
                )),
                _ => None,
            })
            .collect()
    }

    /// Turns a call's outcome into ours, preferring the typed error `submit` recorded.
    fn settle(&mut self, result: wasmtime::Result<()>) -> Result<()> {
        // What the call's requests got, the program sees from its next call on.
        self.store.data_mut().requests.settle();
        self.last_failure = None;
        let failure = self.store.data_mut().failure.take();
        match (result, failure) {
            (Ok(()), _) => Ok(()),
            (Err(_), Some(failure)) => Err(failure),
            (Err(trap), None) => {
                let what = self.describe_trap(&trap);
                let panic = self.take_panic_message();
                let frames = trap
                    .downcast_ref::<wasmtime::WasmBacktrace>()
                    .map(|bt| {
                        bt.frames()
                            .iter()
                            .filter_map(|f| f.module_offset())
                            .map(|o| o as u32)
                            .collect()
                    })
                    .unwrap_or_default();
                let kind = trap.downcast_ref::<wasmtime::Trap>().copied();
                self.last_failure = Some(Failure { frames, trap: kind, panic: panic.clone() });
                Err(Error::Trap(match panic {
                    Some(msg) => format!("panic: {msg}: {what}"),
                    None => what,
                }))
            }
        }
    }

    /// The message a panic left before it trapped (`wrela_abi::memory::PANIC_MESSAGE`), cleared
    /// so a later trap that isn't a panic doesn't repeat it.
    fn take_panic_message(&mut self) -> Option<String> {
        use wrela_abi::memory::{PANIC_CAP, PANIC_MESSAGE, panic_message};
        let memory = &self.store.data().memory;
        let at = PANIC_MESSAGE as usize;
        let message = panic_message(&shared::read(memory, at, 4 + PANIC_CAP as usize)?)?;
        shared::write(memory, at, &0u32.to_le_bytes());
        Some(message)
    }

    /// What trapped, and where in the source if the program says: the location of the
    /// innermost frame that has one.
    fn describe_trap(&self, trap: &wasmtime::Error) -> String {
        let what = match trap.downcast_ref::<wasmtime::Trap>() {
            Some(t) => t.to_string(),
            None => format!("{trap:#}"),
        };
        let at = self.lines.as_ref().and_then(|lines| {
            let bt = trap.downcast_ref::<wasmtime::WasmBacktrace>()?;
            bt.frames().iter().find_map(|f| lines.at(f.module_offset()? as u32))
        });
        match at {
            Some(loc) => format!("{what} at {loc}"),
            None => what,
        }
    }

    /// FNV-1a 64 of every byte submitted since the program loaded.
    /// Renders `quanta` quanta of the program's voice ([`wrela_abi::AUDIO_QUANTUM`] samples
    /// each, at [`wrela_abi::AUDIO_SAMPLE_RATE`]) on an instance of its own, as the audio
    /// thread does, here and now: an offline run. The voice goes on where the last call
    /// stopped. Errors if the program hasn't started one.
    pub(crate) fn render_audio(&mut self, quanta: u32) -> Result<Vec<f32>> {
        let Some((task, context)) = self.store.data().voice else {
            return Err(Error::Program("it hasn't started a voice (`std::audio::play`)".into()));
        };
        let wrap = |e: wasmtime::Error| Error::Program(format!("{e:#}"));
        if self.audio.is_none() {
            let (mut store, instance) =
                other_instance(&self.module, &self.memory, "the audio thread").map_err(wrap)?;
            let render = instance
                .get_typed_func::<(u32, u32), ()>(&mut store, EXPORT_AUDIO)
                .map_err(wrap)?;
            self.audio = Some((store, render));
        }
        let (store, render) = self.audio.as_mut().expect("made above");
        let n = AUDIO_QUANTUM as usize;
        let mut out = Vec::with_capacity(n * quanta as usize);
        for _ in 0..quanta {
            render
                .call(&mut *store, (task, context))
                .map_err(|e| Error::Trap(format!("the voice trapped: {}", e.root_cause())))?;
            let bytes = shared::read(&self.memory, wrela_abi::memory::AUDIO_OUT as usize, n * 4)
                .ok_or_else(|| Error::Program("the samples are past the memory's end".into()))?;
            out.extend(bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
        }
        Ok(out)
    }

    /// How many chunks of parallel jobs the workers have run (`wrela_abi::memory::WORKERS_HELPED`).
    pub(crate) fn helped(&self) -> u32 {
        crate::shared::load_u32(&self.memory, wrela_abi::memory::WORKERS_HELPED)
    }

    pub(crate) fn hash(&self) -> StateHash {
        self.store.data().hash
    }

    /// Queues an input event: the program reads it (`wrela.input`) from its next call on.
    pub(crate) fn push_input(&mut self, e: Event) {
        self.store.data_mut().input.push(e);
    }

    /// The lines printed since the last call.
    pub(crate) fn take_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.store.data_mut().logs)
    }

    pub(crate) fn take_batches(&mut self) -> Vec<Vec<u8>> {
        self.store.data_mut().batches.as_mut().map(std::mem::take).unwrap_or_default()
    }

    #[cfg_attr(not(feature = "gpu"), expect(dead_code, reason = "only `Host` reaches its GPU"))]
    pub(crate) fn executor(&mut self) -> &mut E {
        &mut self.store.data_mut().executor
    }
}

/// A worker thread: an instance of the program, with the shared memory, running
/// `__worker(index)` until the program shuts it down. If it traps, it tells the thread waiting
/// for the job (wrela_abi::memory's workers).
/// Another thread's instance of the program, on the shared memory: a worker's or the audio
/// thread's. Its code records no GPU work and makes no requests (its effects are checked), so
/// the host's functions refuse to run, as `who`.
fn other_instance(
    module: &Module,
    memory: &SharedMemory,
    who: &'static str,
) -> wasmtime::Result<(Store<()>, Instance)> {
    let engine = module.engine();
    let mut store = Store::new(engine, ());
    // On a metered engine, another thread's instance isn't what a call's fuel bounds.
    if engine.get_consume_fuel() {
        store.set_fuel(u64::MAX)?;
    }
    let mut linker = Linker::new(engine);
    linker.define(&store, IMPORT_MODULE, IMPORT_MEMORY, memory.clone())?;
    for f in HOST_FUNCTIONS {
        let i32s = |n| std::iter::repeat_n(ValType::I32, n);
        let ty = FuncType::new(engine, i32s(f.params), i32s(f.results));
        linker.func_new(IMPORT_MODULE, f.name, ty, move |_, _, _| {
            Err(wasmtime::format_err!("{who} can't call the host"))
        })?;
    }
    let instance = linker.instantiate(&mut store, module)?;
    Ok((store, instance))
}

fn spawn_worker(module: Module, memory: SharedMemory, index: u32) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let run = || -> wasmtime::Result<()> {
            let (mut store, instance) = other_instance(&module, &memory, "a worker")?;
            let worker = instance.get_typed_func::<u32, ()>(&mut store, EXPORT_WORKER)?;
            worker.call(&mut store, index)
        };
        if run().is_err() {
            use wrela_abi::memory::{WORKERS_DONE, WORKERS_DONE_FAILED, WORKERS_FAILED};
            shared::store_u32(&memory, WORKERS_FAILED, 1);
            shared::or_u32(&memory, WORKERS_DONE, WORKERS_DONE_FAILED);
            let _ = memory.atomic_notify(u64::from(WORKERS_DONE), u32::MAX);
        }
    })
}

impl<E: 'static> Drop for Program<E> {
    /// Shuts the workers down: they see the flag when they wake.
    fn drop(&mut self) {
        use wrela_abi::memory::{WORKERS_GENERATION, WORKERS_SHUTDOWN};
        if self.threads.is_empty() {
            return;
        }
        shared::store_u32(&self.memory, WORKERS_SHUTDOWN, 1);
        shared::add_u32(&self.memory, WORKERS_GENERATION, 1);
        let _ = self.memory.atomic_notify(u64::from(WORKERS_GENERATION), u32::MAX);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// `wrela.submit(ptr, len)`: decode and execute one batch of `len` bytes at `ptr`.
fn submit<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    ptr: u32,
    len: u32,
) -> wasmtime::Result<()> {
    let state = caller.data_mut();
    if !state.started {
        state.failure = Some(Error::Program(CALLED_WHILE_STARTING.into()));
        return Err(wasmtime::format_err!("{CALLED_WHILE_STARTING}"));
    }
    // A handle of its own, so the batch is read in place while `state` changes.
    let memory = state.memory.clone();
    let result = match shared::slice(&memory, ptr as usize, len as usize) {
        Some(batch) => state.submit(batch),
        None => Err(Error::Trap(format!(
            "submit({ptr}, {len}) reaches past the end of the program's memory ({} bytes)",
            state.memory.data().len()
        ))),
    };
    result.map_err(|e| {
        let message = e.to_string();
        state.failure = Some(e);
        wasmtime::format_err!("{message}")
    })
}

/// `wrela.audio(task, context)`: starts the program's voice. The audio thread renders it
/// ([`Program::render_audio`]). A program has one.
fn audio<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    task: u32,
    context: u32,
) -> wasmtime::Result<()> {
    let state = caller.data_mut();
    if !state.started {
        state.failure = Some(Error::Program(CALLED_WHILE_STARTING.into()));
        return Err(wasmtime::format_err!("{CALLED_WHILE_STARTING}"));
    }
    if state.voice.is_some() {
        let e = Error::Trap("a program starts one voice, and this one started a second".into());
        let message = e.to_string();
        state.failure = Some(e);
        return Err(wasmtime::format_err!("{message}"));
    }
    state.voice = Some((task, context));
    Ok(())
}

/// `wrela.input(ptr, cap) -> count`: copies up to `cap` queued input events to `ptr`, oldest
/// first, and returns how many (wrela_abi `input`).
fn input<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    ptr: u32,
    cap: u32,
) -> wasmtime::Result<i32> {
    let state = caller.data_mut();
    if !state.started {
        state.failure = Some(Error::Program(CALLED_WHILE_STARTING.into()));
        return Err(wasmtime::format_err!("{CALLED_WHILE_STARTING}"));
    }
    let n = (cap as usize).min(state.input.len());
    let fits = (ptr as usize)
        .checked_add(n * EVENT_SIZE as usize)
        .is_some_and(|end| end <= state.memory.data().len());
    if !fits {
        return Err(wasmtime::format_err!(
            "input({ptr}, {cap}) reaches past the end of the program's memory"
        ));
    }
    let bytes = state.input.take(cap);
    shared::write(&state.memory, ptr as usize, &bytes);
    Ok(n as i32)
}

/// `wrela.limit(index) -> i32`: one of the device's limits ([`wrela_abi::Limits`]), as a `u32`'s
/// bits; 0 past the last.
fn limit<E: Executor>(caller: wasmtime::Caller<'_, State<E>>, index: u32) -> i32 {
    caller.data().executor.limits().get(index) as i32
}

/// `wrela.request_status(request) -> i32`: -1 while the request is pending, -2 if it failed,
/// else its answer's length in bytes.
fn request_status<E: Executor>(caller: wasmtime::Caller<'_, State<E>>, request: u32) -> i32 {
    match caller.data().requests.answered.get(&request) {
        None => REQUEST_PENDING,
        Some(Err(_)) => REQUEST_FAILED,
        Some(Ok(bytes)) => bytes.len() as i32,
    }
}

/// `wrela.request_take(request, ptr)`: copies an answered request's bytes to `ptr` in the
/// program's memory, and forgets the request (a failed one too).
fn request_take<E: Executor>(
    mut caller: wasmtime::Caller<'_, State<E>>,
    request: u32,
    ptr: u32,
) -> wasmtime::Result<()> {
    let state = caller.data_mut();
    if !state.started {
        state.failure = Some(Error::Program(CALLED_WHILE_STARTING.into()));
        return Err(wasmtime::format_err!("{CALLED_WHILE_STARTING}"));
    }
    let Some(answer) = state.requests.answered.remove(&request) else {
        return Err(wasmtime::format_err!("request_take({request}): the request isn't answered"));
    };
    let bytes = answer.unwrap_or_default();
    if !shared::write(&state.memory, ptr as usize, &bytes) {
        return Err(wasmtime::format_err!(
            "request_take({request}, {ptr}) reaches past the end of the program's memory"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
