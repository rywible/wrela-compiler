# Changelog

Each release's changes a program or a tool can see. The pull request of each release holds the
detail; `docs/release.md` says how a release is cut.

## 0.10.1

- **An associated constant whose initializer folds (§18.1's list) is folded where it's read**
  (language.md §3.4), however long the function that reads it and in a relaxed-SIMD build too: a
  branch on one that's `false` builds none of what only it reaches, its pipelines included. In
  0.10.0 it was a call, folded only once inlined: the CPU's inliner stops at its budget, and keeps
  strict code a call in relaxable code, so there the branch, and every pipeline behind it, stayed.

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
