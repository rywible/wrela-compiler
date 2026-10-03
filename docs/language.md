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
- **Value semantics, no garbage collector** (D-014). Memory is values, arenas, regions and handles. Every copy, move and mutation is visible where it happens (§6).
- **Generic code is monomorphized** (D-070, D-071). A value's structure is its type, so specialization is ordinary monomorphization and runtime numbers travel as data.
- **The compiler derives interpretations of pure numeric code:** gradients, intervals, Lipschitz bounds and pruning masks (D-012, D-080). Fields are the motivating use, but nothing about it is field-specific.
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
| A unit can follow a number as a suffix: `15cm` means `15 * cm` | T1 | D-025, D-076 |
| `**` is exponentiation; `^` is XOR | T0 | D-076 |

```wrela
let r = ellipsoid(radii: vec3(0.45m, 0.50m, 0.90m))
    .smooth_union(haunch, k: 15cm)      // a leading `.` continues the expression
    .displace(fbm(freq: 25 / m, octaves: 4), amp: 3mm)

let total = base +                      // a continued line ends with the operator
    extra
```

**Keywords** are listed in `spec/lexical.md` (L11), including those reserved for later tiers.

**Control flow (T0)** is expressions and statements in the Rust family: `if`/`else` and `match` are expressions; `for i in 0..n` (and `0..=n`) counts over integers, `for x in xs` walks an array or a run, and `_` names an unused loop variable; `while`, `loop`, `break`, `continue` and `return` work as usual. A block's value is its last line when that's an expression. A local that's bound and never used is a warning (W0001), unless its name starts with `_`. A `let` may shadow an earlier binding of the same name. A literal's type is inferred from the whole function, later uses included: after `let a = 1` and `let b: f32 = a`, `a` is an `f32`.

**Limits.** Expressions, blocks, types and patterns nest at most 128 deep, and an expression's tree is at most 512 deep (E0112). A generic function's instantiations go at most 256 calls deep, and their type arguments, like any value's type, have at most 4096 parts, counting a part each time it appears (E0412, E0329): a type that doubles with each step, like `(a, a)`, grows past any machine. A value larger than the CPU stack traps when the function holding it is entered, as a stack overflow does; in GPU code, a type larger than WGSL allows (2³¹ − 1 bytes) is an error (E0702).

---

## 3. Items

### Functions (T0)

- **Named arguments are optional, Kotlin-style** (D-039). Positional arguments come first, and once an argument is named, the rest must be named too. Parameters may have defaults, which are literal values in tier 0 (`fn.named-args`, `fn.defaults`). Arguments are evaluated in the order they're written, named ones too, whatever the order of the parameters they go to. A lint suggesting names for bare literals such as `6cm` or `true` isn't built yet.
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

(Sketch 01 writes this with units, `f32<m>` and `6cm`, and a field type with channels, `Field<Tissue>`: both tier 1. Its kind parameter, `Exact`, is now a fact: §17.)

### Structs (T0)

- **Fields may have defaults**, which must be evaluable at compile time (D-048); in tier 0, literal values. A struct literal may omit defaulted fields (`struct.defaults`).
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
- **Structural defaults** are default methods that loop over a type's fields at compile time (§10, D-060). That's how `StateHash`, `Serialize` and engine traits like `SimState` get implemented without anyone writing them by hand. The type still opts in by declaring the trait. T1.

  ```wrela
  pub trait StateHash {
      fn state_hash(self, h: mut Hasher) {
          for field in Self::fields() {      // unrolled: each field is checked with its own type
              self.[field].state_hash(mut h)
          }
      }
  }
  ```

### Constants (T0 for literal values; T1 when the initializer must be evaluated)

`const` items are evaluated at compile time (D-073). In tier 0 a constant's value is a literal value: literals, vector constructors, array and struct literals, and other constants (`const.literal`). A `const` whose initializer calls functions needs the compile-time interpreter, which is tier 1 (D-088). Staging work earlier is done by writing a `const`, never by relying on the optimizer (D-072).

```wrela
const HOOF_MODES = modal_modes(hoof())     // an eigenvalue solve, run by the compiler
```

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
| `vec2`, `vec3`, `vec4`, `mat2`, `mat3`, `mat4` | f32 vectors and square matrices. Vectors generic over a unit (`vec3<m>`, §5) and `Quat` are tier 1. | T0 | D-076 |
| `[T; N]` | Fixed-size array; `[x; N]` repeats a `Copy` value. GPU code has no empty arrays (WGSL's), so there N is at least 1. | T0 | |
| `[T]`, `mut [T]` | A contiguous run, borrowed or mutable: the language's one view type. In tier 0 it's a parameter's type only; in tier 1 it's a non-escaping type, usable wherever one is (§6.6). An array passes for a run. `xs.len()` is a run's or an array's length, a `u32`. | T0 / T1 | §6.2 |
| `(A, B)` | Tuple | T0 | |
| `Option<T>` | An ordinary enum in the prelude (`Some`, `None`); there's no null | T0 | §6.1 |
| `Result<T, E>` and `?` | Recoverable errors | T1 | D-061, D-088 |
| `String`, `Text`, `str` | Heap-owned UTF-8, region-resident text, a borrowed run | T1 | D-087 |
| `borrow T`, `mut T` | Projection types: non-escaping. Returning them is T0; using them as type arguments (`Option<borrow T>`) comes with views and iterators in T1. | T0 / T1 | D-064, D-084, D-088 |
| Closures | Non-escaping by default (T0). An `@escaping` closure captures only owned values and handles (T1). | T0 / T1 | D-064 |
| `Handle<T>`, `Arena<T>`, `List<T>`, `Region<T>` | Stdlib containers (§6) | T1 | D-065 |
| `Unorm8`, `Oct16`, … | Lossy encodings are always explicit types | T1 | D-049 |

**`&T` doesn't exist** (D-064). There's no reference type to store, so there are no lifetimes.

Implicit conversions don't exist either: an integer literal can be any numeric type, but a value converts only with a call such as `f32(n)` or `u32(i)` (`ty.scalars`). Units, strings, `?` and `unsafe` are rejected with a diagnostic saying which tier brings them (`ty.tiers`). `dyn` is reserved and never accepted (§18); its diagnostic still names a tier until milestone 2 changes it.

---

## 5. Units (T1)

Units are part of types and erase at compile time (D-022, D-025, D-076).

- **A unit is a dimension plus a scale.** `cm` is metres scaled by 0.01. Scales convert at compile time; dimensions are checked.
- **Types are written with angle brackets:** `f32<kg/m**3>`, `vec3<m>`, `Transform<m>`.
- **Values are written with arithmetic:** `1050 * kg/m**3`, or a suffix for a single unit, `15cm`.
- **Unit suffixes resolve in their own namespace,** which locals can't shadow. `2m` means metres even if a local is named `m`.
- **Angles are dimensionless, as in SI.** `rad` is the dimensionless unit and `deg` is a scale of it, so `90deg` is π/2, converted at compile time, and `sin(time * 1.3)` needs no unit. A bare number meant as degrees still compiles, so write `deg` where you mean degrees. Arc length is `r * θ`. (Revises D-076; see §18.)
- **A bare literal takes a unit only where the type is stated:** an annotation, a parameter, or a field's default, as in `tolerance: f32<m> = 0.002`. In arithmetic it doesn't: `x + 0.5` with `x: f32<m>` is an error (write `0.5m`), while scaling by a bare number, `x * 2.0`, is fine.
- **Units flow through derived gradients:** d(out)/d(in) has unit out/in.
- **Constraint:** no unit may collide with numeric syntax. There's no unit named `e`.

```wrela
let density: f32<kg/m**3> = 1050 * kg/m**3
let wrong = density + 2m        // error: can't add kg/m³ to m
```

---

## 6. Memory

**Goals:**

- **Memory-safe without a garbage collector** (D-014). Frame times stay predictable.
- **Every transfer is visible.** Nothing is copied, moved or mutated without it showing at the point where it happens. The one exception: calling a `mut self` method doesn't repeat `mut`, because the method's name already says what it does (§2).
- **No lifetime annotations, ever.** Agents and humans should never have to reason about lifetime parameters.
- **Friendly to determinism.** Behavior never depends on memory addresses, and snapshots are cheap (D-015).
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
| **Non-escaping types:** any type containing a view (`[T]`, borrowing iterators, projections) follows the projection rules. That's how views and iterator chains work without lifetimes. | T1 (views and iterators, D-088) |
| **Closures are non-escaping by default** (T0). An escaping closure is marked `@escaping` and captures only owned values and handles (T1). | T0 / T1 |
| **Long-lived relationships are handles into arenas,** never pointers. | T1 |
| **Regions:** `Region<T>` holds one root value in chunks. Its containers store offsets. Region-bound values (`Relocatable` but not `Plain`) stay in their region. | T1 |
| **GPU layout:** a declared `GpuData` trait fixes a type's layout to WGSL rules everywhere. This is how tier 0's lossless GPU layout is expressed. | T0 |
| **Rule IDs:** `mem.copy`, `mem.take`, `mem.clone`, `mem.modes`, `mem.receivers`, `mem.bindings`, `mem.projections`, `mem.exclusivity`, `mem.loops`, `mem.no-globals`, `mem.closures` in the conformance suite. | |
| **Byte-level data:** the declared traits `Plain` and `Relocatable`. | T1 |
| **Snapshots:** copy-on-first-write chunk checkpoints are stdlib operations on regions, not language features. | T2 |
| **Destructors:** deterministic, at end of scope in reverse order; user `Drop` for types that aren't region-bound (D-087). | T1 |
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

`[T]` is a contiguous run of `T`. As a parameter, it's borrowed like anything else. `mut [T]` is a mutable run.

### 6.3 Local bindings

Bindings use the vocabulary of parameters (§6.2): a binding either owns its value or projects a place.

| | Owns the value | Projects a place |
|---|---|---|
| **read-only** | `let x = …` | `borrow x = place` |
| **mutable** | `var x = …` | `mut x = place` |

- **`let` and `var` own.** Their value is a temporary, `take place`, `place.clone()`, or a place whose type is `Copy`, which is copied.
- **`let x = place` is an error when the type isn't `Copy`,** and the diagnostic offers three fixes: `borrow x = place` to read it in place, `let x = place.clone()` for a copy, or `let x = take place` to move it. So adding `Copy` to a type never changes what a binding borrows.
- **Names bound inside a pattern or a loop project the matched place,** read-only, or mutably with `mut`: `match e { Some(log) => … }`, `for g in world.grazers`, `for mut g in world.grazers`. Matching a temporary owns. A loop borrows its container for the whole loop, so there a copy and a projection can't be told apart.
- **Closures capture by projection** (§6.7). An `@escaping` closure captures owned values.

**Milestone 2 makes this change.** In tier 0, `let x = place` projects when the type isn't `Copy`, and `borrow` isn't a binding form yet.

```wrela
borrow g = world.grazers[h]    // read-only projection: no copy
let p = world.grazers[h].pos   // a vec3 is `Copy`: an owned copy
mut t = world.terrain          // mutable projection
t.edits.push(Edit::Dig { at, radius: 1m })
var spare = world.grazers[h].clone()   // an owned copy, mutable (fine: GrazerSim is `Plain`, so not region-bound)
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
- **A projection must come from a `borrow` or `mut` parameter.** You can't project from a local or a temporary.
- **What the result borrows.** A `-> mut T` result borrows only the `mut` arguments, because a `mut` projection can't come from a read-only place (E0512). A `-> borrow T` result borrows every `borrow` and `mut` argument; that's conservative, and a `from param` annotation can narrow it later if real code needs it. (Milestone 2; in tier 0 every result borrows every `borrow` and `mut` argument.)
- **Projections can't be stored** in structs or collections, or captured by escaping closures (§6.7). They never outlive the scope that received them, so no lifetimes are needed.
- **Projection types can be type arguments.** `borrow T` and `mut T` are non-escaping types (§6.6), so `arena.get(h)` returns `Option<borrow T>`, and an iterator can yield `mut T`. Inside a package, whether a type parameter accepts non-escaping types is inferred for each instantiation, as effects are (§7). A public generic states it at the package boundary (D-030).

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
- **There are no mutable globals.** State is passed in explicitly. Constants are fine.
- **There's no interior mutability** like Rust's `Cell` or `RefCell`. Shared mutable state lives in an arena and is reached through `mut` access to that arena.

### 6.6 Non-escaping types and views

**There is one kind of borrowed thing: the projection.** It's second-class. It dies with the scope that received it, and it's never stored in a value that can escape. Every form below follows the same rules:

```text
projection
├── borrow T, mut T       a function's result, or a binding (§6.3, §6.4)
├── [T], mut [T]          runs, the one view type (§4)
├── a capturing closure   (§6.7)
├── a borrowing iterator  (this section)
└── GpuSpan<T>            a view of a GPU buffer (§6.13)
```

Some values need to *hold* access to other data: views, and iterators that walk a container. In wrela these are **non-escaping types**, after Swift's `~Escapable`.

- **The stdlib provides the roots:**
  - runs, `[T]` and `mut [T]`: the one view type over contiguous data (§4)
  - iterators that borrow a container
  - closures that capture projections
- **Any type with a non-escaping part is non-escaping too.** This is structural and automatic.
- **Non-escaping values follow the projection rules.** They can be passed down and returned as projections. They can never be stored in an escapable type, put in a collection, or captured by an escaping closure.

```wrela
for e in cell.edges().filter(|e| crosses(e)) { ... }   // a borrowing iterator chain: fine

struct Window { rows: [f32], width: u32 }              // non-escaping, because `[f32]` is
fn window(img: Image, r: Rect) -> Window { ... }       // a projection of `img`
struct Cache { last: Window }                          // error: `Window` is non-escaping; store the
                                                       //   Rect and project again when you need it
```

**What a non-escaping value borrows can grow** (D-084). It starts as the sources it was projected from. These extend it:
- assigning into one of its fields
- passing it as `mut` alongside other borrowed arguments

In both cases its borrow set gains those sources. The check still stays inside one function. Without this rule, a view could outlive what it points at.

This is what removed most of the cost of second-class references (D-058). Views and iterator chains read just as they do in Rust, without lifetimes.

### 6.7 Closures

- **By default, a closure parameter is non-escaping.** The callee must finish with it before it returns. These closures can capture projections, both `borrow` and `mut`, and that access counts as live for the duration of the call. A function type (`fn(f32) -> f32`) is a parameter's type only, so a closure can't be returned or stored: not in a struct, enum, tuple or array, and not as a generic's type argument (`mem.closures`). A closure can be named with `let` (not `mut`), and it borrows its captures while the name is live. In tier 0 a closure can't capture another closure.
- **An escaping closure is marked `@escaping`.** It can be stored or spawned, and it captures only owned values and handles. Moving a named place into one is written `take`.

```wrela
world.grazers.par_each_mut(|g| step(mut g, world.terrain, intent))   // non-escaping: may capture projections
timers.after(2s, @escaping |w| w.spawn(take herd))                   // escaping: owns what it captures
```

### 6.8 Handles and arenas

- **`Arena<T>` stores values in generational slots.** `Handle<T>` is an index plus a generation. It's `Copy`, `Plain` (§6.10), and can be stored anywhere.
- **Access:**
  - `arena[h]` is a projection, either borrow or `mut` depending on context. A stale handle panics.
  - `arena.get(h)` returns an `Option` of a projection.
  - `arena.remove(h)` moves the value out, unless the value is region-bound (§6.10). Then it's destroyed in place, or moved between containers of the same region.
- **Graphs, parent links and "this entity refers to that one" are all handles.** References never last longer than a call.

### 6.9 Allocation and regions

- **Allocation is an effect** (D-010). It's forbidden in GPU and audio code.
- **The global heap** backs `Vec`, `String` and `Box`. They own their memory and free it when dropped.
- **Scratch arenas** (per frame, per pass) hand out non-escaping containers:

```wrela
fn build_lists(scratch: mut Scratch, ...) {
    var near = scratch.list::<Handle<GrazerSim>>()   // non-escaping: borrows `scratch`
    ...
}   // `scratch.reset()` needs `mut scratch`, so exclusivity proves no list outlives a reset
```

- **A region, `Region<T>`,** is a self-contained memory area holding one root value of type `T`. It's built from fixed-size chunks.
  - Region containers (`Arena`, `List`, `Text`) allocate inside their region and store **region-relative offsets**, never absolute pointers.
  - You reach the root through `region.read(|w| ...)` or `region.write(|mut w| ...)`.

- **Region containers are created explicitly** from the region: `r.list::<T>()`, `r.arena::<T>()`. There's no ambient "current region" at creation time.
- **Each container records its region's ID.** When it grows, it finds its region's allocator in the thread's table of open regions.
  - Several regions can be open at once.
  - Using a container whose region isn't open is a panic.

This lives in the stdlib's unsafe core (§6.14). The compiler doesn't know regions exist.

### 6.10 Plain and relocatable data

Two stdlib traits describe data that can be handled as raw bytes. A type declares them, as it declares `Copy`, and the compiler checks that every field has them (§7):

| Trait | Contains | Meaning | Used for |
|---|---|---|---|
| `Plain` | numbers, `Handle<T>`, fixed arrays, `Plain` structs | No pointers and no offsets. Padding is zeroed and NaNs are canonical (D-074), so a value's bytes are a deterministic function of its fields. They aren't equal for every pair of equal values: `-0.0 == 0.0`. Copy the bytes anywhere. | GPU buffers (D-049), network messages, handles |
| `Relocatable` | `Plain` data plus region containers | No absolute pointers. Valid whenever the *whole region* moves together. | Region roots, sim state, keyframes |

- **Heap types (`Vec`, `String`, `Box`) are neither.**
- **The engine's `SimState`** is a declared trait (D-078) that requires `Relocatable`. So a game's world can always be checkpointed and synced as bytes.
- **WASM is little-endian everywhere,** so the bytes are portable between clients.

**Region-bound values (D-084).** A value that's `Relocatable` but not `Plain` holds offsets into its region, so it's *region-bound*:
- It can't be owned outside its region: no `take` or `.clone()` into a local, a capture or another region.
- It's reached by projection only.
- It's cloned only into the same region, through that region's containers.

This is what makes "nothing outside the region points into it" (§6.11) a rule rather than a hope.

**Pointing outward is forbidden too.** Sim state never holds a handle into an arena outside its region, because a rewind would restore the handle but not the arena. It refers to outside data by a deterministic key instead, such as a definition plus a seed (D-084).

**GPU layout.** A type that crosses to the GPU declares `GpuData`, which fixes its layout to WGSL's rules everywhere it's used. Other `Plain` types use a natural CPU layout. There's one layout per type, never two.

### 6.11 Snapshots: copy-on-first-write chunks

```wrela
region.checkpoint(epoch)     // start an epoch: each chunk's old bytes are saved the first time it's written
region.rewind(to: epoch)     // restore every chunk saved since then
region.keyframe() -> Bytes   // the whole region, for save files and repro bundles
```

**Why it's sound:**
1. **Every write to region data goes through a stdlib container.** Containers mark a chunk when they hand out `mut` access to it. Inline data is covered by the projection that reaches it.
2. **Projections are second-class.** None can outlive the call that created it. `checkpoint` and `rewind` take `mut` access to the region, so exclusivity proves that no projection is live across an epoch boundary. A write can never land in the wrong epoch.
3. **Nothing outside the region points into it, and nothing inside points out.** Region-bound values can't leave (§6.10), outside code holds only handles (indices), and sim state refers outward only by deterministic keys.

**Cost, as estimates to be measured:**
- Per-tick cost is proportional to the chunks *written*, not the world's size. Every simulated entity is written every tick, so the cost is roughly the size of the active entity data. Static data, such as the terrain's base field and inventories, is free. For example, 10,000 active entities at 256 bytes each come to about 2.5 MB per tick, roughly 0.3 ms.
- Keyframes cost a full copy, so they're taken every few seconds, not every tick.
- Under D-042, clients only roll back their own predicted entities, which are small.

**Details (D-084):**
- **Parallel combinators** mark every chunk they'll hand out in a sequential pre-pass, so two threads never race to be first to write a chunk.
- **Checksums are incremental.** Each chunk keeps a hash. Only chunks written this tick are rehashed, and the per-chunk hashes are combined. Raw-byte hashing is safe because of canonical bytes (§6.10).
- **Keyframes are same-build only.** They're for rollback, repro bundles and network sync within one version, because a keyframe is a memory image: every layout is baked in.
- **Save files use structural `Serialize`** (D-060), with stable field identifiers, so saves survive layout changes and can be migrated.

Bulk data belongs in containers, which mark one element's chunk at a time. A huge inline array in the root would be marked all at once.

### 6.12 Threads

- **Platform:** web workers plus SharedArrayBuffer (D-017). Every worker shares one WASM memory, whose maximum is reserved at startup. vision.md describes the thread layout (D-098).
- **Two declared stdlib traits:** `Sendable` (may move to another thread) and `Shareable` (may be borrowed from several threads at once). A type declares them, and every field must have them (§7).
- **Parallelism goes through data-parallel combinators** (D-062). Exclusivity proves disjointness, so there are no locks in game or engine code.
- **Atomics and queues** live in the stdlib's unsafe core.

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
- **Workgroup-shared memory** (milestone 2, D-093) is a value that all of a kernel's invocations share. Between barriers, each invocation writes only its own chunk, which the stdlib assigns by `LocalId`, and reads go through `shared.all()`. `barrier(mut shared)` ends every projection of the value, so a phase of writes and a phase of reads can't overlap: the argument §6.11 makes for `checkpoint`. A barrier must sit in uniform control flow, as WGSL requires, and the checker rejects one that doesn't.
- **`@audio` code** borrows preallocated `Plain` buffers and never allocates.

### 6.14 The unsafe core

`unsafe` exists only so the stdlib can implement things the checker can't verify:
- `Vec`, `Arena`, `Region`
- the open-region table
- atomics and queues
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

error: `world` is already mutably borrowed
  --> step(mut g, world, intent)
  note: `g` borrows `world.grazers` mutably until line 14
  help: pass only what `step` reads: `world.terrain`

error: can't move out of `world.grazers[h]`: it's a projection
  help: use `world.grazers[h].clone()`, or `world.grazers.remove(h)` to take it out of the arena

error: `log` was moved into `archive` on line 5
  help: write `archive(log.clone())` there if you still need `log`

error: `edits` is region-bound and can't be owned outside its region
  --> var saved = world.terrain.edits.clone()
  help: read it through a projection, or clone it into a container of the same region
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
| **Every trait a type has is declared.** `Plain`, `Relocatable`, `Sendable` and `Shareable` are declared like `Copy`, and checked structurally: every field must have the trait. There are no auto traits (§18). Declaring `Copy` implies `Clone`, and a generic type's declared trait holds for the instantiations whose arguments have it (§3). | T1 | D-078 |
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
- **`recursion`** is a cycle in the call graph after monomorphization. Whether a recursion is bounded can't be checked, so every cycle counts (revises D-094).

**Each context forbids a subset:**

| Context | Forbidden | Tier |
|---|---|---|
| GPU entry points (`@compute`, `@vertex`, `@fragment`) | `alloc`, `io`, `nondet`, `recursion`, `host`, `panic`. In tier 0 the language has no allocation, I/O or randomness, so `host` (recording GPU work) and recursion are what's checked (E0600, `eff.gpu`). | T0 |
| `@audio` | `alloc`, `io`, `recursion`, `host` | T2 |
| `@deterministic` | `nondet`, `host` (except declared deterministic host calls), `recursion` (D-094) | T1 |
| Compile-time evaluation | `io` (except declared embeds), `host`, `nondet` | T1 |
| Derived interpretations (gradient, interval) | `alloc`, `io`, `nondet`, `host` (E0700, `eff.derived`) | T0 |

- **Inside a package, effects are inferred** (D-010, D-030). Annotations only assert. Errors show the call chain: `extract → foo → bar allocates at line 42`.
- **Exported functions state their effects** (D-030), at the package boundary once packages exist; `wrela fix` will write the annotations, and queries show what was inferred. Neither is in tier 0.
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
| `@comptime` | functions, parameters | Runs, or is known, when the compiler runs (D-051, D-073) | T1 |
| `@deterministic` | functions, function types | The determinism constraint (§14) | T1 |
| `@intrinsic` | functions, in std only | The compiler provides the body: GPU commands and built-in math (D-081). Anywhere else it's rejected. | T0 |
| `@escaping` | closures | The closure may outlive the call. §6.7 writes it on the closure expression: `@escaping \|w\| ...`. (D-064) | T1 |
| `@diagnostic(...)` | traits, types | A library-authored error message; not part of the type (D-055, D-081) | T1 |
| `@audio` | functions | Audio-worklet entry point (D-072) | T2 |

User-defined metadata, if it's ever needed, gets a different syntax, so `@` always means semantics (D-037). An unknown attribute is E0204; a tier-1 one is E0903 (`attr.closed`).

---

## 10. Compile-time evaluation (T1)

- **An interpreter inside the compiler** runs `@comptime` functions and `const` initializers (D-073).
- **Types are compile-time values,** so reflection can enumerate a struct's fields and an enum's variants. That's what structural defaults (§3) are written with (D-060).
- **Compile-time heap values may be embedded in the output only if they're `Plain`** (D-073).
- **File reads at compile time** go through declared `embed` paths inside the package, hashed so builds are reproducible (D-073).
- **A loop over a type's fields runs at compile time.** `for field in Self::fields()` is unrolled, and its body is checked once for each field, with that field's type. `self.[field]` reads the field; the spelling is settled in milestone 2 (§21). Structural defaults are written this way (§3).
- **No procedural macros** (D-060).

---

## 11. Numerics

**CPU code always uses strict IEEE floats** (D-074). There's no fast-math mode, no reassociation, no implicit FMA contraction and no relaxed SIMD. Transcendentals come from the stdlib, compiled to WASM, never from the host (D-015).

**Tiers:** tier 0 emits WASM, whose float arithmetic is already IEEE-strict apart from NaN bits, and the integer rules in the table below. Its transcendentals are already the stdlib's (`std::math`, computed in f64 and rounded once; within an ulp at every point the tests sample, 20,000 per function in the ranges they choose, which is evidence rather than proof). NaN canonicalization is tier 1, with `@deterministic` (D-088). Tier 0's checks: the emitted WASM has no relaxed SIMD (a pass over every module), and a program's state hash is the same in Chrome and in the native host.

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
| NaN | Canonicalized wherever observable in `@deterministic` code: bit casts, sign tests, stores into `Plain` or `Relocatable` memory, hashing. Debug builds trap on NaN creation. | WGSL |

**A value's bytes are a deterministic function of its fields** (zeroed padding, canonical NaNs). Equal values don't always have equal bytes: `-0.0 == 0.0` (D-074).

**Built-in functions (T0)** are in scope everywhere, on CPU and GPU alike, and apply per component to vectors: `sin cos tan asin acos atan atan2 exp exp2 log log2 pow sqrt inverse_sqrt floor ceil round trunc fract abs sign min max clamp saturate mix step smoothstep`, `length distance dot cross normalize` for vectors, `select(if_false, if_true, cond)` (a `bool` condition; the two values may be of any one type), and `bitcast_u32 bitcast_i32 bitcast_f32` (`bitcast_u64`, `bitcast_f64` on the CPU). `dpdx`, `dpdy` and `fwidth` are for fragment shaders, in uniform control flow (WGSL's rule): not inside, or after an early `return` or `break` in, a branch on a value that differs between pixels (E0608, `gpu.uniformity`). The vector functions can also be called as methods: `v.length()`, `v.normalize()`. Integers have the methods `wrapping_add`, `wrapping_sub` and `wrapping_mul`. Conversions are calls of the type: `f32(n)`, `u32(x)`, `vec3(x)` (all components x), `vec3(y: 1.0)` (the rest zero), `vec4(v3, 1.0)`. `std::math` has `PI` and `TAU`.

**Evidence:** spike 01 hashed 1M evaluations of the grazer field, a mass integration and 10K raycasts, compiled from Rust with these rules. WASM in Chromium 152, in Chrome 154 and native aarch64 gave identical bits. That's Rust rather than wrela, on one machine.

---

## 12. GPU code

| Rule | Tier | Decisions |
|---|---|---|
| **Entry points:** `@compute(...)`, `@vertex`, `@fragment` | T0 | D-010, D-035 |
| **Builtins are typed:** `GlobalId`, `WorkgroupId`, `LocalId` (compute), `VertexIndex`, `InstanceIndex` (vertex), `FragCoord` (fragment), `ClipPosition` (a vertex shader's output), and `Flat<T>` for values that aren't interpolated, in `std::gpu`. A stage takes only its own (E0602). | T0 | D-046 |
| **Closures and iterators are allowed when statically resolved:** monomorphized and inlined, fixed-size iterators unrolled. In tier 0 every call in GPU code is inlined, so a kernel reads its uniform data in place; diagnostics for unrolling or inlining blowups are later. | T0 (closures) / T1 (iterators) | D-047, D-088 |
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
| Pruning | `f.prune(bounds) -> LiveMask<F>`, `f.with_live(mask)` | T1 | D-045, D-080 |

- **A choice point** is a `min`, `max`, `select`, `if` or `match` arm whose outcome an interval can decide (D-080).
- **`LiveMask<F>` is opaque and typed by the function it prunes.** Library code can store and pass it, but can't read its bits (D-080).
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
- **Forbidden:** the clock, ambient randomness, GPU readback, unordered iteration, relaxed SIMD, anything that depends on memory addresses (D-015, D-052), and recursion (§8), because stack limits differ between engines (D-015, D-094).
- **A panic is a deterministic trap:** every client traps on the same tick (D-061).
- **Parallelism is data-parallel only** (D-062), through stdlib combinators (`par_each_mut`, `par_map_reduce`). Exclusivity proves disjointness; `Shareable` covers captured data; reductions combine in a fixed tree order; per-entity RNG streams keep results independent of scheduling. T2.

The sim/presentation split is an engine pattern built on this, not a language feature (D-052).

---

## 15. Errors (T1)

- **Recoverable failures are `Result<T, E>`, propagated with `?`** (D-061).
- **Bugs panic.** On the CPU, a panic is a trap, and its message says where in the source it happened (from the `wrela.lines` section of the program's WASM). GPU code can't panic; out-of-bounds access sets a debug flag or clamps (D-074).
- **What happens after a trap** is the program's policy. The engine rewinds to the tick's checkpoint and writes a repro bundle (D-087).

---

## 16. Concurrency (T2)

- **Threads:** values cross threads only if `Sendable`; data shared between threads must be `Shareable` (§6.12).
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
| `std::units` | `m`, `cm`, `mm`, `kg`, `s`, `rad`, `deg`, … |
| `std::gpu` | `dispatch`, `GpuBuffer<T>`, `GpuSpan<T>`, `write`, `read`, `Append<T>`, `Slots<T>`, `AtomicMap`, typed builtins |
| `std::region` | `Region<T>`, `Arena<T>`, `List<T>`, `Handle<T>`, checkpoints |
| `std::stage` | `interpret`: a tape evaluator for fields built at runtime |
| `std::hash`, `std::serialize` | `StateHash`, `Serialize`, with structural defaults |

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
- **A field returns a distance plus channels** (D-002). Channel structs opt in to `Blend`, and each member's type decides how it blends: `Color` in linear space, `f32` linearly, `UnitVec3` renormalized, `Cat<T>` from the winner (D-026).
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
- **Combinators are methods** (D-028): `a.smooth_union(b, k: 15cm)`, and n-ary on collections. There's no operator overloading on fields; vectors and units do get operators.
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
- **Procedural macros:** compile-time reflection replaces them (D-060)
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
- **Const generics.** An array's length is part of its type, but no generic parameter ranges over lengths: generic code over arrays takes a run, `[T]`. Whether std gets n-ary combinators over fixed arrays is open (§21).
- **`Span<T>`.** Runs, `[T]`, are the one view type (§4).

---

## 19. A tier-0 program

This is the subset needed for "hello field" (D-088 tier 0): a field, a derived gradient, one compute kernel and one fragment shader. No units, regions or determinism. `compiler/tests/tests/suite/language.rs` builds this block, so it stays true; `examples/hello-field` is the full program.

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
| **T1** | Units; `@comptime`, evaluated `const` initializers and reflection; structural defaults; declared traits with structural checks; `@diagnostic`; `@deterministic` and the numeric rules; Lipschitz bounds and bandlimits as stdlib methods; pruning; handles, arenas and regions; `Plain`/`Relocatable`; views, iterators and other non-escaping types; lossy GPU encodings; strings; destructors; `Result`, `?` and panics; `@escaping`; the pipeline-count query. |
| **T2** | Threads and parallel combinators; polled requests for IO and GPU readback; `@audio`; checkpoints and keyframes; `stage::interpret`; any compiler tier in the browser. |

---

## 21. Open

- **Packages and dependencies** (D-087).
- **The keyword list** (§2).
- **The closed list of stdlib items the compiler knows** (D-081).
- **The spelling of reflective field access,** `self.[field]` (§10).
- **How a public generic states that it accepts non-escaping types** at a package boundary (§6.4). Inside a package it's inferred.
- **The stdlib's names for workgroup-shared memory** (§6.13). The design is decided; milestone 2 settles the names.
- **Symbolic links to directories in a package:** refused for now (E0208); following them waits on what a module tree is.
- **GPU buffer names and syntax, and kernels whose parameters exceed the target's binding limits** (§12, D-102).
- **`from param` annotations** (§6.4).
- **Region chunk sizes and undo-ring sizes** (§6.11).
- **One container family for the heap and regions.** `Vec` and `List`, and `String` and `Text`, differ only in where their memory lives. A container with a memory parameter could replace each pair, if its type can still say whether a value is region-bound (§6.10).
- **N-ary combinators over fixed arrays** (`impl Surface for [F; N]`): a creature's legs want one, but there are no const generics (§18).
- **A general `schedule` construct** for any function's evaluation stays possible as future sugar (D-053). Add it only when kernels need it.
