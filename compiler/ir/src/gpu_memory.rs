//! Data in GPU memory (language.md §6.10). A type that crosses to the GPU has the CPU's bytes
//! there, but WGSL's `bool` has no layout and WGSL has no unions. This pass rewrites a flattened
//! GPU module so that its memory holds:
//!
//! - a `bool` in a buffer, or a member of a type memory holds (a struct's field, an enum's
//!   payload, an array's element), as a `u32`, 0 or 1: the 4 bytes the CPU holds for one;
//! - an enum that memory holds as its `u32` tag and its payload's words:
//!   `struct { tag: u32, words: array<W, K> }`, `W` a `u32`, `vec2<u32>` or `vec4<u32>` as the
//!   enum is aligned, which WGSL lays out as the CPU lays out the enum. Building a variant writes
//!   its payload's words (bit casts); reading a payload reads them back.
//!
//! The types memory holds are those in the type of a buffer, a uniform block or workgroup
//! memory, at any depth; code uses them as memory holds them, wherever it holds them. Other
//! types stay as they are: a `bool` value, local or parameter is a `bool`, a struct only code
//! holds (a function's `(vec3, bool)`) keeps its `bool`s, and an enum only code holds is a WGSL
//! struct with a member for each payload.

use crate::layout::{column_stride, field_offsets};
use crate::*;
use std::collections::{HashMap, HashSet};

/// Rewrites a flattened GPU module's memory as the module docs say.
pub fn lay_out_gpu_memory(m: &mut Module) -> Result<()> {
    let memory = memory_types(m);
    let words = memory
        .iter()
        .copied()
        .filter(|&t| {
            matches!(m.types.get(t), TypeDef::Enum { variants, .. } if variants.iter().any(|(_, p)| p.is_some()))
        })
        .collect();
    let mut tm = TypeMap { map: HashMap::new(), words, memory };
    for i in 0..m.functions.len() {
        let mut f = std::mem::take(&mut m.functions[i]);
        // The code first, from the types as they were (a place's, a value's); then the types.
        let mut consts = HashMap::new();
        visit::walk(&f.body, &mut |s| {
            if let Stmt::Let(v, Expr::Const(Const::U32(x))) = s {
                consts.insert(*v, *x);
            }
        });
        let body = std::mem::take(&mut f.body);
        let mut rw = Rewriter { m, tm: &mut tm, f: &mut f, consts };
        let body = rw.block(body)?;
        f.body = body;
        for t in &mut f.values {
            *t = tm.stored(&mut m.types, *t);
        }
        for l in &mut f.locals {
            l.ty = tm.stored(&mut m.types, l.ty);
        }
        for p in &mut f.params {
            p.ty = tm.stored(&mut m.types, p.ty);
        }
        f.ret = f.ret.map(|t| tm.stored(&mut m.types, t));
        m.functions[i] = f;
    }
    for r in &mut m.resources {
        r.ty = if in_memory(r.kind) {
            tm.member(&mut m.types, r.ty)
        } else {
            tm.stored(&mut m.types, r.ty)
        };
    }
    for e in &mut m.entry_points {
        if let Stage::Fragment { varyings: Some((t, _)) } = &mut e.stage {
            *t = tm.stored(&mut m.types, *t);
        }
    }
    Ok(())
}

/// The aggregates memory holds: those in a resource's type, at any depth.
fn memory_types(m: &Module) -> HashSet<TypeId> {
    fn walk(types: &Types, t: TypeId, out: &mut HashSet<TypeId>) {
        let parts: Vec<TypeId> = match types.get(t) {
            TypeDef::Struct { fields, .. } => fields.iter().map(|(_, f)| *f).collect(),
            TypeDef::Enum { variants, .. } => variants.iter().filter_map(|(_, p)| *p).collect(),
            TypeDef::Array(e, _) | TypeDef::RuntimeArray(e) => vec![*e],
            _ => return,
        };
        if out.insert(t) {
            for p in parts {
                walk(types, p, out);
            }
        }
    }
    let mut out = HashSet::new();
    for r in &m.resources {
        walk(&m.types, r.ty, &mut out);
    }
    out
}

/// Each type as code uses it: as memory holds it, if memory holds it.
struct TypeMap {
    map: HashMap<TypeId, TypeId>,
    /// The aggregates memory holds.
    memory: HashSet<TypeId>,
    /// Of those, the enums with payloads: memory holds them as their tag and their payload's
    /// words.
    words: HashSet<TypeId>,
}

impl TypeMap {
    /// `t` as code uses it: each type memory holds as memory holds it, its `bool` members
    /// `u32`s and its enums their words; a `bool` itself stays one.
    fn stored(&mut self, types: &mut Types, t: TypeId) -> TypeId {
        if let Some(&s) = self.map.get(&t) {
            return s;
        }
        let s = match types.get(t).clone() {
            TypeDef::Enum { name, .. } if self.words.contains(&t) => {
                let (w, k) = words_shape(types, t);
                let words = types.intern(TypeDef::Array(w, k));
                let tag = types.u32();
                types.intern(TypeDef::Struct {
                    name,
                    fields: vec![("tag".into(), tag), ("words".into(), words)],
                })
            }
            TypeDef::Struct { name, fields } => {
                let fields = fields.into_iter().map(|(n, f)| (n, self.part(types, t, f))).collect();
                types.intern(TypeDef::Struct { name, fields })
            }
            TypeDef::Enum { name, variants } => {
                let variants = variants
                    .into_iter()
                    .map(|(n, p)| (n, p.map(|p| self.part(types, t, p))))
                    .collect();
                types.intern(TypeDef::Enum { name, variants })
            }
            TypeDef::Array(e, n) => {
                let e = self.part(types, t, e);
                types.intern(TypeDef::Array(e, n))
            }
            TypeDef::RuntimeArray(e) => {
                let e = self.part(types, t, e);
                types.intern(TypeDef::RuntimeArray(e))
            }
            _ => t,
        };
        self.map.insert(t, s);
        s
    }

    /// A member of type `t` as GPU memory holds it: a `bool` is a `u32`.
    fn member(&mut self, types: &mut Types, t: TypeId) -> TypeId {
        if is_bool(types, t) { types.u32() } else { self.stored(types, t) }
    }

    /// A part of type `t` of a value of type `whole`: a member of it if memory holds it.
    fn part(&mut self, types: &mut Types, whole: TypeId, t: TypeId) -> TypeId {
        if self.memory.contains(&whole) { self.member(types, t) } else { self.stored(types, t) }
    }

    /// Whether member `k` of a value of type `t` (a struct's field, an enum's tag or payload,
    /// an array's element) is a `bool` memory holds as a `u32`. A vector's or a matrix's never
    /// is.
    fn bool_member(&self, types: &Types, t: TypeId, k: u32) -> bool {
        self.memory.contains(&t)
            && match types.get(t) {
                TypeDef::Struct { .. } | TypeDef::Enum { .. } => {
                    types.field(t, k).is_some_and(|ft| is_bool(types, ft))
                }
                TypeDef::Array(e, _) => is_bool(types, *e),
                _ => false,
            }
    }
}

/// The words of enum `t` as memory holds it: their type (a `u32` or a vector of them, as wide
/// as the enum's alignment) and how many, after its tag (which the first takes the alignment
/// of): its layout, as the CPU's (`layout::compute`).
fn words_shape(types: &mut Types, t: TypeId) -> (TypeId, u32) {
    let l = types.layout(t);
    let w = match l.align {
        16 => types.vector_of(Scalar::U32, 4),
        8 => types.vector_of(Scalar::U32, 2),
        _ => types.u32(),
    };
    (w, (l.size - l.align) / l.align)
}

fn is_bool(types: &Types, t: TypeId) -> bool {
    *types.get(t) == TypeDef::Scalar(Scalar::Bool)
}

/// Whether a resource of this kind is memory with a layout: a uniform block or a storage
/// buffer.
fn in_memory(kind: ResourceKind) -> bool {
    matches!(
        kind,
        ResourceKind::Uniform { .. } | ResourceKind::StorageRead | ResourceKind::StorageReadWrite
    )
}

/// A step from a value to a part of it: `Expr::Extract` (a field, a component, a column), or
/// `Expr::ExtractDyn` at a constant (an array's element).
#[derive(Clone, Copy)]
enum Step {
    Extract(u32),
    Elem(u32),
}

/// A scalar in a value of a type memory holds: its byte offset, its type, and where it is.
struct Leaf {
    offset: u32,
    scalar: Scalar,
    path: Vec<Step>,
}

/// The scalars of a value of type `t` (as memory holds it: no enum but as its words), at
/// `offset` and below `path`.
fn leaves(types: &Types, t: TypeId, offset: u32, path: &mut Vec<Step>, out: &mut Vec<Leaf>) {
    let mut leaf = |offset, scalar, path: &Vec<Step>| {
        out.push(Leaf { offset, scalar, path: path.clone() });
    };
    match types.get(t) {
        TypeDef::Scalar(s) => leaf(offset, *s, path),
        TypeDef::Vector(s, n) => {
            for c in 0..u32::from(*n) {
                path.push(Step::Extract(c));
                leaf(offset + 4 * c, *s, path);
                path.pop();
            }
        }
        TypeDef::Matrix(n) => {
            for col in 0..u32::from(*n) {
                path.push(Step::Extract(col));
                for r in 0..u32::from(*n) {
                    path.push(Step::Extract(r));
                    leaf(offset + col * column_stride(*n) + 4 * r, Scalar::F32, path);
                    path.pop();
                }
                path.pop();
            }
        }
        TypeDef::Array(e, n) => {
            let stride = types.layout(*e).stride();
            for i in 0..*n {
                path.push(Step::Elem(i));
                leaves(types, *e, offset + i * stride, path, out);
                path.pop();
            }
        }
        TypeDef::Struct { fields, .. } => {
            let offsets = field_offsets(types, t);
            for (k, (_, f)) in fields.iter().enumerate() {
                path.push(Step::Extract(k as u32));
                leaves(types, *f, offset + offsets[k], path, out);
                path.pop();
            }
        }
        // None of these is in memory, so none is in an enum's payload there.
        TypeDef::Enum { .. }
        | TypeDef::RuntimeArray(_)
        | TypeDef::Run(_)
        | TypeDef::Ptr(_)
        | TypeDef::Atomic(_) => {}
    }
}

/// Where a word of an enum's payload is: from its enum's place or value.
#[derive(Clone)]
enum Words {
    /// The enum's place in memory.
    Place(Place),
    /// The enum's value.
    Value(ValueId),
}

struct Rewriter<'a> {
    m: &'a mut Module,
    tm: &'a mut TypeMap,
    f: &'a mut Function,
    /// The function's `u32` constants, by value.
    consts: HashMap<ValueId, u32>,
}

impl Rewriter<'_> {
    fn val(&mut self, out: &mut Block, t: TypeId, e: Expr) -> ValueId {
        self.f.let_(out, t, e)
    }

    fn u32_const(&mut self, out: &mut Block, x: u32) -> ValueId {
        let t = self.m.types.u32();
        self.val(out, t, Expr::Const(Const::U32(x)))
    }

    fn is_words(&self, t: TypeId) -> bool {
        self.tm.words.contains(&t)
    }

    /// `t` as memory holds it.
    fn stored(&mut self, t: TypeId) -> TypeId {
        self.tm.stored(&mut self.m.types, t)
    }

    fn block(&mut self, b: Block) -> Result<Block> {
        let mut out = Vec::with_capacity(b.len());
        for mut s in b {
            for inner in s.blocks_mut() {
                let taken = std::mem::take(inner);
                *inner = self.block(taken)?;
            }
            match s {
                Stmt::Let(v, e) => {
                    let e = self.expr(e, &mut out)?;
                    out.push(Stmt::Let(v, e));
                }
                Stmt::Store(p, x) => self.store(p, x, &mut out)?,
                s => out.push(s),
            }
        }
        Ok(out)
    }

    /// `p` split where it goes into the payload of an enum memory holds as its words: the
    /// enum's place, its type, the variant, and the projections into the payload.
    fn through_payload(&self, p: &Place) -> Option<(Place, TypeId, u32, Vec<Proj>)> {
        let (mut t, path) = self.m.place_start(self.f, p)?;
        let skipped = p.path.len() - path.len();
        for (j, proj) in path.iter().enumerate() {
            if self.is_words(t)
                && let Proj::Field(k) = proj
                && *k > 0
            {
                let at = skipped + j;
                let enum_place = Place { root: p.root.clone(), path: p.path[..at].to_vec() };
                return Some((enum_place, t, k - 1, p.path[at + 1..].to_vec()));
            }
            t = self.m.proj_ty(t, proj)?;
        }
        None
    }

    /// Whether `p` is a `bool` that memory holds as a `u32`: a buffer's, or a member of a type
    /// memory holds.
    fn stored_bool(&self, p: &Place) -> bool {
        let Some((t, path)) = self.m.place_start(self.f, p) else { return false };
        let Some((last, rest)) = path.split_last() else {
            let buffer = matches!(p.root, PlaceRoot::Resource(r) if in_memory(self.m.resources[r.index()].kind));
            return buffer && is_bool(&self.m.types, t);
        };
        let Some(parent) = rest.iter().try_fold(t, |t, proj| self.m.proj_ty(t, proj)) else {
            return false;
        };
        self.tm.memory.contains(&parent)
            && self.m.proj_ty(parent, last).is_some_and(|t| is_bool(&self.m.types, t))
    }

    /// Word `i` of the payload of the enum (of type `t`) that `words` holds, a `u32`.
    fn word(&mut self, words: &Words, t: TypeId, i: u32, out: &mut Block) -> Expr {
        let align = self.m.types.layout(t).align;
        let per = align / 4;
        let (e, c) = (i / per, i % per);
        let at = self.u32_const(out, e);
        match words {
            Words::Place(p) => {
                let mut path = p.path.clone();
                path.extend([Proj::Field(1), Proj::Index(at)]);
                if per > 1 {
                    path.push(Proj::Comp(c as u8));
                }
                Expr::Load(Place { root: p.root.clone(), path })
            }
            Words::Value(x) => {
                let (w, k) = words_shape(&mut self.m.types, t);
                let arr = self.m.types.intern(TypeDef::Array(w, k));
                let all = self.val(out, arr, Expr::Extract(*x, 1));
                if per == 1 {
                    return Expr::ExtractDyn(all, at);
                }
                let elem = self.val(out, w, Expr::ExtractDyn(all, at));
                Expr::Extract(elem, c)
            }
        }
    }

    /// A value of type `t` (as memory holds it) read from the payload words of the enum (of
    /// type `et`) that `words` holds, at byte `offset` of the payload.
    fn decode(
        &mut self,
        words: &Words,
        et: TypeId,
        t: TypeId,
        offset: u32,
        out: &mut Block,
    ) -> Result<Expr> {
        let u32t = self.m.types.u32();
        let parts: Vec<(TypeId, u32)> = match self.m.types.get(t).clone() {
            TypeDef::Scalar(Scalar::U32) => return Ok(self.word(words, et, offset / 4, out)),
            TypeDef::Scalar(s @ (Scalar::I32 | Scalar::F32)) => {
                let w = self.word(words, et, offset / 4, out);
                let w = self.val(out, u32t, w);
                return Ok(Expr::Bitcast(w, s));
            }
            TypeDef::Scalar(Scalar::Bool) => {
                let w = self.word(words, et, offset / 4, out);
                let w = self.val(out, u32t, w);
                let zero = self.u32_const(out, 0);
                return Ok(Expr::Binary(BinOp::Ne, w, zero));
            }
            TypeDef::Vector(s, n) => {
                let ct = self.m.types.scalar(s);
                (0..u32::from(n)).map(|c| (ct, offset + 4 * c)).collect()
            }
            TypeDef::Matrix(n) => {
                let col = self.m.types.vector(n);
                (0..u32::from(n)).map(|c| (col, offset + c * column_stride(n))).collect()
            }
            TypeDef::Array(e, n) => {
                let stride = self.m.types.layout(e).stride();
                (0..n).map(|i| (e, offset + i * stride)).collect()
            }
            TypeDef::Struct { fields, .. } => {
                let offsets = field_offsets(&self.m.types, t).to_vec();
                fields.iter().zip(offsets).map(|((_, f), o)| (*f, offset + o)).collect()
            }
            other => {
                return Err(Error::internal(format!(
                    "a `{other:?}` in an enum's payload in GPU memory"
                )));
            }
        };
        let mut xs = Vec::with_capacity(parts.len());
        for (pt, o) in parts {
            let e = self.decode(words, et, pt, o, out)?;
            xs.push(self.val(out, pt, e));
        }
        Ok(Expr::Construct(t, xs))
    }

    /// The scalars of `x`, a value of type `t` (as memory holds it), at byte `offset`: each
    /// one's word index and value as a `u32`.
    fn encode(
        &mut self,
        x: ValueId,
        t: TypeId,
        offset: u32,
        out: &mut Block,
    ) -> Vec<(u32, ValueId)> {
        let mut ls = Vec::new();
        leaves(&self.m.types, t, offset, &mut Vec::new(), &mut ls);
        let u32t = self.m.types.u32();
        let mut words = Vec::with_capacity(ls.len());
        for l in ls {
            let (mut v, mut vt) = (x, t);
            for step in &l.path {
                let (part_t, e) = match *step {
                    Step::Extract(k) => {
                        let pt = match self.m.types.get(vt) {
                            TypeDef::Vector(s, _) => self.m.types.scalar(*s),
                            TypeDef::Matrix(n) => self.m.types.vector(*n),
                            _ => self.m.types.field(vt, k).expect("a leaf's path is in its value"),
                        };
                        (pt, Expr::Extract(v, k))
                    }
                    Step::Elem(i) => {
                        let TypeDef::Array(e, _) = *self.m.types.get(vt) else {
                            unreachable!("a leaf's element is an array's")
                        };
                        let at = self.u32_const(out, i);
                        (e, Expr::ExtractDyn(v, at))
                    }
                };
                v = self.val(out, part_t, e);
                vt = part_t;
            }
            let w = match l.scalar {
                Scalar::U32 => v,
                Scalar::Bool => self.val(out, u32t, Expr::Convert(v, Scalar::U32)),
                _ => self.val(out, u32t, Expr::Bitcast(v, Scalar::U32)),
            };
            words.push((l.offset / 4, w));
        }
        words
    }

    /// `Variant(t, v, payload)` for an enum memory holds as its words.
    fn variant(&mut self, t: TypeId, v: u32, payload: Option<ValueId>, out: &mut Block) -> Expr {
        let st = self.stored(t);
        let (w, k) = words_shape(&mut self.m.types, t);
        let arr = self.m.types.intern(TypeDef::Array(w, k));
        let tag = self.u32_const(out, v);
        let words = match payload {
            None => self.val(out, arr, Expr::Zero(arr)),
            Some(x) => {
                // Values hold their types as memory does (`stored`), as this pass leaves them.
                let pt = self.m.types.field(t, v + 1).expect("the variant has a payload");
                let pt = self.stored(pt);
                let per = self.m.types.layout(t).align / 4;
                let mut all = vec![None; (k * per) as usize];
                for (i, w) in self.encode(x, pt, 0, out) {
                    all[i as usize] = Some(w);
                }
                let zero = self.u32_const(out, 0);
                let all: Vec<ValueId> = all.into_iter().map(|w| w.unwrap_or(zero)).collect();
                let elems = if per == 1 {
                    all
                } else {
                    all.chunks(per as usize)
                        .map(|c| self.val(out, w, Expr::Construct(w, c.to_vec())))
                        .collect()
                };
                self.val(out, arr, Expr::Construct(arr, elems))
            }
        };
        Expr::Construct(st, vec![tag, words])
    }

    /// The part at `rest` of `x`, a value of type `t` (as it was), read as this pass reads
    /// parts: a payload of an enum memory holds from its words, a `bool` member as a `bool`.
    fn project(&mut self, x: ValueId, t: TypeId, rest: &[Proj], out: &mut Block) -> Result<Expr> {
        let Some((first, rest)) = rest.split_first() else {
            return Err(Error::internal("an empty projection"));
        };
        let pt = self.m.proj_ty(t, first).ok_or_else(|| Error::internal("a projection's type"))?;
        let e = match first {
            Proj::Field(k) => self.extract(x, t, *k, out)?,
            Proj::Comp(c) => Expr::Extract(x, u32::from(*c)),
            Proj::Index(i) => self.extract_dyn(x, t, *i, out),
        };
        if rest.is_empty() {
            return Ok(e);
        }
        let spt = self.stored(pt);
        let y = self.val(out, spt, e);
        self.project(y, pt, rest, out)
    }

    /// `Extract(x, k)` of `x`, a value of type `t` (as it was).
    fn extract(&mut self, x: ValueId, t: TypeId, k: u32, out: &mut Block) -> Result<Expr> {
        if self.is_words(t) && k > 0 {
            let pt = self.m.types.field(t, k).expect("the variant has a payload");
            let pt = self.stored(pt);
            return self.decode(&Words::Value(x), t, pt, 0, out);
        }
        if self.tm.bool_member(&self.m.types, t, k) {
            let u32t = self.m.types.u32();
            let w = self.val(out, u32t, Expr::Extract(x, k));
            let zero = self.u32_const(out, 0);
            return Ok(Expr::Binary(BinOp::Ne, w, zero));
        }
        Ok(Expr::Extract(x, k))
    }

    /// `ExtractDyn(x, i)` of `x`, a value of type `t` (as it was).
    fn extract_dyn(&mut self, x: ValueId, t: TypeId, i: ValueId, out: &mut Block) -> Expr {
        if self.tm.bool_member(&self.m.types, t, 0) {
            let u32t = self.m.types.u32();
            let w = self.val(out, u32t, Expr::ExtractDyn(x, i));
            let zero = self.u32_const(out, 0);
            return Expr::Binary(BinOp::Ne, w, zero);
        }
        Expr::ExtractDyn(x, i)
    }

    /// `e` with its types as memory holds them, reading what memory holds as this pass lays it
    /// out; what it needs computed first goes in `out`.
    fn expr(&mut self, e: Expr, out: &mut Block) -> Result<Expr> {
        let u32t = self.m.types.u32();
        Ok(match e {
            Expr::Load(p) => {
                if let Some((ep, et, v, rest)) = self.through_payload(&p) {
                    let words = Words::Place(ep);
                    if let Some((offset, t)) = self.payload_part(et, v, &rest) {
                        // Just the part's words.
                        let st = self.stored(t);
                        self.decode(&words, et, st, offset, out)?
                    } else {
                        // At an index the code computes: the whole payload, then the part.
                        let pt = self.m.types.field(et, v + 1).expect("the variant has a payload");
                        let spt = self.stored(pt);
                        let whole = self.decode(&words, et, spt, 0, out)?;
                        let x = self.val(out, spt, whole);
                        self.project(x, pt, &rest, out)?
                    }
                } else if self.stored_bool(&p) {
                    let w = self.val(out, u32t, Expr::Load(p));
                    let zero = self.u32_const(out, 0);
                    Expr::Binary(BinOp::Ne, w, zero)
                } else {
                    Expr::Load(p)
                }
            }
            Expr::Extract(x, k) => {
                let t = self.f.value_ty(x);
                self.extract(x, t, k, out)?
            }
            Expr::ExtractDyn(x, i) => {
                let t = self.f.value_ty(x);
                self.extract_dyn(x, t, i, out)
            }
            Expr::Construct(t, xs) => {
                let xs = xs
                    .into_iter()
                    .zip(0..)
                    .map(|(x, k)| {
                        if self.tm.bool_member(&self.m.types, t, k) {
                            self.val(out, u32t, Expr::Convert(x, Scalar::U32))
                        } else {
                            x
                        }
                    })
                    .collect();
                Expr::Construct(self.stored(t), xs)
            }
            Expr::Variant(t, v, payload) if self.is_words(t) => self.variant(t, v, payload, out),
            Expr::Variant(t, v, payload) => {
                let payload = payload.map(|x| {
                    if self.tm.bool_member(&self.m.types, t, v + 1) {
                        self.val(out, u32t, Expr::Convert(x, Scalar::U32))
                    } else {
                        x
                    }
                });
                Expr::Variant(self.stored(t), v, payload)
            }
            Expr::Zero(t) => Expr::Zero(self.stored(t)),
            e => e,
        })
    }

    /// The part at `rest` of variant `v`'s payload of enum `et`: its byte offset in the payload,
    /// and its type (as it was). None if an index on the way isn't a constant.
    fn payload_part(&self, et: TypeId, v: u32, rest: &[Proj]) -> Option<(u32, TypeId)> {
        let types = &self.m.types;
        let mut t = types.field(et, v + 1)?;
        let mut offset = 0;
        for proj in rest {
            offset += match (types.get(t), proj) {
                (TypeDef::Struct { .. }, Proj::Field(k)) => field_offsets(types, t)[*k as usize],
                (TypeDef::Enum { .. }, Proj::Field(0)) => 0,
                // A payload is after the tag, at the enum's alignment.
                (TypeDef::Enum { .. }, Proj::Field(_)) => types.layout(t).align,
                (TypeDef::Vector(..), Proj::Comp(c)) => 4 * u32::from(*c),
                (TypeDef::Matrix(n), Proj::Index(i)) => self.consts.get(i)? * column_stride(*n),
                (TypeDef::Array(e, _), Proj::Index(i)) => {
                    self.consts.get(i)? * types.layout(*e).stride()
                }
                _ => return None,
            };
            t = self.m.proj_ty(t, proj)?;
        }
        Some((offset, t))
    }

    /// `Store(p, x)`, writing what memory holds as this pass lays it out.
    fn store(&mut self, p: Place, x: ValueId, out: &mut Block) -> Result<()> {
        if let Some((ep, et, v, rest)) = self.through_payload(&p) {
            let Some((offset, t)) = self.payload_part(et, v, &rest) else {
                return Err(Error::internal(
                    "GPU code writes an enum's payload in memory at an index it computes",
                ));
            };
            let st = self.stored(t);
            let words = Words::Place(ep);
            for (i, w) in self.encode(x, st, offset, out) {
                let Expr::Load(wp) = self.word(&words, et, i, out) else {
                    unreachable!("a place's word is a load")
                };
                out.push(Stmt::Store(wp, w));
            }
        } else if self.stored_bool(&p) {
            let u32t = self.m.types.u32();
            let w = self.val(out, u32t, Expr::Convert(x, Scalar::U32));
            out.push(Stmt::Store(p, w));
        } else {
            out.push(Stmt::Store(p, x));
        }
        Ok(())
    }
}
