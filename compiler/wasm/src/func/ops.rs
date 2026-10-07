//! Operators, conversions, built-ins and host operations.

use super::{Fe, R, function, konst, mem};
use crate::{Helpers, globals, memory};
use wasm_encoder::{BlockType, Function, Instruction as I, ValType};
use wrela_abi::stream::Opcode;
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
    copysign => F32Copysign, F64Copysign;
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
    scalar_bin_known(fe, op, s, a, b, false)
}

/// `scalar_bin`; `small_shift`: a shift's amount is known to be less than the width, so it
/// isn't checked.
fn scalar_bin_known(
    fe: &mut Fe,
    op: ir::BinOp,
    s: ir::Scalar,
    a: u32,
    b: u32,
    small_shift: bool,
) -> R<()> {
    use ir::BinOp as B;
    let (la, lb) = (I::LocalGet(a), I::LocalGet(b));
    if s.is_float() {
        let i = float_op(fe, op, Fw::of(s))?;
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
            if !small_shift {
                fe.ins.extend([lb.clone(), I::I32Const(s.bits() as i32), I::I32GeU]);
                fe.trap_if();
            }
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

/// The instruction of float operator `op`: `%` is a call of the `fmod` helper.
fn float_op(fe: &Fe, op: ir::BinOp, f: Fw) -> R<I<'static>> {
    use ir::BinOp as B;
    Ok(match op {
        B::Add => f.add(),
        B::Sub => f.sub(),
        B::Mul => f.mul(),
        B::Div => f.div(),
        B::Rem => I::Call(if f.f64_ { fe.at.helpers.fmod_f64 } else { fe.at.helpers.fmod_f32 }),
        B::Eq => f.eq(),
        B::Ne => f.ne(),
        B::Lt => f.lt(),
        B::Le => f.le(),
        B::Gt => f.gt(),
        B::Ge => f.ge(),
        _ => return Err(format!("internal: {op:?} on floats")),
    })
}

// ---- vectors and matrices ---------------------------------------------------------------------
//
// A component that is read once is loaded where it's used; one that is read more than once is
// loaded into a local first.

/// Pushes the f32 at byte `off` of aggregate value `x`: from memory, or a `v128`'s lane.
fn load_f32(fe: &mut Fe, x: ir::ValueId, off: u32) {
    if fe.vec_n(fe.vty(x)).is_some() {
        fe.ins.extend([I::LocalGet(fe.v(x)), I::F32x4ExtractLane((off / 4) as u8)]);
    } else {
        fe.ins.extend([I::LocalGet(fe.v(x)), I::F32Load(mem(off, 2))]);
    }
}

/// Pushes the f32 at byte `off` of aggregate `x`, or scalar `x` itself.
fn elem_or_scalar(fe: &mut Fe, x: ir::ValueId, off: u32) {
    if fe.m.types.is_aggregate(fe.vty(x)) {
        load_f32(fe, x, off);
    } else {
        fe.ins.push(I::LocalGet(fe.v(x)));
    }
}

/// `elem_or_scalar`'s f32 in a local, for code that reads it more than once: a new local, or
/// scalar `x`'s own.
fn elem_local(fe: &mut Fe, x: ir::ValueId, off: u32) -> u32 {
    if !fe.m.types.is_aggregate(fe.vty(x)) {
        return fe.v(x);
    }
    let l = fe.new_local(ValType::F32);
    load_f32(fe, x, off);
    fe.ins.push(I::LocalSet(l));
    l
}

/// The byte offset of the element at column `j`, row `i` of a `matN`.
fn mat_elem(n: u8, j: u32, i: u32) -> u32 {
    column_stride(n) * j + 4 * i
}

/// The byte offset of each f32 of a vector of `n` components, or of a `matN` column by column.
fn components(is_mat: bool, n: u8) -> impl Iterator<Item = u32> {
    let cols = if is_mat { u32::from(n) } else { 1 };
    (0..cols).flat_map(move |j| (0..u32::from(n)).map(move |i| mat_elem(n, j, i)))
}

/// Pushes the sum of `n` products, each of the two f32s `factors(fe, k)` pushes, added in order:
/// `((0 + p0) + p1) + ...` from zero, else `(p0 + p1) + ...`. (The two differ when every product
/// is -0: 0 + -0 is 0.)
fn sum_of_products(fe: &mut Fe, n: u32, from_zero: bool, mut factors: impl FnMut(&mut Fe, u32)) {
    if from_zero {
        fe.ins.push(I::F32Const(0.0f32.into()));
    }
    for k in 0..n {
        factors(fe, k);
        fe.ins.push(I::F32Mul);
        if from_zero || k > 0 {
            fe.ins.push(I::F32Add);
        }
    }
}

/// Pushes the sum of the squares of the `n` components of vector `x`, or of `x - y`, from the
/// first square.
fn sum_of_squares(fe: &mut Fe, x: ir::ValueId, minus: Option<ir::ValueId>, n: u32) {
    let t = fe.new_local(ValType::F32);
    sum_of_products(fe, n, false, |fe, c| {
        load_f32(fe, x, 4 * c);
        if let Some(y) = minus {
            load_f32(fe, y, 4 * c);
            fe.ins.push(I::F32Sub);
        }
        fe.ins.extend([I::LocalTee(t), I::LocalGet(t)]);
    });
}

fn dims(fe: &Fe, t: ir::TypeId) -> Option<(bool, u8)> {
    match fe.m.types.get(t) {
        // Only f32 vectors reach the back end's arithmetic: the others' is done a component
        // at a time before it (`wrela_ir::scalarize::scalarize_vectors`).
        ir::TypeDef::Vector(ir::Scalar::F32, n) => Some((false, *n)),
        ir::TypeDef::Matrix(n) => Some((true, *n)),
        _ => None,
    }
}

// ---- vectors in `v128`s (SIMD) ------------------------------------------------------------------
//
// Each lane is computed by the instruction the scalar code uses for that component, and sums
// of lanes are added one lane at a time in the scalar code's order: the bits are the same.

/// Pushes `x` as a `v128`: a vector, or a scalar in every lane.
fn vec_operand(fe: &mut Fe, x: ir::ValueId) {
    fe.ins.push(I::LocalGet(fe.v(x)));
    if fe.vec_n(fe.vty(x)).is_none() {
        fe.ins.push(I::F32x4Splat);
    }
}

/// Sets `v128` value `v` to the vector of `n` components that `comp(fe, c)` pushes, one at a
/// time.
fn assemble(
    fe: &mut Fe,
    v: ir::ValueId,
    n: u8,
    mut comp: impl FnMut(&mut Fe, u8) -> R<()>,
) -> R<()> {
    comp(fe, 0)?;
    fe.ins.extend([I::F32x4Splat, I::LocalSet(fe.v(v))]);
    for c in 1..n {
        fe.ins.push(I::LocalGet(fe.v(v)));
        comp(fe, c)?;
        fe.ins.extend([I::F32x4ReplaceLane(c), I::LocalSet(fe.v(v))]);
    }
    Ok(())
}

/// Pushes the sum of lanes `0..n` of the `v128` on the stack, from the first: `(l0 + l1) + l2`.
fn sum_lanes(fe: &mut Fe, n: u8) {
    let q = fe.new_local(ValType::V128);
    fe.ins.extend([I::LocalTee(q), I::F32x4ExtractLane(0)]);
    for c in 1..n {
        fe.ins.extend([I::LocalGet(q), I::F32x4ExtractLane(c), I::F32Add]);
    }
}

/// Pushes the sum of lanes `0..n` of the `v128` on the stack, from zero: `((0 + l0) + l1)`.
fn sum_lanes_from_zero(fe: &mut Fe, n: u8) {
    let q = fe.new_local(ValType::V128);
    fe.ins.extend([I::LocalSet(q), I::F32Const(0.0f32.into())]);
    for c in 0..n {
        fe.ins.extend([I::LocalGet(q), I::F32x4ExtractLane(c), I::F32Add]);
    }
}

/// A binary operator whose result is a `v128` vector of `n` components.
fn vec_binary(
    fe: &mut Fe,
    v: ir::ValueId,
    n: u8,
    op: ir::BinOp,
    a: ir::ValueId,
    b: ir::ValueId,
) -> R<()> {
    use ir::BinOp as B;
    match (dims(fe, fe.vty(a)), dims(fe, fe.vty(b))) {
        // matrix × vector: r = Σ_j m[j] b[j], from zero, which is each component's sum.
        (Some((true, m)), Some((false, _))) if op == B::Mul => {
            fe.ins.push(I::V128Const(0));
            for j in 0..m {
                fe.ins.push(I::LocalGet(fe.v(a)));
                fe.vec_load(m, column_stride(m) * u32::from(j));
                fe.ins.extend([
                    I::LocalGet(fe.v(b)),
                    I::F32x4ExtractLane(j),
                    I::F32x4Splat,
                    I::F32x4Mul,
                    I::F32x4Add,
                ]);
            }
            fe.ins.push(I::LocalSet(fe.v(v)));
        }
        // vector × matrix: r_j = Σ_i a[i] m[j][i], from zero.
        (Some((false, _)), Some((true, m))) if op == B::Mul => {
            assemble(fe, v, m, |fe, j| {
                fe.ins.extend([I::LocalGet(fe.v(a)), I::LocalGet(fe.v(b))]);
                fe.vec_load(m, column_stride(m) * u32::from(j));
                fe.ins.push(I::F32x4Mul);
                sum_lanes_from_zero(fe, m);
                Ok(())
            })?;
        }
        (Some((true, _)), _) | (_, Some((true, _))) => {
            return Err(format!("internal: {op:?} of a matrix to a vector"));
        }
        // `%` calls the `fmod` helper for each component.
        _ if op == B::Rem => {
            assemble(fe, v, n, |fe, c| {
                elem_or_scalar(fe, a, 4 * u32::from(c));
                elem_or_scalar(fe, b, 4 * u32::from(c));
                fe.ins.push(I::Call(fe.at.helpers.fmod_f32));
                Ok(())
            })?;
        }
        _ => {
            vec_operand(fe, a);
            vec_operand(fe, b);
            fe.ins.push(match op {
                B::Add => I::F32x4Add,
                B::Sub => I::F32x4Sub,
                B::Mul => I::F32x4Mul,
                B::Div => I::F32x4Div,
                _ => return Err(format!("internal: {op:?} on vectors")),
            });
            fe.ins.push(I::LocalSet(fe.v(v)));
        }
    }
    Ok(())
}

/// A componentwise built-in on `v128` vectors (a scalar argument is in every lane): pushes the
/// result.
fn vec_builtin(fe: &mut Fe, b: ir::Builtin, xs: &[ir::ValueId]) -> R<()> {
    lanes_builtin(fe, b, &mut |fe, i| vec_operand(fe, xs[i]))
}

/// Whether [`lanes_builtin`] computes built-in `b`.
pub(super) fn lanes_builtin_ok(b: ir::Builtin) -> bool {
    use ir::Builtin as B;
    matches!(
        b,
        B::Sqrt
            | B::InverseSqrt
            | B::Floor
            | B::Ceil
            | B::Trunc
            | B::Round
            | B::Abs
            | B::Fract
            | B::Sign
            | B::Min
            | B::Max
            | B::Clamp
            | B::Saturate
            | B::Mix
            | B::Step
            | B::Smoothstep
    )
}

/// A componentwise built-in, lane by lane, of the `v128`s `arg(fe, i)` pushes: pushes the
/// result.
pub(super) fn lanes_builtin(
    fe: &mut Fe,
    b: ir::Builtin,
    arg: &mut dyn FnMut(&mut Fe, usize),
) -> R<()> {
    use ir::Builtin as B;
    let splat = |x: f32| [I::F32Const(x.into()), I::F32x4Splat];
    match b {
        B::Sqrt => {
            arg(fe, 0);
            fe.ins.push(I::F32x4Sqrt);
        }
        B::InverseSqrt => {
            fe.ins.extend(splat(1.0));
            arg(fe, 0);
            fe.ins.extend([I::F32x4Sqrt, I::F32x4Div]);
        }
        B::Floor | B::Ceil | B::Trunc | B::Round | B::Abs => {
            arg(fe, 0);
            fe.ins.push(match b {
                B::Floor => I::F32x4Floor,
                B::Ceil => I::F32x4Ceil,
                B::Trunc => I::F32x4Trunc,
                B::Round => I::F32x4Nearest,
                _ => I::F32x4Abs,
            });
        }
        B::Fract => {
            arg(fe, 0);
            arg(fe, 0);
            fe.ins.extend([I::F32x4Floor, I::F32x4Sub]);
        }
        B::Sign => {
            // 1 where x > 0, -1 where x < 0, else x (0, -0 and NaN).
            let x = fe.new_local(ValType::V128);
            arg(fe, 0);
            fe.ins.push(I::LocalSet(x));
            fe.ins.extend(splat(1.0));
            fe.ins.extend(splat(-1.0));
            fe.ins.extend([I::LocalGet(x), I::LocalGet(x)]);
            fe.ins.extend(splat(0.0));
            fe.ins.extend([I::F32x4Lt, I::V128Bitselect, I::LocalGet(x)]);
            fe.ins.extend(splat(0.0));
            fe.ins.extend([I::F32x4Gt, I::V128Bitselect]);
        }
        B::Min | B::Max => {
            arg(fe, 0);
            arg(fe, 1);
            fe.ins.push(if b == B::Min { I::F32x4Min } else { I::F32x4Max });
        }
        B::Clamp => {
            arg(fe, 0);
            arg(fe, 1);
            fe.ins.push(I::F32x4Max);
            arg(fe, 2);
            fe.ins.push(I::F32x4Min);
        }
        B::Saturate => {
            arg(fe, 0);
            fe.ins.extend(splat(0.0));
            fe.ins.push(I::F32x4Max);
            fe.ins.extend(splat(1.0));
            fe.ins.push(I::F32x4Min);
        }
        B::Mix => {
            // WGSL: e1 * (1 - e3) + e2 * e3.
            arg(fe, 0);
            fe.ins.extend(splat(1.0));
            arg(fe, 2);
            fe.ins.extend([I::F32x4Sub, I::F32x4Mul]);
            arg(fe, 1);
            arg(fe, 2);
            fe.ins.extend([I::F32x4Mul, I::F32x4Add]);
        }
        B::Step => {
            // 1 where edge <= x.
            fe.ins.extend(splat(1.0));
            fe.ins.extend(splat(0.0));
            arg(fe, 0);
            arg(fe, 1);
            fe.ins.extend([I::F32x4Le, I::V128Bitselect]);
        }
        B::Smoothstep => {
            // t = clamp((x - e0) / (e1 - e0), 0, 1); t * t * (3 - 2t)
            let t = fe.new_local(ValType::V128);
            arg(fe, 2);
            arg(fe, 0);
            fe.ins.push(I::F32x4Sub);
            arg(fe, 1);
            arg(fe, 0);
            fe.ins.extend([I::F32x4Sub, I::F32x4Div]);
            fe.ins.extend(splat(0.0));
            fe.ins.push(I::F32x4Max);
            fe.ins.extend(splat(1.0));
            fe.ins.extend([I::F32x4Min, I::LocalTee(t), I::LocalGet(t), I::F32x4Mul]);
            fe.ins.extend(splat(3.0));
            fe.ins.extend(splat(2.0));
            fe.ins.extend([I::LocalGet(t), I::F32x4Mul, I::F32x4Sub, I::F32x4Mul]);
        }
        _ => return Err(format!("internal: the built-in {b:?} on vectors in the WASM back end")),
    }
    Ok(())
}

pub(super) fn binary(
    fe: &mut Fe,
    v: ir::ValueId,
    op: ir::BinOp,
    a: ir::ValueId,
    b: ir::ValueId,
) -> R<()> {
    let t = fe.vty(v);
    if let Some(n) = fe.vec_n(t) {
        return vec_binary(fe, v, n, op, a, b);
    }
    let (ta, tb) = (fe.vty(a), fe.vty(b));
    if !fe.m.types.is_aggregate(t) && !fe.m.types.is_aggregate(ta) {
        let s = fe.scalar(ta)?;
        let small_shift =
            matches!(op, ir::BinOp::Shl | ir::BinOp::Shr) && fe.known_below(b, s.bits());
        scalar_bin_known(fe, op, s, fe.v(a), fe.v(b), small_shift)?;
        fe.ins.push(I::LocalSet(fe.v(v)));
        return Ok(());
    }
    // A matrix's columns can have padding; a vector's components are every byte of it.
    let out = fe.fresh_slot(v, !fe.m.types.is_vector(t))?;
    match (dims(fe, ta), dims(fe, tb), dims(fe, t)) {
        // matrix × vector: r_i = Σ_j m[j][i] v[j]
        (Some((true, n)), Some((false, _)), _) if op == ir::BinOp::Mul => {
            for i in 0..n as u32 {
                fe.ins.push(I::LocalGet(out));
                sum_of_products(fe, n as u32, true, |fe, j| {
                    load_f32(fe, a, mat_elem(n, j, i));
                    load_f32(fe, b, 4 * j);
                });
                fe.ins.push(I::F32Store(mem(4 * i, 2)));
            }
        }
        // vector × matrix: r_j = Σ_i v[i] m[j][i]
        (Some((false, _)), Some((true, n)), _) if op == ir::BinOp::Mul => {
            for j in 0..n as u32 {
                fe.ins.push(I::LocalGet(out));
                sum_of_products(fe, n as u32, true, |fe, i| {
                    load_f32(fe, a, 4 * i);
                    load_f32(fe, b, mat_elem(n, j, i));
                });
                fe.ins.push(I::F32Store(mem(4 * j, 2)));
            }
        }
        // matrix × matrix: column j of the result is a × b[j].
        (Some((true, n)), Some((true, _)), _) if op == ir::BinOp::Mul => {
            for j in 0..n as u32 {
                for i in 0..n as u32 {
                    fe.ins.push(I::LocalGet(out));
                    sum_of_products(fe, n as u32, true, |fe, k| {
                        load_f32(fe, a, mat_elem(n, k, i));
                        load_f32(fe, b, mat_elem(n, j, k));
                    });
                    fe.ins.push(I::F32Store(mem(mat_elem(n, j, i), 2)));
                }
            }
        }
        // Elementwise: vector op vector, matrix ± matrix, matrix × scalar.
        (_, _, Some((is_mat, n))) => {
            for off in components(is_mat, n) {
                fe.ins.push(I::LocalGet(out));
                elem_or_scalar(fe, a, off);
                elem_or_scalar(fe, b, off);
                fe.ins.push(float_op(fe, op, Fw::of(ir::Scalar::F32))?);
                fe.ins.push(I::F32Store(mem(off, 2)));
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

pub(super) fn unary(fe: &mut Fe, v: ir::ValueId, op: ir::UnOp, x: ir::ValueId) -> R<()> {
    let t = fe.vty(x);
    if fe.vec_n(t).is_some() {
        fe.ins.extend([I::LocalGet(fe.v(x)), I::F32x4Neg, I::LocalSet(fe.v(v))]);
        return Ok(());
    }
    if let Some((is_mat, n)) = dims(fe, t) {
        let out = fe.fresh_slot(v, is_mat)?;
        for off in components(is_mat, n) {
            fe.ins.extend([
                I::LocalGet(out),
                I::LocalGet(fe.v(x)),
                I::F32Load(mem(off, 2)),
                I::F32Neg,
                I::F32Store(mem(off, 2)),
            ]);
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
            let z = fe.new_local(crate::valtype(s));
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
            // A `u32` of a 32- or 64-bit integer.
            B::CountOnes | B::LeadingZeros | B::TrailingZeros => {
                fe.ins.push(g(0));
                fe.ins.push(match (b, w.wide) {
                    (B::CountOnes, false) => I::I32Popcnt,
                    (B::CountOnes, true) => I::I64Popcnt,
                    (B::LeadingZeros, false) => I::I32Clz,
                    (B::LeadingZeros, true) => I::I64Clz,
                    (_, false) => I::I32Ctz,
                    (_, true) => I::I64Ctz,
                });
                if w.wide {
                    fe.ins.push(I::I32WrapI64);
                }
            }
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
    // Vector reductions in `v128`s.
    if let Some(n) = fe.vec_n(arg0) {
        match b {
            B::Dot => {
                fe.ins.extend([
                    I::LocalGet(fe.v(args[0])),
                    I::LocalGet(fe.v(args[1])),
                    I::F32x4Mul,
                ]);
                sum_lanes(fe, n);
                fe.ins.push(I::LocalSet(fe.v(v)));
                return Ok(());
            }
            B::Length | B::Distance | B::Normalize => {
                let d = fe.new_local(ValType::V128);
                fe.ins.push(I::LocalGet(fe.v(args[0])));
                if b == B::Distance {
                    fe.ins.extend([I::LocalGet(fe.v(args[1])), I::F32x4Sub]);
                }
                fe.ins.extend([I::LocalTee(d), I::LocalGet(d), I::F32x4Mul]);
                sum_lanes(fe, n);
                fe.ins.push(I::F32Sqrt);
                if b == B::Normalize {
                    fe.ins.extend([I::F32x4Splat, I::LocalSet(d)]);
                    fe.ins.extend([I::LocalGet(fe.v(args[0])), I::LocalGet(d), I::F32x4Div]);
                }
                fe.ins.push(I::LocalSet(fe.v(v)));
                return Ok(());
            }
            B::AllEqual => {
                let all = (1 << n) - 1;
                fe.ins.extend([
                    I::LocalGet(fe.v(args[0])),
                    I::LocalGet(fe.v(args[1])),
                    I::F32x4Eq,
                    I::I32x4Bitmask,
                    I::I32Const(all),
                    I::I32And,
                    I::I32Const(all),
                    I::I32Eq,
                    I::LocalSet(fe.v(v)),
                ]);
                return Ok(());
            }
            B::Cross => {
                // a.yzx * b.zxy - a.zxy * b.yzx: lane i is a[j] b[k] - a[k] b[j], as without.
                let yzx = shuffle([1, 2, 0, 0]);
                let zxy = shuffle([2, 0, 1, 0]);
                let (a, bb) = (fe.v(args[0]), fe.v(args[1]));
                fe.ins.extend([
                    I::LocalGet(a),
                    I::LocalGet(a),
                    I::I8x16Shuffle(yzx),
                    I::LocalGet(bb),
                    I::LocalGet(bb),
                    I::I8x16Shuffle(zxy),
                    I::F32x4Mul,
                    I::LocalGet(a),
                    I::LocalGet(a),
                    I::I8x16Shuffle(zxy),
                    I::LocalGet(bb),
                    I::LocalGet(bb),
                    I::I8x16Shuffle(yzx),
                    I::F32x4Mul,
                    I::F32x4Sub,
                    I::LocalSet(fe.v(v)),
                ]);
                return Ok(());
            }
            _ => {}
        }
    }
    if fe.vec_n(t).is_some() {
        vec_builtin(fe, b, args)?;
        fe.ins.push(I::LocalSet(fe.v(v)));
        return Ok(());
    }
    // Vector reductions. A sum starts from its first product, as WGSL's do.
    if let Some((false, n)) = dims(fe, arg0) {
        let n = n as u32;
        match b {
            B::Dot => {
                sum_of_products(fe, n, false, |fe, c| {
                    load_f32(fe, args[0], 4 * c);
                    load_f32(fe, args[1], 4 * c);
                });
                fe.ins.push(I::LocalSet(fe.v(v)));
                return Ok(());
            }
            B::Length | B::Distance => {
                sum_of_squares(fe, args[0], (b == B::Distance).then(|| args[1]), n);
                fe.ins.extend([I::F32Sqrt, I::LocalSet(fe.v(v))]);
                return Ok(());
            }
            B::AllEqual => {
                for c in 0..n {
                    load_f32(fe, args[0], 4 * c);
                    load_f32(fe, args[1], 4 * c);
                    fe.ins.push(I::F32Eq);
                    if c > 0 {
                        fe.ins.push(I::I32And);
                    }
                }
                fe.ins.push(I::LocalSet(fe.v(v)));
                return Ok(());
            }
            B::Cross => {
                let out = fe.fresh_slot(v, false)?;
                // Each component is read twice.
                let a: Vec<u32> = (0..3).map(|c| elem_local(fe, args[0], 4 * c)).collect();
                let bb: Vec<u32> = (0..3).map(|c| elem_local(fe, args[1], 4 * c)).collect();
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
                sum_of_squares(fe, args[0], None, n);
                fe.ins.extend([I::F32Sqrt, I::LocalSet(len)]);
                let out = fe.fresh_slot(v, false)?;
                for c in 0..n {
                    fe.ins.push(I::LocalGet(out));
                    load_f32(fe, args[0], 4 * c);
                    fe.ins.extend([I::LocalGet(len), I::F32Div, I::F32Store(mem(4 * c, 2))]);
                }
                return Ok(());
            }
            _ => {}
        }
    }
    // Componentwise on vectors (all the arguments are), or plain scalars.
    match dims(fe, t) {
        Some((false, n)) => {
            let out = fe.fresh_slot(v, false)?;
            for c in 0..n as u32 {
                // In locals: `scalar_builtin` reads locals, some more than once.
                let xs: Vec<u32> = args.iter().map(|&a| elem_local(fe, a, 4 * c)).collect();
                fe.ins.push(I::LocalGet(out));
                scalar_builtin(fe, b, ir::Scalar::F32, &xs)?;
                fe.ins.push(I::F32Store(mem(4 * c, 2)));
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

/// The bytes of an `i8x16.shuffle` that puts lanes `lanes` of its first operand in lanes 0..4.
pub(super) fn shuffle(lanes: [u8; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (k, lane) in lanes.iter().enumerate() {
        for b in 0..4 {
            out[4 * k + b] = 4 * lane + b as u8;
        }
    }
    out
}

// ---- host operations: the command stream ------------------------------------------------------

/// Stores the i32 `x` at `[a] + off`.
fn put(a: u32, off: u32, x: I<'static>) -> [I<'static>; 3] {
    [I::LocalGet(a), x, I::I32Store(mem(off, 2))]
}

/// Reserves room for a command whose payload is `payload` bytes, sets local `a` to its address,
/// and writes its header.
fn command(h: &Helpers, a: u32, opcode: Opcode, payload: u32) -> impl Iterator<Item = I<'static>> {
    [I::I32Const(CMD_HEADER.saturating_add(payload) as i32), I::Call(h.reserve), I::LocalSet(a)]
        .into_iter()
        .chain(put(a, 0, I::I32Const(opcode as i32)))
        .chain(put(a, 4, I::I32Const(payload as i32)))
}

/// Starts a command whose payload is `payload` bytes: returns the local that holds its address.
fn begin_command(fe: &mut Fe, opcode: Opcode, payload: u32) -> u32 {
    let a = fe.new_local(ValType::I32);
    let h = fe.at.helpers;
    fe.ins.extend(command(h, a, opcode, payload));
    a
}

/// Pushes the byte count of local `x` elements of `elem_size` bytes; traps if it doesn't fit in a
/// u32.
fn checked_bytes(fe: &mut Fe, x: u32, elem_size: u32) {
    let wide = fe.new_local(ValType::I64);
    fe.ins.extend([
        I::LocalGet(x),
        I::I64ExtendI32U,
        I::I64Const(elem_size as i64),
        I::I64Mul,
        I::LocalTee(wide),
        I::I64Const(u32::MAX as i64),
        I::I64GtU,
    ]);
    fe.trap_if();
    fe.ins.extend([I::LocalGet(wide), I::I32WrapI64]);
}

pub(super) fn host(
    fe: &mut Fe,
    op: &ir::HostOp,
    args: &[ir::ValueId],
    result: Option<ir::ValueId>,
) -> R<()> {
    match op {
        ir::HostOp::CreateBuffer { elem_size } => {
            // size = count * elem_size, checked, or one element's for an empty buffer: WebGPU
            // can't bind less (an array's minimum binding size is one element). handle = next
            // handle.
            let size = fe.new_local(ValType::I32);
            checked_bytes(fe, fe.v(args[0]), *elem_size);
            fe.ins.extend([
                I::LocalTee(size),
                I::I32Eqz,
                I::If(BlockType::Empty),
                I::I32Const((*elem_size).max(4) as i32),
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
            // Payload: the handle and the size.
            let a = begin_command(fe, Opcode::CreateBuffer, 8);
            fe.ins.extend(put(a, CMD_HEADER, I::LocalGet(handle)));
            fe.ins.extend(put(a, CMD_HEADER + 4, I::LocalGet(size)));
            if let Some(v) = result {
                fe.ins.extend([I::LocalGet(handle), I::LocalSet(fe.v(v))]);
            }
        }
        ir::HostOp::WriteBuffer { elem, elem_size } => {
            let (handle, at, run) = (fe.v(args[0]), fe.v(args[1]), fe.v(args[2]));
            canonicalize_run(fe, *elem, run);
            fe.ins.push(I::LocalGet(handle));
            checked_bytes(fe, at, *elem_size);
            fe.ins.extend([I::LocalGet(run), I::I32Load(mem(0, 2))]);
            fe.ins.extend([I::LocalGet(run), I::I32Load(mem(4, 2))]);
            let n = fe.new_local(ValType::I32);
            fe.ins.push(I::LocalSet(n));
            checked_bytes(fe, n, *elem_size);
            fe.ins.push(I::Call(fe.at.helpers.write));
        }
        ir::HostOp::Dispatch { pipeline, bindings, uniform, indirect }
        | ir::HostOp::Draw { pipeline, bindings, uniform, indirect, .. } => {
            let dispatch = matches!(op, ir::HostOp::Dispatch { .. });
            let indexed = matches!(op, ir::HostOp::Draw { indexed: true, .. });
            // The pipeline, then the counts: three groups, two counts, or a buffer and offset
            // (after the index buffer's handle, offset and size, for an indexed draw).
            let fixed = if indexed {
                6
            } else if dispatch && !*indirect {
                4
            } else {
                3
            };
            let nb = *bindings as usize;
            let usize_ = uniform.map_or(0, |t| round_up(16, layout(&fe.m.types, t).size));
            // Lowering keeps the uniforms small enough (`ir::MAX_UNIFORM_BYTES`) for the
            // command to fit in the command buffer.
            let payload = (4 * (fixed + 1 + 3 * nb as u32 + 1)).saturating_add(usize_);
            if CMD_HEADER.saturating_add(payload) > memory::CMD_CAP {
                return Err(format!("internal: a {payload}-byte command overflows the buffer"));
            }
            let opc = match (dispatch, *indirect) {
                (true, false) => Opcode::Dispatch,
                (true, true) => Opcode::DispatchIndirect,
                (false, false) => Opcode::Draw,
                (false, true) if indexed => Opcode::DrawIndexedIndirect,
                (false, true) => Opcode::DrawIndirect,
            };
            let a = begin_command(fe, opc, payload);
            fe.ins.extend(put(a, CMD_HEADER, I::I32Const(*pipeline as i32)));
            for (k, x) in args.iter().take(fixed as usize - 1).enumerate() {
                fe.ins.extend(put(a, CMD_HEADER + 4 + 4 * k as u32, I::LocalGet(fe.v(*x))));
            }
            let mut off = CMD_HEADER + 4 * fixed;
            fe.ins.extend(put(a, off, I::I32Const(nb as i32)));
            off += 4;
            // Each binding: handle, offset, size.
            for k in 0..3 * nb {
                fe.ins.extend(put(a, off, I::LocalGet(fe.v(args[fixed as usize - 1 + k]))));
                off += 4;
            }
            fe.ins.extend(put(a, off, I::I32Const(usize_ as i32)));
            off += 4;
            if let Some(t) = uniform {
                let u = fe.v(args[fixed as usize - 1 + 3 * nb]);
                fe.ins.extend([
                    I::LocalGet(a),
                    I::I32Const(off as i32),
                    I::I32Add,
                    I::I32Const(0),
                    I::I32Const(usize_ as i32),
                    I::MemoryFill(0),
                ]);
                fe.ins.extend([I::LocalGet(a), I::I32Const(off as i32), I::I32Add, I::LocalGet(u)]);
                match fe.vec_n(*t) {
                    Some(n) => fe.vec_store(n, 0),
                    None => fe.copy(*t),
                }
                let at = fe.new_local(ValType::I32);
                fe.ins.extend([
                    I::LocalGet(a),
                    I::I32Const(off as i32),
                    I::I32Add,
                    I::LocalSet(at),
                ]);
                canonicalize(fe, *t, at, 0);
            }
        }
        ir::HostOp::DestroyBuffer => {
            let a = begin_command(fe, Opcode::DestroyBuffer, 4);
            fe.ins.extend(put(a, CMD_HEADER, I::LocalGet(fe.v(args[0]))));
        }
        ir::HostOp::CopyBuffer => {
            let a = begin_command(fe, Opcode::CopyBuffer, 20);
            for (k, x) in args.iter().enumerate() {
                fe.ins.extend(put(a, CMD_HEADER + 4 * k as u32, I::LocalGet(fe.v(*x))));
            }
        }
        ir::HostOp::BeginScreenPass => {
            // Payload: the clear colour, four f32s.
            let payload = 16;
            let a = begin_command(fe, Opcode::BeginScreenPass, payload);
            if fe.vec_n(fe.vty(args[0])).is_some() {
                fe.ins.extend([
                    I::LocalGet(a),
                    I::LocalGet(fe.v(args[0])),
                    I::V128Store(mem(CMD_HEADER, 2)),
                ]);
            } else {
                fe.ins.extend([
                    I::LocalGet(a),
                    I::I32Const(CMD_HEADER as i32),
                    I::I32Add,
                    I::LocalGet(fe.v(args[0])),
                    I::I32Const(payload as i32),
                    I::MemoryCopy { src_mem: 0, dst_mem: 0 },
                ]);
            }
        }
        ir::HostOp::Present => {
            begin_command(fe, Opcode::Present, 0);
        }
        ir::HostOp::Command { opcode, words, runs, handle } => {
            command_op(fe, *opcode, *words, *runs, *handle, args, result)?;
        }
        ir::HostOp::NextRequest => {
            let r = result.ok_or("internal: a request number nothing uses")?;
            fe.ins.extend([
                I::GlobalGet(globals::NEXT_REQUEST),
                I::LocalTee(fe.v(r)),
                I::I32Const(1),
                I::I32Add,
                I::GlobalSet(globals::NEXT_REQUEST),
            ]);
        }
        ir::HostOp::RequestStatus | ir::HostOp::Limit => {
            // A request made in this call is pending until the next one, so its command
            // needn't reach the host before its status is read.
            let h = fe.at.helpers;
            let helper = if matches!(op, ir::HostOp::Limit) { h.limit } else { h.request_status };
            fe.ins.extend([I::LocalGet(fe.v(args[0])), I::Call(helper)]);
            match result {
                Some(r) => fe.ins.push(I::LocalSet(fe.v(r))),
                None => fe.ins.push(I::Drop),
            }
        }
        ir::HostOp::Keep => {
            let h = fe.at.helpers;
            fe.ins.extend([I::LocalGet(fe.v(args[0])), I::LocalGet(fe.v(args[1]))]);
            fe.ins.push(I::Call(h.keep));
        }
        ir::HostOp::Input | ir::HostOp::Kept => {
            let h = fe.at.helpers;
            fe.ins.extend([I::LocalGet(fe.v(args[0])), I::LocalGet(fe.v(args[1]))]);
            fe.ins.push(I::Call(if matches!(op, ir::HostOp::Input) { h.input } else { h.kept }));
            match result {
                Some(r) => fe.ins.push(I::LocalSet(fe.v(r))),
                None => fe.ins.push(I::Drop),
            }
        }
        ir::HostOp::Audio | ir::HostOp::RequestTake | ir::HostOp::Tick => {
            let h = fe.at.helpers;
            let helper = match op {
                ir::HostOp::Audio => h.audio,
                ir::HostOp::RequestTake => h.request_take,
                _ => h.tick,
            };
            for &a in args {
                fe.ins.push(I::LocalGet(fe.v(a)));
            }
            fe.ins.push(I::Call(helper));
        }
    }
    Ok(())
}

/// [`ir::HostOp::Command`]: the words, each run's length, then each run's bytes padded to a
/// multiple of 4.
fn command_op(
    fe: &mut Fe,
    opcode: u32,
    words: u32,
    runs: u32,
    handle: bool,
    args: &[ir::ValueId],
    result: Option<ir::ValueId>,
) -> R<()> {
    let given = (words - u32::from(handle)) as usize;
    let (scalars, run_args) = args.split_at(given);
    if run_args.len() != runs as usize {
        return Err("internal: a command's arguments don't match its shape".into());
    }
    // The payload's size: the words, the lengths, and each run padded.
    let size = fe.new_local(ValType::I32);
    fe.ins.extend([I::I32Const((4 * (words + runs)) as i32), I::LocalSet(size)]);
    for r in run_args {
        fe.ins.extend([
            I::LocalGet(size),
            I::LocalGet(fe.v(*r)),
            I::I32Load(mem(4, 2)),
            I::I32Const(3),
            I::I32Add,
            I::I32Const(-4),
            I::I32And,
            I::I32Add,
            I::LocalSet(size),
        ]);
    }
    let a = fe.new_local(ValType::I32);
    let h = fe.at.helpers;
    fe.ins.extend([
        I::LocalGet(size),
        I::I32Const(CMD_HEADER as i32),
        I::I32Add,
        I::Call(h.reserve),
        I::LocalSet(a),
    ]);
    fe.ins.extend(put(a, 0, I::I32Const(opcode as i32)));
    fe.ins.extend(put(a, 4, I::LocalGet(size)));
    let mut off = CMD_HEADER;
    if handle {
        let new = fe.new_local(ValType::I32);
        fe.ins.extend([
            I::GlobalGet(globals::NEXT_HANDLE),
            I::LocalTee(new),
            I::I32Const(1),
            I::I32Add,
            I::GlobalSet(globals::NEXT_HANDLE),
        ]);
        fe.ins.extend(put(a, off, I::LocalGet(new)));
        off += 4;
        if let Some(v) = result {
            fe.ins.extend([I::LocalGet(new), I::LocalSet(fe.v(v))]);
        }
    }
    for x in scalars {
        let local = fe.v(*x);
        let word = match fe.scalar(fe.vty(*x))? {
            ir::Scalar::F32 => vec![I::LocalGet(a), I::LocalGet(local), I::F32Store(mem(off, 2))],
            ir::Scalar::I64 | ir::Scalar::U64 | ir::Scalar::F64 => {
                return Err("internal: a 64-bit word in a command".into());
            }
            _ => put(a, off, I::LocalGet(local)).to_vec(),
        };
        fe.ins.extend(word);
        off += 4;
    }
    for r in run_args {
        fe.ins.extend([I::LocalGet(a), I::LocalGet(fe.v(*r)), I::I32Load(mem(4, 2))]);
        fe.ins.push(I::I32Store(mem(off, 2)));
        off += 4;
    }
    // The bytes, through a cursor: their lengths aren't known here.
    let at = fe.new_local(ValType::I32);
    fe.ins.extend([I::LocalGet(a), I::I32Const(off as i32), I::I32Add, I::LocalSet(at)]);
    for r in run_args {
        let run = fe.v(*r);
        let padded = fe.new_local(ValType::I32);
        fe.ins.extend([
            I::LocalGet(run),
            I::I32Load(mem(4, 2)),
            I::I32Const(3),
            I::I32Add,
            I::I32Const(-4),
            I::I32And,
            I::LocalSet(padded),
            // Zeros first, for the padding.
            I::LocalGet(at),
            I::I32Const(0),
            I::LocalGet(padded),
            I::MemoryFill(0),
            I::LocalGet(at),
            I::LocalGet(run),
            I::I32Load(mem(0, 2)),
            I::LocalGet(run),
            I::I32Load(mem(4, 2)),
            I::MemoryCopy { src_mem: 0, dst_mem: 0 },
            I::LocalGet(at),
            I::LocalGet(padded),
            I::I32Add,
            I::LocalSet(at),
        ]);
    }
    Ok(())
}

// ---- panics the back end writes ----------------------------------------------------------------

/// A panic with the message in constant data `msg` (its id and length): the message where hosts
/// read it, then a trap (as `ir::MemOp::Panic`).
pub(crate) fn panic_with(data: &[u32], msg: (ir::DataId, u32)) -> R<Vec<I<'static>>> {
    let (d, n) = msg;
    let at = *data.get(d.index()).ok_or("internal: a missing message")?;
    let n = n.min(memory::PANIC_CAP);
    Ok(vec![
        I::GlobalGet(globals::THREAD),
        I::I32Const(n as i32),
        I::I32Store(mem(memory::PANIC, 2)),
        I::GlobalGet(globals::THREAD),
        I::I32Const(memory::PANIC as i32 + 4),
        I::I32Add,
        I::I32Const(at as i32),
        I::I32Const(n as i32),
        I::MemoryCopy { src_mem: 0, dst_mem: 0 },
        I::Unreachable,
    ])
}

/// The offsets of the f32s of a value of type `t` that holds floats as a vector or matrix
/// does, or `None` for a scalar (an f32 or f64, `Some(true)` for f64) or a type that holds none.
fn float_parts(types: &ir::Types, t: ir::TypeId) -> Option<(Option<bool>, Vec<u32>)> {
    match types.get(t) {
        ir::TypeDef::Scalar(ir::Scalar::F32) => Some((Some(false), Vec::new())),
        ir::TypeDef::Scalar(ir::Scalar::F64) => Some((Some(true), Vec::new())),
        ir::TypeDef::Vector(ir::Scalar::F32, n) => Some((None, components(false, *n).collect())),
        ir::TypeDef::Matrix(n) => Some((None, components(true, *n).collect())),
        _ => None,
    }
}

/// Pushes whether float value `x` (a scalar, vector or matrix) is or holds a NaN, an i32.
fn any_nan(fe: &mut Fe, x: ir::ValueId, parts: &(Option<bool>, Vec<u32>)) {
    if let Some(n) = fe.vec_n(fe.vty(x)) {
        // Its own lanes only: the others hold anything.
        fe.ins.extend([
            I::LocalGet(fe.v(x)),
            I::LocalGet(fe.v(x)),
            I::F32x4Ne,
            I::I32x4Bitmask,
            I::I32Const((1 << n) - 1),
            I::I32And,
            I::I32Const(0),
            I::I32Ne,
        ]);
        return;
    }
    match parts {
        (Some(f64_), _) => {
            let ne = if *f64_ { I::F64Ne } else { I::F32Ne };
            fe.ins.extend([I::LocalGet(fe.v(x)), I::LocalGet(fe.v(x)), ne]);
        }
        (None, offs) => {
            for (k, &off) in offs.iter().enumerate() {
                load_f32(fe, x, off);
                load_f32(fe, x, off);
                fe.ins.push(I::F32Ne);
                if k > 0 {
                    fe.ins.push(I::I32Or);
                }
            }
        }
    }
}

/// In a debug build (`ir::Module::nan_message`), panics when float value `v` is or holds a NaN
/// that none of `operands` held: a NaN was created, not passed on (language.md §11).
pub(crate) fn nan_check(fe: &mut Fe, v: ir::ValueId, operands: &[ir::ValueId]) {
    let Some(msg) = fe.m.nan_message else { return };
    let Some(parts) = float_parts(&fe.m.types, fe.vty(v)) else { return };
    any_nan(fe, v, &parts);
    for &o in operands {
        if let Some(p) = float_parts(&fe.m.types, fe.vty(o)) {
            any_nan(fe, o, &p);
            fe.ins.extend([I::I32Eqz, I::I32And]);
        }
    }
    fe.ins.push(I::If(BlockType::Empty));
    match panic_with(fe.at.data, msg) {
        Ok(p) => fe.ins.extend(p),
        Err(_) => fe.ins.push(I::Unreachable),
    }
    fe.ins.push(I::End);
}

// ---- canonical NaNs (language.md §11) ---------------------------------------------------------

/// The NaNs a value's bytes hold where they're observed: one bit pattern each.
const CANONICAL_F32: u32 = 0x7fc0_0000;
const CANONICAL_F64: u64 = 0x7ff8_0000_0000_0000;

/// Pushes the f32 or f64 on the stack's top (`f64_`), with any NaN made the canonical one.
pub(crate) fn canonical_on_stack(fe: &mut Fe, f64_: bool) {
    let (t, nan, ne) = if f64_ {
        (ValType::F64, I::F64Const(f64::from_bits(CANONICAL_F64).into()), I::F64Ne)
    } else {
        (ValType::F32, I::F32Const(f32::from_bits(CANONICAL_F32).into()), I::F32Ne)
    };
    let x = fe.new_local(t);
    fe.ins.extend([I::LocalSet(x), nan, I::LocalGet(x), I::LocalGet(x), I::LocalGet(x), ne]);
    fe.ins.push(I::Select);
}

/// Makes every NaN in the value of type `t` at `addr + off` (`addr` a local) the canonical one,
/// in place, so the value's bytes depend only on its fields where they're seen: uploads to the
/// GPU (language.md §11). A NaN's bits can't be told apart in the language otherwise.
pub(crate) fn canonicalize(fe: &mut Fe, t: ir::TypeId, addr: u32, off: u32) {
    let types = &fe.m.types;
    if !types.has_float(t) {
        return;
    }
    let float = |fe: &mut Fe, f64_: bool, off: u32| {
        let (load, store) = if f64_ {
            (I::F64Load(mem(off, 3)), I::F64Store(mem(off, 3)))
        } else {
            (I::F32Load(mem(off, 2)), I::F32Store(mem(off, 2)))
        };
        fe.ins.extend([I::LocalGet(addr), I::LocalGet(addr), load]);
        canonical_on_stack(fe, f64_);
        fe.ins.push(store);
    };
    match types.get(t).clone() {
        ir::TypeDef::Scalar(_) | ir::TypeDef::Vector(..) | ir::TypeDef::Matrix(_) => {
            for (o, s) in ir::layout::scalars(types, t) {
                if s.is_float() {
                    float(fe, s == ir::Scalar::F64, off + o);
                }
            }
        }
        ir::TypeDef::Array(e, n) => {
            let stride = ir::layout::array_stride(&fe.m.types, e);
            if n <= 8 {
                for i in 0..n {
                    canonicalize(fe, e, addr, off + stride * i);
                }
                return;
            }
            let start = [I::LocalGet(addr), I::I32Const(off as i32), I::I32Add];
            canonicalize_loop(fe, e, stride, start, [I::I32Const((stride * n) as i32)]);
        }
        ir::TypeDef::Struct { fields, .. } => {
            let offsets = ir::layout::field_offsets(&fe.m.types, t).to_vec();
            for (&(_, f), o) in fields.iter().zip(offsets) {
                canonicalize(fe, f, addr, off + o);
            }
        }
        ir::TypeDef::Enum { variants, .. } => {
            let offsets = ir::layout::field_offsets(&fe.m.types, t).to_vec();
            for (v, (_, p)) in variants.iter().enumerate() {
                let Some(p) = *p else { continue };
                fe.ins.extend([
                    I::LocalGet(addr),
                    I::I32Load(mem(off, 2)),
                    I::I32Const(v as i32),
                    I::I32Eq,
                    I::If(BlockType::Empty),
                ]);
                canonicalize(fe, p, addr, off + offsets[1 + v]);
                fe.ins.push(I::End);
            }
        }
        ir::TypeDef::Run(_)
        | ir::TypeDef::RuntimeArray(_)
        | ir::TypeDef::Ptr(_)
        | ir::TypeDef::Atomic(_) => {}
    }
}

/// [`canonicalize`] for each element of the run (a local: its pointer, then its count) of
/// elements of type `elem`.
fn canonicalize_run(fe: &mut Fe, elem: ir::TypeId, run: u32) {
    if !fe.m.types.has_float(elem) {
        return;
    }
    let stride = ir::layout::array_stride(&fe.m.types, elem);
    let start = [I::LocalGet(run), I::I32Load(mem(0, 2))];
    let size = [I::LocalGet(run), I::I32Load(mem(4, 2)), I::I32Const(stride as i32), I::I32Mul];
    canonicalize_loop(fe, elem, stride, start, size);
}

/// [`canonicalize`] for each element of type `elem`, `stride` bytes apart, from the address
/// `start` pushes to the end of the `size` bytes that `size` pushes: a loop, in which a local
/// walks the elements.
fn canonicalize_loop(
    fe: &mut Fe,
    elem: ir::TypeId,
    stride: u32,
    start: impl IntoIterator<Item = I<'static>>,
    size: impl IntoIterator<Item = I<'static>>,
) {
    let (at, end) = (fe.new_local(ValType::I32), fe.new_local(ValType::I32));
    fe.ins.extend(start);
    fe.ins.push(I::LocalTee(at));
    fe.ins.extend(size);
    fe.ins.extend([
        I::I32Add,
        I::LocalSet(end),
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty),
        I::LocalGet(at),
        I::LocalGet(end),
        I::I32GeU,
        I::BrIf(1),
    ]);
    canonicalize(fe, elem, at, 0);
    fe.ins.extend([
        I::LocalGet(at),
        I::I32Const(stride as i32),
        I::I32Add,
        I::LocalSet(at),
        I::Br(0),
        I::End,
        I::End,
    ]);
}

/// `fmod(x, y)` for floats of width `f`: `x - y * trunc(x / y)`, exactly, as C's `fmod`. The
/// result has `x`'s sign and is less than `y` in magnitude; NaN if `x` is infinite or `y` is 0.
///
/// The remainder of two floats is a float, and each step below is exact. For f32s whose
/// quotient is below 2^29, one step in f64 gives it: `q * y` takes at most 53 bits, and
/// `x - q * y` is a multiple of `y`'s last place below `2 y`. Otherwise it's long division on
/// `|x|`: subtract `t = |y| * 2^k`, from the largest `t <= |x|` down to `|y|`, wherever it fits
/// (exact, since then `t <= |x| < 2 t`).
fn fmod(f: Fw) -> Function {
    let (x, y, r, d, t, q, w) = (0, 1, 2, 3, 4, 5, 6);
    let g = I::LocalGet;
    let s = I::LocalSet;
    let mut ins = vec![
        // NaN unless x is finite and y is a nonzero number: (x y) / (x y) is.
        g(x),
        f.abs(),
        f.konst(f64::INFINITY),
        f.lt(),
        g(y),
        g(y),
        f.eq(),
        I::I32And,
        g(y),
        f.konst(0.0),
        f.ne(),
        I::I32And,
        I::I32Eqz,
        I::If(BlockType::Empty),
        g(x),
        g(y),
        f.mul(),
        g(x),
        g(y),
        f.mul(),
        f.div(),
        I::Return,
        I::End,
        g(x),
        f.abs(),
        s(r),
        g(y),
        f.abs(),
        s(d),
        // |x| < |y| (y infinite, or x a zero, among others): x itself.
        g(r),
        g(d),
        f.lt(),
        I::If(BlockType::Empty),
        g(x),
        I::Return,
        I::End,
    ];
    if !f.f64_ {
        ins.extend([
            g(r),
            I::F64PromoteF32,
            g(d),
            I::F64PromoteF32,
            I::F64Div,
            I::F64Trunc,
            I::LocalTee(q),
            I::F64Const(536870912.0.into()),
            I::F64Lt,
            I::If(BlockType::Empty),
            g(r),
            I::F64PromoteF32,
            g(q),
            g(d),
            I::F64PromoteF32,
            I::F64Mul,
            I::F64Sub,
            I::LocalTee(w),
            I::F64Const(0.0.into()),
            I::F64Lt,
            // The rounded quotient was one too many.
            I::If(BlockType::Empty),
            g(w),
            g(d),
            I::F64PromoteF32,
            I::F64Add,
            s(w),
            I::End,
            g(w),
            I::F32DemoteF64,
            g(x),
            I::F32Copysign,
            I::Return,
            I::End,
        ]);
    }
    // t = the largest |y| * 2^k <= |x| (t + t is exact, or infinite and so too large).
    ins.extend([g(d), s(t), I::Block(BlockType::Empty), I::Loop(BlockType::Empty)]);
    ins.extend([g(t), g(t), f.add(), g(r), f.le(), I::I32Eqz, I::BrIf(1)]);
    ins.extend([g(t), g(t), f.add(), s(t), I::Br(0), I::End, I::End]);
    // Then down to |y|, subtracting each t that fits.
    ins.extend([I::Block(BlockType::Empty), I::Loop(BlockType::Empty)]);
    ins.extend([g(r), g(t), f.ge(), I::If(BlockType::Empty), g(r), g(t), f.sub(), s(r), I::End]);
    ins.extend([g(t), g(d), f.eq(), I::BrIf(1)]);
    ins.extend([g(t), f.konst(0.5), f.mul(), s(t), I::Br(0), I::End, I::End]);
    ins.extend([g(r), g(x), f.copysign(), I::End]);
    let mut locals = vec![f.valtype(); 3];
    if !f.f64_ {
        locals.extend([ValType::F64; 2]);
    }
    function(locals, &ins)
}

/// The helpers' bodies, in `Helpers` order after the import: flush, reserve, write and the two
/// `fmod`s.
pub(crate) fn helper_bodies(h: &Helpers) -> Vec<(Vec<ValType>, Vec<ValType>, Function)> {
    use memory::{CMD_BASE, CMD_CAP};
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
        I::GlobalGet(globals::NEXT_HANDLE),
        I::GlobalSet(globals::SUBMITTED_TO),
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
    write.extend(put(a, 0, I::I32Const(Opcode::WriteBuffer as i32)));
    write.extend([
        I::LocalGet(a),
        I::LocalGet(chunk),
        I::I32Const(words as i32),
        I::I32Add,
        I::I32Store(mem(4, 2)),
    ]);
    write.extend(put(a, CMD_HEADER, I::LocalGet(handle)));
    write.extend(put(a, CMD_HEADER + 4, I::LocalGet(offset)));
    write.extend(put(a, CMD_HEADER + 8, I::LocalGet(chunk)));
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
    let (f32_, f64_) = (Fw { f64_: false }, Fw { f64_: true });
    vec![
        (vec![], vec![], function(vec![], &flush)),
        (i32x(1), i32x(1), function(vec![], &reserve)),
        (i32x(4), vec![], function(i32x(2), &write)),
        (vec![ValType::F32; 2], vec![ValType::F32], fmod(f32_)),
        (vec![ValType::F64; 2], vec![ValType::F64], fmod(f64_)),
    ]
}
