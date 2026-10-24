# Plan: the agent loop (release 0.5.0)

One scope of work, on one branch, in one pull request (Ryan asked for all of it together,
knowing it is large). Nine stages, each ending in a commit that leaves the checks green. The
stages are ordered so that each one's machinery is in place before a later one uses it.

Why: codegen is done for now (the October research found no structural pass that real code
needs). The compiler's product for an agent-only language is the loop from intent to a correct
program. Each stage shortens that loop in a way the compiler alone can.

## 0. How to work this plan

Read first: `AGENTS.md`, `docs/release.md`, `docs/citations.md`, and `spec/language.md` §10.2,
§13.2, §14, §23.9.

Rules of the repository that apply to every stage:

- **Run as few tests as possible to validate a change.** Each stage names its checks. Run those,
  not the whole suite, until the stage's last step says to run `tools/check.sh`.
- **Cite your sources.** Nothing here adds a crate. If you do add one, add its row to
  `docs/citations.md` in the same commit.
- **The spec and the tests must agree.** Where this plan changes what a program can see, the
  stage names the spec section to edit. A `wrela` block in the spec is compiled by a test
  (`language::every_example_in_language_md_compiles`), so a new example must compile.

How to work:

1. `git switch main && git pull --ff-only origin main && git switch -c agent-loop`.
2. Do the stages in order. Commit at the end of each stage with a one-line message in the
   repository's style (a sentence saying what is now true, no trailing period, for example
   `Every error carries a help or a fix, and near-miss names are suggested`), ending with
   `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
3. Before a stage's commit, run `tools/check.sh clippy` (Clippy over the workspace with
   warnings as errors) and `tools/check.sh rustfmt`.
4. The checks are run by name: `tools/check.sh <name>` runs exactly one test (`explain::every_code_is_explained`),
   and a name ending in `::` runs a group (`tools/check.sh warnings::`). The raw form is
   `cargo test --release -p wrela-tests --test suite -- --exact <module>::<test>`; the CLI's own
   tests are `cargo test --release -p wrela --test suite -- <module>::`.
5. Diagnostic text follows the house style: lowercase, no trailing period, code in backticks,
   the shortest sentence that says what to do. Copy the neighbouring messages' tone.
6. A test file's goldens are re-written with `WRELA_BLESS=1 tools/check.sh <test>`. Before you
   bless, read `git diff` of the goldens and make sure only the keys you meant to change changed.
7. Facts in this plan are given as `path:line`; lines drift. Grep for the identifier if a line
   is off.

Never:

- Work around a gap in another language (a Python script where a `wrela` tool or a Rust test
  belongs) except in `tools/agent-test`, which is Python already.
- Change what an existing code means. Codes are stable forever (§10.2); a retired code goes in
  `RETIRED` (`compiler/diag/src/codes.rs:54`).
- Edit `tools/agent-test/results/m1/` or `results/m1.json`.
- Skip a failing test or mark it ignored. If a stage can't be finished, stop, write what blocks
  it in the pull request description, and go on to the next stage.

Stop and ask Ryan before: running the agent syntax test on the whole task set (stage H.7, it
spends money), and merging. Everything else is yours.

## Stage A: every error carries a next step

**Goal.** Every error code's diagnostic carries at least one `help` or one `fix`, and a
near-miss name (a typo, a transposition) is suggested for names, fields, methods and variants.
Today 30 of the 150 codes with explain programs carry only a message, and the most common
agent-facing codes are among them (E0200, E0207, E0301, E0303).

**Facts.**

- A diagnostic is `wrela_diag::Diagnostic { code, severity, message, primary, secondary, notes,
  help, fixes }` (`compiler/diag/src/diagnostic.rs:70`). Builders: `with_help` (`:132`),
  `with_fix(message, span, replacement)` (`:138`), `with_fix_edits` (`:147`), `with_choice`
  (`:154`), `with_note`. A `Fix` whose `choice` is true is listed by `wrela fix`, not made.
- "Did you mean" is `wrela_sema::resolve::closest(name, candidates)`
  (`compiler/sema/src/resolve.rs:178`): Levenshtein distance, threshold `(name.len() / 3).max(1)`.
  That threshold is 1 for names under 6 bytes, and a transposition (`widht`, `aera`) costs 2, so
  the two commonest typos are never suggested.
- The explain programs are `compiler/driver/explain/<CODE>.wrela`; the test that runs them is
  `explanations_programs_do_what_they_say` in `compiler/tests/tests/suite/explain.rs:63`, with
  the helper `diagnose(code, dir)` (`:23`) and `scratch`. Mirror it.
- The 427 diagnostic goldens in `compiler/tests/diagnostics` are compared as JSON of the
  errors, help and fixes included (`diagnostics_meet_the_bar`, `suite/diagnostics.rs:78`).
  Adding a help to a code changes every golden that has it.
- The spec's `// error:` blocks check that each listed `help:` is among the diagnostic's helps
  and each `fix:` among its fixes (`suite/language.rs:508`). Adding helps or fixes breaks nothing
  there.

**Steps.**

A.1 Add the test first, red. In `compiler/tests/tests/suite/explain.rs` add
`every_diagnostic_carries_a_next_step`: for each explanation with programs, write the wrong
program to `scratch`, `diagnose` it, and require of **every** error it produces (not only the
one with the explanation's code) that `!d.help.is_empty() || !d.fixes.is_empty()`. Then do the
same over every case of `compiler/tests/diagnostics` (the 427 fixtures `diagnostics_meet_the_bar`
reads; reuse its loader): every error of every fixture. One explanation per code reaches one
emission site, and most codes have several (E0207 has seven), so the fixtures are what give the
test reach. Put the codes that fail today in a `const NO_NEXT_STEP: &[&str]` in that file, with
the comment "shrinks to empty in stage A", so the test passes now and each later step removes
entries. The stage is done when the array is empty; keep the test and the (empty) array.

A.1b The site audit. Write a throwaway script (keep it out of the commit) that lists every
`Diagnostic::new(` in `compiler/sema/src`, `compiler/syntax/src`, `compiler/lower/src`,
`compiler/driver/src` and `compiler/ir/src`, whether its code is a literal (`codes::E0207`) or a
variable (as in `compiler/sema/src/borrowck.rs:1972` and `compiler/lower/src/derive.rs:428`,
which map a variable to a code), and for each, every path from the constructor to the
`self.err(` or `push(` that consumes it. A site passes only when every such path attaches a
`.with_help(`, `.with_fix(` or `.with_fix_edits(`: a help on one branch of an `if` and none on
the other is a bare site. Expect roughly 250 sites. Work through the list after A.5: each site
gets a help or a fix in the house style, chosen as the table in A.5 chooses them (the same code's
other sites usually take the same help, with the site's own names filled in). A site that is
unreachable from any program (a defensive branch) may stay as it is; say so in a one-line
comment at the site.

A.2 Make `closest` find transpositions. In `resolve.rs`, change `edit_distance` to optimal
string alignment distance (Levenshtein plus a transposition of two adjacent characters costing
1). Write it as a full `(a.len()+1) × (b.len()+1)` table; names are short. Keep the threshold.
Add unit tests beside the existing ones (`resolve.rs:1599`): `widht` → `width`, `aera` → `area`,
`lenght` → `length`, and that `x` does not suggest `y`.

A.3 Suggest methods. In `compiler/sema/src/check/call.rs`:

- `method_names` (`:2938`) lists inherent and trait methods but not std's impls of built-in types
  (`impl<T> [T]` in `compiler/std/collections.wrela:134`, `impl str` in `std/string.wrela:293`,
  `impl [S; N]` in `std/field.wrela:1056`) nor the built-in methods (`len`, `count_ones`, …).
  Add both: the impls come from `p.builtin_inherent` (match them the way `builtin_method` at
  `:2056` does; factor that matching into a helper that returns the impls for a type), and the
  built-in methods from `BuiltinFn` entries whose kind is `Method` or `Both`, filtered by the
  applicability rules at `:2322-2341` (`Len` on `Slice`/`Array`/`ArrayN`; the integer methods on
  `Int`; the math functions on `Float`/`Vec`).
- At the E0207 site (`:2344`), after the existing notes: if `closest(&name.name, candidates)`
  gives a name, add `with_fix("did you mean `{s}`?", name.span, s)`. Otherwise add a note
  `` `{shown}` has these methods: a, b, c `` (sorted, at most 8, then "…") and the help
  `` `wrela doc {path}` lists them `` (the type's path for a struct or enum; the type's name for a
  built-in, which stage B makes work).
- The associated-function sites (`:1927`, `check/expr.rs:739`): candidates are the type's
  associated functions (inherent methods without `self`); same fix-or-note.
- `Trait::m` (`:192`): candidates are the trait's methods.

A.4 Suggest variants and fields where missing. E0211 (grep `codes::E0211`): candidates are the
enum's variants; fix-or-note as in A.3. E0206 in patterns (`check/pat.rs:541`) gets the same
"did you mean" as `struct_field` (`expr.rs:1514`) already has.

A.5 Give the remaining codes a help. For each, the site is the one the explain program reaches
(run `target/release/wrela check --json` on a scratch copy of the wrong program to find it; the
research located most of them). The text is the plan's, tuned to house style:

| Code | Site | Add |
|---|---|---|
| E0001 | lexer | help: "remove it, or put it in a comment or a string" |
| E0004 | lexer | help: "write the exponent's digits: `1e0`" |
| E0100 | the parser's generic `expected …, found …` site | help built from the expected set: "put `,` or `)` here" |
| E0101 | parser, "never closed" | help: "add the `}` that closes it" |
| E0200 | `expr.rs:472` | already builds a fix from `closest`; with A.2 it now fires for transpositions. When it doesn't, help: "declare it with `let`, or import it with `use`" |
| E0207 | A.3 | fix or note + help |
| E0211 | A.4 | fix or note |
| E0212 | grep | help: "a type is `f32`, `vec3`, a struct or an enum; `{name}` is a function" |
| E0301 | `call.rs:719` | help: "it's declared as `{signature}`" using `p.text(sig_span)` |
| E0302 | `call.rs:738` | already has a "did you mean" fix via `closest` (so A.2 widens it); where no name is close it has only a note listing the parameters: add help "pass it by one of those names" |
| E0303 | `call.rs:797` | help: "pass it by name: `{param}: <{type}>`" for the first missing parameter |
| E0304 | `call.rs:762` | help: "keep one of them" |
| E0308 | grep | help: "keep one of them" |
| E0310 | `call.rs:86`, `:312` | help: "only a function, a closure or a `fn` value is called; use a value in an expression (`x * 2.0`)" |
| E0311 | `expr.rs:1499` | scalar receiver: help "a scalar has no components; `vec2`, `vec3` and `vec4` have `.x`, `.y`, `.z`, `.w`"; enum receiver: help "an enum has no fields: `match` on it"; struct receiver: the fields note A.4 gives |
| E0312 | grep | help: "its components are 0 to {n-1}" |
| E0315 | grep | help: "`break` leaves the innermost `while`, `loop` or `for`; outside a loop, `return`" |
| E0319 | `call.rs:1750` | help: "conversions are between scalars (`f32(i)`) or vectors of one size; a vector's component is `.x`, and its size is `length(v)`" |
| E0320 | grep | help: "a `{Enum}` matches its variants: `{Enum}::{first}`, …" |
| E0322 | grep | help: "it's declared as `{name}<{params}>`" |
| E0323 | `call.rs:1765` | help: "give {n} numbers, a smaller vector and the rest, or one number for every component" |
| E0325 | `resolve.rs:908` | help: "use an integer literal, or a constant: `const N: u32 = 8`" |
| E0400 | the site the explain program reaches | help: "implement it: `impl {Trait} for {Type} { … }`" |
| E0401 | `collect.rs:3545` | help: "add `{signature}` to the impl" using `p.text(tdef.sig_span)` |
| E0403 | grep | help: "remove one of the impls, or implement it for two different types" |
| E0405 | `collect.rs:3640` | help: "declare it as the trait does: `{trait's signature}`" |
| E0407 | grep | help: "remove `Copy` from `{Type}`, or give `{field}` a `Copy` type (`Text` for a string)" |
| E0408 | grep | help: "`{Trait}`'s associated types are …" or "it has none; declare one in the trait: `type {Name}`" |
| E0411 | grep | help: "remove `{B}` from `{A}`'s supertraits, or `{A}` from `{B}`'s" |
| E0700 | `compiler/lower/src/derive.rs:235` (the cycle) | help: "take the derivative as data: compute `gradient(f, x)` outside and pass it in" |
| E0701 | `lower/derive.rs:428` | help: "make the count fixed: `for _ in 0..n { … }`, and `break` when done" (true after stage G) |

A.6 Bless the goldens: `WRELA_BLESS=1 tools/check.sh diagnostics::diagnostics_meet_the_bar`,
then read `git diff compiler/tests/diagnostics` and confirm only `help`, `fixes`, `notes` and
(for E0207) `message` values changed. Say which keys changed in the commit message.

**Checks.** `tools/check.sh explain::`, `tools/check.sh diagnostics::`,
`tools/check.sh codes::every_code_is_tested`, `tools/check.sh language::`, `cargo test -p wrela-sema`.

**Done when** `NO_NEXT_STEP` is empty, the four checks pass, and `wrela check` on a program
with `b.aera()` (where `area` exists) prints `did you mean `area`?`.

## Stage B: `wrela doc` knows the built-in types

**Goal.** `wrela doc vec3`, `wrela doc f32`, `wrela doc "[T]"` (and the aliases `run`, `array`,
`str`) answer, so an agent can find `len`, the swizzles and the constructors without reading the
spec. Today they print "nothing public is named".

**Facts.**

- `compiler/cli/src/doc.rs`: `build_index` (`:255`) indexes std and the package; `type_name`
  (`:328`) resolves an impl's type only for `TypeExprKind::Path`, so `impl<T> [T]`, `impl str` and
  `impl [S; N]` attach their methods to nothing. `run` (`:86-102`) special-cases `builtins`
  through `wrela_driver::builtins()` (`compiler/driver/src/lib.rs:500`).
- The types: `BuiltinTy` and `BuiltinTy::lookup` (`compiler/sema/src/builtins.rs:9-50`). The
  functions: the `builtin_fns!` table (`builtins.rs:132-202`), each with a kind `Free`, `Method`
  or `Both`. Swizzles: `field_of_checked` (`check/expr.rs:1431-1479`): `xyzw` and `rgba`, 1 to 4
  components, no mixing. Vector constructors: `vec_ctor` (`check/call.rs:1765`).
- Spec text: §4.1 scalars (`spec/language.md:479`), §4.2 vectors and matrices (`:487`), §4.3
  arrays (`:506`), §4.4 runs (`:517`, `xs.len()` at `:523`).
- CLI tests: `compiler/cli/tests/suite/doc.rs` (`builtins_are_listed` at `:76`).

**Steps.**

B.1 Add a module `builtins` to the index with one `Entry` of kind `type` per built-in type:
`bool`, the integers, `f32`, `f64`, `vec2`..`vec4` with each element suffix `VecElem::ALL` gives,
`mat2`..`mat4x3`, `str`, and two synthetic names `[T]` (a run) and `[T; N]` (an array). Each
entry's `doc` is two to four sentences taken from the spec section above, naming the section.
Each entry's `signature` is the constructor forms (`vec3(x, y, z)`, `vec3(xy, z)`, `vec3(s)`),
the components and swizzle rule for vectors, and `xs.len()`, `xs[i]`, `for x in xs` for runs and
arrays.

B.2 Attach methods. Make `type_name` return a name for slice, array and `str` type expressions
(`[T]`, `[T; N]`, `str`), so std's impls attach to the entries of B.1. Add the `builtin_fns!`
methods by applicability: `len` on `[T]` and `[T; N]`; the integer methods on the integers; the
`Both` math functions on `f32`, `f64` and the float vectors, listed under "built-in functions
that take it".

B.3 Make `wrela doc <name>` resolve the names of B.1 (including the aliases `run` → `[T]`,
`array` → `[T; N]`) before the "nothing public is named" error, and make `wrela doc builtins`
print the functions as before, then the types.

B.4 `wrela doc --export` (`compiler/cli/src/export.rs`) writes the module as `std/builtins.md`
and its items into `index.json`, as any module. Adding items is compatible with its consumers.

**Tests.** In `compiler/cli/tests/suite/doc.rs`: `builtin_types_are_documented` (`wrela doc vec3`
exits 0 and mentions `.x` and `vec3(x, y, z)`; `wrela doc "[T]"` mentions `len`; `wrela doc run`
is the same page; `wrela doc f32` mentions `sqrt`). In `suite/export.rs` (or wherever the export
is tested): the exported index has an item whose path is `builtins::vec3`.

**Checks.** `cargo test --release -p wrela --test suite -- doc::`, and the export tests.

## Stage C: lints for the mistakes agents make

**Goal.** Four new warnings, each for a mistake only an agent makes often, each with a help, and
each measured on real code before it is kept. Plus `wrela pipelines` says when a pipeline's site
is under a runtime condition.

**Facts.**

- Warnings are declared in `compiler/diag/src/codes.rs:247-255`; the next is `W0010`, and
  numbers must have no gaps (`removed_codes_are_retired`). A warning is not a conformance rule:
  it needs no spec-table row and no primer entry (the conformance suite checks errors only).
- A warning needs: the `codes.rs` line; an explain file `compiler/driver/explain/W00xx.wrela`
  whose fixed program has no diagnostic at all (`explain.rs:95`); a test in
  `compiler/tests/tests/suite/warnings.rs` using its `warnings(name, text)` helper (`:10`), which
  also satisfies `codes::every_code_is_tested`; and the A.1 test (a help or a fix).
- `std_has_no_warnings` (`warnings.rs:332`) fails the suite if a lint fires inside std. Std is
  not borrow-checked, so MIR-borrowck lints (W0006's family) never see it; type-check, MIR-build
  and lowering lints do.
- Where things are: the effects of each body, transitively, are `Effects::build(p, &checked.mir)`
  (`compiler/sema/src/effects.rs:530`) with `Effect::{Alloc, Io, Trace, Nondet, Recursion, Host,
  Panic}` (`:26`). MIR loops are `TerminatorKind::Loop{body, continuing, merge}` (`mir.rs:466`);
  `.clone()` is `Callee::Clone` (`thir.rs:112`, re-exported in MIR). THIR loops are
  `StmtKind::{While, Loop, ForRange, ForEach, ForFields}` (`thir.rs:332-377`). W0006 is the
  pattern for a MIR lint (`borrowck.rs:2266`, `unchanged_vars`); W0008 for a whole-program one
  (`sema/src/lib.rs:300`, `unused_items`); W0005 for a type-check-time one (`call.rs:819`).
- Pipelines: `Output.pipelines: Vec<PipelineSummary { kind, entries, names, sites }>`
  (`compiler/driver/src/lib.rs:57-67`), filled by `summarize` (`:934`) from
  `wrela_lower::Lowered.pipelines` (`PipelineOut { key, sites: Vec<Span> }`, `lower/src/gpu.rs:64`).
  The CLI is `compiler/cli/src/pipelines.rs`.

**The four lints.** For each: declare, implement, explain file, warnings.rs test, then measure.

| Code | Title | Fires when | Help |
|---|---|---|---|
| W0010 | a copy that is only read | a `let` local is bound to `place.clone()` of a named place, and every later use of the local is a read: it is never assigned, never lent `mut`, never moved (`take`, returned, stored in a struct, captured by a closure, passed to a `take` parameter) | "borrow it instead: `borrow {name} = {place}`" (no fix: a borrow can be refused by exclusivity, so it's the author's call) |
| W0011 | a test that can't fail | a `@test` of code (no `frames:`, `ticks:` or `tool`) whose transitive effects lack `Panic` | "assert what it computes: `assert(x == expected)`" |
| W0012 | a request polled in the call that made it | in one body with no loop between them, a local is bound to a call whose result type is `std::io::Pending<T>`, and `.poll()` is called on that local | "keep the `Pending` in the state and poll it next frame" |
| W0013 | a counted `while` over a run | `while i < xs.len() { … }` where every use of `i` in the body is `xs[i]` and the body's last statement is `i += 1` | "walk it: `for x in xs { … }`" |

Where to implement: W0010 in `borrowck.rs` beside `unchanged_vars` (MIR; the moves and `mut`
uses are what borrowck already computes; check `OperandKind` for the move/copy distinction);
W0011 in `sema/src/lib.rs` beside `unused_items`, after `effects::check` (first write a 5-line
test that `assert` gives `Panic` in `Effects` and that a body without one doesn't; if `assert`
does not produce `Panic`, add it where `fn node` (`effects.rs:834`) handles `BuiltinFn::Assert`
and `Panic`); W0012 and W0013 on THIR at the end of `check_fn`, walking `Expr::for_each_child`
(`thir.rs:494`). Skip std's bodies in all four, as `unused_items` does.

C.1 Measure before keeping. For each lint, run `target/release/wrela check` on
`compiler/tests/frozen/{engine,grazer,herd,wolf,ui,hello-field}` and on
`compiler/std` (via `warnings::std_has_no_warnings`), and read every firing. A lint that fires
on code that is right is narrowed until it doesn't, or dropped (say so in the commit message). A
lint that fires on code that is wrong in a frozen program is right; the frozen programs are
copies and are not edited.

C.2 `wrela pipelines`: for each site, say whether the dispatch or draw is under a runtime
condition in its function. The criterion is control dependence in the textbook sense, computed
from post-dominators; neither "every path crosses a branch" (which marks the code after an
`if`/`else` join) nor successor reachability (which, with loops' back edges, fails to mark a
conditional draw inside a loop, since the other arm reaches it on the next iteration) is right.
Compute it in lowering, where a site's span is recorded, on the function's MIR
(`compiler/sema/src/mir.rs:455-480`: `If { cond, then, else_, merge }`, `Loop`, `LoopBack`,
`Break`, `Continue`, `Return`; successors at `:525`): add a virtual exit block that every
`Return` reaches, compute the post-dominator tree over the CFG (the standard iterative dataflow:
a block's post-dominators are itself plus the intersection of its successors'), and then a
block `B` is control dependent on a branch block `X` if some successor `s` of `X` is
post-dominated by `B` and `B` does not strictly post-dominate `X`. A site is conditional when
its block is control dependent on any `If` whose `cond` is not a literal. Carry it as
`PipelineSummary.conditional: Vec<bool>` parallel to `sites`; print ` (under a condition)` after
such a site, and `"conditional": [...]` in `--json`. Tests in the CLI's `pipelines` tests, six
programs: a draw inside an `if` arm (marked), a draw after an `if`/`else` join (not marked), a
draw after `if x { return }` (marked), a draw under `if true { … }` (not marked: a literal
condition is not a runtime one), a draw inside `for i in 0..4 { if flag { draw } }` (marked), and
a draw after that loop (not marked).

**Checks.** `tools/check.sh warnings::`, `tools/check.sh explain::`,
`tools/check.sh codes::every_code_is_tested`, `cargo test --release -p wrela --test suite -- pipelines::`.

**Spec.** §10.2's table doesn't list warnings individually; nothing to add. Add one sentence to
§23.9's `wrela pipelines` row: "a site under a runtime condition is marked".

## Stage D: `wrela migrate`

**Goal.** A change the language moves away from ships as a warning with a fix that is never the
author's choice; `wrela migrate <package>` makes every such fix and nothing else, so moving a
workspace's pin to a new toolchain is one command. The first worked example is W0004 (`Clone`
beside `Copy`), which already has such a fix.

**Facts.**

- `wrela fix` (`compiler/cli/src/fix.rs`): `plan` (`:161`) takes a diagnostic's single fix when
  `!fix.choice`, with no severity filter, so it already applies warnings' fixes. It writes files
  each round and re-checks from disk (`:72-116`); rounds are capped by `ROUNDS = 16`.
- An in-memory check exists: `wrela_driver::check_with(root, &Overlay)` with `Overlay =
  HashMap<PathBuf, String>` keyed by canonical paths (`compiler/driver/src/lib.rs:158-161`).
  Refactor's `Texts` (`driver/src/refactor.rs:80`) and its `plan` (`:132`) show the pattern:
  edit in memory, check, then `write` (`:1280`) atomically.
- Diff rendering: `refactor::hunks` (`:1109`) and `diff` (`:1164`).
- The manifest refuses unknown keys (`driver/src/manifest.rs:418`, E0219), so nothing is written
  to `wrela.toml`. The running version is `pin::VERSION` (`cli/src/pin.rs:19`).
- `docs/release.md` lines 9-13 say what bumps minor and patch.

**Steps.**

D.1 In `codes.rs`, add a range comment `// ---- W01xx: forms the language moved away from
(wrela migrate) ---` after W0009's block (empty for now). The no-gap test
(`removed_codes_are_retired`, `codes.rs:285`) takes a code's first three characters as its
range, so `W01` is a new range that starts at `W0100`; confirm with `cargo test -p wrela-diag`.
Then add
`pub const MIGRATIONS: &[(Code, &str)] = &[(W0004, "0.1.0")]`: the code and the release since
which the old form is a migration. A code is a migration if it is in `MIGRATIONS` or its number
is in W01xx. Add `pub fn is_migration(self) -> bool` and `pub fn migration_since(self) -> Option<&'static str>`.

D.2 Add the totality test in `suite/explain.rs`: for every migration code, its explain program's
diagnostic has exactly one fix and that fix's `choice` is false.

D.3 Rework `fix.rs`'s loop to edit in memory: each round calls `check_with(root, &overlay)`,
applies the planned edits to the overlay's texts, and stops when a round makes none; then it
formats the changed texts, writes each file once with `wrela_driver`'s atomic write (the one
`refactor::write` uses), and reports from a final `check_with` of the written texts. Keep every
rule of `plan` as it is. Add `--dry-run`: no write, print the diff (`refactor::hunks`/`diff`)
and the report. The CLI tests in `compiler/cli/tests/suite/fix.rs` must still pass unchanged.

D.4 Factor the command into `fix_with(args, keep: &dyn Fn(&Diagnostic) -> bool, verb: &str)`;
`wrela fix` passes `|_| true` and "fixed"; a new `compiler/cli/src/migrate.rs` passes
`|d| d.code.is_migration()` and "migrated", and its JSON entries carry `"since"`. Add the
dispatch in `main.rs`, the usage line `wrela migrate <package-dir> [--dry-run] [--json]`, and the
doc-comment table row. Exit status: 0 when no migration diagnostic remains, 1 when one does or
the package has errors, 2 for a write or usage error.

D.5 Tests in `compiler/cli/tests/suite/migrate.rs`, in the style of `fix.rs`'s: a package with
`struct P: Copy + Clone { x: f32 }` is migrated (the text loses `Clone`, exit 0, the report says
`migrated W0004`, `--json` has `"since": "0.1.0"`); `--dry-run` leaves the file as it was and
prints a hunk; a package with nothing to migrate says `nothing to migrate` and exits 0; a
package with an error (not a migration) is left alone and exits 1.

D.6 Docs. In `docs/release.md`, add a section "Deprecations" after "1. Choose the version":

> A change that a program can see ships first as a migration: a W01xx warning whose fix is
> never the author's choice, listed in `codes::MIGRATIONS` with the release it came in. The old
> form keeps compiling for at least two minor releases. When it is removed, the error that
> replaces it carries the same fix, and the warning's code is retired. A workspace moves its pin,
> runs `wrela migrate` on each member, and commits the result; `wrela migrate --dry-run` shows
> the diff first.

And in "6. Use the release in `rywible/wrela`", add step 4: "Run `wrela migrate` on each
member and commit it with the pin." In `spec/language.md` §10.2, add a bullet: "**A migration**
is a W01xx warning whose fix a tool always makes; `wrela migrate` makes every one of a package's
(§23.9)." Add the `wrela migrate` row to §23.9's table.

**Checks.** `cargo test --release -p wrela --test suite -- fix:: migrate::`,
`tools/check.sh explain::`, `cargo test -p wrela-diag`.

## Stage E: `wrela same`

**Goal.** One command says whether two builds of a program behave the same: the same command
bytes frame by frame, the same last frame within a tolerance, and the same pipelines. An agent
runs it after a refactor, a std change, or before moving a pin.

```text
wrela same <package-dir> --against <build-dir> [--frames n] [--fps f] [--size WxH]
    [--input <script.json>] [--within x] [--no-gpu] [--json]
```

**Facts.**

- `trace.rs` shows the whole run loop: `start` (`compiler/cli/src/trace.rs:119`) builds with
  `wrela_driver::build` into `<pkg>/build/<name>` through `crate::generated_path` and
  `crate::write_build` (`main.rs:326`, `:318`), decides GPU vs CPU from `manifest.json`'s
  `pipelines`, loads `Host::load_with(&dir, &Options)` or `CpuBuild::load_cpu(&dir).start_with(w)`,
  and steps with `lockstep_frame(i, fps, width, height, script)` (`runtime/native/src/lib.rs:827`
  and `:1433`). Scripts come from `crate::read_script` (`main.rs:348`).
- The hash: `Host::hash(&mut self) -> u64` (`lib.rs:1051`), `CpuHost::hash` (`:1328`): FNV-1a 64 of
  every submitted command byte, cumulative, readable after each frame. The screen:
  `Host::read_screen() -> Vec<u8>` RGBA8 (`:979`). The diff: `wrela_host::image::compare(a, b)
  -> FrameDiff { mean, max, differing, channels }` (`runtime/native/src/image.rs:84`). The golden
  tolerance default is `wrela_sema::defs::GOLDEN_WITHIN` (0.5).
- Two builds on one device: `Host::restart_in(&mut self, dir, &Options)` (`lib.rs:566`), as
  `profile --compare` uses it (`cli/src/profile/comparison.rs:186`). A build's identity:
  `comparison::build_hash(dir)` (`:103`). Pipeline names from a build directory:
  `Host::pipeline_names()` (`lib.rs:1019`), `CpuBuild::pipeline_names()`.
- Fresh storage per run: `comparison::Storage::new()` (a temp dir removed on drop).
- Nothing records whether a build directory was a debug or a release build. `wrela build`
  without `--debug` is a release build; `same` builds the candidate the same way and requires
  `--against` to be such a build (say so in the usage text).

**Steps.**

E.1 `compiler/cli/src/same.rs`. Parse the arguments (defaults: 60 frames, 60 fps, 640x480,
`within` 0.5, workers 1 always, since more workers can move work between frames). **Before
anything is written**, canonicalize `--against` (it must exist) and the candidate's output
directory `<pkg>/build/same`, which may not exist yet on a first run: canonicalize the package
directory and join `build/same` to it, as `validate_saved_build` does
(`profile/comparison.rs:29-33`, which canonicalizes the generated path only when it exists).
Refuse (exit 2, nothing touched) when either path is the other or an ancestor of the other, or
when `--against`'s `manifest.json` is missing or doesn't parse (`wrela_abi::Manifest::parse`).
Only then build the candidate into `<pkg>/build/same`. Frames are numbered from 0, as `wrela
trace` numbers them (`trace.rs:211`) and as the hosts document (`runtime/native/src/lib.rs:825`);
`first_differing_frame` uses that convention.

E.2 Run `--against` first: fresh `Storage`, load (GPU unless `--no-gpu` or no pipelines),
`init`, then for each frame `lockstep_frame` and record `hash()`; at the end `read_screen()` on
the GPU path. Then the candidate: on the GPU path `restart_in(candidate, &options)` on the same
host with a new `Storage`, `init`, and the same loop; on the CPU path a second `CpuHost`.

E.3 Compare: the first frame index whose cumulative hash differs (or none); the screen diff
(`image::compare`) against `within`; the pipeline name sets (added, removed); the two build
hashes. `same` is true when no hash differs, the screen is within tolerance, and the pipeline
sets are equal.

E.4 Print, for people:

```text
same as build/base: 60 frames, the same commands; last frame within 0.00 (of 0.5); 3 pipelines, the same
```

or, naming the first difference:

```text
differs from build/base: commands differ from frame 12; last frame 1.84 from it (max 41, 12% of channels); pipelines: +shade::<Fog> -shade::<Clear>
```

`--json`: `{"same": bool, "frames": n, "first_differing_frame": n|null, "screen": {"mean",
"max", "differing", "channels", "within"} | null, "pipelines": {"added": [], "removed": []},
"against": {"dir", "hash"}, "candidate": {"dir", "hash"}}`. Exit 0 same, 1 differs, 2 usage.

E.5 Tests in `compiler/cli/tests/suite/same.rs`. `trace.rs`'s plain `BALL` fixture submits no
commands at all, and its rendering variant (`trace.rs:86-93`: a screen pass cleared to a constant
and one draw) submits the same bytes every frame whatever the state. So make a fixture of your
own, `BALL_DRAWN`: `BALL` plus a screen pass whose clear colour carries the state,
`begin_screen_pass(clear: vec4(b.y / 10.0, 0.0, 0.0, 1.0))`, a draw and `present()`, with the
shaders the rendering variant uses. First assert, through `wrela trace`-style runs, that
changing the fixture's gravity changes the command hash (if it doesn't, the fixture is wrong, not
the test). Then: the package built twice is the same (`--no-gpu`, exit 0); the gravity change
reports its first differing frame as 0 (frames count from 0, and `BALL` moves the ball in its
first frame, so the clear colour differs at once) and exits 1; a first run, with no
`build/same` yet, works; `--against` pointing at `<pkg>/build/same` or at its parent exits 2
with the baseline bytes untouched; an `--against` directory with no manifest exits 2. Add one GPU test
only if `trace.rs` has a GPU test to copy; otherwise say in the commit message that the GPU path
was run by hand on `compiler/tests/frozen/hello-field`.

E.6 `main.rs`: usage line, doc-comment row. Spec §23.9: a table row for `wrela same` and one
sentence: "Two builds are the same when every submitted command byte is the same, frame by
frame, their last frames are within a tolerance, and they reach the same pipelines."

**Checks.** `cargo test --release -p wrela --test suite -- same:: trace::`.

## Stage F: three more refactors

**Goal.** `wrela refactor` gains `extract-const`, `inline` and `extract-fn`, each planned and
checked before anything is written, as the existing four are.

**Facts.**

- `compiler/driver/src/refactor.rs`: `Plan { op, files: Vec<FileChange { file, old, new }>,
  notes }` (`:45-53`); `Refusal` (`:63`); `Texts` (`:80`) with `get`, `replace(file,
  Vec<(start, end, text)>)`, `replace_all`, `overlay`, `plan(op, notes)` (`:132`, re-formats a
  file that was formatted and refuses "nothing would change"); `check_clean(root, &texts, why)`
  (`:177`) checks in memory; `checked_or(root, overlay, why, f)` (`:216`) gives the `Checked`
  program; `converge` (`:248`) runs rounds of check-and-fix; `calls_of(root, func)` (`:851`)
  gives every call site with each argument's span by walking MIR `Rvalue::Call`. Items are found
  by `crate::query::Index::new(checked, sources).one(path)` (`query.rs:611`); `Item::decl(p)`
  gives `Decl { module, name, span, name_span, public }`. The CLI is `compiler/cli/src/refactor.rs:43-57`.
- The AST: `FnDecl { name, params, params_close, ret, body: Option<Block>, sig_span }`
  (`compiler/syntax/src/ast.rs:130`), `Block { stmts, span }` (`:342`), `Stmt { kind, span }`
  (`:348`), `Expr { kind, span }` (`:685`), `Expr::walk` (`:914`), `Block::for_each_expr` (`:921`).
  The smallest expression at a position is what `wrela query type <file>:<line>:<col>` finds
  (`driver/src/query.rs`); reuse that lookup. The formatter is `crate::edit::format_text`
  (`driver/src/edit.rs:326`).
- The tests are `compiler/cli/tests/suite/refactor.rs` (helpers `plan(dir, args)`, `package`,
  `SHAPES`); copy their shape. Driver unit tests sit at `refactor.rs:1339+`.

**F.1 `extract-const <file>:<line>:<col> <NAME>`.** The expression at the position must be a
literal or an expression of literals, unit suffixes and constants only (walk it: any local, call
or field is a refusal "it depends on `x`, which a constant can't"). Its type is the checked
program's type for that expression (display it as `wrela query type` does). Insert
`pub const NAME: <type> = <text>` (or `const` when the expression is in a private function's
body; note it) after the file's `use` lines (before the first item), replace the expression's
span with `NAME`, `check_clean`, plan. Refuse a `NAME` that already exists in the module.

**F.2 `inline <fn>`.** Textual substitution changes behaviour whenever an argument has an
effect that the body drops, reorders or makes conditional: arguments are evaluated in the order
written (§3.1, `spec/language.md:176`), and `check_clean` can't see that. So the rule is narrow,
and everything else is refused with its reason:

- the function is a free function (not a method), not `pub` in `main.wrela` (an export), not
  generic, not recursive, and its body is a single tail expression with no statements and no
  `return`;
- every parameter is used **exactly once** in the body, and the parameters' first uses appear
  in the body's text in parameter order (so evaluation order is kept);
- at a call site, every argument's text is a plain local name whose type is fixed by syntax: a
  `let name: T = …` with a written type, or a parameter of the enclosing function. Nothing else:
  not a literal (a literal takes its type from where it lands, so `half(3)` with
  `fn half(x: f32) -> f32 { x / 2 }` is `1.5`, but the expansion `((3) / 2)` is integer
  division, and `f64(16777217.0)` is not `f64(x)` with `x: f32`), not a `let` without a type
  (its type is inferred from its uses, and the expansion changes them), and nothing with a call,
  a field, an index, a `take` or an operator;
- the body's literals keep their types: when the body holds a bare numeric literal, the
  function's declared result type must be a scalar (`f32`, `f64`, `i32`, `u32`, …), and the
  expansion is wrapped in a conversion to it, `f64(( … ))`, so a literal typed by the result
  stays so. A body with a bare numeric literal and a non-scalar result type is refused.

At every call site from `calls_of`: map arguments to parameters (positional, then named),
replace the call's span with the body's text with each parameter name replaced by `(` argument
text `)`, and wrap the whole in parentheses (and in the result conversion, above). Then, if the
function is private and `calls_of` finds no call left, delete the item (whole lines, as
`move_item` does). `check_clean`; the refusal carries the errors. Note in the plan how many
sites were inlined.

**Tests for all three refactors run the program, not only the checker.** `check_clean` accepts
an expansion that type-checks with a different meaning. So every positive refactor test's
fixture package carries `@test` functions that assert the refactored code's values (for
`inline`: `half`, `widen(x: f32) -> f64 { f64(x) }` called with a `let x: f32 = 16777217.0`, and
a named-argument call), and the CLI test runs `wrela test <pkg>` after the plan is written and
requires every test to pass. The refusals must be tested too: an unused parameter, a body that
uses `y` before `x`, a call whose argument is a call, a call whose argument is a literal, a call
whose argument is an untyped `let`, and a parameter used twice.

**F.3 `extract-fn <file>:<first-line>-<last-line> <name>`.** The lines must cover whole
statements of one block of one function body (find the function whose body span holds the
range; then the statements whose spans lie within the lines; refuse if a statement starts or
ends outside). Refuse if the range holds `return`, `break` or `continue`. The new function:

- parameters: every local used in the range and defined before it, in order of first use. Its
  type is the checked program's type at its first use. Its mode starts as `borrow`; a local that
  the range assigns, lends `mut` or passes to a `mut` parameter is `mut`; one that the range
  moves (`take`) is `take`.
- result: the locals defined in the range and used after it. None → no result; one → the
  function returns it and the call becomes `let x = name(…)`; two or more → refuse "more than
  one value flows out: `a`, `b`; extract a smaller range".
- the text: `fn name(params) -> type { <the lines, dedented one level>; <result local> }`,
  inserted after the enclosing function's item; the lines replaced by the call.

Check with `converge`: on an E05xx error that names a parameter (a `mut` use of a `borrow`), flip
that parameter's mode to `mut` and check again, up to `ROUNDS`. Any other error refuses, with
the errors.

**F.4 CLI**: the three words in `compiler/cli/src/refactor.rs`, the usage text, the doc-comment
row and §23.9's `wrela refactor` row. **Tests** in `compiler/cli/tests/suite/refactor.rs`: for
each operation one case that plans and writes, one refusal (a non-constant expression; a
recursive function; a range with two values flowing out), and `--dry-run --json` round trip
through `apply`.

**Checks.** `cargo test --release -p wrela --test suite -- refactor::`, `cargo test -p wrela-driver refactor`.

## Stage G: derivations that decide more

**Goal.** Measure how tight the intervals are, keep that from getting worse, and widen what can
be derived: a fixed-count loop with an input-dependent `break`, and `floor` across two integers.
Affine arithmetic and other new interval forms are out of scope (research, not this plan).

**Facts.**

- The interval derivation is `compiler/ir/src/derive/interval.rs` (entry
  `interval::interval`, `:47`; values are lo/hi pairs `Iv = (ValueId, ValueId)`, `:42`). Branches
  on the input run both sides and `merge` (`:637`, `:1156-1191`). `floor` splits in `split_floor`
  (`:955-1102`): a component whose range crosses exactly one integer (`two`, `:976-984`) gets two
  cases; the loop runs `1 << n` cases and `holds` (`:1036`) skips the ones that don't apply; a
  range crossing two or more integers falls back to the whole `[fl_lo, fl_hi]`.
- E0701 is decided in the activity walk: `Stmt::Loop` (`derive/mod.rs:495-504`) calls
  `loop_exit` (`:528`), which flags any `If` with an active condition inside the loop body whose
  branch `leaves_loop` (`:516`: a `Break`, `Continue` or `Return`). `return`s were already
  turned into store-plus-break by `single_exit` (`compiler/ir/src/single_exit.rs:215`).
- The corpus: 24 functions named in `NAMES` (`compiler/tests/tests/suite/derive.rs:16`),
  defined in `compiler/tests/fields/corpus.wrela` and `grazer.wrela`, selected by
  `with_function` (`fields/main.wrela:335`). `intervals_enclose_every_sample_on_the_cpu`
  (`derive.rs:150`) runs 512 boxes × 16 samples (`random_box` at `main.wrela:276`, `misses` at
  `:294`). The GPU twin (`:200`) is `#[ignore]` and run by hand. No tightness metric exists.
- `std::derive` (`compiler/std/derive.wrela`): `Box3` (`:39`) has only `sample`; `Box4` (`:138`)
  has nothing. `certify_positive` (`std/field.wrela:499-528`) pushes 8 octants it computes inline.
- Spec: §14.2 (`spec/language.md:1474`) states the loop rule and the one-crossing `floor` rule.

**Steps.**

G.1 The tightness ratchet. Add `derive::intervals_stay_as_tight_as_recorded` beside the
enclosure test, on the same boxes and samples (seed fixed): for each corpus function compute
`ratio`, the median over boxes of `(hi - lo) / (max sample - min sample + 1e-6)`, and `decided`,
the fraction of boxes whose interval excludes 0 (`lo > 0 || hi < 0`). Record both per function in
`compiler/tests/fields/tightness.json`. The test fails if a function's `ratio` grows by more than
5% or its `decided` falls by more than 0.01 against the file; `WRELA_BLESS=1` rewrites the file.
Bless it once now, before G.2, and commit the numbers.

G.2 Fixed-count loops with input-dependent exits. Add `derive/exits.rs`: before the activity
walk of a function derived as an interval, rewrite each `Stmt::Loop` whose own trip count is
inactive (its counter's exit does not depend on the input) but which `loop_exit` would flag: each
`Break` under an active condition becomes `done = true`, each `Continue` becomes `skip = true`,
the rest of the body runs under `if !done && !skip`, `skip` resets in the continuing block, and
the loop's own exit also leaves when `done` is surely true. The existing `If` machinery then
runs both sides and merges, which is sound (an over-approximation). `loop_exit` must not flag a
rewritten loop. A `while` whose condition is active stays E0701, and so does a recursion.

- Add `march` to the corpus (`corpus.wrela`, `NAMES`, `with_function`): a 32-step sphere trace of
  the corpus `blob` from a point along a direction, `break` on `d < 1e-3`, returning the distance
  travelled. The enclosure test must pass with it.
- The rule `derive.interval` is described in two places that a test keeps identical
  (`conformance::the_rule_table_is_the_specs`): `RULES` in `compiler/driver/src/primer.rs:235`
  and the spec's rules-by-ID row at `spec/language.md:3046`, both saying "of a function whose
  loops don't depend on its input". Change both, with the same words, to "of a function whose
  loops run a fixed count, whatever its input". Add `derive_loop_break_accept.wrela` to
  `compiler/tests/conformance` with the header `// accepts: derive.interval`, beside
  `derive_reject.wrela`, and a diagnostics case where a `for` with a `break` under an input
  condition now derives (the E0701 explain program, a `while`, stays as it is). Then read
  `wrela primer derive` and §14.2 side by side: they must say the same thing.
- Spec §14.2: replace "A loop whose exit depends on the input cannot be bounded, and nor can a
  recursion that runs under a branch on the input (E0701)" with "A loop runs its fixed count;
  a `break`, `continue` or `return` under a branch on the input is joined with the other
  iterations, as a branch is. A `while` whose condition depends on the input cannot be bounded,
  and nor can a recursion that runs under a branch on the input (E0701)."

G.3 `floor` across two integers. In `split_floor`, a component whose range crosses two
integers (`fl_hi == fl_lo + 2`) gets three cases; the case loop runs `3^n` cases and `holds`
skips the ones that don't apply, as now. Keep the loop a loop (no unrolling), so code doesn't
grow. Run G.1's test: `value noise`, `fbm` and `cells` must improve (`ratio` down or `decided`
up); re-bless `tightness.json` with the better numbers and say the before/after in the commit
message. If nothing improves, revert G.3 and say so. Check the WGSL budgets still hold
(`tools/check.sh wgsl` is the suite's shader-size step; run `tools/check.sh` at the end of the
stage). Spec §14.2's `floor` bullet: "one integer" becomes "one or two integers".

G.4 `std::derive` helpers: `Box3::center()`, `Box3::half()`, `Box3::contains(p)`,
`Box3::split() -> [Box3; 8]`, and `Box2::center/half/contains/split -> [Box2; 4]`, each with a
`@test` in `derive.wrela`; make `certify_positive` push `b.split()`'s boxes. Std's tests run
under `test_items::std_tests_pass`.

**Checks.** `tools/check.sh derive::`, `tools/check.sh lipschitz::`,
`tools/check.sh conformance::`, `tools/check.sh test_items::std_tests_pass`, `cargo test -p wrela-ir`,
then the GPU twin by hand: `cargo test --release -p wrela-tests --test suite derive:: -- --ignored --nocapture`.

## Stage H: the agent syntax test, version 2

**Goal.** The test that measures how learnable the language is becomes the compiler's benchmark:
it grades correctness, not only builds; it has tasks at the scale agents work; it records tokens;
it can be run with one command; and it is run on every release.

**Facts.**

- Version 1 is `tools/agent-test/{config.json, tasks.json, attempt.sh, score.py}` and the frozen
  run `results/m1/` scored in `results/m1.json` (17 of 20 built first try, scored on builds only).
  No launcher exists; the m1 agents were started by hand. `config.json`'s `context` names
  `examples/hello-field/*`, which no longer exists: the copy is
  `compiler/tests/frozen/hello-field/{main,shape}.wrela`.
- A hidden grader works: a module `grade.wrela` in the agent's package with `use main::<export>`
  and `@test` functions runs under `wrela test <pkg> --json` (checked: the JSON lists each test
  with `passed`). Modules are files; `main`'s items are `main::x` from another module (§3.6).
- The Claude CLI is on this machine (`claude`, 2.1.301): `claude -p "<prompt>" --model <m>
  --permission-mode dontAsk --allowedTools "<tools>" --output-format json --max-budget-usd <n>
  --no-session-persistence`. Its JSON result carries the usage and cost; read the fields from a
  real run before relying on names.
- `tools/check.py`'s `tools` step runs every unittest under `tools/tests`; `tools/tests/test_tools.py`
  tests `score.py`. Python here is allowed by `AGENTS.md`'s split: the toolchain's own checks.
- `wrela pipelines <pkg> --json` gives `{"pipelines": [{"kind", "entries", "sites"}], …}`.

**Steps.** Version 1 stays as it is (its results are history). Version 2 lives in
`tools/agent-test/v2/`. Its numbers are a new baseline, not comparable with m1's: the tasks are
rewritten (H.0), the grading is different, and the brief changes.

H.0 Every task names its contract. A hidden grader can only test what the task fixed, so each
of the 30 task texts (v1's 20 rewritten, the 10 new) names every export with its exact
signature, every type the grade needs (its name and fields), and fixed inputs: v1's "one shape
of each kind (any sizes)" becomes "a circle of radius 1, a rectangle 2 by 3 and a square of
side 1"; v1's unnamed functions get names. The substance of each task stays. The export's name
and signature are the contract; how the agent structures the rest is its own.

H.1 `v2/config.json`: like v1's, with `"version": 2`, the model, `max_attempts` 8, the
`"wrela"` key left empty (the runner takes the compiler's path on its command line, H.4), and two
context sets:

```json
"context_sets": {
  "full": ["spec/language.md", "spec/lexical.md", "spec/grammar.ebnf",
           "compiler/tests/frozen/hello-field/main.wrela", "compiler/tests/frozen/hello-field/shape.wrela"],
  "core": ["spec sections: How Wrela differs, §1–§7, §10.1–10.2, §13.2, §21 (as one file the runner extracts)",
           "compiler/tests/frozen/hello-field/main.wrela", "compiler/tests/frozen/hello-field/shape.wrela"]
}
```

The runner writes the `core` set as one file under the run directory from `spec/language.md`'s
headings. The brief is v1's with four changes: the context list is the set's; the agent may run
`<wrela> doc <item>`, `<wrela> explain <code>` and `<wrela> primer <area>` with the compiler's
absolute path the runner fills in (and nothing else but `v2/attempt.sh`); v1's "stop at the
first that builds" becomes "stop when it builds and you are confident it does what the task
says; a program that builds but is wrong counts as wrong"; and it is told that every attempt is
graded by hidden tests of the exports the task names.

H.2 `v2/tasks.json`: v1's 20 tasks, then ten more, each with `"grade"`: either
`{"tests": true}` (a `grades/<id>.wrela` module) or `{"pipelines": ["compute"|"render", …]}`
(the kinds that `wrela pipelines --json` must list). The ten:

| id | task (the brief's text is the full sentence; this is its substance) | grade |
|---|---|---|
| 21-vec-bag | `struct Bag { items: Vec<f32> }`, `fn add(b: mut Bag, x: f32)`; export `pub fn total(n: u32) -> f32`: add 1..=n and sum | `total(4) == 10` |
| 22-arena | an `Arena<Unit>` with `hp: f32`; insert five units with hp 1..5, remove the third by its handle; export `pub fn alive() -> u32` and `pub fn hp_sum() -> f32` | 4 and 12 |
| 23-borrow-struct | `borrow struct Ctx { xs: borrow Vec<f32>, k: f32 }` and `fn scaled_sum(c: Ctx) -> f32`; export `pub fn run() -> f32` over `[1, 2, 3]` with `k = 2` | 12 |
| 24-enum-option | `enum Cmd { Move(vec2), Stop }`, `fn first_move(cmds: [Cmd]) -> Option<vec2>`; export `pub fn where_first() -> vec2` over `[Stop, Move(1, 2), Move(3, 4)]` | `(1, 2)` |
| 25-write-a-test | `fn fib(n: u32) -> u32` and a `@test fn fib_is_right()` that asserts three values; export `pub fn fib_export(n: u32) -> u32` | `fib_export(10) == 55`; `wrela test --json` lists a passed test named `fib_is_right`; and the **validator** compiler (H.4) reports no W0011 (a test that can't fail, stage C) on the snapshot, so an empty test doesn't pass. The validator is fixed across runs, because `main`'s compiler has no W0011 and would grade the same snapshot differently |
| 26-modules-trait | a trait `Shape` in `shape.wrela`, impls in `circle.wrela` and `square.wrela`; export `pub fn areas() -> vec2` of a unit circle and a unit square | within 1e-4 of (π, 1) |
| 27-certify | a unit sphere lifted to y = 1.2; export `pub fn clear() -> bool`: `std::field::certify_clear` of it over the box below y = 0 at depth 5 | true |
| 28-frame-script | `pub struct Game { pub presses: u32 }`, `pub fn init() -> Game`, and a `frame` that counts `Key::Space` presses from `std::input`; plus the agent's own `@test(frames: 10, input: SCRIPT)` with a `Script` constant that presses space at frames 2 and 5 and asserts 2 | the grade module has its own frame test: `use main::Game`, a `Script` constant pressing space at frames 3, 4 and 7, and `@test(frames: 10, input: GRADE) fn grade_presses(g: Game) { assert(g.presses == 3) }` (H.3's reference proves a frame test in a non-main module runs; if it doesn't, make the grade an export `pub fn presses(g: Game) -> u32` and say so) |
| 29-gpu-uniform | a `GpuData` struct `Params { scale: f32, offset: f32 }` as a uniform; a kernel over 64 floats, `out[i] = in[i] * scale + offset`, dispatched each frame | pipelines: compute |
| 30-interval | export `pub fn bound(lo: f32, hi: f32) -> vec2`: the derived interval of `x * x - 2 * x` | `bound(0, 2)`: lo ≤ -1 and hi ≥ 0 |

H.3 Reference solutions, and wrong ones. For each of the 30 tasks write
`v2/reference/<id>/right/main.wrela` (and its modules): what a careful agent would write from
the spec. Beside it write `v2/reference/<id>/wrong/main.wrela`: a program that builds and
meets the contract's shape but is wrong (a constant return; for a pipelines task, no kernel or
draw; for 25, an empty `fib_is_right`; for 28, a `frame` that never counts). Add the unittest
`tools/tests/test_agent_test_v2.py::References`: `test_every_right_reference_passes_its_grade`
and `test_every_wrong_reference_fails_its_grade`. Both take the compiler from the environment
variable `WRELA_BIN` (an absolute path to a built `wrela`) and the validator from
`WRELA_VALIDATOR` (defaulting to `WRELA_BIN`), and when `WRELA_BIN` is unset build one with
`cargo build -q --release -p wrela` as v1's `attempt.sh` does. A task whose right reference you
cannot make pass is a task you must rewrite, and a grade that passes the wrong reference is a
grade you must tighten: these two tests are what keep the tasks solvable and the grades honest.

H.4 The compiler is chosen once, explicitly. v1's `attempt.sh` builds the checkout it lives in,
so pointing it at another target directory with `CARGO_TARGET_DIR` rebuilds *this* branch there
and compares the branch with itself. v2 never builds: `v2/attempt.sh` runs the compiler at
`$WRELA_BIN` (required, absolute, must exist) and otherwise works as v1's; every other use of the
compiler (grading, the agent's `doc`/`explain`/`primer` commands, the version line) uses the same
path. `v2/run.py <run-name> --wrela <abs path> [--validator <abs path>] [--model MODEL]
[--context full|core] [--tasks id,id] [--parallel n]`: refuses a run name that already exists
under `target/agent-test/`; writes `<run>/run.json` with the date, model, context set, the
compiler's path, its SHA-256, its `--version` line, the validator's path and SHA-256, and
`git rev-parse HEAD` of the checkout the run is made from. The **validator** is the compiler
used for grading criteria that depend on a lint (task 25's W0011 check); it defaults to
`--wrela`, and H.7 passes this branch's compiler as the validator for every run, so a criterion
never changes between the runs being compared. Building and the hidden tests always use
`--wrela`. Then for each
task makes `<run>/<id>/`, writes `BRIEF.txt`, and runs
`claude -p "Read <abs dir>/BRIEF.txt and do exactly what it says."` from the repository root with
the flags in the facts above and
`--allowedTools "Read,Write,Edit,Bash(tools/agent-test/v2/attempt.sh:*),Bash(<abs wrela> doc:*),Bash(<abs wrela> explain:*),Bash(<abs wrela> primer:*)"`
and `--max-budget-usd 2`. It saves the CLI's JSON as `<id>/usage.json` and the agent's final
report as `<id>/report.md`. `--parallel` defaults to 2. Add a unittest
`tools/tests/test_agent_test_v2.py::Runner::test_the_chosen_compiler_is_the_one_run`: with
`WRELA_BIN` set to a stub script that records its arguments, `v2/attempt.sh` runs the stub and
builds nothing (the test fails if `cargo` was invoked), and `run.py --dry-run` prints the exact
`claude` command for each task without launching it.

H.5 Grading is per attempt, not of the final package. Every `attempts/N/src` is a snapshot;
for each one, copy it to a temp package, add `v2/grades/<id>.wrela` as `grade.wrela` (refuse the
snapshot if the agent wrote a `grade.wrela`: it is then wrong), and run
`$WRELA_BIN test <pkg> --json > attempts/N/graded.json`. A snapshot is correct when it builds
and every test passed (plus, for 25, a passed test named `fib_is_right`, and no W0011 when the
validator checks the snapshot: `<validator> check <pkg> --json`). For a `pipelines` grade, run
`$WRELA_BIN pipelines <pkg> --json` on the snapshot; correct when each listed kind appears.
`run.py --grade <run-name>` grades a finished run again. The metrics: `first_try_correct` is
attempt 1 correct; `attempts_to_correct` is the first N whose snapshot is correct. Before any
paid run, H.3's wrong-reference test must be run twice: with `WRELA_BIN` as `main`'s compiler and
as this branch's, with the validator this branch's both times; task 25's wrong reference (an
empty `fib_is_right`) must fail under both.

H.6 `v2/score.py <run-dir>`: v1's report plus `first_try_correct`, `correct_eventually`,
`mean_attempts_to_correct`, `mean_tokens` (input + output from `usage.json`), `mean_cost_usd`,
and per task `correct_at`, `tokens`, `cost_usd`, `graded` (the failing test names of each
attempt). Tests in `tools/tests/test_agent_test_v2.py::Score`, mirroring `test_tools.py`'s two
cases plus: a run where attempt 1 builds but fails its grade and attempt 2 passes must score
`correct_at == 2`; a run whose `graded.json` fails every attempt scores `correct_at == null`.

H.7 Run it, with Ryan's go-ahead, because it spends money. The arithmetic: 30 tasks per run,
at most $2 each by the budget flag, so one run is at most $60 (typically far less; the smoke
run tells you the real figure); the two runs the plan needs (`main`'s compiler and this
branch's, both `--context full`) are at most $120; the `core` run is a third, optional, and is a
separate question to Ryan. Build `main`'s compiler first, once, in its own worktree:
`git worktree add ../wrela-compiler-main main && (cd ../wrela-compiler-main && cargo build --release -p wrela)`,
and pass that binary with `--wrela` for the baseline run; pass this branch's binary with
`--validator` on every run, so the lint-dependent criterion is the same in both. Smoke-test three tasks first
(`--tasks 01-area,13-arrays,21-vec-bag`) with this branch's compiler and report the cost. Then
the two full runs. Copy the score reports to `v2/results/m2-main.json` and `m2-branch.json`
(and `m2-core.json` if run), and the attempt logs to `v2/results/m2-*/` as v1 did. Put the
headline numbers (first-try correct, mean attempts to correct, mean tokens) in the pull request.

H.8 Docs: `docs/citations.md`'s "Claude Sonnet 5.5 subagents" row becomes "Claude Sonnet 5.5
through the Claude CLI (`claude -p`)" with v2's path; `docs/release.md` gains a step 6 in
"2. Bump the version": "Run `tools/agent-test/v2/run.py release-X.Y.Z` and put its headline
numbers in the pull request" (and that it costs money, so it is run once per release).

**Checks.** `tools/check.sh tools::test_agent_test_v2.References.test_every_right_reference_passes_its_grade`,
`tools/check.sh tools::test_agent_test_v2.References.test_every_wrong_reference_fails_its_grade`,
`tools/check.sh tools::test_agent_test_v2.Runner.test_the_chosen_compiler_is_the_one_run`,
`tools/check.sh tools::test_agent_test_v2.Score`, and the smoke run.

## Stage I: release 0.5.0

I.1 `Cargo.toml` `[workspace.package] version = "0.5.0"`; `cargo check --workspace` so
`Cargo.lock` follows (new features that programs and tools can see: minor).

I.2 Read the whole diff of `spec/language.md` against `main`; every change this plan asked for
is there (§10.2 migrations, §14.2 loops and `floor`, §23.9's rows for `same`, `migrate`,
`refactor`'s new operations, `pipelines`' marker). `compiler/cli/src/main.rs`'s doc comment and
`usage()` list every new command and flag.

I.3 `tools/check.sh` with no arguments: the whole suite, green. Then the hand-run GPU checks
named in stages E and G.

I.4 The pull request, titled `Release 0.5.0: every error carries a next step, wrela migrate and
same, more refactors, derivations that decide more, the agent syntax test grades correctness`,
with: the stages in order, each in one line; the agent-test numbers (H.7); the tightness numbers
before and after G.3; what was dropped or narrowed in stage C and why; anything left undone.
Mention that `rywible/wrela` issues 238 and 240 reproduce as fixed on this toolchain (verified on
0.4.1; keep it true), and that `rywible/wrela`'s `docs/toolchain.md` gets the new commands when
its pin moves. End the description with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.

Ryan merges, then tags per `docs/release.md`.

## What this plan leaves out, on purpose

- Affine or reduced-affine arithmetic, and boxes over more than four inputs: research.
- Incremental checking or a daemon: `wrela check` is 0.27 s on the frozen herd and 2.3 s on
  the Last Green with the live engine.
- Constrained decoding from `spec/wrela.gbnf`: agents build 85% of tasks first try without it.
- New codegen passes without a V8 measurement.
- `wrela same` for ticked programs' tick logs beyond the frame hashes (the hosts' `--replay`
  does that already).
