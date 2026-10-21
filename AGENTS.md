# wrela-compiler agent instructions

MIT-licensed toolchain of Wrela, a language for browser games with AAA ambition: the compiler, std, the browser, native and server hosts, and the spec. Content is fields: functions over space. The Wrela code (engine, content, games, studio, tools) lives in `rywible/wrela`, which pins a released toolchain. Ship quality code and evidence-backed claims.

## Work

- Before starting work, fetch `origin` and pull the current branch with `--ff-only` if it has a remote branch. For a new task, run `git pull --ff-only origin main` on `main` before creating the task branch. Preserve local changes; report any sync blocker before editing.
- Work in each machine's normal local checkout; do not create worktrees. Ryan assigns work across the machines. Keep one active task branch per checkout; never implement on `main`. Use direct commands and ordinary checkout-local build output.
- Own implementation and verification. Routine work has standing permission. Ryan owns goals, architecture, lore and artistic direction; decide implementation details yourself.
- Ship the smallest complete change. Preserve other sessions' work. Delete losing experiments, dead code and unused constants; avoid speculative abstractions and compatibility scaffolding.
- Add focused regressions for new logic, fix failures at their cause, and run `tools/check.sh` before committing. While working, run the exact test or the group you are changing. Do not weaken assertions or skip tests for a pass.
- End completed work with a committed, pushed branch and a pull request against `main`. Describe the change, checks and any blockers; leave merging to Ryan through the PR workflow.
- Report frame-time and gate-time effects honestly. Use `tools/wrela profile` for paired relative costs on each Linux hardware GPU, with the same scene, input, resolution, workers and timing mode. Run without concurrent builds or GPU work. Check paired M4 Air baselines periodically; keep each machine's reports separate. Distinguish GPU timings, synchronized CPU frame calls and Chrome frame rates.

## Context and layers

- `spec/language.md`: current spec. Find headings with `rg -n '^#' spec/language.md`; read relevant sections. `spec/lexical.md`, `spec/grammar.ebnf` and `spec/wrela.gbnf` are the lexical rules and the grammar.
- `docs/process.md`: public record of this repo's human/AI work and outside sources.
- Prefer `tools/wrela context <item> <package>`, `tools/wrela query`, `tools/wrela doc` and `tools/wrela primer <area>` for Wrela. Fall back to `rg`; report tool gaps as findings.
- `tools/wrela doc --export <dir> <package-dir>...` writes the reference docs: the spec by section and a page per module, in Markdown, with `index.json` (every item and spec section, with its page, anchor and source) and `llms.txt`. Doc comments and each file's opening comment are that reference; keep them true when you change an item.

| Layer | Location and constraint |
|---|---|
| Hosts | `runtime/native` (Rust: wgpu/wasmtime; also the tool host), `runtime/browser` (TS), `runtime/serve` (Rust: rooms over WebTransport), `runtime/chrome` (Rust: Chrome over the DevTools protocol), `runtime/abi` (shared with the compiler); mechanisms here, policy in Wrela |
| Language | `compiler/` (Rust: syntax → sema → IR → lower → WASM/WGSL; `cli` is `wrela`, also the toolchain manager), `compiler/std` (Wrela); the compiler knows no engine |
| Spec | `spec/` |
| Tests and tools | `compiler/tests`, each crate's tests, `runtime/browser/test`, `tools/` (the check, the release packager, the agent syntax test) |

- Language features must make sense outside games. Std holds only what any Wrela program can use; engine, content and game code belong in `rywible/wrela`.
- Tests use only fixtures in `compiler/`, `runtime/` and `spec/`. A test of a program from `rywible/wrela` uses a frozen copy under `compiler/tests/frozen` (for example `hello-field`), changed only on purpose. No test reads the other repository.

## Quality and checks

- Test rules on small fixtures, bounded seeded samples and explicit resource lifecycle assertions.
- Use real shaders on hardware GPUs for pixel/buffer assertions and CPU hosts for command/state assertions. Fail if a hardware adapter is unavailable; never use software rendering. Rendering tests assert correctness, without frame-time or FPS thresholds. Keep performance comparisons in `tools/wrela profile`.
- `tools/check.sh` runs every retained test and infrastructure check, including real GPU shaders and small Chrome agreement scenarios. `tools/check.sh <check-name>` runs exactly one; a name that ends in `::` runs that group. Unknown, ambiguous or empty selections fail.
- Names: Rust `module::test` or `crate::target::module::test`; Python `tools::test_module.Class.test_method`; browser `browser::file.test.ts::full test name`; Wrela `wrela::<package-path>::<test-name>` (`compiler/std` for std, or a package under `compiler/tests`). Groups: a Rust module path such as `renderer::` (in one test program), `wrela::<package-path>::`, `browser::file.test.ts::`.
- Infrastructure names: `rustfmt`, `wrela-fmt` (the toolchain's own Wrela), `clippy`, `doc`, `tools`, `browser`, `browser-types`, `browser-dist`, `browser-size`, `wgsl`. Use normal formatters.
- Passing checks does not prove prolonged stability, whole-scene quality or reference-machine frame rates. Keep extended captures and benchmarks outside checks.

## Releases

- A release is a tag `vX.Y.Z` on `main`. First bump `version` under `[workspace.package]` in `Cargo.toml` to `X.Y.Z` in a PR; after Ryan merges it, tag that commit on `main` and push the tag.
- The tag starts `.github/workflows/toolchain-release.yml`: it runs `tools/package.py` for Linux x86-64 and macOS arm64 and publishes the tarballs and one `SHA256SUMS` as the tag's GitHub release. It only builds and publishes; add no checks to it. A tarball holds `wrela`, `wrela-host`, `wrela-serve` and Binaryen's `wasm-opt`, with their licences.
- `rywible/wrela` pins a release in its root `wrela.toml` (`[workspace] toolchain = "X.Y.Z"`); `wrela` downloads, checks and runs it (`compiler/cli/src/pin.rs`, language.md §3.6). Moving the pin is a change in that repository.

## Working across both repositories

- In a `rywible/wrela` checkout, `WRELA_TOOLCHAIN=~/projects/wrela-compiler` runs this checkout's toolchain, built with Cargo, in place of the pinned release. `wrela toolchain` says which toolchain runs and why.
- A change that both sides need lands here first, with its own tests, and ships in a release; the `rywible/wrela` change that uses it pins that release. Do not make a check here depend on `rywible/wrela`.

## Records

- `AGENTS.md` is the sole repo instruction file. Add no prose docs beyond the spec (`spec/`) and `docs/process.md`: reference docs come from source through `wrela doc --export`; remove any temporary task notes before committing. Run checks locally.
- Update `docs/process.md` for new outside crates, assets, algorithms or specifications, AI tools/models or kinds of AI work: source, rights, what it teaches and how used. The full record of the project, and of this toolchain before the split on 2026-10-21, stays in `rywible/wrela`'s `docs/process.md`.
- Never create assets with generative image/audio/music/voice/video models.
- Use clear, concrete English, roughly 80% ASD-STE100.
