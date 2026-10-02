//! IR types, interned per module.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(pub u32);

impl TypeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

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
    /// An f32 vector of 2 to 4 components.
    Vector(u8),
    /// A square f32 matrix of 2 to 4 columns.
    Matrix(u8),
    Struct {
        name: String,
        fields: Vec<(String, TypeId)>,
    },
    Array(TypeId, u32),
    /// `array<T>` in a storage buffer (GPU only).
    RuntimeArray(TypeId),
    /// A `[T]` parameter on the CPU: a pointer and a length.
    Run(TypeId),
    /// A pointer to a `T` (CPU only).
    Ptr(TypeId),
}

#[derive(Clone, Debug, Default)]
pub struct Types {
    defs: Vec<TypeDef>,
    map: HashMap<TypeDef, TypeId>,
}

impl Types {
    pub fn intern(&mut self, d: TypeDef) -> TypeId {
        if let Some(&t) = self.map.get(&d) {
            return t;
        }
        let t = TypeId(self.defs.len() as u32);
        self.defs.push(d.clone());
        self.map.insert(d, t);
        t
    }

    pub fn get(&self, t: TypeId) -> &TypeDef {
        &self.defs[t.index()]
    }

    /// The type `d`, if it's been interned.
    pub fn lookup(&self, d: &TypeDef) -> Option<TypeId> {
        self.map.get(d).copied()
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
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
    pub fn i32(&mut self) -> TypeId {
        self.scalar(Scalar::I32)
    }
    pub fn vector(&mut self, n: u8) -> TypeId {
        self.intern(TypeDef::Vector(n))
    }

    pub fn as_scalar(&self, t: TypeId) -> Option<Scalar> {
        match self.get(t) {
            TypeDef::Scalar(s) => Some(*s),
            _ => None,
        }
    }

    /// The scalar type of a scalar, vector or matrix: f32 for vectors and matrices.
    pub fn element_scalar(&self, t: TypeId) -> Option<Scalar> {
        match self.get(t) {
            TypeDef::Scalar(s) => Some(*s),
            TypeDef::Vector(_) | TypeDef::Matrix(_) => Some(Scalar::F32),
            _ => None,
        }
    }

    pub fn is_float_like(&self, t: TypeId) -> bool {
        matches!(
            self.get(t),
            TypeDef::Vector(_) | TypeDef::Matrix(_) | TypeDef::Scalar(Scalar::F32 | Scalar::F64)
        )
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
            TypeDef::Vector(_) | TypeDef::Matrix(_) => true,
            TypeDef::Struct { fields, .. } => fields.iter().any(|(_, f)| self.has_float(*f)),
            TypeDef::Array(e, _) | TypeDef::RuntimeArray(e) | TypeDef::Run(e) | TypeDef::Ptr(e) => {
                self.has_float(*e)
            }
        }
    }

    pub fn display(&self, t: TypeId) -> String {
        match self.get(t) {
            TypeDef::Scalar(s) => s.name().to_string(),
            TypeDef::Vector(n) => format!("vec{n}"),
            TypeDef::Matrix(n) => format!("mat{n}"),
            TypeDef::Struct { name, .. } => name.clone(),
            TypeDef::Array(e, n) => format!("[{}; {n}]", self.display(*e)),
            TypeDef::RuntimeArray(e) => format!("[{}]", self.display(*e)),
            TypeDef::Run(e) => format!("run[{}]", self.display(*e)),
            TypeDef::Ptr(e) => format!("*{}", self.display(*e)),
        }
    }
}
