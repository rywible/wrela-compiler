# Plan: the world-class engine, part two (release 0.8.0)

*A copy of §0.2–§0.6 and Part A of [rywible/wrela `docs/plan-world-class-2.md`](https://github.com/rywible/wrela/blob/main/docs/plan-world-class-2.md), the plan for all three repositories. That file is the one edited; this copy is for reviewers of this repository. Part B, the engine's half, and Part C, the other engines' scenes, are there.*

### 0.2 The three repositories and the three pull requests

| Repository | Checkout | Pull request |
|---|---|---|
| `rywible/wrela-compiler` | `~/projects/wrela-compiler` | **Part A.** One PR titled `Release 0.8.0: <summary>`, per `docs/release.md` there. Ryan merges and tags it. A fault found after the tag is a 0.8.1, never a replaced release. |
| `rywible/wrela` | `~/projects/wrela` | **Part B.** One PR from `engine/world-class-2`, pinned to 0.8.0, holding every engine, studio, content and game change, and the docs. |
| `rywible/wrela-others` (new) | `~/projects/wrela-others` | **Part C.** The competitor scenes (JavaScript and TypeScript, which this workspace may not hold) and their harness. One PR. Its numbers go into `docs/bench.md` by hand. |

**Where the plan starts.** Toolchain 0.7.0 shipped on 2026-10-25 (rywible/wrela-compiler#35: a cleanup of everything since #18 and the fixes a code review and a council found; the command stream stays at version 12 and the manifest at 10; the store's value limit fell to 1020 KiB; f16 interval derivation is refused with E0700 by a council decision; the pipelines' post-dominator analysis runs only for `wrela pipelines`; an unknown pipeline fails at once in the browser). The branch this plan lives on, `plan/world-class-2`, pins 0.7.0, so Part B starts from it and §B0 moves the pin to 0.8.0. The anchors in §0.8 were read at v0.6.3; the ones 0.7.0 moved are listed under the table.

The order is fixed: Part A merges and releases first, then Part B moves the pin. While Part A is a
branch, `WRELA_TOOLCHAIN=~/projects/wrela-compiler` in this checkout runs it in place of the pin
(`docs/toolchain.md`), so Part B's first steps are built against it before the release (§A16).
Part C is independent and must be done before Part B's final gates (§B16) need its rows.

The toolchain knows nothing about the engine (the layering rule, `docs/vision.md`). Every Part A
feature is phrased as a general GPU, language or host capability a non-game program would also
want. Nothing in Part A names a shadow, a probe, a tree or a tile.

### 0.3 How to work

- **Read first:** §8's worked examples (the shapes the steps expect), `docs/world-class-next.md` (why each step exists, with the audit's numbers),
  `docs/plan-world-class.md` §0.5 (the gate kinds, reused here by name) and §8–§9 (what the
  council changed last time and why), `docs/bench.md`, `docs/toolchain.md`, `docs/toolchain-gaps.md`,
  `spec/language.md` §13, §14, §16, §18, §23 in the compiler, and `AGENTS.md` in both repositories.
- **One commit per numbered step,** in the order given, each titled with the step's heading. A
  step that says "gate" ends with the gate's result in its commit message, numbers included.
- **Both repositories' rules hold:** one PR for one requested scope (this plan is the scope), run
  as few tests as possible to validate a change, cite your sources. Each outside method in §9 gets
  a row in `docs/citations.md` when its code lands.
- **Nothing lands that the gates don't cover.** A step whose gate fails is fixed before the next
  step; it is never skipped and never "fixed later". A **hypothesis** step whose gate fails ships
  switched off, with the measurement in its commit and in `docs/bench.md`, and the plan goes on.
- **Estimates are labelled.** A cost, a speedup or a budget is `estimated` in code and prose until a
  gate measured it; then the measured number replaces it, with the device and the command.
- **Don't invent design.** The look, the games' feel and their lore are Ryan's (`docs/vision.md`,
  "The lore"). Where a step touches them, build what the plan says and mark anything beyond it
  **proposed**. New looks (rock, water, cloth, fog) are engine capabilities with a default that
  the Last Green turns on; their parameters are the content's and Ryan's.
- **Keep the docs true:** `docs/toolchain-gaps.md` (each gap this plan closes moves to "Fixed",
  naming 0.8.0), `spec/language.md` (every new capability), `docs/toolchain.md` (new commands),
  `docs/bench.md` (every measurement), `docs/citations.md`, the games' `design.md`, and the
  compiler's `docs/release.md` steps.
- **Use the tools:** `wrela same <pkg> --against <saved build>` shows a change kept what a program
  does; `wrela profile --chrome --compare` measures; `wrela pipelines <pkg> --json` counts; `wrela
  precision` (new, §A9) says where `f16` is safe; `wrela tool bench` records; `wrela tool passes`
  (new, §B1.5) shows a frame's passes. The lints are on (W0010–W0013).
- **An existing form changes only as a migration** (`docs/release.md` §1): Part A adds forms and
  removes none; every new parameter has a default.
- **Pixels are a first screen in a look step, never the verdict.** A pixel diff against the
  baseline (`wrela same --within n`, the difference image) is cheap and runs first: it says what
  moved, where and by how much, and catches a change that landed somewhere it shouldn't (a
  shadow step that moved the sky). But a step that changes the image on purpose will not match
  the baseline's pixels and must not be made to: an agent that tunes a look step until the
  pixels match has undone the step, and **no work is discarded or reverted because pixels
  differ**. The floor is G-look's numbers (no shot worse by the reference error, the flicker, the
  clipping, the rubric); the aim is a shot Ryan judges better. Pixel gates (G-pixels) remain the
  verdict only for the steps that change no look: pins, refactors, culling, scheduling.
- **Measure on an idle GPU,** nothing else building; report GPU span, CPU frame call and paced
  intervals as the separate things they are (`docs/toolchain.md`, "Watching and measuring").

### 0.4 Decisions reserved for Ryan

The agent builds each default; Ryan can flip it in review.

| ID | Decision | Default the plan builds (**proposed**) |
|---|---|---|
| D10 | The claim's wording | §0.1 as written, once §B16 measures it |
| D11 | HDR and wide-gamut output | Built (§A12, §B13.3); `Look::output = Sdr` by default; `Hdr` is a setting |
| D12 | Local fog in the air | On (`Quality::fog = true`); each floor authors its density field (`Scene::fog`), the Last Green's is a thin ground mist in the hollows at dawn, **proposed** |
| D13 | Shadow pages | Pages 128², a physical pool of 4096² (1,024 pages, 64 MiB), ten clipmap levels from 8 m to 4 km; the moving casters keep their own 1024² map |
| D14 | The cluster path | `ClusterStones` and `CLUSTERED` leave the Last Green; `engine::virtual` stays as `examples/clusters` and its tests; the field LOD (§B11) replaces it for built things, rocks and distant creatures |
| D15 | A planar reflection of the mere | Drawn at half the scene's size only while the mere is in view (`Quality::planar = true`) |
| D16 | Relaxed SIMD in presentation code | Built behind `--relaxed-simd`; on by default in `wrela build --release` only if §B13.5 measures the CPU frame at least 3 % faster on the Last Green with no pixel over 1/255 |
| D17 | The cloud dome's size | 2048×640 marched, upsampled to the 4096×1280 dome, if §B5.4's pixels stay within 2/255; else the dome stays as it is |
| D18 | Neural impostors, the learned rejector, radiance cascades | Experiments (§B14); each ships behind a switch, off, unless its gate says it pays, then on |
| D19 | The competitors | three.js (its WebGPU renderer), Babylon.js and PlayCanvas; Unity Web only if a licence is to hand; the brief in Part C |
| D20 | The device matrix | M4 Air (the reference), M1 Air 8 GB, an Intel Iris Xe laptop with Windows 11 (Chrome and Firefox), an AMD Radeon 780M machine, an iPad Pro in Safari 26; one Android phone's row if a device is to hand |
| D21 | Grass placement cadence | A ring is re-placed when its cell set moves or every 8 frames (the wind's phase), not every frame |
| D22 | Probe visibility | On (`ProbeQuality::visibility = true`) |
| D23 | Pipeline tiers | Tier 1 (pipelines every site of which is under a runtime condition) starts building when tier 0 is built; the arrival cover may lift before tier 1 is done |
| D24 | The memory budget | `memory::TOTAL` stays 1,024 MiB for the program; the floor's own target is 512 MiB (§B7's gate) |
| D25 | Water looks | Flow, foam, falls and wet margins are engine capabilities; their colours and strengths stay in `WaterLook` (`CLEAR`, `PEAT`, `PAINTED`) for Ryan to set |
| D26 | Compressed files | **Decided** (Ryan, 2026-10-25): none are written by a build or held by a release; the toolchain's servers compress at serve time (§A3) and the release relies on Cloudflare's edge, which §B2.1 checks |

### 0.5 The gate kinds

The first plan's gate kinds are reused by name and are not restated: **G-sim, G-commands,
G-pixels (identical / within n/255), G-draws, G-suite, G-golden, G-pipelines, G-profile, G-both,
G-size, G-bench** (`docs/plan-world-class.md` §0.5; the one-command form is `wrela same`). The
baselines for all of Part B are `build/base-0.7/<name>`, made at §B0. New kinds:

A step's gates are its acceptance criteria: the step is done when every gate it names has passed, with the numbers in its commit.

- **G-cold:** `wrela tool bench --scene <s>`'s cold run (`salt`, no cache) three times, alternating
  with nothing else running; the **median** `playable_ms` is under the step's number. The bench
  prints all three.
- **G-warm:** the same, with the new warm run (§B1.4: the same build opened a second time by the
  same browser profile, no `salt`); median under the step's number.
- **G-hitch:** over the paced run's frames, the count over 33 ms is as the step says, and every
  frame over 33 ms is named in the bench's hitch table (§B1.2) with its heaviest labels.
- **G-device:** `docs/bench.md` has the row for each device D20 names, produced by §B8's procedure,
  with the machine's name from the report.
- **G-memory:** the bench's `gpu_peak` for the scene is under the step's number, and the share
  table (§B7.1) is in `docs/bench.md`.
- **G-stills:** `wrela tool lookbook --package <pkg> --stills` before and after, the pairs in the
  PR; a deliberate change is said to be one.
- **G-spike:** a hypothesis step's own numbers (its cost, its quality measure, its memory) against
  the thresholds the step states, pasted into the commit; the switch's default follows the result.
- **G-look:** the lookdev gate for a step that changes the image on purpose. The baseline's
  pixels are diffed first as a cheap screen (the difference image goes in the PR and says where
  the change landed), but the diff decides nothing and discards nothing. Four parts, all
  required: (1) **pairs**: `wrela tool lookbook
  --stills` before and after for every lookbook shot the step names (the floor's 23 and the
  clearing's 3 are the set), in the PR; (2) **the reference error**: `wrela tool lookbook
  --quality` (§B1.8) renders each named shot in **reference mode** (every quality knob at its
  maximum and 64 jittered samples accumulated **with time frozen**: the sim paused, the wind, the
  clouds' drift, the water's drift and the particles held, so only the jitter changes between the
  samples; no TSR, no pacing) and in play mode, and reports PSNR and SSIM of play against
  reference; neither falls, for any shot, by more than 0.5 dB or
  0.01 against the previous step's record, and §B16's record has every shot at least 1 dB above
  B13's; (3) **artifact measures** from the same tool, all with time frozen so that intended motion
  can't count as an artifact: flicker (mean per-pixel temporal variance over 64 play frames with
  the camera still) not up, clipped share (pixels at 1.0 in any channel) not up, popping (the
  largest mean frame-to-frame difference over a 10 s dolly, the camera the only thing moving)
  not up, `trail` (the ghosting length behind a moving edge against the reference) not up, each for every named shot; a rise the commit names as deliberate is allowed only with
  Ryan's acceptance in "For Ryan". The measures are read together: a change that lowers flicker
  while lowering PSNR by over 0.5 dB fails (smoothing is not quality), which the instrument's
  negative controls (§B1.8) enforce; (4) **judgement**: each pair
  judged **blind** on the lookdev rubric (§B1.8: light, colour, value, depth, artifacts) by a
  **separate** agent in a fresh context (not the implementer), the two stills of a pair shown in
  a random order it isn't told, with Ryan's hero anchors beside them when `WRELA_HERO_DIR` names
  them; it scores both and says which it prefers; the scores, the preferences and the order key go
  in the PR, and Ryan's verdict is asked for in "For Ryan". The judge is calibrated once per step
  with a control pair whose "after" is a deliberate degradation (§B1.8); a judge that prefers the
  degraded still is replaced, and every calibration attempt is recorded, not only the last. A step
  that changes temporal behaviour (the TSR, the probes' re-trace, the fog's blend, any switch
  that gathers over frames) is judged on three-frame strips at a moving edge as well as on
  stills. A step is not done while the judge
  preferred "before" on any named shot, or any of (2) and (3) regressed, unless Ryan accepted the
  exception in writing. The numbers in (2) and (3) are a floor, not a proof: the reference shares
  the approximation's algorithm where no better reference exists (the probes, the fog, the
  cascades at higher settings), and only bypasses it where one does (the TSR by supersampling,
  the shadow pages by finest pages everywhere); §B1.8 names which is which per subsystem.
- **G-precision:** an `f16` twin of a kernel against its `f32` original on the same input: pixels
  within the step's n/255 (`wrela same ... --within n` against the step's own saved build, with
  `--features none` forcing the `f32` path), and `wrela precision`'s report attached.

### 0.6 What this plan leaves out, and why

- **Static-or-moving by data flow in the compiler** (audit §5.7). The engine's kinds know what
  moves; §B13.1 gives each a flag. A taint analysis from the sim's snapshot through every draw
  would add nothing the flags don't yet, and it is research for a compiler agent, not a step.
- **Cost models from the compiler** (audit §5.8). The planner measures costs at run time since
  0.6.x; §B4.6 extends the timed labels to every kind. A static model would be a prior for a
  value the device measures in four frames.
- **Immediates (push constants).** The uniform blocks here are larger than the 64-byte immediate
  limit in almost every pipeline (`manifest.json`'s `uniform.size`); nothing to carry in them yet.
- **Compatibility mode as a rendering tier.** The engine needs `indirect-first-instance`,
  storage textures and multi-target passes that the restricted subset limits; reach to OpenGL ES
  3.1 devices is a separate renderer, out of scope. Phones are a non-goal (`docs/vision.md`).
- **Bindless, 64-bit atomics, subgroup matrices, texture atomics.** Not shipped in Chrome stable
  (audit §3.6). The soft rasterizer stays two-pass; the neural steps use `f16` and subgroups.
- **MSAA, stencil, occlusion queries, render bundles.** As in the first plan: the TSR anti-aliases,
  the depth pyramid replaces queries, the draw counts make bundles moot.
- **Strand hair, Gaussian splats, rollback netcode, compute tessellation of the clipmap.** Later
  work (audit §6.9, §7.7, §7.8); the crag's terracing is a representation problem that §B11b
  fixes without tessellation.
- **A render graph in the toolchain.** §B7.6 pools transient targets in the engine; a compiler
  check of pass liveness is a later language feature if the pool ever misbehaves.
- **Safari and Firefox automation.** `std::tool::browser` drives Chrome; the other browsers are
  checked by hand on the device matrix and recorded.

## Part A: toolchain 0.8.0 (`rywible/wrela-compiler`)

Branch `release-0.8.0` from an up-to-date `main` (at or past `v0.7.0`). Follow `docs/release.md`
§2: bump `[workspace.package] version` in `Cargo.toml` to `0.8.0`, run `cargo check --workspace`,
commit `Cargo.toml` and `Cargo.lock` first. Then copy this plan's §0.2–§0.6 and Part A into
`docs/plans/world-class-2.md` there, as 0.6.0's plan lives at `docs/plans/world-class-gpu.md`; the
copy names this file as its source, and this file is the one that's edited.

**The recipe for a new command** and **for a new texture format** are in
`docs/plan-world-class.md` Part A's preamble and are not repeated. The recipe for a new
**manifest field**: add it to `runtime/abi/src/manifest.rs`'s struct with `#[serde(skip_serializing_if
= ...)]` and a default in its parser (`manifest.rs:1045`, the `flag`/`number` helpers); bump
`VERSION`; mirror it in `runtime/browser/src/manifest.ts`'s type and parser (`:142-159, 329-354`);
regenerate `abi.gen.ts`; add a conformance test that a manifest without the field parses with the
default and one with it parses the value.

### A1. The stream and the manifest move to version 13 and 11

*Needs: nothing.*

- `runtime/abi/src/stream.rs`: `VERSION = 13`. `runtime/abi/src/manifest.rs`: `VERSION = 11`.
  Every host rejects the other version, as now.
- Nothing else changes in this step.
- **Gate:** G-suite (the compiler's). The frozen programs under `compiler/tests/frozen` rebuild.

**Done when:** (1) `stream.rs` says `VERSION = 13` and `manifest.rs` `VERSION = 11`; (2) a test
loads a 0.7.0 build under the 0.8.0 native host and asserts the refusal names both versions;
(3) `cargo test --workspace` passes and `compiler/tests/frozen` rebuilds (paste the counts).
Hill to climb: none; the checklist is the step.


### A2. The WASM arrives by URL, compiles as it streams, and the ABI check reads the manifest

*Needs: A1.* Today the page fetches every preloaded file to bytes and posts them to the worker
(`preload.ts:17-27`); the worker compiles the WASM from bytes with `WebAssembly.compile`
(`program.ts:469-477`), which V8's code cache never serves (the cache keys on a streamed
resource's URL; V8's blog, §9). `checkAbi` (`program.ts:198`) parses the bytes' type and import
sections, so a `Module` alone isn't enough today.

- **The build states the ABI.** `compiler/driver/src/build.rs:201-224`: the manifest gains
  `"wasm_abi": { "memory": { "initial": n, "maximum": m, "shared": true }, "imports": [["wrela",
  "submit"], ...] }` with exactly what `checkAbi` derives from the bytes (`wasm.ts:108`,
  `functionTypes`), by the manifest-field recipe. `checkAbi(module, abi)` compares
  `WebAssembly.Module.imports(module)` and `.exports(module)` with the manifest's lists and builds
  `MemoryLimits` from `wasm_abi.memory`; the byte parser stays only for the native host's tests if
  it is used there (grep; delete it if not).
- **The WASM is handed over as a URL.** `preload.ts`: for the file whose href is the manifest's
  `wasm`, `handOver` posts `{ type: "file", url, bytes: null }` and the worker's `Handed.fetch`
  (`loader.ts:60-89`) falls through to a real `fetch(url)` for it, which the preload has warmed in
  the HTTP cache. `loader.ts:92-99`: the WASM entry of `loadBuild` becomes a `Response`, not
  bytes; `Program.compile` takes `Response | Bytes`: a `Response` whose `Content-Type` is
  `application/wasm` goes to `WebAssembly.compileStreaming(response)`, anything else is read to
  bytes and compiled as today (the fallback for a server that mislabels). The ticker and the
  helpers already receive the `Module` (`worker.ts:826-832`, `messages.ts:42, 125-127`); the
  `wasmHash` for the tick log (`worker.ts:219, 254`) is computed from a `response.clone()`'s bytes
  **only when `settings.hashes`** is on, so a release run reads the bytes once at most.
- **The servers name the type.** `loopback.rs:162-174` already maps `wasm` to `application/wasm`;
  keep it (`compileStreaming` requires it), and §A3's compression keeps the inner type.
- **Tests:** a browser test opens `examples/hello-field` twice in one Chrome profile and asserts
  the second `load.json`'s `wasm` entry shows a compile under half the first's (the code cache
  served it); a test mislabels the WASM (`text/plain`) and asserts the fallback path compiles it;
  the conformance suite's `checkAbi` tests move to the manifest.
- **Spec:** §15 (what the manifest carries). **Docs:** `docs/toolchain.md`'s "Running a program"
  notes that a warm open skips the compile.
- **Gate:** G-suite; the hello-field warm/cold pair recorded in the PR with the numbers.

**Done when:** (1) every frozen program's `manifest.json` carries `wasm_abi` and a test asserts
its fields equal what `functionTypes` derived from the bytes before the change; (2) `grep -rn
functionTypes runtime/browser/src` shows no use on the load path; (3) the browser test opens
`examples/hello-field` twice in one profile and the second `load.json` shows a WASM compile time
at most half the first's (paste both); (4) the mislabelled-type fallback test passes; (5)
`runtime/browser/test` is green; (6) through `WRELA_TOOLCHAIN`, the Last Green's warm compile
in ms is recorded in the PR.
Hill to climb: the warm compile, now equal to the cold (no cache) → at most half; measured by
(3) and by `wrela profile --chrome` twice on the Last Green.


### A3. Compression at serve time, never as files

*Needs: nothing.* No server in the toolchain sets `Content-Encoding` (`loopback.rs:195-198`);
the bench loads 15.7 MB uncompressed of which 92 % is WASM and WGSL; gzip takes the Last Green's
WASM from 8.2 to 3.0 MB and its WGSL from 5.8 to 1.3 MB. **A build writes no compressed files
and a release holds none** (D26, Ryan, 2026-10-25): the release is served by Cloudflare, whose
edge compresses `application/wasm`, `text/plain`, `text/javascript` and `application/json`
responses itself and doesn't properly support pre-compressed objects behind a Worker. What this
step adds is only so the toolchain's own servers show a cold start the way the wire does.

- **The servers compress on the fly.** `loopback.rs:213-245` `get_file`: when the request's
  `Accept-Encoding` lists `gzip` and the file's type is one of `application/wasm`, `text/*`,
  `application/json`, `text/javascript`, compress the bytes with `flate2` (already a dependency,
  `compiler/cli/src/pin.rs:442`) at level 6 and send them with `Content-Encoding: gzip`, `Vary:
  Accept-Encoding` and the inner `Content-Type` (`mime(path)`). The compressed bytes are kept in
  an in-memory cache keyed by `(path, size, mtime)`, bounded at 256 MiB, so a page's second load
  costs no compression; nothing is written to disk. `head` gains the two headers as optional
  parameters. `wrela run`'s hot-reload server (`live.rs:595-630`) keeps `Cache-Control: no-store`
  and compresses the same way.
- **The request's headers are parsed where the path is** (`loopback.rs`'s request reader): read
  `Accept-Encoding` only; the parser stays the same size-bounded reader it is.
- **Nothing in a build directory changes:** `wrela build`'s output is byte-identical to 0.7.0's
  for the same program, and `studio/release` (Part B) gains no compression step that could drift
  into Cloudflare. A test asserts that a build's directory, with and without `--relaxed-simd`
  (A13), holds no `.br` or `.gz` file; §B16's G-size asserts the same of `wrela tool release`'s
  output.
- **Tests:** a Rust test of `get_file` with and without `Accept-Encoding: gzip` checks the headers
  and that the decompressed body equals the file; the browser's `loader.ts` needs no change (the
  browser decodes); a test asserts `load.json`'s `wasm` bytes under test mode are the wire count
  (so the bench's "loaded bytes" means wire bytes, within the gzip-to-Brotli difference of what
  Cloudflare sends).
- **Spec/docs:** `docs/toolchain.md` "Running a program": what the servers send.
- **Gate:** G-suite; `wrela profile --chrome` on the frozen `grazer` shows its loaded bytes fall
  by at least 60 % (recorded).

**Done when:** (1) the `get_file` test shows `Content-Encoding: gzip`, `Vary: Accept-Encoding`
and the inner type with the header, plain bytes without, and the decompressed body equal to the
file; (2) a test builds `examples/hello-field` and asserts no `.br` or `.gz` under its build
directory; (3) a test loads one file twice and the compressor ran once (a counter); (4) `wrela
profile --chrome` on the frozen `grazer` reports loaded bytes at most 40 % of 0.7.0's (paste
both); (5) `docs/toolchain.md` says what the servers send.
Hill to climb: wire bytes of the frozen `grazer`, now 100 % → at most 40 %.


### A4. The data section without its zero runs, and a report of what's in it

*Needs: nothing.* The Last Green's 8.37 MB WASM holds a 4.36 MB data section, 38.8 % zero bytes,
as one passive segment copied by a start function (`compiler/wasm/src/lib.rs:465-490`,
`func.rs:1635-1676`).

- **Segments skip zero runs.** `fn data` returns the blob and a list of `(offset, len)` runs of
  non-zero bytes, splitting at any zero run of 256 bytes or more; the emitter writes one passive
  segment per run (`DataCountSection { count: runs }`), and `init_data` issues one `memory.init`
  per segment and one `data.drop` each, under the same once-only latch. Memory is zero where no
  segment lands, so the result is byte-identical. The active (non-shared) path does the same with
  active segments at their offsets.
- **`wrela build <pkg> --report data`** prints the twenty largest constants by their laid-out size
  (`ir::Data.name` and `ty`), the section's total, and its zero-byte share, as a table; `--json`
  too. It is how §B2.2 decides which constants become files.
- **Tests:** a conformance test builds a program with a `const` holding a 64 KiB array that is
  mostly zeros and asserts the WASM's data sections total under a quarter of it and the program
  reads the array back correctly on both hosts; a test for `--report data`'s shape.
- **Spec:** §13 (what a `const` costs in bytes: "zero runs cost nothing").
- **Gate:** G-suite; G-pixels identical and G-sim on the frozen programs (the data is the same).

**Done when:** (1) the mostly-zero constant test: data sections under a quarter of the array
and correct reads on both hosts; (2) G-pixels identical and G-sim on every frozen program;
(3) `wrela build games/last-green --report data` (through `WRELA_TOOLCHAIN`) prints twenty rows,
the total and the zero share; (4) the Last Green's data section is at most 2.7 MB (now 4.36 MB
with 38.8 % zeros; paste the new size).
Hill to climb: the Last Green's data section, now 4.36 MB → at most 2.7 MB from zero runs alone
(§B2.2 takes it further).


### A5. Pipeline tiers: what's under a condition builds second

*Needs: A1.* The runtime starts all 169 of the Last Green's pipelines at once, biggest first
(`gpu.ts:408-432`), and the first command that needs one that isn't built stalls every command
after it (`gpu.ts:993-1060`). The compiler already knows which pipelines are reached only under a
runtime condition (`driver/src/lib.rs:971-1045`, printed by `wrela pipelines`), 47 of the Last
Green's.

- **The analysis runs in the build again.** 0.7.0 moved the pipelines' post-dominator analysis (`under_condition`) out of the build so only `wrela pipelines` pays for it; the tier needs it at build time. Run it in `wrela build` once per function that records a command, cache it per function, and measure `wrela build games/last-green`'s wall time before and after (paste both; under +5 %, or the analysis is made cheaper first).
- **The manifest says the tier.** By the field recipe: `Pipeline.conditional: bool` (default
  false), true when every reaching site's `conditional` is true (`summarize` runs before `emit`
  at `lib.rs:854-867`; pass `&[bool]` into `emit_in`). `wrela pipelines` prints `tier 1` beside
  such a pipeline and `--json` carries `"tier": 0|1`.
- **The runtime builds tier 0 first.** `buildPipelines`: start tier 0's builds biggest first;
  when `Promise.all` of them settles, start tier 1's. A command that needs a tier-1 pipeline
  before its build has started bumps it (start it at once) rather than waiting for the tier. The
  executor's `built` promise resolves when **tier 0** is built (that is what `pipelines_ms`
  means from now on; `pipelines.json` gains `tier1_ms` for the rest, and `each` keeps every
  pipeline).
- **`features().pipelines_ready()`** is not added: a program can't see tiers and shouldn't; the
  arrival cover (`engine::frame`'s `Arrival`) already hides a scene until it is playable, and a
  conditional pipeline that is reached stalls only its own commands as today.
- **Tests:** a browser test with a program of two pipelines, one under `if false_at_runtime()`,
  asserts `pipelines.json`'s order (tier 0 first) and `tier1_ms > 0`; a test reaches the tier-1
  pipeline in frame 1 and asserts the frame still draws (the bump).
- **Spec:** §18.9 (tiers). **Docs:** `docs/toolchain.md`'s `wrela pipelines` row.
- **Gate:** G-suite; on `games/last-green` through `WRELA_TOOLCHAIN`, `pipelines_ms` (tier 0) is
  under the 0.7.0 number by at least the tier-1 share of WGSL bytes (state both).

**Done when:** (1) the Last Green's manifest marks `conditional: true` on exactly the pipelines
`wrela pipelines --json` lists with every site conditional (now 47; paste the count); (2)
`pipelines.json` orders tier 0 before tier 1 and reports `tier1_ms`; (3) the bump test draws its
frame; (4) `wrela pipelines` prints `tier 1` beside them; (5) through `WRELA_TOOLCHAIN`, the Last
Green's cold `pipelines_ms` (tier 0) is at most 70 % of 0.7.0's in three alternating pairs (now
7.4 s in the cache-defeated bench, 4.3–4.5 s in later runs; paste all six).
Hill to climb: tier-0 `pipelines_ms`, now 100 % → at most 70 %.


### A6. Constants fold after inlining

*Needs: nothing.* `fold_constant_branches` runs once per function before `inline_cpu`
(`opt.rs:1515-1518`) and knows bool literals, `!`, loads of locals that hold known bools and
`if`s on them (`:1942-2032`). A predicate function whose body folds to `false` still returns an
unknown value to its caller until it is inlined, and nothing folds after that. That is why
`CLUSTERED`'s 17 pipelines survive (`docs/toolchain-gaps.md`, "A branch under a false constant").

- **Run the fold again after inlining:** in `inline_cpu`, after the postorder `inline_into` and
  `tidy` and before `remove_unreached`, loop `fold_constant_branches` + `tidy` over every function
  until no function changes (at most 4 rounds; count them in a debug log). Add to `fold_block`:
  `Let(v, Binary(And | Or, a, b))` where one operand is a known bool (`false && x` is `false`;
  `true || x` is `true`; `true && x` is `x`'s value if known), and a known comparison of two
  integer literals (`Eq, Ne, Lt, Le, Gt, Ge`) for the `len() == N` pattern after the length is a
  literal. Nothing else: no arithmetic folding (wasm-opt does it for the CPU; GPU code has
  `flatten_gpu`'s own rules).
- Then `recorded_pipelines_only` (`lower/src/lib.rs:525-528`) drops the pipelines whose records
  are gone, as it does today.
- **Tests:** a conformance test with `const ON: bool = false`, `fn on(s) -> bool { ON && s.n == 3
  }` and a dispatch under `if on(s)` asserts `wrela pipelines --json` lists no such pipeline and
  the manifest has none; the frozen programs' manifests are unchanged (nothing in them was dead).
- **Docs:** `docs/toolchain-gaps.md`'s entry moves to "Fixed" in §B15.
- **Gate:** G-suite; `wrela pipelines games/last-green` through `WRELA_TOOLCHAIN` lists 153, not
  170, with `CLUSTERED` false (the 17 are named in the commit).

**Done when:** (1) the dead-pipeline conformance test passes (no pipeline in the manifest, none
in `wrela pipelines --json`); (2) `wrela pipelines games/last-green` through `WRELA_TOOLCHAIN`
lists 153, not 170, with `CLUSTERED` false, and the 17 are named in the commit; (3) the frozen
programs' manifests are byte-identical; (4) the fold's rounds are at most 4 on every member of
this workspace (a debug counter, pasted).
Hill to climb: the Last Green's pipeline count, now 170 → 153.


### A7. `const` parameters of entry points, as pipeline-overridable constants

*Needs: A1, A5.* `.bind(x: value)` packs every argument into the uniform block
(`lower/src/gpu.rs:849-863`); a `Quality` switch is a uniform branch in every shader that reads
it; the only build-time constants a draw knows are its render state (`render_constant`, E0603).
WGSL has pipeline-overridable constants (`override`, the WGSL spec §9), which naga 30 carries in
`Module.overrides` and WebGPU sets per pipeline through `constants:`.

- **The form.** A GPU entry point's parameter may be `const`: `@compute(64) fn shade(..., ambient:
  const bool, taps: const u32)`. A `const` parameter is a `bool`, `u32`, `i32` or `f32`. At a
  `bind`/dispatch/draw, its argument must be a literal or a `const` item (the `render_constant`
  rule, extended: E0603's message names the parameter). Inside the entry point it is an ordinary
  value; a branch on it is a uniform branch today and a dead branch after this step.
- **Lowering.** `PipelineKey` gains `consts: Vec<(String, ConstValue)>` for both `Compute` and
  `Render`; two records with different values are two pipelines; `recorded_shader` evaluates the
  argument as `render_constant` does. The WGSL writer emits `override <name>: <type>;` for each
  (`naga::Override` **without** an `@id`, so WebGPU's `constants` map is keyed by the name, as the
  manifest is; WGSL §7.2.2 would otherwise require numeric keys), and the manifest's `Pipeline`
  gains `"constants": {"name": value}` (the field recipe). The branch on a `const` parameter
  stays in the WGSL text; the driver's compiler folds it at pipeline creation, which is where the
  value is fixed. Both hosts pass them: `createComputePipelineAsync({ compute: {
  module, entryPoint, constants } })` and `renderDescriptor`'s `vertex.constants`/
  `fragment.constants` (`gpu.ts:282-286, 127-139`); native `compilation_options.constants`
  (`runtime/native/src/gpu.rs:1143-1149, 2674, 2691`). The shader module is created once per
  pipeline today and stays so; the win is the dead code, not module sharing.
- **The pipeline key in `pipelineKey`** (`gpu.ts:352-356`) covers the constants since they are in
  the manifest entry's JSON.
- **`wrela pipelines`** lists each constant set as an instantiation: `shade::<ambient = true,
  taps = 4>`.
- **Tests:** a conformance test dispatches a kernel with `flag: const bool` as `true` and
  `false`, reads back both (the results differ as the kernel's arithmetic says) and asserts two
  pipelines in the manifest with their `constants`, on both hosts with no validation error; a
  test passes a runtime value and gets E0603; a microbench dispatches the `const` form and a
  uniform-branch twin of the same kernel 1,000 times and records both times (no threshold: the
  record shows the driver folded it, or didn't). A browser test asserts the pipeline is created
  with name-keyed `constants`.
- **Spec:** §18.1 (`const` parameters), §18.9 (what makes a pipeline).
- **Gate:** G-suite; `compiler/tests/frozen` unchanged.

**Done when:** (1) the two-variant test passes with two manifest entries carrying `constants`;
(2) the runtime-value test gets E0603 naming the parameter; (3) the microbench's two times are
recorded; (4) the browser test sees name-keyed `constants` in the pipeline descriptor and the
native host passes them (a native test reads back the kernel's result for both values with no
validation error); (5) `wrela pipelines` prints the instantiation with its constants;
(6) the frozen programs are unchanged.
Hill to climb: none; §B5 and §B13 use it.


### A8. Tapes that can hold a creature, and a tape derived from a compiled field

*Needs: nothing (std and the IR).* `std::stage::Tape` is 128 scalar ops of 18 kinds
(`stage.wrela:16-36`), interpreted on the GPU as a 2,064-byte uniform; it has no floor, select,
mix, table read or noise, so no creature or built thing can be a tape. The audit's §5.2 and
§3.1.4: one tape-driven extraction pipeline set serves every kind at load, and the per-kind
kernels (2.4 MB of the Last Green's WGSL, the four slowest compiles) move out of the cold start.

**A8a. The tape grows.**

- `TAPE_LEN = 4096`; `Op` keeps 16 bytes: `code: u32` holds the opcode in its low 16 bits and a
  third register in its high 16; `a`, `b` registers; `c: f32`. A `Tape` of 4,096 ops is 64 KiB +
  16 bytes, over the uniform limit (`MAX_UNIFORM_BUFFER_BINDING_SIZE`), so it binds as read-only
  storage, which `bind_uniform` already does for a block over 64 KiB (`gpu.rs:2540-2545`); a short
  tape costs the same bytes (the type is fixed-size). A second type `ShortTape` (`TAPE_SHORT =
  256`, 4 KiB, uniform) keeps the lens's run-time fields cheap; both implement `Surface`.
- New opcodes, each defined by one line in `apply` (`stage.wrela:72-93`): `FLOOR`, `FRACT`,
  `STEP` (`a < b ? 1 : 0`), `SELECT` (`a > 0 ? b : third`), `MIX` (`a + (b - a) * third`),
  `CLAMP01`, `POW` (`pow(a, c)`), `LOG`, `MOD` (`a - b * floor(a / b)`), `LOAD` (`table[u32(c) +
  u32(clamp(a, 0, b))]`, a constant table read; `b` is a register holding the count), `LERP_LOAD`
  (linear between two entries at `a`), `NOISE3` (`std::field::value_noise(vec3(r[a], r[b],
  r[third]) * c)`, calling std's own noise, so the tape and the compiled field agree bit for bit
  on the GPU), `FBM3` (`fbm` with `c` holding `octaves` in its integer part and `gain` in its
  fraction, `0.5` by default), `HASH` (`hash_u32` of `bitcast_u32(floor(a))` combined with
  `floor(b)`, `floor(third)`, as `lattice` does), `DOT3` (`r[a]·r[b]` over three consecutive
  registers each), `LEN3`, `NORM3` (writes three registers). The tape gains a constant table
  `consts: [f32; 2048]` for `LOAD` (lofts' radii, harmonics). Register `COPY` and the rest stay.
- `interpret` loops to `tape.len` as today; `prune` learns the new ops' intervals through
  std's `interval` of `apply`, as now (`stage.wrela:247-265`), and prunes `SELECT`/`STEP` whose
  condition's interval is one-signed.
- **Tests:** each new op against its std twin at 10⁶ random inputs on both targets, bit-exact
  for `NOISE3`/`FBM3`/`HASH` (they call the same code) and within 1 ulp else; a 4,096-op tape
  binds and runs on both hosts; `prune` on a `SELECT` tape.

**A8b. `std::derive::tape`: a compiled field becomes a tape.**

- std: `@intrinsic pub fn tape(f: fn(vec3) -> f32) -> Tape {}` in `derive.wrela` beside
  `gradient`, and `Surface::to_tape(self) -> Tape` in `field.wrela` as `tape(|p| self.distance(p))`.
  Also `tape_with<T: GpuData>(f: fn(vec3) -> (f32, T)) -> Tape`, the analogue of
  `value_gradient_with`: the tape's last `1 + n` registers are the distance and `T`'s `n` floats
  (`T` is scalars and vectors of `f32`, flattened; anything else is E0708), and
  `interpret_with::<T>(tape, p) -> (f32, T)` reads them. That is how a field's channels (a
  colour, a material) travel with its distance, so a tape can cook colours as a compiled field
  does.
  `Lang::TapeOf = "std::derive::tape"` in `sema/src/defs.rs:576-582`; it joins
  `derived_callables` (`effects.rs:542-551`) so the derived effects apply.
- lower: `DeriveKind::Tape` (`instance.rs:34-46`); `derive.rs:22-27` maps it; `signature()` returns
  the lowered `Tape` ADT; `build()` calls `ir::derive::tape(m, cache, inner, ncap, target)`; add
  the arm at `lower/src/gpu.rs:1180`.
- ir: `derive/tape.rs`, a `Builder` driven by the AD activity analysis with `p` active
  (`mod.rs:177-189, 320, 706-720`). It runs **at build time on the CPU target only** (a tape is
  made once and uploaded): inactive statements run as ordinary code (they compute the concrete
  values: a part's placement, a radius, a seed), and each active value becomes a register. The
  emitted ops: `Binary` add/sub/mul/div; `Builtin` min, max, abs, sqrt, sin, cos, exp, log, pow,
  floor, fract, step, mix, clamp (as `CLAMP01` after scaling, or `MIN`/`MAX`), `length`,
  `dot`, `normalize` on `vec3`s (three registers each: a `vec3` active value is three registers,
  a `vec4` four); `smooth_min` (std's `smin`, recognized by path) → `SMIN`; `select`/`if` on an
  active condition → both sides emitted and `SELECT` (an `if` whose both sides are straight-line
  code; a branch with side effects on inactive state is E0708, "a tape can't hold this branch");
  a call to `std::field::value_noise`/`Noise::value` on an active vector → `NOISE3`/`FBM3`; a
  table read at an active index → the table's values copied into `consts` and `LOAD`/`LERP_LOAD`;
  loops with a count known at tape time (a loft's sections, a creature's parts) unrolled; any
  other active construct (an active loop count, recursion, an active write to captured state) →
  E0708 naming the construct and the line. A tape over 4,096 ops or 2,048 constants → E0709 with
  the counts.
- **Tests:** the derivation corpus's fields (`compiler/tests`: the ones `interval` is tested on)
  each `to_tape()` and agree with their compiled twin within 1e-5 at 10⁶ points on both targets
  (the spec's existing tape test, now over every corpus field, not three); the tape's `gradient`
  (derived from `interpret`) agrees with the field's within 3.4e-4 relative; E0708 and E0709
  tests; a tape of a corpus creature (the herd's grazer, if the corpus has it; else a loft of 20
  sections with a displacement) is under 2,048 ops and the number is recorded in the test.
- **Spec:** §14.4 (fields built at run time; tapes from compiled fields; the op set; the errors),
  §20.3 (`to_tape`).
- **Gate:** G-suite. §B2.5 measures what it buys; if a creature of the Last Green can't tape
  (E0708), the op set grows in 0.8.0 before the tag, not in a 0.8.1.

**Done when, A8a:** (1) `TAPE_LEN = 4096`, `ShortTape`, `consts` and every new opcode exist
with a test per opcode against its std twin at 10⁶ inputs on both targets (bit-exact for
`NOISE3`, `FBM3`, `HASH`; within 1 ulp otherwise; paste the worst error per op); (2) a 4,096-op
tape binds (the manifest shows a storage binding) and runs on both hosts; (3) `prune` folds a
`SELECT` whose condition is one-signed; (4) the cost of `interpret` is measured: nanoseconds per
op per point on the M4 for a 1,024-op tape over 10⁶ points (paste it; it is §B2.5's budget).
**Done when, A8b:** (5) `std::derive::tape`, `tape_with`, `Surface::to_tape` exist and every
corpus field's tape agrees with its compiled twin within 1e-5 at 10⁶ points on both targets
(paste the count of fields and the worst difference); (6) the tape's `gradient` agrees with the
field's within 3.4e-4 relative; (7) E0708 and E0709 tests pass and name the construct; (8) a
loft of 20 sections with a displacement tapes under 2,048 ops (paste the count); (9) through
`WRELA_TOOLCHAIN`, a test in this workspace lists every creature and built thing of
`games/last-green` (the fawn, the wolf, the tower, the posts, the egg stones, the crag beds, the
three bank prototypes, and whatever else `blockouts::built` yields) with its tape's op and
constant counts, and none fails with E0708 (paste the table). A kind that fails names the
construct, and the opcode set grows in this release until the table is complete.
Hill to climb: the count of the Last Green's kinds that tape, now 0 → all; the longest tape,
→ under 4,096 ops.


### A9. `wrela precision`: where `f16` is safe, from the interval derivation

*Needs: nothing.* The interval walker widens every result by its target's rounding
(`interval.rs:44, 472-569`: `F32_EPS`, per-op ulps). An `f16` build of the same walk says, per
operation, how wide the result gets at half precision.

- **The command:** `wrela precision <pkg> <fn> --box "<lo>..<hi>[,...]" [--tol 1e-3] [--json]`,
  registered in `compiler/cli/src/main.rs:92-216`. `<fn>` is a GPU-callable function of scalars
  and vectors (an entry point's helper, a field's `distance`); `--box` gives each input's range
  (the box the interval is taken over; several boxes may be given, comma-separated, for a sampled
  domain).
- **The walk:** a new `Mode::Half` beside `Interval`'s `gpu` widening: `eps = 2⁻¹¹`, `tiny =
  2⁻¹⁴` (f16's smallest normal), `inf = 65504`, and each op's ulps as the GPU table has them;
  every `widen_k` call records `(span, op, width_f32, width_f16)` through a hook on the `Ivx`
  (`:331-358`). The derived function is built for the CPU target and run at build (as `wrela
  solve` runs code) over the boxes.
- **The report:** per operation, in source order: file:line:col, the op, the value's f32 width,
  its f16 width, their ratio, and a verdict: `safe` (f16 width ≤ `tol` × max(|value|, 1)),
  `watch` (≤ 10×tol), `unsafe`; and a summary: the share of ops that are `safe`, and the
  **first unsafe op** on each path. A value that overflows 65504 or underflows `tiny` anywhere
  in the box is `unsafe` with "range".
- **It decides nothing by itself:** the engine's §B13.4 writes `f16` twins by hand where the
  report says `safe`, and G-precision measures them. 0.7.0's council refused *deriving* intervals
  in f16 (E0700: the rounding margins aren't proven sound); this report doesn't derive in f16. It
  walks the f32 interval as today and adds f16's rounding as extra widening per op, and its
  verdicts are advisory: the measured twin (G-precision) is the proof, never the report.
- **Tests:** `x * x + y * y` over `[-1, 1]²` is `safe` at `tol 1e-3`; `length(p) - 1000.0` over
  a box near 1000 is `unsafe` (cancellation at f16's 2⁻¹¹); a value noise's lattice hash is
  `unsafe` by range (its integer path), while `mix` of its corner values is `safe`; the report's
  JSON shape.
- **Docs:** `docs/toolchain.md`, "Watching and measuring".
- **Gate:** G-suite.

**Done when:** (1) `wrela precision` is registered and documented; (2) the four tests pass
(`safe` for `x*x + y*y`, `unsafe` cancellation, `unsafe` range for the lattice hash with `safe`
`mix`, the JSON shape); (3) a run on `std::field::value_noise` over `[-8, 8]³` prints at least
one `unsafe` op by range and names its line; (4) a run on a corpus field's `distance` finishes in
under a minute on the M4 (paste the time).
Hill to climb: none; §B13.4 climbs with it.


### A10. `gather`

*Needs: nothing.* Four depth compares of one texel quad are four `sample_compare_level`s today
(`shadow.wrela:125-149`); WGSL's `textureGather` fetches the quad's one component in one
instruction, and naga's writer supports it (`gather: Some(component)`, `wgsl/src/lib.rs:1519`).

- std: `Texture<F>::gather(s: Sampler, uv: vec2, component: u32 = 0) -> vec4` (the four texels'
  one channel, in WGSL's order), `TextureArray::gather(s, uv, layer, component)`,
  `DepthTexture::gather(s: Sampler, uv) -> vec4` (the four depths) and
  `DepthTexture::gather_compare(s: ComparisonSampler, uv, reference) -> vec4`
  (`textureGatherCompare`), the array forms too. Any GPU code (no derivatives involved).
  `component` is a build-time constant (E0603 otherwise; WGSL requires it).
- lower: `Lang` entries and `T::Gather { component } / T::GatherCompare`; WGSL emission sets
  `gather: Some(SwizzleComponent::…)`.
- **Tests:** a conformance test gathers a 2×2 of known values and asserts the order; a depth
  gather-compare against a known depth.
- **Spec:** §18.8.
- **Gate:** G-suite.

**Done when:** (1) the gather-order and gather-compare tests pass on both hosts; (2) the emitted
WGSL of the test contains `textureGather`; (3) spec §18.8 names the four forms.
Hill to climb: none; §B6c uses it.


### A11. Conservative depth, if WGSL can say it

*Needs: A1.* A fragment that writes `WithDepth` turns early-Z off for its draw (the impostors,
the soft raster's merge). WGSL's `fragment_depth` language extension lets a shader declare its
depth only ever grows (or shrinks) so early-Z stays on; Chrome 155–156 has it temporarily
disabled on every platform for a Windows compiler bug (§9).

- std: `WithDepthGreater<C>` and `WithDepthLess<C>` beside `WithDepth<C>` (`gpu.wrela:245-248`),
  same fields; the draw's manifest entry gains `"depth_mode": "greater" | "less"` (the field
  recipe). Lowering treats them as `WithDepth` for every check (`sema/src/gpu.rs:326`
  `writes_depth`).
- WGSL: **if naga 30.0.1 can emit the attribute** (check `naga::ir` for a conservative-depth
  binding on `FragDepth`; if absent, check whether a newer naga in the 30.x line can, and bump
  within the lockfile's policy), emit it and the `requires fragment_depth;` directive, and the
  browser requests the extension only when `navigator.gpu.wgslLanguageFeatures.has("fragment_depth")`,
  else strips the directive and attribute from the source before `createShaderModule` (a text
  rewrite of two known lines). If naga can't emit it at all, this step records a gap in
  `docs/toolchain-gaps.md` ("conservative depth") and ships the std types as plain `WithDepth`
  so the engine's code can name its intent now.
- **Tests:** a browser test on a device with the feature asserts the shader module compiles with
  the directive; the conformance test draws with `WithDepthGreater` and reads back the same image
  as `WithDepth`.
- **Spec:** §18.1.
- **Gate:** G-suite; the commit says which of the two outcomes happened.

**Done when:** either (a) the browser test on a device with `fragment_depth` compiles the module
with the directive and the conformance test draws `WithDepthGreater` as `WithDepth` does, or
(b) `docs/toolchain-gaps.md` has the "conservative depth" entry with naga's version and the
missing binding named, and the types compile as plain `WithDepth`; (c) in both cases the commit
says which outcome and why.
Hill to climb: none; §B5.6 uses it if (a).


### A12. The screen's colour: HDR and wide gamut

*Needs: A1.* The canvas is configured `{ format, alphaMode: "opaque" }` (`worker.ts:120-125`)
with no colour space or tone mapping; Chrome has had `toneMapping: { mode: "extended" }` with an
`rgba16float` canvas since 129 and ships `srgb-linear`/`display-p3-linear` canvas colour spaces in
155–156 (§9).

- std: `std::gpu::screen(Screen { hdr: bool = false, gamut: Gamut = Gamut::Srgb })` (`Gamut::{Srgb,
  DisplayP3}`), a command (`ConfigureScreen`, by the command recipe) a program may record at any
  time; `std::gpu::screen_is() -> Screen` (nondet: what the host configured, since a display may
  refuse HDR).
- browser: on `ConfigureScreen`, reconfigure the canvas: `format: hdr ? "rgba16float" :
  preferred`, `colorSpace: gamut == DisplayP3 ? "display-p3" : "srgb"`, `toneMapping: { mode: hdr
  ? "extended" : "standard" }`, keep `alphaMode`. The screen's pipelines (manifest `screen: true`
  targets, built in the device's format at load, `gpu.ts:298-320`) get a **variant** for the new
  format, created async; the swap happens when every screen pipeline has it (until then the old
  configuration stays). Test mode ignores the command (it reads back `rgba8unorm`). Native:
  wgpu's surface configure with `TextureFormat::Rgba16Float` where the surface supports it, else
  the command is a no-op and `screen_is()` says so.
- **What a program must do:** in HDR, the fragment of a screen pass returns **linear** values
  with headroom (1.0 is SDR white; the browser maps "extended" to the display); the program's
  tonemap moves from an sRGB curve to a linear output with its own highlight roll-off (§B13.3).
- **Tests:** a browser test configures HDR and asserts `canvas.getContext` reports the format
  and tone mapping (`getConfiguration()`), and that a 2.0 white reads back as 2.0 in an
  `rgba16float` readback of the screen texture; `screen_is()` returns what was set.
- **Spec:** §18.8 ("The screen").
- **Gate:** G-suite.

**Done when:** (1) `std::gpu::screen` and `screen_is` exist; (2) the browser test reads
`rgba16float`, `extended` and `display-p3` from `getConfiguration()` after the command and a
2.0 white back from the screen texture; (3) `screen_is()` reports what was set and `false` for
`hdr` where the device refuses; (4) test mode's goldens are unchanged (`wrela test` on
`engine/tests/frame` passes untouched); (5) the native host configures `Rgba16Float` where the
surface allows and reports otherwise.
Hill to climb: none; §B13.3 uses it.


### A13. Relaxed SIMD outside deterministic code

*Needs: nothing.* Every CPU function uses standard SIMD; relaxed SIMD (fused multiply-add,
relaxed lane ops) is refused module-wide (`wasm/src/lib.rs:603-662`) because `@deterministic`
code must be bit-exact across engines. The frame's CPU time (4–5 ms on the Last Green) is
presentation code, which is not held to bit-exactness.

- **Which functions:** `check_deterministic` (`lower/src/lib.rs:1021-1060`) already walks the call
  graph from every `@deterministic` root; its reach set is the deterministic set. Every function
  outside it, **and not reachable from a job the sim resumes nor from the audio thread** (both
  are deterministic by construction: `@audio` and `@deterministic` roots), may be relaxed. The
  set is computed per instance after inlining (`inline_cpu` merges bodies: a relaxed callee
  inlined into a deterministic caller must not relax it, so the flag is the **caller's**: a
  function is relaxed only if it is outside the set, and inlining into it inherits its flag).
- **The option:** `wrela build --relaxed-simd` (and `wrela profile --relaxed-simd`), default off
  (D16 decides the release default after §B13.5 measures). On: `Options.relaxed: HashSet<FuncId>`
  threaded into `vec_binary` (`ops.rs:487-550`): a `Mul` followed by `Add`/`Sub` on `f32x4`
  lanes in a relaxed function emits `F32x4RelaxedMadd`/`RelaxedNmadd`; `min`/`max` emit the
  relaxed forms; nothing else. `FEATURES` gains `--enable-relaxed-simd` when on.
  `check_no_relaxed_simd` runs on the **emitted** module per function (the emitter knows each
  function's flag) and is skipped after wasm-opt when the option is on (wasm-opt may move code
  across functions; the emitter's check is the guarantee). `@deterministic` code's bytes are
  unchanged: a test compares the tick log (`--compare-ticks`) of a build with and without the
  option.
- **Browsers:** Chrome 114+ and Firefox 146+ run relaxed SIMD; Safari doesn't (§9). The build's
  loader checks `WebAssembly.validate` of a one-function relaxed module at start and, failing
  that, loads the **standard** build: a relaxed build is written beside the standard one
  (`game.relaxed.wasm`), and the manifest names both; no compressed file is written for either (D26). The size budget counts the
  larger.
- **Tests:** a conformance test marks `f` deterministic and `g` not, both with `a * b + c` on
  vectors; the WASM has a relaxed op in `g` and none in `f`; `--compare-ticks` of a ticked program
  with and without the option is equal; a browser test loads the fallback when the validate fails
  (mocked).
- **Spec:** §11.2 (SIMD: "relaxed outside deterministic code, when asked").
- **Gate:** G-suite; the frozen `herd`'s CPU frame time with and without, recorded (no claim).

**Done when:** (1) the conformance test finds a relaxed op in `g`'s WASM and none in `f`'s;
(2) `wrela-host --compare-ticks` of a ticked program built with and without the option prints
"equal"; (3) the fallback-load browser test passes; (4) `optimize.rs` passes `--enable-relaxed-
simd` only when the option is on and a build without it is byte-identical to 0.7.0's; (5) the
frozen `herd`'s CPU frame time with and without, seven alternating pairs, is in the PR (no
threshold here; D16 decides in §B13.5).
Hill to climb: none here.


### A14. Serving on the LAN

*Needs: nothing.* The servers bind `127.0.0.1` (`loopback.rs:47`), and the test page writes its
results back with `PUT results/<name>` (`testmode.ts:193-197`), so a device on the LAN can run a
bench page and report, if the server listens.

- `std::tool::serve` gains `lan: bool = false` (and `wrela run --lan`, `wrela tool serve --lan`):
  bind `0.0.0.0`, print each interface's `http://<ip>:<port>/` with the page's hash, and accept
  `PUT results/` from any peer **only under `--lan`** (the live server's existing size and
  connection limits hold: 64 open requests, 10 s per request, 256 MiB per result). COOP/COEP
  headers stay, so WebGPU and `SharedArrayBuffer` work over plain HTTP only for `localhost`:
  document that a LAN device needs the page served over HTTPS or Chrome's
  `--unsafely-treat-insecure-origin-as-secure=<origin>` flag for the device's run (the bench's
  procedure in §B8 says which). The tool capability `browser = true` is unchanged (the agent's
  own browser is local; a remote browser is a person's).
- `std::tool::browser` gains `navigate(b, url)`: the open browser loads `url` in its tab (the
  same profile, so its caches are warm) and a following `run(b, Script::new().wait(Until::done(),
  …))` waits for that page's `DONE`. The bench's warm run (§B1.4) is this: the same page a second
  time. If the host's driver already exposes a navigation, wrap it; if not, add it to
  `runtime/native/src/tool/browser.rs` through the devtools protocol's `Page.navigate`.
- Test mode gains `show=1`: at the run's end the page draws its summary (the paced intervals'
  p50/p95/p99, the count over 33 ms, `playable_ms`, `pipelines_ms`, the adapter's description) as
  text on the canvas and leaves it there, for a device that can't `PUT` (a phone or a tablet on a
  served release page; §B8).
- **Tests:** a tool test starts `serve` with `lan: true` and GETs the page from the machine's
  own non-loopback address; a PUT from it lands in `results/`; a tool test opens hello-field,
  navigates to it again and reads a second `results/`; a browser test with `show=1` reads the
  summary's pixels back as not black.
- **Docs:** `docs/toolchain.md`, "Running a program".
- **Gate:** G-suite.

**Done when:** (1) `wrela run --lan` and `std::tool::serve(lan: true)` bind `0.0.0.0` and print
every interface's URL; (2) the tool test GETs the page from a non-loopback address and a PUT
lands in `results/`; (3) the navigate test reads a second `results/`; (4) the `show=1` test
reads non-black summary pixels; (5) `docs/toolchain.md` has the LAN notes (the secure-context
flag for Chrome, the release page for Safari).
Hill to climb: none; §B1.4 and §B8 use it.


### A15. Per-frame labels in a paced run, and the worst frames named

*Needs: nothing.* In a `timestamps=1` run every pass already gets timestamp writes
(`gpu.ts:984-988`, `Timer.keep`), and nothing rejects `timestamps=1&paced=1&inflight=2`
(`worker.ts:635-741`); `timings.json`'s rows carry `frame` (`GpuTiming`, `gpu.rs:59-73`). What is
missing is the join and the report.

- `wrela profile --chrome` gains `--worst n` (default 5): after the paced run, read
  `timings.json` (the paced run now requests `timestamps=1` too) and `frames.json`, and print the
  `n` frames with the longest intervals, each with its interval, its CPU ms, its `phases` and its
  labels' GPU ms sorted, largest first. `--json` carries them as `"worst": [...]`.
- `frames.json` rows gain `"labels": [[name, ms], ...]` when timestamps are on, written by the
  worker from the same `GpuTiming`s (`worker.ts:806`), so a tool reads one file per frame.
- The native host's `wrela profile --worst` does the same from its own timer.
- **Tests:** a tool test runs `wrela profile --chrome --worst 2` on `examples/hello-field` and
  asserts the shape; a frame row has `labels`.
- **Docs:** `docs/toolchain.md`.
- **Gate:** G-suite.

**Done when:** (1) `wrela profile --chrome --worst 2` on `examples/hello-field` prints two frames
with their intervals, CPU ms, phases and labels; (2) a paced run's `frames.json` rows carry
`labels` when `timestamps=1`; (3) the native profile prints the same; (4) `docs/toolchain.md`
names the flag.
Hill to climb: none; §B1.2 uses it.


### A16. Integration against the engine, before the tag

*Needs: A1–A15.* With `WRELA_TOOLCHAIN=~/projects/wrela-compiler` (this branch) in `~/projects/wrela`:

1. On `main`'s code, unchanged: `wrela suite` passes; for every program in §B0's list a build with
   this branch against a build with 0.7.0 holds **G-pixels (identical)** and **G-sim**, each run
   by its own release's host. `wrela migrate --dry-run` on every member reports nothing (Part A
   declares no migration).
2. On the `engine/world-class-2` branch, §B0–§B2 as drafted build and pass their gates.
3. `engine/tests/toolchain-0.7` (kept in Part B as the capabilities' living test) uses every new
   capability once: a program opened twice shows the warm compile (A2), a page served
   compressed on the fly (A3, read `load.json`'s wire bytes), a zero-run constant (A4), a tier-1 pipeline bumped (A5), a folded
   predicate (A6), a `const` parameter in two variants (A7), a corpus field's `to_tape()` realized
   through `interpret` and compared with the compiled field (A8), a `wrela precision` run on one
   of its functions (A9, a tool test), a `gather` (A10), a `WithDepthGreater` draw (A11), a
   `screen(Screen { hdr: true })` round trip (A12), a relaxed build's tick log equal to the
   standard's (A13), a `--lan` serve (A14, tool test), a `--worst` profile (A15). It runs `hosts:
   both` and with `features=none`.
4. `wrela run games/capybara` starts in the Claude app's browser pane and a mouse press on
   `Start` starts the game (no regression on 0.5.1's fix).

Faults found here are fixed in Part A before the tag. Record what ran in the PR.

- **Gate:** all four, recorded.

**Done when:** the four items above are recorded in the PR with their numbers: (1) the suite,
the per-program G-pixels and G-sim hashes; (2) B0–B2's gates as drafted; (3)
`engine/tests/toolchain-0.7` passing with `hosts: both` and with `features=none`, and the A8b
kind table complete; (4) the browser-pane check. A fault found here is fixed in Part A before
the tag; the PR lists each.


### A17. Spec, docs and the release

*Needs: A16.*

- `spec/language.md` covers every addition above in the same voice as the rest: §11.2 (relaxed
  SIMD), §13 (zero runs), §14.4 (tapes), §15 (the manifest's ABI), §18.1 (`const` parameters,
  conservative depth), §18.8 (`gather`, the screen), §18.9 (tiers, constants), §23.9 (`wrela
  precision`, `--worst`, `--lan`, `--report data`). `wrela doc std::stage::Tape`, `std::gpu::Screen`
  etc. answer. `wrela primer gpu` mentions tiers, `const` parameters and tapes.
- `docs/toolchain-gaps.md` in this repository: "A branch under a false constant still makes its
  pipelines" moves to "Fixed" (A6), naming 0.8.0 and the PR; a "conservative depth" gap is added
  if A11 found naga can't emit it.
- `docs/citations.md` (compiler): the WGSL specification's sections for overrides,
  `textureGather`, `fragment_depth`; the WebGPU specification's canvas configuration;
  the V8 code-caching note; the relaxed SIMD proposal.
- `tools/check.sh` passes in full on the M4 Air and on the Linux CI host; record which ran.
- `docs/release.md` §2 step 5: the agent syntax test once with the release's compiler; its
  headline numbers in the PR.
- Open the PR per `docs/release.md` §2; Ryan merges; tag `v0.8.0` per §3; check the release holds
  both tarballs and `SHA256SUMS`.
- **Gate:** the release exists and `wrela toolchain` downloads it in this repository.

---

**Done when:** (1) every spec section named above mentions its new item (grep each item's name
in `spec/language.md`); (2) `wrela doc` answers for `std::stage::Tape`, `std::gpu::Screen`,
`std::derive::tape`, `Texture::gather`; (3) the gaps entries moved; (4) `tools/check.sh` passes in
full on the M4 Air and on the Linux host, counts pasted; (5) the agent syntax test's headline
numbers are in the PR; (6) `v0.8.0` is tagged, the release holds both tarballs and `SHA256SUMS`,
and `wrela toolchain` in this repository downloads it.


