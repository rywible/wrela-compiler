# wrela: the language

*The one prose reference for the language, as it stands. Decision IDs (D-NNN), sketches and spikes refer to the design record in the git tag `design-archive-2026-10`.*

**Tiers 0 to 2 are implemented, and these hold them** (milestones 1 and 2):
- **Syntax:** `spec/lexical.md` and `spec/grammar.ebnf`, normative. An oracle parser generated from the grammar is checked against the compiler's parser, on every `.wrela` file in the repository (`compiler/grammar`).
- **Rules:** the conformance suite, `compiler/tests/conformance`. Each rule below names its rule ID there (for example `mem.take`), with a program it accepts and one it rejects with the rule's diagnostic code, or names the test that holds it. A test checks that the IDs here and the suite's are the same set (`conformance.rs`).
- **Examples:** every `wrela` block here compiles in a test (`language.rs`), and a block whose comments say `// error:` gives exactly those errors.
- **Diagnostics:** `compiler/tests/diagnostics`, the common mistakes with their messages, spans and fixes, and `wrela explain <code>` for every code.
- **The derived interpretations, numerics, the stdlib and the hosts:** the tests in `compiler/tests/tests` and `runtime/`.

Where this prose and those disagree, they win, and the prose is a bug.

## How wrela differs from Rust and TypeScript

**From Rust:**
- No references and no lifetimes: a parameter reads its argument in place, and `mut` marks a change at the call site, `step(mut g)` (§6.2).
- `let` owns a value; `borrow x = place` reads a place in place, and `mut x = place` changes it (§6.3).
- Moving out of a named place is written `take x`; a copy of a value that isn't `Copy` is `.clone()` (§6.1).
- A struct can't hold a borrow; a `borrow struct` groups projections, and lives only as a parameter, a result or a local (§6.6).
- No iterator types: `for` walks runs, arrays, ranges and arenas, and a query takes a closure or returns a `Vec` (§6.6).
- No `dyn`: an enum or a generic does the job (§18).
- A type declares its traits, `struct P: Copy + Eq`; there's no `impl Copy`, and `Eq`, `Ord`, `StateHash` and `Serialize` are derived when declared (§3).
- No macros: `f"…"` builds a string, and `std::io::print` shows a line (§4, §6.15).
- No `async`: IO is a request that the program polls (§6.15).
- No destructors in user code, no `Rc`, `Arc`, `Cell` or `RefCell`, and no mutable globals (§18).
- A newline ends a statement (§2); a number's suffix is a unit, `15cm`, never a type (§5).
- Integer overflow traps in every build (§11).
- `-> Surface` returns some type with the trait, as `impl Surface` does in Rust (§7).
- One language for CPU and GPU: `@compute`, `@vertex` and `@fragment` functions compile to WGSL (§12).
- `pub fn` in `main.wrela` is an export to the host, so its parameters are numbers, bools or vectors (§12).

**From TypeScript:**
- Every value has a static type, and there's no `any`, `null` or `undefined`: an absent value is `Option<T>` (§4).
- Values aren't shared: assigning a struct copies it if it's `Copy`, and otherwise needs `.clone()` or `take` (§6.1).
- No classes or inheritance: structs, enums and traits (§3, §7).
- No exceptions: an error is a `Result<T, E>`, passed up with `?`, and a bug panics (§15).
- No garbage collector: memory is values, arenas and handles (§6.8).
- Numbers have sized types (`f32`, `u32`, `i32`, …) and never convert implicitly: `f32(n)` (§4).
- A string literal is `Text`; `String` grows; interpolation is `f"…"`, not a template literal (§4).
- No `async` and `await` (§6.15).

## How to read this

- **Rules cite the decisions they came from** (D-NNN), for history. This document is the current state.
- **Every feature has a tier** (D-088):
  - **T0:** milestone 1, the first program that draws a field.
  - **T1:** milestone 2.
  - **T2:** milestone 2, except a compiler in the browser, which is a non-goal (vision.md).
- **Nothing here is open.** What was open when milestone 1 ended is settled, and §21 says where. Each rule names its conformance case's ID (`area.rule`) or the test that holds it.
- **The compiler knows nothing about the engine** (D-050). Nothing in this document mentions creatures, meshes, frames or sim state, except in examples. A feature that only makes sense in a game belongs in the engine.

---

## 1. What wrela is

- **One language for CPU and GPU** (D-005, D-009). The same source compiles to WASM for the CPU and to WGSL for the GPU. The compiler lays out data that crosses between them.
- **Compiled ahead of time** by a Rust program on the developer's machine (D-007, D-069). A game ships WASM, WGSL and data. There's no compiler in the browser.
- **Value semantics, no garbage collector** (D-014). Memory is values, arenas and handles. Every copy, move and mutation is visible where it happens (§6).
- **Generic code is monomorphized** (D-070, D-071). A value's structure is its type, so specialization is ordinary monomorphization and runtime numbers travel as data.
- **The compiler derives interpretations of pure numeric code:** gradients and intervals, which nest, so the interval of a gradient is a local Lipschitz bound (D-012). Fields are the motivating use, but nothing about it is field-specific.
- **Determinism is a checked property** (`@deterministic`, D-052), with strict floats on the CPU (D-074).
- **Built for agents** (D-021): a familiar Rust/TS/Swift-family syntax, a small spec, machine-readable diagnostics, semantic queries.

### Targets (D-050)

The compiler knows about exactly these execution targets:

| Target | Emitted as | Notes |
|---|---|---|
| CPU, main thread or worker | WASM | Strict floats (D-074) |
| Audio worklet | WASM | `@audio` context (D-072) |
| GPU | WGSL | Compute, vertex and fragment entry points |

---

## 2. Lexical structure and statements

| Rule | Tier | Decisions |
|---|---|---|
| A newline ends a statement, unless it's inside open brackets or the next line starts with `.` (`lex.newline`) | T0 | D-038, D-079 |
| A binary operator that continues a line must *trail* the line; a leading `-` or `\|` starts a new expression (`lex.trailing-op`) | T0 | D-079 |
| `;` may separate statements on one line; the formatter normalizes (`lex.semicolon`) | T0 | D-038 |
| `else` goes on the line of the `}` before it: a line break after `}` ends the `if` (`lex.else`) | T0 | D-079 |
| `//` comments, `///` doc comments; no block comments (`lex.comments`) | T0 | spec/lexical.md |
| Files use the `.wrela` extension | T0 | D-040 |
| Outside comments and strings, only the characters spec/lexical.md lists (L3: no `$`, `#`, backticks or no-break spaces) (`lex.chars`) | T0 | spec/lexical.md |
| An integer literal must fit the type it's used as (`lex.literals`) | T0 | D-074 |
| Number literals have no type suffixes (no `1.0f32`) (`lex.no-suffix`) | T0 | D-025 |
| A unit can follow a number as a suffix: `15cm` means `15 * cm`, which is 0.15. Units are constants, in SI (§5). | T1 | D-025 |
| `**` is exponentiation; `^` is XOR (`lex.pow-xor`) | T0 | D-076 |

```wrela
let r = ellipsoid(radii: vec3(0.45m, 0.50m, 0.90m))
    .smooth_union(haunch, k: 15cm)      // a leading `.` continues the expression
    .displace(fbm(freq: 25 / m, octaves: 4), amp: 3mm)

let total = base +                      // a continued line ends with the operator
    extra
```

**Keywords** are listed in `spec/lexical.md` (L11), including the reserved ones, which no feature uses (`dyn`, §18).

**Control flow (T0)** is expressions and statements in the Rust family: `if`/`else` and `match` are expressions; `for i in 0..n` (and `0..=n`) counts over integers, `for x in xs` walks an array or a run, and `_` names an unused loop variable. Over a sequence, a pattern destructures each element as `let` does, and can't fail (E0309): `for (a, b) in pairs`, `for Spring { rest, k } in springs`; `for mut` binds the parts as mutable projections (`stmt.for-pattern`); `while`, `loop`, `break`, `continue` and `return` work as usual. A block's value is its last line when that's an expression. A local that's bound and never used is a warning (W0001), unless its name starts with `_`, and a `var` that nothing changes is one too (W0006: it's a `let`). A statement that calls a function only to throw its value away, where the call does nothing else (no effect, §8, and no argument lent `mut`), is an error (E0333): `p.normalize()` gives a new vector and leaves `p` as it was, and `let _ = f()` discards a value on purpose (M3: the mistakes that compile and are quietly wrong are the ones nobody finds). A `let` may shadow an earlier binding of the same name. **Tier 1 adds** `if let PATTERN = EXPR { … } else { … }` (`stmt.if-let`), and `let PATTERN = EXPR else { … }`, whose `else` block must leave the scope (`return`, `break`, `continue` or a panic; `stmt.let-else`). `while let PATTERN = EXPR { … }` runs its body while `EXPR` matches, evaluating it again each time: `while let Some(b) = stack.pop() { … }` (`stmt.while-let`). A literal's type is inferred from the whole function, later uses included: after `let a = 1` and `let b: f32 = a`, `a` is an `f32`.

**Limits.** Expressions, blocks, types and patterns nest at most 128 deep, and an expression's tree is at most 512 deep (E0112). A generic function's instantiations go at most 256 calls deep, and their type arguments, like any value's type, have at most 4096 parts, counting a part each time it appears (E0412, E0329): a type that doubles with each step, like `(a, a)`, grows past any machine. A value larger than the CPU stack traps when the function holding it is entered, as a stack overflow does; in GPU code, a type larger than WGSL allows (2³¹ − 1 bytes) is an error (E0702).

---

## 3. Items

### Functions (T0)

- **Named arguments are optional, Kotlin-style** (D-039). Positional arguments come first, and once an argument is named, the rest must be named too. Parameters may have defaults: a literal value, or any other constant expression, which the build computes once (§10). A default whose type is generic must be a literal (E0324): a constant has one type. A default that isn't `Copy` is cloned where a call takes it (`fn.named-args`, `fn.defaults`, `run/const_defaults`). Arguments are evaluated in the order they're written, named ones too, whatever the order of the parameters they go to. A bare literal passed by position where a swap would still compile is a warning with a fix that names it (W0005): two neighbouring parameters of one type both given bare numbers (`round_cone(a, b, 0.09, 0.06)`), or a bare `true` or `false` to a function that takes more than one argument. A `select` whose two values are passed by position is W0005 too, since its order (the value for `false` first) is easy to swap: `select(if_false: a, if_true: b, cond: c)`.
- **Parameters have modes** (§6): `x: T` (borrow), `x: mut T`, `x: take T`.
- **A function that returns a value** ends with it, or returns it, on every path (`fn.return`; E0314). One that returns nothing ends with a statement or a `()`.
- **Properties are attributes** (§9): `@deterministic fn step(...)`.
- **A trait in parameter position** (`fn d(field: Surface, p: vec3)`) makes the function generic over that parameter. It's the usual way to write a generic parameter (§7); write `<F: Surface>` only when the type is named twice.
- **A trait in return position** (`-> Surface`; `Field<C>` in tier 1) names one concrete, inferred type, like Rust's `impl Trait` (D-070, `fn.return-trait`). Callers see only its traits. Several traits can be joined with `+`, in return position or parameter position: `-> Field<Tissue> + Lipschitz`, `f: Field<C> + Lipschitz` (milestone 2; sketch 01's parts needed both).

```wrela
fn leg_segment(len: f32, r_top: f32, r_bottom: f32 = 0.06) -> Surface {
    round_cone(vec3(), vec3(y: -len), r_top, r_bottom)
}

let leg = leg_segment(0.45, r_top: 0.09)   // positional first, then named
```

(Sketch 01 writes this with unit suffixes, `6cm`, and a field type with channels, `Field<Tissue>`: both tier 1. Its kind parameter, `Exact`, became the `Lipschitz` trait: §17.)

### Structs (T0)

- **Fields may have defaults**, as parameters may (§3's functions, §10, D-048). A struct literal may omit defaulted fields (`struct.defaults`).
- **A struct opts in to traits in its declaration:** `struct Tissue: Blend { ... }` (D-026, D-078). `Copy`, `Clone`, `GpuData` and `Plain` are structural: every field must have the trait (`struct.opt-in`). Declaring `Copy` implies `Clone`, so declaring both is a warning with a fix (W0004). A trait set names several traits at once: `struct Gate: Sim` (§7, `trait.sets`). A `@fieldwise` trait is derived (below); any other trait a type declares needs an `impl` (E0401). A generic type's declared trait holds for the instantiations whose type arguments have it: the prelude's `Option<T>: Copy` makes `Option<f32>` `Copy`, and `Option<Log>` isn't when `Log` isn't.
- **`..base`** fills the remaining fields from another value of the same type, as if each were written `field: base.field` (`struct.base`): a `Copy` field is copied, and any other moves, so `..take base` or `..base.clone()` is written for a value that isn't all `Copy` (E0501, E0502).

```wrela
pub struct Look {
    pub tolerance: f32 = 0.002,
    pub resolution: u32 = 1024,
}

const FINE: Look = Look { tolerance: 0.001 }   // resolution keeps its default
```

### Enums (T0)

Enums are sum types with payloads, matched with `match`, which must be exhaustive (`enum.match`; a match with too many cases to check in bounded time is E0309 too, until it ends with `_`). They're how structure is chosen at runtime from a known, finite set: every case is compiled, with a uniform branch (D-070).

```wrela
pub enum Edit: Copy {
    Dig { at: vec3, radius: f32 },
    Fill { at: vec3, radius: f32 },
    Clear,
}

fn reach(e: Edit) -> f32 {
    match e {
        Edit::Dig { radius, .. } => radius,
        Edit::Fill { at, radius } => radius + length(at),
        Edit::Clear => 0.0,
    }
}

let e = Edit::Dig { at: vec3(), radius: 1.5 }
```

### Traits and impls

- **Traits have associated types and default methods** (D-071). T0 (`trait.items`).
- **An impl gives what its trait declares, and nothing else:** its methods take the trait's parameter defaults and can't declare their own (E0405), and associated types belong to traits, not to inherent impls (E0402). A blanket impl bounded only by its own trait (`impl<T: Tr> Tr for T`) implements nothing (E0400). T0 (`trait.items`).
- **Method syntax finds a trait's methods only where the trait is visible:** it's `pub`, or the calling module declares it. A private method is private to its module (E0203). T0.
- **Coherence follows Rust's orphan rule:** an `impl` lives in the package of the trait or of the type; std is another package (D-071). T0 (`trait.orphan`). `Copy`, `Clone` and `GpuData` aren't implemented with an `impl`; a type opts in to them in its declaration. An impl for every closure or function of one signature, `impl<F: Copy + GpuData + fn(vec3) -> f32> Shape for F`, applies to just those: a closure whose signature doesn't fit, or a struct, doesn't have the trait, so the impl doesn't conflict with one for a struct.
- **Fieldwise traits** (T1, `trait.fieldwise`). A trait declared `@fieldwise` is derived for every type that declares it, field by field. For a struct, the trait's method is applied to each field in order. A parameter of type `Self` is taken field by field too: `blend(self, other: Self, t: f32)` blends each field with the same field of `other`. A method that returns `Self` builds the struct from its fields' results, and one that returns `Result<Self, E>` does the same, stopping at the first error. For an enum, the variant comes first, then its fields; a method that builds an enum chooses the variant through the trait, which is how loading works. The trait chooses the variant and learns each field's name through its hooks (§21). `Clone`, `Eq`, `Ord`, `StateHash`, `Serialize` and `std::field`'s `Blend` work this way, and so can a library's own traits. There's no reflection (§18).
- **`==` and ordering come from declared traits** (T1, `trait.eq-ord`). A type that declares `Eq` gets `==` and `!=`, and one that declares `Ord` gets `<`, `<=`, `>` and `>=`. Both are `@fieldwise`: fields compare in order, and an enum's variants compare in the order they're declared. Without `Eq`, `==` works only on numbers, `bool`, vectors and enums whose variants hold nothing, which compare by variant (E0305).

  ```wrela
  @fieldwise
  pub trait StateHash {
      fn state_hash(self, h: mut Hasher)
  }

  impl StateHash for f32 {                       // the leaves are written by hand
      fn state_hash(self, h: mut Hasher) {
          h.write_u32(canonical_bits(self))      // every NaN hashes alike
      }
  }

  struct Dig: Copy + StateHash { depth: f32, radius: f32 }   // derived: `depth`, then `radius`
  ```

### Constants

A `const` is computed when the program is built (§10, D-073). Its initializer can be any expression, function calls included, as long as its effects allow it: no IO except `embed`, no host calls, nothing non-deterministic (§8). A failed `assert` or a panic in it is a compile error, with the call chain (`const.build`). A constant is a place that lives as long as the program: a use reads it or projects part of it, and a function can return a projection of it, but nothing moves out of it or changes it; `.clone()` gives an owned copy (`const.places`). A literal value is folded where it's used; anything else is computed once, by the build.

```wrela
const HIDE_DENSITY = 1050 * kg/m**3                       // 1050.0, in kg/m³ (§5)
const GAIT = GaitNet { weights: embed("grazer_gait.wnn") }   // read in place: no parsing at load
const HOOF_MODES = modal_modes(hoof())                       // an eigenvalue solve, run by the build
const GRAZER = grazer_skeleton()
const CHEST = GRAZER.bone("chest")                           // a misspelled name fails the build
```

Staging is never left to the optimizer (D-072): work that must happen early is written as a `const`, or at load.

### Modules and packages

**Modules (T0):** a program is one package, a directory with a `main.wrela`. A file is a module and a directory is a module tree: `shapes/blob.wrela` is `shapes::blob`. At the package's top level, `build`, `results`, `node_modules` and `target` hold outputs, and are never modules. `use` imports names; `pub` makes an item visible outside its file. In `main.wrela`, `pub` also makes a function an export (§12). A `use` path starts at the package root, or at `std::`; there's no `super::`. A module has one namespace for all its items. The stdlib's root is `std::` (D-081), so no module of the package can be named `std` (E0201). A symbolic link to a directory is refused rather than followed (E0208), until packages decide what a module tree is (`mod.use`, `mod.names`).

```wrela
use shapes::blob::blob      // from shapes/blob.wrela
use std::gpu::dispatch
```

**Packages (M2).** A package is a directory of modules. One that has dependencies, or uses `unsafe`, has a `wrela.toml`; one with neither needs none (`mod.packages`):

```text
[package]
name = "herd"                          # how diagnostics name it
unsafe = true                          # it may use `unsafe` (§6.14; E0214 otherwise)

[dependencies]
fieldkit = { path = "../fieldkit" }    # a local path, relative to this file
```

- **A dependency's modules are named under it:** `use fieldkit::shapes::blob`. A path's first name is `std`, a module of the package, or one of its dependencies, and one name can't be both (E0201).
- **Only `pub` items cross a package boundary.** `pub(package)` makes an item, a field or a method visible to every module of its package and to no other (E0203).
- **The orphan rule is per package** (D-071): an `impl` lives in the package of its trait or of its type, and std is a package too (`trait.orphan`). std's privileges (its core's `unsafe`, intrinsics, destructors and lang items) come from its being std's package.
- **A directory with its own `wrela.toml` is another package,** never part of this one's module tree. Only the program's `main.wrela` is its interface to the host (§12); in a dependency, `main.wrela` is an ordinary module.
- **Dependencies are built from source with the program.** A dependency can't lead back to a package that depends on it (E0221), a `path` that isn't a directory is E0220, and a manifest that isn't valid is E0219. A package two others depend on is built once. Versions, lockfiles and registries are #31.
- **A diagnostic in a dependency names the package and the path inside it:** `[fieldkit] shapes/blob.wrela:3:5`. Builds with dependencies are byte-reproducible, from any path.
- **Effects are inferred across packages** as within one: exported functions don't state them (§8, §18, D-030).

---

## 4. Types

| Type | Meaning | Tier | Decisions |
|---|---|---|---|
| `bool`, `i32`, `u32`, `f32` | Scalars, on CPU and GPU | T0 | D-074 |
| `f64`, `i64`, `u64` (`ty.cpu-only`) | CPU only. GPU code is type-checked against what WGSL has. | T0 | D-074 |
| `u8`, `i8`, `u16`, `i16` | CPU only: WGSL can't hold them in a buffer (E0407), so GPU data packs them into a `u32`, as the lossy encodings do (`Unorm8x4`) | T0 | D-074 |
| `vec2`, `vec3`, `vec4`, `mat2`, `mat3`, `mat4` (`ty.vectors`) | f32 vectors and square matrices. A vector's components are read and written by name, alone or swizzled (`v.x`, `v.zyx`, `c.rgb`); a swizzle that's written can't name a component twice (E0317). `Quat` is tier 1. | T0 | D-076 |
| `[T; N]` (`ty.arrays`) | Fixed-size array; `[x; N]` repeats a `Copy` value. GPU code has no empty arrays (WGSL's), so there N is at least 1. Generic code can range over the length (`ty.const-generics`): `fn sum<const N: u32>(xs: [f32; N])`, `impl<F: Surface, const N: u32> Surface for [F; N]` (T1). A length is a constant expression with no calls, so a type never waits on build-time code; a struct's length argument is one too, a number or a constant holding one: `Bounded<Seen, MAX_SEEN>`. | T0 / T1 | |
| `[T]`, `mut [T]` (`ty.runs`) | A run of `T`, borrowed or mutable: the language's one view type. Its elements are contiguous bytes only where they're viewed as bytes (§6.10). It's a parameter's type, a function's result or a local binding, never a field or a type argument (§6.6). An array passes for a run. `xs.len()` is a run's or an array's length, a `u32`. A run of a type whose values hold nothing (`()`, a struct without fields) isn't supported yet (E0702), nor is a `GpuBuffer` of one. | T0 / T1 | §6.2 |
| `(A, B)` (`ty.tuples`) | Tuple | T0 | |
| `type Name<T> = Type` (`ty.alias`) | A type alias: another name for a type, with parameters or without. An alias can't name itself, directly or through others (E0318), and is used with its parameters (E0322). | T1 | §21 |
| `type Name = Traits` (`ty.opaque-alias`) | An alias that names traits, as a return type can (`-> Surface`): `type GrazerField = Parts<Tissue>`. It names one type, the one the first function in its module whose result is the alias returns, and everywhere it's seen through those traits alone. So a value a function builds can be stored and named (a struct's field, a generic's argument) without writing its type: "structure is types" (D-070) makes that type long, and it changes whenever the function's body does. Other functions that return it return a value of it (E0300). It has no parameters, the function that decides it isn't generic, and a function in its module must return it (E0410). | T1 | |
| `Option<T>` (`ty.option`) | An ordinary enum in the prelude (`Some`, `None`); there's no null | T0 | §6.1 |
| `Result<T, E>` and `?` (`err.try`) | Recoverable errors | T1 | D-061, D-088 |
| `String`, `str`, `Text` | Heap-owned UTF-8; a borrowed run of it; and text known at build time, the type of a string literal: a `Copy` handle to read-only UTF-8 in the build, which can be stored anywhere | T1 | D-087 |
| `Bytes` | Bytes known at build time, what `embed` gives (§10): a `Copy` handle to read-only bytes in the build. It passes as a `[u8]`; `b.at(i)` is byte `i`. | T1 | |
| `f"…"` (`ty.fstrings`) | String interpolation: `f"Weight: {w:.1} kg"` builds a `String`. Each `{expr}` or `{expr:spec}` calls std's `Format` trait, which numbers, `bool`, strings and `Text` implement; `{{` and `}}` are literal braces. It allocates, so it's not for GPU or `@audio` code. | T1 | |
| `borrow T`, `mut T` | Projection types: a function's result or a local binding, never a field of an ordinary type or a type argument (§6.6). | T0 | D-064, D-084 |
| `borrow struct` (`mem.borrow-structs`) | A named group of projections: its fields are projections, runs, `Copy` values and other borrow structs. It's a projection itself (§6.6). | T1 | D-064 |
| Closures | A closure that captures a projection can't outlive its call (T0). One that captures only values can be stored, and its type is its own (T1, §6.7). | T0 / T1 | D-064 |
| `Handle<T>`, `Arena<T>`, `Vec<T>`, `Box<T>` | Stdlib containers (§6) | T1 | D-065 |
| `Unorm8x4`, `Half2`, `Oct32` | Lossy encodings, always explicit types, in `std::gpu`: four [0, 1] values in 8 bits each (within 0.5/255), two half floats (f16, within 2⁻¹¹ relative in its normal range), and a unit vector, octahedral, 16 bits a coordinate (within 1.5 × 10⁻⁴ rad). Each is one `u32` that `encode` makes and `decode` reads, with the same code on both targets (`encodings/`). There's no `f16` scalar: WGSL has one only with an optional feature. | T1 | D-049 |

**`&T` doesn't exist** (D-064). There's no reference type to store, so there are no lifetimes.

Implicit conversions don't exist either: an integer literal can be any numeric type, but a value converts only with a call such as `f32(n)` or `u32(i)` (`ty.scalars`). `dyn` is reserved and never accepted (§18): its diagnostic says to use an enum or a generic (`ty.no-dyn`).

---

## 5. Units (T1)

Unit suffixes are constants (D-025). There are no unit types (§18).

- **Every physical quantity is SI:** metres, seconds, kilograms and radians. An `f32` that holds a length holds metres; names and doc comments say which quantity it is.
- **Suffixes convert to SI at compile time:** `15cm` is `15 * cm`, which is 0.15, and `90deg` is π/2. The product is exact, rounded once to the type the number is used as, so `15cm` is the same number as `0.15` in `f32` and in `f64` (`15.0 * 0.01` in `f64` isn't). Compound units are arithmetic on the constants: `1050 * kg/m**3`.
- **Unit suffixes resolve in their own namespace,** which locals can't shadow. `2m` means metres even if a local is named `m`.
- **Constraint:** no unit may collide with numeric syntax. There's no unit named `e`.
- **A bare number meant in another unit still compiles.** Write the suffix: `sin(90deg)`, not `sin(90)`.
- **Units that can't meet are an error where they're written:** when both sides of `+`, `-` or a comparison are built from unit suffixes and numbers alone (`2m + 3s`, `1km < 30deg`), their dimensions are known, and two that differ are E0305, which names both as written (`units.rs` in the checker). A bare number fits any unit, and a variable's unit isn't known, so `let t = 3s` then `2m + t` compiles.

```wrela
let density = 1050 * kg/m**3      // 1050.0: kg/m³ by the SI convention
let reach   = 15cm + 0.3          // 0.45 m: both are metres
```

If dimension bugs show up in practice, the first remedy is distinct types, as Go's `time.Duration` is (#31).

---

## 6. Memory

**Goals:**

- **Memory-safe without a garbage collector** (D-014). Frame times stay predictable.
- **Every transfer is visible.** Nothing is copied, moved or mutated without it showing at the point where it happens. The one exception: calling a `mut self` method doesn't repeat `mut`, because the method's name already says what it does (§2).
- **No lifetime annotations, ever.** Agents and humans should never have to reason about lifetime parameters.
- **Friendly to determinism.** Behavior never depends on memory addresses, and a snapshot is a clone (D-015).
- **Maps directly onto the targets:** WASM linear memory, WebGPU buffer bindings, and the audio worklet.

**The rules at a glance:**

| Rule | Tier |
|---|---|
| **Everything is a value.** `&T` isn't a type. (`ty.no-ref`) | T0 |
| **Parameters have modes:** borrow (default), `mut`, `take`. Callers write `mut x` and `take x`. A `mut self` receiver isn't marked; a consuming `take self` method on a named place is (`take b.finish()`). | T0 |
| **Moving out of a named place is written `take`.** Deep copies are `.clone()`; small `Copy` types copy implicitly. Temporaries and returned locals need no marker. | T0 |
| **Bindings:** `let` owns, `var` owns mutably, `borrow` projects a place read-only, and `mut` projects it mutably (§6.3). (`mem.let-owns`) | T0 / M2 |
| **Projections:** a function may return `-> borrow T` or `-> mut T` of one of its parameters. Projections never outlive the caller's scope. | T0 |
| **Exclusivity:** while a `mut` access is live, nothing may touch an overlapping place. Disjoint fields don't overlap; every element of a container overlaps every other (`pair_mut` and `split_at_mut` check at runtime). Checked within each function, over its control flow: a loan or a move reaches every path that can follow it, through branches, loops and `continue`. | T0 |
| **No mutable globals, and no interior mutability** like `Cell` or `RefCell`. Shared mutable state lives in an arena. | T0 |
| **A projection must come from a `borrow` or `mut` parameter.** The caller treats a `-> borrow T` result as borrowing every such argument, and a `-> mut T` result as borrowing only the `mut` ones (§6.4). | T0 / M2 |
| **Projections are parameters, results and local bindings only:** `borrow T`, `mut T`, runs, borrow structs and closures that capture projections. They're never fields of ordinary types, or type arguments (§6.6). | T0 / T1 |
| **A closure that captures a projection can't outlive its call.** One that captures only values can be stored (§6.7). | T0 / T1 |
| **Long-lived relationships are handles into arenas,** never pointers. (`mem.arenas`) | T1 |
| **GPU layout:** a declared `GpuData` trait fixes a type's layout to WGSL rules everywhere. This is how tier 0's lossless GPU layout is expressed. | T0 |
| **Rule IDs:** `mem.copy`, `mem.take`, `mem.clone`, `mem.modes`, `mem.receivers`, `mem.bindings`, `mem.projections`, `mem.exclusivity`, `mem.loops`, `mem.no-globals`, `mem.closures` in the conformance suite. | |
| **Byte-level data:** the declared trait `Plain` (§6.10). (`mem.plain`) | T1 |
| **Snapshots are clones** of ordinary values (§6.11). | T1 |
| **Destruction is deterministic:** at the end of scope, in reverse order. Only the stdlib's core defines destructors (§18). (`mem.drop`) | T1 |
| **`unsafe`** exists only for the stdlib's core; packages declare whether they use it. (`mem.unsafe`) | T1 |

### 6.1 Values, copies and moves

```wrela
let a = vec3(1m, 2m, 3m)
let b = a                      // vec3 is Copy: an implicit copy

var log = EditLog::new()
var backup = log.clone()       // a deep copy is always explicit
archive(take log)              // moving out of a named place is written `take`
log.push(edit)                 // error: `log` was moved, so it can't be used here
                               //   help: pass `log.clone()` where it's moved if you still need it here
```

- **`Copy` types copy implicitly.** These are small plain types, like numbers, vectors, `Transform` and `Handle<T>`, that declare `Copy`. Their implementation is structural (D-060).
- **Everything else is copied only with `.clone()`** (the `Clone` trait), **or moved with `take`.** The two words mean what they mean in Rust (D-083).
- **No marker is needed for:** temporaries, as in `archive(EditLog::new())`, or returning a local variable.
- **Destruction is deterministic:** at the end of scope, in reverse declaration order. A value that was moved out isn't destroyed again.
- **There is no null.** Use `Option<T>`.

### 6.2 Parameter modes

| Mode | Signature | The callee may | The caller writes |
|---|---|---|---|
| **borrow** (default) | `x: T` | read | `f(x)` |
| **mut** | `x: mut T` | read and write, but not keep | `f(mut x)` |
| **take** | `x: take T` | own it: keep it, move it, destroy it | `f(take x)` from a named place; `f(T::new())` for a temporary |

**Method receivers** are written `self`, `mut self` and `take self`.
- **A `mut self` receiver isn't marked at the call site:** `list.push(x)`, not `mut list.push(x)`. This is the one stated exception to "every transfer is visible."
- **A consuming (`take self`) method on a named place is marked:** `take builder.finish()`. On a temporary, no marker is needed: `Builder::new().finish()` (D-084).

`[T]` is a run of `T`. As a parameter, it's borrowed like anything else. `mut [T]` is a mutable run.

### 6.3 Local bindings

Bindings use the vocabulary of parameters (§6.2): a binding either owns its value or projects a place.

| | Owns the value | Projects a place |
|---|---|---|
| **read-only** | `let x = …` | `borrow x = place` |
| **mutable** | `var x = …` | `mut x = place` |

- **`let` and `var` own.** Their value is a temporary, `take place`, `place.clone()`, or a place whose type is `Copy`, which is copied.
- **`let x = place` is an error when the type isn't `Copy`,** and the diagnostic offers three fixes: `borrow x = place` to read it in place, `let x = place.clone()` for a copy, or `let x = take place` to move it. So adding `Copy` to a type never changes what a binding borrows.
- **Names bound inside a pattern or a loop project the matched place,** read-only: `match e { Some(log) => … }`, `for g in world.grazers`. `match mut place { … }` (`mem.match-mut`) and `for mut g in world.grazers` make them mutable projections, so a payload can change in place: `match mut slot { Some(s) => s.count += 1, None => {} }`. Matching a temporary owns. A loop borrows its container for the whole loop, so there a copy and a projection can't be told apart.
- **Closures capture by projection or by value** (§6.7).

```wrela
borrow g = world.grazers[h]    // read-only projection: no copy
let p = world.grazers[h].pos   // a vec3 is `Copy`: an owned copy
mut t = world.terrain          // mutable projection
t.edits.push(Edit::Dig { at, radius: 1m })
var spare = world.grazers[h].clone()   // an owned copy, mutable
```

### 6.4 Projections

A function can hand back access to part of one of its parameters, instead of a value:

```wrela
fn grazer(world: mut World, h: Handle<GrazerSim>) -> mut GrazerSim {
    mut world.grazers[h]
}

mut g = grazer(mut world, h)   // `world` stays mutably borrowed while `g` is live
g.root.pos.y += 1m
```

- **Returns are written `-> borrow T` or `-> mut T`.** A plain `-> T` returns an owned value.
- **A projection must come from a `borrow` or `mut` parameter, or from a constant.** You can't project from a local or a temporary (E0508). A constant is a place that lives as long as the program (§10), so a function can return a read-only projection of one: `fn item(id: ItemId) -> borrow ItemDef { ITEMS[id.index] }`.
- **What the result borrows.** A `-> mut T` result borrows only the `mut` arguments, because a `mut` projection can't come from a read-only place (E0512). A `-> borrow T` result borrows every `borrow` and `mut` argument whose type can hold a `T`, by value or on the heap: `s.slice(i, j)` borrows `s`, not `i` and `j`. Either kind of result borrows only arguments that can hold its type. That's conservative, and no sketch needed less, so there are no `from param` annotations (§21).
- **Projections can't be stored** in ordinary structs or collections, or captured by a closure that's stored (§6.7). A borrow struct can group them (§6.6). They never outlive the scope that received them, so no lifetimes are needed.
- **Projection types are never type arguments or fields.** `arena[h]` projects, and a stale handle panics; `arena.contains(h)` checks first. There's no `Option<borrow T>` (§18).

### 6.5 Exclusivity

**Places** are locals, their fields, and elements of containers. Two places **overlap** when one is a prefix of the other. For example, `world` overlaps `world.terrain`, but `world.terrain` and `world.grazers` don't.

**Every element of a container overlaps every other element of that container.** The checker doesn't compare indices.

**The rule:** while a `mut` access is live, from where it's created to its last use, no other access may touch an overlapping place. In a single call, arguments may not overlap if any of them is `mut`. Arguments are evaluated before the call's `mut` access begins, and a `Copy` argument passed by `borrow` is copied then, so it may read a place that a `mut` argument overlaps: `v.push(v.len())`, `s.add(s.count)`. An argument that's a projection still conflicts (E0513).

```wrela
for mut g in world.grazers {
    step(mut g, world, intent)
    // error: `world` overlaps `world.grazers`, which `g` is borrowing mutably
    //   help: pass only the parts that don't overlap `world.grazers`
}

let both = world.grazers.pair_mut(h1, h2)     // two elements at once (`both.a`, `both.b`):
                                              // checked at runtime, panics if h1 == h2
```

- **The check is static and stays within one function.** It never needs to look inside another function, because signatures say everything.
- **A `match` borrows the place it matches until an arm is chosen.** So a guard can't change what a later arm tests (E0507), and an exhaustive match always finds its arm.
- **The only runtime checks are explicit library calls** like `pair_mut` and `split_at_mut`.
- **To move a value out of a borrowed place, swap something in.** std's `swap(mut a, mut b)` exchanges two places, and `replace(mut place, value)` returns the old value: `let stack = replace(mut inv.slots[i], None)`.
- **There are no mutable globals.** State is passed in explicitly. Constants are fine.
- **There's no interior mutability** like Rust's `Cell` or `RefCell`. Shared mutable state lives in an arena and is reached through `mut` access to that arena.

### 6.6 Projections and views

**There is one kind of borrowed thing: the projection.** It's second-class: it's a parameter, a function's result or a local binding, and it dies with the scope that received it. It's never a field of an ordinary type, or a type argument (§18). Every form below follows the same rules:

```text
projection
├── borrow T, mut T       a function's result, or a binding (§6.3, §6.4)
├── [T], mut [T]          runs, the one view type (§4)
├── a borrow struct       a named group of projections (below)
├── a capturing closure   (§6.7)
└── GpuSpan<T>            a view of a GPU buffer (§6.13)
```

**A borrow struct groups projections under one name** (T1), so code that needs the world, the assets and the frame can pass them as one thing:

```wrela
borrow struct DrawCtx {
    world:  borrow World,
    assets: borrow Assets,
    frame:  mut Frame,
    dt:     f32,
}

let ctx = DrawCtx { world, assets, frame: mut frame, dt }   // `mut` is marked, as at a call site
draw_herd(ctx)
```

- **Its fields** are projections, runs, `Copy` values and other borrow structs.
- **Its fields are fixed once it's built,** so it borrows exactly its fields' sources, each in its field's mode. Passing it to a call lends its fields for that call, as passing them one by one would, so it can be passed again afterwards. It's sound for the same reason passing those fields as separate parameters is.
- **It's a projection itself:** a parameter, a function's result or a local binding. It's never a field of an ordinary type, or a type argument. A borrow struct can be generic over types. Copying one copies its projections, and the copy holds the same loans.
- **As a function's result,** it borrows what the call's arguments could give each field, as §6.4's results do: a `mut` field borrows the `mut` arguments mutably, and any other projection or run field borrows every `borrow` and `mut` argument, of a type that can hold the field's. What a returned borrow struct borrows must be the function's parameters, or constants (E0508). `pair_mut` returns one: `let both = herd.pair_mut(h1, h2)` gives `both.a` and `both.b`, checked at run time to be different values (`run/pair_mut`).

**There are no iterator types.** A `for` loop walks runs, arrays, ranges and arenas. A query that finds many items takes a closure, or returns an owned `Vec`:

```wrela
for mut g in world.grazers { g.age += dt }             // an arena
for leg in pose.legs { lift = max(lift, leg.lift) }    // a `[LegPose; 4]` field: a run
grid.each_within(p, 15m, |h| { count += 1 })           // a closure: no iterator type
let mates = grid.within(p, 15m)                        // an owned Vec<Handle<GrazerSim>>
```

This is what removed most of the cost of second-class references (D-058), and it's why wrela needs no lifetimes.

### 6.7 Closures

- **A closure that captures a projection,** `borrow` or `mut`, is a projection itself (§6.6). It can be passed down, and the callee must finish with it before it returns. Its access counts as live for the duration of the call. A closure can be named with `let` (not `mut`), and it borrows its captures while the name is live. A closure can capture another closure that captures only copies: it travels as its value (`run/closures_capture_closures`). One that captures a projection has no value to travel as, and a function-typed parameter is its caller's, so capturing either is E0702.
- **A closure that captures only values,** copied, or moved with `take`, is an ordinary value (T1). It can be returned and stored, for example in a field: `.with_at(|p| dapple(p, seed))`. Its type is its own and is inferred, as `impl Trait` is (§7). A function type, `fn(f32) -> f32`, is still a parameter's type only (`mem.closures`). A named function is a value of its own type, which passes where a function type is expected (`apply(triple, 3.0)`), and so is a type's associated function, its own or a trait's: `Herd::of`, and `R::per_second` in a function generic over `R: Rate`, which calls `R`'s implementation (`fn.values`). A trait's function needs that type (`Rate::per_second` alone is E0212), and its parameters' modes are its own: a function that borrows doesn't pass where one that takes is expected (E0300), and a closure adapts it: `|a| W::work(a)`. A function type gives its parameters modes, `fn(mut Ui)`, and a closure's parameters take their modes from the type it's passed as; a parameter without a mode is borrowed. `_` is a parameter the closure doesn't use, as many times as it likes: `Vec::from_fn(n, |_| None)`. A generic parameter bounded by a function type, `F: fn(f32) -> f32`, keeps the function it's given (it can be stored), and passes it on where its bound fits: the same parameters, modes and result, and every attribute the other asks for. `G: @deterministic fn(u32) -> u32` passes as an `F: fn(u32) -> u32`, but not the reverse (`eff.deterministic-fn`).
- **Escapability comes from the captures.** There's no annotation (§18).
- **Closures of different types can't share a container,** because there's no `dyn`. Deferred work, such as a timer or an event handler, is an enum of actions applied by one `match`. Unlike a closure, an action can be saved.

```wrela
world.grazers.par_each_mut(|g| step(mut g, world.terrain, intent))   // captures a projection: passed down only
let skin = torso.with_at(|p| Tissue { albedo: dapple(p, seed), ..HIDE })   // captures by value: the field stores it
```

### 6.8 Handles and arenas

- **`Arena<T>` stores values in generational slots.** `Handle<T>` is an index plus a generation. It's `Copy`, `Plain` (§6.10), and can be stored anywhere.
- **Access:**
  - `arena[h]` is a projection, either borrow or `mut` depending on context. A stale handle panics.
  - `arena.contains(h)` says whether a handle is live.
  - `arena.remove(h)` moves the value out.
- **Graphs, parent links and "this entity refers to that one" are all handles.** References never last longer than a call.

### 6.9 Allocation

- **Allocation is an effect** (D-010). It's forbidden in GPU and audio code.
- **The global heap** backs `Vec`, `String` and `Box`. They own their memory and free it when dropped.
- **To avoid allocating every frame,** keep a `Vec` and `clear()` it. It keeps its capacity.

### 6.10 Plain data

`Plain` describes data that can be handled as raw bytes. A type declares it, as it declares `Copy`, and the compiler checks that every field has it (§7).

| Trait | Contains | Meaning | Used for |
|---|---|---|---|
| `Plain` | numbers, `bool`, `Handle<T>`, `Bytes`, `Text`, fixed arrays, tuples, `Option`, and structs and enums whose fields are all `Plain` | No pointers. Padding is zeroed and NaNs are canonical (D-074), so a value's bytes are a deterministic function of its fields. They aren't equal for every pair of equal values: `-0.0 == 0.0`. Copy the bytes anywhere. | GPU buffers (D-049), network messages, handles |

- **Heap types (`Vec`, `String`, `Box`) aren't `Plain`.** They can still be sim state: the engine's `SimState` is a declared trait with a structural check (D-078), and sim state is ordinary values.
- **Sim state refers to outside data by a deterministic key,** such as a definition plus a seed, never by a handle into something outside it. A restored snapshot would restore the handle but not what it points into (D-084).
- **WASM is little-endian everywhere,** so the bytes are portable between clients.

**GPU layout.** A type that crosses to the GPU declares `GpuData`, which fixes its byte layout to WGSL's rules wherever its bytes are viewed. Other `Plain` types have a natural CPU byte layout. A type has one byte layout, never two.

**How data is stored is the compiler's choice.** The compiler chooses how a container stores its elements: one after another, field by field (a struct of arrays), or with rarely used fields kept apart. It may choose differently for each container type, on the CPU and on the GPU, because it generates both sides. No program can tell the difference: there are no addresses, projections never escape (§6.6), and a value's bytes exist only where they're viewed: an upload to the GPU, a bitcast, or a network message. There the bytes are canonical, as above. Saves don't depend on layout either, because `Serialize` is derived field by field. Today every container stores its elements one after another. The freedom is reserved so that no feature exposes layout (#31).

### 6.11 Snapshots

- **A snapshot is a clone:** `let saved = world.clone()`, with `Clone` derived (§3). There's nothing else to make sound: it's all values.
- **`clone_into` copies into an existing value and reuses its buffers:** `world.clone_into(mut saved)`. `Clone` provides it, derived field by field, and a `Vec` keeps its capacity, so once `saved` has grown to size, taking a keyframe allocates nothing. It's a memory copy.
- **Rollback** keeps a full copy every few ticks, and replays the inputs since. Clients roll back only their own predicted entities, which are small (D-042).
- **A checksum is the derived `StateHash`** of the whole state. Canonical bytes (§6.10) make it the same on every client.
- **A save is the derived `Serialize`,** with stable field names, so saves survive layout changes and can be migrated (D-084). A repro bundle is a saved state plus the inputs since.
- **Rollback is tested:** over 10,000 ticks with random restores and replays, every replayed tick gives the hash it gave the first time, and taking or restoring a keyframe allocates nothing once the keyframes have grown to size (`snapshots/replayed_ticks_give_the_same_hashes`, which counts the allocator's calls with `std::alloc::allocations`).
- **Cost, measured** (`snapshots/snapshot_costs`, wasmtime on the native host's CPU, 2026-10-04): for 10,000 entities of 256 bytes (2.56 MB), `clone_into` takes 0.12 ms, `clone` 0.24 ms and the derived `StateHash` 0.97 ms. The design estimated 0.3 ms for a copy and about the same for a hash. The hash is slower because each word waits for the word before it: 640,000 multiplies in one chain. (It was 2.2 ms before the CPU inlined small calls, AC12.) If that ever matters, chunked copies and incremental hashes can come back inside `Arena`, as a library optimization (#31).

### 6.12 Threads

- **Platform:** web workers plus SharedArrayBuffer (D-017). Every worker shares one WASM memory, whose maximum is reserved at startup. vision.md describes the thread layout (D-098).
- **Nothing extra is needed to share data between workers.** wrela has no interior mutability and no shared ownership. A value that several workers read can't change under them, and an owned value can always move to another worker (§18).
- **Parallelism goes through data-parallel combinators** (D-062). Exclusivity proves disjointness, so there are no locks in game or engine code. `Vec` has two:
  - `xs.par_each_mut(f)` calls `f(mut x)` for each element.
  - `xs.par_map_reduce(init, map, reduce)` folds `reduce(acc, map(x))` from `init`.

  `f`, `map` and `reduce` are `@parallel fn`s: several workers run them at once, so each writes only its own element, never data it captures (E0520), and does no IO, GPU work or non-deterministic work (E0600, `eff.parallel`). It may read what it captures, allocate and panic; a panic on a worker is the program's panic.
- **Results don't depend on the workers** (`parallel/the_results_dont_depend_on_the_workers`, `parallel/both_hosts_agree_with_any_workers`). A job is cut into chunks of about 64 elements, at most 4096, by its length alone. Workers claim chunks as they're free, but each chunk folds its elements in order, and `par_map_reduce` folds the chunks' results in chunk order. The same program gives the same results with 1, 2, 4 or 8 workers.
- **The layout:** the program's thread and up to 8 workers, each with its own 1 MiB stack, share one memory (`wrela_abi::memory`). Each worker runs its own instance of the module, which a start function lets copy the constants only once. The thread that starts a job runs chunks too, then waits for the rest. A host may run fewer workers; with none, the program's thread runs every chunk.
- **Atomics and queues** live in the stdlib's unsafe core. They're the only interior mutability in the language, and they're built for concurrent use. **Every std type with interior state must be safe to share between workers.** That's a rule on std's core, reviewed when it's written; the compiler doesn't check it.

### 6.13 GPU and audio

- **Modes map onto WebGPU bindings:** `borrow` becomes a read-only storage or uniform binding, and `mut` becomes a `read_write` storage binding.
- **WebGPU forbids aliasing writable bindings** within a dispatch, which matches exclusivity *per binding*.
- **That doesn't cover a single dispatch.** Every invocation holds the same `mut` binding at once, so exclusivity says nothing about how invocations share it (D-084). A kernel's `mut` parameters therefore accept only invocation-safe types:
  - `Slots<T>`, where each invocation writes only the slot keyed by its own ID (tier 0)
    - `Atomics<T>`, `Append<T>` and `AtomicMap` (M2), whose operations happen whole, so invocations can share them (`gpu.atomics`)

  A plain `mut` array parameter is rejected (E0601, `gpu.kernel-mut`).
- **Data that crosses to the GPU must be `Plain` and `GpuData`.**
- **A GPU buffer is an owned value** (D-102, M2). CPU code can't read through it.
  - Passing it to a kernel follows the modes. A parameter the GPU writes (`mut Slots<T>`) takes `mut buf`, and one it reads (`[T]`) takes `buf`, borrowed (E0503 and E0504 as for calls, `gpu.buffer-modes`). In `dispatch(k.bind(values: out, total: mut out), groups: 1)` the arguments overlap, so the call is rejected at compile time (§6.5), not by the host when the command runs.
  - It lives until its owner's scope ends, and a buffer stored in program state lives with that state. Dropping it records its destruction in order with the work that uses it, and the host defers the GPU's release until submitted work that uses it is done (`gpu.buffer-drop`).
  - `GpuSpan<T>` is a projection of a buffer, as `[T]` is of an array: `buf.span(start, count)` to read, and `buf.span_mut(start, count)` (a `GpuSpanMut<T>`) to write. A span binds its range, which WebGPU requires to start at a multiple of 256 bytes; the host checks that. WebGPU's rule against reading and writing one buffer in a pass covers the whole buffer, so two spans of one buffer count as overlapping when bound together, even if their ranges don't: a span borrows its whole buffer. The hosts still check every command, as a backstop. `copy(from: span, to: span_mut)` copies between buffers in recorded order (`run/gpu_spans`).
  - Uploads are explicit copies. Readback is a polled request, and `nondet` (tier 2, §6.15). There's no zero-copy path between WASM memory and the GPU (vision.md).
- **Workgroup-shared memory** (M2, D-093) is a kernel's `mut Shared<T, N>` parameter: `N` `T`s that all of a workgroup's invocations share, which CPU code doesn't pass (`gpu.workgroup-memory`). Between barriers, each invocation writes only its own chunk, which the stdlib assigns by `LocalId` (`let c = shared.chunk(lid)`, then `shared.write(c, k, value)` for `k` below `c.len()`: every workgroup-size-th element from the invocation's index), and any invocation reads any element (`shared.get(i)`). A `LocalId` can't be built or changed (E0601), so chunks don't overlap. `barrier(mut shared)` separates a phase of writes from a phase of reads: the compiler follows each kernel's paths, calls included, and rejects a read of workgroup memory with no barrier since a write, or a write with none since a read (E0609; it doesn't compare indexes, so reading back one's own chunk needs a barrier too). A barrier must sit in uniform control flow, as WGSL requires, and the checker rejects one that doesn't (E0610). Workgroup memory starts zeroed, and a pipeline has at most 16 KiB of it (E0602).
- **`@audio` code** runs on the audio thread, and never allocates, does IO, recurses or records GPU work (E0600, `eff.audio`). A program has one voice: `std::audio::play(state, render)` moves `state` to the audio thread, which from then on calls `render(mut state, mut out)` for each render quantum, 128 samples at 48 kHz, mono. `render` is an `@audio fn`: a function, or a closure that captures nothing, since the audio thread keeps it (E0510). The voice's events come through a `ring::<T>(n)`, whose writer the program keeps and whose reader the voice's state holds: one thread pushes, one pops, so it needs no lock.
- **The hosts run the voice the same way.** Each runs it on its own instance of the module, sharing the program's memory: Chrome in an AudioWorklet, and the native host offline (it plays no sound). A voice whose events are all pushed before it starts renders the same samples in both, bit for bit (`audio/both_hosts_render_the_same_samples`, a modal-synthesis voice, D-004).

### 6.14 The unsafe core

`unsafe` exists only so the stdlib can implement things the checker can't verify:
- `Vec`, `Arena`
- atomics and queues: the only interior mutability, each safe to share between workers (§6.12)
- host bindings

Each package declares whether it uses `unsafe`. Game and engine code shouldn't need to, and the package policy flags it if it does.

### 6.15 Requests, not async

wrela has no `async` (§18). IO and GPU readback are **requests**:
- A call submits the request and returns a `Pending<T>` handle at once (M2): `std::io::load(path)`, `store(path, bytes)` and `fetch(url)` for the program's storage ("bytes at a path") and its build's files, `std::gpu::read(span)` for a GPU readback.
- The program polls the handle on a later tick or frame: `p.poll()` returns `None` until the answer arrives, then `Some(Ok(values))` (a `Vec<T>`: bytes for IO, a buffer's elements for a readback) or `Some(Err(e))` once. The host answers a request no sooner than the next call, and a browser's IO takes as long as it takes.
- Making a request has the `io` effect (a readback `nondet` and `host`), and polling one `nondet`, so `@deterministic` code can't make or poll them (E0600, `eff.requests`). A result reaches the sim only through a tick's input, so it enters on a known tick.
- A path is relative, with `/` between its parts and none of them empty, `.` or `..`; another panics when the request is made. Storage is files under the host's storage directory natively, and the origin's private file system in a browser (`requests.rs`).

A game already has a suspension point, the tick or the frame, so the language doesn't need another. A handle is an ordinary value, so nothing needs `Pin`.

`std::io::post(url, bytes)` is a request to the host that serves the program: it sends the bytes to the URL, relative to the build, and the answer is the host's reply. A browser sends it to the build's own origin and nowhere else (the URL is a relative path, as a fetch's is); the native host answers with whatever its user gives it (`wrela_host::PostHandler`), and fails it otherwise. Tools served by a local server use it (`wrela studio`, §22); a game can use it for its own server.

**Input** (`std::input`) is the pointer and the keyboard. `events()` gives the events the host has queued since the program last read them, oldest first: the pointer moving, a button going down or up, the wheel, a key going down or up (a physical key, `Key::A`), and the text a key types (`Event::Text`, a Unicode scalar value). Positions are in the same pixels as `frame`'s width and height. `Input` keeps what's held and where the pointer is, from the events it's given (`input.read()` reads them, `input.update(events)` takes them as data). A call reads only the events that arrived before it started, in the same order on both hosts: the native host plays a script of them (`wrela-host --input script.json`, and a frame test's `@test(frames: n, input: "script.json")`, §10), and a browser delivers each to the program's next frame.
- **Reading input is `nondet`** (§8), as polling a request is: what a call reads depends on when a person moved the mouse. So `@deterministic` code takes input as data, the events a tick is given, as it takes the time (`eff.input`).

`std::io::print(line)` isn't a request: it shows a line on the host's console (the terminal that runs the native host, a browser's developer console), for whoever is writing the program; a player doesn't see it. It's the `io` effect too, so `@deterministic` code doesn't print (`requests.rs`).

### 6.16 Diagnostics

The errors agents will hit most, and what they say. Each block is checked (`language.rs`): it gives exactly the errors its comments name, with those helps and fixes. A fix is an edit a tool can make, and `wrela fix` makes it when it's the only one (§17).

```wrela
pub struct Herd: Clone {
    pub leader: &GrazerSim,     // error: `&` isn't a type in wrela
                                //   help: hold an owned copy (the fix), a handle (`Handle<T>`) into an arena, or make the struct a `borrow struct`
                                //   fix: hold an owned value
}
```

A struct that holds borrows is a `borrow struct`. Each mistake is one error, and `wrela fix` makes all three fixes: `borrow struct Ctx { world: borrow World }`.

```wrela
struct Ctx<'a> { world: &'a World }
// error: `'a` is a lifetime, which wrela doesn't have
//   fix: make it a `borrow struct`
// error: `&` isn't a type in wrela
//   fix: make it a `borrow` projection
// error: `'a` is a lifetime, which wrela doesn't have
//   fix: remove it
```

An element of an arena is a projection: a copy is `.clone()`, and `remove` takes it out.

```wrela
fn keep(g: take GrazerSim) {}

keep(world.grazers[h])          // error: can't move `world.grazers[h]` here: it's an element of an arena
                                //   help: `world.grazers.remove(h)` takes it out of the arena
                                //   fix: copy it: `.clone()`
```

A closure that captures a projection can only be passed down (§6.7):

```wrela
let f = blob.with_at(|p| p.y - world.terrain.height(p))
// error: a closure that captures `world`, a projection, can't be a generic's type argument
//   help: copy what it needs into a local first (`let x = world.clone()`), or pass the closure straight to a parameter
```

A value used after it moved is §6.1's example, and a `mut` borrow that another argument overlaps is §6.5's. `wrela explain <code>` shows any code's meaning with a wrong and a fixed program.

---

### 6.17 Ticks, hand-offs and long jobs (milestone 4)

A program can run a fixed-rate step on a thread of its own, hand its newest result to the frames, and send long work to a helper. All three are std, built on the unsafe core (§6.14); the engine's `run` (a world on a timeline, #43) is ordinary code on top of them. None of them is specific to games.

- **`std::tick::start(state, hz:, step:, hash:)`** moves `state` to the ticker's thread, where `step(mut state, ticked)` runs `hz` times a second, tick after tick. `Ticked` holds the tick's number, its length in seconds and its **records**: the input events the host stamped at the tick's start, oldest first, at most 256 (the rest wait for the next tick). `step` and `hash` are `@deterministic` (§14) functions, or closures that capture only values (E0510), since they move there too: a tick's state is a function of the first state and the records. `hash` gives the state's hash, which a host asks for when it records or checks ticks. A program has one ticker; a second `start` panics. `std::tick::origin()` is when tick 0 was due, in frame time: a frame places the newest tick with it. It's `nondet`: the host moves it when it drops ticks it couldn't run in time, or the page was hidden.

  ```wrela
  use std::tick::{Ticked, start}

  /// A data logger: it samples a signal at 100 Hz and keeps a running mean, on its own thread,
  /// while the program's thread answers its user.
  struct Logger: Clone {
      samples: u32,
      mean: f32,
  }

  @deterministic
  fn sample(log: mut Logger, t: Ticked) {
      let x = sin(f32(t.tick) * t.dt * 6.2831855)   // the signal at this tick's time
      log.samples += 1
      log.mean += (x - log.mean) / f32(log.samples)
  }

  @deterministic
  fn checksum(log: Logger) -> u64 {
      u64(log.samples) * 4294967296 + u64(bitcast_u32(log.mean))
  }

  pub fn init() -> u32 {
      start(Logger { samples: 0, mean: 0.0 }, hz: 100, step: sample, hash: checksum)
      0
  }

  pub fn frame(state: mut u32, time: f32, width: u32, height: u32) {}
  ```
- **`std::handoff::handoff(initial)`** gives a `Publisher<T>` and a `Latest<T>`, for two threads: `publish(value)` on one, `read()` on the other, which gives the newest complete value. Neither waits, and no value is read half-written: it's a triple buffer, one atomic word between them. `T` is `Plain` (§6.10), so a value is a copy of bytes (E0400 otherwise). `read` is `nondet`: which value it gives depends on timing, so a tick can't read one (E0600). In a debug build each value carries its publish number and a checksum, and a read checks both: none is read torn or older than one read before (`threads/a_hand_off_is_never_read_torn`: 0 in 10⁶ reads, a helper publishing as fast as it can).

  ```wrela
  use std::handoff::{Latest, Publisher, handoff}
  use std::tick::{Ticked, start}

  /// A sensor's reading, as its thread publishes it to the program's UI.
  struct Reading: Copy + Plain {
      sample: u32,
      celsius: f32,
  }

  struct Sensor {
      out: Publisher<Reading>,
      sample: u32,
  }

  @deterministic
  fn measure(s: mut Sensor, t: Ticked) {
      s.sample += 1
      s.out.publish(Reading { sample: s.sample, celsius: 20.0 + 0.1 * f32(s.sample % 7) })
  }

  @deterministic
  fn count(s: Sensor) -> u64 {
      u64(s.sample)
  }

  pub fn init() -> Latest<Reading> {
      let (out, latest) = handoff(Reading { sample: 0, celsius: 0.0 })
      start(Sensor { out: take out, sample: 0 }, hz: 10, step: measure, hash: count)
      latest
  }

  pub fn frame(state: mut Latest<Reading>, time: f32, width: u32, height: u32) {
      borrow r = state.read()   // the newest reading: never half-written, never waits
      if r.sample % 100 == 1 {
          std::io::print(f"{r.celsius:.1} °C")
      }
  }
  ```
- **`std::par::job(input, f)`** starts `f(input)` on a helper, a long job that holds it while it runs, and gives a `Job<O>`; `join()` gives the result, waiting if a helper is still running it. `f` is `@deterministic`, so the result is a function of `input` alone, whichever thread runs it and whenever. A job no helper has started runs inside `join`: with no helpers every job runs there, and a job that joins another can't deadlock. A trap in a job is the program's panic at its `join`. `done()` says whether it has finished, and is `nondet`. A parallel job (`par_each_mut`) inside a job, or inside a `@parallel fn`, runs on its thread alone (`threads/`).

  ```wrela
  use std::par::job

  fn checksum(data: take Vec<u32>) -> u32 {
      var h: u32 = 2166136261
      for x in data {
          h = (h ^ x).wrapping_mul(16777619)
      }
      h
  }

  pub fn verify() -> u32 {
      var data: Vec<u32> = Vec::new()
      for i in 0..100000 {
          data.push(i)
      }
      let pending = job(take data, checksum)   // a helper hashes while this thread goes on
      take pending.join()
  }
  ```
- **The engine's sim** joins a job only at the tick its answer is due (`engine::run`'s `Asks`): if it isn't done then, the ticker waits, so the tick an answer enters on doesn't depend on the helpers. Each thread counts the times it waited at a join (wrela_abi's `JOIN_WAITS`), and the native host logs a tick that waited. Each also counts the blocks it allocated, and the times it found the allocator's lock taken and the tries it spun on it (`ALLOCATIONS`, `LOCK_WAITS`, `LOCK_SPINS`): the native host reports them for each tick.
- **Per-tick input.** Before each frame, a host queues the input events since the last for `std::input::events()`, and the same events become the next tick's records. A tick sees only its records, never the frames' input queue, so its state depends on the records alone.
- **The tick log** (wrela_abi's `ticks`) is a run's ticks as a host ran them: the build's WASM hash, the rate, the first world's hash, then each tick's records and the state hash its step gave. Given the build, the first world and the records, every hash is fixed, so any host can replay a log and check it. Chrome's test mode writes one (`results/ticks.log`), and so does the native host on request.
- **The native host without a GPU:** `wrela-host --no-gpu <build> --ticks N` runs the ticker alone, no frames, and prints each tick's hash; `--log out.ticks` writes the log. `wrela-host --replay <log> [--no-gpu] <build>` replays a log's records and fails at the first tick whose hash differs, naming it. A build of `wrela-host` without its `gpu` feature has these two only: an x86-64 one under Rosetta checks the sim's bits on x86's code generation (`keys/a_run_recorded_in_chrome_replays_on_arm64_and_x86_64`).
- **Test mode's two schedules** (#43 §2.3): in **lockstep**, before frame i the ticks up to ⌊(i + 1)·hz/fps⌋ run and the frame waits for them, the same in both hosts, so frames and hashes compare across hosts; **paced** (`#test&paced=1`), ticks run on the ticker's own clock and neither waits for the other, as in play, for time budgets.

## 7. Generics and traits

| Rule | Tier | Decisions |
|---|---|---|
| **Generics are always monomorphized.** (`gen.bounds`) | T0 | D-071 |
| **Structure is types.** Combinators are generic types, so building a field builds a type, and the field's numbers are plain data that reach the GPU as uniforms. | T0 | D-070 |
| **`impl Trait` convention:** a trait-shaped type in return position is one inferred concrete type; in parameter position it makes the function implicitly generic, and that's the usual way to write a generic parameter. (`gen.impl-trait`) | T0 | D-070 |
| **Choosing structure at runtime:** an enum for a finite set (all cases compiled, uniform branch); `std::stage::interpret` for unbounded structure built at runtime. | T0 / T2 | D-070, D-053 |
| **Control-flow values are data:** loop bounds and comparisons become uniforms; unrolling is the optimizer's choice. | T0 | D-070 |
| **The pipeline-count query** reports how many instantiations each GPU entry point has, and why: `wrela pipelines <package>` lists each entry point's pipelines, their type arguments, and where CPU code dispatches or draws each (`--json` for tools). | T1 | D-044, D-070 |
| **Every trait a type has is declared.** `Plain` and `GpuData` are declared like `Copy`, and checked structurally: every field must have the trait. There are no auto traits (§18). Declaring `Copy` implies `Clone`, and a generic type's declared trait holds for the instantiations whose arguments have it (§3). | T1 | D-078 |
| **Trait sets name traits a type declares or a bound needs together** (`trait.sets`): `trait Sim = Clone + StateHash + Serialize`, then `struct Gate: Sim { ... }`, `fn save<T: Sim>(x: T)` or `fn hash(x: Sim)`. A set stands for its traits wherever traits are listed, so every trait a type has is still declared, by name or in a set. A set can name other sets, but not itself (E0418), and it can have parameters: `trait Scalable<T> = Copy + Scale<T>`. A set isn't a trait, so it can't be implemented (E0419) or used as a type (E0410). | T1 | |
| **Declared traits with a structural check:** a library trait can require that every field also implements it (`SimState` is the engine's example). | T1 | D-078, D-060 |
| **Library-authored diagnostics:** `@diagnostic(...)` attaches a message to a trait or type, for when a bound isn't met. (`trait.diagnostic`) | T1 | D-055 |
| **Effects are checked per instantiation**, through every call: an error shows the chain (`fill` → `scratch`). | T0 | D-071 |
| **A generic parameter that only a bound mentions is inferred** from the one way the argument has that trait: its one declared bound of it (a generic parameter's or a return-position trait's), or its one impl of it. In `fn red<C, F: Field<C>>(f: F)`, a `Field<Color>` argument makes `C` a `Color` (milestone 2, `fn.return-trait`). An impl's parameters are inferred the same way: `impl<C, F: Field<C>> Field<C> for Masked<F>` is found for a `Masked<Ball>` whose `Ball` is a `Field<Tint>` (`run/impl_bound_inference`). | M2 | |

```wrela
/// Generic over any field; monomorphized per concrete field type.
@compute(64)
fn cull<F: Surface>(field: F, grid: Grid, live: mut Slots<u32>, id: GlobalId) {
    let b = grid.block(id.x)
    let r = field.interval(Box3 { lo: b.lo, hi: b.hi })   // `interval` is derived (§13)
    live[id] = if r.contains(0.0) { 1 } else { 0 }
}
```

(compiler/tests/fields/main.wrela has this kernel, culling the grazer's blocks.)

---

## 8. Effects and contexts

**Effects** (D-072): `alloc`, `io`, `nondet`, `recursion`, `host`, `panic`.
- **`panic`** is an explicit panic, a failed `assert`, or a failed unwrap. A target's trap isn't one: §11's table says what each target does on overflow, division by zero and an index out of range, so GPU code can index.
- **`recursion`** is a cycle in the call graph after monomorphization. GPU and `@audio` code forbid it. `@deterministic` code allows it to a fixed depth of 256 calls: the compiler counts the depth, and the 257th call panics on every engine the same way, with "recursion deeper than 256 calls". Engines' stack limits differ (D-015), and a counted limit far below all of them makes the depth the same everywhere. What's counted is every function in a cycle that `@deterministic` code reaches, wherever it's called from; drop and clone glue, which follow the data, aren't. Other recursion on the CPU is limited only by the 8 MiB stack (`run/deterministic_recursion`).

**Each context forbids a subset:**

| Context | Forbidden | Tier |
|---|---|---|
| GPU entry points (`@compute`, `@vertex`, `@fragment`) | `alloc`, `io`, `nondet`, `recursion`, `host`, `panic` (E0600, `eff.gpu`). | T0 |
| `@audio` | `alloc`, `io`, `recursion`, `host` (E0600, `eff.audio`) | T2 |
| A function passed as an `@audio fn` (§6.13) | as `@audio`; a closure captures nothing (E0600 and E0510, `eff.audio`) | T2 |
| A function passed as a `@parallel fn` (§6.12) | `io`, `nondet`, `host`, and writes to data it captures (E0600 and E0520, `eff.parallel`) | T2 |
| `@deterministic` | `io`, `nondet`, `host` (except declared deterministic host calls), and recursion deeper than 256 calls (D-094). A request is `io` to make and `nondet` to poll, and reading input is `nondet` (§6.15). | T1 |
| Derived interpretations (gradient, interval) | `alloc`, `io`, `nondet`, `host` (E0700, `eff.derived`) | T0 |
| A function passed as a `@deterministic fn` | as `@deterministic` (E0600, `eff.deterministic-fn`) | T1 |
| Build-time constants | `io` (except `embed`), `host`, `nondet` | T1 |

- **Inside a package, effects are inferred** (D-010, D-030). Annotations only assert. Errors show the call chain: `extract → foo → bar allocates at line 42`.
- **Effects are inferred everywhere, and never written.** The compiler sees the whole program, and packages are local, so effects aren't stated at a package boundary either (§18, D-030).
- **Public higher-order functions inherit effects from their closure arguments** by default (D-030). `map` is GPU-safe whenever its closure is.
- **Staging is guaranteed or rejected, never best-effort** (D-072). The optimizer may hoist work, but code must not rely on it. Work that must happen earlier is written earlier, as a `const` or a parameter.

---

## 9. Attributes

`@` attributes are a **closed set defined by the language** (D-037, D-081). All of them except `@diagnostic` and `@intrinsic` are part of a type or contract: they change what a function *is*.

| Attribute | On | Meaning | Tier |
|---|---|---|---|
| `@compute(x, y, z)` | functions | GPU compute entry point, with workgroup size | T0 |
| `@vertex`, `@fragment` | functions | GPU render entry points | T0 |
| `@gpu` (`attr.gpu`) | functions | Asserts the function is GPU-safe, so a violation errors at its definition (D-010). A generic one is checked for what its own code does, whatever its type arguments. | T0 |
| `@deterministic` | functions, function types | The determinism constraint (§14) | T1 |
| `@parallel` | function types | The function runs on several workers at once (§6.12, §8) | T2 |
| `@intrinsic` | functions, in std only | The compiler provides the body: GPU commands and built-in math (D-081). Anywhere else it's rejected. | T0 |
| `@fieldwise` | traits | The trait is derived field by field for every type that declares it (§3) | T1 |
| `@diagnostic(...)` | traits, types | A library-authored error message; not part of the type (D-055, D-081) | T1 |
| `@audio` | functions, function types | Code the audio thread runs (§6.13, D-072) | T2 |
| `@test`, `@test(frames: n)` | functions | A test, which `wrela test` runs as the build runs constants, or after `n` of the program's frames (§10) | T1 |

User-defined metadata, if it's ever needed, gets a different syntax, so `@` always means semantics (D-037). An unknown attribute is E0204 (`attr.closed`).

---

## 10. Build-time constants and embedded data (T1)

There's no separate interpreter in the compiler, and no reflection (§18). What's known at build time is:
- **Constants** (§3). The compiler compiles a `const`'s initializer, and every function it calls, to WASM, and runs it in wasmtime during the build. The build runs the program's own CPU code, with the same strict floats and the same traps, so build time and run time can't disagree.
  - Its effects must allow it (§8).
  - A failed `assert` or a panic is a compile error with the call chain. That's how a library checks something at build time, such as a bone name.
  - A fuel limit turns a computation that runs away into a compile error, not a hang.
  - The result can be any owned value: numbers, `Text` and strings, `Vec`s, `Box`es, enums, nested structs. The build reads it out of the module's memory and lays it out as read-only data, with what its pointers point to beside it; a constant `Vec`'s capacity is its length. A constant is a place that lives as long as the program: reading it projects, and `.clone()` gives an owned copy. It can't hold a closure, a run or a borrow struct (E0332), or a handle to runtime state.
  - Constants are computed in rounds: those whose code reads no constant still to be computed first, then those that read them. The code that computes a constant can't read the constant itself, through any call (E0328).
  - A panic, a trap or a failed `assert` is E0704, at the innermost frame in the program's own code, with the call chain. The fuel limit is 2³⁴ units of wasmtime fuel (about one per WASM instruction): under a second of work on a 2024 laptop. Past it is E0705.
  - Constants are values, never types. They're computed after type checking, so a type never waits on build-time code. Array lengths in types are constant expressions with no calls (§4).
- **Tests** (`const.tests`). `@test fn name() { ... }` is a test of code: a free function with no parameters, no result and no generics (E0222). `wrela test <package>` checks the package and computes its constants, then runs each test of the package, in the order written; `wrela test <package> <filter>` runs those whose names contain `filter`, and a filter that chooses none is an error. Its dependencies' tests don't run. A test runs as a constant is computed: compiled to WASM with every function it calls, with a constant's effects (§8). Each test runs on new memory, so one test can't change what another sees.
  - Tests are built as a debug build is (§11): `debug_build()` is true, and a float operation that makes a NaN from operands that hold none fails the test. A test's fuel limit is 2³⁷ units, eight times a constant's: a few seconds, so a simulation can be stepped for a while.
  - A test passes unless it panics, and a failed `assert` panics. A panic or a trap is E0706, at the innermost frame in the program's own code, with the call chain. Past the fuel limit is E0705. `wrela test` exits with status 1 if a test fails (`suite/test_items.rs`).
  - Nothing of a test is in a build, and `wrela check` and `wrela build` don't run tests.
  - **A frame test runs the program first:** `@test(frames: n)` on a function that takes nothing, or the program's state (§12) borrowed; anything else, or an `n` outside 1 to 36,000, is E0222. `@test(frames: n, input: "script.json")` gives the frames a script of input events, a file in the package (runtime/abi `input`): each event is queued before the frame it's for, so a test drives a program's UI as a person would (`ui/`'s widgets are tested this way). `wrela test` builds the program as a debug build, with each frame test exported, and runs it on the native host's CPU: `init`, then `n` frames at 60 a second on a 640 × 480 screen, then the test with the state. The host checks every command a frame records, as both hosts do, and answers storage and fetch requests, with storage of the test's own; it has no GPU, so a readback fails the test. Each call (`init`, a frame, the test) gets a test's fuel, so a frame that never ends fails after the same work on every machine. A failure says in which call: `panicked in frame 12 of 600`. The gameplay paper test's scripted player is one (`sketches/gameplay`).
  - Comparing a rendered frame with a golden image isn't a test of `wrela test`: GPU results differ between devices (§11), so the suite compares frames in Rust, within a tolerance.

  ```wrela
  pub struct Game {
      ticks: u32,
  }

  pub fn init() -> Game {
      Game { ticks: 0 }
  }

  pub fn frame(state: mut Game, time: f32, width: u32, height: u32) {
      state.ticks += 1
  }

  fn halve(x: f32) -> f32 {
      x / 2.0
  }

  @test
  fn halves() {
      assert(halve(3.0) == 1.5)
  }

  @test(frames: 60)
  fn counts_its_frames(game: Game) {
      assert(game.ticks == 60)
  }
  ```
  - A field's natural test is a property at many points: `for i in 0..n { let p = space.sample(seed, i) ... }`. `Box3::sample` (and `Interval`'s and `Box2`'s) gives Sobol's sequence, shifted by `seed`: the first 2ⁿ points split evenly among a box's cells, so a few thousand points cover it. std's own tests check its fields this way: derived gradients against finite differences, intervals against the distances in their boxes, and stated bounds near the surface.
- **A failed `assert` shows what it compared** where the failure is explained: in a test, a constant and a debug build. The operands of its comparison (`==`, `!=`, `<`, `<=`, `>`, `>=`, through `Eq` and `Ord` too) that aren't literals are each evaluated once, before the comparison, and the panic shows those whose types implement `Format`, with their source: ``assertion failed: `got` is 0.33333334``, and text quoted. A message given comes first, and is made only when the `assert` fails, so an f-string costs nothing while it holds. A release build's failed `assert` says only its message, and has no code to format the values: code behind `if debug_build()` isn't in a release build, nor are the functions only it calls. GPU code can ask `debug_build()` too: the engine's vertex pass writes each boundary corner's value for a test only in a debug build (#42 AC2).
- **`embed("path")`,** which reads a file inside the package and gives `Bytes`: a `Copy` handle to read-only data in the build. The path is a string literal, relative to the package's root, with `/` between its parts and no `.` or `..`; a file that can't be read, or a path that leaves the package (a symbolic link included), is E0218. The output depends only on the file's bytes, so builds stay reproducible. Static data lives as long as the program, so a handle to it can be stored. GPU code can't hold `Bytes`.
- **Types,** for monomorphization, and **fieldwise derivations** (§3).

**A `const` bakes its result into the build, so it costs bytes; computing at load costs time on every start.** Choose per value: an eigenvalue solve's result is tiny, so it belongs in a `const`. There are no procedural macros (D-060).

---

## 11. Numerics

**CPU code always uses strict IEEE floats** (D-074). There's no fast-math mode, no reassociation, no implicit FMA contraction and no relaxed SIMD. Transcendentals come from the stdlib, compiled to WASM, never from the host (D-015).

**Tiers:** tier 0 emits WASM, whose float arithmetic is already IEEE-strict apart from NaN bits, and the integer rules in the table below. Its transcendentals are already the stdlib's (`std::math`, computed in f64 and rounded once; within an ulp at every point the tests sample, 20,000 per function in the ranges they choose, which is evidence rather than proof). NaN canonicalization is tier 1, with `@deterministic` (D-088). The checks: the emitted WASM has no relaxed SIMD (a pass over every module), and a program's command-stream hash is the same in Chrome and in the native host. That hash covers every byte the program submits to its host, which is its GPU work and requests, not its state; a state's checksum is `StateHash` (§6.11).

**Vector math uses WASM's 128-bit SIMD wherever the result is the same.** A `vec2`, `vec3` or `vec4` is one SIMD value, and its arithmetic, comparisons and componentwise built-ins are SIMD instructions. A loop also runs four iterations at a time when it counts a `u32` up by one to a bound computed before it (a `for` over `a..b`), and its body is straight-line `f32` code that reads and writes elements only at the counter's index, of arrays and runs it doesn't replace, and keeps nothing from one iteration to the next; the last iterations, and one that would index out of range, run one at a time, so a trap happens at the same iteration with the same elements written. Standard SIMD rounds each lane exactly as the scalar operation does and has no fused multiply-add, so the bits are identical; relaxed SIMD stays forbidden. An operation whose SIMD form behaves differently stays scalar: float-to-int conversion saturates in SIMD but traps in the table below. A reduction keeps its fixed order (§14): a dot product or a length adds its lanes one at a time, in order, and a loop that sums stays one iteration at a time. A debug build runs every loop one iteration at a time, since its NaN checks panic at an iteration. The test suite builds the numerics corpus, a corpus of every vector operation on special values, and programs run for frames, with SIMD and without, and checks that the bits, the command-stream hashes and sketch 03's world hashes (`StateHash`) agree (`simd.rs`).

**GPU code follows WGSL semantics,** and its results are presentation-only: GPU results can't reach `@deterministic` code (§14).

**Numeric semantics** (D-074):

| Case | CPU | GPU |
|---|---|---|
| Integer overflow | Traps in every build. `wrapping_add`, `wrapping_sub` and `wrapping_mul` wrap. | Wraps |
| Integer divide by zero, `MIN / -1`, a shift by the width or more | Traps | WGSL-defined values |
| Float → int | Truncates; out of range or NaN traps | WGSL-defined values |
| Float `%` | `a - b * trunc(a / b)`, exact (C's `fmod`): `a`'s sign, smaller than `b` in magnitude; NaN if `a` is infinite or `b` is 0 | WGSL's `a - b * trunc(a / b)`, rounded at each step |
| Int → int | Keeps the low bits (`u32(-1)` is 4294967295) | Keeps the low bits |
| Out-of-bounds index | Traps | WebGPU's robust access (a value from inside the buffer, or zero). A debug build checks each index, and one out of range sets a flag the host reads after each frame: the host stops with an error that names the pipeline (`suite/bounds.rs`). Not checked: vertex shaders (WebGPU forbids them to write storage), and a pipeline that already binds 8 storage buffers |
| NaN | Canonical (`0x7fc00000`, `0x7ff8000000000000`) wherever its bits are observed, in all CPU code: bit casts (so `Serialize` too), uploads to the GPU (`write` and uniforms), hashing. Nothing else shows a NaN's bits, its sign included. A debug build (`wrela build --debug`) traps where a float operation creates a NaN from operands that hold none (`suite/numerics.rs`, `suite/buffers.rs`). | WGSL |

**A value's bytes are a deterministic function of its fields** (zeroed padding, canonical NaNs). Equal values don't always have equal bytes: `-0.0 == 0.0` (D-074).

**Built-in functions (T0)** are in scope everywhere, on CPU and GPU alike, and apply per component to vectors: `sin cos tan asin acos atan atan2 sinh cosh tanh exp exp2 log log2 pow sqrt inverse_sqrt floor ceil round trunc fract abs sign min max clamp saturate mix step smoothstep`, `length distance dot cross normalize` for vectors, `select(if_false: a, if_true: b, cond: c)` (a `bool` condition; the two values may be of any one type but a closure's or a function's, which is E0702, as choosing one with `if` is; it evaluates both values, and its arguments are the only built-in ones with names), and `bitcast_u32 bitcast_i32 bitcast_f32` (`bitcast_u64`, `bitcast_f64` on the CPU). `dpdx`, `dpdy` and `fwidth` are for fragment shaders, in uniform control flow (WGSL's rule): not inside, or after an early `return` or `break` in, a branch on a value that differs between pixels (E0608, `gpu.uniformity`). The vector functions can also be called as methods: `v.length()`, `v.normalize()`. Integers have the methods `wrapping_add`, `wrapping_sub` and `wrapping_mul`, and 32- and 64-bit ones `count_ones`, `leading_zeros` and `trailing_zeros`: how many bits are set, and how many zeros lead (from the top bit) and trail (from bit 0), as a `u32` (the width for 0). Conversions are calls of the type: `f32(n)`, `u32(x)`, `vec3(x)` (all components x), `vec3(y: 1.0)` (the rest zero), `vec4(v3, 1.0)`. `std::math` has `PI` and `TAU`.

**Evidence:** spike 01 hashed 1M evaluations of the grazer field, a mass integration and 10K raycasts, compiled from Rust with these rules. WASM in Chromium 152, in Chrome 154 and native aarch64 gave identical bits. That's Rust rather than wrela, on one machine.

---

## 12. GPU code

| Rule | Tier | Decisions |
|---|---|---|
| **Entry points:** `@compute(x, y, z)`, `@vertex`, `@fragment`. A workgroup's size is within WebGPU's limits: each dimension at least 1, at most 256, 256 and 64, and 256 invocations in all (`gpu.workgroup`, E0605) | T0 | D-010, D-035 |
| **Builtins are typed:** `GlobalId`, `WorkgroupId`, `LocalId` (compute), `VertexIndex`, `InstanceIndex` (vertex), `FragCoord` (fragment), `ClipPosition` (a vertex shader's output), and `Flat<T>` for values that aren't interpolated, in `std::gpu`. A stage takes only its own (E0602). They, the derivatives and texture sampling exist only in GPU code (`gpu.cpu`, E0607) | T0 | D-046 |
| **Closures are allowed when statically resolved:** monomorphized and inlined; a loop over a fixed-size array may be unrolled. A call in GPU code is inlined, so a kernel reads its uniform data in place, except a large function that takes no uniform data, which stays a function: derived functions called from many places appear once in the WGSL (`compiler/ir/src/opt.rs`; spike 13's pipelines are each under 256 KiB). | T0 (closures) / T1 (iterators) | D-047, D-088 |
| **Layout is automatic but lossless.** `GpuData` fixes a type's layout to WGSL rules everywhere (T0); lossy encodings are explicit types (T1). Nobody pads by hand. | T0 / T1 | D-049, D-084 |
| **A kernel's `mut` parameters must be safe to share across invocations:** `Slots<T>` (each invocation writes only its own slot: the index is the invocation's own `GlobalId`, which code can't build or change), `Atomics<T>` (`u32` or `i32`: `load`, `store`, `add`, `sub`, `min`, `max`, `and`, `or`, `xor`, `exchange`, `compare_exchange`, each whole), `Append<T>` (`push`, into an `AppendBuffer<T>`, whose count is also an indirect dispatch's group counts), `AtomicMap` (`insert`, `add`, `get` of `u32` keys and values, in an `AtomicMapBuffer`) and `Shared<T, N>` (§6.13). CPU code passes each one's buffer `mut`. A plain `mut [u32]` is rejected. (`workgroup.rs`: both hosts give the same results.) | T0 / M2 | D-084 |
| **Entry-point signatures:** a kernel returns nothing; a vertex shader returns a `ClipPosition` or a struct with one `ClipPosition` field (the rest are passed to the fragment shader); a fragment shader returns a `vec4`, which replaces what's in its target, or a `std::gpu::Over`, whose colour is drawn over it (alpha blending, straight alpha; M3, for text and UI). The values passed on are numbers and float vectors (`f32`, `i32`, `u32`, `vecN`, each interpolated or in a `Flat<T>`), at most 16. Parameters are builtins, `GpuData` values (passed as one uniform block), `[T]` buffers to read, `mut Slots<T>`, and, for a fragment shader, the vertex shader's output (`gpu.entry`, `gpu.data`). A pipeline binds at most 8 storage buffers: its `[T]` and `Slots<T>` parameters, and its uniform block when that's over 64 KiB or its layout doesn't meet WGSL's uniform rules. | T0 | D-102 |
| **Uniform vs varying** is the target's own distinction, which WGSL already analyzes. | T0 | D-051 |
| **Workgroup-shared memory and barriers.** Spike 01's `place_vertices` needed them. §6.13: `Shared<T, N>`, a chunk per `LocalId` to write (`chunk`, `write`), `get` to read, and `barrier(mut shared)` between the phases (E0609, E0610). | M2 | D-093 |
| **GPU interval arithmetic widens each result outward** by its operation's WGSL error bound, so it stays conservative (§13). | T0 | D-075 |
| **GPU-resident data is a type.** `GpuBuffer<T: GpuData>` is an owned value: CPU code can create one (`buffer(count)`), pass it to kernels and shaders, write into it, and copy between buffers, but can't read through it. WebGPU can't bind an empty buffer, so `buffer(0)` has room for one (zeroed) element, and on the GPU its `len()` is 1. `GpuSpan<T>` and `GpuSpanMut<T>` are projections of part of one (§6.13). | T0 / M2 | D-102 |
| **A buffer lives with its owner** (M2, §6.13): dropping it destroys it (a `DestroyBuffer` command, after the work recorded before it), and a buffer in the program's state lasts across calls. Handles aren't used again. A call that traps submits nothing, so the buffers it made the host never sees. | M2 | D-102 |
| **A dispatch or a pass can't both read and write one buffer:** passing the same buffer as a `[T]` and a `mut Slots<T>` is rejected at compile time (M2: the arguments overlap, §6.5), and the hosts check every command as a backstop (WebGPU's usage rule). | T0 / M2 | D-102 |
| **Transfers are explicit.** `write(mut buf, at, values)` copies. `dispatch(kernel.bind(a, b), groups: n)` records a dispatch, and `draw(vertex.bind(...), fragment.bind(...), vertices: n)` a draw, between `begin_screen_pass(clear: ...)` and `present()`. **An entry point is bound to its arguments** (`gpu.dispatch`): `k.bind(...)` takes every parameter but the GPU's builtins (and a fragment shader's vertex output), matched as a call's arguments are, positional then named then defaults, so a missing one is E0303 and an unknown name E0302. Each shader in a draw has its own arguments; one that takes nothing is named alone: `draw(cover, shade.bind(scene, field), vertices: 3)`. A bound entry point goes straight into `dispatch` or `draw`; it isn't a value to keep (E0603). `GpuData` arguments travel as one uniform block, `GpuBuffer`s as buffers. A kernel or shader can't be called directly (E0606). Writes and dispatches take effect in recorded order, with no barriers between dispatches. GPU calls carry the `host` effect (`gpu.dispatch`). | T0 | D-102 |
| **A host program exports `frame(time: f32, width: u32, height: u32)`,** called once per frame: a host program without it, or with another signature, is an error (E0703), and so is an export named `memory` (the program's memory has that name). **A program with state** (M2) also exports `init() -> S`: the host calls it once, before anything else, and keeps what it returns for as long as the program runs. Then `frame` is `frame(state: mut S, time: f32, width: u32, height: u32)`, and any other export may take the state first, `mut` or borrowed (`run/program_state`). The state is an ordinary owned value, so GPU buffers it holds last with it. **`main.wrela` is the program's interface to the host:** its `pub` functions are the exports, so code shared between modules goes in other files. A library package has no `main.wrela` and no exports (a top-level file named `main` in another case, such as `Main.wrela`, gets W0003). Exports other than `frame`, with scalar and vector parameters and results, serve tests and tools; a vector crosses as its components (a `vec3` parameter is three `f32`s), and an 8- or 16-bit integer or a `bool` as an `i32`, of which it keeps the low bits (a `bool`, whether it's nonzero). Other types can't cross, and the host can't choose a generic function's types (E0703). | T0 | D-102 |
| **Textures, samplers and passes** (M2, `gpu.textures`). `texture(width, height, format)` makes a colour texture (`Rgba8` or `Rgba16Float`), `depth_texture(width, height)` a depth one, `sampler(filter, address)` and `comparison_sampler(compare)` the ways to read them. Each is an owned value, released when dropped, like a buffer; `tex.write(x, y, width, height, bytes)` copies texels in. A shader takes them as parameters and CPU code passes them by name. `tex.sample(s, uv)` and `depth.sample_compare(s, uv, reference)` pick their detail from neighbouring pixels, so they're for fragment shaders, in uniform control flow (E0607, E0608, as derivatives); `sample_level`, `sample_compare_level`, `load`, `width()` and `height()` work in any GPU code. CPU code can't read texels (E0607). `begin_pass(mut tex)`, `begin_pass_with_depth(mut tex, mut depth)`, `begin_depth_pass(mut depth)` and `begin_screen_pass_with_depth(mut depth)` start passes; each gives a value that borrows its targets until `end()` (or `present()`), so the pass's draws can't bind them (E0506). A pass with depth keeps the nearest fragment. A frame can have any number of passes (`renderer.rs`: both hosts draw the same frame). | M2 | D-102 |
| **Readback is a request:** `std::gpu::read(span)` returns a `Pending<T>`, polled on a later frame (§6.15), whose answer is the span's elements once the GPU work recorded before it is done (`requests.rs`). It has the `nondet` effect, so `@deterministic` code can't call it. | T2 | D-102 |
| **Indirect work** (M2): `dispatch(k.bind(...), groups: buf)` with a `GpuBuffer<u32>` or `GpuSpan<u32>` takes its three group counts from the buffer, and `draw(vs, fs.bind(...), indirect: buf)` its vertex count, instance count, first vertex and first instance, so GPU work can size later GPU work. The buffer is read by the command, so binding it `mut` in the same command is rejected (§6.5). | M2 | D-102 |
| **Indexed draws and draw state** (M4): `draw(vs, fs, indices: buf, indirect: counts)` draws the triangles a `GpuBuffer<u32>` of indices names, its index count, instance count, first index, base vertex and first instance from `counts`. `cull:` (`Cull::None`, the default, `Front` or `Back`) and `depth_bias:` (`DepthBias { constant, slope, clamp }`, none by default) are part of the draw's pipeline, which the host makes when the program loads: build-time constants, written at the draw or in a `const` it names (E0603 otherwise). | M4 | #43 |
| **The GPU's limits** (M2): both hosts open the device with the adapter's own limits, at least WebGPU's defaults, and check commands against them. `std::gpu::limits()` gives them (`nondet`: another GPU gives other numbers), so a program can make a texture or a buffer as large as the GPU allows (`gpu_limits.rs`). Pipelines are compiled for the defaults. | M2 | D-102 |

```wrela
@fragment
fn shade<F: Surface>(pixel: FragCoord, scene: Scene, field: F) -> vec4 {
    let p = scene.hit(pixel.position.xy, field)          // sphere-traced
    let n = normalize(field.gradient(p))                  // the derived gradient
    vec4(scene.light(n), 1.0)
}
```

(examples/hello-field shades this way. With channels and footprints, tier 1, the shader reads only the channels it uses (D-002) and fades noise finer than a pixel (D-077).)

---

## 13. Derived interpretations

The compiler derives these from any function that qualifies under the effect table (§8). It doesn't know what a field is (D-056).

| Interpretation | What it gives | Tier | Decisions |
|---|---|---|---|
| `gradient` | Forward-mode derivative | T0 | D-012 |
| `interval` | A conservative range over a box | T0 | D-012, D-075 |
| Lipschitz bound | Not a compiler feature: a stdlib method, `lipschitz(near)` (§17). Debug builds and tests check it against the derived local bound, the `interval` of the `gradient`. | T1 | D-056, D-092 |

```wrela
use std::derive::{Interval, interval}

pub fn range_over(lo: f32, hi: f32) -> vec2 {
    let r = interval(|t: f32| sin(t) * t, Interval { lo, hi })   // r.lo <= sin(t) t <= r.hi
    vec2(r.lo, r.hi)
}
```

They're `std::derive::{gradient, value_and_gradient, value_gradient_with, interval}`, over a closure or function of an `f32` or a float vector that returns an `f32`, or, for `interval`, a float vector. `value_gradient_with` takes a function that returns `(f32, T)` and gives `(f32, X, T)`: the value, its gradient, and the rest as the function computes it, from one derived evaluation, so a shader gets a field's normal and channels without computing its distance twice (the herd's shading, 30% faster, `derive/a_gradient_carries_the_rest_of_the_result`). `interval` takes the input's box type (`Interval`, `Box2`, `Box3`, `Box4`) and returns an `Interval`, or for a vector result its box, each component's range from one derived function, the same as the per-component intervals (`run/interval_vectors`). `std::field::Surface` provides `gradient`, `sample` (distance and gradient) and `interval` for every surface. The compiler derives them from the function's body, and from every function it calls (`derive.gradient`, `derive.interval`):
- **Gradients** are forward mode, one tangent per input component. They agree with central differences within 3.4e-4 relative, across the test corpus (compiler/tests/tests/suite/derive.rs). Where a vector's `length` is zero and nothing moves it (inside a box's `length(max(q, 0))`), its tangent is zero. Where `min`'s or `max`'s arguments are equal, the tangent is the average of theirs, so std's `smin` has its true gradient on a blend's seam.
- **Derivations nest.** A derived function is ordinary code, so it can be derived again: `gradient` of a `gradient` component is a Hessian row (and once more, a third derivative), and `interval` of a `gradient` component bounds the gradient over a box, which is a local Lipschitz bound. A closure can pass part of its input to an inner derivation as data: `interval(|q: vec4| gradient(|s: vec3| f(s, q.w), q.xyz).x, b)` bounds the spatial gradient over space and time. Spike 13 (the tag `spike-13-field-math`) certifies meshes and an animation's topology from these.
- **Intervals** bound every value a point in the box can give, as the target computes it. On the CPU each rounded result is widened by its rounding (an ulp; four for the stdlib's transcendentals). On the GPU each is widened by twice its WGSL error bound, at least one ulp, plus 2⁻¹²⁶ for flushed subnormals. A branch on the input that could go either way runs both sides and joins their results. A loop whose exit depends on the input can't be bounded, nor can a recursion that runs under a branch on the input (E0701). Integers are exact when single-valued, otherwise their type's whole range. The test corpus encloses every sample of 10⁶ boxes per function, on both targets.
- **What can't be derived (E0700):** code that records GPU work; a function that uses its own derivation (each would need the next); a write to a captured variable that depends on the input, or, for an interval, that runs under a branch on it; and, for an interval, a projection (`-> mut T`) whose place a branch on the input chooses (both sides run, so it would be either place). Writes and reads through other projections are derived like any others.
- **Known limits of the intervals:** a NaN bound becomes infinite on the CPU, but not on the GPU, which may assume NaNs away; WGSL bounds `sin` and `cos` only on [-π, π], and outside it the same absolute error is assumed; a branch run speculatively can still trap on the CPU (an integer overflow, say) even if no point in the box would take it. std's `ellipsoid` bound divides by a length that is 0 at its centre, so a box holding the centre gets an interval that reaches 0, deep inside the body.
- **A branch narrows what its guard tests** (milestone 2, spike 13). On the side where `a < b` holds (or `<=`, `>`, `>=`, through `!`, `&&` and `||`), `a`'s range ends at `b`'s top and `b`'s starts at `a`'s bottom; a strict comparison moves a bound of 0 off it. The narrowing follows back through what the operands were computed from: `let` bindings, a vector's components, `max`, `min`, `+`, `-` and negation. So std's `smin` chooses the smaller distance once, with one branch, and the bound of its gradient on a blend's seam stays between its parts' (the smooth creature certifies to 7.8 mm, `spike13/smins_seams_certify`); and std's `cuboid`, written with one branch on inside or outside, has its face's normal as the bound of its gradient on a face (`spike13/a_boxs_flat_faces_certify`). `normalize`'s range is computed a component at a time from the others' squares, so it's exact where the other components are exactly 0, and a vector's `length` is at least its largest component's least magnitude.
- **A table read at an index that depends on the input** (an array's or a vector's element, as a loft reads its radii) gives the hull of the elements the index's range reaches, held to the array's bounds, joined in a loop the derivation makes: the code doesn't grow with the array, and a read near a point, whose index's range is a few elements, stays tight. One such index per place, into an array of known length whose elements hold no enum (E0700 otherwise). The corpus's `table` reads one with a `u32` index and one with an `i32`.
- **`floor` splits into cases** (milestone 2, spike 13). Where a component's range crosses one integer, its floor isn't one integer, and what's computed from it, such as value noise's hashed lattice values, would get its whole range. So the rest of the block runs once for each case of which integer each component's floor is, in a loop the derivation makes, and the cases are joined; a component that crosses no integer, or more than one, isn't split. The bound of the bark's gradient then tends to the gradient sampled as the boxes shrink (`spike13/the_barks_gradient_bound_tends_to_the_truth`: 1.4× at 0.6 mm, where it was 56×).
- **On the GPU, an interval derivation stays a function** where it's big and takes only scalars, vectors and structs of them without arrays, so nested derivations don't multiply the code; other calls are inlined, as reading a field's uniform data through a copy is slow (the grazer's legs). Spike 13's certifier, which bounds the gradient's three components, is 112 KB and 249 KB of WGSL (865 KiB and 1.73 MiB before) and loads in 0.85 s cold (4.7 s before), with the same GPU time (`spike13/the_gpu_certificate_is_small_quick_and_right`).

**Fields built at runtime** (T2, D-029, D-053) are tapes: `std::stage::Tape` holds up to 128 operations over the point, each writing its own register, and the last register is the field's value. A program builds one while it runs, from an edit log or a sculpting tool: `let d = tape.sphere(center, r)`, `tape.smooth_min(a, b, k)`, and arithmetic, `sqrt`, `sin`, `cos`, `exp`, `min` and `max` over registers. `stage::interpret(tape, p)` evaluates it, and a `Tape` is a `Surface`, so it goes wherever a compiled field goes, on the CPU and the GPU (as a uniform). Nothing about it is special to the compiler: its gradient and interval are derived from `interpret`'s own code. Tapes of three corpus fields agree with their compiled twins within 1e-5 at 10⁶ points on both targets, and pass the corpus's gradient and enclosure tests (`stage/`, `derive/`). `tape.prune(over)` is Keeter's interval pruning: each `min`, `max` or `smooth_min` whose operands' intervals over the box don't overlap becomes the operand it always picks, and what the value no longer uses is dropped, so the pruned tape gives the same values inside the box with less work.

- **Pruning** (`LiveMask`, D-045, D-080) is deferred until a spike measures a gain over the engine's part masks, which use each part's derived interval (#31).
- **Bounds a function states about itself are methods, not facts the compiler knows** (D-050, D-056). A field's Lipschitz bound, and a noise's range and bandlimit, depend on per-individual data such as a displacement's amplitude. So they're ordinary stdlib methods, computed at run time and passed as uniforms (§17). They're cheap where a derived bound would cost too much at run time: segment tracing with derived bounds took 3–50× the evaluations of sphere tracing (spike 13).
- **Bandlimits:** a noise's `bandlimit()` method lets it fade octaves finer than a footprint, instead of aliasing, on the GPU and anywhere else a footprint is known (D-077).
- **Bounds can be scoped** (D-092). `lipschitz(near)` is a bound that holds wherever |f| < `near`.
  - Combinators pass the scope on to their parts.
  - A consumer passes the scope it needs. An interval test over a block that straddles the surface passes at least the block's radius.
  - Why: the stdlib's ellipsoid bound has an unbounded gradient at its centre. A global Lipschitz constant fails there, even though the constant holds everywhere culling, root-finding and sphere tracing look. Spike 01's probe and both authoring agents (D-095) hit this.

---

## 14. Determinism (T1)

`@deterministic` is an effect constraint on functions and function types (D-052).

- **Inside it:** strict floats (always true on the CPU), stdlib transcendentals, canonical NaNs at observation points.
- **Forbidden:** the clock, ambient randomness, GPU readback, unordered iteration, relaxed SIMD, anything that depends on memory addresses (D-015, D-052), and recursion deeper than 256 calls (§8), because stack limits differ between engines (D-015, D-094).
- **A panic is a deterministic trap:** every client traps on the same tick (D-061).
- **Parallelism is data-parallel only** (D-062), through stdlib combinators (`par_each_mut`, `par_map_reduce`). Exclusivity proves disjointness, and captured data can only be read (§6.12). Reductions combine in a fixed order, which the length alone decides, so results don't depend on the workers or how they're scheduled. Per-entity RNG streams keep randomness independent of scheduling too. T2.

The sim/presentation split is an engine pattern built on this, not a language feature (D-052).

---

## 15. Errors (T1)

- **Recoverable failures are `Result<T, E>`, propagated with `?`** (D-061).
- **Bugs panic.** On the CPU, a panic is a trap, and its message says where in the source it happened (from the `wrela.lines` section of the program's WASM). GPU code can't panic; out-of-bounds access sets a debug flag or clamps (D-074).
- **What happens after a trap** is the program's policy. The engine restores its last snapshot and writes a repro bundle (D-087).

---

## 16. Concurrency (T2)

- **Threads:** any owned value can move to another worker, and data shared between workers can only be read (§6.12).
- **No async.** IO and readback are polled requests (§6.15).

---

## 17. The stdlib boundary

- **The compiler knows three things** (D-050): the language, a closed list of stdlib items, and the execution targets.
- **The closed list** (D-081) is every std item the compiler knows by its path, and nothing else: the traits it derives or checks by structure, the types its code has a shape for, the GPU, IO and memory operations it lowers itself (`@intrinsic` in std), and the transcendentals it compiles from std's code. A program can't add to it. `Plain`, `StateHash`, `Serialize`, `Blend`, `Surface` and `Lipschitz` aren't on it: they're ordinary std traits, `@fieldwise` or written by hand (`closed_list.rs` checks this list against the compiler's):
  - `std::prelude`: `Option`, `Result`, `Copy`, `Clone`, `GpuData`
  - `std::gpu`: `GpuBuffer`, `Slots`, `GlobalId`, `LocalId`, `WorkgroupId`, `VertexIndex`, `InstanceIndex`, `FragCoord`, `ClipPosition`, `Flat`, `Over`, `Cull`, `DepthBias`, `dispatch`, `draw`, `buffer`, `write_buffer`, `destroy_buffer`, `copy_buffer`, `GpuSpan`, `GpuSpanMut`, `begin_screen_pass`, `present`, `Texture`, `DepthTexture`, `Sampler`, `ComparisonSampler`, `create_texture`, `write_texture_rows`, `destroy_texture`, `create_sampler`, `destroy_sampler`, `begin_pass_command`, `end_pass_command`, `texture_sample`, `texture_sample_level`, `texture_sample_compare`, `texture_sample_compare_level`, `texture_load`, `depth_load`, `texture_width`, `texture_height`, `read_buffer_command`, `limit`, `Shared`, `Atomics`, `Append`, `AtomicMap`, `AppendBuffer`, `AtomicMapBuffer`, `local_index`, `workgroup_invocations`, `shared_get`, `shared_set`, `workgroup_barrier`, `atomic_len`, `atomic_load`, `atomic_store`, `atomic_add`, `atomic_sub`, `atomic_min`, `atomic_max`, `atomic_and`, `atomic_or`, `atomic_xor`, `atomic_exchange`, `atomic_compare_exchange`, `append_push`
  - `std::io`: `next_request`, `request_status`, `storage_read_command`, `storage_write_command`, `fetch_command`, `print_command`, `post_command`
  - `std::mem`: `take_answer`, `take_input`, `Drop`, `size_of`, `align_of`, `needs_drop`, `read`, `zeroed`, `write`, `drop_at`, `at`, `at_mut`, `at_mut_pair`, `heap_base`, `memory_pages`, `memory_grow`, `load_u32`, `store_u32`, `load_u8`, `store_u8`, `copy`, `fill`, `compare_swap`, `atomic_add`, `atomic_load`, `atomic_store`, `wait`, `notify`, `run_task`, `task`, `thread_block`, `wait_for`, `abort`, `debug_build`
  - `std::derive`: `Interval`, `Domain`, `gradient`, `value_and_gradient`, `value_gradient_with`, `interval`
  - `std::math`: `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `sinh`, `cosh`, `tanh`, `exp`, `exp2`, `log`, `log2`, `pow`
  - `std::par`: `par_each`, `par_each_chunk`, `par_map_reduce`, `par_map_reduce_chunk`, `run_chunks`
  - `std::audio`: `start_voice`
  - `std::tick`: `start_ticker`
  - `std::collections`: `Vec`, `Bounded`, `Box`, `swap`, `replace`
  - `std::string`: `Text`, `Bytes`, `String`, `str_addr`, `str_len`, `str_part`
  - `std::cmp`: `Eq`, `Ord`, `Ordering`
  - `std::fmt`: `Format`, `Spec`, `assert_failed1`, `assert_failed2`
  - `std::arena`: `Arena`, `Handle`
  - `std::lift`: `gradient`, `reads`, `literal_count`, `literal_value`, `literal_built_value`, `set_literal`, `literal_source`, `lifted_files`, `lifted_file`
- **Stdlib modules pass an admission test** (D-081): each would make sense in a program that isn't a game. Acoustics, creatures, terrain and timelines are engine code, in a package of their own (the sketches' `engine`), not in std.
- **The stdlib is written in wrela** with a small unsafe core (§6.14). These files are the core, and the only ones with `unsafe`: `std::mem` (raw memory), `std::alloc` (the allocator), `std::collections` (`Vec` and `Box`), `std::string`, `std::par` (the helpers and jobs), `std::audio` (the voice and its ring), `std::tick` (the ticker) and `std::handoff` (the triple buffer). `closed_list.rs` checks it.
- **The core states what inference can't see** (`closed_list.rs` checks that only the core's files use these two attributes, and the compiler rejects them anywhere else, E0204):
  - `@effects(...)` names effects a std function has that come from another thread or the host through memory, which inference can't see: `Latest::read` and `Job::done` are `@effects(nondet)`, starting the voice and the ticker `@effects(io)`. It's the one place effects are written; everywhere else they're inferred (§8). The effects of the GPU commands, requests and host imports the compiler lowers itself are what it lowers them to.
  - `@thread_entry` marks a std function that a host calls on a thread of its own (wrela_abi's `memory`): its first parameter is the thread's number, which gives it its stack and its block, and its others are `u32`s. It's exported as `__` and its name when the program uses its module: `std::par`'s `worker` (each helper), `std::audio`'s `audio` (the voice's quanta), `std::tick`'s `tick` (each tick). The compiler names none of them.
  - `std::mem::task(run)` gives the number of a task: a function the module calls by number with a chunk and a context's address, as `run_task` and the thread entries do. The voice and the ticker are tasks; a parallel job's chunks are tasks the compiler makes from their closures.

**The module map** (final; `wrela doc std` lists it, and `wrela doc <item>` shows any item):

| Module | Contents |
|---|---|
| `std::prelude` | In scope in every module: `Option`, `Result`, `Copy`, `Clone`, `GpuData`, `Plain`, and `Eq`, `Ord`, `Ordering`, `Vec`, `Bounded`, `Box`, `swap`, `replace`, `String`, `Text`, `Bytes`, `Arena`, `Handle`, `SortedMap` and `Quat` from their modules |
| `std::cmp` | `Eq`, `Ord` and `Ordering`: `==` and `<` for types that declare them (§3) |
| `std::transform` | Matrices for skeletons, cameras and shadow maps, on the CPU and the GPU, in WebGPU's conventions (right-handed, +y up, column vectors, depth 0 to 1): `identity`, `translate`, `scale`, `rotate_x`, `rotate_y`, `rotate_z`, `rotate(axis, angle)`, `from_mat3`, `look_at(eye, at, up)`, `perspective`, `ortho` |
| `std::collections` | `Vec`, `Box`; `Bounded<T, N>`, at most `N` values held in place (an array and how many are in use), `Copy`, and `Plain` and `GpuData` where `T` is, so it can be in a snapshot or on the GPU where a `Vec` can't: `push`, `pop`, `remove`, `clear`, `len`, `is_full`, `Bounded::from(xs)`; `b[i]` projects an element in use (past `len()` panics on the CPU) and `for x in b` walks them, and it hashes, saves and compares only those; `swap` and `replace`, which move values out of places; `Vec::filled(n, value)` and `Vec::from_fn(n, make)` (for a `T` that isn't `Clone`); `sort_by`, a stable O(n log n) sort of a run of `Copy` values. `Vec::sort_by` sorts any `Vec`, and `Vec::sort_by_in` keeps its room in a `Vec<u8>`, so a tick that sorts allocates nothing once it's grown (§6.17) |
| `std::string` | `String`, `Text` (text known at build time), `Bytes` (bytes known at build time: `at(i)`, and little-endian reads at any byte, `u16_le`, `u32_le`, `i32_le`, `f32_le`, `u64_le`, `f64_le`) |
| `std::fmt` | `Format`, which `f"…"` calls, and `Spec`, a hole's format spec |
| `std::arena` | `Arena<T>` and `Handle<T>` (§6.8); `SortedMap<K: Ord, V>`, which iterates in key order, so `@deterministic` code may use it |
| `std::units` | Suffix constants in SI: `km`, `m`, `cm`, `mm`, `um`, `t`, `kg`, `g`, `mg`, `h`, `s`, `ms`, `us`, `rad`, `deg`, `Hz`, `kHz`, `N`, `J`, `W`, `Pa`, `kPa` (§5) |
| `std::math` | The transcendentals for CPU code, compiled to WASM (§11), and `PI`, `TAU` |
| `std::hash` | `StateHash` (`@fieldwise`), `Hasher`, `state_hash` (§6.11) |
| `std::serialize` | `Serialize` (`@fieldwise`), `Writer`, `Reader`, `save`, `load`, `LoadError` (§6.11) |
| `std::field` | `Surface`, `Field<C>`, `Lipschitz`, `Noise`, `Blend`, `Color`, `UnitVec3`, `Cat<T>`; primitives (`sphere`, `ellipsoid`, `round_cone`, `cuboid`, `half_space`), combinators (`union`, `smooth_union`, `intersect`, `translate`, `displace`, `with`, and `union`/`smooth_union` of an array), noise (`value_noise`, `fbm`); provenance (`parts`, `part_at`, `part_distance`, `part_name`, `part_offset`, `named`, `mirror_x`, `PartName`, `part_share`), certified checks (`certify_positive`, `certify_clear`, `certify_apart`, `Certified`), `oval`, `tube`, and measures (`hit`, `top`, `bottom`, `width`, `front`, `back`, `farthest`, `part_named`) (§22) |
| `std::quat` | `Quat`: rotations as unit quaternions, on the CPU and the GPU |
| `std::derive` | `gradient`, `value_and_gradient`, `interval`, and the boxes they range over (§13), each with `sample(seed, i)`: points that spread evenly through it, for checking a property at many points (§10) |
| `std::stage` | `Tape`, `Op`, `Reg` and `interpret`: fields built at runtime (§13) |
| `std::gpu` | Buffers (`GpuBuffer<T>`, `GpuSpan<T>`, `buffer`, `write`, `copy`, `read`), textures and samplers, passes, `dispatch` and `draw`, kernels' outputs (`Slots<T>`, `Shared<T, N>`, `Atomics<T>`, `Append<T>`, `AtomicMap`), typed builtins, lossy encodings, `limits()` (§12) |
| `std::io` | `load`, `store`, `fetch` and `post`, polled through `Pending<T>`; `print` (§6.15) |
| `std::input` | `events`, `Event`, `Key`, `Button`, `Mods`, and `Input`, what's held (§6.15) |
| `std::lift` | A lifted build's literals: `Literal`, `count`, `literal`, their values, sources and files, `set`; `gradient` by literals and `reads`; specs' `Checks`, `Loss` and `Report` (§22) |
| `std::audio` | `play`, the program's voice, and `ring`, a queue to it (§6.13) |
| `std::par` | The workers' protocol behind `par_each_mut` and `par_map_reduce` (§6.12); nothing public |
| `std::mem`, `std::alloc` | The unsafe core: raw memory and the allocator (§6.14) |

### Fields are stdlib code

- **A new primitive is a type that implements `Surface`.** It must be `Copy` and `GpuData` (it travels to the GPU as data), and the combinators and derived methods come with it:

  ```wrela
  use std::field::{Surface, sphere}

  struct Cuboid: Copy + GpuData {
      half: vec3,
  }

  impl Surface for Cuboid {
      fn distance(self, p: vec3) -> f32 {
          let q = abs(p) - self.half
          length(max(q, 0.0)) + min(max(q.x, max(q.y, q.z)), 0.0)
      }
  }

  fn rounded() -> Surface {
      Cuboid { half: vec3(0.5, 0.3, 0.2) }.smooth_union(sphere(0.4), k: 0.05)
  }
  ```
- **A closure of a point is a surface:** `let both = |p| min(ball(p), slab(p))` is a `Surface`, written where it's used, with the derived gradient and interval and every combinator (std's `impl<F: Copy + GpuData + fn(vec3) -> f32> Surface for F`). It has no Lipschitz bound of its own (below), so a field that's sphere traced states one.
- **The field trait is `std::field::Surface`:** a signed distance (`distance(self, p: vec3) -> f32`), with `gradient`, `sample` and `interval` derived, primitives (`sphere`, `ellipsoid`, `round_cone`, `half_space`), combinators (`union`, `smooth_union`, `intersect`, `translate`, `displace`) and noise (`value_noise`, `fbm`).
- **A field returns a distance plus channels** (D-002). Channel structs opt in to `Blend`, a fieldwise trait, so each member's type decides how it blends: `Color` in linear space, `f32` linearly, `UnitVec3` renormalized, `Cat<T>` from the winner (D-026).
- **A field's step safety is a method, not a kind or a fact the compiler knows** (§13, §18). A field that can be sphere traced implements std's `Lipschitz` trait:

  ```wrela
  pub trait Lipschitz: Surface {
      /// A bound on |∇distance| wherever |distance| < near.
      fn lipschitz(self, near: f32) -> f32
  }

  impl<S: Lipschitz, N: Noise> Lipschitz for Displace<S, N> {
      fn lipschitz(self, near: f32) -> f32 {
          self.inner.lipschitz(near) + self.amp * self.noise.lipschitz()
      }
  }
  ```

  Primitives state their bound, and combinators compose it in ordinary code. The result is data, computed for each individual. `.to_bound()` divides a field by it. Debug builds and tests check each bound against the derived local bound, the `interval` of the `gradient` (spike 13). A separate trait leaves tier 0's `Surface` unchanged.
- **Combinators are methods** (D-028): `a.smooth_union(b, k: 15cm)`, and n-ary over a fixed array of one type of part, `legs.smooth_union(k: 6cm)` and `legs.union()`, which fold the parts in order, as the binary ones chained would (`run/nary_union`). There's no operator overloading on fields; vectors and units do get operators.
- **Fields built at runtime** from an unbounded space use `stage::interpret` (D-029, D-053). T2.

---

## 18. What wrela leaves out

**Compared with Rust:**
- **References as types,** and with them lifetime parameters, variance, `'static` and higher-ranked bounds (D-058, D-064)
- **`Pin`:** there's no async, and a request's handle is an ordinary value (§6.15)
- **`Cell` and `RefCell`** (interior mutability), and **`Rc` and `Arc`** in ordinary code (§6.5)
- **Mutable globals** (§6.5)
- **Implicit moves out of named places** (D-064)
- **A garbage collector** (D-014)
- **Procedural macros:** fieldwise traits replace them (§3)
- **Fast-math on the CPU** (D-074)
- **User-defined attributes** (D-037)
- **Null** (§6.1)

**Designs decided against** (by the owner, 2026-10-02):
- **`async` and `await`** (revises D-087). IO and readback are polled requests instead (§6.15).
- **`dyn Trait`** (revises D-071). Enums cover structure chosen from a finite set, generics cover static structure, and `stage::interpret` covers fields built at runtime. `dyn` stays a reserved word, so the compiler can say what to use instead.
- **Auto traits** (revises D-054). Every trait a type has is declared (§7), as D-078 already decided for `SimState`, so no guarantee depends on someone remembering to opt out.
- **Field kinds** `Exact`, `Bound` and `Lipschitz` (revises D-077). The `Lipschitz` trait's bound says the same thing (§17), and the derived interval of a gradient checks it.
- **Facts as compiler attributes**, `@assume` and `@assert` (revises D-057, D-077, D-092). A field's bound depends on per-individual data, so it's a stdlib method (§13, §17). The compiler keeps only what it alone can do: the derived bound that checks it.
- **Angle as its own dimension** (revises D-076). It rejected `sin` of a plain number, which tier 0's code does everywhere. Angles are dimensionless, as in SI (§5).
- **`Span<T>`.** Runs, `[T]`, are the one view type (§4).

**Cut for simplicity** (by the owner, 2026-10-03, after a paper test that rewrote sketches 01 and 03 without them):
- **Regions, `Relocatable` and region-bound values** (revises D-065, D-066, D-084). Sim state is ordinary values. A snapshot is a clone, a checksum is the derived `StateHash`, and a save is the derived `Serialize` (§6.11).
- **Unit types** (revises D-022, D-076). Suffixes are SI constants (§5).
- **A compile-time interpreter, reflection and `@comptime`** (revises D-051, D-060, D-073). Build-time constants run the program's own WASM instead, `embed` gives `Bytes`, and `@fieldwise` traits replace structural defaults (§3, §10). Types are never computed.
- **Projections inside other types:** general non-escaping structs, projection types as type arguments, iterator types and adapter chains (revises D-064, D-084). The one exception is a borrow struct, whose fields are fixed once it's built (§6.6). Loops walk runs and arenas; queries take closures or return a `Vec` (§6.6).
- **`@escaping`** (revises D-064). A closure's captures decide whether it can be stored (§6.7).
- **`Sendable` and `Shareable`** (revises D-062). Without interior mutability or shared ownership, nothing needs them (§6.12).
- **User-defined destructors** (revises D-087). Only the stdlib's core defines them.
- **Stated effects at package boundaries** (revises D-030). Effects are inferred everywhere (§8).
- **The compiler's pruning, `LiveMask`, for now** (D-045, D-080). Deferred to #31 until a spike measures a gain (§13).

---

## 19. A tier-0 program

This is the subset needed for "hello field" (tier 0, D-088): a field, a derived gradient, one compute kernel and one fragment shader. No units or determinism. `compiler/tests/tests/suite/language.rs` builds this block, so it stays true; `examples/hello-field` is the full program.

```wrela
use std::field::{Surface, round_cone, sphere}
use std::gpu::{
    ClipPosition, FragCoord, GlobalId, GpuBuffer, Slots, VertexIndex, begin_screen_pass, buffer,
    dispatch, draw, present,
}

/// A field. Its structure (a smooth union of two primitives) is its type;
/// the radii are data, so every blob shares one pipeline. (In `main.wrela`, a `pub fn` is
/// exported to the host, so this one is private.)
fn blob(r: f32) -> Surface {
    sphere(radius: r).smooth_union(round_cone(vec3(), vec3(y: 1.0), r_top: 0.3, r_bottom: 0.1), k: 0.1)
}

struct Grid: Copy + GpuData {
    origin: vec3, // a `vec3` without a unit is unitless
    cell: f32,
    n: u32,
}

/// One sample per invocation. `Slots` lets each invocation write only its own slot.
@compute(64)
fn sample<F: Surface>(field: F, grid: Grid, out: mut Slots<f32>, id: GlobalId) {
    let i = id.x
    let c = vec3(f32(i % grid.n), f32((i / grid.n) % grid.n), f32(i / (grid.n * grid.n)))
    out[id] = field.distance(grid.origin + c * grid.cell)
}

/// One triangle over the whole screen.
@vertex
fn cover(v: VertexIndex) -> ClipPosition {
    let x = f32(v.index % 2) * 4.0 - 1.0
    let y = f32(v.index / 2) * 4.0 - 1.0
    ClipPosition { position: vec4(x, y, 0.0, 1.0) }
}

/// Normals come from the derived gradient. Nobody wrote it.
@fragment
fn normals<F: Surface>(pixel: FragCoord, field: F) -> vec4 {
    let p = vec3(pixel.position.xy * 0.004 - 1.0, 0.0)
    let n = normalize(field.gradient(p))
    vec4(n * 0.5 + 0.5, 1.0)
}

pub fn frame(time: f32, width: u32, height: u32) {
    var out: GpuBuffer<f32> = buffer(4096)
    let grid = Grid { origin: vec3(-1.0), cell: 0.125, n: 16 }
    dispatch(sample.bind(blob(0.5), grid, mut out), groups: 64)
    begin_screen_pass(clear: vec4(0.0, 0.0, 0.0, 1.0))
    draw(cover, normals.bind(blob(0.5 + 0.1 * sin(time))), vertices: 3)
    present()
}
```

---

## 20. Tiers at a glance

| Tier | Features |
|---|---|
| **T0** | Statements and literals; functions with modes and named arguments; structs with defaults; enums (including `Option`); `const` with literal values; traits with associated types and default methods; monomorphized generics and `impl Trait`; projections and exclusivity; non-escaping closures; scalar, vector and matrix types; `@compute`/`@vertex`/`@fragment`/`@gpu`; typed builtins; invocation-safe kernel outputs (`Slots<T>`); lossless GPU layout through `GpuData`; GPU buffer handles, uploads, dispatches and draws from CPU code (D-102); derived `gradient` and `interval`; WGSL and WASM emission. Built in milestone 1. |
| **M2** | Built in milestone 2: workgroup-shared memory and barriers (D-093); atomics and `Append<T>` as kernel outputs. Changes to tier 0: `borrow` bindings and an owning `let` (§6.3); a `-> mut T` result borrowing only `mut` arguments (§6.4); `Copy` arguments copied before a call's `mut` access (§6.5); owned GPU buffers (§6.13); `Copy` implying `Clone` (§3). |
| **T1** | Built in milestone 2: `if let` and `let … else`; `match mut`; `Eq` and `Ord`; `Text` and `f"…"` interpolation; unit suffixes in SI; build-time constants of any owned value, `embed` and const generics; fieldwise traits; declared traits with structural checks; `@diagnostic`; `@deterministic` and the numeric rules; Lipschitz bounds and bandlimits as stdlib methods; handles and arenas; `Plain`; runs as results and bindings; borrow structs; closures that capture values and can be stored; lossy GPU encodings; `String` and `str`; `Result`, `?` and panics; the pipeline-count query. |
| **T2** | Built in milestone 2: threads and parallel combinators; polled requests for IO and GPU readback; `@audio`; `stage::interpret`. Not built: any compiler tier in the browser, a non-goal (vision.md). |

---

## 21. Settled in milestone 2

These were open when milestone 1 ended. Each is decided, where its section says:

- **Packages and dependencies** (D-087): §3, *Modules and packages* (`mod.packages`).
- **The keyword list:** spec/lexical.md L11, final; a test checks it against the lexer.
- **The closed list of stdlib items the compiler knows** (D-081): §17, checked against the compiler.
- **How a `@fieldwise` derivation chooses an enum's variant and passes each field's name:** the trait's hooks, `m_field(name: Text, ...)` before each field and `m_variant(index: u32, name: Text, ...)` before an enum's fields, or `m_variant(names: [Text], ...) -> u32` to choose the variant a method builds; each has a default body (§3, `trait.fieldwise`). `Serialize` names fields this way, so a save keeps its fields' names.
- **The gameplay paper test's small conveniences,** each on its own:
  - type aliases are in: `type Name<T> = Type` (§4, `ty.alias`);
  - string patterns in `match` are in: `"calm" => ...` matches text (`run/conveniences`);
  - an assignment is a `match` arm (`0 => x = 1.0`);
  - an enum converts to its variant's index: `u32(mood)` (the order they're declared in);
  - an index is a `u32` or an `i32` (a negative one is out of range, which traps); other integer types convert first;
  - tuple structs are out: a struct names its fields, and a pair is a tuple, `type P = (f32, f32)` (the parser says so);
  - labelled `break` is out: `break` leaves the innermost loop; a function that returns, or a flag, leaves more (E0114);
  - reverse ranges are out: a range counts up, `for k in 0..n` with `let i = n - 1 - k` (the parser says so for `(0..n).rev()`).
- **Symbolic links to directories in a package** are refused (E0208): a package is the files under its directory.
- **GPU buffer names, and kernels whose parameters exceed the binding limits** (§12, D-102): the names are `GpuBuffer<T>`, `GpuSpan<T>`, `GpuSpanMut<T>` and the kernel outputs (`Slots<T>`, `Shared<T, N>`, `Atomics<T>`, `Append<T>`, `AtomicMap`). A pipeline binds at most 8 storage buffers, WebGPU's default; one that needs more is an error that names them (E0602), and putting data in fewer buffers is the program's choice.
- **`from param` annotations** (§6.4) aren't needed: a `-> borrow T` result borrows every `borrow` and `mut` argument that can hold a `T`, and no sketch needed less. If real code does, it can narrow later (#31).
- **The stdlib's names for workgroup-shared memory:** `Shared<T, N>`, `chunk`, `write`, `get` and `barrier` (§6.13, `gpu.workgroup-memory`).
- **N-ary combinators over fixed arrays:** `legs.union()` and `legs.smooth_union(k: 6cm)` (§17).
- **`..base`** fills a struct literal's remaining fields from a value (§3, `struct.base`).
- **A general `schedule` construct** for any function's evaluation stays out: sugar for later, only if kernels need it (D-053).

---

## 22. Lifted builds, provenance and the tools (milestone 3)

An agent or a person changes a field by its numbers. Three things in the language make that a tool's job, not a guess: **lifted builds** (a tool changes a number while the program runs), **provenance** (the part of a field that decides a point, and the numbers that move it there), and **`wrela edit`** (a change written back into the source as the author wrote it). The lens (`wrela studio`) is built on them, and so are the command-line tools for agents.

```text
 source ──wrela build --lift──▶ program ──std::lift::set──▶ new value, next frame (CPU and GPU)
   ▲                                │
   │                                ├─ provenance: part_at, part_offset, std::lift::gradient
   └────────── wrela edit ◀─────────┘   (which part, which literal, how much)
```

### Lifted builds

- **`wrela build <package> --lift <name>`** lifts the `f32` literals of package `<name>` (the program's own, or a dependency): each is read from a table, not built into the code. A constant whose value is a literal is built where it's used, so every use of `EAR_H` reads one entry; so is a parameter's or a field's default. A literal that only a computed constant reads isn't lifted: the build runs that code once (§10). A negative literal is one entry, its minus included.
- **The CPU** reads the table from memory. **The GPU** reads it from a storage buffer that every pipeline of a lifted build binds last; before a dispatch or draw, the program uploads the table if it changed since the last upload. So a change reaches both with the next frame, and never needs a rebuild. On the subjects, a lifted build's contact sheet costs 1.05 to 1.09 times the GPU time of a normal build's (`suite/studio.rs`).
- **The build reports every float literal** of the lifted packages in `lift.json`: lifted or not, and why. It keeps each lifted file's text and hash (FNV-1a 64, 16 hex digits), so a tool shows a literal's line and writes a change back.
- **`std::lift`** is the program's view of its table: `count()` and `literal(i)`; a `Literal`'s `value()`, `built_value()`, `set(v)` and `source()` (file, bytes, line, column); `files()`, `file_name(i)`, `file_path(i)`, `file_text(i)`, `file_hash(i)`. A build that isn't lifted has no literals: `count()` is 0.
- **`std::lift::gradient(f, which)`** gives `f`'s value and its derivative by each literal of `which`: a parameter gradient, derived forward mode as `gradient` is (§13), through everything `f` calls, the code that builds values from literals included. It runs on the CPU. `reads(f)` gives the literals `f`'s code can read, through everything it calls.

### Provenance

Every `Surface` (§17) answers which of its parts decides a point. A part is a leaf of its tree of combinators: a primitive, a closure, or a `named` surface.

- `parts()` counts them, numbered from 0 in the order the leaves are written. `mirror_x()` doubles them: those written, then their images.
- `part_at(p)` is the distance at `p` and the part that decides it. Through each combinator it follows the operand the result follows: the nearer for a union or a blend, the farther for an intersection, and the cutter where a cut is the surface.
- `part_distance(p, i)` is part `i`'s own distance, moved, turned, scaled and displaced as the combinators above it do.
- `part_name(i)` is a `PartName`: the name its author gave it (`named`), its group's, the frame it's placed in (a creature's bone), and whether it's a mirror image.
- `part_offset(p, i, by)` is the distance at `p` if part `i`'s own distance were `by` more, through each combinator. `part_share(field, p, i)` is how much the distance changes with it: 1 where the part alone decides the surface, 0 where another part does, and between in a blend. A tool uses it to say which parts an edit may move.

The engine's creatures (`engine::creature`) place named parts on bones and answer all of these.

### Certified checks

A property of a field over a region is proved with intervals (§13), not sampled. `std::field::certify_positive(f, region, depth)` proves `f > 0` in a `Box3`. It takes the derived interval of the region, then of the boxes of its octree, at most `depth` levels down, and it evaluates each box's centre, so a failure is found as a point. `certify_clear(field, region, depth)` is "nothing of the surface is in the region" (nothing below the ground), and `certify_apart(field, a: i, b: j, region, depth)` is "parts `i` and `j` don't overlap there". Each gives a `Certified`: `Holds`, `Fails(point)` (a counterexample) or `Unknown(box)` (the first box the depth couldn't decide).

```wrela
use std::derive::Box3
use std::field::{Surface, certify_apart, certify_clear, sphere}

@test
fn two_balls_rest_above_the_ground_apart() {
    let left = sphere(0.5).translate(vec3(-0.6, 0.51, 0.0)).named("left")
    let right = sphere(0.5).translate(vec3(0.6, 0.51, 0.0)).named("right")
    let balls = left.union(right)
    let below = Box3 { lo: vec3(-2.0, -1.0, -2.0), hi: vec3(2.0, 0.0, 2.0) }
    assert(certify_clear(balls, below, depth: 6).holds())
    let around = Box3 { lo: vec3(-2.0), hi: vec3(2.0) }
    assert(certify_apart(balls, a: 0, b: 1, region: around, depth: 6).holds())
}
```

### `wrela edit`

`wrela edit <package> [--edits <file.json>] [--dry-run] [--json]` writes new values into literals: `{"edits": [{"file", "start", "end", "text", "hash", "value"}]}`, each literal as a lifted build gives it.

- **Style.** A literal keeps its form: its decimals (at least as many as it had, more only where the value needs them to read back exactly), its exponent, its unit suffix, and an integer stays one where the value is whole. The new text reads back as the exact `f32` asked for.
- **Sign.** A literal's value is its own: under a negation (`-0.5`) the expression is its negative. A value of the other sign moves the minus.
- **Refusals.** Nothing is written if a file's hash isn't the edit's (another tool changed it), a literal's text isn't at its place, the file lexes differently after the edit except in the literals, or a file the formatter owned would no longer be formatted.

### The lens: `wrela studio`

A subject package has a module `subject.wrela` with `pub fn subject()`, which returns a `Surface + Lipschitz` (a `Field<C>` too, for its channels). `wrela studio <package>` writes a small program around the lens (the `studio` package, generic over any such surface), builds it with the subject's literals lifted, and serves it on 127.0.0.1 (port 8417 unless `--port`).

- **Every action is an export and a command,** and answers with one line of JSON. `wrela studio <package> <action> [args...] [--png file] [--size WxH]` runs one headless on the native host; `run <script>` runs a session, a command a line; the same session typed into Chrome gives the same answers, files and frames (`suite/studio.rs`).

| Action | What it does |
|---|---|
| `view f`, `mode m p`, `zoom c h` | a facing (side, front, three-quarter, top, by name or 0 to 3) or the contact sheet (4); shaded, parts, tint, isolate, silhouette or colour (the subject's albedo, from the first `r`, `g`, `b` floats of its channels), with a part; a close-up framing, `2 h` metres across at `c` (`zoom off` undoes it; 1 cm grid under 30 cm) |
| `parts`, `probe x y z` | the parts' names; the distance, gradient, part and channels at a point |
| `ray o d`, `measure a b`, `project p` | the first hit and its part; a tape measure; a point's pixel in each view |
| `diagnose c r`, `describe` | round 1's diagnostics (pieces, and pieces if thin parts' cells join; bounds; the largest gradient and the five places it's worst, each with its part; the Lipschitz bound stated and derived); a summary of the subject (each part's volume, box and contacts, the whole's box, volume, mass, pieces, lowest point and asymmetry) |
| `click x y` | the part under a pixel, and every literal that moves the surface there, ranked by the distance's derivative by it, with its source line |
| `literals`, `set i v`, `write` | the lifted literals; a new value (the views see it next frame); every changed value written through `wrela edit` |
| `move a b`, `choose i`, `choose_none` | a drag: the point on the surface at `a` moves to `b`, by the literals named (or those that move it most), with other parts' surfaces held; the literals for the next drags |
| `silhouette f c h n`, `fit f c h n` | a facing's silhouette; the named literals fitted so it matches a reference image (`--reference`) |
| `spec` | the subject's spec: each check's value, target and miss, and how many hold (below) |
| `blueprint f`, `fit_blueprint f n` | the blueprint's outline from the side, front or top (0 to 2) over that view, and where the silhouette misses it (each stretch off by over a centimetre, how far, which way); the named literals fitted to it, as to a reference image (below) |

`wrela studio <package> look [--png file]` is the loop in one command: the contact sheet saved, what changed since the last `look` marked on a copy, and the diagnosis and the spec in brief. Warnings while the lens builds are a count; `wrela check` shows them.

Three commands compare by eye, each writing one PNG of numbered panels and answering with what's in each:

- **`beside <manifest>`:** the subject's side view beside a reference photo at the photo's own scale, and the photo with the subject's silhouette laid over it. A photo's manifest registers it: `ground_px` (the ground's row), `metres_per_px`, `z0_px` (the column at z = 0) and `faces` (`right` or `left`); the photo is read as PNG beside it.
- **`variants <package>...`:** several versions of a subject (copies of its package, changed) side by side at the first one's framing.
- **`sweep <literal> <value>...`:** one literal's values side by side, in one session.

- **A drag** solves for the literals by least squares on the distance's derivatives (`std::lift::gradient`), holding fixed points on the other parts' surfaces (those a part's share says it doesn't decide), and checks the result on a 192³ grid before it's accepted: a step that would split the subject into pieces is taken back. The values end rounded to the literals' own decimals.
- **A fit** minimizes the difference between the silhouette and the reference (Levenberg–Marquardt, each pixel's ray minimum, with the derivatives by the envelope theorem), then rounds to decimals and reports the IoU.
- **The server** answers only requests addressed to this machine by name, and takes posts only from its own pages (their `Origin`). It writes the lens's edits (`post` to `studio/edit`) through `wrela edit`, gives a fit its reference (`studio/reference`), and watches the subject's files: a change to literals only goes to the open lens as new values, and any other change rebuilds it, and the page reloads. Nothing of the studio is in a game's build.

### Specs: proportions as code

A spec says what the subject's numbers should come to, in code the toolchain can check and solve for. It's a module `spec` in the subject package with `pub fn spec<C: Checks>(c: mut C)`, whose calls `c.near(name, value, target, within: ...)`, `c.at_least(...)` and `c.at_most(...)` (std::lift's `Checks`) hold measures to targets:

```text
pub fn spec<C: Checks>(c: mut C) {
    let f = wolf().field()
    c.near("withers height", top(f, x: 0.0, z: 0.20), 0.80, within: 0.01)
    c.at_most("chest width", width(f, y: 0.55, z: 0.15), 0.20, within: 0.01)
}
```

- **The measures** are std::field functions of any surface: `hit` (how far a ray goes before it meets the surface), `top` and `bottom` (the surface's height above a ground point, from above and from below), `width` (across x at a height and depth), `front` and `back` (along z), `farthest` (how far the surface reaches in a direction within a box), and `part_named`. Each searches with values and then takes one Newton step along its ray, so its derivative by a lifted literal is exact.
- **Run with a `Report`** (the `spec` action, `look`), the checks say how they stand; **with a `Loss`**, they're one number, the sum of each miss over its `within`, squared, which touches no memory, so `std::lift::gradient` derives it.
- **`wrela solve <package> --spec --free <file>[:<lines>]`** changes the literals on those lines (a whole file, with no lines) until the spec holds: BFGS on the loss's gradient, then each literal rounded to its decimals, with more where rounding would cost the solve. It reports each check before and after; `--write` writes them back through `wrela edit`. Literals a spec can move are the subject package's own: a proportion of a base (below) is written in the author's file to be one (`Quadruped { withers: 0.80, ..paws() }`).

### Blueprints

A blueprint is outlines of the subject from the side, the front and the top, written as code: a module `blueprint` in the subject package with any of `pub fn side()`, `front()` and `top()`, each returning `Vec<vec2>`, the outline's points in order (a closed polygon), in metres, in its view's coordinates: from the side (z, y), +z the subject's front; from the front (x, y); from the top (x, z). The lens draws an outline over its view (`blueprint f`), says where the silhouette misses it and by how much, and fits the literals named to it (`fit_blueprint f n`): the outline is the reference a fit matches.

### What the medium gives an author

- **std::field** has `oval(radii)`, an ellipsoid whose distance stays well behaved inside (its gradient at most 1, where `ellipsoid`'s grows without bound towards the centre: 3 to 25 in thin or blended ellipsoids), and `tube(points, radii)`, round cones joined end to end through points, smooth and swelling nothing at its joints. The tube's `Lipschitz` bound is 1; the oval's is finite at any range, where the ellipsoid's is infinite once its centre is in range.
- **The engine** (not the language) has creatures that take any number of items (one past the 32nd is never culled), and `engine::quadruped`, a four-legged body from a few proportions (`paws()` and `hooves()` presets, `body(q, look)`, and its groups to use alone). Creatures are molded clay: forms only, with no fur. A coat's mass (a ruff, a tail's brush, a cape) is made of parts, and fine surface detail is left to materials.
- **Lofts** (`engine::loft`): `loft(a, b, up, stations:, angles:, radii:)` is a form around a straight axis from `a` to `b`, given by its sections: a radius at each of `angles` directions around the axis (from `up`, by the right hand) at each of `stations` places along it, linear between them and capped flat at the ends. Any section star-shaped about the axis fits, so a loft can hold a measured form as it is. Its distance is ρ − r over the size of the side's slope, √(1 + r_s² + (r_θ / ρ)²) (ρ held at least half the radius), so its gradient is 1 at the surface where sections slope; its `Lipschitz` bound comes from the table, and is infinite where its scope reaches the zone near the axis. `section(t, theta)` gives the radius and its slopes at a point of the table.
- **Sweeps** (`engine::sweep`): `sweep(points, up:, sections:, places:, ends:)` is a form along a smooth curve through `points` (a limb's joints, hip to toes), given by `sections` at `places` along it (fractions of its length), smooth between them (a Catmull–Rom curve of their numbers). A section is named as an artist reads a form: `section(up:, down:, left:, right:, square:, turn:)`, its extents from the path (toward the sweep's `up`, carried along the path without twisting, and to its left, `up × forward`), how square its quarters are (2 an ellipse's), and its turn; `.bump(at:, height:, width:)` adds a bump (a muscle's belly, a bone under the skin; a groove where the height is below 0), four at most. `round(r)` and `oval(width, height)` are the simple ones. Its ends are flat, or domes reaching `ends` past them. The path is cut into straight pieces, and a point's section is in the plane through it that turns from one piece's end plane to the other's, so a sweep bends through a joint with no seam. Its distance is a loft's (over the side's slope), never less than the distance to the path less its reach (which keeps its derived interval tight away from it); its `Lipschitz` bound comes from samples of its sections and its turns, and is infinite where a turn is sharper than the sweep is thick (its inside folds over itself). A named section with four bumps held 93% of the reference wolf's sections within 2 mm, as eight harmonics held 91%.
- **Digits** (`engine::digits`): `digits(at, forward:, up:, spread:, length:, thick:, splay:, curl:, outer:)` is `N` toes side by side (or fingers, or a hoof's halves), each an arched sweep with a domed tip, its outer ones `outer` times the middle ones' size.
- **Creases** (`engine::creature::crease(k, depth)`): a seam that joins two forms with a fold, the surface sinking into a groove `depth` deep where they meet, fading over `k` (a third of `k` deep at most): a smooth union's fillet turned over. Where the two overlap by less than `depth`, it cuts through.
- **The engine's anatomy kit** (`engine::anatomy`) builds at the level a creature artist works in: `muscle(origin, insertion, bulk)` (a spindle of round cones, exact), `knob` and `tendon` for bones that show, `bundle` to blend them into one part, `CanidHead`, `CanidEar` and `CanidPaw`, `ground()` (nothing below y = 0), and `Paint`, colour that changes with height across a part. `canine(wolf(), coat)` is a whole canine from a few proportions, measured from the wolf reference photo, on a rigged skeleton; its `ruff` and `brush` are the coat's mass, built into the neck's and the tail's forms.
- **Colour across a part:** std::field's `with_at(|p| ...)` gives a surface channels from a function of the point, where `with(c)` gives one value.
- **`wrela doc`** reads the packages a package depends on (`wrela doc engine::quadruped <package>`), shows a struct's fields with their doc comments, and lists the built-in functions (`wrela doc builtins`).

### Tools for agents

Each takes a package directory and answers in JSON (`--json`), so an agent reads what the compiler knows instead of guessing it.

| Command | What it does |
|---|---|
| `wrela query <package> <query>...` | One check answers a batch of queries: `type <file>:<line>:<col>`, `callers <fn>`, `callees <fn>`, `impls <trait or type>`, `effects <fn>` (each with a call chain to where it happens), `borrows <file>:<line>` (the loans live and the places moved out of before the line runs), `instantiations <fn>`, `search <signature>` (`(vec3, _) -> f32`). Queries come from the arguments or a line each on standard input. |
| `wrela context <item> [<package>] --budget n` | The item's source, the types it names, what it calls, where it's called and its impls, in about `n` tokens |
| `wrela refactor <package> <change>` | `rename`, `move`, `add-param`, `change-mode`: planned, checked with the new texts before anything is written, refused with the errors if the program wouldn't check, and refused if a call would quietly bind to another function. `--dry-run --json` gives a plan with each file's hash; `apply <plan>` refuses it if a file has changed since. |
| `wrela solve <package> --minimize <fn> --free <file>:<lines>` | Minimizes a function of no arguments over the literals on those lines (BFGS on `std::lift::gradient`), rounded to their decimals, and with `--write` writes them through `wrela edit` |
| `wrela trace <package> --watch <exports>` | Runs frames and prints what the exports say after each (`--csv`) |
| `wrela reference replica <parts> -o <dir>` | A replica of a reference mesh as a package: a skeleton and a loft or a sweep per part. The parts file names a mesh's manifest in `references/` (whose `axes`, `origin` and `metres_per_unit` place the mesh in a creature's frame, and whose `attribution` the package carries), landmarks, and each part's axis, angles, station spacing and longest radius (the file's top gives defaults). Each axis runs on 2 cm past its ends (`overlap`), then is trimmed to the mesh, so parts that meet at a joint overlap; at an end inside another part, the part sinks by up to `taper` toward its cap, so it passes under its neighbour rather than ending in a step. A radius is where a ray from the axis leaves the mesh (where its winding number falls to 0, so past surfaces inside it where a sculpt crosses itself); a radius held at the longest (its ray ran into another part) is filled from the measured ones around it; radii are integers, tenths of a millimetre, so a lifted build doesn't lift them. `--harmonics k --sections-every m` compresses each table: each section a Fourier series of `k` harmonics, each coefficient a Catmull–Rom curve through a control section every `m` metres, fitted to the measured radii by least squares (a radius held at the longest is free), but held up to a measured radius where no other part reaches its point (or the points halfway to the next radii), so the compressed parts leave no crack where the dense ones left none; it reports the numbers the compact form takes. A part with `sections = k` is a sweep (`engine::sweep`) along a path from `from` through `through` (landmarks) to `to`, with `k` control sections of `bumps` bumps (4 unless it says) and domes `ends` long: its radii are measured in its own planes every `spacing`, each the part's own surface or shared (another part, as measured, holds its point: a ray that ran on along that part); each station's section is fitted alone, outward from the one with the most of its own surface, each from its neighbour's (so a bump follows its feature), the control sections fitted to those by least squares, then all at once (own radii in full, shared ones only where the fit passes them); and where no fitted part reaches 2.5 mm inside a measured radius, the radius is held to within 2 mm and its part fitted again, the parts that came nearest first. `--seam m` joins every part `m` wide |
| `wrela reference deviation <parts> <package>` | How far a package's surface is from the mesh's: at points spread over the mesh's surface (skipping surfaces inside it), how far short of it the package is, or how far past it outward (the shares within 2 and 5 mm, and the mean), by part, with the worst places; the volumes inside one and not the other; and rays from five views (the sides, the top, the front and the lens's three-quarters, every 2 mm) that pass through the mesh but meet nothing of the package, inside its outline or at its rim: where you'd see through it |
| `wrela bisect <package> --until "<export> <op> <number>"` | The first frame a condition holds (`nan <export>` too) |
| `wrela primer [area]` | The language area by area: each rule of the conformance suite, with a program that keeps it and one that breaks it |

**Agent runs are debug builds.** `wrela test` builds the program as a debug build (§11), and so do `wrela trace` and `wrela bisect` unless told `--release`: a simulation that makes a NaN, or indexes a GPU buffer out of range, stops at the frame it did, where the agent looks.
