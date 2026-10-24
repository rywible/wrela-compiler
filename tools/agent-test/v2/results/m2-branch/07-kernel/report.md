1. **Built:** yes, on attempt 1. The compiler printed `attempt 1: it builds`, with no diagnostics.

2. **Failed attempts:** none.

3. **Misleading:** nothing misled me. I only compiled it. I didn't run it, so I haven't seen the kernel's output values. I'm confident it does what the task says because the code follows the spec's §22 example.

The program is in `pkg/main.wrela`:
- **Kernel:** `sphere_distance` is a `@compute(64)` kernel with one invocation per grid point.
- **Grid indexing:** it turns `id.x` into a 16×16×16 cell index with `% 16` and `/ 16`, then computes the position `(-1, -1, -1) + cell * 0.125`.
- **Output:** it writes `length(p) - 0.75`, the distance to the sphere of radius 0.75 at the origin, into `out[id]`.
- **Per frame:** `frame` makes a 4096-element `GpuBuffer<f32>` and dispatches the kernel with `over: 4096`, so it covers the whole grid exactly.

The task didn't give the program state, so the buffer is local to `frame` and is recreated each frame.