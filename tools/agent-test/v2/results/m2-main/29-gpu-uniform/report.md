1. **Built:** yes, on attempt 1 of 8. The compiler printed no diagnostics.
2. **Failed attempts:** none, so there were no diagnostics to report or fix.
3. **Misleading docs:** nothing misled me. I couldn't run the program or check its output. All I know is that it builds.

The program is in `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-main/29-gpu-uniform/pkg/main.wrela`. It does what the brief asks:
- **Uniform:** `Params: Copy + GpuData { scale: f32, offset: f32 }` is passed to the kernel as a uniform.
- **Kernel:** `@compute(64) affine(values: [f32], params: Params, out: mut Slots<f32>, id: GlobalId)` writes `out[id] = values[id.x] * params.scale + params.offset`.
- **Each frame:**
  - It fills a `[f32; 64]` with 0 to 63.
  - It uploads that to a 64-element `GpuBuffer<f32>` with `write`.
  - It dispatches `affine` with scale 2.0 and offset 1.0, `groups: 1`, into a second 64-element output buffer.