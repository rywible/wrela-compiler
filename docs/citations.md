# Citations

Every outside work that the toolchain uses: what it is, its rights, and how and why we use it.
If a line here is wrong, fix it in the same change that makes it wrong. The record of this
toolchain before the split on 2026-10-21 is in `rywible/wrela`'s `docs/process.md`.

Claude and Codex agents write almost all of the code, tests and docs; Ryan decides the goals and
the architecture, and merges every change. No generative image, sound, music, voice or video
model makes anything here.

## Tools and models

| Tool | Rights | How and why we use it |
|---|---|---|
| [Claude Code](https://claude.com/claude-code) (Anthropic), mostly Claude Opus 5.5 | Service | Writes the code, tests and docs, and researches sources |
| Claude Sonnet 5.5, through the Claude CLI (`claude -p`) | Service | The agent syntax test (`tools/agent-test`; version 2 in `tools/agent-test/v2`): fresh agents write programs from the spec alone, each attempt graded by hidden tests, to measure how learnable the language is |
| [OpenAI Codex](https://openai.com/codex/) (GPT-6, GPT-6.1) | Service | Reviews performance and security, and implements fixes with their regression tests |
| GPT-6.1-Sol and GPT-6-Astra (OpenAI) | Service | Read-only reviewers in a council on a plan or a pull request; they do not edit |
| [Binaryen](https://github.com/WebAssembly/binaryen) `wasm-opt` 133 | Apache-2.0 | Shrinks a released build's WASM (`wrela build --optimize`, `-Oz`) and keeps its trap locations as a source map. A release ships it with its licence, checked against a pinned SHA-256 (`tools/package.py`). It runs at build time; nothing of it ships in a game |
| Chrome stable | Found on the machine; not vendored | Runs the browser host's agreement scenarios and `std::tool::browser`, over the DevTools protocol (`runtime/chrome`) |
| [Bun](https://bun.sh/) and [TypeScript](https://www.typescriptlang.org/), with `@types/bun` and `@webgpu/types` | MIT; Apache-2.0 | Build, typecheck and test the browser host (`runtime/browser`); versions locked in its `bun.lock` |
| GitHub Actions | Service | Builds and publishes a toolchain release for each `v*` tag (`docs/release.md`) |

## Crates

All are used as libraries, through Cargo; no code was copied from them.

| Crate | Licence | How and why we use it |
|---|---|---|
| [wasmtime](https://github.com/bytecodealliance/wasmtime) 49, with Cranelift | Apache-2.0 with the LLVM exception | Runs a program's WASM on the native host, the build-time constants and the tool host |
| [wgpu](https://github.com/gfx-rs/wgpu) 30 and [naga](https://github.com/gfx-rs/wgpu/tree/trunk/naga) 30 | MIT or Apache-2.0 | The native host's GPU (Metal, Vulkan, DX12), and the parse and validation of every WGSL shader the compiler emits. The ABI's limits were checked against wgpu-core 30.0.1's validation (commit `40f4a34e`), read as reference |
| [wasm-encoder, wasmparser and wat](https://github.com/bytecodealliance/wasm-tools) | Apache-2.0 with the LLVM exception, Apache-2.0 or MIT | Encode the compiler's WASM, read it back in tests, and write test modules as text |
| [serde](https://serde.rs/) and serde_json | MIT or Apache-2.0 | The JSON of diagnostics, manifests, reports and the ABI |
| [mimalloc](https://github.com/purpleKarrot/mimalloc-rs) (Microsoft's [mimalloc](https://github.com/microsoft/mimalloc)) | MIT | The CLI's and the tests' allocator |
| [elsa](https://github.com/Manishearth/elsa) | MIT or Apache-2.0 | Append-only vectors in sema's type tables |
| [half](https://github.com/VoidStarKat/half-rs) 2.5 | MIT or Apache-2.0 | The WGSL back end writes a half-float literal as naga's `f16` (`std::gpu`'s half floats, toolchain 0.6); naga already depends on it |
| [sha2](https://github.com/RustCrypto/hashes) | MIT or Apache-2.0 | Keys of the constant and code caches, and the check of a downloaded toolchain release |
| [tokio](https://tokio.rs/) and [wtransport](https://github.com/BiagioFesta/wtransport) 0.7 (over [quinn](https://github.com/quinn-rs/quinn)) | MIT; MIT or Apache-2.0 | `wrela-serve` runs a server build's rooms over WebTransport; the tests' client |
| [url](https://github.com/servo/rust-url) 2.5 | MIT or Apache-2.0 | `wrela-serve` accepts only canonical HTTP(S) origins in its WebTransport allowlist |
| [cap-std](https://github.com/bytecodealliance/cap-std) and [rustix](https://github.com/bytecodealliance/rustix) | Apache-2.0 with the LLVM exception, Apache-2.0 or MIT | The tool host opens files relative to a root's directory handle, so no path or link leaves the root; process groups are killed without `unsafe` |
| [libc](https://github.com/rust-lang/libc) | MIT or Apache-2.0 | `O_NONBLOCK` for a tool's files; pipes and directories between `fork` and `exec`; `_exit` and, on Linux, `PR_SET_PDEATHSIG` for a driven build |
| [ctrlc](https://github.com/Detegr/rust-ctrlc) | MIT or Apache-2.0 | `wrela tool` stops a tool on Ctrl-C and cleans up after it |
| [ureq](https://github.com/algesten/ureq) 3, over [rustls](https://github.com/rustls/rustls), with [webpki-roots](https://github.com/rustls/webpki-roots) | MIT or Apache-2.0; rustls also ISC; webpki-roots CDLA-Permissive-2.0 (Mozilla's root certificates, as data) | `std::tool::http`'s client, and the download of a pinned toolchain release |
| [tar](https://github.com/alexcrichton/tar-rs) and [flate2](https://github.com/rust-lang/flate2-rs) | MIT or Apache-2.0 | Unpack a downloaded toolchain release; flate2 also gzips a build's files as the native servers send them (`Accept-Encoding`, toolchain 0.8) |
| [png](https://github.com/image-rs/image-png) 0.18 | MIT or Apache-2.0 | The GPU host's frames and goldens, and `std::tool::image`'s PNGs |
| [zune-jpeg and zune-core](https://github.com/etemesi254/zune-image) 0.5 | MIT, Apache-2.0 or Zlib | `std::tool::image` decodes JPEGs, without SIMD paths, so a JPEG gives the same pixels on every machine |
| [webp](https://github.com/jaredforth/webp) and [libwebp-sys](https://github.com/NoXF/libwebp-sys), building Google's [libwebp](https://chromium.googlesource.com/webm/libwebp) 1.3.1 with the [CVE-2023-4863 fix](https://github.com/NoXF/libwebp-sys/pull/23) | MIT or Apache-2.0; MIT; libwebp BSD-3-Clause with Google's patent grant | `std::tool::image` decodes WebPs and encodes lossy ones |
| [base64](https://github.com/marshallpierce/rust-base64) and [shlex](https://github.com/comex/rust-shlex) | MIT or Apache-2.0 | `runtime/chrome`: DevTools screenshots, and `WRELA_CHROME_ARGS` split as a shell splits it |
| [pollster](https://github.com/zesterer/pollster) | MIT or Apache-2.0 | Waits on wgpu's futures in the native host and the tests |
| [rusqlite](https://github.com/rusqlite/rusqlite) 0.37, building [SQLite](https://sqlite.org/) | MIT; SQLite public domain | A server's store (`std::store`): one SQLite file that every room shares and that outlives a build |
| [p256](https://github.com/RustCrypto/elliptic-curves) 0.13, [rsa](https://github.com/RustCrypto/RSA) 0.9 and [ed25519-dalek](https://github.com/dalek-cryptography/curve25519-dalek) 2 | MIT or Apache-2.0; BSD-3-Clause | A server's signature checks (`std::crypto::verify`): ES256, RS256 and Ed25519, the algorithms passkeys use. No cryptography is written here; the tests sign with the same crates, and rand_chacha (MIT or Apache-2.0) seeds a test's RSA key |
| [getrandom](https://github.com/rust-random/getrandom) 0.3 | MIT or Apache-2.0 | A server's randomness (`std::crypto::random`), from the operating system |

## Specifications and published methods

Each is used as facts or as a method; no code was copied.

| Source | Rights | How and why we use it |
|---|---|---|
| musl's libm; ARM's optimized-routines; Dekker's and Knuth's exact products and sums | MIT; published methods | `compiler/std/math.wrela` follows their algorithms and coefficients (credited in the file), for f32 and f64 |
| FIPS 180-4 (SHA-256); NIST CAVP short-message vectors | US government publications | `std::hash::sha256` and its test vectors |
| RFC 3720, appendix B.4 (CRC-32C) | IETF | The check of the rooms' journal records (`runtime/native/src/rooms.rs`) |
| RFC 8259 (JSON) | IETF | `std::json` |
| W3C Web Authentication Level 3; RFC 8949 (CBOR), RFC 9053 (COSE keys), RFC 4648 (base64url) | W3C and IETF | Passkeys: the browser host's `passkey/create` and `passkey/get`, and `std::webauthn`'s checks of an authenticator's answer (§7, without attestation) |
| RFC 1950 (Adler-32) | IETF | The browser host's PNG writer (`runtime/browser/src/png.ts`) |
| gitignore(5) | Git documentation | `wrela suite allow` reads a workspace's `.gitignore` files as git does |
| Pillow's Lanczos3 resize and `thumbnail` size rule | HPND; read as a description | `std::tool::image`'s resize and `image::fit`; the test computes its reference from the Lanczos formula itself |
| Chrome's [DevTools protocol](https://chromedevtools.github.io/devtools-protocol/), and `--remote-debugging-pipe` as Playwright launches it | Published documentation | `runtime/chrome`'s driver |
| W3C: [WGSL](https://www.w3.org/TR/WGSL/), the [Gamepad API](https://www.w3.org/TR/gamepad/)'s standard mapping, [WebTransport](https://www.w3.org/TR/webtransport/), [WAI-ARIA](https://www.w3.org/TR/wai-aria-1.2/) and its [Authoring Practices](https://www.w3.org/WAI/ARIA/apg/), [UI Events](https://www.w3.org/TR/uievents/), [Input Events](https://www.w3.org/TR/input-events-2/) | W3C document license | The WGSL the compiler emits; the pad's buttons and axes (`runtime/abi`, `std::input`); the browser host's connections, semantic tree and text input |
| W3C [WebGPU](https://www.w3.org/TR/webgpu/): its Texture Format Capabilities tables (filterable, storable, renderable, blendable, render target pixel byte cost and alignment, and the feature each needs); usage scopes, tracked per texture subresource; the optional features (`shader-f16`, `subgroups`, `texture-compression-bc`, `-etc2`, `-astc`, `float32-filterable`, `float32-blendable`, `rg11b10ufloat-renderable`); the limits `maxColorAttachments` and `maxColorAttachmentBytesPerSample`; `setViewport`'s bounds (as wgpu-core 30's `set_viewport` checks them); and whole-subresource copies of depth textures. [WGSL](https://www.w3.org/TR/WGSL/)'s `f16` and `subgroups` extensions and their uniformity rule | W3C document license | `runtime/abi`'s format table and checker, the hosts' feature requests, and the compiler's feature-gated pipelines (toolchain 0.6) |
| Chrome's [multi-draw indirect](https://chromestatus.com/feature/5104557318209536) (`chromium-experimental-multi-draw-indirect`) | Published documentation | The browser host's one-call multi-draw where the device has it |
| W3C [WGSL](https://www.w3.org/TR/WGSL/): [override declarations](https://www.w3.org/TR/WGSL/#override-decls) and pipeline-overridable constants; [`textureGather`](https://www.w3.org/TR/WGSL/#texturegather) and [`textureGatherCompare`](https://www.w3.org/TR/WGSL/#texturegathercompare), their component order and the depth forms' samplers; the [`frag_depth`](https://www.w3.org/TR/WGSL/#built-in-values-frag_depth) built-in. W3C [WebGPU](https://www.w3.org/TR/webgpu/)'s [canvas configuration](https://www.w3.org/TR/webgpu/#canvas-configuration) (`format`, `colorSpace`, `toneMapping`) | W3C document license | Toolchain 0.8: an entry point's `const` parameters (language.md §18.1), `gather` (§18.8), a fragment's depth with its intent (§18.1, `WithDepthGreater`), and the screen's HDR and gamut (§18.8, the browser host's `ConfigureScreen`) |
| V8's [code caching for WebAssembly developers](https://v8.dev/blog/wasm-code-caching) | Published documentation | Why the browser host compiles the WASM as it streams in from a URL (`WebAssembly.compileStreaming`) and the build states its interface in the manifest: a second open finds the compiled code in Chrome's cache (language.md §15, toolchain 0.8) |
| The WebAssembly [relaxed SIMD proposal](https://github.com/WebAssembly/relaxed-simd/blob/main/proposals/relaxed-simd/Overview.md) | W3C Community Group | A relaxed-SIMD build's second module: its `f32x4.relaxed_madd`, `relaxed_min` and `relaxed_max` (language.md §11.2), and the one-op module the browser host validates to choose it |
| Hillis and Steele, "Data parallel algorithms" (CACM 1986) | Published algorithm | `std::gpu::workgroup_prefix_sum`'s scan |
| [WebTransport over HTTP/3 draft](https://ietf-wg-webtrans.github.io/draft-ietf-webtrans-http3/draft-ietf-webtrans-http3.html), July 2026 | IETF Trust Legal Provisions | `wrela-serve` checks the browser Origin before it accepts a session, as the draft requires |
| The [llms.txt proposal](https://llmstxt.org/) | Published proposal | The `llms.txt` that `wrela doc --export` writes |
| Inigo Quilez's [distance functions](https://iquilezles.org/articles/distfunctions/) | Published article | `std::field`'s round cone (credited in the file) |
| SplitMix64 (Steele, Lea and Flood, 2014) | Published algorithm | The grammar fuzzer's generator (`compiler/grammar/src/rng.rs`) |
| [RustSec advisories](https://github.com/RustSec/advisory-db) through the [OSV API](https://google.github.io/osv.dev/post-v1-querybatch/) | Public domain; imported GitHub advisories CC BY 4.0 | Every locked Rust and browser package was matched against published advisories on 2026-10-23; affected Wasmtime, Tokio, rustls-webpki and webpki-roots versions were updated in `Cargo.lock` |

## Research read for compiler optimization

In October 2026 we read these papers and projects to find and judge compiler optimizations.
They informed small experiments, not shipped code; no code, figures or text were taken, and no
dependency was added.

| Source | What it taught us |
|---|---|
| [Keeter, MPR (2020)](https://www.mattkeeter.com/research/mpr/keeter_mpr20.pdf); [Fidget](https://www.mattkeeter.com/projects/fidget/) and its [2024 slides](https://mattkeeter.com/research/fidget-2024.pdf) (source MPL-2.0, not used) | Interval pruning of expression tapes over regions, and how pruning and JIT gains overlap |
| [Lipschitz Pruning (CGF 2025)](https://doi.org/10.1111/cgf.70057) | Region pruning by Lipschitz bounds, and why its contracts do not hold for arbitrary functions |
| [AnyDSL](https://anydsl.github.io/) | Prior art for generic staging and residual programs |
| [Enzyme (2020)](https://arxiv.org/abs/2010.01709) | Optimization before AD changes the derivative's cost in reverse mode |
| [Knoll et al. (2009)](https://doi.org/10.1111/j.1467-8659.2008.01189.x) | Reduced affine arithmetic as a stronger, costlier alternative to intervals |
| [Incremental Lambda Calculus (2014)](https://doi.org/10.1145/2594291.2594304); [cache-transfer style (2019)](https://doi.org/10.1007/978-3-030-17184-1_20); [DBSP (2023)](https://www.vldb.org/pvldb/vol16/p1601-budiu.pdf); [Differential Execution (2025)](https://drops.dagstuhl.de/entities/document/10.4230/LIPIcs.ECOOP.2025.20) | Foundations of exact incremental updates |
| [Chen, Dunfield and Acar (PLDI 2012)](https://research.cs.queensu.ca/home/jana/papers/incr); [Acar et al. (2009)](https://www.cs.cmu.edu/~guyb/papers/ABBHT09.pdf) | Measured gains and costs of self-adjusting computation |
| [ShortCut (2019)](https://doi.org/10.1145/3341301.3359659) | Guarded executable slices with a fallback |
| [Automatic GPU layout (2015)](https://d-nb.info/1160593248/34); [C++ data views (2025)](https://arxiv.org/abs/2502.16517); [Mutable value semantics (2022)](https://www.jot.fm/issues/issue_2022_02/article2.pdf) | Data-layout choice, its conversion costs, and copy-on-write for value types |
| [AsyncTaichi (2021)](https://arxiv.org/abs/2012.08141v2); [TACO autoscheduling (2022)](https://tensor-compiler.org/files/ahrens-pldi22-taco-autoscheduling.pdf); Futhark ([PPoPP 2019](https://www.futhark-lang.org/publications/ppopp19.pdf), [PLDI 2017](https://www.futhark-lang.org/publications/pldi17.pdf)); [Halide (2012)](https://people.csail.mit.edu/jrk/halide12/halide12.pdf) | Kernel fusion, sparse schedules and their measured effects |
| [Guided Equality Saturation (2024)](https://doi.org/10.1145/3632900); [Axon (2026)](https://arxiv.org/abs/2606.26344); [Chassis](https://arxiv.org/pdf/2410.14025); [Synthesized data compression (2026)](https://arxiv.org/abs/2610.09913) | Bounded search over equivalent programs, and why tolerance-based float rewrites cannot apply to strict CPU code |
| [Dex (ICFP 2021)](https://research.google/pubs/getting-to-the-point-index-sets-and-parallelism-preserving-autodiff-for-pointful-array-programming/) | Parallel reverse-mode AD over in-place updates |

## Fixtures

The image fixtures in `compiler/tests/images` are four flat colours that Claude made. The frozen
programs in `compiler/tests/frozen` are copies from `rywible/wrela`, whose `docs/process.md`
records their sources.
