//! Types, interned. A [`TyId`] is cheap to copy and compare; equal types have equal ids.

use std::collections::HashMap;
use std::fmt::Write;

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
#[derive(Debug)]
pub struct Types {
    kinds: Vec<TyKind>,
    map: HashMap<TyKind, TyId>,
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
            kinds: Vec::new(),
            map: HashMap::new(),
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

    pub fn intern(&mut self, kind: TyKind) -> TyId {
        if let Some(&id) = self.map.get(&kind) {
            return id;
        }
        let id = TyId(self.kinds.len() as u32);
        self.kinds.push(kind.clone());
        self.map.insert(kind, id);
        id
    }

    pub fn kind(&self, t: TyId) -> &TyKind {
        &self.kinds[t.index()]
    }

    pub fn vec(&mut self, n: u8) -> TyId {
        self.intern(TyKind::Vec(n))
    }

    pub fn int(&mut self, i: IntTy) -> TyId {
        self.intern(TyKind::Int(i))
    }

    pub fn tuple(&mut self, elems: Vec<TyId>) -> TyId {
        self.intern(TyKind::Tuple(elems))
    }

    pub fn array(&mut self, elem: TyId, n: u32) -> TyId {
        self.intern(TyKind::Array(elem, n))
    }

    pub fn adt(&mut self, adt: AdtId, args: Vec<TyId>) -> TyId {
        self.intern(TyKind::Adt(adt, args))
    }

    pub fn param(&mut self, p: ParamId) -> TyId {
        self.intern(TyKind::Param(p))
    }

    pub fn var(&mut self, v: VarId) -> TyId {
        self.intern(TyKind::Var(v))
    }

    pub fn is_float(&self, t: TyId) -> bool {
        matches!(self.kind(t), TyKind::Float(_))
    }

    pub fn is_int(&self, t: TyId) -> bool {
        matches!(self.kind(t), TyKind::Int(_))
    }

    pub fn is_scalar(&self, t: TyId) -> bool {
        matches!(self.kind(t), TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool)
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

    /// Whether any part of `t` satisfies `f`.
    pub fn any(&self, t: TyId, f: &mut impl FnMut(&TyKind) -> bool) -> bool {
        let k = self.kind(t);
        if f(k) {
            return true;
        }
        match k {
            TyKind::Tuple(ts)
            | TyKind::Adt(_, ts)
            | TyKind::Opaque(_, ts)
            | TyKind::Closure(_, ts)
            | TyKind::FnDef(_, ts) => ts.clone().iter().any(|&t| self.any(t, f)),
            TyKind::Array(e, _) | TyKind::Slice(e) => self.any(*e, f),
            TyKind::Projection { self_ty, trait_args, .. } => {
                let (s, a) = (*self_ty, trait_args.clone());
                self.any(s, f) || a.iter().any(|&t| self.any(t, f))
            }
            TyKind::FnPtr(ps, r) => {
                let (ps, r) = (ps.clone(), *r);
                ps.iter().any(|&t| self.any(t, f)) || self.any(r, f)
            }
            _ => false,
        }
    }

    /// Rebuilds `t`, replacing each part for which `f` returns `Some`.
    pub fn map(&mut self, t: TyId, f: &mut impl FnMut(&mut Types, TyId) -> Option<TyId>) -> TyId {
        if let Some(r) = f(self, t) {
            return r;
        }
        let k = self.kind(t).clone();
        let new = match k {
            TyKind::Tuple(ts) => TyKind::Tuple(ts.iter().map(|&x| self.map(x, f)).collect()),
            TyKind::Adt(a, ts) => TyKind::Adt(a, ts.iter().map(|&x| self.map(x, f)).collect()),
            TyKind::Opaque(a, ts) => {
                TyKind::Opaque(a, ts.iter().map(|&x| self.map(x, f)).collect())
            }
            TyKind::Closure(a, ts) => {
                TyKind::Closure(a, ts.iter().map(|&x| self.map(x, f)).collect())
            }
            TyKind::FnDef(a, ts) => TyKind::FnDef(a, ts.iter().map(|&x| self.map(x, f)).collect()),
            TyKind::Array(e, n) => TyKind::Array(self.map(e, f), n),
            TyKind::Slice(e) => TyKind::Slice(self.map(e, f)),
            TyKind::Projection { self_ty, trait_, trait_args, name } => TyKind::Projection {
                self_ty: self.map(self_ty, f),
                trait_,
                trait_args: trait_args.iter().map(|&x| self.map(x, f)).collect(),
                name,
            },
            TyKind::FnPtr(ps, r) => {
                TyKind::FnPtr(ps.iter().map(|&x| self.map(x, f)).collect(), self.map(r, f))
            }
            other => return self.intern(other),
        };
        self.intern(new)
    }

    /// Replaces generic parameters by `subst`.
    pub fn subst(&mut self, t: TyId, subst: &Subst) -> TyId {
        if subst.is_empty() {
            return t;
        }
        self.map(t, &mut |types, x| match types.kind(x) {
            TyKind::Param(p) => subst.get(*p),
            _ => None,
        })
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
    pub fn extend(&mut self, other: &Subst) {
        for &(p, t) in &other.pairs {
            self.insert(p, t);
        }
    }
    pub fn pairs(&self) -> &[(ParamId, TyId)] {
        &self.pairs
    }
}

/// How a type is shown in diagnostics; names come from the program.
pub trait TyNames {
    fn adt_name(&self, a: AdtId) -> String;
    fn param_name(&self, p: ParamId) -> String;
    fn fn_name(&self, f: FnId) -> String;
    fn trait_name(&self, t: TraitId) -> String;
}

/// Longer type names are cut here, so diagnostics stay readable.
const MAX_TYPE_TEXT: usize = 200;

pub fn display(types: &Types, names: &dyn TyNames, t: TyId) -> String {
    let mut s = String::new();
    write_ty(types, names, t, &mut s);
    if s.len() > MAX_TYPE_TEXT {
        let mut cut = MAX_TYPE_TEXT;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push('…');
    }
    s
}

fn write_list(types: &Types, names: &dyn TyNames, ts: &[TyId], s: &mut String) {
    for (i, &t) in ts.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        write_ty(types, names, t, s);
        if s.len() > MAX_TYPE_TEXT {
            return;
        }
    }
}

fn write_ty(types: &Types, names: &dyn TyNames, t: TyId, s: &mut String) {
    if s.len() > MAX_TYPE_TEXT {
        return;
    }
    match types.kind(t) {
        TyKind::Bool => s.push_str("bool"),
        TyKind::Int(i) => s.push_str(i.name()),
        TyKind::Float(f) => s.push_str(f.name()),
        TyKind::Vec(n) => {
            let _ = write!(s, "vec{n}");
        }
        TyKind::Mat(n) => {
            let _ = write!(s, "mat{n}");
        }
        TyKind::Tuple(ts) => {
            s.push('(');
            write_list(types, names, ts, s);
            if ts.len() == 1 {
                s.push(',');
            }
            s.push(')');
        }
        TyKind::Array(e, n) => {
            s.push('[');
            write_ty(types, names, *e, s);
            let _ = write!(s, "; {n}]");
        }
        TyKind::Slice(e) => {
            s.push('[');
            write_ty(types, names, *e, s);
            s.push(']');
        }
        TyKind::Adt(a, args) => {
            s.push_str(&names.adt_name(*a));
            if !args.is_empty() {
                s.push('<');
                write_list(types, names, args, s);
                s.push('>');
            }
        }
        TyKind::Param(p) => s.push_str(&names.param_name(*p)),
        TyKind::Projection { self_ty, name, .. } => {
            write_ty(types, names, *self_ty, s);
            s.push_str("::");
            s.push_str(name);
        }
        TyKind::Opaque(f, _) => {
            let _ = write!(s, "<the type `{}` returns>", names.fn_name(*f));
        }
        TyKind::FnPtr(ps, r) => {
            s.push_str("fn(");
            write_list(types, names, ps, s);
            s.push(')');
            if *r != types.unit {
                s.push_str(" -> ");
                write_ty(types, names, *r, s);
            }
        }
        TyKind::Closure(..) => s.push_str("<closure>"),
        TyKind::FnDef(f, _) => {
            let _ = write!(s, "fn {}", names.fn_name(*f));
        }
        TyKind::Var(_) => s.push('_'),
        TyKind::Never => s.push('!'),
        TyKind::Error => s.push_str("{error}"),
    }
}
