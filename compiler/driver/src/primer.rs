//! `wrela primer [area]`: the language, area by area, as its conformance suite shows it (#39,
//! the owner's additions). Each rule's statement is the rule table's (`RULES`), its example a
//! program the suite accepts, and its counterexample the lines the suite rejects, with the codes
//! they're rejected with. Everything shown is checked by the suite's tests (`conformance.rs`),
//! so the primer can't drift from the compiler.

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/conformance.rs"));
}

/// Every rule of the conformance suite, in spec/language.md's order, with what it says: the
/// suite's test (`conformance.rs`) checks that this table, language.md's index (*The rules by
/// ID*) and the suite's cases name the same rules, and the index with these words.
pub const RULES: &[(&str, &str)] = &[
    // §2.1 lines and tokens
    (
        "lex.newline",
        "a newline ends a statement, unless brackets are open or the next line starts with `.`",
    ),
    ("lex.trailing-op", "an operator that continues a line trails it; a line can't start with one"),
    ("lex.semicolon", "`;` separates statements on one line"),
    ("lex.else", "`else` follows its `}` on the same line"),
    ("lex.comments", "comments are `//` and doc comments `///`; there are no block comments"),
    ("lex.chars", "outside comments and strings, only the language's characters"),
    ("lex.literals", "an integer literal fits the type it's used as"),
    ("lex.no-suffix", "a number literal has no type suffix"),
    ("lex.pow-xor", "`**` is exponentiation, and `^` is XOR on integers"),
    // §2.2 control flow
    ("stmt.for-pattern", "`for pat in xs` destructures each element, and its pattern can't fail"),
    (
        "stmt.if-let",
        "`if let pat = x { } else { }` tests a pattern; as a value, it needs an `else`",
    ),
    ("stmt.let-else", "`let pat = x else { }` binds a pattern, and its `else` leaves the scope"),
    ("stmt.while-let", "`while let pat = x { }` runs its body while `x` matches"),
    // §3.1 functions
    ("fn.named-args", "positional arguments come first, then named ones, each once, all present"),
    ("fn.defaults", "a parameter may have a constant default"),
    ("fn.return", "a function that returns a value ends with one, or returns one, on every path"),
    ("fn.return-trait", "a trait in return position names one concrete type, which is inferred"),
    // §3.2 structs
    ("struct.defaults", "a struct's fields may have constant defaults, which a literal may omit"),
    (
        "struct.opt-in",
        "a struct opts in to `Copy`, `Clone`, `GpuData` and its other traits in its declaration",
    ),
    ("struct.base", "`..base` fills a literal's other fields from another value of its type"),
    (
        "struct.packed",
        "a `Packed` struct's `Bits<N>` fields are bits of one `u32`, read and written as `u32`s",
    ),
    // §3.3 enums
    ("enum.match", "an enum's variants hold payloads, and a `match` on it is exhaustive"),
    (
        "enum.discriminants",
        "an enum whose variants hold nothing numbers them: `u32(e)`, `E::from_u32(n)`, `Flags<E>`",
    ),
    // §3.4 traits and impls
    (
        "trait.items",
        "a trait has associated types and default methods, and an impl gives what its trait declares",
    ),
    ("trait.orphan", "an impl lives in the package of its trait or of its type"),
    ("trait.eq-ord", "`==` and `<` come from a declared `Eq` and `Ord`"),
    (
        "trait.fieldwise",
        "a `@fieldwise` trait is derived field by field for each type that declares it",
    ),
    (
        "trait.fieldwise-walk",
        "a fieldwise trait's method can walk `for f in fields(self)`, derived from that body for each type",
    ),
    // §3.5 constants
    (
        "const.build",
        "the build computes a `const`; a panic, a trap or running past its fuel is an error",
    ),
    ("const.places", "a constant is a place: read and projected, never moved out of or changed"),
    // §3.6 modules and packages
    (
        "mod.use",
        "a file is a module, `use` imports, and `pub` makes an item visible outside its file",
    ),
    ("mod.names", "a module has one namespace, and defines each name once"),
    (
        "mod.packages",
        "a package depends on others by path; only `pub` items cross, and never in a cycle",
    ),
    // §4 types
    ("ty.no-ref", "`&T` isn't a type"),
    ("ty.no-dyn", "there's no `dyn`: an enum or a generic does its work"),
    ("ty.option", "an absent value is `Option<T>`; there's no null"),
    // §4.1 scalars and conversions
    ("ty.scalars", "the scalar types never convert implicitly"),
    ("ty.cpu-only", "`f64`, `i64`, `u64` and the 8- and 16-bit integers are CPU only"),
    // §4.2 vectors and matrices
    ("ty.vectors", "vectors and matrices: their constructors, components and swizzles"),
    // §4.3 arrays
    ("ty.arrays", "an array `[T; N]` has a constant length"),
    (
        "ty.array-fill",
        "`[a, b, ..fill]` fills an array to the length its type gives, with a `Copy` fill",
    ),
    (
        "ty.enum-arrays",
        "`[T; E]` has an element for each variant of an enum numbered 0, 1, 2 and on, indexed by it",
    ),
    (
        "ty.const-generics",
        "a `const N: u32` parameter ranges over array lengths; a length makes no calls",
    ),
    // §4.4 runs and tuples
    (
        "ty.runs",
        "a run `[T]` is a parameter, a result or a binding, never a field or a type argument",
    ),
    ("ty.tuples", "tuples"),
    // §4.5 type aliases
    ("ty.alias", "`type Name<T> = Type` names a type; an alias can't name itself"),
    (
        "ty.opaque-alias",
        "an alias that names traits names the type that the first function in its module to return it returns",
    ),
    // §4.6 text and bytes
    ("ty.fstrings", "`f\"…\"` builds a `String` from text and values that implement `Format`"),
    // §6.1 values, copies and moves
    ("mem.copy", "a `Copy` value copies implicitly; any other value moves only with `take`"),
    ("mem.take", "a move out of a named place is written `take`, and a moved value can't be used"),
    ("mem.clone", "a deep copy is `.clone()` of a `Clone` type"),
    ("mem.literal-moves", "a struct literal moves the owned locals it names, without `take`"),
    ("mem.loops", "a value from outside a loop can't be moved inside it"),
    (
        "mem.drop",
        "a value is dropped at the end of its owner's scope; only std's core has destructors",
    ),
    (
        "mem.temporaries",
        "a temporary ends with its statement, and a binding can't keep a projection of one",
    ),
    // §6.2 parameter modes
    ("mem.modes", "`mut` and `take` arguments are marked where they're passed"),
    ("mem.receivers", "a `mut self` call isn't marked; a `take self` call on a named place is"),
    // §6.3 local bindings
    ("mem.let-owns", "`let` owns: a place whose type isn't `Copy` is borrowed, cloned or taken"),
    ("mem.bindings", "`let` and `var` own, and `borrow` and `mut` project a place"),
    ("mem.match-mut", "`match mut place` binds mutable projections of the place"),
    // §6.4 returning projections
    (
        "mem.projections",
        "a projection returns part of a `borrow` or `mut` parameter, or of a constant, never of a local",
    ),
    // §6.5 exclusivity
    ("mem.exclusivity", "nothing touches a place that overlaps a live `mut` access"),
    ("mem.no-globals", "there are no mutable globals"),
    // §6.6 kinds of projection
    (
        "mem.borrow-structs",
        "a borrow struct borrows its fields' places as separate parameters would",
    ),
    // §6.7 closures
    (
        "mem.closures",
        "a closure that captures a projection is passed down; one that captures only values can be stored",
    ),
    ("fn.values", "a named function is a value, and so is a type's associated function: `W::work`"),
    (
        "fn.fields",
        "a function in a struct's field, or a `take` parameter, makes the struct or the function generic over it",
    ),
    ("eff.deterministic-fn", "a function passed as a `@deterministic fn` is deterministic"),
    // §6.8 handles and arenas
    (
        "mem.arenas",
        "an arena's values are named by handles: `arena[h]` projects, and a stale handle panics",
    ),
    // §6.10 plain data
    (
        "mem.plain",
        "`Plain` data has no pointers, and every field of a type that declares it is `Plain`",
    ),
    // §6.12 the unsafe core
    ("mem.unsafe", "`unsafe` only in std's core, and only in packages that declare it"),
    // §7 generics and traits
    ("gen.bounds", "generic code is checked against its bounds, and monomorphized"),
    ("gen.impl-trait", "a trait in parameter position makes a function generic"),
    // §7.1 traits
    (
        "trait.sets",
        "`trait Sim = A + B` names a set of traits, which stands for them wherever traits are listed",
    ),
    (
        "trait.diagnostic",
        "`@diagnostic(\"…\")` on a trait or a type replaces the message when a bound isn't met",
    ),
    // §8.1 the seven effects
    (
        "eff.trace",
        "a line printed or a phase timed is `trace`, allowed everywhere but GPU, `@audio` and derived code",
    ),
    // §8.3 what each context forbids
    (
        "eff.gpu",
        "GPU code doesn't allocate, call the host, recurse or panic, through every call and instantiation",
    ),
    (
        "eff.audio",
        "`@audio` code doesn't allocate, do IO, recurse or record GPU work, and its closures capture nothing",
    ),
    (
        "eff.parallel",
        "a `@parallel fn` writes no data it captures, and does no IO, GPU or non-deterministic work",
    ),
    ("eff.derived", "a derived interpretation needs a function with no host effect"),
    // §9 attributes
    ("attr.closed", "attributes are a closed set"),
    ("attr.gpu", "`@gpu` asserts that a function is GPU-safe, checked where it's defined"),
    ("attr.testing", "`@testing` marks an export that only a test build has"),
    // §10.1 results and panics
    ("err.try", "`x?` returns a `None` or an `Err` from a function that returns the same kind"),
    // §11.4 built-in functions
    ("gpu.uniformity", "derivatives only where every pixel reaches them, in uniform control flow"),
    // §13.1 computing constants
    (
        "const.fuel",
        "`@fuel(n)` on a `const` or a test sets the work it may do, its threads' together",
    ),
    // §13.2 tests
    (
        "const.tests",
        "a `@test` is a free function of no parameters and no result, run as constants are; a tool test is such a `@job fn`, run on the tool host",
    ),
    // §14.2 how they're derived
    ("derive.gradient", "`gradient` of a closure or function of an `f32` or a float vector"),
    (
        "derive.interval",
        "`interval` over a box, of a function whose loops don't depend on its input",
    ),
    // §15.3 client, server and tool roles
    (
        "gpu.roles",
        "a client build runs `init` and `frame`, a server build `serve_init` and `serve`, a tool build `tool_init` and `tool`, and `@client`, `@server` and `@tool` code stays in its role",
    ),
    // §16.3 work over several frames
    (
        "fn.jobs",
        "a `@job fn` runs over several frames, to each `yield`: `f.start(...)` takes its `take` parameters, and `job.resume(...)` its `borrow` and `mut` ones",
    ),
    (
        "fn.job-values",
        "a job's value is `Clone`, `StateHash` and `Serialize` when the locals it holds are",
    ),
    ("mem.jobs", "a job keeps only owned values across a `yield`: no projection or loan"),
    // §17.1 requests, not async
    (
        "eff.requests",
        "`@deterministic` code neither makes requests (`io`) nor polls them (`nondet`)",
    ),
    // §17.2 input and messages
    ("eff.input", "reading input is `nondet`, so `@deterministic` code takes events as data"),
    // §17.3 printing and the clock
    ("eff.clock", "reading the clock is `nondet`, so `@deterministic` code takes its time as data"),
    // §18 gpu code
    ("gpu.data", "data that crosses to the GPU is `GpuData`"),
    // §18.1 entry points
    (
        "gpu.entry",
        "an entry point takes builtins, uniforms, buffers, textures, groups and outputs, and returns what its stage does",
    ),
    ("gpu.cpu", "GPU builtins, derivatives and sampling only in GPU code"),
    ("gpu.workgroup", "a workgroup's size is within WebGPU's limits"),
    ("gpu.discard", "`discard()` drops a fragment, in fragment shaders only"),
    // §18.3 buffers
    (
        "gpu.buffer-modes",
        "a buffer that the GPU writes is passed `mut`, and buffers and spans bound together don't overlap",
    ),
    ("gpu.buffer-drop", "a GPU buffer is owned: it moves, drops, and lasts in the program's state"),
    // §18.4 kernel outputs
    (
        "gpu.kernel-mut",
        "a kernel's `mut` parameters are outputs that invocations can share: `Slots<T>` and its kin",
    ),
    (
        "gpu.atomics",
        "atomics, appends and atomic maps are a kernel's `mut` parameters, passed `mut`",
    ),
    (
        "gpu.outputs",
        "a `One<T>` is written by a kernel's first invocation, and indirect work takes typed arguments",
    ),
    // §18.5 workgroup memory
    (
        "gpu.workgroup-memory",
        "each invocation writes its own chunk of workgroup memory, and barriers separate writes from reads",
    ),
    // §18.6 bindings and groups
    (
        "gpu.groups",
        "a borrow struct of textures, samplers, spans and `GpuData` values is one GPU parameter",
    ),
    // §18.7 commands: dispatches and draws
    (
        "gpu.dispatch",
        "`dispatch` and `draw` bind their entry points' arguments; an entry point isn't called",
    ),
    (
        "gpu.bound",
        "`k.bind(...)` is a value that a command takes, and code is generic over `Kernel`, `VertexShader<V>` and `FragmentShader<V>`",
    ),
    (
        "gpu.domains",
        "`over:` dispatches a kernel over a count, a size or a texture, its groups and bounds the compiler's",
    ),
    ("gpu.draw-state", "a draw's cull mode, depth bias and depth test are build-time constants"),
    // §18.8 textures, formats and passes
    (
        "gpu.textures",
        "textures are sampled in fragment shaders, read anywhere on the GPU, and drawn into in passes",
    ),
    (
        "gpu.formats",
        "a texture's format is a type: its texel, and whether it's filtered, written by kernels and drawn into",
    ),
    // §18.9 pipelines
    ("build.budget", "a shipped build's pipelines have at most 256 KiB of WGSL each"),
];

/// The areas (a rule ID's part before its dot), in language.md's order, each with its rules
/// as `RULES` has them: (ID, what it says).
pub fn areas() -> Vec<(String, Vec<(&'static str, &'static str)>)> {
    let mut out: Vec<(String, Vec<(&str, &str)>)> = Vec::new();
    for &(id, statement) in RULES {
        let area = id.split('.').next().unwrap_or("");
        match out.iter_mut().find(|(a, _)| a == area) {
            Some((_, rs)) => rs.push((id, statement)),
            None => out.push((area.to_string(), vec![(id, statement)])),
        }
    }
    out
}

/// A case: its path in the suite (a file, or a package directory's main), the rules its header
/// says it accepts or rejects, and its text.
struct Case {
    path: &'static str,
    accepts: Vec<String>,
    rejects: Vec<String>,
    text: &'static str,
}

fn cases() -> Vec<Case> {
    embedded::CASES
        .iter()
        .filter(|(p, text)| {
            text.starts_with("// accepts:")
                || text.starts_with("// rejects:")
                || p.ends_with("main.wrela")
        })
        .map(|&(path, text)| {
            let header = |key: &str| -> Vec<String> {
                let prefix = format!("// {key}:");
                text.lines()
                    .take_while(|l| l.starts_with("//"))
                    .filter_map(|l| l.strip_prefix(prefix.as_str()))
                    .flat_map(|v| v.split_whitespace().map(String::from).collect::<Vec<_>>())
                    .collect()
            };
            Case { path, accepts: header("accepts"), rejects: header("rejects"), text }
        })
        .collect()
}

/// A case's code without its header comments, at most `n` lines (the rest elided).
fn body(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .skip_while(|l| l.starts_with("// accepts:") || l.starts_with("// rejects:"))
        .collect();
    let mut out: Vec<String> = lines.iter().take(n).map(|l| l.to_string()).collect();
    if lines.len() > n {
        out.push(format!("// ... ({} more lines)", lines.len() - n));
    }
    out.join("\n")
}

/// The lines of a rejected case the suite expects a code at (`//~ CODE`), each with its codes.
fn rejected_lines(text: &str) -> Vec<(usize, String, Vec<String>)> {
    text.lines()
        .enumerate()
        .filter_map(|(i, l)| {
            let (code, marks) = l.split_once("//~")?;
            let codes = marks
                .split_whitespace()
                .filter(|c| c.len() == 5)
                .map(String::from)
                .collect::<Vec<_>>();
            Some((i + 1, code.trim_end().to_string(), codes))
        })
        .collect()
}

/// The primer: with no area, the areas and their rules' count; with one, each of its rules with
/// an example and a counterexample. `None` if there's no such area.
pub fn primer(area: Option<&str>) -> Option<String> {
    let areas = areas();
    let Some(area) = area else {
        let mut out = String::from(
            "The language by area, from its conformance suite (`wrela primer <area>` for one):\n\n",
        );
        for (a, rs) in &areas {
            let first = rs.first().map_or("", |r| r.1);
            out.push_str(&format!("  {a:<8} {:>2} rules  e.g. {}\n", rs.len(), short(first, 70)));
        }
        return Some(out);
    };
    let (_, rules) = areas.into_iter().find(|(a, _)| a == area)?;
    let cases = cases();
    let mut out = format!("# `{area}`: {} rules, from the conformance suite\n", rules.len());
    for &(id, statement) in &rules {
        out.push_str(&format!("\n## {id}\n\n{statement}\n"));
        if let Some(c) = cases.iter().find(|c| c.accepts.iter().any(|a| a == id)) {
            out.push_str(&format!(
                "\nAccepted (conformance/{}):\n\n```wrela\n{}\n```\n",
                c.path,
                body(c.text, 24)
            ));
        }
        if let Some(c) = cases.iter().find(|c| c.rejects.iter().any(|r| r == id)) {
            out.push_str(&format!("\nRejected (conformance/{}):\n\n", c.path));
            for (line, code, codes) in rejected_lines(c.text).into_iter().take(4) {
                let what: Vec<String> = codes
                    .iter()
                    .map(|k| match wrela_diag::codes::Code::lookup(k) {
                        Some(c) => format!("{k}: {}", c.title()),
                        None => k.clone(),
                    })
                    .collect();
                out.push_str(&format!(
                    "    line {line}: `{}`\n      {}\n",
                    code.trim(),
                    what.join("; ")
                ));
            }
        }
    }
    Some(out)
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(n).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each area's primer lists each of its rules, and every rule the suite holds with a case
    /// shows that case.
    #[test]
    fn every_rule_is_in_its_areas_primer() {
        let cases = cases();
        for (area, rules) in areas() {
            let text = primer(Some(&area)).expect("an area's primer");
            for (id, _) in rules {
                assert!(text.contains(&format!("## {id}\n")), "{area}: {id}");
                let held =
                    cases.iter().any(|c| c.accepts.iter().chain(&c.rejects).any(|r| r == id));
                if held {
                    let section = text.split(&format!("## {id}\n")).nth(1).unwrap_or("");
                    let section = section.split("\n## ").next().unwrap_or("");
                    assert!(
                        section.contains("Accepted (") || section.contains("Rejected ("),
                        "{id}: no example"
                    );
                }
            }
        }
        assert!(primer(Some("nothing")).is_none());
        assert!(primer(None).is_some_and(|t| t.contains("mem")));
    }
}
