1. **It built on attempt 1.** There were no diagnostics, so no further attempts were needed.

2. **No failed attempts**, so there are no diagnostics to report.

3. **Nothing misled me.** The spec's §14 example (`interval(|t: f32| sin(t) * t, Interval { lo, hi })`) is the same shape as this task, so I wrote it from that.

I haven't checked the output values, because the only command I could run was the compile. The program is `pkg/main.wrela`:

```wrela
use std::derive::{Interval, interval}

pub fn bound(lo: f32, hi: f32) -> vec2 {
    let r = interval(|x: f32| x * x - 2.0 * x, Interval { lo, hi })
    vec2(r.lo, r.hi)
}

pub fn frame(time: f32, width: u32, height: u32) {}
```

It derives the interval of `x * x - 2.0 * x` over [`lo`, `hi`] with `std::derive::interval` and returns (low, high). I wrote `2.0` because numeric types don't convert implicitly. `frame` is empty because the task has no GPU work.