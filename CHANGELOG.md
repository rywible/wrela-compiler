# Changelog

Each release's changes a program or a tool can see. The pull request of each release holds the
detail; `docs/release.md` says how a release is cut.

## 0.12.0

- **The browser host builds the first frame's pipelines first** (language.md §18.9): no
  pipeline that no command asked for starts until the first frame's commands have all run, and
  then none starts while a command waits for one. 0.11.1 started six at load, the largest first,
  so a pipeline the first frame needed compiled behind those it didn't. rywible/wrela's capybara
  (17 pipelines, headless Chrome on Linux, cold, median of 3): its first frame on the screen at
  2.71 s after the page opened, from 3.56 s; warm, 0.30 s, from 0.28 s (its pipelines from the
  cache now start when the frame asks, not at load).
- **Test mode says when each frame was on its way to the screen**: `frames.json`'s
  `submitted_ms`, when its commands had all run and were submitted (after any that waited for a
  pipeline), and `load.json`'s `first_submitted_ms`. `show=1`'s summary times the first frame and
  `playable` by them; `began_ms` is still when each frame began.
- **`std::tick::post(tag, a:, b:)` puts a value the frames know into the ticks** (§16.2): the
  host stamps it into the next tick's records, as an input event, so a tick log keeps it and a
  replay holds; the step reads it in `Ticked`'s new `posts` (`Posted { tag, a, b }`). A replaying
  host drops what the program posts. runtime/abi: the record kind `Posted` (16), and the host
  service `tick/post`. A `Ticked` literal names `posts` too.

## 0.11.1

- **An associated constant made with `&`, `|`, `^`, `<<` or `>>` folds** (language.md §3.4): a
  look's mask, `const SUBSTANCE: u32 = FORM | TONE`, decides its branches as a literal would. 0.11.0
  left it a call, so an `if` on it built both branches, and a look whose mask was written so drew
  its four-target shaders into a one-target pass (E0603).

## 0.11.0

- **A fragment shader may return its look's associated type** (language.md §18.1: `-> Y::Out`),
  checked for each instantiation, so one shader writes one target for one type and four for
  another. 0.10.3 refused it before instantiation (E0602).
- **An `if` on an associated constant builds only its taken branch** (§3.4), where its condition
  folds: the constant, or a comparison (of integers or `f32`s), `!`, `&&`, `||`, `&`, `|` or `^` of
  such and literals, written in the condition. The branch the instantiation doesn't take
  instantiates nothing, so it may draw into a pass the instantiation's shaders couldn't. 0.10.3 lowered both branches, and a
  draw in the other one was E0603 though it was never taken. The shaders are those 0.10.3 wrote
  but for their locals' names and order, and where a constant `if` reached WGSL before, it's gone.
- **`--set` points a `pub type` of the program's `main.wrela` at another type** (§13: `--set
  LOOK=styles::cel::Cel`), as it sets a `pub const`: so one program builds in each look.

## 0.10.3

- **An associated constant folds an enum's variants** (language.md §3.4), where the enum's
  variants hold nothing: `const FOG: bool = TIER != Tier::Low` and `match TIER { Tier::Low =>
  false, _ => true }` fold, so a program's `const TIER: Tier`, set with `wrela build --set
  TIER=Low`, builds none of what its tier turns off. In 0.10.2 they stayed calls, and the branch
  and its pipelines with them. An arm with a guard, or one that binds, still doesn't fold.

## 0.10.2

- **The browser host builds six pipelines at a time** (language.md §18.9), tier 0's first and
  the largest first, then tier 1's; one a command waits for starts at once, beside them. 0.10.1
  asked for all of tier 0's at once (tier 1's once they'd settled): the GPU process compiles a few
  at a time whatever's asked, so a small pipeline a frame needed waited behind every other. On an
  M4 Air (rywible/wrela's C5, three starts each), the Last Green's warm start went from 6.5 s to
  5.4 s and the vale's cold one from 8.0 s to 6.7 s.
- **Test mode's `starts.json`**: when each pipeline was built, and each time a command waited for
  one (its frame, the pipeline, when it began waiting), in milliseconds since the epoch as
  `frames.json`'s `began_ms`: what a start waited for.

## 0.10.1

- **An associated constant whose initializer folds (§18.1's list) is folded where it's read**
  (language.md §3.4), however long the function that reads it and in a relaxed-SIMD build too: a
  branch on one that's `false` builds none of what only it reaches, its pipelines included. In
  0.10.0 it was a call, folded only once inlined: the CPU's inliner stops at its budget, and keeps
  strict code a call in relaxable code, so there the branch, and every pipeline behind it, stayed.
  One read in a debug build (whose arithmetic checks for NaN as it runs) or a lifted one (whose
  literals are the table's), one only a role's build may read (its call is E0708 elsewhere), and
  one of `f64` arithmetic (the folder's floats are `f32`s) stay calls, as before.

## 0.10.0

- **Associated constants** (language.md §3.4): a trait declares `const K: u32`, with an optional
  default; an impl gives `const K: u32 = 4`; an inherent impl may hold one. `S::K` under a bound is
  a constant of each instantiation, and may be an entry point's `const` argument (§18.1): each
  instantiation is a pipeline of its own. E0420 for a constant of a type it can't have, or one
  called as a function; E0600 for an initializer that does IO, prints, panics or names itself;
  E0603 for one a `const` argument can't fold.
- **`const` parameters are folded into their pipeline's WGSL** (§18.1): no WGSL `override` is left,
  so a switch that's off leaves no code in its shader. The manifest is version 12, without
  `constants`; both hosts still open a version-11 build (0.9's), setting its pipelines'
  `constants`, so `wrela same` compares a build against one 0.9 made.
- **Passes into up to eight colour targets** (§18): `begin_pass5` to `begin_pass8`, their depth
  forms, `Pass5`–`Pass8`, `DepthPass5`–`DepthPass8` and `Targets5`–`Targets8`, within WebGPU's 32
  attachment bytes a sample (E0602 names each target's bytes and the total). The ABI takes eight
  colour attachments (WebGPU's default `maxColorAttachments`). `wrela pipelines --json` gives each
  render pipeline's `attachment_bytes`.
- **A texture as a field's data** (§20.3): `std::gpu::Texture3dHandle<F>`, a 3D texture's handle as
  `Copy + GpuData` data, bound beside any value that holds one in a struct's fields where an entry
  point takes it (E0602 for one in a tuple, an array or an enum), and bound to a GPU local once
  (E0702); and `std::field::Sampled3d`, a `Surface` whose distances are a texture's texels.
- **A struct update over a return-position trait type** no longer makes malformed IR: a type lowers
  as the type it reveals.
- **The constant cache's key** holds the constant's WASM, its fuel, the build's options, the bytes of
  each file it embeds and the toolchain's version; the cache is `build/consts/<version>`, and
  another version's is never read. `wrela build --report consts` lists each computed constant:
  bytes, fuel, seconds, computed or cached.
- **`wrela build --set NAME=VALUE`** (§3.5) gives a `pub const` of the program's `main.wrela` another
  value (a scalar, a `Text`, or a variant of a fieldless enum); `std::tool::toolchain`'s
  `BuildOptions::set` too; `wrela pipelines` and `wrela same` take it. The manifest records the
  sets. E0224 for a constant a build can't set (one only `pub(package)`, or of a type a build
  can't set, an alias's included). A `match` on a constant that holds a variant builds
  only the arm it takes.
