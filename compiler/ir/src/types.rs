//! IR types, interned per module.

use crate::TypeId;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scalar {
    Bool,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
}

impl Scalar {
    pub fn is_float(self) -> bool {
        matches!(self, Scalar::F32 | Scalar::F64)
    }
    pub fn is_int(self) -> bool {
        !self.is_float() && self != Scalar::Bool
    }
    pub fn signed(self) -> bool {
        matches!(self, Scalar::I8 | Scalar::I16 | Scalar::I32 | Scalar::I64)
    }
    pub fn bits(self) -> u32 {
        match self {
            Scalar::Bool => 1,
            Scalar::I8 | Scalar::U8 => 8,
            Scalar::I16 | Scalar::U16 => 16,
            Scalar::I32 | Scalar::U32 | Scalar::F32 => 32,
            Scalar::I64 | Scalar::U64 | Scalar::F64 => 64,
        }
    }
    /// The bytes it takes in memory: 4 for a `bool`, as the `u32` that GPU memory holds one as.
    pub fn bytes(self) -> u32 {
        match self {
            Scalar::I8 | Scalar::U8 => 1,
            Scalar::I16 | Scalar::U16 => 2,
            Scalar::Bool | Scalar::I32 | Scalar::U32 | Scalar::F32 => 4,
            Scalar::I64 | Scalar::U64 | Scalar::F64 => 8,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Scalar::Bool => "bool",
            Scalar::I8 => "i8",
            Scalar::U8 => "u8",
            Scalar::I16 => "i16",
            Scalar::U16 => "u16",
            Scalar::I32 => "i32",
            Scalar::U32 => "u32",
            Scalar::I64 => "i64",
            Scalar::U64 => "u64",
            Scalar::F32 => "f32",
            Scalar::F64 => "f64",
        }
    }
    /// Whether WGSL has it.
    pub fn on_gpu(self) -> bool {
        matches!(self, Scalar::Bool | Scalar::I32 | Scalar::U32 | Scalar::F32)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TypeDef {
    Scalar(Scalar),
    /// A vector of 2 to 4 components: f32s, i32s, u32s, or (CPU only) f64s.
    Vector(Scalar, u8),
    /// An f32 matrix of 2 to 4 columns (the first) of 2 to 4 rows each (the second): WGSL's
    /// `matCxR`.
    Matrix(u8, u8),
    Struct {
        name: String,
        fields: Vec<(String, TypeId)>,
    },
    /// A tag and one variant's payload. Field 0 is the tag (a `u32`: variant `v`'s is
    /// `tags[v]`, its discriminant) and field `1 + v` variant `v`'s payload, if it has one. On
    /// the CPU the payloads share their memory, so the size is the tag's and the largest
    /// payload's; WGSL has no unions, so on the GPU each payload has its own member (an enum
    /// never crosses between them).
    Enum {
        name: String,
        variants: Vec<(String, Option<TypeId>)>,
        tags: Vec<u32>,
    },
    Array(TypeId, u32),
    /// `array<T>` in a storage buffer (GPU only).
    RuntimeArray(TypeId),
    /// A `[T]` parameter on the CPU: a pointer and a length.
    Run(TypeId),
    /// A pointer to a `T` (CPU only).
    Ptr(TypeId),
    /// `atomic<u32>` or `atomic<i32>` (GPU only): read and written with `Expr::Atomic`, loads
    /// and stores.
    Atomic(Scalar),
}

#[derive(Clone, Debug, Default)]
pub struct Types {
    defs: Vec<TypeDef>,
    map: HashMap<TypeDef, TypeId>,
    /// Each type's layout and its fields' offsets, worked out when it's interned from its
    /// parts' layouts (so a type that repeats a part is laid out once, not once per repetition).
    layouts: Vec<crate::layout::Layout>,
    offsets: Vec<Box<[u32]>>,
}

impl Types {
    pub fn intern(&mut self, d: TypeDef) -> TypeId {
        if let Some(&t) = self.map.get(&d) {
            return t;
        }
        // A matrix's columns and a vector's components are types too: projecting one (`m[c]`,
        // `v[i]`) looks them up.
        match d {
            TypeDef::Matrix(_, r) => {
                self.intern(TypeDef::Vector(Scalar::F32, r));
            }
            TypeDef::Vector(s, _) => {
                self.intern(TypeDef::Scalar(s));
            }
            _ => {}
        }
        let t = TypeId(self.defs.len() as u32);
        let (layout, offsets) = crate::layout::compute(self, &d);
        self.layouts.push(layout);
        self.offsets.push(offsets);
        self.defs.push(d.clone());
        self.map.insert(d, t);
        t
    }

    pub(crate) fn layout(&self, t: TypeId) -> crate::layout::Layout {
        self.layouts[t.index()]
    }

    pub(crate) fn field_offsets(&self, t: TypeId) -> &[u32] {
        &self.offsets[t.index()]
    }

    pub fn get(&self, t: TypeId) -> &TypeDef {
        &self.defs[t.index()]
    }

    /// The type `d`, if it's been interned.
    pub fn lookup(&self, d: &TypeDef) -> Option<TypeId> {
        self.map.get(d).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (TypeId, &TypeDef)> {
        self.defs.iter().enumerate().map(|(i, d)| (TypeId(i as u32), d))
    }

    pub fn scalar(&mut self, s: Scalar) -> TypeId {
        self.intern(TypeDef::Scalar(s))
    }
    pub fn bool(&mut self) -> TypeId {
        self.scalar(Scalar::Bool)
    }
    pub fn f32(&mut self) -> TypeId {
        self.scalar(Scalar::F32)
    }
    pub fn u32(&mut self) -> TypeId {
        self.scalar(Scalar::U32)
    }
    /// An f32 vector.
    pub fn vector(&mut self, n: u8) -> TypeId {
        self.intern(TypeDef::Vector(Scalar::F32, n))
    }

    pub fn vector_of(&mut self, s: Scalar, n: u8) -> TypeId {
        self.intern(TypeDef::Vector(s, n))
    }

    pub fn as_scalar(&self, t: TypeId) -> Option<Scalar> {
        match self.get(t) {
            TypeDef::Scalar(s) => Some(*s),
            _ => None,
        }
    }

    /// The scalar type of a scalar, vector or matrix: a vector's components', a matrix's f32.
    pub fn element_scalar(&self, t: TypeId) -> Option<Scalar> {
        match self.get(t) {
            TypeDef::Scalar(s) | TypeDef::Vector(s, _) => Some(*s),
            TypeDef::Matrix(..) => Some(Scalar::F32),
            _ => None,
        }
    }

    /// The tag variant `k` of enum `t` writes: its discriminant.
    pub fn tag(&self, t: TypeId, k: u32) -> u32 {
        match self.get(t) {
            TypeDef::Enum { tags, .. } => tags.get(k as usize).copied().unwrap_or(k),
            _ => k,
        }
    }

    /// The type of field `k` of a struct or an enum (see [`TypeDef::Enum`]).
    pub fn field(&self, t: TypeId, k: u32) -> Option<TypeId> {
        match self.get(t) {
            TypeDef::Struct { fields, .. } => fields.get(k as usize).map(|f| f.1),
            TypeDef::Enum { .. } if k == 0 => self.lookup(&TypeDef::Scalar(Scalar::U32)),
            TypeDef::Enum { variants, .. } => variants.get(k as usize - 1)?.1,
            _ => None,
        }
    }

    /// The type of constant part `k` of a value of type `t`: a struct's or an enum's field (see
    /// [`TypeDef::Enum`]), a vector's component, a matrix's column, an array's element, or a
    /// run's word (its address, then its length). `None` if `t` has no such part, or its type
    /// was never interned.
    pub fn part(&self, t: TypeId, k: u32) -> Option<TypeId> {
        match self.get(t) {
            TypeDef::Struct { .. } | TypeDef::Enum { .. } => self.field(t, k),
            TypeDef::Vector(s, n) if k < u32::from(*n) => self.lookup(&TypeDef::Scalar(*s)),
            TypeDef::Matrix(c, r) if k < u32::from(*c) => {
                self.lookup(&TypeDef::Vector(Scalar::F32, *r))
            }
            TypeDef::Array(e, n) if k < *n => Some(*e),
            TypeDef::Run(_) if k < 2 => self.lookup(&TypeDef::Scalar(Scalar::U32)),
            _ => None,
        }
    }

    pub fn is_vector(&self, t: TypeId) -> bool {
        matches!(self.get(t), TypeDef::Vector(..))
    }

    /// Whether the type lives in memory on the CPU (everything but scalars and pointers).
    pub fn is_aggregate(&self, t: TypeId) -> bool {
        !matches!(self.get(t), TypeDef::Scalar(_) | TypeDef::Ptr(_))
    }

    /// Whether any part of the type is a float (so a derivative or interval of it means
    /// something).
    pub fn has_float(&self, t: TypeId) -> bool {
        match self.get(t) {
            TypeDef::Scalar(s) => s.is_float(),
            TypeDef::Atomic(_) => false,
            TypeDef::Vector(s, _) => s.is_float(),
            TypeDef::Matrix(..) => true,
            TypeDef::Struct { fields, .. } => fields.iter().any(|(_, f)| self.has_float(*f)),
            TypeDef::Enum { variants, .. } => {
                variants.iter().any(|(_, p)| p.is_some_and(|p| self.has_float(p)))
            }
            TypeDef::Array(e, _) | TypeDef::RuntimeArray(e) | TypeDef::Run(e) | TypeDef::Ptr(e) => {
                self.has_float(*e)
            }
        }
    }

    pub fn display(&self, t: TypeId) -> String {
        match self.get(t) {
            TypeDef::Scalar(s) => s.name().to_string(),
            TypeDef::Vector(s, n) => match s {
                Scalar::F32 => format!("vec{n}"),
                Scalar::F64 => format!("vec{n}d"),
                Scalar::I32 => format!("vec{n}i"),
                Scalar::U32 => format!("vec{n}u"),
                _ => format!("vec{n}<{}>", s.name()),
            },
            TypeDef::Matrix(c, r) if c == r => format!("mat{c}"),
            TypeDef::Matrix(c, r) => format!("mat{c}x{r}"),
            TypeDef::Struct { name, .. } | TypeDef::Enum { name, .. } => name.clone(),
            TypeDef::Array(e, n) => format!("[{}; {n}]", self.display(*e)),
            TypeDef::RuntimeArray(e) => format!("[{}]", self.display(*e)),
            TypeDef::Run(e) => format!("run[{}]", self.display(*e)),
            TypeDef::Ptr(e) => format!("*{}", self.display(*e)),
            TypeDef::Atomic(s) => format!("atomic<{}>", s.name()),
        }
    }
}
