The program built on attempt 1 of 8, with no diagnostics. I only checked that it compiles. It has no frame test and I didn't run it on a GPU, so I haven't confirmed it computes the right values.

The package is `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-main/20-uniforms/pkg/main.wrela`:
- **Kernel:** `scale` is `@compute(64)`. It reads `input: [f32]` and a `factor: f32` uniform, and writes `out: mut Slots<f32>`. Each invocation does `out[id] = input[id.x] * factor`.
- **Frame:** `frame` allocates two 64-float GPU buffers. It fills an array with 0, 1, 2, … 63 and uploads it with `write`. It then dispatches `scale` with `factor` 2.0 and `groups: 1`.

1. **Built:** yes, on attempt 1.
2. **Failed attempts:** there were none, so no diagnostics to report.
3. **Misleading documentation:** nothing misled me. I did have to guess two things, and the compiler accepted both:
   - **`write` arguments:** §18.7 gives `write(mut buf, at, values)` without saying what type `values` takes. I passed a `[f32; 64]` array and it was accepted.
   - **Buffers and state:** because the brief gives no program state, I made the buffers fresh in each `frame` call, as §22's example does. Keeping them in state would avoid reallocating every frame.