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
        let mut body = std::mem::take(&mut f.body);
        Rewriter { m, tm: &mut tm, f: &mut f, consts }.block(&mut body)?;
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
        if let Stage::Fragment { varyings: Some((t, _)), .. } = &mut e.stage {
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
                let words = words_shape(types, t).array;
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
            TypeDef::Enum { name, variants, tags } => {
                let variants = variants
                    .into_iter()
                    .map(|(n, p)| (n, p.map(|p| self.part(types, t, p))))
                    .collect();
                types.intern(TypeDef::Enum { name, variants, tags })
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

/// The words that memory holds an enum's payload in, after its tag (which the first takes the
/// alignment of): its layout, as the CPU's (`layout::compute`).
struct WordsShape {
    /// A word's type: a `u32`, or a vector of them as wide as the enum's alignment.
    word: TypeId,
    /// The array of them.
    array: TypeId,
    /// How many words there are.
    count: u32,
    /// How many `u32`s a word holds: the enum's alignment's.
    per: u32,
}

/// The words of enum `t` as memory holds it.
fn words_shape(types: &mut Types, t: TypeId) -> WordsShape {
    let l = types.layout(t);
    let word = match l.align {
        16 => types.vector_of(Scalar::U32, 4),
        8 => types.vector_of(Scalar::U32, 2),
        _ => types.u32(),
    };
    let count = (l.size - l.align) / l.align;
    WordsShape { word, array: types.intern(TypeDef::Array(word, count)), count, per: l.align / 4 }
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

    /// `e`, a `u32` that memory holds a `bool` as, read as the `bool`.
    fn word_to_bool(&mut self, e: Expr, out: &mut Block) -> Expr {
        let u32t = self.m.types.u32();
        let w = self.val(out, u32t, e);
        let zero = self.u32_const(out, 0);
        Expr::Binary(BinOp::Ne, w, zero)
    }

    /// `x`, a `bool`, as the `u32` that memory holds it as: 0 or 1.
    fn bool_to_word(&mut self, x: ValueId, out: &mut Block) -> ValueId {
        let u32t = self.m.types.u32();
        self.val(out, u32t, Expr::Convert(x, Scalar::U32))
    }

    fn is_words(&self, t: TypeId) -> bool {
        self.tm.words.contains(&t)
    }

    /// `t` as memory holds it.
    fn stored(&mut self, t: TypeId) -> TypeId {
        self.tm.stored(&mut self.m.types, t)
    }

    fn block(&mut self, b: &mut Block) -> Result<()> {
        visit::try_expand(b, &mut |s, out| {
            match s {
                Stmt::Let(v, e) => {
                    let e = self.expr(e, out)?;
                    out.push(Stmt::Let(v, e));
                }
                Stmt::Store(p, x) => self.store(p, x, out)?,
                s => out.push(s),
            }
            Ok(())
        })
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
        let per = self.m.types.layout(t).align / 4;
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
                let shape = words_shape(&mut self.m.types, t);
                let all = self.val(out, shape.array, Expr::Extract(*x, 1));
                if per == 1 {
                    return Expr::ExtractDyn(all, at);
                }
                let elem = self.val(out, shape.word, Expr::ExtractDyn(all, at));
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
        let parts = match self.m.types.get(t) {
            TypeDef::Scalar(Scalar::U32) => return Ok(self.word(words, et, offset / 4, out)),
            &TypeDef::Scalar(s @ (Scalar::I32 | Scalar::F32)) => {
                let w = self.word(words, et, offset / 4, out);
                let u32t = self.m.types.u32();
                let w = self.val(out, u32t, w);
                return Ok(Expr::Bitcast(w, s));
            }
            TypeDef::Scalar(Scalar::Bool) => {
                let w = self.word(words, et, offset / 4, out);
                return Ok(self.word_to_bool(w, out));
            }
            TypeDef::Vector(..)
            | TypeDef::Matrix(_)
            | TypeDef::Array(..)
            | TypeDef::Struct { .. } => layout::parts(&self.m.types, t),
            other => {
                return Err(Error::internal(format!(
                    "a `{other:?}` in an enum's payload in GPU memory"
                )));
            }
        };
        let mut xs = Vec::with_capacity(parts.len());
        for (pt, o) in parts {
            let e = self.decode(words, et, pt, offset + o, out)?;
            xs.push(self.val(out, pt, e));
        }
        Ok(Expr::Construct(t, xs))
    }

    /// The scalars of `x`, a value of type `t` (as memory holds it), at byte `offset`: each
    /// one's word index and value as a `u32`, added to `words`. Each part on the way is taken
    /// out once.
    fn encode(
        &mut self,
        x: ValueId,
        t: TypeId,
        offset: u32,
        out: &mut Block,
        words: &mut Vec<(u32, ValueId)>,
    ) {
        let w = match *self.m.types.get(t) {
            TypeDef::Scalar(Scalar::U32) => x,
            TypeDef::Scalar(Scalar::Bool) => self.bool_to_word(x, out),
            TypeDef::Scalar(_) => {
                let u32t = self.m.types.u32();
                self.val(out, u32t, Expr::Bitcast(x, Scalar::U32))
            }
            // An array's element is taken out at a constant index. (No enum, run, pointer or
            // atomic is in memory, so none is in an enum's payload there: they have no parts.)
            ref d => {
                let array = matches!(d, TypeDef::Array(..));
                for (k, (pt, o)) in (0..).zip(layout::parts(&self.m.types, t)) {
                    let e = if array {
                        let at = self.u32_const(out, k);
                        Expr::ExtractDyn(x, at)
                    } else {
                        Expr::Extract(x, k)
                    };
                    let part = self.val(out, pt, e);
                    self.encode(part, pt, offset + o, out, words);
                }
                return;
            }
        };
        words.push((offset / 4, w));
    }

    /// `Variant(t, v, payload)` for an enum memory holds as its words.
    fn variant(&mut self, t: TypeId, v: u32, payload: Option<ValueId>, out: &mut Block) -> Expr {
        let st = self.stored(t);
        let shape = words_shape(&mut self.m.types, t);
        let tag = self.m.types.tag(t, v);
        let tag = self.u32_const(out, tag);
        let words = match payload {
            None => self.val(out, shape.array, Expr::Zero(shape.array)),
            Some(x) => {
                // Values hold their types as memory does (`stored`), as this pass leaves them.
                let pt = self.m.types.field(t, v + 1).expect("the variant has a payload");
                let pt = self.stored(pt);
                let mut encoded = Vec::new();
                self.encode(x, pt, 0, out, &mut encoded);
                let mut all = vec![None; (shape.count * shape.per) as usize];
                for (i, w) in encoded {
                    all[i as usize] = Some(w);
                }
                let zero = self.u32_const(out, 0);
                let all: Vec<ValueId> = all.into_iter().map(|w| w.unwrap_or(zero)).collect();
                let elems = if shape.per == 1 {
                    all
                } else {
                    all.chunks(shape.per as usize)
                        .map(|c| self.val(out, shape.word, Expr::Construct(shape.word, c.to_vec())))
                        .collect()
                };
                self.val(out, shape.array, Expr::Construct(shape.array, elems))
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
            return Ok(self.word_to_bool(Expr::Extract(x, k), out));
        }
        Ok(Expr::Extract(x, k))
    }

    /// `ExtractDyn(x, i)` of `x`, a value of type `t` (as it was).
    fn extract_dyn(&mut self, x: ValueId, t: TypeId, i: ValueId, out: &mut Block) -> Expr {
        if self.tm.bool_member(&self.m.types, t, 0) {
            return self.word_to_bool(Expr::ExtractDyn(x, i), out);
        }
        Expr::ExtractDyn(x, i)
    }

    /// `e` with its types as memory holds them, reading what memory holds as this pass lays it
    /// out; what it needs computed first goes in `out`.
    fn expr(&mut self, e: Expr, out: &mut Block) -> Result<Expr> {
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
                    self.word_to_bool(Expr::Load(p), out)
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
                            self.bool_to_word(x, out)
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
                        self.bool_to_word(x, out)
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
            let k = match (types.get(t), proj) {
                (TypeDef::Struct { .. } | TypeDef::Enum { .. }, Proj::Field(k)) => *k,
                (TypeDef::Vector(..), Proj::Comp(c)) => u32::from(*c),
                (TypeDef::Matrix(_) | TypeDef::Array(..), Proj::Index(i)) => *self.consts.get(i)?,
                _ => return None,
            };
            offset += layout::part_offset(types, t, k)?;
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
            let mut encoded = Vec::new();
            self.encode(x, st, offset, out, &mut encoded);
            for (i, w) in encoded {
                let Expr::Load(wp) = self.word(&words, et, i, out) else {
                    unreachable!("a place's word is a load")
                };
                out.push(Stmt::Store(wp, w));
            }
        } else if self.stored_bool(&p) {
            let w = self.bool_to_word(x, out);
            out.push(Stmt::Store(p, w));
        } else {
            out.push(Stmt::Store(p, x));
        }
        Ok(())
    }
}
