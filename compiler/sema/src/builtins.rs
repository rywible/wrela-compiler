//! The built-in types and functions: the part of the closed list of compiler-known items
//! (language.md §17) that isn't written in wrela. They're in scope everywhere (the prelude).

use crate::program::Program;
use crate::ty::{FloatTy, IntTy, TyId, TyKind, Types, VecElem};

/// A type name in the prelude.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltinTy {
    Bool,
    Int(IntTy),
    Float(FloatTy),
    Vec(VecElem, u8),
    Mat(u8),
    /// `str`: a borrowed run of UTF-8.
    Str,
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
            "mat2" => BuiltinTy::Mat(2),
            "mat3" => BuiltinTy::Mat(3),
            "mat4" => BuiltinTy::Mat(4),
            "str" => BuiltinTy::Str,
            _ => return Self::vec(name),
        })
    }

    /// `vecN` with its element's suffix (`vec3i`), for N from 2 to 4.
    fn vec(name: &str) -> Option<BuiltinTy> {
        let rest = name.strip_prefix("vec")?;
        let n = rest.chars().next()?.to_digit(10).filter(|n| (2..=4).contains(n))?;
        let e = VecElem::ALL.into_iter().find(|e| e.suffix() == &rest[1..])?;
        Some(BuiltinTy::Vec(e, n as u8))
    }

    pub fn ty(self, types: &Types) -> TyId {
        types.intern(match self {
            BuiltinTy::Bool => TyKind::Bool,
            BuiltinTy::Int(i) => TyKind::Int(i),
            BuiltinTy::Float(f) => TyKind::Float(f),
            BuiltinTy::Vec(e, n) => TyKind::Vec(e, n),
            BuiltinTy::Mat(n) => TyKind::Mat(n),
            BuiltinTy::Str => TyKind::Str,
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

impl BuiltinFn {
    /// The names of its parameters, for a built-in whose arguments can be named. Only
    /// `select`'s: its order (the value for `false` first) is easy to swap, so a call names
    /// them (W0005).
    pub fn param_names(self) -> Option<&'static [&'static str]> {
        match self {
            BuiltinFn::Select => Some(&["if_false", "if_true", "cond"]),
            _ => None,
        }
    }
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
    Sinh = "sinh", 1, Both, true, Some(Lang::CpuSinh);
    Cosh = "cosh", 1, Both, true, Some(Lang::CpuCosh);
    Tanh = "tanh", 1, Both, true, Some(Lang::CpuTanh);
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
    /// Integer methods: how many bits are set, and how many zeros lead and trail; a `u32`.
    CountOnes = "count_ones", 1, Method, false, None;
    LeadingZeros = "leading_zeros", 1, Method, false, None;
    TrailingZeros = "trailing_zeros", 1, Method, false, None;
    /// `xs.len()` of a run or an array: its element count, a `u32`.
    Len = "len", 1, Method, false, None;
    /// `panic(message)`: a bug. Traps with the message (§15); its type is `!`.
    Panic = "panic", 1, Free, false, None;
    /// `assert(cond, message)`: panics with the message when `cond` is false.
    Assert = "assert", 2, Free, false, None;
    /// `embed("path")`: the file at `path` in the package, read by the build, as `Bytes` (§10).
    Embed = "embed", 1, Free, false, None;
}

/// E0702: a transcendental (`what`: `sin`, `**`) of an `f64`. std computes them on the CPU in
/// `f32` (language.md §11), and GPU code has no `f64`.
pub fn f64_math(what: &str, span: wrela_diag::Span) -> wrela_diag::Diagnostic {
    wrela_diag::Diagnostic::new(
        wrela_diag::codes::E0702,
        span,
        format!("`{what}` of an `f64` isn't supported yet"),
    )
    .with_note("std's CPU math works in `f32`; convert with `f32(x)`")
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

    /// Works out the result type from the argument types (all resolved, defaults applied), or
    /// says what's wrong.
    pub fn result(self, p: &Program, args: &[TyId]) -> Result<TyId, String> {
        use BuiltinFn::*;
        let types = &p.types;
        let float_like = |t: TyId| match types.kind(t) {
            TyKind::Float(_) => true,
            TyKind::Vec(e, _) => e.is_float(),
            _ => false,
        };
        let numeric =
            |t: TyId| float_like(t) || types.is_int(t) || matches!(types.kind(t), TyKind::Vec(..));
        let is_vec = |t: TyId| matches!(types.kind(t), TyKind::Vec(..));
        // `t` if `ok`; otherwise that this function takes `what`, not a `t`.
        let takes = |ok: bool, what: &str, t: TyId| {
            if ok {
                Ok(t)
            } else {
                Err(format!("`{}` takes {what}, not `{}`", self.name(), p.display_ty(t)))
            }
        };
        let same = |args: &[TyId]| -> Result<TyId, String> {
            if args.windows(2).all(|w| w[0] == w[1]) {
                Ok(args[0])
            } else {
                Err(format!(
                    "`{}` needs arguments of one type, not {}",
                    self.name(),
                    args.iter()
                        .map(|&t| format!("`{}`", p.display_ty(t)))
                        .collect::<Vec<_>>()
                        .join(" and ")
                ))
            }
        };
        match self {
            Sqrt | InverseSqrt | Sin | Cos | Tan | Asin | Acos | Atan | Exp | Exp2 | Log | Log2
            | Sinh | Cosh | Tanh | Floor | Ceil | Round | Trunc | Fract | Saturate | Normalize
            | Dpdx | Dpdy | Fwidth => {
                let t = takes(float_like(args[0]), "a float or float vector", args[0])?;
                if self == Normalize { takes(is_vec(t), "a vector", t) } else { Ok(t) }
            }
            Atan2 | Pow | Step => {
                let t = same(args)?;
                takes(float_like(t), "floats or float vectors", t)
            }
            Abs | Sign => {
                let t = takes(numeric(args[0]), "a number or vector", args[0])?;
                if self == Sign && matches!(types.kind(t), TyKind::Int(i) if !i.signed()) {
                    return Err(
                        "`sign` of an unsigned integer is always 0 or 1; compare with 0 instead"
                            .into(),
                    );
                }
                Ok(t)
            }
            Min | Max | Clamp => {
                let t = same(args)?;
                takes(numeric(t), "numbers or vectors", t)
            }
            Mix | Smoothstep => {
                // mix(a, b, t) and smoothstep(e0, e1, x): the first two match; the third is the
                // same type, or an f32 applied to every component (mix only).
                let t = same(&args[..2])?;
                takes(float_like(t), "floats or float vectors", t)?;
                let third = args[2];
                let or_f32 = self == Mix && is_vec(t);
                if third == t || (or_f32 && third == scalar_of(types, t)) {
                    Ok(t)
                } else {
                    Err(format!(
                        "the last argument of `{}` must be `{}`{}",
                        self.name(),
                        p.display_ty(t),
                        if or_f32 { " or `f32`" } else { "" }
                    ))
                }
            }
            Length => {
                let t = takes(float_like(args[0]), "a float or vector", args[0])?;
                Ok(scalar_of(types, t))
            }
            Distance => {
                let t = same(args)?;
                takes(float_like(t), "float vectors", t)?;
                Ok(scalar_of(types, t))
            }
            // Of any vectors, integers' too, as WGSL's.
            Dot => {
                let t = same(args)?;
                takes(is_vec(t), "vectors", t)?;
                Ok(scalar_of(types, t))
            }
            Cross => {
                let t = same(args)?;
                let d3 = types.vec_of(VecElem::F64, 3);
                takes(t == types.vec3 || t == d3, "two `vec3`s or two `vec3d`s", t)
            }
            Select => {
                // Any type: per component on a vector, and whole on any other value (the GPU
                // back end uses a variable where WGSL's `select` takes only scalars and vectors).
                let t = same(&args[..2])?;
                if args[2] != types.bool {
                    return Err(format!(
                        "the condition of `select` must be `bool`, not `{}`",
                        p.display_ty(args[2])
                    ));
                }
                Ok(t)
            }
            BitcastU32 | BitcastI32 => {
                takes(args[0] == types.f32, "an `f32`", args[0])?;
                Ok(if self == BitcastU32 { types.u32 } else { types.i32 })
            }
            BitcastF32 => {
                takes(args[0] == types.u32 || args[0] == types.i32, "a `u32` or `i32`", args[0])?;
                Ok(types.f32)
            }
            BitcastU64 => {
                takes(args[0] == types.f64, "an `f64`", args[0])?;
                Ok(types.int(IntTy::U64))
            }
            BitcastF64 => {
                takes(args[0] == types.int(IntTy::U64), "a `u64`", args[0])?;
                Ok(types.f64)
            }
            Len => match types.kind(args[0]) {
                TyKind::Slice(_) | TyKind::Array(..) | TyKind::ArrayN(..) => Ok(types.u32),
                _ => Err(format!(
                    "`len` is a method of runs and arrays, not of `{}`",
                    p.display_ty(args[0])
                )),
            },
            WrappingAdd | WrappingSub | WrappingMul => {
                let t = same(args)?;
                takes(types.is_int(t), "integers", t)
            }
            CountOnes | LeadingZeros | TrailingZeros => {
                let wide = matches!(types.kind(args[0]), TyKind::Int(i) if i.bits() >= 32);
                takes(wide, "a 32- or 64-bit integer", args[0])?;
                Ok(types.u32)
            }
            // Checked where they're called: their message is text.
            Panic => Ok(types.never),
            Assert => Ok(types.unit),
            // Checked where it's called: its path is a string literal.
            Embed => Ok(types.error),
        }
    }
}

/// The scalar type of a scalar or vector type.
pub fn scalar_of(types: &Types, t: TyId) -> TyId {
    match types.kind(t) {
        &TyKind::Vec(e, _) => types.elem(e),
        TyKind::Mat(_) => types.f32,
        _ => t,
    }
}

/// Whether math on `t` is math on `f64`s: `t` is an `f64` or a vector of them (E0702).
pub fn is_f64_math(types: &Types, t: TyId) -> bool {
    scalar_of(types, t) == types.f64
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
