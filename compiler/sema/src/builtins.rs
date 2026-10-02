//! The built-in types and functions: the part of the closed list of compiler-known items
//! (language.md §17) that isn't written in wrela. They're in scope everywhere (the prelude).

use crate::ty::{FloatTy, IntTy, TyId, TyKind, Types};

/// A type name in the prelude.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltinTy {
    Bool,
    Int(IntTy),
    Float(FloatTy),
    Vec(u8),
    Mat(u8),
}

impl BuiltinTy {
    pub fn lookup(name: &str) -> Option<BuiltinTy> {
        Some(match name {
            "bool" => BuiltinTy::Bool,
            "i8" => BuiltinTy::Int(IntTy::I8),
            "u8" => BuiltinTy::Int(IntTy::U8),
            "i16" => BuiltinTy::Int(IntTy::I16),
            "u16" => BuiltinTy::Int(IntTy::U16),
            "i32" => BuiltinTy::Int(IntTy::I32),
            "u32" => BuiltinTy::Int(IntTy::U32),
            "i64" => BuiltinTy::Int(IntTy::I64),
            "u64" => BuiltinTy::Int(IntTy::U64),
            "f32" => BuiltinTy::Float(FloatTy::F32),
            "f64" => BuiltinTy::Float(FloatTy::F64),
            "vec2" => BuiltinTy::Vec(2),
            "vec3" => BuiltinTy::Vec(3),
            "vec4" => BuiltinTy::Vec(4),
            "mat2" => BuiltinTy::Mat(2),
            "mat3" => BuiltinTy::Mat(3),
            "mat4" => BuiltinTy::Mat(4),
            _ => return None,
        })
    }

    pub fn ty(self, types: &mut Types) -> TyId {
        types.intern(match self {
            BuiltinTy::Bool => TyKind::Bool,
            BuiltinTy::Int(i) => TyKind::Int(i),
            BuiltinTy::Float(f) => TyKind::Float(f),
            BuiltinTy::Vec(n) => TyKind::Vec(n),
            BuiltinTy::Mat(n) => TyKind::Mat(n),
        })
    }
}

/// A built-in function. Most are overloaded across scalar and vector types, like WGSL's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BuiltinFn {
    // Componentwise on f32 scalars and vectors (and f64 on the CPU).
    Sqrt,
    InverseSqrt,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Exp,
    Exp2,
    Log,
    Log2,
    Floor,
    Ceil,
    Round,
    Trunc,
    Fract,
    Saturate,
    // Two arguments of one float type.
    Atan2,
    Pow,
    Step,
    // Numeric (int or float), componentwise.
    Abs,
    Sign,
    Min,
    Max,
    Clamp,
    // mix(a, b, t): t is the same type as a and b, or f32.
    Mix,
    Smoothstep,
    // Vectors.
    Length,
    Distance,
    Dot,
    Cross,
    Normalize,
    /// select(if_false, if_true, cond)
    Select,
    BitcastU32,
    BitcastI32,
    BitcastF32,
    /// CPU only (64-bit).
    BitcastU64,
    BitcastF64,
    /// Fragment shaders only.
    Dpdx,
    Dpdy,
    Fwidth,
    /// Integer methods: the only arithmetic that wraps on the CPU.
    WrappingAdd,
    WrappingSub,
    WrappingMul,
}

impl BuiltinFn {
    /// The free functions, by name. (Wrapping arithmetic is method-only.)
    pub fn lookup(name: &str) -> Option<BuiltinFn> {
        use BuiltinFn::*;
        Some(match name {
            "sqrt" => Sqrt,
            "inverse_sqrt" => InverseSqrt,
            "sin" => Sin,
            "cos" => Cos,
            "tan" => Tan,
            "asin" => Asin,
            "acos" => Acos,
            "atan" => Atan,
            "exp" => Exp,
            "exp2" => Exp2,
            "log" => Log,
            "log2" => Log2,
            "floor" => Floor,
            "ceil" => Ceil,
            "round" => Round,
            "trunc" => Trunc,
            "fract" => Fract,
            "saturate" => Saturate,
            "atan2" => Atan2,
            "pow" => Pow,
            "step" => Step,
            "abs" => Abs,
            "sign" => Sign,
            "min" => Min,
            "max" => Max,
            "clamp" => Clamp,
            "mix" => Mix,
            "smoothstep" => Smoothstep,
            "length" => Length,
            "distance" => Distance,
            "dot" => Dot,
            "cross" => Cross,
            "normalize" => Normalize,
            "select" => Select,
            "bitcast_u32" => BitcastU32,
            "bitcast_i32" => BitcastI32,
            "bitcast_f32" => BitcastF32,
            "bitcast_u64" => BitcastU64,
            "bitcast_f64" => BitcastF64,
            "dpdx" => Dpdx,
            "dpdy" => Dpdy,
            "fwidth" => Fwidth,
            _ => return None,
        })
    }

    /// Builtins callable as methods on their first argument: `v.length()`, `n.wrapping_mul(3)`.
    pub fn lookup_method(name: &str) -> Option<BuiltinFn> {
        use BuiltinFn::*;
        match name {
            "wrapping_add" => Some(WrappingAdd),
            "wrapping_sub" => Some(WrappingSub),
            "wrapping_mul" => Some(WrappingMul),
            "select" | "bitcast_u32" | "bitcast_i32" | "bitcast_f32" | "bitcast_u64"
            | "bitcast_f64" => None,
            _ => BuiltinFn::lookup(name),
        }
    }

    pub fn name(self) -> &'static str {
        use BuiltinFn::*;
        match self {
            Sqrt => "sqrt",
            InverseSqrt => "inverse_sqrt",
            Sin => "sin",
            Cos => "cos",
            Tan => "tan",
            Asin => "asin",
            Acos => "acos",
            Atan => "atan",
            Exp => "exp",
            Exp2 => "exp2",
            Log => "log",
            Log2 => "log2",
            Floor => "floor",
            Ceil => "ceil",
            Round => "round",
            Trunc => "trunc",
            Fract => "fract",
            Saturate => "saturate",
            Atan2 => "atan2",
            Pow => "pow",
            Step => "step",
            Abs => "abs",
            Sign => "sign",
            Min => "min",
            Max => "max",
            Clamp => "clamp",
            Mix => "mix",
            Smoothstep => "smoothstep",
            Length => "length",
            Distance => "distance",
            Dot => "dot",
            Cross => "cross",
            Normalize => "normalize",
            Select => "select",
            BitcastU32 => "bitcast_u32",
            BitcastI32 => "bitcast_i32",
            BitcastF32 => "bitcast_f32",
            BitcastU64 => "bitcast_u64",
            BitcastF64 => "bitcast_f64",
            Dpdx => "dpdx",
            Dpdy => "dpdy",
            Fwidth => "fwidth",
            WrappingAdd => "wrapping_add",
            WrappingSub => "wrapping_sub",
            WrappingMul => "wrapping_mul",
        }
    }

    /// How many arguments it takes.
    pub fn arity(self) -> usize {
        use BuiltinFn::*;
        match self {
            Atan2 | Pow | Step | Min | Max | Distance | Dot | Cross | WrappingAdd | WrappingSub
            | WrappingMul => 2,
            Clamp | Mix | Smoothstep | Select => 3,
            _ => 1,
        }
    }

    /// Fragment-only derivatives.
    pub fn is_derivative(self) -> bool {
        matches!(self, BuiltinFn::Dpdx | BuiltinFn::Dpdy | BuiltinFn::Fwidth)
    }

    /// Works out the result type from the argument types (all resolved, defaults applied), or
    /// says what's wrong.
    pub fn result(self, types: &mut Types, args: &[TyId]) -> Result<TyId, String> {
        use BuiltinFn::*;
        let float_like =
            |types: &Types, t: TyId| matches!(types.kind(t), TyKind::Float(_) | TyKind::Vec(_));
        let numeric = |types: &Types, t: TyId| {
            matches!(types.kind(t), TyKind::Float(_) | TyKind::Vec(_) | TyKind::Int(_))
        };
        let show = |types: &Types, t: TyId| match types.kind(t) {
            TyKind::Bool => "bool".to_string(),
            TyKind::Int(i) => i.name().to_string(),
            TyKind::Float(f) => f.name().to_string(),
            TyKind::Vec(n) => format!("vec{n}"),
            TyKind::Mat(n) => format!("mat{n}"),
            _ => "this type".to_string(),
        };
        let same = |types: &Types, args: &[TyId]| -> Result<TyId, String> {
            if args.windows(2).all(|w| w[0] == w[1]) {
                Ok(args[0])
            } else {
                Err(format!(
                    "`{}` needs arguments of one type, not {}",
                    self.name(),
                    args.iter()
                        .map(|&t| format!("`{}`", show(types, t)))
                        .collect::<Vec<_>>()
                        .join(" and ")
                ))
            }
        };
        match self {
            Sqrt | InverseSqrt | Sin | Cos | Tan | Asin | Acos | Atan | Exp | Exp2 | Log | Log2
            | Floor | Ceil | Round | Trunc | Fract | Saturate | Normalize | Dpdx | Dpdy
            | Fwidth => {
                let t = args[0];
                if !float_like(types, t) {
                    return Err(format!(
                        "`{}` takes a float or float vector, not `{}`",
                        self.name(),
                        show(types, t)
                    ));
                }
                if self == Normalize && !matches!(types.kind(t), TyKind::Vec(_)) {
                    return Err(format!("`normalize` takes a vector, not `{}`", show(types, t)));
                }
                Ok(t)
            }
            Atan2 | Pow | Step => {
                let t = same(types, args)?;
                if !float_like(types, t) {
                    return Err(format!(
                        "`{}` takes floats or float vectors, not `{}`",
                        self.name(),
                        show(types, t)
                    ));
                }
                Ok(t)
            }
            Abs | Sign => {
                let t = args[0];
                if !numeric(types, t) {
                    return Err(format!(
                        "`{}` takes a number or vector, not `{}`",
                        self.name(),
                        show(types, t)
                    ));
                }
                if self == Sign && matches!(types.kind(t), TyKind::Int(i) if !i.signed()) {
                    return Err(
                        "`sign` of an unsigned integer is always 0 or 1; compare with 0 instead"
                            .into(),
                    );
                }
                Ok(t)
            }
            Min | Max | Clamp => {
                let t = same(types, args)?;
                if !numeric(types, t) {
                    return Err(format!(
                        "`{}` takes numbers or vectors, not `{}`",
                        self.name(),
                        show(types, t)
                    ));
                }
                Ok(t)
            }
            Mix | Smoothstep => {
                // mix(a, b, t) and smoothstep(e0, e1, x): the first two match; the third is the
                // same type, or an f32 applied to every component (mix only).
                let t = same(types, &args[..2])?;
                if !float_like(types, t) {
                    return Err(format!(
                        "`{}` takes floats or float vectors, not `{}`",
                        self.name(),
                        show(types, t)
                    ));
                }
                let third = args[2];
                if third == t
                    || (self == Mix
                        && third == types.f32
                        && matches!(types.kind(t), TyKind::Vec(_)))
                {
                    Ok(t)
                } else {
                    Err(format!(
                        "the last argument of `{}` must be `{}`{}",
                        self.name(),
                        show(types, t),
                        if self == Mix && matches!(types.kind(t), TyKind::Vec(_)) {
                            " or `f32`"
                        } else {
                            ""
                        }
                    ))
                }
            }
            Length => {
                if !float_like(types, args[0]) {
                    return Err(format!(
                        "`length` takes a float or vector, not `{}`",
                        show(types, args[0])
                    ));
                }
                Ok(scalar_of(types, args[0]))
            }
            Distance | Dot => {
                let t = same(types, args)?;
                if !float_like(types, t) {
                    return Err(format!(
                        "`{}` takes float vectors, not `{}`",
                        self.name(),
                        show(types, t)
                    ));
                }
                if self == Dot && !matches!(types.kind(t), TyKind::Vec(_)) {
                    return Err(format!("`dot` takes vectors, not `{}`", show(types, t)));
                }
                Ok(scalar_of(types, t))
            }
            Cross => {
                let t = same(types, args)?;
                if t != types.vec3 {
                    return Err(format!("`cross` takes two `vec3`s, not `{}`", show(types, t)));
                }
                Ok(t)
            }
            Select => {
                let t = same(types, &args[..2])?;
                if args[2] != types.bool {
                    return Err(format!(
                        "the condition of `select` must be `bool`, not `{}`",
                        show(types, args[2])
                    ));
                }
                Ok(t)
            }
            BitcastU32 | BitcastI32 => {
                if args[0] != types.f32 {
                    return Err(format!(
                        "`{}` takes an `f32`, not `{}`",
                        self.name(),
                        show(types, args[0])
                    ));
                }
                Ok(if self == BitcastU32 { types.u32 } else { types.i32 })
            }
            BitcastF32 => {
                if args[0] != types.u32 && args[0] != types.i32 {
                    return Err(format!(
                        "`bitcast_f32` takes a `u32` or `i32`, not `{}`",
                        show(types, args[0])
                    ));
                }
                Ok(types.f32)
            }
            BitcastU64 => {
                if args[0] != types.f64 {
                    return Err(format!(
                        "`bitcast_u64` takes an `f64`, not `{}`",
                        show(types, args[0])
                    ));
                }
                Ok(types.int(IntTy::U64))
            }
            BitcastF64 => {
                let u64_ty = types.int(IntTy::U64);
                if args[0] != u64_ty {
                    return Err(format!(
                        "`bitcast_f64` takes a `u64`, not `{}`",
                        show(types, args[0])
                    ));
                }
                Ok(types.f64)
            }
            WrappingAdd | WrappingSub | WrappingMul => {
                let t = same(types, args)?;
                if !types.is_int(t) {
                    return Err(format!(
                        "`{}` takes integers, not `{}`",
                        self.name(),
                        show(types, t)
                    ));
                }
                Ok(t)
            }
        }
    }
}

/// The scalar type of a scalar or vector type.
pub fn scalar_of(types: &Types, t: TyId) -> TyId {
    match types.kind(t) {
        TyKind::Vec(_) | TyKind::Mat(_) => types.f32,
        _ => t,
    }
}

/// What the type of an argument should default to when it's an unresolved literal: whether the
/// builtin wants floats.
pub fn wants_float(f: BuiltinFn) -> bool {
    !matches!(
        f,
        BuiltinFn::Abs
            | BuiltinFn::Sign
            | BuiltinFn::Min
            | BuiltinFn::Max
            | BuiltinFn::Clamp
            | BuiltinFn::Select
            | BuiltinFn::BitcastF32
            | BuiltinFn::BitcastF64
            | BuiltinFn::WrappingAdd
            | BuiltinFn::WrappingSub
            | BuiltinFn::WrappingMul
    )
}
