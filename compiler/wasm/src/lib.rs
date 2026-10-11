//! The WASM back end (language.md §11, D-074).
//!
//! - **Strict numerics.** WASM's float arithmetic is IEEE-strict. The back end emits standard
//!   SIMD only, never relaxed SIMD (whose fused multiply-add is the only one in WASM);
//!   [`check_no_relaxed_simd`] proves it for any module. Integer overflow, division by zero,
//!   out-of-range shifts, float-to-int conversions out of range, out-of-bounds indexing and
//!   stack overflow all trap.
//! - **SIMD** ([`Options::simd`], language.md §11). A `vec2`, `vec3` or `vec4` value is a
//!   `v128`, its components in the low lanes (the others hold anything, and nothing reads
//!   them), and its arithmetic is `f32x4` instructions. Each lane is rounded exactly as the
//!   scalar operation would round it, and a sum of lanes (a dot product, a length) adds them one
//!   at a time in the scalar order, so the bits are the same as without SIMD.
//! - **Memory.** Scalars, and vectors with SIMD, live in WASM locals. Every other aggregate
//!   (matrix, struct, array, run; vectors without SIMD) lives in memory, laid out by WGSL's
//!   rules (`wrela_ir::layout`), on a shadow stack: a value is a pointer to its own frame slot,
//!   written once. Loading an aggregate from a place copies it; storing copies it back.
//! - **The program ABI** (wrela-abi): the module imports only `wrela.submit` and exports
//!   `memory` and the program's exports. Host operations write commands into a buffer in
//!   memory, flushed through `submit` when it fills and when every exported call returns.

mod func;

use std::collections::HashMap;
use wasm_encoder::*;
use wrela_ir as ir;

/// Where things are in linear memory: `wrela_abi::memory`.
pub mod memory {
    pub use wrela_abi::memory::*;
    // A dispatch or draw with the largest uniform block lowering allows still fits in one batch,
    // with room for its other words.
    const _: () = assert!(CMD_CAP - wrela_ir::MAX_UNIFORM_BYTES >= 256);
}

/// Function indices of the generated helpers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Helpers {
    pub submit: u32,
    /// The imports `wrela.request_status(request) -> i32` and `wrela.request_take(request,
    /// ptr)`.
    pub request_status: u32,
    pub request_take: u32,
    /// The import `wrela.limit(index) -> i32`.
    pub limit: u32,
    /// The import `wrela.audio(task, context)`, in a module that starts a voice.
    pub audio: u32,
    /// The import `wrela.input(ptr, cap) -> i32`, in a module that reads input.
    pub input: u32,
    /// `wrela.keep` and `wrela.kept`, when the program keeps bytes across a hot reload.
    pub keep: u32,
    pub kept: u32,
    /// `wrela.clock`, when the program reads the clock.
    pub clock: u32,
    /// `wrela.print` and `wrela.phase`, when the program prints or times a phase.
    pub print: u32,
    pub phase: u32,
    /// The import `wrela.tick(task, context, hz)`, in a module that starts a ticker.
    pub tick: u32,
    /// The type of a task function ([`ir::MemOp::RunTask`]): (context, chunk) -> ().
    pub task_type: u32,
    /// `flush()`: submits the pending commands, if any.
    pub flush: u32,
    /// `reserve(bytes) -> address`: room for a command, flushing first if needed.
    pub reserve: u32,
    /// `write(handle, offset, ptr, bytes)`: WriteBuffer commands, in chunks that fit.
    pub write: u32,
    /// `fmod(a, b)`, for f32s and f64s: float `%`, exactly (language.md §11).
    pub fmod_f32: u32,
    pub fmod_f64: u32,
}

/// Global indices.
pub(crate) mod globals {
    pub const SP: u32 = 0;
    pub const CMD_LEN: u32 = 1;
    /// The next resource handle: from 1, so a zeroed buffer value names none.
    pub const NEXT_HANDLE: u32 = 2;
    /// The first handle not yet submitted: a batch a trap left unsubmitted made the rest.
    pub const SUBMITTED_TO: u32 = 3;
    /// How many counted calls are in progress (`ir::Function::counted`).
    pub const DEPTH: u32 = 4;
    /// The next request number: from 1. Requests are numbered in the order they're made, and a
    /// number is never used twice.
    pub const NEXT_REQUEST: u32 = 5;
    /// The lowest address the shadow stack may reach: the running thread's stack's (each
    /// thread entry sets it).
    pub const STACK_FLOOR: u32 = 6;
    /// The running thread's block (wrela_abi `memory`): the program's thread's, until a thread
    /// entry sets its own.
    pub const THREAD: u32 = 7;
}

pub(crate) fn valtype(s: ir::Scalar) -> ValType {
    match s {
        ir::Scalar::I64 | ir::Scalar::U64 => ValType::I64,
        ir::Scalar::F32 => ValType::F32,
        ir::Scalar::F64 => ValType::F64,
        _ => ValType::I32,
    }
}

/// The lanes of a value of type `t` that is a `v128`: an f32 vector's components, with SIMD.
pub(crate) fn v128_lanes(types: &ir::Types, t: ir::TypeId, simd: bool) -> Option<u8> {
    match types.get(t) {
        ir::TypeDef::Vector(ir::Scalar::F32, n) if simd => Some(*n),
        _ => None,
    }
}

/// A type's WASM value type: its scalar's, a `v128` for a vector with SIMD, or a pointer (i32)
/// for the aggregates in memory.
pub(crate) fn repr(types: &ir::Types, t: ir::TypeId, simd: bool) -> ValType {
    match types.get(t) {
        ir::TypeDef::Scalar(s) => valtype(*s),
        _ if v128_lanes(types, t, simd).is_some() => ValType::V128,
        _ => ValType::I32,
    }
}

/// Whether a value of type `t` lives in memory: an aggregate, but an f32 vector with SIMD.
/// (Other vectors are in memory, their arithmetic done a component at a time: checked, for an
/// integer's, as a scalar's is.)
pub(crate) fn in_memory(types: &ir::Types, t: ir::TypeId, simd: bool) -> bool {
    types.is_aggregate(t) && v128_lanes(types, t, simd).is_none()
}

/// Function types, deduplicated.
#[derive(Default)]
pub(crate) struct TypeTable {
    pub section: TypeSection,
    map: HashMap<(Vec<ValType>, Vec<ValType>), u32>,
}

impl TypeTable {
    pub fn get(&mut self, params: Vec<ValType>, results: Vec<ValType>) -> u32 {
        let next = self.map.len() as u32;
        *self.map.entry((params, results)).or_insert_with_key(|(params, results)| {
            self.section.ty().function(params.iter().copied(), results.iter().copied());
            next
        })
    }
}

/// Whether an IR function returns its result through a pointer, its first WASM parameter: a
/// value in memory it returns by value.
pub(crate) fn sret(m: &ir::Module, f: &ir::Function, simd: bool) -> bool {
    f.ret.is_some_and(|t| !f.ret_ref && in_memory(&m.types, t, simd))
}

/// The WASM signature of an IR function: (params, results).
pub(crate) fn signature(
    m: &ir::Module,
    f: &ir::Function,
    simd: bool,
) -> (Vec<ValType>, Vec<ValType>) {
    let mut params = Vec::new();
    if sret(m, f, simd) {
        params.push(ValType::I32);
    }
    for p in &f.params {
        params.push(if p.by_ref { ValType::I32 } else { repr(&m.types, p.ty, simd) });
    }
    let results = match f.ret {
        Some(_) if f.ret_ref => vec![ValType::I32],
        Some(t) if !in_memory(&m.types, t, simd) => vec![repr(&m.types, t, simd)],
        _ => Vec::new(),
    };
    (params, results)
}

/// A program's WASM, and where its code's source locations start.
pub struct Emitted {
    pub wasm: Vec<u8>,
    /// Module offsets, ascending, and the source location from each on: `None` where a
    /// function with no source location starts (see `wrela_abi::lines`).
    pub lines: Vec<(u32, Option<wrela_diag::Span>)>,
}

/// What the back end emits.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// The memory is imported and shared (`wrela.memory`). Without, the module defines its own
    /// memory, unshared: for running on one thread, as the build does its constants (no worker
    /// runs it, so nothing waits).
    pub shared_memory: bool,
    /// Vectors are `v128` values, and their arithmetic SIMD (see the crate's docs). Without,
    /// they're in memory and each component is computed on its own: the same results, bit for
    /// bit, which a test checks.
    pub simd: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options { shared_memory: true, simd: true }
    }
}

/// Emits the CPU module as WASM with the default [`Options`], and checks the WASM uses no
/// relaxed SIMD (see [`check_no_relaxed_simd`]).
pub fn emit(m: &ir::Module) -> Result<Emitted, String> {
    emit_with(m, Options::default())
}

/// [`emit`], with these options.
pub fn emit_with(m: &ir::Module, options: Options) -> Result<Emitted, String> {
    let Options { shared_memory, simd } = options;
    if !m.entry_points.is_empty() || !m.resources.is_empty() {
        return Err("internal: a GPU module reached the WASM back end".into());
    }
    let mut types = TypeTable::default();
    let mut imports = ImportSection::new();
    let (data_at, data_base, data_bytes, heap_base) = data(m)?;
    let submit_ty = types.get(vec![ValType::I32, ValType::I32], vec![]);
    let status_ty = types.get(vec![ValType::I32], vec![ValType::I32]);
    for (name, ty) in [
        (wrela_abi::IMPORT_SUBMIT, submit_ty),
        (wrela_abi::IMPORT_REQUEST_STATUS, status_ty),
        (wrela_abi::IMPORT_REQUEST_TAKE, submit_ty),
        (wrela_abi::IMPORT_LIMIT, status_ty),
    ] {
        imports.import(wrela_abi::IMPORT_MODULE, name, EntityType::Function(ty));
    }
    let audio = imports.len();
    if m.audio {
        let ty = EntityType::Function(submit_ty);
        imports.import(wrela_abi::IMPORT_MODULE, wrela_abi::IMPORT_AUDIO, ty);
    }
    let input_ty = types.get(vec![ValType::I32, ValType::I32], vec![ValType::I32]);
    let input = imports.len();
    if m.input {
        let ty = EntityType::Function(input_ty);
        imports.import(wrela_abi::IMPORT_MODULE, wrela_abi::IMPORT_INPUT, ty);
    }
    let keep = imports.len();
    if m.reload {
        let ty = EntityType::Function(submit_ty);
        imports.import(wrela_abi::IMPORT_MODULE, wrela_abi::IMPORT_KEEP, ty);
        let ty = EntityType::Function(input_ty);
        imports.import(wrela_abi::IMPORT_MODULE, wrela_abi::IMPORT_KEPT, ty);
    }
    let clock = imports.len();
    if m.clock {
        let ty = types.get(vec![ValType::I32], vec![]);
        imports.import(wrela_abi::IMPORT_MODULE, wrela_abi::IMPORT_CLOCK, EntityType::Function(ty));
    }
    let print = imports.len();
    if m.trace {
        let ty = EntityType::Function(submit_ty);
        imports.import(wrela_abi::IMPORT_MODULE, wrela_abi::IMPORT_PRINT, ty);
        imports.import(wrela_abi::IMPORT_MODULE, wrela_abi::IMPORT_PHASE, ty);
    }
    let tick = imports.len();
    if m.tick {
        let ty = types.get(vec![ValType::I32; 3], vec![]);
        imports.import(wrela_abi::IMPORT_MODULE, wrela_abi::IMPORT_TICK, EntityType::Function(ty));
    }
    let nimports = imports.len();
    // The memory, shared: workers' instances get the same one (wrela_abi::memory).
    let memory_type = MemoryType {
        minimum: u64::from(heap_base / memory::PAGE + memory::HEAP_START_PAGES),
        maximum: Some(u64::from(memory::MAX_PAGES)),
        memory64: false,
        shared: shared_memory,
        page_size_log2: None,
    };
    let mut memories = MemorySection::new();
    if shared_memory {
        let name = wrela_abi::IMPORT_MEMORY;
        imports.import(wrela_abi::IMPORT_MODULE, name, EntityType::Memory(memory_type));
    } else {
        memories.memory(memory_type);
    }
    let helpers = Helpers {
        submit: 0,
        request_status: 1,
        request_take: 2,
        limit: 3,
        audio,
        input,
        keep,
        kept: keep + 1,
        clock,
        print,
        phase: print + 1,
        tick,
        flush: nimports,
        reserve: nimports + 1,
        write: nimports + 2,
        fmod_f32: nimports + 3,
        fmod_f64: nimports + 4,
        task_type: submit_ty,
    };
    // The backstop's message (`ir::Module::command_message`), where it is in the data.
    let command_message =
        m.command_message.and_then(|(d, n)| data_at.get(d.index()).map(|&at| (at, n)));
    let helper_bodies = func::helper_bodies(&helpers, command_message);
    // The code section: the helpers, the program's functions, then the export wrappers.
    let nhelpers = helper_bodies.len();
    let first_fn = nimports + nhelpers as u32;
    let mut functions = FunctionSection::new();
    let mut code = CodeSection::new();
    for (params, results, body) in helper_bodies {
        let ty = types.get(params, results);
        functions.function(ty);
        code.function(&body);
    }
    // The program's functions, and each one's locations.
    let mut body_lines = Vec::with_capacity(m.functions.len());
    for (i, f) in m.functions.iter().enumerate() {
        let (params, results) = signature(m, f, simd);
        let ty = types.get(params, results);
        functions.function(ty);
        let at = func::At { first_fn, helpers: &helpers, data: &data_at, heap_base, simd };
        let (body, lines) =
            func::emit_function(m, i, &at).map_err(|e| format!("{}: {e}", f.name))?;
        code.function(&body);
        body_lines.push(lines);
    }
    // Export wrappers: scalars and flattened vectors in, a result's scalars out, then a flush.
    let mut exports = ExportSection::new();
    exports.export(wrela_abi::EXPORT_MEMORY, ExportKind::Memory, 0);
    let wrappers = first_fn + m.functions.len() as u32..;
    for (next, (name, f)) in wrappers.zip(&m.exports) {
        let (params, results, body) =
            func::export_wrapper(m, *f, first_fn + f.0, &helpers, memory::STACK_TOP, simd)?;
        let ty = types.get(params, results);
        functions.function(ty);
        code.function(&body);
        exports.export(name, ExportKind::Func, next);
    }
    // Thread entries, `__name(thread, ...)`: the thread's own stack and block, then the
    // entry (wrela_abi `memory`'s threads).
    let mut next = first_fn + m.functions.len() as u32 + m.exports.len() as u32;
    for (name, f) in &m.thread_entries {
        let n = m.functions[f.index()].params.len() as u32;
        let body = func::thread_entry_wrapper(first_fn + f.0, n);
        let ty = types.get(vec![ValType::I32; n as usize], vec![]);
        functions.function(ty);
        code.function(&body);
        exports.export(&format!("__{name}"), ExportKind::Func, next);
        next += 1;
    }
    // A shared memory's constants are copied once, by a start function, not by each instance
    // as it starts: a worker's instance starts while the program runs, and would overwrite
    // what's changed since (the program's state among it).
    let passive = shared_memory && !data_bytes.is_empty();
    let start = passive.then(|| {
        let body = func::init_data(data_base, data_bytes.len() as u32);
        let ty = types.get(vec![], vec![]);
        functions.function(ty);
        code.function(&body);
        next
    });
    // The tasks a parallel job's chunks run, by index.
    let mut tables = TableSection::new();
    let mut elements = ElementSection::new();
    if !m.tasks.is_empty() {
        let n = m.tasks.len() as u64;
        tables.table(TableType {
            element_type: RefType::FUNCREF,
            minimum: n,
            maximum: Some(n),
            table64: false,
            shared: false,
        });
        let funcs: Vec<u32> = m.tasks.iter().map(|t| first_fn + t.0).collect();
        elements.active(Some(0), &ConstExpr::i32_const(0), Elements::Functions(funcs.into()));
    }
    let mut globals = GlobalSection::new();
    let g = |mutable| GlobalType { val_type: ValType::I32, mutable, shared: false };
    globals.global(g(true), &ConstExpr::i32_const(memory::STACK_TOP as i32));
    // CMD_LEN starts at 0, NEXT_HANDLE and SUBMITTED_TO at 1, DEPTH at 0, NEXT_REQUEST at 1,
    // and the stack's floor is the main stack's (each thread entry sets its own).
    for (i, start) in [
        (globals::CMD_LEN, 0),
        (globals::NEXT_HANDLE, 1),
        (globals::SUBMITTED_TO, 1),
        (globals::DEPTH, 0),
        (globals::NEXT_REQUEST, 1),
        (globals::STACK_FLOOR, memory::STACK_LIMIT as i32),
        (globals::THREAD, memory::thread_block(memory::THREAD_MAIN) as i32),
    ] {
        debug_assert_eq!(i, globals.len());
        globals.global(g(true), &ConstExpr::i32_const(start));
    }
    let mut module = Module::new();
    module.section(&types.section);
    module.section(&imports);
    module.section(&functions);
    if !m.tasks.is_empty() {
        module.section(&tables);
    }
    if !shared_memory {
        module.section(&memories);
    }
    module.section(&globals);
    module.section(&exports);
    if let Some(function_index) = start {
        module.section(&StartSection { function_index });
    }
    if !m.tasks.is_empty() {
        module.section(&elements);
    }
    if passive {
        module.section(&DataCountSection { count: 1 });
    }
    module.section(&code);
    if !data_bytes.is_empty() {
        let mut data = DataSection::new();
        if passive {
            data.passive(data_bytes);
        } else {
            data.active(0, &ConstExpr::i32_const(data_base as i32), data_bytes);
        }
        module.section(&data);
    }
    let wasm = module.finish();
    // Where each function's body starts in the module, to make its offsets module offsets; and
    // the check that no body uses relaxed SIMD.
    let mut lines = Vec::new();
    let mut k = 0usize;
    for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
        if let wasmparser::Payload::CodeSectionEntry(body) = payload.map_err(|e| e.to_string())? {
            no_relaxed_simd(&body)?;
            let start = body.range().start as u32;
            lines.push((start, None));
            if let Some(ls) = k.checked_sub(nhelpers).and_then(|i| body_lines.get(i)) {
                lines.extend(ls.iter().map(|&(off, span)| (start + off, Some(span))));
            }
            k += 1;
        }
    }
    Ok(Emitted { wasm, lines })
}

/// Where the module's constant data goes: each one's address, where the data starts (above the
/// stack), its bytes, and where the heap starts (the next page).
fn data(m: &ir::Module) -> Result<(Vec<u32>, u32, Vec<u8>, u32), String> {
    use ir::layout::{layout, round_up};
    let mut offsets = Vec::new();
    let mut end = 0u32;
    for d in &m.data {
        let l = layout(&m.types, d.ty);
        let at = round_up(l.align, end);
        offsets.push(at);
        end = at.saturating_add(l.size);
    }
    let size = round_up(16, end);
    let base = memory::DATA_BASE;
    let room = (memory::MAX_PAGES - memory::HEAP_START_PAGES) * memory::PAGE - base;
    if size > room {
        return Err(format!(
            "the program's constants take {size} bytes, more than memory has room for"
        ));
    }
    let mut bytes = vec![0u8; size as usize];
    let addrs: Vec<u32> = offsets.iter().map(|&o| base + o).collect();
    for (d, &at) in m.data.iter().zip(&offsets) {
        write_const(&m.types, d.ty, &d.value, &addrs, &mut bytes, at as usize)?;
    }
    let heap_base = round_up(memory::PAGE, base + size);
    Ok((addrs, base, bytes, heap_base))
}

/// Writes a constant of type `t` at `at`, laid out as memory holds it (`wrela_ir::layout`).
/// `addrs` is each piece of constant data's address.
fn write_const(
    types: &ir::Types,
    t: ir::TypeId,
    v: &ir::ConstValue,
    addrs: &[u32],
    out: &mut [u8],
    at: usize,
) -> Result<(), String> {
    let mut put = |b: &[u8]| out[at..at + b.len()].copy_from_slice(b);
    match (types.get(t), v) {
        (ir::TypeDef::Scalar(s), ir::ConstValue::Scalar(c)) => match *c {
            ir::Const::Bool(b) => put(&u32::from(b).to_le_bytes()),
            ir::Const::I32(x) => put(&x.to_le_bytes()),
            ir::Const::U32(x) => put(&x.to_le_bytes()),
            ir::Const::I64(x) => put(&x.to_le_bytes()),
            ir::Const::U64(x) => put(&x.to_le_bytes()),
            ir::Const::F32(x) => put(&x.to_le_bytes()),
            ir::Const::F64(x) => put(&x.to_le_bytes()),
            // 8- and 16-bit integers: their low bytes.
            ir::Const::Small(_, x) => put(&x.to_le_bytes()[..s.bytes() as usize]),
        },
        (
            ir::TypeDef::Vector(..)
            | ir::TypeDef::Matrix(..)
            | ir::TypeDef::Array(..)
            | ir::TypeDef::Struct { .. },
            ir::ConstValue::Parts(ps),
        ) => {
            for (k, p) in (0..).zip(ps) {
                let pt = types.part(t, k).ok_or("internal: a constant with too many parts")?;
                let off =
                    ir::layout::part_offset(types, t, k).ok_or("internal: a part's offset")?;
                write_const(types, pt, p, addrs, out, at + off as usize)?;
            }
        }
        (&ir::TypeDef::Array(..), ir::ConstValue::Bytes(bs)) => put(bs),
        (ir::TypeDef::Scalar(ir::Scalar::U32), ir::ConstValue::Addr(d)) => {
            let a = addrs.get(d.index()).ok_or("internal: an address of missing data")?;
            put(&a.to_le_bytes())
        }
        (ir::TypeDef::Enum { .. }, ir::ConstValue::Variant(k, payload)) => {
            put(&types.tag(t, *k).to_le_bytes());
            if let Some(p) = payload {
                let pt = types.field(t, 1 + k).ok_or("internal: a variant with no payload")?;
                let off = ir::layout::field_offsets(types, t)[1 + *k as usize] as usize;
                write_const(types, pt, p, addrs, out, at + off)?;
            }
        }
        (d, v) => return Err(format!("internal: constant {v:?} of type {d:?}")),
    }
    Ok(())
}

/// `wasm` with a custom section `name` holding `payload` appended (which moves no code).
pub fn with_custom_section(mut wasm: Vec<u8>, name: &str, payload: &[u8]) -> Vec<u8> {
    use wasm_encoder::Encode;
    let mut content = Vec::new();
    name.encode(&mut content);
    content.extend_from_slice(payload);
    wasm.push(0);
    content.len().encode(&mut wasm);
    wasm.extend_from_slice(&content);
    wasm
}

/// Fails if a WASM module uses any relaxed-SIMD instruction (AC7): those are the only WASM
/// instructions whose results may differ between engines, including fused multiply-add.
pub fn check_no_relaxed_simd(bytes: &[u8]) -> Result<(), String> {
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        let payload = payload.map_err(|e| e.to_string())?;
        if let wasmparser::Payload::CodeSectionEntry(body) = payload {
            no_relaxed_simd(&body)?;
        }
    }
    Ok(())
}

/// How many SIMD instructions (those with the `0xfd` prefix) a WASM module's code has.
pub fn simd_instructions(bytes: &[u8]) -> Result<usize, String> {
    let mut n = 0;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        let payload = payload.map_err(|e| e.to_string())?;
        if let wasmparser::Payload::CodeSectionEntry(body) = payload {
            let mut ops = body.get_operators_reader().map_err(|e| e.to_string())?;
            while !ops.eof() {
                let (_, at) = ops.read_with_offset().map_err(|e| e.to_string())?;
                n += usize::from(bytes.get(at as usize) == Some(&0xfd));
            }
        }
    }
    Ok(n)
}

/// Fails if a function body uses a relaxed-SIMD instruction (see [`check_no_relaxed_simd`]).
fn no_relaxed_simd(body: &wasmparser::FunctionBody) -> Result<(), String> {
    use wasmparser::Operator;
    let mut ops = body.get_operators_reader().map_err(|e| e.to_string())?;
    while !ops.eof() {
        let op = ops.read().map_err(|e| e.to_string())?;
        let relaxed = matches!(
            op,
            Operator::I8x16RelaxedSwizzle
                | Operator::I32x4RelaxedTruncF32x4S
                | Operator::I32x4RelaxedTruncF32x4U
                | Operator::I32x4RelaxedTruncF64x2SZero
                | Operator::I32x4RelaxedTruncF64x2UZero
                | Operator::F32x4RelaxedMadd
                | Operator::F32x4RelaxedNmadd
                | Operator::F64x2RelaxedMadd
                | Operator::F64x2RelaxedNmadd
                | Operator::I8x16RelaxedLaneselect
                | Operator::I16x8RelaxedLaneselect
                | Operator::I32x4RelaxedLaneselect
                | Operator::I64x2RelaxedLaneselect
                | Operator::F32x4RelaxedMin
                | Operator::F32x4RelaxedMax
                | Operator::F64x2RelaxedMin
                | Operator::F64x2RelaxedMax
                | Operator::I16x8RelaxedQ15mulrS
                | Operator::I16x8RelaxedDotI8x16I7x16S
                | Operator::I32x4RelaxedDotI8x16I7x16AddS
        );
        if relaxed {
            return Err(format!("the module uses the relaxed-SIMD instruction {op:?}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(body: &str) -> Vec<u8> {
        wat::parse_str(format!("(module (func (param v128 v128 v128) (result v128) {body}))"))
            .expect("valid WAT")
    }

    #[test]
    fn relaxed_simd_is_rejected() {
        let fma = module("local.get 0 local.get 1 local.get 2 f32x4.relaxed_madd");
        let e = check_no_relaxed_simd(&fma).expect_err("relaxed SIMD");
        assert!(e.contains("F32x4RelaxedMadd"), "{e}");
        let min = module("local.get 0 local.get 1 f32x4.relaxed_min");
        assert!(check_no_relaxed_simd(&min).is_err());
        // Plain SIMD is deterministic, and fine.
        let plain = module("local.get 0 local.get 1 f32x4.mul");
        assert_eq!(check_no_relaxed_simd(&plain), Ok(()));
    }

    #[test]
    fn emitted_modules_pass() {
        let mut m = wrela_ir::Module::default();
        let f32t = m.types.f32();
        let mut f = wrela_ir::Function::new("double", Vec::new(), Some(f32t));
        f.params.push(wrela_ir::Param {
            name: "x".into(),
            ty: f32t,
            by_ref: false,
            mutable: false,
        });
        let x = f.new_value(f32t);
        let y = f.new_value(f32t);
        f.body = vec![
            wrela_ir::Stmt::Let(x, wrela_ir::Expr::Param(0)),
            wrela_ir::Stmt::Let(y, wrela_ir::Expr::Binary(wrela_ir::BinOp::Add, x, x)),
            wrela_ir::Stmt::Return(Some(y)),
        ];
        let id = m.add_function(f);
        m.exports.push(("double".into(), id));
        let out = emit(&m).expect("emits");
        assert_eq!(check_no_relaxed_simd(&out.wasm), Ok(()));
    }

    /// `fn f(xs: mut [f32], n: u32)`: a loop over `0..n` whose body, after the exit test, is
    /// `body(f, i, xs)`; and how many `f32x4` instructions it compiles to, with SIMD and without.
    fn loop_simd(
        body: impl Fn(&mut wrela_ir::Function, wrela_ir::ValueId) -> Vec<wrela_ir::Stmt>,
        locals: &[&str],
    ) -> (usize, usize) {
        use wrela_ir::{BinOp, Const, Expr, Param, Place, Stmt, TypeDef};
        let mut m = wrela_ir::Module::default();
        let (f32t, u32t, boolt) = (m.types.f32(), m.types.u32(), m.types.bool());
        let run = m.types.intern(TypeDef::Run(f32t));
        let xs = Param { name: "xs".into(), ty: run, by_ref: true, mutable: true };
        let n = Param { name: "n".into(), ty: u32t, by_ref: false, mutable: false };
        let mut f = wrela_ir::Function::new("f", vec![xs, n], None);
        for l in locals {
            f.new_local(*l, f32t);
        }
        let ctr = f.new_local("counter", u32t);
        let (nv, zero, i, done) =
            (f.new_value(u32t), f.new_value(u32t), f.new_value(u32t), f.new_value(boolt));
        let (a, one, next) = (f.new_value(u32t), f.new_value(u32t), f.new_value(u32t));
        let mut lp = vec![
            Stmt::Let(i, Expr::Load(Place::local(ctr))),
            Stmt::Let(done, Expr::Binary(BinOp::Ge, i, nv)),
            Stmt::If { cond: done, then: vec![Stmt::Break], else_: Vec::new() },
        ];
        lp.extend(body(&mut f, i));
        f.body = vec![
            Stmt::Let(nv, Expr::Param(1)),
            Stmt::Let(zero, Expr::Const(Const::U32(0))),
            Stmt::Store(Place::local(ctr), zero),
            Stmt::Loop {
                body: lp,
                continuing: vec![
                    Stmt::Let(a, Expr::Load(Place::local(ctr))),
                    Stmt::Let(one, Expr::Const(Const::U32(1))),
                    Stmt::Let(next, Expr::Binary(BinOp::Add, a, one)),
                    Stmt::Store(Place::local(ctr), next),
                ],
            },
            Stmt::Return(None),
        ];
        m.add_function(f);
        let count = |simd| {
            let out = emit_with(&m, Options { simd, ..Options::default() }).expect("emits");
            let mut n = 0;
            for payload in wasmparser::Parser::new(0).parse_all(&out.wasm) {
                if let Ok(wasmparser::Payload::CodeSectionEntry(body)) = payload {
                    let mut ops = body.get_operators_reader().expect("operators");
                    while !ops.eof() {
                        n +=
                            usize::from(format!("{:?}", ops.read().expect("op")).contains("F32x4"));
                    }
                }
            }
            n
        };
        (count(true), count(false))
    }

    /// `xs[i] = xs[i] * 2.0` runs four iterations at a time; `s = s + xs[i]` doesn't (it keeps
    /// `s` from one iteration to the next); nor does anything without SIMD.
    #[test]
    fn independent_loops_run_four_at_a_time() {
        use wrela_ir::{BinOp, Const, Expr, Place, PlaceRoot, Proj, Stmt};
        let elem = |i| Place::root(PlaceRoot::Param(0)).with(Proj::Index(i));
        let double = |f: &mut wrela_ir::Function, i| {
            let f32t = f.locals[0].ty;
            let (x, two, y) = (f.new_value(f32t), f.new_value(f32t), f.new_value(f32t));
            vec![
                Stmt::Let(x, Expr::Load(elem(i))),
                Stmt::Let(two, Expr::Const(Const::F32(2.0))),
                Stmt::Let(y, Expr::Binary(BinOp::Mul, x, two)),
                Stmt::Store(elem(i), y),
            ]
        };
        assert!(matches!(loop_simd(double, &["unused"]), (n, 0) if n >= 1));
        let sum = |f: &mut wrela_ir::Function, i| {
            let f32t = f.locals[0].ty;
            let s = wrela_ir::LocalId(0);
            let (x, a, b) = (f.new_value(f32t), f.new_value(f32t), f.new_value(f32t));
            vec![
                Stmt::Let(x, Expr::Load(elem(i))),
                Stmt::Let(a, Expr::Load(Place::local(s))),
                Stmt::Let(b, Expr::Binary(BinOp::Add, a, x)),
                Stmt::Store(Place::local(s), b),
            ]
        };
        assert_eq!(loop_simd(sum, &["s"]), (0, 0));
    }

    /// Engines reject a function with more than 50,000 locals; a derived function can have
    /// hundreds of thousands of values, few of them alive at once. Here: 100,000 values in a
    /// chain, half of them in a loop's body, and one read on every iteration.
    #[test]
    fn locals_are_reused() {
        use wrela_ir::{BinOp, Expr, Stmt};
        let mut m = wrela_ir::Module::default();
        let f32t = m.types.f32();
        let mut f = wrela_ir::Function::new("chain", Vec::new(), Some(f32t));
        f.params.push(wrela_ir::Param {
            name: "x".into(),
            ty: f32t,
            by_ref: false,
            mutable: false,
        });
        let x = f.new_value(f32t);
        let mut body = vec![Stmt::Let(x, Expr::Param(0))];
        let chain = |f: &mut wrela_ir::Function, out: &mut Vec<Stmt>, mut at| {
            for _ in 0..50_000 {
                let next = f.new_value(f32t);
                out.push(Stmt::Let(next, Expr::Binary(BinOp::Add, at, x)));
                at = next;
            }
            at
        };
        let last = chain(&mut f, &mut body, x);
        let mut inner = Vec::new();
        chain(&mut f, &mut inner, last);
        inner.push(Stmt::Break);
        body.push(Stmt::Loop { body: inner, continuing: Vec::new() });
        body.push(Stmt::Return(Some(last)));
        f.body = body;
        let id = m.add_function(f);
        m.exports.push(("chain".into(), id));
        wrela_ir::verify(&m).expect("valid IR");
        let out = emit(&m).expect("emits");
        wasmparser::Validator::new().validate_all(&out.wasm).expect("a valid module");
        let mut most = 0;
        for payload in wasmparser::Parser::new(0).parse_all(&out.wasm) {
            if let wasmparser::Payload::CodeSectionEntry(body) = payload.expect("parses") {
                let mut locals = body.get_locals_reader().expect("locals");
                let mut n = 0;
                for _ in 0..locals.get_count() {
                    n += locals.read().expect("a local").0;
                }
                most = most.max(n);
            }
        }
        assert!(most < 16, "a function has {most} locals");
    }

    /// The operators of the emitted function whose code holds the f32 constant `marker`, as
    /// text: a test's function, found among the module's helpers and wrappers.
    fn body_with(wasm: &[u8], marker: f32) -> Vec<String> {
        for payload in wasmparser::Parser::new(0).parse_all(wasm) {
            if let Ok(wasmparser::Payload::CodeSectionEntry(body)) = payload {
                let mut ops = body.get_operators_reader().expect("operators");
                let mut out = Vec::new();
                while !ops.eof() {
                    out.push(format!("{:?}", ops.read().expect("op")));
                }
                let mark = format!("F32Const {{ value: Ieee32({}) }}", marker.to_bits());
                if out.contains(&mark) {
                    return out;
                }
            }
        }
        panic!("no function holds {marker}")
    }

    /// `fn f(p: &[f32; n]) -> f32 { let l = p; l[0] + MARK }`: a copy of `n` floats into a local.
    fn copy_of(n: u32) -> Vec<String> {
        use wrela_ir::{BinOp, Const, Expr, Param, Place, PlaceRoot, Proj, Stmt, TypeDef};
        let mut m = wrela_ir::Module::default();
        let (f32t, u32t) = (m.types.f32(), m.types.u32());
        let arr = m.types.intern(TypeDef::Array(f32t, n));
        let p = Param { name: "p".into(), ty: arr, by_ref: true, mutable: false };
        let mut f = wrela_ir::Function::new("f", vec![p], Some(f32t));
        let l = f.new_local("l", arr);
        let (v, zero, x, mark, out) = (
            f.new_value(arr),
            f.new_value(u32t),
            f.new_value(f32t),
            f.new_value(f32t),
            f.new_value(f32t),
        );
        f.body = vec![
            Stmt::Let(v, Expr::Load(Place::root(PlaceRoot::Param(0)))),
            Stmt::Store(Place::local(l), v),
            Stmt::Let(zero, Expr::Const(Const::U32(0))),
            Stmt::Let(x, Expr::Load(Place::local(l).with(Proj::Index(zero)))),
            Stmt::Let(mark, Expr::Const(Const::F32(1234.5))),
            Stmt::Let(out, Expr::Binary(BinOp::Add, x, mark)),
            Stmt::Return(Some(out)),
        ];
        // Not exported (an export takes no aggregate): the back end emits it all the same.
        m.add_function(f);
        wrela_ir::verify(&m).expect("valid IR");
        let out = emit(&m).expect("emits");
        wasmparser::Validator::new().validate_all(&out.wasm).expect("a valid module");
        body_with(&out.wasm, 1234.5)
    }

    /// A small value is copied with loads and stores of its own (an engine can make
    /// `memory.copy` a call out of the code); a large one with `memory.copy`.
    #[test]
    fn small_copies_are_loads_and_stores() {
        let copies = |ops: &[String]| ops.iter().filter(|o| o.starts_with("MemoryCopy")).count();
        assert_eq!(copies(&copy_of(8)), 0);
        assert!(copies(&copy_of(256)) >= 1);
    }

    /// `fn f(x: f32) -> f32 { x + MARK }` has no frame: no stack check, and the stack pointer
    /// isn't touched. A function with a frame checks and moves it.
    #[test]
    fn a_function_without_a_frame_leaves_the_stack_alone() {
        use wrela_ir::{BinOp, Const, Expr, Param, Stmt};
        let mut m = wrela_ir::Module::default();
        let f32t = m.types.f32();
        let x_param = Param { name: "x".into(), ty: f32t, by_ref: false, mutable: false };
        let mut f = wrela_ir::Function::new("f", vec![x_param], Some(f32t));
        let (x, mark, out) = (f.new_value(f32t), f.new_value(f32t), f.new_value(f32t));
        f.body = vec![
            Stmt::Let(x, Expr::Param(0)),
            Stmt::Let(mark, Expr::Const(Const::F32(1234.5))),
            Stmt::Let(out, Expr::Binary(BinOp::Add, x, mark)),
            Stmt::Return(Some(out)),
        ];
        let id = m.add_function(f);
        m.exports.push(("f".into(), id));
        let out = emit(&m).expect("emits");
        let sp = format!("GlobalGet {{ global_index: {} }}", globals::SP);
        assert!(!body_with(&out.wasm, 1234.5).contains(&sp));
        assert!(copy_of(256).contains(&sp));
    }

    /// In `for i in 0..4 { a[i] }` over a `[f32; 4]`, the loop's own test proves each index in
    /// range: the access isn't checked again. Over `0..5` it is.
    #[test]
    fn an_index_the_loop_test_proves_isnt_checked() {
        use wrela_ir::{BinOp, Const, Expr, Param, Place, PlaceRoot, Proj, Stmt, TypeDef};
        let traps = |len: u32| {
            let mut m = wrela_ir::Module::default();
            let (f32t, u32t, boolt) = (m.types.f32(), m.types.u32(), m.types.bool());
            let arr = m.types.intern(TypeDef::Array(f32t, 4));
            let p = Param { name: "a".into(), ty: arr, by_ref: true, mutable: false };
            let mut f = wrela_ir::Function::new("f", vec![p], Some(f32t));
            let (ctr, sum) = (f.new_local("i", u32t), f.new_local("s", f32t));
            let v = |f: &mut wrela_ir::Function, t| f.new_value(t);
            let (zero, n, zf, mark) =
                (v(&mut f, u32t), v(&mut f, u32t), v(&mut f, f32t), v(&mut f, f32t));
            let (i, done, x, s0, s1) = (
                v(&mut f, u32t),
                v(&mut f, boolt),
                v(&mut f, f32t),
                v(&mut f, f32t),
                v(&mut f, f32t),
            );
            let (a, one, next, out) =
                (v(&mut f, u32t), v(&mut f, u32t), v(&mut f, u32t), v(&mut f, f32t));
            f.body = vec![
                Stmt::Let(zero, Expr::Const(Const::U32(0))),
                Stmt::Let(n, Expr::Const(Const::U32(len))),
                Stmt::Let(zf, Expr::Const(Const::F32(0.0))),
                Stmt::Let(mark, Expr::Const(Const::F32(1234.5))),
                Stmt::Store(Place::local(ctr), zero),
                Stmt::Store(Place::local(sum), mark),
                Stmt::Loop {
                    body: vec![
                        Stmt::Let(i, Expr::Load(Place::local(ctr))),
                        Stmt::Let(done, Expr::Binary(BinOp::Ge, i, n)),
                        Stmt::If { cond: done, then: vec![Stmt::Break], else_: Vec::new() },
                        Stmt::Let(
                            x,
                            Expr::Load(Place::root(PlaceRoot::Param(0)).with(Proj::Index(i))),
                        ),
                        Stmt::Let(s0, Expr::Load(Place::local(sum))),
                        Stmt::Let(s1, Expr::Binary(BinOp::Add, s0, x)),
                        Stmt::Store(Place::local(sum), s1),
                    ],
                    continuing: vec![
                        Stmt::Let(a, Expr::Load(Place::local(ctr))),
                        Stmt::Let(one, Expr::Const(Const::U32(1))),
                        Stmt::Let(next, Expr::Binary(BinOp::Add, a, one)),
                        Stmt::Store(Place::local(ctr), next),
                    ],
                },
                Stmt::Let(out, Expr::Load(Place::local(sum))),
                Stmt::Return(Some(out)),
            ];
            let _ = zf;
            m.add_function(f);
            wrela_ir::verify(&m).expect("valid IR");
            let out = emit_with(&m, Options { simd: false, ..Options::default() }).expect("emits");
            body_with(&out.wasm, 1234.5).iter().filter(|o| *o == "Unreachable").count()
        };
        // The counter's increment is checked either way.
        assert_eq!(traps(5), traps(4) + 1);
    }

    /// `var a = [0.0; 64]` zeros the local's own memory: no zero is made elsewhere and copied.
    #[test]
    fn a_zero_only_stored_is_written_in_place() {
        use wrela_ir::{Const, Expr, Place, Proj, Stmt, TypeDef};
        let mut m = wrela_ir::Module::default();
        let (f32t, u32t) = (m.types.f32(), m.types.u32());
        let arr = m.types.intern(TypeDef::Array(f32t, 64));
        let mut f = wrela_ir::Function::new("f", Vec::new(), Some(f32t));
        let l = f.new_local("a", arr);
        let (z, k, mark, x) =
            (f.new_value(arr), f.new_value(u32t), f.new_value(f32t), f.new_value(f32t));
        f.body = vec![
            Stmt::Let(z, Expr::Zero(arr)),
            Stmt::Store(Place::local(l), z),
            Stmt::Let(k, Expr::Const(Const::U32(7))),
            Stmt::Let(mark, Expr::Const(Const::F32(1234.5))),
            Stmt::Store(Place::local(l).with(Proj::Index(k)), mark),
            Stmt::Let(x, Expr::Load(Place::local(l).with(Proj::Index(k)))),
            Stmt::Return(Some(x)),
        ];
        let id = m.add_function(f);
        m.exports.push(("f".into(), id));
        let out = emit(&m).expect("emits");
        let ops = body_with(&out.wasm, 1234.5);
        assert!(
            !ops.iter().any(|o| o.starts_with("MemoryCopy") || o.starts_with("MemoryFill")),
            "{ops:?}"
        );
    }
}
