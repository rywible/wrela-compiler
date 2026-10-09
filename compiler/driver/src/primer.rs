//! `wrela primer [area]`: the language, area by area, as its conformance suite shows it (#39,
//! the owner's additions). Each rule's statement is the rule table's (`RULES`), its example a
//! program the suite accepts, and its counterexample the lines the suite rejects, with the codes
//! they're rejected with. Everything shown is checked by the suite's tests (`conformance.rs`),
//! so the primer can't drift from the compiler.

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/conformance.rs"));
}

/// Every rule of the conformance suite, by section of docs/language.md (which names each one
/// where it's written), with what it says: the suite's test (`conformance.rs`) checks that this
/// table, language.md and the suite's cases name the same rules.
pub const RULES: &[(&str, &str)] = &[
    // §2 lexical structure and statements
    (
        "lex.newline",
        "a newline ends a statement unless brackets are open or the next line starts with `.`",
    ),
    ("lex.trailing-op", "an operator that continues a line trails it; a line can't start with one"),
    ("lex.semicolon", "`;` separates statements on one line"),
    ("lex.comments", "`//` and `///` comments; no block comments"),
    ("lex.no-suffix", "number literals have no type suffixes"),
    ("lex.literals", "an integer literal must fit its type"),
    ("lex.pow-xor", "`**` is exponentiation, `^` is XOR on integers"),
    ("lex.chars", "only the language's characters outside comments"),
    ("lex.else", "`else` follows its `}` on the same line"),
    // §3 items
    ("fn.named-args", "positional arguments first, then named ones, each once, all present"),
    ("fn.defaults", "parameters may have defaults"),
    ("fn.return", "a function that returns a value ends with one, or returns one, on every path"),
    ("fn.return-trait", "a trait in return position names one inferred concrete type"),
    ("fn.values", "a named function is a value, and so is a type's associated function: `W::work`"),
    (
        "fn.jobs",
        "a `@job fn` runs over several frames, to each `yield`: `f.start(...)` takes its `take` parameters, `job.resume(...)` its `borrow` and `mut` ones",
    ),
    ("struct.defaults", "struct fields may have constant defaults a literal may omit"),
    ("struct.opt-in", "a struct opts in to Copy, Clone and GpuData in its declaration"),
    ("struct.base", "`..base` fills the remaining fields from another value"),
    (
        "struct.packed",
        "a `Packed` struct's `Bits<N>` fields are bits of one `u32`, read and written as `u32`s",
    ),
    ("enum.match", "enums with payloads, matched exhaustively"),
    (
        "enum.discriminants",
        "a fieldless enum's variants are its discriminants: `u32(e)`, `E::from_u32(n)`, `Flags<E>`",
    ),
    ("trait.items", "traits with associated types and default methods; impls provide the rest"),
    ("trait.orphan", "an impl lives with its trait or its type"),
    (
        "trait.diagnostic",
        "`@diagnostic(\"…\")` on a trait or type replaces the message when a bound isn't met",
    ),
    (
        "trait.fieldwise",
        "a `@fieldwise` trait is derived field by field for each type that declares it",
    ),
    ("trait.eq-ord", "`==` and `<` come from declared `Eq` and `Ord`"),
    (
        "trait.sets",
        "`trait Sim = A + B` names a set of traits, which stands for them wherever traits are listed",
    ),
    (
        "const.build",
        "the build computes a `const`; a panic, a trap or running past its fuel is an error",
    ),
    ("const.places", "a constant is a place: read and projected, never moved out of or changed"),
    (
        "const.tests",
        "a `@test` is a free function of no parameters and no result, run as constants are",
    ),
    ("build.budget", "a shipped build's pipelines have at most 256 KiB of WGSL each"),
    ("mod.use", "a file is a module; `use` imports; `pub` exports"),
    (
        "mod.packages",
        "a package depends on others by path; only their `pub` items cross, and never in a cycle",
    ),
    ("mod.names", "one namespace per module; names are defined once"),
    // §4 types
    ("ty.scalars", "the scalar types, checked without implicit conversion"),
    ("ty.cpu-only", "f64, i64, u64 and the small integers are CPU only"),
    ("ty.vectors", "vectors and matrices: constructors and swizzles"),
    ("ty.arrays", "fixed-size arrays `[T; N]` with constant lengths"),
    (
        "ty.opaque-alias",
        "an alias that names traits names the type the first function in its module to return it returns",
    ),
    (
        "ty.const-generics",
        "`const N: u32` parameters range over array lengths; a length makes no calls",
    ),
    ("ty.fstrings", "`f\"…\"` builds a `String` from text and values that implement `Format`"),
    (
        "ty.runs",
        "a run `[T]` is a parameter, a result or a binding; never a field or type argument",
    ),
    ("ty.tuples", "tuples"),
    ("ty.option", "`Option<T>`; there's no null"),
    ("ty.no-ref", "`&T` isn't a type"),
    ("ty.no-dyn", "wrela has no `dyn`: an enum or a generic instead"),
    ("ty.alias", "`type Name<T> = Type` names a type; an alias can't name itself"),
    ("stmt.if-let", "`if let pat = x { } else { }` tests a pattern; its value needs an `else`"),
    ("stmt.let-else", "`let pat = x else { }`, whose `else` leaves the scope"),
    ("stmt.while-let", "`while let pat = x { }` runs its body while `x` matches"),
    ("stmt.for-pattern", "`for pat in xs` destructures each element, and its pattern can't fail"),
    ("err.try", "`x?` returns a `None` or an `Err` from a function returning the same kind"),
    ("mem.match-mut", "`match mut place` binds mutable projections of a place"),
    ("mem.let-owns", "`let` owns: a place whose type isn't `Copy` is borrowed, cloned or taken"),
    // §6 memory
    ("mem.copy", "Copy types copy implicitly; other values move only with `take`"),
    ("mem.take", "moving out of a named place is `take`; a moved value can't be used"),
    ("mem.clone", "deep copies are `.clone()` of a `Clone` type"),
    ("mem.modes", "`mut` and `take` parameters are marked at the call site"),
    ("mem.receivers", "a `mut self` call isn't marked; a `take self` call on a named place is"),
    ("mem.bindings", "`let` is read-only, `var` owns mutably, `mut` projects mutably"),
    (
        "mem.projections",
        "a projection returns part of a `borrow` or `mut` parameter, never of a local",
    ),
    ("mem.exclusivity", "nothing touches a place overlapping a live `mut` access"),
    ("mem.loops", "a value from outside a loop can't be moved inside it"),
    ("mem.no-globals", "no mutable globals"),
    ("mem.closures", "closures are non-escaping: passed down, never returned or stored"),
    ("mem.jobs", "a job keeps only owned values across a `yield`: no projection or loan"),
    (
        "mem.arenas",
        "an arena's values are named by handles: `arena[h]` projects, a stale handle panics",
    ),
    (
        "mem.plain",
        "`Plain` data has no pointers; every field of a type that declares it is `Plain`",
    ),
    (
        "mem.borrow-structs",
        "a borrow struct borrows its fields' places as separate parameters would",
    ),
    (
        "mem.drop",
        "values are dropped at the end of their owner's scope; only std's core has destructors",
    ),
    ("mem.unsafe", "`unsafe`, and std's unsafe core, only in packages that declare it"),
    // §7 generics and traits
    ("gen.bounds", "generic code is checked against its bounds and monomorphized"),
    ("gen.impl-trait", "a trait-shaped parameter makes a function implicitly generic"),
    // §8 effects
    (
        "eff.gpu",
        "GPU entry points forbid host calls and recursion, through every call, per instantiation",
    ),
    ("eff.derived", "a derived interpretation needs a function with no host effect"),
    ("eff.input", "reading input is `nondet`, so `@deterministic` code takes events as data"),
    (
        "eff.requests",
        "`@deterministic` code neither makes requests (`io`) nor polls them (`nondet`)",
    ),
    (
        "eff.parallel",
        "a `@parallel fn` writes no data it captures, and does no IO, GPU or non-deterministic work",
    ),
    ("eff.deterministic-fn", "a function passed as a `@deterministic fn` is deterministic"),
    (
        "eff.audio",
        "`@audio` code doesn't allocate, do IO, recurse or record GPU work, and captures nothing",
    ),
    // §9 attributes
    ("attr.closed", "attributes are a closed set; tier-1 ones aren't available"),
    ("attr.gpu", "`@gpu` asserts a function is GPU-safe, checked at its definition"),
    ("attr.testing", "`@testing` marks an export only a test build has"),
    // §12 GPU code
    ("gpu.entry", "entry points' signatures: builtins, uniforms, buffers, varyings"),
    ("gpu.kernel-mut", "a kernel's `mut` parameters are invocation-safe (`Slots<T>`)"),
    ("gpu.data", "data that crosses to the GPU is `GpuData`"),
    ("gpu.dispatch", "`dispatch` and `draw` match their entry points; entry points aren't called"),
    (
        "gpu.draw-state",
        "a draw's cull mode, depth bias and depth test are build-time constants, written where it draws",
    ),
    ("gpu.discard", "`discard()` drops a fragment, in fragment shaders only"),
    (
        "gpu.buffer-modes",
        "a buffer the GPU writes is passed `mut`; buffers and spans bound together don't overlap",
    ),
    ("gpu.buffer-drop", "a GPU buffer is owned: it moves, drops, and lasts in program state"),
    (
        "gpu.workgroup-memory",
        "workgroup memory: each invocation writes its own chunk, and barriers separate writes from reads",
    ),
    ("gpu.atomics", "atomics, appends and atomic maps are kernels' `mut` parameters, passed `mut`"),
    (
        "gpu.textures",
        "textures are sampled in fragment shaders, read anywhere on the GPU, drawn into in passes",
    ),
    (
        "gpu.outputs",
        "indirect work takes typed arguments; a `One<T>` is written by a kernel's first invocation",
    ),
    (
        "gpu.domains",
        "`over:` dispatches a kernel over a count, a size or a texture: its groups and its bounds the compiler's",
    ),
    (
        "gpu.formats",
        "a texture's format is a type: its texel, and whether it's filtered, written by kernels, drawn into",
    ),
    (
        "gpu.groups",
        "a borrow struct of textures, samplers, spans and `GpuData` values is one GPU parameter",
    ),
    (
        "gpu.bound",
        "`k.bind(...)` is a value a command takes; code is generic over `Kernel`, `VertexShader<V>` and `FragmentShader<V>`",
    ),
    ("gpu.cpu", "GPU builtins and derivatives only in GPU code"),
    ("gpu.workgroup", "workgroup sizes within WebGPU's limits"),
    ("gpu.uniformity", "derivatives only where every pixel reaches them (uniform control flow)"),
    // §13 derived interpretations
    ("derive.gradient", "`gradient` of a closure or function of an f32 or float vector"),
    (
        "derive.interval",
        "`interval` over a box, of a function whose loops don't depend on the input",
    ),
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
