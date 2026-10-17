# The Wrela language

This is the specification of Wrela: its syntax, its types, its memory model, its effects, its GPU code, and the boundary between the language and its standard library (std). It is written for two readers: a person who learns the language, and an agent that writes it. Each section starts with one sentence that says what it is about, so a reader can stop there.

**How to read it.**

- **Sections are numbered by topic,** in the order a reader learns them: §1 to §14 are the language, §15 to §19 are how a program runs (its hosts, its threads, its host services, the GPU and servers), and §20 to §23 are std, what the language leaves out, a complete program and the tools. `§17.1` is a section of this document; a reference to another document names it, as `vision.md §2` does.
- **Diagnostics have codes.** `E0500` is an error and `W0001` a warning; §10.2 lists the ranges. `wrela explain <code>` explains any code, with a wrong program and a fixed one.
- **Every example compiles.** A test builds every `wrela` block here. A block whose comments say `// error:` gives exactly those errors, with the helps and fixes that its comments name. A `text` block is a sketch or a file format.
- **Rules have IDs.** The conformance suite (`compiler/tests/conformance`) holds, for each rule, a program that the compiler accepts and one that it rejects with the rule's code. *The rules by ID*, at the end, lists every rule with what it says and the section that gives it; `wrela primer [area]` teaches them area by area.
- **The syntax is in its own files:** `spec/lexical.md` gives the tokens and `spec/grammar.ebnf` the grammar.

Where this text and the tests disagree, the tests are right.

**Contents.**

| § | Section | What it covers |
|---|---|---|
| | How Wrela differs from Rust and TypeScript | A short list for a reader who knows either language |
| §1 | What Wrela is | The design goals, and the three targets: the CPU, the audio worklet and the GPU |
| §2 | Lexical structure and statements | Lines and tokens, control flow, unused values and limits |
| §3 | Items | Functions, structs, enums, traits and impls, constants, modules and packages |
| §4 | Types | Scalars, vectors and matrices, arrays, runs, tuples, aliases, text and encodings |
| §5 | Units | SI constants written as number suffixes: `15cm` |
| §6 | Memory | Values, moves, projections, exclusivity, closures, arenas, plain data and snapshots |
| §7 | Generics and traits | Monomorphization, inference and trait sets |
| §8 | Effects and contexts | The seven effects, and what each context forbids |
| §9 | Attributes | The closed set of `@` attributes |
| §10 | Errors | `Result`, panics, failed asserts and diagnostic codes |
| §11 | Numerics | Strict floats on the CPU, SIMD, WGSL on the GPU, and the built-in functions |
| §12 | Determinism | What `@deterministic` code may do, and why its results agree everywhere |
| §13 | Constants, tests and embedded data | Constants that the build computes, tests, and `embed` |
| §14 | Derived interpretations | Gradients, intervals and filtering that the compiler derives from code |
| §15 | Programs and hosts | Exports, a program's state, client and server builds, and hand-over |
| §16 | Concurrency | Threads, ticks, hand-offs, tasks, jobs, the audio thread and caches |
| §17 | Host services | Requests, input, messages, printing and the clock |
| §18 | GPU code | Entry points, buffers, kernel outputs, bindings, commands, textures, passes and pipelines |
| §19 | Rooms | A server's ordered log of what its peers propose, kept and served over the network |
| §20 | The stdlib boundary | What the compiler knows about std, the map of std's modules, and fields as std code |
| §21 | What Wrela leaves out | Each missing feature, and what a program uses instead |
| §22 | A complete program | One program with a field, a kernel and a shader |
| §23 | Lifted builds, provenance and the tools | Changing a program's numbers while it runs, and the tools for agents |
| | The rules by ID | Every rule of the conformance suite, with its section |

## How Wrela differs from Rust and TypeScript

Wrela looks like Rust but has no references, lifetimes, macros or async; unlike TypeScript, every value has a static type, and there is no garbage collector, `null` or exception.

### From Rust

- No references and no lifetimes. A parameter reads its argument in place, and `mut` marks a change at the call site: `step(mut g)` (§6.2).
- `let` owns a value; `borrow x = place` reads a place in place, and `mut x = place` changes it (§6.3).
- `take x` moves a value out of a named place; `.clone()` copies a value that isn't `Copy` (§6.1).
- A struct can't hold a borrow. A `borrow struct` groups projections, and lives only as a parameter, a result or a local (§6.6).
- No iterator types: `for` walks runs, arrays, ranges and arenas, and a query takes a closure or returns a `Vec` (§6.6).
- No `dyn`: use an enum or a generic (§21).
- A type declares its traits: `struct P: Copy + Eq`. There's no `impl Copy`, and `Eq`, `Ord`, `StateHash` and `Serialize` are derived when a type declares them (§3.4).
- No macros: `f"…"` builds a string, and `std::io::print` shows a line (§4.6, §17.3).
- No `async`: IO is a request that the program polls (§17.1).
- No destructors in user code; no `Rc`, `Arc`, `Cell` or `RefCell`; no mutable globals (§21).
- A newline ends a statement (§2.1). A number's suffix is a unit, `15cm`, never a type (§5).
- Integer overflow traps in every build (§11).
- `-> Surface` returns some type with the trait, as `impl Surface` does in Rust (§3.1).
- One language for CPU and GPU: `@compute`, `@vertex` and `@fragment` functions compile to WGSL (§18).
- `pub fn` in `main.wrela` is an export to the host. Its parameters are numbers, bools or vectors, and its result can also be a struct, tuple or array of them (§15.2).

### From TypeScript

- Every value has a static type. There's no `any`, `null` or `undefined`: an absent value is `Option<T>` (§4).
- Values aren't shared. Assigning a struct copies it if it's `Copy`; otherwise it needs `.clone()` or `take` (§6.1).
- No classes or inheritance: structs, enums and traits (§3, §7).
- No exceptions: an error is a `Result<T, E>`, passed up with `?`, and a bug panics (§10).
- No garbage collector: memory is values, arenas and handles (§6.8).
- Numbers have sized types (`f32`, `u32`, `i32`, …) and never convert implicitly: `f32(n)` (§4.1).
- A string literal is `Text`; `String` grows; interpolation is `f"…"`, not a template literal (§4.6).
- No `async` and `await` (§17.1).

## 1. What Wrela is

Wrela is a language compiled ahead of time for the CPU and the GPU, with value semantics, derived interpretations of numeric code and checked determinism.

- **One language for CPU and GPU.** The same source compiles to WASM for the CPU and to WGSL for the GPU. The compiler lays out the data that crosses between them.
- **Compiled ahead of time** by a Rust program on the developer's machine. A game ships WASM, WGSL and data. There's no compiler in the browser.
- **Value semantics, no garbage collector.** Memory is values, arenas and handles. Every copy, move and mutation is visible where it happens (§6).
- **Generic code is monomorphized.** A value's structure is its type, so specialization is ordinary monomorphization, and the numbers that change at run time travel as data.
- **The compiler derives interpretations of pure numeric code:** gradients and intervals. They nest, so the interval of a gradient is a local Lipschitz bound. Fields are the main use, but nothing about it is specific to fields.
- **Determinism is a checked property** (`@deterministic`), with strict floats on the CPU.
- **The compiler knows nothing about the engine.** A feature that only makes sense in a game belongs in the engine, not in the language.
- **Built for agents:** a familiar syntax of the Rust, TypeScript and Swift family, a small spec, machine-readable diagnostics and semantic queries.

### 1.1 Targets

The compiler knows exactly three execution targets:

| Target | Emitted as | Notes |
|---|---|---|
| CPU, main thread or worker | WASM | Strict floats |
| Audio worklet | WASM | `@audio` context |
| GPU | WGSL | Compute, vertex and fragment entry points |

## 2. Lexical structure and statements

Wrela source is `.wrela` files of lines, and a newline ends a statement; this section gives lines and tokens, control flow, the unused values that the compiler reports, and how deep code may nest.

### 2.1 Lines and tokens

- Files use the `.wrela` extension.
- A newline ends a statement, unless it's inside open brackets or the next line starts with `.`.
- A binary operator that continues a line must *trail* the line; a leading `-` or `|` starts a new expression.
- `;` may separate statements on one line; the formatter normalizes them.
- `else` goes on the line of the `}` before it: a line break after `}` ends the `if`.
- Comments start with `//`, and doc comments with `///`. There are no block comments.
- Outside comments and strings, only the characters that `spec/lexical.md` lists (L3) are allowed: no `$`, `#`, backticks or no-break spaces.
- An integer literal must fit the type it's used as.
- Number literals have no type suffixes: no `1.0f32`.
- A unit can follow a number as a suffix: `15cm` means `15 * cm`, which is 0.15. Units are constants, in SI (§5).
- `**` is exponentiation; `^` is XOR.

```wrela
let r = ellipsoid(radii: vec3(0.45m, 0.50m, 0.90m))
    .smooth_union(haunch, k: 15cm)      // a leading `.` continues the expression
    .displace(fbm(freq: 25 / m, octaves: 4), amp: 3mm)

let total = base +                      // a continued line ends with the operator
    extra
```

**Keywords** are listed in `spec/lexical.md` (L11), with the reserved ones, which no feature uses (`dyn`, §21). The list is final, and a test checks it against the lexer.

### 2.2 Control flow

Control flow is the expressions and statements of the Rust family.

- `if`/`else` and `match` are expressions. A block's value is its last line when that line is an expression.
- `for i in 0..n` (and `0..=n`) counts over integers, `for x in xs` walks an array or a run, and `_` names an unused loop variable.
- Over a sequence, a pattern destructures each element as `let` does, and can't fail (E0309): `for (a, b) in pairs`, `for Spring { rest, k } in springs`. `for mut` binds the parts as mutable projections.
- `while`, `loop`, `break`, `continue` and `return` work as in Rust.
- `if let PATTERN = EXPR { … } else { … }` runs the first block when the pattern matches.
- `let PATTERN = EXPR else { … }` binds the pattern, and its `else` block must leave the scope: `return`, `break`, `continue` or a panic.
- `while let PATTERN = EXPR { … }` runs its body while `EXPR` matches, and evaluates `EXPR` again each time: `while let Some(b) = stack.pop() { … }`.
- A string literal is a `match` pattern, and matches text: a `str`, `Text` or `String`. For example, `"calm" => 1.0`.
- An assignment can be a `match` arm: `0 => x = 1.0`.
- A `let` may shadow an earlier binding of the same name.
- A literal's type is inferred from the whole function, later uses included: after `let a = 1` and `let b: f32 = a`, `a` is an `f32`.

### 2.3 Unused values

An unused value is a warning, and a discarded result is an error.

- A local that's bound and never used is a warning (W0001), unless its name starts with `_`.
- A `var` that nothing changes is a warning (W0006): it's a `let`.
- A private function or constant that no other code uses is a warning (W0008). So is one that calls only itself. A trait's method, a test, and a name that starts with `_` are exempt.
- A statement that calls a function only to throw its value away, where the call does nothing else (no effect, §8, and no argument lent `mut`), is an error (E0333). `p.normalize()` gives a new vector and leaves `p` as it was. `let _ = f()` discards a value on purpose.

### 2.4 Limits

Limits keep every build finite.

- Expressions, blocks, types and patterns nest at most 128 deep, and an expression's tree is at most 512 deep (E0112).
- A generic function's instantiations go at most 256 calls deep.
- A type has at most 4096 parts, counting a part each time it appears (E0412, E0329). This holds for any value's type and for type arguments: a type that doubles at each step, like `(a, a)`, grows past any machine.
- A value larger than the CPU stack traps when the function that holds it is entered, as a stack overflow does.
- In GPU code, a type larger than WGSL allows (2³¹ − 1 bytes) is an error (E0702).

## 3. Items

Items are what a module declares: functions, structs, enums, traits and impls, constants, and type aliases (§4.5); modules and packages hold them.

### 3.1 Functions

A function takes parameters by position or by name, each with a mode and an optional default, and returns its value on every path.

- **Named arguments are optional, as in Kotlin.** Positional arguments come first, and once an argument is named, the rest must be named too.
- **Parameters may have defaults:** a literal value, or any other constant expression, which the build computes once (§13.1). A default whose type is generic must be a literal (E0324): a constant has one type. A default that isn't `Copy` is cloned where a call takes it.
- **Arguments are evaluated in the order they're written,** named ones too, whatever the order of the parameters they go to.
- **A bare literal passed by position where a swap would still compile is a warning** (W0005), with a fix that names it. This applies to two neighbouring parameters of one type that both get bare numbers (`round_cone(a, b, 0.09, 0.06)`), and to a bare `true` or `false` given to a function that takes more than one argument. A `select` whose two values are passed by position is W0005 too, since its order (the value for `false` first) is easy to swap: `select(if_false: a, if_true: b, cond: c)`.
- **Parameters have modes** (§6.2): `x: T` (borrow), `x: mut T`, `x: take T`.
- **A function that returns a value** ends with it, or returns it, on every path (E0314). One that returns nothing ends with a statement or a `()`.
- **Properties are attributes** (§9): `@deterministic fn step(...)`.
- **A trait in parameter position** (`fn d(field: Surface, p: vec3)`) makes the function generic over that parameter. It's the usual way to write a generic parameter (§7); write `<F: Surface>` only when the type is named twice.
- **A trait in return position** (`-> Surface`, `-> Field<C>`) names one concrete, inferred type, like Rust's `impl Trait`. Callers see only its traits. `+` joins several traits, in return position or parameter position: `-> Surface + Colours`, `f: Field<C> + Eq`.

```wrela
fn leg_segment(len: f32, r_top: f32, r_bottom: f32 = 0.06) -> Surface {
    round_cone(vec3(), vec3(y: -len), r_top, r_bottom)
}

let leg = leg_segment(0.45, r_top: 0.09)   // positional first, then named
```

### 3.2 Structs

A struct is a named group of fields, which may have defaults, and its declaration names the traits it has.

- **Fields may have defaults**, as parameters may (§3.1, §13.1). A struct literal may omit defaulted fields.
- **A struct opts in to traits in its declaration:** `struct Tissue: Blend { ... }`.
  - `Copy`, `Clone`, `GpuData` and `Plain` are structural: every field must have the trait.
  - Declaring `Copy` implies `Clone`, so declaring both is a warning with a fix (W0004).
  - A trait set names several traits at once: `struct Gate: Sim` (§7.1).
  - A `@fieldwise` trait is derived (§3.4). Any other trait that a type declares needs an `impl` (E0401).
  - A generic type's declared trait holds for the instantiations whose type arguments have it: the prelude's `Option<T>: Copy` makes `Option<f32>` `Copy`, and `Option<Log>` isn't `Copy` when `Log` isn't.
- **`..base`** fills the remaining fields from another value of the same type, as if each were written `field: base.field`: a `Copy` field is copied, and any other field moves. A base that's an owned local moves as a field does (§6.1's struct literals). A base that's another named place is written `..take base`, or `..base.clone()` when it isn't all `Copy` (E0501, E0502).

```wrela
pub struct Look {
    pub tolerance: f32 = 0.002,
    pub resolution: u32 = 1024,
}

const FINE: Look = Look { tolerance: 0.001 }   // resolution keeps its default
```

**A `Packed` struct is bits of one `u32`:**
- Each field is a `Bits<N>`, `N` of the word's bits: 1 to 32, the first field in the low bits, at most 32 in all. Another field type, a default, or an enum is E0407.
- A field reads as a `u32` (`cell.vertex`). It is written, added to (`cell.signs += 1`) and given in a literal (`Cell { vertex: v, signs: s }`, `..base` too) as a `u32`.
- A value past its field's bits panics on the CPU; the GPU keeps its low bits, as its integers wrap.
- `u32(p)` is the word, and `T::of_bits(w)` is the struct that a word holds. `of_bits` is `Packed`'s method, so generic code over `T: Packed` calls it too. A `Packed` struct crosses to the GPU and to a host as a `u32` does, and the GPU packs and reads its fields as the CPU does.
- A field isn't a place of its own: it can't be lent `mut` or projected.

```wrela
struct Cell: Packed + Copy + GpuData {
    vertex: Bits<24>,
    signs: Bits<8>,
}

let c = Cell { vertex: vi, signs }
let word = u32(c)                     // the vertex in bits 0 to 23, the signs in 24 to 31
```

### 3.3 Enums

An enum is a sum type whose variants can hold payloads, and a `match` on it must be exhaustive.

- A match that isn't exhaustive is E0309, and so is a match with too many cases to check in bounded time, unless it ends with `_`.
- An enum chooses structure at run time from a known, finite set: every case is compiled, with a uniform branch.

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

**An enum whose variants hold nothing has discriminants:** each variant's number, which is also its tag on the CPU and the GPU.
- A variant's discriminant is written (`Hidden = 99`), or is one past the discriminant of the variant before it; the first variant's is 0.
- Each discriminant is a `u32` greater than the one before it, so the variants are ordered as they're declared. A discriminant out of order is E0334, as is a discriminant in an enum with a variant that holds fields.
- `u32(e)` converts an enum to its variant's discriminant, as `u32(mood)` does. `E::from_u32(n)` reads it back: `Some` of the variant whose discriminant it is, or `None`.
- An enum that declares `Fieldless` converts in generic code too, both ways: `u32(t)`, and `T::from_u32(n)`, `Fieldless`'s method. `Fieldless` is declared like `Copy`, and checked: E0407 on a struct, or for a variant with fields.
- `std::collections::Flags<E>` holds a set of an enum's variants as one `u32`, bit `u32(e)` for `e`, for an enum whose discriminants are below 32. Its methods are `has`, `with`, `without`, `toggled` and `changed`; `bits()` and `of_bits` cross to a host or the GPU.
- An export takes such an enum as its number, and a number that no variant has traps (§15.2).

### 3.4 Traits and impls

A trait declares methods and associated types; a type gets a trait through an `impl`, or by declaring a structural or `@fieldwise` trait that the compiler derives.

- **Traits have associated types and default methods.**
- **An impl gives what its trait declares, and nothing else:**
  - its methods take the trait's parameter defaults, and can't declare their own (E0405);
  - associated types belong to traits, not to inherent impls (E0402);
  - a blanket impl bounded only by its own trait (`impl<T: Tr> Tr for T`) implements nothing (E0400).
- **Method syntax finds a trait's methods only where the trait is visible:** it's `pub`, or the calling module declares it. A private method is private to its module (E0203).
- **Coherence follows Rust's orphan rule:** an `impl` lives in the package of the trait or of the type, and std is another package.
  - `Copy`, `Clone` and `GpuData` aren't implemented with an `impl`: a type opts in to them in its declaration.
  - An impl for every closure or function of one signature, `impl<F: Copy + GpuData + fn(vec3) -> f32> Shape for F`, applies to just those. A closure whose signature doesn't fit, or a struct, doesn't have the trait, so the impl doesn't conflict with one for a struct.
- **`==` and ordering come from declared traits.** A type that declares `Eq` gets `==` and `!=`, and one that declares `Ord` gets `<`, `<=`, `>` and `>=`. Both are fieldwise (below): fields compare in order, and an enum's variants compare in the order they're declared. Without `Eq`, `==` works only on numbers, `bool`, vectors and enums whose variants hold nothing, which compare by variant (E0305).

#### Fieldwise traits

A trait declared `@fieldwise` is derived, field by field, for every type that declares it. `Clone`, `Eq`, `Ord`, `StateHash`, `Serialize` and `std::field`'s `Blend` work this way, and so can a library's own traits. This is how Wrela does without reflection and macros (§21).

- **A struct** applies the trait's method to each field in order. A parameter of type `Self` is taken field by field too: `blend(self, other: Self, t: f32)` blends each field with the same field of `other`.
- **A method that returns `Self`** builds the struct from its fields' results. One that returns `Result<Self, E>` does the same, and stops at the first error.
- **An enum** gives its variant first, then its fields. A method that builds an enum chooses the variant through the trait, which is how loading works.
- **Tuples of up to 12 elements, and arrays,** have every fieldwise trait that their elements have, derived element by element, in order. An array's is a loop, so a method that builds an array needs `Copy` elements: it copies the first element's result, then replaces the rest. An empty array is built without a call.

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

**Hooks** tell the trait each field's name, and let it choose an enum's variant. For a method `m`, the trait may declare these hooks, each with a default body:

- `m_field(name: Text, ...)`, called before each field with its name and the method's other parameters;
- `m_variant(index: u32, name: Text, ...)`, called before an enum's fields with its variant;
- `m_variant(names: [Text], ...) -> u32`, which chooses the variant that a method builds, by its index into `names`.

Tuple and array elements have no names, so the field hooks aren't called for them. `Serialize` names fields this way, so a save keeps its fields' names.

**A method's body can walk the fields.** In a fieldwise trait, a method whose default body has `for f in fields(self) { ... }` is derived from that body for each type that declares the trait, and for tuples and arrays.

```text
@fieldwise
pub trait Parts: Copy {
    fn count(self) -> u32 {
        var n = 0
        for part in fields(self) {
            n += part.count()
        }
        n
    }
}
```

- The walk runs once for each field: a struct's fields in order, a tuple's elements in order, and an array's elements in a loop. `f` is a projection of the field, of the field's own type, which has the trait.
- `for mut f in fields(self)` walks with `mut self`. `fields(self).rev()` walks the fields last first, for a fold from the right.
- So the trait decides how its fields' results combine where the shapes above don't: a sum, a fold that threads a value through them, or the field that holds an index.
- A walk whose body always returns returns from its first field, so `for f in fields(self) { return f.first() }` needs nothing after it. It's derived as that field's body alone (an array's first element's).
- A tuple's or an array's derived impl has a generic trait's arguments as its first element does: a pair of `PartChannels<Tissue>` items is a `PartChannels<Tissue>`.
- The body is checked once, with `f` of the trait's field type (any type with the trait); its memory rules are checked where it's derived.
- The walk is the fields one after another, not a loop. So it can't `break` or `continue` (`return` leaves the method), and it can't hold a closure, since its body is written again for each field: E0337, as is `fields(self)` anywhere else.
- An impl written by hand gives the method itself (E0401).

For example, engine::creature's `PartList` and `PartChannels` are derived this way for pairs and arrays of parts.

### 3.5 Constants

A `const` is computed when the program is built (§13.1), and lives as long as the program.

- **Its initializer can be any expression,** function calls included, as long as its effects allow it: no IO except `embed`, no host calls, nothing non-deterministic (§8).
- **A failed `assert` or a panic in it is a compile error,** with the call chain.
- **A constant is a place:** a use reads it or projects part of it, and a function can return a projection of it, but nothing moves out of it or changes it. `.clone()` gives an owned copy.
- **A literal value is folded where it's used;** anything else is computed once, by the build.

```wrela
const HIDE_DENSITY = 1050 * kg/m**3                       // 1050.0, in kg/m³ (§5)
const GAIT = GaitNet { weights: embed("grazer_gait.wnn") }   // read in place: no parsing at load
const HOOF_MODES = modal_modes(hoof())                       // an eigenvalue solve, run by the build
const GRAZER = grazer_skeleton()
const CHEST = GRAZER.bone("chest")                           // a misspelled name fails the build
```

The optimizer never decides staging: work that must happen early is written as a `const`, or done at load.

### 3.6 Modules and packages

A program is one package: a directory with a `main.wrela`, in which each file is a module, and which can depend on other packages.

#### Modules

- A file is a module and a directory is a module tree: `shapes/blob.wrela` is `shapes::blob`. `use` imports names; `pub` makes an item visible outside its file. In `main.wrela`, `pub` also makes a function an export (§15.1).
- At the package's top level, `build`, `results`, `node_modules` and `target` hold outputs, and are never modules.
- A `use` path starts at the package root, or at `std::`; there's no `super::`.
- A module has one namespace for all its items.
- std's root is `std::`, so no module of the package can be named `std` (E0201).
- A package is the files under its directory: a symbolic link to a directory inside it is refused, not followed (E0208).

```wrela
use shapes::blob::blob      // from shapes/blob.wrela
use std::gpu::dispatch
```

#### Packages

A package is a directory of modules. One that has dependencies, or uses `unsafe`, has a `wrela.toml`; one with neither needs none.

```text
[package]
name = "herd"                          # how diagnostics name it
unsafe = true                          # it may use `unsafe` (§6.12; E0214 otherwise)

[dependencies]
fieldkit = { path = "../fieldkit" }    # a local path, relative to this file
```

- **A dependency's modules are named under it:** `use fieldkit::shapes::blob`. A path's first name is `std`, a module of the package, or one of its dependencies, and one name can't be both (E0201).
- **Only `pub` items cross a package boundary.** `pub(package)` makes an item, a field or a method visible to every module of its package and to no other (E0203).
- **The orphan rule is per package:** an `impl` lives in the package of its trait or of its type, and std is a package too. std's privileges (its core's `unsafe`, intrinsics, destructors and lang items) come from its being std's package.
- **A directory with its own `wrela.toml` is another package,** never part of this one's module tree. Only the program's `main.wrela` is its interface to the host (§15.1); in a dependency, `main.wrela` is an ordinary module.
- **Dependencies are built from source with the program.** A dependency is a local path: there are no versions, lockfiles or registries. A dependency can't lead back to a package that depends on it (E0221), a `path` that isn't a directory is E0220, and a manifest that isn't valid is E0219. A package that two others depend on is built once.
- **A diagnostic in a dependency names the package and the path inside it:** `[fieldkit] shapes/blob.wrela:3:5`. Builds with dependencies are byte-reproducible, from any path.
- **Effects are inferred across packages** as within one: exported functions don't state them (§8, §21).

## 4. Types

Every value has a static type: a scalar, a vector or matrix, an array, a run or a tuple, an enum such as `Option`, text, a projection, or a std container.

| Type | What it is | Where |
|---|---|---|
| `bool`, `i32`, `u32`, `f32` | Scalars, on the CPU and the GPU | §4.1 |
| `f64`, `i64`, `u64`, `u8`, `i8`, `u16`, `i16` | Scalars on the CPU only | §4.1 |
| `vec2`…`vec4`, `mat2`…`mat4`, `matCxR` | `f32` vectors and matrices | §4.2 |
| `vec2i`…`vec4i`, `vec2u`…`vec4u`, `vec2d`…`vec4d` | Vectors of `i32`s, `u32`s and `f64`s | §4.2 |
| `[T; N]` | A fixed-size array | §4.3 |
| `[T]`, `mut [T]` | A run: a borrowed or mutable view of elements | §4.4 |
| `(A, B)` | A tuple | §4.4 |
| `type Name<T> = Type`, `type Name = Traits` | Aliases | §4.5 |
| `Option<T>` | An ordinary enum in the prelude (`Some`, `None`); there's no null (§6.1) | §3.3 |
| `Result<T, E>`, and `?` | A recoverable error | §10 |
| `String`, `str`, `Text`, `Bytes`, `f"…"` | Text and bytes | §4.6 |
| `Unorm8x4`, `Half2`, `Oct32` | Lossy encodings for the GPU | §4.7 |
| `borrow T`, `mut T` | Projections: a function's result or a local binding, never a field of an ordinary type or a type argument | §6.4 |
| `borrow struct` | A named group of projections, and a projection itself | §6.6 |
| A closure's type | Its own: a closure that captures a projection can't outlive its call, and one that captures only values can be stored | §6.7 |
| `Handle<T>`, `Arena<T>`, `Vec<T>`, `Box<T>` | std's containers | §6.8, §20.2 |

Two kinds of type don't exist:

- **There's no reference type, `&T`.** Nothing can store a reference, so there are no lifetimes (§6).
- **There's no `dyn`.** The word is reserved and never accepted, and its diagnostic says to use an enum or a generic (§21).

### 4.1 Scalars and conversions

- **`bool`, `i32`, `u32` and `f32`** work on the CPU and the GPU.
- **`f64`, `i64` and `u64` are CPU only.** GPU code is type-checked against what WGSL has.
- **`u8`, `i8`, `u16` and `i16` are CPU only** too: WGSL can't hold them in a buffer (E0407). GPU data packs them into a `u32`, as the lossy encodings do (§4.7).
- **Nothing converts implicitly.** An integer literal can be any numeric type, but a value converts only with a call such as `f32(n)` or `u32(i)`. §11.3 says what a conversion does with each value.
- **An index is a `u32` or an `i32`.** A negative index is out of range, and traps. Other integer types convert first: `xs[u32(b)]`. An enum whose variants are numbered 0, 1, 2 and on also indexes, by its discriminant: `heights[Species::Oak]` (§4.3). Any other index type is E0312.

### 4.2 Vectors and matrices

`vec2`, `vec3` and `vec4` hold `f32`s, and so do the matrices.

- **A vector's components** are read and written by name, alone or swizzled: `v.x`, `v.zyx`, `c.rgb`. A swizzle that's written can't name a component twice (E0317).
- **A matrix is C columns of R rows,** as WGSL's are: `mat2` to `mat4` are square, and `mat2x3` to `mat4x3` aren't.
  - It's built from C column vectors (`mat3x4(r, g, b)` is three `vec4`s), and its column `m[i]` is a `vecR`.
  - `M * v` takes C components and gives R; `v * M` takes R and gives C; `A * B` needs A's columns to be B's rows, and gives B's columns of A's rows. Matrices add, subtract and scale too.
  - It's laid out column by column, each column a vector's 16 or 8 bytes. So a `mat3x4` (three `vec4` columns) holds twelve floats in 48 bytes, and a `mat4x3` (four `vec3` columns, each padded to 16 bytes) holds them in 64.
- **Rotations** are `std::quat`'s `Quat`, which the prelude puts in scope.

`vec2i`…`vec4i`, `vec2u`…`vec4u` and `vec2d`…`vec4d` hold `i32`s, `u32`s and `f64`s, as WGSL's `vec3i` and `vec3u` do. They're for grid code's cells and keys, and for geometry that must be `f64` to match JavaScript bit for bit.

- **They do what an `f32` vector does where their components can:** arithmetic, with a scalar of their component's type too; `==`; swizzles and indexing; `min`, `max`, `clamp`, `abs`, `sign` (signed ones), and `dot` (any vector, as WGSL's).
- **Integer vectors** add the bit operators, and shifts by a `u32` or by a `vec3u` of amounts.
- **`vec3d` is CPU only,** as `f64` is (E0600 on the GPU), and aligned as its components, so it's 24 bytes. It has the float built-ins (`length`, `normalize`, `cross`, `floor` and the rest); its transcendentals are E0702, as `f64`'s are.
- **A vector of another element converts a component at a time:** `vec3u(p / cell)` truncates, and `vec3(cell)` and `vec3d(p)` convert.
- **On the CPU, their arithmetic is a component's at a time,** so an integer component overflows (and traps) as a scalar does. The GPU's wraps, as its scalars do.

### 4.3 Arrays

`[T; N]` is an array of `N` values of `T`.

- **`[]` is an empty array.** Its element type is the one where it goes: a parameter's, a field's or an annotated binding's (`count([])` for `ids: [u32]`). Where nothing says, it's E0306. GPU code has no empty arrays, as WGSL has none, so there `N` is at least 1.
- **`[x; N]` repeats a `Copy` value.**
- **`[a, b, ..fill]` writes the first elements and fills the rest**, to the length of the type that the array is used as (a field's, a parameter's or a binding's). The fill is a `Copy` value, evaluated once, after the others: `hills: [vec4(1.0), vec4(2.0), ..vec4(0.0)]` for a `[vec4; 8]`.
- **A length can be an enum** whose variants hold nothing and are numbered 0, 1, 2 and on. `[f32; Species]` has an element for each species, and `heights[Species::Oak]` reads the one at `Oak`'s discriminant. So a table and its enum can't fall out of step.
- **Generic code can range over the length:** `fn sum<const N: u32>(xs: [f32; N])`, `impl<F: Surface, const N: u32> Surface for [F; N]`.
- **A length is a constant expression with no calls,** so a type never waits on build-time code (§13.1). A struct's length argument is one too, a number or a constant that holds one: `Bounded<Seen, MAX_SEEN>`.

### 4.4 Runs and tuples

`[T]` is a **run** of `T`, borrowed, and `mut [T]` a mutable one: the language's one view type.

- **A run is a projection** (§6.6): a parameter's type, a function's result or a local binding, never a field or a type argument.
- **An array passes for a run.**
- **`xs.len()`** is a run's or an array's length, a `u32`.
- **Its elements are contiguous bytes only where they're viewed as bytes** (§6.10).
- **A run of a type whose values hold nothing** (`()`, a struct without fields) isn't supported (E0702), nor is a `GpuBuffer` of one.

`(A, B)` is a tuple. A struct names its fields; a pair whose parts need no names is a tuple.

### 4.5 Type aliases

**`type Name<T> = Type`** is another name for a type, with parameters or without.

- An alias can't name itself, directly or through others (E0318), and is used with its parameters (E0322).
- An alias of a struct names it in a literal too, with its parameters given or inferred: `type Range = Span<f32>`, then `Range { lo: 0.0, hi: 1.0 }`.

**`type Name = Traits`** is an alias that names traits, as a return type can (`-> Surface`): `type GrazerField = Parts<Tissue>`.

- It names one type: the one that the first function in its module whose result is the alias returns. Everywhere, that type is seen through those traits alone.
- So a value that a function builds can be stored and named, as a struct's field or a generic's argument, without writing its type. That type is long, since a value's structure is its type (§1), and it changes whenever the function's body does.
- Other functions that return the alias return a value of that type (E0300).
- The alias has no parameters, the function that decides it isn't generic, and a function in its module must return it (E0410).

### 4.6 Text and bytes

| Type | What it is |
|---|---|
| `String` | Heap-owned UTF-8, which grows |
| `str` | A borrowed run of UTF-8: a `String`'s, or part of one |
| `Text` | Text known at build time, the type of a string literal: a `Copy` handle to read-only UTF-8 in the build, which can be stored anywhere |
| `Bytes` | Bytes known at build time, which `embed` gives (§13.3): a `Copy` handle to read-only bytes in the build. It passes as a `[u8]`, and `b.at(i)` is byte `i`. |

**`f"…"` builds a `String`:** `f"Weight: {w:.1} kg"`.

- Each `{expr}` or `{expr:spec}` calls std's `Format` trait, which numbers, `bool`, strings and `Text` implement.
- `{{` and `}}` are literal braces.
- It allocates, so it's not for GPU or `@audio` code (§8).

### 4.7 Lossy encodings

A lossy encoding is always an explicit type, in `std::gpu`. Each is one `u32` that `encode` makes and `decode` reads, with the same code on both targets.

| Type | What it holds | How close |
|---|---|---|
| `Unorm8x4` | Four values in [0, 1], 8 bits each | Within 0.5/255 |
| `Half2` | Two half floats (f16) | Within 2⁻¹¹ relative, in f16's normal range |
| `Oct32` | A unit vector, octahedral, 16 bits a coordinate | Within 1.5 × 10⁻⁴ rad |

There's no `f16` scalar: WGSL has one only with an optional feature.

## 5. Units

Units in Wrela are SI constants that a number can take as a suffix (`15cm`); there are no unit types (§21).

- **Every physical quantity is SI:** metres, seconds, kilograms and radians. An `f32` that holds a length holds metres; names and doc comments say which quantity it is.
- **Suffixes convert to SI at compile time:** `15cm` is `15 * cm`, which is 0.15, and `90deg` is π/2. The product is exact, rounded once to the type the number is used as, so `15cm` is the same number as `0.15` in `f32` and in `f64` (`15.0 * 0.01` in `f64` isn't). Compound units are arithmetic on the constants: `1050 * kg/m**3`.
- **Unit suffixes resolve in their own namespace,** which locals can't shadow. `2m` means metres even if a local is named `m`.
- **No unit may collide with numeric syntax.** There's no unit named `e`.
- **A bare number meant in another unit still compiles.** Write the suffix: `sin(90deg)`, not `sin(90)`.
- **Units that can't meet are an error where they're written.** When both sides of `+`, `-` or a comparison are built from unit suffixes and numbers alone (`2m + 3s`, `1km < 30deg`), their dimensions are known, and two that differ are E0305, which names both as written. A bare number fits any unit, and a variable's unit isn't known, so `let t = 3s` then `2m + t` compiles.

```wrela
let density = 1050 * kg/m**3      // 1050.0: kg/m³ by the SI convention
let reach   = 15cm + 0.3          // 0.45 m: both are metres
```

## 6. Memory

Memory in Wrela is values, moves and projections, with handles into arenas for long-lived links; there is no garbage collector and no lifetime annotation.

**The goals.**

- **Memory safety without a garbage collector,** so frame times stay predictable.
- **Every transfer is visible.** A copy, a move or a change shows where it happens. The one exception: a call to a `mut self` method doesn't repeat `mut`, because the method's name says what it does (§6.2).
- **No lifetime annotations.** Agents and people never reason about lifetime parameters.
- **Determinism.** Behavior never depends on memory addresses, and a snapshot is a clone.
- **A direct map onto the targets:** WASM linear memory, WebGPU buffer bindings and the audio worklet.

**The rules at a glance.**

- **Everything is a value.** `&T` isn't a type (§4).
- **A `Copy` value copies implicitly; any other value copies only with `.clone()`, and a move out of a named place is written `take`.** Temporaries and returned locals need no marker (§6.1).
- **Parameters have modes:** borrow (the default), `mut` and `take`. Callers write `mut x` and `take x`. A `mut self` receiver isn't marked; a consuming `take self` method on a named place is: `take b.finish()` (§6.2).
- **Bindings:** `let` owns, `var` owns mutably, `borrow` projects a place read-only, and `mut` projects it mutably (§6.3).
- **Projections:** a function may return `-> borrow T` or `-> mut T` of a `borrow` or `mut` parameter (or a constant). A `-> borrow T` result borrows every such argument, and a `-> mut T` result only the `mut` ones (§6.4).
- **Exclusivity:** while a `mut` access is live, nothing may touch an overlapping place. Disjoint fields don't overlap; every element of a container overlaps every other. The compiler checks this within each function, over its control flow: a loan or a move reaches every path that can follow it, through branches, loops and `continue` (§6.5).
- **Projections are parameters, results and local bindings only:** `borrow T`, `mut T`, runs, borrow structs and closures that capture projections. They're never fields of ordinary types or type arguments (§6.6).
- **A closure that captures a projection can't outlive its call;** one that captures only values can be stored (§6.7).
- **Long-lived relationships are handles into arenas,** never pointers (§6.8).
- **No mutable globals, and no interior mutability** such as `Cell` or `RefCell`. Shared mutable state lives in an arena (§6.5).
- **Byte-level data declares `Plain`,** and a type that crosses to the GPU declares `GpuData`, which fixes its layout to WGSL's rules everywhere (§6.10).
- **Snapshots are clones** of ordinary values (§6.11).
- **Destruction is deterministic:** at the end of scope, in reverse order. Only std's core defines destructors (§6.1).
- **`unsafe` exists only for std's core,** and each package declares whether it uses it (§6.12).

How memory meets other threads is §16, the host's requests §17, and the GPU's buffers and bindings §18.

### 6.1 Values, copies and moves

A value whose type is `Copy` copies implicitly; any other value is copied only with `.clone()` or moved with `take`.

A **place** is a local, a field of a place, or an element of a container. A **move** transfers a value out of its place, and a use of the place after the move is an error (E0500).

```wrela
let a = vec3(1m, 2m, 3m)
let b = a                      // vec3 is Copy: an implicit copy

var log = EditLog::new()
var backup = log.clone()       // a deep copy is always explicit
archive(take log)              // moving out of a named place is written `take`
log.push(edit)                 // error: `log` was moved, so it can't be used here
                               //   help: pass `log.clone()` where it's moved if you still need it here
```

- **`Copy` types copy implicitly.** A `Copy` type is a small plain type that declares `Copy`, such as a number, a vector, `Transform` or `Handle<T>`. The implementation of `Copy` is structural.
- **A value of any other type is copied only with `.clone()`** (the `Clone` trait), **or moved with `take`.** A clone is a deep copy and leaves the place as it was. A clone and a move mean what they mean in Rust.
- **No marker is needed for:**
  - a temporary, as in `archive(EditLog::new())`;
  - a returned local variable;
  - an owned local that a struct literal names.
- **A struct literal moves the owned locals it names.** `Impostor { colour, normal, bounds }`, or `..base` for a local `base`, moves each named local whose type isn't `Copy` into the new value, as `take` would. A use after that is a use after a move (E0500), and the diagnostic shows the literal as the place of the move. A `take` written in a literal is a warning (W0009). Calls, bindings and assignments still mark every move.
- **A move reaches every path that can follow it,** through branches, loops and `continue`. So a move inside a loop of a value from outside it is an error (E0515): the second time round, there'd be nothing to move. Move a `.clone()`, or move the value after the loop.
- **Destruction is deterministic:** at the end of scope, in reverse declaration order. A value that was moved out isn't destroyed again.
- **There is no null.** Use `Option<T>`.

### 6.2 Parameter modes

A parameter's mode says what the callee may do with its argument and what the caller writes: borrow (the default), `mut` or `take`.

| Mode | Signature | The callee may | The caller writes |
|---|---|---|---|
| **borrow** (default) | `x: T` | read | `f(x)` |
| **mut** | `x: mut T` | read and write, but not keep | `f(mut x)` |
| **take** | `x: take T` | own it: keep it, move it, destroy it | `f(take x)` from a named place; `f(T::new())` for a temporary |

**Method receivers** are written `self`, `mut self` and `take self`.
- **A `mut self` receiver isn't marked at the call site:** `list.push(x)`, not `mut list.push(x)`. This is the one exception to "every transfer is visible".
- **A consuming (`take self`) method on a named place is marked:** `take builder.finish()`. On a temporary, no marker is needed: `Builder::new().finish()`.

`[T]` is a **run** of `T` (§4.4). As a parameter, a run is borrowed like anything else. `mut [T]` is a mutable run.

### 6.3 Local bindings

A local binding either owns its value (`let`, `var`) or projects a place (`borrow`, `mut`), with the same words as the parameter modes (§6.2).

A **projection** gives access to a place where it is, with no copy. A `borrow` projection reads the place; a `mut` projection reads and writes it. While a projection is live, it holds a **loan** on its place (§6.5).

| | Owns the value | Projects a place |
|---|---|---|
| **read-only** | `let x = …` | `borrow x = place` |
| **mutable** | `var x = …` | `mut x = place` |

- **`let` and `var` own.** Their value is a temporary, `take place`, `place.clone()`, or a place whose type is `Copy`, which is copied.
- **`let x = place` is an error when the type isn't `Copy`.** The diagnostic offers three fixes:
  - `borrow x = place` reads it in place;
  - `let x = place.clone()` makes a copy;
  - `let x = take place` moves it.

  So a type that adds `Copy` never changes what a binding borrows.
- **`let` reads a run in place too:** `let line = text.slice(a, b)`, `let name = s.as_str()`, `let rest = ids` for a parameter `ids: [u32]`. A run (`str`, `[T]`, §6.6) is never owned and never `Copy`, so a binding of a run can only project its place. For a run, `let` means the same as `borrow`.
- **Names bound in a pattern or a loop project the matched place, read-only:** `match e { Some(log) => … }`, `for g in world.grazers`.
  - `match mut place { … }` and `for mut g in world.grazers` make them mutable projections, so a payload can change in place: `match mut slot { Some(s) => s.count += 1, None => {} }`.
  - A match on a temporary owns its value.
  - A loop borrows its container for the whole loop, so in a loop a program can't tell a copy from a projection.
- **A projection can choose its place:** `borrow log = if dark { w.night } else { w.day }` projects the place that the `if` picks, and a `mut` projection writes through to it.
  - Each branch, `else if` too, names a place.
  - While the binding lives, it holds the loans of every place it can pick. A use of one of those places meanwhile is E0506.
  - A branch that makes a value, or a `match`, gives a value, not a place: that is E0502, unless the type is `Copy`, which copies it.
  - GPU code can't choose a place at run time (E0702), because WGSL has no pointer to keep.
- **Closures capture by projection or by value** (§6.7).

```wrela
borrow g = world.grazers[h]    // read-only projection: no copy
let p = world.grazers[h].pos   // a vec3 is `Copy`: an owned copy
mut t = world.terrain          // mutable projection
t.edits.push(Edit::Dig { at, radius: 1m })
var spare = world.grazers[h].clone()   // an owned copy, mutable
```

### 6.4 Returning projections

A function can return a projection of part of one of its parameters, `-> borrow T` or `-> mut T`, instead of a value:

```wrela
fn grazer(world: mut World, h: Handle<GrazerSim>) -> mut GrazerSim {
    mut world.grazers[h]
}

mut g = grazer(mut world, h)   // `world` stays mutably borrowed while `g` is live
g.root.pos.y += 1m
```

- **A projection result is written `-> borrow T` or `-> mut T`.** A plain `-> T` returns an owned value.
- **A projection comes from a `borrow` or `mut` parameter, or from a constant.** A projection of a local or a temporary is an error (E0508). A constant is a place that lives as long as the program (§3.5), so a function can return a read-only projection of one: `fn item(id: ItemId) -> borrow ItemDef { ITEMS[id.index] }`.
- **What the result borrows:**
  - A `-> mut T` result borrows only the `mut` arguments, because a `mut` projection can't come from a read-only place (E0512).
  - A `-> borrow T` result borrows every `borrow` and `mut` argument whose type can hold a `T`, by value or on the heap: `s.slice(i, j)` borrows `s`, not `i` and `j`.
  - Either kind of result borrows only arguments that can hold its type.

  This rule is conservative. The language has no `from param` annotations to narrow it.
- **Projections can't be stored** in ordinary structs or collections, or captured by a closure that is stored (§6.7). A borrow struct can group them (§6.6). A projection never outlives the scope that received it, so no lifetimes are needed.
- **A projection type is never a type argument or a field.** `arena[h]` projects, and a stale handle panics; `arena.contains(h)` checks first. There is no `Option<borrow T>` (§21).

### 6.5 Exclusivity

Exclusivity is the rule that, while a `mut` access to a place is live, no other access may touch an overlapping place; the compiler checks it statically, one function at a time.

- **Two places overlap** when one is a prefix of the other. For example, `world` overlaps `world.terrain`, but `world.terrain` and `world.grazers` don't.
- **Every element of a container overlaps every other element of that container.** The checker doesn't compare indices. `pair_mut` gives two elements at once, and checks at run time that they differ.
- **An access is live** from where it's created to its last use.
- **The check is static and stays within one function.** It never looks inside another function, because signatures say everything. The only checks at run time are explicit library calls, such as `pair_mut` on a `Vec` or an `Arena`.
- **A `match` borrows the place it matches until an arm is chosen.** So a guard can't change what a later arm tests (E0507), and an exhaustive match always finds its arm.
- **To move a value out of a borrowed place, swap something in.** std's `swap(mut a, mut b)` exchanges two places, and `replace(mut place, value)` returns the old value: `let stack = replace(mut inv.slots[i], None)`.
- **There are no mutable globals.** Code passes state in explicitly. Constants are allowed.
- **There's no interior mutability** such as Rust's `Cell` or `RefCell`. Shared mutable state lives in an arena, and code reaches it through `mut` access to that arena.

#### In a call

- Arguments may not overlap if any of them is `mut`.
- The arguments are evaluated before the call's `mut` access begins. A `Copy` argument passed by borrow is copied then, so it may read a place that a `mut` argument overlaps: `v.push(v.len())`, `s.add(s.count)`.
- An argument that is a projection still conflicts (E0513).

```wrela
for mut g in world.grazers {
    step(mut g, world, intent)
    // error: `world` overlaps `world.grazers`, which `g` is borrowing mutably
    //   help: pass only the parts that don't overlap `world.grazers`
}

let both = world.grazers.pair_mut(h1, h2)     // two elements at once (`both.a`, `both.b`):
                                              // checked at run time, panics if h1 == h2
```

### 6.6 Kinds of projection

Wrela has one kind of borrowed thing, the projection, in five forms that follow the same rules: `borrow T` and `mut T`, runs, borrow structs, capturing closures and `GpuSpan<T>`.

A projection is second-class: it is a parameter, a function's result or a local binding, and it dies with the scope that received it. It is never a field of an ordinary type or a type argument (§21).

```text
projection
├── borrow T, mut T       a function's result, or a binding (§6.3, §6.4)
├── [T], mut [T]          runs, the one view type (§4.4)
├── a borrow struct       a named group of projections (below)
├── a capturing closure   (§6.7)
└── GpuSpan<T>            a view of a GPU buffer (§18.3)
```

**A borrow struct groups projections under one name.** Code that needs the world, the assets and the frame can pass them as one thing:

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
- **Its fields are fixed once it is built,** so it borrows exactly its fields' sources, each in its field's mode.
  - A call that takes it lends its fields for that call, as passing them one by one would, so it can be passed again afterwards.
  - It is sound for the same reason that passing those fields as separate parameters is sound.
- **It is a projection itself:** a parameter, a function's result or a local binding. It is never a field of an ordinary type or a type argument.
  - A borrow struct can be generic over types.
  - A copy of a borrow struct copies its projections, and holds the same loans.
- **As a function's result,** it borrows what the call's arguments could give each field, as §6.4's results do:
  - a `mut` field borrows the `mut` arguments mutably;
  - any other projection or run field borrows every `borrow` and `mut` argument of a type that can hold the field's type.

  What a returned borrow struct borrows must be the function's parameters or constants (E0508). `pair_mut` returns one: `let both = herd.pair_mut(h1, h2)` gives `both.a` and `both.b`, which it checks at run time to be different values.

**There are no iterator types.** A `for` loop walks runs, arrays, ranges and arenas. A query that finds many items takes a closure, or returns an owned `Vec`:

```wrela
for mut g in world.grazers { g.age += dt }             // an arena
for leg in pose.legs { lift = max(lift, leg.lift) }    // a `[LegPose; 4]` field: a run
grid.each_within(p, 15m, |h| { count += 1 })           // a closure: no iterator type
let mates = grid.within(p, 15m)                        // an owned Vec<Handle<GrazerSim>>
```

Loops and closures cover most uses of iterators, so second-class projections cost little. This is why Wrela needs no lifetimes.

### 6.7 Closures

A closure's captures decide where it can go: a closure that captures a projection is a projection and can only be passed down, and a closure that captures only values is an ordinary value.

**A closure that captures a projection** (`borrow` or `mut`) is a projection itself (§6.6).
- It can be passed down, and the callee must finish with it before the callee returns. Its access counts as live for the duration of the call.
- `let` can name it (not `mut`). It borrows its captures while the name is live.
- A closure can capture another closure that captures only copies: the inner closure travels as its value. A closure that captures a projection has no value to travel as, and a function-typed parameter belongs to its caller, so a capture of either is E0702.

**A closure that captures only values** (copied, or moved with `take`) is an ordinary value.
- It can be returned and stored, for example in a field: `.with_at(|p| dapple(p, seed))`.
- Its type is its own and is inferred, as `impl Trait` is (§7).

#### Function types and function values

- A function type, `fn(f32) -> f32`, is the type of a parameter or a struct field.
- A named function is a value of its own type, which passes where a function type is expected: `apply(triple, 3.0)`.
- A type's associated function, its own or a trait's, is a value too: `Herd::of`, and `R::per_second` in a function generic over `R: Rate`, which calls `R`'s implementation. A trait's function needs that type: `Rate::per_second` alone is E0212.
- A function's parameter modes are its own. A function that borrows doesn't pass where one that takes is expected (E0300). A closure adapts it: `|a| W::work(a)`.
- A function type gives its parameters modes, `fn(mut Ui)`. A closure's parameters take their modes from the type it is passed as. A parameter without a mode is borrowed.
- `_` is a parameter that the closure doesn't use, as many times as needed: `Vec::from_fn(n, |_| None)`.
- A generic parameter bounded by a function type, `F: fn(f32) -> f32`, keeps the function it is given (it can be stored). It passes that function on where its bound fits: the same parameters, modes and result, and every attribute that the other bound asks for. `G: @deterministic fn(u32) -> u32` passes as an `F: fn(u32) -> u32`, but not the reverse.

#### Functions kept in fields

A function is kept in a struct's field or a `take` parameter.

- A field of a function type makes its struct generic over that function, as a trait in parameter position makes a function generic (§7). `struct Driver<W, Snap> { step: @deterministic fn(mut W, Tick), present: @deterministic fn(W) -> Snap }` is written `Driver<W, Snap>`. A parameter of that type, `d: mut Driver<W, Snap>`, makes the function generic over the driver's functions too.
- A struct literal gives the field a named function, or a closure that captures only values. The compiler checks it against the field's type and attributes as it checks an argument (E0300, E0600). A closure that captures a projection is E0510.
- A `take` parameter of a function type is kept, so the function is generic over it as over `F: fn(..)`: `run(world, step: take @deterministic fn(mut W, Tick))` stores `step` in its driver.
- A parameter of a function type without `take` is borrowed. It may be a closure that captures projections, and the function can't keep it.
- Each function that a value holds is a type argument, so a call through it is static, on the CPU and the GPU. No generic parameter is written for it.
- Such a struct is named only as a parameter's type, and built only by a literal. As the type of a field, a result or a binding, it is E0410: naming it there would need its functions written, and no syntax names them.

#### Where a closure can go

- **Escapability comes from the captures.** There is no annotation (§21).
- **Closures of different types can't share a container,** because there is no `dyn`. Deferred work, such as a timer or an event handler, is an enum of actions that one `match` applies. Unlike a closure, an action can be saved.

```wrela
world.grazers.par_each_mut(|g| step(mut g, world.terrain, intent))   // captures a projection: passed down only
let skin = torso.with_at(|p| Tissue { albedo: dapple(p, seed), ..HIDE })   // captures by value: the field stores it
```

### 6.8 Handles and arenas

An arena stores values in generational slots, and a handle names one slot; together they express every long-lived relationship between values.

- **`Arena<T>` stores values in generational slots.** A **handle**, `Handle<T>`, is an index plus a generation. It is `Copy` and `Plain` (§6.10), and can be stored anywhere.
- **Access:**
  - `arena[h]` is a projection, borrow or `mut` depending on context. A stale handle panics.
  - `arena.contains(h)` says whether a handle is live.
  - `arena.remove(h)` moves the value out.
- **Graphs, parent links and "this entity refers to that one" are all handles.** A projection never lasts longer than a call.

### 6.9 Allocation

Allocation is an effect on the global heap, and GPU and audio code can't allocate.

- **Allocation is the `alloc` effect** (§8). GPU and audio code forbid it.
- **The global heap** backs `Vec`, `String` and `Box`. They own their memory and free it when they are dropped.
- **To avoid an allocation every frame,** keep a `Vec` and `clear()` it. It keeps its capacity.

### 6.10 Plain data

`Plain` is the declared trait for data that can be handled as raw bytes, and `GpuData` is the declared trait that fixes a type's byte layout to WGSL's rules.

A type declares `Plain`, as it declares `Copy`, and the compiler checks that every field is `Plain` (§3.2).

- **Types:** numbers, `bool`, `Handle<T>`, `Bytes`, `Text`, fixed arrays, tuples, `Option`, and structs and enums whose fields are all `Plain`.
- **Meaning:** no pointers. Padding is zeroed and NaNs are canonical, so a value's bytes are a deterministic function of its fields. Two equal values can still have different bytes: `-0.0 == 0.0`. The bytes can be copied anywhere.
- **Uses:** GPU buffers, network messages, handles.
- **Heap types (`Vec`, `String`, `Box`) aren't `Plain`.** They can still be sim state: the engine's `SimState` is a declared trait with a structural check, and sim state is ordinary values.
- **Sim state refers to outside data by a deterministic key,** such as a definition plus a seed, never by a handle into something outside it. A restored snapshot would restore the handle, but not what it points into.
- **WASM is little-endian everywhere,** so the bytes are portable between clients.

#### GPU layout

A type that crosses to the GPU declares `GpuData`, which fixes its byte layout to WGSL's rules wherever its bytes are viewed. Other `Plain` types have a natural CPU byte layout. A type has one byte layout, never two.

- **Field order:** each field starts at a multiple of its alignment. So a `vec3` followed by a 4-byte field fills its 16 bytes, and a `vec3` followed by another `vec3` leaves 4 of them as padding. A `GpuData` struct whose fields take fewer bytes in another order gets a warning that names that order (W0007).
- **`bool`** is `GpuData`. WGSL's `bool` has no layout, so in a buffer or a uniform block (and in a struct or an array there), a `bool` is a `u32` that holds 0 or 1: the 4 bytes that the CPU holds for it. GPU code reads and writes it as a `bool`.
- **An enum whose payloads are `GpuData`** is `GpuData` too. WGSL has no unions, so GPU memory holds the enum's `u32` tag and its payload's bytes as words, laid out as the CPU lays out the enum: the tag, then the payload at the enum's alignment. GPU code reads and writes the payload through those words.
- **Enums in a uniform block:** WGSL's rules for uniform blocks need such an enum to be aligned to 16 bytes (a payload with a `vec3`, a `vec4` or a matrix), or to have no payloads. A block that holds any other enum is a storage buffer (§18.6).

#### Layout is the compiler's choice

The compiler chooses how a container stores its elements: one after another, field by field (a struct of arrays), or with rarely used fields kept apart. It may choose differently for each container type, on the CPU and on the GPU, because it generates both sides. No program can tell the difference:
- there are no addresses;
- projections never escape (§6.6);
- a value's bytes exist only where they are viewed: an upload to the GPU, a bitcast or a network message. There the bytes are canonical, as above.

Saves don't depend on layout either, because `Serialize` is derived field by field. Today every container stores its elements one after another. The language keeps the choice open: no feature exposes layout.

### 6.11 Snapshots

A snapshot is a clone of the program's state, and the same derived traits give rollback, checksums, saves and network messages.

- **A snapshot is a clone:** `let saved = world.clone()`, with `Clone` derived (§3.4). Nothing else is needed to make it sound, because the state is all values.
- **`clone_into` copies into an existing value and reuses its buffers:** `world.clone_into(mut saved)`. `Clone` provides it, derived field by field, and a `Vec` keeps its capacity. So once `saved` has grown to size, a keyframe allocates nothing: it is a memory copy.
- **Rollback** keeps a full copy every few ticks, and replays the inputs since then. Clients roll back only their own predicted entities, which are small.
- **Rollback is exact:** a replayed tick gives the hash it gave the first time. A keyframe taken or restored allocates nothing once the keyframes have grown to size. A test checks both over 10,000 ticks with random restores and replays.
- **A checksum is the derived `StateHash`** of the whole state. Canonical bytes (§6.10) make it the same on every client.
- **A save is the derived `Serialize`,** with stable field names, so saves survive layout changes and can be migrated. A repro bundle is a saved state plus the inputs since then.
- **A message is the derived `Pack`** (`std::pack`): the same values in as few bits as they allow. It is for bytes that only the build that wrote them reads, such as a network message or a durable log record.
  - Fields come in order, with no names; a variant comes as its index.
  - A `bool` is a bit. Integers are varints (zigzag for signed ones). Floats are their bits. Text and vectors put a varint count first.
  - A value has exactly one packing: varints in their shortest form, one NaN, UTF-8 text, zero padding bits and no bytes left over. Other bytes fail to read, with a `PackError`.
  - Reading never traps, and stays within a `Budget` of elements and nesting depth. So a hostile count can't make it loop, or allocate more than the bytes could hold.
  - A whiteboard edit that `Serialize` saves in 22 bytes packs in 3.

### 6.12 The unsafe core

`unsafe` exists only so that std can implement what the checker can't verify:

- `Vec` and `Arena`;
- atomics and queues: the only interior mutability, each safe to share between workers (§16.1);
- host bindings.

Each package declares whether it uses `unsafe` (§3.6). Game and engine code shouldn't need it, and the package policy flags it if it does. §20 lists the core's files and the two attributes that only they may use.

### 6.13 Memory errors and their fixes

This section shows the memory errors that agents meet most often, with their messages, helps and fixes.

Each block below gives exactly the errors that its comments name, with those helps and fixes; a test checks this. A **fix** is an edit that a tool can make. `wrela fix` makes a fix when it is the only one, unless the fix is the author's choice:

- A fix that changes what the program owns or costs is the author's choice. For example, `.clone()` copies heap memory, and an index, `remove` or a parameter that borrows may be what the author meant.
- `wrela fix` lists such a fix and doesn't make it, and its JSON says `"choice": true`.

```wrela
pub struct Herd: Clone {
    pub leader: &GrazerSim,     // error: `&` isn't a type in wrela
                                //   help: hold an owned copy (the fix), a handle (`Handle<T>`) into an arena, or make the struct a `borrow struct`
                                //   fix: hold an owned value
}
```

A struct that holds borrows is a `borrow struct`. Each mistake below is one error, and `wrela fix` makes all three fixes: `borrow struct Ctx { world: borrow World }`.

```wrela
struct Ctx<'a> { world: &'a World }
// error: `'a` is a lifetime, which wrela doesn't have
//   fix: make it a `borrow struct`
// error: `&` isn't a type in wrela
//   fix: make it a `borrow` projection
// error: `'a` is a lifetime, which wrela doesn't have
//   fix: remove it
```

An element of an arena is a projection: a copy is `.clone()`, and `remove` takes the element out.

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

A value used after a move is §6.1's example, and a `mut` borrow that another argument overlaps is §6.5's. `wrela explain <code>` shows the meaning of any code, with a wrong program and a fixed one.

## 7. Generics and traits

Generic code in Wrela is always monomorphized, and every trait that a type has is declared on the type.

- **Generic code is checked against its bounds, and always monomorphized:** each instantiation is compiled as code of its own.
- **Structure is types.** Combinators are generic types, so building a field builds a type. The field's numbers are plain data, which reach the GPU as uniforms.
- **`impl Trait` convention.** A trait written as a type means one of two things:
  - In return position, it is one concrete type, which the compiler infers.
  - In parameter position, it makes the function implicitly generic. This is the usual way to write a generic parameter.
- **Choosing structure at run time:**
  - For a finite set, use an enum (§3.3). Every case is compiled, and the branch is uniform.
  - For unbounded structure built at run time, use `std::stage::interpret` (§14.4).
- **Control-flow values are data.** Loop bounds and comparisons become uniforms. The optimizer chooses whether to unroll a loop.
- **Effects are checked per instantiation**, through every call. An error shows the call chain (`fill` → `scratch`).
- **A generic parameter that only a bound mentions is inferred.** The compiler uses the one way the argument has that trait: its one declared bound of the trait (a generic parameter's or a return-position trait's), or its one impl of the trait.
  - In `fn red<C, F: Field<C>>(f: F)`, a `Field<Color>` argument makes `C` a `Color`.
  - An impl's parameters are inferred the same way. `impl<C, F: Field<C>> Field<C> for Masked<F>` is found for a `Masked<Ball>` whose `Ball` is a `Field<Tint>`.

### 7.1 Traits

- **Every trait a type has is declared.** `Plain` and `GpuData` are declared like `Copy`. The compiler checks them by structure: every field must have the trait. There are no auto traits (§21).
  - Declaring `Copy` implies `Clone`.
  - A generic type's declared trait holds for the instantiations whose type arguments have it (§3.2).
- **A library trait can require a structural check:** every field of a type that declares the trait must also implement it. The engine's `SimState` is an example.
- **Trait sets** name traits that a type declares, or a bound needs, together: `trait Sim = Clone + StateHash + Serialize`, then `struct Gate: Sim { ... }`, `fn save<T: Sim>(x: T)` or `fn hash(x: Sim)`.
  - A set stands for its traits wherever traits are listed. So every trait a type has is still declared, by name or in a set.
  - A set can name other sets, but not itself (E0418).
  - A set can have parameters: `trait Scalable<T> = Copy + Scale<T>`.
  - A set isn't a trait. So it can't be implemented (E0419) or used as a type (E0410).
- **Library-authored diagnostics:** `@diagnostic(...)` attaches a message to a trait or a type. The compiler shows it when a bound isn't met.

## 8. Effects and contexts

An effect is something a function does besides computing its result, such as using the heap or doing IO; each context, such as GPU code or `@deterministic` code, forbids some effects.

### 8.1 The seven effects

- **`alloc`:** the code uses the heap, for example a `Vec`, a `String`, a `Box` or an f-string. Freeing heap memory is `alloc` too.
- **`io`:** the code reads or writes outside the program. Making a request to the host is `io` (§17.1).
- **`trace`:** the code makes an output that the program never reads back: a line printed (`std::io::print`) or a phase timed (`std::time::phase`, §17.3). A trace changes no value the program computes. So only three contexts forbid it:
  - GPU code, which can't call the host.
  - `@audio` code, whose thread mustn't wait on the host.
  - Derived interpretations, which run in another form.
- **`nondet`:** the result isn't a function of the inputs. The clock, randomness and a GPU readback are `nondet`. Polling a request and reading input are `nondet` too (§17.1, §17.2).
- **`recursion`:** a cycle in the call graph after monomorphization.
- **`host`:** the code records GPU work. Releasing a GPU buffer is `host` too.
- **`panic`:** an explicit panic, a failed `assert`, a failed unwrap, or an index past the end of a `Vec` or an arena. A target's trap isn't a `panic`. §11.3 says what each target does on overflow, division by zero and an index out of range, so GPU code can index.

### 8.2 How the compiler finds effects

- **Effects are inferred everywhere, and never written.** The compiler infers a function's effects from its body and everything it calls. The compiler sees the whole program, and packages are local, so effects aren't written at a package boundary either (§21). The one exception is std's unsafe core, which states effects that inference can't see (§20.1).
- **Attributes only assert.** The compiler checks the inferred effects against a context attribute such as `@gpu` or `@deterministic`.
- **Errors show the call chain:** `extract → foo → bar allocates at line 42`.
- **Public higher-order functions inherit effects from their closure arguments.** `map` is GPU-safe whenever its closure is.
- **Staging is guaranteed or rejected, never best-effort.** The optimizer may hoist work, but code must not rely on it. Work that must happen earlier is written earlier, as a `const` or a parameter.

### 8.3 What each context forbids

Each context forbids a subset of the effects.

| Context | Forbidden |
|---|---|
| GPU entry points (`@compute`, `@vertex`, `@fragment`) and `@gpu` functions | `alloc`, `io`, `trace`, `nondet`, `recursion`, `host`, `panic` (E0600). |
| `@audio` | `alloc`, `io`, `trace`, `recursion`, `host` (E0600) |
| A function passed as an `@audio fn` (§16.4) | as `@audio`; a closure captures nothing (E0600, E0510) |
| A function passed as a `@parallel fn` (§16.1) | `io`, `nondet`, `host`, and writes to data it captures (E0600, E0520) |
| `@deterministic` | `io`, `nondet`, `host` (except declared deterministic host calls), and recursion deeper than 256 calls. A request is `io` to make and `nondet` to poll, and reading input is `nondet` (§17.1, §17.2). |
| A function passed as a `@deterministic fn` | as `@deterministic` (E0600) |
| Derived interpretations (gradient, interval) | `alloc`, `io`, `trace`, `nondet`, `host` (E0700) |
| Build-time constants and tests (§13) | `io` (except `embed`), `host`, `nondet` |

`@deterministic` code must compute the same results on every host and machine (§12).

### 8.4 Recursion

Recursion in `@deterministic` code is limited to a depth of 256 calls. GPU code and `@audio` code forbid recursion entirely.

- The compiler counts the depth. The 257th call panics, the same way on every engine, with "recursion deeper than 256 calls".
- WASM engines have different stack limits. A counted limit far below all of them makes the depth the same everywhere.
- The count covers every function in a cycle that `@deterministic` code reaches, wherever the function is called from. Drop and clone glue, which follow the data, aren't counted.
- Other recursion on the CPU is limited only by the 8 MiB stack.

## 9. Attributes

An attribute (`@name`) gives a function, type, trait, constant or export a meaning that the language defines, and the set of attributes is closed.

All attributes except `@diagnostic`, `@intrinsic` and `@fuel` are part of a type or a contract: they change what a function *is*.

| Attribute | On | Meaning |
|---|---|---|
| `@compute(x, y, z)` | functions | A GPU compute entry point, with its workgroup size |
| `@vertex`, `@fragment` | functions | GPU render entry points |
| `@gpu` | functions | Asserts that the function is GPU-safe, so a violation is an error at its definition. A generic function is checked for what its own code does, whatever its type arguments. For the check, its buffers, textures, samplers and groups are bound to resources of their own, as a shader's are. |
| `@deterministic` | functions, function types | The function must compute the same results on every host and machine (§8, §12) |
| `@parallel` | function types | The function runs on several workers at once (§16.1, §8) |
| `@intrinsic` | functions, in std only | The compiler provides the body: GPU commands and built-in math. The compiler rejects it anywhere else. |
| `@fieldwise` | traits | The trait is derived field by field for every type that declares it (§3.4) |
| `@diagnostic(...)` | traits, types | An error message written by a library. It isn't part of the type. |
| `@audio` | functions, function types | Code that the audio thread runs (§16.4) |
| `@job` | functions | A job: a function whose body runs over several frames, to each `yield` (§16.3) |
| `@test`, `@test(frames: n)` | functions | A test. `wrela test` runs it as the build runs constants, or after `n` of the program's frames (§13.2). With `gpu: true` it runs on the GPU, and with `golden: "frame.png"` it compares its screen with a PNG. |
| `@fuel(n)` | constants, tests of code | The work that the build may do to compute the constant or run the test (§13.1) |
| `@client`, `@server` | functions | Only that role's build may run the function (§15.3). Reaching it from the other role's entries is E0708. A function can't have both (E0204). |
| `@testing` | exports | An export that only a test build has. See below. |

**`@testing` exports exist only in a test build:**

- A test build is the build of `wrela build --testing`, of a test harness, or of `wrela test`.
- A shipped build has neither the export nor what only it reaches: its pipelines and its functions.
- `@testing` on anything but a `pub fn` of `main.wrela` is E0222.
- In the rest of a program, `std::mem::test_build()` asks whether the build is a test build. It is a constant, as `debug_build()` is. So a path that only tests take, such as a view that only a test shows, isn't in a shipped build either.

**std's unsafe core** has two more attributes, `@effects(...)` and `@thread_entry` (§20.1). Anywhere else they are E0204.

Programs can't define attributes, so `@` always has a meaning the language defines. An unknown attribute is E0204.

## 10. Errors

Wrela separates recoverable failures, which are `Result` values, from bugs, which panic; the compiler reports what it rejects as diagnostics with stable codes.

### 10.1 Results and panics

- **A recoverable failure is a `Result<T, E>`,** passed up with `?`. `x?` returns a `None` or an `Err` from a function that returns the same kind.
- **A bug panics.** On the CPU, a panic is a trap, and its message says where in the source it happened, from the `wrela.lines` section of the program's WASM.
  - An optimized build (`wrela build --optimize`) keeps that table beside its WASM, in `game.lines`. A browser fetches the file only when a trap needs it. Without the file, the message gives the trap's code offset.
  - GPU code can't panic. An index out of range sets a debug flag or reads a value from inside the buffer (§11.3).
- **What happens after a trap** is the program's policy. The engine restores its last snapshot and writes a repro bundle.

### 10.2 Diagnostic codes

Each error and warning has a code, whose number and meaning never change once released; a retired code stays reserved.

| Codes | What they're about |
|---|---|
| E00xx | Lexical structure (§2) |
| E01xx | Syntax |
| E02xx | Names, modules and packages (§3.6) |
| E03xx | Types (§4) |
| E04xx | Traits and generics (§3.4, §7) |
| E05xx | Modes, moves, projections and exclusivity (§6) |
| E06xx | Effects and GPU rules (§8, §18) |
| E07xx | Derived interpretations, constants, programs and the back ends (§13, §14, §15) |
| W0xxx | Warnings |
| I0xxx | A bug in the compiler itself |

- **`wrela explain <code>`** gives a code's meaning, with a program that breaks the rule and the same program fixed.
- **A diagnostic can carry fixes:** edits that a tool can make. `wrela fix` makes each fix that's the only one, unless it's the author's choice (§6.13).
- **`--json`** gives diagnostics for tools, in a versioned format: each has its code, severity, message, file, span, labels, notes, helps and fixes.

### 10.3 Failed asserts

A failed `assert` shows the values it compared in a test, in a constant and in a debug build; a release build shows only its message.

- The operands of the assert's comparison (`==`, `!=`, `<`, `<=`, `>`, `>=`, through `Eq` and `Ord` too) that aren't literals are each evaluated once, before the comparison.
- The panic shows each operand whose type implements `Format`, with its source: ``assertion failed: `got` is 0.33333334``. Text is quoted.
- A message comes first if the `assert` gives one. The message is made only when the `assert` fails, so an f-string costs nothing while the assert holds.
- A release build's failed `assert` says only its message, and the build has no code to format the values. Code behind `if debug_build()` isn't in a release build, and neither are the functions that only that code calls.
- GPU code can call `debug_build()` too. For example, the engine's vertex pass writes each boundary corner's value for a test only in a debug build.

## 11. Numerics

CPU code uses strict IEEE floats, std's own transcendentals and integer arithmetic that traps on errors; GPU code follows WGSL, and its results are for presentation only.

### 11.1 Strict floats on the CPU

CPU code always uses strict IEEE floats. There's no fast-math mode, no reassociation, no implicit FMA contraction and no relaxed SIMD.

- WASM's float arithmetic is already IEEE-strict, apart from NaN bits. NaN bits are canonical wherever they're observed (§11.3).
- Transcendentals come from std (`std::math`), compiled to WASM, never from the host. They compute in f64 and round once. Tests sample 20,000 points for each function, in the ranges they choose, and each result is within an ulp. This is evidence, not proof.
- The compiler checks that each WASM module it emits has no relaxed SIMD.
- A program's command-stream hash is the same in Chrome and in the native host. That hash covers every byte that the program submits to its host: its GPU work and its requests, not its state. A state's checksum is `StateHash` (§6.11).

### 11.2 SIMD

Vector math uses WASM's 128-bit SIMD wherever the result is the same.

- A `vec2`, `vec3` or `vec4` is one SIMD value. Its arithmetic, comparisons and componentwise built-ins are SIMD instructions.
- A loop also runs four iterations at a time when all of these are true:
  - It counts a `u32` up by one to a bound computed before the loop (a `for` over `a..b`).
  - Its body is straight-line `f32` code.
  - The body reads and writes elements only at the counter's index, of arrays and runs that it doesn't replace.
  - The body keeps nothing from one iteration to the next.
- The last iterations, and an iteration that would index out of range, run one at a time. So a trap happens at the same iteration, with the same elements written.
- Standard SIMD rounds each lane exactly as the scalar operation does, and has no fused multiply-add, so the bits are identical. Relaxed SIMD stays forbidden.
- An operation whose SIMD form behaves differently stays scalar. For example, float-to-int conversion saturates in SIMD but traps in scalar code (§11.3).
- A reduction keeps its fixed order (§12). A dot product or a length adds its lanes one at a time, in order. A loop that sums stays one iteration at a time.
- A debug build runs every loop one iteration at a time, since its NaN checks panic at a specific iteration.

### 11.3 What each target does

GPU code follows WGSL's semantics, and its results are for presentation only: GPU results can't reach `@deterministic` code (§12). This table says where the targets differ.

| Case | CPU | GPU |
|---|---|---|
| Integer overflow | Traps in every build. `wrapping_add`, `wrapping_sub` and `wrapping_mul` wrap. | Wraps |
| Integer divide by zero, `MIN / -1`, a shift by the width or more | Traps | WGSL-defined values |
| Float → int | Truncates; out of range or NaN traps | WGSL-defined values |
| Float `%` | `a - b * trunc(a / b)`, exact (C's `fmod`): `a`'s sign, smaller than `b` in magnitude; NaN if `a` is infinite or `b` is 0 | WGSL's `a - b * trunc(a / b)`, rounded at each step |
| Int → int | Keeps the low bits (`u32(-1)` is 4294967295) | Keeps the low bits |
| Out-of-bounds index | Traps | WebGPU's robust access (a value from inside the buffer, or zero). A debug build checks each index. An index out of range sets a flag that the host reads after each frame, and the host stops with an error that names the pipeline. Not checked: vertex shaders (WebGPU forbids them to write storage), and a pipeline that already binds 8 storage buffers |
| NaN | Canonical (`0x7fc00000`, `0x7ff8000000000000`) wherever its bits are observed, in all CPU code: bit casts (so `Serialize` too), uploads to the GPU (`write` and uniforms), and hashing. Nothing else shows a NaN's bits, its sign included. A debug build (`wrela build --debug`) traps where a float operation creates a NaN from operands that hold none. | WGSL |

**A value's bytes are a deterministic function of its fields** (zeroed padding, canonical NaNs). Equal values don't always have equal bytes: `-0.0 == 0.0`.

### 11.4 Built-in functions

The built-in functions are in scope everywhere, on the CPU and the GPU alike; `wrela doc builtins` lists them.

- **Math**, which applies per component to vectors: `sin cos tan asin acos atan atan2 sinh cosh tanh exp exp2 log log2 pow sqrt inverse_sqrt floor ceil round trunc fract abs sign min max clamp saturate mix step smoothstep`.
- **Vectors:** `length distance dot cross normalize`. They can also be called as methods: `v.length()`, `v.normalize()`.
- **`select(if_false: a, if_true: b, cond: c)`** takes a `bool` condition. The two values may be of any one type, except a closure's or a function's type, which is E0702 (as choosing one with `if` is). It evaluates both values. Its arguments are the only named arguments of a built-in function.
- **Bit casts:** `bitcast_u32 bitcast_i32 bitcast_f32`, and `bitcast_u64`, `bitcast_f64` on the CPU.
- **Derivatives:** `dpdx`, `dpdy` and `fwidth` are for fragment shaders, in uniform control flow (WGSL's rule). They can't be inside a branch on a value that differs between pixels, or after an early `return` or `break` in one (E0608).
- **Integer methods:** `wrapping_add`, `wrapping_sub` and `wrapping_mul`. 32-bit and 64-bit integers also have `count_ones`, `leading_zeros` and `trailing_zeros`: how many bits are set, and how many zeros lead (from the top bit) and trail (from bit 0), as a `u32` (the width for 0).
- **Conversions** are calls of the type: `f32(n)`, `u32(x)`, `vec3(x)` (all components x), `vec3(y: 1.0)` (the rest zero), `vec4(v3, 1.0)`.
- **Constants:** `std::math` has `PI` and `TAU`.

## 12. Determinism

Deterministic code gives the same results on every engine and every client; `@deterministic` on a function or a function type makes the compiler check it.

- **Inside it:** strict floats (always, on the CPU), std's transcendentals, and canonical NaNs wherever bits are observed (§11.1).
- **Forbidden:**
  - the clock;
  - ambient randomness;
  - GPU readback;
  - unordered iteration;
  - relaxed SIMD;
  - anything that depends on memory addresses;
  - recursion deeper than 256 calls (§8.4), because stack limits differ between engines.
- **Checked through every call, instantiation included.** A call whose target only instantiation decides (a trait method of a generic parameter) is checked where the function is instantiated. Each body that its instance reaches is checked for `io`, `nondet` and `host`, with the call chain.
- **A panic is a deterministic trap:** every client traps on the same tick.
- **Parallelism is data-parallel only,** through std's combinators (`par_each_mut`, `par_map_reduce`).
  - Exclusivity proves disjointness, and captured data can only be read (§16.1).
  - Reductions combine in a fixed order, which the length alone decides, so results don't depend on the workers or how they're scheduled.
  - Per-entity RNG streams keep randomness independent of scheduling too.

The split between simulation and presentation is an engine pattern built on this, not a language feature.

## 13. Constants, tests and embedded data

The build computes each constant by running the program's own code, and `embed` puts a package file's bytes into the build.

The compiler has no separate interpreter and no reflection (§21). What the build knows of a program before it runs:

- **constants** (§3.5), which it computes by running the program's code (§13.1);
- **embedded files,** which `embed` reads (§13.3);
- **types,** for monomorphization, and **fieldwise derivations** (§3.4).

It runs tests the way that it computes constants (§13.2).

**A `const` bakes its result into the build, so it costs bytes; computing a value at load costs time on every start.** Choose for each value: an eigenvalue solve's result is tiny, so it belongs in a `const`. There are no procedural macros.

### 13.1 Computing constants

The compiler compiles a `const`'s initializer, and every function it calls, to WASM, and runs it in wasmtime during the build.

- **Build time and run time can't disagree.** The build runs the program's own CPU code, with the same strict floats and the same traps.
- **Its effects must be allowed** in a build-time constant (§8.3): no `io` except `embed`, no `host`, no `nondet`.
- **A panic, a trap or a failed `assert` is a compile error,** E0704. The error is at the innermost frame in the program's own code, with the call chain. A library uses this to check something at build time, such as a bone name.
- **A fuel limit turns a computation that runs away into a compile error,** not a hang. The limit is 2³⁴ units of wasmtime fuel (about one unit for each WASM instruction): under a second of work on a 2024 laptop. Past it is E0705.
- **A constant may set its own fuel.** `@fuel(2 ** 40)` on the `const` replaces the build's limit for that constant, above it or below.
  - The value is a whole number of units from 1 to 2⁴⁶. It is a literal, a power or a product of them (`@fuel(4 * 2 ** 30)`), and it is set once. Anything else is E0108.
  - `@fuel` anywhere but on a `const` (or a test of code, below) is E0108.
  - Past the constant's limit is E0705, which names the limit the constant set.
  - So a computation that needs hours says so where it is declared, and every other constant still stops within a second.
- **The result can be any owned value:** numbers, `Text` and strings, `Vec`s, `Box`es, enums and nested structs.
  - The build reads the value out of the module's memory and lays it out as read-only data, with what its pointers point to beside it.
  - A constant `Vec`'s capacity is its length.
  - A constant is a place that lives as long as the program. Reading it projects, and `.clone()` gives an owned copy.
  - A constant can't hold a closure, a run or a borrow struct (E0332), or a handle to state that exists only at run time.
- **Constants are computed in rounds.** First come the constants whose code reads no constant still to be computed, then the constants that read them. The code that computes a constant can't read that constant, through any call (E0328).
- **A constant's parallel jobs run on the build's threads.** Its code runs as a program's code does, on memory it shares with one helper for each core (§16.1). So `par_each_mut` and `par_map_reduce` in a constant use every core.
  - The value doesn't depend on how many threads computed it (§16.1's rule).
  - The fuel limit applies to the threads' work together. Each thread's work is counted, and a sum past the limit is E0705, whichever thread ran out.
- **Constants are values, never types.** The build computes them after type checking, so a type never waits on build-time code. Array lengths in types are constant expressions with no calls (§4.3).

#### Caching constants between builds

The build computes a constant again only when its inputs change: the code it runs, the constants it reads or the files it embeds.

- The build lowers each constant into a module of its own, which holds exactly those inputs. The cache key is that module's WASM, with its fuel and a hash of the compiler's and the host's code that runs it and reads its value back. So an edit to a function that the constant doesn't call leaves its key unchanged.
- The cache is files in the package's `build/consts`, one for each constant's value.
- If `WRELA_CONST_CACHE` names a directory, that directory backs the package's cache. Every package and checkout that sets it shares it, since a key holds everything that a value depends on.
- A build reads the package's cache first. It copies a value that it finds in the shared cache into the package's cache, and it writes new values to both.
- The build removes a value that no build has used for a week.
- `wrela build` reports how many constants it computed and how many it read from the cache.

#### Shipped files

A constant can ship files beside the program (`std::io::Shipped`). This is for data too large for the first frame's download, such as a world's tiles, which then stream in as the program needs them.

- `ship(dir, files)` makes a `Shipped` value from (name, bytes) pairs.
- A build writes a computed constant of this type as files under `files/<dir>/` of its output.
- The program's copy keeps each file's name and size, not its bytes. The program fetches a file with `fetch(i)`, a request like `std::io::fetch`'s (§17.1). Or it gives the file's path in the build to the host, for example as a page's icon: `path(i)` is `files/<dir>/<name>`.
- Build-time code reads the bytes in place with `bytes(i)`: a constant computed from the value, or a test, which sees the whole value. `bytes` panics in a program that a build shipped.
- A name contains only letters, digits, `_`, `-` and `.`. Another name panics, which fails the build.
- Two constants that ship into one directory are E0704.

### 13.2 Tests

A test is a function marked `@test`, which `wrela test` runs the same way that the build computes a constant.

#### Tests of code

- **A test of code** is `@test fn name() { ... }`: a free function with no parameters, no result and no generics (E0222).
- **`wrela test <package>`** checks the package and computes its constants. Then it runs each test of the package, in the order written.
  - `wrela test <package> <filter>` runs the tests whose names contain `filter`. A filter that chooses no test is an error.
  - The tests of the package's dependencies don't run.
- **A test runs as a constant is computed:** compiled to WASM with every function it calls, with a constant's effects (§8.3). Each test runs on new memory, so one test can't change what another sees.
- **Tests are built as a debug build is** (§11.3). `debug_build()` is true, and a float operation that makes a NaN from operands that hold none fails the test.
- **A test's fuel limit is 2³⁷ units,** eight times a constant's: a few seconds of work, so a test can step a simulation for a while. A test of code may set its own, as a constant does: `@fuel(2 ** 40)` on the `@test`, for a check that does more work. On a frame test, `@fuel` is E0108, since each call of a frame test gets a test's fuel.
- **A test passes unless it panics,** and a failed `assert` panics.
  - A panic or a trap is E0706, at the innermost frame in the program's own code, with the call chain.
  - Past the fuel limit is E0705.
  - `wrela test` exits with status 1 if a test fails.
- **Nothing of a test is in a build.** `wrela check` and `wrela build` don't run tests. A program's `@testing` exports are in a test build only (§9).

#### Frame tests

A frame test runs the program first. `@test(frames: n)` goes on a function that takes nothing, or takes the program's state (§15.1) borrowed. Anything else, or an `n` outside 1 to 36,000, is E0222.

- `wrela test` builds the program as a debug build, with each frame test exported. It runs the program on the native host's CPU: `init`, then `n` frames at 60 a second on a 640 × 480 screen, then the test with the state.
- The host checks every command that a frame records, as both hosts do. It answers storage and fetch requests, with storage of the test's own. It has no GPU, so a readback fails the test.
- Each call (`init`, a frame, the test) gets a test's fuel. So a frame that never ends fails after the same work on every machine.
- A failure says in which call it happened: `panicked in frame 12 of 600`.
- `@test(frames: n, input: "script.json")` gives the frames a script of input events, from a file in the package (in the format of runtime/abi's `input`). The host queues each event before the frame it is for. So a test drives a program's UI as a person would; `ui/`'s widgets are tested this way.

#### GPU frame tests and goldens

- **`@test(frames: n, gpu: true)` runs the frames on the native host's GPU.** The host answers each frame's readbacks (`std::gpu::read`) before the next frame. So a test can check what kernels and passes computed in Wrela.
  - The GPU host meters no fuel, as `wrela run` doesn't.
  - A compiler built without the GPU host fails such a test, and says why.
- **A GPU frame test can compare its screen with a golden:** `@test(frames: n, gpu: true, golden: "goldens/frame.png", within: 0.5)`.
  - After the frames, the test compares the 640 × 480 screen with that PNG in the package. It fails if their mean difference, over every channel of every pixel in 8-bit steps, is more than `within`. `within` is from 0 to 255, and 0.5 if the test doesn't give it.
  - GPU results differ a little between devices (§11.3). So a golden is a frame and a tolerance, not exact bits.
  - `wrela test --bless` first writes each golden it runs from this machine's screen. A test whose golden is missing fails, and says so.
  - A golden or `within` anywhere else is E0222: on a CPU frame test, on a tick test, or `within` with no golden.
  - A failure says how far the screen is from the golden, the largest difference in one channel, and how many channels differ.

#### An example

A test of code and a frame test:

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

#### Testing a field

- **A field's natural test is a property at many points:** `for i in 0..n { let p = space.sample(seed, i) ... }`.
  - `Box3::sample` (and `Interval`'s and `Box2`'s) gives Sobol's sequence, shifted by `seed`. The first 2ⁿ points split evenly among a box's cells, so a few thousand points cover it.
  - std's own tests check its fields this way: derived gradients against finite differences, intervals against the distances in their boxes, and stated bounds near the surface.

### 13.3 Embedded data

`embed("path")` reads a file inside the package at build time and gives `Bytes`, a `Copy` handle to read-only data in the build.

- The path is a string literal, relative to the package's root, with `/` between its parts and no `.` or `..`.
- A file that can't be read, or a path that leaves the package (through a symbolic link too), is E0218.
- The output depends only on the file's bytes, so builds stay reproducible.
- Static data lives as long as the program, so a handle to it can be stored.
- GPU code can't hold `Bytes`.

## 14. Derived interpretations

The compiler derives a function's gradient and its interval over a box from the function's own code; it derives them from any function whose effects allow it (§8.3), and it doesn't know what a field is.

| Interpretation | What it gives |
|---|---|
| `gradient` | The forward-mode derivative |
| `interval` | A conservative range over a box |
| `filtered` | The value box-filtered over a footprint (antialiasing) |
| Lipschitz bound | Not a compiler feature: a std method, `lipschitz(near)` (§14.5, §20.3). Debug builds and tests check it against the derived local bound, the `interval` of the `gradient`. |

```wrela
use std::derive::{Interval, interval}

pub fn range_over(lo: f32, hi: f32) -> vec2 {
    let r = interval(|t: f32| sin(t) * t, Interval { lo, hi })   // r.lo <= sin(t) t <= r.hi
    vec2(r.lo, r.hi)
}
```

### 14.1 Calling a derivation

The derivations are `std::derive::{gradient, value_and_gradient, value_gradient_with, interval}`. Each takes a closure or function of an `f32` or a float vector that returns an `f32`, or, for `interval`, a float vector.

- `value_gradient_with` takes a function that returns `(f32, T)` and gives `(f32, X, T)`: the value, its gradient, and the rest as the function computes it, from one derived evaluation. So a shader gets a field's normal and channels without computing its distance twice.
- `interval` takes the input's box type (`Interval`, `Box2`, `Box3`, `Box4`) and returns an `Interval`. For a vector result, it returns the vector's box: each component's range, from one derived function, the same as the per-component intervals.
- `filtered(f, at, width)` gives `f(at)` averaged over a footprint `width` wide along each of `at`'s components: a pixel's spread in `f`'s input, from `fwidth`, for example. It is forward-mode differentiation with each direction's seed its width, so each value's tangents span its range over the footprint, to first order. Each `fract`, `floor`, `ceil`, `round`, `trunc`, `step`, `sign`, `abs`, `min`, `max`, `clamp` and `saturate` on the input's path takes the average of its operand's range in place of its point value: the point value plus what the jumps or kinks inside the footprint add. So a footprint that crosses none keeps the point's value exactly, at any magnitude, and a footprint under 10⁻⁶ keeps it too. Each value is averaged alone: a product of patterns along different components averages exactly, but `abs` of a difference of two averaged patterns does not, and what a branch on the input chooses is not filtered.
- `std::field::Surface` provides `gradient`, `sample` (distance and gradient) and `interval` for every surface.

### 14.2 How they're derived

The compiler derives them from the function's body, and from every function it calls.

- **Gradients** are forward mode, one tangent per input component. They agree with central differences within 3.4e-4 relative, across the test corpus. Where a vector's `length` is zero and nothing moves it (inside a box's `length(max(q, 0))`), its tangent is zero. Where `min`'s or `max`'s arguments are equal, the tangent is the average of theirs, so std's `smin` has its true gradient on a blend's seam.
- **Derivations nest.** A derived function is ordinary code, so it can be derived again. The `gradient` of a `gradient` component is a Hessian row (and once more, a third derivative). The `interval` of a `gradient` component bounds the gradient over a box, which is a local Lipschitz bound. A closure can pass part of its input to an inner derivation as data: `interval(|q: vec4| gradient(|s: vec3| f(s, q.w), q.xyz).x, b)` bounds the spatial gradient over space and time.
- **Intervals** bound every value a point in the box can give, as the target computes it.
  - On the CPU, each rounded result is widened by its rounding: an ulp, or four for std's transcendentals.
  - On the GPU, each result is widened by twice its WGSL error bound, at least one ulp, plus 2⁻¹²⁶ for flushed subnormals.
  - A branch on the input that could go either way runs both sides and joins their results.
  - A loop whose exit depends on the input cannot be bounded, and nor can a recursion that runs under a branch on the input (E0701).
  - Integers are exact when single-valued, and otherwise their type's whole range.
  - The test corpus encloses every sample of 10⁶ boxes per function, on both targets.
- **What cannot be derived (E0700):**
  - code that records GPU work;
  - a function that uses its own derivation (each would need the next);
  - a write to a captured variable that depends on the input, or, for an interval, that runs under a branch on the input;
  - for an interval, a projection (`-> mut T`) whose place a branch on the input chooses: both sides run, so it would be either place.

  Writes and reads through other projections are derived like any others.
- **A branch narrows what its guard tests.** On the side where `a < b` holds (or `<=`, `>`, `>=`, through `!`, `&&` and `||`), `a`'s range ends at `b`'s top, and `b`'s range starts at `a`'s bottom. A strict comparison moves a bound of 0 off it. The narrowing follows back through what the operands were computed from: `let` bindings, a vector's components, `max`, `min`, `+`, `-` and negation.
  - So std's `smin` chooses the smaller distance once, with one branch, and the bound of its gradient on a blend's seam stays between its parts' bounds.
  - std's `cuboid`, written with one branch on inside or outside, has its face's normal as the bound of its gradient on a face.
  - `normalize`'s range is computed one component at a time from the others' squares. So it is exact where the other components are exactly 0, and a vector's `length` is at least its largest component's least magnitude.
- **A table read at an index that depends on the input** (an array's or a vector's element, as a loft reads its radii) gives the hull of the elements that the index's range reaches, held to the array's bounds. The derivation joins them in a loop it makes, so the code does not grow with the array, and a read near a point, whose index's range is a few elements, stays tight. Each place has at most one such index, into an array of known length whose elements hold no enum (E0700 otherwise). An index can be a `u32` or an `i32`.
- **`floor` splits into cases.** Where a component's range crosses one integer, its floor is not one integer, and what is computed from it, such as value noise's hashed lattice values, would get its whole range. So the rest of the block runs once for each case of which integer each component's floor is, in a loop the derivation makes, and the cases are joined. A component that crosses no integer, or more than one, is not split. The bound of a gradient then tends to the sampled gradient as the boxes shrink.
- **On the GPU, an interval derivation stays a function** where it is big and takes only scalars, vectors and structs of them without arrays, so nested derivations do not multiply the code. Other calls are inlined, because reading a field's uniform data through a copy is slow.

### 14.3 Known limits of the intervals

- A NaN bound becomes infinite on the CPU, but not on the GPU, which may assume NaNs away.
- WGSL bounds `sin` and `cos` only on [-π, π]; outside that range, the same absolute error is assumed.
- A branch run speculatively can still trap on the CPU (an integer overflow, for example), even if no point in the box would take it.
- std's `ellipsoid` bound divides by a length that is 0 at its centre. So a box that holds the centre gets an interval that reaches 0, deep inside the body.

### 14.4 Fields built at run time

A field built while the program runs is a tape: `std::stage::Tape` holds up to 128 operations over the point. Each operation writes its own register, and the last register is the field's value.

- A program builds a tape while it runs, from an edit log or a sculpting tool: `let d = tape.sphere(center, r)`, `tape.smooth_min(a, b, k)`, and arithmetic, `sqrt`, `sin`, `cos`, `exp`, `min` and `max` over registers.
- `stage::interpret(tape, p)` evaluates a tape. A `Tape` is a `Surface`, so it goes wherever a compiled field goes, on the CPU and the GPU (as a uniform).
- Nothing about a tape is special to the compiler: its gradient and interval are derived from `interpret`'s own code.
- Tapes of three corpus fields agree with their compiled twins within 1e-5 at 10⁶ points on both targets, and pass the corpus's gradient and enclosure tests.
- `tape.prune(over)` is interval pruning. Each `min`, `max` or `smooth_min` whose operands' intervals over the box do not overlap becomes the operand it always picks, and the tape drops what the value no longer uses. So the pruned tape gives the same values inside the box with less work.
- The compiler does not prune compiled fields (no `LiveMask`). The engine's part masks use each part's derived interval instead.

### 14.5 Bounds as methods

A bound that a function states about itself is a std method, not a fact the compiler knows.

- **Bounds depend on data.** A field's Lipschitz bound, and a noise's range and bandlimit, depend on per-individual data such as a displacement's amplitude. So they're ordinary std methods, computed at run time and passed as uniforms (§20.3). They are cheap where a derived bound would cost too much at run time: segment tracing with derived bounds takes many times the evaluations of sphere tracing.
- **Bandlimits:** a noise's `bandlimit()` method lets it fade octaves finer than a footprint, instead of aliasing, on the GPU and anywhere else a footprint is known.
- **Bounds can be scoped.** `lipschitz(near)` is a bound that holds wherever |f| < `near`.
  - Combinators pass the scope on to their parts.
  - A consumer passes the scope it needs. An interval test over a block that straddles the surface passes at least the block's radius.
  - Why: std's ellipsoid bound has an unbounded gradient at its centre. A global Lipschitz constant fails there, even though the constant holds everywhere culling, root-finding and sphere tracing look.

## 15. Programs and hosts

A program is a package whose `main.wrela` exports the functions that a host calls; a host runs the program's build, and gives it a screen, a GPU, threads, input and its other services.

Three hosts run builds, from the same ABI (`runtime/abi`):

| Host | What it is | What it runs |
|---|---|---|
| The browser host | A page in Chrome, on WebGPU (`runtime/browser`) | A client build |
| The native host | `wrela-host`, on wgpu and wasmtime (`runtime/native`) | A client build, on the machine's GPU; or its CPU alone (`--no-gpu`, and the frame tests of §13.2), which checks commands but runs no shaders |
| The server host | `wrela-serve` (`wrela_host::server`) | A server build's rooms (§15.3, §19) |

Where this spec says "both hosts", it means the browser host and the native host: a program gives the same results, and the same command stream, on both.

### 15.1 Exports and state

- **`frame`:** a host program exports `frame(time: f32, width: u32, height: u32)`, which the host calls once per frame. A host program without `frame`, or with another signature for it, is E0703.
- **`memory` is reserved:** an export named `memory` is E0703, because the program's memory has that name.
- **State:** a program with state also exports `init() -> S`. The host calls `init` once, before anything else, and keeps what it returns for as long as the program runs. Then `frame` is `frame(state: mut S, time: f32, width: u32, height: u32)`, and any other export may take the state first, `mut` or borrowed. The state is an ordinary owned value, so GPU buffers it holds last as long as it does.
- **`main.wrela` is the program's interface to the host.** Its `pub` functions are the exports, so code shared between modules goes in other files. A library package has no `main.wrela` and no exports. A top-level file named `main` in another case, such as `Main.wrela`, gets W0003.

### 15.2 What crosses to the host

Exports other than `frame` serve tests and tools, and take scalar and vector parameters.

- A vector crosses as its components: a `vec3` parameter is three `f32`s.
- An 8- or 16-bit integer or a `bool` crosses as an `i32`, of which it keeps the low bits. A `bool` is whether the `i32` is nonzero.
- An enum whose variants hold nothing crosses as its discriminant, a `u32`. A discriminant that no variant has traps (§3.3).
- An export returns a number, a `bool`, a vector, or a struct, tuple or array of those, at any depth. It cannot return an enum, or a type that std gives a meaning, such as `Text`.
- A returned value crosses as its numbers in order (fields, then elements, then components), at most 64. `wrela trace` names them `.x` to `.w`, or `.0` and on for more than four.
- Other types cannot cross, and the host cannot choose a generic function's types (E0703).

### 15.3 Client and server roles

A program has a role for each host that runs it: a client build that a browser host runs, and a server build that a server host runs.

- **Client:** a browser host runs the client build, through `init` and `frame` (§15.1).
- **Server:** a server host runs the server build. It calls `serve_init() -> S` once, when it starts the program, and `serve(state: mut S, time: f32)` when something happens. `serve` reads what happened as events. Without state, it is `serve(time: f32)`.

A program that exports `frame` has a client build, one that exports `serve` has a server build, and one with both has both. `wrela build` writes the client's files, and the server's module as `server/game.wasm`.

- Each build is lowered from its own entries, so neither module holds the other's code.
- The server's module has a memory of its own, not shared, because a server build runs on one thread and its host caps each instance's memory.
- Both modules carry the program's pair id, in the custom section `wrela.pair`: a hash of both modules. A client gives the pair id with each connection, and a server refuses a client of another pair.
- `@client` and `@server` (§9) mark code that only that role's build may run. std's screen passes, `present`, `std::audio::play` and `std::tick::start` are `@client`. A server build records no GPU work.
- Reaching the other role's code from a build's entries, through calls, generics or closures, is E0708, with the call chain.

**The server host** (`wrela_host::server`) runs a server build as one instance per room. Each instance has a cap on its memory and a deadline on every call into it, `serve_init` included. Nothing a room calls waits: the host answers a request after the call returns, for the next call to read. A call that fails says why:

- the program's own trap, with its panic's message; or
- one of the host's limits (the deadline, the memory cap, a stack too deep). This says nothing about the program, and the call may succeed with more room or time.

### 15.4 Hand-over: one build in place of another

A program can give its place to another build, in the same page or the same native process, on the same device. A game can be several builds, each its own program: for example, a tower of floors, each floor built alone and started when the player reaches it.

```text
 prepare("../floor-2") ──▶ fetched, compiled, pipelines built ──▶ answered (the program runs meanwhile)
 hand_over("../floor-2") ──after the call──▶ old program stops ──▶ floor-2's init, given kept()
                                                    │
                       init fails ──▶ the old build starts again, with what it kept
```

- **A build's path** names its directory relative to the running build's directory. It may start with one `..` part, to name a build beside it (`"../floor-2"`), and it climbs no further. So a chain of hand-overs never reaches a directory above the first build's site. A path that breaks this rule is an invalid command.
- **`std::reload::prepare(path)`** makes the build ready while the program runs: it fetches the build's files, compiles its WASM, and builds the pipelines that the device does not hold yet. A pipeline whose WGSL and manifest entry are those of a pipeline that the device holds is shared. `prepare` answers when the build is ready, or fails with the reason; asked again, it gives the same answer. **`unprepare(path)`** releases what `prepare` made ready.
- **`hand_over(path)`** gives the program's place to the build after the call that asked. Only the last request in a call counts; an earlier one fails. If the build is not ready, the host makes it ready, and the frames wait. Then the host stops the program and starts the build with `init`:
  - **`kept()`** gives what the old program kept (`keep`), and input that the old program did not read goes to the new one.
  - **No ticks go across:** the new build's sim is its own, and in lockstep its ticks count from the frame it started at. The old program's voice stops.
  - **Storage belongs to the page,** and every build that the page runs shares it. A fetch reads the running build's own files.
  - If the build cannot be made ready, the request fails and the program continues. If the new build's `init` fails, the old build starts again in its place with what it had kept, and the host prints the reason: the screen never goes black.
- The host keeps the pipelines of the build that handed over, which the new one does not use, until the next hand-over. So going back builds no pipelines.
- A host that runs one build alone (the CPU host) fails these requests.

## 16. Concurrency

A program runs on several threads that share one memory, and shares data between them only as values that no thread can change under another; it has no `async`, as IO is a polled request (§17.1).

| Thread | What runs there | Where |
|---|---|---|
| The program's thread | `init`, `frame` and the other exports | §15.1 |
| Workers | The chunks of a parallel call, and tasks | §16.1, §16.2 |
| The ticker | A fixed-rate step: the simulation's ticks | §16.2 |
| The audio thread | The program's voice, 128 samples at a time | §16.4 |

- **Any owned value can move to another thread,** and data that several threads share can only be read. Wrela has no interior mutability and no shared ownership, so nothing needs marking as safe to send or share (§21).
- **Work that spans frames** is a job, which the program resumes each frame (§16.3), and results cooked from inputs are cached by those inputs (§16.5).

### 16.1 Threads

A program spreads work across workers that share its memory only through data-parallel combinators, whose results don't depend on how many workers there are.

- **Workers share one WASM memory,** whose maximum is reserved at startup: in a browser, web workers and a SharedArrayBuffer. vision.md describes the threads' layout.
- **Parallelism goes through data-parallel combinators.** Exclusivity proves that their parts are disjoint, so game and engine code has no locks. `Vec` has two combinators:
  - `xs.par_each_mut(f)` calls `f(mut x)` for each element.
  - `xs.par_map_reduce(init, map, reduce)` folds `reduce(acc, map(x))` from `init`.

  `f`, `map` and `reduce` are `@parallel fn`s. Several workers run them at once, so each of them:
  - writes only its own element, never data it captures (E0520);
  - does no IO, GPU work or non-deterministic work (E0600);
  - may read what it captures, allocate and panic. A panic on a worker is the program's panic.
- **Results don't depend on the workers.**
  - A **parallel call** (one call of a combinator) is cut into chunks by its length alone: about 64 elements a chunk, and at most 4096 chunks.
  - Workers claim chunks as they become free, but each chunk folds its elements in order, and `par_map_reduce` folds the chunks' results in chunk order.
  - So the same program gives the same results with 1, 2, 4 or 8 workers.
- **The layout:** the program's thread and up to 8 workers share one memory (`wrela_abi::memory`), each with its own 1 MiB stack.
  - Each worker runs its own instance of the module. A start function lets the constants be copied only once.
  - The thread that starts a parallel call runs chunks too, then waits for the rest.
  - A host may run fewer workers. With none, the program's thread runs every chunk.
- **Atomics and queues** live in std's unsafe core. They are the only interior mutability in the language, and they are built for concurrent use. **Every std type with interior state must be safe to share between workers.** This is a rule for std's core, which a review checks when the code is written; the compiler doesn't check it.

### 16.2 Ticks, hand-offs and tasks

A program can run a fixed-rate step on a thread of its own (ticks), pass its newest result to the frames (a hand-off), and send long work to a helper thread (a task).

All three are in std, built on the unsafe core (§6.12), and none of them is specific to games. The engine's `run`, which runs a world on a timeline, is ordinary code on top of them.

#### Ticks

- **`std::tick::start(state, hz:, step:, hash:)`** moves `state` to the **ticker**'s thread. There, `step(mut state, ticked)` runs `hz` times a second. Each run of `step` is a **tick.**
  - `Ticked` holds the tick's number, its length in seconds and its **records:** the input events that the host stamped at the tick's start, oldest first, at most 256. The rest wait for the next tick.
  - `step` and `hash` are `@deterministic` functions (§12), or closures that capture only values (E0510), because they move to the ticker's thread too. So a tick's state is a function of the first state and the records.
  - `hash` gives the state's hash. A host asks for it when it records or checks ticks.
  - A program has one ticker; a second `start` panics.
  - `std::tick::origin()` is the frame time at which tick 0 was due; a frame uses it to place the newest tick. It is `nondet`: the host moves it when it drops ticks that it couldn't run in time, or when the page was hidden.

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

#### Hand-offs

- **`std::handoff::handoff(initial)`** makes a **hand-off**, which passes the newest complete value from one thread to another. It gives a `Publisher<T>` and a `Latest<T>`.
  - `publish(value)` runs on one thread, and `read()` on the other gives the newest complete value.
  - Neither waits, and no value is read half-written: a hand-off is a triple buffer, with one atomic word between the threads.
  - `T` is `Plain` (§6.10), so a value is a copy of bytes (E0400 otherwise).
  - `read` is `nondet`: which value it gives depends on timing, so a tick can't read one (E0600).
  - In a debug build, each value carries its publish number and a checksum, and `read` checks both. No value is read torn, or older than a value read before it.

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

#### Tasks

- **`std::par::spawn(input, f)`** starts a **task**, `f(input)` on a helper thread, and gives a `Task<O>`. The helper holds the task while it runs.
  - `join()` gives the result, and waits if a helper is still running the task.
  - `f` is `@deterministic`, so the result is a function of `input` alone, whichever thread runs it and whenever.
  - A task that no helper has started runs inside `join`. With no helpers, every task runs there, and a task that joins another can't deadlock.
  - A trap in a task is the program's panic at its `join`.
  - `done()` says whether the task has finished, and is `nondet`. With no helpers (a page on a machine with two cores), `done()` runs a task that hasn't run yet, so a program that polls a task before it joins it doesn't wait forever.
  - A parallel call (`par_each_mut`, §16.1) inside a task, or inside a `@parallel fn`, runs on that thread alone.

  ```wrela
  use std::par::spawn

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
      let pending = spawn(take data, checksum)   // a helper hashes while this thread goes on
      take pending.join()
  }
  ```
- **The engine's sim joins a task only at the tick its answer is due** (`engine::run`'s `Asks`). If the task isn't done then, the ticker waits. So the tick on which an answer enters doesn't depend on the helpers.

#### Counting, logging and replaying ticks

- **Each thread keeps counters** in wrela_abi:
  - `JOIN_WAITS`, the times it waited at a join;
  - `ALLOCATIONS`, the blocks it allocated;
  - `LOCK_WAITS`, the times it found the allocator's lock taken;
  - `LOCK_SPINS`, the tries it spun on that lock.

  The native host logs a tick that waited at a join, and reports the counts for each tick.
- **Per-tick input.** Before each frame, a host queues the input events since the last frame for `std::input::events()`. The same events, except messages and connection events, become the next tick's records. A tick sees only its records, never the frames' input queue, so its state depends on the records alone.
- **The tick log** (wrela_abi's `ticks`) holds a run's ticks as a host ran them: the build's WASM hash, the rate, the first world's hash, then each tick's records and the state hash that its step gave. The build, the first world and the records fix every hash, so any host can replay a log and check it. Chrome's test mode writes a log (`results/ticks.log`), and so does the native host on request.
- **The native host without a GPU:**
  - `wrela-host --no-gpu <build> --ticks N` runs the ticker alone, with no frames, and prints each tick's hash. `--log out.ticks` writes the log.
  - `wrela-host --replay <log> [--no-gpu] <build>` replays a log's records. It fails at the first tick whose hash differs, and names that tick.
  - A build of `wrela-host` without its `gpu` feature has only these two commands. An x86-64 build under Rosetta checks the sim's bits on x86's code generation.
- **Test mode's two schedules:**
  - **Lockstep:** before frame i, the ticks up to ⌊(i + 1)·hz/fps⌋ run, and the frame waits for them. Both hosts do the same, so frames and hashes compare across hosts.
  - **Paced** (`#test&paced=1`): ticks run on the ticker's own clock, and neither ticks nor frames wait for the other, as in play.
  - A GPU that sets its clock by its load stretches a paced frame's work to fill most of its interval. So GPU budgets are measured with the frames back to back (`#test&saturate=1`, in lockstep): two frames in flight, the canvas left alone, the GPU busy and its clock high.

### 16.3 Work over several frames

A job is a function whose body runs over several frames: it stops at each `yield`, and goes on from there each time the program resumes it.

Without jobs, work too long for one frame needs a state machine written by hand: a step counter kept in the state and stepped each frame. A cache that edits make stale one part at a time stays a flag for each part, not a job: a job runs once from its start to its end, and a job restarted by an edit would have to know what the last run hadn't done.

**A job is a function whose body runs over several frames.**
- `@job fn realize(creature: take Fawn, scratch: mut Scratch) -> Mesh { cull(...); for i in 0..slices { place(i); yield } finish() }`. `yield` ends the job's work for this frame. It is written in the job's own body, not in a closure or another function (E0335).
- `realize.start(creature)` takes the job's `take` parameters, matched as a call's arguments are, and makes a `Job<realize>` (`std::job`). This is an owned value that holds where the job stopped and the owned locals of its body. It is an ordinary field of the state.
- `job.resume(mut s.scratch)` lends the job its `borrow` and `mut` parameters and runs it to its next `yield` or its end. It gives `Step::Yielded` or `Step::Done(result)`.
  - Each `resume` lends the parameters again, so the job may use them after a `yield`. A stopped job holds none.
  - A `resume` of a job that has ended traps.
- A job is a free function: not a method, an entry point or a test. It returns a value that it owns (E0336).
- A job may be generic: `Job<realize<Coat, Fawn>>` names its types, and `start` infers them as a call does.
- A job isn't called or passed as a value (E0336): it is started. A `pub` job isn't an export.
- **Nothing that holds a loan lives across a `yield`:** not a projection, nor a closure or a borrow struct that holds one (E0521). This includes a projection of a lent parameter, which the next `resume` may lend from another place. Owned values live across a `yield`, and so does a closure that copies what it captures.
- A job has no executor and no IO of its own. The program resumes a job where it would test a flag, and a request stays a polled value (§17.1): `while p.poll().is_none() { yield }`.

**A job's value is state like any other.**
- Its parts are where it stopped and each local it holds: its body's owned locals, its `take` parameters, and the values that a loop keeps, such as a range's bound.
- It has `Clone`, `StateHash` and `Serialize` when all its parts do, derived from them as a struct's are from its fields. So a simulation's state can hold a boss's attack as a job, which `@deterministic` code `resume`s a tick at a time. Snapshots clone it, tick hashes hash it, saves save it, and each copy goes on as the original does.
- A struct that declares one of these traits over a job that holds a local without the trait is E0407, which names the local.
- A local that isn't set yet at the place where the job stopped, or that was moved out, is zeros, which clone, hash and save as zeros (std's `Box` saves an empty box as one).
- Where the job stopped is a place in the job's code, so its saved name holds a hash of that code. A save made by a build whose job is different doesn't load (`LoadError`), and `std::reload::keep` carries a job across a hot reload only when its code is the same.

**How it is built.** The memory checker already knows what is live at each `yield`. Lowering gives the job's value a field for each owned local of its body, its `take` parameters among them. `resume` moves the fields into its locals, runs its blocks from the one that the value names, as cases of a loop, and at a `yield` moves the locals back. Its locals live in IR locals and its projections in pointers, as a projection chosen at run time does (§6.3), since no IR value flows between its blocks.

**Why it fits Wrela.** Projections can't be stored, so a stopped job holds only owned values, and its value is plain data. The frame is already the suspension point (§17.1). A job isn't `async` (§21): it has no futures, no executor and no IO. It is a loop body that the compiler splits at its `yield`s, and the program calls it.

### 16.4 The audio thread

`@audio` code runs on the audio thread. It never allocates, does IO, recurses or records GPU work (E0600).

- **A program has one voice.** `std::audio::play(state, render)` moves `state` to the audio thread. From then on, the audio thread calls `render(mut state, mut left, mut right)` for each render quantum: 128 samples at 48 kHz in each of two channels.
- **`render` is an `@audio fn`:** a function, or a closure that captures nothing, because the audio thread keeps it (E0510).
- **The voice's events come through a `ring::<T>(n)`.** The program keeps its writer, and the voice's state holds its reader. One thread pushes and one pops, so the ring needs no lock.
- **Both hosts run the voice the same way,** each on its own instance of the module, which shares the program's memory: the browser host in an AudioWorklet, and the native host offline (it plays no sound). A voice whose events are all pushed before it starts renders the same samples on both hosts, bit for bit.

### 16.5 Results cached by their inputs

`std::cache` keeps results that a program cooks from inputs up to date: each cache names the inputs it was cooked from, so a change to an input reaches every cache cooked from it.

**Without it,** a program that cooks results over frames (a sky's tables from its air, shadows from a forest, probes from the shadows) keeps a flag for each cache. When an input changes (an edit, a test, a key), the program must clear each flag cooked from it by hand. A missed flag leaves a cache stale; one flag too many cooks for nothing.

**A cache names what it is cooked from** (`std::cache`).
- `Stale<K>` holds the key that a result was cooked from:
  - `stale(key)` says whether the result must be cooked again: it was never cooked, or was cooked from other inputs;
  - `cooked(key)` records the key;
  - `version()` counts the cooks.
- A key is the inputs themselves (`Stale<(Air, Light)>`: anything `Eq` and `Clone`), or the versions of what they come from: another cache's `version()`, or a `std::lift::Watch`'s `version()`. A `Watch`'s version counts how many times the literals that a function's code reads have changed; `Watch::new(reads(|| f(x)))` watches any function's literals.
- So a change reaches every cache cooked from it, through any number of steps. A cache cooked from another cache names that cache's version.
- `forget()` is for an input that the program changed by hand and that no key names.
- `Cached<K, V>` keeps the value too: `get(key, make)` gives it, and makes it again only when the key changes.

The clearing example's `cook_again`, after the light's literals change:

```text
cook_again: the light's literals change ──► s.light, s.air reloaded
  air's tables:    Stale<(Air, Light)>               stale ──► cooked
  weather's maps:  Stale<(light's version, clouds')>  stale ──► cooked ──► version + 1
  clouds' shadow:  Stale<(on, weather's version)>     stale ──► cooked
  static shadows:  Stale<(Light, on, versions...)>    stale ──► drawn  ──► version + 1
  probes:          Stale<(versions...)>               stale ──► the next probes bake
```

`std::cache` is a library, not a language feature. The inputs that a key names are the program's choice, as a function's arguments are. The compiler's part is what it already knows: the literals that a function reads (`reads`). The clearing's five cooked flags are caches (`examples/clearing`), and its `cook_again` reloads only what changed.

## 17. Host services

A program asks its host for IO, GPU readback and other work through requests that it polls, and reads input, prints lines and reads the clock through the host's other services.

The services are `platform::host`'s and std's functions over one ABI (`runtime/abi`), which both hosts implement alike. What a program asks of its host is `io` to make, and what it reads back is `nondet` (§8.1), so `@deterministic` code takes the host's answers as data, through a tick's records (§16.2).

### 17.1 Requests, not async

A **request** is work that the program asks the host for, and reads the answer of later; Wrela has no `async` (§21).

- A call submits the request and returns a `Pending<T>` handle at once:
  - `std::io::load(path)`, `store(path, bytes)` and `fetch(url)`, for the program's storage ("bytes at a path") and its build's files;
  - `std::gpu::read(span)`, for a GPU readback.
- The program polls the handle on a later tick or frame. `p.poll()` returns `None` until the answer arrives. Then it returns, once, `Some(Ok(values))` or `Some(Err(e))`. `values` is a `Vec<T>`: bytes for IO, a buffer's elements for a readback.
- The host answers a request no sooner than the next call. A browser's IO takes as long as it takes.
- A request has the `io` effect to make (a readback `nondet` and `host`), and the `nondet` effect to poll. So `@deterministic` code can't make or poll one (E0600). A result reaches the sim only through a tick's input, so it enters on a known tick.
- A path is relative, with `/` between its parts. No part may be empty, `.` or `..`; another path panics when the request is made.
- Storage is files under the host's storage directory on the native host, and the origin's private file system in a browser.

A game already has a suspension point, the tick or the frame, so the language doesn't need another. A handle is an ordinary value, so nothing needs `Pin`.

#### Posts

`std::io::post(url, bytes)` is a request to the host that serves the program. It sends the bytes to the URL, relative to the build, and the answer is the host's reply.

- A browser sends it to the build's own origin and nowhere else: the URL is a relative path, as a fetch's is.
- The native host answers with whatever its user gives it (`wrela_host::PostHandler`), and fails the request otherwise.
- Tools served by a local server use it (`wrela studio`, §23.6). A game can use it for its own server.

#### Application services

`std::host::request(service, bytes)` uses the same `Pending<u8>` model for the capabilities of an application host.

- The service name is a relative path. The payload and the reply are opaque bytes.
- An unsupported service fails as an ordinary IO request.
- The shared command ABI and both hosts carry the request; Wrela packages own the protocol schemas and the policy. The compiler knows no application service names.
- The command buffer bounds a request. Reading an answer has the same `nondet` effect as the other requests.
- Examples are an embedded application's message channel and a file that the user selects. These mechanisms are not specific to games.

### 17.2 Input and messages

Input (`std::input`) is the pointer, the keyboard and up to four gamepads. `events()` gives the events that the host has queued since the program last read them, oldest first:

- The pointer moves, a button goes down or up, or the wheel turns.
- A key goes down or up (a physical key, `Key::A`), or types text (`Event::Text`, a Unicode scalar value).
- A gamepad's button moves (`Event::PadButton`) or its stick moves (`Event::PadAxis`), named by where it is on the pad (`PadButton::South`, the W3C standard mapping), for pads 0 to 3.
- `Event::Compose` is one character of the text an input method is composing. Each time that text changes, all of it comes again, one event a character. A `Composer` puts each change together, and ignores a change that a reader fell behind on and saw only part of. Committed text comes as `Text`.
- `Event::Paste` is a paste. It carries a serial, and the program asks its host for the pasted data by that serial.
- `Event::Assist` is what a person or an assistive technology does to a node of the program's semantic tree (`std::semantics`), by the node's number.
- `Event::Field` says that a text field's text changed. The host keeps the text, and the program reads it by revision.

All are host services, `platform::host`. Positions are in the same pixels as `frame`'s width and height. `Input` keeps what is held and where the pointer is, from the events it is given: `input.read()` reads them, and `input.update(events)` takes them as data.

A call reads only the events that arrived before it started, in the same order on both hosts. The native host plays a script of them (`wrela-host --input script.json`, and a frame test's `@test(frames: n, input: "script.json")`, §13.2). A browser delivers each event to the program's next frame.

#### Messages

Messages from a connection (§19) are input too.

- `Event::Message { conn, serial, len }` is bytes from a connection. `payload(e)` reads them in the call that was given the event, and no later. An earlier call's message reads as `None`: its bytes are gone, and a host never uses a serial twice.
- `Event::Connection { conn, open }` says that a connection opened or closed.
- A call is given messages until their bytes reach 1 MiB (its first message whatever its size), and the rest wait.
- A host keeps queued messages across a hot reload to the same program's build. It drops them, and connection events, on a reload to another build, whose types may read their bytes differently.
- Reading a payload is `nondet`, as reading input is.

#### Input is `nondet`

Reading input is `nondet` (§8.1), as polling a request is: what a call reads depends on when a person moved the mouse. So `@deterministic` code takes input as data, the events a tick is given, as it takes the time.

### 17.3 Printing and the clock

#### Printing

`std::io::print(line)` isn't a request. It shows a line on the host's console now (the terminal that runs the native host or the build, or a browser's developer console), for whoever is writing the program; a player doesn't see it.

- The program never reads the line back, so printing is the `trace` effect, not `io` (§8.1). A constant, a test, `@deterministic` code and a chunk of a parallel call can print too, and a tick that runs again prints again.
- The line goes through its own import, `wrela.print`, which every thread's instance has, not through the command stream. So the host shows it as it is made: a frame that traps after it, or a constant that runs for an hour, has already shown it.

#### The clock

The clock (`std::time`) measures how long the program's work takes.

- `now()` is seconds on the host's clock: an `f64` that never goes back, the same clock on every thread. `since(t)` is the seconds since a time that `now` gave.
- The host chooses where the clock starts (the native host's first program; a page's time origin), so a time is only for comparison with another time the program read.
- Reading the clock is `nondet` (§8.1), so `@deterministic` code, a constant and a test can't read it. A simulation's time is its ticks' (§16.2).

#### Timing work

Timing work is an output (`std::time::phase(name)`), and its effect is `trace`.

- The time from a `phase` to the same thread's next `phase`, or to the end of the call that the host made on that thread (a frame, a tick, a constant, a test), counts under `name`. The time is summed over every time it is timed. An empty name times nothing until the next `phase`.
- The host reads the clock, through `wrela.phase`, and the program never reads what the host read. So timing is the `trace` effect, not `nondet`: a constant and a test time their phases too.
- `wrela build` shows each constant's phases when it computes the constant, `wrela test` each test's, and the native host a program's when its run ends (`wrela-host`). The browser host times none, since a page has its browser's profiler.

## 18. GPU code

GPU code is the kernels and shaders that a program runs on the GPU, the functions they call, and the CPU commands that make GPU resources and record GPU work.

The memory model carries over to the GPU:

- **Parameter modes map onto WebGPU's bindings:** a borrowed parameter is a read-only storage or uniform binding, and a `mut` one a `read_write` storage binding (§18.6).
- **WebGPU forbids writable bindings that alias** within a dispatch, which is exclusivity applied to each binding (§6.5).
- **Exclusivity says nothing about one dispatch,** whose invocations all hold the same `mut` binding at once. So a kernel's `mut` parameters take only types that invocations can share safely (§18.4).
- **Data that crosses to the GPU is `Plain` and `GpuData`** (§6.10).

### 18.1 Entry points

An entry point is a kernel or shader, marked `@compute(x, y, z)`, `@vertex` or `@fragment`.

- **Workgroup size:** a kernel's workgroup size is within WebGPU's limits (E0605). Each dimension is at least 1, and at most 256, 256 and 64. A workgroup has at most 256 invocations in all.
- **Builtins are typed.** `std::gpu` gives them by stage. A stage takes only its own (E0602).
  - Compute: `GlobalId`, `WorkgroupId` and `LocalId`. The `.xyz()` of each is a `vec3u`.
  - Vertex: `VertexIndex` and `InstanceIndex`.
  - Fragment: `FragCoord`.
  - `ClipPosition` is a vertex shader's output, and `Flat<T>` holds a value that is not interpolated.
- **GPU-only operations:** builtins, derivatives and texture sampling exist only in GPU code (E0607).

#### What an entry point returns

- A kernel returns nothing.
- A vertex shader returns a `ClipPosition`, or a struct with one `ClipPosition` field. It passes the other fields to the fragment shader.
- A fragment shader returns one of these:
  - a `vec4`, which replaces what is in its target;
  - a `u32`, the texel of an `R32Uint` target. A draw of a `u32` into another target, or of a colour into an `R32Uint` target, is E0603 at the draw.
  - a `std::gpu::Over`, whose colour is drawn over the target: alpha blending with straight alpha, for text and UI;
  - a `std::gpu::WithDepth<C>`: a colour `C` (a `vec4`, or a `u32` for an `R32Uint` target) and the fragment's own depth, in place of its triangle's depth. It draws a flat stand-in for something with depth, such as an impostor's leaves and the id of the tree that won the pixel. It is drawn only in a pass with a depth target (E0603 otherwise).

**What passes from the vertex shader to the fragment shader:** numbers and vectors of `f32`s, `i32`s and `u32`s, matrices, and structs and tuples of those. Each value is interpolated, or held in a `Flat<T>`; integers are never interpolated. WGSL gives each number or vector a location of its own, each matrix column one, and each struct field one. At most 16 locations pass.

#### What an entry point takes as parameters

- builtins;
- `GpuData` values, passed together as one uniform block;
- `[T]` buffers and `GpuSpan<T>`s, to read;
- textures and samplers;
- groups of those (§18.6);
- a kernel's outputs (§18.4);
- for a fragment shader, the vertex shader's output, and `mut Atomics<T>`, for example to count what it shades in a test. WebGPU gives a vertex shader no writable buffers, so a vertex shader's `mut Atomics<T>` is E0602.

A fragment shader that writes a buffer runs before the depth test, because WGSL has no early fragment tests. So it also writes for fragments that the depth test then drops. To count what passes, the shader compares the fragment's depth itself.

**Discard:** `std::gpu::discard()` drops the fragment that a fragment shader is shading. The fragment writes no colour and no depth, so a shader can cut leaves and grass from cards (an alpha test). The invocation goes on as a helper, so derivatives stay defined (WGSL's demotion). Anywhere but a fragment shader, or a function that only a fragment shader calls, it is E0607: a kernel, a vertex shader and CPU code have no fragment to drop.

```wrela
@fragment
fn shade<F: Surface>(pixel: FragCoord, scene: Scene, field: F) -> vec4 {
    let p = scene.hit(pixel.position.xy, field)          // sphere-traced
    let n = normalize(field.gradient(p))                  // the derived gradient
    vec4(scene.light(n), 1.0)
}
```

`examples/hello-field` shades this way. With channels and footprints, the shader reads only the channels it uses and fades noise finer than a pixel.

### 18.2 What GPU code may contain

GPU code is ordinary Wrela with static calls, a fixed data layout and a few GPU-only tools.

- **Closures are allowed when they resolve statically.** The compiler monomorphizes and inlines them. It may unroll a loop over a fixed-size array.
- **Calls are inlined,** so a kernel reads its uniform data in place. The exception is a large function that takes no uniform data: it stays a function, so a derived function called from many places appears once in the WGSL.
- **Layout is automatic but lossless.** `GpuData` fixes a type's layout to WGSL's rules everywhere. A lossy encoding is an explicit type. Nobody pads by hand.
- **Uniform and varying values** are the target's own distinction, which WGSL already analyzes.
- **Workgroup-shared memory and barriers** use `Shared<T, N>` (§18.5). Each `LocalId` writes its own chunk (`chunk`, `write`), `get` reads, and `barrier(mut shared)` separates the phases (E0609, E0610).
- **GPU interval arithmetic widens each result outward** by a margin from its operation's WGSL error bound, so the interval stays conservative (§14.2).

### 18.3 Buffers

Data on the GPU has types of its own: the buffers, `GpuBuffer<T>`, `GpuSpan<T>` and `GpuSpanMut<T>`; and the kernel outputs, `Slots<T>`, `Shared<T, N>`, `Atomics<T>`, `Append<T>`, `AtomicMap`, `One<T>`, `Texels` and `Texels3d` (§18.4, §18.5).

- **`GpuBuffer<T: GpuData>` is an owned value.** CPU code can make one (`buffer(count)`), pass it to kernels and shaders, write into it, and copy between buffers. CPU code can't read through it.
- **An empty buffer has one element.** WebGPU can't bind an empty buffer, so `buffer(0)` has room for one zeroed element, and its `len()` on the GPU is 1.
- **Passing a buffer follows the modes.** A parameter that the GPU writes (`mut Slots<T>`) takes `mut buf`, and one that it reads (`[T]`) takes `buf`, borrowed (E0503 and E0504, as for calls).
- **A dispatch or a pass can't both read and write one buffer.** The compiler checks the arguments' overlap as for a call (§6.5): `dispatch(k.bind(values: out, total: mut out), groups: 1)` passes one buffer as a `[T]` and a `mut Slots<T>`, so the compiler rejects it, and the host never meets it. As a backstop, the hosts check every command against WebGPU's usage rules.
- **A buffer lives with its owner**, until its owner's scope ends; a buffer in the program's state lasts across calls.
  - Dropping a buffer records a `DestroyBuffer` command, in order with the work that uses the buffer. The host releases it once the GPU work submitted before it is done, and never uses its handle again.
  - A call that traps submits nothing, so the host never sees the buffers that call made.
- **A span is a projection of part of a buffer,** as a run is of an array: `buf.span(start, count)` gives a `GpuSpan<T>` to read, and `buf.span_mut(start, count)` a `GpuSpanMut<T>` to write.
  - A span binds its range. WebGPU needs the range to start at a multiple of 256 bytes, and the host checks that.
  - WebGPU forbids a read and a write of one buffer in a pass, over the whole buffer. So a span borrows its whole buffer, and two spans of one buffer overlap when they're bound together, even if their ranges don't.
  - In GPU code, a `GpuSpan<T>` is its elements: it indexes as a `[T]` does, its `len()` is its count, and it passes where a `[T]` is taken. CPU code can't read the elements (E0607).
- **Copies are commands:** `write(mut buf, at, values)` uploads (§18.7), and `copy(from: span, to: span_mut)` copies between buffers in recorded order. There's no zero-copy path between WASM memory and the GPU (vision.md).
- **Readback is a request.** `std::gpu::read(span)` returns a `Pending<T>`, which the program polls on a later frame (§17.1). Its answer is the span's elements, once the GPU work recorded before the request is done. `read` has the `nondet` effect, so `@deterministic` code cannot call it.

### 18.4 Kernel outputs

A kernel's `mut` parameters must be safe for its invocations to share, so each is one of the output types below: a type whose writes don't race, because each invocation writes only its own place, or because each operation is atomic. A plain `mut` array or run is E0601. CPU code passes each output's buffer `mut`, and both hosts give the same results.

- **`Slots<T>`:** each invocation writes only its own slot. The index is the invocation's own `GlobalId`, which code cannot build or change.
- **`Atomics<T>`,** of `u32` or `i32`: `load`, `store`, `add`, `sub`, `min`, `max`, `and`, `or`, `xor`, `exchange` and `compare_exchange`, each one atomic.
- **`Append<T>`:** `push` into an `AppendBuffer<T>`. The buffer's count also serves as an indirect dispatch's group counts.
- **`AtomicMap`:** `insert`, `add` and `get` of `u32` keys and values, in an `AtomicMapBuffer`.
- **`Shared<T, N>`:** workgroup-shared memory (§18.5).
- **`One<T>`:** one value, written by the dispatch's first invocation, whose `GlobalId` is (0, 0, 0).
  - `out.set(id, value)` writes the value. `out.update(id, |old| new)` changes a value kept from dispatch to dispatch.
  - Any invocation may call them, but only the first invocation's call does anything, so no calls race.
  - CPU code passes `mut buf` of a `GpuBuffer<T>` or a `GpuSpanMut<T>`. The value is its first element.
- **`Texels<F>`:** each invocation writes only its own texel, at its `GlobalId`'s x and y, with `out.store(id, value)`. The value is of the format's texel type.
  - CPU code passes `mut tex` of a `Texture<F>` made by `writable_texture`. Kernels and passes may write that texture, and shaders may also read it.
  - Only a kernel takes a `Texels<F>` (E0602 elsewhere).
- **`Texels3d<F>`:** the same as `Texels<F>`, for a `Texture3d<F>`, at the `GlobalId`'s x, y and z.

### 18.5 Workgroup memory

Workgroup memory is a kernel's `mut Shared<T, N>` parameter: `N` values of `T` that all the invocations of a workgroup share. CPU code doesn't pass it.

- `let c = shared.chunk(lid)` gives an invocation its own chunk, which std assigns by `LocalId`: every workgroup-size-th element, from the invocation's index.
- `shared.write(c, k, value)` writes element `k` of chunk `c`, for `k` below `c.len()`. Between barriers, each invocation writes only its own chunk.
- `shared.get(i)` reads any element, from any invocation.
- `barrier(mut shared)` separates a phase of writes from a phase of reads.

The rules:

- A `LocalId` can't be built or changed (E0601), so chunks don't overlap.
- The compiler follows each kernel's paths, calls included. It rejects a read of workgroup memory with no barrier since a write, and a write with no barrier since a read (E0609). It doesn't compare indexes, so a read of an invocation's own chunk needs a barrier too.
- A barrier must be in uniform control flow, as WGSL requires (E0610).
- Workgroup memory starts zeroed. A pipeline has at most 16 KiB of it (E0602).

### 18.6 Bindings and groups

Each parameter of an entry point is a binding, and WebGPU limits how many bindings each shader stage has; a group gathers bindings into one parameter.

**Storage buffers:** each shader of a pipeline binds at most 8 storage buffers, WebGPU's default for a stage. They are:

- the shader's own `[T]` and `Slots<T>` parameters;
- the uniform block, when it is over 64 KiB or its layout does not meet WGSL's uniform rules.

Each shader's parameters are its own bindings, even where the two shaders use the same names: the fragment shader does not see the vertex shader's bindings, and the reverse. A shader that needs more than 8 storage buffers is an error that names them (E0602). The compiler does not combine buffers: putting data in fewer buffers is the program's choice.

**Groups:** a borrow struct (§6.6) is one parameter of a shader, a kernel or any GPU function. Its fields are:

- textures, depth textures, 3D textures and samplers, borrowed;
- buffers' spans (`GpuSpan<T>`), to read;
- `GpuData` values;
- other groups.

An entry point binds each texture, sampler and span in a group as it would bind a parameter of its own, named by its path: `lit.probes` binds `lit_probes`. It puts each value in its uniform block. WebGPU's limits on what a stage binds count a group's fields: 16 textures, 16 samplers, 8 storage buffers and 4 storage textures. E0602 names the first field past a limit.

- CPU code builds a group once and binds it to each draw or dispatch that takes it: `draw(pass, cover, shade.bind(lit), vertices: 3)`.
- GPU code passes a group on whole, passes its fields on, and builds groups of its own.
- A group has no value on the GPU. So GPU code cannot choose between two groups at run time, store one or capture one in a closure (E0702).
- A group holds only what the GPU reads. In an entry point's group, a field the GPU writes (`mut`, `GpuSpanMut<T>`, `Texels`, `Slots<T>`) or a run of the CPU's memory is E0602. A value that is not `GpuData` is E0604.
- A group draws the same result, to the bit, as its fields bound one by one.

### 18.7 Commands: dispatches and draws

CPU code records GPU work with explicit commands, which bind entry points to their arguments. GPU calls carry the `host` effect.

- `write(mut buf, at, values)` copies values into a buffer.
- `dispatch(kernel.bind(a, b), groups: n)` records a dispatch. `over: n` in place of `groups:` dispatches over a domain (below).
- `draw(pass, vertex.bind(...), fragment.bind(...), vertices: n)` records a draw into an open pass: `let screen = begin_screen_pass(clear: ...)`, then the draws, then `screen.present()`. §18.8 gives the other passes.
- Writes and dispatches take effect in the order they are recorded, with no barriers between dispatches.
- A kernel or shader cannot be called directly (E0606).

**An entry point is bound to its arguments.** `k.bind(...)` takes every parameter except the GPU's builtins and a fragment shader's vertex output. It matches arguments as a call does: positional, then named, then defaults. A missing argument is E0303, and an unknown name is E0302. Each shader in a draw has its own arguments. A shader that takes no arguments is named alone: `draw(screen, cover, shade.bind(scene, field), vertices: 3)`. `GpuData` arguments travel as one uniform block, and `GpuBuffer`s as buffers.

**Bound entry points are values.** Outside a command, `k.bind(...)` is a value of `k`'s bound type: a borrow struct (§6.6) that the compiler makes for each entry point. Its fields hold the arguments, matched as `bind`'s are in a command.

- A buffer given for a `[T]` is held as a `GpuSpan<T>` of all of it (`items()`).
- A buffer given `mut` for a `mut Slots<T>` is held as a `GpuSpanMut<T>` (`items_mut()`).
- What the GPU writes through a container (an `AppendBuffer<T>`, a texture for `Texels<F>`) is held `mut`.
- A texture, a sampler, and a value that is not `Copy` are borrowed. A value that is not `Copy` takes a place (E0508).

`dispatch` and `draw` take the value where they take an entry point: `let k = fill.bind(0.5, mut out)`, then `dispatch(k, over: n)`, as often as needed. The value holds its loans while it lives, so binding its buffer to another command in that time is E0506.

A function generic over a bound entry point takes it by its stage's trait:

- `K: Kernel` for a kernel;
- `V: VertexShader<O>` for a vertex shader whose output is `O`;
- `F: FragmentShader<I>` for a fragment shader whose input from the vertex shader is `I`. A fragment shader that takes no input has the trait for every `I`.

So `fn fullscreen<P: RenderPass, F: FragmentShader<ClipPosition>>(pass: P, fs: F) { draw(pass, cover.bind(z: 0.5), fs, vertices: 3) }` draws any fragment shader that takes nothing from its vertex shader, into any pass.

- A draw's shaders must agree on what passes between them (E0603).
- A value must be of the command's stage (E0603).
- Each instance records its own pipeline, as named entry points do. Lowering checks what only the instance knows: a kernel with workgroup memory dispatched `over:` a domain through a generic function is E0603.
- Only bound entry points have these traits. An `impl` of one is E0415.
- Bound entry points recorded by generic functions draw the same result, to the bit, as named entry points.

**Dispatch over a domain:** `dispatch(k.bind(...), over: n)` runs a kernel once for each element of a domain. The domain is one of:

- a count, a `u32`;
- a size, a `(u32, u32)` or `(u32, u32, u32)`;
- a texture's texels: a `Texture<F>`, `Texture3d<F>` or `DepthTexture`. The host reads the texture's size before the dispatch borrows its arguments, so a kernel can cover the texture it writes.

The compiler sizes the workgroup counts from the kernel's `@compute` size. An invocation past the domain returns at once, so the kernel checks no bounds of its own. The domain travels in the uniform block, so the same kernel dispatched by `groups:` is a pipeline of its own. Each dispatch covers its domain exactly, in both hosts.

- A dispatch takes `over:` or `groups:`. Both is E0304; neither is E0603.
- A kernel that shares workgroup memory is not dispatched over a domain (E0603). Its invocations past the domain must still reach its barriers, and the kernel decides what they add to the memory. So it is dispatched by `groups:` and checks its `GlobalId` itself.

**Indirect work:** a command can take its counts from a buffer, so GPU work can size later GPU work.

- `dispatch(k.bind(...), groups: buf)` takes its three group counts from a buffer.
- `draw(pass, vs, fs.bind(...), indirect: buf)` takes its vertex count, instance count, first vertex and first instance from a buffer.

The buffer is a `GpuBuffer` or `GpuSpan` of the command's typed arguments. Each is a struct of its words, which kernels write whole:

- `DispatchArgs` for a dispatch;
- `DrawArgs` for a draw;
- `DrawIndexedArgs` for an indexed draw.

Another command's arguments are E0603. A buffer of structs that hold arguments is read in place. `buf.at(i)` is a `GpuField<S, S>`: element `i`, where it is. A field of it is a `GpuField` too (`lists.at(i).cards`, a `GpuField<S, DrawIndexedArgs>`), which a command takes as its arguments. A `GpuField` borrows its buffer, as a span does. Only CPU code names one (E0607 in GPU code). The arguments' words as `u32`s still serve: an `AppendBuffer`'s count is a dispatch's three counts. The command reads the buffer, so binding it `mut` in the same command is rejected (§6.5).

**Indexed draws and draw state:** `draw(pass, vs, fs, indices: buf, indirect: counts)` draws the triangles that a `GpuBuffer<u32>` of indices names. It takes its index count, instance count, first index, base vertex and first instance from `counts`. Three arguments set the draw's state:

- `cull:` is `Cull::None` (the default), `Front` or `Back`.
- `depth_bias:` is `DepthBias { constant, slope, clamp }`, with none by default.
- `depth:` is `Depth { compare, write }`. A fragment is drawn where its depth `compare`s with the target's as `compare` says: `Less`, `LessEqual`, `Equal`, `NotEqual`, `Greater`, `GreaterEqual`, `Always` or `Never`. If `write` is true, the fragment keeps its depth there. The default is `Less`, with writing.

The draw state is part of the draw's pipeline, which the host makes when the program loads. So each state is a build-time constant, written at the draw or in a `const` it names (E0603 otherwise). For example, a card's shading after its depth prepass draws with `depth: Depth { compare: Compare::Equal, write: false }`.

### 18.8 Textures, formats and passes

Textures and samplers are owned GPU values that shaders read and passes draw into; a texture's format is a type.

#### Making textures and samplers

- `texture::<F>(width, height)` makes a colour texture of format `F`.
- `writable_texture::<F>(width, height)` makes a colour texture that can also be a kernel's `Texels<F>`. The hosts give a texture storage usage only then, because storage can cost a drawn texture its compression.
- `depth_texture(width, height)` makes a depth texture.
- `texture3d::<F>(width, height, depth)` makes a 3D texture, each side 1 to 2048 texels.
- `sampler(filter, address)` and `comparison_sampler(compare)` make the ways to read them.

Each is an owned value, released when dropped, like a buffer. `tex.write(x, y, width, height, bytes)` copies texels in. A shader takes textures and samplers as parameters, and CPU code passes them by name. CPU code cannot read texels (E0607).

#### Reading textures

- `tex.sample(s, uv)` and `depth.sample_compare(s, uv, reference)` pick their detail from neighbouring pixels. So they are for fragment shaders, in uniform control flow, as derivatives are (E0607, E0608).
- `sample_level`, `sample_compare_level`, `load`, `width()` and `height()` work in any GPU code.
- Only kernels write a 3D texture's texels (`Texels3d<F>`). Shaders read it with `sample_level(s, uvw)` and `load(x, y, z)`, in any GPU code, and its size with `width()`, `height()` and `depth()`. A 3D texture is never a pass's target.

**Formats are types.** A colour texture is a `Texture<F>` (and a `Texture3d<F>`, a `Texels<F>`), with `F` its format:

- `Rgba8`: four 8-bit channels, read as floats in [0, 1];
- `Rgba16Float`, `R16Float`, `Rg16Float` and `R32Float`;
- `R32Uint`: integer texels, whole to 32 bits, drawn into by a fragment shader that returns a `u32`.

Each format says what a texel is in GPU code (`F::Texel`): a `vec4`, a `vec2`, an `f32` or a `u32`. `load`, `sample` and `store` give and take that type. The channels a format lacks are not written, and GPU code never pads them. Code generic over formats reads a texel as four channels with `F::rgba(texel)`: a channel the format lacks reads 0, and a missing alpha reads 1, as WGSL reads them.

What else a format can do is a trait it has or lacks, as WebGPU's core formats allow:

- `Filtered` formats are sampled (`sample`, `sample_level`). `R32Float` and `R32Uint` are not; GPU code `load`s them.
- `Storage` formats are written by kernels (`writable_texture`, `texture3d`, `Texels`). `R16Float` and `Rg16Float` are not.
- `Target` formats are drawn into: all six formats.

A missing trait is E0400 where it is needed. Each binding in the manifest names its texture's format, and the hosts check that the bound texture has that format. Each format holds what was written, in both hosts.

**Passes:** these functions start a pass, as `begin_screen_pass` does (§18.7):

- `begin_pass(mut tex)`;
- `begin_pass_with_depth(mut tex, mut depth)`;
- `begin_depth_pass(mut depth)`;
- `begin_screen_pass_with_depth(mut depth)`.

Each gives a value that borrows its targets until `end()` (or `present()`), so the pass's draws cannot bind them (E0506). A frame can have any number of passes. A pass with depth keeps the nearest fragment, unless a draw's depth state says otherwise.

**A draw takes its pass:** `draw(pass, vs, fs, ...)`. The pass is the value its `begin_` function gave (`Pass<F>`, `DepthPass<F>`, `DepthOnlyPass`, `ScreenPass`, `ScreenDepthPass`), or a parameter bounded by `std::gpu::RenderPass`, which only those types have (an `impl` is E0415).

- The pass's type says what the draw draws into. So the compiler knows each render pipeline's targets: the manifest's `targets`, every pass the pipeline is drawn in.
- The hosts make each pipeline while the program loads, not at its first draw.
- The hosts refuse a draw into a pass that is not one of its pipeline's targets.

**Joined passes:** a pass that keeps both its targets may `join` the pass before it: `begin_pass_with_depth(..., join: true)`. If that pass ended right before it, on the same targets, the host may run the two as one render pass. A tile-based GPU then does not store the targets and load them again between the two. The serial timing mode still times each pass alone. A light pass is worth joining; two heavy passes are not, because a pass's geometry is binned before its tiles are drawn.

### 18.9 Pipelines

A pipeline is an entry point compiled for one set of type arguments: a generic kernel or shader has a pipeline for each instantiation that the program records (§7), and a field's numbers travel as data, so one pipeline serves every field of one structure.

- **The pipeline-count query** reports how many instantiations each GPU entry point has, and why. `wrela pipelines <package>` lists each entry point's pipelines, their type arguments, and where CPU code dispatches or draws each. `--json` gives the same list for tools.
- **A shipped build keeps a budget for its shaders:** at most 256 KiB of WGSL for each pipeline. A browser compiles each pipeline while the scene loads, and the GPU's compile time grows faster than the shader's size.
  - A release build over the budget is E0707, at the command that records the large pipeline.
  - The budget isn't checked for a test build, a debug build or a lifted build.
  - A pipeline is in the build only if a command in the optimized CPU code records it. A pipeline behind a constant branch, such as `if test_build() && counting`, leaves the build with the code that records it. (The `&&` holds its value in a local, which constant folding sees through.)
- **The number of pipelines in a build isn't limited.** The time to create them counts in the cold start's budget, measured cold in Chrome.
- **The hosts start the program while they create its pipelines,** the biggest shaders first. A command that needs a pipeline not yet created waits for it, and so do the commands after it.

```wrela
/// Generic over any field; monomorphized per concrete field type.
@compute(64)
fn cull<F: Surface>(field: F, grid: Grid, live: mut Slots<u32>, id: GlobalId) {
    let b = grid.block(id.x)
    let r = field.interval(Box3 { lo: b.lo, hi: b.hi })   // `interval` is derived (§14)
    live[id] = if r.contains(0.0) { 1 } else { 0 }
}
```

### 18.10 Timing and limits

The hosts can time each pass and dispatch on the GPU, and they open the GPU with its own limits.

**Labels:** `std::gpu::label(name)` names the passes and dispatches after it in the hosts' timings, up to the next label or the frame's end. So a system's helper dispatches are timed with it. Before a frame's first label, a pass is named `pass` (`screen pass` on the screen), and a dispatch is named by its kernel's pipeline.

**Timestamps:** when asked, both hosts time each pass and dispatch with GPU timestamps: its name, its start and its end. Test mode's `timestamps` and the native host's `Options::timestamps` ask for them.

- A frame's GPU time is its span, from its first pass's start to its last pass's end.
- On Apple GPUs passes overlap, so one pass's start-to-end span holds other passes' work.
- The serial mode (`timestamps=2`, `Options::serial`) runs each pass and dispatch alone and waits for the GPU between them, so each time is its own. In the native host, a serial time is within 1% of the same pass drawn alone in the same mode. In Chrome, the wait goes through the GPU process and lets the GPU's clock fall, so serial times run slow by an amount that changes from run to run.
- A frame's passes run in order after the frame, while the host records the next frame.
- A GPU sets its clocks by its load, so a light frame paced at 60 Hz times slower than the same frame run back to back.

**The GPU's limits:** both hosts open the device with the adapter's own limits, which are at least WebGPU's defaults, and check commands against them.

- `std::gpu::limits()` gives the limits, so a program can make a texture or a buffer as large as the GPU allows. It has the `nondet` effect, because another GPU gives other numbers.
- Pipelines are compiled for the default limits.

## 19. Rooms

A room is a server's ordered log of what its peers propose: `std::net` gives the log, and the server host keeps it durably and serves it over the network.

### 19.1 The ordered log

A server orders what its peers propose (`std::net`). The room's state changes only by a reducer of what the room commits: the room's state type's `Reduce<T>`.

- `check` says whether the reducer takes a proposal, and changes nothing.
- `apply` applies a proposal that the reducer took.
- The reducer is checked as `@deterministic` code is, wherever it is instantiated (§12).

Every peer applies the committed entries in the room's order, with the same code, so its committed state is the room's. `Log<S, T>` is a peer's replica. It shows the peer's own proposals at once (its view), and rebases them on what the room commits.

**Each proposal is applied exactly once,** whatever is lost:

- A proposal's id is a peer's token's id, its session, and a counter that goes up by one.
- The room keeps each session's next counter, and the outcomes a peer has not acknowledged: at most `WINDOW` (256) past the last one it acknowledged.
- A room's host keeps each step's records durably before it sends what follows them. So a room rebuilt from its records (`Room::replay`) is the same room.
- A seeded model of a network checks this over 10,000 runs, with connections that drop, a server that crashes and loses what it had not kept, and a room that compacts. Every peer ends with the room's state and every proposal the reducer took, once.

**Who may propose:** a room takes a proposal only from its peer's connection, whose hello carried the peer's token. The room tags that connection with the peer (`platform::net::tag`), so a room that its host starts again knows it. A peer's id, which every entry shows, proves nothing. A connection says hello once; the room refuses a second hello (`HELLO_AGAIN`).

**Catching up and compaction:**

- A peer that rejoins more than `CATCH_UP` (128) entries behind gets the state, not the entries.
- A room compacts once its journal holds `COMPACT_AFTER` (1024) records past its snapshot, refusals and joins included.
- The messages between peers and the room are `Pack`ed (`Up<T>`, `Down<S, T>`).

### 19.2 Durable rooms

The server host keeps each room durably (`wrela_host::rooms`). `platform::net`'s `serve_room` runs a room for `serve`. The host writes each call's records (`keep`, `compact`) to the room's journal, flushed to disk, before it sends anything the call sent. A room that starts reads its journal back first.

- The journal's header names its format, the server build's WASM hash and the room. The host does not read a journal of another build or room.
- Each record carries a CRC-32C. A record cut short by a crash ends the journal.
- A compaction writes a snapshot beside the journal and renames it into place. A compaction made while the journal is read back keeps, after the snapshot, the records not yet read.
- A room's journal is made with its first record. One room host at a time in a process holds it; another host of the same room waits for it to close.
- A data directory is for one server process at a time.

**Calls and failures:** the host gives each call a call's worth of what waits (`PAYLOAD_PER_CALL` bytes of messages), and calls again until all is read. A `serve` that returns without reading everything it was given fails, as the program's own trap. The host takes a failed call again, one event at a time, on a fresh instance:

- The message that the program traps on, run alone, goes back to the room poisoned. The room refuses it durably without running it (`TRAPPED`).
- An event that is past the host's limits, run alone, keeps nothing. It waits, with the events after it, for the room to try again. The room tries at each wake, and every 250 ms in `wrela-serve`.
- After three failures in a row, the host lets the room's peers go, to come back later.

A journal that the program traps on, or a poisoned message that it traps on, quarantines the room: the host drops what waits, and the room's connections close.

### 19.3 Rooms over the network

`wrela-serve <build>` runs a build's server rooms over WebTransport.

- A connection is a session at `/room/<name>?pair=<id>`. The name is 1 to 64 of a-z, 0-9, `-` and `_`. The id is the client's pair id, 16 hex digits.
- A session has one bidirectional stream. Each message on it is a frame: its length (a varint) and its bytes. A frame is at most 64 KiB, and 64 bytes less from a client, so a room's answer to a message fits a frame.
- A room's time (`serve`'s `time`) counts from the server's start, so a room that stops and starts again continues from that time.

**Bounds** (`Bounds`): what a server holds is bounded.

- Sessions open or opening: 256.
- Rooms running: 64. A room with no connections for 30 s stops, and starts again from its journal.
- Bytes queued: each connection may hold 128 KiB each way, whatever the others hold. Past that, it draws on 64 MiB that every queue shares. Each message counts as 64 bytes more than its size.
- The server closes a connection with more than 1 MiB waiting for its room, more than 16 MiB waiting for the connection, or a write that takes 10 s. Its peer comes back.
- A call's network commands hold at most `Limits::commands` (16 MiB) bytes.
- The browser closes a connection with more than 1 MiB waiting to be written, or whose messages would hold more than 16 MiB waiting for the program.
- A room's state must pack into one host request (1 MiB) to compact, and into one frame (64 KiB) to be sent to a peer that is far behind. Larger states are not split.

**Clients:** a client opens connection `conn` to a room with `platform::net::open(conn, room)` (`@client`), and uses `send` and `close` as a server does. Each connection it opens gives one `Connection` event when it is open, and exactly one when it is closed, whatever closes it.

#### Finding and running the server

- The browser host finds the server in `net.json` beside the build: its address and, for a development certificate, the certificate's hash, which the browser trusts in place of a certificate authority's signature. `wrela-serve --dev` writes both.
- Each server build's rooms are kept apart (`<data>/<build hash>/`), because only the build that wrote a journal reads it. A server given a new build closes its rooms' connections, and their peers come back to the new build's rooms.
- `wrela run` serves a program's rooms beside its page (`--rooms-port`, 4433 by default), writes each build's `net.json`, and gives the rooms each new server build.

## 20. The stdlib boundary

The compiler knows the language, a closed list of std items and its targets; the rest of std, fields included, is ordinary Wrela code.

### 20.1 What the compiler knows

- **The compiler knows three things:** the language, a closed list of std items, and the execution targets (§1.1).
- **The closed list** is every std item that the compiler knows by its path, and nothing else. It holds the traits that the compiler derives or checks by structure, the types that its code has a shape for, the GPU, IO and memory operations that it lowers itself (`@intrinsic` in std), and the transcendentals that it compiles from std's code. A program cannot add to the list. `Plain`, `StateHash`, `Serialize`, `Blend` and `Surface` are not on it: they are ordinary std traits, `@fieldwise` or written by hand. The list, by module:
  - `std::prelude`: `Option`, `Result`, `Copy`, `Clone`, `GpuData`, `Fieldless`, `Packed`, `Bits`, `packed_fit`, `is_discriminant`, `of_discriminant`, `packed_of`
  - `std::gpu`: `GpuBuffer`, `Slots`, `GlobalId`, `LocalId`, `WorkgroupId`, `VertexIndex`, `InstanceIndex`, `FragCoord`, `ClipPosition`, `Flat`, `Over`, `WithDepth`, `Cull`, `DepthBias`, `Depth`, `dispatch`, `draw`, `buffer`, `write_buffer`, `destroy_buffer`, `copy_buffer`, `GpuSpan`, `GpuSpanMut`, `GpuField`, `field_at`, `begin_screen_pass_command`, `RenderPass`, `Pass`, `DepthPass`, `DepthOnlyPass`, `ScreenPass`, `ScreenDepthPass`, `present`, `Texture`, `DepthTexture`, `Sampler`, `ComparisonSampler`, `create_texture`, `write_texture_rows`, `destroy_texture`, `create_sampler`, `destroy_sampler`, `begin_pass_command`, `end_pass_command`, `label_command`, `texture_sample`, `texture_sample_level`, `texture_sample_compare`, `texture_sample_compare_level`, `texture_load`, `depth_load`, `Texture3d`, `Rgba8`, `Rgba16Float`, `R16Float`, `Rg16Float`, `R32Float`, `R32Uint`, `create_texture_3d`, `texture3d_sample_level`, `texture3d_load`, `texture_width`, `texture_height`, `texture_depth`, `span_len`, `read_buffer_command`, `limit`, `Shared`, `Atomics`, `Append`, `AtomicMap`, `AppendBuffer`, `AtomicMapBuffer`, `local_index`, `workgroup_invocations`, `shared_get`, `shared_set`, `workgroup_barrier`, `discard`, `atomic_len`, `atomic_load`, `atomic_store`, `atomic_add`, `atomic_sub`, `atomic_min`, `atomic_max`, `atomic_and`, `atomic_or`, `atomic_xor`, `atomic_exchange`, `atomic_compare_exchange`, `append_push`, `One`, `one_get`, `one_set`, `DrawArgs`, `DrawIndexedArgs`, `DispatchArgs`, `Texels`, `texels_store`, `Texels3d`, `texels3d_store`, `Kernel`, `VertexShader`, `FragmentShader`
  - `std::host`: `host_command`
  - `std::io`: `next_request`, `request_status`, `storage_read_command`, `storage_write_command`, `fetch_command`, `print_command`, `post_command`, `Shipped`
  - `std::mem`: `take_answer`, `take_input`, `take_payload`, `keep_bytes`, `kept_bytes`, `read_clock`, `Drop`, `size_of`, `align_of`, `needs_drop`, `read`, `zeroed`, `write`, `drop_at`, `at`, `at_mut`, `at_mut_pair`, `heap_base`, `memory_pages`, `memory_grow`, `load_u32`, `store_u32`, `load_u8`, `store_u8`, `copy`, `fill`, `compare_swap`, `atomic_add`, `atomic_load`, `atomic_store`, `wait`, `notify`, `run_task`, `task`, `thread_block`, `wait_for`, `abort`, `debug_build`, `test_build`
  - `std::derive`: `Interval`, `Domain`, `gradient`, `value_and_gradient`, `value_gradient_with`, `interval`, `filtered`
  - `std::math`: `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `sinh`, `cosh`, `tanh`, `exp`, `exp2`, `log`, `log2`, `pow`
  - `std::par`: `par_each`, `par_each_chunk`, `par_map_reduce`, `par_map_reduce_chunk`, `run_chunks`
  - `std::audio`: `start_voice`
  - `std::time`: `phase_command`
  - `std::tick`: `start_ticker`
  - `std::reload`: `prepare_command`, `unprepare_command`, `hand_over_command`
  - `std::collections`: `Vec`, `Bounded`, `Box`, `swap`, `replace`
  - `std::string`: `Text`, `Bytes`, `String`, `str_addr`, `str_len`, `str_part`
  - `std::cmp`: `Eq`, `Ord`, `Ordering`
  - `std::fmt`: `Format`, `Spec`, `assert_failed1`, `assert_failed2`
  - `std::arena`: `Arena`, `Handle`
  - `std::lift`: `gradient`, `reads`, `literal_count`, `literal_value`, `literal_built_value`, `set_literal`, `literal_generation`, `literal_source`, `lifted_files`, `lifted_file`
  - `std::job`: `Job`, `Step`
- **Each std module passes an admission test:** it makes sense in a program that is not a game. The compiler knows nothing about the engine; a feature that only makes sense in a game belongs in the engine. Acoustics, creatures, terrain and timelines are engine code, in the `engine` package, not in std.
- **std is written in Wrela** with a small unsafe core (§6.12). The core is these files, and only they contain `unsafe`: `std::mem` (raw memory), `std::alloc` (the allocator), `std::collections` (`Vec` and `Box`), `std::string`, `std::par` (the helpers and jobs), `std::audio` (the voice and its ring), `std::tick` (the ticker), `std::handoff` (the triple buffer) and `std::time` (the clock).
- **The core states what inference cannot see,** with two attributes. Only the core's files may use them; the compiler rejects them anywhere else (E0204).
  - `@effects(...)` names the effects of a std function that come from another thread or from the host through memory. `Latest::read` and `Job::done` are `@effects(nondet)`; starting the voice and starting the ticker are `@effects(io)`. This is the one place where effects are written; everywhere else the compiler infers them (§8.2). A GPU command, request or host import that the compiler lowers itself has the effects of what it lowers to.
  - `@thread_entry` marks a std function that a host calls on a thread of its own (the `memory` of `wrela_abi`). Its first parameter is the thread's number, which gives the thread its stack and its block; its other parameters are `u32`s. When a program uses the function's module, the function is exported as `__` followed by its name. The thread entries are `std::par`'s `worker` (each helper), `std::audio`'s `audio` (the voice's quanta) and `std::tick`'s `tick` (each tick). The compiler names none of them.
- **A task is a function that a module calls by number,** with a chunk and a context's address, as `run_task` and the thread entries do. `std::mem::task(run)` gives a task's number. The voice and the ticker are tasks; the chunks of a parallel job are tasks that the compiler makes from their closures.

### 20.2 The module map

`wrela doc std` lists std's modules, and `wrela doc <item>` shows any item.

| Module | Contents |
|---|---|
| `std::prelude` | In scope in every module: `Option`, `Result`, `Copy`, `Clone`, `GpuData`, `Plain`, and, from their modules, `Eq`, `Ord`, `Ordering`, `Vec`, `Bounded`, `Box`, `swap`, `replace`, `String`, `Text`, `Bytes`, `Arena`, `Handle`, `SortedMap` and `Quat` |
| `std::cmp` | `Eq`, `Ord` and `Ordering`: `==` and `<` for the types that declare them (§3.4) |
| `std::transform` | Matrices for skeletons, cameras and shadow maps, on the CPU and the GPU, in WebGPU's conventions (right-handed, +y up, column vectors, depth 0 to 1): `identity`, `translate`, `scale`, `rotate_x`, `rotate_y`, `rotate_z`, `rotate(axis, angle)`, `from_mat3`, `look_at(eye, at, up)`, `perspective`, `ortho` |
| `std::collections` | `Vec` and `Box`. `Bounded<T, N>`: at most `N` values held in place (an array and a count of the values in use). It is `Copy`, and it is `Plain` and `GpuData` where `T` is, so it can be in a snapshot or on the GPU, where a `Vec` cannot. It has `push`, `pop`, `remove`, `clear`, `len`, `is_full` and `Bounded::from(xs)`; `b[i]` projects an element in use (an index past `len()` panics on the CPU), `for x in b` walks the elements in use, and it hashes, saves and compares only those. `swap` and `replace` move values out of places. `Vec::filled(n, value)`, and `Vec::from_fn(n, make)` for a `T` that is not `Clone`. `sort_by` is a stable O(n log n) sort of a run of `Copy` values; `Vec::sort_by` sorts any `Vec`, and `Vec::sort_by_in` keeps its working space in a `Vec<u8>`, so a tick that sorts allocates nothing once that space has grown (§16.2) |
| `std::string` | `String`; `Text`, text known at build time; `Bytes`, bytes known at build time, with `at(i)` and little-endian reads at any byte (`u16_le`, `u32_le`, `i32_le`, `f32_le`, `u64_le`, `f64_le`) |
| `std::fmt` | `Format`, which `f"…"` calls, and `Spec`, a hole's format spec; `json_text` and `json_number`, which write text and floats as JSON (a NaN or an infinity is `null`) |
| `std::arena` | `Arena<T>` and `Handle<T>` (§6.8); `SortedMap<K: Ord, V>`, which iterates in key order, so `@deterministic` code may use it |
| `std::units` | Suffix constants in SI: `km`, `m`, `cm`, `mm`, `um`, `t`, `kg`, `g`, `mg`, `h`, `s`, `ms`, `us`, `rad`, `deg`, `Hz`, `kHz`, `N`, `J`, `W`, `Pa`, `kPa` (§5) |
| `std::math` | The transcendentals for CPU code, compiled to WASM (§11.1); `PI`, `TAU` and `is_finite` |
| `std::hash` | `StateHash` (`@fieldwise`), `Hasher`, `state_hash` (§6.11) |
| `std::serialize` | `Serialize` (`@fieldwise`), `Writer`, `Reader`, `save`, `load`, `LoadError` (§6.11) |
| `std::field` | `Surface`, `Field<C>`, `Noise`, `Blend`, `Color`, `UnitVec3`, `Cat<T>`. Primitives: `sphere`, `ellipsoid`, `round_cone`, `cuboid`, `half_space`. Forms: `oval`, `tube`. Combinators: `union`, `smooth_union`, `intersect`, `translate`, `displace`, `with`, and `union` and `smooth_union` of an array. Noise: `value_noise`, `fbm`. Provenance: `parts`, `part_at`, `part_distance`, `part_name`, `part_offset`, `named`, `mirror_x`, `PartName`, `part_share`. Certified checks: `certify_positive`, `certify_clear`, `certify_apart`, `Certified`. Measures: `hit`, `top`, `bottom`, `width`, `front`, `back`, `farthest`, `part_named` (§23.7) |
| `std::quat` | `Quat`: rotations as unit quaternions, on the CPU and the GPU |
| `std::derive` | `gradient`, `value_and_gradient`, `interval`, and the boxes that they range over (§14.1). Each box has `sample(seed, i)`: points spread evenly through it, to check a property at many points (§13.2) |
| `std::stage` | `Tape`, `Op`, `Reg` and `interpret`: fields built at run time (§14.4) |
| `std::gpu` | Buffers (`GpuBuffer<T>`, `GpuSpan<T>`, `buffer`, `write`, `copy`, `read`), textures and samplers, passes, `dispatch` and `draw`, kernel outputs (`Slots<T>`, `Shared<T, N>`, `Atomics<T>`, `Append<T>`, `AtomicMap`, `Texels`, `Texels3d`), typed builtins, lossy encodings, `limits()` (§18) |
| `std::io` | `load`, `store`, `fetch` and `post`, polled through `Pending<T>`; `print` (§17.1, §17.3) |
| `std::input` | `events`, `Event`, `Key`, `Button`, `Mods`, `payload` (a message's bytes), and `Input`, the keys and buttons held (§17.2) |
| `std::semantics` | A program's semantic tree, which a host shows as real elements: `Node`, `Role`, `Field`, `FieldKind`; `structure`, `geometry` and `clip`, the tree's bytes; `changes`, the part of the tree to send since `Sent`; `steps`, a slider's step from an `Assist`; `FieldState` and `read_field_state`, a field's text as the host has it (§17.2) |
| `std::pack` | `Pack`: values in as few bits as they allow, read back exactly (§6.11) |
| `std::net` | `Room`, `Log`, `Reduce`: a room's ordered log, delivered exactly once (§19) |
| `std::time` | `now`, seconds on the host's clock, and `since`, to time work while the program runs; both are `nondet`, so not in `@deterministic` code, a constant or a test (§17.3). `phase(name)` marks a phase of the thread's work, which the host times and reports (`trace`); it is available everywhere except GPU, `@audio` and derived code |
| `std::lift` | A lifted build's literals: `Literal`, `count`, `literal`, their values, sources and files, `set`, `generation` and `Watch`; `gradient` by literals, and `reads`; the spec types `Checks`, `Loss` and `Report`; `Minimize`, which makes a function of some literals as small as it can (BFGS, then rounding to the literals' decimals) (§23.1, §23.7) |
| `std::reload` | Hot reload: `keep` gives bytes to the build that replaces this one, and that build's `kept` returns them. One build hands its place to another with `prepare`, `unprepare` and `hand_over` (§15.4, §23.2) |
| `std::job` | Work over several frames: `Job<f>`, a `@job fn` started and resumed to each `yield`, and `Step`, what a `resume` did (§16.3) |
| `std::cache` | Results cooked from inputs, and cooked again when the inputs change: `Stale<K>`, what a result was cooked from, and `Cached<K, V>`, a value kept until its key changes (§16.5) |
| `std::audio` | `play`, the program's voice, and `ring`, a queue to it (§16.4) |
| `std::par` | The workers' protocol behind `par_each_mut` and `par_map_reduce` (§16.1); nothing public |
| `std::mem`, `std::alloc` | The unsafe core: raw memory and the allocator (§6.12) |

### 20.3 Fields are std code

A field is ordinary std code: its trait, its primitives and its combinators are in `std::field`, and a program adds its own the same way.

- **The field trait is `std::field::Surface`:** a signed distance (`distance(self, p: vec3) -> f32`), with `gradient`, `sample` and `interval` derived. std gives primitives (`sphere`, `ellipsoid`, `round_cone`, `half_space`), combinators (`union`, `smooth_union`, `intersect`, `translate`, `displace`) and noise (`value_noise`, `fbm`).
- **A new primitive is a type that implements `Surface`.** It must be `Copy` and `GpuData`, because it travels to the GPU as data. The combinators and derived methods come with it:

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
- **A closure of a point is a surface.** `let both = |p| min(ball(p), slab(p))` is a `Surface`, written where it is used, with the derived gradient and interval and every combinator (std's `impl<F: Copy + GpuData + fn(vec3) -> f32> Surface for F`). A closure states no Lipschitz bound (below): its `lipschitz` is infinite, and a field with an unbounded part is unbounded. So a field that is sphere traced is built from primitives, which state their bounds.
- **A field returns a distance plus channels.** A channel struct opts in to `Blend`, a fieldwise trait, so each member's type decides how it blends: `Color` in linear space, `f32` linearly, `UnitVec3` renormalized, and `Cat<T>` from the winner.
- **A field's step safety is a method, not a kind or a fact that the compiler knows** (§14.5, §21.4). `Surface`'s `lipschitz(near)` bounds the field's gradient where |distance| < `near`:

  ```wrela
  use std::field::{Noise, Surface}

  /// A bump on a sphere: its bound is the sphere's, plus the bump's steepness.
  struct Bumpy<N>: Copy + GpuData {
      noise: N,
      amp: f32,
  }

  impl<N: Noise> Surface for Bumpy<N> {
      fn distance(self, p: vec3) -> f32 {
          length(p) - 1.0 + self.amp * self.noise.value(p)
      }

      fn lipschitz(self, near: f32) -> f32 {
          1.0 + self.amp * self.noise.lipschitz()
      }
  }
  ```

  - Primitives state their bound, and combinators compose it in ordinary code: `Displace`'s bound is its surface's plus its amplitude times its noise's. The bound is data, computed for each individual.
  - `.to_bound()` divides a field by its bound. An unbounded field gives 0, which no tracer can step by. Two examples are a closure of a point, whose `lipschitz` is infinite by default, and an ellipsoid with its centre in scope.
  - Debug builds and tests check each stated bound against the derived local bound: the `interval` of the `gradient`.
- **Combinators are methods:** `a.smooth_union(b, k: 15cm)`. Over a fixed array of parts of one type, they are n-ary: `legs.union()` and `legs.smooth_union(k: 6cm)` fold the parts in order, as the binary combinators chained would. Fields have no operator overloading; vectors and units do have operators.
- **Fields built at run time** from an unbounded space use `stage::interpret` (§14.4).

## 21. What Wrela leaves out

Wrela has no references as types, null, garbage collector, `async` or macros; for each feature it leaves out, a program uses something else, given here.

### 21.1 Memory and types

- **References as types,** and with them lifetime parameters, variance, `'static` and higher-ranked bounds. Use parameter modes (§6.2) and projections (§6.4).
- **Projections inside other types:** general non-escaping structs, projection types as type arguments, iterator types and adapter chains. The one exception is a borrow struct, whose fields are fixed once it is built (§6.6). Loops walk runs and arenas; queries take closures or return a `Vec` (§6.6).
- **`Span<T>`.** Runs, `[T]`, are the one view type (§4.4).
- **Implicit moves out of named places.** A call, binding or assignment writes each move as `take` (§6.1).
- **`Cell` and `RefCell`** (interior mutability), and **`Rc` and `Arc`** in ordinary code (§6.5).
- **Mutable globals** (§6.5).
- **Null.** Use `Option` (§6.1).
- **A garbage collector.** Memory is values, arenas and handles (§6).
- **Regions, `Relocatable` and region-bound values.** Sim state is ordinary values: a snapshot is a clone, a checksum is the derived `StateHash`, and a save is the derived `Serialize` (§6.11).
- **User-defined destructors.** Only std's core defines them (§6.12).
- **Tuple structs.** A struct names its fields, and a pair is a tuple: `type P = (f32, f32)` (§4.4). The parser rejects a tuple struct and says so.
- **Unit types.** Unit suffixes are SI constants (§5).
- **Angle as its own dimension.** Angles are dimensionless, as in SI, so `sin` takes a plain number (§5).

### 21.2 Control flow and concurrency

- **Labelled `break`.** `break` leaves the innermost loop; to leave more, return from a function or set a flag (E0114).
- **Reverse ranges.** A range counts up: write `for k in 0..n` with `let i = n - 1 - k` (§2.2). The parser rejects `(0..n).rev()` and says so.
- **`async` and `await`, and `Pin`.** IO and readback are polled requests, and a request's handle is an ordinary value (§17.1).
- **`Sendable` and `Shareable`.** Without interior mutability or shared ownership, nothing needs them (§16.1).
- **A general `schedule` construct** for a function's evaluation. GPU work is kernels, dispatches and draws, written in the program (§18).

### 21.3 Traits, attributes and metaprogramming

- **`dyn Trait`.** Enums cover structure chosen from a finite set, generics cover static structure, and `stage::interpret` covers fields built at run time (§3.3, §14.4). `dyn` stays a reserved word, so the compiler can say what to use instead.
- **Auto traits.** A type declares every trait it has (§7.1), so no guarantee depends on an author who remembers to opt out.
- **Procedural macros.** Use fieldwise traits (§3.4).
- **A compile-time interpreter, reflection and `@comptime`.** Build-time constants run the program's own WASM, `embed` gives `Bytes`, and `@fieldwise` traits replace structural defaults (§3.4, §13). Types are never computed.
- **User-defined attributes.** The language defines the closed set of attributes (§9).
- **`@escaping`.** A closure's captures decide whether it can be stored (§6.7).
- **Stated effects at package boundaries.** The compiler infers effects everywhere (§8).

### 21.4 Numerics and fields

- **Fast-math on the CPU.** CPU code uses strict IEEE floats (§11).
- **Field kinds** `Exact`, `Bound` and `Lipschitz`. `Surface`'s `lipschitz` method states the bound (§20.3), and the derived interval of the gradient checks it.
- **Facts as compiler attributes,** `@assume` and `@assert`. A field's bound depends on data for each individual, so it's a std method (§14.5, §20.3). The compiler does only what it alone can do: it derives the bound that checks the stated one.

## 22. A complete program

This program defines a field, a tesseract at rest; samples it on a grid with a compute kernel; and each frame draws the field's normals, from its derived gradient, with a fragment shader. A test builds it, and the docs draw its field live. `examples/hello-field` is a fuller program, which sphere-traces a creature and lights it.

```wrela
use std::field::{Surface, cuboid, round_cone}
use std::gpu::{
    ClipPosition, FragCoord, GlobalId, GpuBuffer, Slots, VertexIndex, begin_screen_pass, buffer,
    dispatch, draw, present,
}

/// A cube's twelve edges, `t` thick: the cube, `h` from its centre to a face, with a hole cut
/// through it along each axis.
fn edges(h: f32, t: f32) -> Surface {
    cuboid(half: vec3(h))
        .smooth_subtract(cuboid(half: vec3(h - t, h - t, 2.0)), k: 0.01)
        .smooth_subtract(cuboid(half: vec3(h - t, 2.0, h - t)), k: 0.01)
        .smooth_subtract(cuboid(half: vec3(2.0, h - t, h - t)), k: 0.01)
}

/// Four rods, `r` thick, along a cube's diagonals from corner to corner.
fn struts(h: f32, r: f32) -> Surface {
    round_cone(vec3(-h), vec3(h), r_top: r, r_bottom: r)
        .union(round_cone(vec3(-h, -h, h), vec3(h, h, -h), r_top: r, r_bottom: r))
        .union(round_cone(vec3(-h, h, -h), vec3(h, -h, h), r_top: r, r_bottom: r))
        .union(round_cone(vec3(h, -h, -h), vec3(-h, h, h), r_top: r, r_bottom: r))
}

/// The field: a tesseract at rest, its far cell a cube `far` from its centre to a face inside its
/// near cell, each corner joined to its twin. Its structure is its type, and `far` is data, so
/// every size shares one pipeline. (In `main.wrela`, a `pub fn` is exported to the host, so these
/// are private.)
fn tesseract(far: f32) -> Surface {
    let joins = struts(h: 1.0, r: 0.035).smooth_subtract(cuboid(half: vec3(far)), k: 0.01)
    edges(h: 1.0, t: 0.07).union(edges(far, t: 0.05)).union(joins)
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
    dispatch(sample.bind(tesseract(0.42), grid, mut out), groups: 64)
    let screen = begin_screen_pass(clear: vec4(0.0, 0.0, 0.0, 1.0))
    draw(screen, cover, normals.bind(tesseract(0.42 + 0.05 * sin(time))), vertices: 3)
    screen.present()
}
```

## 23. Lifted builds, provenance and the tools

A person or an agent changes a field by its numbers while the program runs, with tools built for that. Three features make this a tool's job, not a guess:

- **Lifted builds:** a tool changes a number while the program runs.
- **Provenance:** the part of a field that decides a point, and the numbers that move the surface there.
- **`wrela edit`:** a change written back into the source, in the form the author wrote.

The lens (`wrela studio`) and the command-line tools for agents use all three.

```text
 source ──wrela build --lift──▶ program ──std::lift::set──▶ new value, next frame (CPU and GPU)
   ▲                                │
   │                                ├─ provenance: part_at, part_offset, std::lift::gradient
   └────────── wrela edit ◀─────────┘   (which part, which literal, how much)
```

### 23.1 Lifted builds

A lifted build reads a package's `f32` literals from a table instead of building them into the code, so a tool can change them while the program runs.

- **`wrela build <package> --lift <name>`** lifts the `f32` literals of package `<name>`: the program's own package or a dependency. `--lift <name>/<file>` lifts one file of the package, by its path in the package, and the rest runs at a normal build's speed (for example, a floor's `map.wrela` alone).
- **Which literals are lifted:**
  - A constant whose value is a literal is built where it is used, so every use of `EAR_H` reads one table entry. A parameter's default and a field's default are built the same way.
  - A literal that only a computed constant reads is not lifted: the build runs that code once (§13.1).
  - A negative literal is one entry, with its minus.
- **The CPU** reads the table from memory. **The GPU** reads it from a storage buffer that every pipeline of a lifted build binds last. Before a dispatch or a draw, the program uploads the table if it changed since the last upload. So a change reaches both with the next frame, and never needs a rebuild.
- **The build reports every float literal** of the lifted packages in `lift.json`: whether it is lifted, and why. The build also keeps each lifted file's text and hash (FNV-1a 64, 16 hex digits), so a tool can show a literal's line and write a change back.
- **`std::lift`** is the program's view of its table: `count()` and `literal(i)`; a `Literal`'s `value()`, `built_value()`, `set(v)` and `source()` (file, bytes, line, column); `files()`, `file_name(i)`, `file_path(i)`, `file_text(i)` and `file_hash(i)`. A build that is not lifted has no literals: `count()` is 0.
- **`std::lift::gradient(f, which)`** gives `f`'s value and its derivative by each literal of `which`. This is a parameter gradient, derived in forward mode as `gradient` is (§14.2), through everything that `f` calls, including the code that builds values from literals. It runs on the CPU. `reads(f)` gives the literals that `f`'s code can read on the CPU, through everything that it calls, for any `fn() -> T`.
- **A host changes a literal** through the export that a lifted build has, `__lift_set(literal, value)`, as `set` does. `std::lift::generation()` counts the table's changes. A `Watch` (`Watch::new(reads(f))`) says when a literal that it watches has a new value (`changed()`, once per change). A program cooks again, at that time, what it cooked from literals at load.

### 23.2 Hot reload

`wrela run <package>` serves a lifted build of a program and sends each saved change to the running program without a restart. It lifts the program's own package, or the `--lift` packages, serves the program on 127.0.0.1, and watches the program's files and its dependencies' files. A saved file, or a `wrela edit`, reaches the running program:

```text
 save ──scan (20 ms)──▶ literals alone? ──yes──▶ __lift_set ──▶ the next frame
                              │
                              no──▶ wrela build --lift ──▶ the new build swapped in, in the same page
```

- **A literal edit** is an edit after which the file's tokens are the build's, except for numbers. It changes the literals' values at the next frame, and a `Watch` cooks again what it reads.
- **Any other edit** builds the program again. The host swaps the new build in where the old one ran, on the same device: the page is not reloaded, and the native host's `Host::reload` stays in its process. The host keeps each pipeline whose WGSL and manifest entry are unchanged. The new build starts with `init`:
  - **`std::reload::keep(bytes)`** gives the host bytes to keep, in place of the bytes the program kept before. The next build's **`kept()`** returns them (no bytes if it replaced no build). The program decides what it carries and how, for example with `std::serialize`'s `save` and `load`, which fail cleanly when the type has changed.
  - **The ticker first runs the old build's ticks again,** each with the records it had. So the sim is where it was, or where the new code takes the same input.
  - Input that the old build did not read goes to the new one. A program that plays a voice (`std::audio`) is reloaded with its page.
- The page tells the server the first frame that drew each change (`/live/shown`), and `wrela run` prints how long after the save that frame was.

### 23.3 Provenance

Provenance tells which part of a field decides a point, and how the field's distance changes when one part changes. Every `Surface` (§20.3) answers these questions. A part is a leaf of the surface's tree of combinators: a primitive, a closure, or a `named` surface.

- `parts()` counts the parts, numbered from 0 in the order the leaves are written. `mirror_x()` doubles them: the parts as written, then their images.
- `part_at(p)` is the distance at `p` and the part that decides it. Through each combinator it follows the operand that the result follows: the nearer for a union or a blend, the farther for an intersection, and the cutter where a cut is the surface.
- `part_distance(p, i)` is part `i`'s own distance, moved, turned, scaled and displaced as the combinators above it do.
- `part_name(i)` is a `PartName`: the name its author gave it (`named`), its group's name, the frame it is placed in (a creature's bone), and whether it is a mirror image.
- `part_offset(p, i, by)` is the distance at `p` if part `i`'s own distance were `by` more, through each combinator.
- `part_share(field, p, i)` is how much the distance changes with part `i`: 1 where the part alone decides the surface, 0 where another part does, and between in a blend. A tool uses it to find which parts an edit may move.

The engine's creatures (`engine::creature`) place named parts on bones and answer all of these.

### 23.4 Certified checks

A certified check proves a property of a field over a region with intervals (§14), instead of sampling it.

- `std::field::certify_positive(f, region, depth)` proves `f > 0` in a `Box3`. It takes the derived interval of the region, then of the boxes of its octree, at most `depth` levels down. It also evaluates each box's centre, so it finds a failure as a point.
- `certify_clear(field, region, depth)` proves that no part of the surface is in the region (for example, nothing below the ground).
- `certify_apart(field, a: i, b: j, region, depth)` proves that parts `i` and `j` do not overlap in the region.
- Each gives a `Certified`: `Holds`, `Fails(point)` (a counterexample) or `Unknown(box)` (the first box that the depth could not decide).

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

### 23.5 `wrela edit`

`wrela edit` writes new values into a package's literals, and keeps each literal's written form. The command is `wrela edit <package> [--edits <file.json>] [--dry-run] [--json]`, and its input is `{"edits": [{"file", "start", "end", "text", "hash", "value"}]}`, with each literal as a lifted build gives it.

- **Style.** A literal keeps its form: its decimals, its exponent and its unit suffix, and an integer stays an integer where the value is whole. It keeps at least as many decimals as it had, and gets more only where the value needs them to read back exactly. The new text reads back as the exact `f32` asked for.
- **Sign.** A literal's value is its own: under a negation (`-0.5`), the expression is its negative. A value of the other sign moves the minus.
- **Refusals.** `wrela edit` writes nothing if any of these is true:
  - a file's hash is not the edit's hash (another tool changed the file);
  - a literal's text is not at its place;
  - the file lexes differently after the edit, except in the literals;
  - a file that the formatter owned would no longer be formatted.

### 23.6 The lens: `wrela studio`

The lens, `wrela studio`, shows a subject's surface and lets a person or an agent measure it and change its literals. A subject package has a module `subject.wrela` with `pub fn subject()`, which returns a `Surface` (or a `Field<C>`, for its channels). `wrela studio <package>` writes a small program around the lens (the `studio` package, generic over any such surface), builds it with the subject's literals lifted, and serves it on 127.0.0.1 (port 8417 unless `--port` is given).

- **Every action is an export and a command,** and answers with one line of JSON. `wrela studio <package> <action> [args...] [--png file] [--size WxH]` runs one action headless on the native host. `run <script>` runs a session, one command per line. The same session typed into Chrome gives the same answers, files and frames.

| Action | What it does |
|---|---|
| `view f` | Shows a facing (side, front, three-quarter or top, by name or 0 to 3) or the contact sheet (4) |
| `mode m p` | Sets the shading: shaded, parts, tint, isolate, silhouette or colour, with a part. Colour is the subject's albedo, from the first `r`, `g`, `b` floats of its channels |
| `zoom c h` | Frames a close-up `2 h` metres across at `c`, with a 1 cm grid under 30 cm; `zoom off` undoes it |
| `parts` | Lists the parts' names |
| `probe x y z` | Gives the distance, gradient, part and channels at a point |
| `ray o d` | Gives the first hit and its part |
| `measure a b` | Measures between two points, as a tape measure does |
| `project p` | Gives a point's pixel in each view |
| `diagnose c r` | Gives the diagnostics: pieces, and pieces if thin parts' cells join; bounds; the largest gradient and the five places where it is worst, each with its part; the Lipschitz bound, stated and derived |
| `describe` | Summarizes the subject: each part's volume, box and contacts, and the whole subject's box, volume, mass, pieces, lowest point and asymmetry |
| `click x y` | Gives the part under a pixel, and every literal that moves the surface there, ranked by the distance's derivative by it, with its source line |
| `literals` | Lists the lifted literals |
| `set i v` | Sets a new value; the views show it at the next frame |
| `write` | Writes every changed value through `wrela edit` |
| `move a b` | Drags: the point on the surface at `a` moves to `b`, by the literals named (or the literals that move it most), with other parts' surfaces held |
| `choose i`, `choose_none` | Names the literals for the next drags, or clears them |
| `silhouette f c h n` | Gives a facing's silhouette |
| `fit f c h n` | Fits the named literals so the silhouette matches a reference image (`--reference`) |
| `spec` | Runs the subject's spec: each check's value, target and miss, and how many checks hold (§23.7) |
| `blueprint f` | Draws the blueprint's outline from the side, front or top (0 to 2) over that view, and says where the silhouette misses it: each stretch off by more than a centimetre, how far, and which way (§23.8) |
| `fit_blueprint f n` | Fits the named literals to the blueprint, as to a reference image |

`wrela studio <package> look [--png file]` is the work loop in one command. It saves the contact sheet, marks on a copy what changed since the last `look`, and gives the diagnosis and the spec in brief. Warnings while the lens builds are shown as a count; `wrela check` shows them in full.

Three commands compare by eye. Each writes one PNG of numbered panels and answers with what is in each panel:

- **`beside <manifest>`:** the subject's side view beside a reference photo at the photo's own scale, and the photo with the subject's silhouette laid over it. A manifest registers a photo: `ground_px` (the ground's row), `metres_per_px`, `z0_px` (the column at z = 0) and `faces` (`right` or `left`). The photo is read as a PNG beside the manifest.
- **`variants <package>...`:** several versions of a subject (changed copies of its package) side by side, at the first version's framing.
- **`sweep <literal> <value>...`:** one literal's values side by side, in one session.

How the lens solves and serves:

- **A drag** solves for the literals by least squares on the distance's derivatives (`std::lift::gradient`). It holds fixed points on the other parts' surfaces (those points that a part's share says the part does not decide). It checks the result on a 192³ grid before it accepts it: it takes back a step that would split the subject into pieces. The values end rounded to the literals' own decimals.
- **A fit** minimizes the difference between the silhouette and the reference with Levenberg–Marquardt, using each pixel's ray minimum, with the derivatives by the envelope theorem. It then rounds to decimals and reports the IoU.
- **The server** answers only requests addressed to this machine by name, and takes posts only from its own pages (by their `Origin`). It writes the lens's edits (`post` to `studio/edit`) through `wrela edit` and gives a fit its reference (`studio/reference`). It watches the subject's files: a change to literals only goes to the open lens as new values; any other change rebuilds the lens, and the page reloads.
- Nothing of the studio is in a game's build.

### 23.7 Specs: proportions as code

A spec states what a subject's measures should be, in code that the toolchain checks and solves for. It is a module `spec` in the subject package with `pub fn spec<C: Checks>(c: mut C)`. Its calls `c.near(name, value, target, within: ...)`, `c.at_least(...)` and `c.at_most(...)` (std::lift's `Checks`) hold measures to targets:

```text
pub fn spec<C: Checks>(c: mut C) {
    let f = wolf().field()
    c.near("withers height", top(f, x: 0.0, z: 0.20), 0.80, within: 0.01)
    c.at_most("chest width", width(f, y: 0.55, z: 0.15), 0.20, within: 0.01)
}
```

- **The measures** are std::field functions of any surface:
  - `hit`: how far a ray goes before it meets the surface;
  - `top` and `bottom`: the surface's height above a ground point, from above and from below;
  - `width`: across x, at a height and a depth;
  - `front` and `back`: along z;
  - `farthest`: how far the surface reaches in a direction, within a box;
  - `part_named`.

  Each measure searches with values, then takes one Newton step along its ray, so its derivative by a lifted literal is exact.
- **Run with a `Report`** (the `spec` action, `look`), the checks say how they stand. **Run with a `Loss`,** they are one number: the sum of each miss over its `within`, squared. The loss touches no memory, so `std::lift::gradient` derives it.
- **`wrela solve <package> --spec --free <file>[:<lines>]`** changes the literals on those lines (a whole file if no lines are given) until the spec holds. It runs BFGS on the loss's gradient, then rounds each literal to its decimals, with more decimals where rounding would undo the solve. It reports each check before and after; `--write` writes the literals back through `wrela edit`.
- A spec can move only the subject package's own literals. So a proportion of a base (§23.9) is written in the author's file, to be one of them: `Quadruped { withers: 0.80, ..paws() }`.

### 23.8 Blueprints

A blueprint is outlines of the subject from the side, the front and the top, written as code. It is a module `blueprint` in the subject package with any of `pub fn side()`, `front()` and `top()`. Each returns `Vec<vec2>`: the outline's points in order (a closed polygon), in metres, in its view's coordinates:

- from the side: (z, y), with +z the subject's front;
- from the front: (x, y);
- from the top: (x, z).

The lens draws an outline over its view (`blueprint f`), says where the silhouette misses it and by how much, and fits the named literals to it (`fit_blueprint f n`): the outline is the reference that a fit matches.

### 23.9 What the medium gives an author

std and the engine give an author forms for building subjects such as creatures, and `wrela doc` gives the reference for them.

- **std::field** has `oval(radii)` and `tube(points, radii)`:
  - `oval` is an ellipsoid whose distance stays well behaved inside: its gradient is at most 1, where `ellipsoid`'s gradient grows without bound toward its centre. The oval's Lipschitz bound is finite at any range; the ellipsoid's is infinite once its centre is in range.
  - `tube` is round cones joined end to end through points, smooth and with no swelling at its joints. Its Lipschitz bound is 1.
- **The engine, not the language,** has creatures that take any number of items (an item past the 32nd is never culled). `engine::quadruped` is a four-legged body from a few proportions: the `paws()` and `hooves()` presets, `body(q, look)`, and its groups to use alone.
- **Creatures are forms only, with no fur.** A coat's mass (a ruff, a tail's brush, a cape) is made of parts, and materials give fine surface detail.
- **Lofts** (`engine::loft`): `loft(a, b, up, stations:, angles:, radii:)` is a form around a straight axis from `a` to `b`, given by its sections.
  - A section is a radius at each of `angles` directions around the axis (from `up`, by the right hand), at each of `stations` places along the axis. The loft is linear between them and capped flat at the ends.
  - Any section that is star-shaped about the axis fits, so a loft can hold a measured form as it is.
  - Its distance is ρ − r over the size of the side's slope, √(1 + r_s² + (r_θ / ρ)²), with ρ held at least half the radius. So its gradient is 1 at the surface where sections slope.
  - Its Lipschitz bound comes from the table, and is infinite where its scope reaches the zone near the axis.
  - `section(t, theta)` gives the radius and its slopes at a point of the table.
- **Sweeps** (`engine::sweep`): `sweep(points, up:, sections:, places:, ends:)` is a form along a smooth curve through `points` (for example, a limb's joints, from hip to toes). It is given by `sections` at `places` along it (fractions of its length), smooth between them (a Catmull–Rom curve of their numbers).
  - A section is named as an artist reads a form: `section(up:, down:, left:, right:, square:, turn:)`. These are its extents from the path, how square its quarters are (2 is an ellipse's), and its turn. "Up" is toward the sweep's `up`, carried along the path without twisting; "left" is `up × forward`.
  - `.bump(at:, height:, width:)` adds a bump, four at most: a muscle's belly or a bone under the skin, or a groove where the height is below 0. `round(r)` and `oval(width, height)` are the simple sections.
  - Its ends are flat, or domes that reach `ends` past them.
  - The path is cut into straight pieces. A point's section is in the plane through the point that turns from one piece's end plane to the other's, so a sweep bends through a joint with no seam.
  - Its distance is a loft's (over the side's slope), and never less than the distance to the path less its reach, which keeps its derived interval tight away from the path.
  - Its Lipschitz bound comes from samples of its sections and its turns. The bound is infinite where a turn is sharper than the sweep is thick, because there its inside folds over itself.
- **Digits** (`engine::digits`): `digits(at, forward:, up:, spread:, length:, thick:, splay:, curl:, outer:)` is `N` toes side by side (or fingers, or a hoof's halves). Each digit is an arched sweep with a domed tip, and the outer digits are `outer` times the size of the middle ones.
- **Creases** (`engine::creature::crease(k, depth)`): a seam that joins two forms with a fold. The surface sinks into a groove `depth` deep where the forms meet, and the groove fades over `k` (it is at most a third of `k` deep). It is a smooth union's fillet turned over. Where the two forms overlap by less than `depth`, the crease cuts through.
- **The anatomy kit** (`engine::anatomy`) builds at the level a creature artist works at:
  - `muscle(origin, insertion, bulk)`: a spindle of round cones, exact;
  - `knob` and `tendon`: bones that show;
  - `bundle`: blends muscles, knobs and tendons into one part;
  - `CanidHead`, `CanidEar` and `CanidPaw`;
  - `ground()`: nothing below y = 0;
  - `Paint`: colour that changes with height across a part;
  - `canine(wolf(), coat)`: a whole canine from a few proportions (`wolf()` gives a grey wolf's), on a rigged skeleton. Its `ruff` and `brush` are the coat's mass, built into the forms of the neck and the tail.
- **Colour across a part:** std::field's `with_at(|p| ...)` gives a surface channels from a function of the point, where `with(c)` gives one value.
- **`wrela doc`** reads the packages that a package depends on (`wrela doc engine::quadruped <package>`), shows a struct's fields with their doc comments, and lists the built-in functions (`wrela doc builtins`).
- **`wrela doc --export <dir> [--commit <sha>] [<package-dir>...]`** writes the reference docs into a new directory, from the same index as `wrela doc`. The same sources give the same bytes. It writes:
  - in Markdown: this spec split at its sections, the lexical structure, the grammar, and a page for each module of std and of each package given;
  - `index.json`: each public item (its path, kind, signature, doc, page, anchor, and source file and line) and each section of the spec's prose;
  - `llms.txt`, a map of the pages that says how to read them, and `llms-full.txt`, every page in one file; each group's pages are in one file too (`spec/llms-full.txt`, `std/llms-full.txt`), with its size in `llms.txt`.

### 23.10 Tools for agents

These commands let an agent read what the compiler knows about a package, instead of guessing it, and change or measure the package. Each takes a package directory and answers in JSON (`--json`).

| Command | What it does |
|---|---|
| `wrela query <package> <query>...` | Answers a batch of queries with one check (below) |
| `wrela context <item> [<package>] --budget n` | Gives the item's source, the types it names, what it calls, where it is called and its impls, in about `n` tokens. On a program with errors, it says at the end what the errors keep from being known |
| `wrela refactor <package> <change>` | Makes a change across the package: `rename`, `move`, `add-param` or `change-mode` (below) |
| `wrela fix <package-dir> [--json]` | Makes every fix a tool can make, and reports each one; a fix that is the author's choice is listed, not made (§6.13) |
| `wrela solve <package> --minimize <fn> --free <file>:<lines>` | Minimizes a function of no arguments over the literals on those lines, and rounds them to their decimals. It builds a program around the function that uses `std::lift::Minimize` (BFGS on `std::lift::gradient`). `--write` writes the literals through `wrela edit` |
| `wrela run <package> [--port N]` | Serves the program lifted on 127.0.0.1 with hot reload (§23.2): a saved literal reaches the running program at its next frame, and any other edit swaps in a new build, in the same page |
| `wrela trace <package> --watch <exports>` | Runs frames and prints what the exports say after each frame (`--csv`) |
| `wrela reference replica <parts> -o <dir>` | Writes a replica of a reference mesh as a package: a skeleton, and a loft or a sweep for each part (below) |
| `wrela reference deviation <parts> <package>` | Measures how far a package's surface is from the mesh's surface (below) |
| `wrela bisect <package> --until "<export> <op> <number>"` | Finds the first frame at which a condition holds (`nan <export>` too) |
| `wrela profile <package> [--frames n]` | Shows where a frame's time goes on this machine (below) |
| `wrela primer [area]` | Teaches the language area by area: each rule of the conformance suite, with a program that keeps it and one that breaks it |

**`wrela query`** takes its queries from the arguments, or one per line on standard input:

- `type <file>:<line>:<col>`, `callers <fn>`, `callees <fn>`, `impls <trait or type>` and `instantiations <fn>`;
- `effects <fn>`: each effect, with a call chain to where it happens;
- `borrows <file>:<line>`: the loans live and the places moved out of before the line runs;
- `search <signature>`, for example `(vec3, _) -> f32`.

It also answers on a program with errors. A function whose body has an error has no memory IR, so what it calls, borrows and does is not known. An answer that such a function, or an error outside every body, could change lists those errors as `unsure`.

**`wrela refactor`** plans the change and checks the new texts before it writes anything. It refuses the change, with the errors, if the program would not check, and refuses it if a call would quietly bind to another function. `--dry-run --json` gives a plan with each file's hash; `apply <plan>` refuses the plan if a file has changed since.

**`wrela reference replica <parts> -o <dir>`** builds the replica from a parts file:

- The parts file names a mesh's manifest in `references/`, landmarks, and each part's axis, angles, station spacing and longest radius. The file's top gives defaults. The manifest's `axes`, `origin` and `metres_per_unit` place the mesh in a creature's frame, and the package carries the manifest's `attribution`.
- Each axis runs 2 cm past its ends (`overlap`) and is then trimmed to the mesh, so parts that meet at a joint overlap. At an end inside another part, the part sinks by up to `taper` toward its cap, so it passes under its neighbour instead of ending in a step.
- A radius is where a ray from the axis leaves the mesh: where the ray's winding number falls to 0. So the ray passes surfaces inside the mesh where a sculpt crosses itself. A radius held at the longest (its ray ran into another part) is filled from the measured radii around it.
- Radii are integers, in tenths of a millimetre, so a lifted build does not lift them.
- `--harmonics k --sections-every m` compresses each table:
  - Each section is a Fourier series of `k` harmonics, and each coefficient is a Catmull–Rom curve through a control section every `m` metres.
  - The coefficients are fitted to the measured radii by least squares; a radius held at the longest is free.
  - Where no other part reaches a radius's point (or the points halfway to the next radii), the fit is held up to the measured radius. So the compressed parts leave no crack where the dense parts left none.
  - The command reports how many numbers the compact form takes.
- A part with `sections = k` is a sweep (`engine::sweep`) along a path from `from` through `through` (landmarks) to `to`, with `k` control sections of `bumps` bumps (4 unless the part says otherwise) and domes `ends` long:
  - Its radii are measured in its own planes every `spacing`. Each radius is the part's own surface, or shared: another part, as measured, holds its point (the ray ran on along that part).
  - Each station's section is fitted alone, outward from the station with the most of its own surface, each from its neighbour's, so a bump follows its feature.
  - The control sections are fitted to those sections by least squares, then all at once: own radii in full, shared radii only where the fit passes them.
  - Where no fitted part reaches 2.5 mm inside a measured radius, the radius is held to within 2 mm and its part is fitted again, the parts that came nearest first.
- `--seam m` joins every part with a seam `m` wide.

**`wrela reference deviation <parts> <package>`** reports:

- at points spread over the mesh's surface (skipping surfaces inside it), how far short of the mesh the package is, or how far past it outward: the shares within 2 mm and 5 mm, and the mean, by part, with the worst places;
- the volumes inside one and not the other;
- rays from five views (the two sides, the top, the front and the lens's three-quarter view, one every 2 mm) that pass through the mesh but meet nothing of the package, inside its outline or at its rim: places where a viewer would see through it.

**`wrela profile`** builds the program as `wrela build` builds it and runs it on the native host's GPU. It reports:

- each label's median milliseconds per frame (a pass or a dispatch, named by `std::gpu::label`), and the frame's span;
- work done in some frames only (for example, a first frame's cooking), by its total;
- the median frame call's CPU time, and the phases that the program timed (`std::time::phase`);
- each pipeline's WGSL bytes.

Each time is a median, with its 95th percentile beside it, because a budget is about the slow frames too. `--serial` runs each pass alone, so a label's time is its own where passes overlap (as on Apple GPUs). Times belong to the machine: compare a change with a profile from before it. `--baseline <report.json>` (an earlier `--json` report) does this: each time shows its earlier value and how it changed, and the report lists the labels that no longer run.

**Agent runs are debug builds.** `wrela test` builds the program as a debug build (§11.3). `wrela trace` and `wrela bisect` do too, unless given `--release`. So a simulation that makes a NaN, or indexes a GPU buffer out of range, stops at the frame where it did, and the agent looks there.

- `trace` and `bisect` accept `--no-gpu`: the CPU host checks submitted commands but does not execute shaders. Use it to watch CPU state that needs no GPU readbacks.
- Both accept `--workers N`; without it, worker counts follow the available CPUs.
- CPU-only frame and tick tests keep language checks and command validation, and generate no unused shader or browser files. Tests marked `gpu: true` still build and execute shaders.

## The rules by ID

Each rule of the conformance suite (`compiler/tests/conformance`), with what it says and the section that gives it in full. A rule's ID is its area and its name: the area is the part of the language it's about (`mem` the memory model, `gpu` GPU code, and so on), and `wrela primer <area>` teaches an area's rules, each with a program that keeps it and one that breaks it. A test checks that this table, the primer and the suite's cases name the same rules, with the same words.

| Rule | What it says | § |
|---|---|---|
| `lex.newline` | A newline ends a statement, unless brackets are open or the next line starts with `.` | §2.1 |
| `lex.trailing-op` | An operator that continues a line trails it; a line can't start with one | §2.1 |
| `lex.semicolon` | `;` separates statements on one line | §2.1 |
| `lex.else` | `else` follows its `}` on the same line | §2.1 |
| `lex.comments` | Comments are `//` and doc comments `///`; there are no block comments | §2.1 |
| `lex.chars` | Outside comments and strings, only the language's characters | §2.1 |
| `lex.literals` | An integer literal fits the type it's used as | §2.1 |
| `lex.no-suffix` | A number literal has no type suffix | §2.1 |
| `lex.pow-xor` | `**` is exponentiation, and `^` is XOR on integers | §2.1 |
| `stmt.for-pattern` | `for pat in xs` destructures each element, and its pattern can't fail | §2.2 |
| `stmt.if-let` | `if let pat = x { } else { }` tests a pattern; as a value, it needs an `else` | §2.2 |
| `stmt.let-else` | `let pat = x else { }` binds a pattern, and its `else` leaves the scope | §2.2 |
| `stmt.while-let` | `while let pat = x { }` runs its body while `x` matches | §2.2 |
| `fn.named-args` | Positional arguments come first, then named ones, each once, all present | §3.1 |
| `fn.defaults` | A parameter may have a constant default | §3.1 |
| `fn.return` | A function that returns a value ends with one, or returns one, on every path | §3.1 |
| `fn.return-trait` | A trait in return position names one concrete type, which is inferred | §3.1 |
| `struct.defaults` | A struct's fields may have constant defaults, which a literal may omit | §3.2 |
| `struct.opt-in` | A struct opts in to `Copy`, `Clone`, `GpuData` and its other traits in its declaration | §3.2 |
| `struct.base` | `..base` fills a literal's other fields from another value of its type | §3.2 |
| `struct.packed` | A `Packed` struct's `Bits<N>` fields are bits of one `u32`, read and written as `u32`s | §3.2 |
| `enum.match` | An enum's variants hold payloads, and a `match` on it is exhaustive | §3.3 |
| `enum.discriminants` | An enum whose variants hold nothing numbers them: `u32(e)`, `E::from_u32(n)`, `Flags<E>` | §3.3 |
| `trait.items` | A trait has associated types and default methods, and an impl gives what its trait declares | §3.4 |
| `trait.orphan` | An impl lives in the package of its trait or of its type | §3.4 |
| `trait.eq-ord` | `==` and `<` come from a declared `Eq` and `Ord` | §3.4 |
| `trait.fieldwise` | A `@fieldwise` trait is derived field by field for each type that declares it | §3.4 |
| `trait.fieldwise-walk` | A fieldwise trait's method can walk `for f in fields(self)`, derived from that body for each type | §3.4 |
| `const.build` | The build computes a `const`; a panic, a trap or running past its fuel is an error | §3.5 |
| `const.places` | A constant is a place: read and projected, never moved out of or changed | §3.5 |
| `mod.use` | A file is a module, `use` imports, and `pub` makes an item visible outside its file | §3.6 |
| `mod.names` | A module has one namespace, and defines each name once | §3.6 |
| `mod.packages` | A package depends on others by path; only `pub` items cross, and never in a cycle | §3.6 |
| `ty.no-ref` | `&T` isn't a type | §4 |
| `ty.no-dyn` | There's no `dyn`: an enum or a generic does its work | §4 |
| `ty.option` | An absent value is `Option<T>`; there's no null | §4 |
| `ty.scalars` | The scalar types never convert implicitly | §4.1 |
| `ty.cpu-only` | `f64`, `i64`, `u64` and the 8- and 16-bit integers are CPU only | §4.1 |
| `ty.vectors` | Vectors and matrices: their constructors, components and swizzles | §4.2 |
| `ty.arrays` | An array `[T; N]` has a constant length | §4.3 |
| `ty.array-fill` | `[a, b, ..fill]` fills an array to the length its type gives, with a `Copy` fill | §4.3 |
| `ty.enum-arrays` | `[T; E]` has an element for each variant of an enum numbered 0, 1, 2 and on, indexed by it | §4.3 |
| `ty.const-generics` | A `const N: u32` parameter ranges over array lengths; a length makes no calls | §4.3 |
| `ty.runs` | A run `[T]` is a parameter, a result or a binding, never a field or a type argument | §4.4 |
| `ty.tuples` | Tuples | §4.4 |
| `ty.alias` | `type Name<T> = Type` names a type; an alias can't name itself | §4.5 |
| `ty.opaque-alias` | An alias that names traits names the type that the first function in its module to return it returns | §4.5 |
| `ty.fstrings` | `f"…"` builds a `String` from text and values that implement `Format` | §4.6 |
| `mem.copy` | A `Copy` value copies implicitly; any other value moves only with `take` | §6.1 |
| `mem.take` | A move out of a named place is written `take`, and a moved value can't be used | §6.1 |
| `mem.clone` | A deep copy is `.clone()` of a `Clone` type | §6.1 |
| `mem.literal-moves` | A struct literal moves the owned locals it names, without `take` | §6.1 |
| `mem.loops` | A value from outside a loop can't be moved inside it | §6.1 |
| `mem.drop` | A value is dropped at the end of its owner's scope; only std's core has destructors | §6.1 |
| `mem.modes` | `mut` and `take` arguments are marked where they're passed | §6.2 |
| `mem.receivers` | A `mut self` call isn't marked; a `take self` call on a named place is | §6.2 |
| `mem.let-owns` | `let` owns: a place whose type isn't `Copy` is borrowed, cloned or taken | §6.3 |
| `mem.bindings` | `let` and `var` own, and `borrow` and `mut` project a place | §6.3 |
| `mem.match-mut` | `match mut place` binds mutable projections of the place | §6.3 |
| `mem.projections` | A projection returns part of a `borrow` or `mut` parameter, or of a constant, never of a local | §6.4 |
| `mem.exclusivity` | Nothing touches a place that overlaps a live `mut` access | §6.5 |
| `mem.no-globals` | There are no mutable globals | §6.5 |
| `mem.borrow-structs` | A borrow struct borrows its fields' places as separate parameters would | §6.6 |
| `mem.closures` | A closure that captures a projection is passed down; one that captures only values can be stored | §6.7 |
| `fn.values` | A named function is a value, and so is a type's associated function: `W::work` | §6.7 |
| `fn.fields` | A function in a struct's field, or a `take` parameter, makes the struct or the function generic over it | §6.7 |
| `eff.deterministic-fn` | A function passed as a `@deterministic fn` is deterministic | §6.7 |
| `mem.arenas` | An arena's values are named by handles: `arena[h]` projects, and a stale handle panics | §6.8 |
| `mem.plain` | `Plain` data has no pointers, and every field of a type that declares it is `Plain` | §6.10 |
| `mem.unsafe` | `unsafe` only in std's core, and only in packages that declare it | §6.12 |
| `gen.bounds` | Generic code is checked against its bounds, and monomorphized | §7 |
| `gen.impl-trait` | A trait in parameter position makes a function generic | §7 |
| `trait.sets` | `trait Sim = A + B` names a set of traits, which stands for them wherever traits are listed | §7.1 |
| `trait.diagnostic` | `@diagnostic("…")` on a trait or a type replaces the message when a bound isn't met | §7.1 |
| `eff.trace` | A line printed or a phase timed is `trace`, allowed everywhere but GPU, `@audio` and derived code | §8.1 |
| `eff.gpu` | GPU code doesn't allocate, call the host, recurse or panic, through every call and instantiation | §8.3 |
| `eff.audio` | `@audio` code doesn't allocate, do IO, recurse or record GPU work, and its closures capture nothing | §8.3 |
| `eff.parallel` | A `@parallel fn` writes no data it captures, and does no IO, GPU or non-deterministic work | §8.3 |
| `eff.derived` | A derived interpretation needs a function with no host effect | §8.3 |
| `attr.closed` | Attributes are a closed set | §9 |
| `attr.gpu` | `@gpu` asserts that a function is GPU-safe, checked where it's defined | §9 |
| `attr.testing` | `@testing` marks an export that only a test build has | §9 |
| `err.try` | `x?` returns a `None` or an `Err` from a function that returns the same kind | §10.1 |
| `gpu.uniformity` | Derivatives only where every pixel reaches them, in uniform control flow | §11.4 |
| `const.fuel` | `@fuel(n)` on a `const` or a test sets the work it may do, its threads' together | §13.1 |
| `const.tests` | A `@test` is a free function of no parameters and no result, run as constants are | §13.2 |
| `derive.gradient` | `gradient` of a closure or function of an `f32` or a float vector | §14.2 |
| `derive.interval` | `interval` over a box, of a function whose loops don't depend on its input | §14.2 |
| `gpu.roles` | A client build runs `init` and `frame`, a server build `serve_init` and `serve`, and `@client` and `@server` code stays in its role | §15.3 |
| `fn.jobs` | A `@job fn` runs over several frames, to each `yield`: `f.start(...)` takes its `take` parameters, and `job.resume(...)` its `borrow` and `mut` ones | §16.3 |
| `fn.job-values` | A job's value is `Clone`, `StateHash` and `Serialize` when the locals it holds are | §16.3 |
| `mem.jobs` | A job keeps only owned values across a `yield`: no projection or loan | §16.3 |
| `eff.requests` | `@deterministic` code neither makes requests (`io`) nor polls them (`nondet`) | §17.1 |
| `eff.input` | Reading input is `nondet`, so `@deterministic` code takes events as data | §17.2 |
| `eff.clock` | Reading the clock is `nondet`, so `@deterministic` code takes its time as data | §17.3 |
| `gpu.data` | Data that crosses to the GPU is `GpuData` | §18 |
| `gpu.entry` | An entry point takes builtins, uniforms, buffers, textures, groups and outputs, and returns what its stage does | §18.1 |
| `gpu.cpu` | GPU builtins, derivatives and sampling only in GPU code | §18.1 |
| `gpu.workgroup` | A workgroup's size is within WebGPU's limits | §18.1 |
| `gpu.discard` | `discard()` drops a fragment, in fragment shaders only | §18.1 |
| `gpu.buffer-modes` | A buffer that the GPU writes is passed `mut`, and buffers and spans bound together don't overlap | §18.3 |
| `gpu.buffer-drop` | A GPU buffer is owned: it moves, drops, and lasts in the program's state | §18.3 |
| `gpu.kernel-mut` | A kernel's `mut` parameters are outputs that invocations can share: `Slots<T>` and its kin | §18.4 |
| `gpu.atomics` | Atomics, appends and atomic maps are a kernel's `mut` parameters, passed `mut` | §18.4 |
| `gpu.outputs` | A `One<T>` is written by a kernel's first invocation, and indirect work takes typed arguments | §18.4 |
| `gpu.workgroup-memory` | Each invocation writes its own chunk of workgroup memory, and barriers separate writes from reads | §18.5 |
| `gpu.groups` | A borrow struct of textures, samplers, spans and `GpuData` values is one GPU parameter | §18.6 |
| `gpu.dispatch` | `dispatch` and `draw` bind their entry points' arguments; an entry point isn't called | §18.7 |
| `gpu.bound` | `k.bind(...)` is a value that a command takes, and code is generic over `Kernel`, `VertexShader<V>` and `FragmentShader<V>` | §18.7 |
| `gpu.domains` | `over:` dispatches a kernel over a count, a size or a texture, its groups and bounds the compiler's | §18.7 |
| `gpu.draw-state` | A draw's cull mode, depth bias and depth test are build-time constants | §18.7 |
| `gpu.textures` | Textures are sampled in fragment shaders, read anywhere on the GPU, and drawn into in passes | §18.8 |
| `gpu.formats` | A texture's format is a type: its texel, and whether it's filtered, written by kernels and drawn into | §18.8 |
| `build.budget` | A shipped build's pipelines have at most 256 KiB of WGSL each | §18.9 |
