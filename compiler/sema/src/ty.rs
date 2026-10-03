//! Types, interned. A [`TyId`] is cheap to copy and compare; equal types have equal ids.

use elsa::FrozenVec;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

macro_rules! id_type {
    ($($(#[$m:meta])* $name:ident;)*) => {$(
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u32);
        impl $name {
            pub fn index(self) -> usize { self.0 as usize }
        }
    )*};
}
pub(crate) use id_type;

id_type! {
    /// An interned type.
    TyId;
    /// A struct or enum (user, std or built in).
    AdtId;
    /// A function: free, a method in an impl, or a method in a trait.
    FnId;
    TraitId;
    ImplId;
    ConstId;
    ModuleId;
    /// A generic parameter of some definition, including a trait's `Self`.
    ParamId;
    /// A closure expression in some function body.
    ClosureId;
    /// A type inference variable, local to one body's inference.
    VarId;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IntTy {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
}

impl IntTy {
    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::U8 => "u8",
            IntTy::I16 => "i16",
            IntTy::U16 => "u16",
            IntTy::I32 => "i32",
            IntTy::U32 => "u32",
            IntTy::I64 => "i64",
            IntTy::U64 => "u64",
        }
    }
    pub fn signed(self) -> bool {
        matches!(self, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64)
    }
    pub fn bits(self) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 => 64,
        }
    }
    /// The largest value, as a u64.
    pub fn max(self) -> u64 {
        if self.signed() {
            (1u64 << (self.bits() - 1)) - 1
        } else if self.bits() == 64 {
            u64::MAX
        } else {
            (1u64 << self.bits()) - 1
        }
    }
    /// Whether WGSL has this type (i32 and u32 only).
    pub fn on_gpu(self) -> bool {
        matches!(self, IntTy::I32 | IntTy::U32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FloatTy {
    F32,
    F64,
}

impl FloatTy {
    pub fn name(self) -> &'static str {
        match self {
            FloatTy::F32 => "f32",
            FloatTy::F64 => "f64",
        }
    }
}

/// What an unresolved inference variable may become.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VarKind {
    /// Anything.
    General,
    /// An integer literal: any integer type, or any float type (an integer literal converts
    /// to a float exactly where a float is expected, as in WGSL). Defaults to `i32`.
    Int,
    /// A float literal: any float type. Defaults to `f32`.
    Float,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TyKind {
    Bool,
    Int(IntTy),
    Float(FloatTy),
    /// `vec2`, `vec3`, `vec4`: f32 vectors.
    Vec(u8),
    /// `mat2`, `mat3`, `mat4`: square f32 matrices, column-major.
    Mat(u8),
    /// `()` is the empty tuple.
    Tuple(Vec<TyId>),
    Array(TyId, u32),
    /// `[T]`: a borrowed run of `T`.
    Slice(TyId),
    Adt(AdtId, Vec<TyId>),
    Param(ParamId),
    /// `<T as Trait<..>>::Name`, not yet resolved to a type.
    Projection {
        self_ty: TyId,
        trait_: TraitId,
        trait_args: Vec<TyId>,
        name: String,
    },
    /// A return-position trait type: the concrete type `func` returns, hidden from callers.
    Opaque(FnId, Vec<TyId>),
    /// `fn(A, B) -> R`: a parameter that accepts a closure or a function. Functions with such
    /// parameters are monomorphized per argument, like generics.
    FnPtr(Vec<TyId>, TyId),
    /// The type of a closure expression; `env` is its enclosing function's generic arguments.
    Closure(ClosureId, Vec<TyId>),
    /// A named function used as a value.
    FnDef(FnId, Vec<TyId>),
    Var(VarId),
    /// The type of `return`, `break` and `continue`: it converts to any type.
    Never,
    /// A type that already produced an error; it unifies with anything, silently.
    Error,
}

/// The type interner. Also caches the common types.
///
/// Interning takes `&self`: the table only grows, and a [`TyKind`] never moves once it's in
/// (each is boxed), so the program's definitions can be shared read-only by everything after
/// collection while types are still being made.
pub struct Types {
    kinds: FrozenVec<Box<TyKind>>,
    map: RefCell<HashMap<TyKind, TyId>>,
    /// Each type's size: the nodes of its tree, shared parts counted each time they appear
    /// (saturating).
    sizes: RefCell<Vec<u32>>,
    pub bool: TyId,
    pub i32: TyId,
    pub u32: TyId,
    pub f32: TyId,
    pub f64: TyId,
    pub unit: TyId,
    pub never: TyId,
    pub error: TyId,
    pub vec2: TyId,
    pub vec3: TyId,
    pub vec4: TyId,
}

impl Default for Types {
    fn default() -> Self {
        Self::new()
    }
}

impl Types {
    pub fn new() -> Types {
        let mut t = Types {
            kinds: FrozenVec::new(),
            map: RefCell::new(HashMap::new()),
            sizes: RefCell::new(Vec::new()),
            bool: TyId(0),
            i32: TyId(0),
            u32: TyId(0),
            f32: TyId(0),
            f64: TyId(0),
            unit: TyId(0),
            never: TyId(0),
            error: TyId(0),
            vec2: TyId(0),
            vec3: TyId(0),
            vec4: TyId(0),
        };
        t.bool = t.intern(TyKind::Bool);
        t.i32 = t.intern(TyKind::Int(IntTy::I32));
        t.u32 = t.intern(TyKind::Int(IntTy::U32));
        t.f32 = t.intern(TyKind::Float(FloatTy::F32));
        t.f64 = t.intern(TyKind::Float(FloatTy::F64));
        t.unit = t.intern(TyKind::Tuple(Vec::new()));
        t.never = t.intern(TyKind::Never);
        t.error = t.intern(TyKind::Error);
        t.vec2 = t.intern(TyKind::Vec(2));
        t.vec3 = t.intern(TyKind::Vec(3));
        t.vec4 = t.intern(TyKind::Vec(4));
        t
    }

    pub fn intern(&self, kind: TyKind) -> TyId {
        if let Some(&id) = self.map.borrow().get(&kind) {
            return id;
        }
        let id = TyId(self.kinds.len() as u32);
        let mut size = 1u32;
        children(&kind, &mut |c| size = size.saturating_add(self.size(c)));
        self.sizes.borrow_mut().push(size);
        self.kinds.push(Box::new(kind.clone()));
        self.map.borrow_mut().insert(kind, id);
        id
    }

    pub fn kind(&self, t: TyId) -> &TyKind {
        &self.kinds[t.index()]
    }

    /// The nodes of the type's tree, counting a shared part each time it appears (saturating):
    /// what a walk over it that doesn't remember where it's been would visit. See
    /// [`MAX_TYPE_SIZE`].
    pub fn size(&self, t: TyId) -> u32 {
        self.sizes.borrow()[t.index()]
    }

    /// How many types there are.
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.len() == 0
    }

    pub fn vec(&self, n: u8) -> TyId {
        self.intern(TyKind::Vec(n))
    }

    pub fn int(&self, i: IntTy) -> TyId {
        self.intern(TyKind::Int(i))
    }

    pub fn tuple(&self, elems: Vec<TyId>) -> TyId {
        self.intern(TyKind::Tuple(elems))
    }

    pub fn array(&self, elem: TyId, n: u32) -> TyId {
        self.intern(TyKind::Array(elem, n))
    }

    pub fn adt(&self, adt: AdtId, args: Vec<TyId>) -> TyId {
        self.intern(TyKind::Adt(adt, args))
    }

    pub fn param(&self, p: ParamId) -> TyId {
        self.intern(TyKind::Param(p))
    }

    pub fn var(&self, v: VarId) -> TyId {
        self.intern(TyKind::Var(v))
    }

    pub fn is_float(&self, t: TyId) -> bool {
        matches!(self.kind(t), TyKind::Float(_))
    }

    pub fn is_int(&self, t: TyId) -> bool {
        matches!(self.kind(t), TyKind::Int(_))
    }

    pub fn is_unit(&self, t: TyId) -> bool {
        t == self.unit
    }

    /// Whether a type mentions inference variables.
    pub fn has_vars(&self, t: TyId) -> bool {
        self.any(t, &mut |k| matches!(k, TyKind::Var(_)))
    }

    /// Whether a type mentions generic parameters or projections.
    pub fn has_params(&self, t: TyId) -> bool {
        self.any(t, &mut |k| matches!(k, TyKind::Param(_) | TyKind::Projection { .. }))
    }

    /// Whether any part of `t` satisfies `f`, a test of the part alone.
    pub fn any(&self, t: TyId, f: &mut impl FnMut(&TyKind) -> bool) -> bool {
        // Parts a type shares are tested once.
        let mut seen = HashSet::new();
        self.any_in(t, f, &mut seen)
    }

    fn any_in(
        &self,
        t: TyId,
        f: &mut impl FnMut(&TyKind) -> bool,
        seen: &mut HashSet<TyId>,
    ) -> bool {
        if self.size(t) > SHARING && !seen.insert(t) {
            return false;
        }
        let k = self.kind(t);
        if f(k) {
            return true;
        }
        let mut found = false;
        children(k, &mut |c| found = found || self.any_in(c, f, seen));
        found
    }

    /// Rebuilds `t`, replacing each part for which `f`, a function of the part alone, returns
    /// `Some`.
    pub fn map(&self, t: TyId, f: &mut impl FnMut(&Types, TyId) -> Option<TyId>) -> TyId {
        // Parts a type shares are rebuilt once.
        let mut done = HashMap::new();
        self.map_in(t, f, &mut done)
    }

    fn map_in(
        &self,
        t: TyId,
        f: &mut impl FnMut(&Types, TyId) -> Option<TyId>,
        done: &mut HashMap<TyId, TyId>,
    ) -> TyId {
        let shared = self.size(t) > SHARING;
        if shared && let Some(&r) = done.get(&t) {
            return r;
        }
        let r = self.map_parts(t, f, done);
        if shared {
            done.insert(t, r);
        }
        r
    }

    fn map_parts(
        &self,
        t: TyId,
        f: &mut impl FnMut(&Types, TyId) -> Option<TyId>,
        done: &mut HashMap<TyId, TyId>,
    ) -> TyId {
        if let Some(r) = f(self, t) {
            return r;
        }
        let mut m = |x: TyId| self.map_in(x, f, done);
        // The part rebuilt from its mapped children; `None` when none of them changed.
        let new = match self.kind(t) {
            TyKind::Tuple(ts) => map_list(ts, &mut m).map(TyKind::Tuple),
            TyKind::Adt(a, ts) => map_list(ts, &mut m).map(|ts| TyKind::Adt(*a, ts)),
            TyKind::Opaque(a, ts) => map_list(ts, &mut m).map(|ts| TyKind::Opaque(*a, ts)),
            TyKind::Closure(a, ts) => map_list(ts, &mut m).map(|ts| TyKind::Closure(*a, ts)),
            TyKind::FnDef(a, ts) => map_list(ts, &mut m).map(|ts| TyKind::FnDef(*a, ts)),
            TyKind::Array(e, n) => {
                let x = m(*e);
                (x != *e).then_some(TyKind::Array(x, *n))
            }
            TyKind::Slice(e) => {
                let x = m(*e);
                (x != *e).then_some(TyKind::Slice(x))
            }
            TyKind::Projection { self_ty, trait_, trait_args, name } => {
                let x = m(*self_ty);
                let args = map_list(trait_args, &mut m);
                (x != *self_ty || args.is_some()).then(|| TyKind::Projection {
                    self_ty: x,
                    trait_: *trait_,
                    trait_args: args.unwrap_or_else(|| trait_args.clone()),
                    name: name.clone(),
                })
            }
            TyKind::FnPtr(ps, r) => {
                let ps2 = map_list(ps, &mut m);
                let r2 = m(*r);
                (ps2.is_some() || r2 != *r)
                    .then(|| TyKind::FnPtr(ps2.unwrap_or_else(|| ps.clone()), r2))
            }
            _ => None,
        };
        new.map_or(t, |k| self.intern(k))
    }

    /// Replaces generic parameters by `subst`.
    pub fn subst(&self, t: TyId, subst: &Subst) -> TyId {
        if subst.is_empty() {
            return t;
        }
        self.map(t, &mut |types, x| match types.kind(x) {
            TyKind::Param(p) => subst.get(*p),
            _ => None,
        })
    }
}

impl std::fmt::Debug for Types {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Types({} interned)", self.len())
    }
}

/// The largest type a program may have, in [`Types::size`]: far past any real type (a struct
/// counts once, however many fields it has), and small enough that walking one is cheap. A
/// type can double with each line (`let b = (a, a)`), so without a bound a short program
/// could take the compiler forever (E0329).
pub const MAX_TYPE_SIZE: u32 = 4096;

/// Types larger than this are walked remembering the parts they share.
const SHARING: u32 = 64;

/// Each of `ts` mapped by `m`, in order; `None` when none of them changed.
fn map_list(ts: &[TyId], m: &mut impl FnMut(TyId) -> TyId) -> Option<Vec<TyId>> {
    let new: Vec<TyId> = ts.iter().map(|&x| m(x)).collect();
    (new != ts).then_some(new)
}

/// Calls `f` on each type `kind` is made of.
pub fn children(kind: &TyKind, f: &mut impl FnMut(TyId)) {
    match kind {
        TyKind::Tuple(ts)
        | TyKind::Adt(_, ts)
        | TyKind::Opaque(_, ts)
        | TyKind::Closure(_, ts)
        | TyKind::FnDef(_, ts) => ts.iter().for_each(|&t| f(t)),
        TyKind::Array(e, _) | TyKind::Slice(e) => f(*e),
        TyKind::Projection { self_ty, trait_args, .. } => {
            f(*self_ty);
            trait_args.iter().for_each(|&t| f(t));
        }
        TyKind::FnPtr(ps, r) => {
            ps.iter().for_each(|&t| f(t));
            f(*r);
        }
        TyKind::Bool
        | TyKind::Int(_)
        | TyKind::Float(_)
        | TyKind::Vec(_)
        | TyKind::Mat(_)
        | TyKind::Param(_)
        | TyKind::Var(_)
        | TyKind::Never
        | TyKind::Error => {}
    }
}

/// A substitution of generic parameters.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Subst {
    pairs: Vec<(ParamId, TyId)>,
}

impl Subst {
    pub fn new() -> Subst {
        Subst::default()
    }
    pub fn from_pairs(params: &[ParamId], args: &[TyId]) -> Subst {
        Subst { pairs: params.iter().copied().zip(args.iter().copied()).collect() }
    }
    pub fn insert(&mut self, p: ParamId, t: TyId) {
        if let Some(e) = self.pairs.iter_mut().find(|(q, _)| *q == p) {
            e.1 = t;
        } else {
            self.pairs.push((p, t));
        }
    }
    pub fn get(&self, p: ParamId) -> Option<TyId> {
        self.pairs.iter().find(|(q, _)| *q == p).map(|(_, t)| *t)
    }
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
}
