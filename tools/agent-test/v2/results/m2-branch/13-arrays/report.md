1. **Built:** yes, on attempt 1. The program is `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-branch/13-arrays/pkg/main.wrela`.
   - `sum(xs: [f32]) -> f32` loops over the run and adds each element.
   - `largest(xs: [f32; 5]) -> f32` starts from `xs[0]` and keeps the `max` of each element.
   - `pub fn stats() -> vec2` builds `[3.0, -1.0, 4.0, 1.0, 5.0]` and returns `vec2(sum(xs), largest(xs))`. By hand that is (12, 5).
   - `pub fn frame(time: f32, width: u32, height: u32) {}` has an empty body, as the brief allows.
   - I didn't run `stats`: the brief allows no other command, so "does what the task says" rests on reading the code.

2. **Failed attempts:** none, so there are no diagnostics to report.

3. **Misleading parts of the language or docs:** nothing misled me.
   - I wrote the literals as `3.0` and `-1.0` rather than relying on integer literals inferring as `f32`.
   - I passed the `[f32; 5]` array to `sum`, which takes a run, using §4.4's "An array passes for a run".