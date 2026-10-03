//! The WASM back end (language.md §11, D-074).
//!
//! - **Strict numerics.** WASM's float arithmetic is IEEE-strict, and this back end emits no
//!   SIMD at all, so no relaxed-SIMD instruction (the only fused multiply-add in WASM) can
//!   appear; [`check_no_relaxed_simd`] proves it for any module. Integer overflow, division by
//!   zero, out-of-range shifts, float-to-int conversions out of range, out-of-bounds indexing and
//!   stack overflow all trap.
//! - **Memory.** Scalars live in WASM locals. Every aggregate (vector, matrix, struct, array,
//!   run) lives in memory, laid out by WGSL's rules (`wrela_ir::layout`), on a shadow stack: a
//!   value is a pointer to its own frame slot, written once. Loading an aggregate from a place
//!   copies it; storing copies it back.
//! - **The program ABI** (wrela-abi): the module imports only `wrela.submit` and exports
//!   `memory` and the program's exports. Host operations write commands into a buffer in
//!   memory, flushed through `submit` when it fills and when every exported call returns.

mod func;

use std::collections::HashMap;
use wasm_encoder::*;
use wrela_ir as ir;

/// Where things are in linear memory. Constant data is at the top, and the shadow stack starts
/// below it.
pub mod memory {
    /// The command buffer: a batch header, then commands.
    pub const CMD_BASE: u32 = 1024;
    pub const CMD_CAP: u32 = 1 << 20;
    /// The shadow stack grows down from below the constant data to here.
    pub const STACK_LIMIT: u32 = CMD_BASE + wrela_abi::stream::HEADER_LEN as u32 + CMD_CAP + 1024;
    /// Memory size in 64 KiB pages: 16 MiB. Fixed: tier 0 has no heap.
    pub const PAGES: u64 = 256;
    pub const STACK_TOP: u32 = (PAGES as u32) * 65536;
    // A dispatch or draw with the largest uniform block lowering allows still fits in one batch,
    // with room for its other words.
    const _: () = assert!(CMD_CAP - wrela_ir::MAX_UNIFORM_BYTES >= 256);
}

/// Function indices of the generated helpers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Helpers {
    pub submit: u32,
    /// `flush()`: submits the pending commands, if any.
    pub flush: u32,
    /// `reserve(bytes) -> address`: room for a command, flushing first if needed.
    pub reserve: u32,
    /// `write(handle, offset, ptr, bytes)`: WriteBuffer commands, in chunks that fit.
    pub write: u32,
    /// `release()`: DestroyBuffer for the buffers made since the last release. Each export
    /// calls it first: a buffer lives until the program's next call (language.md §12).
    pub release: u32,
    /// `fmod(a, b)`, for f32s and f64s: float `%`, exactly (language.md §11).
    pub fmod_f32: u32,
    pub fmod_f64: u32,
}

/// Global indices.
pub(crate) mod globals {
    pub const SP: u32 = 0;
    pub const CMD_LEN: u32 = 1;
    pub const NEXT_HANDLE: u32 = 2;
    /// The first handle not yet released.
    pub const LIVE_FROM: u32 = 3;
    /// The first handle not yet submitted: a batch a trap left unsubmitted made the rest.
    pub const SUBMITTED_TO: u32 = 4;
    /// `LIVE_FROM` as submitted: the handles below it were destroyed.
    pub const RELEASED_TO: u32 = 5;
}

pub(crate) fn valtype(s: ir::Scalar) -> ValType {
    match s {
        ir::Scalar::I64 | ir::Scalar::U64 => ValType::I64,
        ir::Scalar::F32 => ValType::F32,
        ir::Scalar::F64 => ValType::F64,
        _ => ValType::I32,
    }
}

/// A type's WASM value type: its scalar's, or a pointer (i32) for aggregates.
pub(crate) fn repr(types: &ir::Types, t: ir::TypeId) -> ValType {
    match types.get(t) {
        ir::TypeDef::Scalar(s) => valtype(*s),
        _ => ValType::I32,
    }
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

/// Whether an IR function returns its result through a pointer, its first WASM parameter: an
/// aggregate it returns by value.
pub(crate) fn sret(m: &ir::Module, f: &ir::Function) -> bool {
    f.ret.is_some_and(|t| !f.ret_ref && m.types.is_aggregate(t))
}

/// The WASM signature of an IR function: (params, results).
pub(crate) fn signature(m: &ir::Module, f: &ir::Function) -> (Vec<ValType>, Vec<ValType>) {
    let mut params = Vec::new();
    if sret(m, f) {
        params.push(ValType::I32);
    }
    for p in &f.params {
        params.push(if p.by_ref { ValType::I32 } else { repr(&m.types, p.ty) });
    }
    let results = match f.ret {
        Some(_) if f.ret_ref => vec![ValType::I32],
        Some(t) if !m.types.is_aggregate(t) => vec![repr(&m.types, t)],
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

/// Emits the CPU module as WASM, and checks the WASM uses no relaxed SIMD (see
/// [`check_no_relaxed_simd`]).
pub fn emit(m: &ir::Module) -> Result<Emitted, String> {
    if !m.entry_points.is_empty() || !m.resources.is_empty() {
        return Err("internal: a GPU module reached the WASM back end".into());
    }
    let mut types = TypeTable::default();
    let mut imports = ImportSection::new();
    let submit_ty = types.get(vec![ValType::I32, ValType::I32], vec![]);
    imports.import(
        wrela_abi::IMPORT_MODULE,
        wrela_abi::IMPORT_SUBMIT,
        EntityType::Function(submit_ty),
    );
    let helpers =
        Helpers { submit: 0, flush: 1, reserve: 2, write: 3, release: 4, fmod_f32: 5, fmod_f64: 6 };
    let (data_at, data_base, data_bytes) = data(m)?;
    let helper_bodies = func::helper_bodies(&helpers);
    // The code section: the helpers, the program's functions, then the export wrappers.
    let nhelpers = helper_bodies.len();
    let first_fn = 1 + nhelpers as u32;
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
        let (params, results) = signature(m, f);
        let ty = types.get(params, results);
        functions.function(ty);
        let (body, lines) = func::emit_function(m, i, first_fn, &helpers, &data_at)
            .map_err(|e| format!("{}: {e}", f.name))?;
        code.function(&body);
        body_lines.push(lines);
    }
    // Export wrappers: plain scalars in, scalars or a flattened vector out, then a flush.
    let mut exports = ExportSection::new();
    exports.export(wrela_abi::EXPORT_MEMORY, ExportKind::Memory, 0);
    let wrappers = first_fn + m.functions.len() as u32..;
    for (next, (name, f)) in wrappers.zip(&m.exports) {
        let (params, results, body) =
            func::export_wrapper(m, *f, first_fn + f.0, &helpers, data_base)?;
        let ty = types.get(params, results);
        functions.function(ty);
        code.function(&body);
        exports.export(name, ExportKind::Func, next);
    }
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: memory::PAGES,
        maximum: Some(memory::PAGES),
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    let mut globals = GlobalSection::new();
    let g = |mutable| GlobalType { val_type: ValType::I32, mutable, shared: false };
    globals.global(g(true), &ConstExpr::i32_const(data_base as i32));
    // CMD_LEN, NEXT_HANDLE, LIVE_FROM, SUBMITTED_TO and RELEASED_TO start at 0.
    for _ in globals::CMD_LEN..=globals::RELEASED_TO {
        globals.global(g(true), &ConstExpr::i32_const(0));
    }
    let mut module = Module::new();
    module.section(&types.section);
    module.section(&imports);
    module.section(&functions);
    module.section(&memories);
    module.section(&globals);
    module.section(&exports);
    module.section(&code);
    if !data_bytes.is_empty() {
        let mut data = DataSection::new();
        data.active(0, &ConstExpr::i32_const(data_base as i32), data_bytes);
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

/// Where the module's constant data goes: each one's address, where the data starts (the stack
/// starts below it), and its bytes.
fn data(m: &ir::Module) -> Result<(Vec<u32>, u32, Vec<u8>), String> {
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
    if size > memory::STACK_TOP - memory::STACK_LIMIT - (1 << 20) {
        return Err(format!(
            "the program's constants take {size} bytes, more than memory has room for"
        ));
    }
    let base = memory::STACK_TOP - size;
    let mut bytes = vec![0u8; size as usize];
    for (d, &at) in m.data.iter().zip(&offsets) {
        write_const(&m.types, d.ty, &d.value, &mut bytes, at as usize)?;
    }
    Ok((offsets.iter().map(|&o| base + o).collect(), base, bytes))
}

/// Writes a constant of type `t` at `at`, laid out as memory holds it (`wrela_ir::layout`).
fn write_const(
    types: &ir::Types,
    t: ir::TypeId,
    v: &ir::ConstValue,
    out: &mut [u8],
    at: usize,
) -> Result<(), String> {
    use ir::layout::{array_stride, column_stride, field_offsets, layout};
    let mut put = |b: &[u8]| out[at..at + b.len()].copy_from_slice(b);
    match (types.get(t), v) {
        (ir::TypeDef::Scalar(_), ir::ConstValue::Scalar(c)) => match *c {
            ir::Const::Bool(b) => put(&u32::from(b).to_le_bytes()),
            ir::Const::I32(x) => put(&x.to_le_bytes()),
            ir::Const::U32(x) => put(&x.to_le_bytes()),
            ir::Const::I64(x) => put(&x.to_le_bytes()),
            ir::Const::U64(x) => put(&x.to_le_bytes()),
            ir::Const::F32(x) => put(&x.to_le_bytes()),
            ir::Const::F64(x) => put(&x.to_le_bytes()),
            // 8- and 16-bit integers: their low bytes.
            ir::Const::Small(_, x) => put(&x.to_le_bytes()[..layout(types, t).size as usize]),
        },
        (&ir::TypeDef::Vector(_), ir::ConstValue::Parts(ps)) => {
            let f32 = types.lookup(&ir::TypeDef::Scalar(ir::Scalar::F32)).ok_or("no f32")?;
            for (k, p) in ps.iter().enumerate() {
                write_const(types, f32, p, out, at + 4 * k)?;
            }
        }
        (&ir::TypeDef::Matrix(n), ir::ConstValue::Parts(ps)) => {
            let col = types.lookup(&ir::TypeDef::Vector(n)).ok_or("no column type")?;
            for (k, p) in ps.iter().enumerate() {
                write_const(types, col, p, out, at + (column_stride(n) as usize) * k)?;
            }
        }
        (&ir::TypeDef::Array(e, _), ir::ConstValue::Parts(ps)) => {
            let stride = array_stride(types, e) as usize;
            for (k, p) in ps.iter().enumerate() {
                write_const(types, e, p, out, at + stride * k)?;
            }
        }
        (ir::TypeDef::Struct { fields, .. }, ir::ConstValue::Parts(ps)) => {
            let offsets = field_offsets(types, t);
            for ((&(_, ft), p), &off) in fields.iter().zip(ps).zip(offsets) {
                write_const(types, ft, p, out, at + off as usize)?;
            }
        }
        (ir::TypeDef::Enum { .. }, ir::ConstValue::Variant(k, payload)) => {
            put(&k.to_le_bytes());
            if let Some(p) = payload {
                let pt = types.field(t, 1 + k).ok_or("internal: a variant with no payload")?;
                let off = field_offsets(types, t)[1 + *k as usize] as usize;
                write_const(types, pt, p, out, at + off)?;
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
}
