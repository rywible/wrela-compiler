//! The heap and raw memory on the CPU (language.md §6.9, §6.14): string literals, `Vec`'s
//! elements, the runs a `Vec` or a string passes as, moves and drops, clones, panics, and the
//! intrinsics of std's unsafe core.

use crate::ModuleBuilder;
use crate::body::Fl;
use crate::glue::GlueKind;
use crate::instance::InstanceKey;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_ir as ir;
use wrela_sema::defs::Lang;
use wrela_sema::mir::{self, MoveKind};
use wrela_sema::ty::*;

impl crate::Cx<'_> {
    /// The constant data holding `s`'s UTF-8 (one copy per text in a module), and its length.
    pub fn text_data(&mut self, mb: &mut ModuleBuilder, s: &str) -> (ir::DataId, u32) {
        let n = s.len() as u32;
        if let Some(&d) = mb.texts.get(s) {
            return (d, n);
        }
        let u8 = mb.m.types.scalar(ir::Scalar::U8);
        // An empty text still has an address.
        let ty = mb.m.types.intern(ir::TypeDef::Array(u8, n.max(1)));
        let mut bytes = s.as_bytes().to_vec();
        if bytes.is_empty() {
            bytes.push(0);
        }
        let d = mb.m.add_data(ir::Data {
            name: "text".into(),
            ty,
            value: ir::ConstValue::Bytes(bytes),
        });
        mb.texts.insert(s.to_string(), d);
        (d, n)
    }

    /// The constant data holding the file `embed` reads at `path`, and its length. The driver
    /// read it before lowering.
    pub fn embed_data(&mut self, mb: &mut ModuleBuilder, path: &str) -> Option<(ir::DataId, u32)> {
        let Some(bytes) = self.data.embeds.get(path).cloned() else {
            self.err(Diagnostic::internal(format!("the build didn't read `{path}` for `embed`")));
            return None;
        };
        let n = bytes.len() as u32;
        // An empty file still has an address.
        let mut padded = bytes.to_vec();
        if padded.is_empty() {
            padded.push(0);
        }
        Some((self.bytes_data(mb, &padded), n))
    }
}

impl Fl<'_, '_> {
    fn u32_ty(&mut self) -> ir::TypeId {
        self.mb.m.types.u32()
    }

    fn lang(&self, t: TyId) -> Option<Lang> {
        self.cx.checked.program.lang_of_ty(t)
    }

    /// The address of text `s` in the build, and its length.
    fn text_parts(&mut self, s: &str) -> (ir::ValueId, ir::ValueId) {
        let (d, n) = self.cx.text_data(self.mb, s);
        let data_ty = self.mb.m.data[d.index()].ty;
        let pt = self.mb.m.types.intern(ir::TypeDef::Ptr(data_ty));
        let ptr = self.value(pt, ir::Expr::Addr(ir::Place::root(ir::PlaceRoot::Data(d))));
        let u = self.u32_ty();
        let addr = self.value(u, ir::Expr::Mem(ir::MemOp::Addr, vec![ptr]));
        let len = self.u32c(n);
        (addr, len)
    }

    /// A string literal: a `Text`, the address and length of its UTF-8 in the build.
    pub fn text(&mut self, s: &str, ty: TyId, span: Span) -> Option<ir::ValueId> {
        if self.is_gpu() {
            self.cx.err(Diagnostic::new(
                codes::E0326,
                span,
                "GPU code can't hold text: strings live on the CPU (language.md §4)",
            ));
            return None;
        }
        let t = self.ty(ty, span)?;
        let (addr, len) = self.text_parts(s);
        Some(self.value(t, ir::Expr::Construct(t, vec![addr, len])))
    }

    /// `embed("path")`: `Bytes`, the address and length of the file's bytes in the build.
    pub fn embedded(&mut self, path: &str, ty: TyId, span: Span) -> Option<ir::ValueId> {
        if self.is_gpu() {
            self.cx.err(Diagnostic::new(
                codes::E0326,
                span,
                "GPU code can't hold `Bytes`: embedded files live on the CPU (language.md §10)",
            ));
            return None;
        }
        let t = self.ty(ty, span)?;
        let (d, n) = self.cx.embed_data(self.mb, path)?;
        let data_ty = self.mb.m.data[d.index()].ty;
        let pt = self.mb.m.types.intern(ir::TypeDef::Ptr(data_ty));
        let ptr = self.value(pt, ir::Expr::Addr(ir::Place::root(ir::PlaceRoot::Data(d))));
        let u = self.u32_ty();
        let addr = self.value(u, ir::Expr::Mem(ir::MemOp::Addr, vec![ptr]));
        let len = self.u32c(n);
        Some(self.value(t, ir::Expr::Construct(t, vec![addr, len])))
    }

    /// Panics with `s`: a message for the host, then a trap (§15).
    pub fn panic_text(&mut self, s: &str) {
        let (addr, len) = self.text_parts(s);
        self.emit(ir::Stmt::Eval(ir::Expr::Mem(ir::MemOp::Panic, vec![addr, len])));
    }

    /// Panics with `s` if `cond` holds.
    fn panic_if(&mut self, cond: ir::ValueId, s: &str) {
        self.push_block();
        self.panic_text(s);
        let then = self.pop_block();
        self.emit(ir::Stmt::If { cond, then, else_: Vec::new() });
    }

    /// Panics with `s` if the `u32` `i` isn't below `len`.
    fn check_below(&mut self, i: ir::ValueId, len: ir::ValueId, s: &str) {
        let b = self.mb.m.types.bool();
        let past = self.value(b, ir::Expr::Binary(ir::BinOp::Ge, i, len));
        self.panic_if(past, s);
    }

    /// The run a place passes as for a `[T]` or `str` parameter (`run_t`): an array's, a
    /// `Vec`'s elements, a `Text`'s or a `String`'s UTF-8, or the run it is.
    pub fn run_of(&mut self, p: &mir::Place, run_t: ir::TypeId) -> Option<ir::ValueId> {
        let t = self.place_src_ty(p);
        match self.types().kind(t) {
            TyKind::Array(..) => {
                let place = self.place_or_copy(p)?;
                Some(self.value(run_t, ir::Expr::Run(place)))
            }
            TyKind::Adt(..) => match self.lang(t) {
                Some(Lang::Vec | Lang::Text | Lang::Bytes) => {
                    let addr = self.read(&p.with(mir::Proj::Field(0)))?;
                    let len = self.read(&p.with(mir::Proj::Field(1)))?;
                    Some(self.value(run_t, ir::Expr::Construct(run_t, vec![addr, len])))
                }
                Some(Lang::String) => self.run_of(&p.with(mir::Proj::Field(0)), run_t),
                _ => self.read(p),
            },
            _ => self.read(p),
        }
    }

    /// The length of what `p` holds, if it's a `Vec` or a string; `None` for anything else.
    pub fn container_len(&mut self, p: &mir::Place) -> Option<Option<ir::ValueId>> {
        let t = self.place_src_ty(p);
        Some(match self.lang(t)? {
            Lang::Vec | Lang::Text | Lang::Bytes | Lang::Bounded => {
                self.read(&p.with(mir::Proj::Field(1)))
            }
            // An arena's values: its `values` field, a `Vec`.
            Lang::Arena => self.read(&p.with(mir::Proj::Field(0)).with(mir::Proj::Field(1))),
            Lang::String => self.read(&p.with(mir::Proj::Field(0)).with(mir::Proj::Field(1))),
            _ => return None,
        })
    }

    /// Element `i` of the `Vec<elem>` at `vec`: a place at its address. An index past the end
    /// panics.
    pub fn vec_elem(
        &mut self,
        vec: ir::Place,
        vec_ty: TyId,
        elem: TyId,
        i: ir::ValueId,
    ) -> Option<ir::Place> {
        let span = self.code.span;
        let et = self.cx.lower_ty(self.mb, elem, span)?;
        let u = self.u32_ty();
        let (fd, _) = self.cx.field(self.mb, vec_ty, None, 0, span)?;
        let (fl, _) = self.cx.field(self.mb, vec_ty, None, 1, span)?;
        let data = self.load(vec.with(ir::Proj::Field(fd)), u);
        let len = self.load(vec.with(ir::Proj::Field(fl)), u);
        self.check_below(i, len, "index out of range: past the end of a `Vec`");
        let stride = ir::layout::array_stride(&self.mb.m.types, et);
        let s = self.u32c(stride);
        let off = self.value(u, ir::Expr::Binary(ir::BinOp::Mul, i, s));
        let addr = self.value(u, ir::Expr::Binary(ir::BinOp::Add, data, off));
        let pt = self.mb.m.types.intern(ir::TypeDef::Ptr(et));
        let ptr = self.value(pt, ir::Expr::Mem(ir::MemOp::Ptr, vec![addr]));
        Some(ir::Place::root(ir::PlaceRoot::Ptr(ptr)))
    }

    /// Element `i` of the `Bounded` at `b`: its array's, in place. On the CPU, an index at or
    /// past the elements in use panics; the GPU indexes the array as any other.
    pub fn bounded_elem(&mut self, b: ir::Place, b_ty: TyId, i: ir::ValueId) -> Option<ir::Place> {
        let span = self.code.span;
        let (fi, _) = self.cx.field(self.mb, b_ty, None, 0, span)?;
        if !self.is_gpu() {
            let (fl, _) = self.cx.field(self.mb, b_ty, None, 1, span)?;
            let u = self.u32_ty();
            let len = self.load(b.clone().with(ir::Proj::Field(fl)), u);
            self.check_below(i, len, "index out of range: past the elements of a `Bounded`");
        }
        Some(b.with(ir::Proj::Field(fi)).with(ir::Proj::Index(i)))
    }

    /// The value of the `Arena<elem>` at `arena` that `index` names: a `Handle<elem>`, whose
    /// slot must still hold that generation (a stale handle panics), or a position (a loop).
    pub fn arena_elem(
        &mut self,
        arena: ir::Place,
        arena_ty: TyId,
        elem: TyId,
        index: ir::ValueId,
        index_ty: TyId,
    ) -> Option<ir::Place> {
        let span = self.code.span;
        let (fv, values_ty) = self.cx.field(self.mb, arena_ty, None, 0, span)?;
        let values = arena.with(ir::Proj::Field(fv));
        let pos = if self.lang(index_ty) == Some(Lang::Handle) {
            let u = self.u32_ty();
            let (hi, _) = self.cx.field(self.mb, index_ty, None, 0, span)?;
            let (hg, _) = self.cx.field(self.mb, index_ty, None, 1, span)?;
            let slot_index = self.value(u, ir::Expr::Extract(index, hi));
            let generation = self.value(u, ir::Expr::Extract(index, hg));
            let (fs, slots_ty) = self.cx.field(self.mb, arena_ty, None, 1, span)?;
            let TyKind::Adt(_, sargs) = self.types().kind(slots_ty).clone() else { return None };
            let slot_ty = sargs[0];
            let slot =
                self.vec_elem(arena.with(ir::Proj::Field(fs)), slots_ty, slot_ty, slot_index)?;
            let (sg, _) = self.cx.field(self.mb, slot_ty, None, 0, span)?;
            let (sa, _) = self.cx.field(self.mb, slot_ty, None, 1, span)?;
            let live = self.load(slot.with(ir::Proj::Field(sg)), u);
            let b = self.mb.m.types.bool();
            let stale = self.value(b, ir::Expr::Binary(ir::BinOp::Ne, live, generation));
            self.panic_if(stale, "a stale handle: its value was removed from the arena");
            self.load(slot.with(ir::Proj::Field(sa)), u)
        } else {
            index
        };
        self.vec_elem(values, values_ty, elem, pos)
    }

    // ---- moves, drops and clones -----------------------------------------------------------

    /// A move out of `p`, after it's read: zeros there, so its owner's drop finds nothing. A
    /// whole temporary passed on isn't dropped at all, so it needs none.
    pub fn moved(&mut self, p: &mir::Place, kind: MoveKind) {
        if self.is_gpu() || (kind == MoveKind::Temp && p.proj.is_empty()) {
            return;
        }
        let t = self.place_src_ty(p);
        if !self.cx.needs_drop(t) {
            return;
        }
        let Some(ty) = self.cx.lower_ty(self.mb, t, self.code.span) else { return };
        let Some(place) = self.place(p) else { return };
        let z = self.value(ty, ir::Expr::Zero(ty));
        self.emit(ir::Stmt::Store(place, z));
    }

    /// Drops the value at `p` (MIR's `Drop`), if its type needs it.
    pub fn drop_place(&mut self, p: &mir::Place) {
        if self.is_gpu() {
            return;
        }
        let t = self.place_src_ty(p);
        if !self.cx.needs_drop(t) || self.cx.lower_ty(self.mb, t, self.code.span).is_none() {
            return;
        }
        let Some(place) = self.place(p) else { return };
        self.glue_call(GlueKind::Drop, t, vec![place]);
    }

    /// Calls the glue of `kind` for `t`.
    pub fn glue_call(
        &mut self,
        kind: GlueKind,
        t: TyId,
        places: Vec<ir::Place>,
    ) -> Option<ir::ValueId> {
        let key = InstanceKey::Glue { kind, ty: t };
        let callee = self.cx.instance(self.mb, key, Some((self.id, self.code.span)));
        self.emit_call(callee, places.into_iter().map(ir::Arg::Place).collect())
    }

    /// `x.clone()`: a copy of a `Copy` value, else the type's clone glue.
    pub fn clone_arg(&mut self, a: &mir::Arg) -> Option<ir::ValueId> {
        let p = a.place()?;
        let t = self.place_src_ty(p);
        if self.cx.is_copy(t) || self.is_gpu() {
            return self.read(p);
        }
        self.cx.lower_ty(self.mb, t, self.code.span)?;
        let place = self.place_or_copy(p)?;
        self.glue_call(GlueKind::Clone, t, vec![place])
    }

    /// `x.clone_into(mut y)`.
    pub fn clone_into_args(&mut self, src: &mir::Arg, dst: &mir::Arg) {
        let (Some(sp), Some(dp)) = (src.place(), dst.place()) else { return };
        let t = self.place_src_ty(sp);
        if self.cx.lower_ty(self.mb, t, self.code.span).is_none() {
            return;
        }
        if self.cx.is_copy(t) {
            if let Some(v) = self.read(sp) {
                self.store_mir(dp, v);
            }
            return;
        }
        let (Some(s), Some(d)) = (self.place_or_copy(sp), self.place(dp)) else { return };
        self.glue_call(GlueKind::CloneInto, t, vec![s, d]);
    }

    // ---- intrinsics ------------------------------------------------------------------------

    /// A call of one of std's CPU intrinsics (`std::mem`, `std::string`, `swap`, `replace`):
    /// `Some(result)`, or `None` if `lang` isn't one of them.
    pub fn mem_intrinsic(
        &mut self,
        lang: Lang,
        substs: &[TyId],
        c: &mir::Call,
        ty: Option<TyId>,
    ) -> Option<Option<ir::ValueId>> {
        use ir::MemOp as M;
        let span = c.span;
        let u = self.u32_ty();
        let elem = |fl: &mut Self, k: usize| fl.cx.lower_ty(fl.mb, *substs.get(k)?, span);
        let arg = |fl: &mut Self, k: usize| fl.arg_value(c.args.get(k)?);
        let mem = |fl: &mut Self, op: M, n: usize, t: Option<ir::TypeId>| -> Option<ir::ValueId> {
            let args: Option<Vec<ir::ValueId>> = (0..n).map(|k| fl.arg_value(&c.args[k])).collect();
            let e = ir::Expr::Mem(op, args?);
            match t {
                Some(t) => Some(fl.value(t, e)),
                None => {
                    fl.emit(ir::Stmt::Eval(e));
                    None
                }
            }
        };
        use Lang as L;
        let raw = matches!(
            lang,
            L::MemRead
                | L::MemWrite
                | L::MemDropAt
                | L::MemAt
                | L::MemAtMut
                | L::MemAtMutPair
                | L::MemHeapBase
                | L::MemPages
                | L::MemGrow
                | L::MemLoadU32
                | L::MemStoreU32
                | L::MemLoadU8
                | L::MemStoreU8
                | L::MemCopy
                | L::MemFill
                | L::MemCas
                | L::MemAtomicAdd
                | L::MemAtomicLoad
                | L::MemAtomicStore
                | L::MemWait
                | L::MemNotify
                | L::MemRunTask
                | L::MemTask
                | L::MemThreadBlock
                | L::MemWaitFor
                | L::StartVoice
                | L::StartTicker
                | L::MemAbort
                | L::StrAddr
                | L::StrLen
                | L::StrPart
        );
        let known = raw
            || matches!(
                lang,
                L::MemSizeOf
                    | L::MemAlignOf
                    | L::MemZeroed
                    | L::MemNeedsDrop
                    | L::Swap
                    | L::Replace
                    | L::DebugBuild
                    | L::TestBuild
            );
        if !known {
            return None;
        }
        if self.is_gpu() && raw {
            let name = self.cx.checked.program.func(match &c.callee {
                mir::Callee::Fn { func, .. } => *func,
                _ => return Some(None),
            });
            self.cx.err(Diagnostic::new(
                codes::E0600,
                span,
                format!("GPU code can't use raw memory: `{}` is CPU only", name.name),
            ));
            return Some(None);
        }
        let out = match lang {
            Lang::MemSizeOf => {
                let n = elem(self, 0).map_or(0, |e| ir::layout::array_stride(&self.mb.m.types, e));
                Some(self.u32c(n))
            }
            Lang::MemAlignOf => {
                let n = elem(self, 0).map_or(1, |e| ir::layout::layout(&self.mb.m.types, e).align);
                Some(self.u32c(n))
            }
            Lang::MemZeroed => {
                let t = elem(self, 0)?;
                Some(self.value(t, ir::Expr::Zero(t)))
            }
            Lang::MemNeedsDrop => {
                let d = self.cx.needs_drop(substs[0]);
                Some(self.konst(ir::Const::Bool(d)))
            }
            Lang::MemRead => {
                let Some(et) = elem(self, 0) else { return Some(None) };
                let addr = arg(self, 0)?;
                let place = self.ptr_place(et, addr);
                Some(self.load(place, et))
            }
            Lang::MemWrite => {
                let addr = arg(self, 0)?;
                let v = arg(self, 1);
                if let (Some(et), Some(v)) = (elem(self, 0), v) {
                    let place = self.ptr_place(et, addr);
                    self.emit(ir::Stmt::Store(place, v));
                }
                None
            }
            Lang::MemDropAt => {
                let addr = arg(self, 0)?;
                if self.cx.needs_drop(substs[0])
                    && let Some(et) = elem(self, 0)
                {
                    let place = self.ptr_place(et, addr);
                    self.glue_call(GlueKind::Drop, substs[0], vec![place]);
                }
                None
            }
            Lang::MemAt | Lang::MemAtMut => {
                let et = elem(self, 0)?;
                let addr = arg(self, 1)?;
                let pt = self.mb.m.types.intern(ir::TypeDef::Ptr(et));
                Some(self.value(pt, ir::Expr::Mem(M::Ptr, vec![addr])))
            }
            Lang::MemAtMutPair => {
                // A `Pair<T>`: a pointer to each.
                let et = elem(self, 0)?;
                let (a, b) = (arg(self, 1)?, arg(self, 2)?);
                let pt = self.mb.m.types.intern(ir::TypeDef::Ptr(et));
                let pa = self.value(pt, ir::Expr::Mem(M::Ptr, vec![a]));
                let pb = self.value(pt, ir::Expr::Mem(M::Ptr, vec![b]));
                let t = self.ty(ty?, span)?;
                Some(self.value(t, ir::Expr::Construct(t, vec![pa, pb])))
            }
            Lang::MemHeapBase => mem(self, M::HeapBase, 0, Some(u)),
            Lang::MemPages => mem(self, M::Pages, 0, Some(u)),
            Lang::MemGrow => mem(self, M::Grow, 1, Some(u)),
            Lang::MemLoadU32 => mem(self, M::Load(ir::Scalar::U32), 1, Some(u)),
            Lang::MemStoreU32 => mem(self, M::Store(ir::Scalar::U32), 2, None),
            Lang::MemLoadU8 => {
                let u8 = self.mb.m.types.scalar(ir::Scalar::U8);
                mem(self, M::Load(ir::Scalar::U8), 1, Some(u8))
            }
            Lang::MemStoreU8 => mem(self, M::Store(ir::Scalar::U8), 2, None),
            Lang::MemCopy => mem(self, M::Copy, 3, None),
            Lang::MemFill => mem(self, M::Fill, 3, None),
            Lang::MemCas => mem(self, M::Cas, 3, Some(u)),
            Lang::MemAtomicAdd => mem(self, M::AtomicAdd, 2, Some(u)),
            Lang::MemAtomicLoad => mem(self, M::AtomicLoad, 1, Some(u)),
            Lang::MemAtomicStore => mem(self, M::AtomicStore, 2, None),
            Lang::MemWait => mem(self, M::Wait, 2, Some(u)),
            Lang::MemNotify => mem(self, M::Notify, 2, Some(u)),
            Lang::MemRunTask => mem(self, M::RunTask, 3, None),
            Lang::MemThreadBlock => mem(self, M::ThreadBlock, 0, Some(u)),
            Lang::MemWaitFor => mem(self, M::WaitFor, 3, Some(u)),
            Lang::MemTask => crate::task::task(self, substs, c),
            Lang::StartVoice | Lang::StartTicker => {
                let (op, n) = if lang == Lang::StartVoice {
                    self.mb.m.audio = true;
                    (ir::HostOp::Audio, 2)
                } else {
                    self.mb.m.tick = true;
                    (ir::HostOp::Tick, 3)
                };
                let args: Option<Vec<ir::ValueId>> = (0..n).map(|k| arg(self, k)).collect();
                self.emit(ir::Stmt::Eval(ir::Expr::Host(op, args?)));
                None
            }
            Lang::DebugBuild => Some(self.konst(ir::Const::Bool(self.cx.data.debug))),
            Lang::TestBuild => Some(self.konst(ir::Const::Bool(self.cx.data.testing))),
            Lang::MemAbort => {
                // A trap that keeps the panic message a worker left.
                self.emit(ir::Stmt::Trap);
                None
            }
            Lang::StrAddr | Lang::StrLen => {
                let run = self.str_arg(&c.args[0])?;
                let k = u32::from(lang == Lang::StrLen);
                Some(self.value(u, ir::Expr::Extract(run, k)))
            }
            Lang::StrPart => {
                let run = self.str_arg(&c.args[0])?;
                let from = arg(self, 1)?;
                let to = arg(self, 2)?;
                let addr = self.value(u, ir::Expr::Extract(run, 0));
                let start = self.value(u, ir::Expr::Binary(ir::BinOp::Add, addr, from));
                let len = self.value(u, ir::Expr::Binary(ir::BinOp::Sub, to, from));
                let rt = self.f.value_ty(run);
                Some(self.value(rt, ir::Expr::Construct(rt, vec![start, len])))
            }
            Lang::Swap => {
                let (Some(a), Some(b)) = (c.args[0].place(), c.args[1].place()) else {
                    return Some(None);
                };
                let (va, vb) = (self.read(a), self.read(b));
                if let (Some(va), Some(vb)) = (va, vb) {
                    self.store_mir(a, vb);
                    self.store_mir(b, va);
                }
                None
            }
            Lang::Replace => {
                let p = c.args[0].place()?;
                let old = self.read(p);
                if let Some(v) = arg(self, 1) {
                    self.store_mir(p, v);
                }
                old
            }
            _ => return None,
        };
        Some(out)
    }

    /// A place at `addr`, holding an `et`.
    fn ptr_place(&mut self, et: ir::TypeId, addr: ir::ValueId) -> ir::Place {
        let pt = self.mb.m.types.intern(ir::TypeDef::Ptr(et));
        let ptr = self.value(pt, ir::Expr::Mem(ir::MemOp::Ptr, vec![addr]));
        ir::Place::root(ir::PlaceRoot::Ptr(ptr))
    }

    /// A `str` argument's run (a `Text` or `String` passes as one).
    pub(crate) fn str_arg(&mut self, a: &mir::Arg) -> Option<ir::ValueId> {
        let u8 = self.mb.m.types.scalar(ir::Scalar::U8);
        let rt = self.mb.m.types.intern(ir::TypeDef::Run(u8));
        match a.place() {
            Some(p) => self.run_of(p, rt),
            None => self.arg_value(a),
        }
    }

    /// `panic(message)`: the message for the host, then a trap.
    pub fn panic_call(&mut self, msg: &mir::Arg) {
        let Some(run) = self.str_arg(msg) else { return };
        let u = self.u32_ty();
        let addr = self.value(u, ir::Expr::Extract(run, 0));
        let len = self.value(u, ir::Expr::Extract(run, 1));
        self.emit(ir::Stmt::Eval(ir::Expr::Mem(ir::MemOp::Panic, vec![addr, len])));
    }

    /// `assert(cond, message)`, with the operands of its comparison after the message (`shown`).
    /// Where a failure is explained, those whose types implement `Format` are shown with their
    /// source: std's `assert_failed1` or `assert_failed2` panics.
    pub fn assert_call(&mut self, cond: &mir::Arg, msg: &mir::Arg, shown: &[mir::Arg], span: Span) {
        let Some(c) = self.arg_value(cond) else { return };
        let b = self.mb.m.types.bool();
        let not = self.value(b, ir::Expr::Unary(ir::UnOp::Not, c));
        self.push_block();
        let operands = if self.cx.explain { self.showable(shown) } else { Vec::new() };
        let helper = match operands.len() {
            1 => self.cx.checked.program.lang_fn(Lang::AssertFailed1),
            2 => self.cx.checked.program.lang_fn(Lang::AssertFailed2),
            _ => None,
        };
        match helper {
            Some(f) => self.assert_failed(f, msg, &operands, span),
            None => self.panic_call(msg),
        }
        let then = self.pop_block();
        self.emit(ir::Stmt::If { cond: not, then, else_: Vec::new() });
    }

    /// The operands of a failed `assert` that can be shown: each with its type, its source, and
    /// whether it's text (which is quoted).
    fn showable<'m>(&mut self, shown: &'m [mir::Arg]) -> Vec<(&'m mir::Arg, TyId, String, bool)> {
        let mut out = Vec::new();
        for a in shown {
            let ty = match a {
                mir::Arg::Borrow(pl, _) | mir::Arg::Mut(pl, _) => self.place_src_ty(pl),
                mir::Arg::Take(o) => self.concrete(o.ty),
            };
            let program = &self.cx.checked.program;
            let Some(source) = program.text(a.span()) else { continue };
            let Some(format) = program.lang_trait(Lang::Format) else { continue };
            let r = wrela_sema::defs::TraitRef { trait_: format, args: Vec::new() };
            if !wrela_sema::traits::implements(program, ty, &r) {
                continue;
            }
            let text = match program.types.kind(ty) {
                TyKind::Str => true,
                TyKind::Adt(a, _) => {
                    program.is_lang_adt(*a, Lang::Text) || program.is_lang_adt(*a, Lang::String)
                }
                _ => false,
            };
            out.push((a, ty, source.to_string(), text));
        }
        out
    }

    /// Calls `f`, std's `assert_failed1` or `assert_failed2`, which panics: with the message,
    /// each operand's source and value, and which are text. Each argument is passed as the
    /// parameter it goes to asks; the sources are text the build holds only here.
    fn assert_failed(
        &mut self,
        f: FnId,
        msg: &mir::Arg,
        operands: &[(&mir::Arg, TyId, String, bool)],
        span: Span,
    ) {
        let substs: Vec<TyId> = operands.iter().map(|o| o.1).collect();
        let quoted = operands.iter().enumerate().fold(0, |q, (i, o)| q | (u32::from(o.3) << i));
        let program = &self.cx.checked.program;
        let def = program.func(f);
        let subst = Subst::from_pairs(&program.fn_all_generics(f), &substs);
        let u8 = self.mb.m.types.scalar(ir::Scalar::U8);
        let run_t = self.mb.m.types.intern(ir::TypeDef::Run(u8));
        // Each parameter's value: the message, each operand's source then value, `quoted`.
        let mut values = vec![self.str_arg(msg)];
        for (a, _, source, _) in operands {
            let (addr, len) = self.text_parts(source);
            values.push(Some(self.value(run_t, ir::Expr::Construct(run_t, vec![addr, len]))));
            values.push(match a.place() {
                Some(pl) => self.read(pl),
                None => self.arg_value(a),
            });
        }
        values.push(Some(self.u32c(quoted)));
        let mut args = Vec::new();
        for (p, v) in def.params.clone().iter().zip(values) {
            let pt = self.cx.concrete(p.ty, &subst);
            let (Some(ty), Some(v)) = (self.cx.lower_ty(self.mb, pt, p.span), v) else { return };
            let (by_ref, _) = crate::ty::param_passing(
                self.mb.target(),
                &self.mb.m.types,
                ty,
                p.mode,
                def.ret_mode,
            );
            args.push(if by_ref { ir::Arg::Place(self.temp_place(v)) } else { ir::Arg::Value(v) });
        }
        let key = InstanceKey::Fn { func: f, substs, callables: Vec::new(), resources: Vec::new() };
        let callee = self.cx.instance(self.mb, key, Some((self.id, span)));
        self.emit_call(callee, args);
        self.emit(ir::Stmt::Trap);
    }
}
