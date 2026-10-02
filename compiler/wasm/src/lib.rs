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

/// Where things are in linear memory.
pub mod memory {
    /// The command buffer: a 12-byte batch header, then commands.
    pub const CMD_BASE: u32 = 1024;
    pub const CMD_CAP: u32 = 1 << 20;
    /// The shadow stack grows down from the top of memory to here.
    pub const STACK_LIMIT: u32 = CMD_BASE + 12 + CMD_CAP + 1024;
    /// Memory size in 64 KiB pages: 16 MiB. Fixed: tier 0 has no heap.
    pub const PAGES: u64 = 256;
    pub const STACK_TOP: u32 = (PAGES as u32) * 65536;
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
}

/// Global indices.
pub(crate) mod globals {
    pub const SP: u32 = 0;
    pub const CMD_LEN: u32 = 1;
    pub const NEXT_HANDLE: u32 = 2;
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
        if let Some(&i) = self.map.get(&(params.clone(), results.clone())) {
            return i;
        }
        let i = self.map.len() as u32;
        self.section.ty().function(params.clone(), results.clone());
        self.map.insert((params, results), i);
        i
    }
}

/// The WASM signature of an IR function: (params, results, whether it has a result pointer).
pub(crate) fn signature(m: &ir::Module, f: &ir::Function) -> (Vec<ValType>, Vec<ValType>, bool) {
    let mut params = Vec::new();
    let sret = f.ret.is_some_and(|t| !f.ret_ref && m.types.is_aggregate(t));
    if sret {
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
    (params, results, sret)
}

/// Emits the CPU module as WASM.
pub fn emit(m: &ir::Module) -> Result<Vec<u8>, String> {
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
    let helpers = Helpers { submit: 0, flush: 1, reserve: 2, write: 3 };
    let nhelpers = 3;
    let first_fn = 1 + nhelpers;
    let mut functions = FunctionSection::new();
    let mut code = CodeSection::new();
    // Helpers.
    for (params, results, body) in func::helper_bodies(&helpers) {
        let ty = types.get(params, results);
        functions.function(ty);
        code.function(&body);
    }
    // The program's functions.
    let indices: Vec<u32> = (0..m.functions.len() as u32).map(|i| first_fn + i).collect();
    for (i, f) in m.functions.iter().enumerate() {
        let (params, results, _) = signature(m, f);
        let ty = types.get(params, results);
        functions.function(ty);
        let body = func::emit_function(m, i, &indices, &helpers)
            .map_err(|e| format!("{}: {e}", f.name))?;
        code.function(&body);
    }
    // Export wrappers: plain scalars in, scalars or a flattened vector out, then a flush.
    let mut exports = ExportSection::new();
    exports.export(wrela_abi::EXPORT_MEMORY, ExportKind::Memory, 0);
    let mut next = first_fn + m.functions.len() as u32;
    for (name, f) in &m.exports {
        let (params, results, body) = func::export_wrapper(m, *f, indices[f.index()], &helpers)?;
        let ty = types.get(params, results);
        functions.function(ty);
        code.function(&body);
        exports.export(name, ExportKind::Func, next);
        next += 1;
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
    globals.global(g(true), &ConstExpr::i32_const(memory::STACK_TOP as i32));
    globals.global(g(true), &ConstExpr::i32_const(0));
    globals.global(g(true), &ConstExpr::i32_const(0));
    let mut module = Module::new();
    module.section(&types.section);
    module.section(&imports);
    module.section(&functions);
    module.section(&memories);
    module.section(&globals);
    module.section(&exports);
    module.section(&code);
    Ok(module.finish())
}

/// Fails if a WASM module uses any relaxed-SIMD instruction (AC7): those are the only WASM
/// instructions whose results may differ between engines, including fused multiply-add.
pub fn check_no_relaxed_simd(bytes: &[u8]) -> Result<(), String> {
    use wasmparser::{Operator, Parser, Payload};
    for payload in Parser::new(0).parse_all(bytes) {
        let payload = payload.map_err(|e| e.to_string())?;
        if let Payload::CodeSectionEntry(body) = payload {
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
        }
    }
    Ok(())
}
