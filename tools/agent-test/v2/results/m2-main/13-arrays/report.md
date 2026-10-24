1. **Built on attempt 1.** The compiler printed no diagnostics, so I made no further attempts. I haven't checked the return value. I only know it compiles, and by my reading `stats()` returns (12, 5).

2. There were no failed attempts, so there are no diagnostics to report.

3. Nothing in the language or its documentation misled me. Two points were fine as written:
   - An array passes where a run `[f32]` is taken (§4.4).
   - A bare literal in `[3.0, -1.0, 4.0, 1.0, 5.0]` is typed `f32` from how it's used (§2.2).

   The program is `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-main/13-arrays/pkg/main.wrela`. It has:
   - `fn sum(xs: [f32]) -> f32`
   - `fn largest(xs: [f32; 5]) -> f32`
   - `pub fn stats() -> vec2`
   - an empty `pub fn frame(time: f32, width: u32, height: u32)`