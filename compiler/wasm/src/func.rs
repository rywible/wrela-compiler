//! One IR function to one WASM function, plus the generated helpers and export wrappers.

use crate::{Helpers, globals, memory, repr, sret, valtype};
use std::collections::HashMap;
use wasm_encoder::{BlockType, Function, Instruction as I, MemArg, ValType};
use wrela_ir as ir;
use wrela_ir::layout::{array_stride, column_stride, field_offsets, layout, round_up};

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
    /// The index of the module's first IR function: function `f`'s is `first_fn + f`.
    first_fn: u32,
    helpers: &'m Helpers,
    /// Each constant's address (`ir::Module::data`).
    data: &'m [u32],
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
    value_slots: Vec<Option<u32>>,
    ir_locals: Vec<LocalRepr>,
    frame: u32,
    labels: Vec<Label>,
    /// Where the source location changes: the index in `ins` and the location (`ir::Stmt::At`).
    marks: Vec<(usize, wrela_diag::Span)>,
}

/// Whether an expression makes a new aggregate in memory (and so needs a frame slot).
fn makes_memory(m: &ir::Module, f: &ir::Function, v: ir::ValueId, e: &ir::Expr) -> bool {
    if !m.types.is_aggregate(f.value_ty(v)) {
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
        ir::Expr::Call(callee, _) => sret(m, &m.functions[callee.index()]),
        _ => false,
    }
}

/// A function's code, and where each source location starts in it (byte offsets from the
/// start of its body).
pub(crate) fn emit_function(
    m: &ir::Module,
    index: usize,
    first_fn: u32,
    helpers: &Helpers,
    data: &[u32],
) -> R<(Function, Vec<(u32, wrela_diag::Span)>)> {
    let f = &m.functions[index];
    let sret = sret(m, f);
    let mut fe = Fe {
        m,
        f,
        first_fn,
        helpers,
        data,
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
        value_slots: vec![None; f.values.len()],
        ir_locals: Vec::new(),
        frame: 0,
        labels: Vec::new(),
        marks: Vec::new(),
    };
    fe.fp = fe.new_local(ValType::I32);
    fe.saved_sp = fe.new_local(ValType::I32);
    // Frame slots: aggregate and address-taken locals, then each aggregate value made in
    // memory. More slots can be added while the body is emitted: the prologue, which sets the
    // frame's size, is emitted last.
    let mut addressed = vec![false; f.locals.len()];
    let mut in_memory = Vec::new();
    ir::visit::walk(&f.body, &mut |s| {
        mark_addressed(s, &mut addressed);
        if let ir::Stmt::Let(v, e) = s
            && makes_memory(m, f, *v, e)
        {
            in_memory.push(*v);
        }
    });
    for (l, &taken) in f.locals.iter().zip(&addressed) {
        if m.types.is_aggregate(l.ty) || taken {
            let off = fe.slot(l.ty);
            fe.ir_locals.push(LocalRepr::Slot(off));
        } else {
            let w = fe.new_local(repr(&m.types, l.ty));
            fe.ir_locals.push(LocalRepr::Scalar(w));
        }
    }
    for v in in_memory {
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
    let prologue = [
        I::GlobalGet(globals::SP),
        I::LocalTee(fe.saved_sp),
        I::I32Const(memory::STACK_LIMIT as i32),
        I::I32Sub,
        I::I32Const(frame as i32),
        I::I32LtU,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End,
        I::LocalGet(fe.saved_sp),
        I::I32Const(frame as i32),
        I::I32Sub,
        I::LocalTee(fe.fp),
        I::GlobalSet(globals::SP),
    ];
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
        self.ins.extend([I::LocalGet(self.saved_sp), I::GlobalSet(globals::SP)]);
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
        let size = layout(&self.m.types, t).size;
        self.ins.extend([I::I32Const(size as i32), I::MemoryCopy { src_mem: 0, dst_mem: 0 }]);
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
                let at = self.data.get(d.index()).ok_or("internal: missing constant data")?;
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
                self.add_offset(field_offsets(&self.m.types, t)[*k as usize]);
            }
            (ir::TypeDef::Vector(_), ir::Proj::Comp(c)) => self.add_offset(4 * u32::from(*c)),
            (ir::TypeDef::Vector(n), ir::Proj::Index(i)) => {
                self.bounds_check(*i, *n as u32);
                self.ins.extend([I::LocalGet(self.v(*i)), I::I32Const(4), I::I32Mul, I::I32Add]);
            }
            (ir::TypeDef::Matrix(n), ir::Proj::Index(i)) => {
                self.bounds_check(*i, *n as u32);
                self.ins.extend([
                    I::LocalGet(self.v(*i)),
                    I::I32Const(column_stride(*n) as i32),
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
        self.ins.extend([I::LocalGet(self.v(i)), I::I32Const(n as i32), I::I32GeU]);
        self.trap_if();
    }

    /// A type the module must already have (a vector's component, a matrix's column).
    fn find_type(&self, d: &ir::TypeDef) -> R<ir::TypeId> {
        self.m.types.lookup(d).ok_or_else(|| format!("internal: the type {d:?} isn't interned"))
    }

    fn load_place(&mut self, p: &ir::Place, dst: ir::ValueId) -> R<()> {
        if let ir::PlaceRoot::Local(l) = &p.root
            && p.path.is_empty()
            && let LocalRepr::Scalar(w) = self.ir_locals[l.index()]
        {
            self.ins.extend([I::LocalGet(w), I::LocalSet(self.v(dst))]);
            return Ok(());
        }
        let t = self.vty(dst);
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
            && p.path.is_empty()
            && let LocalRepr::Scalar(w) = self.ir_locals[l.index()]
        {
            self.ins.extend([I::LocalGet(self.v(v)), I::LocalSet(w)]);
            return Ok(());
        }
        let t = self.vty(v);
        self.addr(p)?;
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
            self.ins.extend([
                I::LocalGet(a),
                I::I32Const(0),
                I::I32Const(size as i32),
                I::MemoryFill(0),
            ]);
        }
        Ok(a)
    }

    // ---- statements ------------------------------------------------------------------------

    fn block(&mut self, b: &ir::Block) -> R<()> {
        for s in b {
            self.stmt(s)?;
        }
        Ok(())
    }

    fn depth_of(&self, want: Label) -> R<u32> {
        let pos =
            self.labels.iter().rposition(|l| *l == want).ok_or("internal: break outside a loop")?;
        Ok((self.labels.len() - 1 - pos) as u32)
    }

    /// Emits a statement: a `let`'s value gets a local first; afterwards, the statement's
    /// scratch locals, and those of the values it last uses, are free.
    fn stmt(&mut self, s: &ir::Stmt) -> R<()> {
        if let ir::Stmt::Let(v, _) = s {
            let t = repr(&self.m.types, self.vty(*v));
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
                self.block(then)?;
                if !else_.is_empty() {
                    self.ins.push(I::Else);
                    self.block(else_)?;
                }
                self.labels.pop();
                self.ins.push(I::End);
                Ok(())
            }
            ir::Stmt::Loop { body, continuing } => {
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
                let sret = sret(self.m, callee);
                if let Some(t) = callee.ret
                    && sret
                {
                    let off = self.slot(t);
                    self.frame_addr(off);
                }
                self.call_args(args)?;
                self.ins.push(I::Call(self.first_fn + f.0));
                if callee.ret.is_some() && !sret {
                    self.ins.push(I::Drop);
                }
                Ok(())
            }
            ir::Expr::Host(op, args) => ops::host(self, op, args, None),
            _ => Ok(()),
        }
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
                if self.m.types.is_aggregate(t) {
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
                let sret = sret(self.m, &self.m.functions[f.index()]);
                if sret {
                    self.slot_addr(v)?;
                }
                self.call_args(args)?;
                self.ins.push(I::Call(self.first_fn + f.0));
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
                self.ins.extend([I::LocalGet(a), I::I32Const(*k as i32), I::I32Store(mem(0, 2))]);
                if let Some(p) = payload {
                    let off = field_offsets(&self.m.types, t)[1 + *k as usize];
                    self.store_at(a, off, *p)?;
                }
            }
            ir::Expr::Extract(x, i) => self.extract(v, *x, *i)?,
            ir::Expr::ExtractDyn(x, i) => {
                let xt = self.vty(*x);
                self.ins.push(I::LocalGet(self.v(*x)));
                let et = self.project(xt, &ir::Proj::Index(*i))?;
                if self.m.types.is_aggregate(et) {
                    self.ins.push(I::LocalSet(self.v(v)));
                } else {
                    let s = self.scalar(et)?;
                    self.ins.extend([load_op(s, 0), I::LocalSet(self.v(v))]);
                }
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
                match (from, to) {
                    (ir::Scalar::F32, ir::Scalar::U32 | ir::Scalar::I32) => {
                        self.ins.push(I::I32ReinterpretF32)
                    }
                    (ir::Scalar::U32 | ir::Scalar::I32, ir::Scalar::F32) => {
                        self.ins.push(I::F32ReinterpretI32)
                    }
                    (ir::Scalar::F64, ir::Scalar::U64 | ir::Scalar::I64) => {
                        self.ins.push(I::I64ReinterpretF64)
                    }
                    (ir::Scalar::U64 | ir::Scalar::I64, ir::Scalar::F64) => {
                        self.ins.push(I::F64ReinterpretI64)
                    }
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
            ir::Expr::Binary(op, a, b) => ops::binary(self, v, *op, *a, *b)?,
            ir::Expr::Builtin(b, args) => ops::builtin(self, v, *b, args)?,
            ir::Expr::Host(op, args) => ops::host(self, op, args, Some(v))?,
            ir::Expr::EntryInput(_) => return Err("internal: a GPU entry input in CPU code".into()),
            ir::Expr::ArrayLength(_) => {
                return Err("internal: a storage buffer's length in CPU code".into());
            }
        }
        Ok(())
    }

    fn place_ty(&mut self, p: &ir::Place) -> R<ir::TypeId> {
        self.m.place_ty(self.f, p).ok_or_else(|| format!("internal: a place with no type: {p:?}"))
    }

    fn construct(&mut self, v: ir::ValueId, t: ir::TypeId, parts: &[ir::ValueId]) -> R<()> {
        // A vector's parts are its components; other types can have padding.
        let a = self.fresh_slot(v, !self.m.types.is_vector(t))?;
        match *self.ty(t) {
            ir::TypeDef::Struct { .. } => {
                let offs = field_offsets(&self.m.types, t);
                for (k, &p) in parts.iter().enumerate() {
                    self.store_at(a, offs[k], p)?;
                }
            }
            ir::TypeDef::Vector(_) => {
                for (k, &p) in parts.iter().enumerate() {
                    self.store_at(a, 4 * k as u32, p)?;
                }
            }
            ir::TypeDef::Matrix(n) => {
                let stride = column_stride(n);
                for (k, &p) in parts.iter().enumerate() {
                    self.store_at(a, stride * k as u32, p)?;
                }
            }
            ir::TypeDef::Array(e, _) => {
                // Saturating: an array of 4 GiB or more has a function that traps on entry (its
                // frame saturates too), so this code never runs.
                let stride = array_stride(&self.m.types, e);
                for (k, &p) in parts.iter().enumerate() {
                    self.store_at(a, stride.saturating_mul(k as u32), p)?;
                }
            }
            ref d => return Err(format!("internal: construct {d:?}")),
        }
        Ok(())
    }

    fn extract(&mut self, v: ir::ValueId, x: ir::ValueId, i: u32) -> R<()> {
        let xt = self.vty(x);
        let (off, et) = match *self.ty(xt) {
            ir::TypeDef::Struct { .. } | ir::TypeDef::Enum { .. } => {
                let ft = self.m.types.field(xt, i).ok_or("internal: a missing field")?;
                (field_offsets(&self.m.types, xt)[i as usize], ft)
            }
            ir::TypeDef::Vector(_) => {
                (4 * i, self.find_type(&ir::TypeDef::Scalar(ir::Scalar::F32))?)
            }
            ir::TypeDef::Matrix(n) => {
                (column_stride(n) * i, self.find_type(&ir::TypeDef::Vector(n))?)
            }
            // Saturating, as in `construct`.
            ir::TypeDef::Array(e, _) => (array_stride(&self.m.types, e).saturating_mul(i), e),
            ir::TypeDef::Run(_) => (4 * i, self.find_type(&ir::TypeDef::Scalar(ir::Scalar::U32))?),
            ref d => return Err(format!("internal: extract from {d:?}")),
        };
        self.ins.push(I::LocalGet(self.v(x)));
        if self.m.types.is_aggregate(et) {
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
pub(crate) use ops::helper_bodies;

/// An export's wrapper: scalars and flattened vectors in, a scalar or a flattened vector out,
/// then a flush. A vector crosses as its components; inside, it's in memory on the shadow stack
/// (the result's room first, then each vector parameter's). A host passes an 8- or 16-bit
/// integer or a `bool` as any i32: it's brought into range as a conversion would (the low bits;
/// any nonzero `bool` is true), since the code inside assumes it is.
pub(crate) fn export_wrapper(
    m: &ir::Module,
    f: ir::FuncId,
    index: u32,
    h: &Helpers,
) -> R<(Vec<ValType>, Vec<ValType>, Function)> {
    const VEC: i32 = 16;
    let func = &m.functions[f.index()];
    let (results, sret, comps) = match func.ret {
        None => (Vec::new(), false, 0),
        Some(t) => match m.types.get(t) {
            ir::TypeDef::Scalar(s) => (vec![valtype(*s)], false, 0),
            ir::TypeDef::Vector(n) => (vec![ValType::F32; *n as usize], true, u32::from(*n)),
            d => return Err(format!("internal: export returns {d:?}")),
        },
    };
    // Each parameter: its first WASM parameter, and for a vector, its components and offset.
    let mut params = Vec::new();
    let mut ins_args = Vec::new();
    let mut normalize = Vec::new();
    let mut size = if sret { VEC } else { 0 };
    for p in &func.params {
        let at = params.len() as u32;
        match m.types.get(p.ty) {
            // By value or as a place, a vector parameter is its address.
            ir::TypeDef::Vector(n) => {
                params.extend(std::iter::repeat_n(ValType::F32, *n as usize));
                ins_args.push((at, Some((u32::from(*n), size))));
                size += VEC;
            }
            _ if m.types.is_aggregate(p.ty) => {
                return Err(format!("internal: export {} takes an aggregate", func.name));
            }
            _ if p.by_ref => return Err(format!("internal: export {} takes a place", func.name)),
            _ => {
                params.push(repr(&m.types, p.ty));
                ins_args.push((at, None));
                if let Some(s) = m.types.as_scalar(p.ty) {
                    normalize.push((at, s));
                }
            }
        }
    }
    let n = params.len() as u32;
    // Locals: the frame's address, then the scalar result while the flush runs.
    let frame = n;
    let mut locals: Vec<ValType> = vec![ValType::I32];
    if !sret && !results.is_empty() {
        locals.push(results[0]);
    }
    let result = n + 1;
    // The previous call's buffers go first.
    let mut ins: Vec<I<'static>> = vec![I::Call(h.release)];
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
    for &(at, vector) in &ins_args {
        if let Some((comps, off)) = vector {
            for c in 0..comps {
                ins.extend([
                    I::LocalGet(frame),
                    I::LocalGet(at + c),
                    I::F32Store(mem(off as u32 + 4 * c, 2)),
                ]);
            }
        }
    }
    if sret {
        ins.push(I::LocalGet(frame));
    }
    for &(at, vector) in &ins_args {
        match vector {
            Some((_, off)) => ins.extend([I::LocalGet(frame), I::I32Const(off), I::I32Add]),
            None => ins.push(I::LocalGet(at)),
        }
    }
    ins.push(I::Call(index));
    if !sret && !results.is_empty() {
        ins.push(I::LocalSet(result));
    }
    ins.push(I::Call(h.flush));
    if sret {
        for c in 0..comps {
            ins.extend([I::LocalGet(frame), I::F32Load(mem(4 * c, 2))]);
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
