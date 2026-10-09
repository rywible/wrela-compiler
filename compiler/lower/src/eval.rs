//! Build-time constants (language.md §10). A constant whose value isn't literal is computed by
//! the build: it lowers the function that computes it ([`FnOwner::Const`]) and everything that
//! calls into a CPU module of its own, runs that in wasmtime, and reads the value back out of
//! the module's memory as a [`Value`]. The program's own module then lays each value out as
//! read-only data ([`Cx::place_value`]), with the data its pointers point to beside it.
//!
//! ```text
//!  const fn ──lower_consts──▶ eval module ──wasmtime──▶ memory ──read_value──▶ Value
//!                                                                               │
//!  program module ◀──────────────── Data + ConstValue::Addr ◀──place_value──────┘
//! ```
//!
//! [`FnOwner::Const`]: wrela_sema::defs::FnOwner::Const

use crate::instance::InstanceKey;
use crate::{Cx, ModuleBuilder};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use wrela_diag::{Diagnostic, Span};
use wrela_ir as ir;
use wrela_sema::Checked;
use wrela_sema::defs::Lang;
use wrela_sema::ty::{ConstId, FnId, TyId, TyKind};

/// A constant's value as the build computed it, shaped like its IR type ([`ir::ConstValue`]'s
/// parts), with what its pointers point to inside it, so any module can lay it out.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Scalar(ir::Const),
    Parts(Vec<Value>),
    Variant(u32, Option<Box<Value>>),
    /// A `u32` that is the address of these values, one after another: a `Vec`'s elements, a
    /// `Box`'s value. With none, the address is 0.
    Points(Vec<Value>),
    /// A `u32` that is the address of these bytes: a `Text`'s, a `Vec<u8>`'s.
    Bytes(Vec<u8>),
}

/// The values of the constants the build has computed.
pub type Values = BTreeMap<ConstId, Rc<Value>>;

/// The files `embed` reads, by path.
pub type Embeds = BTreeMap<std::sync::Arc<str>, std::sync::Arc<[u8]>>;

/// What the build knows besides the program's code: the values of the constants it has
/// computed, and the files `embed` reads.
#[derive(Default)]
pub struct BuildData {
    pub values: Values,
    pub embeds: Embeds,
    /// A debug build: it pays for checks a release build doesn't make (language.md §11).
    pub debug: bool,
    /// A test build: it has the program's `@testing` exports, and `test_build()` is true (§9).
    pub testing: bool,
    /// A build whose WASM uses no SIMD (`wrela_wasm::Options::simd`): its results are the same,
    /// bit for bit, which a test checks.
    pub no_simd: bool,
    /// A lifted build's literals (language.md §22).
    pub lift: Option<crate::LiftTable>,
}

/// The module the build runs to compute some constants.
pub struct ConstModule {
    pub module: ir::Module,
    /// Each constant, and the export that computes it: it returns the address of the value in
    /// memory (0 for a value with no runtime parts).
    pub exports: Vec<(ConstId, String)>,
}

/// What lowering the constants' module found: the module, or the constants it needs that
/// aren't computed yet.
pub enum ConstLowering {
    Ready(ConstModule),
    Needs(BTreeSet<ConstId>),
}

/// Whether the build computes constant `c`: its value isn't literal (the checker built the
/// function that computes it).
pub fn is_computed(checked: &Checked, c: ConstId) -> bool {
    checked.mir.contains_key(&checked.program.const_(c).eval)
}

/// Lowers a module that computes constants `cs`, given the values of those already computed.
/// Errors lowering finds are returned with it; only a module without them is run.
pub fn lower_consts(
    checked: &Checked,
    cs: &[ConstId],
    data: &BuildData,
) -> (ConstLowering, Vec<Diagnostic>) {
    let mut cx = Cx::new(checked, data, true);
    cx.explain = true;
    let mut mb = ModuleBuilder::default();
    let mut exports = Vec::new();
    for (i, &c) in cs.iter().enumerate() {
        let def = checked.program.const_(c);
        let f = def.eval;
        let callee = cx.instance(&mut mb, InstanceKey::plain(f, Vec::new()), None);
        let ret = checked.program.func(f).ret;
        let ret_ir = cx.lower_ty(&mut mb, ret, def.span);
        let u32 = mb.m.types.u32();
        let mut func = ir::Function::new(format!("const {}", def.name), Vec::new(), Some(u32));
        let mut body = vec![ir::Stmt::At(def.span)];
        let addr = match ret_ir {
            Some(t) => {
                // The value goes in data of its own, where the build reads it.
                let value = zero(&mb.m.types, t);
                let d = mb.m.add_data(ir::Data { name: "result".into(), ty: t, value });
                let v = func.new_value(t);
                body.push(ir::Stmt::Let(v, ir::Expr::Call(callee, Vec::new())));
                let pt = mb.m.types.intern(ir::TypeDef::Ptr(t));
                let p = func.new_value(pt);
                let at = ir::Place::root(ir::PlaceRoot::Data(d));
                body.push(ir::Stmt::Let(p, ir::Expr::Addr(at)));
                body.push(ir::Stmt::Store(ir::Place::root(ir::PlaceRoot::Ptr(p)), v));
                let a = func.new_value(u32);
                body.push(ir::Stmt::Let(a, ir::Expr::Mem(ir::MemOp::Addr, vec![p])));
                a
            }
            None => {
                body.push(ir::Stmt::Eval(ir::Expr::Call(callee, Vec::new())));
                let a = func.new_value(u32);
                body.push(ir::Stmt::Let(a, ir::Expr::Const(ir::Const::U32(0))));
                a
            }
        };
        body.push(ir::Stmt::Return(Some(addr)));
        func.body = body;
        let id = mb.m.add_function(func);
        let name = format!("const{i}");
        mb.m.exports.push((name.clone(), id));
        exports.push((c, name));
    }
    cx.drain(&mut mb);
    crate::rewrite_cpu_math(&mut cx, &mut mb);
    cx.drain(&mut mb);
    if !cx.missing.is_empty() {
        return (ConstLowering::Needs(cx.missing), Vec::new());
    }
    if !wrela_diag::has_errors(&cx.diags)
        && let Err(e) = ir::verify(&mb.m)
    {
        cx.err(Diagnostic::internal(format!("the constants' module's IR is malformed: {e}")));
    }
    let mut diags = cx.diags;
    wrela_diag::sort_and_dedup(&mut diags);
    (ConstLowering::Ready(ConstModule { module: mb.m, exports }), diags)
}

/// A module whose exports run tests (§10): each of `tests`, a `@test` function, called with
/// nothing; each export returns 0. The constants they read are computed (in `data`).
pub struct TestModule {
    pub module: ir::Module,
    pub exports: Vec<(FnId, String)>,
}

/// Lowers the module that runs `tests`, with the errors lowering found: the module only when
/// there are none.
pub fn lower_tests(
    checked: &Checked,
    tests: &[FnId],
    data: &BuildData,
) -> (Option<TestModule>, Vec<Diagnostic>) {
    let mut cx = Cx::new(checked, data, true);
    cx.explain = true;
    let mut mb = ModuleBuilder::default();
    let mut exports = Vec::new();
    for (i, &f) in tests.iter().enumerate() {
        let def = checked.program.func(f);
        let callee = cx.instance(&mut mb, InstanceKey::plain(f, Vec::new()), None);
        let u32 = mb.m.types.u32();
        let mut func = ir::Function::new(format!("test {}", def.name), Vec::new(), Some(u32));
        let zero = func.new_value(u32);
        func.body = vec![
            ir::Stmt::At(def.span),
            ir::Stmt::Eval(ir::Expr::Call(callee, Vec::new())),
            ir::Stmt::Let(zero, ir::Expr::Const(ir::Const::U32(0))),
            ir::Stmt::Return(Some(zero)),
        ];
        let id = mb.m.add_function(func);
        let name = format!("test{i}");
        mb.m.exports.push((name.clone(), id));
        exports.push((f, name));
    }
    cx.drain(&mut mb);
    crate::rewrite_cpu_math(&mut cx, &mut mb);
    cx.drain(&mut mb);
    if data.debug {
        // A debug build's NaN checks (§11), made by the back end.
        mb.m.nan_message = Some(cx.text_data(&mut mb, crate::NAN_MESSAGE));
    }
    if !cx.missing.is_empty() {
        cx.err(Diagnostic::internal("a test reads a constant the build didn't compute"));
    }
    if !wrela_diag::has_errors(&cx.diags)
        && let Err(e) = ir::verify(&mb.m)
    {
        cx.err(Diagnostic::internal(format!("the tests' module's IR is malformed: {e}")));
    }
    let mut diags = cx.diags;
    wrela_diag::sort_and_dedup(&mut diags);
    let module = (!wrela_diag::has_errors(&diags)).then_some(TestModule { module: mb.m, exports });
    (module, diags)
}

/// A zero of scalar type `s`.
pub(crate) fn zero_scalar(s: ir::Scalar) -> ir::Const {
    match s {
        ir::Scalar::Bool => ir::Const::Bool(false),
        ir::Scalar::I32 => ir::Const::I32(0),
        ir::Scalar::U32 => ir::Const::U32(0),
        ir::Scalar::I64 => ir::Const::I64(0),
        ir::Scalar::U64 => ir::Const::U64(0),
        ir::Scalar::F32 => ir::Const::F32(0.0),
        ir::Scalar::F64 => ir::Const::F64(0.0),
        s => ir::Const::Small(s, 0),
    }
}

/// A zero of IR type `t`, as constant data.
pub(crate) fn zero(types: &ir::Types, t: ir::TypeId) -> ir::ConstValue {
    use ir::ConstValue as V;
    match types.get(t) {
        &ir::TypeDef::Scalar(s) | &ir::TypeDef::Atomic(s) => V::Scalar(zero_scalar(s)),
        &ir::TypeDef::Vector(s, n) => V::Parts(vec![V::Scalar(zero_scalar(s)); n as usize]),
        &ir::TypeDef::Matrix(c, r) => {
            let col = V::Parts(vec![V::Scalar(zero_scalar(ir::Scalar::F32)); r as usize]);
            V::Parts(vec![col; c as usize])
        }
        &ir::TypeDef::Array(e, n) => V::Parts(vec![zero(types, e); n as usize]),
        ir::TypeDef::Struct { fields, .. } => {
            V::Parts(fields.iter().map(|&(_, f)| zero(types, f)).collect())
        }
        ir::TypeDef::Enum { variants, .. } => {
            V::Variant(0, variants[0].1.map(|p| Box::new(zero(types, p))))
        }
        ir::TypeDef::Run(_) | ir::TypeDef::RuntimeArray(_) | ir::TypeDef::Ptr(_) => {
            V::Scalar(ir::Const::U32(0))
        }
    }
}

/// The memory a constant's value is read from, and where.
pub struct Memory<'m> {
    pub bytes: &'m [u8],
}

impl Memory<'_> {
    fn get(&self, at: u32, n: u32) -> Result<&[u8], String> {
        let end = at.checked_add(n).ok_or("an address past the end of memory")?;
        self.bytes
            .get(at as usize..end as usize)
            .ok_or_else(|| format!("the value points past the end of memory, at {at}"))
    }

    fn u32(&self, at: u32) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.get(at, 4)?.try_into().expect("4 bytes")))
    }
}

/// Reads constant `c`'s value out of the memory of the module that computed it, at `addr`.
pub fn read_value(checked: &Checked, c: ConstId, mem: &Memory, addr: u32) -> Result<Value, String> {
    let empty = BuildData::default();
    let mut cx = Cx::new(checked, &empty, true);
    let mut mb = ModuleBuilder::default();
    let def = checked.program.const_(c);
    let t = checked.program.func(def.eval).ret;
    match cx.lower_ty(&mut mb, t, def.span) {
        Some(it) => cx.read(&mut mb, t, it, mem, addr, def.span),
        None => Ok(Value::Parts(Vec::new())),
    }
}

impl Cx<'_> {
    /// The pointed-to type of a `Vec<T>` or `Box<T>`, and which: `Some(true)` for a `Vec`.
    fn owner_of(&self, t: TyId) -> Option<(bool, TyId)> {
        let p = &self.checked.program;
        match (p.lang_of_ty(t), p.types.kind(t)) {
            (Some(Lang::Vec), TyKind::Adt(_, args)) => Some((true, args[0])),
            (Some(Lang::Box), TyKind::Adt(_, args)) => Some((false, args[0])),
            _ => None,
        }
    }

    /// Whether `t` is a `Text` or `Bytes`: an address and a length of bytes in the build.
    fn is_text(&self, t: TyId) -> bool {
        matches!(self.checked.program.lang_of_ty(t), Some(Lang::Text | Lang::Bytes))
    }

    /// Reads a value of type `t` (IR type `it`) at `at`.
    fn read(
        &mut self,
        mb: &mut ModuleBuilder,
        t: TyId,
        it: ir::TypeId,
        mem: &Memory,
        at: u32,
        span: Span,
    ) -> Result<Value, String> {
        use ir::layout::{array_stride, column_stride, field_offsets};
        let t = self.checked.reveal(t);
        let offsets = field_offsets(&mb.m.types, it).to_vec();
        if let Some((is_vec, elem)) = self.owner_of(t) {
            let data = mem.u32(at + offsets[0])?;
            let len = if is_vec { mem.u32(at + offsets[1])? } else { 1 };
            let pointee = match self.lower_ty(mb, elem, span) {
                None => Value::Points(Vec::new()),
                Some(e) if mb.m.types.get(e) == &ir::TypeDef::Scalar(ir::Scalar::U8) => {
                    Value::Bytes(mem.get(data, len)?.to_vec())
                }
                Some(e) => {
                    let stride = array_stride(&mb.m.types, e);
                    let mut out = Vec::with_capacity(len as usize);
                    for i in 0..len {
                        let a = i.checked_mul(stride).and_then(|o| data.checked_add(o));
                        let a = a.ok_or("an element past the end of memory")?;
                        out.push(self.read(mb, elem, e, mem, a, span)?);
                    }
                    Value::Points(out)
                }
            };
            if !is_vec {
                return Ok(Value::Parts(vec![pointee]));
            }
            // A constant's `Vec` holds exactly its elements: its capacity is its length.
            let n = Value::Scalar(ir::Const::U32(len));
            return Ok(Value::Parts(vec![pointee, n.clone(), n]));
        }
        if self.is_text(t) {
            let data = mem.u32(at + offsets[0])?;
            let len = mem.u32(at + offsets[1])?;
            let bytes = mem.get(data, len)?.to_vec();
            return Ok(Value::Parts(vec![Value::Bytes(bytes), Value::Scalar(ir::Const::U32(len))]));
        }
        let def = mb.m.types.get(it).clone();
        Ok(match def {
            ir::TypeDef::Scalar(s) | ir::TypeDef::Atomic(s) => {
                Value::Scalar(read_scalar(mem, s, at)?)
            }
            ir::TypeDef::Vector(s, n) => Value::Parts(
                (0..u32::from(n))
                    .map(|k| read_scalar(mem, s, at + s.bytes() * k).map(Value::Scalar))
                    .collect::<Result<_, _>>()?,
            ),
            ir::TypeDef::Matrix(columns, rows) => {
                let mut cols = Vec::new();
                for c in 0..u32::from(columns) {
                    let base = at + column_stride(rows) * c;
                    let col = (0..u32::from(rows))
                        .map(|k| read_scalar(mem, ir::Scalar::F32, base + 4 * k).map(Value::Scalar))
                        .collect::<Result<_, _>>()?;
                    cols.push(Value::Parts(col));
                }
                Value::Parts(cols)
            }
            ir::TypeDef::Array(e, n) => {
                let elem = match self.checked.program.types.kind(t) {
                    TyKind::Array(x, _) | TyKind::ArrayN(x, _) => *x,
                    _ => return Err(format!("an array of type {}", self.show(t))),
                };
                let stride = array_stride(&mb.m.types, e);
                let mut out = Vec::with_capacity(n as usize);
                for i in 0..n {
                    out.push(self.read(mb, elem, e, mem, at + i * stride, span)?);
                }
                Value::Parts(out)
            }
            ir::TypeDef::Struct { fields, .. } => {
                let mut out = Vec::new();
                for (k, ft) in self.field_map(mb, t, None, span) {
                    let Some(k) = k else { continue };
                    let (_, fit) = fields[k as usize];
                    out.push(self.read(mb, ft, fit, mem, at + offsets[k as usize], span)?);
                }
                Value::Parts(out)
            }
            ir::TypeDef::Enum { variants, .. } => {
                // Memory holds the variant's tag, its discriminant: its variant, by index.
                let tag = mem.u32(at + offsets[0])?;
                let variant = mb.m.types.variant_of(it, tag);
                let Some((k, (_, payload))) =
                    variant.and_then(|k| Some((k, variants.get(k as usize)?)))
                else {
                    return Err(format!("an enum `{}` with the tag {tag}", self.show(t)));
                };
                let payload = match payload {
                    Some(pt) => {
                        let base = at + offsets[1 + k as usize];
                        let poffsets = field_offsets(&mb.m.types, *pt).to_vec();
                        let pfields = match mb.m.types.get(*pt) {
                            ir::TypeDef::Struct { fields, .. } => fields.clone(),
                            _ => Vec::new(),
                        };
                        let mut out = Vec::new();
                        for (j, ft) in self.field_map(mb, t, Some(k), span) {
                            let Some(j) = j else { continue };
                            let (_, fit) = pfields[j as usize];
                            out.push(self.read(
                                mb,
                                ft,
                                fit,
                                mem,
                                base + poffsets[j as usize],
                                span,
                            )?);
                        }
                        Some(Box::new(Value::Parts(out)))
                    }
                    None => None,
                };
                Value::Variant(k, payload)
            }
            ir::TypeDef::Run(_) | ir::TypeDef::RuntimeArray(_) | ir::TypeDef::Ptr(_) => {
                return Err(format!("a value of type {} points into memory", self.show(t)));
            }
        })
    }

    fn show(&self, t: TyId) -> String {
        self.checked.program.display_ty(t)
    }

    /// A computed constant's value as constant data in this module: what it points to is laid
    /// out as data of its own, and the pointer is that data's address.
    pub fn place_value(
        &mut self,
        mb: &mut ModuleBuilder,
        t: TyId,
        it: ir::TypeId,
        v: &Value,
        span: Span,
    ) -> Option<ir::ConstValue> {
        let t = self.checked.reveal(t);
        if let Some((is_vec, elem)) = self.owner_of(t) {
            let Value::Parts(ps) = v else { return None };
            let data = match ps.first()? {
                Value::Points(xs) if xs.is_empty() => ir::ConstValue::Scalar(ir::Const::U32(0)),
                Value::Points(xs) => {
                    let e = self.lower_ty(mb, elem, span)?;
                    let mut parts = Vec::with_capacity(xs.len());
                    for x in xs {
                        parts.push(self.place_value(mb, elem, e, x, span)?);
                    }
                    let (ty, value) = if is_vec {
                        let n = parts.len() as u32;
                        (mb.m.types.intern(ir::TypeDef::Array(e, n)), ir::ConstValue::Parts(parts))
                    } else {
                        (e, parts.pop()?)
                    };
                    ir::ConstValue::Addr(mb.m.add_data(ir::Data {
                        name: "const".into(),
                        ty,
                        value,
                    }))
                }
                Value::Bytes(bs) if bs.is_empty() => ir::ConstValue::Scalar(ir::Const::U32(0)),
                Value::Bytes(bs) => ir::ConstValue::Addr(self.bytes_data(mb, bs)),
                _ => return None,
            };
            let mut out = vec![data];
            let (u32, u32_ir) = (self.checked.program.types.u32, mb.m.types.u32());
            for p in &ps[1..] {
                out.push(self.place_value(mb, u32, u32_ir, p, span)?);
            }
            return Some(ir::ConstValue::Parts(out));
        }
        if self.is_text(t) {
            let Value::Parts(ps) = v else { return None };
            let (Value::Bytes(bs), Value::Scalar(len)) = (ps.first()?, ps.get(1)?) else {
                return None;
            };
            let d = self.bytes_data(mb, bs);
            return Some(ir::ConstValue::Parts(vec![
                ir::ConstValue::Addr(d),
                ir::ConstValue::Scalar(len.clone()),
            ]));
        }
        let def = mb.m.types.get(it).clone();
        Some(match (def, v) {
            (ir::TypeDef::Scalar(_), Value::Scalar(c)) => ir::ConstValue::Scalar(c.clone()),
            (ir::TypeDef::Vector(..) | ir::TypeDef::Matrix(..), Value::Parts(_)) => to_plain(v)?,
            (ir::TypeDef::Array(e, _), Value::Parts(xs)) => {
                let elem = match self.checked.program.types.kind(t) {
                    TyKind::Array(x, _) | TyKind::ArrayN(x, _) => *x,
                    _ => return None,
                };
                let mut out = Vec::with_capacity(xs.len());
                for x in xs {
                    out.push(self.place_value(mb, elem, e, x, span)?);
                }
                ir::ConstValue::Parts(out)
            }
            (ir::TypeDef::Struct { fields, .. }, Value::Parts(xs)) => {
                let map: Vec<TyId> = self
                    .field_map(mb, t, None, span)
                    .into_iter()
                    .filter_map(|(k, ft)| k.map(|_| ft))
                    .collect();
                let mut out = Vec::with_capacity(xs.len());
                for ((x, ft), (_, fit)) in xs.iter().zip(map).zip(fields) {
                    out.push(self.place_value(mb, ft, fit, x, span)?);
                }
                ir::ConstValue::Parts(out)
            }
            (ir::TypeDef::Enum { variants, .. }, Value::Variant(k, payload)) => {
                let payload = match (payload, variants.get(*k as usize)?.1) {
                    (Some(p), Some(pt)) => {
                        let Value::Parts(xs) = p.as_ref() else { return None };
                        let pfields = match mb.m.types.get(pt) {
                            ir::TypeDef::Struct { fields, .. } => fields.clone(),
                            _ => return None,
                        };
                        let map: Vec<TyId> = self
                            .field_map(mb, t, Some(*k), span)
                            .into_iter()
                            .filter_map(|(k, ft)| k.map(|_| ft))
                            .collect();
                        let mut out = Vec::with_capacity(xs.len());
                        for ((x, ft), (_, fit)) in xs.iter().zip(map).zip(pfields) {
                            out.push(self.place_value(mb, ft, fit, x, span)?);
                        }
                        Some(Box::new(ir::ConstValue::Parts(out)))
                    }
                    _ => None,
                };
                ir::ConstValue::Variant(*k, payload)
            }
            _ => return None,
        })
    }

    /// Constant data holding `bytes`: text's, shared with string literals.
    pub fn bytes_data(&mut self, mb: &mut ModuleBuilder, bytes: &[u8]) -> ir::DataId {
        match std::str::from_utf8(bytes) {
            Ok(s) => self.text_data(mb, s).0,
            Err(_) => {
                let u8 = mb.m.types.scalar(ir::Scalar::U8);
                let ty = mb.m.types.intern(ir::TypeDef::Array(u8, bytes.len() as u32));
                let value = ir::ConstValue::Bytes(bytes.to_vec());
                mb.m.add_data(ir::Data { name: "bytes".into(), ty, value })
            }
        }
    }
}

/// A value with no pointers in it, as constant data.
fn to_plain(v: &Value) -> Option<ir::ConstValue> {
    Some(match v {
        Value::Scalar(c) => ir::ConstValue::Scalar(c.clone()),
        Value::Parts(xs) => ir::ConstValue::Parts(xs.iter().map(to_plain).collect::<Option<_>>()?),
        Value::Variant(k, p) => ir::ConstValue::Variant(
            *k,
            match p {
                Some(p) => Some(Box::new(to_plain(p)?)),
                None => None,
            },
        ),
        Value::Points(_) | Value::Bytes(_) => return None,
    })
}

/// The scalar of kind `s` at `at`, as memory holds it (`wrela_ir::layout`).
fn read_scalar(mem: &Memory, s: ir::Scalar, at: u32) -> Result<ir::Const, String> {
    let b = |n: u32| mem.get(at, n);
    Ok(match s {
        ir::Scalar::Bool => ir::Const::Bool(mem.u32(at)? != 0),
        ir::Scalar::I32 => ir::Const::I32(i32::from_le_bytes(b(4)?.try_into().expect("4"))),
        ir::Scalar::U32 => ir::Const::U32(mem.u32(at)?),
        ir::Scalar::I64 => ir::Const::I64(i64::from_le_bytes(b(8)?.try_into().expect("8"))),
        ir::Scalar::U64 => ir::Const::U64(u64::from_le_bytes(b(8)?.try_into().expect("8"))),
        ir::Scalar::F32 => ir::Const::F32(f32::from_le_bytes(b(4)?.try_into().expect("4"))),
        ir::Scalar::F64 => ir::Const::F64(f64::from_le_bytes(b(8)?.try_into().expect("8"))),
        ir::Scalar::I8 => ir::Const::Small(s, i64::from(b(1)?[0] as i8)),
        ir::Scalar::U8 => ir::Const::Small(s, i64::from(b(1)?[0])),
        ir::Scalar::I16 => {
            ir::Const::Small(s, i64::from(i16::from_le_bytes(b(2)?.try_into().expect("2"))))
        }
        ir::Scalar::U16 => {
            ir::Const::Small(s, i64::from(u16::from_le_bytes(b(2)?.try_into().expect("2"))))
        }
    })
}
