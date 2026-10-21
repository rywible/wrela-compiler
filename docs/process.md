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
| [Claude Code](https://claude.com/claude-code) (Anthropic), mostly Claude Opus 5.5 | Writes the code, tests and docs, on Ryan's MacBook Air and in cloud sessions |
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

## Keeping this record

AGENTS.md tells every agent to update this file in the same change when it adds an outside
crate, program, asset, algorithm or specification, a new AI tool or model, or AI in a new kind of
work.
