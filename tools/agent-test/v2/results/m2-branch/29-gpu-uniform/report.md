The program built on the first attempt. I haven't run it. The only check is the compiler's own, and the hidden tests haven't been run.

**1. Build:** it built on attempt 1, and the compiler gave no diagnostics. The file is `pkg/main.wrela`:
- `Params: Copy + GpuData { scale: f32, offset: f32 }` is passed to the kernel as a uniform.
- `@compute(64) fn affine(params: Params, input: [f32], out: mut Slots<f32>, id: GlobalId)` writes `out[id] = input[id.x] * params.scale + params.offset`.
- `frame` fills a `[f32; 64]` with 0 to 63 and makes two 64-element `GpuBuffer<f32>`s, `input` and `out`.
- `frame` then uploads the values with `write(mut input, 0, values)` and runs `dispatch(affine.bind(params, input, mut out), groups: 1)` with scale 2.0 and offset 1.0.

**2. Failed attempts:** there were none.

**3. Anything that misled me:** nothing did. I did assume `write` accepts an array as its values argument, and the compiler accepted it.

One thing I did not check is whether `out` is observable. `frame` has no state, so the buffers are made and dropped each frame. If the hidden tests read `out` back, I can't tell from the compiler's output whether they would see it.