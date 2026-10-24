1. **It built on attempt 1.** The compiler gave no diagnostics. I didn't run the exports, so I haven't checked the areas themselves. By inspection, `areas()` returns `vec2(PI, 1.0)`.

2. **There were no failed attempts.**

3. **Nothing in the language or its documentation misled me.** Two things were unconfirmed when I wrote the code, and the compiler accepted both:
   - The spec doesn't state the type of `std::math::PI`. I assumed `f32` and `use std::math::PI` worked.
   - `main.wrela` calls `.area()` without importing `Shape`. §3.4 says method syntax finds a trait's methods when the trait is `pub`, and that was enough.

The files are in `.../26-modules-trait/pkg/`:
- `shape.wrela`: `pub trait Shape` with `fn area(self) -> f32`.
- `circle.wrela`: `pub struct Circle { pub r: f32 }`, implementing `Shape` as `PI * r * r`.
- `square.wrela`: `pub struct Square { pub side: f32 }`, implementing `Shape` as `side * side`.
- `main.wrela`: `pub fn areas() -> vec2` and an empty `pub fn frame(time: f32, width: u32, height: u32)`.