# Plan: superpowers (release 0.34.0)

One scope of work, on one branch, in one pull request (Ryan asked for all of it together,
knowing it is absurdly large). Fourteen stages, A to N, each ending in commits that leave the
checks green. The stages are ordered so that each one's machinery is in place before a later one
uses it, and so that the safety net (stage A) exists before anything it would catch is changed.

Why: the agent test is at its ceiling (30 of 30 first try, `tools/agent-test/v2/results`), so the
language is learnable; what an agent still lacks is sight (why is this pixel wrong), search
(which numbers matter), proof (is this approximation safe, can this tick trap) and speed it
doesn't have to hand-write. Each stage gives one of those from something the compiler alone
knows: one source for CPU and GPU, derived gradients and intervals, checked determinism, lifted
literals.

**Written against `main` at v0.33.0 (`e86d9fb9`).** Every `path:line` below was read there. If
your checkout says another version, stop and rebase the facts first (§0.3 step 1).

**What 0.21–0.33 already did, and how this plan builds on it** (the ideas were first sketched
against 0.20):

| Idea as sketched | Already in the toolchain | What the stage here adds |
|---|---|---|
| A tape pipeline drawn until the real one is built | Stand-ins, §18.12 (0.26): a cheap entry point runs until its pipeline is built | Stage J: the stand-in that draws the *real* field, through its tape |
| Bake an expensive function into a texture | `wrela pipelines` lists "calls a table could answer", §18.13 (0.31) | Stage H: the table itself, with a proven error |
| Rank approximations by cost | Work and weight, §18.11 (0.25) | Stage I: twins certified within a tolerance, ranked by work |
| An SVD of the Jacobian by literals | `std::lift::jacobian`, `wrela solve --fit` (0.32) | Stage F: the sliders, on that Jacobian |
| Literals that do nothing | `wrela solve` warns of a freed literal it never felt (0.33) | Stages E and F: the same said of the whole surface, without a solve |

**0.33.0 also made one rule explicit that four stages must keep** (§14.2): a derivation can't
follow a float through heap memory. A function it derives may not read or write a `Vec`, a
`String` or a `Box`, and a value that depends on the input put in one is E0700. A function given
nothing that depends on the input isn't derived: it runs as it is, heap and all. And where the
function derived is **named at the call** (`gradient(f, x)`), nothing it reaches may allocate
(§8.3); one handed to the derivation **through a parameter** is held to the heap rule alone. See
§0.2's last rule; stages C, D, E, F and H say where it bites.

## 0. How to work this plan

### 0.1 Read first

`AGENTS.md` (all of it: the layers table and its six rules bind every stage), `docs/release.md`,
`docs/citations.md`, `docs/plans/agent-loop.md` §0 (the house rules this plan repeats), and
`spec/language.md` §11, §12, §13, §14, §16.2, §18 (all), §20, §23. Then each stage's **Facts**
before its steps.

### 0.2 Rules of the repository that apply to every stage

- **Layers** (`AGENTS.md`). The compiler and std know types, effects and math: gradients,
  intervals, bounds, how often code runs, how much work it does. They never know frames, meshes,
  levels of detail, cameras, a game or a scene. Every feature here is phrased in math or timing
  terms, and each stage names its **second user** (rule 2). Where a stage's words drift toward
  the engine's ("creature", "render"), that is the motivating case only; the names in std and in
  the spec stay general.
- **Performance claims carry measurements** (rule 5): before and after, on the same machine, in
  the pull request. §0.5 gives the protocol.
- **One way to do each job** (rule 6). Where a stage's new path replaces an old one, the old one
  is removed in the same stage; where both stay, the stage says why they are different jobs.
- **Run as few tests as possible to validate a change.** Each stage names its checks. Run those,
  not the whole suite, until stage N says to run `tools/check.sh`.
- **Cite your sources.** §0.10 lists the outside work each stage uses; add its row to
  `docs/citations.md` in the commit that lands its code. No stage adds a crate. If you find you
  need one, stop and ask (§0.7).
- **The spec and the tests must agree.** Where a stage changes what a program or a tool can see,
  it names the spec section to edit. A `wrela` block in the spec is compiled by a test
  (`language::every_example_in_language_md_compiles`, `compiler/tests/tests/suite/language.rs:626`),
  so a new example must compile. A new lang item must be in §20's closed list
  (`closed_list::the_closed_list_is_the_compilers`, `suite/closed_list.rs:22`). A new code needs an
  explain file and a test that expects it (`explain::every_code_is_explained`,
  `codes::every_code_is_tested`). A new public std item needs a doc comment and a doc page
  (`std_docs::every_public_std_item_has_a_doc_comment`, the CLI's
  `doc::every_public_std_item_has_a_page`).
- **Nothing documents a command for you.** No test checks that a command is in `usage()`
  (`compiler/cli/src/main.rs:121`), in that file's doc comment, or in §23.9's table. A new command
  goes in all three by hand, in the commit that adds it.
- **Keep what a derivation follows out of heap memory** (§14.2, §8.3; the check is
  `no_gpu_or_heap_work`, `compiler/ir/src/derive/mod.rs:1201`, with `harmless` at `:1173` saying
  what may stay; the tests are `a_value_that_depends_on_the_input_put_in_heap_memory_is_an_error`
  and `heap_memory_that_holds_nothing_of_the_input_is_left_alone`,
  `compiler/tests/tests/suite/derive.rs:402`, `:484`). Three habits, for every std function and
  every glue program this plan adds:
  1. A function a derivation may derive holds what depends on its input in arrays and values,
     and reads no `Vec` itself.
  2. Work that needs a `Vec` (walking an outline, decoding an image, a union-find) goes in a
     function given **nothing** that depends on the input, which hands back a value or an array.
  3. A tool's glue hands its function to `gradient` or `jacobian` **through a parameter**, as
     `wrela solve`'s glue does through `Minimize`, never naming it at the call: the code a
     subject reaches may then allocate where it holds nothing of the input.

### 0.3 How to work

1. The branch is made: `superpowers`, cut from `main` at v0.33.0 (`e86d9fb9`), checked out at
   `.claude/worktrees/superpowers` in the repository. Work there; don't make it again. Check
   `git log -1` says `e86d9fb9` (or, if `main` has moved since, rebase onto it and re-anchor the
   facts) and that `Cargo.toml`'s `[workspace.package] version` (line 22) says `0.33.0`. Commit
   this plan file first, alone (it is untracked). Then follow `docs/release.md` §2: set the
   version to `0.34.0`, `cargo check --workspace`, commit `Cargo.toml` and `Cargo.lock`.
2. Do the stages in order. **One commit per numbered step** (A.1, A.2, ...), titled in the
   repository's style: a sentence saying what is now true, no trailing period. End each with your
   own `Co-Authored-By` line, as your harness gives it.
3. Before each commit: `tools/check.sh clippy` and `tools/check.sh rustfmt`. If you touched
   `runtime/browser`, also `tools/check.sh browser-types` and `tools/check.sh browser`.
4. The checks are run by name: `tools/check.sh <name>` runs exactly one test
   (`tools/check.sh derive::gradients_agree_with_central_differences`), and a name ending in `::`
   runs a group (`tools/check.sh derive::`). The gate runs every test, the ones marked
   `#[ignore = "needs a GPU"]` included (`tools/check.py:103` passes `--include-ignored`): there are
   no tiers. On a machine with no GPU, `WRELA_ALLOW_SOFTWARE=1` admits a software adapter (§18.10).
5. Diagnostic text follows the house style: lowercase, no trailing period, code in backticks,
   the shortest sentence that says what to do. Copy the neighbouring messages' tone.
6. Goldens are re-written with `WRELA_BLESS=1 tools/check.sh <test>`. Before you bless, read the
   `git diff` of the goldens and make sure only what you meant to change changed.
7. Facts are given as `path:line`; lines drift. Grep for the identifier if a line is off.
8. Write every tool so an agent can read it: `--json` on every new command, stable keys, numbers
   as numbers, a file as `{"file", "line", "col"}`.

### 0.4 Two kinds of step

- A **build step** is done when its checks pass.
- A **hypothesis step** makes a claim that only a measurement settles (a speedup, a tightness, a
  share of cases covered). It is marked *hypothesis* and names a gate. If its gate fails, the
  code **ships switched off** (the std item left out of std's public surface, the pass behind a
  flag that defaults off, the command marked experimental in its usage line), the measurement
  goes in its commit message and in the pull request, and the plan goes on. A failed hypothesis
  is a result, not a defect. Never tune the benchmark until the gate passes.

Estimates in this plan are labelled *estimated*. Replace each with the measured number, the
machine and the command once a gate has run.

### 0.5 The gates

- **G-suite:** the stage's named checks pass, then `tools/check.sh clippy` and `rustfmt`.
- **G-oracle:** stage A's typed programs pass the oracle the step names, over the gate's seeds and
  over one soak of the size the step gives, with the seed printed in the commit.
- **G-agree:** two ways of computing one thing agree within the step's tolerance over the step's
  inputs; the worst difference is pasted in the commit.
- **G-sound:** a proven bound is never beaten: over the step's samples, no measured value lies
  outside what was proven. One miss fails the gate, and is a bug to fix before going on, never a
  tolerance to widen.
- **G-measure:** `wrela profile <pkg> --compare <saved build dir> --pairs 7` (AB/BA alternating
  pairs, `compiler/cli/src/profile/comparison.rs`) on an idle machine, natively and, where the
  step says, with `--chrome`. A change **pays** when the report's status is `faster` (the two
  ranges don't overlap) and the median change is at least the step's threshold. Paste the
  report's `metric` block. Say the machine.
- **G-cold:** the browser's test mode, cold (a fresh profile), three runs: `load.json`'s
  `first_submitted_ms` and `starts.json`; the **median** is the number. Paste all three.

### 0.6 Never

- Work around a gap in another language (a Python script where a `wrela` tool or a Rust test
  belongs), except in `tools/agent-test`, which is Python already.
- Change what an existing code means. Codes are stable forever (§10.2).
- Skip a failing test or mark it ignored. If a step can't be finished, stop, write what blocks it
  in the pull request description, ship what the step's "if it can't be done" says, and go on.
- Let a proof depend on a sample. Words like `proven`, `certified` and `holds` in a tool's
  output mean an interval or a rule proved it; anything else says `sampled`.
- Put an engine idea in the compiler or std (§0.2). If a step seems to need one, the step is
  wrong: stop and say so in the pull request.
- Edit `tools/agent-test/results/` or `tools/agent-test/v2/results/`.

### 0.7 Stop and ask Ryan before

- Spending money: the agent test (stage M's runs, stage N's release run).
- Adding a crate, or a new attribute beyond the ones §0.8 lists.
- Merging. Everything else is yours.

### 0.8 Decisions reserved for Ryan

The agent builds each default; Ryan can flip it in review. Each is **proposed**.

| ID | Decision | Default the plan builds |
|---|---|---|
| S1 | How a kernel says its results are the same bits everywhere | The existing `@deterministic` on a `@compute` kernel (stage K). No new attribute |
| S2 | Whether `f16` twins can be *proven* | No: 0.7.0's council refused deriving intervals in `f16`. `wrela twin --half` writes them as **advised**, never proven (stage I) |
| S3 | A struct-of-arrays container | An explicit std type, `Columns<T>`, plus a report that says where one would pay; the compiler never turns a `Vec` into columns silently (stage L) |
| S4 | The fuzzer's coverage instrumentation | Only in builds `wrela fuzz` makes; no flag on `wrela build` (stage G) |
| S5 | Whether `wrela pixel` may run without a GPU | No: it needs the GPU's own pixel to compare with; under `WRELA_ALLOW_SOFTWARE=1` it runs on the software adapter (stage B) |
| S6 | Reverse mode's place in `wrela solve` | Used above a measured count of free literals; the 16-at-a-time forward path stays below it only if it measures faster there (stage C) |
| S7 | Hints for exact kernels in release builds | Checked one time in 64 against the CPU's result; a miss turns hints off for the run and logs it (stage K) |
| S8 | The agent test's version 3 | Built and its references tested; run only with Ryan's go-ahead (stage M) |

### 0.9 The stages, what each needs, and its kind

| Stage | What it gives | Needs | Kind |
|---|---|---|---|
| A | Random well-typed programs checked by six oracles | nothing | build |
| B | A GPU code interpreter, and `wrela pixel` | A (its oracle) | build |
| C | Reverse-mode gradients (`std::derive::reverse`, `std::lift::reverse`) | A | build, with one hypothesis (S6) |
| D | Outlines as measures: `coverage`, `outline_miss`, `image_miss` | nothing (C helps) | build |
| E | `wrela inspect`: pieces, necks, dead parts, dead literals, previews | D (previews share its rays) | build |
| F | `wrela sliders` | E (its surface samples) | build |
| G | `wrela fuzz` for ticks and rooms | nothing | build |
| H | Proven tables (`std::table`), baked fields | A | hypothesis |
| I | Certified twins (`wrela twin`) | B (the interpreter's walk) | hypothesis |
| J | Tape stand-ins | nothing | hypothesis |
| K | Exact kernels | B, G | hypothesis |
| L | CPU loops: range proofs, `Vec` loops four at a time, branches as masks, `Columns<T>` | A | hypothesis, step by step |
| M | The agent test, version 3 | B to G | build; runs need Ryan |
| N | Spec, docs, the release | all | build |

If you run short, the order above is also the order of value per risk: A to G are tools an agent
uses the day they land; H to L are measured bets.

### 0.10 What this plan adds, by name

Keep this table true as you go; stage N checks it against the code.

| Kind | Names |
|---|---|
| Commands | `wrela pixel`, `wrela inspect`, `wrela sliders`, `wrela fuzz`, `wrela twin`; `wrela build --report layout` |
| std | `std::derive::{reverse, Trace}`; `std::lift::{reverse, reverse_sum}`; `std::field::{View, coverage, outline_miss, image_miss, pieces, Pieces, preview, baked, BakedField}`; `std::table::{Table1, Table2, Table3, Tabled1, Tabled2, Tabled3, table1, table2, table3, Baked}`; `std::stage::interpret_at`; `std::gpu::exact::{Exact, offer}` and a deterministic kernel's `run`; `std::collections::Columns` |
| Codes | E0713 (a `@deterministic` kernel that uses what isn't the same on every GPU), E0339 (a `Columns` element taken as one place) |
| Driver | `wrela_driver::gpu_modules` (each pipeline's emitted IR), a `no_opt` test build |
| IR | `compiler/ir/src/interp/` (the interpreter and the error walk), `compiler/ir/src/ranges.rs` |
| Tests | `compiler/tests/src/typed.rs`, `compiler/tests/tests/suite/typed.rs`, `compiler/tests/typed/regress/` |

**Outside work to cite** (`docs/citations.md`, "Specifications and published methods", each as a
method, no code copied). I wrote these from memory: **check each one's authors, title, venue
and year against the source itself before you write its row**, and correct this list if one is
off.

- A: MacIver and Donaldson, "Test-case reduction via test-case generation: insights from the
  Hypothesis reducer" (ECOOP 2020): the shrinker's choice sequence. Yang, Chen, Eide and Regehr,
  "Finding and understanding bugs in C compilers" (PLDI 2011): typed random programs and
  differential oracles.
- B: Olano and Greer, "Triangle scan conversion using 2D homogeneous coordinates" (1997):
  coverage without clipping.
- C: Griewank and Walther, *Evaluating Derivatives* (2nd ed., 2008): reverse mode with a tape of
  local partials.
- F: Golub and Van Loan, *Matrix Computations* (4th ed., 2013): orthogonal iteration for the
  leading singular vectors.
- G: Zeller and Hildebrandt, "Simplifying and isolating failure-inducing input" (IEEE TSE,
  2002): minimizing a failing tick log. Zalewski's American Fuzzy Lop technical notes: a
  coverage map as a fuzzer's feedback.
- H: the error of a multilinear interpolant (a numerical analysis text, for example Süli and
  Mayers, *An Introduction to Numerical Analysis*, 2003): the table's proof.
- L: Pharr and Mark, "ispc: a SPMD compiler for high-performance CPU programming" (InPar 2012):
  branches as masks. Bodík, Gupta and Sarkar, "ABCD: eliminating array bounds checks on demand"
  (PLDI 2000): range proofs.

### 0.11 What this plan leaves out, on purpose

- **Affine arithmetic and boxes over more than four inputs.** Still research (agent-loop's list).
- **Sparse or adaptive tables.** Stage H's tables are uniform grids; where a uniform grid can't
  meet a tolerance, the answer is the analytic function there, not a brick map.
- **A general `schedule`, kernel fusion, a render graph.** As before (§21.2).
- **Deterministic floats on the GPU.** Stage K's kernels are integer-only. Soft floats in integer
  WGSL are a later experiment.
- **Running GPU tests on the interpreter.** Stage B's interpreter serves `wrela pixel` and stage
  I's walk. Making it a third host is a separate decision.
- **Trained approximators.** No stage fits a network to a field.
- **A daemon, incremental checking, constrained decoding.** As before.

## Stage A: the compiler tests itself

**Goal.** A generator of random **well-typed** numeric programs, and six oracles that check the
compiler against itself on each one. Every later stage changes a derivation, a back end or the
optimizer; this is what catches it.

**Second user.** The compiler's own development, on every change, by any agent.

**Facts.**

- The grammar's generator makes programs that parse, not programs that type-check
  (`compiler/grammar/src/generate.rs`). The mutation fuzzer feeds mutated files to the compiler
  and requires only that it doesn't crash (`mutated_programs_dont_crash_the_compiler`,
  `compiler/tests/tests/suite/fuzz.rs:390`). Nothing generates programs that build.
- Random numbers: `wrela_grammar::rng::Rng` (`compiler/grammar/src/rng.rs:12`: `new`, `next_u64`,
  `below`, `unit`, `chance`, `pick`), `mix` (`:50`), `par_seeded` (`:67`). `wrela_tests`
  re-exports them (`compiler/tests/src/lib.rs`).
- The cheapest way for a Rust test to compile a source string and call an export on the CPU:
  `crate::package(name, src)` (`suite/main.rs:122`, adds an empty `frame`),
  `wrela_tests::must_build(&dir, &dir.join("build"))` (`compiler/tests/src/lib.rs:84`),
  `CpuBuild::load(dir.join("build"))?.start()?`, then `host.call_export("f", &[Value::F32(x)])`
  or `wrela_tests::one_f32(&mut host, "f", &args)` (`:206`).
- The cheapest way to evaluate a function at many points on the GPU is the pattern of
  `compiler/tests/fields/main.wrela`: a `@compute(64)` kernel that writes a `Slots<f32>` kept in
  the program's state, an export that dispatches it, and in Rust `Host::load(dir)`,
  `host.call_export(..)`, `let out = *host.buffers().last()`, `u32s(&host.read_buffer(out)?)`
  (as `intervals_enclose_every_sample_on_the_gpu`, `suite/derive.rs:201`, does).
- The fixed corpus already has four of the oracles, over 25 hand-written fields:
  `gradients_agree_with_central_differences` (`derive.rs:77`),
  `intervals_enclose_every_sample_on_the_cpu` (`:151`) and `..._on_the_gpu` (`:201`),
  `tapes_match_their_compiled_fields_on_the_cpu` (`suite/stage.rs:36`). Read them: the tolerances
  below are theirs.
- The CPU optimizer is one call, `ir::opt::inline_cpu` (`compiler/ir/src/opt.rs:1526`), made at
  `compiler/lower/src/lib.rs:515`. The flags a lowering takes are `BuildData`'s
  (`compiler/lower/src/eval.rs:48`: `debug`, `testing`, `no_simd`, `relaxed_simd`, `lift`, `role`,
  `few`). GPU modules must be flattened (WGSL can't pass pointers), so they have no unoptimized
  form.
- Strict floats: CPU results are bit-exact across optimization (§11.1). An interval "bounds every
  value a point in the box can give, as the target computes it", widened by each target's
  rounding (§14.2), so over a box that is one point it holds the exact real result and that
  target's computed one.

**Design.**

*The generator* is `compiler/tests/src/typed.rs`, in the `wrela_tests` library, so the suite and
a soak binary both use it. It builds programs type-first: `expr(ty, depth)` picks only
productions that give `ty`, so a program is well-typed by construction.

- Types: `f32`, `vec2`, `vec3`, `bool`, and `u32` for loop counters and table indices.
- A program is zero to three helper functions (`fn h0(a: f32, b: vec3) -> f32`), a constant table
  `const T: [f32; 8]`, and `fn f(p: vec3) -> f32`, the function under test. The harness's exports
  are appended as fixed text (below).
- Productions, by result type. `f32`: a literal (from `0.0 1.0 -1.0 0.5 2.0 0.25 3.0 0.1`, or one
  in [-4, 4] with two decimals); an `f32` in scope; a component of a vector in scope; `a + b`,
  `a - b`, `a * b`, `-a`; `a / d`; `sin cos tanh atan abs floor fract` of one; `sqrt`, `exp`,
  `log`; `min`, `max`, `atan2`, `step`, `pow`; `clamp`, `mix`, `smoothstep`; `length`, `dot`,
  `distance`; `if c { a } else { b }`; `select(if_false: a, if_true: b, cond: c)`; a helper's
  call; `T[u32(clamp(e, 0.0, 7.0))]`. `vec3`: `vec3(a, b, c)`; one in scope; `u + v`, `u - v`,
  `u * s`, `u * v`, `-u`; `abs`, `min`, `max`, `mix`; `cross`; `normalize`; a swizzle (`u.yzx`).
  `bool`: `a < b`, `a > b`, `a <= b`, `c && d`, `c || d`, `!c`.
- Statements: `let x = e`; `var acc = e` with `for i in 0..N { acc = acc + g }` (`N` a literal
  from 1 to 6, `g` may read `f32(i)`), optionally with `if c { break }` inside; `if c { acc = e }`.
- Two modes. **Tame**: every operation is total and finite on the domain, by guards the
  generator writes: a divisor is `(abs(e) + 0.5)`; `sqrt(abs(e))`; `log(abs(e) + 0.5)`;
  `exp(clamp(e, -4.0, 4.0))`; `pow(abs(a) + 0.1, clamp(b, -2.0, 2.0))`; `clamp(a, min(l, h),
  max(l, h))`; `smoothstep(e0, e0 + abs(d) + 0.1, x)`; `normalize(u + vec3(0.0, 0.0, 2.5))` only
  where `u`'s components are each `tanh` of something (so the sum is never zero). **Wild**: no
  guards. Wild programs are only built and run (oracle O1); their values may be NaN.
- No production allocates, or reads a `Vec`, a `String` or a `Box`: three of the oracles derive
  `f`, and a derived function can't touch heap memory (§14.2). That is why the table is a
  constant array.
- Size: at most 60 nodes and depth 6 unless the caller says otherwise.
- `struct Features { loops, breaks, tables, helpers, pow, select, ... }`, all on by default: a
  switch for each production family, so one can be turned off (below).

*Every random choice goes through one buffer*, so a failing program can be shrunk without a
shrinker that knows the grammar (MacIver and Donaldson): `struct Choices { data: Vec<u32>, at:
usize, rng: Option<Rng> }` with `fn draw(&mut self, n: u32) -> u32`. Generating: draw from the
`Rng` and record. Replaying: return `data[at] % n`, or 0 once the buffer runs out. **Choice 0 is
always the simplest production** (a literal; the shorter alternative; no loop), so a buffer of
zeros is the smallest program. To shrink a failure: (1) cut the buffer's tail, halving; (2) zero
blocks of 8, 4, 2, 1 entries; (3) lower single entries; keep a change when the oracle still
fails; stop when a full pass changes nothing or after 2,000 tries.

*The harness* appended to each program (fixed text; write it once in `typed.rs`):

```text
pub fn value(x: f32, y: f32, z: f32) -> f32          // f(vec3(x, y, z))
pub fn grad(x: f32, y: f32, z: f32) -> vec3          // std::derive::gradient(|p| f(p), ...)
pub fn range(lx, ly, lz, hx, hy, hz: f32) -> vec2    // std::derive::interval over the Box3
pub fn taped(x: f32, y: f32, z: f32) -> f32          // interpret(tape(|p| f(p)), ...)
// and, for the GPU: a state holding a points buffer and two outputs, with
// `@compute(64)` kernels that write f(p) and f's interval over the box that is p,
// and an export that uploads `count` points from a seed and dispatches both
```

*The oracles* (`compiler/tests/tests/suite/typed.rs`, module `typed`; add `mod typed;` to
`suite/main.rs`). One build per program serves O1 to O4; O5 adds one more build; O6 loads the
build on the GPU.

| | Oracle | What it requires |
|---|---|---|
| O1 | builds | The program checks and builds (WASM, and WGSL through naga's validation), with no error and no I0001; it runs at 16 points without a trap. Both modes |
| O2 | the interval encloses | For 8 boxes (centre in [-2, 2]³, half sizes log-uniform from 1e-4 to 2, as `random_box` in `fields/main.wrela:276`) and 16 points in each: `lo <= value <= hi` |
| O3 | the gradient agrees | At 16 points: central differences `D(h)` and `D(h/2)`, `h` = 1e-3. A point **counts** when `abs(D(h) - D(h/2)) <= 1e-3 * (1 + abs(D(h)))` in each component (the function is smooth there). At a counting point, `abs(grad - D(h/2)) <= 2e-3 * (1 + abs(D))`. Over a run, at least 30% of points count, or the generator is degenerate |
| O4 | the tape agrees | `abs(taped - value) <= 3e-6 * max(1, abs(value))` at 16 points (§14.4's bound). E0709 is a failure, unless the program uses a feature on a short allowlist that starts empty |
| O5 | the optimizer keeps the bits | The same 16 values from a `no_opt` build, compared by `f32::to_bits` |
| O6 | the targets agree | At 64 points: the GPU's value lies in the GPU's interval over the point; and `abs(gpu - cpu) <= (cpu_hi - cpu_lo) + (gpu_hi - gpu_lo)`, the two point intervals' widths. `#[ignore = "needs a GPU"]` |

**Steps.**

A.1 The generator, the choice buffer and the shrinker, with unit tests in `typed.rs`: the same
seed gives the same text; a buffer of zeros gives the smallest program; 200 seeds in each mode
all check (`wrela_driver::check`). Print one program in the commit message.

A.2 `no_opt`. Add `no_opt: bool` to `BuildData` and guard the `inline_cpu` call
(`lower/src/lib.rs:515`) with it. Give tests a way to ask:
`wrela_driver::build_for_tests_with(root, debug, no_opt)` beside `build_for_tests`
(`compiler/driver/src/lib.rs:273`). Nothing else may set it. If a module that skipped
`inline_cpu` fails `ir::verify` or WASM emission for a reason that is not a bug (some later
stage needs what inlining did), record what, and instead make `no_opt` skip only
`inline_by_size` and `tidy` inside `inline_cpu`, keeping `fold_constant_branches` and
`remove_unreached`.

A.3 The oracles O1 to O5 as tests, each over 32 seeds from a fixed base seed, and O6 over 16.
On a failure the test shrinks the program, writes it to `target/typed/<oracle>-<seed>.wrela`
and panics with the seed, the path and the first failing point.

A.4 The soak. A binary, `cargo run --release -p wrela-tests --bin typed-soak -- <oracle> <count>
[--seed N] [--threads N]` (use `par_seeded`), not in the gate. Read `WRELA_TYPED_SEEDS` in the
tests as an override of the 32, and add it to `CONTROLS` in `tools/check.py:50` so the gate
clears it.

A.5 Run the soak once: 5,000 seeds for each of O1 to O5, 1,000 for O6. For each failure:

1. Shrink it. Copy the shrunk program to `compiler/tests/typed/regress/<oracle>-<seed>.wrela`
   with a first line `// oracle: O3`.
2. Decide what it is. A compiler bug: fix it in this step if the fix is local, with the
   regression file as its test. A generator bug (a guard that doesn't hold): fix the generator.
   A tolerance the fixed corpus's tests already allow (a kink the difference test straddles): fix
   the oracle's skip rule, never the tolerance.
3. A compiler bug you can't fix here: turn its production family off in `Features` with a
   comment naming the bug, keep the regression file listed in a `KNOWN` array the regression
   test skips, and put it in the pull request under "Found by the typed fuzzer, not fixed".

A.6 `typed::every_regression_still_holds`: each file in `regress/` (but `KNOWN`) through the
oracle its first line names.

**Checks.** `tools/check.sh typed::`, `cargo test -p wrela-tests --lib typed`.

**Done when:** the six oracles pass over the gate's seeds; the soak's counts, seed and each
failure's fate are in the commit; `regress/` holds a file for each bug found.
**Gate:** G-oracle (all six).

## Stage B: `wrela pixel`

**Goal.** An agent asks why a pixel has the colour it has, and gets: every draw that touched
it, the draw that decided it, that fragment's inputs, and each source line's values as the
shader ran for that one pixel, with the first value that wasn't finite named.

```text
wrela pixel <package> --frame <n> --at <x>,<y> [--target <label>] [--draw <k>] [--fragment <j>]
            [--input <script.json>] [--size <w>x<h>] [--line <file>:<line>] [--full] [--json]
```

**Second user.** Any GPU program: the same interpreter answers for a kernel's invocation
(`--kernel <label> --id <i>`, step B.9), and stage I's proofs walk the same code.

**Facts.**

- The code a pipeline runs is an `ir::Module`: `Lowered::pipelines` (`compiler/lower/src/lib.rs:50`)
  holds a `PipelineOut` for each (`compiler/lower/src/gpu.rs:77`) with `module`, the flattened
  module (`verify`, `gpu.rs:3518`, calls `ir::opt::flatten_gpu` then
  `ir::gpu_memory::lay_out_gpu_memory`). The driver writes WGSL from exactly that module
  (`wrela_wgsl::emit(&p.module)`, `compiler/driver/src/build.rs:251`). So interpreting
  `PipelineOut::module` interprets what the GPU was given.
- The IR: structured control flow, single-assignment values, mutable locals
  (`compiler/ir/src/lib.rs`: `BinOp` `:102`, `Builtin` `:141`, `PlaceRoot` `:219`, `Place` `:242`,
  `TextureOp` `:525`, `Expr` `:617`, `Stmt` `:682`, `Function` `:739`, `ResourceKind` `:850`,
  `BuiltinInput` `:890`, `EntryPoint` `:950`, `Module` `:984`). A `Loop` runs `body` then
  `continuing`; `Continue` jumps to `continuing`. A resource is read through
  `PlaceRoot::Resource`; an entry point's builtins and varyings are `Expr::EntryInput(i)` and a
  `varyings` parameter (`entry_function`, `gpu.rs:3269`; `entry_inputs`, `:3250`).
- Source positions survive: lowering marks where code comes from with `Stmt::At(span)`
  (`compiler/lower/src/body.rs:395`), one for each statement of the memory IR, and the GPU
  inliner keeps them (`opt.rs`, the `At` arms near `:1303`). Locals keep their names
  (`LocalDecl { name, ty }`, `ir/src/lib.rs:729`). `wrela precision` already reports operations
  by these positions.
- The compiler can already evaluate GPU operations on constants as WGSL defines them:
  `compiler/ir/src/const_fold.rs` has private `unary` (`:559`), `binary` (`:570`), `convert`
  (`:650`), `select` (`:673`) and `builtin` (`:710`) over `Const` and `K` (`:22`), "a float
  computed as WGSL computes it, in `f32`, a transcendental through `f64`", and it returns `None`
  where WGSL leaves a result undefined.
- The native host executes a decoded command stream: `impl Executor for Gpu`
  (`runtime/native/src/gpu.rs:2396`, `execute`), with a pass's items collected (`PassItem`, `:436`;
  `pass_item`, `:1855`) and recorded when the pass ends (`record_ended`, `:1964`). Commands:
  `wrela_abi::stream::Command` (`runtime/abi/src/stream.rs:895`), `decode` (`:1339`). A draw
  carries `pipeline`, `vertices`, `instances`, `bindings: Vec<Binding>` and its uniform bytes
  inline; a `Binding` is `{ handle, offset, size }` (`:764`).
- What the host can read back today: `read_screen`, `read_screen_region`, `read_target(label)`
  (the last pass with that label, as RGBA8), `read_buffer(handle)`
  (`runtime/native/src/lib.rs:1013`–`:1064`). **It cannot read a texture by its handle**, nor a
  target between two draws of a pass.
- The build's manifest names each pipeline's entry points, its uniform block's size and its
  bindings (`runtime/abi/src/manifest.rs:184`), and for a render pipeline its blends, cull, depth
  test and targets. The uniform block's field layout is `ir::layout`'s
  (`compiler/ir/src/layout.rs:48`), from the module's own types.
- A program run from `init` with the same input script gives the same commands every time
  (§11.1's command-stream hash), so a frame can be run twice and be the same frame. `wrela trace`
  shows how to build a package, load it and run frames (`compiler/cli/src/trace.rs:127`, `start`).
- Derivatives (`dpdx`, `dpdy`, `fwidth`) and sampling with an implicit level are only in
  uniform control flow (E0608, §11.4), so the four pixels of a 2 × 2 block make the same
  sequence of such calls.

**Design.**

*The interpreter* is new: `compiler/ir/src/interp/mod.rs` (the walk and the values) and
`interp/concrete.rs` (running it on real inputs). It runs one invocation of one entry point of
a flattened GPU module.

- Values: `enum V { S(Const), P(Vec<V>) }`, the shape `const_fold`'s `K` has. Make
  `const_fold`'s `unary`, `binary`, `convert`, `select` and `builtin` `pub(crate)` and call them:
  do not write the arithmetic a second time (rule 6). Where one returns `None` (WGSL leaves the
  result undefined), the interpreter computes IEEE's result, and marks the value **undefined on
  the GPU** in the trace: that mark is often the answer an agent is looking for.
- Integers wrap, as on the GPU (§11.3). An index out of range reads zero and is marked.
- Resources come from a `trait Inputs`: `uniform() -> &[u8]`; `buffer(binding) -> &[u8]`;
  `texture(binding) -> &Texels` (its size, format, levels and raw texels); `sampler(binding)`;
  `builtin(BuiltinInput) -> V`; `varyings() -> V`. Reads decode bytes by `ir::layout` at the
  place's offset. A write to a storage buffer or texture is kept in the interpreter and listed
  in the result.
- Textures: `load` (a texel read) is exact. A sample is computed in the interpreter: nearest and
  linear, with mip levels and repeat or clamp, for 2D, 2D array, 3D and cube textures, and
  comparison samplers. Anisotropic filtering is computed as trilinear and marked
  **approximate**.
- The trace: `trait Observer { fn at(&mut self, span); fn value(&mut self, v: ValueId, ty,
  val: &V); fn store(&mut self, local: Option<&str>, place, val: &V); fn branch(&mut self,
  taken: bool); fn pass(&mut self, of_loop, n: u32) }`. A no-op observer costs nothing.
- Not run in this stage: atomics, workgroup memory, subgroup operations, barriers. The
  interpreter returns `Err(Unsupported("workgroup memory"))` and the command says so.

*The block of four.* A fragment's derivatives need its neighbours. The interpreter runs the
2 × 2 block the pixel is in (the block's corner is at even coordinates), each pixel a separate
run, in passes. In pass 0 every derivative call returns 0 and records its operand, by its place
in the sequence of derivative calls. In pass `k` the `j`th call returns the difference of pass
`k - 1`'s `j`th operands across the block (`dpdx`: this row's right minus left; `dpdy`: this
column's lower minus upper; `fwidth`: the sum of their sizes), and records its operand again.
Stop when a pass's operands equal the pass before's, bit for bit, or after 4 passes (then say
"derivatives nested more than three deep"). An implicit-level sample takes its level from the
same differences of its coordinates. A pixel of the block outside the triangle still runs, on
the triangle's plane, as a GPU's helper invocation does.

*The host's side* is a **probe**, in the native host only.

- `Options::probe: Option<Probe>` (`runtime/native/src/lib.rs:105`), `Probe { frame: u32, target:
  ProbeTarget, x: u32, y: u32, capture: Option<u32> }`, `ProbeTarget::{Screen, Label(String)}`.
- In frame `frame`, when a pass that draws into the probed target is recorded, the host records
  **each draw as its own pass** (the pass ended and begun again, loading, not clearing), and
  after each copies the one texel of every colour attachment and of the depth attachment to a
  buffer it reads back. Splitting a pass this way draws the same picture.
- With `capture: Some(k)`, just before the frame's `k`th draw into the target runs, the host
  submits what it has recorded and reads back everything that draw binds: each buffer, each
  texture (every level and layer, raw, in its own format), each sampler's settings. This needs a
  new `Gpu::read_texture(handle, level, layer) -> Result<Vec<u8>>` beside `read_buffer`
  (`gpu.rs:2216`); a depth texture is read through its depth aspect.
- `Host::take_probe() -> ProbeResult`: for each draw into the target, in order: its place in the
  frame, its pipeline, the label in force, its counts, its uniform bytes and bindings, the pass's
  viewport and scissor, and the texel before and after. With `capture`, that draw's resources.

*The draw's geometry.* To find the fragment, the tool runs the vertex entry point in the
interpreter for the draw's vertices and instances (for an indexed draw, through the index
buffer it captured), and tests each triangle against the pixel's centre in homogeneous
coordinates (Olano and Greer: three edge functions of the clip-space positions, so a triangle
that crosses the near plane needs no clipping), with the top-left rule, the pipeline's cull mode
and the viewport and scissor. Each covering triangle gives a fragment: its depth and its
varyings, interpolated perspective-correct. The fragments are replayed in order against the
texel's depth before the draw, with the pipeline's depth test and write, to mark each `passed`
or `failed`. The last one that passed is the one traced, unless `--fragment` names another.

*The command* (`compiler/cli/src/pixel.rs`) runs the program twice, both times from `init` with
the same `--input`: once with a probe for the history, once more with `capture` set to the
chosen draw (the last whose `after` differs from its `before`, or `--draw k`). It then runs the
block of four and prints.

```json
{"frame": 37, "at": [412, 300], "target": "screen",
 "history": [{"draw": 3, "label": "far", "pipeline": "cover+far", "before": [0, 0, 0, 1],
              "after": [0.21, 0.4, 0.62, 1], "changed": true}],
 "traced": {"draw": 3, "fragment": 0, "vertex": "cover", "shader": "far",
            "varyings": {...}, "depth": 0.5, "passed": true},
 "result": {"interpreted": [0.2101, 0.4003, 0.6198, 1], "gpu": [0.21, 0.4, 0.62, 1],
            "differs_by": 0.0003},
 "first_not_finite": null,
 "undefined_on_gpu": [], "approximate": [],
 "lines": [{"file": "sky.wrela", "line": 88, "text": "let d = field.distance(p)",
            "ran": 64, "first": 2.31, "last": 0.0004, "least": 0.0004, "most": 2.31}],
 "branches": [{"file": "sky.wrela", "line": 90, "taken": 1, "not_taken": 63}]}
```

`lines` is grouped by source line: how many times it ran, and its value the first and last time
and at its extremes (a vector's value is its components). `--line` gives every run of one line;
`--full` gives every value in order, one JSON object a line. Without `--json`, the same as text,
the source line beside its values.

**Steps.**

B.1 The interpreter over pure code: `interp/mod.rs` and `concrete.rs`, with `const_fold`'s
evaluators shared. Unit tests in the `wrela-ir` crate: straight-line arithmetic, a loop with
`break` and `continue`, a `select`, locals, a struct and an array through a place, integer wrap,
an index out of range.

B.2 `wrela_driver::gpu_modules(dir, options) -> Result<Vec<GpuModule>, Output>`: the package
built as `wrela build` builds it, returning for each pipeline its name, its entry points' names,
its `ir::Module` and its manifest entry. It is a `pub` function beside `check_pipelines`
(`compiler/driver/src/lib.rs:209`).

B.3 The interpreter against the GPU, as stage A's seventh oracle, **O7**: for each typed program,
the kernel that writes `f(p)` is interpreted at 64 points (uniforms and the points buffer as
`Inputs`), and its results equal the GPU's within the same bound as O6. Add it to
`suite/typed.rs` and run 1,000 seeds of soak. This is what makes the interpreter trustworthy;
do not go on while it fails.

B.4 Resources and textures in the interpreter: uniform blocks and buffers by layout, texel
loads, samples. Test against the GPU with `compiler/tests/sampled` and `compiler/tests/texels`
(they are in `tools/wgsl_budgets.py`'s corpus, so they build): interpret their kernels or
fragments on captured inputs and compare with what the GPU wrote, within 1/255 for a sample and
exactly for a load.

B.5 The probe in the native host: `Options::probe`, the pass split, `Gpu::read_texture`,
`Host::take_probe`. Tests in `compiler/tests/tests/suite/render.rs`'s style: a program that
draws two triangles over each other into the screen; the history of a pixel under both shows two
draws with the right before and after; a frame run with a probe and without has the same
`read_screen` bytes.

B.6 The geometry: the vertex stage interpreted, coverage, interpolation, the depth replay. Test
with the same program: the fragment found for a pixel inside one triangle has varyings equal to
the ones a fragment shader writing its varyings as its colour produced (within 1/255).

B.7 The block of four and derivatives: a fragment shader that returns `fwidth` of a varying; the
interpreted pixel equals the GPU's within 2/255.

B.8 The command, its JSON and text, its row in §23.9, its usage line and doc comment. CLI tests
in `compiler/cli/tests/suite/pixel.rs` (helper `wrela()`, `common.rs`): the JSON's shape on the
two-triangle program; a shader that divides by a varying that is zero at one pixel reports
`first_not_finite` at the right line; a pixel no draw touched says so and exits 0 with an empty
`history`.

B.9 Kernels: `wrela pixel <package> --frame n --kernel <label> --id <i>` interprets one
invocation of the last dispatch under that label, with the dispatch's buffers captured before
it. A kernel that uses what B.1 leaves out says which.

**If it can't be done.** If the pass split changes what a program draws on some adapter (B.5's
second test fails), ship the history from whole passes (one entry a pass, not a draw) and say
so in the output (`"history_by": "pass"`).

**Checks.** `cargo test -p wrela-ir interp`, `tools/check.sh typed::`, `tools/check.sh render::`,
`cargo test --release -p wrela --test suite -- pixel::`.

**Done when:** O7 holds over its soak; on `compiler/tests/fields`'s `shade` the interpreted
pixel is within 2/255 of the GPU's at 100 pixels (paste the worst); the NaN test names the
line. **Gate:** G-oracle (O7), G-agree (the 100 pixels).

## Stage C: reverse-mode gradients

**Goal.** The gradient of a function of many numbers at the cost of a few evaluations, whatever
the count: `std::derive::reverse(f, at)` for an array of numbers, and
`std::lift::reverse(f, which)` for any number of lifted literals.

**Second user.** A fit over thousands of literals (`wrela solve`), and anything that learns
weights held in an array (the engine's small nets today compute their own derivatives).

**Why it isn't there.** `gradient` is forward mode: one tangent for each input component, so
its input is at most a `vec4`. `std::lift::gradient` takes at most 255 literals a call
(`compiler/ir/src/derive/literal.rs:34`), and `Minimize` asks for them 16 at a time
(`gradient_at`, `compiler/std/lift.wrela:584`): `N` literals cost about `N / 16` derived
evaluations, each carrying 16 tangents.

**Facts.**

- Forward mode is `compiler/ir/src/derive/ad.rs`. `derive(m, cache, f, mask, n, target, filter)`
  (`:216`) makes a function with `n` tangents for each active value: tangent parameters appended
  (`{p}_d{k}`), tangent locals, and a result `dual{n}<T> { v, d0.. }`. **A tangent has its
  value's type.** The cache key is `(f, mask, n | FILTERED)` (`FILTERED = 0x80`, `:141`).
- `struct Ad` (`:313`) holds the mode (`filter: bool`), `tv: HashMap<ValueId,
  Vec<Option<ValueId>>>` (each active value's tangents; `None` is structurally zero), and
  per-statement caches (`made`, `recips`, `scaled`), cleared for each statement (`stmt`, `:518`).
- `stmt_inner` (`:527`) is the plumbing. For `Let(v, e)` with `v` active it calls
  `self.tangent(*v, e, k, out)` for each direction `k` and stores the results in `tv`. Stores,
  returns and calls move tangents as they move values (`call`, `:575`, derives the callee with
  the same `n` and passes each tangent after the arguments).
- `tangent` (`:839`) and `builtin_tangent` (`:1000`) are **all the mathematics**: every rule,
  with its special cases (a length of zero, `min` and `max` at a tie, `a * a`). An arm reads its
  operands' tangents through `self.t(x, k)` (`:375`).
- **The heap rule** (0.33.0, §14.2). `derive` first refuses a body that does what a derived
  function can't (`no_gpu_or_heap_work`, called at `ad.rs:231`; defined at
  `compiler/ir/src/derive/mod.rs:1201`): GPU work, work with other threads, and any raw memory
  operation that isn't `harmless` (`:1173`), which is how a `Vec`'s, a `String`'s or a `Box`'s
  element shows in the IR. The activity analysis also finds a value that depends on the input
  stored through a view of heap memory, here or in a function called with one (`heap_store`,
  `:316`; `heap_views`, `:583`). Both are E0700 with words of their own (`Error::heap_memory`,
  `compiler/ir/src/error.rs:33`). A function given nothing active is copied as it is, heap and
  all. §8.3 also forbids `alloc` in a function a derivation is given by name at the call.
- The lifted-literal gradient copies every function that reads the literal table into one that
  takes the table as a by-reference parameter (`readers`, `literal.rs:110`), then derives by that
  parameter; direction `k`'s seed is a zeroed table with `1.0` at literal `k`'s index
  (`literal_gradient`, `:21`).
- A std derivation is an `@intrinsic` with an empty body (`compiler/std/derive.wrela:234`–`:310`)
  wired through: the lang item (`lang_items!`, `compiler/sema/src/defs.rs:182`; the derive items
  at `:648`–`:669`); `derived_callables` (`compiler/sema/src/effects.rs:558`); the match in
  `intrinsic` (`compiler/lower/src/gpu.rs:1375`, the derive arms from `:1387`); `DeriveKind`
  (`compiler/lower/src/instance.rs:34`); and in `compiler/lower/src/derive.rs`: `call` (`:15`,
  which maps the lang item to a kind and picks the arguments and the result), `signature`
  (`:136`), `build_next` (`:245`), `build` (`:379`), and the error mapping (`:537`–`:539`).
  `tape_fns` (`:84`) shows how a derivation is handed std functions to call
  (`ir::derive::TapeFns`).
- Derivations nest because a derived function is ordinary IR that a later derivation walks
  (`build_next` builds inner ones first).
- On the CPU a NaN's bits are made canonical where a bit cast observes them (§11.3). No other
  move of an `f32` changes its bits: a load, a store, building or taking apart a value, a
  `select`.

**Design.** Reverse mode is **forward mode's plumbing with one direction, whose "tangent" is a
slot number**, plus a record of each operation's local derivatives, swept backward afterwards
(Griewank and Walther's tape of local partials). No rule is written again: the local
derivatives come from calling the existing forward rules with unit seeds.

*The record* is std code, in `compiler/std/derive.wrela`:

```text
/// What one evaluation did, for a reverse sweep: for each number it computed from its input,
/// which earlier numbers it came from and its derivative by each.
pub struct Trace { a: Vec<u32>, b: Vec<u32>, da: Vec<f32>, db: Vec<f32> }

impl Trace {
    pub fn new() -> Trace             // with node 0, the number nothing flows from
    pub fn clear(mut self)            // back to node 0 alone, keeping its room
    pub fn nodes(self) -> u32
    fn leaf(mut self) -> u32                                         // a new input
    fn one(mut self, a: u32, da: f32) -> u32                         // 0 if a is 0
    fn two(mut self, a: u32, da: f32, b: u32, db: f32) -> u32        // 0 if both are 0
    fn sweep(self, from: u32) -> Vec<f32>   // each node's adjoint, the result's being 1
}
```

`sweep`: `adj[from] = 1`; for `i` from the last node down to 1, where `adj[i] != 0`:
`adj[a[i]] += adj[i] * da[i]` and `adj[b[i]] += adj[i] * db[i]`. Sixteen bytes a node.
`TRACE_MOST = 1 << 26` nodes (1 GiB); a push past it panics with "a reverse gradient's trace
passed 2^26 numbers: sum its terms with `std::lift::reverse_sum`".

*A slot travels as the bits of a value of its number's type.* An active `f32` whose node is
`s` has the shadow `bitcast_f32(s)`; a `vec3`'s shadow is a `vec3` of three such; a struct's is
a struct. So every move of a value moves its shadow with the same instruction, which is what
forward mode's plumbing already does for tangents. Node 0's bits are `0.0`, which is what the
plumbing writes for a tangent that is structurally zero. A slot is below 2²⁶, so its bits are
never a NaN's and no cast changes them.

*In `ad.rs`:* add `record: Option<RecordFns>` to `Ad` and to `derive`'s arguments, where
`RecordFns { one: FuncId, two: FuncId }` are std's `Trace::one` and `Trace::two`. With
`record`:

- `n` is 1. The cache key's top bits say the mode: `RECORDED: u8 = 0x40` beside `FILTERED`.
- The derived function takes one more parameter after the tangents, `trace` (by reference,
  mutable), and `call` passes it on to each derived callee.
- In `stmt_inner`'s `Let(v, e)` arm, for an active `v`, sort `e`:

| `e` is | Its shadow |
|---|---|
| **A move**: `Param`, `Load`, `Construct`, `Variant`, `Extract`, `ExtractDyn`, `Splat`, `Swizzle`, `Select` | `self.tangent(v, e, 0, out)`, unchanged: the forward rule for a move is the same move of the tangents |
| **A float converted to another float type** | The slot in the new type: cast the operand's shadow to its integer, widen or narrow the integer, cast to the new float type. No node |
| **Arithmetic**: `Unary(Neg)`, any `Binary`, any `Builtin` | Probed and recorded, below |
| Anything else (`Const`, `Zero`, `Bitcast`, an integer) | None, as in forward mode |

- **Probing.** For arithmetic `v = e(x1, x2, ...)`, for each distinct active operand `x` (the
  same `ValueId` twice is one operand): set `tv[x]` to a unit seed and every other operand's to
  `None`, call `self.tangent(v, e, 0, out)`, and put `tv` back. What comes back is the IR value
  of `∂v/∂x` along that seed, computed by the forward rule. Which seeds:
  - `e` is componentwise in `x` and `x` has `v`'s type (`+ - * /` of like types, `Neg`, the math
    builtins that §11.4 says apply per component, among them `min`, `max`, `clamp`, `mix`,
    `step`, `smoothstep`): one probe, seed `splat(1.0)`; component `c` of the answer is
    `∂v_c/∂x_c`.
  - `x` is a scalar: one probe, seed `1.0`; component `c` is `∂v_c/∂x`.
  - otherwise (`dot`, `length`, `distance`, `cross`, `normalize`, a vector by a scalar's
    type, any matrix): one probe for each component `j` of `x`, seed the unit vector `e_j`;
    component `c` of answer `j` is `∂v_c/∂x_j`.
- **Recording.** For each component `c` of `v`: gather its terms `(slot of x_j, ∂v_c/∂x_j)`
  over every probed operand, skipping a probe that came back `None`. Zero terms: slot 0. One:
  `trace.one`. More: chain `trace.two` (`r = two(s1, d1, s2, d2)`; then `r = two(r, 1.0, s3,
  d3)`; ...). `v`'s shadow is those slots' bits, built into `v`'s type. An `f64` operand's
  derivative is narrowed to `f32` for the record.
- Refuse, as E0700 with its own words, what a record can't hold: the GPU target ("a reverse
  gradient runs on the CPU"), and `filter` together with `record`.
- **The heap rule holds unchanged.** A recorded function is checked exactly as a forward one is
  (`no_gpu_or_heap_work`, `heap_store`): the function being derived may not touch heap memory.
  The trace is heap memory too, but it is the derivation's own: the calls to `Trace::one` and
  `Trace::two` are written into the **derived** function, which nothing checks again, and they
  are never themselves derived. It follows that a recorded function can't be derived a second
  time (a `reverse` inside a `gradient` or an `interval` is E0700, by the heap rule): say so in
  the spec.

*In `derive/mod.rs`:* `pub fn recorded(m, cache, f, ncap, fns: RecordFns) -> Result<FuncId>`:
the function `(captures.., x, slots, trace) -> (value, slot)` for the input parameter at
`ncap`, `mask[ncap] = all_bits(..)` as `value_and_gradient` sets it (`ad.rs:20`).

*In std* (`derive.wrela`):

```text
@intrinsic
@effects(alloc)
fn recorded<X>(f: fn(X) -> f32, at: X, slots: X, trace: mut Trace) -> (f32, f32) {}

/// `f`'s value at `at`, and its derivative by each of `at`'s numbers.
pub fn reverse<const N: u32>(f: fn([f32; N]) -> f32, at: [f32; N]) -> (f32, [f32; N])
```

**The input is an array, not a run or a `Vec`**: what a derivation follows stays out of heap
memory, and an array is what the literal table already is to the forward derivation. (A run of
an array may work as an input too; it is untried, so it is not this stage's.) `reverse` is std
code, not derived, so it may use what it likes: a new `Trace`; an array of `N` leaves' bits;
`recorded`; `sweep` from the result's slot; the adjoints of the leaves, in order. `recorded` is
the only intrinsic (lang item `Recorded = "std::derive::recorded"`, kind
`DeriveKind::Recorded`). It carries `@effects(alloc)`, since the trace grows: so §8.3 refuses a
`reverse` named inside another derivation before the heap rule would.

*For literals* (`literal.rs` and `lift.wrela`): `literal_recorded`, as `literal_gradient` but
deriving the table-taking copies with `record`, the shadow table zero but for each chosen
literal's leaf bits at its index, and no limit of 255.

```text
/// `f`'s value, and its derivative by each literal of `which`: as `gradient`, at the cost of
/// a few evaluations however many literals.
pub fn reverse(f: fn() -> f32, which: [Literal]) -> (f32, Vec<f32>)

/// The same for a sum of `n` terms, one sweep a term, so the trace holds one term at a time.
pub fn reverse_sum(n: u32, term: fn(u32) -> f32, which: [Literal]) -> (f32, Vec<f32>)
```

Both take `f` as a parameter and hand it on, so, as with `Minimize`, the code `f` reaches is
held to the heap rule alone and may allocate where it holds nothing of a literal. The `Vec`
they return is made by std's own code, after the sweep.

**Steps.**

C.1 `Trace` in std with `@test`s: `x * y + sin(x)` recorded by hand (`leaf`, `two`, `one`) and
swept gives `(y + cos(x), x)`; a push past `TRACE_MOST` panics (test with a small constant
through a private constructor).

C.2 The record mode in `ad.rs` for scalars only (`f32` operands and results), `recorded` in
`mod.rs`, and the wiring list above for `std::derive::recorded`. Add the lang item to §20's
closed list in the spec in this commit. Test: `reverse(|x| x[0] * x[1] + sin(x[0]), [2.0, 3.0])`.

C.3 Vectors, matrices and `f64`: the three probe shapes. Test
`a_record_moves_slots_bit_for_bit` in the `wrela-ir` crate: a function that only builds a
`vec3` from three inputs, swizzles it, passes it through a helper and a `select`, and returns a
component; the returned slot is the right leaf's.

C.3b The heap rule under a record, in `suite/derive.rs` beside the two heap tests (`:402`,
`:484`): the same cases with `reverse` in place of `gradient` give E0700 with the same words at
the same lines; a function that makes its weights in a `Vec` in a helper given nothing of the
input, and hands them back as an array, records and gives the right gradient; `reverse` inside
`gradient` is refused.

C.4 **G-agree, the stage's main gate.** For each of the corpus's 25 fields
(`compiler/tests/fields`): the reverse gradient by the point (through
`|x: [f32; 3]| f.distance(vec3(x[0], x[1], x[2]))`) agrees with `gradient` within
`1e-5 * (1 + abs(g))` at 1,000 points. Add an export for it beside `gradient`
(`fields/main.wrela:546`) and a test beside `gradients_agree_with_central_differences`. Then
stage A's eighth oracle, **O8**: the same agreement on typed programs, 32 seeds in the gate,
5,000 in a soak. Paste the worst difference of each.

C.5 Literals: `literal_recorded`, `std::lift::reverse` and `reverse_sum`. Test in
`compiler/tests/tests/suite/lift.rs` on `compiler/tests/lift`: `reverse` agrees with `gradient`
on every literal, within 1e-5 relative.

C.6 *Hypothesis (S6):* `Minimize::run` uses `reverse`. In `gradient_at`, when `free.len() >=
REVERSE_FROM`, call `std::lift::reverse`; if the first evaluation's trace passes `TRACE_MOST / 4`
nodes, stay on the forward path for that solve. Measure `wrela solve` on a scratch copy of the
frozen wolf (`compiler/tests/frozen/wolf`; its subject is `subject::subject()`, and it has no
spec, so give the copy one of three measures: a `top`, a `width` and a `front`) with 16, 64 and
256 literals of `creature.wrela` free: seconds for the whole solve, both paths, three runs
each, the median. Set `REVERSE_FROM` to the
least count where reverse is at least 1.3 times faster. If reverse wins at every count, delete
the 16-at-a-time path (rule 6). If it never wins by 1.3 times at 256, leave `REVERSE_FROM` at
`u32::MAX` and say so.

C.7 A test of scale, `reverse_is_a_few_evaluations`: a function of 4,096 numbers held in a
`[f32; 4096]` (a two-layer net: 64 inputs, 60 hidden `tanh`s, one output); `reverse`'s gradient
agrees with central differences on 16 of its weights within 1e-3 relative, and the reverse call
takes less than 10 times one plain evaluation's time (measure with fuel, not a clock:
`CpuBuild::load_metered`, `runtime/native/src/lib.rs:1166`, and compare the fuel spent). Then
find how large `N` may be: arrays live on the stack, so try 65,536 and 1,048,576, and put the
largest that runs in the spec as the limit, with what happens past it.

**Spec.** §14 (a row for `reverse` in its table; §14.1 and §14.2: how it is derived, that it runs
on the CPU, the trace's limit), §20's closed list, §23.1 (`std::lift::reverse`).

**Checks.** `tools/check.sh derive::`, `tools/check.sh lift::`, `tools/check.sh typed::`,
`tools/check.sh test_items::std_tests_pass`, `tools/check.sh closed_list::`,
`cargo test -p wrela-ir`.

**Done when:** C.4's two agreements hold with their worst differences pasted; C.7's fuel ratio
is pasted; C.6's table of seconds is in the commit, whichever way it came out.

## Stage D: outlines as measures

**Goal.** A spec can hold a subject to its blueprint's outlines and to a reference image, the
way it holds it to a height or a width, with a derivative `wrela solve` can follow through the
edge of the silhouette.

**Second user.** Any surface compared with any outline: a logo's field against its drawing, a
terrain's profile against a survey.

**Facts.**

- A blueprint is a module `blueprint` with `side()`, `front()`, `top()`, each a closed polygon
  as `Vec<vec2>` in metres: side is (z, y), front (x, y), top (x, z) (§23.7). Nothing in std
  reads one yet: "a tool can ... measure where the subject's silhouette misses it, and fit
  literals to it".
- The measures are `std::field` functions of any surface (`hit`, `compiler/std/field.wrela:297`;
  `top`, `:329`; `farthest`, `:369`). `hit` searches with values, then takes a Newton step, "so
  its derivative by anything the field reads is the exact one: the search's own change cancels".
  There is no way to hold a value out of a derivative, so a measure must be written so that what
  it searches with doesn't depend on the literals, or cancels.
- `std::derive::filtered(f, at, width)` (`derive.wrela:290`) is `f(at)` averaged over a
  footprint: a `step` on the input's path gives the share of the footprint on each side of the
  step, to first order (§14.1). It is forward mode, so it nests under `std::lift::gradient` and
  `jacobian`.
- A spec's checks take numbers: `c.near(name, value, target, within:)`, `c.at_most(..)`
  (`trait Checks`, `compiler/std/lift.wrela:192`). `wrela solve --spec` minimizes their `Loss`;
  `--fit` minimizes a `[f32; M]`'s squares by Levenberg–Marquardt on `jacobian` (`fit`, `:372`).
  Since 0.33.0, `--spec --write` writes only where every check holds, and exits 1 otherwise.
- A blueprint's outline is a `Vec<vec2>` and a decoded image holds its pixels in heap memory. A
  spec's loss is derived, so by §14.2 **neither may be read by a function the derivation
  derives**; a function given nothing that depends on the literals may read them, and hand back
  an array. `wrela solve`'s glue hands the loss to `gradient` through `Minimize`, so such a
  function may be reached.
- `std::png::decode(embed("x.png"))` gives an `Image` of RGBA8 with `texel` and `sample`
  (0.19.0), in a constant.
- Every surface states where it is: `Surface::bound()` (`field.wrela:67`), or `UNBOUNDED`.

**Design.** All of this is std code; the compiler doesn't change.

```text
/// The axis a subject is seen along, with a blueprint's coordinates for what is seen.
pub enum View: Copy { Side, Front, Top }

impl View {
    /// The point of space at `at` in this view's coordinates, `depth` along its axis.
    pub fn point(self, at: vec2, depth: f32) -> vec3
}

/// How much of a footprint `width` across, at `at` in `view`'s coordinates, `field`'s
/// silhouette covers, from 0 to 1. The silhouette is looked for between `near` and `far`
/// along the view's axis.
pub fn coverage<S: Surface>(field: S, view: View, at: vec2, width: f32, near: f32, far: f32) -> f32
```

`coverage`: let `least(u)` be the least of `field.distance(view.point(u, t))` over 48 depths `t`
evenly spaced from `near` to `far` (fixed, so they don't depend on the field), by a chain of
`min`s. The silhouette at `u` is where `least(u) <= 0`. Then
`coverage = filtered(|u: vec2| step(least(u), 0.0), at, vec2(width))`: the share of the footprint
inside. Its derivative by a literal is, by the chain rule through `filtered`'s first-order
range, `least`'s derivative where the edge crosses the footprint, and zero away from the edge,
which is what moves an outline.

```text
/// How far `field`'s silhouette in `view` is from `outline` (a closed polygon in the view's
/// coordinates): the root of the mean square difference of their coverages over a grid of
/// cells `cell` across that holds the outline and two cells round it. 0 is a match; 1 is no
/// overlap at all.
pub fn outline_miss<S: Surface>(field: S, view: View, outline: [vec2], cell: f32, near: f32, far: f32) -> f32

/// The same against an image's opaque part (its alpha at least a half), the image laid over
/// the rectangle from `lo` to `hi` in the view's coordinates, its top row at `hi.y`.
pub fn image_miss<S: Surface>(field: S, view: View, image: Image, lo: vec2, hi: vec2, near: f32, far: f32) -> f32
```

A cell's share of the polygon is the share of a 4 × 4 grid of points in it that are inside
(even-odd rule). The grid is at most 64 cells a side: a `cell` that would need more panics with
the size it would need.

**How each is split, for the heap rule.** The polygon and the image are heap memory; the field
depends on the literals. So each measure is two functions:

- `wanted(outline: [vec2], lo: vec2, cell: f32, cells: [u32; 2]) -> [f32; 4096]` (and
  `wanted_of(image: Image, ..)`): every cell's share of the outline or of the image, as an
  **array**. It is given nothing that depends on a literal, so a derivation runs it as it is,
  and it may walk the `Vec`. It allocates nothing itself.
- The measure proper, which a derivation derives: it calls `wanted` once, then loops over the
  cells with `coverage` and the array, summing in locals. It reads no `Vec`.

If lending the blueprint's `Vec` to the measure at the spec's call site turns out to be
refused (the lending happens in the spec's own function, which is derived), take the outline as
a function instead (`outline: fn() -> Vec<vec2>`, called only inside `wanted`), and write the
spec's line as `outline_miss(f, View::Side, blueprint::side, ..)`. D.3's last test settles which.

A spec then says:

```text
c.at_most("side outline", outline_miss(f, View::Side, blueprint::side(), cell: 0.02, near: -1.0, far: 1.0), 0.0, within: 0.05)
```

**Steps.**

D.1 `View`, `coverage`, with `@test`s in `field.wrela`: a sphere of radius 0.5 from each view:
1 at its centre, 0 two radii out, between 0.4 and 0.6 on its edge with a footprint of 0.05.

D.2 The derivative is the edge's, **G-agree**: for a sphere whose radius is a lifted literal
(a test package beside `compiler/tests/lift`), the sum of `coverage` over a 32 × 32 grid. Its
derivative by the radius from `std::lift::gradient` agrees within 10% with the central
difference (radius ± 0.002) of the same sum computed at 16 × 16 points a cell with a footprint
of zero (a hard edge, supersampled). Paste both numbers. This is the claim "a correct
derivative through an antialiased edge", tested.

D.3 `outline_miss` and `image_miss` with `@test`s: a sphere against a 64-gon of its own radius
is under 0.03; against one of twice its radius is over 0.3. A fixture PNG for `image_miss`:
make it as the PNG fixtures were made and say how in `docs/citations.md`'s Fixtures paragraph.
Then the test that the heap rule is kept, in `suite/measures.rs`: a spec whose check is an
`outline_miss` against a module's `blueprint::side()` (a `Vec`), run with a `Loss` under
`std::lift::gradient` handed through a parameter (as `slope` is in `derive.rs`'s heap tests):
it builds with no E0700, and the derivative by the sphere's radius isn't 0. If it is E0700,
take the fallback above and say so in the commit.

D.4 A solve, in `compiler/cli/tests/suite/solve.rs`: a package whose subject is a sphere joined
to a box, its spec holding it to a blueprint drawn from other sizes. `wrela solve --spec --free
<file> --write` from sizes 20% off ends with every size within 2 mm, the miss under its
`within`, `"written": true`, and no warning that a freed size was never felt (0.33.0 warns of
a literal whose derivative was 0 at every step: one here would mean the outline's derivative
was lost).

D.5 Spec: §23.6 (the two measures, with the example above), §23.7 (its last sentence becomes
what std now does). `wrela doc std::field::coverage` answers.

**Checks.** `tools/check.sh test_items::std_tests_pass`, `tools/check.sh measures::`,
`tools/check.sh lift::`, `cargo test --release -p wrela --test suite -- solve::`.

**Done when:** D.2's two derivatives are pasted and agree; D.4's solve recovers the sizes.

## Stage E: `wrela inspect`

**Goal.** Lints for content, as the compiler has lints for code: a report an agent reads to
learn what it made, with a counterexample point for each finding, and small pictures that need
no GPU.

```text
wrela inspect <package> <module>::<fn> [--depth <n>] [--region <lo>..<hi>,<lo>..<hi>,<lo>..<hi>]
              [--thin <metres>] [--lift] [--views <dir>] [--json]
```

`<fn>` takes nothing and returns a `Surface` (as a spec's subject does).

**Second user.** Any field anyone writes: a level's collision field, a font's glyph.

**Facts.**

- `interval(f, box)` bounds a function over a box; `Box3::split()` gives its eight octants
  (`compiler/std/derive.wrela:113`); `certify_positive` walks an octree with it and gives
  `Holds`, `Fails(point)` or `Unknown(box)` (`compiler/std/field.wrela:509`, `Certified` at `:481`).
- A surface answers `parts()`, `part_at(p)`, `part_distance(p, i)`, `part_name(i)`
  (`field.wrela:199`–`:218`; `PartName` at `:558`), and `part_share(field, p, i)` (`:284`).
- Debug builds check a stated Lipschitz bound and a stated bound against derived ones:
  `check_lipschitz` (`:2133`), `check_bound` and `bound_broken` (`:2175`, `:2186`).
- A lifted build knows each literal's place in the source (`std::lift::Literal::source()`), and
  `std::lift::reads(f)` gives the literals a function can read.
- `wrela solve` and `wrela precision` show how a command builds a program round a function of a
  package and runs it: write a wrapper `main.wrela` with `glue_package`
  (`compiler/cli/src/main.rs:409`; `solve.rs`'s `glue`, `:252`), build it
  (`wrela_driver::build_lifted`, `compiler/driver/src/lib.rs:306`, or `build`), load it on the
  CPU host (`CpuHost::load`, `solve.rs:404`) and call its exports. An export's result can be a
  number, a vector, or a struct, tuple or array of them (§15.2).
- `wrela solve` already warns of a freed literal its function never changed with
  (`Minimize::changes`, `compiler/std/lift.wrela:451`; 0.33.0). That is about one function along
  one solve's path. This stage's `dead-literal` is about the whole surface, with no solve.
- The subject's own code may build what it likes in heap memory where that holds nothing of a
  literal, but only if the derivation is handed its function through a parameter (§0.2).
- The `png` crate is already a dependency of the hosts (`docs/citations.md`), and the native
  host writes PNGs for goldens (`runtime/native/src/image.rs`).

**Design.** The analysis is std code over any surface; the command is glue.

*Pieces.* `std::field::pieces(field, region, depth) -> Pieces`: an octree over `region`, each
cell classed by `field.interval(cell)`: **inside** (`hi < 0`), **outside** (`lo > 0`),
**edge** (neither; split until `depth`). Cells are kept as keys at the finest level they
reached. Two sets joined by faces (a union-find over the keys, sorted):

- `sure`: inside cells only. Two inside cells that share a face are joined in the body. So each
  group of `sure` is one piece, **certified connected**.
- `maybe`: inside and edge cells. A path inside the body can only pass through such cells. So
  two `sure` groups in different `maybe` groups are **certified apart**, by at least the
  thickness of the outside cells between them.

The report: the pieces certainly there (`maybe` groups that hold an inside cell), each with its
box, its volume's bounds and the parts that decide it (from `part_at` at its inside cells'
centres); **necks**: two `sure` groups in one `maybe` group ("joined, if at all, by something
thinner than a cell: `<size>`"), with the edge cell between them; **specks**: `maybe` groups
with no inside cell, with one point each ("something smaller than a cell may be here").
`pieces.one()` is true when there is one piece, no neck and no speck: the subject is certified
to be a single body down to the cell size.

*The findings.* Each has a stable `kind`, a `proof` (`"certified"` or `"sampled"`), and a point.

| `kind` | What it says | How |
|---|---|---|
| `floating` | A piece apart from the largest one, with the parts that make it and the gap | `pieces`, certified |
| `neck`, `speck` | As above | `pieces` |
| `dead-part` | A part that decides the surface nowhere | Certified where, over every edge cell, `interval(|p| field.part_distance(p, i) - field.distance(p))` is above the largest blend radius in reach; else sampled: `part_at` never names it and `part_share` is under 1e-3 at every edge cell's centre |
| `thin` | A part whose box is thinner than `--thin` (5 mm unless said) | The part's own cells, certified |
| `dead-literal` (with `--lift`) | A literal that moves the surface nowhere: its derivative of the distance is exactly 0 at every sample | `std::lift::jacobian` of the distances at up to 512 edge cells' centres, sampled. The points travel as a `[vec3; 64]` the closure captures, never a `Vec`, and the glue hands the closure to `jacobian` through a function of its own (`fn by<const N: u32, R>(f: fn() -> R, which: [Literal; N]) -> (R, [R; N])`) |
| `faint-literal` (with `--lift`) | One whose largest derivative is under 1e-6 of the largest literal's | The same |
| `bound-broken`, `steeper-than-stated` | `bound()` or `lipschitz()` states less than the field does | `bound_broken`, `check_lipschitz`; the point they find |

*Previews.* `std::field::preview(field, view, size: u32, row: u32) -> [vec4; 128]`: one row of a
picture `size` across (at most 128) from `view`: each pixel a `hit` along the view's axis from
outside the bound; its colour the surface's normal against a fixed light (`0.2 + 0.8 *
max(dot(n, l), 0)`), darker with depth; alpha 0 where nothing is hit. `--views <dir>` writes
`side.png`, `front.png` and `top.png` (the command asks for each row through an export and
writes the files with the `png` crate), and the report names them, so an agent with no GPU can
look at what it made.

*The command* (`compiler/cli/src/inspect.rs`): glue that calls the std functions on the
subject and prints their answers as lines the command parses, as `precision.rs` parses its notes
(`notes`, `:154`). The region is `field.bound()`'s box grown by a tenth; a surface that says
`UNBOUNDED` needs `--region` (the command says so and exits 2). `--depth` is 7 unless said
(cells a 128th of the region), at most 9.

```json
{"subject": "creature::wolf", "region": {"lo": [..], "hi": [..]}, "cell": 0.0125,
 "pieces": {"certain": 1, "one": false},
 "findings": [
   {"kind": "floating", "proof": "certified", "parts": ["left ear"], "gap_at_least": 0.025,
    "at": [0.11, 0.92, 0.3], "box": {...}},
   {"kind": "dead-literal", "proof": "sampled", "file": "wolf.wrela", "line": 41, "col": 18,
    "text": "0.35", "samples": 512}],
 "views": {"side": "views/side.png", "front": "views/front.png", "top": "views/top.png"}}
```

It exits 0 with findings (they are advice), 1 if the package doesn't build, 2 on a usage error.

**Steps.**

E.1 `pieces` and `Pieces` in std, with `@test`s: one ball is one piece; two balls 0.2 apart are
two, certified apart by at least a cell; two balls joined by a rod thinner than a cell report a
neck; a ball and a ball of radius under half a cell report a speck.

E.2 The other findings as std functions (`dead_parts`, `thin_parts`), with `@test`s: a ball
wholly inside a larger one in a union is a certified dead part; in a smooth union with a blend
that reaches the surface it is not.

E.3 `preview`, with a `@test` that a ball's middle row is opaque in the middle and clear at its
ends, and brighter toward the light.

E.4 The command, its JSON and text, `--views`, `--lift` (a lifted build of the subject's
package, `dead-literal` and `faint-literal`). If the subject's own code is E0700 under
`--lift` (it keeps something that depends on a literal in a `Vec`), that is the most useful
finding of all: report it as a finding of kind `not-derivable`, with the compiler's message
and line, and go on with the findings that need no derivative. CLI tests in `compiler/cli/tests/suite/inspect.rs`:
on the frozen wolf (`compiler/tests/frozen/wolf`, `subject::subject`) it reports one certain
piece (if it doesn't,
that is a finding about the fixture: read the picture, and say in the commit which it is); on a
test package with an ear moved off the head it reports `floating` naming the ear; a literal
multiplied by zero is a `dead-literal` at its line.

E.5 Its row in §23.9, a new §23.10 "Reading what was made" in the spec (the kinds, what
`certified` means, the previews), its usage line and doc comment.

**Checks.** `tools/check.sh test_items::std_tests_pass`,
`cargo test --release -p wrela --test suite -- inspect::`.

**Done when:** E.4's three tests pass, and the wolf's report and its three pictures are in the
pull request.

## Stage F: `wrela sliders`

**Goal.** A subject with four hundred literals has a handful of independent ways to change.
The command finds them, says what each does and where, moves along one, and draws them.

```text
wrela sliders <package> <module>::<fn> --free <file>[:<lines or names>] [--top <k>]
              [--depth <n>] [--move <i>=<amount>]... [--write] [--sheet <file.png>] [--json]
```

**Second user.** Any lifted program with a field: the directions a level's terrain can move, a
font's weight and width found rather than authored.

**Facts.**

- `std::lift::jacobian(f, which)` gives, for a function of several numbers, how each changes
  with each literal of `which`, from one derived evaluation (`compiler/std/lift.wrela:113`; at
  most 255 literals a call, `literal.rs:34`).
- A literal's written form (its decimals, its unit) is `Literal::source()`; `wrela solve` rounds
  a moved literal to its decimals (`round_to`, `lift.wrela:933`) and writes through `wrela edit`
  (`solve.rs`, its `--write`).
- `--free` names literals by line or, since 0.33.0, by the name a file binds a value to
  (`body.wrela:reach,drive,40-44`): `parse_free` (`compiler/cli/src/solve.rs:118`) and `named`
  (`:219`). Make them `pub(crate)` and use them: one way to say which literals (rule 6). The
  report's literals carry their `name`, as `wrela solve`'s do.
- Stage E's `pieces` gives the edge cells; `part_at` names the part at a point.

**Design.**

*The matrix.* Take up to 512 points on the surface: the centres of stage E's edge cells (every
`n`th, to 512), each moved onto the surface by two Newton steps along the gradient. For point
`p_i` and literal `θ_j`, the surface there moves along its normal by
`J[i][j] = -(∂f/∂θ_j)(p_i) / |∇f(p_i)|` for a unit change of the literal. The glue's export
evaluates `jacobian` of the distances at 64 points at a time, 16 literals at a time, and returns
the block; the command gathers `J` (points × literals) in Rust. As in stage E: the points are a
`[vec3; 64]` the closure captures, and the closure reaches `jacobian` through a function of the
glue's own, so the heap rule is the only one the subject's code is held to (§0.2).

*The scale.* A literal's unit step is `s_j = max(abs(θ_j), q_j)`, `q_j` being ten of its last
written decimal (a literal written `0.25` has `q = 0.1`). Work with `J' = J · diag(s)`: a
slider's unit is "each literal by its own size".

*The sliders* are `J'`'s leading right singular vectors: the directions in literal space that
move the surface most, each at right angles to the ones before. Compute the top `k` (8 unless
said) in Rust, in `f64`, by orthogonal iteration on `J'ᵀJ'` applied as two products (Golub and
Van Loan): `k` start vectors from a fixed seed, 60 rounds of multiply and Gram–Schmidt, then
the singular values from the vectors' images. No crate. A unit test checks it against a matrix
whose singular vectors are known.

*What each slider is.* For slider `v` with singular value `σ`: the surface's motion is `J'v`
(a number for each point). Report: `σ / sqrt(points)`, the root mean square motion in metres
for one unit; the literals with the largest `|v_j|` (at most 12), each with its file, line,
text and share; the parts that move most (the points grouped by `part_at`'s name, by their
share of the squared motion); whether the motion is the same on both sides of x = 0, or
opposite, or neither (pair each point with the nearest point to its mirror image); and the mean
motion's sign (`grows` or `shrinks`). Its **name** is made from those, by rule, so it is the
same every run: the two parts that move most, then `grows`, `shrinks` or `shifts` (mixed
signs), then `(mirrored)` or `(one side)`: `ears, head: grows (mirrored)`.

Also reported, because the matrix shows them: **dead** literals (a zero column) and
**redundant** pairs (two columns whose angle's cosine is over 0.999 in size: one literal does
what the other does, in the ratio given).

*Moving.* `--move 2=1.5` adds `1.5 · s_j · v_j` of slider 2 to each literal `θ_j` and rounds
each as `wrela solve` does. Without `--write` it prints the edits; with it, it writes them
through the same path `wrela solve --write` uses. Several `--move`s add up.

*The sheet.* `--sheet` writes one PNG: a row for each slider, three pictures across (the
subject at -1, 0 and +1 units), each a stage E `preview` from the side at 96 pixels, with the
slider's number. It uses the lifted build's `__lift_set` to move the literals between pictures;
nothing is written to the source.

**Steps.**

F.1 The linear algebra in `compiler/cli/src/sliders.rs`, with unit tests (a known 6 × 4 matrix;
a matrix of rank 2 gives two sliders and zeros).

F.2 The glue and the matrix, on the frozen wolf's own file. A CLI test: a test package with two
balls whose radii are separate literals has two sliders, each naming one ball, of nearly equal
singular values; with a third literal that scales both, the report lists it as redundant with
neither but the third singular value is under a hundredth of the first.

F.3 The report, the names, the JSON, `--move` and `--write`. A CLI test: `--move 1=1 --write`
changes only the literals its report lists, each within its rounding of what the report said.

F.4 The sheet. A CLI test that the PNG has the size it should and that the two outer pictures
of a row differ from the middle one.

F.5 Its row in §23.9, a paragraph in §23.10, its usage line and doc comment.

**Checks.** `cargo test -p wrela sliders`, `cargo test --release -p wrela --test suite --
sliders::`.

**Done when:** on the frozen wolf the command prints eight named sliders in under two minutes on
the M4 (paste the report and the time), and the sheet is in the pull request.

## Stage G: `wrela fuzz`

**Goal.** Find the inputs that trap a program's simulation, and hand back each as a tick log
that replays it. A trap in `@deterministic` code is a trap on every client at the same tick
(§12), so one peer's input that causes it stops a whole room.

```text
wrela fuzz <package> [--ticks <n>] [--time <seconds> | --runs <n>] [--seed <n>] [--fuel <n>]
           [--seeds <dir>] [--out <dir>] [--replay <file-or-dir>] [--room] [--json]
```

**Second user.** Any ticked program and any server's rooms; the release process, as a check.

**Facts.**

- A ticker is `std::tick::start(state, hz, step, hash)` (`compiler/std/tick.wrela:148`); each tick
  the step gets a `Ticked` with its records (`:22`). The host calls the WASM export `__tick`
  (`EXPORT_TICK`, `runtime/abi/src/lib.rs:98`), with the tick's records written at `TICK_RECORDS`
  (at most `MAX_TICK_RECORDS` = 256, `runtime/abi/src/memory.rs:218`–`:220`), and reads the state's
  hash at `TICK_HASH` (`:105`).
- A record is a 24-byte event (`EVENT_SIZE`, `runtime/abi/src/input.rs:149`) of an `EventKind`
  (`:158`: pointer, wheel, key, text, pad, compose, paste, assist, field, posted; the network's
  kinds never become tick records). A script of input is `Vec<Scripted>` (`:490`), parsed from
  JSON by `parse_script` (`:511`); `records_at(script, tick)` (`:502`) gives a tick's records.
- A tick log is `TickLog` (`runtime/abi/src/ticks.rs:40`) with `encode` (`:147`) and `decode`
  (`:167`): a header with the WASM's hash, then for each tick its records and the state's hash.
  `wrela-host --replay <log>` runs one; `CpuBuild::replay(&log, workers)`
  (`runtime/native/src/lib.rs:1269`) does it in process.
- A tick test drives ticks in process: `host.push_records(records_at(script, k))` then
  `host.tick()` (`CpuHost`, `runtime/native/src/lib.rs:1461`, `:1468`; `run_on`,
  `compiler/driver/src/frames.rs:633`, does it at `:698`). A CPU host can be metered: `CpuBuild::load_metered(dir,
  fuel)` (`:1166`), fuel set for each tick. A trap comes back as an error with its source line
  (the `lines` section).
- A debug build traps where a float operation makes a NaN (§11.3), and every build traps on
  integer overflow and an index out of range.
- The WASM back end's options are `wrela_wasm::Options` (`compiler/wasm/src/lib.rs:186`); a
  function's blocks are emitted in `compiler/wasm/src/func.rs` (an `If` and a `Loop` each in
  their own arm, near `:1262`).
- A room's program is driven by `RoomHost` (`runtime/native/src/rooms.rs:308`, `open`): it feeds
  connections and messages to the build's `serve`, retries a call that trapped one event at a
  time (`one_by_one`, `:638`), marks an event that traps alone as poisoned (`alone`, `:657`) and
  quarantines a room whose retry traps too (`quarantine`, `:413`).
- The memory of a program is one shared linear memory; nothing copies it whole today except a
  test (`runtime/native/src/program/tests.rs`).

**Design.**

*Coverage.* `wrela_wasm::Options::coverage: bool`. With it, the module reserves a 64 KiB map
(a region the emitter places after the module's data, its address in an exported global
`__wrela_cov`) and, at the start of every function, of each side of every `If` and of every
loop body, emits a store of 1 to `map[site % 65536]`, `site` counting up through the module.
Only `wrela fuzz` builds with it (S4): add `coverage` to `BuildData` and a driver entry
`wrela_driver::build_fuzz(dir)` (a debug build with coverage and no GPU output, as
`lower_for_cpu` makes for CPU-only runs, `compiler/lower/src/lib.rs:389`). A unit test in the
`wrela-wasm` crate: a function with an `if` writes two different sites depending on its
argument.

*The loop* is `compiler/cli/src/fuzz.rs`, in process on the CPU host with no helpers
(`start_with(0)`), so a run is the same every time for a seed.

- An **input** is a `Vec<Scripted>` over `--ticks` ticks (64 unless said). Making one new: for
  each tick, 0 to 3 events of the kinds with no payload (pointer move, down and up; wheel; key
  down and up; pad button and axis; posted), each field drawn from small sets with an edge now
  and then (0, 1, the largest value, a negative where signed). Mutating one: change a field,
  drop an event, copy an event to another tick, splice two inputs, add a new event. Text and
  paste events come only from seeds.
- **Running** an input: a fresh host, `init`, then each tick's records and `tick()`, with
  `--fuel` (2²⁸ unless said) a tick. After the run, read the map: an input that set a site no
  input set before joins the corpus.
- **A finding** is: a trap (its message and source line); a tick past its fuel (`hang`); and,
  checked once for every input that joins the corpus, **a different hash on a second run** of
  the same input (`not-deterministic`: something the checker let through).
- **Seeds:** `--seeds <dir>` holds `.json` scripts and `.ticks` logs (a log's records become an
  input). With none, the corpus starts from the empty input.
- **Minimizing** a finding (Zeller and Hildebrandt): try dropping the ticks after the failing
  one; then halves, quarters, ... of the events; then single events; then each field toward 0;
  keep a change that still gives the same finding (the same kind at the same source line).
- **Output:** `--out` (`build/fuzz` unless said) gets, for each distinct finding (by kind and
  line), `<kind>-<hash>.ticks`, a tick log `wrela-host --replay` runs, and `<kind>-<hash>.json`,
  the same as a script. The report: executions, executions a second, sites covered, corpus
  size, and each finding with its kind, message, file and line, tick, and paths. It exits 1 if
  there is a finding, 0 if not.
- **`--replay <file-or-dir>`** runs saved logs and scripts and reports which still fail: a
  regression check. A log made by another build of the program is run as a script (its records
  alone), since its hashes are the other build's.

*StateHash as coverage* is not used: nearly every input gives a new hash, so it tells inputs
apart no better than chance. The hash is the determinism check above.

*Snapshots* are an optimization, tried last (G.5): running every input from `init` is simple
and right.

*Rooms* (`--room`): the package's server build, driven through `RoomHost` in a scratch
directory. An input is a list of events: a connection opens; a message of bytes from one; a
connection closes; time passes. New messages are random bytes of 0 to 64; mutation flips bits,
changes lengths, repeats a message, and takes bytes from the seeds (`--seeds` may hold a room's
journal, whose records become messages). Coverage is the same map. Findings: `poison` (an
event that traps alone: the room goes on, but its reducer has a bug; the bytes are the report)
and `quarantine` (the room is lost). Output: the events as JSON and the message as a `.bin`.

**Steps.**

G.1 Coverage in the back end and `build_fuzz`.

G.2 The loop for ticks, the findings, the report. Tests in
`compiler/cli/tests/suite/fuzz_cmd.rs` (the suite already has a module named for another fuzz;
don't reuse the name): a ticked program whose step adds a key's code into a `u8` counter until
it overflows: `wrela fuzz --runs 20000 --seed 1` finds a trap at that line; a program that
divides by `pointer x - 300` finds the trap only because coverage leads it there (assert it is
found within `--runs 200000`; if it isn't, the mutator needs the comparison's operand: add
"values seen in comparisons" to the field sets, and say so); a program with no bug reports
none and exits 0.

G.3 Minimizing, the files, `--replay`. Test: the overflow's log, minimized, has only the events
that count; `wrela-host --replay` on it fails at the same line; `wrela fuzz --replay` on the
directory exits 1 before the bug is fixed and 0 after.

G.4 Rooms. Test: the rooms test program (`compiler/tests/rooms-client` and its server) with a
reducer changed to index an array by a message's first byte: `--room` reports `poison` with a
message whose first byte is out of range.

G.5 *Hypothesis:* snapshots. Keep the memory and each instance's globals after `init` and put
them back before each input, in place of a fresh host. Measure executions a second on G.2's
first program with and without. Keep it if it is at least 2 times faster and G.2's and G.3's
tests pass with it; else delete it and say what the number was.

G.6 Its row in §23.9, a paragraph in §16.2 ("Counting, logging and replaying ticks") on
fuzzing and replaying a finding, its usage line and doc comment. `docs/release.md` §2 gains a
line: a release runs `wrela fuzz --replay` over any saved findings of the frozen programs.

**Checks.** `cargo test -p wrela-wasm coverage`, `cargo test --release -p wrela --test suite --
fuzz_cmd::`.

**Done when:** G.2 to G.4's tests pass; the executions a second on the M4 are in the commit.

## Stage H: proven tables

*Hypothesis.* **Goal.** A function of one, two or three numbers, worked out once over a box
into a texture, with the error of reading it there **proven**; and a field kept that way, with
its own code where the proof doesn't reach.

**Second user.** §18.13's own two: the sky along a direction, a noise at a point. And any field.

**Facts.**

- `wrela pipelines` lists, under each pipeline, "calls a table could answer": the functions an
  entry point calls with at most three numbers that vary between invocations, with their count
  and work (§18.13; `few` in `--json`; `ir::same::few_input_calls`,
  `compiler/ir/src/same.rs:511`). "Whether a table pays is the reader's to judge, and its
  measure." Nothing builds the table or says how fine it must be.
- Work (§18.11): a texel load is 8, a sample 16; a division, a root or a transcendental 4 a
  component.
- Derivations nest: the `gradient` of a `gradient`'s component is a row of second derivatives,
  and the `interval` of a derivative bounds it over a box (§14.2). Whether three deep is
  practical is this stage's first question.
- `std::field::Sampled3d` (`compiler/std/field.wrela:643`) is a surface read from an `R32Float`
  3D texture between the eight texels round a point, with that interpolant's own gradient
  (`trilinear`, `:667`), on the GPU only; it states no bound.
- A constant can be any owned value, computed at build time by the program's own code on every
  core, within its `@fuel` (§13.1); a large one a program reads only at load is better shipped
  as a file (`std::io::Shipped`). A 3D texture is made and written with `std::gpu`'s texture API
  (`Texture3d`, `compiler/std/gpu.wrela:1023`; its constructors near `:1239`).
- An interval over a box that is one point is as wide as that evaluation's rounding (§14.2).

**The proof.** For a function `f` with two continuous derivatives on a cell with sides `h_i`,
the multilinear interpolant `If` of its corner values satisfies
`|f - If| <= Σ_i (h_i² / 8) · max|∂²f/∂x_i²|`. For a function that is only Lipschitz there, with
constant `L`, `|f - If| <= L · d / 2`, `d` the cell's diagonal (the interpolant is a weighted
mean of the corners, and the weighted mean distance to them is at most `d / 2`). Both are
standard; write the two-line argument for each in the std file's comment. A cell's bound is the
**smaller** of the two, with `max|∂²f/∂x_i²|` from the `interval` of the second derivative and
`L` from the `interval` of the gradient's components, **plus** what the stored numbers
themselves are off by: the width of `f`'s interval over each corner as a point, and
`8 · 2⁻²³ · max|corner|` for the interpolation's own arithmetic.

**Design.** A new std module, `compiler/std/table.wrela`. Nothing in the compiler changes
unless H.1 says the nesting needs help.

```text
/// A table of a function of three numbers over a box, with what reading it is proven to be
/// off by. `Table1` and `Table2` are the same for one and two numbers.
pub struct Table3 { size: [u32; 3], over: Box3, values: Vec<f32>, error: f32, worst: Box3 }

/// `f` over `over` as a table of at most `most` numbers, as fine as `within` needs: the
/// coarsest grid (each side doubling from 8) every cell of which is proven within `within`,
/// or the finest `most` allows.
pub fn table3(f: fn(vec3) -> f32, over: Box3, within: f32, most: u32) -> Table3

impl Table3 {
    pub fn error(self) -> f32          // proven: no read is off by more
    pub fn holds(self, within: f32) -> bool
    pub fn worst(self) -> Box3         // the cell with the largest bound
    pub fn read(self, p: vec3) -> f32  // on the CPU, the same interpolation
    @client pub fn upload(self) -> Tabled3
}

/// The table on the GPU: a texture's handle and where it lies. `Copy + GpuData`.
pub struct Tabled3 { texels: Texture3dHandle<R32Float>, lo: vec3, scale: vec3, error: f32 }
impl Tabled3 { pub fn read(self, p: vec3) -> f32 }   // eight loads and seven mixes
```

Outside `over`, `read` clamps to the edge (and says so in its doc). A function of a vector
result is a table for each component, for now.

Two things §14.2's heap rule decides here. `table3` takes `f` as a parameter and derives it
inside std (`interval` of its derivatives), so `f`'s own code is held to the heap rule alone;
the builder's `Vec` of values is std's, filled by code that is not derived. And a `Table`'s
`read` on the CPU reads that `Vec`, so **a function that reads a table can't itself be
derived** (E0700): say so in `read`'s doc, with what to do (derive the function the table was
made from). `Tabled3::read` on the GPU reads a texture, which GPU code derives through as it
does `Sampled3d`.

*A field kept as a table* (`std::field`):

```text
/// `field` over `region`, read from a table wherever a cell is proven within `within`, and
/// from `field`'s own code elsewhere. Cells farther from the surface than `near` need only
/// keep its sign and never overstate its distance.
pub fn baked<S: Surface>(field: S, region: Box3, near: f32, within: f32, most: u32) -> Baked<S>
impl Baked<S> { @client pub fn upload(self) -> BakedField<S> }
pub struct BakedField<S>: Copy + GpuData { exact: S, samples: Sampled3d, proven: Texture3dHandle<R8Uint>, .. }
```

`BakedField` is a `Surface` on the GPU: `distance(p)` loads the cell's flag; where it is set,
the table's value **less the cell's proven error** (so a step by it never passes the surface);
where it isn't, `self.exact.distance(p)`. Its `bound()` and `lipschitz()` are `exact`'s. On the
CPU its distance is `exact`'s.

**Steps.**

H.1 **The first question, measured before anything is built.** In a scratch test, for each of
the corpus's 25 fields and for `value_noise` and a six-octave `fbm` along a direction: the
`interval` over cells of 1/32, 1/64 and 1/128 of a 2 m box of (a) the gradient's components and
(b) the second derivatives' diagonal, three deep. Record: whether (b) derives at all (E0700,
E0701, a build past a minute); the time to build; and for each grid, the share of cells within
5 cm of the surface whose bound, from the proof above, is under 1 mm. Paste the table.
- If (b) doesn't derive or takes too long: build with the Lipschitz bound alone, and say so in
  the spec.
- **The stage's gate:** at 128 cells a side, at least half the fields have at least 70% of
  their near-surface cells proven within 1 mm. If not, the idea doesn't hold for fields: build
  `table1`, `table2`, `table3` only (H.2, H.3, H.5), leave `baked` out, and say so.

H.2 `Table1`, `Table2`, `Table3` on the CPU, with `@test`s. **G-sound:** for `sin(x)` over
[0, 6], `x * y` over the unit square, a ball's distance, and `fbm` at a point: at 100,000 random
points, `abs(read(p) - f(p)) <= error()`. One miss fails the stage: find the term the proof
left out.

H.3 `Tabled1..3` and `upload`. A GPU test (in `compiler/tests/tests/suite/renderer.rs`'s style):
a kernel reads the uploaded table at 4,096 points; each is within `error()` plus the CPU and
GPU's rounding of the CPU's `read`.

H.4 `baked` and `BakedField` (if H.1's gate passed). Tests: G-sound on the GPU at 100,000
points near the surface, against the field's own distance; a sphere trace of the baked field
and of the field itself gives depths within `within` plus a step.

H.5 **G-measure.** Two fixture programs under `compiler/tests`: one whose fragment shader calls
a costly function of a direction 16 times a pixel (so `wrela pipelines` lists it under `few`),
and its twin reading a `Tabled3`; one that marches a corpus creature, and its twin marching the
`BakedField`. For each pair: `wrela pipelines`'s work for the call before and after;
`wrela profile --compare --pairs 7` of the label; `wrela same --within` at the proven error in
8-bit steps. The stage **pays** if either pair is at least 1.5 times faster on the M4 and both
are within their bound. If neither pays, the module stays out of std's public surface (move it
under `compiler/tests`), with the numbers in the commit.

H.6 Spec: §18.13's last paragraph names `std::table` as how such a call is answered; a new
§14.6 "Tables with a proven error" gives the proof and what `error()` promises; §20.2's module
map; §20.3 (`baked`). `wrela pipelines`'s text under `few` adds one line: "`std::table` builds
one with a proven error".

**Checks.** `tools/check.sh test_items::std_tests_pass`, `tools/check.sh renderer::`,
`tools/check.sh language::`.

**Done when:** H.1's table and H.5's reports are in the pull request, and G-sound held at every
point.

## Stage I: certified twins

*Hypothesis.* **Goal.** `wrela twin` writes, beside a function GPU code calls, a cheaper twin
whose results are **proven** within a tolerance of the original's over a box, as source an
agent reads and a later run can check.

```text
wrela twin <package> <module>::<fn> --box <lo>..<hi>,... --within <eps> [--name <twin>]
           [--half] [--write | --check] [--json]
```

**Second user.** Presentation code anywhere: a shader's lighting term, a kernel's falloff.
`wrela precision` already advises half-float twins that the engine writes by hand and measures.

**Why GPU code only.** CPU code is strict, so no rewrite that changes a bit is allowed there
(§11.1; `docs/citations.md`, the equality-saturation row). GPU results are for presentation
only (§11.3), and a pixel is 1/255 wide.

**Facts.**

- `wrela precision <package> <fn> --box ..` already walks a function's interval over boxes and
  reports each operation by its place in the source (`compiler/cli/src/precision.rs`; the notes
  come from `interval.rs`'s `note`, `:548`, which reads the place from the latest `Stmt::At`).
  Its verdicts are advice: 0.7.0's council refused intervals derived in `f16` (S2).
- Work is counted from the IR: `ir::work::builtin_cost` (`compiler/ir/src/work.rs:777`),
  `entry_work` (`:87`).
- Stage B's interpreter walks a flattened GPU module, and `wrela_driver::gpu_modules` gives one
  for a pipeline. A kernel that only writes `f(x)` flattens to `f`'s body, inlined, with its
  `Stmt::At` marks.
- `wrela refactor extract-const <file>:<line>:<col>` finds the expression at a place in the
  syntax tree and edits the text there (`compiler/driver/src/refactor.rs` and `refactor/`);
  `wrela fix` and `wrela edit` refuse to write a file whose hash changed.

**Design.**

*The error walk* is a second way to run stage B's interpreter: `compiler/ir/src/interp/error.rs`.
Where the concrete run holds a number, this one holds, for a box of inputs, **a range and an
error**: `(lo, hi, err)`, meaning "over the box the original's value lies in `[lo, hi]`, and the
twin's value is within `err` of the original's at the same input". Arithmetic is in `f64` with
each bound rounded outward (add `2⁻⁴⁰` of its size). The rules, with `|a|` the largest size in
`a`'s range and `⌊a⌋` the least:

| Operation | Range | Error |
|---|---|---|
| `a + b`, `a - b` | interval | `err_a + err_b` |
| `a * b` | interval | `|a| err_b + |b| err_a + err_a err_b` |
| `a / b` | interval, refused if `b`'s range holds 0 | `(err_a + |a / b| err_b) / (⌊b⌋ - err_b)`, refused unless `⌊b⌋ > err_b` |
| `-a`, `abs`, `min`, `max`, `clamp`, `saturate` | interval | the largest operand's (each is 1-Lipschitz) |
| `sin`, `cos`, `atan`, `tanh` | interval | `err_a` |
| `exp` | interval | `exp(hi_a + err_a) · err_a` |
| `log` | interval | `err_a / (lo_a - err_a)`, refused unless `lo_a > err_a` |
| `sqrt` | interval | `err_a / (2 sqrt(lo_a - err_a))`, refused unless `lo_a > err_a` |
| `pow(a, c)`, `c` a constant | interval | `|c| · |a|^(c-1) · err_a`, over `a`'s range widened by `err_a` |
| `mix(a, b, t)` | interval | `err_a + err_b + |b - a| err_t` |
| `smoothstep(e0, e1, x)`, edges exact | interval | `1.5 err_x / ⌊e1 - e0⌋` |
| `length`, `distance` | interval | the sum of the components' errors |
| `dot`, `cross`, `normalize` | interval | by the rules above, a component at a time (`normalize` as `v / length(v)`) |
| `floor`, `ceil`, `round`, `trunc`, `fract`, `step`, `sign`, a comparison, a conversion to an integer, an index | interval | fine if every operand's `err` is 0; else **refused**: a changed value would decide a jump |
| A branch or `select` whose condition the range doesn't settle | both sides, joined (the hull; the larger `err`) | |
| A loop of a count the ranges settle | each pass in turn | |
| Anything else | | refused, naming it |

An integer is exact when its range is one value, as in the interval derivation. A **site** is a
value the twin computes differently: there the walk adds the site's own bound `δ` to `err`.
Every rule above is a line of Lipschitz reasoning; write each one's line in the file. A
**refusal** is not an error of the tool: it means no proof, and the rewrite is not made.

*Soundness is tested, not assumed* (step I.2): for random functions and random sites, the walk's
bound is never beaten by a sampled difference.

*The rewrites* this stage knows, each a source edit and a `δ`:

| Rewrite | The edit | `δ` over the box |
|---|---|---|
| Drop a term | `a + t` or `a - t` becomes `a` | `|t|`, from the walk's range of `t` |
| A power as products | `pow(x, 2.0)`, `3.0`, `4.0` become `x * x`, ... | `4 · 2⁻²³ · |result|` |
| A reciprocal | `x / c`, `c` a constant, becomes `x * (1 / c)` | `2⁻²³ · |result|` |
| A polynomial | `g(a)` for `g` in `sin cos exp log atan tanh` or `pow(a, c)`, where `a`'s range is narrow enough, becomes a polynomial of degree at most 5 in Horner's form (`std::math::poly(a, [c0, ..])`, a new small std function) | proven in one dimension: fit at Chebyshev's points of the range; over 4,096 pieces of the range, `|p - g| <= |p(m) - g(m)| + (max|p'| + max|g'|) · h / 2` at each piece's middle `m`, the derivatives' bounds from the same interval rules |
| A half float (`--half`) | a local's type becomes `f16`, where `wrela precision` says `safe` | **none**: it is advice (S2) |

*The search.* Build the function as the body of a one-line kernel (glue, as `precision.rs`'s);
take its flattened module from `gpu_modules`; walk it over the box once to get every
operation's range; list the candidate rewrites with the work each saves (`builtin_cost` and a
dropped term's own work); take them greedily, most work saved first, re-walking with every
chosen site's `δ` each time, and keep a candidate only while the result's `err` stays within
`--within`. The box is cut in eight and walked again (to depth 3) where a walk is refused or its
`err` is over the tolerance, and the largest `err` over the pieces is the bound.

*The twin is source.* Each chosen rewrite is an edit of the original function's text, found by
the candidate's place (`Stmt::At`'s span and the operation), through the syntax tree as
`extract-const` finds its expression. The command prints, or with `--write` adds after the
original, a function `<fn>_twin` (or `--name`) with a doc comment that is its certificate:

```text
/// `falloff`'s twin, proven within 0.002 of it for d in 0..40, k in 0.5..2 (`wrela twin`):
/// drops `0.0003 * sin(8.0 * d)` (at most 0.0003); `pow(x, 2.0)` as a product.
/// Work 61, from 148. Check with `wrela twin <pkg> shade::falloff --check`.
```

`--check` derives the twin again from the original with the box and tolerance in that comment
and compares it with the twin in the file, both formatted: it exits 1 where they differ (the
original changed, or the twin was edited), so a stale twin is caught as a stale golden is. With
`--half`, the comment says `advised, not proven` for those edits, and names G-precision (a
measured comparison) as their proof.

**Steps.**

I.1 The error walk, with unit tests of each rule against sampled perturbations.

I.2 **G-sound.** Stage A's generator makes tame programs; for each, pick up to three sites at
random with a random `δ`; the walk's bound `E` over a box; then at 256 points in the box, with
each site's value moved by a random amount within its `δ` (the interpreter's observer lets a
test overwrite a value), `abs(moved - original) <= E`. 2,000 programs in a soak, 32 in the
gate, as oracle **O9**. One miss fails the stage until the rule is fixed.

I.3 The rewrites and their `δ`s; `std::math::poly` with a `@test`; the polynomial's proof as a
Rust unit test against 10⁶ samples of each function over three ranges.

I.4 The search, the source edits, the command, `--write`, `--check`. CLI tests in
`compiler/cli/tests/suite/twin.rs`: a function with a term proven tiny loses it; one whose term
is not tiny keeps it; a twin written, then the original edited, fails `--check`; the twin
builds and, run on the CPU at 10,000 points, is within the tolerance.

I.5 **G-measure, the stage's gate.** On three functions taken from the frozen programs' GPU
code (pick the three with the most work that take only numbers and vectors; name them in the
commit) and on the fixture of H.5: the work before and after, and `wrela profile --compare
--pairs 7` of a kernel that calls each 10⁶ times. The stage **pays** if at least one real
function's twin is 1.2 times faster with a tolerance of 0.002. If none is, the command ships
marked experimental in its usage line, with the table in the commit: the certificate machinery
(I.1, I.2) stays, since stage K and later work use it.

I.6 Its row in §23.9, a paragraph in §14.3 beside `wrela precision` (what a twin is, what
`proven` covers: real arithmetic over the box, each target's rounding as §14.2 says), usage and
doc comment.

**Checks.** `cargo test -p wrela-ir interp`, `tools/check.sh typed::`, `cargo test --release -p
wrela --test suite -- twin::`.

**Done when:** O9 held over its soak; I.5's table is in the pull request.

## Stage J: tape stand-ins

*Hypothesis.* **Goal.** While a field's own pipeline compiles, its stand-in draws the same field
through the tape interpreter, so the first frames show the real shape, not a flat colour.

**Second user.** Any pipeline generic over a `Surface` (a cook, a cull, a march).

**Facts.**

- Stand-ins are §18.12 (0.26): `@standin(name)` on a kernel or a fragment shader names another
  of the same stage that takes parameters of **the same types, modes and kinds**; the compiler
  builds its pipeline beside the real one with the same bindings and uniform block
  (`add_standins`, `compiler/lower/src/gpu.rs:667`; `match_standin`, `:747`); the manifest names
  it (`standin`, `runtime/abi/src/manifest.rs:204`); the browser builds stand-ins first and runs
  a command on its stand-in until its own pipeline is built (`runtime/browser/src/gpu.ts`, the
  `standin` handling from `:375`); the native host builds everything first and runs none. Test
  mode waits for the real pipeline unless `standins=1`, and `starts.json` lists the frames that
  ran on one (`stood_in`). Tests: `compiler/tests/tests/suite/standin.rs`.
- Any compiled field has a tape: `Surface::to_tape()` (`compiler/std/field.wrela:76`), agreeing
  with the field within about 1e-6 (§14.4). A `Tape` is 72 KiB (`compiler/std/stage.wrela:275`),
  over a uniform's 64 KiB, so as a by-value parameter it binds as read-only storage
  (`bind_uniform`, `gpu.rs:2784`), and **its bytes travel in every command that binds it**
  (a draw's uniforms are inline in the stream).
- `interpret(tape, p)` (`stage.wrela:283`) takes the tape by value. A tape's interpretation
  costs about 0.32 ns an operation a point on the M4 (§14.4).
- The opening scene's 21 pipelines took 22.6 s cold on SwiftShader before 0.27's rewrites and
  under 4 s after (§18.11); capybara's first frame was 2.71 s cold against 0.30 s warm (0.12.0).

**Design.** No new rule in the language. The real entry point takes one more parameter it
doesn't use, a buffer holding the field's tape, and its stand-in reads it:

```text
@fragment
@standin(shade_taped)
fn shade<F: Surface>(pixel: FragCoord, camera: Camera, field: F, _tapes: [Tape]) -> vec4 { .. }

/// The same picture through the field's tape, until `shade` is built.
@fragment
fn shade_taped<F: Surface>(pixel: FragCoord, camera: Camera, _field: F, tapes: [Tape]) -> vec4 { .. }
```

The parameters match, so §18.12 holds as it is. The tape is in a buffer, so a command carries
its handle, not its 72 KiB. What std must give is a way to interpret a tape **where it lies**:

```text
/// `interpret` of the tape at `tapes[i]`, read in place.
pub fn interpret_at(tapes: [Tape], i: u32, p: vec3) -> f32      // std::stage
```

**Steps.**

J.1 Does `interpret(tapes[0], p)` already read the buffer in place? Build a kernel that does
that and read its WGSL: a copy of the tape into a function's variable (72 KiB a call) is what
`flatten_gpu`'s forwarding exists to remove for a uniform block (`compiler/ir/src/opt.rs:29`'s
module comment; `read_only`, `:2427`). If the WGSL holds no such copy, `interpret_at` is
one line of std over `interpret`. If it does, extend the forwarding to an element of a
read-only buffer at an index that doesn't change, and add an `ir` unit test beside
`calls_given_the_same_uniform_data_share_a_specialized_copy`.

J.2 `interpret_at`, with a GPU test: over the 25 corpus fields, `interpret_at` on a buffer of
their tapes agrees with `interpret` of each by value, bit for bit.

J.3 The fixture `compiler/tests/standin_tape`: `shade` and `shade_taped` as above over a corpus
creature, marched the same way but for where the distance comes from. Tests in `standin.rs`:
the manifest names the stand-in; with the native host forced to the stand-in (give the
executor a test switch, as the suite's other stand-in tests reach it) its frame is within 2/255
mean of the real pipeline's; in Chrome with `standins=1`, `starts.json` shows frame 0 in
`stood_in` and a later frame not.

J.4 **G-cold, the stage's gate.** In Chrome, cold, three runs each: the fixture made heavy
(eight creatures, each its own instantiation, so eight slow pipelines), (a) with no stand-in,
(b) with a flat-colour stand-in, (c) with the tape stand-in. `first_submitted_ms` for each, and
the first frame that is the real pipeline's. The stage **pays** if (c)'s first frame is within
1.5 times (b)'s and shows the field (its pixels within 4/255 mean of the real frame's). Also
record the eight stand-ins' own build times: if identical stand-in shaders are compiled eight
times over, make the browser host reuse one shader module for identical WGSL texts, and
measure again.

J.5 Spec: §18.12 gains a paragraph and this example ("a stand-in for a field can draw the
field: through its tape"), §14.4 a line on `interpret_at`. If J.4 doesn't pay, the paragraph
says what was measured and the fixture stays as a test.

**Checks.** `tools/check.sh standin::`, `tools/check.sh stage::`, `tools/check.sh browser`.

**Done when:** J.4's nine numbers are in the pull request.

## Stage K: exact kernels

*Hypothesis.* **Goal.** A kernel whose results are the same bits on every GPU and on the CPU,
so the simulation may use them: `@deterministic` on a `@compute` kernel. Its CPU form is the
definition; a GPU's answer, when one has arrived, is only a faster way to the same bits.

**Second user.** Any lockstep program with a large grid or crowd: a cellular automaton, a flow
field, a visibility grid; and its server, which has no GPU and runs the same kernel.

**Facts.**

- GPU results can't reach `@deterministic` code: a readback is `nondet` (`Lang::is_nondet`,
  `compiler/sema/src/defs.rs:255`, holds `ReadBufferCommand`), and `DETERMINISTIC` forbids `io`,
  `nondet` and `host` (`compiler/sema/src/effects.rs:547`). A kernel can't be called (E0606,
  `compiler/sema/src/check/call.rs:162`).
- WGSL defines integer arithmetic exactly: it wraps; a division by zero, `MIN / -1` and a shift
  by the width or more have defined results (§11.3's table, "WGSL-defined values"). Floats
  differ between GPUs. An index out of range is "a value from inside the buffer, or zero":
  **not** the same everywhere.
- On the CPU, `Add`, `Sub`, `Mul` trap on overflow and `WrappingAdd`, `WrappingSub`,
  `WrappingMul` don't (`ir::BinOp`, `compiler/ir/src/lib.rs:102`).
- A kernel's parameters are classed in `class_of` (`compiler/lower/src/gpu.rs:249`): `[T]` is a
  read-only buffer, `Slots<T>` one each invocation writes its own place of, a `GpuData` value
  part of the uniform block, `GlobalId` a builtin. Atomics, `Shared` (workgroup memory),
  `Append` and subgroup operations make results depend on how invocations interleave or group.
- A kernel is checked by sema (`check_entry`, `compiler/sema/src/gpu.rs:531`) and lowered by
  `lower_compute` (`gpu.rs:3008`); the bound-kernel trait is `Kernel` (§18.7).
- A tick's step is `@deterministic` and runs on the ticker's thread, which records no GPU work;
  frames run beside it. A value goes from frames to ticks only as input (`std::tick::post`),
  which a log keeps. Stage B has an interpreter of GPU code in the toolchain, not in a program.

**Design.**

*The class.* `@deterministic` on a `@compute` kernel (S1). Such a kernel must have the shape
`fn k(input: [A], params: P, out: mut Slots<B>, id: GlobalId)`: one read-only buffer, one
`GpuData` value, one output it writes at its own place, its place. Checked when it is lowered,
through every function it reaches, **E0713** otherwise, naming what and where:

- no float arithmetic, float comparison, float builtin, or conversion between floats and
  integers (a float may be loaded, stored and bit-cast: its bits are data);
- no atomic, workgroup memory, `Append`, subgroup operation, texture, barrier or derivative;
- no other parameter kinds.

The walk is a new pass over the kernel's module, shaped as `collectives_in_non_uniform_flow`
is (`compiler/ir/src/uniformity.rs:55`): each expression's scalar type (`Scalar::is_float`,
`compiler/ir/src/types.rs:24`) and kind.

*Two things the compiler does to make both forms agree:* every index into a buffer or an array
whose range it can't prove is **clamped** to the last element, in the WGSL and in the CPU form
alike; and the CPU form's integer arithmetic is WGSL's (`Add` as `WrappingAdd`, and so on; a
division or remainder guarded to give WGSL's result for a zero divisor and for `MIN / -1`; a
shift's amount masked to the width).

*The CPU form.* For a deterministic kernel the compiler gives the method `k.run(input, params,
mut out)`: a loop over `out`'s places, the kernel's body run for each with its `GlobalId`. It
is ordinary CPU code, lowered from the same body with the rules above, callable from
`@deterministic` code, on any role. That alone is "one source, the same bits, on a server
too".

*The fast path.* `run` first asks the host whether an answer for exactly these inputs is
already there:

1. `key` = a 64-bit hash of the kernel's pipeline name, `params`' bytes and `input`'s bytes
   (std's `Hasher`, `compiler/std/hash.wrela`).
2. An intrinsic `exact_take(key, out) -> bool` copies a held answer of the right length into
   `out`. The compiler lets `@deterministic` code call it, and only through `run`: what `run`
   returns doesn't depend on whether it found one.
3. If none: the loop.

On the client's frames, `std::gpu::exact::Exact<K>` (`K` a deterministic kernel) keeps a small
pool of buffers and: `ahead(input, params)` uploads them, dispatches the kernel and reads `out`
back; `poll()` hands a finished answer to the host under its key (`exact::offer(key, bytes)`,
`@client`). The frames know what the next tick will ask because they read the simulation's
state (a hand-off, §16.2) and compute the same inputs from it. The hosts keep the last 16
answers, by key, where the ticker's thread can read them: a table in `runtime/native` and in
`runtime/browser`, behind two host services. An answer that isn't there in time costs only the
loop.

*Checking the hints* (S7): a debug build runs the loop every time and panics if a held answer
differs, naming the kernel. A release build does so for one `run` in 64; on a difference it
logs once, tells the host to drop every answer, and takes no more for the run.

A tick log holds no answers: a replay takes them if a GPU offers them, or runs the loop, to
the same hashes.

**Steps.**

K.1 E0713 and its walk, with its explain file, conformance cases (`gpu_exact_accept.wrela`,
`gpu_exact_reject.wrela`, rule `gpu.exact`, added to `RULES` in
`compiler/driver/src/primer.rs:14` and to the spec's rules table with the same words), and
diagnostics cases for a float add, an atomic and a second buffer.

K.2 The clamps and the CPU form: `k.run`. Tests: a Life step on a 64 × 64 grid, its `run`
equal to a hand-written CPU version; a kernel that indexes past its input gets the clamped
element in both forms.

K.3 **G-agree, the evidence.** For the Life kernel, a hash-mixing kernel (multiplies, shifts,
xors, a division by a value that is sometimes zero) and a kernel with an index out of range:
10,000 random inputs each; the GPU's output equals `run`'s, bit for bit. On the M4, and under
`WRELA_ALLOW_SOFTWARE=1`; in Chrome too (a page test). Add a typed-program oracle, **O10**:
stage A's generator with an integer mode (u32 and i32 arithmetic, shifts, comparisons, table
reads), each program as an exact kernel, GPU against `run`. One difference fails the stage
until it is understood: either a compiler bug, or a thing WGSL doesn't define after all, which
then joins E0713's list.

K.4 `exact_take`, `exact::offer`, the hosts' tables and services (native and browser), with
`run` using them. Tests: a ticked program whose step runs the Life kernel; its tick log's
hashes are the same with offers and without (`wrela-host --compare-ticks`); a replay with no
GPU gives the same hashes.

K.5 `Exact<K>` in std, and the checks of S7. Test: a host made to hold a wrong answer (a test
switch) makes a debug build panic naming the kernel, and a release build log and stop taking
answers.

K.6 **G-measure, the stage's gate.** A 1,024 × 1,024 Life at 20 ticks a second beside 60
frames: the tick's time with answers and without (`std::time::phase` round the step, `wrela
profile`), and the share of ticks whose answer was there in time. The stage **pays** if the
tick is at least 3 times faster with answers on the M4 and at least 90% arrive in time. If it
doesn't, `run` (the CPU form) and E0713 ship, since "the same code on the server" stands on its
own, and `Exact` stays out of std's public surface.

K.7 Spec: §12 (the one way a GPU result reaches the simulation, and why it changes no bit),
§18.2 and a new §18.14 "Kernels the same on every GPU", §11.3's table (what a deterministic
kernel does at an index out of range), §20's closed list.

**Checks.** `tools/check.sh conformance::`, `tools/check.sh explain::`, `tools/check.sh
typed::`, `tools/check.sh keys::` (the ticker's tests), `tools/check.sh browser`.

**Done when:** K.3's counts (inputs, adapters, differences: none) and K.6's numbers are in the
pull request.

## Stage L: CPU loops

*Hypothesis, step by step.* **Goal.** The loops programs actually write run four at a time: a
loop over a `Vec`, a loop with a branch in it; and where a struct's fields are walked one at a
time, a container that keeps them apart.

**Second user.** Every CPU loop over floats in std, the engine and games.

**Facts.**

- A loop runs four passes at a time only when `vloop::plan` accepts it
  (`compiler/wasm/src/func/vloop.rs:72`; called from `func.rs:1267` when SIMD is on and the
  build isn't a debug one). The scalar loop is always emitted after it and takes what's left.
- `plan` needs a `u32` counter stepped by one against a bound from before the loop; a body of
  only `Let`s and `Store`s (`check`, `vloop.rs:218`: any other statement, an `If` among them,
  ends in `_ => return None`); `f32` arithmetic, comparisons and `select`s (`expr`, `:279`);
  elements read and written only at the counter's index of a run or an array that is a local or
  a by-reference parameter (`element_place`, `:201`).
- **A loop over a `Vec` is never taken.** A `Vec` is `{ data, len, cap }`
  (`compiler/std/collections.wrela:33`), and indexing one lowers to a pointer computed from
  `data` (`vec_elem`, `compiler/lower/src/mem.rs:182`), behind a bounds check that is an `If`
  and a panic in the IR (`check_below`, `:138`). `element_place` refuses the pointer and `check`
  refuses the `If`.
- Integer `+ - *` are checked in the back end (`scalar_bin_known`,
  `compiler/wasm/src/func/ops.rs:188`); the wrapping forms aren't. The back end knows only "a
  value below a constant, inside an `if` that tested it" (`learn`, `known_below`,
  `func.rs:1158`, `:1182`).
- GPU code turns branches into selects (`if_convert`, `compiler/ir/src/opt.rs:83`) only where
  both sides are safe to run (`select_safe`, `:396`); on the CPU a side not taken could trap, so
  it isn't done there.
- "Layout is the compiler's choice" (§6.10), and today every container stores its elements one
  after another; a struct's fields are packed in order (`compiler/ir/src/layout.rs:126`).
- The measuring tool is `wrela profile --compare --pairs n` (§0.5); CPU numbers that count are
  Chrome's (`--chrome`), as the earlier plans held ("New codegen passes without a V8
  measurement" were left out for want of one).

**Design and steps.** Each step is its own hypothesis with its own gate; do them in order, and
stop a step that doesn't pay without stopping the stage.

L.0 **The bench.** `compiler/tests/loops`: a package of kernels, each an export that runs one
loop over 2²⁰ floats 100 times and returns a checksum: a run loop (taken today, the control);
the same over a `Vec`; a `Vec` loop with a branch (`if x[i] > 0.0 { y[i] = a } else { y[i] =
b }`); a `Vec<Particle>` loop that reads two of its eight fields; a loop with an integer index
sum. A Rust test, `loops::the_kernels_agree`, checks each checksum against a build with
`no_simd`. The measure for every step below: this package under `wrela profile --compare
--pairs 7 --chrome` (each kernel in a `std::time::phase`), and the frozen herd's CPU frame
(`compiler/tests/frozen/herd`) the same way.

L.1 **Range proofs** (`compiler/ir/src/ranges.rs`, run in `inline_cpu` after inlining; Bodík,
Gupta and Sarkar's question, answered for the shapes below only).
- The shape: a loop whose body begins with the counter's load, `i >= bound`, and a `break`, and
  whose `continuing` adds one (what `vloop::plan` looks for), with no other store to the
  counter. Inside its body, `i < bound`.
- A check `if i >= n { panic }` (what `check_below` makes) is removed when `n` is `bound`'s own
  value, or both are loads of one place (a `Vec`'s `len`) that nothing in the loop can write:
  no store through it, and no call given the `Vec` to change.
- The counter's `+ 1` becomes `WrappingAdd` (it is below `bound`, so it can't overflow).
- Nothing else. Each removal is counted, and `WRELA_DUMP_IR=cpu` shows the result.
- **Soundness:** stage A's generator gains a **loop mode**: programs with `Vec` loops and
  integer sums, checked by O1 and O5 only (a function that reads a `Vec` can't be derived,
  §14.2, so O2 to O4 and O8 never see this mode). Oracle **O5** now also requires that a
  program traps in the optimized build exactly when it does in the `no_opt` one, at the same
  pass. 5,000 seeds of soak.
- **Gate:** the `Vec` loop's kernel is not slower, and the count of checks removed in the
  frozen herd is in the commit. (This step mostly makes L.2 possible.)

L.2 **A `Vec`'s elements, four at a time.** `element_place` also accepts the pointer
`vec_elem` makes (`data + i * 4`, `data` loaded before the loop and the `Vec` not changed in
it), once L.1 has removed its check. **Gate:** the `Vec` kernel at least 2 times faster in
Chrome; bits equal (`loops::the_kernels_agree`, and `simd::`'s tests); the herd not slower.

L.3 **Branches as masks** (Pharr and Mark). In `vloop::plan`, when `check` fails only on `If`s:
try the body with each `If` flattened, **for the four-at-a-time loop only** (the scalar loop
keeps its branches). An `If` flattens when its condition is a comparison of `f32`s the planner
accepts, and each side holds only `Let`s the planner accepts and stores to the loop's
temporaries or to elements at the counter's index. A store `p = v` under condition `c` becomes
`p = select(c, v, load p)`. Both sides then run for every lane: they are float arithmetic,
which doesn't trap in a release build, and loads at the counter's index, which the loop's own
test covers. A debug build doesn't vectorize at all, so its NaN checks are untouched.
**Gate:** the branchy kernel at least 2 times faster in Chrome; bits equal; the herd not
slower.

L.4 **Where columns would pay.** `wrela build <package> --report layout`: for each loop over a
`Vec` or an `Arena` of a struct, the struct's size, the bytes of it the loop reads and writes,
and whether elements are ever taken whole as a place in that loop. Sorted by bytes skipped a
pass. Run it on the frozen programs and, through `WRELA_TOOLCHAIN`, on `rywible/wrela`'s
engine; paste the top ten of each.

L.5 **`Columns<T>`** (S3), only if L.4 shows at least three loops in the engine that skip
three quarters of a struct of 32 bytes or more. `std::collections::Columns<T: Copy>`: a lang
item whose storage is each field of `T` in its own array, back to back in one allocation.
`push`, `len`, `clear`; `c[i]` read whole gathers a `T`; `c[i].x` reads and writes one field in
its array; `for i in 0..c.len()`. An element is **not one place**: `mut e = c[i]`, or lending
`c[i]` to a `mut` parameter, is **E0339**, whose help says to read it, or to name a field. Its
field loops are then runs of floats at the counter's index, which L.2 takes. **Gate:** the
`Particle` kernel rewritten over `Columns` at least 1.5 times faster in Chrome than over `Vec`.
If L.4's count isn't met, or the gate fails, leave `Columns` out and keep the report.

L.6 Spec: §11.2 (what a loop needs to run four at a time, now: `Vec`s and branches), §6.10
(`Columns`, if it shipped; else that the report exists), §23.9's `wrela build` notes.

**Checks.** `tools/check.sh loops::`, `tools/check.sh simd::`, `tools/check.sh typed::`,
`cargo test -p wrela-wasm`, `cargo test -p wrela-ir`.

**Done when:** each step's numbers, kept or not, are in the pull request.

## Stage M: the agent test, version 3

**Goal.** A benchmark the new tools can move. Version 2 is at its ceiling on correctness, so
it can't show whether `wrela pixel` or `wrela inspect` helps. Version 3's tasks are judged on
what was made and how it runs, and each is run **with and without** the tools this plan adds.

**Facts.**

- Version 2: `tools/agent-test/v2/{run.py, grading.py, score.py, tasks.json, grades/,
  reference/}`; `run.py` launches one `claude -p` a task with an allowlist of tools
  (`claude_command`, `run.py:135`); its results' README asks for harder tasks and a weaker
  model. Its unit tests are `tools/tests/test_agent_test_v2.py`. A run costs money
  (`docs/release.md` §2 step 5).
- Grading runs hidden `@test`s copied into the agent's package (`grading.py`), or checks
  `wrela pipelines --json`.

**Design.** `tools/agent-test/v3/`, version 2's shape with three changes: tasks are packages
to change, not single files; grades are commands this plan adds; and `run.py --tools
with|without` sets the allowlist (`without` is version 2's; `with` adds `wrela pixel`,
`inspect`, `sliders`, `fuzz`, `twin`, `solve`, `test`). Twenty tasks, each with a `right` and a
`wrong` reference as in version 2:

| Kind | Tasks | Graded by |
|---|---|---|
| Make a shape | Five briefs (a stool with four legs, a mug, a quadruped from proportions, ...) each with a hidden spec and blueprint | The hidden spec's checks hold; `wrela inspect` says one piece, no `floating`, no `dead-part`; `outline_miss` under 0.1 |
| Repair a shape | Four packages with a planted fault (an ear off the head, a part swallowed whole, a literal that does nothing, a neck thinner than a cell) | `wrela inspect` no longer reports it, and the hidden spec still holds |
| Find the pixel | Four programs whose picture is wrong at some pixels (a NaN from a zero length, a branch on the wrong sign, a texture read at the wrong level, a missing clamp) | A hidden frame test's golden, `within` 1 |
| Stop the trap | Three ticked programs with an input that traps | `wrela fuzz --runs 100000 --seed 7` finds nothing, and hidden tick tests pass |
| Make it cheaper | Four programs with a costly GPU function | `wrela pipelines`'s work for the named pipeline under the task's number, and `wrela same --within 1` against the original |

Work, not time, grades the last kind: it is the same on every machine.

**Steps.**

M.1 The tasks, grades and both references, and `tools/tests/test_agent_test_v3.py` with the
two tests that keep them honest (`test_every_right_reference_passes_its_grade`,
`test_every_wrong_reference_fails_its_grade`), run through `tools/check.sh tools`.

M.2 `run.py` and `score.py`: version 2's, with `--tools`, and a report of both arms side by
side: correct, attempts, tokens, cost, and which tools each task's agent called.

M.3 `run.py --dry-run` prints the commands of both arms. **Stop here and ask Ryan** (S8), with
the arithmetic: 20 tasks, two arms, at most the budget a task.

M.4 With his go-ahead: a three-task smoke run, then both arms. The scores go in
`v3/results/` as version 2's did, and the headline (correct with and without, tokens with and
without) in the pull request.

**Checks.** `tools/check.sh tools`.

**Done when:** both reference tests pass and M.3's dry run is in the pull request; M.4 if Ryan
said go.

## Stage N: the spec, the docs, the release

N.1 Read the whole diff of `spec/language.md` against `main`. Every stage's spec edits are
there, in the spec's voice (each section opens with one sentence that says what it is about).
The contents table lists the new sections. `wrela primer` covers the new rule (`gpu.exact`).
Every new `wrela` block compiles (`tools/check.sh language::`).

N.2 §0.10's table against the code: every command is in `usage()`, in `main.rs`'s doc comment
and in §23.9's table; every std item has a doc comment and a page (`wrela doc <item>`
answers); every code has its explain file; every lang item is in §20's closed list. A stage
whose hypothesis failed has its names struck from the table, with a line saying where its
measurement is.

N.3 `docs/citations.md`: a row for each source in §0.10 whose stage landed code, each
reference checked against the source itself (author, title, venue, year). The research table
gains what stages H, I and L read. No crate was added; say so if that changed.

N.4 `CHANGELOG.md`: a `## 0.34.0` entry, in the file's voice: each thing a program or a tool
can now see, one bullet, with its spec section. Hypotheses that failed are not in it.

N.5 `tools/check.sh` with no arguments, on the M4 and on the Linux host. Paste the counts. Then
the soaks one more time at their stage's sizes, with the seeds.

N.6 `docs/release.md` §2 step 5: the agent test, version 2, once, with Ryan's go-ahead (it
costs money); its headline numbers in the pull request.

N.7 The pull request, titled `Release 0.34.0: wrela pixel, inspect, sliders and fuzz, reverse
gradients, outlines as measures, and what the measurements kept of tables, twins, tape
stand-ins, exact kernels and faster loops`. Its description, in this order:

1. The stages in order, one line each: what is now true.
2. **Measured:** every gate's numbers, with the machine and the command, kept or not. A
   hypothesis that failed is said plainly, with its number.
3. **Found by the typed fuzzer:** each bug, fixed or not.
4. **For Ryan:** §0.8's decisions as built; anything left undone and why; the engine-side
   follow-ups this makes possible in `rywible/wrela` (a tape stand-in for its marches; `baked`
   for its cooked fields; `Exact` for a crowd; `wrela inspect` and `sliders` in its authoring
   briefs), none of which this pull request does.

Ryan merges, then tags per `docs/release.md` §3.

**Done when:** N.1 to N.5 are true and the pull request is open.
