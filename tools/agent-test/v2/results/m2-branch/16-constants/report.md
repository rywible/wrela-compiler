1. **Built:** yes, on attempt 1 of 8. The compiler printed `attempt 1: it builds`.
2. **Failed attempts:** none, so there are no diagnostics to report.
3. **Misleading parts of the language or docs:** nothing misled me. I'm confident about the result from reading the spec, but I didn't call `smooth` or check its output, so the hidden tests are the first real check of the arithmetic. Two small points:
   - The spec doesn't say whether a `const` array can be indexed with a literal (`WEIGHTS[0]`) in ordinary code. §3.5 says a constant is a place you can read or project, so I assumed it could, and it compiled.
   - `frame` was required, so I wrote it as an empty export.

The program is in `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-branch/16-constants/pkg/main.wrela`.

- **`WEIGHTS`:** `pub const WEIGHTS: [f32; 5] = [0.1, 0.2, 0.4, 0.2, 0.1]`.
- **`smooth`:** `pub fn smooth(a, b, c, d, e) -> f32` returns the weighted sum, `a` taking the first weight.
- **`frame`:** `pub fn frame(time: f32, width: u32, height: u32)` has an empty body.