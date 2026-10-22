# wrela-compiler: how it's made

*What AI does in this repository, what people do, and every outside work the toolchain uses and
how. If a line here is wrong, that is a bug: fix it in the same change that makes it wrong.*

This repository is the Wrela toolchain: the compiler, std, the browser, native and server hosts,
the spec and the toolchain's own checks. It split from `rywible/wrela` on 2026-10-21, with its
history. Its record before that date, and the record of the whole project, is in `rywible/wrela`'s
`docs/process.md` ("Two-repository split" and "The split, done" tell how the split was made).

Ryan (the owner) decides the goals, the architecture and what ships. Claude and Codex agents write
almost all of the code, tests and docs. Every change passes `tools/check.sh` locally and goes in
by a pull request that Ryan merges. No generative image, sound, music, voice or video model makes
anything here.

## Tools and models

| Tool | What it does here |
|---|---|
| [Claude Code](https://claude.com/claude-code) (Anthropic), mostly Claude Opus 5.5 | Writes the code, tests and docs, on Ryan's MacBook Air and in cloud sessions. Its deep-research workflow searches the web, reads sources and has subagents vote on each claim (see "A second check, by Claude deep research"). One session can orchestrate parallel Opus subagents, each in its own git worktree, sharing the machine through a lock (see "Exploration of each opportunity") |
| [OpenAI Codex](https://openai.com/codex/) (GPT-6, GPT-6.1) and parallel subagents | Reviews performance across the compiler, the hosts and the tools, implements optimizations and regression tests, and measures their effects |
| [GPT-6.1-Sol and GPT-6-Astra](https://openai.com/) (OpenAI), as T3 Code child tasks or `codex exec` runs | Reviewers in a council on an implementation plan or a pull request: each reads the brief and the checkout read-only, returns findings, and rebuts the leader's answers; they don't edit. Claude leads, decides, implements and verifies |
| Claude Sonnet 5.5 subagents | The agent syntax test (`tools/agent-test`): fresh agents write small programs from the spec alone, to measure how learnable the language is |
| [Binaryen](https://github.com/WebAssembly/binaryen) (Apache 2.0), `wasm-opt` 133 | Shrinks a released build's WASM (`wrela build --optimize`, `-Oz`), carrying its trap locations through as a source map. A toolchain release ships `wasm-opt` 133 from Binaryen's GitHub release, checked against a pinned SHA-256, with its licence (`tools/package.py`); a development build uses 133 or later from `PATH`. Runs at build time; nothing of it ships in a game |
| Chrome stable | Runs the browser host's agreement scenarios and `std::tool::browser`, driven over the DevTools protocol (`runtime/chrome`). Found on the machine; not vendored |
| [Bun](https://bun.sh/) and [TypeScript](https://www.typescriptlang.org/) (MIT, Apache 2.0), with `@types/bun` and `@webgpu/types` | Build, typecheck and test the browser host (`runtime/browser`); the versions are locked in its `bun.lock` |
| GitHub Actions | Builds and publishes a toolchain release for each `v*` tag (`.github/workflows/toolchain-release.yml`); it runs no checks |

## Outside crates

All are used as libraries, through Cargo; no code was copied from them.

| Crate | Licence | What it does here |
|---|---|---|
| [wasmtime](https://github.com/bytecodealliance/wasmtime) 49, with Cranelift | Apache-2.0 with the LLVM exception | Runs a program's WASM on the native host, the build-time constants and the tool host |
| [wgpu](https://github.com/gfx-rs/wgpu) 30 and [naga](https://github.com/gfx-rs/wgpu/tree/trunk/naga) 30 | MIT or Apache-2.0 | The native host's GPU (Metal, Vulkan, DX12), and the WGSL parse and validation of every shader the compiler emits. The ABI's limits were checked against wgpu-core 30.0.1's resource and render validation (commit `40f4a34e`), read as reference |
| [wasm-encoder, wasmparser and wat](https://github.com/bytecodealliance/wasm-tools) | Apache-2.0 with the LLVM exception, Apache-2.0 or MIT | Encode the compiler's WASM, read it back in tests, and write test modules as text |
| [serde](https://serde.rs/) and serde_json | MIT or Apache-2.0 | The JSON of diagnostics, manifests, reports and the ABI |
| [mimalloc](https://github.com/purpleKarrot/mimalloc-rs) (Microsoft's [mimalloc](https://github.com/microsoft/mimalloc)) | MIT | The CLI's and the tests' allocator |
| [elsa](https://github.com/Manishearth/elsa) | MIT or Apache-2.0 | Append-only vectors in sema's type tables |
| [sha2](https://github.com/RustCrypto/hashes) | MIT or Apache-2.0 | Keys of the constant cache and of the code cache (and of a run's driven builds' code), and the check of a downloaded toolchain release |
| [tokio](https://tokio.rs/) and [wtransport](https://github.com/BiagioFesta/wtransport) 0.7 (over [quinn](https://github.com/quinn-rs/quinn)'s QUIC) | MIT; MIT or Apache-2.0 | `wrela-serve` runs a server build's rooms over WebTransport; the tests' client |
| [cap-std](https://github.com/bytecodealliance/cap-std) and [rustix](https://github.com/bytecodealliance/rustix) | Apache-2.0 with the LLVM exception, Apache-2.0 or MIT | The tool host opens files relative to a root's directory handle, so no path or link leaves the root; process groups are killed without `unsafe` |
| [libc](https://github.com/rust-lang/libc) | MIT or Apache-2.0 | `O_NONBLOCK` for a tool's files; Chrome's pipe, and a driven build's pipes and directories, between `fork` and `exec`; a driven build's process's `_exit` and, on Linux, `PR_SET_PDEATHSIG` |
| [ctrlc](https://github.com/Detegr/rust-ctrlc) | MIT or Apache-2.0 | `wrela tool` stops a tool on Ctrl-C and cleans up after it |
| [ureq](https://github.com/algesten/ureq) 3, over [rustls](https://github.com/rustls/rustls), with [webpki-roots](https://github.com/rustls/webpki-roots) | MIT or Apache-2.0; rustls also ISC; webpki-roots CDLA-Permissive-2.0 (Mozilla's root certificates, as data) | `std::tool::http`'s client, and the download of a pinned toolchain release |
| [tar](https://github.com/alexcrichton/tar-rs) and [flate2](https://github.com/rust-lang/flate2-rs) | MIT or Apache-2.0 | Unpack a downloaded toolchain release |
| [png](https://github.com/image-rs/image-png) 0.18 | MIT or Apache-2.0 | The GPU host's frames and goldens, and `std::tool::image`'s PNGs |
| [zune-jpeg and zune-core](https://github.com/etemesi254/zune-image) 0.5 | MIT, Apache-2.0 or Zlib | `std::tool::image` decodes JPEGs, without SIMD paths, so a JPEG gives the same pixels on every machine |
| [webp](https://github.com/jaredforth/webp) and [libwebp-sys](https://github.com/NoXF/libwebp-sys), building Google's [libwebp](https://chromium.googlesource.com/webm/libwebp) 1.3.1 | MIT or Apache-2.0; MIT; libwebp BSD-3-Clause with Google's patent grant | `std::tool::image` decodes WebPs and encodes lossy ones |
| [base64](https://github.com/marshallpierce/rust-base64) and [shlex](https://github.com/comex/rust-shlex) | MIT or Apache-2.0 | `runtime/chrome`: DevTools screenshots, and `WRELA_CHROME_ARGS` split as a shell splits it |
| [pollster](https://github.com/zesterer/pollster) | MIT or Apache-2.0 | Waits on wgpu's futures in the native host and the tests |

## Specifications and published methods

Each is used as facts or as a method; no code was copied.

| Source | Rights | How we use it |
|---|---|---|
| musl's libm; ARM's optimized-routines; Dekker's and Knuth's exact products and sums | MIT; published methods | `compiler/std/math.wrela` follows their algorithms and coefficients (credited in the file), for f32 and f64 |
| FIPS 180-4 (SHA-256); NIST CAVP's short-message vectors | US government publications | `std::hash::sha256`, and its test vectors |
| RFC 3720, appendix B.4 (CRC-32C) | IETF | The rooms' journal records' check (`runtime/native/src/rooms.rs`) |
| RFC 8259 (JSON) | IETF | `std::json` |
| RFC 1950 (Adler-32) | IETF | The browser host's PNG writer (`runtime/browser/src/png.ts`) |
| gitignore(5), git's documented pattern format | Git documentation | `wrela suite allow` reads a workspace's `.gitignore` files as git does |
| Pillow's Lanczos3 resize and `thumbnail` size rule (HPND), from its documentation and source | Read as a description | `std::tool::image`'s resize and `image::fit`; the resize's test computes its reference from the Lanczos formula itself |
| Chrome's [DevTools protocol](https://chromedevtools.github.io/devtools-protocol/), and Chrome's `--remote-debugging-pipe` as Playwright launches it | Published documentation | `runtime/chrome`'s driver (Target, Runtime, Log, Input, Fetch, Emulation, Browser and Page) |
| W3C: [WGSL](https://www.w3.org/TR/WGSL/) (its memory model and atomics), the [Gamepad API](https://www.w3.org/TR/gamepad/)'s standard mapping, [WebTransport](https://www.w3.org/TR/webtransport/), [WAI-ARIA](https://www.w3.org/TR/wai-aria-1.2/) and its [Authoring Practices](https://www.w3.org/WAI/ARIA/apg/), [UI Events](https://www.w3.org/TR/uievents/) and [Input Events](https://www.w3.org/TR/input-events-2/) | W3C specifications, under its document license | The WGSL the compiler emits; the pad's buttons and axes (`runtime/abi`, `std::input`); the browser host's connections, its semantic tree and its text input |
| The [llms.txt proposal](https://llmstxt.org/) | Published proposal | The `llms.txt` that `wrela doc --export` writes |
| Inigo Quilez's [distance functions](https://iquilezles.org/articles/distfunctions/) | A published article; no code taken | `std::field`'s round cone (credited in the file) |
| SplitMix64 (Steele, Lea and Flood, 2014) | A published algorithm | The grammar fuzzer's generator (`compiler/grammar/src/rng.rs`) |

The image fixtures in `compiler/tests/images` are four flat colours that Claude made. The
frozen copies under `compiler/tests/frozen` are copies of programs from `rywible/wrela`, whose
`docs/process.md` records their sources.

## Compiler optimization research, 2026-10-21

Ryan asked Codex and research subagents to find compiler changes that could produce orders of
magnitude faster programs. Six subagents audited the compiler and investigated spatial bounds,
incremental computation, program synthesis, GPU scheduling and representation. Exa found source
papers and author projects. This section records the findings and proposed experiments. No
optimization was implemented and no Wrela performance comparison was run. Research includes
publications through 2026-10-21; it does not establish that an idea is unprecedented.

Across five external-research workstreams and the lead's coverage checks, successful Exa
searches returned 309 result entries, including repeated papers and mirrors. Failed schema
requests are excluded. The subagents report 46 primary documents or URLs read in relevant
sections, before cross-workstream deduplication; this is not a count of 46 independent studies.
Only claims supported by the linked original sources are retained below.

The strongest opportunity is to remove whole computations using facts about their domain,
dependencies and consumers. Ordinary instruction selection seldom changes how much work a
program must do. A function that currently evaluates N branches but needs only k of them in a
region has a possible N/k work reduction. An independent computation with k changed inputs out
of N has a similar opportunity. These are conditional work ratios, before preparation, memory,
dispatches and other program costs. They are not measured speedups.

No pass below is something that only Wrela could implement. Wrela's useful combination is
whole-program callable specialization, derived interpretations, exclusive mutation, no observable
addresses, strict CPU arithmetic, and control of both CPU and GPU compilation. Compilers given
the same contracts can perform the same transformations. The research opportunity is to use
these contracts together, without teaching the compiler what an engine or a surface is.

### What the compiler already does

The source audit used the checked-out compiler and the language spec, rather than assuming a
general-purpose optimizer's feature set:

- [Lowering](../compiler/lower/src/lib.rs) specializes generic functions, callable identities
  and GPU resources. CPU optimization already includes inlining, scalar replacement, load
  forwarding, CSE, dead code and store removal, branch folding and sinking.
- [GPU optimization](../compiler/ir/src/opt.rs) already forwards uniform reads, specializes
  fixed uniform paths, controls inlining and outlining, unrolls small loops and converts eligible
  branches to selects. Its opening comment records a historical 19x gap caused by uniform copies,
  and the existing fix. That gap is not a new opportunity.
- [Derivation](../compiler/ir/src/derive/mod.rs) already tracks active leaves, produces gradients
  and target-aware intervals, and combines values, gradients and payloads. The spec's section 14
  describes guard narrowing, indexed-table bounds and splitting narrow floor crossings.
- [Tape pruning](../compiler/std/stage.wrela) already eliminates dominated min/max/smooth-min
  operands. Section 14.4 explicitly says compiled fields are not pruned.
- [WASM loop SIMD](../compiler/wasm/src/func/vloop.rs), vector SIMD, some bounds-check removal,
  build-time constants and their cache already exist. Explicit result caches and lifted-literal
  dependency watches also exist. `--optimize` runs Binaryen with `-Oz`; a speed-oriented option
  comparison would be a useful baseline, without being novel research.

`tools/wrela primer derive` and `primer mem` supplied checked language examples. There is no
`primer compiler` area; Rust source inspection supplied the architecture. This is a tooling
discoverability limit, not a missing language feature.

### Proposed passes and experiments

The order below favors structural work removal. Each proposal needs a small falsifiable
experiment before an implementation plan. A prerequisite is retaining useful ownership,
read/write-region, trap and dependency facts in optimization IR; the current broad effects
and conservative invalidation of nonlocal loads do not supply all of those facts.

**1. Generate region proofs and residual compiled functions.** Derive intermediate ranges from
ordinary numeric code. For `min(a(p), b(p))`, a proof that `a(B).hi < b(B).lo` permits the result
inside box B to use a alone, subject to effects, traps and the required interpretations. Remove
the dead producer computations too. A caller chooses regions, batching and lifetime; the compiler
supplies a generic transformation, without recognizing `Surface` or rendering policy.

Specialize for what the consumer observes too. A threshold-only query may need a proof of one
comparison rather than an exact scalar, all payload channels and a gradient. Generate a cheap
decision program with exact fallback, preserving any effects and traps of the original call.
Do not count removal of unused tuple fields as new: existing value/gradient fusion and dead-code
elimination already cover some of this case. Generic staging has substantial prior art,
including [AnyDSL](https://anydsl.github.io/).

The high ceiling is replacing a large expression by a small local program before thousands of
point queries. [Keeter's MPR paper (2020)](https://www.mattkeeter.com/research/mpr/keeter_mpr20.pdf)
is direct prior art for interval-based arithmetic-tape simplification. Its example's 17x and 216x
reductions measure average tape length, not frame speed. The paper also measures substantial
interpreter overhead. [Lipschitz Pruning (2025)](https://doi.org/10.1111/cgf.70057) reports a
629x rendering example against naive sphere tracing on a 6023-node scene. Its operand and blend
contracts do not hold for arbitrary Wrela functions. Wrela's scoped `lipschitz(near)` is not a
global 1-Lipschitz certificate, and an engine may already remove much of this work.
The paper also uses conservative far-field replacement, which changes distance values. Its
headline result does not isolate transparent exact residualization; such replacement needs a
consumer contract that accepts bounds instead of the original values.

The difficult design problem is representing local programs. A shader compiled for every tile
would cost too much; every branch-choice combination compiled ahead of time grows exponentially.
Compare capped ahead-of-time variants, coherent compiled subtree skipping and shortened tapes.
Keep an original-function fallback. Use the existing interval transform before GPU if-conversion,
then the current forwarding/CSE/dead-code passes on the residual IR.

First experiment: frozen functions with 32, 128 and 512 separated branches, overlapping smooth
branches, and a noise-heavy control. Compare current compiled queries, existing tape pruning and
regional residual queries. Use the tape baseline only when the complete function fits its
128-operation capacity; larger cases compare compiled and residual queries. Charge proof
construction, lookup, variant creation, transfer, memory,
shader size and query execution. Reject if realistic reuse cannot repay preparation or if gains
vanish when gradients and payloads are included. This is the strongest possible large-factor
rendering direction found, with no measured Wrela gain yet.

**2. Optimize invariants before derived-code expansion.** Classify inputs by when they change:
build, edit/instance, frame/dispatch, workgroup and invocation. Extract repeated invariant work
and preserve it across derivation. Examples include transform coefficients and bounded-loop
subexpressions that do not depend on the differentiated input. Existing uniform-path
specialization avoids copies; it does not generally precompute arbitrary uniform-only work.

[Enzyme (2020)](https://arxiv.org/abs/2010.01709) shows that optimization and differentiation do
not commute in cost: hoisting before AD can produce an O(N) derivative where AD followed by
hoisting leaves O(N squared). Its reported 4.5x geometric mean belongs to its benchmarks, not
Wrela. Wrela currently derives during lowering and runs major CPU/GPU cleanup later. This is a
concrete pass-ordering question worth testing.

Only interpretation-preserving transformations qualify. Primal equivalence alone need not
preserve rounded tangents, interval enclosures, filtering or tie rules. Preserve the target of
numeric computation: CPU and GPU transcendental results are not interchangeable. Hoisting from
a zero-iteration loop must not introduce a trap. First compare current lowering with safe
invariant extraction before AD on bounded loops, for 1, 4, 32 and 128 active parameters. Count
derived operations, code size and total execution. Reject when cached storage and preparation
outweigh saved computation. This is established compiler research with a Wrela-specific seam.

**3. Generate exact sparse update programs and owned caches.** Extend dependency analysis to
parameter fields, literal reads and bounded buffer regions, including control and index
dependencies. Generate initialization and update functions for independent maps and bounded
stencils. Given an explicit dirty set, reexecute affected leaves with the original arithmetic;
keep unchanged outputs and profitable intermediates in caller-owned state. Avoid an O(N) scan
just to discover k changes.

The foundations are [Incremental Lambda Calculus (2014)](https://doi.org/10.1145/2594291.2594304),
[cache-transfer style (2019)](https://doi.org/10.1007/978-3-030-17184-1_20), and
[DBSP (2023)](https://www.vldb.org/pvldb/vol16/p1601-budiu.pdf).
[Differential Execution (2025)](https://drops.dagstuhl.de/entities/document/10.4230/LIPIcs.ECOOP.2025.20)
adds a proved imperative formulation for assignments, branches and loops. Its large Bellman-Ford
update gains require context: the tested inserted edges do not change shortest paths, table
indices are assumed stable, and initialization has substantial overhead. It does not establish
fast changing scenes or GPU execution.

Start with fixed-size pure maps, then stencils, before general loops. Existing `std::cache`
rebuilds a whole result after a key change; this proposal updates within it. Numerical gradients
are not exact finite changes. A strict float sum cannot generally use
`new_sum = old_sum - old_term + new_term`, or change its reduction tree. Cached expensive leaves
may still help while the original ordered fold is replayed. Checked integer overflow also
prevents unrestricted reassociation.

First experiment: a seeded expensive map with dirty fractions 0, 0.001, 0.01, 0.1 and 1. Compare
full recomputation, a manual sparse-update oracle and generated updates. Check every resulting
CPU value by canonical bits, including signed zero, branch/index changes and rollback. Measure
initialization, 100 updates, dense-update slowdown, dirty-list cost and cache bytes. A proposed
success target is a net 10x update gain at 0.1% changes, close to the manual oracle. That target
is a test of the idea, not a prediction. Global coupling may make every output dirty.

**4. Keep spatial certificates valid across time and parameter changes.** Combine proposals 1
and 3: prove a branch gap stays positive over a spatial cell and a bounded parameter/time box.
Reuse its local program until an input leaves that box. The useful output is a validity region
and its dependencies, rather than a per-frame flag that discards everything.

This is a research composition of parametric partial evaluation, interval proofs and
incrementalization. [ShortCut (2019)](https://doi.org/10.1145/3341301.3359659) is prior art for
guarded executable slices with fallback; guarded residual programs are not unprecedented.
Wrela already supports a nested spatial-gradient interval with time in a fourth component.
General capture-domain proofs need more than the current at-most-four-component derivation API.
Test slow deformation, fast crossings and noise discontinuities. Charge certificate refreshes
and failed guards. Reject when boxes become too loose or expire too frequently. Hot reload and
every relevant captured input must invalidate or leave the certificate's validity domain.

Use cheap intervals first. If a valuable decision remains unknown, try a bounded correlated
domain or centered enclosure. [Knoll et al. (2009)](https://doi.org/10.1111/j.1467-8659.2008.01189.x)
demonstrate reduced affine arithmetic, including cases that improve on intervals and cases
where its extra cost loses. This is a selective proof-strengthening tool, not a default 100x
optimization. Target rounding and nonlinear remainder error must be enclosed too.

**5. Choose AD mode for many editable parameters.**
[Literal gradients](../compiler/ir/src/derive/literal.rs) propagate up to 255 forward directions
and build dense seeds. For one scalar objective with P inputs and evaluation cost C, reverse
mode can replace roughly O(P C) work by O(C + P), plus saved state and checkpointing. This is a
credible large-factor authoring/solver opportunity; spatial gradients have only 1-4 inputs.
Compare reverse, sparse forward and vector forward at increasing P, including memory and
initialization. Vector forward can be competitive on SIMD/SIMT hardware; parameter count alone
does not select a winner.

The current spec explicitly promises forward-mode parameter gradients. Reverse mode can change
rounded derivative values and accumulation order, so it requires a defined API/spec contract,
not a silent optimizer substitution. It must preserve the chosen branch and nonsmooth tie rules.
This is a proposed language/tooling decision for Ryan, not an adopted architecture change.

**6. Plan container representation across consumers and byte boundaries.** Section 6.10 already
permits SoA, AoSoA and hot/cold storage, while current containers use AoS. Choose representations
using field-use summaries and total conversion cost, with canonical `GpuData`, bitcast, upload
and network bytes at the points where bytes are observed. Fold conversion into an existing
boundary traversal where legal. No addresses and nonescaping projections make this easier than
in a language with arbitrary pointer observation.

SoA alone is established work and no blanket large-factor claim follows. Large cold fields,
repeated gathers or bad scalarization are promising cases. Profile complete producer/consumer
pipelines, not only a field-streaming loop. Include cloning, canonicalization, upload and readback.
Reject plans that save one kernel but add more conversion traffic elsewhere. Persistent
representation planning is more distinctive than a local AoS-to-SoA rewrite.
[Automatic GPU layout research (2015)](https://d-nb.info/1160593248/34) and
[C++ data views (2025)](https://arxiv.org/abs/2502.16517) establish the prior art and conversion
tradeoffs. The current `Vec` data/length/capacity representation is assumed by both std's unsafe
core and lowering; changing only the IR layout table would be unsound.

Snapshot-heavy programs offer a separate representation experiment: chunked copy-on-write may
turn full copies into shared roots plus copies of written chunks. It is established mutable
value-semantics work, not a new algorithm. Do not apply it to arbitrary `Clone`: element cloning
can have effects, and snapshot timing, ownership and resource lifetimes matter. Restrict an
experiment to proven exact copies, with sparse writes and explicit lifecycle assertions. See
[Implementation Strategies for Mutable Value Semantics (2022)](https://www.jot.fm/issues/issue_2022_02/article2.pdf).
Whole-array copy-on-write still copies the whole array on its first write; chunking is needed
for sparse-write benefits.

**7. Fuse proven independent dispatches and generate sparse schedules.** Retain typed dispatches
and resource dependence information before the command stream is serialized. Consecutive maps
over the same domain can sometimes keep per-slot state in registers and remove intermediate
buffer traffic. Reject fusion with neighbor dependencies, observed intermediates, incompatible
domains, atomics, append operations, or synchronization that cannot be preserved. Portable WGSL
barriers synchronize a workgroup, not an entire dispatch
([WGSL memory model](https://www.w3.org/TR/2026/CRD-WGSL-20261021/#memory-model)). CUDA persistent task graphs cannot
simply become WGSL megakernels.

[AsyncTaichi (2021, final v2)](https://arxiv.org/abs/2012.08141v2) is prior art for whole-program sparse
kernel optimization. [TACO asymptotic autoscheduling (2022)](https://tensor-compiler.org/files/ahrens-pldi22-taco-autoscheduling.pdf)
shows why sparse schedules can change asymptotic work, rather than only constants.
[Futhark incremental flattening (2019)](https://www.futhark-lang.org/publications/ppopp19.pdf)
uses multiple equivalent versions selected by input shape; a single static GPU schedule is not
always sufficient. These are mechanisms to adapt, not evidence of an exclusive Wrela pass.

First test 4, 16 and 64 independent repeated maps, then explicit sparse domains, against the
current stream and a manual fused kernel. Charge compilation, command recording, spills and
memory. Compaction of a divergent branch does not give 1000x just because only 1/1000 lanes are
active: subgroup width, work distribution and prior early exits bound the useful savings.

**8. Use synthesis to select structural plans, with bounded search.** Equality saturation can
explore schedules and equivalent residual forms after legality is established. It should not
be a license to reassociate strict floats. Score plans by repeated work, memory traffic,
register pressure, code size and preparation amortization, then benchmark finalists. The
search itself is supporting infrastructure; it does not supply an algorithmic speedup.

[Guided Equality Saturation (2024)](https://doi.org/10.1145/3632900) demonstrates much cheaper
search with sketches, which is a compiler-time result.
[Axon (2026)](https://arxiv.org/abs/2606.26344) and other recent synthesis work must be checked
for their numeric contracts: proving real arithmetic and accepting a floating-point tolerance
does not prove strict CPU equivalence. A useful small experiment compares a bounded set of
legal plans against the best existing compiler/manual plan, with total search time reported.

Other research was considered but does not justify a large runtime claim here.
[Synthesized data compression (October 2026)](https://arxiv.org/abs/2610.09913) suggests an offline
experiment for immutable tables: generate a bounded decoder and exhaustively verify every
reconstructed byte. Its textual-codec results do not establish faster numeric tables or frames.
Correctly rounded math and mixed-precision tuning likewise need their own numeric contracts;
they are not permission to change current CPU results silently.

### Concrete preparation improvement

Before the larger residualization experiment, measure `Tape::prune` preparation. It calls
`register_interval` twice per eligible binary operation, and that helper re-evaluates a tape
prefix. One forward abstract evaluation producing all intermediate ranges, followed by reverse
liveness and remapping, could avoid repeated prefixes. Share the generated target arithmetic
rules rather than writing a divergent second interval implementation. Test 16/32/64/128-op tapes
and enclosure/tie cases. The 128-op limit bounds the absolute upside; this is an established
linear-work technique and a preparation improvement, not a new 100x frame optimization.

### Correctness and evidence requirements

CPU strict floats, ordered reductions, canonical NaNs, signed zero and observable trap order
remain contracts. Deterministic code may allocate, trace or panic; determinism is not enough
to justify caching or deleting computations. Unknown proof results take the general fallback.
Min/max ties average tangents, so non-strict dominance that preserves a value can still change
its gradient. Payloads and filtering need their own preservation argument. Existing GPU interval
NaN and transcendental-domain limits must be resolved or excluded before using them as a proof
oracle. Samples are useful regressions, not universal soundness proofs.

Do not multiply paper speedups. The [Fidget author's comparison](https://www.mattkeeter.com/projects/fidget/)
is a useful warning: JIT improves its brute-force example by 31x, but after pruning the comparison
is 6 ms versus 4.6 ms. The two mechanisms eliminate overlapping costs. The project is prior art,
not a dependency; its [source](https://github.com/mkeeter/fidget) is MPL-2.0 and no code was copied.

For every experiment record current-compiler, manual-oracle and candidate results; preparation,
invalidation, memory, code size, compilation, warm and total time; and dense/adversarial controls.
Use frozen fixtures here. Keep correctness assertions separate from performance thresholds.
Run paired `tools/wrela profile` with equal scene, input, size, workers and timing mode on each
Linux hardware GPU, without concurrent builds/GPU work. Keep periodic M4 Air reports separate.
Distinguish GPU times, synchronized CPU frame calls and Chrome frame rates. No such performance
run was made for this research. If a kernel occupies fraction f of total time and speeds up by
s, total speedup is `1 / ((1 - f) + f/s)`; even a 100x improvement of 10% of a frame is only
about 1.11x for the frame.

The proposed first research spike is region-certified compiled residualization, with dependency
and trap summaries as its narrow prerequisite. Optimize-before-AD and sparse exact updates are
separate experiments with strong source-level seams. Reverse AD targets authoring tools. Layout,
fusion and synthesis support these directions; none is evidence for a universal 100x compiler.

All external papers and specifications in this section were read as descriptions of methods
and published evidence. No source code, figures or substantial quoted text were imported; no
new crate or runtime dependency was added. Paper publication/access rights remain with their
authors and publishers. Individual reported gains retain their original workload and baseline.

### A second check, by Claude deep research

Ryan also asked Claude Code (Opus 5.5) the same question, through its deep-research workflow.
Five subagents searched the web, each on one angle. Other subagents fetched 19 sources and
extracted 87 falsifiable claims. Three independent subagents then tried to refute each of 25
claims. Two refutations removed a claim. 19 claims survived and 6 were removed. The other
62 claims were not checked. A last subagent merged the survivors. A separate
read-only subagent listed the current compiler passes. The run used 101 subagents. It changed
no code and made no Wrela performance run.

The result agrees with the section above. The first experiment is region-certified compiled
residualization. Sparse exact updates are second. Layout, fusion and synthesis support them.
This subsection adds only the vote results and the numbers that the check confirmed. The vote
is given as votes for the claim against votes to refute it.

- **Region pruning** ([MPR](https://www.mattkeeter.com/research/mpr/keeter_mpr20.pdf),
  [Fidget slides](https://mattkeeter.com/research/fidget-2024.pdf)), 3-0. The text model's tape
  has 6056 clauses, 2354 of them min/max. The mean pruned tape is 356±125 clauses for 64x64
  tiles and 28±13 for 8x8 subtiles. These means count only ambiguous tiles. On a GTX 1080 Ti,
  the brute-force interpreter costs about 58 ms per megapixel and the compiled expression about
  3 ms, so the interpreter costs 19x. The pruned renderer takes 7.4 ms at 4096x4096. It is
  faster than the compiled kernel from about 1536x1536. The "6x faster than compiled" and
  "more than 100x faster than brute force" multiples come from fitted slopes; the brute-force
  line is extrapolated past about 4 megapixels.
- **Code quality alone** (Fidget slides), 3-0. Fidget's CPU JIT is about 2.4-2.6x faster than
  its VM and 2.7-2.8x faster than libfive. It is near the GPU MPR at 1024³ (23.6 ms against
  22.6 ms). So better instruction selection gives less than 3x here.
- **Lipschitz pruning** ([CGF 44(2), 2025](https://diglib.eg.org/handle/10.1111/cgf70057)). Only
  the abstract was readable, because the publisher refused the full paper. This check did not
  confirm a speedup factor, so the 629x above is from the first research only.
- **Fusion, coalescing and tiling** ([Halide 2012](https://people.csail.mit.edu/jrk/halide12/halide12.pdf),
  [Futhark PLDI 2017](https://www.futhark-lang.org/publications/pldi17.pdf)), 3-0. These are
  Futhark ablations on a GTX 780 Ti, each with one pass off:
  - fusion: 1.2-10.1x; without it, three benchmarks fail from storage;
  - coalescing transposes: 4.2-9.3x;
  - tiling: 1.3-2.3x.

  Against expert code the gains are small. Halide is within 5% of hand-written NEON on the camera
  pipeline and 2.1x faster than OpenMP plus IPP on the local Laplacian. Futhark ranges from 0.6x
  to 16x, with a 1.81x geometric mean on the 12 benchmarks that have CUDA or OpenCL references.
  Its 16x comes from a sequential reduction in the reference.
- **Incremental computation** ([Chen, Dunfield and Acar, PLDI 2012](https://research.cs.queensu.ca/home/jana/papers/incr),
  [Acar et al. 2009](https://www.cs.cmu.edu/~guyb/papers/ABBHT09.pdf)). The PLDI 2012 results
  were confirmed 3-0: `map` at n=10⁶ updates 4.6x10⁴ times faster, and a 1000x1000 blocked matrix
  multiply 1200 times faster. The ray tracer gains only 0.22-4.29x, and it is slower than
  recomputing when 57% of pixels change. A from-scratch run costs 2-30x more, and memory is
  about 10x the input. The 2009 result, more than 10⁴x for one list insert at n=10⁶, passed only
  2-1.
- **Accuracy-aware e-graph rewriting** ([Chassis, preprint](https://arxiv.org/pdf/2410.14025)),
  3-0. It is 3.5x faster in aggregate, and up to 8.9x faster than Clang at equal accuracy on
  the C99 target. Against Herbie it is up to about 2x faster, but only a few percent faster on
  hardware instruction sets. It changes float results, so it cannot apply to strict CPU code.
- **Dex's `Accum` effect** ([ICFP 2021](https://research.google/pubs/getting-to-the-point-index-sets-and-parallelism-preserving-autodiff-for-pointful-array-programming/)),
  3-0 but low value. It keeps reverse-mode AD parallel over in-place updates, but it has no
  measured gain, and its reduction order conflicts with strict float sums.

The check refuted six claims:

- A static ahead-of-time compiler could close MPR's 19x interpreter gap (0-3). The paper does not
  show this. It remains the open question of proposal 1's first experiment.
- Lipschitz pruning needs only the Lipschitz property and supports smooth CSG as such (0-3).
- Spores gives a 1.2-5x end-to-end gain (1-2).
- Halide's choice to recompute or store needs purity in the way the claim stated (0-3).
- Self-adjusting computation is more than 1000x faster on the geometry benchmarks (1-2).
- Dex's effect typing alone makes its parallelization legal (1-2).

The check confirmed nothing on these angles: superoptimization, supercompilation and staging,
learned cost models, subgroup and register-pressure work, f16 tuning, loop perforation, shader
variant pruning, struct-of-arrays layout for WASM SIMD, and optimization after AD. No source
measured WebGPU or WASM. The hardware ranges from Fermi to Pascal.

The pass list adds these gaps to "What the compiler already does":

- No pass hoists loop invariants. `sink_values` never moves code into a loop.
- CSE never merges loads or calls.
- CPU and GPU cleanup decide purity from IR expression kinds, not from the source effects. They
  approximate no-aliasing with an `address_taken` scan in each function.
- [Layout](../compiler/ir/src/layout.rs) gives every type the same WGSL array-of-structs layout.
- `vloop` takes only counted `u32` loops of straight-line f32 code.

The pass list missed `Tape::prune`, which the section above names. The run used the sources
only as descriptions of methods and evidence. It imported no code, figures or substantial quoted
text, and it added no crate.

### Exploration of each opportunity, 2026-10-21

Ryan then asked Claude Code (Opus 5.5) to explore every opportunity above. One orchestrating
session started nine Opus 5.5 subagents, each in its own git worktree with its own branch from
`main`. They shared one Linux machine: a Ryzen 7 5700U with 13 GB RAM and its AMD Lucienne
integrated GPU (RADV). A file lock let one build, test, GPU or profile run happen at a time, with
cargo at 6 jobs and each run capped at 7 GB. A watchdog reported work outside the lock and low
memory. Ryan asked for cheap estimates rather than full workloads, so each agent counted
operations, bytes or code size first and timed at most a few small cases. A number is MEASURED
when it was timed or counted from real output, and ESTIMATE when it comes from a model. Most
timings are one run without repetitions. The agents changed no default. Eight kept their
experiments as local commits. One opened a pull request (`Tape::prune`, below).

Each verdict comes with what the agent found in the code. Where an agent corrected the research
above, the correction is given.

- **1. Region residuals: Ryan's decision; leaning towards a narrow form.**
  - On 128 separated spheres, exact residuals gave 6.2x for the CPU value, 12x for the CPU
    gradient and colour, and 8x on the GPU (MEASURED). They were 1.0–1.3x for overlapping
    smooth minima and 1–2x for noise.
  - A region proof costs 4.4 µs. It pays back after about 22 value queries or 3 gradient queries.
  - Compiled per-region variants grow with regions times kept branches; at 128 spheres the
    gradient shader exceeds the 256 KiB limit. The tape interpreter costs 72x compiled code on
    the CPU. Skipping subtrees with a per-site mask, or an index list over data, matched the
    hand-written residual.
  - The interval derivation must output its min/max choices, with site IDs that stay stable
    through AD, inlining and CSE. The IR has no summary of what a function can trap on, and
    every call counts as having an effect.
  - `Tape::prune` keeps an operand at an exact tie (`<=`), where the original gradient averages
    both operands' tangents (§14.2). A tie can therefore change the gradient. Pruning one
    operation at a time keeps 23 of 128 spheres where global dominance keeps about 8.
  - The straight-line full gradient for 128 spheres takes 8.2 ms on the GPU, 16x the loop form.
    That is a separate codegen issue.
- **2. Invariants before AD: rejected as a general loop-hoisting pass for now.**
  - Correction: the Enzyme O(N²)→O(N) result comes from caching in reverse mode. Wrela's forward
    mode copies inactive code as is, so hoisting before or after AD gives the same arithmetic
    counts.
  - Correction to the pass list above: CPU `reuse_loads` does merge redundant loads. Only calls
    are never merged.
  - A hand-hoisted fixture ran 10.7x faster (MEASURED). But in the frozen programs, invariant
    work is only 0.7–1% of derived-loop statements, 2–5% of CPU loops and 7.5–15.6% of GPU loops
    (ESTIMATE). Debug NaN checks and zero-trip loops make float hoisting change traps.
  - The pass worth trying is narrower: about 40% of the fixture's transcendental calls were
    duplicates. They become calls in `rewrite_cpu_math`, after derivation, and nothing merges
    them later.
- **3. Sparse exact updates: worth pursuing as a std library first.**
  - The fixture was 65,536 elements, a map with a 3-point stencil and an ordered f32 sum.
    Correctness was checked bit for bit, including signed zero, branch and index changes, and
    rollback.
  - At 0.1% changed inputs, a hand-written update was 42.9x faster than recomputing everything:
    4.10 ms against 175.8 ms for 20 updates (MEASURED). The model predicted about 44x. Replaying
    the strict ordered sum was 91% of the sparse cost. At 100% changed inputs, it was 0.84x.
  - Comparing values misses a change from +0 to −0. `total - old + new` gives different bits.
  - A compiler-generated update needs memory read/write facts that the IR does not keep, and a
    source for the changed set. Both are language or architecture choices for Ryan.
- **4. Certificates across time: Ryan's decision; leaning towards folding it into proposal 1.**
  - Time does not destroy certificates. About 89% of one-frame proofs still held after 64
    frames, for slow deformation and drifting noise; 35% held for a fast crossing (MEASURED).
  - Plain intervals are already loose in one frame: they decide 0.28 of cells, where sampling
    allows up to 0.55. A box over more than 4 inputs cannot be expressed today.
  - An interval of an interval is infinite everywhere on the CPU. The CPU's NaN guard,
    `select(x == x, x, ±inf)`, cannot be decided over a range. So the "derivations nest" promise
    of §14.2 holds for the interval of a gradient, not of an interval.
- **5. Reverse-mode AD: Ryan's decision.**
  - At 16 directions, each forward direction cost about 0.69x of the primal. A hand-written
    reverse pass got all 256 derivatives at 3.1x of the primal, about 61x faster than forward
    mode's 16 calls (MEASURED). 132 of 256 derivatives were bitwise equal; the worst differed
    by 96 ulps. Reverse mode therefore needs a spec decision.
  - Code grows linearly with directions, but wasmtime's load time grows about as the largest
    function's size to the power 2.5. That was 1 s at 16 directions; a 255-direction run reached
    9 GB and was stopped. Above about 32 directions, the generated code is not usable today.
  - Sparse forward mode is not value-preserving here: skipping a direction gives +0 where the
    dense tangent can be −0 or NaN.
  - `gradient` with N = 0 makes malformed IR, because of `n.max(1)` in `derive/literal.rs`.
- **6. Container representation: Ryan's decision; leaning against a compiler-chosen layout.**
  - Correction: `layout.rs` lays out element types only. Std's `Vec` (about 42 raw
    `data + i*size` uses), lowering, the constant evaluator and address projections fix
    array-of-structs.
  - With 16,384 bodies of 112 bytes, hand-written struct-of-arrays was 14x faster on the hot
    pass and uploaded the hot field in 22.7 µs, against 2.0 ms for the whole buffer (MEASURED).
    A snapshot was slower, 309 µs against 209 µs, because `Vec::clone_into` copies one element
    at a time.
  - Struct-of-arrays wins only if a conversion back to array-of-structs bytes happens less than
    about once every 10 frames (ESTIMATE).
  - `vloop` never vectorizes a loop over struct fields or over `Vec` indexing.
- **7. Dispatch fusion: worth pursuing as a narrow pass in MIR.**
  - A chain of 16 maps over 1M slots took 3.87 ms of GPU time unfused and 0.22 ms fused, 17x,
    as the bandwidth model predicted (MEASURED). The fused kernel matched bit for bit on RADV.
  - MIR dispatches keep the kernel, buffers and domain. Lowering turns them into runtime
    handles, and the hosts give each dispatch its own compute pass. So only the compiler can
    fuse.
  - Ryan's decisions: whether fusion must keep the same bits or only §11.3's GPU tolerance, an
    output type that can scatter for sparse schedules, and the change to the pipeline count.
- **8. Synthesis: rejected as an e-graph for now.**
  - Commutative CSE and repeated CPU cleanup changed sizes by less than 1%. Running the GPU
    cleanup to a fixed point cut WGSL by 4.1% (herd) and 2.1% (grazer), for 32–108 ms of
    compile time (MEASURED; GPU time not measured). The CPU inline limit moves WASM size by up
    to 24%, in different directions for different programs.
  - Binaryen levels, MEASURED: on wasmtime, herd's CPU simulation was fastest without
    `wasm-opt`. It was 5.2% slower at `-Oz` and up to 13% slower at other levels. `-Oz` took 24 s
    on grazer. `-O4` gave the smallest gzip size on grazer (−8%) and wolf (−6.6%). The browser
    was not measured. The level choice is Ryan's.
- **`Tape::prune` preparation: a pull request.**
  - [rywible/wrela-compiler#10](https://github.com/rywible/wrela-compiler/pull/10) finds every
    register's interval in one pass of `interval` over each operation. It keeps the old prefix
    derivation where a bound is not finite. Pruned tapes and intervals are bit-identical.
  - Prune time on 128-operation tapes went from 31.2 µs to 5.57 µs (CSG) and from 142 µs to
    7.24 µs (dense), MEASURED.
  - Its Chrome tests could not start Chrome in the agents' sandbox. They also fail there on
    unchanged `main`, so they need a run from a normal terminal.

The strongest measured results are fusion (17x on a memory-bound chain), sparse updates (42.9x
at 0.1% changed inputs) and region residuals (6–12x on separated branches). Each is a fixture
result on one machine, not a frame-rate result. The frame effect follows Amdahl's formula above.

### Follow-through, 2026-10-21

Ryan then gave the orchestrating Claude session full autonomy to do what it judged best, and to
merge pull requests. It chose the cheap correctness fixes, the local codegen wins, and checks of
real code before any large pass. Five more Opus 5.5 subagents worked in their own worktrees,
through the same lock. The lock now also stops each command's systemd scope when it ends, so a
process that a test leaves behind is killed.

Merged:

- [#10](https://github.com/rywible/wrela-compiler/pull/10): `Tape::prune` finds each register's
  interval in one pass.
- [#12](https://github.com/rywible/wrela-compiler/pull/12): #10 passed two literals by position
  (W0005), which failed `warnings::std_has_no_warnings`; the operands are now named.
- [#11](https://github.com/rywible/wrela-compiler/pull/11): `std::lift::gradient` by no literals
  gives an empty gradient. The derived type was `[f32; n.max(1)]`, which disagreed with sema's
  `[f32; 0]`.
- [#13](https://github.com/rywible/wrela-compiler/pull/13): intervals nest on the CPU. A float
  `x == x` or `x != x` is decided where `x`'s range is finite, which agrees with the documented
  convention that an interval bounds a function's non-NaN values. A nested interval that was
  [-∞, ∞] is now within about 4 ulps of the flat 4D one. `Tape::prune`'s dominance tests are
  strict, so a tie keeps both operands and the derived gradient matches the original's.
- [#14](https://github.com/rywible/wrela-compiler/pull/14): `Vec::clone` and `clone_into` copy
  `Copy` elements with one `copy`, through a std-only `std::mem::is_copy` intrinsic. A
  16,384-element `Vec<f32>` snapshot went from 22.2 µs to 2.5 µs (MEASURED, 7 pairs).
- [#15](https://github.com/rywible/wrela-compiler/pull/15): two lift tests shared one temporary
  package directory. In parallel, one removed the other's, so one test failed in every full check
  and passed alone.

After these merges, the full `tools/check.sh` on `main` passed 29 of 31 checks. The failures were
the five environment tests below and the lift race, which #15 fixes.

Checked and stopped, because real code does not have the case:

- **Dispatch fusion.** A MIR survey of the frozen programs and every test package found 0
  fusible pairs. Different domains or workgroup sizes, texture or neighbourhood reads, atomics
  and indirect groups block all of them. Read-only, `rywible/wrela`'s engine has one per-frame
  candidate (wood `cull` → `hide`), worth tens of µs at most and only after a proof that an
  intermediate field is dead. The planned pass would be about 2,100 lines. Not built.
- **Merging duplicate math calls.** Normal builds of herd, grazer and hello-field have 0
  duplicate transcendental calls. Duplicates occur only in lifted builds' literal-gradient
  functions, where d/dθ sin θ computes a second cos θ beside the primal's.
- **Binaryen for native builds.** `--optimize` is already opt-in, and `rywible/wrela` uses it only
  for releases, so native runs already skip `wasm-opt`. Choosing a browser level needs a V8
  measurement.

Three codegen settings were then measured on `rywible/wrela`'s `examples/clearing` (108dc7ae).
Each run used `wrela profile --compare` with 3 alternating pairs of 30 warmup and 60 measured
frames, at 1280×720 on the AMD Lucienne iGPU, with experimental switches that were not kept:

- **GPU cleanup run to a fixed point.** It cut shader statements by 2–10%, but no label's GPU
  time moved (all within ±0.5%, inconclusive). The driver's shader compiler already does this
  work. Rejected.
- **No GPU unrolling.** `look` was 8.3% slower and nothing else changed measurably, so the
  current policy (at most 16 passes and 2,000 statements) stays.
- **CPU inline limit 125 instead of 250.** `clearing`'s `-Oz` WASM was 4.8% smaller (5.0%, 35 KB,
  gzipped) and its build was 5.5% faster. The herd's CPU benchmarks (`bench_eval` whole and
  pruned, `mass` at 2 cm, `raycast`; 7 alternating rounds in one wasmtime) were 0.2–1.1% slower,
  with identical results. The frames of `clearing` and the grazer are GPU-bound and did not
  change. A small size-against-speed trade, not changed without a V8 measurement.

A real scene, `rywible/wrela`'s `examples/clearing` at 108dc7ae, run with this toolchain at
1280×720 on the AMD Lucienne iGPU (one profile, 30 warmup and 60 measured frames): the GPU frame
span is 20.4 ms, and the clouds take 10.0 ms of it (49%). In the cloud march loop, 8 of 58
statements per step are loop-invariant, and 4 of those are constants (MEASURED with an
experimental survey). So the clouds' cost is steps times density evaluations, the engine's
algorithm, and not missing hoisting. Interval-based region skipping, which the engine can already
write with `interval`, is the lever there. `games/last-green` was not profiled, because its world
bake takes minutes.

Environment limits found in the agents' sandbox: Chrome does not start there, so the three Chrome
tests fail on unchanged `main`. Both `drive::` process-group tests also fail on unchanged `main`
there, with or without the lock's scope; they passed in the same sandbox earlier in the day.
Bun 1.4.2 aborts in about half of full `browser` runs, on `main` too. Each needs a run in a normal
terminal.

## Keeping this record

AGENTS.md tells every agent to update this file in the same change when it adds an outside
crate, program, asset, algorithm or specification, a new AI tool or model, or AI in a new kind of
work.
