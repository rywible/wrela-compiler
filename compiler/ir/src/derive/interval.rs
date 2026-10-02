//! Interval arithmetic (in progress).

use super::*;

pub(super) fn interval(
    m: &mut Module,
    cache: &mut DeriveCache,
    f: FuncId,
    ncap: u32,
    target: Target,
) -> Result<FuncId, String> {
    let _ = (m, cache, f, ncap, target);
    Err("intervals aren't implemented yet".into())
}
