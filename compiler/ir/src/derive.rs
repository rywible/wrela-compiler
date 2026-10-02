//! Derived interpretations of IR functions (language.md §13): forward-mode gradients and
//! sound intervals. Nothing here knows what a field is (D-056).

use crate::{FuncId, Module, Target};

/// A function `(f32, X)` of `f`'s captures and `x`: `f(x)` and its gradient with respect to
/// `x`, the last parameter.
pub fn value_and_gradient(m: &mut Module, f: FuncId, ncap: u32) -> Result<FuncId, String> {
    let _ = (m, f, ncap);
    Err("gradients aren't implemented yet".into())
}

/// A function of `f`'s captures and a box, returning an interval containing `f(x)` for every
/// `x` in the box.
pub fn interval(m: &mut Module, f: FuncId, ncap: u32, target: Target) -> Result<FuncId, String> {
    let _ = (m, f, ncap, target);
    Err("intervals aren't implemented yet".into())
}
