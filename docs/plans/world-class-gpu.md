# Plan: the world-class GPU capabilities (release 0.6.0)

*A copy of §0.2–§0.5 and Part A of [rywible/wrela `docs/plan-world-class.md`](https://github.com/rywible/wrela/blob/engine/world-class/docs/plan-world-class.md), the plan for both repositories. That file is the one edited; this copy is for reviewers of this repository. Part B, the engine's half, is there.*

### 0.2 The two repositories and the two pull requests

| Repository | Checkout | Pull request |
|---|---|---|
| `rywible/wrela-compiler` | `~/projects/wrela-compiler` | **Part A.** One PR titled `Release 0.6.0: <summary>`, per `docs/release.md` there. Ryan merges and tags it. |
| `rywible/wrela` | `~/projects/wrela` | **Part B.** One PR, pinned to 0.6.0, holding every engine, studio, content and game change. |

The order is fixed: Part A merges and releases first, then Part B moves the pin. While Part A is a
branch, `WRELA_TOOLCHAIN=~/projects/wrela-compiler` in this checkout runs it in place of the pin
(`docs/toolchain.md`), so Part B can be built against it before the release. Unset it before
running Part B's final checks.

The toolchain knows nothing about the engine (the layering rule). Every Part A feature is phrased
as a general GPU or host capability a non-game program would also want. Nothing in Part A names a
shadow, a probe or a tree.

**Integration before the release.** Part A is tagged only after Part B's first steps (§B0–§B2)
and one consumer of each new capability build and pass against the Part A branch through
`WRELA_TOOLCHAIN` (§A18). A published release is never replaced (`docs/release.md` §5), so a
toolchain fault found after the tag costs a 0.6.1; §A18 is where such faults are meant to
surface.

**Prerequisites, not just order.** Each Part A step names what it needs. The numbering respects
the prerequisites, so following it in order works; an agent that reorders must keep them.

**Where the plan starts.** `main` pins toolchain 0.5.2 (commit `9f1429bf`). The plan was first
written against 0.4.1 and revised when 0.5.0–0.5.2 shipped (§9). Those releases brought the
agent loop's tools (`wrela same`, `wrela migrate`, the `extract-const`, `inline` and
`extract-fn` refactors, lints W0010–W0013, a next step on every error, `wrela pipelines`'
`(under a condition)` marker; rywible/wrela-compiler#28) and two of this plan's steps: the page
starts the render worker's threads, so ticked programs run in the Claude app's browser pane, and
a pointer presses a semantic tree's buttons and menu items (0.5.1 and 0.5.2; #29, #30). §A14 and
§A17 record them as done. Nothing in `std::gpu`, the command stream (`VERSION` 11), the manifest
(`VERSION` 9) or spec §18 changed between 0.4.1 and 0.5.2, so every other step stands as written.
A new feature is a minor release (`docs/release.md` §1), so Part A is 0.6.0 and a fix after its
tag is 0.6.1.

### 0.3 How to work

- **One commit per numbered step,** in the order given, each titled with the step's heading. A
  step that says "gate" ends with the gate's result in its commit message.
- **Both repositories' rules hold:** one PR for one requested scope (this plan is the scope), run
  as few tests as possible to validate a change, cite your sources. Each outside method named in
  §7 gets a row in `docs/citations.md` when its code lands.
- **Nothing lands that the gates don't cover.** The gate kinds are in §0.5. A step whose gate
  fails is fixed before the next step; it is never skipped and never "fixed later".
- **Estimates are labelled.** A cost, a speedup or a budget is `estimated` in code and prose until
  a gate measured it; then the measured number replaces it, with the device and the command.
- **Don't invent design.** The look, the games' feel and their lore are Ryan's. Where a step touches
  them, build what the plan says and mark anything beyond it **proposed**.
- **Keep the docs true:** `docs/toolchain-gaps.md` (each gap this plan closes moves to "Fixed", in
  release 0.6.0), `spec/language.md` §18 (every new GPU capability), `docs/toolchain.md` (new
  commands), the capybara's `design.md` contracts (§5), and the compiler's `docs/release.md` steps.
- **Use 0.5.0's tools.** `wrela same <pkg> --against <saved build>` is the one command that shows
  a change kept what a program does (§0.5); `wrela refactor <pkg> extract-fn|inline|extract-const
  ... --dry-run` makes the mechanical edits checked; `wrela fix --dry-run` shows the compiler's
  own fixes. The lints are on: a copy that is only read is a borrow (W0010), a test that reaches
  no `assert` can't fail and is W0011 (every test this plan adds asserts something), a `Pending`
  polled in the call that made it is W0012, and a counted `while` over a run is a `for` (W0013).
- **An existing form changes only as a migration.** Part A adds forms and removes none: every new
  parameter has a default (`levels`, `mip`, `anisotropy`, `blend`), and a fragment shader
  returning `Over` still draws over. If a step finds it must change a form a program writes today,
  it ships the change as a migration (`docs/release.md` §1: a W01xx warning whose one fix
  `wrela migrate` makes, the old form compiling for two more minor releases), never as a break.

### 0.4 Decisions reserved for Ryan

The agent builds each default; Ryan can flip it in review.

| ID | Decision | Default the plan builds (**proposed**) |
|---|---|---|
| D1 | `Quality::measured` on by default | On. Measured costs are bucketed to damp order churn between runs; a measuring frame's order is **not** guaranteed identical run to run (§B2) |
| D2 | Temporal super-resolution as the default 3D path | On: scene at 0.667 of the output, `Shared` resolver, sharpened (§B5) |
| D3 | The capybara's visible width on wide screens | A fixed 16:9 play area, pillarboxed; `Aspect::Free` kept behind a setting (§B10.5) |
| D4 | A horizontal camera dead zone in the capybara | Not built; the camera stays locked (the current feel). Noted as an option |
| D5 | Dynamic sun in the Last Green | The frame supports it (§B8.2); the Last Green's sun stays put until Ryan moves it |
| D6 | Release WASM optimized for speed or size | Whichever the V8 measurement in §A15 favours, within the size budgets |
| D7 | Keep the clearing on the engine's one wood after §B4 | Yes; §B4's culling phase removes the regression that raised the question |
| D8 | Multi-draw indirect and 64-bit atomics (Chromium-experimental) | Detected and used when present, never required (§A9) |
| D9 | The public claim's wording (§0.1) | As written, once §B1 measures it |

### 0.5 The gate kinds

Each step's gate names one or more of these. The first four compare a candidate build with the
saved baseline build of the same program (`build/base-0.6/<name>`, made at §B0), both run by the
same command on the native host:

```bash
~/.wrela/toolchains/0.6.0/bin/wrela-host <build> --frames N --size 640x360 --workers 1 --input build/gates/<SCRIPT>.json --rgba out.rgba --log out.ticks
```

The scripts and frame counts per program are in `build/gates/` (`CAPY_RUN.json`,
`CLEARING_PATH.json`, `LG_WALK.json`, `LG_SHOT.json`). They test different things; a step names
only the ones it means.

**The one-command form.** Where a step means to record exactly the same commands, 0.5.0's
`wrela same` (`spec/language.md` §23.9; `compiler/cli/src/same.rs`) is G-commands, G-pixels
(identical) and G-pipelines in one run:

```bash
wrela same <pkg> --against build/base-0.6/<name> --frames N --size 640x360 --input build/gates/<SCRIPT>.json --within 0
```

It builds the package now (into `<pkg>/build/same`, under `.gitignore`'s `**/build/`), runs both
builds from `init` on one worker with the same script, and exits 0 only if every frame's command
hash is equal, the last frames' mean channel difference is within `--within` (0 here: identical)
and the pipelines are the same; otherwise it names the first frame whose commands differ. Both
builds must load under the pinned host, so it compares two builds of one toolchain: it can't
bridge §B0's pin move (the stream version in the batch header changes), which keeps the two-host
form above for that step. A step that changes a dispatch, a pass or a draw by design fails `same`
and names the separate gates.

- **G-sim:** the simulation is unchanged. Both builds run the same tick-keyed input script and
  write their tick logs (`--log`); `wrela-host --compare-ticks a.ticks b.ticks` (added in §A12)
  checks that every tick's hash is equal and names the first that isn't. `--replay` is **not**
  used across builds: it checks the log's WASM hash first (`runtime/native/src/lib.rs` ~1191), so
  a rendering-only change would fail it before any tick is compared. Only for a ticked program
  (the capybara, the Last Green). It says nothing about rendering.
- **G-commands:** the two runs' command-stream hashes are equal (the last line `wrela-host`
  prints; it covers every byte the program submits, so equal hashes mean the same GPU work was
  recorded). Only for a step that intends to record exactly the same commands (a refactor in
  §B3; not §B0's pin, whose two builds carry different stream versions). A step that changes a
  dispatch, a pass or a draw fails it by design and must not name it. `wrela same` is its
  one-command form.
- **G-pixels (identical):** the two runs' `--rgba` bytes are equal. Rendering changed nothing the
  eye sees, whatever the commands did.
- **G-pixels (within n/255):** each pixel's channels differ by at most `n`; the step says `n` and
  why the image may change (a strip landing a frame later, a quantization). The commit lists the
  largest difference and where.
- **G-draws:** two counts for a named frame, both stated by the step. The **logical** counts
  (passes, dispatches, draws as the program recorded them) come from `std::tool::drive`'s
  `Commands`. The **physical** counts (compute passes the executor opened, draws it issued after
  any multi-draw expansion, `writeBuffer` calls, bind-group sets) come from the hosts' executor
  counters in test mode (`counters.json`, §A12), in Chrome and natively. A multi-draw is one
  logical draw and, on a device without the feature, `n` physical ones; the recorded command
  stream can't show either physical number, so a step that claims one names the counters.
- **G-suite:** `wrela suite` passes in this repository (fmt, allow, every member's tests), and
  `tools/check.sh` passes in the compiler's.
- **G-golden:** the named `@test(frames:, gpu: true, golden:, pass:)` tests pass; a step that
  changes an image deliberately re-blesses with `wrela test --bless` and says so.
- **G-pipelines:** `engine/tests/frame-pipelines` and `wrela pipelines <pkg>` report the counts
  the step states; a step that adds pipelines says how many and why.
- **G-profile:** `wrela profile <pkg> --compare build/base-0.6/<name> --pairs 3 --size 1920x1080
  --workers 4` on the M4 Air, GPU idle; the step states the change it requires (or "no slower than
  1%"), and the result is pasted in the commit message. From §A12 on, `wrela profile --chrome`
  runs too, and the Chrome numbers are the ones that count.
- **G-both:** the frame's `hosts: both` tests pass (same state hash, same frame within a GPU's
  differences in Chrome and natively).
- **G-size:** the release's runtime ≤ 1 MB, each game's time-to-play ≤ 6 MB and cold start ≤ 8 MB
  as served, playable within 5 s cold in Chrome (`wrela tool release`, `studio/release`).
- **G-bench:** `wrela tool bench` (§B1) runs and its report is pasted into `docs/bench.md`.

## Part A: toolchain 0.6.0 (`rywible/wrela-compiler`)

Branch `release-0.6.0` from an up-to-date `main` (at or past `v0.5.2`). Follow `docs/release.md`
§2: bump `[workspace.package] version` in `Cargo.toml` to `0.6.0`, run `cargo check --workspace`,
commit `Cargo.toml` and `Cargo.lock` first. Then copy this plan's §0.2–§0.5 and Part A into
`docs/plans/world-class-gpu.md` there, as release 0.5.0's plan lives at
`docs/plans/agent-loop.md`: the compiler's reviewers read the copy in their own repository. The
copy names this file as its source; this file is the one that's edited.

**Where things live** (read these before the first step):

| Concern | Where |
|---|---|
| The command stream: opcodes, payloads, formats | `runtime/abi/src/stream.rs` (the module doc is the spec of the stream; `VERSION` is 11), `runtime/abi/src/stream/` (`TextureFormat`, `Compare`) |
| The manifest: pipelines, targets, bindings | `runtime/abi/src/manifest.rs` (`Pipeline`, `RenderTarget`, the `blend: bool` field at line 154) |
| The browser's copy of the ABI | `runtime/browser/src/abi.gen.ts`, generated by `runtime/abi/src/typescript.rs` (`TS_PATH`); regenerate it rather than editing it |
| The browser's GPU executor | `runtime/browser/src/gpu.ts` (pipelines built at `buildPipeline`, bind groups cached at `#bindGroups`, draws at ~1500, uploads at ~1217) |
| The browser's checker and sequencer | `runtime/browser/src/check.ts` (mirrors WebGPU's usage rules), `runtime/browser/src/program.ts` (`this.checker.check(cmd)` at 608; deferred execution at ~993) |
| The device and its features | `runtime/browser/src/device.ts` (`requiredFeatures`, `requiredLimits`), `runtime/browser/src/check.ts` (`LIMIT_SOURCES`) |
| The page, the render worker, the ticker | `runtime/browser/src/main.ts` (starts the render worker at ~309; starts and stops its threads on its `start-thread` and `stop-thread` messages at ~314–335, since 0.5.1; hands the canvas over at ~384), `runtime/browser/src/worker.ts` (asks the page for each thread: the ticker's channel at ~144, `spawnWorker` at ~796; `normalWorkers()` at ~382), `runtime/browser/src/messages.ts` (the thread messages) |
| The native host's GPU | `runtime/native/src/gpu.rs` (textures at ~922 and ~1180, samplers at ~1208, pipelines at ~2055, device features at ~478) |
| std's GPU API | `compiler/std/gpu.wrela` (formats at 425–570, samplers at 625–750, textures at 690–860, passes at 869–1010, `@intrinsic` functions throughout) |
| The compiler's closed list of std items | `compiler/sema/src/defs.rs` (`Lang`: every std item the compiler lowers itself, e.g. `TextureSample = "std::gpu::texture_sample"` at 369) |
| GPU lowering | `compiler/lower/src/gpu.rs` (resources, `Lang::TextureSample => T::Sample` at 1589, the `Over` blend at 958 and 2318, draw state) |
| WGSL emission | `compiler/wgsl/src/lib.rs` (texture types at 285–335: `arrayed: false`, `multi: false`; storage formats at 309–323; the barrier at ~825) |
| WASM optimization | `compiler/driver/src/optimize.rs` (`-Oz` at ~84, `FEATURES`) |
| The profiler | `compiler/cli/src/profile.rs` (native only), `runtime/browser/src/testmode.ts` (Chrome's `timings.json`, `frames.json`, `saturate`, `inflight`), `runtime/chrome/src/lib.rs` (the headless Chrome driver) |
| What 0.5.0 added | `compiler/cli/src/same.rs` (`wrela same`), `compiler/cli/src/migrate.rs` and `compiler/diag/src/codes.rs` (`MIGRATIONS`, the W01xx codes), `compiler/cli/src/refactor.rs` (`extract-const`, `inline`, `extract-fn`), `compiler/tests/tests/suite/warnings.rs` (W0010–W0013), `docs/plans/agent-loop.md` (the 0.5.0 plan, the precedent for a plan in that repository) |
| The spec | `spec/language.md` §18 (GPU code), §16 (threads), §17 (host services), §23.9 (`wrela same`, `wrela migrate`) |
| Checks | `tools/check.sh` (`python3 tools/check.py`: rustfmt, wrela-fmt, Rust tests, browser tests in Bun, WGSL budgets) |

**The recipe for a new command** (used by several steps): add the opcode and its payload to
`stream.rs`'s table and `Opcode` (never renumber; `VERSION` becomes 12 once in §A1); decode it in
`runtime/abi/src/stream.rs`'s decoder and the sequencer; check it in `runtime/browser/src/check.ts`
and the native checker; execute it in `runtime/browser/src/gpu.ts` and `runtime/native/src/gpu.rs`;
add the std function (an `@intrinsic` in `compiler/std/gpu.wrela`) and its `Lang` entry in
`compiler/sema/src/defs.rs`; lower it in `compiler/lower/src/gpu.rs`; regenerate `abi.gen.ts`; add a
conformance test under `compiler/tests/` and a browser test under `runtime/browser/test/`; document
it in `spec/language.md` §18.

**The recipe for a new texture format:** add it to `TextureFormat::ALL` in `runtime/abi/src/stream/`
with its name, bytes, and `storable`/`filterable`/`is_uint`; regenerate `abi.gen.ts`; add the
`pub struct <Name> {}` and `impl Format`, plus `Filtered`/`Storage`/`Target` as WebGPU allows, in
`compiler/std/gpu.wrela` beside `Rgba16Float`; add it to `ir::TextureFormat` and the WGSL storage
match in `compiler/wgsl/src/lib.rs`; map it in `runtime/native/src/gpu.rs`; test that a pass draws
into it and a kernel writes it where allowed.

### A1. The stream and manifest move to version 12

*Needs: nothing.*

- `runtime/abi/src/stream.rs`: `VERSION = 12`. `runtime/abi/src/manifest.rs`: `VERSION` 9
  becomes 10. Every host rejects the other version, as now. (Both were unchanged from 0.4.1 to
  0.5.2.)
- Nothing else changes in this step.
- **Gate:** G-suite (the compiler's). The frozen programs under `compiler/tests/frozen` rebuild.

### A2. Texture copies and texture readback

*Needs: A1.* Every later texture step's tests read a texture back through these.

- Commands, by the recipe: `CopyTexture` (`source handle, source level, source layer, x, y,
  destination handle, level, layer, x, y, width, height`), `CopyTextureToBuffer` (`texture,
  level, layer, x, y, width, height, buffer, offset`; rows padded to 256 bytes, which std
  unpads), `CopyBufferToTexture` (the reverse). Until §A4 and §A5 land, `level` and `layer` are
  always 0 and the hosts check that.
- std: `copy_texture(from: tex, to: mut other, x, y, width, height, to_x, to_y)`,
  `copy_texture_to_buffer(tex, mut buf, ...)`, `copy_buffer_to_texture(buf, mut tex, ...)`, and
  `read_texture(tex) -> Pending<Vec<F::Texel>>` (nondet, like `read`: it records a copy to a
  buffer the host reads back). The usage rules of §6.5 apply: a texture bound to a pass can't be a
  copy's source in that pass.
- **Tests:** a conformance test clears a 4×4 `Rgba8` texture through a pass, copies it to a
  second texture, reads the second back and checks every texel. A browser test checks
  `copyTextureToTexture` and `copyTextureToBuffer` arguments (row padding).
- **Spec:** §18.8 ("Copies and readback").
- **Gate:** G-suite.

### A3. Texture formats and the format table

*Needs: A2 (the tests read back).*

**The format table.** One table in `runtime/abi/src/stream/` (beside `TextureFormat`) is the
single source for the compiler and both hosts. Per format: WebGPU's name; bytes per texel (or per
block, with the block size); `filterable`; `storable`; `renderable`; `blendable`; the **render
target pixel byte cost** and **component alignment** from the WebGPU specification's "Texture
Format Capabilities" table (not the storage size: `rgba8unorm` costs 8 bytes as an attachment,
`rgba16float` 8, `r32float` 4, `rg32float` 8, `rgba32float` 16, `r8unorm` 1, `rg8unorm` 2,
`rgb10a2unorm` 8, `rg11b10ufloat` 8, `r32uint` 4, `rg32uint` 8); and the feature it needs for
each capability (`rg11b10ufloat-renderable` to render into `rg11b10ufloat`; `float32-blendable` to
blend into `r32float`, `rg32float` or `rgba32float`; `float32-filterable` to filter the three).
`typescript.rs` regenerates `abi.gen.ts` from it. Compiler checks (E0602, E0603) and host checks
read the same table.

**New formats**, by the recipe: `R8` (unorm; filterable, renderable), `Rg8` (unorm; filterable,
renderable), `Rgb10a2` (unorm; filterable, renderable), `Rg32Float` (storable, renderable; not
filterable; blends only with `float32-blendable`), `Rgba32Float` (storable, renderable; likewise),
`Rg32Uint` (storable, renderable; `vec2u` texels, loaded), `Rg11b10Float` (filterable; renderable
only with `rg11b10ufloat-renderable`, §A10; never storable). Texel types: `vec4` for the unorm and
float colour formats, `vec2` for `Rg*` floats, `vec2u` for `Rg32Uint`.

- A fragment shader returning a `vec2u` draws into an `Rg32Uint` target (extend the E0603 rule
  that pairs `u32` with `R32Uint`). `WithDepth<C>` accepts `C = vec2u`.
- A format that needs a feature for a use is checked at run time by the hosts against the
  device's features (the program checks `features()` first, §A10); the compiler can't know the
  device.
- **Tests:** a conformance test per format draws a known value and reads it back (§A2); a test
  asserts the table's byte costs for the eleven formats above against the spec's numbers, written
  out in the test. A browser test checks the names match WebGPU's.
- **Spec:** §18.8's format list and the table's columns.
- **Gate:** G-suite.

### A4. Mip chains, trilinear and anisotropic sampling

*Needs: A2, A3.*

**std (`compiler/std/gpu.wrela`):**
- `texture::<F>(width, height, levels: u32 = 1)` and `writable_texture` alike. `levels` is at
  most `floor(log2(max(width, height))) + 1` (E0603 at the call with a constant, else a trap).
- `Texture<F>::levels() -> u32`.
- `Texture<F>::level(i) -> TextureLevel<F>`: one mip level as a bindable view. A kernel takes a
  `mut Texels<F>` of a level (`writable_texture` only); any GPU code `load`s a level; a pass may
  draw into a level (`begin_pass(mut tex.level(i))`); a copy (§A2) names a level. Two levels of
  one texture bound together in one dispatch, one read and one written, are **allowed**: WebGPU
  validates usage per subresource (a texture's level and layer), and Dawn's checker iterates
  subresources. The compiler's overlap check (§6.5) treats `tex.level(i)` and `tex.level(j)` as
  disjoint when `i ≠ j` are constants, and both hosts' checkers track usage per (texture, level,
  layer).
- `sample(s, uv)` picks the level from the derivatives, as WGSL's `textureSample` does; add
  `sample_grad(s, uv, ddx, ddy)` (`textureSampleGrad`, any GPU code) and `sample_bias(s, uv, bias)`
  (fragment only). `sample_level`'s `level` now means what it says.
- `sampler(filter, address, mip: Filter = Filter::Linear, anisotropy: u32 = 1)`; `anisotropy`
  2–16 needs `filter` and `mip` linear (E0603 otherwise, WebGPU's rule).
- `generate_mips(mut tex, s: Sampler)` in std, as ordinary Wrela: a kernel per level that reads
  level `i − 1` through `sample_level` and writes level `i` as `Texels`. It records `levels − 1`
  dispatches. No host code. Each dispatch binds `tex.level(i − 1)` to read and `tex.level(i)` to
  write, **never the whole chain**: a whole-chain binding includes the written level and WebGPU
  refuses the overlap. The same rule holds for any program that reads one level while writing
  another.
- `DepthTexture` keeps one level. A depth pyramid is an `R32Float` texture with levels, filled by
  the program (the engine does this in §B4 with a **minimum** reduction, not `generate_mips`).

**Stream and hosts:** `CreateTexture` gains a `levels` word; texture bindings become `handle,
level, layer` where `level` is the view's level or `ALL` (`0xFFFF_FFFF`) for the whole chain;
`CreateSampler` gains `mip filter` and `anisotropy` words. The browser creates views per (level,
layer) and caches them on the texture; the native host likewise.

**Compiler:** `Lang` entries for `texture_sample_grad`, `texture_sample_bias`, `texture_level`,
`texture_levels`; lowering emits `textureSampleGrad`, `textureSampleBias`, `textureLoad(tex, xy,
level)`. A `TextureLevel<F>` is a `Texture` resource with a level in its binding.

- **Tests:** a conformance test makes a 4-level 16×16 texture, writes a gradient to level 0, runs
  `generate_mips`, draws a full-screen quad with a trilinear sampler at a size that selects level
  2 and reads back: the centre's value is the gradient's mean within 1/255. Another binds levels 0
  and 1 in one dispatch (one read, one `Texels`) and passes; binding level 0 twice, read and
  written, fails E0506. A browser test checks `mipLevelCount` and `maxAnisotropy` on the created
  objects, and that a dispatch binding two levels of one texture passes WebGPU's validation (no
  `uncapturederror`).
- **Spec:** §18.8 (levels, views, samplers), §18.3 (two levels of a texture don't overlap).
- **Gate:** G-suite.

### A5. Texture arrays and cube maps

*Needs: A4 (levels and per-view bindings).*

- `TextureArray<F>` (`texture_array::<F>(width, height, layers, levels: u32 = 1)`), with
  `layer(i) -> TextureLayer<F>` (a bindable 2D view: a pass's target, a kernel's `Texels`, a
  `load`, a copy's source or destination), `sample(s, uv, layer: u32)`, `sample_level(s, uv,
  layer, level)`, `load(x, y, layer, level)`, `layers()`. WGSL: `texture_2d_array`, so
  `compiler/wgsl/src/lib.rs`'s `arrayed: false` becomes a property of the resource kind.
- `Cubemap<F>`: a six-layer array with `sample_cube(s, dir: vec3)` (`texture_cube`) and
  `face(i) -> TextureLayer<F>` for drawing or writing a face. `cubemap::<F>(side, levels)`.
- `DepthTextureArray` with `layers`, `layer(i)` as a depth pass's target, and
  `sample_compare(s, uv, layer, reference)` (`texture_depth_2d_array`): shadow cascades in one
  texture.
- **Tests:** conformance tests for each: an array of 3 layers written by three kernels and
  sampled by layer; a cube drawn into face by face and sampled by direction; a depth array's layer
  drawn into and compared. A browser test checks `dimension: "2d"`, `depthOrArrayLayers` and the
  `cube` view dimension.
- **Spec:** §18.8.
- **Gate:** G-suite.

### A6. Blend states

*Needs: A3 (the table's `blendable` and feature columns).*

- `std::gpu::Blend: Copy + Eq + Fieldless { Replace, Over, Premultiplied, Add, Multiply, Min,
  Max }`, each defined in the doc comment by its WebGPU `GPUBlendComponent`s:
  - `Over`: colour `src·src.a + dst·(1 − src.a)`, alpha `src.a + dst.a·(1 − src.a)` (what `Over`
    returns today);
  - `Premultiplied`: colour `src + dst·(1 − src.a)`, alpha likewise;
  - `Add`: `src + dst`; `Multiply`: `src·dst`; `Min`, `Max`: the operations.
- A draw takes `blend: Blend = Blend::Replace` beside `cull:`, `depth_bias:` and `depth:`; it is
  part of the pipeline, so a build-time constant (E0603 otherwise), and the manifest's render entry
  gets `"blend": "<name>"` in place of `blend: bool`. A fragment shader returning `std::gpu::Over`
  still draws over, as a default `blend: Blend::Over`: giving both is E0304.
- With several colour targets (§A7), `blend:` applies to all; `blends: [Blend; N]` sets each.
- **Validity comes from the table.** A blend other than `Replace` into a format the table marks
  not blendable is E0603 at build; into `R32Float`, `Rg32Float` or `Rgba32Float` it needs
  `float32-blendable`, which the hosts check at the draw against the device (the program checks
  `features().float32_blendable` first). `R32Uint` and `Rg32Uint` never blend.
- Hosts build the `GPUBlendState` from the name (`runtime/browser/src/gpu.ts` `OVER` becomes a
  table; `runtime/native/src/gpu.rs` likewise).
- **Tests:** a conformance test per blend draws two quads into a cleared `Rgba16Float` target and
  reads back the expected arithmetic (§A2). A test checks each unsupported (blend, format) pair
  from the table is refused. The frozen programs' manifests say `"over"` where they said `true`.
- **Spec:** §18.7.
- **Gate:** G-suite.

### A7. Several colour targets in one pass

*Needs: A3 (byte costs), A6.*

- Pass constructors: `begin_pass2(mut a, mut b, ...)`, `begin_pass3`, `begin_pass4`, and the
  `_with_depth` forms, giving `Pass2<F0, F1>`, `Pass3<...>`, `Pass4<...>` and `DepthPass2..4`,
  all `RenderPass`. Each target has its own `clear:`/`keep:`.
- A fragment shader drawn into a `PassN` returns `std::gpu::Targets2<C0, C1>` (`Targets3`,
  `Targets4`), whose fields are the colours in target order (each a `vec4`, `vec2`, `u32` or
  `vec2u` matching its format), or a `WithDepth<TargetsN<...>>`.
- **The attachment limit is WebGPU's algorithm, not a sum of storage bytes.** Four colour
  targets at most, and their cost by the specification's "calculating color attachment bytes per
  sample": start at 0; for each target in order, **align the running total up to the format's
  component alignment, then add the format's render target pixel byte cost** (both from the §A3
  table); the result must be at most the default `maxColorAttachmentBytesPerSample` of 32. The
  order of targets matters. The compiler computes it at the pass's construction and refuses more
  with E0602 naming the targets and the total. Worked examples the test encodes:
  `[Rgba32Float, Rgba8, Rgba8, Rgba16Float]` is 16 → 24 → 32 → 40, refused, though its storage
  bytes sum to 32; `[Rgba16Float, Rg16Float, R8, R32Uint]` is 8 → 12 → 13 → align(13, 4) + 4 =
  20, accepted. wgpu's `validation.rs` implements the same algorithm and is a second reference.
- The manifest's `targets` already is a list; each pipeline variant's `targets` holds all of
  them. `BeginPass` grows: a count of colour targets, then each's `texture, load, clear r g b a`.
- **Tests:** a conformance test draws into `Rgba16Float` + `Rgb10a2` + `R32Uint` and reads each
  back. The two worked examples above: one E0602, one accepted. A browser test confirms the
  accepted one passes WebGPU's own validation.
- **Spec:** §18.8 (passes), §18.1 (what a fragment shader returns).
- **Gate:** G-suite.

### A8. Scissor and viewport

*Needs: A1.*

- `pass.scissor(x, y, width, height)` and `pass.viewport(x, y, width, height, min_depth: f32 =
  0.0, max_depth: f32 = 1.0)` on every pass type; they apply to the draws recorded after them in
  that pass and reset at the pass's end. Commands `SetScissor` and `SetViewport` (inside a pass
  only; the sequencer rejects them outside).
- **Tests:** a conformance test draws a full-screen triangle with a scissor of the left half and
  reads back both halves (§A2).
- **Spec:** §18.7.
- **Gate:** G-suite.

### A9. Workgroup atomics and shared scatter

*Needs: A1.*

- `SharedAtomics<N>` of `u32` (and `SharedAtomicsI<N>` of `i32`): workgroup memory any
  invocation may `add`, `min`, `max`, `and`, `or`, `xor`, `exchange`, `compare_exchange`, `load`,
  `store` at any index below `N`. It needs no barrier for its own operations (they are atomic) and
  a `barrier` between a phase of atomic writes and a phase of plain `load`s, which the existing
  E0609 analysis extends to. It counts toward the 16 KiB workgroup limit.
- `Shared<T, N>` is **unchanged**: its own-chunk rule stays the only plain write. A general
  scatter is `SharedAtomics::store` or `exchange`, which can't race; a syntactic "one writer per
  index" rule was considered and dropped, because two affine write sites in one phase (`lid` and
  `lid + 1`) satisfy any such rule and still race.
- `workgroup_prefix_sum(shared: mut SharedAtomics<N>, lid, value) -> u32` in std, as Wrela: a
  Hillis–Steele scan over `Shared`, the standard building block for compaction. §A10 adds a
  subgroup form with the same results.
- **Tests:** a conformance test builds a 256-bin histogram of 65,536 values with `SharedAtomics`
  and checks the counts; another compacts a list with the prefix sum and checks order and count.
- **Spec:** §18.5.
- **Gate:** G-suite.

### A10. Optional device features, detected and used, never required

*Needs: A3 (compressed formats use the table), A9 (the subgroup scan).*

- `std::gpu::features() -> Features` (nondet): `f16`, `subgroups`, `subgroup_min: u32`,
  `subgroup_max: u32`, `bc`, `etc2`, `astc`, `float32_filterable`, `float32_blendable`,
  `rg11b10_renderable`, `bgra8_storage`, `timestamps`, `multi_draw_indirect`, `atomic64_min_max`,
  `indirect_first_instance` (always true: still required).
- `runtime/browser/src/device.ts` and `runtime/native/src/gpu.rs` request each of these the
  adapter has: `shader-f16`, `subgroups`, `texture-compression-bc`, `-etc2`, `-astc`,
  `float32-filterable`, `float32-blendable`, `rg11b10ufloat-renderable`, `bgra8unorm-storage`,
  `timestamp-query`, `chromium-experimental-multi-draw-indirect`, and the 64-bit min/max atomics
  extension by whatever name Dawn exposes at the time (check `adapter.features` in Chrome stable
  first; if neither Chrome stable nor wgpu has it, skip it and record that in
  `docs/toolchain-gaps.md`). A program checks `features()` before it records anything that needs
  one.
- **Test mode masks features:** `#test&features=none` (or a comma list of the ones to keep) makes
  `features()` report the rest absent and the device request only `indirect-first-instance`, so a
  program's fallback paths are measured on a device that has the features. `wrela profile
  --chrome --features none` passes it through (§A12).
- **Feature-gated pipelines.** An entry point marked `@gpu(needs: f16)` (and `subgroups`,
  `atomic64`) compiles with `enable f16;` (etc.) and its manifest entry lists `"requires":
  ["shader-f16"]`. The hosts create such a pipeline only when the device has the feature, and a
  command that records it without the feature is a host error naming the feature. Inside such an
  entry point: the `f16` scalar and `vec2h`/`vec3h`/`vec4h` types with conversions `f16(x)`,
  `f32(h)`; `std::gpu::subgroup::{size, invocation_id, ballot, any, all, add, min, max,
  exclusive_add, inclusive_add, broadcast, broadcast_first, shuffle}`; `Atomics<u64>` with `min`
  and `max` only (and `Slots<u64>`). CPU code never sees `f16` or `u64` atomics.
  `workgroup_prefix_sum` gets a `@gpu(needs: subgroups)` twin, `subgroup_prefix_sum`, with the
  same results.
- **Compressed formats** `Bc1`, `Bc4`, `Bc5`, `Bc7`, `Etc2Rgb8`, `Astc4x4`: `Filtered` only (no
  `Target`, no `Storage`); `texture::<Bc7>(w, h, levels)` traps on a device without the feature,
  so a program checks `features()` first; `Texture<Bc7>::write_blocks(level, x, y, w, h, bytes)`
  uploads block rows (`WriteTexture` learns block formats from the table's block size). A kernel
  encodes blocks into a `GpuBuffer<u32>` and `copy_buffer_to_texture` (§A2) moves them: encoding
  is std or engine code.
- **Tests:** a conformance test runs an `f16` kernel when `features().f16` and checks a sum
  within f16's precision; a subgroup test checks `subgroup_prefix_sum` against
  `workgroup_prefix_sum`; a compressed-texture test uploads a known BC1 block and samples it. Each
  test passes trivially (and says so in its output) when the feature is absent. A browser test
  checks `features=none` masks them.
- **Spec:** §18.10 ("Features"), §18.1 (`@gpu(needs:)`), §18.8 (compressed formats).
- **Gate:** G-suite on the M4 Air (f16, subgroups and BC present) and on the Linux CI host.

### A11. Direct indexed draws and multi-draw

*Needs: A10 (the feature flag).*

- `draw(pass, vs, fs, indices: buf, index_count: n, instances: m = 1, first_index: 0,
  base_vertex: 0)` without `indirect:` records a `DrawIndexed` command (new opcode): no argument
  buffer for a static mesh.
- `draw_multi(pass, vs, fs, indirect: buf, count: n)` and the indexed form, with an optional
  `count_buffer: GpuSpan<u32>`: `MultiDrawIndirect` and `MultiDrawIndexedIndirect` commands. On a
  device without `features().multi_draw_indirect` the **host** records `n` indirect draws in a
  loop, so the program's code is the same either way (the command's semantics don't depend on the
  feature; only the host's cost does). The checker treats one multi-draw as `n` draws.
- **Tests:** a conformance test draws an indexed cube directly; another draws three indirect
  draws through `draw_multi` and reads back the same image as three `draw`s, with and without
  `features=none`.
- **Spec:** §18.7.
- **Gate:** G-suite.

### A12. `wrela profile --chrome` and CPU phases in the browser

*Needs: A1.* Built here so §A13 and §A15 can measure in Chrome.

- `compiler/cli/src/profile.rs`: `--chrome` runs the build in headless Chrome through
  `runtime/chrome` in test mode with `timestamps=1&saturate=1&nohash=1` (GPU spans) and a second
  run with `paced=1&inflight=2` (frame intervals as a player sees them), the same warmup and
  frame counts, and reports the same JSON shape as the native run with `"host": "chrome"`.
  `--compare` and `--baseline` work across two Chrome reports. `--features none` passes §A10's
  mask. Full-resolution timestamps need the developer flag the driver already sets
  (`runtime/chrome/src/lib.rs` ~216).
- `std::time::phase` works in the browser: `performance.now()` in the render worker, reported in
  `frames.json` per phase per frame.
- **Executor counters.** Both hosts count, per frame, what they actually did below the recorded
  stream: compute passes opened, render passes, draws issued (a multi-draw expanded by the host
  counts each), `writeBuffer` calls and bytes, bind-group sets, pipeline sets. Test mode writes
  them to `counters.json`; `wrela profile` (native and `--chrome`) reports the frame's medians
  beside the GPU times. These are the physical half of G-draws.
- **`wrela-host --compare-ticks a.ticks b.ticks`:** two tick logs from two builds run on the same
  input; prints the first tick whose hash differs, or "equal" (G-sim). It does not check the logs'
  WASM hashes, which `--replay` keeps checking. 0.5.0's `wrela same` compares each frame's
  command hash, not the ticks', so G-sim still needs this.
- `docs/toolchain.md`'s "Watching and measuring" gains the commands.
- **Tests:** a tool test runs `wrela profile --chrome` on `examples/hello-field` for 30 frames and
  checks the report's shape.
- **Gate:** G-suite.

### A13. The command path: fewer copies, less JavaScript

*Needs: A12 (the measurement).* Each item is measured by `wrela profile --chrome` on the frozen
`herd` and `grazer` programs (`compiler/tests/frozen`) and, through `WRELA_TOOLCHAIN`, on
`examples/clearing` and `games/last-green`: CPU frame time before and after. Keep what helps,
revert what doesn't, and record both in the commit.

**What must not change.** Commands are kept until the export that recorded them returns
(`runtime/browser/src/program.ts` ~618 pushes each to `#pending`; ~996 executes them after the
export succeeds, so a trapping export submits nothing), and a batch's bytes are copied at submit
because the program may overwrite its command buffer before the export returns (~755). The tests
`runtime/browser/test/program.test.ts` ~456 and ~509 guard both; they stay green through every
item below.

1. **One copy fewer per upload.** `runtime/browser/src/gpu.ts` ~1217 copies each upload's bytes
   into the upload ring's staging array (`up.bytes.set`) and later `writeBuffer`s the whole ring.
   Instead, at execution, `queue.writeBuffer(up.buffer, at, batch, offset, length)` straight from
   the retained batch slice (an owned `Uint8Array`), and the ring keeps no staging array. The
   retained slice is the one copy the runtime makes; the browser's own copy into the GPU buffer
   is the second. The same for `WriteTexture` (its rows padded to 256 bytes need a padded source,
   so a texture upload may keep a staging copy). "Two copies" describes the runtime's path, not a
   count of the browser's internal transfers. **The risk:** today one `writeBuffer` carries the
   whole ring (`gpu.ts` ~489); this item makes one call per upload. Measure the call count
   (`counters.json`, §A12) and the CPU time on a small-upload workload (the capybara: many small
   writes) as well as the Last Green; if many calls cost more than one copy saved, keep the
   staging array for uploads under a threshold (say 4 KiB) and go direct above it, or revert.
2. **Decode into a table, not objects.** `program.ts` ~601 decodes each command into a fresh
   object. Instead, decode into a reusable struct-of-arrays table (opcode, offset into the
   retained batch, length) appended per command, which the sequencer, checker and executor read
   through accessors; the batch slice stays the owner of the bytes. Pending commands stay distinct
   entries; nothing is reused until the frame's execution completes.
3. **The checker in debug, test and lifted builds only.** `runtime/browser/src/check.ts` mirrors
   WebGPU's own validation. In a release build's normal run the checker is off and an
   `uncapturederror` is the fatal error (it already is). The checker stays on under `#test`, in
   debug builds and in hot reload, where its messages name the program's mistake. `program.ts`
   ~608 branches on a `checking` option; test mode sets it.
4. **Redundant state.** `gpu.ts` ~1509: skip `setPipeline` when the pipeline is the last set in
   this pass, and `setBindGroup` when the bind group object is the last set.
5. **One compute pass per run of dispatches.** `gpu.ts` ~1391: open a `GPUComputePassEncoder` at
   the first dispatch after a pass or a write, close it at the next non-dispatch command.
6. **NaN canonicalization only for types with floats.** `compiler/wasm/src/func/ops.rs` ~1284: a
   `GpuData` type with no `f32` field skips the pass.
- **Tests:** the browser tests under `runtime/browser/test/` that check uploads, bind-group
  caching, command-memory reuse and trapping exports still pass; a new test submits several
  different `WriteBuffer`s in one call, overwrites the program's command buffer before returning,
  and checks each destination holds its own bytes; a test asserts that a release run with a usage
  error ends in a `GpuError` (from `uncapturederror`), and a test-mode run ends in the checker's
  message.
- **Gate:** G-suite; G-profile (Chrome): the Last Green's CPU frame time does not rise; the
  commit records the change for each item.

### A14. The frame loop: the ticker and helpers start from the page (done in 0.5.1)

*Needs: nothing. Nothing to build.* Shipped in toolchain 0.5.1 (rywible/wrela-compiler#29), kept
in 0.5.2. The Claude app's browser pane (Chrome 152) starts no nested worker, module or classic,
so the render worker now asks the page for each thread and speaks to it over a `MessageChannel`
(`runtime/browser/src/main.ts` ~314–335, `worker.ts` ~796, `messages.ts`); failures and stops go
through the page. `examples/climber-lab` and `games/capybara` run in the pane, and
`docs/toolchain-gaps.md` records the fix with the confirmed cause. The step keeps its number so
the `Needs:` lines stay true.

- **What this plan still owes it:** §A13 changes the command path inside the render worker, not
  the thread start; §A18.4 re-checks the pane on the branch, and §B11 records it.

### A15. Release WASM: speed or size, measured (D6)

*Needs: A12.*

- `compiler/driver/src/optimize.rs`: `--optimize` takes `size` (today's `-Oz`) or `speed` (`-O3`
  with the same `FEATURES`). The default for `wrela build --release` is chosen **in this step** by
  measurement on fixtures Part A has: the frozen `herd` and `grazer` programs' CPU frame time in
  Chrome (`wrela profile --chrome`, 7 alternating pairs) and their gzipped sizes. Through
  `WRELA_TOOLCHAIN`, the same on `games/last-green`. Speed wins if it is at least 3 % faster on the
  Last Green's CPU frame time and keeps every G-size budget; otherwise size stays. Record both
  builds' numbers in the commit. §B1's bench confirms the choice later; if it disagrees, 0.6.1
  flips the default.
- Also measure the CPU inline limit 125 vs 250 (`compiler/lower`), as the 2026-10-21 research
  suggested, on the same numbers.
- **Gate:** G-suite; the measurement in the commit.

### A16. GPU memory accounting

*Needs: A4, A5 (levels and layers count).*

- `std::gpu::memory() -> Memory { buffers: u64, textures: u64, peak_buffers, peak_textures }`:
  std counts every `buffer`, `texture`, `texture3d`, `texture_array` it creates and every drop
  (block formats by their block size; mip chains by their levels). Deterministic: it's the
  program's own arithmetic, so `@deterministic` code may read it.
- Test mode's `memory.json` stays the host's view; a test checks the two agree for a program that
  makes and drops known resources.
- **Gate:** G-suite.

### A17. A pointer presses a semantic tree's buttons (done in 0.5.1 and 0.5.2)

*Needs: nothing. Nothing to build.* Shipped in 0.5.1 and corrected in 0.5.2
(rywible/wrela-compiler#29, #30). Every role a press activates (`Button`, `Checkbox`, `Switch`,
`MenuItem`, a `Link` with an address) takes the pointer where it's drawn
(`runtime/browser/src/semantics.ts` `PRESSED_ROLES` ~194, `wrela-hit` ~390), a mouse press
doesn't take the keyboard's focus, and a click hands it back to the canvas (~338). An `Option` or
a `Tab` stays the program's: 0.5.1 gave those to the pointer too, the docsite reader's search
results stopped opening on a click, this repository's suite caught it, and 0.5.2 took it back.
`games/capybara`'s menu items are `MenuItem`s since `main` 9f1429bf. The gaps entry is "Fixed".

- **What this plan still owes it:** nothing in Part A. §B10's screens keep their `MenuItem`s; a
  new role that needs the pointer (none is planned) is a gaps entry, not a change here.

### A18. Integration against the engine, before the tag

*Needs: A1–A17.* This is the gate that keeps a toolchain fault from becoming a 0.6.1.

With `WRELA_TOOLCHAIN=~/projects/wrela-compiler` (this branch) in `~/projects/wrela`:

1. On `main`'s code, unchanged: `wrela suite` passes, and for every program in §B0's list a build
   with this branch against a build with 0.5.2, each run by its own host, holds **G-pixels
   (identical)** and **G-sim**. G-commands can't hold across toolchains (the batch header carries
   the stream version, 11 against 12, and payload layouts changed), so it is not asked here; if a
   normalized comparison of decoded commands is wanted, `wrela-host --dump-commands` prints each
   frame's decoded commands and `diff` of the two dumps must show only the fields the stream change
   renamed or added. The new toolchain changes nothing a shipped program shows or simulates.
   `wrela migrate --dry-run` on every member reports nothing, or exactly the migrations Part A
   declared (§0.3).
2. On the `engine/world-class` branch, §B0–§B2 as drafted build and pass their gates.
3. A throwaway test package, `engine/tests/toolchain-0.6` (kept in Part B as the capabilities'
   living test), uses every new capability once: a 3-target pass with blends, a mip chain
   generated and sampled trilinear, a cube face drawn and sampled, an array layer written by a
   kernel, a scissored draw, a `SharedAtomics` histogram, a direct indexed draw, a `draw_multi`,
   an `f16` kernel and a `subgroup_prefix_sum` behind `features()`, a BC1 upload, a texture
   readback, and `memory()`. It runs `hosts: both` (G-both) and with `features=none`.
4. `wrela run games/capybara` still starts in the Claude desktop app's browser pane (0.5.1's fix,
   §A14, through §A13's changes inside the render worker), and a mouse press on its `Start`
   starts the game (§A17).

Faults found here are fixed in Part A before the tag. Record what ran in the PR.

- **Gate:** all four, recorded.

### A19. Spec, docs and the release

*Needs: A18.*

- `spec/language.md` §18 covers every addition above with the same voice as the rest. `wrela doc
  std::gpu::Blend` etc. answer. `wrela primer gpu` mentions features, levels and blends.
- `docs/toolchain-gaps.md` in this repository: the entries this part fixes move to "Fixed", each
  naming 0.6.0 and the PR.
- `docs/citations.md` (compiler): rows for any new crate (none expected) and for the WebGPU and
  WGSL specification sections used.
- `tools/check.sh` passes in full on the M4 Air (every Rust test, the browser tests, the WGSL
  budgets) and on a Linux machine with a GPU if one is available; record which ran, with counts.
- `docs/release.md` §2 step 5 (since 0.5.0): run the agent syntax test once with the release's
  compiler and put its headline numbers (first try correct, mean attempts to correct, mean tokens)
  in the PR:

  ```bash
  cargo build --release -p wrela && tools/agent-test/v2/run.py release-0.6.0 --wrela "$PWD/target/release/wrela"
  ```

  It runs an agent per task, 30 tasks, and costs money (0.5.0's run: about $0.71 a task with
  Sonnet 5.5), so it runs once, after §A18 passes; `--dry-run` shows what it would run.
- Open the PR per `docs/release.md` §2; Ryan merges; tag `v0.6.0` per §3; check the release holds
  both tarballs and `SHA256SUMS`.
- **Gate:** the release exists and `wrela toolchain` downloads it in this repository.

---
