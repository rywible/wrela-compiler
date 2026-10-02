//! Operators, conversions, built-ins and host operations.

use super::{Fe, R, konst, mem};
use crate::{Helpers, globals, memory};
use wasm_encoder::{BlockType, Function, Instruction as I, ValType};
use wrela_ir as ir;
use wrela_ir::layout::{column_stride, layout, round_up};

/// A batch's header and a command's header, in bytes (wrela-abi's stream format).
const BATCH_HEADER: u32 = wrela_abi::stream::HEADER_LEN as u32;
const CMD_HEADER: u32 = wrela_abi::stream::COMMAND_HEADER_LEN as u32;

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

/// Float instructions of one width.
#[derive(Clone, Copy)]
struct Fw {
    f64_: bool,
}

macro_rules! float_ops {
    ($($name:ident => $f32:ident, $f64:ident;)*) => {
        impl Fw {
            $(fn $name(self) -> I<'static> { if self.f64_ { I::$f64 } else { I::$f32 } })*
        }
    };
}

float_ops! {
    add => F32Add, F64Add;
    sub => F32Sub, F64Sub;
    mul => F32Mul, F64Mul;
    div => F32Div, F64Div;
    eq => F32Eq, F64Eq;
    ne => F32Ne, F64Ne;
    lt => F32Lt, F64Lt;
    le => F32Le, F64Le;
    gt => F32Gt, F64Gt;
    ge => F32Ge, F64Ge;
    sqrt => F32Sqrt, F64Sqrt;
    floor => F32Floor, F64Floor;
    ceil => F32Ceil, F64Ceil;
    trunc => F32Trunc, F64Trunc;
    nearest => F32Nearest, F64Nearest;
    abs => F32Abs, F64Abs;
    min => F32Min, F64Min;
    max => F32Max, F64Max;
    neg => F32Neg, F64Neg;
}

impl Fw {
    fn of(s: ir::Scalar) -> Fw {
        Fw { f64_: s == ir::Scalar::F64 }
    }

    fn konst(self, x: f64) -> I<'static> {
        if self.f64_ { I::F64Const(x.into()) } else { I::F32Const((x as f32).into()) }
    }

    fn valtype(self) -> ValType {
        if self.f64_ { ValType::F64 } else { ValType::F32 }
    }
}

/// Integer instructions of one width (8- and 16-bit integers are held in i32s).
#[derive(Clone, Copy)]
struct Iw {
    wide: bool,
    signed: bool,
}

macro_rules! int_ops {
    ($($name:ident => $i32:ident, $i64:ident;)*) => {
        impl Iw {
            $(fn $name(self) -> I<'static> { if self.wide { I::$i64 } else { I::$i32 } })*
        }
    };
}

macro_rules! signed_int_ops {
    ($($name:ident => $i32s:ident, $i32u:ident, $i64s:ident, $i64u:ident;)*) => {
        impl Iw {
            $(fn $name(self) -> I<'static> {
                match (self.wide, self.signed) {
                    (false, true) => I::$i32s,
                    (false, false) => I::$i32u,
                    (true, true) => I::$i64s,
                    (true, false) => I::$i64u,
                }
            })*
        }
    };
}

int_ops! {
    add => I32Add, I64Add;
    sub => I32Sub, I64Sub;
    mul => I32Mul, I64Mul;
    and => I32And, I64And;
    or => I32Or, I64Or;
    xor => I32Xor, I64Xor;
    eq => I32Eq, I64Eq;
    ne => I32Ne, I64Ne;
    shl => I32Shl, I64Shl;
    ltu => I32LtU, I64LtU;
    lts => I32LtS, I64LtS;
    gts => I32GtS, I64GtS;
}

signed_int_ops! {
    lt => I32LtS, I32LtU, I64LtS, I64LtU;
    le => I32LeS, I32LeU, I64LeS, I64LeU;
    gt => I32GtS, I32GtU, I64GtS, I64GtU;
    ge => I32GeS, I32GeU, I64GeS, I64GeU;
    div => I32DivS, I32DivU, I64DivS, I64DivU;
    rem => I32RemS, I32RemU, I64RemS, I64RemU;
    shr => I32ShrS, I32ShrU, I64ShrS, I64ShrU;
}

impl Iw {
    fn of(s: ir::Scalar) -> Iw {
        Iw { wide: is64(s), signed: s.signed() }
    }

    fn konst(self, x: i64) -> I<'static> {
        if self.wide { I::I64Const(x) } else { I::I32Const(x as i32) }
    }

    fn valtype(self) -> ValType {
        if self.wide { ValType::I64 } else { ValType::I32 }
    }
}

/// Pushes `a op b` for scalars of type `s`, held in locals `a` and `b`. Integer arithmetic is
/// checked unless the op is a wrapping one.
fn scalar_bin(fe: &mut Fe, op: ir::BinOp, s: ir::Scalar, a: u32, b: u32) -> R<()> {
    use ir::BinOp as B;
    let (la, lb) = (I::LocalGet(a), I::LocalGet(b));
    if s.is_float() {
        let f = Fw::of(s);
        let i = match op {
            B::Add => f.add(),
            B::Sub => f.sub(),
            B::Mul => f.mul(),
            B::Div => f.div(),
            B::Rem => {
                // a - b * trunc(a / b), as WGSL defines it.
                fe.ins.extend([
                    la.clone(),
                    lb.clone(),
                    la,
                    lb,
                    f.div(),
                    f.trunc(),
                    f.mul(),
                    f.sub(),
                ]);
                return Ok(());
            }
            B::Eq => f.eq(),
            B::Ne => f.ne(),
            B::Lt => f.lt(),
            B::Le => f.le(),
            B::Gt => f.gt(),
            B::Ge => f.ge(),
            _ => return Err(format!("internal: {op:?} on floats")),
        };
        fe.ins.extend([la, lb, i]);
        return Ok(());
    }
    let w = Iw::of(s);
    let small = small_range(s).is_some();
    match op {
        B::Add | B::Sub | B::Mul if small => {
            // In 32 bits, then check it fits the small type.
            let i = match op {
                B::Add => w.add(),
                B::Sub => w.sub(),
                _ => w.mul(),
            };
            fe.ins.extend([la, lb, i]);
            check_small(fe, s);
        }
        B::Add | B::Sub => {
            let r = fe.new_local(w.valtype());
            let i = if op == B::Add { w.add() } else { w.sub() };
            fe.ins.extend([la.clone(), lb.clone(), i, I::LocalSet(r)]);
            match (op, w.signed) {
                // Overflow iff both operands' signs differ from the result's.
                (B::Add, true) => fe.ins.extend([
                    la,
                    I::LocalGet(r),
                    w.xor(),
                    lb,
                    I::LocalGet(r),
                    w.xor(),
                    w.and(),
                    w.konst(0),
                    w.lts(),
                ]),
                // Overflow iff the operands' signs differ and the result's differs from a's.
                (B::Sub, true) => fe.ins.extend([
                    la.clone(),
                    lb,
                    w.xor(),
                    la,
                    I::LocalGet(r),
                    w.xor(),
                    w.and(),
                    w.konst(0),
                    w.lts(),
                ]),
                (B::Add, false) => fe.ins.extend([I::LocalGet(r), la, w.ltu()]),
                _ => fe.ins.extend([la, lb, w.ltu()]),
            }
            fe.trap_if();
            fe.ins.push(I::LocalGet(r));
        }
        B::Mul if w.wide => {
            // r = a * b; overflow iff a != 0 and r / a != b. (The signed MIN * -1 case traps in
            // the division itself, which is the overflow trap we want.)
            let r = fe.new_local(ValType::I64);
            fe.ins.extend([la.clone(), lb.clone(), I::I64Mul, I::LocalSet(r)]);
            fe.ins.extend([la.clone(), I::I64Const(0), I::I64Ne, I::If(BlockType::Empty)]);
            fe.ins.extend([I::LocalGet(r), la, w.div(), lb, I::I64Ne]);
            fe.trap_if();
            fe.ins.extend([I::End, I::LocalGet(r)]);
        }
        B::Mul => {
            // In 64 bits, then check it fits.
            let wide = fe.new_local(ValType::I64);
            let ext = if w.signed { I::I64ExtendI32S } else { I::I64ExtendI32U };
            fe.ins.extend([la, ext.clone(), lb, ext, I::I64Mul, I::LocalTee(wide)]);
            if w.signed {
                fe.ins.extend([
                    I::I64Const(i32::MIN as i64),
                    I::I64LtS,
                    I::LocalGet(wide),
                    I::I64Const(i32::MAX as i64),
                    I::I64GtS,
                    I::I32Or,
                ]);
            } else {
                fe.ins.extend([I::I64Const(u32::MAX as i64), I::I64GtU]);
            }
            fe.trap_if();
            fe.ins.extend([I::LocalGet(wide), I::I32WrapI64]);
        }
        B::Div => {
            fe.ins.extend([la, lb, w.div()]);
            check_small(fe, s);
        }
        B::Rem => fe.ins.extend([la, lb, w.rem()]),
        B::WrappingAdd | B::WrappingSub | B::WrappingMul => {
            let i = match op {
                B::WrappingAdd => w.add(),
                B::WrappingSub => w.sub(),
                _ => w.mul(),
            };
            fe.ins.extend([la, lb, i]);
            wrap_small(fe, s);
        }
        B::Shl | B::Shr => {
            // A shift of at least the width traps on the CPU. The amount is a u32.
            fe.ins.extend([lb.clone(), I::I32Const(s.bits() as i32), I::I32GeU]);
            fe.trap_if();
            fe.ins.extend([la, lb]);
            if w.wide {
                fe.ins.push(I::I64ExtendI32U);
            }
            fe.ins.push(if op == B::Shl { w.shl() } else { w.shr() });
            wrap_small(fe, s);
        }
        B::BitAnd => fe.ins.extend([la, lb, w.and()]),
        B::BitOr => fe.ins.extend([la, lb, w.or()]),
        B::And | B::Or if w.wide => return Err("internal: logical op on i64".into()),
        B::And => fe.ins.extend([la, lb, w.and()]),
        B::Or => fe.ins.extend([la, lb, w.or()]),
        B::BitXor => fe.ins.extend([la, lb, w.xor()]),
        B::Eq => fe.ins.extend([la, lb, w.eq()]),
        B::Ne => fe.ins.extend([la, lb, w.ne()]),
        B::Lt => fe.ins.extend([la, lb, w.lt()]),
        B::Le => fe.ins.extend([la, lb, w.le()]),
        B::Gt => fe.ins.extend([la, lb, w.gt()]),
        B::Ge => fe.ins.extend([la, lb, w.ge()]),
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
        (ir::UnOp::Neg, s) if s.is_float() => {
            fe.ins.extend([I::LocalGet(fe.v(x)), Fw::of(s).neg()])
        }
        (ir::UnOp::Neg, s) => {
            // 0 - x, checked: negating the minimum overflows.
            let z = fe.new_local(super::super::valtype(s));
            fe.ins.extend([konst(s, 0.0), I::LocalSet(z)]);
            scalar_bin(fe, ir::BinOp::Sub, s, z, fe.v(x))?;
        }
        (ir::UnOp::Not, ir::Scalar::Bool) => fe.ins.extend([I::LocalGet(fe.v(x)), I::I32Eqz]),
        (ir::UnOp::Not, s) => {
            let w = Iw::of(s);
            fe.ins.extend([I::LocalGet(fe.v(x)), w.konst(-1), w.xor()]);
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
    let g = |i: usize| I::LocalGet(xs[i]);
    if s.is_int() {
        let w = Iw::of(s);
        match b {
            B::Min | B::Max => {
                // select(a, b, a < b) for min.
                fe.ins.extend([g(0), g(1), g(0), g(1), w.lt()]);
                if b == B::Max {
                    fe.ins.push(I::I32Eqz);
                }
                fe.ins.push(I::Select);
            }
            B::Clamp => {
                let lo = fe.new_local(w.valtype());
                scalar_builtin(fe, B::Max, s, &[xs[0], xs[1]])?;
                fe.ins.push(I::LocalSet(lo));
                scalar_builtin(fe, B::Min, s, &[lo, xs[2]])?;
            }
            B::Abs if w.signed => {
                let z = fe.new_local(w.valtype());
                fe.ins.extend([w.konst(0), I::LocalSet(z)]);
                let neg = fe.new_local(w.valtype());
                scalar_bin(fe, ir::BinOp::Sub, s, z, xs[0])?;
                fe.ins.push(I::LocalSet(neg));
                fe.ins.extend([I::LocalGet(neg), g(0), g(0), w.konst(0), w.lts(), I::Select]);
            }
            B::Abs => fe.ins.push(g(0)),
            B::Sign => {
                fe.ins.extend([g(0), w.konst(0), w.gts(), g(0), w.konst(0), w.lts(), I::I32Sub]);
                if w.wide {
                    fe.ins.push(I::I64ExtendI32S);
                }
            }
            _ => return Err(format!("internal: {b:?} on integers")),
        }
        return Ok(());
    }
    let f = Fw::of(s);
    match b {
        B::Sqrt => fe.ins.extend([g(0), f.sqrt()]),
        B::InverseSqrt => fe.ins.extend([f.konst(1.0), g(0), f.sqrt(), f.div()]),
        B::Floor => fe.ins.extend([g(0), f.floor()]),
        B::Ceil => fe.ins.extend([g(0), f.ceil()]),
        B::Trunc => fe.ins.extend([g(0), f.trunc()]),
        B::Round => fe.ins.extend([g(0), f.nearest()]),
        B::Fract => fe.ins.extend([g(0), g(0), f.floor(), f.sub()]),
        B::Abs => fe.ins.extend([g(0), f.abs()]),
        B::Sign => {
            // 1 if x > 0, -1 if x < 0, else x (keeping 0, -0 and NaN).
            fe.ins.extend([
                f.konst(1.0),
                f.konst(-1.0),
                g(0),
                g(0),
                f.konst(0.0),
                f.lt(),
                I::Select,
                g(0),
                f.konst(0.0),
                f.gt(),
                I::Select,
            ]);
        }
        B::Min => fe.ins.extend([g(0), g(1), f.min()]),
        B::Max => fe.ins.extend([g(0), g(1), f.max()]),
        B::Clamp => fe.ins.extend([g(0), g(1), f.max(), g(2), f.min()]),
        B::Saturate => fe.ins.extend([g(0), f.konst(0.0), f.max(), f.konst(1.0), f.min()]),
        B::Mix => {
            // WGSL: e1 * (1 - e3) + e2 * e3.
            fe.ins.extend([
                g(0),
                f.konst(1.0),
                g(2),
                f.sub(),
                f.mul(),
                g(1),
                g(2),
                f.mul(),
                f.add(),
            ]);
        }
        B::Step => {
            // 1 if edge <= x.
            fe.ins.extend([f.konst(1.0), f.konst(0.0), g(0), g(1), f.le(), I::Select]);
        }
        B::Smoothstep => {
            // t = clamp((x - e0) / (e1 - e0), 0, 1); t * t * (3 - 2t)
            let t = fe.new_local(f.valtype());
            fe.ins.extend([
                g(2),
                g(0),
                f.sub(),
                g(1),
                g(0),
                f.sub(),
                f.div(),
                f.konst(0.0),
                f.max(),
                f.konst(1.0),
                f.min(),
                I::LocalSet(t),
            ]);
            fe.ins.extend([
                I::LocalGet(t),
                I::LocalGet(t),
                f.mul(),
                f.konst(3.0),
                f.konst(2.0),
                I::LocalGet(t),
                f.mul(),
                f.sub(),
                f.mul(),
            ]);
        }
        B::Length => fe.ins.extend([g(0), f.abs()]),
        B::Distance => fe.ins.extend([g(0), g(1), f.sub(), f.abs()]),
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
            // Payload: the handle and the size.
            let payload = 8;
            fe.ins.extend([
                I::I32Const((CMD_HEADER + payload) as i32),
                I::Call(h.reserve),
                I::LocalSet(a),
            ]);
            put(fe, a, 0, I::I32Const(Opcode::CreateBuffer as i32));
            put(fe, a, 4, I::I32Const(payload as i32));
            put(fe, a, CMD_HEADER, I::LocalGet(handle));
            put(fe, a, CMD_HEADER + 4, I::LocalGet(size));
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
            fe.ins.extend([
                I::I32Const((CMD_HEADER + payload) as i32),
                I::Call(h.reserve),
                I::LocalSet(a),
            ]);
            let opc = if dispatch { Opcode::Dispatch } else { Opcode::Draw };
            put(fe, a, 0, I::I32Const(opc as i32));
            put(fe, a, 4, I::I32Const(payload as i32));
            put(fe, a, CMD_HEADER, I::I32Const(*pipeline as i32));
            for (k, x) in args.iter().take(fixed as usize - 1).enumerate() {
                put(fe, a, CMD_HEADER + 4 + 4 * k as u32, I::LocalGet(fe.v(*x)));
            }
            let mut off = CMD_HEADER + 4 * fixed;
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
            // Payload: the clear colour, four f32s.
            let payload = 16;
            fe.ins.extend([
                I::I32Const((CMD_HEADER + payload) as i32),
                I::Call(h.reserve),
                I::LocalSet(a),
            ]);
            put(fe, a, 0, I::I32Const(Opcode::BeginScreenPass as i32));
            put(fe, a, 4, I::I32Const(payload as i32));
            fe.ins.extend([
                I::LocalGet(a),
                I::I32Const(CMD_HEADER as i32),
                I::I32Add,
                I::LocalGet(fe.v(args[0])),
                I::I32Const(payload as i32),
                I::MemoryCopy { src_mem: 0, dst_mem: 0 },
            ]);
        }
        ir::HostOp::Present => {
            let a = fe.new_local(ValType::I32);
            fe.ins.extend([I::I32Const(CMD_HEADER as i32), I::Call(h.reserve), I::LocalSet(a)]);
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
        I::I32Const(BATCH_HEADER as i32),
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
        I::I32Const((CMD_BASE + BATCH_HEADER) as i32),
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
    // Payload: the handle, the offset, the byte count, then the bytes.
    let words = 12;
    write.extend([
        I::LocalGet(chunk),
        I::I32Const((CMD_HEADER + words) as i32),
        I::I32Add,
        I::Call(h.reserve),
        I::LocalSet(a),
    ]);
    let w = |x: I<'static>, off: u32| [I::LocalGet(a), x, I::I32Store(mem(off, 2))];
    write.extend(w(I::I32Const(wrela_abi::stream::Opcode::WriteBuffer as i32), 0));
    write.extend([
        I::LocalGet(a),
        I::LocalGet(chunk),
        I::I32Const(words as i32),
        I::I32Add,
        I::I32Store(mem(4, 2)),
    ]);
    write.extend(w(I::LocalGet(handle), CMD_HEADER));
    write.extend(w(I::LocalGet(offset), CMD_HEADER + 4));
    write.extend(w(I::LocalGet(chunk), CMD_HEADER + 8));
    write.extend([
        I::LocalGet(a),
        I::I32Const((CMD_HEADER + words) as i32),
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
