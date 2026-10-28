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
- No `async`: IO is a request that the program polls (§17.1), or that a tool waits on (§15.5).
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
- A computed constant's value nests at most 256 aggregate containers deep (E0704). Each struct, tuple, enum, array, vector, matrix, `Box` and `Vec` counts once along a path through the value; scalar leaves do not count. This also bounds chains through heap pointers.
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

A trait declares methods, associated types and associated constants; a type gets a trait through an `impl`, or by declaring a structural or `@fieldwise` trait that the compiler derives.

- **Traits have associated types, associated constants and default methods.**
- **An impl gives what its trait declares, and nothing else:**
  - its methods take the trait's parameter defaults, and can't declare their own (E0405);
  - associated types belong to traits, not to inherent impls (E0402);
  - a blanket impl bounded only by its own trait (`impl<T: Tr> Tr for T`) implements nothing (E0400).
- **Method syntax finds a trait's methods only where the trait is visible:** it's `pub`, or the calling module declares it. A private method is private to its module (E0203).
- **Coherence follows Rust's orphan rule:** an `impl` lives in the package of the trait or of the type, and std is another package.
  - `Copy`, `Clone` and `GpuData` aren't implemented with an `impl`: a type opts in to them in its declaration.
  - An impl for every closure or function of one signature, `impl<F: Copy + GpuData + fn(vec3) -> f32> Shape for F`, applies to just those. A closure whose signature doesn't fit, or a struct, doesn't have the trait, so the impl doesn't conflict with one for a struct.
- **An associated constant** is declared in a trait as `const K: u32`, with an optional default (`const ON: bool = true`), and given in an impl as `const K: u32 = 4`; an inherent impl may hold one too (`impl<S: Taps> Frame<S> { const N: u32 = S::K * 2 }`).
  - It's read as `X::K`, `Self::K`, or under a bound `S: Taps` as `S::K`: a constant of each instantiation, since generic code is monomorphized (§7). Its value is folded where it's used, so a branch on `S::ON` costs an instantiation whose `ON` is `false` nothing.
  - Its type is a `bool`, `u32`, `i32`, `f32`, an enum whose variants hold nothing, or a `Text` (E0420). Larger values belong in a `const` item (§3.5), which a method can name.
  - An impl gives each constant its trait declares without a default (E0401), and no other (E0402); it isn't a function, so `S::K()` is E0420.
  - An entry point's `const` argument may be one (`blur.bind(taps: S::K)`, §18.1): each instantiation's value is a pipeline of its own. There, its initializer must fold: literals, constants, other associated constants, `const` generic parameters, arithmetic, comparisons and `if` (E0603 otherwise).

```wrela
use std::gpu::{GlobalId, GpuBuffer, Slots, dispatch}

trait Taps {
    const TAPS: u32
    const HEAVY: bool = true
}

struct Narrow {}

impl Taps for Narrow {
    const TAPS: u32 = 2
    const HEAVY: bool = false
}

@compute(64)
fn blur(out: mut Slots<f32>, id: GlobalId, taps: const u32, heavy: const bool) {
    out[id] = if heavy { f32(taps) * 2.0 } else { f32(taps) }
}

fn run<S: Taps>(out: mut GpuBuffer<f32>) {
    dispatch(blur.bind(out: mut out, taps: S::TAPS, heavy: S::HEAVY), groups: 1)   // a pipeline per `S`
}
```

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
- A tuple's or an array's derived impl has a generic trait's arguments as its first element does, however deep the pairs nest: a pair of `PartChannels<Tissue>` items is a `PartChannels<Tissue>`, and so is a pair whose first element is such a pair.
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

A manifest value can nest arrays and inline tables at most 128 levels deep; deeper values are E0219.

```text
[package]
name = "herd"                          # how diagnostics name it
unsafe = true                          # it may use `unsafe` (§6.12; E0214 otherwise)

[dependencies]
fieldkit = { path = "../fieldkit" }    # a local path, relative to this file
engine = { workspace = true }          # the member of the workspace named `engine`
```

- **A dependency's modules are named under it:** `use fieldkit::shapes::blob`. A path's first name is `std`, a module of the package, or one of its dependencies, and one name can't be both (E0201).
- **Only `pub` items cross a package boundary.** `pub(package)` makes an item, a field or a method visible to every module of its package and to no other (E0203).
- **The orphan rule is per package:** an `impl` lives in the package of its trait or of its type, and std is a package too. std's privileges (its core's `unsafe`, intrinsics, destructors and lang items) come from its being std's package.
- **A directory with its own `wrela.toml` is another package,** never part of this one's module tree. Only the program's `main.wrela` is its interface to the host (§15.1); in a dependency, `main.wrela` is an ordinary module.
- **Dependencies are built from source with the program.** A dependency is a local package, found by its path or by its name in the workspace: there are no versions, lockfiles or registries. A dependency can't lead back to a package that depends on it (E0221), a `path` that isn't a directory is E0220, and a manifest that isn't valid is E0219. A package that two others depend on is built once.
- **A diagnostic in a dependency names the package and the path inside it:** `[fieldkit] shapes/blob.wrela:3:5`. Builds with dependencies are byte-reproducible, from any path.
- **Effects are inferred across packages** as within one: exported functions don't state them (§8, §21).

#### Workspaces

A workspace is a set of packages that depend on each other by name. A `wrela.toml` with `[workspace]` lists its members:

```text
[workspace]
members = [
    "engine",       # a package's directory, relative to this file
    "content/*",    # each directory in content/ that has a wrela.toml
]
```

- **A package's workspace is the nearest `wrela.toml` with `[workspace]`** in its directory or above it. When workspaces nest, the nearest one is used.
- **`name = { workspace = true }` is the member named `name`.** A member's name is its `[package] name`; a member without one has its directory's name, with `-` read as `_`. A package can use a member this way whether it is a member or not. A dependency has a `path` or `workspace = true`, not both (E0219).
- **A workspace's manifest is a package's too only if it has `[package]`.** Without one, its directory isn't a package, and checking or building it is E0223.
- **Each member is a package with a name of its own.** A member that isn't a directory with a `wrela.toml`, a `/*` that names no package, and two members with one name are E0223. So is a member that the workspace's versioned `.gitignore` files ignore, read as `wrela suite` reads them (a directory inside an ignored one is ignored too): git would never commit it, so the workspace would break in every other checkout. A directory listed twice is one member. A workspace dependency with no workspace above it, or with no member of that name, is E0220.
- **`path` dependencies work in a workspace and outside it.**
- **`[workspace]` has these keys.** Any other key, a value of another kind, or a section declared twice, is E0219.
  - `members`, above: an array of strings.
  - `allow`, `exclude` and `pending`: arrays of globs of the workspace's files, for `wrela suite allow` (§13.2). A glob is a path from this file, with `/` between its parts, and no part empty, `.` or `..`. `*` and `?` match within a part, and a part `**` matches any number of parts.
  - `untested`: an array of members, by their directories, whose tests `wrela suite` runs only when they are named. A directory that isn't a member is E0223.
  - `toolchain`: the toolchain release that the workspace runs, a string of three whole numbers such as `"0.1.0"` (below).

#### The toolchain

A workspace pins the toolchain release it is built with: `toolchain = "0.1.0"` under `[workspace]`. A release is `wrela`, `wrela-host`, `wrela-serve` and Binaryen's `wasm-opt` for one target (`x86_64-unknown-linux-gnu` or `aarch64-apple-darwin`). `wrela` holds std and the browser runtime, so a release needs nothing else.

- **A `wrela` runs the release that the workspace pins.** The workspace is the nearest `wrela.toml` with `[workspace]` at or above the current directory. If its `toolchain` isn't this `wrela`'s release, `wrela` runs that release's `wrela` instead, with the same arguments, and exits as it does. With no pin, or with this release, it runs as itself.
- **Releases are kept in `~/.wrela/toolchains/X.Y.Z`.** A release that isn't there is downloaded over HTTPS from `https://github.com/rywible/wrela-compiler/releases/download/vX.Y.Z/`: `wrela-X.Y.Z-<target>.tar.gz` and `SHA256SUMS`. A tarball whose SHA-256 isn't the one that `SHA256SUMS` gives is refused. It is unpacked into a temporary directory, which is then renamed into place. A release that doesn't exist, a checksum that doesn't match, and a network that can't be reached each stop the command with an error (status 2). `WRELA_TOOLCHAIN_RELEASES` names another place to download releases from.
- **`WRELA_TOOLCHAIN` overrides the pin.** It is the path of a `wrela-compiler` checkout (a directory with a `Cargo.toml`), which `wrela` builds (`cargo build --release -p wrela -p wrela-host -p wrela-serve --bins`) and runs from its `target/release`, or of an unpacked release, which it runs from its `bin/`.
- **The `wrela` that runs is never replaced again.** It runs with `WRELA_TOOLCHAIN_CHOSEN` set to why it was chosen, and a `wrela` with that variable set runs as itself.
- **`wrela toolchain` says which toolchain runs,** where it is and why. `wrela --version` (and `wrela-host --version`, `wrela-serve --version`) says the release and the source revision that it was built from.
- **An optimized build uses the release's own `wasm-opt`** (`wrela build --optimize`): the one beside `wrela`, which must be Binaryen 133 exactly. A `wrela` without one, such as a checkout's, uses `WRELA_WASM_OPT` or the `wasm-opt` on `PATH`, Binaryen 133 or later.
- **A release's reference docs link std to the toolchain's source** (`wrela doc --export`): `https://github.com/rywible/wrela-compiler` at the revision the release was built from. A package's items link to the repository and commit that the export is given.

A workspace also declares its tools (§15.5), each in a `[tools.<name>]`: a member with a tool build, and the most that `wrela tool <name>` grants it.

```text
[tools.release]
package = "tools/release"         # a member's directory, relative to this file
read = ["ws=."]                   # a root's name and its directory, relative to this file
write = ["out=build/release"]     # made if it is missing
net = ["api.cloudflare.com"]      # as --net takes them
run = ["wasm-opt"]                # as --allow-run takes them; a path is relative to this file
secrets = ["CLOUDFLARE_API_TOKEN"]
browser = true                    # --browser
timeout = 1800                    # seconds
workers = 4                       # helpers for std::par, 0 to 8
```

- **`package` is required.** A tool's name has letters, digits, `_` and `-`. Each root's directory stays inside the workspace: no part of its path is empty, `.` or `..`. A key that isn't one of these, a value of the wrong kind, and a `[tools.<name>]` in a manifest without `[workspace]` are E0219.
- **A tool's package is a member with a tool build.** A `package` that isn't a member, or whose `main.wrela` doesn't export `tool`, is E0223. This is checked each time the workspace is read: by the check of a package that depends on a member by name, by `wrela tool` and by `wrela suite`.

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

CPU code has no `f16` scalar, so a buffer holds half floats as `Half2`s. GPU code has `f16` on a device with the optional feature (§18.10).

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
- **A temporary lives until the end of its statement,** so every projection of it in the statement reads it: `assert(make().as_str() == "ab")`, `f(make().as_str())`. A temporary that something consumes (a `take` argument, a binding, a field of a new value) is its new owner's. A temporary made in a branch of an `if` or a `match`, or in a block's statement, ends there; one made by a loop body's last expression, its value, ends with each pass.
- **A binding can't keep a projection of a temporary** (E0522): `let s = make().as_str()`, `borrow v = make()` and `let View { s } = view(make())` point into values that end with their statement. Bind the owner first, then project it: `let owned = make()`, `let s = owned.as_str()`.
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
- **A consuming (`take self`) method on a named place is marked:** `take builder.finish()`. On a temporary, no marker is needed: `Builder::new().finish()`. The mark is the call's when a `?` follows it: `take call.wait()?` is `(take call.wait())?`.

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
- **A projection binds a place that outlives it,** never a temporary (E0522, §6.1): `let name = make_name().as_str()` is an error; `let owned = make_name()` and then `let name = owned.as_str()` is the fix. A run or a borrow struct that a pattern binds is a projection too, wherever it comes from, so it keeps the loans of what it points into.
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
| `@test`, `@test(frames: n)`, `@test(tool)` | functions | A test. `wrela test` runs it as the build runs constants, or after `n` of the program's frames (§13.2). With `gpu: true` it runs on the GPU, and with `golden: "frame.png"` it compares its screen with a PNG. `@test(tool)` goes on a function, which runs on the package's tool build. |
| `@fuel(n)` | constants, tests of code | The work that the build may do to compute the constant or run the test (§13.1) |
| `@client`, `@server`, `@tool` | functions | Only that role's build may run the function (§15.3). Reaching it from another role's entries is E0708. A function has one role (E0204). |
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

- **A recoverable failure is a `Result<T, E>`,** passed up with `?`. `x?` returns a `None` or an `Err` from a function that returns the same kind, with the same error type: `x.map_err(f)?` makes the error the function's first.
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
- **Every diagnostic has a next step:** a help or a fix of its own, or, where its code has nothing more to say there, a help that points at `wrela explain`.
- **A migration** is a warning for a form the language moved away from, whose one fix a tool always makes; `wrela migrate` makes every one of a package's, and nothing else (§23.9). The old form compiles for at least two minor releases before the error that replaces it.
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

CPU code always uses strict IEEE floats. There's no fast-math mode, no reassociation, no implicit FMA contraction and no relaxed SIMD. The one exception is a relaxed-SIMD build's second module, and only in presentation code (§11.2).

- WASM's float arithmetic is already IEEE-strict, apart from NaN bits. NaN bits are canonical wherever they're observed (§11.3).
- Transcendentals come from std (`std::math`), compiled to WASM, never from the host. They compute in f64 and round once. Tests sample 20,000 points for each function, in the ranges they choose, and each result is within an ulp. This is evidence, not proof.
- The compiler checks that each WASM module it emits has no relaxed SIMD, outside a relaxed module's presentation code.
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
- Standard SIMD rounds each lane exactly as the scalar operation does, and has no fused multiply-add, so the bits are identical. Relaxed SIMD is forbidden, except in a relaxed-SIMD build's presentation code (below).
- An operation whose SIMD form behaves differently stays scalar. For example, float-to-int conversion saturates in SIMD but traps in scalar code (§11.3).
- A reduction keeps its fixed order (§12). A dot product or a length adds its lanes one at a time, in order. A loop that sums stays one iteration at a time.
- A debug build runs every loop one iteration at a time, since its NaN checks panic at a specific iteration.

A relaxed-SIMD build (`wrela build --relaxed-simd`, release only) has a second module beside `game.wasm`, `game.relaxed.wasm`, named in the manifest as `wasm_relaxed`. A host that runs relaxed SIMD loads it; any other host loads `game.wasm`. The browser runtime asks `WebAssembly.validate` about a module of one `f32x4.relaxed_madd`; the native host loads it with `wrela-host --relaxed`.

- **Only presentation code relaxes.** A function relaxes when only the client's `frame` reaches it. What `init`, any other export, a test, a job or a task reaches stays strict, and so does everything an `@deterministic` function or an `@audio` entry point reaches. `init` makes the world that the simulation starts from, so it's strict too.
- **What relaxes:** a vector multiply whose only use is a later add in the same block fuses into one `f32x4.relaxed_madd`, and vector `min` and `max` become `f32x4.relaxed_min` and `f32x4.relaxed_max`. Their results may differ in the last bit, or in which NaN or which zero comes back, from engine to engine.
- **Strict code stays strict where presentation code calls it.** A relaxable function doesn't inline a strict one: the call stays a call, so a `@deterministic` helper that `frame` calls isn't fused there.
- **The simulation's bits don't change.** `game.wasm` has no relaxed SIMD, and a build without the option marks no function relaxable, so it's as it was. Both modules of a relaxed build run the same ticks, so their tick logs compare `equal` (`wrela-host --compare-ticks`). Presentation results reach the simulation only as input, which a tick log records.

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

**What a `const` costs in bytes.** The program's constants are laid out together, as memory holds them, and the WASM carries that layout in data segments. Zero runs cost nothing: wherever 256 zero bytes or more lie together, the segment ends and the next starts past them, since memory is zero where no segment lands. Zeros scattered among other bytes (the high bytes of small integers, a struct's zero fields) stay in their segment; a server's compression takes most of them on the wire. `wrela build <pkg> --report data` lists the twenty largest constants by their laid-out size, with each one's share of zero bytes, and the section's total; a constant a program reads only at load, and that is large, is better shipped as a file (`std::io::Shipped`, below).

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
- Build-time code reads the bytes in place with `bytes(i)`: a constant computed from the value, or a test, which sees the whole value. `bytes` panics in a program that a build shipped. A frame test's build holds the files too, so its program fetches them as a shipped build's does.
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
- **A test's fuel limit is 2³⁷ units,** eight times a constant's: a few seconds of work, so a test can step a simulation for a while. A test of code may set its own, as a constant does: `@fuel(2 ** 40)` on the `@test`, for a check that does more work. On a frame test, `@fuel` is E0108, since each call of a frame test gets a test's fuel. On a tool test it is E0108 too: a tool test has a budget of time instead.
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
- `@test(frames: n, input: SCRIPT)` gives the frames a script of input events: `SCRIPT` names a constant of type `std::input::Script`, in scope of the test. The host queues each event before the frame it is for (a tick test's, as its tick's records). So a test drives a program's UI as a person would; `ui/`'s widgets are tested this way.
  - A script is a constant. `Script::frames()` (or `Script::ticks()`) starts one, and each method adds events at a frame (or a tick): `move_to`, `down`, `up`, `wheel`, `key_down`, `key_up`, `press`, `text`, `compose`, `paste`, `assist`, `field`, `message`, `message_text`, `connection`, `pad_button`, `pad_button_value`, `pad_press` and `pad_axis`.
  - Frames (or ticks) don't decrease. A method panics on what the hosts would refuse, so a bad script fails the build of its constant.
  - A name that isn't a `Script` constant is E0222, and so is a tick test given a script of frames.
  - `input: "path"` reads the script from a file in the package instead, in runtime/abi `input`'s format: a JSON array of events. A tick test's file can be a tick log (§16.2).
- **`@test(frames: n, hold: [("tiles/far.bin", 120)])` holds requests:** the fetches and storage reads of each path are carried out but not answered until the frame it names begins, so a test sees a file come late, as a slow network brings it. A path is held from `init` on, and a frame past the test's last never lets it go. A path that isn't a relative path inside the build, a frame that isn't a whole number, or `hold` on a tick test is E0222.

```wrela
use std::input::{Key, Mods, Script}

pub const PEN: Script = Script::frames()
    .press(2, Key::P, Mods { meta: true })
    .down(5, vec2(150.0, 150.0))
    .move_to(6, vec2(180.0, 160.0))
    .up(7, vec2(180.0, 160.0))
```

#### GPU frame tests and goldens

- **`@test(frames: n, gpu: true)` runs the frames on the native host's GPU.** The host answers each frame's readbacks (`std::gpu::read`) before the next frame. So a test can check what kernels and passes computed in Wrela.
  - The GPU host meters no fuel, as `wrela run` doesn't.
  - A compiler built without the GPU host fails such a test, and says why.
- **A GPU frame test can compare its screen with a golden:** `@test(frames: n, gpu: true, golden: "goldens/frame.png", within: 0.5)`.
  - After the frames, the test compares the 640 × 480 screen with that PNG in the package. It fails if their mean difference, over every channel of every pixel in 8-bit steps, is more than `within`. `within` is from 0 to 255, and 0.5 if the test doesn't give it.
  - GPU results differ a little between devices (§11.3). So a golden is a frame and a tolerance, not exact bits.
  - `wrela test --bless` first writes each golden it runs from this machine's screen. A test whose golden is missing fails, and says so.
  - A golden or `within` anywhere else is E0222: on a CPU frame test, on a tick test, or `within` with no golden.
  - A failure says how far the screen is from the golden, the largest difference in one channel, and how many channels differ. Under `wrela test` (not under a tool's roots), it also writes what it saw and how far each pixel is (each channel's difference times 8) under the package's `build/goldens/`, beside the golden's own path (`build/goldens/goldens/frame.actual.png`, `frame.diff.png`), and names them.
- **`@test(frames: n, gpu: true, hosts: both)` runs the frames in Chrome too:** after the native host's frames, the same build runs in headless Chrome's test mode with the same frames, rate and screen, one worker and the test's script. The test fails unless Chrome's state hash (FNV-1a 64 of every command byte the program submitted, as `wrela-host` prints it) is the native host's and its last frame is within `within` of the native host's (the golden's `within`, or 0.5): the same bits in every host. A program whose commands depend on when an answer arrives (a readback polled into a dispatch, say) differs between the hosts, and such a test says so. `hosts: both` on a CPU frame test, a tick test or a test that holds requests (the browser holds none) is E0222.
- **A golden can be of a pass's target:** `golden: "goldens/scene.png", pass: "scene"` compares the target of the last pass labelled `scene` (`std::gpu::label`) that drew into a texture, its colour, else its depth: the level and layer it drew into, at that level's size. An 8-bit target is compared as it is; a float one's channels are clamped to 0 to 1 (one channel grey, two red and green, alpha opaque unless it has one); a depth target's depth is grey. A label that no such pass had, a target dropped since, or one that holds integers fails the test, and says so. `pass` without a golden is E0222.

#### Tool tests

A tool test checks a tool (§15.5) with fakes for what the tool reaches: its files, sites and programs. `@test(tool)` goes on a function of a package that has a tool build.

```text
use std::tool::http
use std::tool::run

@test(tool, net: ["game.example"], run: [("wrangler", "fakes/wrangler")], budget: 30)
fn deploys_the_release() {
    let Ok(ran) = run::run(run::command("wrangler", "scratch")).wait() else { panic("it didn't run") }
    assert(ran.status == 0)
    let Ok(response) = http::fetch(http::get("https://game.example/")).wait() else { panic("no response") }
    assert(response.status == 200)
}
```

- **The spelling is `@test(tool, ...)`.** A tool test is a test, and `tool` chooses how it runs, as `frames: n` does. So `wrela test` runs it, a filter chooses it, and `tools/check.sh wrela::<package>::<name>` runs exactly it.
- **A tool test is a free function with no parameters and no result,** of a package whose main.wrela exports `tool`. A tool test of another function, or in a package with no tool build, is E0222. No test is a job (E0336). A test of code keeps a constant's effects; a tool test has a tool's, and waits on its calls as a tool does.
- **Its options are named:** `budget: s`, the seconds that the run may take (more than 0, at most 600; 10 if the test doesn't say); `net: ["host", ...]`, the fake sites that it reaches; `run: [("name", "dir"), ...]`, the programs that it may run; `read: [("name", "dir"), ...]`, the roots that it may read, each a directory relative to the test's package (`..` may lead above it; the name is not `scratch`); `browser: true`, Chrome (`std::tool::browser`, as `--browser` grants it); `blobs: "8M,64"`, the blob budget, as `--blob-budget` takes it; and `workers: n`, how many helpers run its parallel work (§15.5), from 0 to 8, 2 if the test doesn't say, so its runs are alike on every machine. Any other option, or a value of the wrong kind, is E0222.
- **`wrela test` builds the package's tool build with the tests as its entries,** in place of `tool`. It runs each test in a `wrela` process of its own (`wrela tool --test`), which leads a process group of its own. Each process uses several cores (its builds, its driven builds, its helpers), so the tests run half as many at a time as the machine has cores, at least 2: one a core would leave a test's budget, which is real time, to the others' load.
- **`wrela test` owns every process group that a test starts.** Each program that the test runs leads a group of its own, and the test's host records each group; a `tool:` program's host records its own programs' groups in the same place. A program starts in its host's group (through `wrela tool --launch`) and takes its own only once its group is recorded, so no group of a test's is ever unknown. A driven build's process (§15.5) leads its own group from its start, runs nothing until its host has recorded the group, and ends at once when its host does. If the test's host can't end them itself (it still runs 10 s after its budget, it is stopped, or it ends without saying how), `wrela test` stops its group, then stops and kills every recorded group, then kills the host's. Ctrl-C on `wrela test` does the same for every test that is running, then ends `wrela test` with status 130. So the run always ends, and nothing that a test started outlives it.
- **Each test's run has only what its attribute gives:**
  - a new, empty write root, `scratch`, which another test never sees;
  - its budget, as the run's timeout, and its blob budget if it gives one;
  - a fake clock;
  - its `net` hosts, as fake sites;
  - its `run` programs, as tool packages;
  - its `read` roots, for reading only;
  - the toolchain and drive services, within those roots;
  - with `browser: true`, Chrome, on the system's clock rather than the fake one, as a live page needs: its timers wait in real time. Chrome's process group is recorded with the test's programs', and Chrome ends with the test's host however it ends.

  It has no other root, no secret, no network, and no browser unless it asks. engine/tests/run-jobs is an example: its tool test builds its own package through the toolchain and drives its ticks.
- **The fake clock moves only when the test waits on the clock alone.** It starts at 2026-01-01T00:00:00Z. The clock jumps to the soonest timer only when all of these are true, and then the timers fire in the order of their deadlines (the order made, for equal ones):
  - every request outstanding is answered by the clock (a timer, or a fake site's delay) or waits for a post to the test's server (`serve::next_post`, which nothing in a tool test posts to);
  - every thread of the test waits: the test's own thread and each helper is idle (it waits for work, and holds none), waits on requests none of which is answered (`wait`, `any`), or joins a tool task that another thread still runs;
  - no task waits for a helper to start it while a helper is idle: that helper is about to take it.

  A thread that runs `std::par`'s work counts as running. While a file, a read of a fake site or a program is in progress, the clock waits. `time::utc_now` reads the fake date, and `std::tool::testing::set_utc` sets it. So a test that waits an hour takes no time, and so does one whose tasks each wait.
- **A fake site serves files from the test's scratch:** `https://<host>/<path>` is `scratch/sites/<host>/<path>`. A path that ends in `/` is its `index.html`. A `GET` or `HEAD` of a file that isn't there is a 404, and another method is a 405. `std::tool::testing::fake(url, f)` changes what the site answers for one URL: `Status(n)`, `Missing`, `Bytes(b)` or `Delay(s)`, a delay in fake seconds, and `Fail(n)`, the next `n` requests failed in transport, as a connection reset fails one (`http::is_transport`). `Clear` takes them back. A fake site keeps a real site's limits: a delay past the request's `timeout` fails the request at the timeout, on the fake clock, and a body is at most 256 MiB.
- **A fake program is a tool package in the test's package:** `run: [("wrangler", "fakes/wrangler")]` grants `wrangler` as the tool build of the package in `fakes/wrangler` (`--allow-run wrangler=tool:DIR`, §15.5). So no fake is written in another language. It runs in a process of its own, on the system's clock.
- **A tool test passes if it returns.** It fails if it panics, if a failed `assert` panics, if it traps, if its budget runs out, or if the host fails. Each failure is E0706, with the host's reason. What it prints is the test's output (`printed` in `wrela test --json`).

#### A workspace's suite

`wrela suite [<name>] [--workspace <dir>]` checks the workspace at or above the current directory (or at `dir`) in one command:

- `fmt`: each member's `.wrela` files are formatted, as `wrela fmt --check` finds them in the member's directory.
- `allow`: each of the workspace's files is one that `[workspace] allow` declares (§3.6).
  - The workspace's files are the files under its directory that its `.gitignore` files don't ignore. The check reads no git index, so it runs the same on a copy of the files, and it checks a file that git shows as untracked too. It reads only the versioned `.gitignore` files: a checkout's own excludes (`.git/info/exclude`, `core.excludesFile`) hide nothing, so every checkout of the same files gives the same answer. It reads the patterns as git does (a trailing `/**` matches what is inside a directory, not the directory), but a `[...]` class as its text.
  - A file that an `exclude` glob matches isn't looked at.
  - A file that no `allow` glob matches fails the check, by its name, unless a `pending` glob matches it. Then the check reports it as pending removal. A `pending` glob that matches no such file fails the check, so the list says what is there.
- `wrela::<package-path>::`: each test of the member at that path from the workspace's directory, as `wrela test` runs them, in a `wrela test` process of its own. These are tests of code, frame and tick tests, GPU frame tests with their goldens (on the hardware GPU), and tool tests, each supervised as `wrela test` supervises it.
- `wrela::<package-path>::<test-name>`: exactly that test (`wrela test --exact`).

How it runs:

- **With no name, the suite runs every check:** `fmt`, `allow`, and each member's tests, several members at a time. It doesn't run the tests of a member that `untested` lists, and says so.
- **A name selects exactly what it names, as `tools/check.sh` reads a name.** A malformed name, an empty one, a second one, a member that the workspace doesn't have, and a test name that no test of the package has, or more than one, are usage errors (status 2). A member's tests that are named fail if it has none.
- **Every check runs, also when another fails.** The output names each test and each check as it ends, then each failure's output, then a summary. The status is 0 if every check passes and 1 if one fails.

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
  - A loop runs its fixed count. A `break` or `continue` under a branch on the input is joined with the other passes, as a branch is: the rest of each pass runs where it didn't leave, and the result holds both. A loop whose own count depends on the input (a `while` on it, or a `loop` that only such a `break` leaves) cannot be bounded, and nor can a recursion that runs under a branch on the input (E0701).
  - Integers are exact when single-valued, and otherwise their type's whole range.
  - The test corpus encloses every sample of 10⁶ boxes per function, on both targets.
- **What cannot be derived (E0700):**
  - code that records GPU work;
  - a function that uses its own derivation (each would need the next);
  - a write to a captured variable that depends on the input, or, for an interval, that runs under a branch on the input;
  - for an interval, a projection (`-> mut T`) whose place a branch on the input chooses: both sides run, so it would be either place;
  - for an interval, arithmetic on `f16`s on the input's path: the widening is `f32`'s, which a half float rounds away.

  Writes and reads through other projections are derived like any others.
- **A branch narrows what its guard tests.** On the side where `a < b` holds (or `<=`, `>`, `>=`, through `!`, `&&` and `||`), `a`'s range ends at `b`'s top, and `b`'s range starts at `a`'s bottom. A strict comparison moves a bound of 0 off it. The narrowing follows back through what the operands were computed from: `let` bindings, a vector's components, `max`, `min`, `+`, `-` and negation.
  - So std's `smin` chooses the smaller distance once, with one branch, and the bound of its gradient on a blend's seam stays between its parts' bounds.
  - std's `cuboid`, written with one branch on inside or outside, has its face's normal as the bound of its gradient on a face.
  - `normalize`'s range is computed one component at a time from the others' squares. So it is exact where the other components are exactly 0, and a vector's `length` is at least its largest component's least magnitude.
- **A table read at an index that depends on the input** (an array's or a vector's element, as a loft reads its radii) gives the hull of the elements that the index's range reaches, held to the array's bounds. The derivation joins them in a loop it makes, so the code does not grow with the array, and a read near a point, whose index's range is a few elements, stays tight. Each place has at most one such index, into an array of known length whose elements hold no enum (E0700 otherwise). An index can be a `u32` or an `i32`.
- **`floor` splits into cases.** Where a component's range crosses one or two integers, its floor is not one integer, and what is computed from it, such as value noise's hashed lattice values, would get its whole range. So the rest of the block runs once for each case of which integer each component's floor is, in a loop the derivation makes, and the cases are joined. A component that crosses no integer, or more than two, is not split. The bound of a gradient then tends to the sampled gradient as the boxes shrink.
- **On the GPU, an interval derivation stays a function** where it is big and takes only scalars, vectors and structs of them without arrays, so nested derivations do not multiply the code. Other calls are inlined, because reading a field's uniform data through a copy is slow.

### 14.3 Known limits of the intervals

- A NaN bound becomes infinite on the CPU, but not on the GPU, which may assume NaNs away.
- WGSL bounds `sin` and `cos` only on [-π, π]; outside that range, the same absolute error is assumed.
- A branch run speculatively can still trap on the CPU (an integer overflow, for example), even if no point in the box would take it.
- std's `ellipsoid` bound divides by a length that is 0 at its centre. So a box that holds the centre gets an interval that reaches 0, deep inside the body.

**Where a half float would do (`std::derive::precision`, `wrela precision`).** An interval can't be derived in `f16` (E0700): its widening is `f32`'s, which a half float rounds away. `precision(f, over)` doesn't derive one either. It runs `f`'s interval over the box twice on the CPU. The first walk widens each operation as an `f32` rounds. The second widens it as a half float rounds: by `max(|x| k 2⁻¹¹, 2⁻¹⁴)` for an operation of `k` ulps in std's table, `2⁻¹⁴` being the smallest normal, below which a GPU may flush. Its input box is rounded outward to the half floats' grid first, as a half float's input would be (0.9999 is 1). Each walk notes every operation's range through `precision_note`.

- `wrela precision` reads the notes. For each operation, it compares the half walk's extra width `e` with the value's magnitude `|v|`. The operation is `safe` where `e ≤ tol × max(|v|, 1)`, `watch` within ten times that, and `unsafe` past it. A value past 65504 is `unsafe` by range, and so is an integer past 2048, the last integer a half float holds exactly.
- **The extra width accumulates.** An operation's verdict counts what the operations before it rounded too. So a subtraction that cancels two large values is `unsafe` even where its own rounding is small.
- **Constants are taken as they are.** A constant or a captured value is exact in both walks, so one that a half float can't hold (`1000.1`) is the reader's to see.
- **Its verdicts are advice.** The half walk is a model of the rounding, not a bound. A twin written in `f16` where the report says `safe` is proven by measuring it.

### 14.4 Fields built at run time

A field built while the program runs is a tape: `std::stage::Tape` holds up to 4,096 operations over the point and a table of up to 2,048 constants (`TAPE_LEN`, `TAPE_CONSTS`); `ShortTape` holds 256 of each, for a pruned tape. Both are `Taped<N, K>` of their sizes, so code written for one takes the other. Each operation writes its own register, and the last register is the field's value.

- A program builds a tape while it runs, from an edit log or a sculpting tool: `let d = tape.sphere(center, r)`, `tape.smooth_min(a, b, k)`, and arithmetic over registers.
- The operations: the point's coordinates and constants; `+ - * /`, `min`, `max`, `smooth_min`, `-`, `abs`, `sqrt`, `sin`, `cos`, `exp`, `log`, `pow` (to a constant power), `floor`, `fract`, `saturate` (`clamp01`), `a - b * floor(a / b)` (`modulo`); `step` (1 where `a < b`), `select` (`b` where `a > 0`, else a third register) and `mix` (`a + (b - a) * t`), the ternary ones reading a third register; a table read at a computed index (`load`, clamped to the table's entries) and between two entries (`lerp_load`); `std::field`'s own value noise (`noise`), fractal noise (`fbm`) and lattice hash (`hash`), so a tape and a compiled field agree bit for bit; `dot3`, `length3` and `normalize3` over three registers in a row; and `noise_box`, `std::field::value_noise_box` (value noise averaged over a box, a filtered field's), over two such rows, the point and the box's half widths. A box noise's interval is [-1, 1] (`tape.interval`), and an interval derived through the tape (a `Taped`'s `Surface::interval`) is every number's where a box noise reaches the value (its loops run as often as its box is wide). Each is one line of `interpret`'s step, and each matches the std function it stands for within an ulp at 10⁶ points on both targets (bit for bit, on the M4 and in Chrome).
- `stage::interpret(tape, p)` evaluates a tape. A `Taped` is a `Surface`, so it goes wherever a compiled field goes, on the CPU and the GPU: a `ShortTape` as a uniform-sized block, a `Tape` (72 KiB, over the 64 KiB a uniform holds) as read-only storage. On the M4 a 1,024-operation tape costs 0.32 ns an operation a point on the GPU.
- Nothing about a tape is special to the compiler: its gradient and interval are derived from `interpret`'s own code.
- Integers on a tape are two registers in a row, a 32-bit integer's high and low 16 bits, each exact as a float, so a tape hashes the point's lattice cells exactly, as compiled code does: wrapping `+ - *`, `& | ^`, shifts, `/` and `%` (0 where they'd trap), `<` and `==` (1 or 0), `min` and `max` of `u32`s and `i32`s, conversions between them and floats, a multiply-add (`IMAD`) and `std::hash::pcg`, PCG's output step, which `hash_u32` ends with (`integer`, `integer3`, `integer_constant`). An operation that gives an integer is two operations, one for each half. `atan2` is one too.
- **A tape of a compiled field.** `std::derive::tape(f)` derives, from the body of `f`, a function of the point, the function that builds `f`'s tape, and runs it on the CPU: the point is active, and each operation on what depends on it puts its tape operation on the tape, while everything else (a part's placement, a radius, a seed, a loop's count) runs as ordinary code and reaches the tape as constants. `Surface::to_tape()` is `tape(|p| self.distance(p))`, and a `Taped`'s is itself. `tape_with(f)`, of an `f` that returns `(f32, T)`, puts the value and `T`'s floats (scalars and vectors of `f32`) in the tape's last registers, which `interpret_with::<N, K, M>(tape, p)` reads as `(f32, [f32; M])`: a field's channels travel with its distance. `tape_sized` builds into a `Taped<N, K>` of another room.
  - It holds a branch on the point by running both sides and selecting what each left (`select`), an early `return` and a loop that leaves early on the point as the interval derivation does (one exit, and a loop's own count with flags, §14.2), a loop of a count known when the tape is built by running it, and a read at an index that depends on the point by copying what it reads from into the tape's table (`load`; an integer's table is two, its high and its low 16 bits). `std::field`'s `smin`, `value_noise` and `value_noise_box`, and `std::hash::pcg`, are one operation each, and `lattice_coord` is `F2I`, which wraps past `i32`'s range as it does; a sum of an integer product is a multiply-add.
  - The tape is built in a `std::stage::Building`, which finds each operation the tape has already (every operation is a function of its registers, so two of the same give the same value: the field's repeated constants, and the corners of a noise's cell that share a lattice coordinate), and `finish` drops what the value doesn't use.
  - What it can't hold is E0709, at the code: a loop whose count depends on the point, a write at an index that depends on it, a 64-bit number, a float's bits. A tape past its room panics with its counts.
  - Every corpus field's tape agrees with the field within 3e-6 at 10⁶ points on the CPU and within 1.2e-6 on the GPU (relative past 1; a sphere trace's stop flips at 0.04% of points, where the GPU rounds the two differently), and its gradient, derived through `interpret`, within 5e-7. The herd's grazer tapes in 1,131 operations; a 20-section tube with fractal displacement in 1,067.
- `tape.interval(over)` is the tape's interval over the box: every value it takes there lies within it (the bound `prune` reads), so a realizer culls a block by a tape as by a compiled field.
- `tape.prune(over)` is interval pruning. Each `min`, `max` or `smooth_min` whose operands' intervals over the box do not overlap (intervals that only touch overlap) becomes the operand it always picks; so does a `select` whose condition has one sign over the box, and a `step` whose operands' intervals are apart becomes its constant; then the tape drops what the value no longer uses. So the pruned tape gives the same values and gradients inside the box with less work. Each operation's interval is `interval` of its step over its operands' intervals; a dot product's (six registers, more than a step's box) is the tape's up to it.
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

Three hosts run builds, from the same ABI (`runtime/abi`), and the native host has a profile for tools:

| Host | What it is | What it runs |
|---|---|---|
| The browser host | A page in Chrome, on WebGPU (`runtime/browser`) | A client build |
| The native host | `wrela-host`, on wgpu and wasmtime (`runtime/native`) | A client build, on the machine's GPU; or its CPU alone (`--no-gpu`, and the frame tests of §13.2), which checks commands but runs no shaders |
| The server host | `wrela-serve` (`wrela_host::server`) | A server build's rooms (§15.3, §19) |
| The native host's tool profile | `wrela tool` (`wrela_host::tool`), on wasmtime | A tool build, with what its command line grants (§15.5) |

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

**What a build states about its module.** A client build's `manifest.json` carries `wasm_abi`: the module's imported memory's limits, each import as `[module, name, type]` and each exported function as `[name, type]`, in the module's order (a function's type is written `(i32, i32) -> ()`, another import's is its kind, `memory`). The browser host checks the module against it rather than reading the module's bytes, so it compiles the WASM as it streams in (`WebAssembly.compileStreaming`, which needs the server to send it as `application/wasm`; any other type is read whole, then compiled), and a second open of the same build finds the compiled code in the browser's code cache. A build that `wasm-opt` rewrote (`--optimize`) states the rewritten module's interface. A relaxed-SIMD build states its second module and that module's interface too, as `wasm_relaxed` and `wasm_relaxed_abi` (§11.2).

### 15.3 Client, server and tool roles

A program has a role for each host that runs it: a client build that a browser host runs, a server build that a server host runs, and a tool build that the native host's tool profile runs.

- **Client:** a browser host runs the client build, through `init` and `frame` (§15.1).
- **Server:** a server host runs the server build. It calls `serve_init() -> S` once, when it starts the program, and `serve(state: mut S, time: f32)` when something happens. `serve` reads what happened as events. Without state, it is `serve(time: f32)`.
- **Tool:** the tool profile runs the tool build. It calls `tool() -> u32` once, and its result is the exit status (§15.5). Another signature is E0703.

A program that exports `frame` has a client build, one that exports `serve` has a server build, and one that exports `tool` has a tool build; a program with several has each. `wrela build` writes the client's files, the server's module as `server/game.wasm`, and the tool's as `tool/tool.wasm`.

- Each build is lowered from its own entries, so no module holds another role's code.
- The server's module has a memory of its own, not shared, because it runs on one thread and its host caps the memory. The tool's module imports its memory, shared, as the client's does, so that its helpers run `std::par`'s work on it (§16.1). It may grow to 4 GiB.
- The client's and the server's modules carry the program's pair id, in the custom section `wrela.pair`: a hash of both modules. A client gives the pair id with each connection, and a server refuses a client of another pair. A tool build has none.
- `@client`, `@server` and `@tool` (§9) mark code that only that role's build may run. std's screen passes, `present`, `std::audio::play`, `std::tick::start` and `std::host`'s services are `@client`, and `std::tool`'s services `@tool`. Neither a server build nor a tool build records GPU work.
- Reaching another role's code from a build's entries, through calls, generics or closures, is E0708, with the call chain.

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

### 15.5 Tools

A tool is a program for the work around a program: builds, captures, releases, checks. The native host's tool profile runs its tool build (§15.3), with only what its command line grants:

```text
wrela tool <package> [--read NAME=DIR]... [--write NAME=DIR]... [--secret NAME]...
           [--net HOST[:PORT]]... [--allow-run NAME[=PATH|=tool:DIR]]... [--browser]
           [--timeout S] [--workers N] [--blob-budget B] [-- args...]
```

**The host calls `tool` once.**
- `tool() -> u32` runs the whole tool, on the tool's thread, and its result is the tool's exit status, from 0 to 255. That status is the process's.
- A tool build has its own thread and helpers, and no GPU, ticker or voice. Its memory may grow to 4 GiB. A call has no deadline: a tool may compute for a long time between requests.
- **Helpers run its parallel work and its tasks** (`std::par`, §16.1; `std::tool::spawn`, §16.2), as a client build's workers do: each is an instance of the module on the tool's memory, on a thread of its own, with the tool's host functions. `--workers N` sets how many, from 0 to 8; by default, one fewer than the machine's cores, at most 8. With none, the tool's thread runs every chunk and every task. A parallel job's result is the same with any number.
  - The helpers are the run's. `--timeout`, Ctrl-C and the run's end stop their calls as they stop the tool's, within about 10 ms: the first thread that sees the reason stops every other, so a call that waits for a helper's work ends when the stopped helper does.
  - A helper's trap ends the run with the helper's message (status 70), whatever the tool's thread does then, as a trap in the tool's thread does.
  - When the tool ends, its helpers stop with the same grace as the workers. A helper still running after it makes the run the host's failure (status 71).
- `--timeout` is a budget for the whole run (status 124); a time that the clock cannot reach is a usage error (status 2). Ctrl-C stops the tool (status 130), also inside a call: a thread that computes, and a thread that waits on a call, a task or the first of several calls. A reason to stop that is already there wins: over a status that `tool` returns, and over an answer that a wait finds ready; so an interrupt or a budget spent before the tool runs (while it compiles) stops it before it starts. A trap, a status that is not 0 to 255, or a wait on nothing (`any` of no calls, or of a request whose answer was read, also by another thread while it waited) gives status 70. If the host cannot run the tool, or cannot stop it (it cannot take Ctrl-C, or cannot start the thread that interrupts calls), the status is 71, and no Wasm runs.
- `print` never waits either. The host puts each line on a queue (at most 1024 lines and 4 MiB), and a thread of its own writes them out. A line that finds the queue full is dropped, and the host prints how many it dropped when the console takes lines again. When the tool ends, the console has the same grace as the workers to take what is queued; lines it does not take are dropped. So a console that nobody reads, such as a full pipe, cannot stop `--timeout` or Ctrl-C, and the host's last message cannot hold up the end.
- Services run on the host's worker threads, beside the tool. The workers are bounded: 4 for files and blobs, 4 for the network and programs, 4 for the compiler and images (so at most 4 builds at once), and one per core, from 4 to 16, for driven builds' work (so a tool that drives several instances of a build runs that many at once).
- However the tool ends, the host stops its workers and releases all that the tool held. Work that has not started is dropped, and work in progress has a grace of 5 s. A worker that waits for a program kills the program's process group, one that waits for an HTTP response shuts its connection down and waits for its client's thread, and one that waits for the toolchain or an image cancels its work. Every driven build's process group is killed, and then each build and each server is closed, with the same grace: a server's listener and connections are shut and its threads joined, so its port is closed when the run ends. If a worker is still running after the grace, the run is the host's failure (status 71), whatever the tool's own end was, and `wrela tool` ends the process, so no work outlives it. No file work holds the lock on the tool's blobs, so their release never waits on a file.
- A resource that a request makes belongs to that request until the tool reads the answer. If the tool ends first, the host releases it with the rest. The host keeps compiled code only in its code cache (`drive`, below), outside every root that it grants.

**A tool waits for what it asks.** A tool has its own thread and no frame, so nothing in it must not stall (§17.1 holds for clients and servers). Each service's function makes its request at once, and gives a `Call<T>`, `T` being what the service gives:
- `call.wait()` blocks the thread until the answer comes, and gives a `Result<T, ToolError>`: what the service gives, or the host's reason for its failure. `ToolError::new(message)` is a failure of the tool's own, so a tool's work can fail with one type, and `?` passes both up. A stop of the run (`--timeout`, Ctrl-C, a helper's trap, stuck work) ends the tool there instead: it can't catch the stop, and the host says why.
- Requests made before a `wait` are already being carried out, so `let a = build(x)` then `let b = build(y)` builds both at once, whichever is waited on first.
- `std::tool::any(requests)` blocks until the first of several requests is answered, and gives its index. `call.request()` gives a call's request, a `Copy` number; the other calls are untouched, and each still waits for its own answer. So one thread answers events as they come: a served post, a timer, a rebuild.
- A `Call` dropped without `wait` leaves its request to finish. Its answer, and what the request made, is released when the tool ends.
- `std::tool::spawn(input, f)` runs `f(input)` as a tool task, and `std::tool::all(inputs, f)` runs `f` on each input as a task, as many at once as the helpers run, and gives the results in the inputs' order (§16.2). A task's waits block only its own thread, so several sequences of calls go on at once.

```wrela
// A tool that reads two files at once, waits a moment, and exits with their sizes.
use std::tool::files
use std::tool::time::after

pub fn tool() -> u32 {
    let a = files::read("src/a.txt")
    let b = files::read("src/b.txt")
    let _ = after(0.1).wait()
    let Ok(first) = take a.wait() else { return 1 }
    let Ok(second) = take b.wait() else { return 2 }
    (first.len() + second.len()) % 256
}
```

A request's deadline counts from when the tool makes it (below), so a call that is waited on late has only what is left of its deadline. A task that waits on work that another task must do (a post that another task's page makes) can wait until its deadline when no helper is free to run the other; such a tool orders that work on one thread.

**Grants.**
- Grants come only from the command line, and from a workspace's `[tools.<name>]` (§3.6). The host opens them once, when the tool starts, into one capability context that every service gets. A request outside the grants fails, and the tool continues.
- `--read NAME=DIR` grants the directory as the root `NAME`. `--write NAME=DIR` grants it for writing, and for reading too. A root that is not a directory, a name that is granted twice, or a root at or above the code cache's directory (`drive`, below), stops the command before the tool starts (status 2).
- A path is `NAME/relative/path`, with `/` between its parts and no part empty, `.` or `..`. `NAME` alone is the root itself. The host opens each path relative to its root's directory handle, one part at a time. So no path or symbolic link leaves the root, also a link that replaces a directory after a check.
- The host opens a file without waiting on it, and the opened handle must be a regular file. A FIFO, a socket or a device fails the request at once, by the type of the handle, not of the path. The host never writes a file that has another name (a hard link): its other names would change too, so such a write fails.
- `--secret NAME` reads the environment variable `NAME` when the tool starts. `std::tool::secret(name)` gives it, and gives `None` for a name that is not granted. The host never shows a secret.
- `--net HOST` grants a host to `std::tool::http`, over HTTPS. The URL's host must be the grant exactly: `--net HOST:PORT` grants that port, and a URL with a user is refused.
- `--allow-run NAME` grants the program `NAME` to `std::tool::run`. The host finds it on `PATH` when the grants are opened and keeps its absolute path, so a later change to `PATH` does not change which file runs. `--allow-run NAME=PATH` names the file itself. `--allow-run NAME=tool:DIR` names the tool build of the package at `DIR`, which this `wrela` runs as `wrela tool`.
- `--browser` grants `std::tool::browser`: the host may start Chrome stable for the tool. Chrome is `WRELA_CHROME` if that is set, else the macOS app or the first Chrome on Linux's `PATH`. `WRELA_CHROME_ARGS` adds arguments to an unrestricted launch; a browser held to network grants refuses extra arguments, since Chrome flags can bypass its proxy.
- **`wrela tool <name>`** runs the tool that the workspace at or above the current directory declares as `[tools.<name>]`: its package's tool build, with the grants that the workspace declares, each path from the workspace's directory. A word that names a declared tool is the tool; any other argument is a package's directory.
  - **The command line can only narrow the declared grants.** If it grants nothing, the run has the declared grants. If it grants anything, its grants are the run's grants instead, and each must be within a declared grant: a root at or below a declared root of the same name (a declared write root, to write), a program as it is declared (`--allow-run NAME` alone keeps the declared one), and a host, a secret or `--browser` that is declared. `--timeout` can only shorten a declared `timeout`, and `--workers` can only lower a declared `workers`. Any other grant stops the command before the tool starts (status 2). `--blob-budget` is a limit, not a grant, and the command line sets it as for a package.
  - Each declared root is opened from the workspace's directory one part at a time, following no link, and a missing write root is made the same way. A narrowed root is opened from its declared root's directory the same way. The run gets these opened directories, not their paths opened again: so a directory replaced after the check, by a link or anything else, leads nowhere new, and nothing outside the workspace is made or granted.
- **A granted program has the user's authority.** It runs natively, as any process does: it can reach what the user can, and `npx` runs code that it downloads. Its working directory is where it starts, not a confinement. Grant only the programs that the tool needs.

**The services** are `std::tool`'s, and each is `@tool`. Each function but `args` and `secret` makes a request and gives its `Call`, of the type named here:
- `args()`, the arguments after `--`, and `secret(name)`. The host gives both when it starts the tool, as the bytes that `kept` reads (`runtime/abi`), so neither is a request.
- `files`: `read(path)` (the bytes, a `Vec<u8>`) and `write(path, bytes)` (`Done`) for whole files of at most 512 KiB (`write` makes the directories it needs). A write replaces the file whole: a reader sees the old file or the new one, and a write that fails leaves the old one. `list(dir)` gives each entry's name, kind and size (a `Vec<Entry>`), and `stat(path)` the kind and size (an `Entry`). `remove(path)` removes a file, a link or an empty directory, and `remove_all(dir)` a tree; both need a write root and never remove a root. `make_dirs(path)`. `read_blob(path)` (a `Blob`) and `write_blob(path, blob)` move a file of any size. What gives nothing gives `Done`.
- `blob`: bytes that the host holds for the tool. `new()` (a `Blob`), `append(b, bytes)` and `read(b, offset, len)` (a `Vec<u8>`) move at most 512 KiB at a time; `len(b)` (a `u64`); `release(b)`. While `write_blob` writes a blob, an append to it or its release fails, so its bytes stay counted. A tool holds at most 2 GiB in at most 4096 blobs. `--blob-budget` changes this: `64M` for bytes, or `2G,16` for bytes and handles. A request past the budget fails, as other requests fail. The host releases every blob when the tool ends.
- `time`: `after(seconds)`, a timer (`after(s).wait()` sleeps), and `utc_now()`, the seconds and nanoseconds since 1970 (a `Utc`). Both are requests, so a timer can be waited on with other calls (`any`).
- `http`: `fetch(Request { method, url, headers, body, timeout })` gives a `Response { status, headers, body }`. Only `https://` URLs reach a host, and only a granted host. The tool can't set the `Host` header, or another that names where the request goes or how its body is framed (`Content-Length`, `Transfer-Encoding`, `Connection`, a name starting with `:`): such a request fails, so the grant is the host that the request reaches. The host follows no redirect: the tool gets the response, with its `location` header. A body over 512 KiB is a blob (`blob::Data`), in both directions, and a response's body is at most 256 MiB. A request has a deadline of 30 s, or the `timeout` that it gives, at most 600 s. A request that gets no response fails **in transport**: the connection fails, is refused or is reset, or the deadline passes. Its reason starts with `transport: `, and `http::is_transport(e)` says so: a retry may succeed. Any other failure is the request's own, and a response is not a failure, whatever its status.
  - The host makes the connection, and keeps a handle to its socket. Past the deadline, or when the tool ends, it shuts the socket down, so the client returns, and it waits for the client's thread before it answers. A client that still hasn't returned the grace after it was cancelled (a name lookup, which no socket cuts) ends the run at once as the host's failure (status 71), with or without `--timeout`. No connection or thread of a request outlives the run.
- `run`: `run(Command { program, args, stdin, env, secrets, cwd, timeout })` runs a granted program, and gives a `Ran { status, stdout, stderr }`. There is no shell. `cwd` names a write root, where the program starts. Its input and outputs are bytes, or blobs over 512 KiB, and each output is at most 256 MiB. The status is the exit code, or minus the signal that ended the program.
  - The program gets an empty environment and a fixed baseline: `PATH` as it was when the grants were opened, `HOME` a directory that the host owns for the run (and removes at its end), and `LANG=C.UTF-8`. Then it gets the request's `env`, and the granted secrets that the request names in `secrets`, each as a variable of its name.
  - The program leads a process group of its own. The host kills the whole group when the program ends (so nothing that it started outlives it), past its deadline (60 s, or the `timeout` that it gives, at most 600 s), past 256 MiB of output, and when the tool ends.
  - A `tool:` program runs as `wrela tool <DIR>`, with the request's `cwd` as its one write root (by the same name), its arguments, its named secrets, and the deadline as its `--timeout`.
- `toolchain`: the compiler's own work, in the host's process, on a package in a root (`NAME/rel`). `build(pkg, out, options)` writes into `out`, a directory in a write root, the same files as `wrela build -o`, byte for byte (a `Built`: whether it has no errors, the files, the diagnostics, the constants computed and cached). Its options are `optimize` (through `wasm-opt`), `lift` (packages whose literals are lifted, §23), `overrides` (a dependency's name and another package to read for it, wherever a manifest declares it: a program a tool writes around another package), `simd`, `debug` and `testing` (a test build, with the program's `@testing` exports, as `wrela build --testing`). `out` follows `wrela build`'s rules: a symbolic link at the path of an output or of `.wrela-build` stops the build before anything changes; the files that `.wrela-build` names and that a build writes (no other file) are removed if this build does not write them; and a file that no build wrote stops the build. The host writes each output through the root's handle, one part at a time, following no link. If the request fails, or is cancelled while it writes (its last write too), the host removes each file that it opened to write, also one that it wrote part of, and then each directory that it made, so a retry finds nothing in its way. `doc_export` writes its pages the same way. `test(pkg, filter)` runs the package's tests as `wrela test` does (a `Tested`: each test's name, place, result, message and printed lines); a tool test does not run there, and fails, saying so. `check(pkg)` gives the diagnostics (a `Checked`), each with its code, message, place and rendered text, and the pipelines that the package's build makes, as `wrela pipelines` counts them (§18.9; each a `Pipeline`: `compute` or `render`, its entry points with their type arguments, and where CPU code dispatches or draws it), so a test can assert what a program reaches. `test_with(pkg, filter, overrides)` and `check_with(pkg, overrides)` read dependencies as a build's `overrides` say, so a tool can test or check a package it writes around others. `fmt(paths, check)` formats the `.wrela` files at the paths, or with `check` writes nothing and gives the files that are not formatted (a `Formatted`); a path that cannot be read, or a directory with no `.wrela` file, fails the request. A root itself (`NAME`) is a package or a directory, as a path in it is. `doc_export(packages, out, repository, revision)` writes the reference docs, as `wrela doc --export` does (a `u32`, the files it wrote). `query(pkg, queries, overrides)` answers `wrela query`'s queries (§23.9) with one check, as `wrela query --json` does (its JSON, a `String`), reading dependencies as a build's `overrides` say. A program's errors are in the answer; the request fails only for what the host cannot do.
  - **A lifted build's literals are a tool's to read and write** (§23.1, §23.5). `literals(program, build)` gives the literals of the lifted build in `build` (by its `lift.json`): their values as the build has them, their values in the files it lifted as they are now, and those files' root paths (a `Literals`); the values are `None` if a file has changed past its literals, as a hot reload finds (§23.2). `edit(program, build, edits)` writes new values of those literals, each by its index in the build, into the files as they are now, through `wrela edit`'s code (§23.5): a literal keeps its form, and nothing is written if a file has changed past its literals or `wrela edit` refuses. The answer is `wrela edit --json`'s, on one line, or the refusal (a `String`). The report names each file by its path from the program's package; the host resolves it within the roots, and reads and writes it through its root's handle one part at a time, following no link. An edited file must be in a write root, and the host replaces it whole: a new file beside it takes its place, after the host has checked each file's hash again. An edit cancelled at its deadline or the tool's end replaces no more files: the host looks before each file, and right before each new file takes the old one's place.
  - **The compiler reads only through the grants.** Every file that it reads goes through the roots' directory handles: the package's sources and manifest, its dependencies' (a dependency outside the roots is an error, E0220), the workspace's manifest and its members' (a search for the workspace stops at the top of the roots), the files that it embeds, and a GPU frame test's golden. It keeps its constants' cache in a directory of the host's own (`WRELA_CONST_CACHE`, else `~/.cache/wrela/consts`), never beside a package; `wrela tool` keeps the tool's own build's cache there too.
  - **The work stops with its request.** It runs on a thread of its own. Past its deadline (300 s, or the `timeout` that it gives: at most 600 s, or the run's `--timeout` if that is longer, and in a run with a `--timeout`, a longer one is cut to that), or when the tool ends, the host cancels it, and the compiler stops at its next phase (loading, checking, constants, lowering a role, emitting). A constant that is being computed stops within about 10 ms, on each of its threads, and no other constant starts. `wasm-opt` runs in a process group that the host owns and records for a supervisor, and the host kills the group at once. If the work does not stop within the grace, the run is the host's failure (status 71).
- `drive`: a build on the native host, beside the tool, in a process of its own. `load(build, options)` loads the build in a root's directory (a `Loaded`: a `Build` handle, how long the load took, and where its code came from, a `Code`: `Compiled`, `Cached` or `Reused`), with a real GPU (`drive::gpu()`, never software rendering) or on the CPU alone (`drive::cpu()`), its screen's size, its frames' rate, its workers, its GPU timing, how long each helper holds a job's result back (`hold`, in microseconds, as `CpuHost::hold_jobs`), and whether it records the commands it submits (`record`). It does not run `init`. Then `steps(b, list)` runs a list of at most 1024 `Step`s on the build, in order, in one request, and gives what they saw (a `Seen`); `run(build, options, list)` loads the build, runs the steps and closes it, in one request (the load within its `timeout`, then the steps within the request's).
  - **Steps that run code**: `Init`, the build's `init`, once, before any other; `Frames(n, script)`, the next frames in lockstep with the ticker, as `wrela-host` runs them, with a `std::input::Script`; `FrameAt(time)`, one frame at a time that the tool gives (a camera's, a light's or a jitter's), outside lockstep, waiting for no ticks; `Ticks(n, script)`, ticks alone on a CPU build, as `wrela-host --no-gpu`; `Call(export, args)`, an export with scalar arguments (§15.2); and `Audio(quanta, timed)`, the voice's next quanta rendered offline (§16.4): the samples that the browser host's AudioWorklet renders, `f32`s with the channels interleaved, as a blob, and with `timed` each quantum rendered alone and its seconds, as another (an `Audio`). Each of the first five gives a `Ran`: when it started (seconds since the load), how long it took, when each frame or tick started and ended (a `Span`), and an export's results. **Steps that observe**: `Capture`, a GPU build's screen, RGBA8, as a blob (a `Capture`); `Hash`, the state hash (a `u64`); `Phases`, the timed phases (a `Vec<Phase>`); `Buffers`, the handles of the GPU buffers that a GPU build holds, oldest first (a `Vec<u32>`); `ReadBuffer(handle)`, one's bytes, read back from the GPU, as a blob (a `Data`); `Commands`, with `record`, the commands that the build submitted since the last `Commands`, decoded: each one's opcode's name (`Draw`, `Dispatch`, `Label`, ...), a label's name, the name of the pipeline that a dispatch or a draw uses, a dispatch's workgroups, the resources that it binds (each one's handle, offset and size), what a host-facing command names (a storage path, a fetch's or a post's URL, a host request's service, a build's path) and the bytes that a storage write, a post or a host request carries, each batch begun by a `Batch` of its own, as many as the answer has room for, at most 65 536 (the rest wait for the next `Commands`; a command that no answer holds fails the request and is dropped) (a `Commands`); and `Running`, the root path of the build that runs (a `String`). **Steps that hold requests**: `Hold(path)`, the build's fetches and storage reads of `path` held from then on, each carried out but not answered, so the program sees a late file; and `Release(path)`, the path let go, its held requests answered from the build's next call on. They give nothing.
  - **What steps saw** (`Seen`) has a field for each kind, its entries in the order of the steps: `ran`, `captures`, `hashes`, `phases`, `audio`, `buffers`, `bytes`, `commands` and `running`. The build keeps every line that it prints during the steps, up to 256 MiB of them (as a program's output is bounded); `dropped_lines` counts the lines past it. If they all fit the answer, they are `lines` (a line of more than 256 KiB cut, with a note); else they are whole in a blob, `log`, each followed by a newline, which the host reserves in the blob budget before its bytes are sent (one that does not fit fails the request), and `lines` is empty. `lines(mut seen)` gives every line whichever way it came, and releases the blob, so its bytes go back to the budget. `trap` is the build's first `Trap`, in this request or an earlier one: the host's message (with the tick, for a trap in one), the panic's message, the source place, what it happened in (`init`, a frame, a tick (also one that a lockstep frame ran), a call, or the voice), the number that the host gives the frame or the tick, and the index of the step that it ended. **After a trap, the steps that run code are skipped, and the steps that observe still run**, in that request and in every later one: a build that trapped stays alive, and can be observed. A step that can't run (`init` twice, a step before `init`, ticks on a GPU build, an export that isn't there, a screen of a CPU build, no voice) fails the request, saying which step.
  - **One request is bounded.** A request has at most 1024 steps; one with more fails before any runs. Its answer's inline part (all but its blobs) is at most 512 KiB: the build's process counts each step's bytes as it collects them (a `Frames` or `Ticks` step its frames' times before it runs them), and a step past it fails the request, saying which. Every blob (a screen, a buffer's bytes, a voice's samples) is reserved in the blob budget, under the request, before its bytes are read back or rendered: the process asks the host, which reserves it or refuses, so a blob that does not fit fails the request without being read. A request that fails gives back every blob reserved for it, and a blob reserved for a step that a trap ended is given back when the request ends.
  - **A driven build's posts go to the tool** (`std::io::post`, §17.1). `posts(b)` gives the posts that the build made and the tool has not been given, oldest first, each with its number, URL and body (a `Vec<Post>`). `answer(b, post, bytes)` answers one, and `fail(b, post, why)` fails it; the program reads the answer from its next call on. Each post is given once and answered once: another answer fails. A build holds at most 64 posts unanswered, each of at most 512 KiB; past either, the post fails at once. A post that nobody answers waits until the build closes, and goes with it. `close(b)` closes the build.
  - **A driven build is its load request's.** The host registers it under the request before the answer that names it, and closes it if the request fails or the tool ends first. It closes every driven build when the tool ends. One request runs at a time on a build: another fails while one runs.
  - **Each driven build runs in a process of its own**: this `wrela`, run again as `wrela tool --drive`, with the same host as `wrela-host` (so the same frames give the same hash). The host opens the build's directory once, at the load, through the root's handle, and its storage (a directory of the host's, removed with the build, unless the load names one: `storage`, a directory in a write root, which stays, so another load of the build reads what it stored), and gives the process those directories, and the root's, open. The process opens nothing by a path: it reads the build's files and the program's fetches, and writes its storage, through those directories alone, one part at a time, following no link, so a link put in place of a directory later leads nowhere. A GPU build can hand over to another build (§15.4): the process resolves the path that the program names from the running build's root path, refuses one that leads out of its root, and opens the other build through the root's directory as the host opened the first. Commands and answers cross a pair of pipes, each message announced by its size, so the host checks a blob's bytes against what it reserved before it reads them.
  - **The process leads a process group of its own**, which the host records for a supervisor (§13.2) before it gives the process its first command. The process ends at once when its commands' pipe ends or fails, whatever the build is running (WASM in a loop, a join, a large answer half written), so a host that dies, however it dies, leaves no build's process; on Linux it also asks to be killed when its parent ends. Only the process holds its pipes' other ends.
  - **A request's lines are its own.** Those that a failed request printed go with its failure, if they fit an answer, and the next request starts with none and its whole quota.
  - **The host stops a build by killing its process group**, never by interrupting its calls: a kill stops every thread of the build at once, also one that waits in a join or writes a large answer, and leaves nothing of the build half running. The host signals and reaps the process in one place, and never signals its group once it's reaped (its number may be another process's by then). Closing a build doesn't wait for work that its host thread is still doing for it (compiling its code, say): that work ends with the run, within the grace, or the run is the host's failure (status 71). It does so past a request's deadline (60 s, or the `timeout` that it gives, at most 600 s, from when the host takes the request), at Ctrl-C or `--timeout`, and when the tool ends. The request fails, and the build can't be observed after it: a later request fails, saying so.
  - **The host compiles each build once in a run.** It compiles a build's WASM, by its SHA-256, for the engine that the process runs, keeps the code for the run, and gives it to every process that loads the build or hands over to it. Between runs it keeps the code in the code cache (`WRELA_CODE_CACHE`, else `~/.cache/wrela/code`, mode 0700; `off` keeps none), each file named by the WASM's SHA-256 and the engine's settings, as it keeps the tool's own build's code. It opens the cache's directory once, and reaches each file through it by its name alone, following no link; it reads back only a regular file of the user's own with no other name, and removes any other entry by that name (the code is compiled again). It writes a new file of its own, made then and there, which then takes the name's place. **No root can reach the code cache**: a grant (read or write, declared or on the command line, a tool test's roots and scratch too) whose directory is at or above the cache's stops the command (status 2), each directory known by its device and inode, links followed. So no tool can write code that a later run reads back.
- **Work that does not stop is the host's failure, after the tool has its say.** When a service's work does not stop within the grace of its cancel, the request fails, and the tool has one more grace to read that answer and end on its own; then the run ends as the host's failure (status 71), whatever the tool's end was. So what the tool prints about the failure comes before the host's message.
- **Deadlines count from the request.** A request's deadline starts when the host takes it, as the tool makes it. Work that is still queued at its deadline (every worker is busy) fails without starting, and work that starts later has only what is left. HTTP, programs and servers wait on their own workers, and the toolchain, driven builds and images on others, apart from the workers for files and blobs, so a slow network, program or build never holds up a file, and a build never holds up the network.
- **Bytes are charged as they arrive.** A response's body, or a program's output, goes into the answer a chunk at a time, charged to the blob budget as each chunk is read. Past the budget, or past 256 MiB, the request fails at once, without reading the rest. A request that fails gives back every blob made for it, and the host removes the files it used.
- `image`: images on the CPU, beside the tool, deterministic for the same inputs. `decode(bytes)` reads a PNG (its first frame), a JPEG or a still WebP as an `Image { width, height, rgba }`, RGBA8, rows top to bottom, in a blob; a grey, indexed or 16-bit PNG comes out as RGBA8, and an image with no alpha comes out opaque. `resize(image, to)` is Lanczos3 as Pillow's `Image.Resampling.LANCZOS` weighs pixels (the kernel widened by the scale when it shrinks, the weights normalized), over premultiplied alpha, horizontally then vertically, rounded once at the end. `encode_png(image)` writes a PNG of RGBA8, and `encode_webp(image, quality, method)` a lossy WebP through libwebp, at a quality from 0 to 100 and a method from 0 (the fastest) to 6 (the smallest), with alpha at quality 100 and none for an opaque image (the file's bytes, a `Data`). `fit(size, within)` is the size within a box, keeping the aspect, as Pillow's `thumbnail` computes it. An image is 1 to 16384 pixels each way (a WebP, 16383).
  - **Its bytes are reserved before they are held.** The host reads a request's sizes first (an image's headers, the pixels' dimensions), then reserves the answer's blob and the most that its own work holds in the blob budget, in one step, before anything is decoded or allocated: a decoder's buffers, audited for each codec from its allocations (for a lossless WebP, its Huffman tables too), a resize's rows and weights, an encoder's output at its worst case and the codec's own bytes. An image that does not fit fails then, without being decoded. A WebP's chunks are read first: each must lie within the file, an animation is refused, and its image must be its canvas's size, so a decoder never works at a size that the reservation did not count. What the work does not use is given back, a blob keeps only its bytes, and the blob is the request's until the tool reads the answer.
  - **The work stops with its request.** It runs on a thread of its own. Past its deadline (60 s, or the `timeout` that it gives, at most 600 s), or when the tool ends, the host cancels it: a resize stops between rows, and a codec between its phases. If it does not stop within the grace (a codec's own call does not stop), the run is the host's failure (status 71), and its thread is counted with the workers left when the run ends.
  - Pillow's covers are not reproduced byte for byte. The size that `fit` gives is Pillow's, and the resize's weights are Pillow's, but Pillow rounds its weights to fixed point and its passes to 8 bits, and its `thumbnail` first shrinks an image by a whole factor with a box filter when it shrinks it 4 times or more, so a pixel can differ by about 1, and more after such a reduction. The WebP has the same settings, through another version of libwebp, so its bytes can differ.
- `serve`: a local static server on 127.0.0.1, over a directory in a root. `start(root, port, post_timeout)` serves it on the port, or a free one for 0 (a `Started`: a `Server` handle, the port, and the origin, `http://127.0.0.1:<port>`). It keeps the rules of the toolchain's server (`wrela run`'s): a request whose `Host` does not name this machine (`127.0.0.1`, `localhost` or `[::1]`, with or without the port) is refused (403), so a page elsewhere cannot reach it through DNS rebinding, and every answer is cross-origin isolated (COOP `same-origin`, COEP `require-corp`, CORP `same-origin`), as the browser runtime needs. A `GET` gives the file (`/`, or a path that ends in `/`, is its `index.html`), reached through the root's handle one part at a time, following no link. A `PUT` of `/results/<name>` (a plain file name) writes the page's result into `results/` if the root is a write root, through a new file that then takes its place; a read root refuses it (403), and a body that ends short leaves nothing, not even a `results/` that the put made. A body is framed by its `Content-Length`, and is at most 16 MiB, but a `PUT`'s, which goes to its file as it arrives: a result (a screen, a clip) is at most 256 MiB.
  - **A post goes to the tool.** A `POST` is taken only from the server's own pages (its `Origin` is the server's), else refused (403). Its body is charged to the blob budget as soon as its length is known, before it is read (413 past the budget). `next_post(server, timeout)` gives the oldest post not given yet (a `Post`: its number, its path and query, and its body, a blob over 512 KiB), and fails if none comes within its deadline (60 s, or the `timeout`, at most 600 s). `answer(server, post, bytes)` sends the page the bytes, with status 200. A post has a deadline (30 s, or the `post_timeout` that `start` gives) from when the server takes its connection: a body still being read then, a post not yet given to the tool, and one that the tool has not answered each fail with 504, and their bytes are given back. The tool is never given a post past its deadline, and an answer to one fails. A `next_post` waits for an event, not on a worker: the server answers it when a post comes, so a tool waits for it with other calls (`any`). These deadlines are on the system's clock, also in a tool test.
  - **A server is its start request's.** The host registers it under the request before the answer that names it, and closes it if the request fails or the tool ends first. `close(server)` closes it, and the host closes every server when the tool ends: its waiting `next_post`s fail, its posts fail (503), and its port is closed.
- `browser`: Chrome stable, headless, on the hardware GPU (never a software adapter), with a throwaway profile in a directory of the host's for the run, driven over the DevTools protocol on Chrome's pipe (its fds 3 and 4). `open(url, width, height)` opens the page in a browser of its own, with that viewport (a `Browser` handle), and fails if the browser offers no hardware WebGPU adapter. `run(b, script)` carries out a `Script`'s steps in order: a trusted click at an element's centre (the first that matches a CSS selector, and has exactly a text if one is given, of some size in the viewport once scrolled into view) or at a point; a key pressed and released (`std::input::Key`, with `Mods`), text that an input method commits, and a composition with its selection; an element's focus; a permission (`Permission::Clipboard`), focus emulation and the viewport's size; and waits for an `Until`: the title, an element existing, a count of elements, the texts of a selector's elements in order, an attribute, the element with the focus, the location or its hash, the clipboard (each text by a `Match`: `is`, `starts`, `ends` or `contains`), a line the program printed, or the page's test-mode result (`results/DONE`, once the tool's server that serves the page has written it from a request of this browser's, which the proxy names with the browser's session: a put that the server refuses, or hasn't finished, and another browser's page's, aren't this page's result; one of more than 1 MiB is an error that says so, never its first part), and `and` of two. A click, a focus and a wait each wait at most their timeout (20 s unless they say). The answer fails at the first step that fails, with what the page had. The observations are `title`, `location` and `clipboard` (each a `String`), `count` (a `u32`), `texts` and `console` (a `Vec<String>`: the console's lines, its workers' among them), `attribute` and `results` (an `Option<String>`), and `active` (an `Option<Focused>`: a tag and an id); a tool compares what they give in its own code. An observation is whole: one whose answer would be more than 512 KiB, or a console of more lines than a tab keeps (100 000), fails, saying so, rather than be cut short. `capture(b)` gives the viewport as a PNG in a blob (a `Capture`); the host reserves the most bytes such a PNG takes in the budget before it asks Chrome for it. `close(b)` closes it. A `Script` is a value: the compiler's own browser tests read one from a constant (`wrela_driver::script::browser_script`). No step or observation is JavaScript.
  - **A browser reaches only what the tool is granted.** Every connection that the browser makes, its pages', their workers' and Chrome's own, goes through a proxy of the host's, with no implicit bypass (`<-loopback>`: no loopback, local or link-local address goes around it), and QUIC, WebTransport and WebRTC's UDP are off (the profile's WebRTC policy sends UDP only through a proxy, which carries none), since a proxy can't carry them. The proxy lets a plain HTTP request through only to the origin of one of the tool's running servers (`http://127.0.0.1:<port>`), as the one request on its connection, and a tunnel (HTTPS, and every WebSocket, secure or not) only to a host and port that `--net` grants (443 for a grant with no port). Any other connection is refused before it is made, and the page's console says so. A tool test's fake sites are not the browser's.
  - **A browser is its open request's.** Chrome leads a process group of its own, which the host records for a supervisor before Chrome runs (as a program's, through `wrela tool --launch`). The host registers the browser under the request as soon as Chrome answers, before the page opens and before the answer that names it; if the request fails, or the tool ends first, the host closes it. A start in progress when the tool ends is given up. When the tool ends, the host closes every browser's connection first, so each request waiting on Chrome fails at once, then kills each process group and removes each profile. If the host itself is killed, even by `SIGKILL`, Chrome's end of its pipe closes and Chrome shuts down; the run's directory, with the profile, is left until the next run of a tool removes the directories of hosts that have died. One request at a time uses a browser: another fails while one runs. `close` sets the browser's cancel flag and closes its pipe as the host takes the request, before any worker carries it out (every worker may be busy, with this browser's wait among them), so a request using it fails at once (a wait reads the flag, also a wait of 600 s on what the page has printed or put), then takes the page within the grace. Each request has a deadline (60 s, or the `timeout` that it gives, at most 600 s) from when the host takes it, and a script's waits are bounded by it.
- `testing`: a tool test's fakes (§13.2): `fake(url, f)` for a fake site's URL, and `set_utc(date)` for the fake clock. Outside a tool test, these requests fail.
- The first byte of each answer tells whether the service succeeded: 0, then what the service gives, or 1, then the host's reason in UTF-8, which `wait` gives as a `ToolError`. Each type that a service gives reads the rest of its answer (`std::tool::Answer`, a trait: `read(bytes)`); `Done` is what a service gives that gives nothing.
- A tool makes requests to these services only. Storage, fetches and posts (`std::io`) are a client's: the tool host fails the call that makes one.

## 16. Concurrency

A program runs on several threads that share one memory, and shares data between them only as values that no thread can change under another; it has no `async`: a client's and a server's IO is a polled request (§17.1), and a tool's a call that it waits on (§15.5).

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
  - A client build and a tool build have workers (§15.5); a server build runs on its one thread, so its parallel calls run there.
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
  - Dropping a task completes its work as `join` does, then drops the result and releases its slot and memory. A trap is the program's panic there too.
  - `done()` says whether the task has finished, and is `nondet`. With no helpers (a page on a machine with two cores), `done()` runs a task that hasn't run yet, so a program that polls a task before it joins it doesn't wait forever.
  - A parallel call (`par_each_mut`, §16.1) inside a task, or inside a `@parallel fn`, runs on that thread alone.
- **A tool's tasks** (`std::tool::spawn(input, f)`, a `std::tool::Task<O>`) use the same slots and helpers, but `f` has no effect bound: it may make calls and wait on them (§15.5), which blocks only the thread that runs it. The reach rule still holds (E0708).
  - `join()` and dropping the task wait in the tool host, not in WASM, so `--timeout` and Ctrl-C stop a thread that waits there, and a test's fake clock knows that it waits (§13.2). Both are `nondet` (§8): when the task ends depends on the helpers. Their host functions are `@tool`, so a client or a server that reaches one, also through a destructor (a `Vec` of tool tasks cleared), is E0708. A task that no helper has started runs inside `join`, so with no helpers each runs there, in turn, and a task may join another.
  - A trap in a tool's task ends the run with its message (§15.5).
  - `std::tool::all(inputs, f)` runs `f` on each input as a task and gives the results in the inputs' order, whichever finishes first.
  - A tool build's helpers enter through `std::par`'s `tool_worker` in place of `worker`: the same loop, which tells the host when the helper waits for work and when it wakes (§20.1).

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
- A job has no executor and no IO of its own. The program resumes a job where it would test a flag, and a request stays a polled value (§17.1): `while p.poll().is_none() { yield }`. A tool needs no job: it waits on its calls (§15.5).

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

The services are `std::host`'s and std's functions over one ABI (`runtime/abi`), which both hosts implement alike. What a program asks of its host is `io` to make, and what it reads back is `nondet` (§8.1), so `@deterministic` code takes the host's answers as data, through a tick's records (§16.2).

### 17.1 Requests, not async

A **request** is work that the program asks the host for, and reads the answer of later; Wrela has no `async` (§21).

- A call submits the request and returns a `Pending<T>` handle at once:
  - `std::io::load(path)`, `store(path, bytes)` and `fetch(url)`, for the program's storage ("bytes at a path") and its build's files;
  - `std::gpu::read(span)`, for a GPU readback.
  - `std::gpu::timed(label)`, the GPU time of the passes and dispatches labelled `label` (`std::gpu::label`), in microseconds: their time in the first frame from the one that asks that runs one, summed, once the host has read it. A host times only the work of a label asked for and not yet answered, from the ask on in the order the program recorded it, and none of the rest (whose passes and dispatches cost nothing more), so a program asks before the work it times; it fails on a host that can't time GPU work (the CPU host, a GPU without timestamp queries). A program measures its own costs with it; a test checks that a cost was measured, never how small it is (§13).
- The program polls the handle on a later tick or frame. `p.poll()` returns `None` until the answer arrives. Then it returns, once, `Some(Ok(values))` or `Some(Err(e))`. `values` is a `Vec<T>`: bytes for IO, a buffer's elements for a readback.
- The host answers a request no sooner than the next call. A browser's IO takes as long as it takes.
- A request has the `io` effect to make (a readback or a time `nondet` and `host`), and the `nondet` effect to poll. So `@deterministic` code can't make or poll one (E0600). A result reaches the sim only through a tick's input, so it enters on a known tick.
- A path is relative, with `/` between its parts. No part may be empty, `.` or `..`; another path panics when the request is made.
- Storage is files under the host's storage directory on the native host, and the origin's private file system in a browser.
- A `load` of a path that holds nothing (neither the file nor a directory on its way) fails with an `IoError` whose `missing()` is true: nothing was stored there. Every other failure, a storage that can't be read among them, has `missing()` false and says nothing of what's stored, so a program that would store over it doesn't take it as empty.

A game already has a suspension point, the tick or the frame, so the language doesn't need another. A handle is an ordinary value, so nothing needs `Pin`.

- **Requests stay polled for clients and servers,** and nothing in them blocks, so a frame or a room never stalls on its IO. A tool has no frame: its calls block the thread that waits on them, and only it (§15.5). Its build imports the host's functions for that (`tool_request`, `tool_wait`, `tool_any`, `tool_take`, runtime/abi), and neither a client's nor a server's does.

#### Posts

`std::io::post(url, bytes)` is a request to the host that serves the program. It sends the bytes to the URL, relative to the build, and the answer is the host's reply.

- A browser sends it to the build's own origin and nowhere else: the URL is a relative path, as a fetch's is.
- The native host answers with whatever its user gives it (`wrela_host::PostHandler`, or a driven build's tool), and fails the request otherwise.
- A tool's local server (`std::tool::serve`) and a tool's driven build (`std::tool::drive`) give a program's posts to the tool, which answers them (§15.5). A game can use it for its own server.

#### Application services

`std::host` gives a client the services of an application host: the page's environment, decoding images, the cursor and icons, other programs shown in this one's place and the messages between them, files the user imports or exports, the clipboard, text input, the semantic tree and the page's address. Each service is a function that makes one request and gives a `Pending<u8>`.

- Underneath, each is `std::host::request(service, bytes)`: a service name, a relative path, and bytes that only the service reads. `request` is `pub(package)`, so only std's own functions reach it (E0203): a program asks for a service through its function.
- std owns each service's bytes, the request's and the answer's, and the hosts implement them. So one toolchain release ships both sides of every host contract. The compiler still knows no service names: the services are ordinary std code, not items of the closed list (§20.1).
- Each service's function is `@client`, so neither a server build nor a tool build can reach it (E0708). A tool's services are `std::tool`'s (§15.5). A host that doesn't have a service fails the request, as an IO request fails: the native client host has none of a browser's application services.
- The command buffer bounds a request. Reading an answer has the same `nondet` effect as the other requests.
- **`std::host::custom(service, bytes)`** asks for a service that the program's embedding host defines, named `app/<service>` (a Rust program that embeds the native or server host answers it, `wrela_host`'s handlers). No host of the toolchain's own has one. It is the one public request, and its names can't be a toolchain service's.
- Protocols between programs, such as an embedded program's messages with the program that shows it, are the packages' own: std carries their bytes and never reads them.

### 17.2 Input and messages

Input (`std::input`) is the pointer, the keyboard and up to four gamepads. `events()` gives the events that the host has queued since the program last read them, oldest first:

- The pointer moves, a button goes down or up, or the wheel turns.
- A key goes down or up (a physical key, `Key::A`), or types text (`Event::Text`, a Unicode scalar value).
- A gamepad's button moves (`Event::PadButton`) or its stick moves (`Event::PadAxis`), named by where it is on the pad (`PadButton::South`, the W3C standard mapping), for pads 0 to 3.
- `Event::Compose` is one character of the text an input method is composing. Each time that text changes, all of it comes again, one event a character. A `Composer` puts each change together, and ignores a change that a reader fell behind on and saw only part of. Committed text comes as `Text`.
- `Event::Paste` is a paste. It carries a serial, and the program asks its host for the pasted data by that serial.
- `Event::Assist` is what a person or an assistive technology does to a node of the program's semantic tree (`std::semantics`), by the node's number.
- `Event::Field` says that a text field's text changed. The host keeps the text, and the program reads it by revision.

All are host services, `std::host`. Positions are in the same pixels as `frame`'s width and height. `Input` keeps what is held and where the pointer is, from the events it is given: `input.read()` reads them, and `input.update(events)` takes them as data.

A call reads only the events that arrived before it started, in the same order on both hosts. The native host plays a script of them: a frame test's `@test(frames: n, input: SCRIPT)` (§13.2), and `wrela-host <build> --input script.json`. `wrela script <package> <module>::<NAME> -o script.json` writes a `Script` constant in the hosts' format for it. A browser delivers each event to the program's next frame.

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
- `wrela build` shows each constant's phases when it computes the constant, `wrela test` each test's, and the native host a program's when its run ends (`wrela-host`). The browser host times them only in test mode, each frame's in `frames.json` (what `wrela profile --chrome` reads); a normal run times none, since a page has its browser's profiler.

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
  - a `std::gpu::WithDepth<C>`: a colour `C` (a `vec4`, or a `u32` for an `R32Uint` target) and the fragment's own depth, in place of its triangle's depth. It draws a flat stand-in for something with depth, such as an impostor's leaves and the id of the tree that won the pixel. It is drawn only in a pass with a depth target (E0603 otherwise). `WithDepthGreater<C>` and `WithDepthLess<C>` name a depth that's never less (or never greater) than the triangle's, which would keep the GPU's early depth test on; WGSL can't say it yet, so each is a `WithDepth<C>`.

**What passes from the vertex shader to the fragment shader:** numbers and vectors of `f32`s, `i32`s and `u32`s, matrices, and structs and tuples of those. Each value is interpolated, or held in a `Flat<T>`; integers are never interpolated. WGSL gives each number or vector a location of its own, each matrix column one, and each struct field one. At most 16 locations pass.

#### What an entry point takes as parameters

- builtins;
- `GpuData` values, passed together as one uniform block;
- `[T]` buffers and `GpuSpan<T>`s, to read;
- textures and samplers;
- groups of those (§18.6);
- a kernel's outputs (§18.4);
- for a fragment shader, the vertex shader's output, and `mut Atomics<T>`, for example to count what it shades in a test. WebGPU gives a vertex shader no writable buffers, so a vertex shader's `mut Atomics<T>` is E0602.

**`const` parameters**: an entry point's parameter may be `const`, a `bool`, `u32`, `i32` or `f32` written `taps: const u32`. Its value is the pipeline's, not the command's: each set of values an entry point is recorded with is a pipeline of its own, and its shader is written with the value in place, so a branch on it is a branch on a constant, which the compiler folds: a `Quality` switch that's a `const` parameter leaves its shader no code when off, and the shader has no pipeline-overridable constant to set. The argument is a literal, a negated literal, a `const` item that holds one, or an associated constant (§3.4: `S::K` under a bound, folded for each instantiation), written where the entry point is bound for the command, `dispatch(blur.bind(out: mut o, taps: 4), groups: n)` (E0603 for any other value, and for binding the entry point to a value of its own, `let k = blur.bind(...)`: the command names its pipeline). Only an entry point's parameter is `const`, and only of those four types (E0602). `wrela pipelines` lists each set as an instantiation, `blur::<taps = 4>`.

```wrela
@compute(64)
pub fn blur(src: [f32], out: mut Slots<f32>, taps: const u32, id: GlobalId) {
    var sum = 0.0
    for k in 0..taps {
        sum += src[min(id.x + k, src.len() - 1)]
    }
    out[id] = sum / f32(taps)
}

fn smooth(a: GpuBuffer<f32>, o: mut GpuBuffer<f32>) {
    dispatch(blur.bind(src: a.items(), out: mut o, taps: 4), over: o.len()) // a pipeline per `taps`
}
```

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
- Workgroup memory holds an enum's payloads as 32-bit words, so it holds no enum with an `f16` in a payload (E0602).

**Workgroup atomics:** a kernel's `mut SharedAtomics<N>` is `N` `u32`s of workgroup memory that any invocation changes at once, each operation whole: `load`, `store`, `add`, `min`, `max`, `and`, `or`, `xor`, `exchange` and `compare_exchange`, as `Atomics<T>` has them. Its operations need no barrier between them. `atomics.barrier()` before reading what every invocation added sees all of it, in uniform control flow (E0610). A histogram counts a workgroup's values in workgroup atomics and adds each bin to the global count once, not once a value.

**Prefix sums:** `workgroup_prefix_sum(mut shared, lid, value)` gives each invocation the sum of `value` over the invocations before it by `LocalId`'s index, an exclusive scan, over a `Shared<u32, N>` of the workgroup's size. It is the building block of a compaction: each kept value's place in the list of what's kept. It is a Hillis–Steele scan, log2(N) steps a barrier apart, so every invocation calls it. `subgroup_prefix_sum` gives the same results with subgroup operations (§18.10).

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

**Blends:** `blend:` is a draw's blend state, a `Blend`, and `blends: [...]` gives one for each of a pass's targets. Each is a build-time constant, part of the pipeline:

- `Replace` (the default) writes the fragment's colour.
- `Over` blends a straight-alpha colour over the target's; a fragment shader that returns `Over` asks for it.
- `Premultiplied` adds the colour to the target's times one less its alpha.
- `Add` adds, `Multiply` multiplies, and `Min` and `Max` keep the smaller or larger of each channel.

A blend needs a format that blends: an integer format never does, and the 32-bit float formats do only on a device with `float32-blendable` (`features()`, §18.10), which the host checks.

**Direct indexed draws:** `draw(pass, vs, fs, indices: buf, index_count: n)` draws `n` indices of a `GpuBuffer<u32>`, with `first_index:`, `base_vertex:`, `instances:` and `first_instance:` if the draw says. Its counts are the CPU's, as a plain draw's are, and its instances, `first_instance` to `first_instance + instances`, are within a `u32` (the hosts check).

**Multi-draws:** `draw_multi(pass, vs, fs, indirect: buf, count: n)` draws `n` indirect draws whose arguments are consecutive in `buf`, as one command; with `indices:` they are indexed. A device with Chrome's multi-draw feature runs them as one call; without it, the host draws them one by one, which draws the same. The count is the CPU's: a draw GPU code means to skip has no instances. (A count the GPU writes would draw what a device without the feature can't, so the hosts refuse one.)

**Scissors and viewports:** `pass.scissor(x, y, width, height)` keeps the pass's later draws inside a rectangle of its targets, and `pass.viewport(x, y, width, height, min_depth, max_depth)` maps clip space onto one. Each lasts until the next, or the pass's end. Both are a pass's methods, through `RenderPass`. A viewport's width and height are at most the GPU's largest texture side, and its edges are within twice that of the origin, as WebGPU requires (the hosts check).

### 18.8 Textures, formats and passes

Textures and samplers are owned GPU values that shaders read and passes draw into; a texture's format is a type.

#### Making textures and samplers

- `texture::<F>(width, height)` makes a colour texture of format `F`.
- `writable_texture::<F>(width, height)` makes a colour texture that can also be a kernel's `Texels<F>`. The hosts give a texture storage usage only then, because storage can cost a drawn texture its compression.
- `depth_texture(width, height)` makes a depth texture.
- `texture3d::<F>(width, height, depth)` makes a 3D texture, each side 1 to 2048 texels.
- `texture_array::<F>(width, height, layers)` (and `writable_texture_array`) makes `layers` 2D textures of one size and format, 1 to 256 of them; `depth_texture_array(width, height, layers)` makes depths' (a shadow map's cascades).
- `cubemap::<F>(side)` (and `writable_cubemap`) makes six square faces, in WebGPU's order (+x, −x, +y, −y, +z, −z), sampled by a direction.
- `sampler(filter, address, mip, anisotropy)` and `comparison_sampler(compare)` make the ways to read them. `mip` says whether a sample between two levels blends them (trilinear, the default) or takes the nearer; `anisotropy` (1 to 16) samples a slanted surface along its slant.

Each is an owned value, released when dropped, like a buffer. `tex.write(x, y, width, height, bytes, level)` copies texels in (an array's and a cube's take the layer or face too).

**Mip levels:** a texture, array or cube has `levels` mip levels (1 by default), each half its predecessor's side, rounded down, to 1 texel: `mip_levels(width, height)` is the whole chain's count. `generate_mips(mut tex, sampler)` fills levels 1 on from level 0, each the mean of the level before. A kernel makes a chain of its own with a `MipStep<F>` (`tex.mip_step(i)`): it reads level `i − 1` and writes level `i` in one dispatch, because WebGPU tracks each level apart.

**Views:** a part of a texture is a view, a borrow struct:

- `tex.level(i)` is one level to read, and `tex.level_mut(i)` one to write (a kernel's `Texels<F>`, a copy's destination, a pass's target with `begin_pass(..., level: i)`).
- `arr.layer(i)` is one layer of an array or a cube's face, all its levels, read as a 2D texture; `arr.layer_mut(i, level)` writes one of its levels.

Two views of one texture bound together don't overlap when their levels or layers don't, so a kernel reads one level and writes another, or one layer and another.

**Copies and readback:** `copy_texture(from, to)` copies a rectangle between textures of one format, in recorded order; `copy_texture_to_buffer` and `copy_buffer_to_texture` move texels to and from a `GpuBuffer<u32>`, a row of texels (of blocks, for a compressed format) a row, each row padded to 256 bytes as WebGPU requires. `read_texture(view)` is readback, a request (§17.1) whose answer is the texels' bytes, unpadded; the buffer it reads them through is held until the read is dropped. Each takes a texture's level or an array's layer. A shader takes textures and samplers as parameters, and CPU code passes them by name. CPU code cannot read texels (E0607).

#### Reading textures

- `tex.sample(s, uv)` and `depth.sample_compare(s, uv, reference)` pick their detail from neighbouring pixels. So they are for fragment shaders, in uniform control flow, as derivatives are (E0607, E0608).
- `sample_level`, `sample_compare_level`, `load`, `width()` and `height()` work in any GPU code.
- **The screen** is SDR in sRGB until a program asks otherwise: `std::gpu::screen(hdr: true, gamut: Gamut::DisplayP3)` (a command, at any frame) shows it in half floats, tone mapped `extended`, so 1.0 is SDR white and brighter is brighter where the display can, and in Display P3's gamut. The browser builds every pipeline that draws the screen for its new format first, and swaps between frames; until then the frames go on as they were. In HDR a screen pass's fragments are linear values with headroom, which the program's own tone map rolls off. `screen_is()` (`nondet`) says what the host did: a host that can't (the native host draws offscreen; a display may refuse) keeps SDR in sRGB. Test mode keeps its screen, and so its goldens, unless the test asks (`hdr=1`).
- **Gathers** fetch the four texels a bilinear sample would read, in one instruction, in WGSL's order: (min u, max v), (max u, max v), (max u, min v), (min u, min v). `tex.gather(s, uv, component: 0)` gives one component of each (0 to 3; each its own fetch, so a component that isn't a constant picks one with a branch), `depth.gather(s, uv)` the four depths, and `depth.gather_compare(s, uv, reference)` each one's comparison, 1 or 0: four shadow taps in one fetch. Arrays take a `layer` after `uv`. They work in any GPU code. A depth texture's `gather` takes a plain `Sampler`, which the pipeline binds as non-filtering (WebGPU pairs a depth texture only with comparison and non-filtering samplers), so its filters must be `Nearest`.
- Only kernels write a 3D texture's texels (`Texels3d<F>`). Shaders read it with `sample_level(s, uvw)` and `load(x, y, z)`, in any GPU code, and its size with `width()`, `height()` and `depth()`. A 3D texture is never a pass's target.

**Formats are types.** A colour texture is a `Texture<F>` (and a `Texture3d<F>`, a `Texels<F>`), with `F` its format:

- `Rgba8`, `Rg8` and `R8`: 8-bit channels, read as floats in [0, 1];
- `Rgb10a2`: 10 bits for each colour and 2 for alpha;
- `Rgba16Float`, `Rg16Float`, `R16Float` and `Rg11b10Float` (three small floats, no alpha);
- `Rgba32Float`, `Rg32Float` and `R32Float`;
- `R32Uint` and `Rg32Uint`: integer texels, whole to 32 bits, drawn into by a fragment shader that returns `u32`s;
- the compressed formats `Bc1`, `Bc4`, `Bc5`, `Bc7`, `Etc2Rgb8` and `Astc4x4`: blocks of 4 × 4 texels, each 8 or 16 bytes. A compressed texture is only sampled. Its texels are written in rows of blocks, and it exists only on a device with the format's feature (`features()`, §18.10), so a program checks first.

Each format says what a texel is in GPU code (`F::Texel`): a `vec4`, a `vec2`, an `f32` or a `u32`. `load`, `sample` and `store` give and take that type. The channels a format lacks are not written, and GPU code never pads them. Code generic over formats reads a texel as four channels with `F::rgba(texel)`: a channel the format lacks reads 0, and a missing alpha reads 1, as WGSL reads them.

What else a format can do is a trait it has or lacks, as WebGPU's core formats allow:

- `Filtered` formats are sampled (`sample`, `sample_level`). The integer formats and the 32-bit float formats are not; GPU code `load`s them. (`features().float32_filterable` says whether the device could filter the 32-bit floats; toolchain 0.6 doesn't sample them yet.)
- `Storage` formats are written by kernels (`writable_texture`, `texture3d`, `Texels`): `Rgba8`, `Rgba16Float`, `R32Float`, `Rg32Float`, `Rgba32Float`, `R32Uint` and `Rg32Uint`.
- `Target` formats are drawn into: every uncompressed format (`Rg11b10Float` on a device with `rg11b10ufloat-renderable`).

A missing trait is E0400 where it is needed. Each binding in the manifest names its texture's format, and the hosts check that the bound texture has that format. Each format holds what was written, in both hosts.

**Passes:** these functions start a pass, as `begin_screen_pass` does (§18.7):

- `begin_pass(mut tex)`, `begin_pass(mut tex, level: i)` for one of its levels;
- `begin_pass_with_depth(mut tex, mut depth)`;
- `begin_depth_pass(mut depth)`, and `begin_depth_layer_pass(mut depths, layer)` for a layer of a depth array;
- `begin_layer_pass(mut arr, layer)` and `begin_face_pass(mut cube, face)`, with or without depth;
- `begin_pass2` to `begin_pass8`, with or without depth, for two to eight colour targets of one size;
- `begin_screen_pass_with_depth(mut depth)`.

**Several targets:** a fragment shader for a pass of several targets returns a `Targets2` to `Targets8` of its colours (fields `first` to `eighth`), each one's channels its target's format's: floats or `u32`s, one to four of them. A pass has at most eight targets (WebGPU's `maxColorAttachments`), and its colours take at most 32 bytes a sample together (`maxColorAttachmentBytesPerSample`), counted as WebGPU counts them: each target's render-target cost, not its texel's size (`rgba8unorm` costs 8 though it stores 4, `rgba16float` and `rg32uint` 8, `r32uint` 4), aligned before the next's is added. Past that is E0602, naming each target's bytes and the total; `wrela pipelines --json` gives each render pipeline's `attachment_bytes`. Eight `r32uint`s fit; eight `rgba16float`s (64) don't.

Each gives a value that borrows its targets until `end()` (or `present()`), so the pass's draws cannot bind them (E0506). A frame can have any number of passes. A pass with depth keeps the nearest fragment, unless a draw's depth state says otherwise.

**A draw takes its pass:** `draw(pass, vs, fs, ...)`. The pass is the value its `begin_` function gave (`Pass<F>`, `DepthPass<F>`, `DepthOnlyPass`, `ScreenPass`, `ScreenDepthPass`), or a parameter bounded by `std::gpu::RenderPass`, which only those types have (an `impl` is E0415).

- The pass's type says what the draw draws into. So the compiler knows each render pipeline's targets: the manifest's `targets`, every pass the pipeline is drawn in.
- The hosts make each pipeline while the program loads, not at its first draw.
- The hosts refuse a draw into a pass that is not one of its pipeline's targets.

**Joined passes:** a pass that keeps both its targets may `join` the pass before it: `begin_pass_with_depth(..., join: true)`. If that pass ended right before it, on the same targets and under the same label (§18.10), the host may run the two as one render pass, timed and named by that label; a `label` between them ends the first. A tile-based GPU then does not store the targets and load them again between the two. The serial timing mode still times each pass alone. A light pass is worth joining; two heavy passes are not, because a pass's geometry is binned before its tiles are drawn.

### 18.9 Pipelines

A pipeline is an entry point compiled for one set of type arguments: a generic kernel or shader has a pipeline for each instantiation that the program records (§7), and a field's numbers travel as data, so one pipeline serves every field of one structure.

- **The pipeline-count query** reports how many instantiations each GPU entry point has, and why. `wrela pipelines <package>` lists each entry point's pipelines, their type arguments, and where CPU code dispatches or draws each. A site that runs only under a runtime condition of its function is marked `(under a condition)` (in `--json`, `conditional`): its pipeline is in the build however rarely the condition holds, so a stage to leave out is left out by the program's structure. `--json` gives the same list for tools.
- **A shipped build keeps a budget for its shaders:** at most 256 KiB of WGSL for each pipeline. A browser compiles each pipeline while the scene loads, and the GPU's compile time grows faster than the shader's size.
  - A release build over the budget is E0707, at the command that records the large pipeline.
  - The budget isn't checked for a test build, a debug build or a lifted build.
  - A pipeline is in the build only if a command in the optimized CPU code records it. A pipeline behind a constant branch, such as `if test_build() && counting`, leaves the build with the code that records it. (The `&&` holds its value in a local, which constant folding sees through.) Constants fold again after inlining, so a predicate whose body folds to a constant, `fn on(s: State) -> bool { ON && s.n == 3 }` with `ON` false, is that constant where it's called, and the branch on it folds too; so do `&&` and `||` of values where one side settles them, and a comparison of two integer constants. `wrela pipelines` lists what the build makes: it inlines and folds as a build does.
- **The number of pipelines in a build isn't limited.** The time to create them counts in the cold start's budget, measured cold in Chrome.
- **The hosts start the program while they create its pipelines,** the biggest shaders first. A command that needs a pipeline not yet created waits for it, and so do the commands after it.
- **Pipelines come in two tiers.** A pipeline every site of which runs only under a runtime condition (`(under a condition)` above) is tier 1; any other is tier 0. The manifest marks tier 1 (`"conditional": true`), and `wrela pipelines` prints `tier 1` beside it (in `--json`, `"tier": 0` or `1`). The browser host creates tier 0's pipelines first, and tier 1's once those have all settled; a command that needs a tier-1 pipeline before its turn starts its creation at once, and waits for it as above. A program can't see the tiers: the test mode's `pipelines.json` reports tier 0's time as `ms` and every pipeline's as `tier1_ms`, so "pipelines ready" means tier 0.

```wrela
/// Generic over any field; monomorphized per concrete field type.
@compute(64)
fn cull<F: Surface>(field: F, grid: Grid, live: mut Slots<u32>, id: GlobalId) {
    let b = grid.block(id.x)
    let r = field.interval(Box3 { lo: b.lo, hi: b.hi })   // `interval` is derived (§14)
    live[id] = if r.contains(0.0) { 1 } else { 0 }
}
```

### 18.10 Timing, limits and features

The hosts can time each pass and dispatch on the GPU, they open the GPU with its own limits and every optional feature it has, and std counts the GPU memory a program holds.

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

**Optional features:** both hosts open the device with every optional feature the adapter has, and none is required. `std::gpu::features()` says which the device has: half floats (`f16`), subgroups (with the least and most invocations a subgroup has), the compressed formats (`bc`, `etc2`, `astc`), the 32-bit float formats filtered and blended, `Rg11b10Float` drawn into, timestamps, and multi-draws. It has the `nondet` effect. A program checks it before it records what needs a feature, and keeps a path without. Test mode's `features=none` (and `WRELA_FEATURES=none` for the native host) shows a program a device without them, so its fallback paths run on a device that has them.

- **A pipeline that uses a feature requires it.** The compiler sees what a shader uses: one with half floats requires `shader-f16`, and one with subgroup operations `subgroups`, in its manifest entry. The hosts make such a pipeline only on a device with the feature, and refuse a command that records it otherwise, naming the feature.
- **Half floats:** `f16` and `vec2h`, `vec3h` and `vec4h` are GPU code's, converted with `f16(x)` and `f32(h)`. CPU code has none (E0338): a buffer holds half floats packed, two to a `u32` (`Half2`).
- **Subgroups:** a subgroup is the invocations of a kernel that run in lockstep. `subgroup_add`, `subgroup_exclusive_add`, `subgroup_inclusive_add`, `subgroup_min`, `subgroup_max`, the bitwise `subgroup_and`, `subgroup_or` and `subgroup_xor`, `subgroup_any`, `subgroup_all`, `subgroup_ballot`, `subgroup_broadcast_first` and `subgroup_shuffle` combine their values without workgroup memory. A kernel takes its invocation's place in its subgroup as a `SubgroupInvocation` and the subgroup's size as a `SubgroupSize`. WebGPU numbers a subgroup's invocations only in a workgroup of one dimension, so a kernel whose workgroup has more than one row or layer takes no `SubgroupInvocation` (E0602). They are kernels' only (E0607 elsewhere, and for a type WGSL's don't take), in uniform control flow (E0610), so such a kernel is dispatched by `groups:`. WGSL leaves which invocations share a subgroup to the GPU, so `subgroup_prefix_sum` promises a compaction's places, not `LocalId`'s order: each invocation gets the sum before it in the subgroups' order (by each subgroup's first invocation's index), distinct and dense on any GPU. Where subgroups are runs of consecutive indices, as on every GPU this toolchain has run on, the results equal `workgroup_prefix_sum`'s; a compaction that must keep `LocalId` order uses that one.
- 64-bit atomics are not in toolchain 0.6: neither WebGPU's core nor a shipped browser has them.

**Memory:** `std::gpu::memory()` gives the bytes of the program's buffers and textures, now and at their most. std counts them as it makes and drops each (a texture by its levels' blocks and its layers). Test mode's `memory.json` is the host's count, the same numbers. It has the `nondet` effect: the program's thread changes the count as it records GPU work, so another thread's read (a ticker's) would depend on timing, and `@deterministic` code takes it as data.

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

**Who may propose:** a room takes a proposal only from its peer's connection, whose hello carried the peer's token. The room tags that connection with the peer (`std::net::tag`), so a room that its host starts again knows it. A peer's id, which every entry shows, proves nothing. A connection says hello once; the room refuses a second hello (`HELLO_AGAIN`).

**Catching up and compaction:**

- A peer that rejoins more than `CATCH_UP` (128) entries behind gets the state, not the entries.
- A room compacts once its journal holds `COMPACT_AFTER` (1024) records past its snapshot, refusals and joins included.
- The messages between peers and the room are `Pack`ed (`Up<T>`, `Down<S, T>`).

### 19.2 Durable rooms

The server host keeps each room durably (`wrela_host::rooms`). `std::net`'s `serve_room` runs a room for `serve`. The host writes each call's records (`keep`, `compact`) to the room's journal, flushed to disk, before it sends anything the call sent. A room that starts reads its journal back first.

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
- A client that sends no first byte of its next frame for 60 s, or takes more than 10 s to finish a frame after its first byte, is closed. QUIC keepalives do not count as messages. QUIC stream and receive windows bound traffic on streams the room does not use.
- A browser's WebTransport Origin must be allowed before it joins a room. A production `wrela-serve` names each allowed page origin with `--origin`; each value uses the browser's serialized HTTP(S) origin form, with a scheme and host, an optional non-default port, and no path, user information, query or fragment. The page and room server may have different origins. `wrela-serve --dev` and `wrela run` accept local page origins only. A client without an Origin is not a browser and the room's protocol must decide whether to admit it.
- A room's time (`serve`'s `time`) counts from the server's start, so a room that stops and starts again continues from that time.

**Bounds** (`Bounds`): what a server holds is bounded.

- Sessions open or opening: 256.
- Rooms running: 64. A room with no connections for 30 s stops, and starts again from its journal.
- Bytes queued: each connection may hold 128 KiB each way, whatever the others hold. Past that, it draws on 64 MiB that every queue shares. Each message counts as 64 bytes more than its size.
- The server closes a connection with more than 1 MiB waiting for its room, more than 16 MiB waiting for the connection, or a write that takes 10 s. Its peer comes back.
- A call's network commands hold at most `Limits::commands` (16 MiB) bytes.
- The browser closes a connection with more than 1 MiB waiting to be written, or whose messages would hold more than 16 MiB waiting for the program.
- A room's state must pack into one host request (1 MiB) to compact, and into one frame (64 KiB) to be sent to a peer that is far behind. Larger states are not split.

**Clients:** a client opens connection `conn` to a room with `std::net::open(conn, room)` (`@client`), and uses `send` and `close` as a server does. Each connection it opens gives one `Connection` event when it is open, and exactly one when it is closed, whatever closes it.

#### Finding and running the server

- The browser host finds the server in `net.json` beside the build: its address and, for a development certificate, the certificate's hash, which the browser trusts in place of a certificate authority's signature. `wrela-serve --dev` writes both.
- Development room servers bind only to loopback addresses. A production `wrela-serve` binds to all interfaces and requires `--cert`, `--key` and at least one `--origin`.
- Each server build's rooms are kept apart (`<data>/<build hash>/`), because only the build that wrote a journal reads it. A server given a new build closes its rooms' connections, and their peers come back to the new build's rooms.
- `wrela run` serves a program's rooms beside its page (`--rooms-port`, 4433 by default), writes each build's `net.json`, and gives the rooms each new server build.

### 19.4 A server's services

A server build asks its host for what outlives a room, and for cryptography, through requests (§17.1), answered on the call after it makes them (`wrela_host::services`). Every room of the server shares them.

- **The store** (`std::store`): keys and values, both bytes, in order of their keys. It is one file beside the rooms' journals (`<data>/store.sqlite`, SQLite), which every server build reads, so it outlives a build as a journal does not. A server's data that is not a room's log lives here: accounts, leaderboards, what peers ask for one piece at a time.
  - `get(key)` and `scan(from, to, limit, down)` are requests, answered from what is committed when the host takes them. A scan gives at most 1 MiB of keys and values; the next scan starts after its last key.
  - `put(key, value)` and `delete(key)` are commands, as `keep` is: the host commits a call's writes in one transaction when the call returns, then keeps its records, then releases its sends. A call that fails commits none of its writes. A crash after the writes and before the records leaves the writes kept and the event not settled, so its peer sends it again: a write should be the same when it is made again.
  - A key is at most 1 KiB and a value at most 1020 KiB, so a put of both fits in one host request (1 MiB). Keys sort as bytes: `append_u32` and `append_u64` put a number in a key big-endian, so keys sort by it.
- **Randomness** (`std::crypto::random(n)`): up to 1 KiB of bytes from the operating system's generator.
- **Signatures** (`std::crypto::verify(algorithm, key, message, signature)`): whether the signature is the key's over the message, by ES256, Ed25519 or RS256, the algorithms of passkeys. The host checks it with vetted libraries; std does no cryptography of its own. A key that is not one of the algorithm's fails the request.

Each is `@server`. `wrela-serve` and `wrela run` open the store in their data directory.

**Passkeys** are the client's half and the server's together. A page asks the person's authenticator to make a passkey or to sign a challenge (`std::host::create_passkey` and `use_passkey`, `@client`, §17.1). The host makes every passkey discoverable, asks for the person's verification each time and asks for no attestation, and offers ES256, Ed25519 and RS256; the program chooses the relying party, the account's handle and the challenge (from `std::crypto::random`), and names at most 64 credentials to exclude or allow (`MAX_CREDENTIALS`: more panics). The server checks what the authenticator answered with `std::webauthn` (WebAuthn Level 3, §7): `registration` gives the passkey's public key once the client's data (its type, challenge, origin, and that it wasn't made inside another site's frame) and the authenticator's data (the relying party's hash, the person present and verified, a backup only of a passkey eligible for one, the credential the browser named) pass; `assertion` gives the bytes the signature must cover once the same checks and the signature counter pass, for `std::crypto::verify`. A passkey's attestation is not checked.

## 20. The stdlib boundary

The compiler knows the language, a closed list of std items and its targets; the rest of std, fields included, is ordinary Wrela code.

### 20.1 What the compiler knows

- **The compiler knows three things:** the language, a closed list of std items, and the execution targets (§1.1).
- **The closed list** is every std item that the compiler knows by its path, and nothing else. It holds the traits that the compiler derives or checks by structure, the types that its code has a shape for, the GPU, IO and memory operations that it lowers itself (`@intrinsic` in std), and the transcendentals that it compiles from std's code. A program cannot add to the list. `Plain`, `StateHash`, `Serialize`, `Blend` and `Surface` are not on it: they are ordinary std traits, `@fieldwise` or written by hand. The list, by module:
  - `std::prelude`: `Option`, `Result`, `Copy`, `Clone`, `GpuData`, `Fieldless`, `Packed`, `Bits`, `packed_fit`, `is_discriminant`, `of_discriminant`, `packed_of`
  - `std::gpu`: `GpuBuffer`, `Slots`, `GlobalId`, `LocalId`, `WorkgroupId`, `VertexIndex`, `InstanceIndex`, `FragCoord`, `ClipPosition`, `Flat`, `Over`, `WithDepth`, `Cull`, `DepthBias`, `Depth`, `dispatch`, `draw`, `write_buffer`, `destroy_buffer`, `copy_buffer`, `GpuSpan`, `GpuSpanMut`, `GpuField`, `field_at`, `begin_screen_pass_command`, `RenderPass`, `Pass`, `DepthPass`, `DepthOnlyPass`, `ScreenPass`, `ScreenDepthPass`, `present`, `Texture`, `DepthTexture`, `Sampler`, `ComparisonSampler`, `write_texture_rows`, `destroy_texture`, `create_sampler`, `destroy_sampler`, `end_pass_command`, `label_command`, `texture_sample`, `texture_sample_level`, `texture_sample_compare`, `texture_sample_compare_level`, `texture_load`, `depth_load`, `Texture3d`, `Rgba8`, `Rgba16Float`, `R16Float`, `Rg16Float`, `R32Float`, `R32Uint`, `texture3d_sample_level`, `texture3d_load`, `texture_width`, `texture_height`, `texture_depth`, `span_len`, `read_buffer_command`, `timed_command`, `limit`, `Shared`, `Atomics`, `Append`, `AtomicMap`, `AppendBuffer`, `AtomicMapBuffer`, `local_index`, `workgroup_invocations`, `shared_get`, `shared_set`, `workgroup_barrier`, `discard`, `atomic_len`, `atomic_load`, `atomic_store`, `atomic_add`, `atomic_sub`, `atomic_min`, `atomic_max`, `atomic_and`, `atomic_or`, `atomic_xor`, `atomic_exchange`, `atomic_compare_exchange`, `append_push`, `One`, `one_get`, `one_set`, `DrawArgs`, `DrawIndexedArgs`, `DispatchArgs`, `Texels`, `texels_store`, `Texels3d`, `texels3d_store`, `Kernel`, `VertexShader`, `FragmentShader`, `Astc4x4`, `Bc1`, `Bc4`, `Bc5`, `Bc7`, `Blend`, `Cubemap`, `DepthLayerPass`, `DepthPass2`, `DepthPass3`, `DepthPass4`, `DepthPass5`, `DepthPass6`, `DepthPass7`, `DepthPass8`, `DepthTextureArray`, `Etc2Rgb8`, `LayerDepthPass`, `LayerPass`, `MipStep`, `Pass2`, `Pass3`, `Pass4`, `Pass5`, `Pass6`, `Pass7`, `Pass8`, `R8`, `Rg11b10Float`, `Rg32Float`, `Rg32Uint`, `Rg8`, `Rgb10a2`, `Rgba32Float`, `SharedAtomics`, `SubgroupInvocation`, `SubgroupSize`, `Texture3dHandle`, `texture3d_handle_load`, `Targets2`, `Targets3`, `Targets4`, `Targets5`, `Targets6`, `Targets7`, `Targets8`, `TexelsArray`, `TextureArray`, `TextureLayer`, `TextureLayerMut`, `TextureLevel`, `TextureLevelMut`, `TextureStep`, `array_load`, `array_sample`, `array_sample_grad`, `array_sample_level`, `atomics_barrier`, `begin_pass0_command`, `begin_pass1_command`, `begin_pass2_command`, `begin_pass3_command`, `begin_pass4_command`, `begin_pass5_command`, `begin_pass6_command`, `begin_pass7_command`, `begin_pass8_command`, `copy_buffer_to_texture_command`, `copy_texture_command`, `copy_texture_to_buffer_command`, `cube_sample`, `cube_sample_level`, `depth_array_compare`, `depth_array_compare_level`, `depth_array_load`, `draw_multi`, `mip_step_height`, `mip_step_load`, `mip_step_sample`, `mip_step_store`, `mip_step_width`, `new_buffer`, `new_texture`, `scissor_command`, `subgroup_add`, `subgroup_all`, `subgroup_and`, `subgroup_any`, `subgroup_ballot`, `subgroup_broadcast_first`, `subgroup_exclusive_add`, `subgroup_inclusive_add`, `subgroup_max`, `subgroup_min`, `subgroup_or`, `subgroup_shuffle`, `subgroup_xor`, `texels_array_store`, `texture_layers`, `texture_levels`, `texture_sample_bias`, `texture_sample_grad`, `viewport_command`, `screen_command`, `SharedAtomicsI`, `texture_gather`, `array_gather`, `depth_gather`, `depth_gather_compare`, `depth_array_gather`, `depth_array_gather_compare`
  - `std::host`: `host_command`
  - `std::io`: `next_request`, `request_status`, `storage_read_command`, `storage_write_command`, `fetch_command`, `print_command`, `post_command`, `Shipped`
  - `std::input`: `Script`
  - `std::tool::browser`: `Script`
  - `std::mem`: `take_answer`, `take_input`, `take_payload`, `keep_bytes`, `kept_bytes`, `read_clock`, `Drop`, `size_of`, `align_of`, `needs_drop`, `is_copy`, `read`, `zeroed`, `write`, `drop_at`, `at`, `at_mut`, `at_mut_pair`, `heap_base`, `memory_pages`, `memory_grow`, `load_u32`, `store_u32`, `load_u8`, `store_u8`, `copy`, `fill`, `compare_swap`, `atomic_add`, `atomic_load`, `atomic_store`, `wait`, `notify`, `run_task`, `task`, `thread_block`, `wait_for`, `abort`, `debug_build`, `test_build`, `tool_request`, `tool_wait`, `tool_any`, `tool_take`, `tool_idle`, `tool_active`, `tool_join`, `tool_done`
  - `std::derive`: `Interval`, `Domain`, `gradient`, `value_and_gradient`, `value_gradient_with`, `interval`, `precision`, `precision_note`, `filtered`, `tape`, `tape_with`, `tape_sized`
  - `std::field`: `smin`, `value_noise`, `value_noise_box`, `lattice_coord`
  - `std::hash`: `pcg`
  - `std::stage`: `Building`, `push_one`, `push_two`, `push_new`, `finish`
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
- **std is written in Wrela** with a small unsafe core (§6.12). The core is these files, and only they contain `unsafe`: `std::mem` (raw memory), `std::alloc` (the allocator), `std::collections` (`Vec` and `Box`), `std::string`, `std::par` (the helpers and jobs), `std::audio` (the voice and its ring), `std::tick` (the ticker), `std::handoff` (the triple buffer), `std::time` (the clock) and `std::tool` (a tool's calls and tasks).
- **The core states what inference cannot see,** with two attributes. Only the core's files may use them; the compiler rejects them anywhere else (E0204).
  - `@effects(...)` names the effects of a std function that come from another thread or from the host through memory. `Latest::read` and `Job::done` are `@effects(nondet)`; starting the voice and starting the ticker are `@effects(io)`; a tool task's destructor is `@effects(nondet)`, and dropping a value counts what the destructors that it runs declare. This is the one place where effects are written; everywhere else the compiler infers them (§8.2). A GPU command, request or host import that the compiler lowers itself has the effects of what it lowers to.
  - `@thread_entry` marks a std function that a host calls on a thread of its own (the `memory` of `wrela_abi`). Its first parameter is the thread's number, which gives the thread its stack and its block; its other parameters are `u32`s. When a program uses the function's module, the function is exported as `__` followed by its name. The thread entries are `std::par`'s `worker` (each helper), `std::par`'s `tool_worker` (each helper of a tool build: a `@tool` entry, which a tool build exports in place of its module's others, and no other build), `std::audio`'s `audio` (the voice's quanta) and `std::tick`'s `tick` (each tick). The compiler names none of them.
- **A task is a function that a module calls by number,** with a chunk and a context's address, as `run_task` and the thread entries do. `std::mem::task(run)` gives a task's number. The voice and the ticker are tasks; the chunks of a parallel job are tasks that the compiler makes from their closures.

### 20.2 The module map

`wrela doc std` lists std's modules, and `wrela doc <item>` shows any item.

| Module | Contents |
|---|---|
| `std::prelude` | In scope in every module: `Option`, `Result`, `Copy`, `Clone`, `GpuData`, `Plain`, and, from their modules, `Eq`, `Ord`, `Ordering`, `Vec`, `Bounded`, `Box`, `swap`, `replace`, `String`, `Text`, `Bytes`, `Arena`, `Handle`, `SortedMap` and `Quat` |
| `std::cmp` | `Eq`, `Ord` and `Ordering`: `==` and `<` for the types that declare them (§3.4) |
| `std::transform` | Matrices for skeletons, cameras and shadow maps, on the CPU and the GPU, in WebGPU's conventions (right-handed, +y up, column vectors, depth 0 to 1): `identity`, `translate`, `scale`, `rotate_x`, `rotate_y`, `rotate_z`, `rotate(axis, angle)`, `from_mat3`, `look_at(eye, at, up)`, `perspective`, `ortho` |
| `std::collections` | `Vec` and `Box`. `Bounded<T, N>`: at most `N` values held in place (an array and a count of the values in use). It is `Copy`, and it is `Plain` and `GpuData` where `T` is, so it can be in a snapshot or on the GPU, where a `Vec` cannot. It has `push`, `pop`, `remove`, `clear`, `len`, `is_full` and `Bounded::from(xs)`; `b[i]` projects an element in use (an index past `len()` panics on the CPU), `for x in b` walks the elements in use, and it hashes, saves and compares only those. `swap` and `replace` move values out of places. `Vec::filled(n, value)`, and `Vec::from_fn(n, make)` for a `T` that is not `Clone`. `sort_by` is a stable O(n log n) sort of a run of `Copy` values; `Vec::sort_by` sorts any `Vec`, and `Vec::sort_by_in` keeps its working space in a `Vec<u8>`, so a tick that sorts allocates nothing once that space has grown (§16.2) |
| `std::string` | `String`; `Text`, text known at build time; `Bytes`, bytes known at build time, with `at(i)` and little-endian reads at any byte (`u16_le`, `u32_le`, `i32_le`, `f32_le`, `u64_le`, `f64_le`); numbers read from text (`parse_u32`, `parse_i32`, `parse_f32`), and `parse_f64` and `parse_f64_at(bytes, from, to)`, which read a decimal as the nearest f64, as Rust's `parse` does |
| `std::fmt` | `Format`, which `f"…"` calls, and `Spec`, a hole's format spec; `json_text` and `json_number`, which write text and floats as JSON (a NaN or an infinity is `null`) |
| `std::json` | JSON read: `parse` gives a `Doc`, its values found by index from `TOP` (`kind`, `get`, `at`, `count`, `num`, `text`, `is_true`, `write`); a number keeps its text, and `number_of` reads it as the nearest f64 |
| `std::arena` | `Arena<T>` and `Handle<T>` (§6.8); `SortedMap<K: Ord, V>`, which iterates in key order, so `@deterministic` code may use it |
| `std::units` | Suffix constants in SI: `km`, `m`, `cm`, `mm`, `um`, `t`, `kg`, `g`, `mg`, `h`, `s`, `ms`, `us`, `rad`, `deg`, `Hz`, `kHz`, `N`, `J`, `W`, `Pa`, `kPa` (§5) |
| `std::math` | The transcendentals for CPU code, compiled to WASM (§11.1); `PI`, `TAU` and `is_finite`. Their f64 counterparts, ordinary functions since an f64's built-in transcendentals are E0702 (§11.4): `sin_f64`, `cos_f64`, `atan_f64`, `atan2_f64`, `exp_f64`, `log_f64`, `log10_f64`, `log2_f64` and `pow_f64`, each within an ulp of libm where sampled (`pow_f64` within two near overflow, an infinity or 0 past f64's range); `PI_F64` and `TAU_F64` |
| `std::hash` | `StateHash` (`@fieldwise`), `Hasher`, `state_hash` (§6.11) |
| `std::hash::sha256` | SHA-256 (FIPS 180-4): `Sha256`, a digest made a run of bytes at a time (`update`, `finish`), so a blob is hashed a chunk at a time; `sha256(bytes)`; `hex(bytes)` |
| `std::serialize` | `Serialize` (`@fieldwise`), `Writer`, `Reader`, `save`, `load`, `LoadError` (§6.11) |
| `std::field` | `Surface`, `Field<C>`, `Noise`, `Blend`, `Color`, `UnitVec3`, `Cat<T>`. Primitives: `sphere`, `ellipsoid`, `round_cone`, `cuboid`, `half_space`. Forms: `oval`, `tube`. Combinators: `union`, `smooth_union`, `intersect`, `translate`, `displace`, `with`, and `union` and `smooth_union` of an array. Noise: `value_noise`, `fbm`. Provenance: `parts`, `part_at`, `part_distance`, `part_name`, `part_offset`, `named`, `mirror_x`, `PartName`, `part_share`. Certified checks: `certify_positive`, `certify_clear`, `certify_apart`, `Certified`. Measures: `hit`, `top`, `bottom`, `width`, `front`, `back`, `farthest`, `part_named` (§23.6) |
| `std::quat` | `Quat`: rotations as unit quaternions, on the CPU and the GPU |
| `std::derive` | `gradient`, `value_and_gradient`, `interval`, and the boxes that they range over (§14.1). Each box has `sample(seed, i)`: points spread evenly through it, to check a property at many points (§13.2) |
| `std::stage` | `Tape`, `Op`, `Reg` and `interpret`: fields built at run time (§14.4) |
| `std::gpu` | Buffers (`GpuBuffer<T>`, `GpuSpan<T>`, `buffer`, `write`, `copy`, `read`), the GPU time of labelled work (`label`, `timed`), textures and samplers, passes, `dispatch` and `draw`, kernel outputs (`Slots<T>`, `Shared<T, N>`, `Atomics<T>`, `Append<T>`, `AtomicMap`, `Texels`, `Texels3d`), typed builtins, lossy encodings, `limits()` (§18) |
| `std::io` | `load`, `store`, `fetch` and `post`, polled through `Pending<T>`; `print` (§17.1, §17.3) |
| `std::tool` | A tool's start, its calls and its tasks (§15.5): `args`, `secret`; `Call<T>` (`wait`, `request`), `Request`, `any`, `Answer`, `Done`, `ToolError`; `spawn`, `Task`, `all` |
| `std::tool::files` | A tool's files in its granted roots: `read`, `write`, `list`, `stat`, `remove`, `remove_all`, `make_dirs`, `read_blob`, `write_blob`; `Entry`, `Kind`; `rooted(root, path)`, a command line's relative path as a path in a root, or `None` if it isn't a plain one |
| `std::tool::blob` | Bytes the tool host holds for a tool: `Blob`, `new`, `append`, `read`, `len`, `release`; `Data`, bytes or a blob, which `http` and `run` carry |
| `std::tool::time` | A tool's timer and the date: `after`, `utc_now`, `Utc` |
| `std::tool::http` | HTTPS to a granted host: `fetch`, `get`, `Request`, `Response`, `Header`, `is_transport` |
| `std::tool::run` | A granted program: `run`, `command`, `Command`, `Var`, `Ran` |
| `std::tool::toolchain` | The compiler's work on packages in the tool's roots: `build` (`options`, `BuildOptions`, `Override`, `Built`), `test` (`Tested`, `TestResult`), `check` (`Checked`, `Pipeline`), `fmt` (`Formatted`, `Unformatted`), `doc_export`, `query`, `literals` (`Literals`), `edit`; `Diagnostic` |
| `std::tool::drive` | A build on the native host, in a process of its own: `load` (`cpu`, `gpu`, `Load`, `Timing`, `Build`, `Loaded`, `Code`), `steps` and `run` of a list of `Step`s (`Scalar`), what they saw (`Seen`, every printed line through `lines`: `Ran`, `Span`, `Capture`, `Audio`, `Phase`, `Commands`, `Recorded`, `Bound`, `Trap`, `Stage`), `posts` (`Post`), `answer`, `fail`, `close` |
| `std::tool::image` | Images on the CPU: `decode`, `resize`, `encode_png`, `encode_webp`, `fit`; `Image`, `Size` |
| `std::tool::serve` | A local static server over a root: `start` (`Started`; `lan: true` serves the LAN too, answering requests that name one of this machine's private addresses, never a name, from this machine or a private network: RFC 1918's, 100.64/10 and link-local), `next_post` (`Post`), `answer`, `close`; `Server` |
| `std::tool::browser` | Chrome driven for a tool: `open` (`Browser`), `run` with a `Script` of steps (`Element`, `Until`, `Match`, `Permission`), the observations `title`, `location`, `clipboard`, `count`, `texts`, `attribute`, `active` (`Focused`), `console` and `results`, `capture` (`Capture`), `navigate` (the same tab to another page, its caches warm, and its result its own), `close` |
| `std::tool::testing` | A tool test's fakes (§13.2): `fake`, `Fake`, `set_utc` |
| `std::host` | A client's application services, each a request: `environment`, `decode_image`, `cursor`, `icon`, embedded programs (`open`, `wait_ready`, `show`, `send`, `receive`, `reply`, `wait_closed`, `close`), files (`import_file`, `export_file`), the clipboard (`offer`, `copy`, `pasted`), text input, `publish` (the semantic tree), the page's address and links, passkeys (`create_passkey`, `use_passkey`, §19.4); and their answers read: `read_environment` (`Environment`), `read_surface`, `read_call` (`Call`), `read_pasted`, `read_glyph` (`GlyphImage`) (§17.1) |
| `std::input` | `events`, `Event`, `Key`, `Button`, `Mods`, `payload` (a message's bytes), `Input`, the keys and buttons held (§17.2), and `Script`, a script of input events for a frame test (§13.2) |
| `std::semantics` | A program's semantic tree, which a host shows as real elements: `Node`, `Role`, `Field`, `FieldKind`; `structure`, `geometry` and `clip`, the tree's bytes; `changes`, the part of the tree to send since `Sent`; `steps`, a slider's step from an `Assist`; `FieldState` and `read_field_state`, a field's text as the host has it (§17.2) |
| `std::pack` | `Pack`: values in as few bits as they allow, read back exactly (§6.11) |
| `std::net` | `Room`, `Log`, `Reduce`: a room's ordered log, delivered exactly once; the host's network commands: a client's `open`, `send` and `close`, and a server's `keep`, `compact`, `tag` and `serve_room` (§19) |
| `std::store` | A server's store, which outlives its builds: `get` (`value`), `scan` (`entries`, `Entry`), `put`, `delete`; keys in order: `append_u32`, `append_u64`, `past` (§19.4) |
| `std::crypto` | A server's randomness and signature checks: `random`, `verify` (`Signature`, `verified`), and keys in the forms it takes, `p256_key` and `rsa_key` (§19.4) |
| `std::webauthn` | A server's checks of a passkey's ceremonies: `registration` (`Credential`), `assertion` (`Signed`), against what it `Expected`; `base64url` (§19.4) |
| `std::time` | `now`, seconds on the host's clock, and `since`, to time work while the program runs; both are `nondet`, so not in `@deterministic` code, a constant or a test (§17.3). `phase(name)` marks a phase of the thread's work, which the host times and reports (`trace`); it is available everywhere except GPU, `@audio` and derived code |
| `std::lift` | A lifted build's literals: `Literal`, `count`, `literal`, their values, sources and files, `set`, `generation` and `Watch`; `gradient` by literals, and `reads`; the spec types `Checks`, `Loss` and `Report`; `Minimize`, which makes a function of some literals as small as it can (BFGS, then rounding to the literals' decimals) (§23.1, §23.6) |
| `std::reload` | Hot reload: `keep` gives bytes to the build that replaces this one, and that build's `kept` returns them. One build hands its place to another with `prepare`, `unprepare` and `hand_over` (§15.4, §23.2) |
| `std::job` | Work over several frames: `Job<f>`, a `@job fn` started and resumed to each `yield`, and `Step`, what a `resume` did (§16.3) |
| `std::cache` | Results cooked from inputs, and cooked again when the inputs change: `Stale<K>`, what a result was cooked from, and `Cached<K, V>`, a value kept until its key changes (§16.5) |
| `std::audio` | `play`, the program's voice, and `ring`, a queue to it (§16.4) |
| `std::par` | The workers' protocol behind `par_each_mut` and `par_map_reduce` (§16.1); nothing public |
| `std::mem`, `std::alloc` | The unsafe core: raw memory and the allocator (§6.12) |

### 20.3 Fields are std code

A field is ordinary std code: its trait, its primitives and its combinators are in `std::field`, and a program adds its own the same way.

- **The field trait is `std::field::Surface`:** a signed distance (`distance(self, p: vec3) -> f32`), with `gradient`, `sample` and `interval` derived, and `to_tape`, its tape (§14.4). std gives primitives (`sphere`, `ellipsoid`, `round_cone`, `half_space`), combinators (`union`, `smooth_union`, `intersect`, `translate`, `displace`) and noise (`value_noise`, `fbm`).
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
- **A field's data can be a texture.** `std::gpu::Texture3dHandle<F>` is a 3D texture's handle as data, `Copy` and `GpuData` (`t.to_handle()`), so a primitive can hold one: `std::field::Sampled3d` is a surface whose distances are an `R32Float` texture's texels on a grid (its corner `origin`, each texel `voxel` on a side), read between the eight texels round a point, with the trilinear's own gradient. Where an entry point takes a value that holds a handle, at any depth of its fields (a `Sampled3d`, or a `Union` of one and an analytic field), the compiler binds the texture beside it as it binds a group's (§18): its other fields are the entry point's uniforms. So a march generic over `Surface` takes a cooked field as it takes an analytic one. GPU code reads the texels; CPU code only the size (E0607). A handle names its texture while the texture lives; its owner keeps it. A closure in GPU code can't capture such a value yet (E0702), so a derived gradient of a combinator over one is computed on the CPU's side of the program or by the primitive's own `gradient`.
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
- **`async` and `await`, and `Pin`.** IO and readback are polled requests, and a request's handle is an ordinary value (§17.1). A tool, which has no frame, blocks its thread on a call instead, and runs several sequences of calls as tasks on its helpers (§15.5, §16.2).
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

The command-line tools for agents (§23.9) and a tool's toolchain service (§15.5) use all three.

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
- **`std::lift`** is the program's view of its table: `count()` and `literal(i)`; a `Literal`'s `value()`, `built_value()`, `set(v)` and `source()` (file, bytes, line, column, and the written mantissa's unit including its exponent and suffix); `files()`, `file_name(i)`, `file_path(i)`, `file_text(i)` and `file_hash(i)`. A build that is not lifted has no literals: `count()` is 0.
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
- If one save changes more than 16,384 lifted literals, the watcher builds again. A live poll sends at most 32 changes or 1 MiB at once; the page requests the remaining changes by their sequence. The text of one build error in a live event is at most 64 KiB.
- **Any other edit** builds the program again. The host swaps the new build in where the old one ran, on the same device: the page is not reloaded, and the native host's `Host::reload` stays in its process. The host keeps each pipeline whose WGSL and manifest entry are unchanged. The new build starts with `init`:
  - **`std::reload::keep(bytes)`** gives the host bytes to keep, in place of the bytes the program kept before. The next build's **`kept()`** returns them (no bytes if it replaced no build). The program decides what it carries and how, for example with `std::serialize`'s `save` and `load`, which fail cleanly when the type has changed.
  - **The ticker first runs the old build's ticks again,** each with the records it had. So the sim is where it was, or where the new code takes the same input.
  - Input that the old build did not read goes to the new one. A program that plays a voice (`std::audio`) is reloaded with its page.
- The page tells the server the first frame that drew each change (`/live/shown`), and `wrela run` prints how long after the save that frame was.
- The live server handles at most 64 open requests at once; an extra connection gets 503. A request must finish its headers and body within 10 seconds, even if its bytes keep arriving. A `/live/shown` report is at most 64 KiB. A `PUT /results/<name>` streams its body into a temporary file, up to 256 MiB, before the file takes the result's name.

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

### 23.6 Specs: proportions as code

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
- **Run with a `Report`,** the checks say how they stand: each check's value, target and miss, and how many hold. **Run with a `Loss`,** they are one number: the sum of each miss over its `within`, squared. The loss touches no memory, so `std::lift::gradient` derives it.
- **`wrela solve <package> --spec --free <file>[:<lines>]`** changes the literals on those lines (a whole file if no lines are given) until the spec holds. It runs BFGS on the loss's gradient, then rounds each literal to its decimals, with more decimals where rounding would undo the solve. It reports each check before and after; `--write` writes the literals back through `wrela edit`.
- A spec can move only the subject package's own literals. So a proportion of a base (§23.8) is written in the author's file, to be one of them: `Quadruped { withers: 0.80, ..paws() }`.

### 23.7 Blueprints

A blueprint is outlines of the subject from the side, the front and the top, written as code. It is a module `blueprint` in the subject package with any of `pub fn side()`, `front()` and `top()`. Each returns `Vec<vec2>`: the outline's points in order (a closed polygon), in metres, in its view's coordinates:

- from the side: (z, y), with +z the subject's front;
- from the front: (x, y);
- from the top: (x, z).

An outline is a reference: a tool can draw it over its view, measure where the subject's silhouette misses it, and fit literals to it.

### 23.8 What the medium gives an author

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
- **`wrela doc`** reads the packages that a package depends on (`wrela doc engine::quadruped <package>`), shows a struct's fields with their doc comments, and lists the built-in functions and types (`wrela doc builtins`). A built-in type has a page too: `wrela doc vec3` (or `u8`, `str`, `[T]`, `run`, `[T; N]`) shows its section of this spec, the built-in functions that take it as a method, and std's methods of it.
- **`wrela doc --export <dir> [--commit <sha>] [<package-dir>...]`** writes the reference docs into a new directory, from the same index as `wrela doc`. The same sources give the same bytes. A package given can't be named `std`, `spec` or `builtins`, in any case: those pages are the toolchain's. It writes:
  - in Markdown: this spec split at its sections, the lexical structure, the grammar, and a page for each module of std and of each package given;
  - `index.json`: each public item (its path, kind, signature, doc, page, anchor, and source file and line) and each section of the spec's prose;
  - `llms.txt`, a map of the pages that says how to read them, and `llms-full.txt`, every page in one file; each group's pages are in one file too (`spec/llms-full.txt`, `std/llms-full.txt`), with its size in `llms.txt`.

### 23.9 Tools for agents

These commands let an agent read what the compiler knows about a package, instead of guessing it, and change or measure the package. Each takes a package directory and answers in JSON (`--json`).

| Command | What it does |
|---|---|
| `wrela query <package> <query>...` | Answers a batch of queries with one check (below) |
| `wrela context <item> [<package>] --budget n` | Gives the item's source, the types it names, what it calls, where it is called and its impls, in about `n` tokens. On a program with errors, it says at the end what the errors keep from being known |
| `wrela refactor <package> <change>` | Makes a change across the package: `rename`, `move`, `add-param`, `change-mode`, `extract-const`, `inline` or `extract-fn` (below) |
| `wrela fix <package-dir> [--dry-run] [--json]` | Makes every fix a tool can make, and reports each one; a fix that is the author's choice is listed, not made (§6.13). `--dry-run` prints the diff and writes nothing |
| `wrela migrate <package-dir> [--dry-run] [--json]` | Makes every migration's fix and nothing else (§10.2): after a workspace moves its toolchain pin, one command brings each member to the new forms |
| `wrela solve <package> --minimize <fn> --free <file>:<lines>` | Minimizes a function of no arguments over the literals on those lines, and rounds them to their decimals. It builds a program around the function that uses `std::lift::Minimize` (BFGS on `std::lift::gradient`). `--write` writes the literals through `wrela edit` |
| `wrela precision <package> <fn> --box <lo>..<hi>,...` | Says, operation by operation, where a half-float twin of a function of a point would be safe, from `std::derive::precision` over each box (§14.3), and names the first operation that isn't. `<fn>` is `<module>::<name>` in the package, or a path in std |
| `wrela run <package> [--port N] [--lan]` | Serves the program lifted on 127.0.0.1 with hot reload (§23.2): a saved literal reaches the running program at its next frame, and any other edit swaps in a new build, in the same page. `--lan` serves this machine's private addresses too, to this machine and private networks only, and prints each (§15.5) |
| `wrela trace <package> --watch <exports>` | Runs frames, in lockstep with the program's ticker if it has one (as `wrela-host` does), and prints what the exports say after each frame (`--csv`) |
| `wrela bisect <package> --until "<export> <op> <number>"` | Finds the first frame at which a condition holds (`nan <export>` too) |
| `wrela same <package> --against <build-dir>` | Whether the package built now behaves as a saved build of it does (below) |
| `wrela profile <package> [--frames n]` | Shows where a frame's time goes on this machine (below) |
| `wrela primer [area]` | Teaches the language area by area: each rule of the conformance suite, with a program that keeps it and one that breaks it |

**`wrela query`** takes its queries from the arguments, or one per line on standard input:

- `type <file>:<line>:<col>`, `callers <fn>`, `callees <fn>`, `impls <trait or type>` and `instantiations <fn>`;
- `fields <file>:<line>:<col>`: the type of what is there, the path another package names it by (if it is a public struct or enum without type arguments), and each public field of a struct by its path from it (`albedo`, `albedo.r`), with its type, through the public fields of the structs it holds, four deep;
- `effects <fn>`: each effect, with a call chain to where it happens;
- `borrows <file>:<line>`: the loans live and the places moved out of before the line runs;
- `search <signature>`, for example `(vec3, _) -> f32`.

It also answers on a program with errors. A function whose body has an error has no memory IR, so what it calls, borrows and does is not known. An answer that such a function, or an error outside every body, could change lists those errors as `unsure`.

**`wrela refactor`** plans the change and checks the new texts before it writes anything. It refuses the change, with the errors, if the program would not check, and refuses it if a call would quietly bind to another function. `--dry-run --json` gives a plan with each file's hash; `apply <plan>` refuses the plan if a file has changed since.

- `extract-const <file>:<line>:<col> <NAME>` makes the expression there, of literals, unit suffixes and constants alone, a constant of its module, with the type the program gives it there.
- `inline <fn>` makes each call of a free function whose body is one expression that expression, and removes the function if it is private. It inlines only where nothing about a call can be observed to move: each parameter is used exactly once and in its order, the body binds no parameter's name again, and each argument is a parameter or a local whose type is written where it is declared. The body becomes a block that binds it with the function's result type, `{ let f: T = body; f }`, so its numbers keep their types and its temporaries end where they ended in the function. Anything else is refused, with why.
- `extract-fn <file>:<first>-<last> <name>` makes whole lines of statements of one block a function after the item they are in, which takes the locals they use that are bound before them and returns the one they bind that the code after uses; a parameter the lines change is `mut`, and one they move is `take`. Lines that leave early, that make two values used after them, or that bind a value that owns something (a buffer, a `Vec`) which the code after doesn't use, unless they end their block, are refused: the new function would drop it sooner.

**`wrela same`** runs a saved build (`wrela build -o <dir>`) and the package built now, from `init`, on one worker, each with storage of its own and the same input (`--input`), for `--frames` frames (60 unless said), numbered from 0. Two builds are the same when every command byte each submits is the same, frame by frame, their last frames are within a tolerance (`--within`, a mean over every channel in 8-bit steps, 0.5 unless said, as a golden is compared), and they reach the same pipelines. It reports the first frame whose commands differ, how far the last frames are apart, and the pipelines added or removed (`--json` for tools); with `--no-gpu`, or for a program with no pipelines, it compares the commands only. So a refactor, a change to std or a new toolchain can be checked to keep what a program does.

**`wrela profile`** builds the program as `wrela build` builds it and runs it on the native host's GPU. It reports:

- each label's median milliseconds per frame (a pass or a dispatch, named by `std::gpu::label`), and the frame's span;
- work done in some frames only (for example, a first frame's cooking), by its total;
- the median frame call's CPU time, and the phases that the program timed (`std::time::phase`);
- each pipeline's WGSL bytes.

Each time is a median, with its 95th percentile beside it, because a budget is about the slow frames too. `--worst n` (5 by default) names the slowest frames as well: natively by their call, and with `--chrome` by their paced interval. Each shows its call, its GPU span, its phases and its labels' GPU time. `--serial` runs each pass alone, so a label's time is its own where passes overlap (as on Apple GPUs). Times belong to the machine: compare a change with a profile from before it. `--baseline <report.json>` (an earlier `--json` report) does this: each time shows its earlier value and how it changed, and the report lists the labels that no longer run.

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
| `trait.assoc-const` | A trait's associated constant, with an optional default, is given by each impl, read as `S::K`, and folded per instantiation | §3.4 |
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
| `mem.temporaries` | A temporary ends with its statement, and a binding can't keep a projection of one | §6.1 |
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
| `const.tests` | A `@test` is a free function of no parameters and no result, run as constants are; a tool test (`@test(tool)`) is one too, run on the tool host | §13.2 |
| `derive.gradient` | `gradient` of a closure or function of an `f32` or a float vector | §14.2 |
| `derive.interval` | `interval` over a box, of a function whose loops run a fixed count, whatever its input | §14.2 |
| `derive.tape` | `tape` of a function of a `vec3`, whose work on the point is the tape's operations, on the CPU | §14.4 |
| `gpu.roles` | A client build runs `init` and `frame`, a server build `serve_init` and `serve`, a tool build `tool`, and `@client`, `@server` and `@tool` code stays in its role | §15.3 |
| `fn.jobs` | A `@job fn` runs over several frames, to each `yield`: `f.start(...)` takes its `take` parameters, and `job.resume(...)` its `borrow` and `mut` ones | §16.3 |
| `fn.job-values` | A job's value is `Clone`, `StateHash` and `Serialize` when the locals it holds are | §16.3 |
| `mem.jobs` | A job keeps only owned values across a `yield`: no projection or loan | §16.3 |
| `eff.requests` | `@deterministic` code neither makes requests (`io`) nor polls them (`nondet`) | §17.1 |
| `eff.input` | Reading input is `nondet`, so `@deterministic` code takes events as data | §17.2 |
| `eff.clock` | Reading the clock is `nondet`, so `@deterministic` code takes its time as data | §17.3 |
| `gpu.data` | Data that crosses to the GPU is `GpuData` | §18 |
| `gpu.entry` | An entry point takes builtins, uniforms, buffers, textures, groups and outputs, and returns what its stage does | §18.1 |
| `gpu.const` | An entry point's `const` parameter is a `bool`, `u32`, `i32` or `f32` whose value is a build-time constant, written where it's dispatched or drawn | §18.1 |
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
| `gpu.draw-state` | A draw's cull mode, depth bias, depth test and blends are build-time constants | §18.7 |
| `gpu.textures` | Textures are sampled in fragment shaders, read anywhere on the GPU, and drawn into in passes, by their mip levels and layers | §18.8 |
| `gpu.formats` | A texture's format is a type: its texel, and whether it's filtered, written by kernels and drawn into | §18.8 |
| `gpu.features` | Half floats are GPU code's, and subgroup operations a kernel's, in uniform control flow, on a device with the feature | §18.10 |
| `build.budget` | A shipped build's pipelines have at most 256 KiB of WGSL each | §18.9 |
