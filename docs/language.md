# wrela: the language

*The one prose reference for the language, as it stands. The syntax is imagined until the parser exists (M1). As features are built, their rules move into executable form (the grammar in `spec/`, the conformance tests, `wrela explain`), and the prose here shrinks to a pointer. Decision IDs (D-NNN), sketches and spikes refer to the design record in the git tag `design-archive-2026-10`.*

## How to read this

- **Rules cite the decisions they came from** (D-NNN), for history. This document is the current state.
- **Every feature has a tier** (D-088):
  - **T0:** milestone 1, the first program that draws a field.
  - **T1:** milestone 2 for the core (units, determinism, compile-time evaluation, regions, iterators, errors); milestone 3 for facts, bandlimits, pruning and lossy GPU encodings.
  - **T2:** later; tracked in the vision backlog.
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
| A newline ends a statement, unless it's inside open brackets or the next line starts with `.` | T0 | D-038, D-079 |
| A binary operator that continues a line must *trail* the line; a leading `-` or `\|` starts a new expression | T0 | D-079 |
| `;` may separate statements on one line; the formatter normalizes | T0 | D-038 |
| `//` comments, `///` doc comments | T0 | Placeholder (sketches) |
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

**Keywords:** the sketches use `fn let var mut take borrow struct enum trait impl for in if else match return pub use const self Self true false unsafe`, plus the usual `while loop break continue`. **Open:** this is an inventory, not a decision.

---

## 3. Items

### Functions (T0)

- **Named arguments are optional, Kotlin-style** (D-039). Positional arguments come first, and once an argument is named, the rest must be named too. Parameters may have defaults. A lint suggests names for bare literals such as `6cm` or `true`.
- **Parameters have modes** (§6): `x: T` (borrow), `x: mut T`, `x: take T`.
- **Properties are attributes** (§9): `@deterministic fn step(...)`.
- **Return-position `Field<K, C>`** (or any trait-shaped type) names one concrete, inferred type, like Rust's `impl Trait` (D-070).

```wrela
fn leg_segment(len: f32<m>, r_top: f32<m>, r_bottom: f32<m> = 6cm) -> Field<Exact, Tissue> {
    round_cone(vec3(), vec3(y: -len), r_top, r_bottom).with(HIDE)
}

leg_segment(45cm, r_top: 9cm)          // positional first, then named
```

### Structs (T0)

- **Fields may have defaults**, which must be evaluable at compile time (D-048). A struct literal may omit defaulted fields.
- **A struct opts in to traits in its declaration:** `struct Tissue: Blend { ... }` (D-026, D-078).
- **`..base`** fills the remaining fields from another value. Placeholder: sketch 01 uses it; no decision covers it.

```wrela
pub struct CreatureLook {
    pub surface: Mesh   = Mesh { tolerance: 2mm },
    pub shadow:  Shadow = Shadow::Map { resolution: 1024 },
}

const GRAZER_LOOK = CreatureLook { surface: Mesh { tolerance: 1mm } }   // shadow keeps its default
```

### Enums (T0)

Enums are sum types with payloads, matched with `match`. They're how structure is chosen at runtime from a known, finite set: every case is compiled, with a uniform branch (D-070).

```wrela
pub enum Edit: SimState + StateHash + Serialize + Copy {
    Dig  { at: vec3<m>, radius: f32<m> },
    Fill { at: vec3<m>, radius: f32<m> },
}
```

### Traits and impls

- **Traits have associated types and default methods** (D-071). T0.
- **Coherence follows Rust's orphan rule:** an `impl` lives in the crate of the trait or of the type (D-071). T0.
- **Structural defaults:** a trait can provide an implementation written in ordinary wrela that runs at compile time over a type's fields (D-060). That's how `Copy`, `Clone`, `StateHash`, `Serialize` and engine traits like `SimState` get implemented without anyone writing them by hand. T1. **Placeholder syntax:** `@comptime default for<T: struct> { ... }` (sketch 03 §5).

### Constants (T0 for literal values; T1 when the initializer must be evaluated)

`const` items are evaluated at compile time (D-073). A `const` whose initializer calls functions needs the compile-time interpreter, which is tier 1 (D-088). Staging work earlier is done by writing a `const`, never by relying on the optimizer (D-072).

```wrela
const HOOF_MODES = modal_modes(hoof())     // an eigenvalue solve, run by the compiler
```

### Modules and packages

**Modules (T0):** a program is one package. A file is a module and a directory is a module tree: `shapes/blob.wrela` is `shapes::blob`. `use` imports names; `pub` makes an item visible outside its file. The stdlib's root is `std::` (D-081).

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
| `u8`, `i8`, `u16`, `i16` | CPU; on the GPU only packed in storage | T0 | D-074 |
| `f16` | Explicit lossy type | T1 | D-049 |
| `vec2<U>`, `vec3<U>`, `vec4<U>`, `mat3`, `mat4`, `Quat` | Vectors are generic over a unit (§5); `vec3` alone is unitless | T0 | D-076 |
| `[T; N]` | Fixed-size array | T0 | |
| `[T]`, `mut [T]` | A contiguous run, borrowed or mutable | T0 | §6.2 |
| `(A, B)` | Tuple | T0 | |
| `Option<T>` | An ordinary enum; there's no null | T0 | §6.1 |
| `Result<T, E>` and `?` | Recoverable errors | T1 | D-061, D-088 |
| `String`, `Text`, `str` | Heap-owned UTF-8, region-resident text, a borrowed run | T1 | D-087 |
| `borrow T`, `mut T` | Projection types: non-escaping. Returning them is T0; using them as type arguments (`Option<borrow T>`) comes with views and iterators in T1. | T0 / T1 | D-064, D-084, D-088 |
| Closures | Non-escaping by default (T0). An `@escaping` closure captures only owned values and handles (T1). | T0 / T1 | D-064 |
| `Handle<T>`, `Arena<T>`, `List<T>`, `Region<T>` | Stdlib containers (§6) | T1 | D-065 |
| `Unorm8`, `Oct16`, … | Lossy encodings are always explicit types | T1 | D-049 |
| `dyn Trait` | CPU only, never in GPU or `@audio` code | T2 | D-071 |

**`&T` doesn't exist** (D-064). There's no reference type to store, so there are no lifetimes.

---

## 5. Units (T1)

Units are part of types and erase at compile time (D-022, D-025, D-076).

- **A unit is a dimension plus a scale.** `cm` is metres scaled by 0.01. Scales convert at compile time; dimensions are checked.
- **Types are written with angle brackets:** `f32<kg/m**3>`, `vec3<m>`, `Transform<m>`.
- **Values are written with arithmetic:** `1050 * kg/m**3`, or a suffix for a single unit, `15cm`.
- **Unit suffixes resolve in their own namespace,** which locals can't shadow. `2m` means metres even if a local is named `m`.
- **Angle is its own dimension.** `sin` takes `f32<rad>`, so mixing degrees and radians is a type error. Arc length is written `r * θ.ratio()`.
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
| **Bindings:** `let` projects a place read-only or owns a temporary; `var` owns mutably; `mut` projects a place mutably. | T0 |
| **Projections:** a function may return `-> borrow T` or `-> mut T` of one of its parameters. Projections never outlive the caller's scope. | T0 |
| **Exclusivity:** while a `mut` access is live, nothing may touch an overlapping place. Disjoint fields don't overlap; every element of a container overlaps every other (`pair_mut` and `split_at_mut` check at runtime). Checked within each function. | T0 |
| **No mutable globals, and no interior mutability** like `Cell` or `RefCell`. Shared mutable state lives in an arena. | T0 |
| **A projection must come from a `borrow` or `mut` parameter,** and the caller treats the result as borrowing every such argument. | T0 |
| **Non-escaping types:** any type containing a view (`Span<T>`, borrowing iterators, projections) follows the projection rules. That's how views and iterator chains work without lifetimes. | T1 (views and iterators, D-088) |
| **Closures are non-escaping by default** (T0). An escaping closure is marked `@escaping` and captures only owned values and handles (T1). | T0 / T1 |
| **Long-lived relationships are handles into arenas,** never pointers. | T1 |
| **Regions:** `Region<T>` holds one root value in chunks. Its containers store offsets. Region-bound values (`Relocatable` but not `Plain`) stay in their region. | T1 |
| **GPU layout:** a declared `GpuData` trait fixes a type's layout to WGSL rules everywhere. This is how tier 0's lossless GPU layout is expressed. | T0 |
| **Byte-level data:** the auto traits `Plain` and `Relocatable`. | T1 |
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

| Binding | Meaning |
|---|---|
| `let x = place` | A read-only projection of an existing place. Nothing is copied. |
| `let x = temporary`, or `let x = take place` | Owned and immutable. |
| `var x = temporary`, `var x = take place`, or `var x = place.clone()` | Owned and mutable. |
| `mut x = place` | A mutable projection of an existing place. |

```wrela
let g = world.grazers[h]       // read-only projection: no copy
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
- **The caller treats the result as borrowing every `borrow` and `mut` argument.** This is conservative. An optional `from param` annotation to narrow it can come later, if real code needs it.
- **Projections can't be stored** in structs or collections, or captured by escaping closures (§6.7). They never outlive the scope that received them, so no lifetimes are needed.
- **Projection types can be type arguments.** `borrow T` and `mut T` are non-escaping types (§6.6), so `arena.get(h)` returns `Option<borrow T>`, and an iterator can yield `mut T`. Generic code that accepts them declares its type parameter as possibly non-escaping.

### 6.5 Exclusivity

**Places** are locals, their fields, and elements of containers. Two places **overlap** when one is a prefix of the other. For example, `world` overlaps `world.terrain`, but `world.terrain` and `world.grazers` don't.

**Every element of a container overlaps every other element of that container.** The checker doesn't compare indices.

**The rule:** while a `mut` access is live, from where it's created to its last use, no other access may touch an overlapping place. In a single call, arguments may not overlap if any of them is `mut`.

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
- **The only runtime checks are explicit library calls** like `pair_mut` and `split_at_mut`.
- **There are no mutable globals.** State is passed in explicitly. Constants are fine.
- **There's no interior mutability** like Rust's `Cell` or `RefCell`. Shared mutable state lives in an arena and is reached through `mut` access to that arena.

### 6.6 Non-escaping types and views

Some values need to *hold* access to other data: views, and iterators that walk a container. In wrela these are **non-escaping types**, after Swift's `~Escapable`.

- **The stdlib provides the roots:**
  - `Span<T>` and `SpanMut<T>`, which are views over contiguous data
  - iterators that borrow a container
  - closures that capture projections
- **Any type with a non-escaping part is non-escaping too.** This is structural and automatic.
- **Non-escaping values follow the projection rules.** They can be passed down and returned as projections. They can never be stored in an escapable type, put in a collection, or captured by an escaping closure.

```wrela
for e in cell.edges().filter(|e| crosses(e)) { ... }   // a borrowing iterator chain: fine

struct Window { rows: Span<f32>, width: u32 }          // non-escaping, because Span is
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

- **By default, a closure parameter is non-escaping.** The callee must finish with it before it returns. These closures can capture projections, both `borrow` and `mut`, and that access counts as live for the duration of the call.
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

Two stdlib auto traits (D-054) describe data that can be handled as raw bytes:

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
- **Two stdlib auto traits:** `Sendable` (may move to another thread) and `Shareable` (may be borrowed from several threads at once).
- **Parallelism goes through data-parallel combinators** (D-062). Exclusivity proves disjointness, so there are no locks in game or engine code.
- **Atomics and queues** live in the stdlib's unsafe core.

### 6.13 GPU and audio

- **Modes map onto WebGPU bindings:** `borrow` becomes a read-only storage or uniform binding, and `mut` becomes a `read_write` storage binding.
- **WebGPU forbids aliasing writable bindings** within a dispatch, which matches exclusivity *per binding*.
- **That doesn't cover a single dispatch.** Every invocation holds the same `mut` binding at once, so exclusivity says nothing about how invocations share it (D-084). A kernel's `mut` parameters therefore accept only invocation-safe types:
  - atomics
  - `Append<T>`
  - `Slots<T>`, where each invocation writes only the slot keyed by its own ID

  A plain `mut` array parameter is rejected.
- **Data that crosses to the GPU must be `Plain` and `GpuData`.**
- **GPU-resident data is reached through handles** (D-102). `GpuBuffer<T>` is `Plain` and `Copy`, like `Handle<T>`, and can't be read through on the CPU. Uploads are explicit copies; readback is asynchronous and `nondet`. There's no zero-copy path between WASM memory and the GPU (vision.md).
- **`@audio` code** borrows preallocated `Plain` buffers and never allocates.

### 6.14 The unsafe core

`unsafe` exists only so the stdlib can implement things the checker can't verify:
- `Vec`, `Arena`, `Span`, `Region`
- the open-region table
- atomics and queues
- host bindings

Each package declares whether it uses `unsafe`. Game and engine code shouldn't need to, and the package policy flags it if it does.

### 6.15 Async

Async uses structured concurrency only (D-087). Projections and non-escaping values can't cross an `await` in a task that outlives its caller. That's how wrela avoids ever needing `Pin`.

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
| **`impl Trait` convention:** a trait-shaped type in return position is one inferred concrete type; in parameter position it makes the function implicitly generic. | T0 | D-070 |
| **Choosing structure at runtime:** an enum for a finite set (all cases compiled, uniform branch); `std::stage::interpret` for unbounded structure built at runtime. | T0 / T2 | D-070, D-053 |
| **Control-flow values are data:** loop bounds and comparisons become uniforms; unrolling is the optimizer's choice. | T0 | D-070 |
| **The pipeline-count query** reports how many instantiations each GPU entry point has, and why. | T1 | D-044, D-070 |
| **Auto traits:** `Plain`, `Relocatable`, `Sendable`, `Shareable`. A type gets one when all its fields have it, and can opt out. | T1 | D-054, D-078 |
| **Declared traits with a structural check:** a library trait can require that every field also implements it (`SimState` is the engine's example). | T1 | D-078, D-060 |
| **Library-authored diagnostics:** `@diagnostic(...)` attaches a message to a trait or type, for when a bound isn't met. | T1 | D-055 |
| **Effects are checked per instantiation.** | T0 | D-071 |

```wrela
/// Generic over any field with this shape; monomorphized per concrete field type.
@compute(64)
fn cull_blocks<F: Surface>(field: F, grid: Grid, live: mut Append<LiveBlock>, id: GlobalId) {
    let block = grid.block(id.x)
    if !field.interval(block.bounds).contains(0m) { return }   // `interval` is derived (§13)
    live.push(LiveBlock { block })
}
```

---

## 8. Effects and contexts

**Effects** (D-072): `alloc`, `io`, `nondet`, `recursion` (unbounded), `dyn`, `host`, `panic`.

**Each context forbids a subset:**

| Context | Forbidden | Tier |
|---|---|---|
| GPU entry points (`@compute`, `@vertex`, `@fragment`) | `alloc`, `io`, `nondet`, `recursion`, `dyn`, `host`, `panic` | T0 |
| `@audio` | `alloc`, `io`, `recursion`, `dyn`, `host` | T2 |
| `@deterministic` | `nondet`, `host` (except declared deterministic host calls), `recursion` (D-094) | T1 |
| Compile-time evaluation | `io` (except declared embeds), `host`, `nondet` | T1 |
| Derived interpretations (gradient, interval) | `alloc`, `io`, `nondet`, `host` | T0 |

- **Inside a package, effects are inferred** (D-010, D-030). Annotations only assert. Errors show the call chain: `extract → foo → bar allocates at line 42`.
- **Exported functions state their effects** (D-030). `wrela fix` writes the annotations, and queries show what was inferred.
- **Public higher-order functions inherit effects from their closure arguments** by default (D-030). `map` is GPU-safe whenever its closure is.
- **Staging is guaranteed or rejected, never best-effort** (D-072). The optimizer may hoist work, but code must not rely on it. Work that must happen earlier is written earlier, as a `const` or a parameter.

---

## 9. Attributes

`@` attributes are a **closed set defined by the language** (D-037, D-081). All of them except `@diagnostic` are part of a type or contract: they change what a function *is*.

| Attribute | On | Meaning | Tier |
|---|---|---|---|
| `@compute(x, y, z)` | functions | GPU compute entry point, with workgroup size | T0 |
| `@vertex`, `@fragment` | functions | GPU render entry points | T0 |
| `@gpu` | functions | Asserts the function is GPU-safe, so a violation errors at its definition (D-010) | T0 |
| `@comptime` | functions, parameters | Runs, or is known, when the compiler runs (D-051, D-073) | T1 |
| `@deterministic` | functions, function types | The determinism constraint (§14) | T1 |
| `@assert(fact)` | functions | A checked fact, e.g. `@assert(lipschitz <= 1.5, near: 10cm)`. The `near:` scope is optional. (D-077, D-092) | T1 |
| `@assume(fact)` | functions | A trusted fact, e.g. `@assume(lipschitz: 1)`, `@assume(bandlimit: ...)`, optionally scoped with `near:`. Debug builds spot-check by sampling within the scope; every assumption is greppable. (D-077, D-092) | T1 |
| `@escaping` | closures | The closure may outlive the call. §6.7 writes it on the closure expression: `@escaping \|w\| ...`. (D-064) | T1 |
| `@diagnostic(...)` | traits, types | A library-authored error message; not part of the type (D-055, D-081) | T1 |
| `@audio` | functions | Audio-worklet entry point (D-072) | T2 |

User-defined metadata, if it's ever needed, gets a different syntax, so `@` always means semantics (D-037).

---

## 10. Compile-time evaluation (T1)

- **An interpreter inside the compiler** runs `@comptime` functions and `const` initializers (D-073).
- **Types are compile-time values,** so reflection can enumerate a struct's fields and an enum's variants. That's what structural defaults (§3) are written with (D-060).
- **Compile-time heap values may be embedded in the output only if they're `Plain`** (D-073).
- **File reads at compile time** go through declared `embed` paths inside the package, hashed so builds are reproducible (D-073).
- **No procedural macros** (D-060).

---

## 11. Numerics

**CPU code always uses strict IEEE floats** (D-074). There's no fast-math mode, no reassociation, no implicit FMA contraction and no relaxed SIMD. Transcendentals come from the stdlib, compiled to WASM, never from the host (D-015).

**Tiers:** tier 0 emits WASM, whose float arithmetic is already IEEE-strict apart from NaN bits. The numeric rules (the table below, NaN canonicalization, stdlib transcendentals) are tier 1 with `@deterministic` (D-088).

**GPU code follows WGSL semantics,** and its results are presentation-only: GPU results can't reach `@deterministic` code (§14).

**Numeric semantics** (D-074):

| Case | CPU | GPU |
|---|---|---|
| Integer overflow | Traps in every build | Wraps |
| Integer divide by zero, out-of-range float → int | Traps | WGSL-defined values |
| Out-of-bounds index | Traps | Debug builds set an error flag; release builds clamp |
| NaN | Canonicalized wherever observable in `@deterministic` code: bit casts, sign tests, stores into `Plain` or `Relocatable` memory, hashing. Debug builds trap on NaN creation. | WGSL |

**A value's bytes are a deterministic function of its fields** (zeroed padding, canonical NaNs). Equal values don't always have equal bytes: `-0.0 == 0.0` (D-074).

**Evidence:** spike 01 hashed 1M evaluations of the grazer field, a mass integration and 10K raycasts, compiled from Rust with these rules. WASM in Chromium 152, in Chrome 154 and native aarch64 gave identical bits. That's Rust rather than wrela, on one machine.

---

## 12. GPU code

| Rule | Tier | Decisions |
|---|---|---|
| **Entry points:** `@compute(...)`, `@vertex`, `@fragment` | T0 | D-010, D-035 |
| **Builtins are typed:** `GlobalId`, `WorkgroupId`, `LocalId`, `ClipPosition`, and `Flat<T>` for values that aren't interpolated. The docs map them to WGSL's `@builtin(...)`. | T0 | D-046 |
| **Closures and iterators are allowed when statically resolved:** monomorphized and inlined, fixed-size iterators unrolled. Diagnostics flag unrolling or inlining blowups. | T0 (closures) / T1 (iterators) | D-047, D-088 |
| **Layout is automatic but lossless.** `GpuData` fixes a type's layout to WGSL rules everywhere (T0); lossy encodings are explicit types (T1). Nobody pads by hand. | T0 / T1 | D-049, D-084 |
| **A kernel's `mut` parameters must be safe to share across invocations:** atomics, `Append<T>`, `Slots<T>` (each invocation writes only its own slot), `AtomicMap`. A plain `mut [u32]` is rejected. | T0 | D-084 |
| **Uniform vs varying** is the target's own distinction, which WGSL already analyzes. | T0 | D-051 |
| **Workgroup-shared memory and barriers.** Spike 01's `place_vertices` needed them. **Open:** the design. | T0 | D-093 |
| **GPU interval arithmetic widens each result outward** by its operation's WGSL error bound, so it stays conservative. | T0 | D-075 |
| **GPU-resident data is a type.** `GpuBuffer<T: GpuData>` and `GpuSpan<T>` are opaque `Plain`, `Copy` handles: CPU code can pass them to kernels, write into them or copy between them, but can't read through them. Names are placeholders. | T0 | D-102 |
| **Transfers are explicit.** `gpu.write(buf, data)` copies. Calling a `@compute` function from CPU code (`dispatch`) records a dispatch; small `GpuData` arguments travel as uniforms, bulk data as buffers. Writes and dispatches take effect in recorded order, with no barriers between dispatches. GPU calls carry the `host` effect. | T0 | D-102 |
| **Readback is asynchronous:** `gpu.read(span)` returns a future and has the `nondet` effect, so `@deterministic` code can't call it. | T2 | D-102 |

```wrela
@fragment
fn shade<F: Surface + Channels<C>, C: Blend>(field: F, s: Skinned, lights: Lights) -> Color {
    let fp = fwidth(s.rest_pos)                         // this pixel's footprint
    let t  = field.channels(s.rest_pos, footprint: fp)  // only the channels used here are computed (D-002)
    let n  = s.rot * field.gradient(s.rest_pos, footprint: fp).normalize()   // derived gradient
    lights.shade(t.albedo, t.roughness, n)
}
```

---

## 13. Derived interpretations

The compiler derives these from any function that qualifies under the effect table (§8). It doesn't know what a field is (D-056).

| Interpretation | What it gives | Tier | Decisions |
|---|---|---|---|
| `gradient` | Forward-mode derivative | T0 | D-012 |
| `interval` | A conservative range over a box | T0 | D-012, D-075 |
| Lipschitz bound | Composed from declared facts on primitives; read through `facts::lipschitz(f)` at compile time | T1 | D-057, D-077 |
| Pruning | `f.prune(bounds) -> LiveMask<F>`, `f.with_live(mask)` | T1 | D-045, D-080 |

- **A choice point** is a `min`, `max`, `select`, `if` or `match` arm whose outcome an interval can decide (D-080).
- **`LiveMask<F>` is opaque and typed by the function it prunes.** Library code can store and pass it, but can't read its bits (D-080).
- **Declared facts** supply what derivation can't: `@assume(lipschitz: 1)` on `length`, and a range and `@assume(bandlimit: ...)` on noise (D-057, D-077). The compiler composes them through callers. Placeholder: D-077 spells only `lipschitz` and `bandlimit`, so the spelling of a range fact is open.
- **Bandlimits** let noise fade octaves finer than a footprint instead of aliasing, on the GPU and anywhere else a footprint is known (D-077).
- **Facts can be scoped** (D-092). `@assume(lipschitz: 1, near: 10cm)` means the fact holds wherever |f| < 10cm.
  - Derived bounds carry the scope through composition.
  - A consumer states the scope it needs. An interval test over a block that straddles the surface needs `near` of at least the block's radius.
  - Why: the stdlib's ellipsoid bound has an unbounded gradient at its centre. A global Lipschitz constant fails there, even though the constant holds everywhere culling, root-finding and sphere tracing look. Spike 01's probe and both authoring agents (D-095) hit this.

---

## 14. Determinism (T1)

`@deterministic` is an effect constraint on functions and function types (D-052).

- **Inside it:** strict floats (always true on the CPU), stdlib transcendentals, canonical NaNs at observation points.
- **Forbidden:** the clock, ambient randomness, GPU readback, unordered iteration, relaxed SIMD, anything that depends on memory addresses (D-015, D-052), and unbounded recursion, because stack limits differ between engines (D-015, D-094).
- **A panic is a deterministic trap:** every client traps on the same tick (D-061).
- **Parallelism is data-parallel only** (D-062), through stdlib combinators (`par_each_mut`, `par_map_reduce`). Exclusivity proves disjointness; `Shareable` covers captured data; reductions combine in a fixed tree order; per-entity RNG streams keep results independent of scheduling. T2.

The sim/presentation split is an engine pattern built on this, not a language feature (D-052).

---

## 15. Errors (T1)

- **Recoverable failures are `Result<T, E>`, propagated with `?`** (D-061).
- **Bugs panic.** On the CPU, a panic is a trap. GPU code can't panic; out-of-bounds access sets a debug flag or clamps (D-074).
- **What happens after a trap** is the program's policy. The engine rewinds to the tick's checkpoint and writes a repro bundle (D-087).

---

## 16. Concurrency (T2)

- **Threads:** values cross threads only if `Sendable`; data shared between threads must be `Shareable` (§6.12).
- **Async is structured concurrency only.** Projections and non-escaping values can't cross an `await` in a task that outlives its caller (D-087).

---

## 17. The stdlib boundary

- **The compiler knows three things** (D-050): the language, a closed list of stdlib items, and the execution targets.
- **The closed list is published in the spec** (D-081). Candidates: `Copy`, `Clone`, `Option`, the iteration protocol, the operator traits, GPU entry-point lowering, `Plain` layout. **Open:** the final list.
- **Stdlib modules pass an admission test** (D-081): would this make sense in a program that isn't a game? Acoustics, for example, is engine code.
- **The stdlib is written in wrela** with a small unsafe core (D-081).

**Indicative stdlib map** (placeholder; module boundaries aren't decided):

| Module | Contents |
|---|---|
| `std::field` | `Field<K, C>`, the kinds `Exact` / `Bound` / `Lipschitz`, `Surface`, `Channels`, `Blend`, `Cat<T>`, primitives, combinators, noise |
| `std::units` | `m`, `cm`, `mm`, `kg`, `s`, `rad`, `deg`, … |
| `std::gpu` | `dispatch`, `GpuBuffer<T>`, `GpuSpan<T>`, `write`, `read`, `Append<T>`, `Slots<T>`, `AtomicMap`, typed builtins |
| `std::region` | `Region<T>`, `Arena<T>`, `List<T>`, `Handle<T>`, checkpoints |
| `std::stage` | `interpret`: a tape evaluator for fields built at runtime |
| `std::hash`, `std::serialize` | `StateHash`, `Serialize`, with structural defaults |

### Fields are stdlib code

- **A field returns a distance plus channels** (D-002). Channel structs opt in to `Blend`, and each member's type decides how it blends: `Color` in linear space, `f32` linearly, `UnitVec3` renormalized, `Cat<T>` from the winner (D-026).
- **Kinds are ordinary types** (D-056, D-077):

  | Kind | Meaning |
  |---|---|
  | `Exact` | The true distance |
  | `Bound` | Never overestimates; Lipschitz ≤ 1 |
  | `Lipschitz` | Sign-correct; derived L may exceed 1 |

  Each op states in its return type what it preserves. `.to_bound()` divides by the derived L (D-077). Placeholder: sketch 01 has `Exact` convert to `Bound`, and `Bound` to `Lipschitz`, implicitly. No decision covers implicit conversions, and the language has no general mechanism for them yet.
- **Combinators are methods** (D-028): `a.smooth_union(b, k: 15cm)`, and n-ary on collections. There's no operator overloading on fields; vectors and units do get operators.
- **Fields built at runtime** from an unbounded space use `stage::interpret` (D-029, D-053). T2.

---

## 18. What wrela leaves out, compared with Rust

- **References as types,** and with them lifetime parameters, variance, `'static` and higher-ranked bounds (D-058, D-064)
- **`Pin`,** because references can't live across suspension points (§6.15)
- **`Cell` and `RefCell`** (interior mutability), and **`Rc` and `Arc`** in ordinary code (§6.5)
- **Mutable globals** (§6.5)
- **Implicit moves out of named places** (D-064)
- **A garbage collector** (D-014)
- **Procedural macros:** compile-time reflection replaces them (D-060)
- **Fast-math on the CPU** (D-074)
- **User-defined attributes** (D-037)
- **Null** (§6.1)

---

## 19. A tier-0 program

This is the subset needed for "hello field" (D-088 tier 0): a field, a derived gradient, one compute kernel and one fragment shader. No units, regions or determinism.

```wrela
use std::field::{Field, Bound, Surface, sphere, round_cone}
use std::gpu::{GlobalId, Slots}

/// A field. Its structure (a smooth union of two primitives) is its type;
/// the radii are data, so every blob shares one pipeline.
pub fn blob(r: f32) -> Field<Bound, ()> {
    sphere(radius: r)
        .smooth_union(round_cone(vec3(), vec3(y: 1.0), 0.3, 0.1), k: 0.1)
}

pub struct Grid: GpuData {
    origin: vec3,             // `vec3` without a unit is unitless
    cell:   f32,
    n:      u32,
}

/// One sample per invocation. `Slots` lets each invocation write only its own slot.
@compute(64)
fn sample<F: Surface>(field: F, grid: Grid, out: mut Slots<f32>, id: GlobalId) {
    let i = id.x
    let p = grid.origin + vec3(f32(i % grid.n), f32((i / grid.n) % grid.n), f32(i / (grid.n * grid.n))) * grid.cell
    out[id] = field.distance(p)
}

/// Normals come from the derived gradient. Nobody wrote it.
@fragment
fn normals<F: Surface>(field: F, pos: vec3) -> Color {
    let n = field.gradient(pos).normalize()
    Color(n * 0.5 + 0.5)
}
```

---

## 20. Tiers at a glance

| Tier | Features |
|---|---|
| **T0** | Statements and literals; functions with modes and named arguments; structs with defaults; enums (including `Option`); `const` with literal values; traits with associated types and default methods; monomorphized generics and `impl Trait`; projections and exclusivity; non-escaping closures; scalar, vector and matrix types; `@compute`/`@vertex`/`@fragment`/`@gpu`; typed builtins; invocation-safe kernel outputs; workgroup-shared memory (D-093); lossless GPU layout through `GpuData`; GPU buffer handles, uploads and dispatch from CPU code (D-102); derived `gradient` and `interval`; WGSL and WASM emission. |
| **T1** | Units; `@comptime`, evaluated `const` initializers and reflection; structural defaults; auto and declared traits; `@diagnostic`; `@deterministic` and the numeric rules; `@assert`/`@assume` and bandlimits; Lipschitz facts and pruning; handles, arenas and regions; `Plain`/`Relocatable`; views, iterators and other non-escaping types; lossy GPU encodings; strings; destructors; `Result`, `?` and panics; `@escaping`; the pipeline-count query. |
| **T2** | Threads and parallel combinators; async; GPU readback; `@audio`; checkpoints and keyframes; `dyn Trait`; `stage::interpret`; any compiler tier in the browser. |

---

## 21. Open

- **Packages and dependencies** (D-087).
- **The keyword list** (§2).
- **The closed list of stdlib items the compiler knows** (D-081).
- **Syntax for structural defaults:** `@comptime default for<T: struct>` is a placeholder (§3).
- **How a generic parameter declares that it accepts non-escaping types** (§6.4).
- **Checking scopes:** how the compiler matches the scope a consumer needs against the scope a fact declares, and how scopes compose (D-092).
- **Workgroup-shared memory and barriers in kernels** (§12, D-093).
- **GPU buffer names and syntax, and kernels whose parameters exceed the target's binding limits** (§12, D-102).
- **`from param` annotations** (§6.4).
- **Region chunk sizes and undo-ring sizes** (§6.11).
- **A general `schedule` construct** for any function's evaluation stays possible as future sugar (D-053). Add it only when kernels need it.
