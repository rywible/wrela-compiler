**(1) Built:** yes, on attempt 1. The compiler printed no diagnostics.

**(2) Failed attempts:** none, so there are no diagnostics to report.

**What I wrote** (`pkg/main.wrela`):
- **Kernel:** `scale` is `@compute(64)`. It takes `input: [f32]`, `factor: f32` as the uniform, and `out: mut Slots<f32>`. It computes `out[id] = input[id.x] * factor`.
- **Each frame:** `frame` makes two 64-element `GpuBuffer<f32>`s. It fills a `[f32; 64]` with 0, 1, 2, … and uploads it with `write(mut input, 0, values)`.
- **Dispatch:** `dispatch(scale.bind(input, factor: 2.0, out: mut out), over: 64)` runs the kernel with factor 2. I used `over: 64` so the compiler sizes the groups and an invocation past the domain returns on its own.

I only checked that it builds. I did not run it, and I can't see the hidden tests' results. I think it does what the task says because it follows the §18 and §22 examples.

**(3) What misled me:** nothing misled me, but the spec leaves two points open:
- **`write`'s `values` type:** §18.7 says `write(mut buf, at, values)` and never names the type. I guessed that a fixed array works, and it built.
- **Mixed binding arguments:** the spec doesn't say whether `bind` accepts a positional buffer, then a named uniform, then a named `mut` argument. It was accepted.