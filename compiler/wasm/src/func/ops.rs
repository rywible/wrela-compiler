//! Operators, conversions, built-ins and host operations.

use super::{Fe, R, konst, mem};
use crate::{Helpers, globals, memory};
use wasm_encoder::{BlockType, Function, Instruction as I, ValType};
use wrela_ir as ir;
use wrela_ir::layout::{column_stride, layout, round_up};

// ---- scalar arithmetic ---------------------------------------------------------------------------

/// The range of a small integer type, for checked arithmetic on i8..u16 (held in i32).
fn small_range(s: ir::Scalar) -> Option<(i64, i64)> {
    match s {
        ir::Scalar::I8 => Some((-128, 127)),
        ir::Scalar::U8 => Some((0, 255)),
        ir::Scalar::I16 => Some((-32768, 32767)),
        ir::Scalar::U16 => Some((0, 65535)),
        _ => None,
    }
}

/// Traps unless the i32 on top of the stack (kept there) is in `s`'s range.
fn check_small(fe: &mut Fe, s: ir::Scalar) {
    if let Some((lo, hi)) = small_range(s) {
        let t = fe.new_local(ValType::I32);
        fe.ins.extend([
            I::LocalTee(t),
            I::I32Const(lo as i32),
            I::I32LtS,
            I::LocalGet(t),
            I::I32Const(hi as i32),
            I::I32GtS,
            I::I32Or,
        ]);
        fe.trap_if();
        fe.ins.push(I::LocalGet(t));
    }
}

/// Wraps the i32 on top of the stack into `s`'s range (for wrapping ops and conversions).
fn wrap_small(fe: &mut Fe, s: ir::Scalar) {
    match s {
        ir::Scalar::I8 => fe.ins.push(I::I32Extend8S),
        ir::Scalar::I16 => fe.ins.push(I::I32Extend16S),
        ir::Scalar::U8 => fe.ins.extend([I::I32Const(0xFF), I::I32And]),
        ir::Scalar::U16 => fe.ins.extend([I::I32Const(0xFFFF), I::I32And]),
        _ => {}
    }
}

fn is64(s: ir::Scalar) -> bool {
    matches!(s, ir::Scalar::I64 | ir::Scalar::U64)
}

/// Pushes `a op b` for scalars of type `s`, held in locals `a` and `b`. Integer arithmetic is
/// checked unless the op is a wrapping one.
fn scalar_bin(fe: &mut Fe, op: ir::BinOp, s: ir::Scalar, a: u32, b: u32) -> R<()> {
    use ir::BinOp as B;
    let (la, lb) = (I::LocalGet(a), I::LocalGet(b));
    if s.is_float() {
        let f64_ = s == ir::Scalar::F64;
        let i = match op {
            B::Add => {
                if f64_ {
                    I::F64Add
                } else {
                    I::F32Add
                }
            }
            B::Sub => {
                if f64_ {
                    I::F64Sub
                } else {
                    I::F32Sub
                }
            }
            B::Mul => {
                if f64_ {
                    I::F64Mul
                } else {
                    I::F32Mul
                }
            }
            B::Div => {
                if f64_ {
                    I::F64Div
                } else {
                    I::F32Div
                }
            }
            B::Rem => {
                // a - b * trunc(a / b), as WGSL defines it.
                fe.ins.extend([la.clone(), lb.clone(), la, lb]);
                if f64_ {
                    fe.ins.extend([I::F64Div, I::F64Trunc, I::F64Mul, I::F64Sub]);
                } else {
                    fe.ins.extend([I::F32Div, I::F32Trunc, I::F32Mul, I::F32Sub]);
                }
                return Ok(());
            }
            B::Eq => {
                if f64_ {
                    I::F64Eq
                } else {
                    I::F32Eq
                }
            }
            B::Ne => {
                if f64_ {
                    I::F64Ne
                } else {
                    I::F32Ne
                }
            }
            B::Lt => {
                if f64_ {
                    I::F64Lt
                } else {
                    I::F32Lt
                }
            }
            B::Le => {
                if f64_ {
                    I::F64Le
                } else {
                    I::F32Le
                }
            }
            B::Gt => {
                if f64_ {
                    I::F64Gt
                } else {
                    I::F32Gt
                }
            }
            B::Ge => {
                if f64_ {
                    I::F64Ge
                } else {
                    I::F32Ge
                }
            }
            _ => return Err(format!("internal: {op:?} on floats")),
        };
        fe.ins.extend([la, lb, i]);
        return Ok(());
    }
    let signed = s.signed();
    if is64(s) {
        return bin64(fe, op, signed, a, b);
    }
    let r = fe.new_local(ValType::I32);
    match op {
        B::Add | B::Sub | B::Mul if small_range(s).is_some() => {
            fe.ins.extend([
                la,
                lb,
                match op {
                    B::Add => I::I32Add,
                    B::Sub => I::I32Sub,
                    _ => I::I32Mul,
                },
            ]);
            check_small(fe, s);
        }
        B::Add => {
            fe.ins.extend([la.clone(), lb.clone(), I::I32Add, I::LocalSet(r)]);
            if signed {
                // Overflow iff both operands' signs differ from the result's.
                fe.ins.extend([
                    la,
                    I::LocalGet(r),
                    I::I32Xor,
                    lb,
                    I::LocalGet(r),
                    I::I32Xor,
                    I::I32And,
                    I::I32Const(0),
                    I::I32LtS,
                ]);
            } else {
                fe.ins.extend([I::LocalGet(r), la, I::I32LtU]);
            }
            fe.trap_if();
            fe.ins.push(I::LocalGet(r));
        }
        B::Sub => {
            fe.ins.extend([la.clone(), lb.clone(), I::I32Sub, I::LocalSet(r)]);
            if signed {
                fe.ins.extend([
                    la.clone(),
                    lb,
                    I::I32Xor,
                    la,
                    I::LocalGet(r),
                    I::I32Xor,
                    I::I32And,
                    I::I32Const(0),
                    I::I32LtS,
                ]);
            } else {
                fe.ins.extend([la, lb, I::I32LtU]);
            }
            fe.trap_if();
            fe.ins.push(I::LocalGet(r));
        }
        B::Mul => {
            // In 64 bits, then check it fits.
            let w = fe.new_local(ValType::I64);
            let ext = if signed { I::I64ExtendI32S } else { I::I64ExtendI32U };
            fe.ins.extend([la, ext.clone(), lb, ext, I::I64Mul, I::LocalTee(w)]);
            if signed {
                fe.ins.extend([
                    I::I64Const(i32::MIN as i64),
                    I::I64LtS,
                    I::LocalGet(w),
                    I::I64Const(i32::MAX as i64),
                    I::I64GtS,
                    I::I32Or,
                ]);
            } else {
                fe.ins.extend([I::I64Const(u32::MAX as i64), I::I64GtU]);
            }
            fe.trap_if();
            fe.ins.extend([I::LocalGet(w), I::I32WrapI64]);
        }
        B::Div => {
            fe.ins.extend([la, lb, if signed { I::I32DivS } else { I::I32DivU }]);
            check_small(fe, s);
        }
        B::Rem => fe.ins.extend([la, lb, if signed { I::I32RemS } else { I::I32RemU }]),
        B::WrappingAdd | B::WrappingSub | B::WrappingMul => {
            fe.ins.extend([
                la,
                lb,
                match op {
                    B::WrappingAdd => I::I32Add,
                    B::WrappingSub => I::I32Sub,
                    _ => I::I32Mul,
                },
            ]);
            wrap_small(fe, s);
        }
        B::Shl | B::Shr => {
            // A shift of at least the width traps on the CPU.
            fe.ins.extend([lb.clone(), I::I32Const(s.bits() as i32), I::I32GeU]);
            fe.trap_if();
            fe.ins.extend([
                la,
                lb,
                match (op, signed) {
                    (B::Shl, _) => I::I32Shl,
                    (_, true) => I::I32ShrS,
                    _ => I::I32ShrU,
                },
            ]);
            wrap_small(fe, s);
        }
        B::BitAnd | B::And => fe.ins.extend([la, lb, I::I32And]),
        B::BitOr | B::Or => fe.ins.extend([la, lb, I::I32Or]),
        B::BitXor => fe.ins.extend([la, lb, I::I32Xor]),
        B::Eq => fe.ins.extend([la, lb, I::I32Eq]),
        B::Ne => fe.ins.extend([la, lb, I::I32Ne]),
        B::Lt => fe.ins.extend([la, lb, if signed { I::I32LtS } else { I::I32LtU }]),
        B::Le => fe.ins.extend([la, lb, if signed { I::I32LeS } else { I::I32LeU }]),
        B::Gt => fe.ins.extend([la, lb, if signed { I::I32GtS } else { I::I32GtU }]),
        B::Ge => fe.ins.extend([la, lb, if signed { I::I32GeS } else { I::I32GeU }]),
    }
    Ok(())
}

fn bin64(fe: &mut Fe, op: ir::BinOp, signed: bool, a: u32, b: u32) -> R<()> {
    use ir::BinOp as B;
    let (la, lb) = (I::LocalGet(a), I::LocalGet(b));
    let r = fe.new_local(ValType::I64);
    match op {
        B::Add => {
            fe.ins.extend([la.clone(), lb.clone(), I::I64Add, I::LocalSet(r)]);
            if signed {
                fe.ins.extend([
                    la,
                    I::LocalGet(r),
                    I::I64Xor,
                    lb,
                    I::LocalGet(r),
                    I::I64Xor,
                    I::I64And,
                    I::I64Const(0),
                    I::I64LtS,
                ]);
            } else {
                fe.ins.extend([I::LocalGet(r), la, I::I64LtU]);
            }
            fe.trap_if();
            fe.ins.push(I::LocalGet(r));
        }
        B::Sub => {
            fe.ins.extend([la.clone(), lb.clone(), I::I64Sub, I::LocalSet(r)]);
            if signed {
                fe.ins.extend([
                    la.clone(),
                    lb,
                    I::I64Xor,
                    la,
                    I::LocalGet(r),
                    I::I64Xor,
                    I::I64And,
                    I::I64Const(0),
                    I::I64LtS,
                ]);
            } else {
                fe.ins.extend([la, lb, I::I64LtU]);
            }
            fe.trap_if();
            fe.ins.push(I::LocalGet(r));
        }
        B::Mul => {
            // r = a * b; overflow iff a != 0 and r / a != b. (The signed MIN * -1 case traps
            // in the division itself, which is the overflow trap we want.)
            fe.ins.extend([la.clone(), lb.clone(), I::I64Mul, I::LocalSet(r)]);
            fe.ins.extend([la.clone(), I::I64Const(0), I::I64Ne, I::If(BlockType::Empty)]);
            fe.ins.extend([
                I::LocalGet(r),
                la,
                if signed { I::I64DivS } else { I::I64DivU },
                lb,
                I::I64Ne,
            ]);
            fe.trap_if();
            fe.ins.extend([I::End, I::LocalGet(r)]);
        }
        B::Div => fe.ins.extend([la, lb, if signed { I::I64DivS } else { I::I64DivU }]),
        B::Rem => fe.ins.extend([la, lb, if signed { I::I64RemS } else { I::I64RemU }]),
        B::WrappingAdd => fe.ins.extend([la, lb, I::I64Add]),
        B::WrappingSub => fe.ins.extend([la, lb, I::I64Sub]),
        B::WrappingMul => fe.ins.extend([la, lb, I::I64Mul]),
        B::Shl | B::Shr => {
            // The amount is a u32.
            fe.ins.extend([lb.clone(), I::I32Const(64), I::I32GeU]);
            fe.trap_if();
            fe.ins.extend([
                la,
                lb,
                I::I64ExtendI32U,
                match (op, signed) {
                    (B::Shl, _) => I::I64Shl,
                    (_, true) => I::I64ShrS,
                    _ => I::I64ShrU,
                },
            ]);
        }
        B::BitAnd => fe.ins.extend([la, lb, I::I64And]),
        B::BitOr => fe.ins.extend([la, lb, I::I64Or]),
        B::BitXor => fe.ins.extend([la, lb, I::I64Xor]),
        B::Eq => fe.ins.extend([la, lb, I::I64Eq]),
        B::Ne => fe.ins.extend([la, lb, I::I64Ne]),
        B::Lt => fe.ins.extend([la, lb, if signed { I::I64LtS } else { I::I64LtU }]),
        B::Le => fe.ins.extend([la, lb, if signed { I::I64LeS } else { I::I64LeU }]),
        B::Gt => fe.ins.extend([la, lb, if signed { I::I64GtS } else { I::I64GtU }]),
        B::Ge => fe.ins.extend([la, lb, if signed { I::I64GeS } else { I::I64GeU }]),
        B::And | B::Or => return Err("internal: logical op on i64".into()),
    }
    Ok(())
}

// ---- vectors and matrices ---------------------------------------------------------------------

/// Loads component `c` of value `x` (a vector) into a new f32 local, or returns the local of
/// a scalar `x` (broadcast).
fn comp(fe: &mut Fe, x: ir::ValueId, c: u32) -> u32 {
    if fe.m.types.is_aggregate(fe.vty(x)) {
        let l = fe.new_local(ValType::F32);
        fe.ins.extend([I::LocalGet(fe.v(x)), I::F32Load(mem(4 * c, 2)), I::LocalSet(l)]);
        l
    } else {
        fe.v(x)
    }
}

/// Loads element (column `j`, row `i`) of matrix value `m` of size `n` into a new local.
fn mat_elem(fe: &mut Fe, m: ir::ValueId, n: u8, j: u32, i: u32) -> u32 {
    let stride = column_stride(n);
    let l = fe.new_local(ValType::F32);
    fe.ins.extend([I::LocalGet(fe.v(m)), I::F32Load(mem(stride * j + 4 * i, 2)), I::LocalSet(l)]);
    l
}

fn dims(fe: &Fe, t: ir::TypeId) -> Option<(bool, u8)> {
    match fe.m.types.get(t) {
        ir::TypeDef::Vector(n) => Some((false, *n)),
        ir::TypeDef::Matrix(n) => Some((true, *n)),
        _ => None,
    }
}

pub(super) fn binary(
    fe: &mut Fe,
    v: ir::ValueId,
    op: ir::BinOp,
    a: ir::ValueId,
    b: ir::ValueId,
) -> R<()> {
    let t = fe.vty(v);
    let (ta, tb) = (fe.vty(a), fe.vty(b));
    if !fe.m.types.is_aggregate(t) && !fe.m.types.is_aggregate(ta) {
        let s = fe.scalar(ta)?;
        scalar_bin(fe, op, s, fe.v(a), fe.v(b))?;
        fe.ins.push(I::LocalSet(fe.v(v)));
        return Ok(());
    }
    let out = fe.fresh_slot(v)?;
    match (dims(fe, ta), dims(fe, tb), dims(fe, t)) {
        // matrix × vector: r_i = Σ_j m[j][i] v[j]
        (Some((true, n)), Some((false, _)), _) if op == ir::BinOp::Mul => {
            for i in 0..n as u32 {
                let acc = fe.new_local(ValType::F32);
                fe.ins.extend([I::F32Const(0.0f32.into()), I::LocalSet(acc)]);
                for j in 0..n as u32 {
                    let e = mat_elem(fe, a, n, j, i);
                    let x = comp(fe, b, j);
                    fe.ins.extend([
                        I::LocalGet(acc),
                        I::LocalGet(e),
                        I::LocalGet(x),
                        I::F32Mul,
                        I::F32Add,
                        I::LocalSet(acc),
                    ]);
                }
                fe.ins.extend([I::LocalGet(out), I::LocalGet(acc), I::F32Store(mem(4 * i, 2))]);
            }
        }
        // vector × matrix: r_j = Σ_i v[i] m[j][i]
        (Some((false, _)), Some((true, n)), _) if op == ir::BinOp::Mul => {
            for j in 0..n as u32 {
                let acc = fe.new_local(ValType::F32);
                fe.ins.extend([I::F32Const(0.0f32.into()), I::LocalSet(acc)]);
                for i in 0..n as u32 {
                    let x = comp(fe, a, i);
                    let e = mat_elem(fe, b, n, j, i);
                    fe.ins.extend([
                        I::LocalGet(acc),
                        I::LocalGet(x),
                        I::LocalGet(e),
                        I::F32Mul,
                        I::F32Add,
                        I::LocalSet(acc),
                    ]);
                }
                fe.ins.extend([I::LocalGet(out), I::LocalGet(acc), I::F32Store(mem(4 * j, 2))]);
            }
        }
        // matrix × matrix: column j of the result is a × b[j].
        (Some((true, n)), Some((true, _)), _) if op == ir::BinOp::Mul => {
            let stride = column_stride(n);
            for j in 0..n as u32 {
                for i in 0..n as u32 {
                    let acc = fe.new_local(ValType::F32);
                    fe.ins.extend([I::F32Const(0.0f32.into()), I::LocalSet(acc)]);
                    for k in 0..n as u32 {
                        let x = mat_elem(fe, a, n, k, i);
                        let y = mat_elem(fe, b, n, j, k);
                        fe.ins.extend([
                            I::LocalGet(acc),
                            I::LocalGet(x),
                            I::LocalGet(y),
                            I::F32Mul,
                            I::F32Add,
                            I::LocalSet(acc),
                        ]);
                    }
                    fe.ins.extend([
                        I::LocalGet(out),
                        I::LocalGet(acc),
                        I::F32Store(mem(stride * j + 4 * i, 2)),
                    ]);
                }
            }
        }
        // Elementwise: vector op vector/scalar, matrix ± matrix, matrix × scalar.
        (_, _, Some((is_mat, n))) => {
            let stride = if is_mat { column_stride(n) } else { 0 };
            let cols = if is_mat { n as u32 } else { 1 };
            for j in 0..cols {
                for i in 0..n as u32 {
                    let off = stride * j + 4 * i;
                    let x = elem_or_scalar(fe, a, off);
                    let y = elem_or_scalar(fe, b, off);
                    scalar_bin(fe, op, ir::Scalar::F32, x, y)?;
                    let r = fe.new_local(ValType::F32);
                    fe.ins.extend([
                        I::LocalSet(r),
                        I::LocalGet(out),
                        I::LocalGet(r),
                        I::F32Store(mem(off, 2)),
                    ]);
                }
            }
        }
        _ => {
            return Err(format!(
                "internal: {op:?} on {} and {}",
                fe.m.types.display(ta),
                fe.m.types.display(tb)
            ));
        }
    }
    Ok(())
}

/// The f32 at byte `off` of aggregate `x`, or scalar `x` itself.
fn elem_or_scalar(fe: &mut Fe, x: ir::ValueId, off: u32) -> u32 {
    if fe.m.types.is_aggregate(fe.vty(x)) {
        let l = fe.new_local(ValType::F32);
        fe.ins.extend([I::LocalGet(fe.v(x)), I::F32Load(mem(off, 2)), I::LocalSet(l)]);
        l
    } else {
        fe.v(x)
    }
}

pub(super) fn unary(fe: &mut Fe, v: ir::ValueId, op: ir::UnOp, x: ir::ValueId) -> R<()> {
    let t = fe.vty(x);
    if let Some((is_mat, n)) = dims(fe, t) {
        let out = fe.fresh_slot(v)?;
        let stride = if is_mat { column_stride(n) } else { 0 };
        for j in 0..if is_mat { n as u32 } else { 1 } {
            for i in 0..n as u32 {
                let off = stride * j + 4 * i;
                fe.ins.extend([
                    I::LocalGet(out),
                    I::LocalGet(fe.v(x)),
                    I::F32Load(mem(off, 2)),
                    I::F32Neg,
                    I::F32Store(mem(off, 2)),
                ]);
            }
        }
        return Ok(());
    }
    let s = fe.scalar(t)?;
    match (op, s) {
        (ir::UnOp::Neg, ir::Scalar::F32) => fe.ins.extend([I::LocalGet(fe.v(x)), I::F32Neg]),
        (ir::UnOp::Neg, ir::Scalar::F64) => fe.ins.extend([I::LocalGet(fe.v(x)), I::F64Neg]),
        (ir::UnOp::Neg, s) => {
            // 0 - x, checked: negating the minimum overflows.
            let z = fe.new_local(super::super::valtype(s));
            fe.ins.extend([konst(s, 0.0), I::LocalSet(z)]);
            scalar_bin(fe, ir::BinOp::Sub, s, z, fe.v(x))?;
        }
        (ir::UnOp::Not, ir::Scalar::Bool) => fe.ins.extend([I::LocalGet(fe.v(x)), I::I32Eqz]),
        (ir::UnOp::Not, s) if is64(s) => {
            fe.ins.extend([I::LocalGet(fe.v(x)), I::I64Const(-1), I::I64Xor])
        }
        (ir::UnOp::Not, s) => {
            fe.ins.extend([I::LocalGet(fe.v(x)), I::I32Const(-1), I::I32Xor]);
            wrap_small(fe, s);
        }
    }
    fe.ins.push(I::LocalSet(fe.v(v)));
    Ok(())
}

/// Converts the scalar on top of the stack from `from` to `to`. Float to int truncates and traps
/// out of range; int to int keeps the low bits; bool to a number is 0 or 1.
pub(super) fn convert(fe: &mut Fe, from: ir::Scalar, to: ir::Scalar) -> R<()> {
    use ir::Scalar as S;
    if from == to {
        return Ok(());
    }
    let from_int = from.is_int() || from == S::Bool;
    match (from_int, to.is_float(), is64(from), is64(to)) {
        // int -> int
        (true, false, false, false) => wrap_small(fe, to),
        (true, false, true, false) => {
            fe.ins.push(I::I32WrapI64);
            wrap_small(fe, to);
        }
        (true, false, false, true) => {
            fe.ins.push(if from.signed() { I::I64ExtendI32S } else { I::I64ExtendI32U })
        }
        (true, false, true, true) => {}
        // int -> float
        (true, true, w64, _) => fe.ins.push(match (to, w64, from.signed()) {
            (S::F32, false, true) => I::F32ConvertI32S,
            (S::F32, false, false) => I::F32ConvertI32U,
            (S::F32, true, true) => I::F32ConvertI64S,
            (S::F32, true, false) => I::F32ConvertI64U,
            (_, false, true) => I::F64ConvertI32S,
            (_, false, false) => I::F64ConvertI32U,
            (_, true, true) => I::F64ConvertI64S,
            (_, true, false) => I::F64ConvertI64U,
        }),
        // float -> float
        (false, true, _, _) => {
            fe.ins.push(if to == S::F32 { I::F32DemoteF64 } else { I::F64PromoteF32 })
        }
        // float -> int: the trapping truncations.
        (false, false, _, _) => {
            let f64_ = from == S::F64;
            match to {
                S::I64 => fe.ins.push(if f64_ { I::I64TruncF64S } else { I::I64TruncF32S }),
                S::U64 => fe.ins.push(if f64_ { I::I64TruncF64U } else { I::I64TruncF32U }),
                S::U32 => fe.ins.push(if f64_ { I::I32TruncF64U } else { I::I32TruncF32U }),
                S::Bool => return Err("internal: float to bool".into()),
                _ => {
                    fe.ins.push(if f64_ { I::I32TruncF64S } else { I::I32TruncF32S });
                    check_small(fe, to);
                }
            }
        }
    }
    Ok(())
}

// ---- built-ins --------------------------------------------------------------------------------

/// Pushes the scalar built-in `b` of the f32 locals `xs`.
fn scalar_builtin(fe: &mut Fe, b: ir::Builtin, s: ir::Scalar, xs: &[u32]) -> R<()> {
    use ir::Builtin as B;
    let f64_ = s == ir::Scalar::F64;
    let g = |i: usize| I::LocalGet(xs[i]);
    let fone = || if f64_ { I::F64Const(1.0f64.into()) } else { I::F32Const(1.0f32.into()) };
    let fzero = || if f64_ { I::F64Const(0.0f64.into()) } else { I::F32Const(0.0f32.into()) };
    if s.is_int() {
        let signed = s.signed();
        match b {
            B::Min | B::Max => {
                let lt = if is64(s) {
                    if signed { I::I64LtS } else { I::I64LtU }
                } else if signed {
                    I::I32LtS
                } else {
                    I::I32LtU
                };
                // select(a, b, a < b) for min.
                fe.ins.extend([g(0), g(1), g(0), g(1), lt]);
                if b == B::Max {
                    fe.ins.push(I::I32Eqz);
                }
                fe.ins.push(I::Select);
            }
            B::Clamp => {
                let lo = fe.new_local(super::super::valtype(s));
                scalar_builtin(fe, B::Max, s, &[xs[0], xs[1]])?;
                fe.ins.push(I::LocalSet(lo));
                scalar_builtin(fe, B::Min, s, &[lo, xs[2]])?;
            }
            B::Abs if signed => {
                let z = fe.new_local(super::super::valtype(s));
                fe.ins.extend([konst(s, 0.0), I::LocalSet(z)]);
                let neg = fe.new_local(super::super::valtype(s));
                scalar_bin(fe, ir::BinOp::Sub, s, z, xs[0])?;
                fe.ins.push(I::LocalSet(neg));
                let lt = if is64(s) { I::I64LtS } else { I::I32LtS };
                fe.ins.extend([I::LocalGet(neg), g(0), g(0), konst(s, 0.0), lt, I::Select]);
            }
            B::Abs => fe.ins.push(g(0)),
            B::Sign => {
                let (gt, lt) =
                    if is64(s) { (I::I64GtS, I::I64LtS) } else { (I::I32GtS, I::I32LtS) };
                fe.ins.extend([g(0), konst(s, 0.0), gt, g(0), konst(s, 0.0), lt, I::I32Sub]);
                if is64(s) {
                    fe.ins.push(I::I64ExtendI32S);
                }
            }
            _ => return Err(format!("internal: {b:?} on integers")),
        }
        return Ok(());
    }
    let (add, sub, mul, div) = if f64_ {
        (I::F64Add, I::F64Sub, I::F64Mul, I::F64Div)
    } else {
        (I::F32Add, I::F32Sub, I::F32Mul, I::F32Div)
    };
    match b {
        B::Sqrt => fe.ins.extend([g(0), if f64_ { I::F64Sqrt } else { I::F32Sqrt }]),
        B::InverseSqrt => {
            fe.ins.extend([fone(), g(0), if f64_ { I::F64Sqrt } else { I::F32Sqrt }, div])
        }
        B::Floor => fe.ins.extend([g(0), if f64_ { I::F64Floor } else { I::F32Floor }]),
        B::Ceil => fe.ins.extend([g(0), if f64_ { I::F64Ceil } else { I::F32Ceil }]),
        B::Trunc => fe.ins.extend([g(0), if f64_ { I::F64Trunc } else { I::F32Trunc }]),
        B::Round => fe.ins.extend([g(0), if f64_ { I::F64Nearest } else { I::F32Nearest }]),
        B::Fract => fe.ins.extend([g(0), g(0), if f64_ { I::F64Floor } else { I::F32Floor }, sub]),
        B::Abs => fe.ins.extend([g(0), if f64_ { I::F64Abs } else { I::F32Abs }]),
        B::Sign => {
            // 1 if x > 0, -1 if x < 0, else x (keeping 0, -0 and NaN).
            let (gt, lt) = if f64_ { (I::F64Gt, I::F64Lt) } else { (I::F32Gt, I::F32Lt) };
            let neg_one =
                if f64_ { I::F64Const((-1.0f64).into()) } else { I::F32Const((-1.0f32).into()) };
            fe.ins.extend([
                fone(),
                neg_one,
                g(0),
                g(0),
                fzero(),
                lt,
                I::Select,
                g(0),
                fzero(),
                gt,
                I::Select,
            ]);
        }
        B::Min => fe.ins.extend([g(0), g(1), if f64_ { I::F64Min } else { I::F32Min }]),
        B::Max => fe.ins.extend([g(0), g(1), if f64_ { I::F64Max } else { I::F32Max }]),
        B::Clamp => fe.ins.extend([
            g(0),
            g(1),
            if f64_ { I::F64Max } else { I::F32Max },
            g(2),
            if f64_ { I::F64Min } else { I::F32Min },
        ]),
        B::Saturate => fe.ins.extend([
            g(0),
            fzero(),
            if f64_ { I::F64Max } else { I::F32Max },
            fone(),
            if f64_ { I::F64Min } else { I::F32Min },
        ]),
        B::Mix => {
            // WGSL: e1 * (1 - e3) + e2 * e3.
            fe.ins.extend([g(0), fone(), g(2), sub.clone(), mul.clone(), g(1), g(2), mul, add]);
        }
        B::Step => {
            // 1 if edge <= x.
            fe.ins.extend([
                fone(),
                fzero(),
                g(0),
                g(1),
                if f64_ { I::F64Le } else { I::F32Le },
                I::Select,
            ]);
        }
        B::Smoothstep => {
            // t = clamp((x - e0) / (e1 - e0), 0, 1); t * t * (3 - 2t)
            let t = fe.new_local(if f64_ { ValType::F64 } else { ValType::F32 });
            let (mx, mn) = if f64_ { (I::F64Max, I::F64Min) } else { (I::F32Max, I::F32Min) };
            fe.ins.extend([
                g(2),
                g(0),
                sub.clone(),
                g(1),
                g(0),
                sub.clone(),
                div,
                fzero(),
                mx,
                fone(),
                mn,
                I::LocalSet(t),
            ]);
            let three = if f64_ { I::F64Const(3.0f64.into()) } else { I::F32Const(3.0f32.into()) };
            let two = if f64_ { I::F64Const(2.0f64.into()) } else { I::F32Const(2.0f32.into()) };
            fe.ins.extend([
                I::LocalGet(t),
                I::LocalGet(t),
                mul.clone(),
                three,
                two,
                I::LocalGet(t),
                mul.clone(),
                sub,
                mul,
            ]);
        }
        B::Length => fe.ins.extend([g(0), if f64_ { I::F64Abs } else { I::F32Abs }]),
        B::Distance => fe.ins.extend([g(0), g(1), sub, if f64_ { I::F64Abs } else { I::F32Abs }]),
        _ => {
            return Err(format!(
                "internal: the built-in {b:?} should have been lowered before the WASM back end"
            ));
        }
    }
    Ok(())
}

pub(super) fn builtin(fe: &mut Fe, v: ir::ValueId, b: ir::Builtin, args: &[ir::ValueId]) -> R<()> {
    use ir::Builtin as B;
    let t = fe.vty(v);
    let arg0 = fe.vty(args[0]);
    // Vector reductions.
    if let Some((false, n)) = dims(fe, arg0) {
        match b {
            B::Dot | B::Length | B::Distance | B::AllEqual => {
                let n = n as u32;
                let acc = fe.new_local(if b == B::AllEqual { ValType::I32 } else { ValType::F32 });
                // For distance: the difference first.
                let diffs: Vec<u32> = if b == B::Distance {
                    (0..n)
                        .map(|c| {
                            let x = comp(fe, args[0], c);
                            let y = comp(fe, args[1], c);
                            let d = fe.new_local(ValType::F32);
                            fe.ins.extend([
                                I::LocalGet(x),
                                I::LocalGet(y),
                                I::F32Sub,
                                I::LocalSet(d),
                            ]);
                            d
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                fe.ins.extend([
                    if b == B::AllEqual { I::I32Const(1) } else { I::F32Const(0.0f32.into()) },
                    I::LocalSet(acc),
                ]);
                for c in 0..n {
                    let (x, y) = match b {
                        B::Dot | B::AllEqual => (comp(fe, args[0], c), comp(fe, args[1], c)),
                        B::Length => {
                            let x = comp(fe, args[0], c);
                            (x, x)
                        }
                        _ => (diffs[c as usize], diffs[c as usize]),
                    };
                    if b == B::AllEqual {
                        fe.ins.extend([
                            I::LocalGet(acc),
                            I::LocalGet(x),
                            I::LocalGet(y),
                            I::F32Eq,
                            I::I32And,
                            I::LocalSet(acc),
                        ]);
                    } else if c == 0 {
                        fe.ins.extend([
                            I::LocalGet(x),
                            I::LocalGet(y),
                            I::F32Mul,
                            I::LocalSet(acc),
                        ]);
                    } else {
                        fe.ins.extend([
                            I::LocalGet(acc),
                            I::LocalGet(x),
                            I::LocalGet(y),
                            I::F32Mul,
                            I::F32Add,
                            I::LocalSet(acc),
                        ]);
                    }
                }
                fe.ins.push(I::LocalGet(acc));
                if matches!(b, B::Length | B::Distance) {
                    fe.ins.push(I::F32Sqrt);
                }
                fe.ins.push(I::LocalSet(fe.v(v)));
                return Ok(());
            }
            B::Cross => {
                let out = fe.fresh_slot(v)?;
                let a: Vec<u32> = (0..3).map(|c| comp(fe, args[0], c)).collect();
                let bb: Vec<u32> = (0..3).map(|c| comp(fe, args[1], c)).collect();
                for (i, (j, k)) in [(1usize, 2usize), (2, 0), (0, 1)].into_iter().enumerate() {
                    fe.ins.extend([
                        I::LocalGet(out),
                        I::LocalGet(a[j]),
                        I::LocalGet(bb[k]),
                        I::F32Mul,
                        I::LocalGet(a[k]),
                        I::LocalGet(bb[j]),
                        I::F32Mul,
                        I::F32Sub,
                        I::F32Store(mem(4 * i as u32, 2)),
                    ]);
                }
                return Ok(());
            }
            B::Normalize => {
                let len = fe.new_local(ValType::F32);
                fe.ins.extend([I::F32Const(0.0f32.into()), I::LocalSet(len)]);
                for c in 0..n as u32 {
                    let x = comp(fe, args[0], c);
                    if c == 0 {
                        fe.ins.extend([
                            I::LocalGet(x),
                            I::LocalGet(x),
                            I::F32Mul,
                            I::LocalSet(len),
                        ]);
                    } else {
                        fe.ins.extend([
                            I::LocalGet(len),
                            I::LocalGet(x),
                            I::LocalGet(x),
                            I::F32Mul,
                            I::F32Add,
                            I::LocalSet(len),
                        ]);
                    }
                }
                fe.ins.extend([I::LocalGet(len), I::F32Sqrt, I::LocalSet(len)]);
                let out = fe.fresh_slot(v)?;
                for c in 0..n as u32 {
                    let x = comp(fe, args[0], c);
                    fe.ins.extend([
                        I::LocalGet(out),
                        I::LocalGet(x),
                        I::LocalGet(len),
                        I::F32Div,
                        I::F32Store(mem(4 * c, 2)),
                    ]);
                }
                return Ok(());
            }
            _ => {}
        }
    }
    // Componentwise on vectors (scalar arguments broadcast), or plain scalars.
    match dims(fe, t) {
        Some((false, n)) => {
            let out = fe.fresh_slot(v)?;
            for c in 0..n as u32 {
                let xs: Vec<u32> = args.iter().map(|&a| comp(fe, a, c)).collect();
                scalar_builtin(fe, b, ir::Scalar::F32, &xs)?;
                let r = fe.new_local(ValType::F32);
                fe.ins.extend([
                    I::LocalSet(r),
                    I::LocalGet(out),
                    I::LocalGet(r),
                    I::F32Store(mem(4 * c, 2)),
                ]);
            }
        }
        Some((true, _)) => return Err(format!("internal: {b:?} on a matrix")),
        None => {
            let s = fe.scalar(arg0)?;
            let xs: Vec<u32> = args.iter().map(|&a| fe.v(a)).collect();
            scalar_builtin(fe, b, s, &xs)?;
            fe.ins.push(I::LocalSet(fe.v(v)));
        }
    }
    Ok(())
}

// ---- host operations: the command stream ------------------------------------------------------

/// Stores the i32 `x` at `[a] + off`.
fn put(fe: &mut Fe, a: u32, off: u32, x: I<'static>) {
    fe.ins.extend([I::LocalGet(a), x, I::I32Store(mem(off, 2))]);
}

pub(super) fn host(
    fe: &mut Fe,
    op: &ir::HostOp,
    args: &[ir::ValueId],
    result: Option<ir::ValueId>,
) -> R<()> {
    use wrela_abi::stream::Opcode;
    let h = *fe.helpers;
    match op {
        ir::HostOp::CreateBuffer { elem_size } => {
            // size = max(count * elem_size, 4), checked; handle = next handle.
            let size = fe.new_local(ValType::I32);
            let wide = fe.new_local(ValType::I64);
            fe.ins.extend([
                I::LocalGet(fe.v(args[0])),
                I::I64ExtendI32U,
                I::I64Const(*elem_size as i64),
                I::I64Mul,
                I::LocalTee(wide),
                I::I64Const(u32::MAX as i64),
                I::I64GtU,
            ]);
            fe.trap_if();
            fe.ins.extend([
                I::LocalGet(wide),
                I::I32WrapI64,
                I::LocalTee(size),
                I::I32Eqz,
                I::If(BlockType::Empty),
                I::I32Const(4),
                I::LocalSet(size),
                I::End,
            ]);
            let handle = fe.new_local(ValType::I32);
            fe.ins.extend([
                I::GlobalGet(globals::NEXT_HANDLE),
                I::LocalTee(handle),
                I::I32Const(1),
                I::I32Add,
                I::GlobalSet(globals::NEXT_HANDLE),
            ]);
            let a = fe.new_local(ValType::I32);
            fe.ins.extend([I::I32Const(16), I::Call(h.reserve), I::LocalSet(a)]);
            put(fe, a, 0, I::I32Const(Opcode::CreateBuffer as i32));
            put(fe, a, 4, I::I32Const(8));
            put(fe, a, 8, I::LocalGet(handle));
            put(fe, a, 12, I::LocalGet(size));
            if let Some(v) = result {
                fe.ins.extend([I::LocalGet(handle), I::LocalSet(fe.v(v))]);
            }
        }
        ir::HostOp::WriteBuffer { elem_size } => {
            let (handle, at, run) = (fe.v(args[0]), fe.v(args[1]), fe.v(args[2]));
            let check = |fe: &mut Fe, x: I<'static>| {
                let w = fe.new_local(ValType::I64);
                fe.ins.extend([
                    x,
                    I::I64ExtendI32U,
                    I::I64Const(*elem_size as i64),
                    I::I64Mul,
                    I::LocalTee(w),
                    I::I64Const(u32::MAX as i64),
                    I::I64GtU,
                ]);
                fe.trap_if();
                fe.ins.extend([I::LocalGet(w), I::I32WrapI64]);
            };
            fe.ins.push(I::LocalGet(handle));
            check(fe, I::LocalGet(at));
            fe.ins.extend([I::LocalGet(run), I::I32Load(mem(0, 2))]);
            fe.ins.extend([I::LocalGet(run), I::I32Load(mem(4, 2))]);
            let n = fe.new_local(ValType::I32);
            fe.ins.push(I::LocalSet(n));
            check(fe, I::LocalGet(n));
            fe.ins.push(I::Call(h.write));
        }
        ir::HostOp::Dispatch { pipeline, buffers, uniform }
        | ir::HostOp::Draw { pipeline, buffers, uniform } => {
            let dispatch = matches!(op, ir::HostOp::Dispatch { .. });
            let fixed = if dispatch { 4 } else { 3 };
            let nb = *buffers as usize;
            let usize_ = uniform.map_or(0, |t| round_up(16, layout(&fe.m.types, t).size));
            let payload = 4 * (fixed + 1 + nb as u32 + 1) + usize_;
            let a = fe.new_local(ValType::I32);
            fe.ins.extend([I::I32Const(8 + payload as i32), I::Call(h.reserve), I::LocalSet(a)]);
            let opc = if dispatch { Opcode::Dispatch } else { Opcode::Draw };
            put(fe, a, 0, I::I32Const(opc as i32));
            put(fe, a, 4, I::I32Const(payload as i32));
            put(fe, a, 8, I::I32Const(*pipeline as i32));
            for (k, x) in args.iter().take(fixed as usize - 1).enumerate() {
                put(fe, a, 12 + 4 * k as u32, I::LocalGet(fe.v(*x)));
            }
            let mut off = 8 + 4 * fixed;
            put(fe, a, off, I::I32Const(nb as i32));
            off += 4;
            for k in 0..nb {
                put(fe, a, off, I::LocalGet(fe.v(args[fixed as usize - 1 + k])));
                off += 4;
            }
            put(fe, a, off, I::I32Const(usize_ as i32));
            off += 4;
            if let Some(t) = uniform {
                let u = fe.v(args[fixed as usize - 1 + nb]);
                let size = layout(&fe.m.types, *t).size;
                fe.ins.extend([
                    I::LocalGet(a),
                    I::I32Const(off as i32),
                    I::I32Add,
                    I::I32Const(0),
                    I::I32Const(usize_ as i32),
                    I::MemoryFill(0),
                ]);
                fe.ins.extend([
                    I::LocalGet(a),
                    I::I32Const(off as i32),
                    I::I32Add,
                    I::LocalGet(u),
                    I::I32Const(size as i32),
                    I::MemoryCopy { src_mem: 0, dst_mem: 0 },
                ]);
            }
        }
        ir::HostOp::BeginScreenPass => {
            let a = fe.new_local(ValType::I32);
            fe.ins.extend([I::I32Const(24), I::Call(h.reserve), I::LocalSet(a)]);
            put(fe, a, 0, I::I32Const(Opcode::BeginScreenPass as i32));
            put(fe, a, 4, I::I32Const(16));
            fe.ins.extend([
                I::LocalGet(a),
                I::I32Const(8),
                I::I32Add,
                I::LocalGet(fe.v(args[0])),
                I::I32Const(16),
                I::MemoryCopy { src_mem: 0, dst_mem: 0 },
            ]);
        }
        ir::HostOp::Present => {
            let a = fe.new_local(ValType::I32);
            fe.ins.extend([I::I32Const(8), I::Call(h.reserve), I::LocalSet(a)]);
            put(fe, a, 0, I::I32Const(Opcode::Present as i32));
            put(fe, a, 4, I::I32Const(0));
        }
    }
    Ok(())
}

/// The helpers' bodies, in `Helpers` order after the import: flush, reserve, write.
pub(super) fn helper_bodies(h: &Helpers) -> Vec<(Vec<ValType>, Vec<ValType>, Function)> {
    use memory::{CMD_BASE, CMD_CAP};
    let finish = |locals: Vec<ValType>, ins: Vec<I<'static>>| {
        let mut f = Function::new(locals.iter().map(|t| (1, *t)));
        for i in &ins {
            f.instruction(i);
        }
        f
    };
    // flush(): header, then submit(CMD_BASE, 12 + len); len = 0.
    let magic = u32::from_le_bytes(wrela_abi::stream::MAGIC) as i32;
    let flush = vec![
        I::GlobalGet(globals::CMD_LEN),
        I::If(BlockType::Empty),
        I::I32Const(CMD_BASE as i32),
        I::I32Const(magic),
        I::I32Store(mem(0, 2)),
        I::I32Const(CMD_BASE as i32),
        I::I32Const(wrela_abi::stream::VERSION as i32),
        I::I32Store(mem(4, 2)),
        I::I32Const(CMD_BASE as i32),
        I::GlobalGet(globals::CMD_LEN),
        I::I32Store(mem(8, 2)),
        I::I32Const(CMD_BASE as i32),
        I::GlobalGet(globals::CMD_LEN),
        I::I32Const(12),
        I::I32Add,
        I::Call(h.submit),
        I::I32Const(0),
        I::GlobalSet(globals::CMD_LEN),
        I::End,
        I::End,
    ];
    // reserve(n) -> addr
    let reserve = vec![
        I::GlobalGet(globals::CMD_LEN),
        I::LocalGet(0),
        I::I32Add,
        I::I32Const(CMD_CAP as i32),
        I::I32GtU,
        I::If(BlockType::Empty),
        I::Call(h.flush),
        I::LocalGet(0),
        I::I32Const(CMD_CAP as i32),
        I::I32GtU,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End,
        I::End,
        I::I32Const(CMD_BASE as i32 + 12),
        I::GlobalGet(globals::CMD_LEN),
        I::I32Add,
        I::GlobalGet(globals::CMD_LEN),
        I::LocalGet(0),
        I::I32Add,
        I::GlobalSet(globals::CMD_LEN),
        I::End,
    ];
    // write(handle, offset, ptr, n): WriteBuffer commands of at most CHUNK bytes each.
    let chunk_max = (CMD_CAP - 64) & !3;
    let (handle, offset, ptr, n, chunk, a) = (0, 1, 2, 3, 4, 5);
    let mut write = vec![I::Block(BlockType::Empty), I::Loop(BlockType::Empty)];
    write.extend([I::LocalGet(n), I::I32Eqz, I::BrIf(1)]);
    write.extend([
        I::LocalGet(n),
        I::I32Const(chunk_max as i32),
        I::LocalGet(n),
        I::I32Const(chunk_max as i32),
        I::I32LtU,
        I::Select,
        I::LocalSet(chunk),
    ]);
    write.extend([
        I::LocalGet(chunk),
        I::I32Const(20),
        I::I32Add,
        I::Call(h.reserve),
        I::LocalSet(a),
    ]);
    let w = |x: I<'static>, off: u32| [I::LocalGet(a), x, I::I32Store(mem(off, 2))];
    write.extend(w(I::I32Const(wrela_abi::stream::Opcode::WriteBuffer as i32), 0));
    write.extend([
        I::LocalGet(a),
        I::LocalGet(chunk),
        I::I32Const(12),
        I::I32Add,
        I::I32Store(mem(4, 2)),
    ]);
    write.extend(w(I::LocalGet(handle), 8));
    write.extend(w(I::LocalGet(offset), 12));
    write.extend(w(I::LocalGet(chunk), 16));
    write.extend([
        I::LocalGet(a),
        I::I32Const(20),
        I::I32Add,
        I::LocalGet(ptr),
        I::LocalGet(chunk),
        I::MemoryCopy { src_mem: 0, dst_mem: 0 },
    ]);
    write.extend([I::LocalGet(offset), I::LocalGet(chunk), I::I32Add, I::LocalSet(offset)]);
    write.extend([I::LocalGet(ptr), I::LocalGet(chunk), I::I32Add, I::LocalSet(ptr)]);
    write.extend([I::LocalGet(n), I::LocalGet(chunk), I::I32Sub, I::LocalSet(n)]);
    write.extend([I::Br(0), I::End, I::End, I::End]);
    let i32x = |k| vec![ValType::I32; k];
    vec![
        (vec![], vec![], finish(vec![], flush)),
        (i32x(1), i32x(1), finish(vec![], reserve)),
        (i32x(4), vec![], finish(i32x(2), write)),
    ]
}
