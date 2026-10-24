The program built on attempt 1, with no diagnostics. I didn't run it, so I haven't seen the output. I'm confident it matches the brief.

1. **Build:** it built on attempt 1. The compiler printed no errors or warnings.

2. **Failed attempts:** there were none, so there are no diagnostics to report.

3. **Misleading parts:** nothing in the language or its documentation misled me.
   - The one thing I had to infer was which way the fragment coordinate runs. The spec doesn't say that `FragCoord`'s y is 0 at the top row. I assumed WebGPU's convention, so the colour goes from deep blue at the top to white at the bottom. The build can't confirm this.
   - The unused `time` parameter of `frame` produced no warning.

**What it does:** `pkg/main.wrela` has two shaders.
- **Vertex shader** (`cover`): it covers the screen with one triangle, copied from the spec's example.
- **Fragment shader** (`sky`): it blends from deep blue to white by `pixel.position.y / screen.size.y`.
- **Screen size:** a `Screen` struct holding a `vec2` of the width and height reaches the shader as a uniform. `frame` builds it from `width` and `height`.