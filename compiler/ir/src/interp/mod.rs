//! An interpreter of GPU code: one invocation of one entry point of a flattened GPU module
//! (`opt::flatten_gpu`, then `gpu_memory::lay_out_gpu_memory`), the module the WGSL is written
//! from. So what it computes is what the GPU was given, operation by operation, with each
//! source line's values on the way (`wrela pixel`, `wrela twin`'s proofs).
//!
//! - Values are [`V`]s, the shape constant folding uses; each operation is computed by constant
//!   folding's own evaluators (`const_fold`), as WGSL defines it: a float in `f32`, a
//!   transcendental through `f64`. Where WGSL leaves a result undefined (`sqrt` of a negative,
//!   an integer divided by zero, a `smoothstep` whose edges meet) or not finite, the evaluators'
//!   total form gives IEEE's (or WGSL's defined) result, and the value is marked **undefined on
//!   the GPU**: often what someone asking about a pixel is looking for.
//! - Integers wrap, as on the GPU. An index out of range reads zero, writes nothing, and is
//!   marked.
//! - Resources come from [`Inputs`]: a uniform block's and a buffer's bytes are decoded by the
//!   module's own layout ([`crate::layout`]); writes to a storage buffer are kept and listed.
//! - What isn't run: atomics, workgroup memory, subgroup operations and barriers
//!   ([`Error::Unsupported`]). Textures are [`Inputs::texture`]'s.
//!
//! [`Observer`] sees each statement's place in the source, each value as it's made (and may
//! change it, which the proofs' tests use to move a value within a bound), each store, each
//! branch and each loop's passes. The no-op observer, `()`, costs nothing.

pub mod concrete;
pub mod raster;

pub use crate::const_fold::K as V;

use crate::const_fold::{binary, builtin, convert, each1, each2, select, unary};
use crate::*;
use std::collections::HashMap;
use wrela_diag::Span;

/// Why an invocation couldn't be run.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// What the interpreter doesn't run (workgroup memory, an atomic, ...).
    Unsupported(String),
    /// The module isn't one the interpreter can make sense of (a malformed place, a missing
    /// input): a bug, or a module that wasn't flattened.
    Malformed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported(what) => write!(f, "the interpreter doesn't run {what}"),
            Error::Malformed(what) => write!(f, "the module can't be interpreted: {what}"),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn malformed<T>(what: impl Into<String>) -> Result<T> {
    Err(Error::Malformed(what.into()))
}

/// A texture's texels, as the GPU holds them: its size, its format, and each level's (and
/// layer's) raw bytes, rows top to bottom.
#[derive(Clone, Debug)]
pub struct Texels {
    pub width: u32,
    pub height: u32,
    /// A 3D texture's depth, or an array's layers (6 for a cube); 1 otherwise.
    pub depth: u32,
    pub format: TextureFormat,
    /// `levels[l][layer]`: level `l`'s bytes for each layer (a 3D texture has one layer of all
    /// its slices).
    pub levels: Vec<Vec<Vec<u8>>>,
}

/// A sampler's settings, as `std::gpu::sampler` makes them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SamplerDesc {
    pub linear: bool,
    pub mip_linear: bool,
    /// Repeat (else clamp to the edge), along u and v (and w).
    pub repeat: [bool; 3],
    /// Anisotropic filtering's largest ratio (1: none). Computed as trilinear, and marked
    /// approximate.
    pub anisotropy: u16,
    /// A comparison sampler's test: what `SampleCompare` counts as passing.
    pub compare: Option<BinOp>,
}

/// Where an invocation's resources and builtins come from.
pub trait Inputs {
    /// A uniform block's or a storage buffer's bytes, by its resource.
    fn bytes(&self, r: &Resource) -> Option<&[u8]>;
    /// A texture's texels, by its resource.
    fn texture(&self, _r: &Resource) -> Option<&Texels> {
        None
    }
    /// A sampler's settings, by its resource.
    fn sampler(&self, _r: &Resource) -> Option<SamplerDesc> {
        None
    }
    /// A builtin input's value (a `vec3u` for an id, a `vec4` for a fragment's position, ...).
    fn builtin(&self, b: BuiltinInput) -> Option<V>;
    /// A fragment's varyings, as its last parameter takes them.
    fn varyings(&self) -> Option<V> {
        None
    }
    /// The `j`th derivative call of the invocation (`dpdx`, `dpdy`, `fwidth`, or an
    /// implicit-level sample's), given its operand: the block of four's answer
    /// (`concrete::Block`). Without one, a derivative is 0.
    fn derivative(&mut self, _j: u32, _op: Builtin, operand: &V) -> V {
        zero_like(operand)
    }
}

/// What a walk shows an observer.
pub trait Observer {
    fn at(&mut self, _span: Span) {}
    /// A value as it's made: the observer may change it.
    fn value(&mut self, _f: FuncId, _v: ValueId, _ty: TypeId, _val: &mut V) {}
    fn store(&mut self, _local: Option<&str>, _place: &Place, _val: &V) {}
    fn branch(&mut self, _taken: bool) {}
    /// A loop left after `n` passes.
    fn pass(&mut self, _n: u32) {}
}

impl Observer for () {}

/// A mark the run left: an operation whose result WGSL leaves undefined (or isn't finite), an
/// index out of range, a sample computed approximately.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub at: Option<Span>,
    pub what: String,
}

/// A write to a storage buffer: its binding, the element's index, and the element.
#[derive(Clone, Debug, PartialEq)]
pub struct Write {
    pub binding: u32,
    pub index: u32,
    pub value: V,
}

/// What an invocation did.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// The entry point's result (a fragment's colours, a vertex's position and varyings).
    pub result: Option<V>,
    pub writes: Vec<Write>,
    pub undefined: Vec<Mark>,
    pub out_of_range: Vec<Mark>,
    pub approximate: Vec<Mark>,
    /// The fragment was discarded.
    pub discarded: bool,
}

/// The value a `Let` defines.
fn v_id(s: &Stmt) -> &ValueId {
    match s {
        Stmt::Let(v, _) => v,
        _ => unreachable!("only a `Let` defines a value"),
    }
}

/// A zero shaped as `v`.
pub fn zero_like(v: &V) -> V {
    match v {
        V::S(c) => V::S(match c {
            Const::Bool(_) => Const::Bool(false),
            Const::I32(_) => Const::I32(0),
            Const::U32(_) => Const::U32(0),
            Const::I64(_) => Const::I64(0),
            Const::U64(_) => Const::U64(0),
            Const::Small(s, _) => Const::Small(*s, 0),
            Const::F32(_) => Const::F32(0.0),
            Const::F64(_) => Const::F64(0.0),
            Const::F16(_) => Const::F16(0.0),
        }),
        V::P(ps) => V::P(ps.iter().map(zero_like).collect()),
    }
}

/// A zero of type `t`.
pub fn zero(types: &Types, t: TypeId) -> V {
    let s = |s: Scalar| {
        V::S(match s {
            Scalar::Bool => Const::Bool(false),
            Scalar::I32 => Const::I32(0),
            Scalar::U32 => Const::U32(0),
            Scalar::I64 => Const::I64(0),
            Scalar::U64 => Const::U64(0),
            Scalar::F32 => Const::F32(0.0),
            Scalar::F64 => Const::F64(0.0),
            Scalar::F16 => Const::F16(0.0),
            small => Const::Small(small, 0),
        })
    };
    match types.get(t) {
        TypeDef::Scalar(x) | TypeDef::Atomic(x) => s(*x),
        TypeDef::Vector(x, n) => V::P(vec![s(*x); *n as usize]),
        TypeDef::Matrix(c, r) => V::P(vec![V::P(vec![s(Scalar::F32); *r as usize]); *c as usize]),
        TypeDef::Struct { fields, .. } => {
            V::P(fields.iter().map(|(_, f)| zero(types, *f)).collect())
        }
        TypeDef::Enum { variants, .. } => V::P(
            std::iter::once(s(Scalar::U32))
                .chain(variants.iter().map(|(_, p)| p.map_or(V::P(Vec::new()), |p| zero(types, p))))
                .collect(),
        ),
        TypeDef::Array(e, n) => V::P(vec![zero(types, *e); *n as usize]),
        TypeDef::RuntimeArray(_) | TypeDef::Run(_) | TypeDef::Ptr(_) => V::P(Vec::new()),
    }
}

/// The value of type `t` in `bytes` at `at`, by the module's layout (a `bool` in memory is a
/// `u32` there already). Bytes past the end read as zero.
pub fn decode(types: &Types, t: TypeId, bytes: &[u8], at: usize) -> V {
    let word = |o: usize, n: usize| -> u64 {
        let mut b = [0u8; 8];
        for (i, x) in b.iter_mut().enumerate().take(n) {
            *x = bytes.get(o + i).copied().unwrap_or(0);
        }
        u64::from_le_bytes(b)
    };
    let scalar = |s: Scalar, o: usize| -> V {
        V::S(match s {
            Scalar::Bool => Const::Bool(word(o, 4) != 0),
            Scalar::I32 => Const::I32(word(o, 4) as u32 as i32),
            Scalar::U32 => Const::U32(word(o, 4) as u32),
            Scalar::F32 => Const::F32(f32::from_bits(word(o, 4) as u32)),
            Scalar::F16 => Const::F16(half_to_f32(word(o, 2) as u16)),
            Scalar::I64 => Const::I64(word(o, 8) as i64),
            Scalar::U64 => Const::U64(word(o, 8)),
            Scalar::F64 => Const::F64(f64::from_bits(word(o, 8))),
            small => Const::Small(small, word(o, small.bytes() as usize) as i64),
        })
    };
    match types.get(t) {
        TypeDef::Scalar(s) | TypeDef::Atomic(s) => scalar(*s, at),
        TypeDef::Vector(s, n) => {
            V::P((0..*n as usize).map(|i| scalar(*s, at + i * s.bytes() as usize)).collect())
        }
        TypeDef::Matrix(c, r) => {
            let stride = layout::column_stride(*r) as usize;
            V::P(
                (0..*c as usize)
                    .map(|i| {
                        V::P(
                            (0..*r as usize)
                                .map(|k| scalar(Scalar::F32, at + i * stride + 4 * k))
                                .collect(),
                        )
                    })
                    .collect(),
            )
        }
        TypeDef::Array(e, n) => {
            let stride = layout::array_stride(types, *e) as usize;
            V::P((0..*n as usize).map(|i| decode(types, *e, bytes, at + i * stride)).collect())
        }
        TypeDef::Struct { fields, .. } => {
            let offsets = layout::field_offsets(types, t);
            V::P(
                fields
                    .iter()
                    .zip(offsets)
                    .map(|((_, f), o)| decode(types, *f, bytes, at + *o as usize))
                    .collect(),
            )
        }
        _ => zero(types, t),
    }
}

/// A half float's bits as the `f32` it is.
pub fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = i32::from((h >> 10) & 0x1f);
    let m = f32::from(h & 0x3ff);
    sign * match e {
        0 => m * 2f32.powi(-24),
        31 if m == 0.0 => f32::INFINITY,
        31 => f32::NAN,
        _ => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

/// A place resolved to where its value lives: the frame's local, a resource, a private
/// variable, and the indexes and fields below it.
#[derive(Clone, Debug, PartialEq)]
enum Root {
    Local(usize, LocalId),
    Resource(ResourceId),
}

#[derive(Clone, Debug, PartialEq)]
struct Abs {
    root: Root,
    path: Vec<usize>,
}

/// A parameter's argument: a value, or (by reference) a place.
#[derive(Clone, Debug)]
enum ArgV {
    Value(V),
    Ref(Abs),
}

struct Frame {
    func: FuncId,
    vals: Vec<Option<V>>,
    locals: Vec<V>,
    args: Vec<ArgV>,
}

/// How a block ended.
enum Flow {
    Normal,
    Break,
    Continue,
    Return(Option<V>),
    Discard,
}

/// The most passes a loop may make before the run stops (a GPU would hang).
pub const MOST_PASSES: u32 = 1 << 20;

/// The interpreter: a module, its inputs, an observer, and what the run has made.
pub struct Interp<'a, I: Inputs, O: Observer> {
    pub m: &'a Module,
    pub inputs: &'a mut I,
    pub obs: &'a mut O,
    frames: Vec<Frame>,
    /// Each resource's decoded value, once read: a uniform block, a storage buffer's elements
    /// by index (a write replaces one), a private variable.
    memory: HashMap<ResourceId, HashMap<u32, V>>,
    out: Outcome,
    at: Option<Span>,
    derivatives: u32,
}

impl<'a, I: Inputs, O: Observer> Interp<'a, I, O> {
    pub fn new(m: &'a Module, inputs: &'a mut I, obs: &'a mut O) -> Self {
        Interp {
            m,
            inputs,
            obs,
            frames: Vec::new(),
            memory: HashMap::new(),
            out: Outcome::default(),
            at: None,
            derivatives: 0,
        }
    }

    /// Runs entry point `e` (by its index in the module) once.
    pub fn run_entry(mut self, e: usize) -> Result<Outcome> {
        let entry =
            self.m.entry_points.get(e).ok_or_else(|| Error::Malformed("no entry".into()))?;
        let f = &self.m.functions[entry.function.index()];
        let mut args = Vec::new();
        if !f.params.is_empty() {
            let v = self.inputs.varyings().ok_or_else(|| Error::Malformed("no varyings".into()))?;
            args.push(ArgV::Value(v));
        }
        let result = self.call(entry.function, args)?;
        self.out.result = result;
        Ok(self.out)
    }

    /// Runs function `f` with by-value arguments `args`: its result and what it did.
    pub fn run_function(mut self, f: FuncId, args: Vec<V>) -> Result<Outcome> {
        let result = self.call(f, args.into_iter().map(ArgV::Value).collect())?;
        self.out.result = result;
        Ok(self.out)
    }

    fn mark(&mut self, what: impl Into<String>) {
        self.out.undefined.push(Mark { at: self.at, what: what.into() });
    }

    fn call(&mut self, f: FuncId, args: Vec<ArgV>) -> Result<Option<V>> {
        let func = &self.m.functions[f.index()];
        if self.frames.len() > 64 {
            return Err(Error::Unsupported("calls nested more than 64 deep".into()));
        }
        let locals = func.locals.iter().map(|l| zero(&self.m.types, l.ty)).collect();
        self.frames.push(Frame { func: f, vals: vec![None; func.values.len()], locals, args });
        let r = self.block(&func.body);
        self.frames.pop();
        match r? {
            Flow::Return(v) => Ok(v),
            Flow::Discard => {
                self.out.discarded = true;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn frame(&self) -> &Frame {
        self.frames.last().expect("a frame")
    }

    fn func(&self) -> &'a Function {
        &self.m.functions[self.frame().func.index()]
    }

    fn val(&self, v: ValueId) -> Result<V> {
        self.frame()
            .vals
            .get(v.index())
            .cloned()
            .flatten()
            .ok_or_else(|| Error::Malformed(format!("value {v:?} read before it's made")))
    }

    fn block(&mut self, b: &[Stmt]) -> Result<Flow> {
        for s in b {
            match self.stmt(s)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn stmt(&mut self, s: &Stmt) -> Result<Flow> {
        match s {
            Stmt::At(span) => {
                self.at = Some(*span);
                self.obs.at(*span);
            }
            Stmt::Let(v, e) => {
                let mut x = self.expr(e)?;
                // A builtin arrives as the std struct that holds it (`FragCoord { position }`,
                // `VertexIndex { index }`): a struct of one field holds the value given.
                if let (Expr::EntryInput(_), Some(v)) = (e, &mut x) {
                    let ty = self.func().value_ty(*v_id(s));
                    if let TypeDef::Struct { fields, .. } = self.m.types.get(ty)
                        && fields.len() == 1
                        && !matches!(v, V::P(p) if p.len() == 1)
                    {
                        *v = V::P(vec![v.clone()]);
                    }
                }
                if let Some(x) = &mut x {
                    let f = self.frame().func;
                    let ty = self.func().value_ty(*v);
                    self.obs.value(f, *v, ty, x);
                }
                if matches!(e, Expr::Discard) {
                    return Ok(Flow::Discard);
                }
                let frame = self.frames.last_mut().expect("a frame");
                frame.vals[v.index()] = x;
            }
            Stmt::Eval(e) => {
                if matches!(e, Expr::Discard) {
                    return Ok(Flow::Discard);
                }
                self.expr(e)?;
            }
            Stmt::Store(p, v) => {
                let x = self.val(*v)?;
                let local = match &p.root {
                    PlaceRoot::Local(l) => Some(self.func().locals[l.index()].name.as_str()),
                    _ => None,
                };
                self.obs.store(local, p, &x);
                let abs = self.resolve(p)?;
                self.write(&abs, x)?;
            }
            Stmt::If { cond, then, else_ } => {
                let c = matches!(self.val(*cond)?, V::S(Const::Bool(true)));
                self.obs.branch(c);
                return self.block(if c { then } else { else_ });
            }
            Stmt::Loop { body, continuing } => {
                let mut n = 0;
                loop {
                    n += 1;
                    if n > MOST_PASSES {
                        return Err(Error::Unsupported(format!(
                            "a loop of more than {MOST_PASSES} passes"
                        )));
                    }
                    match self.block(body)? {
                        Flow::Break => break,
                        f @ (Flow::Return(_) | Flow::Discard) => {
                            self.obs.pass(n);
                            return Ok(f);
                        }
                        Flow::Normal | Flow::Continue => {}
                    }
                    if let f @ (Flow::Return(_) | Flow::Discard) = self.block(continuing)? {
                        return Ok(f);
                    }
                }
                self.obs.pass(n);
            }
            Stmt::Break => return Ok(Flow::Break),
            Stmt::Continue => return Ok(Flow::Continue),
            Stmt::Return(v) => {
                let v = v.map(|v| self.val(v)).transpose()?;
                return Ok(Flow::Return(v));
            }
            Stmt::Trap => return Err(Error::Unsupported("a trap (unreachable code)".into())),
        }
        Ok(Flow::Normal)
    }

    /// A place's root and path, its indexes evaluated. A by-reference parameter's place is the
    /// caller's, continued.
    fn resolve(&mut self, p: &Place) -> Result<Abs> {
        let depth = self.frames.len() - 1;
        let mut abs = match &p.root {
            PlaceRoot::Local(l) => Abs { root: Root::Local(depth, *l), path: Vec::new() },
            PlaceRoot::Resource(r) => Abs { root: Root::Resource(*r), path: Vec::new() },
            PlaceRoot::Param(i) => match self.frame().args.get(*i as usize) {
                Some(ArgV::Ref(a)) => a.clone(),
                _ => return malformed(format!("parameter {i} isn't a place")),
            },
            PlaceRoot::Ptr(_) | PlaceRoot::Data(_) => {
                return malformed("a pointer or constant data in GPU code");
            }
        };
        for proj in &p.path {
            abs.path.push(match proj {
                Proj::Field(k) => *k as usize,
                Proj::Comp(c) => usize::from(*c),
                Proj::Index(i) => match self.val(*i)? {
                    V::S(Const::U32(x)) => x as usize,
                    V::S(Const::I32(x)) => {
                        if x < 0 {
                            usize::MAX
                        } else {
                            x as usize
                        }
                    }
                    other => return malformed(format!("an index of {other:?}")),
                },
            });
        }
        Ok(abs)
    }

    /// The resource's value at `abs` (a buffer's element, decoded once).
    fn resource_part(&mut self, r: ResourceId, path: &[usize]) -> Result<(V, usize)> {
        let res = &self.m.resources[r.index()];
        match res.kind {
            ResourceKind::StorageRead | ResourceKind::StorageReadWrite => {
                let Some(&i) = path.first() else {
                    return Err(Error::Unsupported("a whole buffer read at once".into()));
                };
                // A buffer the inputs don't hold is one only written here: as long as it's
                // written.
                let given = self.inputs.bytes(res);
                let bytes = given.unwrap_or(&[]);
                let stride = layout::array_stride(&self.m.types, res.ty) as usize;
                let len = if given.is_some() { bytes.len() / stride.max(1) } else { usize::MAX };
                if i >= len && !self.memory.get(&r).is_some_and(|m| m.contains_key(&(i as u32))) {
                    return Ok((zero(&self.m.types, res.ty), usize::MAX));
                }
                let types = &self.m.types;
                let el = self
                    .memory
                    .entry(r)
                    .or_default()
                    .entry(i as u32)
                    .or_insert_with(|| decode(types, res.ty, bytes, i * stride))
                    .clone();
                Ok((el, 1))
            }
            ResourceKind::Uniform { .. } | ResourceKind::Private => {
                let types = &self.m.types;
                let bytes = if res.kind == (ResourceKind::Private) {
                    &[][..]
                } else {
                    self.inputs.bytes(res).unwrap_or(&[])
                };
                let v = self
                    .memory
                    .entry(r)
                    .or_default()
                    .entry(0)
                    .or_insert_with(|| decode(types, res.ty, bytes, 0))
                    .clone();
                Ok((v, 0))
            }
            ResourceKind::Workgroup => Err(Error::Unsupported("workgroup memory".into())),
            _ => malformed("a texture or sampler as a place"),
        }
    }

    fn read(&mut self, abs: &Abs) -> Result<V> {
        let (whole, skip) = match &abs.root {
            Root::Local(d, l) => (self.frames[*d].locals[l.index()].clone(), 0),
            Root::Resource(r) => self.resource_part(*r, &abs.path)?,
        };
        if skip == usize::MAX {
            self.out.out_of_range.push(Mark { at: self.at, what: "a buffer read".into() });
            let mut v = whole;
            for &k in abs.path.iter().skip(1) {
                v = match v {
                    V::P(mut ps) if k < ps.len() => ps.swap_remove(k),
                    other => other,
                };
            }
            return Ok(v);
        }
        let mut v = whole;
        for &k in &abs.path[skip..] {
            v = match v {
                V::P(mut ps) => {
                    if k < ps.len() {
                        ps.swap_remove(k)
                    } else {
                        self.out.out_of_range.push(Mark { at: self.at, what: "a read".into() });
                        match ps.first() {
                            Some(p) => zero_like(p),
                            None => return malformed("an index into nothing"),
                        }
                    }
                }
                V::S(_) => return malformed("a projection of a scalar"),
            };
        }
        Ok(v)
    }

    fn write(&mut self, abs: &Abs, x: V) -> Result<()> {
        fn put(v: &mut V, path: &[usize], x: V) -> bool {
            match path.split_first() {
                None => {
                    *v = x;
                    true
                }
                Some((&k, rest)) => match v {
                    V::P(ps) if k < ps.len() => put(&mut ps[k], rest, x),
                    _ => false,
                },
            }
        }
        let ok = match &abs.root {
            Root::Local(d, l) => put(&mut self.frames[*d].locals[l.index()], &abs.path, x),
            Root::Resource(r) => {
                let (mut whole, skip) = self.resource_part(*r, &abs.path)?;
                if skip == usize::MAX {
                    false
                } else {
                    let done = put(&mut whole, &abs.path[skip..], x);
                    let key = if skip == 1 { abs.path[0] as u32 } else { 0 };
                    let res = &self.m.resources[r.index()];
                    if res.kind == ResourceKind::StorageReadWrite {
                        self.out.writes.retain(|w| !(w.binding == res.binding && w.index == key));
                        self.out.writes.push(Write {
                            binding: res.binding,
                            index: key,
                            value: whole.clone(),
                        });
                    }
                    self.memory.entry(*r).or_default().insert(key, whole);
                    done
                }
            }
        };
        if !ok {
            self.out.out_of_range.push(Mark { at: self.at, what: "a write".into() });
        }
        Ok(())
    }

    /// An operation's result: the folder's (WGSL's, where it's defined and finite), or its
    /// total form's, marked.
    fn eval(
        &mut self,
        what: &str,
        strict: Option<V>,
        total: impl FnOnce() -> Option<V>,
    ) -> Result<V> {
        if let Some(v) = strict {
            return Ok(v);
        }
        match total() {
            Some(v) => {
                self.mark(format!("{what} is undefined on the GPU or not finite"));
                Ok(v)
            }
            None => malformed(format!("{what} of values of the wrong kinds")),
        }
    }

    fn expr(&mut self, e: &Expr) -> Result<Option<V>> {
        let types = &self.m.types;
        Ok(Some(match e {
            Expr::Const(c) => V::S(c.clone()),
            Expr::Zero(t) => zero(types, *t),
            Expr::Param(i) => match self.frame().args.get(*i as usize) {
                Some(ArgV::Value(v)) => v.clone(),
                Some(ArgV::Ref(a)) => {
                    let a = a.clone();
                    self.read(&a)?
                }
                None => return malformed(format!("no parameter {i}")),
            },
            Expr::EntryInput(i) => {
                let entry = self
                    .m
                    .entry_points
                    .iter()
                    .find(|e| e.function == self.frame().func)
                    .ok_or_else(|| Error::Malformed("an entry input outside an entry".into()))?;
                let b = *entry
                    .inputs
                    .get(*i as usize)
                    .ok_or_else(|| Error::Malformed(format!("no entry input {i}")))?;
                let v = self
                    .inputs
                    .builtin(b)
                    .ok_or_else(|| Error::Malformed(format!("no input for {b:?}")))?;
                // The std struct that holds it: a vector's components as its fields.
                v
            }
            Expr::Load(p) => {
                let abs = self.resolve(p)?;
                self.read(&abs)?
            }
            Expr::Unary(op, x) => {
                let x = self.val(*x)?;
                let strict = each1(x.clone(), &|c| unary(*op, c));
                self.eval("a negation", strict, || each1(x, &|c| unary(*op, c)))?
            }
            Expr::Binary(op, a, b) => {
                let (a, b) = (self.val(*a)?, self.val(*b)?);
                let (ta, tb) = (self.val_ty_of(e, 0), self.val_ty_of(e, 1));
                if ta.is_some_and(|t| matches!(types.get(t), TypeDef::Matrix(..)))
                    || tb.is_some_and(|t| matches!(types.get(t), TypeDef::Matrix(..)))
                {
                    matrix(*op, &a, &b)?
                } else {
                    let strict = each2(a.clone(), b.clone(), &|x, y| binary(false, *op, x, y));
                    let what = format!("`{op:?}`");
                    self.eval(&what, strict, || each2(a, b, &|x, y| binary(true, *op, x, y)))?
                }
            }
            Expr::Builtin(b, args) => {
                let args: Vec<V> = args.iter().map(|a| self.val(*a)).collect::<Result<_>>()?;
                if matches!(b, Builtin::Dpdx | Builtin::Dpdy | Builtin::Fwidth) {
                    let j = self.derivatives;
                    self.derivatives += 1;
                    self.inputs.derivative(j, *b, &args[0])
                } else {
                    let strict = builtin(false, *b, &args);
                    let what = format!("`{}`", format!("{b:?}").to_lowercase());
                    let total = || match b {
                        Builtin::AllEqual => Some(V::S(Const::Bool(args[0] == args[1]))),
                        Builtin::CountOnes | Builtin::LeadingZeros | Builtin::TrailingZeros => {
                            each1(args[0].clone(), &|c| bits(*b, c))
                        }
                        _ => builtin(true, *b, &args),
                    };
                    self.eval(&what, strict, total)?
                }
            }
            Expr::Construct(t, parts) => {
                let parts: Vec<V> = parts.iter().map(|p| self.val(*p)).collect::<Result<_>>()?;
                match types.get(*t) {
                    // A matrix from columns, an array, a struct, a vector of scalars; a vector
                    // from smaller vectors is its components in order.
                    TypeDef::Vector(..) => V::P(
                        parts
                            .into_iter()
                            .flat_map(|p| match p {
                                V::P(ps) => ps,
                                s => vec![s],
                            })
                            .collect(),
                    ),
                    _ => V::P(parts),
                }
            }
            Expr::Variant(t, k, payload) => {
                let mut z = zero(types, *t);
                if let V::P(ps) = &mut z {
                    ps[0] = V::S(Const::U32(types.tag(*t, *k)));
                    if let Some(p) = payload {
                        ps[1 + *k as usize] = self.val(*p)?;
                    }
                }
                z
            }
            Expr::Extract(x, i) => match self.val(*x)? {
                V::P(mut ps) if (*i as usize) < ps.len() => ps.swap_remove(*i as usize),
                other => return malformed(format!("part {i} of {other:?}")),
            },
            Expr::ExtractDyn(x, i) => {
                let (x, i) = (self.val(*x)?, self.val(*i)?);
                let i = match i {
                    V::S(Const::U32(i)) => i as usize,
                    V::S(Const::I32(i)) if i >= 0 => i as usize,
                    _ => usize::MAX,
                };
                match x {
                    V::P(mut ps) if i < ps.len() => ps.swap_remove(i),
                    V::P(ps) => {
                        self.out.out_of_range.push(Mark { at: self.at, what: "an element".into() });
                        ps.first().map(zero_like).unwrap_or(V::P(Vec::new()))
                    }
                    other => return malformed(format!("an element of {other:?}")),
                }
            }
            Expr::Splat(x, n) => V::P(vec![self.val(*x)?; *n as usize]),
            Expr::Swizzle(x, comps) => match self.val(*x)? {
                V::P(ps) => V::P(
                    comps
                        .iter()
                        .map(|&c| ps.get(c as usize).cloned())
                        .collect::<Option<_>>()
                        .ok_or_else(|| Error::Malformed("a swizzle past the end".into()))?,
                ),
                other => return malformed(format!("a swizzle of {other:?}")),
            },
            Expr::Convert(x, s) => {
                let x = self.val(*x)?;
                let strict = each1(x.clone(), &|c| convert(false, c, *s));
                self.eval("a conversion", strict, || each1(x, &|c| convert(true, c, *s)))?
            }
            Expr::Bitcast(x, s) => each1(self.val(*x)?, &|c| bitcast(c, *s))
                .ok_or_else(|| Error::Malformed("a bit cast".into()))?,
            Expr::Select { cond, if_true, if_false } => {
                let (c, t, f) = (self.val(*cond)?, self.val(*if_true)?, self.val(*if_false)?);
                select(c, t, f).ok_or_else(|| Error::Malformed("a select".into()))?
            }
            Expr::Call(g, args) => {
                let mut vals = Vec::new();
                for a in args {
                    vals.push(match a {
                        Arg::Value(v) => ArgV::Value(self.val(*v)?),
                        Arg::Place(p) => ArgV::Ref(self.resolve(p)?),
                    });
                }
                return self.call(*g, vals);
            }
            Expr::ArrayLength(p) => {
                let PlaceRoot::Resource(r) = p.root else { return malformed("an array length") };
                let res = &self.m.resources[r.index()];
                let stride = layout::array_stride(types, res.ty).max(1);
                V::S(Const::U32(self.inputs.bytes(res).map_or(0, |b| b.len() as u32 / stride)))
            }
            Expr::Texture(op, tex, sampler, args) => {
                let args: Vec<V> = args.iter().map(|a| self.val(*a)).collect::<Result<_>>()?;
                return self.texture(*op, *tex, *sampler, args).map(Some);
            }
            Expr::Discard => return Ok(None),
            Expr::TextureStore(..) => {
                return Err(Error::Unsupported("a write to a storage texture".into()));
            }
            Expr::Atomic(..) => return Err(Error::Unsupported("atomics".into())),
            Expr::Subgroup(..) => return Err(Error::Unsupported("subgroup operations".into())),
            Expr::Barrier => return Err(Error::Unsupported("barriers".into())),
            Expr::Run(_) | Expr::Addr(_) | Expr::Host(..) | Expr::Mem(..) => {
                return malformed("CPU-only code");
            }
        }))
    }

    /// The type of operand `k` of a binary expression, where it can be found.
    fn val_ty_of(&self, e: &Expr, k: usize) -> Option<TypeId> {
        match e {
            Expr::Binary(_, a, b) => Some(self.func().value_ty(if k == 0 { *a } else { *b })),
            _ => None,
        }
    }

    fn texture(
        &mut self,
        op: TextureOp,
        tex: ResourceId,
        sampler: Option<ResourceId>,
        args: Vec<V>,
    ) -> Result<V> {
        let res = &self.m.resources[tex.index()];
        let ResourceKind::Texture { depth, dim, .. } = res.kind else {
            return malformed("a texture read of something else");
        };
        let texels = self
            .inputs
            .texture(res)
            .ok_or_else(|| Error::Malformed(format!("no texels for `{}`", res.name)))?
            .clone();
        let samp = sampler.and_then(|s| self.inputs.sampler(&self.m.resources[s.index()]));
        let (v, approximate) = concrete::texture(
            op,
            &texels,
            dim,
            depth,
            samp,
            &args,
            &mut |j, b, x| self.inputs.derivative(j, b, x),
            &mut self.derivatives,
        )?;
        if approximate {
            self.out.approximate.push(Mark { at: self.at, what: "an anisotropic sample".into() });
        }
        Ok(v)
    }
}

fn bits(b: Builtin, c: &Const) -> Option<Const> {
    let (x, w) = match c {
        Const::U32(x) => (u64::from(*x), 32),
        Const::I32(x) => (u64::from(*x as u32), 32),
        _ => return None,
    };
    let n = match b {
        Builtin::CountOnes => x.count_ones(),
        Builtin::LeadingZeros => (x as u32).leading_zeros().min(w),
        _ => (x as u32).trailing_zeros().min(w),
    };
    Some(Const::U32(n))
}

fn bitcast(c: &Const, to: Scalar) -> Option<Const> {
    let raw = match c {
        Const::U32(x) => *x,
        Const::I32(x) => *x as u32,
        Const::F32(x) => x.to_bits(),
        _ => return None,
    };
    Some(match to {
        Scalar::U32 => Const::U32(raw),
        Scalar::I32 => Const::I32(raw as i32),
        Scalar::F32 => Const::F32(f32::from_bits(raw)),
        _ => return None,
    })
}

fn floats(v: &V) -> Option<Vec<f32>> {
    match v {
        V::S(Const::F32(x)) => Some(vec![*x]),
        V::P(ps) => ps
            .iter()
            .map(|p| match p {
                V::S(Const::F32(x)) => Some(*x),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn cols(v: &V) -> Option<Vec<Vec<f32>>> {
    match v {
        V::P(cs) => cs.iter().map(floats).collect(),
        V::S(_) => None,
    }
}

fn fv(xs: Vec<f32>) -> V {
    V::P(xs.into_iter().map(|x| V::S(Const::F32(x))).collect())
}

/// A matrix operation: a product (matrix × vector, vector × matrix, matrix × matrix), a sum or
/// a difference, or a matrix by a scalar, as WGSL computes it in `f32`.
fn matrix(op: BinOp, a: &V, b: &V) -> Result<V> {
    let bad = || Error::Malformed("a matrix operation".into());
    let dot = |x: &[f32], y: &[f32]| x.iter().zip(y).fold(0.0f32, |s, (p, q)| s + p * q);
    match (a, b) {
        (V::P(ca), V::P(cb)) if matches!(ca.first(), Some(V::P(_))) => {
            let ma = cols(a).ok_or_else(bad)?;
            if let Some(mb) = cols(b) {
                if op == BinOp::Mul {
                    // A × B: column j is A × (B's column j).
                    let rows = ma[0].len();
                    return Ok(V::P(
                        mb.iter()
                            .map(|bc| {
                                fv((0..rows)
                                    .map(|r| {
                                        ma.iter().zip(bc).fold(0.0f32, |s, (ac, x)| s + ac[r] * x)
                                    })
                                    .collect())
                            })
                            .collect(),
                    ));
                }
                return Ok(V::P(
                    ma.iter()
                        .zip(&mb)
                        .map(|(x, y)| {
                            fv(x.iter()
                                .zip(y)
                                .map(|(p, q)| if op == BinOp::Add { p + q } else { p - q })
                                .collect())
                        })
                        .collect(),
                ));
            }
            let _ = cb;
            if let Some(v) = floats(b).filter(|v| v.len() > 1) {
                // M × v: the columns weighted by v's components.
                let rows = ma[0].len();
                return Ok(fv((0..rows)
                    .map(|r| ma.iter().zip(&v).fold(0.0f32, |s, (c, x)| s + c[r] * x))
                    .collect()));
            }
            let s = floats(b).and_then(|v| v.first().copied()).ok_or_else(bad)?;
            Ok(V::P(ma.iter().map(|c| fv(c.iter().map(|x| x * s).collect())).collect()))
        }
        (_, V::P(cb)) if matches!(cb.first(), Some(V::P(_))) => {
            let mb = cols(b).ok_or_else(bad)?;
            match floats(a) {
                // v × M: v dotted with each column.
                Some(v) if v.len() > 1 => Ok(fv(mb.iter().map(|c| dot(&v, c)).collect())),
                Some(v) => {
                    Ok(V::P(mb.iter().map(|c| fv(c.iter().map(|x| x * v[0]).collect())).collect()))
                }
                None => Err(bad()),
            }
        }
        _ => Err(bad()),
    }
}

#[cfg(test)]
mod tests;
