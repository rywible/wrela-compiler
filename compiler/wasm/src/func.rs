//! One IR function to one WASM function, plus the generated helpers and export wrappers.

use crate::{Helpers, globals, in_memory, memory, repr, sret, v128_lanes, valtype};
use std::collections::{HashMap, HashSet};
use wasm_encoder::{BlockType, Function, Instruction as I, MemArg, ValType};
use wrela_ir as ir;
use wrela_ir::layout::{array_stride, column_stride, field_offsets, layout, part_offset, round_up};

type R<T> = Result<T, String>;

fn mem(offset: u32, align: u32) -> MemArg {
    MemArg { offset: offset as u64, align, memory_index: 0 }
}

/// Load and store instructions for a scalar in memory.
fn load_op(s: ir::Scalar, offset: u32) -> I<'static> {
    match s {
        ir::Scalar::I8 => I::I32Load8S(mem(offset, 0)),
        ir::Scalar::U8 => I::I32Load8U(mem(offset, 0)),
        ir::Scalar::I16 => I::I32Load16S(mem(offset, 1)),
        ir::Scalar::U16 => I::I32Load16U(mem(offset, 1)),
        ir::Scalar::I64 | ir::Scalar::U64 => I::I64Load(mem(offset, 3)),
        ir::Scalar::F32 => I::F32Load(mem(offset, 2)),
        ir::Scalar::F64 => I::F64Load(mem(offset, 3)),
        _ => I::I32Load(mem(offset, 2)),
    }
}

fn store_op(s: ir::Scalar, offset: u32) -> I<'static> {
    match s {
        ir::Scalar::I8 | ir::Scalar::U8 => I::I32Store8(mem(offset, 0)),
        ir::Scalar::I16 | ir::Scalar::U16 => I::I32Store16(mem(offset, 1)),
        ir::Scalar::I64 | ir::Scalar::U64 => I::I64Store(mem(offset, 3)),
        ir::Scalar::F32 => I::F32Store(mem(offset, 2)),
        ir::Scalar::F64 => I::F64Store(mem(offset, 3)),
        _ => I::I32Store(mem(offset, 2)),
    }
}

/// The most bytes a copy of a value does as loads and stores of its own, and a zeroing as
/// stores; more take `memory.copy` and `memory.fill`.
const INLINE_BYTES: u32 = 128;
const INLINE_ZERO_BYTES: u32 = 256;

/// A load or store's width, in a copy done as loads and stores.
#[derive(Clone, Copy)]
enum Width {
    V128,
    I64,
    I32,
    I16,
    I8,
}

impl Width {
    /// log2 of its bytes.
    fn log2(self) -> u32 {
        match self {
            Width::V128 => 4,
            Width::I64 => 3,
            Width::I32 => 2,
            Width::I16 => 1,
            Width::I8 => 0,
        }
    }

    fn val_type(self) -> ValType {
        match self {
            Width::V128 => ValType::V128,
            Width::I64 => ValType::I64,
            _ => ValType::I32,
        }
    }

    /// The alignment to promise at offset `off` in a value aligned to `align` bytes.
    fn memarg(self, off: u32, align: u32) -> MemArg {
        let at = if off == 0 { u32::MAX } else { off.trailing_zeros() };
        mem(off, self.log2().min(align.max(1).trailing_zeros()).min(at))
    }

    fn load(self, off: u32, align: u32) -> I<'static> {
        let m = self.memarg(off, align);
        match self {
            Width::V128 => I::V128Load(m),
            Width::I64 => I::I64Load(m),
            Width::I32 => I::I32Load(m),
            Width::I16 => I::I32Load16U(m),
            Width::I8 => I::I32Load8U(m),
        }
    }

    fn store(self, off: u32, align: u32) -> I<'static> {
        let m = self.memarg(off, align);
        match self {
            Width::V128 => I::V128Store(m),
            Width::I64 => I::I64Store(m),
            Width::I32 => I::I32Store(m),
            Width::I16 => I::I32Store16(m),
            Width::I8 => I::I32Store8(m),
        }
    }
}

/// `size` bytes as loads or stores, widest first: each one's offset and width.
fn chunks(size: u32, simd: bool) -> Vec<(u32, Width)> {
    let widths: &[Width] = if simd {
        &[Width::V128, Width::I64, Width::I32, Width::I16, Width::I8]
    } else {
        &[Width::I64, Width::I32, Width::I16, Width::I8]
    };
    let mut out = Vec::new();
    let mut off = 0;
    for &w in widths {
        let n = 1 << w.log2();
        while size - off >= n {
            out.push((off, w));
            off += n;
        }
    }
    out
}

/// Zero, of WASM type `t`.
fn zero(t: ValType) -> I<'static> {
    match t {
        ValType::I64 => I::I64Const(0),
        ValType::F32 => I::F32Const(0.0.into()),
        ValType::F64 => I::F64Const(0.0.into()),
        ValType::V128 => I::V128Const(0),
        _ => I::I32Const(0),
    }
}

/// A WASM constant of scalar type `s`.
fn konst(s: ir::Scalar, v: f64) -> I<'static> {
    match s {
        ir::Scalar::F32 => I::F32Const((v as f32).into()),
        ir::Scalar::F64 => I::F64Const(v.into()),
        ir::Scalar::I64 | ir::Scalar::U64 => I::I64Const(v as i64),
        _ => I::I32Const(v as i32),
    }
}

/// A function with these locals that starts with the instructions `ins`.
fn function(locals: impl IntoIterator<Item = ValType>, ins: &[I]) -> Function {
    let mut f = Function::new_with_locals_types(locals);
    for i in ins {
        f.instruction(i);
    }
    f
}

#[derive(Clone, Copy, Debug)]
enum LocalRepr {
    /// A scalar in a WASM local.
    Scalar(u32),
    /// At this offset in the frame: an aggregate, or a scalar whose address is taken (passed
    /// by reference, or a projection's target).
    Slot(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Label {
    Plain,
    LoopExit,
    LoopTop,
    LoopContinue,
}

struct Fe<'m> {
    m: &'m ir::Module,
    f: &'m ir::Function,
    at: &'m At<'m>,
    ins: Vec<I<'static>>,
    locals: Vec<ValType>,
    nparams: u32,
    sret: Option<u32>,
    fp: u32,
    /// The stack pointer on entry, restored on every exit.
    saved_sp: u32,
    /// Each value's local, from its definition on.
    values: Vec<u32>,
    /// Locals free for reuse, by type: a value's, once its last use is emitted, and a
    /// statement's scratch locals, once it is. Engines limit a function to 50,000 locals, and a
    /// derived function can have hundreds of thousands of values (few alive at once).
    free: HashMap<ValType, Vec<u32>>,
    /// The scratch locals of each statement being emitted, innermost last.
    scratch: Vec<Vec<u32>>,
    /// The values whose locals are free after each statement (see [`releases`]).
    release: HashMap<*const ir::Stmt, Vec<ir::ValueId>>,
    /// The scalar IR locals that get a local before each statement, and those whose local is
    /// free after it (see [`live_ranges`]).
    local_starts: HashMap<*const ir::Stmt, Vec<ir::LocalId>>,
    local_ends: HashMap<*const ir::Stmt, Vec<ir::LocalId>>,
    /// How many statements mention each IR local.
    local_mentions: Vec<u32>,
    value_slots: Vec<Option<u32>>,
    ir_locals: Vec<LocalRepr>,
    frame: u32,
    labels: Vec<Label>,
    /// Where the source location changes: the index in `ins` and the location (`ir::Stmt::At`).
    marks: Vec<(usize, wrela_diag::Span)>,
    /// Where each restore of the stack pointer starts in `ins` (`epilogue`): two instructions,
    /// dropped when the function has no frame.
    restores: Vec<usize>,
    /// Each comparison of a `u32` value with a constant (`if i >= n { break }`), and each
    /// negation of one: the operator, the value, the constant, and whether it's negated.
    compares: HashMap<ir::ValueId, (ir::BinOp, ir::ValueId, u32, bool)>,
    /// What's known where code is being emitted: some `u32` values are each less than a
    /// number, by a condition that leads there. A bounds check or a shift's check it proves is
    /// left out.
    less: HashMap<ir::ValueId, u32>,
    /// The aggregate zeros (`Expr::Zero`) that are only ever stored: each store zeros its place
    /// itself, rather than copy the zero from a slot of its own (`var a = [0.0; 64]`).
    stored_zeros: HashSet<ir::ValueId>,
}

/// Whether an expression makes a new aggregate in memory (and so needs a frame slot).
fn makes_memory(
    m: &ir::Module,
    f: &ir::Function,
    v: ir::ValueId,
    e: &ir::Expr,
    simd: bool,
) -> bool {
    if !in_memory(&m.types, f.value_ty(v), simd) {
        return false;
    }
    match e {
        ir::Expr::Load(_)
        | ir::Expr::Construct(..)
        | ir::Expr::Variant(..)
        | ir::Expr::Zero(_)
        | ir::Expr::Splat(..)
        | ir::Expr::Swizzle(..)
        | ir::Expr::Unary(..)
        | ir::Expr::Binary(..)
        | ir::Expr::Builtin(..)
        | ir::Expr::Run(_) => true,
        ir::Expr::Call(callee, _) => sret(m, &m.functions[callee.index()], simd),
        _ => false,
    }
}

/// Where the module's parts are, for each function's code.
pub(crate) struct At<'m> {
    /// The index of the module's first IR function: function `f`'s is `first_fn + f`.
    pub first_fn: u32,
    pub helpers: &'m Helpers,
    /// Each constant's address (`ir::Module::data`).
    pub data: &'m [u32],
    /// Where the heap starts.
    pub heap_base: u32,
    /// Vectors are `v128`s (`crate::Options::simd`).
    pub simd: bool,
}

/// A function's code, and where each source location starts in it (byte offsets from the
/// start of its body).
pub(crate) fn emit_function(
    m: &ir::Module,
    index: usize,
    at: &At,
) -> R<(Function, Vec<(u32, wrela_diag::Span)>)> {
    let f = &m.functions[index];
    let simd = at.simd;
    let sret = sret(m, f, simd);
    let mut fe = Fe {
        m,
        f,
        at,
        ins: Vec::new(),
        locals: Vec::new(),
        nparams: u32::from(sret) + f.params.len() as u32,
        sret: sret.then_some(0),
        fp: 0,
        saved_sp: 0,
        values: vec![u32::MAX; f.values.len()],
        free: HashMap::new(),
        scratch: Vec::new(),
        release: HashMap::new(),
        local_starts: HashMap::new(),
        local_ends: HashMap::new(),
        local_mentions: Vec::new(),
        value_slots: vec![None; f.values.len()],
        ir_locals: Vec::new(),
        frame: 0,
        labels: Vec::new(),
        marks: Vec::new(),
        restores: Vec::new(),
        compares: compares(m, f),
        less: HashMap::new(),
        stored_zeros: stored_zeros(m, f, simd),
    };
    fe.fp = fe.new_local(ValType::I32);
    fe.saved_sp = fe.new_local(ValType::I32);
    // Frame slots: aggregate and address-taken locals, then each aggregate value made in
    // memory. More slots can be added while the body is emitted: the prologue, which sets the
    // frame's size, is emitted last.
    let mut addressed = vec![false; f.locals.len()];
    let mut made = Vec::new();
    ir::visit::walk(&f.body, &mut |s| {
        mark_addressed(s, &mut addressed);
        // A vector local indexed by a value is in memory (a `v128`'s lanes are numbered by
        // constants).
        s.for_each_place(&mut |p| {
            if let Some(l) = p.root_local()
                && p.has_index()
            {
                addressed[l.index()] = true;
            }
        });
        if let ir::Stmt::Let(v, e) = s
            && makes_memory(m, f, *v, e, simd)
            && !fe.stored_zeros.contains(v)
        {
            made.push(*v);
        }
    });
    for (l, &taken) in f.locals.iter().zip(&addressed) {
        if in_memory(&m.types, l.ty, simd) || taken {
            let off = fe.slot(l.ty);
            fe.ir_locals.push(LocalRepr::Slot(off));
        } else {
            // Its local comes with its live range.
            fe.ir_locals.push(LocalRepr::Scalar(u32::MAX));
        }
    }
    let scalar: Vec<bool> =
        fe.ir_locals.iter().map(|r| matches!(r, LocalRepr::Scalar(_))).collect();
    let mut ranges = Ranges { scalar: &scalar, total: vec![0; scalar.len()], ..Ranges::default() };
    ir::visit::count_local_mentions(&f.body, &mut ranges.total);
    ranges.block(&f.body, false);
    (fe.local_starts, fe.local_ends, fe.local_mentions) =
        (ranges.starts, ranges.ends, ranges.total);
    for v in made {
        fe.value_slots[v.index()] = Some(fe.slot(f.value_ty(v)));
    }
    releases(&f.body, &mut fe.release);
    fe.block(&f.body)?;
    // Falling off the end: only a function with no result can.
    fe.epilogue();
    if f.ret.is_some() && !sret {
        fe.ins.push(I::Unreachable);
    }
    fe.ins.push(I::End);
    // Prologue, now that the frame's size is final: saved = sp; trap if the room left below sp
    // (sp - limit, never negative) is less than the frame; sp = fp = sp - frame. (Comparing
    // sp - frame with the limit instead would wrap around when the frame is larger than sp.)
    let frame = round_up(16, fe.frame);
    let mut prologue = Vec::new();
    if f.counted {
        // Past the limit, a panic with the module's message; then one more call in progress.
        let msg = m.depth_message.ok_or("internal: a counted function with no message")?;
        prologue.extend([
            I::GlobalGet(globals::DEPTH),
            I::I32Const(ir::RECURSION_LIMIT as i32),
            I::I32GeU,
            I::If(BlockType::Empty),
        ]);
        prologue.extend(ops::panic_with(at.data, msg)?);
        prologue.extend([
            I::End,
            I::GlobalGet(globals::DEPTH),
            I::I32Const(1),
            I::I32Add,
            I::GlobalSet(globals::DEPTH),
        ]);
    }
    if frame == 0 {
        // No frame: nothing to check, and the stack pointer stays as it is. (Its callees check
        // their own frames.)
        drop_restores(&mut fe);
    } else {
        prologue.extend([
            I::GlobalGet(globals::SP),
            I::LocalTee(fe.saved_sp),
            I::GlobalGet(globals::STACK_FLOOR),
            I::I32Sub,
            I::I32Const(frame as i32),
            I::I32LtU,
            I::If(BlockType::Empty),
        ]);
        match m.stack_message {
            Some(msg) => prologue.extend(ops::panic_with(at.data, msg)?),
            None => prologue.push(I::Unreachable),
        }
        prologue.extend([
            I::End,
            I::LocalGet(fe.saved_sp),
            I::I32Const(frame as i32),
            I::I32Sub,
            I::LocalTee(fe.fp),
            I::GlobalSet(globals::SP),
        ]);
    }
    let mut func = function(fe.locals.iter().copied(), &prologue);
    // Each location starts at the offset its first instruction is written at.
    let mut lines = Vec::new();
    let mut marks = fe.marks.iter().peekable();
    for (k, i) in fe.ins.iter().enumerate() {
        while let Some(&(_, span)) = marks.next_if(|(at, _)| *at == k) {
            lines.push((func.byte_len() as u32, span));
        }
        func.instruction(i);
    }
    Ok((func, lines))
}

/// `Fe::stored_zeros`: the zeros of types in memory whose every use is a store of them.
fn stored_zeros(m: &ir::Module, f: &ir::Function, simd: bool) -> HashSet<ir::ValueId> {
    let mut zeros = HashSet::new();
    let mut other = HashSet::new();
    ir::visit::walk(&f.body, &mut |s| {
        match s {
            // (A value too large for any stack is made, to trap: `ir::opt::KEEP_BYTES`.)
            ir::Stmt::Let(v, ir::Expr::Zero(t))
                if in_memory(&m.types, *t, simd)
                    && layout(&m.types, *t).size < ir::opt::KEEP_BYTES =>
            {
                zeros.insert(*v);
            }
            // The stored value is the only use that's allowed; a place's index isn't.
            ir::Stmt::Store(p, _) => p.for_each_value(&mut |x| {
                other.insert(x);
            }),
            s => s.for_each_value(&mut |x| {
                other.insert(x);
            }),
        }
    });
    zeros.retain(|v| !other.contains(v));
    zeros
}

/// Whether a block always leaves where it ends: it ends in a `break`, `continue`, `return` or
/// trap.
fn leaves(b: &ir::Block) -> bool {
    matches!(
        b.iter().rev().find(|s| !matches!(s, ir::Stmt::At(_))),
        Some(ir::Stmt::Break | ir::Stmt::Continue | ir::Stmt::Return(_) | ir::Stmt::Trap)
    )
}

/// `Fe::compares`: each `u32` value compared with a constant, and each negation of such a
/// comparison.
fn compares(
    m: &ir::Module,
    f: &ir::Function,
) -> HashMap<ir::ValueId, (ir::BinOp, ir::ValueId, u32, bool)> {
    let mut consts = HashMap::new();
    let mut out = HashMap::new();
    let u32_ty = m.types.lookup(&ir::TypeDef::Scalar(ir::Scalar::U32));
    ir::visit::walk(&f.body, &mut |s| match s {
        ir::Stmt::Let(v, ir::Expr::Const(ir::Const::U32(n))) => {
            consts.insert(*v, *n);
        }
        ir::Stmt::Let(c, ir::Expr::Binary(op, a, b))
            if matches!(op, ir::BinOp::Lt | ir::BinOp::Le | ir::BinOp::Gt | ir::BinOp::Ge)
                && Some(f.value_ty(*a)) == u32_ty =>
        {
            if let Some(&n) = consts.get(b) {
                out.insert(*c, (*op, *a, n, false));
            }
        }
        ir::Stmt::Let(c, ir::Expr::Unary(ir::UnOp::Not, x)) => {
            if let Some(&(op, a, n, negated)) = out.get(x) {
                out.insert(*c, (op, a, n, !negated));
            }
        }
        _ => {}
    });
    out
}

/// Removes the restores of the stack pointer from a function that has no frame, keeping each
/// source location's mark at its instruction.
fn drop_restores(fe: &mut Fe) {
    let mut gone = vec![false; fe.ins.len()];
    for &at in &fe.restores {
        gone[at] = true;
        gone[at + 1] = true;
    }
    // Instruction k's new index: the instructions before it that stay.
    let mut index = Vec::with_capacity(fe.ins.len() + 1);
    let mut n = 0;
    for &g in &gone {
        index.push(n);
        n += usize::from(!g);
    }
    index.push(n);
    for mark in &mut fe.marks {
        mark.0 = index[mark.0];
    }
    let mut k = 0;
    fe.ins.retain(|_| {
        k += 1;
        !gone[k - 1]
    });
}

/// For each statement, the values whose locals are free once it's emitted: those its block
/// defines and last uses in it (or in the blocks nested in it). A value is only visible in the
/// block that defines it (`ir::verify` checks), so that's where it dies; a use in a nested loop
/// counts as a use by the loop's whole statement, so a value read on every iteration lives
/// until the loop ends.
fn releases(b: &ir::Block, out: &mut HashMap<*const ir::Stmt, Vec<ir::ValueId>>) {
    fn uses(s: &ir::Stmt, f: &mut impl FnMut(ir::ValueId)) {
        s.for_each_value(f);
        for inner in s.blocks() {
            for t in inner {
                uses(t, f);
            }
        }
    }
    let mut last = HashMap::new();
    for (i, s) in b.iter().enumerate() {
        uses(s, &mut |v| {
            if let Some(l) = last.get_mut(&v) {
                *l = i;
            }
        });
        if let ir::Stmt::Let(v, _) = s {
            last.insert(*v, i);
        }
        for inner in s.blocks() {
            releases(inner, out);
        }
    }
    // In value order, so the build is the same every time.
    let mut last: Vec<(ir::ValueId, usize)> = last.into_iter().collect();
    last.sort_unstable_by_key(|&(v, _)| v.0);
    for (v, i) in last {
        out.entry(std::ptr::from_ref(&b[i])).or_default().push(v);
    }
}

/// The locals a statement mentions itself (not in its nested blocks), once per mention.
fn mentions(s: &ir::Stmt, f: &mut impl FnMut(ir::LocalId)) {
    s.for_each_place(&mut |p| {
        if let Some(l) = p.root_local() {
            f(l);
        }
    });
}

/// Where each scalar IR local lives: from the first to the last statement that mentions it, of
/// the innermost block that holds all its mentions. In a loop, a local keeps its value from
/// one iteration to the next, so a block in a loop gives way to the outermost loop's block.
/// Engines limit a function to 50,000 locals, and each `let` is an IR local.
#[derive(Default)]
struct Ranges<'a> {
    scalar: &'a [bool],
    /// How many statements mention each local.
    total: Vec<u32>,
    starts: HashMap<*const ir::Stmt, Vec<ir::LocalId>>,
    ends: HashMap<*const ir::Stmt, Vec<ir::LocalId>>,
}

impl Ranges<'_> {
    /// Places the ranges that fall in `b`, and returns the mentions of the other locals in it.
    fn block(&mut self, b: &ir::Block, in_loop: bool) -> HashMap<ir::LocalId, u32> {
        // Each local mentioned: how often, and the first and last statements that do.
        let mut seen: HashMap<ir::LocalId, (u32, usize, usize)> = HashMap::new();
        for (i, s) in b.iter().enumerate() {
            let mut add = |l: ir::LocalId, n: u32| {
                let e = seen.entry(l).or_insert((0, i, i));
                e.0 += n;
                e.2 = i;
            };
            mentions(s, &mut |l| add(l, 1));
            let looped = in_loop || matches!(s, ir::Stmt::Loop { .. });
            for inner in s.blocks() {
                for (l, n) in self.block(inner, looped) {
                    add(l, n);
                }
            }
        }
        if in_loop {
            return seen.into_iter().map(|(l, (n, ..))| (l, n)).collect();
        }
        let mut open = HashMap::new();
        // In local order, so the build is the same every time.
        let mut seen: Vec<_> = seen.into_iter().collect();
        seen.sort_unstable_by_key(|(l, _)| l.0);
        for (l, (n, first, last)) in seen {
            if n < self.total[l.index()] {
                open.insert(l, n);
            } else if self.scalar[l.index()] {
                self.starts.entry(std::ptr::from_ref(&b[first])).or_default().push(l);
                self.ends.entry(std::ptr::from_ref(&b[last])).or_default().push(l);
            }
        }
        open
    }
}

/// Marks the locals whose address a statement takes (not in its nested blocks): passed by
/// reference, or `Addr` or `Run` of a place rooted at them.
fn mark_addressed(s: &ir::Stmt, out: &mut [bool]) {
    let mut mark = |p: &ir::Place| {
        if let Some(l) = p.root_local() {
            out[l.index()] = true;
        }
    };
    match s.expr() {
        Some(ir::Expr::Addr(p) | ir::Expr::Run(p)) => mark(p),
        Some(ir::Expr::Call(_, args)) => {
            for a in args {
                if let ir::Arg::Place(p) = a {
                    mark(p);
                }
            }
        }
        _ => {}
    }
}

impl<'m> Fe<'m> {
    /// A local of type `t`: while a statement is emitted, a scratch local for it (perhaps one
    /// used before, so it's set before it's read), and before that, one for the whole function.
    fn new_local(&mut self, t: ValType) -> u32 {
        if self.scratch.is_empty() {
            return self.fresh_local(t);
        }
        let l = self.reuse_local(t);
        if let Some(frame) = self.scratch.last_mut() {
            frame.push(l);
        }
        l
    }

    fn fresh_local(&mut self, t: ValType) -> u32 {
        self.locals.push(t);
        self.nparams + self.locals.len() as u32 - 1
    }

    /// A free local of type `t`, else a new one.
    fn reuse_local(&mut self, t: ValType) -> u32 {
        match self.free.get_mut(&t).and_then(Vec::pop) {
            Some(l) => l,
            None => self.fresh_local(t),
        }
    }

    fn free_local(&mut self, l: u32) {
        let t = self.locals[(l - self.nparams) as usize];
        self.free.entry(t).or_default().push(l);
    }

    fn slot(&mut self, t: ir::TypeId) -> u32 {
        let l = layout(&self.m.types, t);
        let align = l.align.max(4);
        let off = round_up(align, self.frame);
        self.frame = off.saturating_add(round_up(16, l.size));
        off
    }

    fn ty(&self, t: ir::TypeId) -> &'m ir::TypeDef {
        self.m.types.get(t)
    }

    /// A vector type's components, when its values are `v128`s.
    fn vec_n(&self, t: ir::TypeId) -> Option<u8> {
        v128_lanes(&self.m.types, t, self.at.simd)
    }

    /// Pushes, as a `v128`, the vector of `n` components at the address on top of the stack
    /// plus `off`: only its own bytes, so a `vec3` at the end of memory isn't read past.
    fn vec_load(&mut self, n: u8, off: u32) {
        match n {
            2 => self.ins.push(I::V128Load64Zero(mem(off, 2))),
            3 => {
                let a = self.new_local(ValType::I32);
                self.ins.extend([
                    I::LocalTee(a),
                    I::LocalGet(a),
                    I::V128Load64Zero(mem(off, 2)),
                    I::V128Load32Lane { memarg: mem(off + 8, 2), lane: 2 },
                ]);
            }
            _ => self.ins.push(I::V128Load(mem(off, 2))),
        }
    }

    /// Stores the `v128` on top of the stack as a vector of `n` components at the address
    /// below it plus `off`: only its own bytes.
    fn vec_store(&mut self, n: u8, off: u32) {
        match n {
            2 => self.ins.push(I::V128Store64Lane { memarg: mem(off, 2), lane: 0 }),
            3 => {
                let x = self.new_local(ValType::V128);
                let a = self.new_local(ValType::I32);
                self.ins.extend([
                    I::LocalSet(x),
                    I::LocalTee(a),
                    I::LocalGet(x),
                    I::V128Store64Lane { memarg: mem(off, 2), lane: 0 },
                    I::LocalGet(a),
                    I::LocalGet(x),
                    I::V128Store32Lane { memarg: mem(off + 8, 2), lane: 2 },
                ]);
            }
            _ => self.ins.push(I::V128Store(mem(off, 2))),
        }
    }

    /// The scalar a value of type `t` is in memory: a pointer is a `u32` (a derived
    /// projection's result holds pointers in a struct).
    fn scalar(&self, t: ir::TypeId) -> R<ir::Scalar> {
        if let ir::TypeDef::Ptr(_) = self.ty(t) {
            return Ok(ir::Scalar::U32);
        }
        self.m
            .types
            .as_scalar(t)
            .ok_or_else(|| format!("expected a scalar, found {}", self.m.types.display(t)))
    }

    fn epilogue(&mut self) {
        self.restores.push(self.ins.len());
        self.ins.extend([I::LocalGet(self.saved_sp), I::GlobalSet(globals::SP)]);
        if self.f.counted {
            self.ins.extend([
                I::GlobalGet(globals::DEPTH),
                I::I32Const(1),
                I::I32Sub,
                I::GlobalSet(globals::DEPTH),
            ]);
        }
    }

    fn trap_if(&mut self) {
        self.ins.extend([I::If(BlockType::Empty), I::Unreachable, I::End]);
    }

    fn v(&self, v: ir::ValueId) -> u32 {
        let l = self.values[v.index()];
        debug_assert!(l != u32::MAX, "v{} used before its definition", v.0);
        l
    }

    fn vty(&self, v: ir::ValueId) -> ir::TypeId {
        self.f.value_ty(v)
    }

    /// The WASM local of IR parameter `i`: after the result's pointer, if there is one.
    fn param(&self, i: u32) -> u32 {
        u32::from(self.sret.is_some()) + i
    }

    /// Pushes the address at offset `off` in the frame.
    fn frame_addr(&mut self, off: u32) {
        self.ins.extend([I::LocalGet(self.fp), I::I32Const(off as i32), I::I32Add]);
    }

    /// Adds `off` to the address on top of the stack.
    fn add_offset(&mut self, off: u32) {
        if off != 0 {
            self.ins.extend([I::I32Const(off as i32), I::I32Add]);
        }
    }

    /// Copies a value of aggregate type `t` from the address on top of the stack to the
    /// address below it.
    fn copy(&mut self, t: ir::TypeId) {
        let l = layout(&self.m.types, t);
        if l.size > INLINE_BYTES {
            self.ins.extend([I::I32Const(l.size as i32), I::MemoryCopy { src_mem: 0, dst_mem: 0 }]);
            return;
        }
        // Small copies as loads and stores: engines can make `memory.copy` a call out of the
        // code, which costs more than the copy. Every load is before every store, so the two
        // regions can overlap, as they can for `memory.copy`.
        let (dst, src) = (self.new_local(ValType::I32), self.new_local(ValType::I32));
        self.ins.extend([I::LocalSet(src), I::LocalSet(dst)]);
        let chunks = chunks(l.size, self.at.simd);
        let mut held = Vec::with_capacity(chunks.len());
        for &(off, w) in &chunks {
            let t = self.new_local(w.val_type());
            self.ins.extend([I::LocalGet(src), w.load(off, l.align), I::LocalSet(t)]);
            held.push(t);
        }
        for (&(off, w), t) in chunks.iter().zip(held) {
            self.ins.extend([I::LocalGet(dst), I::LocalGet(t), w.store(off, l.align)]);
        }
    }

    /// Zeros the `size` bytes at the address in local `a`.
    fn zero(&mut self, a: u32, size: u32) {
        if size > INLINE_ZERO_BYTES {
            self.ins.extend([
                I::LocalGet(a),
                I::I32Const(0),
                I::I32Const(size as i32),
                I::MemoryFill(0),
            ]);
            return;
        }
        for (off, w) in chunks(size, self.at.simd) {
            self.ins.extend([I::LocalGet(a), zero(w.val_type()), w.store(off, 0)]);
        }
    }

    /// The address of a value's own frame slot.
    fn slot_addr(&mut self, v: ir::ValueId) -> R<()> {
        let off = self.value_slots[v.index()].ok_or("internal: a value without a slot")?;
        self.frame_addr(off);
        Ok(())
    }

    /// Sets value `v` to its slot's address (for aggregate results).
    fn set_to_slot(&mut self, v: ir::ValueId) -> R<()> {
        self.slot_addr(v)?;
        self.ins.push(I::LocalSet(self.v(v)));
        Ok(())
    }

    // ---- places ----------------------------------------------------------------------------

    /// Pushes the address of a place's root, which must be in memory (not a scalar local), and
    /// returns its type.
    fn root(&mut self, p: &ir::Place) -> R<ir::TypeId> {
        match &p.root {
            ir::PlaceRoot::Local(l) => match self.ir_locals[l.index()] {
                LocalRepr::Scalar(_) => Err("internal: the address of a scalar local".into()),
                LocalRepr::Slot(off) => {
                    self.frame_addr(off);
                    Ok(self.f.locals[l.index()].ty)
                }
            },
            ir::PlaceRoot::Param(i) => {
                self.ins.push(I::LocalGet(self.param(*i)));
                Ok(self.f.params[*i as usize].ty)
            }
            ir::PlaceRoot::Ptr(v) => {
                let t = match self.ty(self.vty(*v)) {
                    ir::TypeDef::Ptr(t) => *t,
                    _ => return Err("internal: a pointer place of a non-pointer value".into()),
                };
                self.ins.push(I::LocalGet(self.v(*v)));
                Ok(t)
            }
            ir::PlaceRoot::Data(d) => {
                let at = self.at.data.get(d.index()).ok_or("internal: missing constant data")?;
                self.ins.push(I::I32Const(*at as i32));
                Ok(self.m.data[d.index()].ty)
            }
            ir::PlaceRoot::Resource(_) => Err("internal: a GPU resource in CPU code".into()),
        }
    }

    /// Pushes a place's address (it must be in memory) and returns its type.
    fn addr(&mut self, p: &ir::Place) -> R<ir::TypeId> {
        let mut t = self.root(p)?;
        for proj in &p.path {
            t = self.project(t, proj)?;
        }
        Ok(t)
    }

    /// Given an address on the stack of a value of type `t`, applies one projection.
    fn project(&mut self, t: ir::TypeId, proj: &ir::Proj) -> R<ir::TypeId> {
        match (self.ty(t), proj) {
            (ir::TypeDef::Struct { .. } | ir::TypeDef::Enum { .. }, ir::Proj::Field(k)) => {
                self.add_offset(self.part_offset(t, *k)?);
            }
            (ir::TypeDef::Vector(..), ir::Proj::Comp(c)) => {
                self.add_offset(self.part_offset(t, u32::from(*c))?)
            }
            (&ir::TypeDef::Vector(s, n), ir::Proj::Index(i)) => {
                self.bounds_check(*i, n as u32);
                let size = s.bytes() as i32;
                self.ins.extend([I::LocalGet(self.v(*i)), I::I32Const(size), I::I32Mul, I::I32Add]);
            }
            (ir::TypeDef::Matrix(c, r), ir::Proj::Index(i)) => {
                self.bounds_check(*i, *c as u32);
                self.ins.extend([
                    I::LocalGet(self.v(*i)),
                    I::I32Const(column_stride(*r) as i32),
                    I::I32Mul,
                    I::I32Add,
                ]);
            }
            (ir::TypeDef::Array(e, n), ir::Proj::Index(i)) => {
                self.bounds_check(*i, *n);
                self.ins.extend([
                    I::LocalGet(self.v(*i)),
                    I::I32Const(array_stride(&self.m.types, *e) as i32),
                    I::I32Mul,
                    I::I32Add,
                ]);
            }
            (ir::TypeDef::Run(e), ir::Proj::Index(i)) => {
                // The run is {ptr, len}: check the index against len, then ptr + i * stride.
                let stride = array_stride(&self.m.types, *e);
                let base = self.new_local(ValType::I32);
                self.ins.push(I::LocalSet(base));
                self.ins.extend([
                    I::LocalGet(self.v(*i)),
                    I::LocalGet(base),
                    I::I32Load(mem(4, 2)),
                    I::I32GeU,
                ]);
                self.trap_if();
                self.ins.extend([
                    I::LocalGet(base),
                    I::I32Load(mem(0, 2)),
                    I::LocalGet(self.v(*i)),
                    I::I32Const(stride as i32),
                    I::I32Mul,
                    I::I32Add,
                ]);
            }
            (d, p) => return Err(format!("internal: projection {p:?} of {d:?}")),
        }
        self.m.proj_ty(t, proj).ok_or_else(|| format!("internal: projection {proj:?} has no type"))
    }

    fn bounds_check(&mut self, i: ir::ValueId, n: u32) {
        if self.known_below(i, n) {
            return;
        }
        self.ins.extend([I::LocalGet(self.v(i)), I::I32Const(n as i32), I::I32GeU]);
        self.trap_if();
    }

    /// The byte offset of constant part `k` of a value of type `t` (`ir::layout::part_offset`).
    fn part_offset(&self, t: ir::TypeId, k: u32) -> R<u32> {
        part_offset(&self.m.types, t, k)
            .ok_or_else(|| format!("internal: part {k} of {:?}", self.ty(t)))
    }

    fn load_place(&mut self, p: &ir::Place, dst: ir::ValueId) -> R<()> {
        if let ir::PlaceRoot::Local(l) = &p.root
            && let LocalRepr::Scalar(w) = self.ir_locals[l.index()]
        {
            self.ins.push(I::LocalGet(w));
            match p.path.as_slice() {
                [] => {}
                // A component of a vector in a `v128`.
                [ir::Proj::Comp(c)] => self.ins.push(I::F32x4ExtractLane(*c)),
                _ => return Err(format!("internal: a load of {p:?} from a local in a register")),
            }
            self.ins.push(I::LocalSet(self.v(dst)));
            return Ok(());
        }
        let t = self.vty(dst);
        if let Some(n) = self.vec_n(t) {
            self.addr(p)?;
            self.vec_load(n, 0);
            self.ins.push(I::LocalSet(self.v(dst)));
            return Ok(());
        }
        if self.m.types.is_aggregate(t) {
            // Copy into the value's own slot.
            self.slot_addr(dst)?;
            self.addr(p)?;
            self.copy(t);
            self.set_to_slot(dst)
        } else {
            self.addr(p)?;
            let s = self.scalar(t)?;
            self.ins.extend([load_op(s, 0), I::LocalSet(self.v(dst))]);
            Ok(())
        }
    }

    fn store_place(&mut self, p: &ir::Place, v: ir::ValueId) -> R<()> {
        if let ir::PlaceRoot::Local(l) = &p.root
            && let LocalRepr::Scalar(w) = self.ir_locals[l.index()]
        {
            match p.path.as_slice() {
                [] => self.ins.extend([I::LocalGet(self.v(v)), I::LocalSet(w)]),
                // A component of a vector in a `v128`.
                [ir::Proj::Comp(c)] => self.ins.extend([
                    I::LocalGet(w),
                    I::LocalGet(self.v(v)),
                    I::F32x4ReplaceLane(*c),
                    I::LocalSet(w),
                ]),
                _ => return Err(format!("internal: a store to {p:?} in a local in a register")),
            }
            return Ok(());
        }
        let t = self.vty(v);
        self.addr(p)?;
        if self.stored_zeros.contains(&v) {
            let a = self.new_local(ValType::I32);
            self.ins.push(I::LocalSet(a));
            self.zero(a, layout(&self.m.types, t).size);
            return Ok(());
        }
        if let Some(n) = self.vec_n(t) {
            self.ins.push(I::LocalGet(self.v(v)));
            self.vec_store(n, 0);
            return Ok(());
        }
        if self.m.types.is_aggregate(t) {
            self.ins.push(I::LocalGet(self.v(v)));
            self.copy(t);
        } else {
            let s = self.scalar(t)?;
            self.ins.extend([I::LocalGet(self.v(v)), store_op(s, 0)]);
        }
        Ok(())
    }

    /// Stores scalar or aggregate value `v` at `[addr local] + offset`.
    fn store_at(&mut self, addr: u32, offset: u32, v: ir::ValueId) -> R<()> {
        let t = self.vty(v);
        if let Some(n) = self.vec_n(t) {
            self.ins.extend([I::LocalGet(addr), I::LocalGet(self.v(v))]);
            self.vec_store(n, offset);
            return Ok(());
        }
        if self.m.types.is_aggregate(t) {
            self.ins.extend([
                I::LocalGet(addr),
                I::I32Const(offset as i32),
                I::I32Add,
                I::LocalGet(self.v(v)),
            ]);
            self.copy(t);
        } else {
            let s = self.scalar(t)?;
            self.ins.extend([I::LocalGet(addr), I::LocalGet(self.v(v)), store_op(s, offset)]);
        }
        Ok(())
    }

    /// The address of value `v`'s slot, in a fresh local. `zero` fills the slot with zeros: for
    /// a value whose code doesn't write every byte of it (a struct's padding, say). A vector's
    /// code writes each component, which is every byte.
    fn fresh_slot(&mut self, v: ir::ValueId, zero: bool) -> R<u32> {
        let a = self.new_local(ValType::I32);
        self.slot_addr(v)?;
        self.ins.extend([I::LocalTee(a), I::LocalSet(self.v(v))]);
        if zero {
            let size = layout(&self.m.types, self.vty(v)).size;
            self.zero(a, size);
        }
        Ok(a)
    }

    // ---- statements ------------------------------------------------------------------------

    fn block(&mut self, b: &ir::Block) -> R<()> {
        let mut learned = Vec::new();
        for s in b {
            self.stmt(s)?;
            // Past an `if` one of whose branches always leaves and the other is empty, the
            // condition is what takes the empty one.
            if let ir::Stmt::If { cond, then, else_ } = s {
                let gone = match (leaves(then), leaves(else_)) {
                    (true, false) if else_.is_empty() => Some(false),
                    (false, true) if then.is_empty() => Some(true),
                    _ => None,
                };
                if let Some(holds) = gone {
                    learned.extend(self.learn(*cond, holds));
                }
            }
        }
        self.forget(learned);
        Ok(())
    }

    /// Adds what `cond` being `holds` says (a value less than a number) to what's known, and
    /// returns what it replaced, for `forget`.
    fn learn(&mut self, cond: ir::ValueId, holds: bool) -> Option<(ir::ValueId, Option<u32>)> {
        let &(op, v, n, negated) = self.compares.get(&cond)?;
        let below = match (op, holds != negated) {
            (ir::BinOp::Lt, true) | (ir::BinOp::Ge, false) => n,
            (ir::BinOp::Le, true) | (ir::BinOp::Gt, false) => n.checked_add(1)?,
            _ => return None,
        };
        let old = self.less.get(&v).copied();
        if old.is_none_or(|o| below < o) {
            self.less.insert(v, below);
        }
        Some((v, old))
    }

    fn forget(&mut self, learned: Vec<(ir::ValueId, Option<u32>)>) {
        for (v, old) in learned.into_iter().rev() {
            match old {
                Some(o) => self.less.insert(v, o),
                None => self.less.remove(&v),
            };
        }
    }

    /// Whether `v` is known to be less than `n`.
    fn known_below(&self, v: ir::ValueId, n: u32) -> bool {
        self.less.get(&v).is_some_and(|&b| b <= n)
    }

    fn depth_of(&self, want: Label) -> R<u32> {
        let pos =
            self.labels.iter().rposition(|l| *l == want).ok_or("internal: break outside a loop")?;
        Ok((self.labels.len() - 1 - pos) as u32)
    }

    /// Emits a statement: a `let`'s value gets a local first; afterwards, the statement's
    /// scratch locals, and those of the values it last uses, are free.
    fn stmt(&mut self, s: &ir::Stmt) -> R<()> {
        for l in self.local_starts.remove(&std::ptr::from_ref(s)).unwrap_or_default() {
            // A local used before holds an old value; the IR local starts at zero.
            let t = repr(&self.m.types, self.f.locals[l.index()].ty, self.at.simd);
            let used = self.free.get(&t).is_some_and(|free| !free.is_empty());
            let w = self.reuse_local(t);
            if used {
                self.ins.extend([zero(t), I::LocalSet(w)]);
            }
            self.ir_locals[l.index()] = LocalRepr::Scalar(w);
        }
        if let ir::Stmt::Let(v, _) = s {
            let t = repr(&self.m.types, self.vty(*v), self.at.simd);
            self.values[v.index()] = self.reuse_local(t);
        }
        self.scratch.push(Vec::new());
        let r = self.stmt_code(s);
        for l in self.scratch.pop().unwrap_or_default() {
            self.free_local(l);
        }
        for v in self.release.remove(&std::ptr::from_ref(s)).unwrap_or_default() {
            self.free_local(self.values[v.index()]);
        }
        for l in self.local_ends.remove(&std::ptr::from_ref(s)).unwrap_or_default() {
            if let LocalRepr::Scalar(w) = self.ir_locals[l.index()] {
                self.free_local(w);
            }
        }
        r
    }

    fn stmt_code(&mut self, s: &ir::Stmt) -> R<()> {
        match s {
            ir::Stmt::Let(v, e) => self.expr(*v, e),
            ir::Stmt::Eval(e) => self.eval(e),
            ir::Stmt::Store(p, v) => self.store_place(p, *v),
            ir::Stmt::If { cond, then, else_ } => {
                self.ins.extend([I::LocalGet(self.v(*cond)), I::If(BlockType::Empty)]);
                self.labels.push(Label::Plain);
                let learned = self.learn(*cond, true);
                self.block(then)?;
                self.forget(learned.into_iter().collect());
                if !else_.is_empty() {
                    self.ins.push(I::Else);
                    let learned = self.learn(*cond, false);
                    self.block(else_)?;
                    self.forget(learned.into_iter().collect());
                }
                self.labels.pop();
                self.ins.push(I::End);
                Ok(())
            }
            ir::Stmt::Loop { body, continuing } => {
                // Four iterations at a time first, where they can be (`vloop`); not in a debug
                // build, whose NaN checks panic at an iteration.
                if self.at.simd
                    && self.m.nan_message.is_none()
                    && let Some(plan) = vloop::plan(self, body, continuing)
                {
                    vloop::emit(self, &plan)?;
                }
                self.ins.push(I::Block(BlockType::Empty));
                self.labels.push(Label::LoopExit);
                self.ins.push(I::Loop(BlockType::Empty));
                self.labels.push(Label::LoopTop);
                self.ins.push(I::Block(BlockType::Empty));
                self.labels.push(Label::LoopContinue);
                self.block(body)?;
                self.labels.pop();
                self.ins.push(I::End);
                self.block(continuing)?;
                self.ins.push(I::Br(0));
                self.labels.pop();
                self.ins.push(I::End);
                self.labels.pop();
                self.ins.push(I::End);
                Ok(())
            }
            ir::Stmt::Break => {
                let d = self.depth_of(Label::LoopExit)?;
                self.ins.push(I::Br(d));
                Ok(())
            }
            ir::Stmt::Continue => {
                let d = self.depth_of(Label::LoopContinue)?;
                self.ins.push(I::Br(d));
                Ok(())
            }
            ir::Stmt::Return(v) => {
                match (v, self.sret) {
                    (Some(v), Some(sret)) => {
                        self.ins.extend([I::LocalGet(sret), I::LocalGet(self.v(*v))]);
                        self.copy(self.vty(*v));
                        self.epilogue();
                    }
                    (Some(v), None) => {
                        self.epilogue();
                        self.ins.push(I::LocalGet(self.v(*v)));
                    }
                    (None, _) => self.epilogue(),
                }
                self.ins.push(I::Return);
                Ok(())
            }
            ir::Stmt::Trap => {
                self.ins.push(I::Unreachable);
                Ok(())
            }
            ir::Stmt::At(span) => {
                self.marks.push((self.ins.len(), *span));
                Ok(())
            }
        }
    }

    /// An expression evaluated for its effect.
    fn eval(&mut self, e: &ir::Expr) -> R<()> {
        match e {
            ir::Expr::Call(f, args) => {
                let callee = &self.m.functions[f.index()];
                // A callee that returns a value through a pointer still needs somewhere to put it.
                let sret = sret(self.m, callee, self.at.simd);
                if let Some(t) = callee.ret
                    && sret
                {
                    let off = self.slot(t);
                    self.frame_addr(off);
                }
                self.call_args(args)?;
                self.ins.push(I::Call(self.at.first_fn + f.0));
                if callee.ret.is_some() && !sret {
                    self.ins.push(I::Drop);
                }
                Ok(())
            }
            ir::Expr::Host(op, args) => ops::host(self, op, args, None),
            ir::Expr::Mem(op, args) => self.mem_op(*op, args, None),
            _ => Ok(()),
        }
    }

    /// A raw memory operation, its result (if any) in `dst`.
    fn mem_op(&mut self, op: ir::MemOp, args: &[ir::ValueId], dst: Option<ir::ValueId>) -> R<()> {
        let arg = |fe: &Self, i: usize| -> R<I<'static>> {
            let v = args.get(i).ok_or("internal: a memory operation without its arguments")?;
            Ok(I::LocalGet(fe.v(*v)))
        };
        match op {
            ir::MemOp::HeapBase => self.ins.push(I::I32Const(self.at.heap_base as i32)),
            ir::MemOp::Pages => self.ins.push(I::MemorySize(0)),
            ir::MemOp::Grow => self.ins.extend([arg(self, 0)?, I::MemoryGrow(0)]),
            ir::MemOp::Load(s) => self.ins.extend([arg(self, 0)?, load_op(s, 0)]),
            ir::MemOp::Store(s) => self.ins.extend([arg(self, 0)?, arg(self, 1)?, store_op(s, 0)]),
            ir::MemOp::Copy => self.ins.extend([
                arg(self, 0)?,
                arg(self, 1)?,
                arg(self, 2)?,
                I::MemoryCopy { src_mem: 0, dst_mem: 0 },
            ]),
            ir::MemOp::Fill => {
                self.ins.extend([arg(self, 0)?, arg(self, 1)?, arg(self, 2)?, I::MemoryFill(0)])
            }
            ir::MemOp::Ptr | ir::MemOp::Addr => self.ins.push(arg(self, 0)?),
            // The memory is shared: workers run the same code at once.
            ir::MemOp::Cas => self.ins.extend([
                arg(self, 0)?,
                arg(self, 1)?,
                arg(self, 2)?,
                I::I32AtomicRmwCmpxchg(mem(0, 2)),
            ]),
            ir::MemOp::AtomicAdd => {
                self.ins.extend([arg(self, 0)?, arg(self, 1)?, I::I32AtomicRmwAdd(mem(0, 2))])
            }
            ir::MemOp::AtomicLoad => self.ins.extend([arg(self, 0)?, I::I32AtomicLoad(mem(0, 2))]),
            ir::MemOp::AtomicStore => {
                self.ins.extend([arg(self, 0)?, arg(self, 1)?, I::I32AtomicStore(mem(0, 2))])
            }
            ir::MemOp::Wait => self.ins.extend([
                arg(self, 0)?,
                arg(self, 1)?,
                // No time limit.
                I::I64Const(-1),
                I::MemoryAtomicWait32(mem(0, 2)),
            ]),
            ir::MemOp::WaitFor => self.ins.extend([
                arg(self, 0)?,
                arg(self, 1)?,
                // Microseconds to nanoseconds.
                arg(self, 2)?,
                I::I64ExtendI32U,
                I::I64Const(1000),
                I::I64Mul,
                I::MemoryAtomicWait32(mem(0, 2)),
            ]),
            ir::MemOp::ThreadBlock => self.ins.push(I::GlobalGet(globals::THREAD)),
            ir::MemOp::Notify => {
                self.ins.extend([arg(self, 0)?, arg(self, 1)?, I::MemoryAtomicNotify(mem(0, 2))])
            }
            ir::MemOp::RunTask => self.ins.extend([
                arg(self, 1)?,
                arg(self, 2)?,
                arg(self, 0)?,
                I::CallIndirect { type_index: self.at.helpers.task_type, table_index: 0 },
            ]),
            ir::MemOp::Panic => {
                // The count, cut to the room there is; the bytes; then the trap.
                let n = self.new_local(ValType::I32);
                self.ins.extend([
                    arg(self, 1)?,
                    I::I32Const(memory::PANIC_CAP as i32),
                    arg(self, 1)?,
                    I::I32Const(memory::PANIC_CAP as i32),
                    I::I32LeU,
                    I::Select,
                    I::LocalSet(n),
                    I::GlobalGet(globals::THREAD),
                    I::LocalGet(n),
                    I::I32Store(mem(memory::PANIC, 2)),
                    I::GlobalGet(globals::THREAD),
                    I::I32Const(memory::PANIC as i32 + 4),
                    I::I32Add,
                    arg(self, 0)?,
                    I::LocalGet(n),
                    I::MemoryCopy { src_mem: 0, dst_mem: 0 },
                    I::Unreachable,
                ]);
                return Ok(());
            }
        }
        if let Some(v) = dst {
            self.ins.push(I::LocalSet(self.v(v)));
        } else if !matches!(
            op,
            ir::MemOp::Store(_)
                | ir::MemOp::Copy
                | ir::MemOp::Fill
                | ir::MemOp::AtomicStore
                | ir::MemOp::RunTask
        ) {
            self.ins.push(I::Drop);
        }
        Ok(())
    }

    fn call_args(&mut self, args: &[ir::Arg]) -> R<()> {
        for a in args {
            match a {
                ir::Arg::Value(v) => self.ins.push(I::LocalGet(self.v(*v))),
                ir::Arg::Place(p) => {
                    self.addr(p)?;
                }
            }
        }
        Ok(())
    }

    // ---- expressions -----------------------------------------------------------------------

    fn expr(&mut self, v: ir::ValueId, e: &ir::Expr) -> R<()> {
        let t = self.vty(v);
        match e {
            ir::Expr::Const(c) => {
                let i = match c {
                    ir::Const::Bool(b) => I::I32Const(i32::from(*b)),
                    ir::Const::I32(x) => I::I32Const(*x),
                    ir::Const::U32(x) => I::I32Const(*x as i32),
                    ir::Const::I64(x) => I::I64Const(*x),
                    ir::Const::U64(x) => I::I64Const(*x as i64),
                    ir::Const::Small(_, x) => I::I32Const(*x as i32),
                    ir::Const::F32(x) => I::F32Const((*x).into()),
                    ir::Const::F64(x) => I::F64Const((*x).into()),
                };
                self.ins.extend([i, I::LocalSet(self.v(v))]);
            }
            ir::Expr::Zero(_) => {
                if self.stored_zeros.contains(&v) {
                    // Each store zeros its place (`store_place`).
                } else if self.vec_n(t).is_some() {
                    self.ins.extend([I::V128Const(0), I::LocalSet(self.v(v))]);
                } else if self.m.types.is_aggregate(t) {
                    self.fresh_slot(v, true)?;
                } else {
                    let s = self.scalar(t)?;
                    self.ins.extend([konst(s, 0.0), I::LocalSet(self.v(v))]);
                }
            }
            ir::Expr::Param(i) => {
                self.ins.extend([I::LocalGet(self.param(*i)), I::LocalSet(self.v(v))])
            }
            ir::Expr::Load(p) => self.load_place(p, v)?,
            ir::Expr::Addr(p) => {
                self.addr(p)?;
                self.ins.push(I::LocalSet(self.v(v)));
            }
            ir::Expr::Run(p) => {
                let pt = self.place_ty(p)?;
                let n = match self.ty(pt) {
                    ir::TypeDef::Array(_, n) => *n,
                    _ => return Err("internal: a run of something that isn't an array".into()),
                };
                // Both of the run's words are written.
                let a = self.fresh_slot(v, false)?;
                self.ins.push(I::LocalGet(a));
                self.addr(p)?;
                self.ins.extend([
                    I::I32Store(mem(0, 2)),
                    I::LocalGet(a),
                    I::I32Const(n as i32),
                    I::I32Store(mem(4, 2)),
                ]);
            }
            ir::Expr::Call(f, args) => {
                let sret = sret(self.m, &self.m.functions[f.index()], self.at.simd);
                if sret {
                    self.slot_addr(v)?;
                }
                self.call_args(args)?;
                self.ins.push(I::Call(self.at.first_fn + f.0));
                if sret {
                    self.set_to_slot(v)?;
                } else {
                    self.ins.push(I::LocalSet(self.v(v)));
                }
            }
            ir::Expr::Construct(_, parts) => self.construct(v, t, parts)?,
            ir::Expr::Variant(_, k, payload) => {
                // The tag, and the payload where every variant's starts.
                let a = self.fresh_slot(v, true)?;
                let tag = self.m.types.tag(t, *k);
                self.ins.extend([I::LocalGet(a), I::I32Const(tag as i32), I::I32Store(mem(0, 2))]);
                if let Some(p) = payload {
                    let off = field_offsets(&self.m.types, t)[1 + *k as usize];
                    self.store_at(a, off, *p)?;
                }
            }
            ir::Expr::Extract(x, i) => self.extract(v, *x, *i)?,
            ir::Expr::ExtractDyn(x, i) if self.vec_n(self.vty(*x)).is_some() => {
                // A lane of a `v128`, by a value: checked, then each lane chosen in turn.
                let n = self.vec_n(self.vty(*x)).unwrap_or(4);
                self.bounds_check(*i, u32::from(n));
                self.ins.extend([I::LocalGet(self.v(*x)), I::F32x4ExtractLane(0)]);
                for lane in 1..n {
                    self.ins.extend([
                        I::LocalSet(self.v(v)),
                        I::LocalGet(self.v(*x)),
                        I::F32x4ExtractLane(lane),
                        I::LocalGet(self.v(v)),
                        I::LocalGet(self.v(*i)),
                        I::I32Const(i32::from(lane)),
                        I::I32Eq,
                        I::Select,
                    ]);
                }
                self.ins.push(I::LocalSet(self.v(v)));
            }
            ir::Expr::ExtractDyn(x, i) => {
                let xt = self.vty(*x);
                self.ins.push(I::LocalGet(self.v(*x)));
                let et = self.project(xt, &ir::Proj::Index(*i))?;
                if let Some(n) = self.vec_n(et) {
                    self.vec_load(n, 0);
                    self.ins.push(I::LocalSet(self.v(v)));
                } else if self.m.types.is_aggregate(et) {
                    self.ins.push(I::LocalSet(self.v(v)));
                } else {
                    let s = self.scalar(et)?;
                    self.ins.extend([load_op(s, 0), I::LocalSet(self.v(v))]);
                }
            }
            ir::Expr::Splat(x, _) if self.at.simd => {
                self.ins.extend([I::LocalGet(self.v(*x)), I::F32x4Splat, I::LocalSet(self.v(v))]);
            }
            ir::Expr::Swizzle(x, comps) if self.at.simd => {
                // Lanes past the result's repeat its first component.
                let lanes = std::array::from_fn(|k| comps.get(k).copied().unwrap_or(comps[0]));
                self.ins.extend([
                    I::LocalGet(self.v(*x)),
                    I::LocalGet(self.v(*x)),
                    I::I8x16Shuffle(ops::shuffle(lanes)),
                    I::LocalSet(self.v(v)),
                ]);
            }
            ir::Expr::Splat(x, n) => {
                let a = self.fresh_slot(v, false)?;
                for c in 0..*n as u32 {
                    self.ins.extend([
                        I::LocalGet(a),
                        I::LocalGet(self.v(*x)),
                        I::F32Store(mem(4 * c, 2)),
                    ]);
                }
            }
            ir::Expr::Swizzle(x, comps) => {
                let a = self.fresh_slot(v, false)?;
                for (k, c) in comps.iter().enumerate() {
                    self.ins.extend([
                        I::LocalGet(a),
                        I::LocalGet(self.v(*x)),
                        I::F32Load(mem(4 * *c as u32, 2)),
                        I::F32Store(mem(4 * k as u32, 2)),
                    ]);
                }
            }
            ir::Expr::Convert(x, to) => {
                let from = self.scalar(self.vty(*x))?;
                self.ins.push(I::LocalGet(self.v(*x)));
                ops::convert(self, from, *to)?;
                self.ins.push(I::LocalSet(self.v(v)));
            }
            ir::Expr::Bitcast(x, to) => {
                let from = self.scalar(self.vty(*x))?;
                self.ins.push(I::LocalGet(self.v(*x)));
                // A NaN's bits are canonical where a bit cast observes them (§11).
                match (from, to) {
                    (ir::Scalar::F32, ir::Scalar::U32 | ir::Scalar::I32) => {
                        ops::canonical_on_stack(self, false);
                        self.ins.push(I::I32ReinterpretF32)
                    }
                    (ir::Scalar::U32 | ir::Scalar::I32, ir::Scalar::F32) => {
                        self.ins.push(I::F32ReinterpretI32)
                    }
                    (ir::Scalar::F64, ir::Scalar::U64 | ir::Scalar::I64) => {
                        ops::canonical_on_stack(self, true);
                        self.ins.push(I::I64ReinterpretF64)
                    }
                    (ir::Scalar::U64 | ir::Scalar::I64, ir::Scalar::F64) => {
                        self.ins.push(I::F64ReinterpretI64)
                    }
                    // Integers of one width share their bits: nothing to do.
                    (ir::Scalar::U32 | ir::Scalar::I32, ir::Scalar::U32 | ir::Scalar::I32)
                    | (ir::Scalar::U64 | ir::Scalar::I64, ir::Scalar::U64 | ir::Scalar::I64) => {}
                    _ => return Err(format!("internal: bitcast {from:?} to {to:?}")),
                }
                self.ins.push(I::LocalSet(self.v(v)));
            }
            ir::Expr::Select { cond, if_true, if_false } => {
                self.ins.extend([
                    I::LocalGet(self.v(*if_true)),
                    I::LocalGet(self.v(*if_false)),
                    I::LocalGet(self.v(*cond)),
                    I::Select,
                    I::LocalSet(self.v(v)),
                ]);
            }
            ir::Expr::Unary(op, x) => ops::unary(self, v, *op, *x)?,
            ir::Expr::Binary(op, a, b) => {
                ops::binary(self, v, *op, *a, *b)?;
                if !op.is_comparison() {
                    ops::nan_check(self, v, &[*a, *b]);
                }
            }
            ir::Expr::Builtin(b, args) => {
                ops::builtin(self, v, *b, args)?;
                ops::nan_check(self, v, args);
            }
            ir::Expr::Host(op, args) => ops::host(self, op, args, Some(v))?,
            ir::Expr::Mem(op, args) => self.mem_op(*op, args, Some(v))?,
            ir::Expr::EntryInput(_) => return Err("internal: a GPU entry input in CPU code".into()),
            ir::Expr::ArrayLength(_) => {
                return Err("internal: a storage buffer's length in CPU code".into());
            }
            ir::Expr::Texture(..) | ir::Expr::TextureStore(..) => {
                return Err("internal: a texture read or write in CPU code".into());
            }
            ir::Expr::Atomic(..) | ir::Expr::Barrier | ir::Expr::Discard => {
                return Err("internal: an atomic or a barrier in CPU code".into());
            }
        }
        Ok(())
    }

    fn place_ty(&mut self, p: &ir::Place) -> R<ir::TypeId> {
        self.m.place_ty(self.f, p).ok_or_else(|| format!("internal: a place with no type: {p:?}"))
    }

    fn construct(&mut self, v: ir::ValueId, t: ir::TypeId, parts: &[ir::ValueId]) -> R<()> {
        if self.vec_n(t).is_some() {
            // The first component in every lane, then the others in theirs.
            let (first, rest) = parts.split_first().ok_or("internal: an empty vector")?;
            self.ins.extend([I::LocalGet(self.v(*first)), I::F32x4Splat]);
            for (k, p) in rest.iter().enumerate() {
                self.ins.extend([I::LocalGet(self.v(*p)), I::F32x4ReplaceLane(k as u8 + 1)]);
            }
            self.ins.push(I::LocalSet(self.v(v)));
            return Ok(());
        }
        // A vector's parts are its components; other types can have padding. An array's
        // offsets saturate: an array of 4 GiB or more has a function that traps on entry (its
        // frame saturates too), so this code never runs.
        let a = self.fresh_slot(v, !self.m.types.is_vector(t))?;
        for (k, &p) in parts.iter().enumerate() {
            self.store_at(a, self.part_offset(t, k as u32)?, p)?;
        }
        Ok(())
    }

    fn extract(&mut self, v: ir::ValueId, x: ir::ValueId, i: u32) -> R<()> {
        let xt = self.vty(x);
        if self.vec_n(xt).is_some() {
            let lane = u8::try_from(i).map_err(|_| "internal: a vector component past 3")?;
            self.ins.extend([
                I::LocalGet(self.v(x)),
                I::F32x4ExtractLane(lane),
                I::LocalSet(self.v(v)),
            ]);
            return Ok(());
        }
        let off = self.part_offset(xt, i)?;
        let et = self.m.types.part(xt, i).ok_or("internal: a part with no type")?;
        self.ins.push(I::LocalGet(self.v(x)));
        if let Some(n) = self.vec_n(et) {
            self.vec_load(n, off);
        } else if self.m.types.is_aggregate(et) {
            // An immutable view of part of an immutable value: no copy needed.
            self.add_offset(off);
        } else {
            let s = self.scalar(et)?;
            self.ins.push(load_op(s, off));
        }
        self.ins.push(I::LocalSet(self.v(v)));
        Ok(())
    }
}

mod ops;
mod vloop;
pub(crate) use ops::helper_bodies;

/// The start function of a module whose memory is shared: copies the constants (passive data
/// segment 0, `len` bytes) to `base` once, however many instances share the memory. The first
/// instance claims [`memory::DATA_READY`] and copies; any other waits until it reads 2. Each
/// then drops its segment.
pub(crate) fn init_data(base: u32, len: u32) -> Function {
    let flag = memory::DATA_READY as i32;
    let ins = [
        I::I32Const(flag),
        I::I32Const(0),
        I::I32Const(1),
        I::I32AtomicRmwCmpxchg(mem(0, 2)),
        I::If(BlockType::Empty),
        // Another instance copies, or has.
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty),
        I::I32Const(flag),
        I::I32AtomicLoad(mem(0, 2)),
        I::I32Const(2),
        I::I32Eq,
        I::BrIf(1),
        I::I32Const(flag),
        I::I32Const(1),
        I::I64Const(-1),
        I::MemoryAtomicWait32(mem(0, 2)),
        I::Drop,
        I::Br(0),
        I::End,
        I::End,
        I::Else,
        I::I32Const(base as i32),
        I::I32Const(0),
        I::I32Const(len as i32),
        I::MemoryInit { mem: 0, data_index: 0 },
        I::I32Const(flag),
        I::I32Const(2),
        I::I32AtomicStore(mem(0, 2)),
        I::I32Const(flag),
        I::I32Const(-1),
        I::MemoryAtomicNotify(mem(0, 2)),
        I::Drop,
        I::End,
        I::DataDrop(0),
        I::End,
    ];
    function([], &ins)
}

/// A thread entry's export (`@thread_entry`, wrela_abi `memory`'s threads): its first argument
/// is the thread's number, which picks the stack it runs on and the block it uses (a number
/// past the last thread traps); then it calls the entry, `entry`, with every argument, `params`
/// `i32`s.
pub(crate) fn thread_entry_wrapper(entry: u32, params: u32) -> Function {
    let size = memory::THREAD_STACK_SIZE as i32;
    let mut ins = vec![
        I::LocalGet(0),
        I::I32Const(memory::THREADS as i32),
        I::I32GeU,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End,
        // The program's thread's stack, or, for thread t from 1, STACK_TOP + (t - 1) × size up
        // to one size more.
        I::LocalGet(0),
        I::If(BlockType::Empty),
        I::LocalGet(0),
        I::I32Const(1),
        I::I32Sub,
        I::I32Const(size),
        I::I32Mul,
        I::I32Const(memory::STACK_TOP as i32),
        I::I32Add,
        I::GlobalSet(globals::STACK_FLOOR),
        I::GlobalGet(globals::STACK_FLOOR),
        I::I32Const(size),
        I::I32Add,
        I::GlobalSet(globals::SP),
        I::Else,
        I::I32Const(memory::STACK_LIMIT as i32),
        I::GlobalSet(globals::STACK_FLOOR),
        I::I32Const(memory::STACK_TOP as i32),
        I::GlobalSet(globals::SP),
        I::End,
        // Its block.
        I::LocalGet(0),
        I::I32Const(memory::THREAD_BLOCK_SIZE as i32),
        I::I32Mul,
        I::I32Const(memory::THREAD_BLOCKS as i32),
        I::I32Add,
        I::GlobalSet(globals::THREAD),
    ];
    for i in 0..params {
        ins.push(I::LocalGet(i));
    }
    ins.extend([I::Call(entry), I::End]);
    function([], &ins)
}

/// An export's wrapper: scalars and flattened vectors in, a result's scalars out (a struct's,
/// a tuple's or an array's in order), then a flush. A vector crosses as its components;
/// inside, it's in memory on the shadow stack (the result's room first, then each vector
/// parameter's). A host passes an 8- or 16-bit integer or a `bool` as any i32: it's brought
/// into range as a conversion would (the low bits; any nonzero `bool` is true), since the code
/// inside assumes it is.
pub(crate) fn export_wrapper(
    m: &ir::Module,
    f: ir::FuncId,
    index: u32,
    h: &Helpers,
    stack_top: u32,
    simd: bool,
) -> R<(Vec<ValType>, Vec<ValType>, Function)> {
    let func = &m.functions[f.index()];
    // The bytes of a value's slot in the wrapper's frame: a multiple of 16, so each slot is
    // aligned.
    let slot_bytes = |t| round_up(16, layout(&m.types, t).size) as i32;
    // A result: a scalar; an f32 vector as a `v128`, with SIMD (`comps` lanes); or anything
    // else in memory (`sret`), returned as its scalars in order (`leaves`).
    let (results, sret, comps, leaves, ret_bytes) = match func.ret {
        None => (Vec::new(), false, 0, Vec::new(), 0),
        Some(t) => match (m.types.get(t), v128_lanes(&m.types, t, simd)) {
            (ir::TypeDef::Scalar(s), _) => (vec![valtype(*s)], false, 0, Vec::new(), 0),
            (_, Some(n)) => (vec![ValType::F32; n as usize], false, u32::from(n), Vec::new(), 0),
            _ => {
                let leaves = ir::layout::scalars(&m.types, t);
                if leaves.is_empty() {
                    return Err(format!("internal: export {} returns no numbers", func.name));
                }
                let results = leaves.iter().map(|&(_, s)| valtype(s)).collect();
                (results, true, 0, leaves, slot_bytes(t))
            }
        },
    };
    // Each parameter: its first WASM parameter, and for a vector, its components and offset
    // (`None` for a `v128`).
    let mut params = Vec::new();
    let mut ins_args = Vec::new();
    let mut normalize = Vec::new();
    let mut size = ret_bytes;
    for p in &func.params {
        let at = params.len() as u32;
        match (m.types.get(p.ty), v128_lanes(&m.types, p.ty, simd)) {
            (_, Some(n)) if !p.by_ref => {
                params.extend(std::iter::repeat_n(ValType::F32, n as usize));
                ins_args.push((at, Some((u32::from(n), None))));
            }
            // As a place (or by value, without SIMD, or not f32s), a vector parameter is its
            // address.
            (&ir::TypeDef::Vector(s, n), _) => {
                params.extend(std::iter::repeat_n(valtype(s), n as usize));
                ins_args.push((at, Some((u32::from(n), Some(size)))));
                size += slot_bytes(p.ty);
            }
            _ if in_memory(&m.types, p.ty, simd) => {
                return Err(format!("internal: export {} takes an aggregate", func.name));
            }
            _ if p.by_ref => return Err(format!("internal: export {} takes a place", func.name)),
            _ => {
                params.push(repr(&m.types, p.ty, simd));
                ins_args.push((at, None));
                if let Some(s) = m.types.as_scalar(p.ty) {
                    normalize.push((at, s));
                }
            }
        }
    }
    let n = params.len() as u32;
    // Locals: the frame's address, then the scalar or `v128` result while the flush runs.
    let frame = n;
    let mut locals: Vec<ValType> = vec![ValType::I32];
    if !sret && !results.is_empty() {
        locals.push(if comps > 0 { ValType::V128 } else { results[0] });
    }
    let result = n + 1;
    // A call starts afresh even after one that trapped (a host may go on calling, as tests
    // do): the shadow stack empty, and the batch the trap left unsubmitted dropped. The
    // resources it made, the host never saw, so their handles are made again.
    let mut ins: Vec<I<'static>> = vec![
        I::I32Const(stack_top as i32),
        I::GlobalSet(globals::SP),
        I::I32Const(0),
        I::GlobalSet(globals::DEPTH),
        I::GlobalGet(globals::CMD_LEN),
        I::If(BlockType::Empty),
        I::I32Const(0),
        I::GlobalSet(globals::CMD_LEN),
        I::GlobalGet(globals::SUBMITTED_TO),
        I::GlobalSet(globals::NEXT_HANDLE),
        I::End,
    ];
    for (at, s) in normalize {
        let fix = match s {
            ir::Scalar::U8 => vec![I::I32Const(0xFF), I::I32And],
            ir::Scalar::U16 => vec![I::I32Const(0xFFFF), I::I32And],
            ir::Scalar::I8 => vec![I::I32Extend8S],
            ir::Scalar::I16 => vec![I::I32Extend16S],
            ir::Scalar::Bool => vec![I::I32Const(0), I::I32Ne],
            _ => continue,
        };
        ins.push(I::LocalGet(at));
        ins.extend(fix);
        ins.push(I::LocalSet(at));
    }
    if size > 0 {
        ins.extend([
            I::GlobalGet(globals::SP),
            I::I32Const(size),
            I::I32Sub,
            I::LocalTee(frame),
            I::GlobalSet(globals::SP),
        ]);
    }
    for (p, &(at, vector)) in func.params.iter().zip(&ins_args) {
        if let Some((_, Some(off))) = vector {
            for (c, (o, s)) in (at..).zip(ir::layout::scalars(&m.types, p.ty)) {
                ins.extend([I::LocalGet(frame), I::LocalGet(c), store_op(s, off as u32 + o)]);
            }
        }
    }
    if sret {
        ins.push(I::LocalGet(frame));
    }
    for &(at, vector) in &ins_args {
        match vector {
            Some((_, Some(off))) => ins.extend([I::LocalGet(frame), I::I32Const(off), I::I32Add]),
            Some((comps, None)) => {
                ins.extend([I::LocalGet(at), I::F32x4Splat]);
                for c in 1..comps {
                    ins.extend([I::LocalGet(at + c), I::F32x4ReplaceLane(c as u8)]);
                }
            }
            None => ins.push(I::LocalGet(at)),
        }
    }
    ins.push(I::Call(index));
    if !sret && !results.is_empty() {
        ins.push(I::LocalSet(result));
    }
    ins.push(I::Call(h.flush));
    if sret {
        for &(at, s) in &leaves {
            ins.extend([I::LocalGet(frame), load_op(s, at)]);
        }
    } else if comps > 0 {
        for c in 0..comps {
            ins.extend([I::LocalGet(result), I::F32x4ExtractLane(c as u8)]);
        }
    } else if !results.is_empty() {
        ins.push(I::LocalGet(result));
    }
    if size > 0 {
        ins.extend([I::LocalGet(frame), I::I32Const(size), I::I32Add, I::GlobalSet(globals::SP)]);
    }
    ins.push(I::End);
    Ok((params, results, function(locals, &ins)))
}
