**(1) Did it build?** Yes, on attempt 1 (the first of 8). I made no further attempts.

The program is `/Users/ryanwible/projects/wrela-compiler/target/agent-test/m2-branch/08-sky/pkg/main.wrela`:
- **Vertex shader (`cover`):** one triangle that covers the screen, built from `VertexIndex`.
- **Fragment shader (`sky`):** it takes `FragCoord` and a `Screen` struct that holds the screen size as a `vec2`. It blends from deep blue at the top to white at the bottom with `mix`, using `pixel.position.y / size.y`.
- **`frame`:** it passes `vec2(f32(width), f32(height))` to the shader as the uniform. It draws with `sky.bind(size)` into the screen pass and presents it.

I didn't run it on a GPU or check any pixels. "Deep blue at the top" assumes `FragCoord`'s y is 0 at the top of the screen. That follows WebGPU's convention, but the spec doesn't say it for Wrela. A hidden test that reads pixels would settle it.

**(2) Failed attempts:** none, so there are no diagnostics to report.

**(3) Anything that misled me:** nothing. §22 and the `hello-field` example showed the vertex shader, fragment shader and `draw`/`bind` pattern. Both show a no-argument vertex shader passed by name and a `GpuData` struct bound to the fragment shader.

The spec doesn't state which way `FragCoord`'s y runs. I had to infer the direction from WebGPU convention.