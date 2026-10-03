//! One IR function to one WASM function, plus the generated helpers and export wrappers.

use crate::{Helpers, globals, memory, repr, signature, valtype};
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
    fn_indices: &'m [u32],
    helpers: &'m Helpers,
    ins: Vec<I<'static>>,
    locals: Vec<ValType>,
    nparams: u32,
    sret: Option<u32>,
    params: Vec<u32>,
    fp: u32,
    /// The stack pointer on entry, restored on every exit.
    saved_sp: u32,
    values: Vec<u32>,
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
        ir::Expr::Call(callee, _) => !m.functions[callee.index()].ret_ref,
        _ => false,
    }
}

/// A function's code, and where each source location starts in it (byte offsets from the
/// start of its body).
pub(crate) fn emit_function(
    m: &ir::Module,
    index: usize,
    fn_indices: &[u32],
    helpers: &Helpers,
) -> R<(Function, Vec<(u32, wrela_diag::Span)>)> {
    let f = &m.functions[index];
    let (params, _results, sret) = signature(m, f);
    let mut fe = Fe {
        m,
        f,
        fn_indices,
        helpers,
        ins: Vec::new(),
        locals: Vec::new(),
        nparams: params.len() as u32,
        sret: sret.then_some(0),
        params: Vec::new(),
        fp: 0,
        saved_sp: 0,
        values: Vec::new(),
        value_slots: vec![None; f.values.len()],
        ir_locals: Vec::new(),
        frame: 0,
        labels: Vec::new(),
        marks: Vec::new(),
    };
    let first = u32::from(sret);
    fe.params = (0..f.params.len() as u32).map(|i| first + i).collect();
    fe.fp = fe.new_local(ValType::I32);
    fe.saved_sp = fe.new_local(ValType::I32);
    // Frame slots: aggregate and address-taken locals, then each aggregate value made in
    // memory. More slots can be added while the body is emitted: the prologue, which sets the
    // frame's size, is emitted last.
    let mut addressed = vec![false; f.locals.len()];
    address_taken(&f.body, &mut addressed);
    for (l, &taken) in f.locals.iter().zip(&addressed) {
        if m.types.is_aggregate(l.ty) || taken {
            let off = fe.slot(l.ty);
            fe.ir_locals.push(LocalRepr::Slot(off));
        } else {
            let w = fe.new_local(repr(&m.types, l.ty));
            fe.ir_locals.push(LocalRepr::Scalar(w));
        }
    }
    let mut lets = Vec::new();
    collect_lets(&f.body, &mut lets);
    for (v, e) in lets {
        if makes_memory(m, f, v, e) {
            let t = f.value_ty(v);
            fe.value_slots[v.index()] = Some(fe.slot(t));
        }
    }
    fe.values = f
        .values
        .iter()
        .map(|&t| repr(&m.types, t))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|vt| fe.new_local(vt))
        .collect();
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
    let mut func = Function::new(fe.locals.iter().map(|t| (1, *t)));
    for i in &prologue {
        func.instruction(i);
    }
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

/// Marks the locals whose address a body takes: passed by reference, or `Addr` of a place
/// rooted at them.
fn address_taken(b: &ir::Block, out: &mut [bool]) {
    fn mark(p: &ir::Place, out: &mut [bool]) {
        if let ir::PlaceRoot::Local(l) = p.root {
            out[l.index()] = true;
        }
    }
    for s in b {
        match s {
            ir::Stmt::Let(_, e) | ir::Stmt::Eval(e) => match e {
                ir::Expr::Addr(p) | ir::Expr::Run(p) => mark(p, out),
                ir::Expr::Call(_, args) => {
                    for a in args {
                        if let ir::Arg::Place(p) = a {
                            mark(p, out);
                        }
                    }
                }
                _ => {}
            },
            ir::Stmt::If { then, else_, .. } => {
                address_taken(then, out);
                address_taken(else_, out);
            }
            ir::Stmt::Loop { body, continuing } => {
                address_taken(body, out);
                address_taken(continuing, out);
            }
            _ => {}
        }
    }
}

fn collect_lets<'a>(b: &'a ir::Block, out: &mut Vec<(ir::ValueId, &'a ir::Expr)>) {
    for s in b {
        match s {
            ir::Stmt::Let(v, e) => out.push((*v, e)),
            ir::Stmt::If { then, else_, .. } => {
                collect_lets(then, out);
                collect_lets(else_, out);
            }
            ir::Stmt::Loop { body, continuing } => {
                collect_lets(body, out);
                collect_lets(continuing, out);
            }
            _ => {}
        }
    }
}

impl<'m> Fe<'m> {
    fn new_local(&mut self, t: ValType) -> u32 {
        self.locals.push(t);
        self.nparams + self.locals.len() as u32 - 1
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

    fn scalar(&self, t: ir::TypeId) -> R<ir::Scalar> {
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
        self.values[v.index()]
    }

    fn vty(&self, v: ir::ValueId) -> ir::TypeId {
        self.f.value_ty(v)
    }

    /// The address of a value's own frame slot.
    fn slot_addr(&mut self, v: ir::ValueId) -> R<()> {
        let off = self.value_slots[v.index()].ok_or("internal: a value without a slot")?;
        self.ins.extend([I::LocalGet(self.fp), I::I32Const(off as i32), I::I32Add]);
        Ok(())
    }

    /// Sets value `v` to its slot's address (for aggregate results).
    fn set_to_slot(&mut self, v: ir::ValueId) -> R<()> {
        self.slot_addr(v)?;
        self.ins.push(I::LocalSet(self.v(v)));
        Ok(())
    }

    // ---- places ----------------------------------------------------------------------------

    /// The root's type and, for memory roots, pushes its address. Returns `None` for a scalar
    /// local (which has no address).
    fn root(&mut self, p: &ir::Place) -> R<(ir::TypeId, Option<u32>)> {
        match &p.root {
            ir::PlaceRoot::Local(l) => {
                let t = self.f.locals[l.index()].ty;
                match self.ir_locals[l.index()] {
                    LocalRepr::Scalar(w) => Ok((t, Some(w))),
                    LocalRepr::Slot(off) => {
                        self.ins.extend([I::LocalGet(self.fp), I::I32Const(off as i32), I::I32Add]);
                        Ok((t, None))
                    }
                }
            }
            ir::PlaceRoot::Param(i) => {
                let t = self.f.params[*i as usize].ty;
                self.ins.push(I::LocalGet(self.params[*i as usize]));
                Ok((t, None))
            }
            ir::PlaceRoot::Ptr(v) => {
                let t = match self.ty(self.vty(*v)) {
                    ir::TypeDef::Ptr(t) => *t,
                    _ => return Err("internal: a pointer place of a non-pointer value".into()),
                };
                self.ins.push(I::LocalGet(self.v(*v)));
                Ok((t, None))
            }
            ir::PlaceRoot::Resource(_) => Err("internal: a GPU resource in CPU code".into()),
        }
    }

    /// Pushes a place's address (it must be in memory) and returns its type.
    fn addr(&mut self, p: &ir::Place) -> R<ir::TypeId> {
        let (mut t, scalar_local) = self.root(p)?;
        if scalar_local.is_some() {
            return Err("internal: the address of a scalar local".into());
        }
        for proj in &p.path {
            t = self.project(t, proj)?;
        }
        Ok(t)
    }

    /// Given an address on the stack of a value of type `t`, applies one projection.
    fn project(&mut self, t: ir::TypeId, proj: &ir::Proj) -> R<ir::TypeId> {
        match (self.ty(t).clone(), proj) {
            (ir::TypeDef::Struct { .. } | ir::TypeDef::Enum { .. }, ir::Proj::Field(k)) => {
                let off = field_offsets(&self.m.types, t)[*k as usize];
                if off != 0 {
                    self.ins.extend([I::I32Const(off as i32), I::I32Add]);
                }
                Ok(self.m.types.field(t, *k).ok_or("internal: a missing field")?)
            }
            (ir::TypeDef::Vector(_), ir::Proj::Comp(c)) => {
                if *c != 0 {
                    self.ins.extend([I::I32Const(4 * *c as i32), I::I32Add]);
                }
                match self.m.types.as_scalar(t) {
                    Some(_) => Ok(t),
                    None => self.f32_ty(),
                }
            }
            (ir::TypeDef::Vector(n), ir::Proj::Index(i)) => {
                self.bounds_check(*i, n as u32);
                self.ins.extend([I::LocalGet(self.v(*i)), I::I32Const(4), I::I32Mul, I::I32Add]);
                self.f32_ty()
            }
            (ir::TypeDef::Matrix(n), ir::Proj::Index(i)) => {
                let col = self.vector_ty(n)?;
                let stride = column_stride(n);
                self.bounds_check(*i, n as u32);
                self.ins.extend([
                    I::LocalGet(self.v(*i)),
                    I::I32Const(stride as i32),
                    I::I32Mul,
                    I::I32Add,
                ]);
                Ok(col)
            }
            (ir::TypeDef::Array(e, n), ir::Proj::Index(i)) => {
                let stride = array_stride(&self.m.types, e);
                self.bounds_check(*i, n);
                self.ins.extend([
                    I::LocalGet(self.v(*i)),
                    I::I32Const(stride as i32),
                    I::I32Mul,
                    I::I32Add,
                ]);
                Ok(e)
            }
            (ir::TypeDef::Run(e), ir::Proj::Index(i)) => {
                // The run is {ptr, len}: check the index against len, then ptr + i * stride.
                let stride = array_stride(&self.m.types, e);
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
                Ok(e)
            }
            (d, p) => Err(format!("internal: projection {p:?} of {d:?}")),
        }
    }

    fn bounds_check(&mut self, i: ir::ValueId, n: u32) {
        self.ins.extend([I::LocalGet(self.v(i)), I::I32Const(n as i32), I::I32GeU]);
        self.trap_if();
    }

    fn f32_ty(&self) -> R<ir::TypeId> {
        self.find_type(&ir::TypeDef::Scalar(ir::Scalar::F32))
    }

    fn vector_ty(&self, n: u8) -> R<ir::TypeId> {
        self.find_type(&ir::TypeDef::Vector(n))
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
            self.ins.extend([
                I::I32Const(layout(&self.m.types, t).size as i32),
                I::MemoryCopy { src_mem: 0, dst_mem: 0 },
            ]);
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
            self.ins.extend([
                I::LocalGet(self.v(v)),
                I::I32Const(layout(&self.m.types, t).size as i32),
                I::MemoryCopy { src_mem: 0, dst_mem: 0 },
            ]);
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
                I::I32Const(layout(&self.m.types, t).size as i32),
                I::MemoryCopy { src_mem: 0, dst_mem: 0 },
            ]);
        } else {
            let s = self.scalar(t)?;
            self.ins.extend([I::LocalGet(addr), I::LocalGet(self.v(v)), store_op(s, offset)]);
        }
        Ok(())
    }

    /// The address of value `v`'s slot, in a fresh local, zeroed.
    fn fresh_slot(&mut self, v: ir::ValueId) -> R<u32> {
        let a = self.new_local(ValType::I32);
        self.slot_addr(v)?;
        self.ins.push(I::LocalTee(a));
        let size = layout(&self.m.types, self.vty(v)).size;
        self.ins.extend([I::I32Const(0), I::I32Const(size as i32), I::MemoryFill(0)]);
        self.ins.extend([I::LocalGet(a), I::LocalSet(self.v(v))]);
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

    fn stmt(&mut self, s: &ir::Stmt) -> R<()> {
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
                        let t = self.vty(*v);
                        self.ins.extend([
                            I::LocalGet(sret),
                            I::LocalGet(self.v(*v)),
                            I::I32Const(layout(&self.m.types, t).size as i32),
                            I::MemoryCopy { src_mem: 0, dst_mem: 0 },
                        ]);
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
                let aggregate = callee.ret.is_some_and(|t| self.m.types.is_aggregate(t));
                if let Some(t) = callee.ret
                    && !callee.ret_ref
                    && aggregate
                {
                    let off = self.slot(t);
                    self.ins.extend([I::LocalGet(self.fp), I::I32Const(off as i32), I::I32Add]);
                }
                self.call_args(args)?;
                self.ins.push(I::Call(self.fn_indices[f.index()]));
                if callee.ret.is_some() && (callee.ret_ref || !aggregate) {
                    self.ins.push(I::Drop);
                }
                Ok(())
            }
            ir::Expr::Host(op, args) => crate::func::host(self, op, args, None),
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
                    self.fresh_slot(v)?;
                } else {
                    let s = self.scalar(t)?;
                    self.ins.extend([konst(s, 0.0), I::LocalSet(self.v(v))]);
                }
            }
            ir::Expr::Param(i) => {
                self.ins.extend([I::LocalGet(self.params[*i as usize]), I::LocalSet(self.v(v))])
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
                let a = self.fresh_slot(v)?;
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
                let callee = &self.m.functions[f.index()];
                let sret =
                    callee.ret.is_some_and(|rt| !callee.ret_ref && self.m.types.is_aggregate(rt));
                if sret {
                    self.slot_addr(v)?;
                }
                self.call_args(args)?;
                self.ins.push(I::Call(self.fn_indices[f.index()]));
                if sret {
                    self.set_to_slot(v)?;
                } else {
                    self.ins.push(I::LocalSet(self.v(v)));
                }
            }
            ir::Expr::Construct(_, parts) => self.construct(v, t, parts)?,
            ir::Expr::Variant(_, k, payload) => {
                // The tag, and the payload where every variant's starts.
                let a = self.fresh_slot(v)?;
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
                let a = self.fresh_slot(v)?;
                for c in 0..*n as u32 {
                    self.ins.extend([
                        I::LocalGet(a),
                        I::LocalGet(self.v(*x)),
                        I::F32Store(mem(4 * c, 2)),
                    ]);
                }
            }
            ir::Expr::Swizzle(x, comps) => {
                let a = self.fresh_slot(v)?;
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
                crate::func::convert(self, from, *to)?;
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
            ir::Expr::Unary(op, x) => crate::func::unary(self, v, *op, *x)?,
            ir::Expr::Binary(op, a, b) => crate::func::binary(self, v, *op, *a, *b)?,
            ir::Expr::Builtin(b, args) => crate::func::builtin(self, v, *b, args)?,
            ir::Expr::Host(op, args) => crate::func::host(self, op, args, Some(v))?,
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
        let a = self.fresh_slot(v)?;
        match self.ty(t).clone() {
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
                let stride = array_stride(&self.m.types, e);
                for (k, &p) in parts.iter().enumerate() {
                    self.store_at(a, stride * k as u32, p)?;
                }
            }
            d => return Err(format!("internal: construct {d:?}")),
        }
        Ok(())
    }

    fn extract(&mut self, v: ir::ValueId, x: ir::ValueId, i: u32) -> R<()> {
        let xt = self.vty(x);
        let (off, et) = match self.ty(xt).clone() {
            ir::TypeDef::Struct { .. } | ir::TypeDef::Enum { .. } => {
                let ft = self.m.types.field(xt, i).ok_or("internal: a missing field")?;
                (field_offsets(&self.m.types, xt)[i as usize], ft)
            }
            ir::TypeDef::Vector(_) => (4 * i, self.f32_ty()?),
            ir::TypeDef::Matrix(n) => {
                let col = self.vector_ty(n)?;
                (column_stride(n) * i, col)
            }
            ir::TypeDef::Array(e, _) => (array_stride(&self.m.types, e) * i, e),
            ir::TypeDef::Run(_) => (4 * i, self.find_type(&ir::TypeDef::Scalar(ir::Scalar::U32))?),
            d => return Err(format!("internal: extract from {d:?}")),
        };
        self.ins.push(I::LocalGet(self.v(x)));
        if self.m.types.is_aggregate(et) {
            // An immutable view of part of an immutable value: no copy needed.
            if off != 0 {
                self.ins.extend([I::I32Const(off as i32), I::I32Add]);
            }
        } else {
            let s = match self.ty(xt) {
                ir::TypeDef::Run(_) => ir::Scalar::U32,
                _ => self.scalar(et)?,
            };
            self.ins.push(load_op(s, off));
        }
        self.ins.push(I::LocalSet(self.v(v)));
        Ok(())
    }
}

mod ops;
use ops::{binary, builtin, convert, host, unary};

/// The generated helpers: flush, reserve and write (see `Helpers`).
pub(crate) fn helper_bodies(h: &Helpers) -> Vec<(Vec<ValType>, Vec<ValType>, Function)> {
    ops::helper_bodies(h)
}

/// An export's wrapper: scalars and flattened vectors in, a scalar or a flattened vector out,
/// then a flush. A vector crosses as its components; inside, it's in memory on the shadow stack
/// (the result's room first, then each vector parameter's).
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
    let mut fun = Function::new(locals.iter().map(|t| (1, *t)));
    for i in &ins {
        fun.instruction(i);
    }
    Ok((params, results, fun))
}
