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

    pub fn ty(self, types: &Types) -> TyId {
        types.intern(match self {
            BuiltinTy::Bool => TyKind::Bool,
            BuiltinTy::Int(i) => TyKind::Int(i),
            BuiltinTy::Float(f) => TyKind::Float(f),
            BuiltinTy::Vec(n) => TyKind::Vec(n),
            BuiltinTy::Mat(n) => TyKind::Mat(n),
        })
    }
}

/// How a built-in function is called.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    /// `f(x)`, and `x.f()` on its first argument.
    Both,
    /// `f(x)` only.
    Free,
    /// `x.f()` only.
    Method,
}

/// The built-in functions, one row each: the variant, its name, how many arguments it takes,
/// how it's called, whether an argument whose literal type isn't settled defaults to a float,
/// and (for transcendentals) the std function that implements it on the CPU (language.md §11).
macro_rules! builtin_fns {
    ($( $(#[$m:meta])* $v:ident = $name:literal, $arity:literal, $call:ident, $float:literal, $cpu:expr; )*) => {
        /// A built-in function. Most are overloaded across scalar and vector types, like WGSL's.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum BuiltinFn {
            $( $(#[$m])* $v, )*
        }

        impl BuiltinFn {
            pub const ALL: &[BuiltinFn] = &[$(BuiltinFn::$v),*];

            pub fn name(self) -> &'static str {
                match self { $(BuiltinFn::$v => $name,)* }
            }

            /// How many arguments it takes.
            pub fn arity(self) -> usize {
                match self { $(BuiltinFn::$v => $arity,)* }
            }

            fn call(self) -> Call {
                match self { $(BuiltinFn::$v => Call::$call,)* }
            }

            /// Whether an argument that's an unresolved literal defaults to a float.
            pub fn wants_float(self) -> bool {
                match self { $(BuiltinFn::$v => $float,)* }
            }

            /// The std function that computes it on the CPU, for a transcendental.
            pub fn cpu_impl(self) -> Option<crate::defs::Lang> {
                use crate::defs::Lang;
                match self { $(BuiltinFn::$v => $cpu,)* }
            }
        }
    };
}

builtin_fns! {
    // Componentwise on f32 scalars and vectors (and f64 on the CPU).
    Sqrt = "sqrt", 1, Both, true, None;
    InverseSqrt = "inverse_sqrt", 1, Both, true, None;
    Sin = "sin", 1, Both, true, Some(Lang::CpuSin);
    Cos = "cos", 1, Both, true, Some(Lang::CpuCos);
    Tan = "tan", 1, Both, true, Some(Lang::CpuTan);
    Asin = "asin", 1, Both, true, Some(Lang::CpuAsin);
    Acos = "acos", 1, Both, true, Some(Lang::CpuAcos);
    Atan = "atan", 1, Both, true, Some(Lang::CpuAtan);
    Exp = "exp", 1, Both, true, Some(Lang::CpuExp);
    Exp2 = "exp2", 1, Both, true, Some(Lang::CpuExp2);
    Log = "log", 1, Both, true, Some(Lang::CpuLog);
    Log2 = "log2", 1, Both, true, Some(Lang::CpuLog2);
    Floor = "floor", 1, Both, true, None;
    Ceil = "ceil", 1, Both, true, None;
    Round = "round", 1, Both, true, None;
    Trunc = "trunc", 1, Both, true, None;
    Fract = "fract", 1, Both, true, None;
    Saturate = "saturate", 1, Both, true, None;
    // Two arguments of one float type.
    Atan2 = "atan2", 2, Both, true, Some(Lang::CpuAtan2);
    Pow = "pow", 2, Both, true, Some(Lang::CpuPow);
    Step = "step", 2, Both, true, None;
    // Numeric (int or float), componentwise.
    Abs = "abs", 1, Both, false, None;
    Sign = "sign", 1, Both, false, None;
    Min = "min", 2, Both, false, None;
    Max = "max", 2, Both, false, None;
    Clamp = "clamp", 3, Both, false, None;
    /// `mix(a, b, t)`: `t` is the same type as `a` and `b`, or `f32`.
    Mix = "mix", 3, Both, true, None;
    Smoothstep = "smoothstep", 3, Both, true, None;
    // Vectors.
    Length = "length", 1, Both, true, None;
    Distance = "distance", 2, Both, true, None;
    Dot = "dot", 2, Both, true, None;
    Cross = "cross", 2, Both, true, None;
    Normalize = "normalize", 1, Both, true, None;
    /// `select(if_false, if_true, cond)`.
    Select = "select", 3, Free, false, None;
    BitcastU32 = "bitcast_u32", 1, Free, true, None;
    BitcastI32 = "bitcast_i32", 1, Free, true, None;
    BitcastF32 = "bitcast_f32", 1, Free, false, None;
    /// CPU only (64-bit).
    BitcastU64 = "bitcast_u64", 1, Free, true, None;
    BitcastF64 = "bitcast_f64", 1, Free, false, None;
    /// Fragment shaders only.
    Dpdx = "dpdx", 1, Both, true, None;
    Dpdy = "dpdy", 1, Both, true, None;
    Fwidth = "fwidth", 1, Both, true, None;
    /// Integer methods: the only arithmetic that wraps on the CPU.
    WrappingAdd = "wrapping_add", 2, Method, false, None;
    WrappingSub = "wrapping_sub", 2, Method, false, None;
    WrappingMul = "wrapping_mul", 2, Method, false, None;
    /// `xs.len()` of a run or an array: its element count, a `u32`.
    Len = "len", 1, Method, false, None;
}

impl BuiltinFn {
    /// The free function of this name.
    pub fn lookup(name: &str) -> Option<BuiltinFn> {
        BuiltinFn::ALL.iter().copied().find(|b| b.name() == name && b.call() != Call::Method)
    }

    /// The built-in callable as a method of this name, on its first argument: `v.length()`,
    /// `n.wrapping_mul(3)`.
    pub fn lookup_method(name: &str) -> Option<BuiltinFn> {
        BuiltinFn::ALL.iter().copied().find(|b| b.name() == name && b.call() != Call::Free)
    }

    /// Fragment-only derivatives.
    pub fn is_derivative(self) -> bool {
        matches!(self, BuiltinFn::Dpdx | BuiltinFn::Dpdy | BuiltinFn::Fwidth)
    }

    /// Works out the result type from the argument types (all resolved, defaults applied), or
    /// says what's wrong.
    pub fn result(self, types: &Types, args: &[TyId]) -> Result<TyId, String> {
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
            Len => match types.kind(args[0]) {
                TyKind::Slice(_) | TyKind::Array(..) => Ok(types.u32),
                _ => Err(format!(
                    "`len` is a method of runs and arrays, not of `{}`",
                    show(types, args[0])
                )),
            },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_is_found_by_its_name() {
        for &b in BuiltinFn::ALL {
            let free = BuiltinFn::lookup(b.name());
            let method = BuiltinFn::lookup_method(b.name());
            assert!(free == Some(b) || method == Some(b), "{} isn't found", b.name());
            assert!(b.arity() >= 1);
        }
        assert_eq!(BuiltinFn::lookup("wrapping_add"), None);
        assert_eq!(BuiltinFn::lookup_method("select"), None);
        assert_eq!(BuiltinFn::lookup_method("len"), Some(BuiltinFn::Len));
    }
}
