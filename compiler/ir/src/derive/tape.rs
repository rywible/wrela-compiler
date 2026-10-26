//! Tapes of compiled fields (language.md §14.4, `std::derive::tape`): a function of the point
//! becomes a function that builds a `std::stage::Tape` of it, once, on the CPU. The point is
//! active; each active value carries, beside its value, its **registers**: a value of the same
//! shape (`reg_ty`) whose every leaf holds the index of the tape register that computes that
//! leaf: an `f32` for a float or a bool (1 or 0), a `u32` for an integer, which is two registers
//! in a row (its high and low 16 bits, exact as floats: `std::stage`'s integers). An active operation appends the tape's operation for it and takes the
//! register it writes; an inactive one runs as it is, so a field's data (its parts' placements,
//! its radii, its seeds, its loop counts) is computed as ordinary code and reaches the tape as
//! constants. A branch on an active condition runs both sides and selects between what each
//! left; a loop runs as it is, unrolled into the tape by running.
//!
//! The opcodes are `std::stage`'s (`stage.wrela`), which `std::stage`'s tests read back.

use super::*;
use std::collections::{HashMap, HashSet};

// `std::stage`'s opcodes.
const X: u32 = 0;
const CONST: u32 = 3;
const ADD: u32 = 4;
const SUB: u32 = 5;
const MUL: u32 = 6;
const DIV: u32 = 7;
const MIN: u32 = 8;
const MAX: u32 = 9;
const SMIN: u32 = 10;
const NEG: u32 = 11;
const ABS: u32 = 12;
const SQRT: u32 = 13;
const SIN: u32 = 14;
const COS: u32 = 15;
const EXP: u32 = 16;
const COPY: u32 = 17;
const FLOOR: u32 = 18;
const FRACT: u32 = 19;
const STEP: u32 = 20;
const SELECT: u32 = 21;
const MIX: u32 = 22;
const CLAMP01: u32 = 23;
const POW: u32 = 24;
const LOG: u32 = 25;
const LOAD: u32 = 27;
const NOISE3: u32 = 29;
const HASH: u32 = 31;
const DOT3: u32 = 32;
const LEN3: u32 = 33;
const NORM3: u32 = 34;
const ATAN2: u32 = 35;
const IADD: u32 = 36;
const ISUB: u32 = 37;
const IMUL: u32 = 38;
const AND: u32 = 39;
const OR: u32 = 40;
const XOR: u32 = 41;
const SHL: u32 = 42;
const USHR: u32 = 43;
const ISHR: u32 = 44;
const UDIV: u32 = 45;
const IDIV: u32 = 46;
const UREM: u32 = 47;
const IREM: u32 = 48;
const IMIN: u32 = 49;
const IMAX: u32 = 50;
const UMIN: u32 = 51;
const UMAX: u32 = 52;
const ULT: u32 = 53;
const ILT: u32 = 54;
const IEQ: u32 = 55;
const I2F: u32 = 56;
const U2F: u32 = 57;
const F2I: u32 = 58;
const F2U: u32 = 59;
const PCG: u32 = 60;
const IMAD: u32 = 61;

/// What a tape is of: a function of its captures (`ncap` of them) and the point, returning an
/// `f32` (with `with`, an `(f32, T)`), into a tape of type `tape`, built by `fns`.
#[derive(Clone, Copy, Debug)]
pub struct TapeOf {
    pub ncap: u32,
    pub tape: TypeId,
    pub with: bool,
    pub fns: TapeFns,
}

/// std's functions that build a tape (`std::stage`'s `push_one`, `push_two`, `push_new` and
/// `finish`), for a tape of one size, and the type they build it in (its `Building`).
#[derive(Clone, Copy, Debug)]
pub struct TapeFns {
    pub building: TypeId,
    pub one: FuncId,
    pub two: FuncId,
    pub new: FuncId,
    pub finish: FuncId,
}

/// Where a tape's parts are in what builds it (`std::stage::Building<N, K>`, whose `tape` is a
/// `Taped<N, K>`): its table and its table's count, and the table's room; and std's functions
/// that add its operations.
#[derive(Clone, Copy, Debug)]
pub(super) struct Layout {
    pub building: TypeId,
    pub inner: u32,
    pub consts: u32,
    pub nconsts: u32,
    pub kcap: u32,
    pub fns: TapeFns,
}

impl Layout {
    /// The layout of `fns.building`: `{ tape: Taped<N, K>, .. }`, a `Taped` `{ consts: [f32; K],
    /// nconsts: u32, .. }`.
    pub(super) fn of(types: &Types, fns: TapeFns) -> Result<Layout> {
        let bad = || Error::internal("`std::stage::Building` isn't laid out as a tape's");
        let field = |t: TypeId, name: &str| match types.get(t) {
            TypeDef::Struct { fields, .. } => fields
                .iter()
                .position(|(n, _)| n == name)
                .map(|i| (i as u32, fields[i].1))
                .ok_or_else(bad),
            _ => Err(bad()),
        };
        let (inner, tape) = field(fns.building, "tape")?;
        let ((consts, table), (nconsts, _)) = (field(tape, "consts")?, field(tape, "nconsts")?);
        let TypeDef::Array(_, kcap) = *types.get(table) else { return Err(bad()) };
        Ok(Layout { building: fns.building, inner, consts, nconsts, kcap, fns })
    }
}

/// A function of `f`'s captures returning the tape of `f` (its point, parameter `ncap`, a
/// `vec3`): `f`'s value in its last register; with `with`, `f` returns `(f32, T)`, and the last
/// `1 + n` registers are the value and `T`'s `n` floats, in field order.
pub(super) fn tape(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    target: Target,
    of: TapeOf,
) -> Result<FuncId> {
    let TapeOf { ncap, tape: tape_ty, with, fns } = of;
    if target != Target::Cpu {
        return Err(Error::not_tapeable("a tape is made on the CPU, once, and uploaded"));
    }
    let lay = Layout::of(&m.types, fns)?;
    let func = m.functions[f.index()].clone_signature();
    let xi = ncap as usize;
    let x_ty = func.params.get(xi).ok_or_else(|| Error::internal("no input parameter"))?.ty;
    if m.types.get(x_ty) != &TypeDef::Vector(Scalar::F32, 3) {
        return Err(Error::not_tapeable("a tape is of a function of a `vec3`"));
    }
    let ret = func.ret.ok_or_else(|| Error::internal("a field returns nothing"))?;
    let f32 = m.types.f32();
    match (with, m.types.get(ret)) {
        (false, _) if ret == f32 => {}
        (true, TypeDef::Struct { fields, .. }) if fields.first().map(|f| f.1) == Some(f32) => {}
        (false, _) => {
            return Err(Error::not_tapeable("a tape is of a function returning an `f32`"));
        }
        (true, _) => {
            return Err(Error::not_tapeable("`tape_with` needs a function returning `(f32, T)`"));
        }
    }
    let mut mask = vec![0; func.params.len()];
    mask[xi] = all_bits(&m.types, x_ty);
    let d = derive(m, cache, f, mask.clone(), lay)?;
    let returns = returns_active(m, cache, f, mask, Mode::Interval) != 0;
    let mut params = func.params.clone();
    params.truncate(xi);
    let mut g = Function::new(format!("{}_tape", func.name), params, Some(tape_ty));
    let mut body = Vec::new();
    let t = g.new_local("building", lay.building);
    let zero = g.let_(&mut body, lay.building, Expr::Zero(lay.building));
    body.push(Stmt::Store(Place::local(t), zero));
    let tp = Place::local(t);
    // The point's coordinates: registers 0, 1 and 2.
    let mut coords = Vec::new();
    for k in 0..3u32 {
        let r = emit(&mut g, &m.types, &mut body, &tp, lay, Push::One(OpArgs::leaf(X + k, None)))?;
        coords.push(r);
    }
    let x_regs = g.let_(&mut body, x_ty, Expr::Construct(x_ty, coords));
    let x_value = g.let_(&mut body, x_ty, Expr::Zero(x_ty));
    let mut args: Vec<Arg> = (0..xi as u32).map(|i| g.param_arg(i, &mut body)).collect();
    let pass = |g: &mut Function, body: &mut Block, v: ValueId, by_ref: bool, name: &str| {
        if by_ref {
            let l = g.new_local(name.to_string(), x_ty);
            body.push(Stmt::Store(Place::local(l), v));
            Arg::Place(Place::local(l))
        } else {
            Arg::Value(v)
        }
    };
    let by_ref = func.params[xi].by_ref;
    args.push(pass(&mut g, &mut body, x_value, by_ref, "x"));
    args.push(pass(&mut g, &mut body, x_regs, by_ref, "x_regs"));
    args.push(Arg::Place(tp.clone()));
    let dret = m.functions[d.index()].ret.ok_or_else(|| Error::internal("no result"))?;
    let r = g.let_(&mut body, dret, Expr::Call(d, args));
    // The value's registers (with `with`, the payload's too), copied last in order: what
    // `interpret` and `interpret_with` read. A constant field's value is a constant.
    let regs = if returns {
        let rt = reg_ty(&mut m.types, ret)?;
        g.let_(&mut body, rt, Expr::Extract(r, 1))
    } else {
        let mut cx = Consts { g: &mut g, types: &mut m.types, tape: &tp, lay };
        cx.materialize(&mut body, r)?
    };
    let leaves = scalar_leaves(&mut m.types, &mut g, &mut body, regs)?;
    let u = m.types.u32();
    let keep = leaves.len() as u32;
    for reg in leaves {
        if g.value_ty(reg) == u {
            return Err(Error::not_tapeable(
                "`tape_with`'s `T` holds `f32`s: scalars and vectors of them",
            ));
        }
        let a = g.let_(&mut body, u, Expr::Convert(reg, Scalar::U32));
        let copy = OpArgs { a: Some(a), ..OpArgs::leaf(COPY, None) };
        emit(&mut g, &m.types, &mut body, &tp, lay, Push::New(copy))?;
    }
    // What the value doesn't use dropped; past the tape's room, a panic with its counts.
    let keep = g.let_(&mut body, u, Expr::Const(Const::U32(keep)));
    let built = finish_arg(m, &mut g, &mut body, lay, &tp);
    let out = g.let_(&mut body, tape_ty, Expr::Call(lay.fns.finish, vec![built, Arg::Value(keep)]));
    body.push(Stmt::Return(Some(out)));
    g.body = body;
    Ok(m.add_function(g))
}

/// The type of the registers of a value of type `t`: its shape, every float or bool an `f32`
/// (a register's index), every integer a `u32` (its pair's first). A matrix, an enum, a pointer
/// or a 64-bit number that depends on the point can't be a tape's.
pub(super) fn reg_ty(types: &mut Types, t: TypeId) -> Result<TypeId> {
    let leaf = |s: Scalar| match s {
        Scalar::F64 | Scalar::I64 | Scalar::U64 => {
            Err(Error::not_tapeable("a tape's registers hold 32-bit numbers, not 64-bit ones"))
        }
        s if s.is_int() => Ok(Scalar::U32),
        _ => Ok(Scalar::F32),
    };
    Ok(match types.get(t).clone() {
        TypeDef::Scalar(s) => types.scalar(leaf(s)?),
        TypeDef::Vector(s, n) => types.vector_of(leaf(s)?, n),
        TypeDef::Array(e, n) => {
            let e = reg_ty(types, e)?;
            types.intern(TypeDef::Array(e, n))
        }
        TypeDef::Struct { name, fields } => {
            let mut fs = Vec::new();
            for (n, ft) in fields {
                fs.push((n, reg_ty(types, ft)?));
            }
            types.intern(TypeDef::Struct { name: format!("{name}_regs"), fields: fs })
        }
        _ => {
            return Err(Error::not_tapeable(format!(
                "a tape can't hold a `{}` that depends on the point",
                types.display(t)
            )));
        }
    })
}

/// A tape operation's operands: its opcode, its registers (`a`, `b`, the third), and its
/// constant, each a value of the function being built (`None`: 0).
#[derive(Clone, Copy)]
struct OpArgs {
    code: u32,
    a: Option<ValueId>,
    b: Option<ValueId>,
    t: Option<ValueId>,
    c: Option<ValueId>,
}

impl OpArgs {
    fn leaf(code: u32, c: Option<ValueId>) -> OpArgs {
        OpArgs { code, a: None, b: None, t: None, c }
    }
}

/// How an operation goes on the tape: found if the tape has it already, else added
/// (`push_one`); two in a row, found together or added together (`push_two`: an integer's
/// halves); or added where it's put (`push_new`: one of three in a row, or one of the value's).
#[derive(Clone, Copy)]
enum Push {
    One(OpArgs),
    Two(OpArgs, OpArgs),
    New(OpArgs),
}

/// The scalar type `s`, which the module has.
fn scalar_id(types: &Types, s: Scalar) -> Result<TypeId> {
    types.lookup(&TypeDef::Scalar(s)).ok_or_else(|| Error::internal(format!("no {s:?}")))
}

/// An operation's arguments to std's `push_*`: its code (the third register in its high 16
/// bits), its registers and its constant.
fn op_values(g: &mut Function, types: &Types, out: &mut Block, op: OpArgs) -> Result<[ValueId; 4]> {
    let u = scalar_id(types, Scalar::U32)?;
    let f32 = scalar_id(types, Scalar::F32)?;
    let mut code = g.let_(out, u, Expr::Const(Const::U32(op.code)));
    if let Some(t) = op.t {
        let sixteen = g.let_(out, u, Expr::Const(Const::U32(16)));
        let high = g.let_(out, u, Expr::Binary(BinOp::Shl, t, sixteen));
        code = g.let_(out, u, Expr::Binary(BinOp::BitOr, code, high));
    }
    let mut reg = |r: Option<ValueId>| match r {
        Some(r) => r,
        None => g.let_(out, u, Expr::Const(Const::U32(0))),
    };
    let (a, b) = (reg(op.a), reg(op.b));
    let c = match op.c {
        Some(c) => c,
        None => g.let_(out, f32, Expr::Const(Const::F32(0.0))),
    };
    Ok([code, a, b, c])
}

/// Puts an operation (or two) on the tape that `building` builds: its register's index (the
/// first's), as an `f32`. Past the tape's room the register is past it too, and `finish` panics.
fn emit(
    g: &mut Function,
    types: &Types,
    out: &mut Block,
    building: &Place,
    lay: Layout,
    push: Push,
) -> Result<ValueId> {
    let f32 = scalar_id(types, Scalar::F32)?;
    let (func, ops) = match push {
        Push::One(op) => (lay.fns.one, vec![op]),
        Push::Two(first, second) => (lay.fns.two, vec![first, second]),
        Push::New(op) => (lay.fns.new, vec![op]),
    };
    let mut args = vec![Arg::Place(building.clone())];
    for op in ops {
        args.extend(op_values(g, types, out, op)?.map(Arg::Value));
    }
    Ok(g.let_(out, f32, Expr::Call(func, args)))
}

/// The building passed to `finish`, as its parameter takes it.
fn finish_arg(m: &Module, g: &mut Function, out: &mut Block, lay: Layout, at: &Place) -> Arg {
    if m.functions[lay.fns.finish.index()].params[0].by_ref {
        return Arg::Place(at.clone());
    }
    Arg::Value(g.let_(out, lay.building, Expr::Load(at.clone())))
}

/// Element `i` of the tape's array field `field` of `cap` elements; past its end, its last
/// (the count goes on, and `finish` panics).
fn slot(
    g: &mut Function,
    types: &Types,
    out: &mut Block,
    tape: &Place,
    field: u32,
    i: ValueId,
    cap: u32,
) -> Result<Place> {
    let u = scalar_id(types, Scalar::U32)?;
    let b = scalar_id(types, Scalar::Bool)?;
    let n = g.let_(out, u, Expr::Const(Const::U32(cap)));
    let last = g.let_(out, u, Expr::Const(Const::U32(cap.saturating_sub(1))));
    let fits = g.let_(out, b, Expr::Binary(BinOp::Lt, i, n));
    let k = g.let_(out, u, Expr::Select { cond: fits, if_true: i, if_false: last });
    let mut at = tape.clone();
    at.path.extend([Proj::Field(field), Proj::Index(k)]);
    Ok(at)
}

/// Each scalar register of a register value, in field order.
fn scalar_leaves(
    types: &mut Types,
    g: &mut Function,
    out: &mut Block,
    regs: ValueId,
) -> Result<Vec<ValueId>> {
    let t = g.value_ty(regs);
    let f32 = types.f32();
    Ok(match types.get(t).clone() {
        TypeDef::Scalar(_) => vec![regs],
        TypeDef::Vector(s, n) => {
            let e = if s == Scalar::U32 { types.u32() } else { f32 };
            (0..n as u32).map(|k| g.let_(out, e, Expr::Extract(regs, k))).collect()
        }
        TypeDef::Struct { fields, .. } => {
            let mut all = Vec::new();
            for (k, (_, ft)) in fields.iter().enumerate() {
                let x = g.let_(out, *ft, Expr::Extract(regs, k as u32));
                all.extend(scalar_leaves(types, g, out, x)?);
            }
            all
        }
        TypeDef::Array(e, n) => {
            let mut all = Vec::new();
            for k in 0..n {
                let x = g.let_(out, e, Expr::Extract(regs, k));
                all.extend(scalar_leaves(types, g, out, x)?);
            }
            all
        }
        _ => return Err(Error::internal("registers of a type a tape doesn't hold")),
    })
}

/// What makes constants' registers: the function being built and its tape.
struct Consts<'a> {
    g: &'a mut Function,
    types: &'a mut Types,
    tape: &'a Place,
    lay: Layout,
}

impl Consts<'_> {
    /// Registers holding `x` (not active): a `CONST` operation for each of its numbers.
    fn materialize(&mut self, out: &mut Block, x: ValueId) -> Result<ValueId> {
        let t = self.g.value_ty(x);
        let f32 = self.types.f32();
        let rt = reg_ty(self.types, t)?;
        Ok(match self.types.get(t).clone() {
            TypeDef::Scalar(s) if s.is_int() => {
                // Its high and its low 16 bits, two constants in a row.
                let u = self.types.u32();
                let bits = if s == Scalar::U32 {
                    x
                } else {
                    self.g.let_(out, u, Expr::Convert(x, Scalar::U32))
                };
                let sixteen = self.g.let_(out, u, Expr::Const(Const::U32(16)));
                let mask = self.g.let_(out, u, Expr::Const(Const::U32(65535)));
                let hi = self.g.let_(out, u, Expr::Binary(BinOp::Shr, bits, sixteen));
                let lo = self.g.let_(out, u, Expr::Binary(BinOp::BitAnd, bits, mask));
                let (hf, lf) = (
                    self.g.let_(out, f32, Expr::Convert(hi, Scalar::F32)),
                    self.g.let_(out, f32, Expr::Convert(lo, Scalar::F32)),
                );
                let pair = Push::Two(OpArgs::leaf(CONST, Some(hf)), OpArgs::leaf(CONST, Some(lf)));
                let r = emit(self.g, self.types, out, self.tape, self.lay, pair)?;
                self.g.let_(out, u, Expr::Convert(r, Scalar::U32))
            }
            TypeDef::Scalar(s) => {
                let c = match s {
                    Scalar::F32 => x,
                    Scalar::Bool => {
                        let one = self.g.let_(out, f32, Expr::Const(Const::F32(1.0)));
                        let zero = self.g.let_(out, f32, Expr::Const(Const::F32(0.0)));
                        self.g.let_(
                            out,
                            f32,
                            Expr::Select { cond: x, if_true: one, if_false: zero },
                        )
                    }
                    _ => self.g.let_(out, f32, Expr::Convert(x, Scalar::F32)),
                };
                emit(
                    self.g,
                    self.types,
                    out,
                    self.tape,
                    self.lay,
                    Push::One(OpArgs::leaf(CONST, Some(c))),
                )?
            }
            TypeDef::Vector(s, n) => {
                let et = self.types.scalar(s);
                let mut comps = Vec::new();
                for k in 0..n as u32 {
                    let e = self.g.let_(out, et, Expr::Extract(x, k));
                    comps.push(self.materialize(out, e)?);
                }
                self.g.let_(out, rt, Expr::Construct(rt, comps))
            }
            TypeDef::Struct { fields, .. } => {
                let mut parts = Vec::new();
                for (k, (_, ft)) in fields.iter().enumerate() {
                    let e = self.g.let_(out, *ft, Expr::Extract(x, k as u32));
                    parts.push(self.materialize(out, e)?);
                }
                self.g.let_(out, rt, Expr::Construct(rt, parts))
            }
            TypeDef::Array(e, n) => {
                let mut parts = Vec::new();
                for k in 0..n {
                    let v = self.g.let_(out, e, Expr::Extract(x, k));
                    parts.push(self.materialize(out, v)?);
                }
                self.g.let_(out, rt, Expr::Construct(rt, parts))
            }
            _ => {
                return Err(Error::not_tapeable(format!(
                    "a tape can't hold a `{}`",
                    self.types.display(t)
                )));
            }
        })
    }
}

/// The tape-building function of `f` with active parameters `mask`: `f`'s parameters, a
/// register value for each active one (appended, in order), and the tape, written (last).
/// Returning `{value, registers}` when its result is active, else what `f` returns.
pub(super) fn derive(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    mask: Vec<Bits>,
    lay: Layout,
) -> Result<FuncId> {
    if let Some(&d) = cache.tape.get(&(f, mask.clone())) {
        return Ok(d);
    }
    let orig = m.functions[f.index()].clone();
    no_gpu_work(&orig.body)?;
    let returns = returns_active(m, cache, f, mask.clone(), Mode::Interval) != 0;
    // As an interval does: one exit, so a `return` under a branch on the point is a store both
    // sides can make, and a loop that leaves early on the point runs its count with flags.
    let (func, _, act) = super::one_exit(m, cache, f, orig, &mask);
    if let Some(at) = act.active_loop_exit {
        return Err(Error::not_tapeable(
            "a tape can't hold a loop that leaves early on what depends on the point",
        )
        .at(at));
    }
    no_write_through_mut(&func, &act, &mask, "registers")?;
    let mut nf = func.clone_signature();
    nf.name = format!("{}_taped", func.name);
    let mut rparam: Vec<Option<u32>> = vec![None; func.params.len()];
    for (i, p) in func.params.iter().enumerate() {
        if mask[i] != 0 {
            let ty = reg_ty(&mut m.types, p.ty)?;
            rparam[i] = Some(nf.params.len() as u32);
            nf.params.push(Param { name: format!("{}_regs", p.name), ty, ..p.clone() });
        }
    }
    let tape = nf.params.len() as u32;
    nf.params.push(Param { name: "tape".into(), ty: lay.building, by_ref: true, mutable: true });
    let mut rlocal: Vec<Option<LocalId>> = vec![None; func.locals.len()];
    for (l, decl) in func.locals.iter().enumerate() {
        if act.locals[l] != 0 {
            let ty = reg_ty(&mut m.types, decl.ty)?;
            rlocal[l] = Some(nf.new_local(format!("{}_regs", decl.name), ty));
        }
    }
    if returns {
        let r = func.ret.ok_or_else(|| Error::internal("a missing result type"))?;
        if func.ret_ref {
            return Err(Error::not_tapeable("a tape can't hold a projection's result"));
        }
        let rr = reg_ty(&mut m.types, r)?;
        nf.ret = Some(m.types.intern(TypeDef::Struct {
            name: format!("taped<{}>", m.types.display(r)),
            fields: vec![("v".into(), r), ("r".into(), rr)],
        }));
    }
    let id = m.add_function(Function::new(nf.name.clone(), nf.params.clone(), nf.ret));
    let key = (f, mask);
    cache.tape.insert(key.clone(), id);
    let mut b = Tp {
        m,
        cache,
        f: &func,
        nf,
        act,
        lay,
        rparam,
        rlocal,
        rv: HashMap::new(),
        zeros: HashSet::new(),
        products: HashMap::new(),
        tape,
        returns,
        track: Track::default(),
    };
    let body = match b.block(&func.body) {
        Ok(body) => body,
        Err(e) => {
            b.cache.tape.remove(&key);
            return Err(e);
        }
    };
    let mut nf = b.nf;
    nf.body = body;
    m.functions[id.index()] = nf;
    Ok(id)
}

struct Tp<'a> {
    m: &'a mut Module,
    cache: &'a mut DeriveCache,
    f: &'a Function,
    nf: Function,
    act: Activity,
    lay: Layout,
    rparam: Vec<Option<u32>>,
    rlocal: Vec<Option<LocalId>>,
    /// Each active value's registers.
    rv: HashMap<ValueId, ValueId>,
    /// The values that are zero (`Expr::Zero`): their registers are one constant's.
    zeros: HashSet<ValueId>,
    /// Integer products' registers, by their factors' (`IMUL`): a sum with one is an `IMAD`
    /// instead, and `finish` drops the product if nothing else reads it.
    products: HashMap<ValueId, (ValueId, ValueId)>,
    /// The tape's parameter.
    tape: u32,
    returns: bool,
    track: Track,
}

/// `Error::not_tapeable` placed nowhere yet: a statement's `Track` places it.
fn cant(what: impl Into<String>) -> Error {
    Error::not_tapeable(what)
}

impl Tp<'_> {
    fn tape_place(&self) -> Place {
        Place::root(PlaceRoot::Param(self.tape))
    }

    fn f32(&mut self) -> TypeId {
        self.m.types.f32()
    }

    fn u32(&mut self) -> TypeId {
        self.m.types.u32()
    }

    fn val(&mut self, out: &mut Block, ty: TypeId, e: Expr) -> ValueId {
        self.nf.let_(out, ty, e)
    }

    fn fconst(&mut self, out: &mut Block, x: f32) -> ValueId {
        let f = self.f32();
        self.val(out, f, Expr::Const(Const::F32(x)))
    }

    /// A register as a `u32` operand (an integer's is one already).
    fn as_u32(&mut self, out: &mut Block, r: ValueId) -> ValueId {
        let u = self.u32();
        if self.nf.value_ty(r) == u {
            return r;
        }
        self.val(out, u, Expr::Convert(r, Scalar::U32))
    }

    /// Whether register `r` is an integer's (its pair's first).
    fn is_pair(&mut self, r: ValueId) -> bool {
        let u = self.u32();
        self.nf.value_ty(r) == u
    }

    /// The second register of the pair `r` begins.
    fn low(&mut self, out: &mut Block, r: ValueId) -> ValueId {
        let u = self.u32();
        let one = self.val(out, u, Expr::Const(Const::U32(1)));
        self.val(out, u, Expr::Binary(BinOp::Add, r, one))
    }

    /// Integer operation `code` of the pairs `regs`, which writes an integer: its two halves,
    /// in a row.
    fn int_op(&mut self, out: &mut Block, code: u32, regs: &[ValueId]) -> Result<ValueId> {
        let (hi, lo) = (self.fconst(out, 0.0), self.fconst(out, 1.0));
        let (first, second) =
            (self.args(out, code, regs, Some(hi)), self.args(out, code, regs, Some(lo)));
        let r = self.push(out, Push::Two(first, second))?;
        Ok(self.as_u32(out, r))
    }

    /// Two constants in a row: an integer's halves.
    fn const_pair(&mut self, out: &mut Block, hi: f32, lo: f32) -> Result<ValueId> {
        let (h, l) = (self.fconst(out, hi), self.fconst(out, lo));
        self.push(out, Push::Two(OpArgs::leaf(CONST, Some(h)), OpArgs::leaf(CONST, Some(l))))
    }

    /// The integer `x`, two constants in a row.
    fn int_const(&mut self, out: &mut Block, x: u32) -> Result<ValueId> {
        let r = self.const_pair(out, (x >> 16) as f32, (x & 65535) as f32)?;
        Ok(self.as_u32(out, r))
    }

    /// `cond > 0 ? a : b` for two registers of one kind: a float's, or an integer's pair.
    fn select1(
        &mut self,
        out: &mut Block,
        cond: ValueId,
        a: ValueId,
        b: ValueId,
    ) -> Result<ValueId> {
        if !self.is_pair(a) {
            return self.op(out, SELECT, &[cond, a, b], None);
        }
        let (al, bl) = (self.low(out, a), self.low(out, b));
        let (first, second) = (
            self.args(out, SELECT, &[cond, a, b], None),
            self.args(out, SELECT, &[cond, al, bl], None),
        );
        let r = self.push(out, Push::Two(first, second))?;
        Ok(self.as_u32(out, r))
    }

    /// A float register holding integer register `r`'s value (`signed`: an `i32`'s); a float's
    /// is its own.
    fn as_float(&mut self, out: &mut Block, r: ValueId, signed: bool) -> Result<ValueId> {
        if !self.is_pair(r) {
            return Ok(r);
        }
        self.op(out, if signed { I2F } else { U2F }, &[r, r], None)
    }

    /// Operation `code` of registers `regs` (`a`, `b`, the third) and constant `c`.
    fn args(&mut self, out: &mut Block, code: u32, regs: &[ValueId], c: Option<ValueId>) -> OpArgs {
        let mut args = OpArgs::leaf(code, c);
        let r: Vec<ValueId> = regs.iter().map(|&x| self.as_u32(out, x)).collect();
        args.a = r.first().copied();
        args.b = r.get(1).copied().or(args.a);
        args.t = r.get(2).copied();
        args
    }

    fn push(&mut self, out: &mut Block, push: Push) -> Result<ValueId> {
        let tape = self.tape_place();
        emit(&mut self.nf, &self.m.types, out, &tape, self.lay, push)
    }

    fn op(
        &mut self,
        out: &mut Block,
        code: u32,
        regs: &[ValueId],
        c: Option<ValueId>,
    ) -> Result<ValueId> {
        let args = self.args(out, code, regs, c);
        self.push(out, Push::One(args))
    }

    /// The building's tape, whose table a table read fills.
    fn table_place(&self) -> Place {
        let mut p = self.tape_place();
        p.path.push(Proj::Field(self.lay.inner));
        p
    }

    fn constant(&mut self, out: &mut Block, x: f32) -> Result<ValueId> {
        let c = self.fconst(out, x);
        self.op(out, CONST, &[], Some(c))
    }

    /// The registers of `x`: its own, or constants' for an inactive value.
    fn regs(&mut self, out: &mut Block, x: ValueId) -> Result<ValueId> {
        if let Some(&r) = self.rv.get(&x) {
            return Ok(r);
        }
        if self.zeros.contains(&x) {
            // A zeroed array of registers (an interpreter's) is two `CONST`s everywhere: a
            // float's and an integer's pair alike.
            let zero = self.const_pair(out, 0.0, 0.0)?;
            let rt = reg_ty(&mut self.m.types, self.nf.value_ty(x))?;
            let r = self.fill(out, rt, zero);
            self.rv.insert(x, r);
            return Ok(r);
        }
        let tape = self.tape_place();
        let mut cx =
            Consts { g: &mut self.nf, types: &mut self.m.types, tape: &tape, lay: self.lay };
        let r = cx.materialize(out, x)?;
        self.rv.insert(x, r);
        Ok(r)
    }

    /// A register value of type `rt` whose every register is `r` (an `f32`; as a `u32` for an
    /// integer's).
    fn fill(&mut self, out: &mut Block, rt: TypeId, r: ValueId) -> ValueId {
        let parts: Vec<ValueId> = match self.m.types.get(rt).clone() {
            TypeDef::Scalar(Scalar::U32) => return self.as_u32(out, r),
            TypeDef::Scalar(_) => return r,
            TypeDef::Vector(s, n) => {
                let leaf = if s == Scalar::U32 { self.as_u32(out, r) } else { r };
                return self.val(out, rt, Expr::Splat(leaf, n));
            }
            TypeDef::Array(e, n) => {
                let x = self.fill(out, e, r);
                vec![x; n as usize]
            }
            TypeDef::Struct { fields, .. } => {
                fields.iter().map(|(_, ft)| self.fill(out, *ft, r)).collect()
            }
            _ => vec![r],
        };
        self.val(out, rt, Expr::Construct(rt, parts))
    }

    /// The `k`th component's register of a register value (a scalar's is itself).
    fn comp(&mut self, out: &mut Block, r: ValueId, k: u32) -> ValueId {
        match self.m.types.get(self.nf.value_ty(r)).clone() {
            TypeDef::Vector(s, _) => {
                let e = self.m.types.scalar(s);
                self.val(out, e, Expr::Extract(r, k))
            }
            _ => r,
        }
    }

    /// How many components a value of type `t` has: a vector's, else 1.
    fn width(&self, t: TypeId) -> Result<u8> {
        match self.m.types.get(t) {
            TypeDef::Scalar(_) => Ok(1),
            TypeDef::Vector(_, n) => Ok(*n),
            _ => Err(cant(format!("a tape can't compute with a `{}`", self.m.types.display(t)))),
        }
    }

    /// `each(component registers)` over each component of `ty` (a scalar or a vector), the
    /// operands' registers `rs` (a scalar operand of a vector operation is every component's).
    fn each(
        &mut self,
        out: &mut Block,
        ty: TypeId,
        rs: &[ValueId],
        mut each: impl FnMut(&mut Self, &mut Block, &[ValueId]) -> Result<ValueId>,
    ) -> Result<ValueId> {
        let n = self.width(ty)?;
        let mut comps = Vec::new();
        for k in 0..n as u32 {
            let parts: Vec<ValueId> = rs.iter().map(|&r| self.comp(out, r, k)).collect();
            comps.push(each(self, out, &parts)?);
        }
        if n == 1 {
            return Ok(comps[0]);
        }
        let rt = reg_ty(&mut self.m.types, ty)?;
        Ok(self.val(out, rt, Expr::Construct(rt, comps)))
    }

    /// `code` of each component of the operands.
    fn lift(&mut self, out: &mut Block, ty: TypeId, code: u32, xs: &[ValueId]) -> Result<ValueId> {
        let rs: Vec<ValueId> = xs.iter().map(|&x| self.regs(out, x)).collect::<Result<_>>()?;
        self.each(out, ty, &rs, |s, out, p| s.op(out, code, p, None))
    }

    /// Three registers in a row holding `v`'s components (a `vec3`'s registers): its first.
    /// They often are in a row already (three operations, one per component); otherwise, copies.
    fn in_a_row(&mut self, out: &mut Block, v: ValueId) -> Result<ValueId> {
        let (f, b) = (self.f32(), self.m.types.bool());
        let c: Vec<ValueId> = (0..3).map(|k| self.comp(out, v, k)).collect();
        let row = self.nf.new_local("row", f);
        out.push(Stmt::Store(Place::local(row), c[0]));
        let mut apart = None;
        for k in 1..3 {
            let step = self.fconst(out, k as f32);
            let at = self.val(out, f, Expr::Binary(BinOp::Add, c[0], step));
            let off = self.val(out, b, Expr::Binary(BinOp::Ne, c[k], at));
            apart = Some(match apart {
                None => off,
                Some(a) => self.val(out, b, Expr::Binary(BinOp::Or, a, off)),
            });
        }
        let mut copies = Vec::new();
        let mut first = None;
        for &ck in &c {
            let copy = self.args(&mut copies, COPY, &[ck], None);
            let r = self.push(&mut copies, Push::New(copy))?;
            first.get_or_insert(r);
        }
        let first = first.ok_or_else(|| Error::internal("no components"))?;
        copies.push(Stmt::Store(Place::local(row), first));
        let apart = apart.ok_or_else(|| Error::internal("no components"))?;
        out.push(Stmt::If { cond: apart, then: copies, else_: Vec::new() });
        Ok(self.val(out, f, Expr::Load(Place::local(row))))
    }

    fn block(&mut self, b: &Block) -> Result<Block> {
        let mut out = Vec::new();
        for s in b {
            self.track.before(s);
            let r = self.stmt(s, &mut out);
            self.track.after(s, r)?;
        }
        Ok(out)
    }

    fn stmt(&mut self, s: &Stmt, out: &mut Block) -> Result<()> {
        match s {
            Stmt::Let(v, Expr::Call(g, args)) => self.call(Some(*v), *g, args, out),
            Stmt::Eval(Expr::Call(g, args)) => self.call(None, *g, args, out),
            Stmt::Let(v, e) => {
                if !self.act.value(*v) {
                    if matches!(e, Expr::Zero(_)) {
                        self.zeros.insert(*v);
                    }
                    out.push(s.clone());
                    return Ok(());
                }
                // What depends on the point is computed by the tape, not here: its value is
                // left zero, so a side of a branch that the point would skip runs safely.
                let ty = self.f.value_ty(*v);
                let r = self.reg_expr(*v, e, out)?;
                out.push(Stmt::Let(*v, Expr::Zero(ty)));
                self.rv.insert(*v, r);
                Ok(())
            }
            Stmt::Store(p, v) => {
                out.push(s.clone());
                if self.act.place(self.m, self.f, p) {
                    let rp = self.reg_place(p)?;
                    let r = self.regs(out, *v)?;
                    out.push(Stmt::Store(rp, r));
                }
                Ok(())
            }
            Stmt::If { cond, then, else_ } if self.act.value(*cond) => {
                self.active_if(*cond, then, else_, out)
            }
            Stmt::Return(Some(v)) if self.returns => {
                let r = self.regs(out, *v)?;
                let dual = self.nf.ret.ok_or_else(|| Error::internal("a missing result type"))?;
                let d = self.val(out, dual, Expr::Construct(dual, vec![*v, r]));
                out.push(Stmt::Return(Some(d)));
                Ok(())
            }
            other => pass_through(other, out, &mut |b| self.block(b)),
        }
    }

    /// A place's registers: the same path in its root's register local or parameter.
    fn reg_place(&self, p: &Place) -> Result<Place> {
        if self.act.index_active(p) {
            return Err(cant("a tape can't write at an index that depends on the point"));
        }
        let root = match &p.root {
            PlaceRoot::Local(l) => PlaceRoot::Local(
                self.rlocal[l.index()]
                    .ok_or_else(|| Error::internal("an inactive local's registers"))?,
            ),
            PlaceRoot::Param(i) => PlaceRoot::Param(
                self.rparam[*i as usize]
                    .ok_or_else(|| Error::internal("an inactive parameter's registers"))?,
            ),
            _ => return Err(cant("a tape can't follow a pointer that depends on the point")),
        };
        Ok(Place { root, path: p.path.clone() })
    }

    /// `if cond { then } else { else_ }` on an active condition: both sides run, each writing
    /// the registers of the locals it changes, and each such local takes, per register,
    /// `select(cond, then's, else's)`. Neither side may leave the function or a loop, or write
    /// anything but a local.
    fn active_if(
        &mut self,
        cond: ValueId,
        then: &Block,
        else_: &Block,
        out: &mut Block,
    ) -> Result<()> {
        // `depth`: how many loops in the branch `b` is in; a `break` of one of them stays in it.
        fn check(b: &Block, depth: u32, written: &mut Vec<LocalId>) -> Result<()> {
            for s in b {
                match s {
                    Stmt::Break | Stmt::Continue if depth > 0 => {}
                    Stmt::Return(_) | Stmt::Break | Stmt::Continue | Stmt::Trap => {
                        return Err(cant(
                            "a tape can't hold a branch on what depends on the point that leaves early",
                        ));
                    }
                    Stmt::Store(p, _) => match p.root {
                        PlaceRoot::Local(l) => written.push(l),
                        _ => {
                            return Err(cant(
                                "a tape can't hold a branch on what depends on the point that writes through a reference",
                            ));
                        }
                    },
                    _ => {}
                }
                let deeper = depth + u32::from(matches!(s, Stmt::Loop { .. }));
                for inner in s.blocks() {
                    check(inner, deeper, written)?;
                }
            }
            Ok(())
        }
        let mut written = Vec::new();
        check(then, 0, &mut written)?;
        check(else_, 0, &mut written)?;
        let mut seen = HashSet::new();
        written.retain(|l| seen.insert(*l) && self.rlocal[l.index()].is_some());
        let c = self.regs(out, cond)?;
        // Before: each written local's registers, kept for the other side.
        let mut before = Vec::new();
        for &l in &written {
            let rl = self.rlocal[l.index()].unwrap_or(l);
            let ty = self.nf.locals[rl.index()].ty;
            let keep = self.nf.new_local("before", ty);
            let v = self.val(out, ty, Expr::Load(Place::local(rl)));
            out.push(Stmt::Store(Place::local(keep), v));
            before.push((rl, keep, ty));
        }
        let t = self.block(then)?;
        out.extend(t);
        let mut after_then = Vec::new();
        for &(rl, keep, ty) in &before {
            let mine = self.nf.new_local("then", ty);
            let v = self.val(out, ty, Expr::Load(Place::local(rl)));
            out.push(Stmt::Store(Place::local(mine), v));
            let k = self.val(out, ty, Expr::Load(Place::local(keep)));
            out.push(Stmt::Store(Place::local(rl), k));
            after_then.push(mine);
        }
        let e = self.block(else_)?;
        out.extend(e);
        for (&(rl, _, ty), mine) in before.iter().zip(after_then) {
            let a = self.val(out, ty, Expr::Load(Place::local(mine)));
            let b = self.val(out, ty, Expr::Load(Place::local(rl)));
            let s = self.select_regs(out, c, a, b)?;
            out.push(Stmt::Store(Place::local(rl), s));
        }
        Ok(())
    }

    /// `cond > 0 ? a : b`, register by register, for register values `a` and `b`.
    fn select_regs(
        &mut self,
        out: &mut Block,
        cond: ValueId,
        a: ValueId,
        b: ValueId,
    ) -> Result<ValueId> {
        let ty = self.nf.value_ty(a);
        match self.m.types.get(ty).clone() {
            TypeDef::Scalar(_) => self.select1(out, cond, a, b),
            TypeDef::Vector(_, n) => {
                let mut comps = Vec::new();
                for k in 0..n as u32 {
                    let (x, y) = (self.comp(out, a, k), self.comp(out, b, k));
                    comps.push(self.select1(out, cond, x, y)?);
                }
                Ok(self.val(out, ty, Expr::Construct(ty, comps)))
            }
            TypeDef::Struct { fields, .. } => {
                let mut parts = Vec::new();
                for (k, (_, ft)) in fields.iter().enumerate() {
                    let x = self.val(out, *ft, Expr::Extract(a, k as u32));
                    let y = self.val(out, *ft, Expr::Extract(b, k as u32));
                    parts.push(self.select_regs(out, cond, x, y)?);
                }
                Ok(self.val(out, ty, Expr::Construct(ty, parts)))
            }
            TypeDef::Array(e, n) => {
                let mut parts = Vec::new();
                for k in 0..n {
                    let x = self.val(out, e, Expr::Extract(a, k));
                    let y = self.val(out, e, Expr::Extract(b, k));
                    parts.push(self.select_regs(out, cond, x, y)?);
                }
                Ok(self.val(out, ty, Expr::Construct(ty, parts)))
            }
            _ => Err(cant("a tape can't select between these")),
        }
    }

    fn call(&mut self, v: Option<ValueId>, g: FuncId, args: &[Arg], out: &mut Block) -> Result<()> {
        let mask: Vec<Bits> = args.iter().map(|a| arg_bits(self.m, self.f, &self.act, a)).collect();
        if mask.iter().all(|b| *b == 0) {
            out.push(let_or_eval(v, Expr::Call(g, args.to_vec())));
            return Ok(());
        }
        // std's own functions that are one operation of the tape.
        if let (Some(v), Some(known)) = (v, self.m.functions[g.index()].known) {
            let ty = self.f.value_ty(v);
            out.push(Stmt::Let(v, Expr::Zero(ty)));
            // Each argument's value, and its registers.
            let mut vals = Vec::new();
            let mut regs = Vec::new();
            for a in args {
                let (x, r) = self.arg_regs(out, a)?;
                vals.push(x);
                regs.push(r);
            }
            let r = match known {
                Known::Smin => {
                    if mask[2] != 0 {
                        return Err(cant(
                            "a tape's smooth minimum blends over a width that doesn't depend on the point",
                        ));
                    }
                    self.op(out, SMIN, &[regs[0], regs[1]], Some(vals[2]))?
                }
                Known::ValueNoise => {
                    let p = regs[0];
                    let (x, y, z) =
                        (self.comp(out, p, 0), self.comp(out, p, 1), self.comp(out, p, 2));
                    let one = self.fconst(out, 1.0);
                    self.op(out, NOISE3, &[x, y, z], Some(one))?
                }
                // An `i32` toward zero; `HASH` floors its operands' values and takes their
                // `lattice_coord`, which gives each one back.
                Known::LatticeCoord => self.int_op(out, F2I, &[regs[0], regs[0]])?,
                Known::Pcg => self.int_op(out, PCG, &[regs[0], regs[0]])?,
                Known::Lattice => {
                    let mut coords = Vec::new();
                    for &r in &regs[..3] {
                        coords.push(self.as_float(out, r, true)?);
                    }
                    self.op(out, HASH, &coords, None)?
                }
            };
            self.rv.insert(v, r);
            return Ok(());
        }
        let returns = returns_active(self.m, self.cache, g, mask.clone(), Mode::Interval) != 0;
        let params = &self.m.functions[g.index()].params;
        let writes = mask.iter().zip(params).any(|(b, p)| *b != 0 && p.mutable);
        if !returns && !writes {
            out.push(let_or_eval(v, Expr::Call(g, args.to_vec())));
            return Ok(());
        }
        let at = self.track.at;
        let dg =
            derive(self.m, self.cache, g, mask.clone(), self.lay).map_err(|e| e.through(at))?;
        let mut new_args = args.to_vec();
        for (i, a) in args.iter().enumerate() {
            if mask[i] == 0 {
                continue;
            }
            new_args.push(match a {
                Arg::Value(x) => Arg::Value(self.regs(out, *x)?),
                Arg::Place(p) => {
                    if self.act.place(self.m, self.f, p) {
                        Arg::Place(self.reg_place(p)?)
                    } else {
                        // An inactive place passed where an active parameter is: its registers
                        // are its value's constants.
                        let ty = place_type(self.m, self.f, p)?;
                        let x = self.val(out, ty, Expr::Load(p.clone()));
                        let r = self.regs(out, x)?;
                        let rt = self.nf.value_ty(r);
                        let l = self.nf.new_local("regs", rt);
                        out.push(Stmt::Store(Place::local(l), r));
                        Arg::Place(Place::local(l))
                    }
                }
            });
        }
        new_args.push(Arg::Place(self.tape_place()));
        match (v, returns) {
            (Some(v), true) => {
                let dual = self.m.functions[dg.index()]
                    .ret
                    .ok_or_else(|| Error::internal("a missing result type"))?;
                let r = self.val(out, dual, Expr::Call(dg, new_args));
                out.push(Stmt::Let(v, Expr::Extract(r, 0)));
                let rt = reg_ty(&mut self.m.types, self.f.value_ty(v))?;
                let regs = self.val(out, rt, Expr::Extract(r, 1));
                self.rv.insert(v, regs);
            }
            (v, _) => out.push(let_or_eval(v, Expr::Call(dg, new_args))),
        }
        Ok(())
    }

    /// An argument's value and its registers: a place's are its registers' place's, if it's
    /// active.
    fn arg_regs(&mut self, out: &mut Block, a: &Arg) -> Result<(ValueId, ValueId)> {
        match a {
            Arg::Value(x) => Ok((*x, self.regs(out, *x)?)),
            Arg::Place(p) => {
                let ty = place_type(self.m, self.f, p)?;
                let x = self.val(out, ty, Expr::Load(p.clone()));
                if !self.act.place(self.m, self.f, p) {
                    return Ok((x, self.regs(out, x)?));
                }
                let rt = reg_ty(&mut self.m.types, ty)?;
                let rp = self.reg_place(p)?;
                Ok((x, self.val(out, rt, Expr::Load(rp))))
            }
        }
    }

    /// The registers of active `v = e`.
    fn reg_expr(&mut self, v: ValueId, e: &Expr, out: &mut Block) -> Result<ValueId> {
        let ty = self.f.value_ty(v);
        let rt = reg_ty(&mut self.m.types, ty)?;
        Ok(match e {
            Expr::Param(i) => {
                let p = self.rparam[*i as usize]
                    .ok_or_else(|| Error::internal("an inactive parameter read as active"))?;
                self.val(out, rt, Expr::Param(p))
            }
            Expr::Load(p) => {
                if !self.act.place(self.m, self.f, p) {
                    return self.regs(out, v);
                }
                if self.act.index_active(p) && !self.act.root(&p.root) {
                    return self.table_load(out, p, ty);
                }
                let rp = self.reg_place(p)?;
                self.val(out, rt, Expr::Load(rp))
            }
            Expr::Unary(UnOp::Neg, x) => {
                let r = self.regs(out, *x)?;
                self.each(out, ty, &[r], |s, out, p| {
                    if !s.is_pair(p[0]) {
                        return s.op(out, NEG, p, None);
                    }
                    let zero = s.int_const(out, 0)?;
                    s.int_op(out, ISUB, &[zero, p[0]])
                })?
            }
            Expr::Unary(UnOp::Not, x) => {
                let r = self.regs(out, *x)?;
                self.each(out, ty, &[r], |s, out, p| {
                    if s.is_pair(p[0]) {
                        let ones = s.int_const(out, u32::MAX)?;
                        return s.int_op(out, XOR, &[p[0], ones]);
                    }
                    let one = s.constant(out, 1.0)?;
                    s.op(out, SUB, &[one, p[0]], None)
                })?
            }
            Expr::Binary(op, a, b) => self.binary(out, *op, *a, *b, ty)?,
            Expr::Builtin(b, args) => self.builtin(out, *b, args, ty)?,
            Expr::Construct(_, parts) => {
                let mut rs = Vec::new();
                for p in parts {
                    rs.push(self.regs(out, *p)?);
                }
                self.val(out, rt, Expr::Construct(rt, rs))
            }
            Expr::Extract(x, i) => {
                let r = self.regs(out, *x)?;
                self.val(out, rt, Expr::Extract(r, *i))
            }
            Expr::ExtractDyn(x, i) if self.act.value(*i) && !self.act.value(*x) => {
                let xt = self.f.value_ty(*x);
                let n = match self.m.types.get(xt) {
                    TypeDef::Array(_, n) => *n,
                    TypeDef::Vector(_, n) => *n as u32,
                    _ => return Err(cant("a tape can't read at an index into this")),
                };
                let mut elems = Vec::new();
                for k in 0..n {
                    elems.push(self.val(out, ty, Expr::Extract(*x, k)));
                }
                self.table(out, &elems, *i, ty)?
            }
            Expr::ExtractDyn(x, i) => {
                if self.act.value(*i) {
                    return Err(cant(
                        "a tape can't read at an index that depends on the point from what depends on it",
                    ));
                }
                let r = self.regs(out, *x)?;
                self.val(out, rt, Expr::ExtractDyn(r, *i))
            }
            Expr::Splat(x, n) => {
                let r = self.regs(out, *x)?;
                self.val(out, rt, Expr::Splat(r, *n))
            }
            Expr::Swizzle(x, c) => {
                let r = self.regs(out, *x)?;
                self.val(out, rt, Expr::Swizzle(r, c.clone()))
            }
            Expr::Convert(x, to) => {
                let from = self.m.types.element_scalar(self.f.value_ty(*x)).unwrap_or(Scalar::F32);
                let to = *to;
                let r = self.regs(out, *x)?;
                match (from, to) {
                    (f, t) if f.is_int() && t.is_int() => {
                        if f.bits() < 32 || t.bits() < 32 {
                            return Err(cant("a tape's integers are 32-bit: `u32` and `i32`"));
                        }
                        r
                    }
                    // Toward zero; out of range, 0.
                    (f, t) if f.is_float() && t.is_int() => {
                        let code = if t.signed() { F2I } else { F2U };
                        self.each(out, ty, &[r], |s, out, p| s.int_op(out, code, &[p[0], p[0]]))?
                    }
                    (f, t) if f.is_int() && t.is_float() => {
                        self.each(out, ty, &[r], |s, out, p| s.as_float(out, p[0], f.signed()))?
                    }
                    // A bool's register is 1 or 0 already; as an integer, a pair of 0 and it.
                    (Scalar::Bool, t) if t.is_int() => self.each(out, ty, &[r], |s, out, p| {
                        let zero = s.fconst(out, 0.0);
                        let copy = s.args(out, COPY, &[p[0]], None);
                        let hi = s.push(out, Push::Two(OpArgs::leaf(CONST, Some(zero)), copy))?;
                        Ok(s.as_u32(out, hi))
                    })?,
                    (f, Scalar::Bool) if f.is_int() => self.each(out, ty, &[r], |s, out, p| {
                        let zero = s.int_const(out, 0)?;
                        let eq = s.op(out, IEQ, &[p[0], zero], None)?;
                        let one = s.constant(out, 1.0)?;
                        s.op(out, SUB, &[one, eq], None)
                    })?,
                    (Scalar::Bool, Scalar::Bool) => r,
                    (f, t) if (f.is_float() || f == Scalar::Bool) && t.is_float() => r,
                    _ => {
                        return Err(cant(
                            "a tape can't hold this conversion of what depends on the point",
                        ));
                    }
                }
            }
            Expr::Bitcast(x, to) => {
                let from = self.m.types.element_scalar(self.f.value_ty(*x)).unwrap_or(Scalar::F32);
                if !(from.is_int() && to.is_int()) {
                    return Err(cant("a tape can't hold a float's bits that depend on the point"));
                }
                self.regs(out, *x)?
            }
            Expr::Select { cond, if_true, if_false } => {
                let (a, b) = (self.regs(out, *if_true)?, self.regs(out, *if_false)?);
                if self.act.value(*cond) {
                    let c = self.regs(out, *cond)?;
                    self.each(out, ty, &[c, a, b], |s, out, p| s.select1(out, p[0], p[1], p[2]))?
                } else {
                    self.val(out, rt, Expr::Select { cond: *cond, if_true: a, if_false: b })
                }
            }
            Expr::Const(_) | Expr::Zero(_) => self.regs(out, v)?,
            other => {
                return Err(cant(format!(
                    "a tape can't hold this operation on what depends on the point ({})",
                    expr_kind(other)
                )));
            }
        })
    }

    /// A read of `p` (of type `ty`) at an index that depends on the point, from what doesn't: a
    /// table of what each index would read, and a `LOAD` of it. Only the first such index is
    /// the table's; the rest of the path must not depend on the point.
    fn table_load(&mut self, out: &mut Block, p: &Place, ty: TypeId) -> Result<ValueId> {
        let at = p
            .path
            .iter()
            .position(|x| matches!(x, Proj::Index(v) if self.act.value(*v)))
            .ok_or_else(|| Error::internal("no index that depends on the point"))?;
        let Proj::Index(i) = p.path[at] else { return Err(Error::internal("not an index")) };
        if p.path[at + 1..].iter().any(|x| matches!(x, Proj::Index(v) if self.act.value(*v))) {
            return Err(cant("a tape can't read at two indices that depend on the point"));
        }
        let prefix = Place { root: p.root.clone(), path: p.path[..at].to_vec() };
        let n = match place_type(self.m, self.f, &prefix).map(|t| self.m.types.get(t).clone())? {
            TypeDef::Array(_, n) => n,
            TypeDef::Vector(_, n) => n as u32,
            _ => return Err(cant("a tape can't read at an index into this")),
        };
        let u = self.u32();
        let mut elems = Vec::new();
        for k in 0..n {
            let kv = self.val(out, u, Expr::Const(Const::U32(k)));
            let mut q = prefix.clone();
            q.path.push(Proj::Index(kv));
            q.path.extend(p.path[at + 1..].iter().cloned());
            elems.push(self.val(out, ty, Expr::Load(q)));
        }
        self.table(out, &elems, i, ty)
    }

    /// The registers of `elems[i]` (each of type `ty`, none depending on the point) at the
    /// index `i`, a value of the function derived: each of their numbers in a table of the tape's, and a `LOAD` of it.
    fn table(
        &mut self,
        out: &mut Block,
        elems: &[ValueId],
        i: ValueId,
        ty: TypeId,
    ) -> Result<ValueId> {
        let f = self.f32();
        let signed_index =
            self.m.types.element_scalar(self.f.value_ty(i)).is_some_and(Scalar::signed);
        let i = self.regs(out, i)?;
        let n = elems.len() as u32;
        let width = match self.m.types.get(ty) {
            TypeDef::Scalar(_) => 1,
            TypeDef::Vector(_, w) => *w as u32,
            _ => return Err(cant("a tape's table holds numbers and vectors of them")),
        };
        let from = self.m.types.element_scalar(ty).unwrap_or(Scalar::F32);
        let mut comps = Vec::new();
        for c in 0..width {
            let base = self.table_base(out, n)?;
            for (k, &e) in elems.iter().enumerate() {
                let x = if width == 1 {
                    e
                } else {
                    let et = self.m.types.scalar(from);
                    self.val(out, et, Expr::Extract(e, c))
                };
                let x = match from {
                    Scalar::F32 => x,
                    Scalar::Bool => {
                        let one = self.fconst(out, 1.0);
                        let zero = self.fconst(out, 0.0);
                        self.val(out, f, Expr::Select { cond: x, if_true: one, if_false: zero })
                    }
                    _ => self.val(out, f, Expr::Convert(x, Scalar::F32)),
                };
                let u = self.u32();
                let kv = self.val(out, u, Expr::Const(Const::U32(k as u32)));
                let at = self.val(out, u, Expr::Binary(BinOp::Add, base, kv));
                let tape = self.table_place();
                let slot = slot(
                    &mut self.nf,
                    &self.m.types,
                    out,
                    &tape,
                    self.lay.consts,
                    at,
                    self.lay.kcap,
                )?;
                out.push(Stmt::Store(slot, x));
            }
            let bf = self.val(out, f, Expr::Convert(base, Scalar::F32));
            let ut = self.u32();
            let count = self.val(out, ut, Expr::Const(Const::U32(n)));
            let index = self.as_float(out, i, signed_index)?;
            let ia = self.as_u32(out, index);
            let args = OpArgs { code: LOAD, a: Some(ia), b: Some(ia), t: Some(count), c: Some(bf) };
            let read = self.push(out, Push::One(args))?;
            // An integer's table holds its values, exact below 2²⁴: back to a pair.
            comps.push(if from.is_int() {
                let code = if from.signed() { F2I } else { F2U };
                self.int_op(out, code, &[read, read])?
            } else {
                read
            });
        }
        if width == 1 {
            return Ok(comps[0]);
        }
        let rt = reg_ty(&mut self.m.types, ty)?;
        Ok(self.val(out, rt, Expr::Construct(rt, comps)))
    }

    /// Room for `n` more constants in the tape's table: where they start (a `u32`).
    fn table_base(&mut self, out: &mut Block, n: u32) -> Result<ValueId> {
        let u = self.u32();
        let mut at = self.table_place();
        at.path.push(Proj::Field(self.lay.nconsts));
        let base = self.val(out, u, Expr::Load(at.clone()));
        let nv = self.val(out, u, Expr::Const(Const::U32(n)));
        let next = self.val(out, u, Expr::Binary(BinOp::Add, base, nv));
        out.push(Stmt::Store(at, next));
        Ok(base)
    }

    /// `x` toward zero: `x > 0 ? floor(x) : -floor(-x)`.
    fn trunc(&mut self, out: &mut Block, x: ValueId) -> Result<ValueId> {
        let fl = self.op(out, FLOOR, &[x], None)?;
        let nx = self.op(out, NEG, &[x], None)?;
        let fln = self.op(out, FLOOR, &[nx], None)?;
        let up = self.op(out, NEG, &[fln], None)?;
        self.op(out, SELECT, &[x, fl, up], None)
    }

    fn binary(
        &mut self,
        out: &mut Block,
        op: BinOp,
        a: ValueId,
        b: ValueId,
        ty: TypeId,
    ) -> Result<ValueId> {
        let (at, bt) = (self.f.value_ty(a), self.f.value_ty(b));
        let is_matrix = |s: &Self, t: TypeId| matches!(s.m.types.get(t), TypeDef::Matrix(..));
        if is_matrix(self, at) || is_matrix(self, bt) {
            return self.matrix_mul(out, op, a, b, ty);
        }
        let scalar = self.m.types.element_scalar(at).unwrap_or(Scalar::F32);
        let (int, signed) = (scalar.is_int(), scalar.signed());
        let (ra, rb) = (self.regs(out, a)?, self.regs(out, b)?);
        let simple = |code: u32| {
            move |s: &mut Self, out: &mut Block, p: &[ValueId]| s.op(out, code, p, None)
        };
        let integer =
            |code: u32| move |s: &mut Self, out: &mut Block, p: &[ValueId]| s.int_op(out, code, p);
        let pick = |i: u32, u: u32| if signed { i } else { u };
        // `a < b`, 1 or 0, of two operands' component registers.
        let lt = move |s: &mut Self, out: &mut Block, x: ValueId, y: ValueId| {
            if int {
                s.op(out, pick(ILT, ULT), &[x, y], None)
            } else {
                s.op(out, STEP, &[x, y], None)
            }
        };
        match op {
            BinOp::Add | BinOp::WrappingAdd if int => self.each(out, ty, &[ra, rb], |s, out, p| {
                let fused = s
                    .products
                    .get(&p[0])
                    .map(|f| (*f, p[1]))
                    .or_else(|| s.products.get(&p[1]).map(|f| (*f, p[0])));
                match fused {
                    Some(((x, y), z)) => s.int_op(out, IMAD, &[x, y, z]),
                    None => s.int_op(out, IADD, p),
                }
            }),
            BinOp::Sub | BinOp::WrappingSub if int => self.each(out, ty, &[ra, rb], integer(ISUB)),
            BinOp::Mul | BinOp::WrappingMul if int => self.each(out, ty, &[ra, rb], |s, out, p| {
                let r = s.int_op(out, IMUL, p)?;
                s.products.insert(r, (p[0], p[1]));
                Ok(r)
            }),
            BinOp::Div if int => self.each(out, ty, &[ra, rb], integer(pick(IDIV, UDIV))),
            BinOp::Rem if int => self.each(out, ty, &[ra, rb], integer(pick(IREM, UREM))),
            BinOp::Shl if int => self.each(out, ty, &[ra, rb], integer(SHL)),
            BinOp::Shr if int => self.each(out, ty, &[ra, rb], integer(pick(ISHR, USHR))),
            BinOp::BitAnd if int => self.each(out, ty, &[ra, rb], integer(AND)),
            BinOp::BitOr if int => self.each(out, ty, &[ra, rb], integer(OR)),
            BinOp::BitXor if int => self.each(out, ty, &[ra, rb], integer(XOR)),
            BinOp::Add | BinOp::WrappingAdd => self.each(out, ty, &[ra, rb], simple(ADD)),
            BinOp::Sub | BinOp::WrappingSub => self.each(out, ty, &[ra, rb], simple(SUB)),
            BinOp::Mul | BinOp::WrappingMul => self.each(out, ty, &[ra, rb], simple(MUL)),
            BinOp::Div => self.each(out, ty, &[ra, rb], simple(DIV)),
            // `a - b trunc(a / b)`.
            BinOp::Rem => self.each(out, ty, &[ra, rb], |s, out, p| {
                let q = s.op(out, DIV, p, None)?;
                let t = s.trunc(out, q)?;
                let bt = s.op(out, MUL, &[p[1], t], None)?;
                s.op(out, SUB, &[p[0], bt], None)
            }),
            // Comparisons: 1 or 0.
            BinOp::Lt => self.each(out, ty, &[ra, rb], |s, out, p| lt(s, out, p[0], p[1])),
            BinOp::Gt => self.each(out, ty, &[ra, rb], |s, out, p| lt(s, out, p[1], p[0])),
            BinOp::Le | BinOp::Ge => {
                let (x, y) = if op == BinOp::Le { (rb, ra) } else { (ra, rb) };
                self.each(out, ty, &[x, y], |s, out, p| {
                    let l = lt(s, out, p[0], p[1])?;
                    let one = s.constant(out, 1.0)?;
                    s.op(out, SUB, &[one, l], None)
                })
            }
            BinOp::Eq | BinOp::Ne => self.each(out, ty, &[ra, rb], |s, out, p| {
                if int {
                    let eq = s.op(out, IEQ, p, None)?;
                    if op == BinOp::Eq {
                        return Ok(eq);
                    }
                    let one = s.constant(out, 1.0)?;
                    return s.op(out, SUB, &[one, eq], None);
                }
                // Apart: one of `a < b` and `b < a`.
                let (l, g) = (lt(s, out, p[0], p[1])?, lt(s, out, p[1], p[0])?);
                let apart = s.op(out, ADD, &[l, g], None)?;
                if op == BinOp::Ne {
                    return Ok(apart);
                }
                let one = s.constant(out, 1.0)?;
                s.op(out, SUB, &[one, apart], None)
            }),
            // Bools, 1 or 0.
            BinOp::And | BinOp::BitAnd => self.each(out, ty, &[ra, rb], simple(MUL)),
            BinOp::Or | BinOp::BitOr => self.each(out, ty, &[ra, rb], simple(MAX)),
            BinOp::BitXor => self.each(out, ty, &[ra, rb], |s, out, p| {
                let d = s.op(out, SUB, p, None)?;
                s.op(out, ABS, &[d], None)
            }),
            other => {
                Err(cant(format!("a tape can't hold `{other:?}` of what depends on the point")))
            }
        }
    }

    /// A matrix (not active) times a vector, or a vector times one: each component a sum of
    /// products.
    fn matrix_mul(
        &mut self,
        out: &mut Block,
        op: BinOp,
        a: ValueId,
        b: ValueId,
        ty: TypeId,
    ) -> Result<ValueId> {
        let (at, bt) = (self.f.value_ty(a), self.f.value_ty(b));
        let (matrix, vector, row_major) =
            match (self.m.types.get(at).clone(), self.m.types.get(bt).clone()) {
                (TypeDef::Matrix(..), TypeDef::Vector(..)) => (a, b, false),
                (TypeDef::Vector(..), TypeDef::Matrix(..)) => (b, a, true),
                _ => return Err(cant("a tape can't hold a matrix that depends on the point")),
            };
        if op != BinOp::Mul || self.act.value(matrix) {
            return Err(cant("a tape can't hold a matrix that depends on the point"));
        }
        let TypeDef::Matrix(cols, rows) = *self.m.types.get(self.f.value_ty(matrix)) else {
            return Err(Error::internal("not a matrix"));
        };
        let f = self.f32();
        let col_ty = self.m.types.vector_of(Scalar::F32, rows);
        let v = self.regs(out, vector)?;
        let n = self.width(ty)?;
        let mut comps = Vec::new();
        for i in 0..n as u32 {
            let mut sum: Option<ValueId> = None;
            let terms = if row_major { rows } else { cols };
            for j in 0..terms as u32 {
                // M v: Σ_j M[j][i] v[j]; v M: Σ_j M[i][j] v[j] (columns are M's parts).
                let (c, r) = if row_major { (i, j) } else { (j, i) };
                let col = self.val(out, col_ty, Expr::Extract(matrix, c));
                let m = self.val(out, f, Expr::Extract(col, r));
                let mr = self.op(out, CONST, &[], Some(m))?;
                let vj = self.comp(out, v, j);
                let p = self.op(out, MUL, &[mr, vj], None)?;
                sum = Some(match sum {
                    None => p,
                    Some(s) => self.op(out, ADD, &[s, p], None)?,
                });
            }
            comps.push(sum.ok_or_else(|| Error::internal("an empty matrix"))?);
        }
        if n == 1 {
            return Ok(comps[0]);
        }
        let rt = reg_ty(&mut self.m.types, ty)?;
        Ok(self.val(out, rt, Expr::Construct(rt, comps)))
    }

    fn builtin(
        &mut self,
        out: &mut Block,
        b: Builtin,
        args: &[ValueId],
        ty: TypeId,
    ) -> Result<ValueId> {
        use Builtin as B;
        let scalar = self.m.types.element_scalar(self.f.value_ty(args[0])).unwrap_or(Scalar::F32);
        if scalar.is_int() {
            return self.int_builtin(out, b, args, ty, scalar.signed());
        }
        let r = match b {
            B::Min => self.lift(out, ty, MIN, args)?,
            B::Max => self.lift(out, ty, MAX, args)?,
            B::Abs => self.lift(out, ty, ABS, args)?,
            B::Sqrt => self.lift(out, ty, SQRT, args)?,
            B::Sin => self.lift(out, ty, SIN, args)?,
            B::Cos => self.lift(out, ty, COS, args)?,
            B::Exp => self.lift(out, ty, EXP, args)?,
            B::Log => self.lift(out, ty, LOG, args)?,
            B::Floor => self.lift(out, ty, FLOOR, args)?,
            B::Fract => self.lift(out, ty, FRACT, args)?,
            B::Saturate => self.lift(out, ty, CLAMP01, args)?,
            B::Mix => self.lift(out, ty, MIX, args)?,
            B::Ceil => {
                let r = self.regs(out, args[0])?;
                self.each(out, ty, &[r], |s, out, p| {
                    let n = s.op(out, NEG, p, None)?;
                    let f = s.op(out, FLOOR, &[n], None)?;
                    s.op(out, NEG, &[f], None)
                })?
            }
            B::Trunc => {
                let r = self.regs(out, args[0])?;
                self.each(out, ty, &[r], |s, out, p| s.trunc(out, p[0]))?
            }
            B::Sign => {
                let r = self.regs(out, args[0])?;
                self.each(out, ty, &[r], |s, out, p| {
                    let zero = s.constant(out, 0.0)?;
                    let pos = s.op(out, STEP, &[zero, p[0]], None)?;
                    let neg = s.op(out, STEP, &[p[0], zero], None)?;
                    s.op(out, SUB, &[pos, neg], None)
                })?
            }
            B::Clamp => {
                // WGSL's: min(max(x, lo), hi).
                let rs: Vec<ValueId> =
                    args.iter().map(|&x| self.regs(out, x)).collect::<Result<_>>()?;
                self.each(out, ty, &rs, |s, out, p| {
                    let lo = s.op(out, MAX, &[p[0], p[1]], None)?;
                    s.op(out, MIN, &[lo, p[2]], None)
                })?
            }
            B::Step => {
                // step(edge, x): 1 where x >= edge.
                let rs: Vec<ValueId> =
                    args.iter().map(|&x| self.regs(out, x)).collect::<Result<_>>()?;
                self.each(out, ty, &rs, |s, out, p| {
                    let lt = s.op(out, STEP, &[p[1], p[0]], None)?;
                    let one = s.constant(out, 1.0)?;
                    s.op(out, SUB, &[one, lt], None)
                })?
            }
            B::Smoothstep => {
                let rs: Vec<ValueId> =
                    args.iter().map(|&x| self.regs(out, x)).collect::<Result<_>>()?;
                self.each(out, ty, &rs, |s, out, p| {
                    let d = s.op(out, SUB, &[p[2], p[0]], None)?;
                    let w = s.op(out, SUB, &[p[1], p[0]], None)?;
                    let q = s.op(out, DIV, &[d, w], None)?;
                    let t = s.op(out, CLAMP01, &[q], None)?;
                    let two = s.constant(out, 2.0)?;
                    let three = s.constant(out, 3.0)?;
                    let tt = s.op(out, MUL, &[two, t], None)?;
                    let k = s.op(out, SUB, &[three, tt], None)?;
                    let t2 = s.op(out, MUL, &[t, t], None)?;
                    s.op(out, MUL, &[t2, k], None)
                })?
            }
            B::Pow => {
                if self.act.value(args[1]) {
                    // exp(b log a).
                    let rs: Vec<ValueId> =
                        args.iter().map(|&x| self.regs(out, x)).collect::<Result<_>>()?;
                    self.each(out, ty, &rs, |s, out, p| {
                        let l = s.op(out, LOG, &[p[0]], None)?;
                        let m = s.op(out, MUL, &[p[1], l], None)?;
                        s.op(out, EXP, &[m], None)
                    })?
                } else {
                    let a = self.regs(out, args[0])?;
                    let e = args[1];
                    let n = self.width(ty)?;
                    let f = self.f32();
                    let mut comps = Vec::new();
                    for k in 0..n as u32 {
                        let ak = self.comp(out, a, k);
                        let ek = if n == 1 { e } else { self.val(out, f, Expr::Extract(e, k)) };
                        comps.push(self.op(out, POW, &[ak], Some(ek))?);
                    }
                    if n == 1 {
                        comps[0]
                    } else {
                        let rt = reg_ty(&mut self.m.types, ty)?;
                        self.val(out, rt, Expr::Construct(rt, comps))
                    }
                }
            }
            B::Exp2 => {
                let r = self.regs(out, args[0])?;
                self.each(out, ty, &[r], |s, out, p| {
                    let ln2 = s.constant(out, std::f32::consts::LN_2)?;
                    let m = s.op(out, MUL, &[p[0], ln2], None)?;
                    s.op(out, EXP, &[m], None)
                })?
            }
            B::Log2 => {
                let r = self.regs(out, args[0])?;
                self.each(out, ty, &[r], |s, out, p| {
                    let l = s.op(out, LOG, p, None)?;
                    let k = s.constant(out, std::f32::consts::LOG2_E)?;
                    s.op(out, MUL, &[l, k], None)
                })?
            }
            B::InverseSqrt => {
                let r = self.regs(out, args[0])?;
                self.each(out, ty, &[r], |s, out, p| {
                    let q = s.op(out, SQRT, p, None)?;
                    let one = s.constant(out, 1.0)?;
                    s.op(out, DIV, &[one, q], None)
                })?
            }
            B::Tan => {
                let r = self.regs(out, args[0])?;
                self.each(out, ty, &[r], |s, out, p| {
                    let sn = s.op(out, SIN, p, None)?;
                    let cs = s.op(out, COS, p, None)?;
                    s.op(out, DIV, &[sn, cs], None)
                })?
            }
            B::Atan2 => self.lift(out, ty, ATAN2, args)?,
            B::Atan | B::Asin | B::Acos => {
                let r = self.regs(out, args[0])?;
                self.each(out, ty, &[r], |s, out, p| {
                    let one = s.constant(out, 1.0)?;
                    if b == B::Atan {
                        return s.op(out, ATAN2, &[p[0], one], None);
                    }
                    // asin x = atan2(x, √(1 − x²)); acos x = atan2(√(1 − x²), x).
                    let xx = s.op(out, MUL, &[p[0], p[0]], None)?;
                    let d = s.op(out, SUB, &[one, xx], None)?;
                    let c = s.op(out, SQRT, &[d], None)?;
                    let (y, x) = if b == B::Asin { (p[0], c) } else { (c, p[0]) };
                    s.op(out, ATAN2, &[y, x], None)
                })?
            }
            B::Sinh | B::Cosh | B::Tanh => {
                let r = self.regs(out, args[0])?;
                self.each(out, ty, &[r], |s, out, p| {
                    if b == B::Tanh {
                        // 1 − 2 / (e^{2x} + 1): ±1 where e^{2x} overflows or vanishes.
                        let two = s.constant(out, 2.0)?;
                        let one = s.constant(out, 1.0)?;
                        let x2 = s.op(out, MUL, &[two, p[0]], None)?;
                        let e = s.op(out, EXP, &[x2], None)?;
                        let e1 = s.op(out, ADD, &[e, one], None)?;
                        let q = s.op(out, DIV, &[two, e1], None)?;
                        return s.op(out, SUB, &[one, q], None);
                    }
                    let e = s.op(out, EXP, p, None)?;
                    let n = s.op(out, NEG, p, None)?;
                    let en = s.op(out, EXP, &[n], None)?;
                    let half = s.constant(out, 0.5)?;
                    let sum = s.op(out, if b == B::Sinh { SUB } else { ADD }, &[e, en], None)?;
                    s.op(out, MUL, &[half, sum], None)
                })?
            }
            B::Length | B::Distance | B::Dot | B::Normalize | B::Cross => {
                self.geometric(out, b, args, ty)?
            }
            other => {
                return Err(cant(format!(
                    "a tape has no operation for `{}` of what depends on the point",
                    format!("{other:?}").to_lowercase()
                )));
            }
        };
        Ok(r)
    }

    /// `min`, `max`, `abs` and `clamp` of integers.
    fn int_builtin(
        &mut self,
        out: &mut Block,
        b: Builtin,
        args: &[ValueId],
        ty: TypeId,
        signed: bool,
    ) -> Result<ValueId> {
        let rs: Vec<ValueId> = args.iter().map(|&x| self.regs(out, x)).collect::<Result<_>>()?;
        let (min, max) = if signed { (IMIN, IMAX) } else { (UMIN, UMAX) };
        self.each(out, ty, &rs, |s, out, p| match b {
            Builtin::Min => s.int_op(out, min, p),
            Builtin::Max => s.int_op(out, max, p),
            Builtin::Clamp => {
                let lo = s.int_op(out, max, &p[..2])?;
                s.int_op(out, min, &[lo, p[2]])
            }
            Builtin::Abs if !signed => Ok(p[0]),
            Builtin::Abs => {
                let zero = s.int_const(out, 0)?;
                let neg = s.op(out, ILT, &[p[0], zero], None)?;
                let flipped = s.int_op(out, ISUB, &[zero, p[0]])?;
                s.select1(out, neg, flipped, p[0])
            }
            other => Err(cant(format!(
                "a tape has no operation for `{}` of integers that depend on the point",
                format!("{other:?}").to_lowercase()
            ))),
        })
    }

    /// `length`, `distance`, `dot`, `normalize` and `cross`: a `vec3`'s with the tape's vector
    /// operations, over its registers copied three in a row; other widths in scalar steps.
    fn geometric(
        &mut self,
        out: &mut Block,
        b: Builtin,
        args: &[ValueId],
        ty: TypeId,
    ) -> Result<ValueId> {
        use Builtin as B;
        let at = self.f.value_ty(args[0]);
        let n = self.width(at)?;
        let mut rs: Vec<ValueId> =
            args.iter().map(|&x| self.regs(out, x)).collect::<Result<_>>()?;
        if b == B::Distance {
            let d = self.each(out, at, &rs, |s, out, p| s.op(out, SUB, p, None))?;
            rs = vec![d];
        }
        let dot = |s: &mut Self, out: &mut Block, x: ValueId, y: ValueId| -> Result<ValueId> {
            if n == 3 {
                let (ax, ay) = (s.in_a_row(out, x)?, s.in_a_row(out, y)?);
                return s.op(out, DOT3, &[ax, ay], None);
            }
            let mut sum: Option<ValueId> = None;
            for k in 0..n as u32 {
                let (xa, ya) = (s.comp(out, x, k), s.comp(out, y, k));
                let p = s.op(out, MUL, &[xa, ya], None)?;
                sum = Some(match sum {
                    None => p,
                    Some(acc) => s.op(out, ADD, &[acc, p], None)?,
                });
            }
            sum.ok_or_else(|| Error::internal("an empty vector"))
        };
        Ok(match b {
            B::Length | B::Distance => {
                if n == 3 {
                    let a = self.in_a_row(out, rs[0])?;
                    self.op(out, LEN3, &[a], None)?
                } else if n == 1 {
                    self.op(out, ABS, &[rs[0]], None)?
                } else {
                    let d = dot(self, out, rs[0], rs[0])?;
                    self.op(out, SQRT, &[d], None)?
                }
            }
            B::Dot => dot(self, out, rs[0], rs[1])?,
            B::Normalize => {
                let rt = reg_ty(&mut self.m.types, ty)?;
                if n == 3 {
                    let a = self.in_a_row(out, rs[0])?;
                    let mut comps = Vec::new();
                    for k in 0..3u32 {
                        let c = self.fconst(out, k as f32);
                        comps.push(self.op(out, NORM3, &[a], Some(c))?);
                    }
                    self.val(out, rt, Expr::Construct(rt, comps))
                } else {
                    let d = dot(self, out, rs[0], rs[0])?;
                    let l = self.op(out, SQRT, &[d], None)?;
                    let mut comps = Vec::new();
                    for k in 0..n as u32 {
                        let c = self.comp(out, rs[0], k);
                        comps.push(self.op(out, DIV, &[c, l], None)?);
                    }
                    if n == 1 { comps[0] } else { self.val(out, rt, Expr::Construct(rt, comps)) }
                }
            }
            B::Cross => {
                let (x, y) = (rs[0], rs[1]);
                let c = |s: &mut Self, out: &mut Block, v: ValueId, k: u32| s.comp(out, v, k);
                let mut comps = Vec::new();
                for (i, j) in [(1, 2), (2, 0), (0, 1)] {
                    let (xi, yj, xj, yi) = (
                        c(self, out, x, i),
                        c(self, out, y, j),
                        c(self, out, x, j),
                        c(self, out, y, i),
                    );
                    let p = self.op(out, MUL, &[xi, yj], None)?;
                    let q = self.op(out, MUL, &[xj, yi], None)?;
                    comps.push(self.op(out, SUB, &[p, q], None)?);
                }
                let rt = reg_ty(&mut self.m.types, ty)?;
                self.val(out, rt, Expr::Construct(rt, comps))
            }
            _ => return Err(Error::internal("not a geometric builtin")),
        })
    }
}

/// What kind of expression `e` is, for an error.
fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Variant(..) => "an enum's variant",
        Expr::Run(_) => "a run of an array",
        Expr::Addr(_) => "a reference",
        Expr::Texture(..) | Expr::TextureStore(..) => "a texture",
        Expr::Host(..) => "GPU work",
        Expr::Mem(..) => "raw memory",
        Expr::Atomic(..) | Expr::Barrier | Expr::Subgroup(..) => "an atomic or a barrier",
        _ => "an expression",
    }
}
