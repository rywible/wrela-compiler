# wrela: the language

*The one prose reference for the language, as it stands. Decision IDs (D-NNN), sketches and spikes refer to the design record in the git tag `design-archive-2026-10`.*

**Tier 0 is implemented, and these hold it** (milestone 1):
- **Syntax:** `spec/lexical.md` and `spec/grammar.ebnf`, normative. An oracle parser generated from the grammar is checked against the compiler's parser (`compiler/grammar`).
- **Rules:** the conformance suite, `compiler/tests/conformance`. Each tier-0 rule below has a rule ID there (for example `mem.take`) with a program it accepts and one it rejects with the rule's diagnostic code.
- **Diagnostics:** `compiler/tests/diagnostics`, the common mistakes with their messages, spans and fixes.
- **The derived interpretations, numerics and the hosts:** the tests in `compiler/tests/tests` and `runtime/`.

Where this prose and those disagree, they win, and the prose is a bug. Syntax beyond tier 0 is still imagined.

## How to read this

- **Rules cite the decisions they came from** (D-NNN), for history. This document is the current state.
- **Every feature has a tier** (D-088):
  - **T0:** milestone 1, the first program that draws a field.
  - **T1:** milestone 2.
  - **T2:** milestone 2, except a compiler in the browser, which is a non-goal (vision.md).
- **"Open"** marks something undecided. **"Placeholder"** marks syntax used in the sketches that no decision has settled.
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
| A binary operator that continues a line must *trail* the line; a leading `-` or `\|` starts a new expression | T0 | D-079 |
| `;` may separate statements on one line; the formatter normalizes | T0 | D-038 |
| `else` goes on the line of the `}` before it: a line break after `}` ends the `if` (`lex.else`) | T0 | D-079 |
| `//` comments, `///` doc comments; no block comments (`lex.comments`) | T0 | spec/lexical.md |
| Files use the `.wrela` extension | T0 | D-040 |
| Number literals have no type suffixes (no `1.0f32`) | T0 | D-025 |
| A unit can follow a number as a suffix: `15cm` means `15 * cm`, which is 0.15. Units are constants, in SI (§5). | T1 | D-025 |
| `**` is exponentiation; `^` is XOR | T0 | D-076 |

```wrela
let r = ellipsoid(radii: vec3(0.45m, 0.50m, 0.90m))
    .smooth_union(haunch, k: 15cm)      // a leading `.` continues the expression
    .displace(fbm(freq: 25 / m, octaves: 4), amp: 3mm)

let total = base +                      // a continued line ends with the operator
    extra
```

**Keywords** are listed in `spec/lexical.md` (L11), including those reserved for later tiers.

**Control flow (T0)** is expressions and statements in the Rust family: `if`/`else` and `match` are expressions; `for i in 0..n` (and `0..=n`) counts over integers, `for x in xs` walks an array or a run, and `_` names an unused loop variable; `while`, `loop`, `break`, `continue` and `return` work as usual. A block's value is its last line when that's an expression. A local that's bound and never used is a warning (W0001), unless its name starts with `_`. A `let` may shadow an earlier binding of the same name. **Tier 1 adds** `if let PATTERN = EXPR { … } else { … }`, and `let PATTERN = EXPR else { … }`, whose `else` block must leave the scope (`return`, `break`, `continue` or a panic). A literal's type is inferred from the whole function, later uses included: after `let a = 1` and `let b: f32 = a`, `a` is an `f32`.

**Limits.** Expressions, blocks, types and patterns nest at most 128 deep, and an expression's tree is at most 512 deep (E0112). A generic function's instantiations go at most 256 calls deep, and their type arguments, like any value's type, have at most 4096 parts, counting a part each time it appears (E0412, E0329): a type that doubles with each step, like `(a, a)`, grows past any machine. A value larger than the CPU stack traps when the function holding it is entered, as a stack overflow does; in GPU code, a type larger than WGSL allows (2³¹ − 1 bytes) is an error (E0702).

---

## 3. Items

### Functions (T0)

- **Named arguments are optional, Kotlin-style** (D-039). Positional arguments come first, and once an argument is named, the rest must be named too. Parameters may have defaults, which are constant expressions (§10); in tier 0, literal values (`fn.named-args`, `fn.defaults`). Arguments are evaluated in the order they're written, named ones too, whatever the order of the parameters they go to. A lint suggesting names for bare literals such as `6cm` or `true` isn't built yet.
- **Parameters have modes** (§6): `x: T` (borrow), `x: mut T`, `x: take T`.
- **Properties are attributes** (§9): `@deterministic fn step(...)`.
- **A trait in parameter position** (`fn d(field: Surface, p: vec3)`) makes the function generic over that parameter. It's the usual way to write a generic parameter (§7); write `<F: Surface>` only when the type is named twice.
- **A trait in return position** (`-> Surface`; `Field<C>` in tier 1) names one concrete, inferred type, like Rust's `impl Trait` (D-070, `fn.return-trait`).

```wrela
fn leg_segment(len: f32, r_top: f32, r_bottom: f32 = 0.06) -> Surface {
    round_cone(vec3(), vec3(y: -len), r_top, r_bottom)
}

leg_segment(0.45, r_top: 0.09)         // positional first, then named
```

(Sketch 01 writes this with unit suffixes, `6cm`, and a field type with channels, `Field<Tissue>`: both tier 1. Its kind parameter, `Exact`, became the `Lipschitz` trait: §17.)

### Structs (T0)

- **Fields may have defaults**, which are constant expressions (§10, D-048); in tier 0, literal values. A struct literal may omit defaulted fields (`struct.defaults`).
- **A struct opts in to traits in its declaration:** `struct Tissue: Blend { ... }` (D-026, D-078). In tier 0 these are `Copy`, `Clone` and `GpuData`, structural: every field must have the trait (`struct.opt-in`). Declaring `Copy` implies `Clone` (milestone 2; tier 0 requires both). A generic type's declared trait holds for the instantiations whose type arguments have it: the prelude's `Option<T>: Copy + Clone` makes `Option<f32>` `Copy`, and `Option<Log>` isn't when `Log` isn't.
- **`..base`** fills the remaining fields from another value of the same type (`struct.base`). Sketch 01 uses it; no decision covers it.

```wrela
pub struct Look {
    pub tolerance: f32 = 0.002,
    pub resolution: u32 = 1024,
}

const FINE: Look = Look { tolerance: 0.001 }   // resolution keeps its default
```

### Enums (T0)

Enums are sum types with payloads, matched with `match`, which must be exhaustive (`enum.match`). They're how structure is chosen at runtime from a known, finite set: every case is compiled, with a uniform branch (D-070).

```wrela
pub enum Edit: Copy + Clone {
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
- **Coherence follows Rust's orphan rule:** an `impl` lives in the package of the trait or of the type; std is another package (D-071). T0 (`trait.orphan`). `Copy`, `Clone` and `GpuData` aren't implemented with an `impl`; a type opts in to them in its declaration.
- **Fieldwise traits** (T1). A trait declared `@fieldwise` is derived for every type that declares it, field by field. For a struct, the trait's method is applied to each field in order. A parameter of type `Self` is taken field by field too: `blend(self, other: Self, t: f32)` blends each field with the same field of `other`. A method that returns `Self` builds the struct from its fields' results, and one that returns `Result<Self, E>` does the same, stopping at the first error. For an enum, the variant comes first, then its fields; a method that builds an enum chooses the variant through the trait, which is how loading works. How the trait chooses the variant and learns each field's name is settled in milestone 2 (§21). `Clone`, `Eq`, `Ord`, `StateHash`, `Serialize` and `std::field`'s `Blend` work this way, and so can a library's own traits. There's no reflection (§18).
- **`==` and ordering come from declared traits** (T1). A type that declares `Eq` gets `==` and `!=`, and one that declares `Ord` gets `<`, `<=`, `>` and `>=`. Both are `@fieldwise`: fields compare in order, and an enum's variants compare in the order they're declared. In tier 0, `==` works only on numbers, `bool` and vectors (E0305).

  ```wrela
  @fieldwise
  pub trait StateHash {
      fn state_hash(self, h: mut Hasher)
  }

  impl StateHash for f32 {                       // the leaves are written by hand
      fn state_hash(self, h: mut Hasher) {
          h.write_u32(self.canonical_nan().bits())
      }
  }

  struct Edit: Copy + StateHash { at: vec3, radius: f32 }   // derived: `at`, then `radius`
  ```

### Constants

A `const` is computed when the program is built (§10, D-073). Its initializer can be any expression, function calls included, as long as its effects allow it: no IO except `embed`, no host calls, nothing non-deterministic (§8). A failed `assert` or a panic in it is a compile error, with the call chain. In tier 0 a constant is a literal value (`const.literal`); the rest is tier 1.

```wrela
const HIDE_DENSITY = 1050 * kg/m**3                       // 1050.0, in kg/m³ (§5)
const GAIT = GaitNet { weights: embed("grazer_gait.wnn") }   // read in place: no parsing at load
const HOOF_MODES = modal_modes(hoof())                       // an eigenvalue solve, run by the build
const GRAZER = grazer_skeleton()
const CHEST = GRAZER.bone("chest")                           // a misspelled name fails the build
```

Staging is never left to the optimizer (D-072): work that must happen early is written as a `const`, or at load.

### Modules and packages

**Modules (T0):** a program is one package, a directory with a `main.wrela`. A file is a module and a directory is a module tree: `shapes/blob.wrela` is `shapes::blob`. `use` imports names; `pub` makes an item visible outside its file. In `main.wrela`, `pub` also makes a function an export (§12). A `use` path starts at the package root, or at `std::`; there's no `super::`. A module has one namespace for all its items. The stdlib's root is `std::` (D-081), so no module of the package can be named `std` (E0201). A symbolic link to a directory is refused rather than followed (E0208), until packages decide what a module tree is (`mod.use`, `mod.names`).

```wrela
use shapes::blob::blob      // from shapes/blob.wrela
use std::gpu::dispatch
```

**Packages and dependencies** come later (D-087). D-030's rule, that exported functions state their effects, applies at the package boundary once packages exist.

---

## 4. Types

| Type | Meaning | Tier | Decisions |
|---|---|---|---|
| `bool`, `i32`, `u32`, `f32` | Scalars, on CPU and GPU | T0 | D-074 |
| `f64`, `i64`, `u64` | CPU only. GPU code is type-checked against what WGSL has. | T0 | D-074 |
| `u8`, `i8`, `u16`, `i16` | CPU only in tier 0; packed in GPU storage later | T0 | D-074 |
| `f16` | Explicit lossy type | T1 | D-049 |
| `vec2`, `vec3`, `vec4`, `mat2`, `mat3`, `mat4` | f32 vectors and square matrices. `Quat` is tier 1. | T0 | D-076 |
| `[T; N]` | Fixed-size array; `[x; N]` repeats a `Copy` value. GPU code has no empty arrays (WGSL's), so there N is at least 1. Generic code can range over the length: `fn sum<const N: u32>(xs: [f32; N])`, `impl<F: Surface, const N: u32> Surface for [F; N]` (T1). A length is a constant expression with no calls, so a type never waits on build-time code. | T0 / T1 | |
| `[T]`, `mut [T]` | A run of `T`, borrowed or mutable: the language's one view type. Its elements are contiguous bytes only where they're viewed as bytes (§6.10). In tier 0 it's a parameter's type only; in tier 1 it can also be a function's result or a local binding (§6.6). An array passes for a run. `xs.len()` is a run's or an array's length, a `u32`. | T0 / T1 | §6.2 |
| `(A, B)` | Tuple | T0 | |
| `Option<T>` | An ordinary enum in the prelude (`Some`, `None`); there's no null | T0 | §6.1 |
| `Result<T, E>` and `?` | Recoverable errors | T1 | D-061, D-088 |
| `String`, `str`, `Text` | Heap-owned UTF-8; a borrowed run of it; and text known at build time, the type of a string literal: a `Copy` handle to read-only UTF-8 in the build, which can be stored anywhere | T1 | D-087 |
| `f"…"` | String interpolation: `f"Weight: {w:.1} kg"` builds a `String`. Each `{expr}` or `{expr:spec}` calls std's `Format` trait, which numbers, `bool`, strings and `Text` implement; `{{` and `}}` are literal braces. It allocates, so it's not for GPU or `@audio` code. | T1 | |
| `borrow T`, `mut T` | Projection types: a function's result or a local binding, never a field of an ordinary type or a type argument (§6.6). | T0 | D-064, D-084 |
| `borrow struct` | A named group of projections: its fields are projections, runs, `Copy` values and other borrow structs. It's a projection itself (§6.6). | T1 | D-064 |
| Closures | A closure that captures a projection can't outlive its call (T0). One that captures only values can be stored, and its type is its own (T1, §6.7). | T0 / T1 | D-064 |
| `Handle<T>`, `Arena<T>`, `Vec<T>`, `Box<T>` | Stdlib containers (§6) | T1 | D-065 |
| `Unorm8`, `Oct16`, … | Lossy encodings are always explicit types | T1 | D-049 |

**`&T` doesn't exist** (D-064). There's no reference type to store, so there are no lifetimes.

Implicit conversions don't exist either: an integer literal can be any numeric type, but a value converts only with a call such as `f32(n)` or `u32(i)` (`ty.scalars`). Units, strings, `?` and `unsafe` are rejected with a diagnostic saying which tier brings them (`ty.tiers`). `dyn` is reserved and never accepted (§18); its diagnostic still names a tier until milestone 2 changes it.

---

## 5. Units (T1)

Unit suffixes are constants (D-025). There are no unit types (§18).

- **Every physical quantity is SI:** metres, seconds, kilograms and radians. An `f32` that holds a length holds metres; names and doc comments say which quantity it is.
- **Suffixes convert to SI at compile time:** `15cm` is `15 * cm`, which is 0.15, and `90deg` is π/2. Compound units are arithmetic on the constants: `1050 * kg/m**3`.
- **Unit suffixes resolve in their own namespace,** which locals can't shadow. `2m` means metres even if a local is named `m`.
- **Constraint:** no unit may collide with numeric syntax. There's no unit named `e`.
- **A bare number meant in another unit still compiles.** Write the suffix: `sin(90deg)`, not `sin(90)`.

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
| **Everything is a value.** `&T` isn't a type. | T0 |
| **Parameters have modes:** borrow (default), `mut`, `take`. Callers write `mut x` and `take x`. A `mut self` receiver isn't marked; a consuming `take self` method on a named place is (`take b.finish()`). | T0 |
| **Moving out of a named place is written `take`.** Deep copies are `.clone()`; small `Copy` types copy implicitly. Temporaries and returned locals need no marker. | T0 |
| **Bindings:** `let` owns, `var` owns mutably, `borrow` projects a place read-only, and `mut` projects it mutably (§6.3). | T0 / M2 |
| **Projections:** a function may return `-> borrow T` or `-> mut T` of one of its parameters. Projections never outlive the caller's scope. | T0 |
| **Exclusivity:** while a `mut` access is live, nothing may touch an overlapping place. Disjoint fields don't overlap; every element of a container overlaps every other (`pair_mut` and `split_at_mut` check at runtime). Checked within each function, over its control flow: a loan or a move reaches every path that can follow it, through branches, loops and `continue`. | T0 |
| **No mutable globals, and no interior mutability** like `Cell` or `RefCell`. Shared mutable state lives in an arena. | T0 |
| **A projection must come from a `borrow` or `mut` parameter.** The caller treats a `-> borrow T` result as borrowing every such argument, and a `-> mut T` result as borrowing only the `mut` ones (§6.4). | T0 / M2 |
| **Projections are parameters, results and local bindings only:** `borrow T`, `mut T`, runs, borrow structs and closures that capture projections. They're never fields of ordinary types, or type arguments (§6.6). | T0 / T1 |
| **A closure that captures a projection can't outlive its call.** One that captures only values can be stored (§6.7). | T0 / T1 |
| **Long-lived relationships are handles into arenas,** never pointers. | T1 |
| **GPU layout:** a declared `GpuData` trait fixes a type's layout to WGSL rules everywhere. This is how tier 0's lossless GPU layout is expressed. | T0 |
| **Rule IDs:** `mem.copy`, `mem.take`, `mem.clone`, `mem.modes`, `mem.receivers`, `mem.bindings`, `mem.projections`, `mem.exclusivity`, `mem.loops`, `mem.no-globals`, `mem.closures` in the conformance suite. | |
| **Byte-level data:** the declared trait `Plain` (§6.10). | T1 |
| **Snapshots are clones** of ordinary values (§6.11). | T1 |
| **Destruction is deterministic:** at the end of scope, in reverse order. Only the stdlib's core defines destructors (§18). | T1 |
| **`unsafe`** exists only for the stdlib's core; packages declare whether they use it. | T1 |

### 6.1 Values, copies and moves

```wrela
let a = vec3(1m, 2m, 3m)
let b = a                      // vec3 is Copy: an implicit copy

var log = EditLog::new()
var backup = log.clone()       // a deep copy is always explicit
archive(take log)              // moving out of a named place is written `take`
log.push(edit)                 // error: `log` was moved into `archive` on line 6
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
- **Names bound inside a pattern or a loop project the matched place,** read-only: `match e { Some(log) => … }`, `for g in world.grazers`. `match mut place { … }` and `for mut g in world.grazers` make them mutable projections, so a payload can change in place: `match mut slot { Some(s) => s.count += 1, None => {} }`. Matching a temporary owns. A loop borrows its container for the whole loop, so there a copy and a projection can't be told apart.
- **Closures capture by projection or by value** (§6.7).

**Milestone 2 makes this change.** In tier 0, `let x = place` projects when the type isn't `Copy`, and `borrow` isn't a binding form yet.

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
- **A projection must come from a `borrow` or `mut` parameter, or from a constant.** A constant lives as long as the program, so `fn item(id: ItemId) -> borrow ItemDef { ITEMS[id.index] }` is fine. You can't project from a local or a temporary.
- **What the result borrows.** A `-> mut T` result borrows only the `mut` arguments, because a `mut` projection can't come from a read-only place (E0512). A `-> borrow T` result borrows every `borrow` and `mut` argument; that's conservative, and a `from param` annotation can narrow it later if real code needs it. (Milestone 2; in tier 0 every result borrows every `borrow` and `mut` argument.)
- **Projections can't be stored** in ordinary structs or collections, or captured by a closure that's stored (§6.7). A borrow struct can group them (§6.6). They never outlive the scope that received them, so no lifetimes are needed.
- **Projection types are never type arguments or fields.** `arena[h]` projects, and a stale handle panics; `arena.contains(h)` checks first. There's no `Option<borrow T>` (§18).

### 6.5 Exclusivity

**Places** are locals, their fields, and elements of containers. Two places **overlap** when one is a prefix of the other. For example, `world` overlaps `world.terrain`, but `world.terrain` and `world.grazers` don't.

**Every element of a container overlaps every other element of that container.** The checker doesn't compare indices.

**The rule:** while a `mut` access is live, from where it's created to its last use, no other access may touch an overlapping place. In a single call, arguments may not overlap if any of them is `mut`. Arguments are evaluated before the call's `mut` access begins, and a `Copy` argument passed by `borrow` is copied then, so it may read a place that a `mut` argument overlaps: `v.push(v.len())`, `s.add(s.count)`. An argument that's a projection still conflicts. (Milestone 2; tier 0 rejects these with E0513.)

```wrela
for mut g in world.grazers {
    step(mut g, world, intent)
    // error: `world` overlaps `world.grazers`, which `g` is mutably borrowing
    //   help: pass only what `step` reads, e.g. `world.terrain`
}

let (a, b) = world.grazers.pair_mut(h1, h2)   // two elements at once: checked at runtime,
                                              // panics if h1 == h2
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
- **It's a projection itself:** a parameter, a function's result or a local binding. It's never a field of an ordinary type, or a type argument. A borrow struct can be generic over types.

**There are no iterator types.** A `for` loop walks runs, arrays, ranges and arenas. A query that finds many items takes a closure, or returns an owned `Vec`:

```wrela
for mut g in world.grazers { ... }                     // an arena
for leg in pose.legs { ... }                           // a `[LegPose; 4]` field: a run
grid.each_within(p, 15m, |h| { count += 1 })          // a closure: no iterator type
let mates = grid.within(p, 15m)                        // an owned Vec<Handle<GrazerSim>>
```

This is what removed most of the cost of second-class references (D-058), and it's why wrela needs no lifetimes.

### 6.7 Closures

- **A closure that captures a projection,** `borrow` or `mut`, is a projection itself (§6.6). It can be passed down, and the callee must finish with it before it returns. Its access counts as live for the duration of the call. A closure can be named with `let` (not `mut`), and it borrows its captures while the name is live. In tier 0 a closure can't capture another closure.
- **A closure that captures only values,** copied, or moved with `take`, is an ordinary value (T1). It can be returned and stored, for example in a field: `.with(|p| dapple(p, seed))`. Its type is its own and is inferred, as `impl Trait` is (§7). A function type, `fn(f32) -> f32`, is still a parameter's type only (`mem.closures`). A function type gives its parameters modes, `fn(mut Ui)`, and a closure's parameters take their modes from the type it's passed as.
- **Escapability comes from the captures.** There's no annotation (§18).
- **Closures of different types can't share a container,** because there's no `dyn`. Deferred work, such as a timer or an event handler, is an enum of actions applied by one `match`. Unlike a closure, an action can be saved.

```wrela
world.grazers.par_each_mut(|g| step(mut g, world.terrain, intent))   // captures a projection: passed down only
let skin = torso.with(|p| Tissue { albedo: dapple(p, seed), ..HIDE })   // captures by value: the field stores it
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
- **Rollback** keeps a full copy every few ticks, and replays the inputs since. Under D-042, clients roll back only their own predicted entities, which are small.
- **A checksum is the derived `StateHash`** of the whole state. Canonical bytes (§6.10) make it the same on every client.
- **A save is the derived `Serialize`,** with stable field names, so saves survive layout changes and can be migrated (D-084). A repro bundle is a saved state plus the inputs since.
- **Cost, as estimates to be measured:** 10,000 entities of 256 bytes are 2.5 MB, so a copy is roughly 0.3 ms, with no allocation when it goes through `clone_into`, and a hash of it is about the same. If that ever matters, chunked copies and incremental hashes can come back inside `Arena`, as a library optimization (#31).

### 6.12 Threads

- **Platform:** web workers plus SharedArrayBuffer (D-017). Every worker shares one WASM memory, whose maximum is reserved at startup. vision.md describes the thread layout (D-098).
- **Nothing extra is needed to share data between workers.** wrela has no interior mutability and no shared ownership. A value that several workers read can't change under them, and an owned value can always move to another worker (§18).
- **Parallelism goes through data-parallel combinators** (D-062). Exclusivity proves disjointness, so there are no locks in game or engine code.
- **Atomics and queues** live in the stdlib's unsafe core. They're the only interior mutability in the language, and they're built for concurrent use. **Every std type with interior state must be safe to share between workers.** That's a rule on std's core, reviewed when it's written; the compiler doesn't check it.

### 6.13 GPU and audio

- **Modes map onto WebGPU bindings:** `borrow` becomes a read-only storage or uniform binding, and `mut` becomes a `read_write` storage binding.
- **WebGPU forbids aliasing writable bindings** within a dispatch, which matches exclusivity *per binding*.
- **That doesn't cover a single dispatch.** Every invocation holds the same `mut` binding at once, so exclusivity says nothing about how invocations share it (D-084). A kernel's `mut` parameters therefore accept only invocation-safe types:
  - `Slots<T>`, where each invocation writes only the slot keyed by its own ID (tier 0)
  - atomics and `Append<T>` (milestone 2, with workgroup-shared memory)

  A plain `mut` array parameter is rejected (E0601, `gpu.kernel-mut`).
- **Data that crosses to the GPU must be `Plain` and `GpuData`.**
- **A GPU buffer is an owned value** (D-102; milestone 2. In tier 0, `GpuBuffer<T>` is a `Copy` handle.) CPU code can't read through it.
  - Passing it to a kernel follows the modes. In `dispatch(k, values: out, total: mut out)` the arguments overlap, so the call is rejected at compile time (§6.5), not by the host when the command runs.
  - It lives until its owner's scope ends, and a buffer stored in program state lives with that state. The host defers the GPU's release until submitted work that uses it is done.
  - `GpuSpan<T>` is a projection of a buffer, as `[T]` is of an array. WebGPU's rule against reading and writing one buffer in a pass covers the whole buffer, so two spans of one buffer count as overlapping when bound together, even if their ranges don't. The hosts still check every command, as a backstop.
  - Uploads are explicit copies. Readback is a polled request, and `nondet` (tier 2, §6.15). There's no zero-copy path between WASM memory and the GPU (vision.md).
- **Workgroup-shared memory** (milestone 2, D-093) is a value that all of a kernel's invocations share. Between barriers, each invocation writes only its own chunk, which the stdlib assigns by `LocalId`, and reads go through `shared.all()`. `barrier(mut shared)` ends every projection of the value, so a phase of writes and a phase of reads can't overlap: ordinary exclusivity (§6.5). A barrier must sit in uniform control flow, as WGSL requires, and the checker rejects one that doesn't.
- **`@audio` code** borrows preallocated `Plain` buffers and never allocates.

### 6.14 The unsafe core

`unsafe` exists only so the stdlib can implement things the checker can't verify:
- `Vec`, `Arena`
- atomics and queues: the only interior mutability, each safe to share between workers (§6.12)
- host bindings

Each package declares whether it uses `unsafe`. Game and engine code shouldn't need to, and the package policy flags it if it does.

### 6.15 Requests, not async

wrela has no `async` (§18). IO and GPU readback are **requests**:
- A call submits the request and returns a `Pending<T>` handle at once.
- The program polls the handle on a later tick or frame: `p.poll()` returns an `Option<T>`.
- Requests have the `io` or `nondet` effect, so `@deterministic` code can't make or poll them. A result reaches the sim only through a tick's input, so it enters on a known tick.

A game already has a suspension point, the tick or the frame, so the language doesn't need another. A handle is an ordinary value, so nothing needs `Pin`.

### 6.16 Diagnostics

The errors agents will hit most, and what they say:

```
error: `&` isn't a type in wrela
  --> leader: &GrazerSim
  help: to refer to another grazer, store `Handle<GrazerSim>`

error: a struct can't hold a borrow
  --> struct Ctx<'a> { world: &'a World }
  help: declare it `borrow struct Ctx { world: borrow World }`, and pass it down as a parameter

error: `world` is already mutably borrowed
  --> step(mut g, world, intent)
  note: `g` borrows `world.grazers` mutably until line 14
  help: pass only what `step` reads: `world.terrain`

error: can't move out of `world.grazers[h]`: it's a projection
  help: use `world.grazers[h].clone()`, or `world.grazers.remove(h)` to take it out of the arena

error: `log` was moved into `archive` on line 5
  help: write `archive(log.clone())` there if you still need `log`

error: this closure captures `world.terrain`, a projection, so it can't be stored
  --> let f = blob.with(|p| p.y - world.terrain.height(p))
  help: capture a value: clone what it needs first, or move it in with `take`
```

---

## 7. Generics and traits

| Rule | Tier | Decisions |
|---|---|---|
| **Generics are always monomorphized.** | T0 | D-071 |
| **Structure is types.** Combinators are generic types, so building a field builds a type, and the field's numbers are plain data that reach the GPU as uniforms. | T0 | D-070 |
| **`impl Trait` convention:** a trait-shaped type in return position is one inferred concrete type; in parameter position it makes the function implicitly generic, and that's the usual way to write a generic parameter. | T0 | D-070 |
| **Choosing structure at runtime:** an enum for a finite set (all cases compiled, uniform branch); `std::stage::interpret` for unbounded structure built at runtime. | T0 / T2 | D-070, D-053 |
| **Control-flow values are data:** loop bounds and comparisons become uniforms; unrolling is the optimizer's choice. | T0 | D-070 |
| **The pipeline-count query** reports how many instantiations each GPU entry point has, and why. | T1 | D-044, D-070 |
| **Every trait a type has is declared.** `Plain` and `GpuData` are declared like `Copy`, and checked structurally: every field must have the trait. There are no auto traits (§18). Declaring `Copy` implies `Clone`, and a generic type's declared trait holds for the instantiations whose arguments have it (§3). | T1 | D-078 |
| **Declared traits with a structural check:** a library trait can require that every field also implements it (`SimState` is the engine's example). | T1 | D-078, D-060 |
| **Library-authored diagnostics:** `@diagnostic(...)` attaches a message to a trait or type, for when a bound isn't met. | T1 | D-055 |
| **Effects are checked per instantiation**, through every call: an error shows the chain (`fill` → `scratch`). | T0 | D-071 |

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
- **`recursion`** is a cycle in the call graph after monomorphization. GPU and `@audio` code forbid it. `@deterministic` code allows it to a fixed depth of 256 calls: the compiler counts the depth, and the 257th call traps on every engine the same way. D-015's concern was that engines' stack limits differ; a counted limit far below all of them removes it. (Revises the rule that every cycle counts.)

**Each context forbids a subset:**

| Context | Forbidden | Tier |
|---|---|---|
| GPU entry points (`@compute`, `@vertex`, `@fragment`) | `alloc`, `io`, `nondet`, `recursion`, `host`, `panic`. In tier 0 the language has no allocation, I/O or randomness, so `host` (recording GPU work) and recursion are what's checked (E0600, `eff.gpu`). | T0 |
| `@audio` | `alloc`, `io`, `recursion`, `host` | T2 |
| `@deterministic` | `nondet`, `host` (except declared deterministic host calls), and recursion deeper than 256 calls (D-094) | T1 |
| Derived interpretations (gradient, interval) | `alloc`, `io`, `nondet`, `host` (E0700, `eff.derived`) | T0 |
| Build-time constants | `io` (except `embed`), `host`, `nondet` | T1 |

- **Inside a package, effects are inferred** (D-010, D-030). Annotations only assert. Errors show the call chain: `extract → foo → bar allocates at line 42`.
- **Effects are inferred everywhere, and never written.** The compiler sees the whole program, and packages are local, so D-030's case for stating effects at a package boundary doesn't apply (§18).
- **Public higher-order functions inherit effects from their closure arguments** by default (D-030). `map` is GPU-safe whenever its closure is.
- **Staging is guaranteed or rejected, never best-effort** (D-072). The optimizer may hoist work, but code must not rely on it. Work that must happen earlier is written earlier, as a `const` or a parameter.

---

## 9. Attributes

`@` attributes are a **closed set defined by the language** (D-037, D-081). All of them except `@diagnostic` and `@intrinsic` are part of a type or contract: they change what a function *is*.

| Attribute | On | Meaning | Tier |
|---|---|---|---|
| `@compute(x, y, z)` | functions | GPU compute entry point, with workgroup size | T0 |
| `@vertex`, `@fragment` | functions | GPU render entry points | T0 |
| `@gpu` | functions | Asserts the function is GPU-safe, so a violation errors at its definition (D-010). A generic one is checked for what its own code does, whatever its type arguments. | T0 |
| `@deterministic` | functions, function types | The determinism constraint (§14) | T1 |
| `@intrinsic` | functions, in std only | The compiler provides the body: GPU commands and built-in math (D-081). Anywhere else it's rejected. | T0 |
| `@fieldwise` | traits | The trait is derived field by field for every type that declares it (§3) | T1 |
| `@diagnostic(...)` | traits, types | A library-authored error message; not part of the type (D-055, D-081) | T1 |
| `@audio` | functions | Audio-worklet entry point (D-072) | T2 |

User-defined metadata, if it's ever needed, gets a different syntax, so `@` always means semantics (D-037). An unknown attribute is E0204; a tier-1 one is E0903 (`attr.closed`).

---

## 10. Build-time constants and embedded data (T1)

There's no separate interpreter in the compiler, and no reflection (§18). What's known at build time is:
- **Constants** (§3). The compiler compiles a `const`'s initializer, and every function it calls, to WASM, and runs it in wasmtime during the build. The build runs the program's own CPU code, with the same strict floats and the same traps, so build time and run time can't disagree.
  - Its effects must allow it (§8).
  - A failed `assert` or a panic is a compile error with the call chain. That's how a library checks something at build time, such as a bone name.
  - A fuel limit turns a computation that runs away into a compile error, not a hang.
  - The result can be any owned value: numbers, `Text` and strings, `Vec`s, enums, nested structs. The build lays it out as read-only data. A constant is a place that lives as long as the program: reading it projects, and `.clone()` gives an owned copy. It can't hold a handle to runtime state.
  - Constants are values, never types. They're computed after type checking, so a type never waits on build-time code. Array lengths in types are constant expressions with no calls (§4).
- **`embed("path")`,** which reads a file inside the package and gives `Bytes`: a `Copy` handle to read-only data in the build. Builds hash it, so they stay reproducible. Static data lives as long as the program, so a handle to it can be stored.
- **Types,** for monomorphization, and **fieldwise derivations** (§3).

**A `const` bakes its result into the build, so it costs bytes; computing at load costs time on every start.** Choose per value: an eigenvalue solve's result is tiny, so it belongs in a `const`. There are no procedural macros (D-060).

---

## 11. Numerics

**CPU code always uses strict IEEE floats** (D-074). There's no fast-math mode, no reassociation, no implicit FMA contraction and no relaxed SIMD. Transcendentals come from the stdlib, compiled to WASM, never from the host (D-015).

**Tiers:** tier 0 emits WASM, whose float arithmetic is already IEEE-strict apart from NaN bits, and the integer rules in the table below. Its transcendentals are already the stdlib's (`std::math`, computed in f64 and rounded once; within an ulp at every point the tests sample, 20,000 per function in the ranges they choose, which is evidence rather than proof). NaN canonicalization is tier 1, with `@deterministic` (D-088). Tier 0's checks: the emitted WASM has no relaxed SIMD (a pass over every module), and a program's state hash is the same in Chrome and in the native host.

**Vector math uses WASM's 128-bit SIMD wherever the result is the same** (milestone 2; today the emitter uses no SIMD). That covers `vec2`, `vec3` and `vec4` arithmetic, and loops whose iterations the compiler can run four at a time. Standard SIMD rounds each lane exactly as the scalar operation does and has no fused multiply-add, so the bits are identical; relaxed SIMD stays forbidden. An operation whose SIMD form behaves differently stays scalar: float-to-int conversion saturates in SIMD but traps in the table below. A reduction keeps its fixed order (§14), so it uses SIMD only where that order allows.

**GPU code follows WGSL semantics,** and its results are presentation-only: GPU results can't reach `@deterministic` code (§14).

**Numeric semantics** (D-074):

| Case | CPU | GPU |
|---|---|---|
| Integer overflow | Traps in every build. `wrapping_add`, `wrapping_sub` and `wrapping_mul` wrap. | Wraps |
| Integer divide by zero, `MIN / -1`, a shift by the width or more | Traps | WGSL-defined values |
| Float → int | Truncates; out of range or NaN traps | WGSL-defined values |
| Float `%` | `a - b * trunc(a / b)`, exact (C's `fmod`): `a`'s sign, smaller than `b` in magnitude; NaN if `a` is infinite or `b` is 0 | WGSL's `a - b * trunc(a / b)`, rounded at each step |
| Int → int | Keeps the low bits (`u32(-1)` is 4294967295) | Keeps the low bits |
| Out-of-bounds index | Traps | WebGPU's robust access (a value from inside the buffer, or zero); a debug flag is later |
| NaN | Canonicalized wherever observable in `@deterministic` code: bit casts, sign tests, stores into `Plain` memory, hashing. Debug builds trap on NaN creation. | WGSL |

**A value's bytes are a deterministic function of its fields** (zeroed padding, canonical NaNs). Equal values don't always have equal bytes: `-0.0 == 0.0` (D-074).

**Built-in functions (T0)** are in scope everywhere, on CPU and GPU alike, and apply per component to vectors: `sin cos tan asin acos atan atan2 exp exp2 log log2 pow sqrt inverse_sqrt floor ceil round trunc fract abs sign min max clamp saturate mix step smoothstep`, `length distance dot cross normalize` for vectors, `select(if_false, if_true, cond)` (a `bool` condition; the two values may be of any one type), and `bitcast_u32 bitcast_i32 bitcast_f32` (`bitcast_u64`, `bitcast_f64` on the CPU). `dpdx`, `dpdy` and `fwidth` are for fragment shaders, in uniform control flow (WGSL's rule): not inside, or after an early `return` or `break` in, a branch on a value that differs between pixels (E0608, `gpu.uniformity`). The vector functions can also be called as methods: `v.length()`, `v.normalize()`. Integers have the methods `wrapping_add`, `wrapping_sub` and `wrapping_mul`. Conversions are calls of the type: `f32(n)`, `u32(x)`, `vec3(x)` (all components x), `vec3(y: 1.0)` (the rest zero), `vec4(v3, 1.0)`. `std::math` has `PI` and `TAU`.

**Evidence:** spike 01 hashed 1M evaluations of the grazer field, a mass integration and 10K raycasts, compiled from Rust with these rules. WASM in Chromium 152, in Chrome 154 and native aarch64 gave identical bits. That's Rust rather than wrela, on one machine.

---

## 12. GPU code

| Rule | Tier | Decisions |
|---|---|---|
| **Entry points:** `@compute(...)`, `@vertex`, `@fragment` | T0 | D-010, D-035 |
| **Builtins are typed:** `GlobalId`, `WorkgroupId`, `LocalId` (compute), `VertexIndex`, `InstanceIndex` (vertex), `FragCoord` (fragment), `ClipPosition` (a vertex shader's output), and `Flat<T>` for values that aren't interpolated, in `std::gpu`. A stage takes only its own (E0602). | T0 | D-046 |
| **Closures are allowed when statically resolved:** monomorphized and inlined; a loop over a fixed-size array may be unrolled. In tier 0 every call in GPU code is inlined, so a kernel reads its uniform data in place; diagnostics for unrolling or inlining blowups are later. | T0 (closures) / T1 (iterators) | D-047, D-088 |
| **Layout is automatic but lossless.** `GpuData` fixes a type's layout to WGSL rules everywhere (T0); lossy encodings are explicit types (T1). Nobody pads by hand. | T0 / T1 | D-049, D-084 |
| **A kernel's `mut` parameters must be safe to share across invocations:** `Slots<T>` (each invocation writes only its own slot: the index is the invocation's own `GlobalId`, which code can't build or change) in tier 0; atomics, `Append<T>` and `AtomicMap` in milestone 2. A plain `mut [u32]` is rejected. | T0 / M2 | D-084 |
| **Entry-point signatures:** a kernel returns nothing; a vertex shader returns a `ClipPosition` or a struct with one `ClipPosition` field (the rest are passed to the fragment shader); a fragment shader returns a `vec4`. The values passed on are numbers and float vectors (`f32`, `i32`, `u32`, `vecN`, each interpolated or in a `Flat<T>`), at most 16. Parameters are builtins, `GpuData` values (passed as one uniform block), `[T]` buffers to read, `mut Slots<T>`, and, for a fragment shader, the vertex shader's output (`gpu.entry`, `gpu.data`). A pipeline binds at most 8 storage buffers: its `[T]` and `Slots<T>` parameters, and its uniform block when that's over 64 KiB or its layout doesn't meet WGSL's uniform rules. | T0 | D-102 |
| **Uniform vs varying** is the target's own distinction, which WGSL already analyzes. | T0 | D-051 |
| **Workgroup-shared memory and barriers.** Spike 01's `place_vertices` needed them. The design is in §6.13: a chunk per `LocalId` to write, `shared.all()` to read, and `barrier(mut shared)` between the phases. | M2 | D-093 |
| **GPU interval arithmetic widens each result outward** by its operation's WGSL error bound, so it stays conservative (§13). | T0 | D-075 |
| **GPU-resident data is a type.** `GpuBuffer<T: GpuData>` is an opaque `Copy` handle: CPU code can create one (`buffer(count)`), pass it to kernels and shaders, and write into it, but can't read through it. WebGPU can't bind an empty buffer, so `buffer(0)` has room for one (zeroed) element, and on the GPU its `len()` is 1. `GpuSpan<T>` and copies between buffers are later. Names are placeholders. Milestone 2 makes a buffer an owned value, and adds `GpuSpan<T>` and copies (§6.13). | T0 | D-102 |
| **A buffer lives until the program's next call.** Tier 0 has no state that outlives a call of `frame` (or of another export), so nothing can refer to a call's buffers once it returns; the program destroys them (a `DestroyBuffer` command) when its next call begins. A buffer that persists across frames comes with the state that would hold it (later). Milestone 2 replaces this rule with ownership: a buffer lives with its owner (§6.13). | T0 | D-102 |
| **A dispatch or a screen pass can't both read and write one buffer:** passing the same buffer as a `[T]` and a `mut Slots<T>` is an error when the command runs, reported by the host (WebGPU's usage rule). `GpuBuffer` is a `Copy` handle, so the compiler can't see every alias; the hosts check every command. In milestone 2 the compiler rejects it (§6.13), and the hosts' check stays as a backstop. | T0 | D-102 |
| **Transfers are explicit.** `write(buf, at, values)` copies. `dispatch(kernel, groups: n, arg: value, ...)` records a dispatch, and `draw(vertex, fragment, vertices: n, arg: value, ...)` a draw, between `begin_screen_pass(clear: ...)` and `present()`; `GpuData` arguments travel as one uniform block, `GpuBuffer`s as buffers. A kernel or shader can't be called directly (E0606). Writes and dispatches take effect in recorded order, with no barriers between dispatches. GPU calls carry the `host` effect (`gpu.dispatch`). | T0 | D-102 |
| **A host program exports `frame(time: f32, width: u32, height: u32)`,** called once per frame: a host program without it, or with another signature, is an error (E0703), and so is an export named `memory` (the program's memory has that name). **`main.wrela` is the program's interface to the host:** its `pub` functions are the exports, so code shared between modules goes in other files. A library package has no `main.wrela` and no exports. Exports other than `frame`, with scalar and vector parameters and results, serve tests and tools; a vector crosses as its components (a `vec3` parameter is three `f32`s), and an 8- or 16-bit integer or a `bool` as an `i32`, of which it keeps the low bits (a `bool`, whether it's nonzero). Other types can't cross (E0703). | T0 | D-102 |
| **Readback is a request:** `gpu.read(span)` returns a `Pending` handle, polled on a later frame (§6.15). It has the `nondet` effect, so `@deterministic` code can't call it. | T2 | D-102 |

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

```wrela
use std::derive::{Interval, interval}

pub fn range_over(lo: f32, hi: f32) -> vec2 {
    let r = interval(|t: f32| sin(t) * t, Interval { lo, hi })   // r.lo <= sin(t) t <= r.hi
    vec2(r.lo, r.hi)
}
```

**In tier 0** they're `std::derive::{gradient, value_and_gradient, interval}`, over a closure or function of an `f32` or a float vector that returns an `f32`. `interval` takes the input's box type (`Interval`, `Box2`, `Box3`, `Box4`) and returns an `Interval`. `std::field::Surface` provides `gradient`, `sample` (distance and gradient) and `interval` for every surface. The compiler derives them from the function's body, and from every function it calls (`derive.gradient`, `derive.interval`):
- **Gradients** are forward mode, one tangent per input component. They agree with central differences within 3.4e-4 relative, across the test corpus (compiler/tests/tests/suite/derive.rs). Where a vector's `length` is zero and nothing moves it (inside a box's `length(max(q, 0))`), its tangent is zero. Where `min`'s or `max`'s arguments are equal, the tangent is the average of theirs, so std's `smin` has its true gradient on a blend's seam.
- **Derivations nest.** A derived function is ordinary code, so it can be derived again: `gradient` of a `gradient` component is a Hessian row (and once more, a third derivative), and `interval` of a `gradient` component bounds the gradient over a box, which is a local Lipschitz bound. A closure can pass part of its input to an inner derivation as data: `interval(|q: vec4| gradient(|s: vec3| f(s, q.w), q.xyz).x, b)` bounds the spatial gradient over space and time. Spike 13 (the tag `spike-13-field-math`) certifies meshes and an animation's topology from these.
- **Intervals** bound every value a point in the box can give, as the target computes it. On the CPU each rounded result is widened by its rounding (an ulp; four for the stdlib's transcendentals). On the GPU each is widened by twice its WGSL error bound, at least one ulp, plus 2⁻¹²⁶ for flushed subnormals. A branch on the input that could go either way runs both sides and joins their results. A loop whose exit depends on the input can't be bounded, nor can a recursion that runs under a branch on the input (E0701). Integers are exact when single-valued, otherwise their type's whole range. The test corpus encloses every sample of 10⁶ boxes per function, on both targets.
- **What can't be derived (E0700):** code that records GPU work; a function that uses its own derivation (each would need the next); a write to a captured variable that depends on the input, or, for an interval, that runs under a branch on it; and, for an interval, a projection (`-> mut T`) whose place a branch on the input chooses (both sides run, so it would be either place). Writes and reads through other projections are derived like any others.
- **Known limits of tier 0's intervals:** a NaN bound becomes infinite on the CPU, but not on the GPU, which may assume NaNs away; WGSL bounds `sin` and `cos` only on [-π, π], and outside it the same absolute error is assumed; a branch run speculatively can still trap on the CPU (an integer overflow, say) even if no point in the box would take it. Bounds lose tightness, without losing soundness, in three places spike 13 measured: choices made separately on one comparison (std's `smin` uses `min(a, b)` and `abs(a - b)`) are joined as if independent, so the gradient's bound doesn't tighten on a blend's seam; a branch doesn't narrow the ranges its condition tests; and `floor` of a range that crosses an integer gives two integers, so value noise across a lattice plane gets its whole range. On the GPU every call is inlined, so each nested derivation multiplies the code: a certifier that bounds the gradient's three components is 0.9 to 1.8 MB of WGSL.
| Lipschitz bound | Not a compiler feature: a stdlib method, `lipschitz(near)` (§17). Debug builds and tests check it against the derived local bound, the `interval` of the `gradient`. | T1 | D-056, D-092 |

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
- **Parallelism is data-parallel only** (D-062), through stdlib combinators (`par_each_mut`, `par_map_reduce`). Exclusivity proves disjointness, and captured data can only be read (§6.12); reductions combine in a fixed tree order; per-entity RNG streams keep results independent of scheduling. T2.

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
- **The closed list is published in the spec** (D-081). Candidates: `Copy`, `Clone`, `Option`, the iteration protocol, the operator traits, GPU entry-point lowering, `Plain` layout. **Open:** the final list.
- **Stdlib modules pass an admission test** (D-081): would this make sense in a program that isn't a game? Acoustics, for example, is engine code.
- **The stdlib is written in wrela** with a small unsafe core (D-081).

**Indicative stdlib map** (placeholder; module boundaries aren't decided):

| Module | Contents |
|---|---|
| `std::field` | `Field<C>`, `Surface`, `Lipschitz`, `Channels`, `Blend`, `Cat<T>`, primitives, combinators, noise |
| `std::units` | Suffix constants in SI: `m`, `cm`, `mm`, `kg`, `s`, `rad`, `deg`, … |
| `std::gpu` | `dispatch`, `GpuBuffer<T>`, `GpuSpan<T>`, `write`, `read`, `Append<T>`, `Slots<T>`, `AtomicMap`, typed builtins |
| `std::arena` | `Arena<T>`, `Handle<T>` |
| `std::stage` | `interpret`: a tape evaluator for fields built at runtime |
| `std::hash`, `std::serialize` | `StateHash`, `Serialize`, both `@fieldwise` |
| `std::collections` | `Vec`, and `SortedMap<K: Ord, V>`, which iterates in key order, so it's allowed in `@deterministic` code; `swap` and `replace` |
| `std::fmt` | `Format`, which `f"…"` calls |

### Fields are stdlib code

- **A new primitive is a type that implements `Surface`.** It must be `Copy` and `GpuData` (it travels to the GPU as data), and the combinators and derived methods come with it:

  ```wrela
  use std::field::{Surface, sphere}

  struct Cuboid: Copy + Clone + GpuData {
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
- **Tier 0's field is `std::field::Surface`:** a signed distance (`distance(self, p: vec3) -> f32`), with `gradient`, `sample` and `interval` derived, primitives (`sphere`, `ellipsoid`, `round_cone`, `half_space`), combinators (`union`, `smooth_union`, `intersect`, `translate`, `displace`) and noise (`value_noise`, `fbm`). Channels and Lipschitz facts below are tier 1.
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
- **Combinators are methods** (D-028): `a.smooth_union(b, k: 15cm)`, and n-ary over fixed arrays, `legs.smooth_union(k: 6cm)` (§4). There's no operator overloading on fields; vectors and units do get operators.
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

This is the subset needed for "hello field" (D-088 tier 0): a field, a derived gradient, one compute kernel and one fragment shader. No units or determinism. `compiler/tests/tests/suite/language.rs` builds this block, so it stays true; `examples/hello-field` is the full program.

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
    sphere(radius: r).smooth_union(round_cone(vec3(), vec3(y: 1.0), 0.3, 0.1), k: 0.1)
}

struct Grid: Copy + Clone + GpuData {
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
    let out: GpuBuffer<f32> = buffer(4096)
    let grid = Grid { origin: vec3(-1.0), cell: 0.125, n: 16 }
    dispatch(sample, groups: 64, field: blob(0.5), grid: grid, out: out)
    begin_screen_pass(clear: vec4(0.0, 0.0, 0.0, 1.0))
    draw(cover, normals, vertices: 3, field: blob(0.5 + 0.1 * sin(time)))
    present()
}
```

---

## 20. Tiers at a glance

| Tier | Features |
|---|---|
| **T0** | Statements and literals; functions with modes and named arguments; structs with defaults; enums (including `Option`); `const` with literal values; traits with associated types and default methods; monomorphized generics and `impl Trait`; projections and exclusivity; non-escaping closures; scalar, vector and matrix types; `@compute`/`@vertex`/`@fragment`/`@gpu`; typed builtins; invocation-safe kernel outputs (`Slots<T>`); lossless GPU layout through `GpuData`; GPU buffer handles, uploads, dispatches and draws from CPU code (D-102); derived `gradient` and `interval`; WGSL and WASM emission. Built in milestone 1. |
| **M2** | Workgroup-shared memory and barriers (D-093); atomics and `Append<T>` as kernel outputs. Changes to tier 0: `borrow` bindings and an owning `let` (§6.3); a `-> mut T` result borrowing only `mut` arguments (§6.4); `Copy` arguments copied before a call's `mut` access (§6.5); owned GPU buffers (§6.13); `Copy` implying `Clone` (§3). |
| **T1** | `if let` and `let … else`; `match mut`; `Eq` and `Ord`; `Text` and `f"…"` interpolation; unit suffixes in SI; build-time constants of any owned value, `embed` and const generics; fieldwise traits; declared traits with structural checks; `@diagnostic`; `@deterministic` and the numeric rules; Lipschitz bounds and bandlimits as stdlib methods; handles and arenas; `Plain`; runs as results and bindings; borrow structs; closures that capture values and can be stored; lossy GPU encodings; `String` and `str`; `Result`, `?` and panics; the pipeline-count query. |
| **T2** | Threads and parallel combinators; polled requests for IO and GPU readback; `@audio`; `stage::interpret`; any compiler tier in the browser (a non-goal). |

---

## 21. Open

- **Packages and dependencies** (D-087).
- **The keyword list** (§2).
- **The closed list of stdlib items the compiler knows** (D-081).
- **How a `@fieldwise` derivation chooses an enum's variant and passes each field's name,** which loading and `Serialize`'s stable field names need (§3).
- **Small conveniences the gameplay paper test asked for,** each decided on its own: type aliases, tuple structs, an enum's integer value, labelled `break`, reverse ranges, an assignment as a `match` arm, string patterns in `match`, and indexing with other integer types.
- **The stdlib's names for workgroup-shared memory** (§6.13). The design is decided; milestone 2 settles the names.
- **Symbolic links to directories in a package:** refused for now (E0208); following them waits on what a module tree is.
- **GPU buffer names and syntax, and kernels whose parameters exceed the target's binding limits** (§12, D-102).
- **`from param` annotations** (§6.4).
- **A general `schedule` construct** for any function's evaluation stays possible as future sugar (D-053). Add it only when kernels need it.
