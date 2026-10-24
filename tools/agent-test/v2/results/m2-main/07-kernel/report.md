1. **Built:** yes, on attempt 1 of 8. There were no diagnostics, so I made no further attempts.

2. **Failed attempts:** none, so there are no diagnostics to report.

3. **Misleading parts of the language or its docs:** nothing misled me. I only compiled it. I did not run it, so I haven't confirmed that it does what the task says. The program is in `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-main/07-kernel/pkg/main.wrela`.

I made two choices where the task left room:
- **Sign of the distance:** I wrote `length(p) - 0.75`, so points inside the sphere get negative values. The task says "the distance to a sphere", so if the hidden tests expect an unsigned distance, this will fail them.
- **Buffer lifetime:** `frame` makes the 4096-element `GpuBuffer<f32>` as a local, as §22's example does, so it is dropped at the end of each frame. Nothing keeps the results, so a test that reads the buffer back would see nothing.

The kernel is `@compute(64)` and takes the grid (origin `(-1,-1,-1)`, cell `0.125`, `n = 16`, radius `0.75`) and a `Slots<f32>` output. Each invocation turns its `GlobalId` into an x, y, z cell index and writes its own slot. `frame` dispatches it every frame with `over: 4096`, covering the whole grid.