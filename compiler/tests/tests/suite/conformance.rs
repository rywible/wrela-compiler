//! AC4: the conformance suite (compiler/tests/conformance). Every tier-0 rule of
//! docs/language.md, the memory model (§6) included, has at least one program that's accepted
//! and one that's rejected with the rule's diagnostic code.
//!
//! A case is a `.wrela` file, built as a one-file package (its `main.wrela`), or a directory,
//! built as a package. Its first lines say which rules it covers:
//!
//! ```text
//! // accepts: mem.take mem.copy
//! // rejects: mem.take
//! ```
//!
//! A rejecting case marks each expected error on the line where its span starts, with
//! `//~ E0500` (or `//~^ E0500` for the line above; several codes separated by spaces). The
//! errors the compiler reports, through lowering, must be exactly the marked ones; an
//! accepting case must report none. Warnings aren't checked.

use crate::scratch;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use wrela_tests::{cases, files_under, header, repo_root};

/// The tier-0 rules, by section of docs/language.md.
const RULES: &[(&str, &str)] = &[
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
    ("struct.defaults", "struct fields may have constant defaults a literal may omit"),
    ("struct.opt-in", "a struct opts in to Copy, Clone and GpuData in its declaration"),
    ("struct.base", "`..base` fills the remaining fields from another value"),
    ("enum.match", "enums with payloads, matched exhaustively"),
    ("trait.items", "traits with associated types and default methods; impls provide the rest"),
    ("trait.orphan", "an impl lives with its trait or its type"),
    ("const.literal", "a `const` holds a literal value in tier 0"),
    ("mod.use", "a file is a module; `use` imports; `pub` exports"),
    ("mod.names", "one namespace per module; names are defined once"),
    // §4 types
    ("ty.scalars", "the scalar types, checked without implicit conversion"),
    ("ty.cpu-only", "f64, i64, u64 and the small integers are CPU only"),
    ("ty.vectors", "vectors and matrices: constructors and swizzles"),
    ("ty.arrays", "fixed-size arrays `[T; N]` with constant lengths"),
    ("ty.runs", "a run `[T]` is a parameter type only"),
    ("ty.tuples", "tuples"),
    ("ty.option", "`Option<T>`; there's no null"),
    ("ty.no-ref", "`&T` isn't a type"),
    ("ty.tiers", "units, strings, `?`, `unsafe` and `dyn` aren't tier 0"),
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
    // §7 generics and traits
    ("gen.bounds", "generic code is checked against its bounds and monomorphized"),
    ("gen.impl-trait", "a trait-shaped parameter makes a function implicitly generic"),
    // §8 effects
    (
        "eff.gpu",
        "GPU entry points forbid host calls and recursion, through every call, per instantiation",
    ),
    ("eff.derived", "a derived interpretation needs a function with no host effect"),
    // §9 attributes
    ("attr.closed", "attributes are a closed set; tier-1 ones aren't available"),
    ("attr.gpu", "`@gpu` asserts a function is GPU-safe, checked at its definition"),
    // §12 GPU code
    ("gpu.entry", "entry points' signatures: builtins, uniforms, buffers, varyings"),
    ("gpu.kernel-mut", "a kernel's `mut` parameters are invocation-safe (`Slots<T>`)"),
    ("gpu.data", "data that crosses to the GPU is `GpuData`"),
    ("gpu.dispatch", "`dispatch` and `draw` match their entry points; entry points aren't called"),
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

struct Case {
    name: String,
    /// The package's files: (name, text).
    files: Vec<(String, String)>,
    accepts: Vec<String>,
    rejects: Vec<String>,
    /// Expected errors: (file, line from 1) -> codes.
    expected: BTreeMap<(String, usize), Vec<String>>,
}

/// The rules a case's header names after `key`.
fn rules(text: &str, key: &str) -> Vec<String> {
    header(text, key).flat_map(str::split_whitespace).map(String::from).collect()
}

fn load_case(path: &Path) -> Case {
    let name = path.file_name().expect("name").to_string_lossy().into_owned();
    let files: Vec<(String, String)> = if path.is_dir() {
        files_under(path, &[])
            .iter()
            .map(|p| {
                let rel = p.strip_prefix(path).expect("inside").to_string_lossy().into_owned();
                (rel, std::fs::read_to_string(p).expect("read"))
            })
            .collect()
    } else {
        vec![("main.wrela".into(), std::fs::read_to_string(path).expect("read"))]
    };
    let main = &files.iter().find(|f| f.0 == "main.wrela").expect("a main.wrela").1;
    let (accepts, rejects) = (rules(main, "accepts"), rules(main, "rejects"));
    let mut expected: BTreeMap<(String, usize), Vec<String>> = BTreeMap::new();
    for (file, text) in &files {
        for (i, line) in text.lines().enumerate() {
            if let Some(at) = line.find("//~") {
                let mut rest = &line[at + 3..];
                let mut target = i + 1;
                while let Some(r) = rest.strip_prefix('^') {
                    target -= 1;
                    rest = r;
                }
                for code in rest.split_whitespace() {
                    expected.entry((file.clone(), target)).or_default().push(code.to_string());
                }
            }
        }
    }
    for codes in expected.values_mut() {
        codes.sort();
    }
    Case { name, files, accepts, rejects, expected }
}

/// The errors a case's package gets, through lowering: (file, line) -> codes.
fn errors(case: &Case) -> BTreeMap<(String, usize), Vec<String>> {
    let dir = scratch(&format!("conformance/{}", case.name));
    for (file, text) in &case.files {
        let p = dir.join(file);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("dir");
        std::fs::write(p, text).expect("write");
    }
    let built = wrela_driver::build(&dir);
    let mut out: BTreeMap<(String, usize), Vec<String>> = BTreeMap::new();
    // A case is about something else than `frame`, which a program must export: a case
    // without one isn't told so (`a_program_without_frame_is_rejected` is).
    let no_frame = |d: &wrela_diag::Diagnostic| {
        d.code == wrela_diag::codes::E0703 && d.span().is_some_and(|s| s.start == 0 && s.end == 0)
    };
    for d in built.diagnostics.iter().filter(|d| d.is_error() && !no_frame(d)) {
        let Some(span) = d.span() else {
            // An internal error: a case can't expect one.
            out.entry(("<internal>".into(), 0)).or_default().push(d.code.as_str().to_string());
            continue;
        };
        let file = built.sources.file(span.file);
        let line = file.line_index(span.start) + 1;
        let name = file.name.rsplit('/').next().unwrap_or(&file.name).to_string();
        let key = if file.name.starts_with('<') { file.name.clone() } else { name };
        out.entry((key, line)).or_default().push(d.code.as_str().to_string());
    }
    for codes in out.values_mut() {
        codes.sort();
    }
    out
}

#[test]
fn conformance() {
    let paths = cases(&repo_root().join("compiler/tests/conformance"));
    let known: BTreeSet<&str> = RULES.iter().map(|r| r.0).collect();
    let mut accepted: BTreeSet<String> = BTreeSet::new();
    let mut rejected: BTreeSet<String> = BTreeSet::new();
    let mut failures = Vec::new();
    for path in &paths {
        let case = load_case(path);
        for r in case.accepts.iter().chain(&case.rejects) {
            assert!(known.contains(r.as_str()), "{}: unknown rule `{r}`", case.name);
        }
        assert!(
            !case.accepts.is_empty() || !case.rejects.is_empty(),
            "{}: names no rules",
            case.name
        );
        assert!(
            case.rejects.is_empty() || !case.expected.is_empty(),
            "{}: rejects a rule but marks no errors",
            case.name
        );
        assert!(
            !case.rejects.is_empty() || case.expected.is_empty(),
            "{}: marks errors but rejects no rule",
            case.name
        );
        let got = errors(&case);
        if got != case.expected {
            failures.push(format!(
                "{}:\n  expected {:?}\n  got      {:?}",
                case.name, case.expected, got
            ));
        }
        accepted.extend(case.accepts.iter().cloned());
        rejected.extend(case.rejects.iter().cloned());
    }
    let missing: Vec<String> = RULES
        .iter()
        .filter_map(|(id, what)| {
            let a = accepted.contains(*id);
            let r = rejected.contains(*id);
            (!a || !r).then(|| {
                format!(
                    "{id} ({what}): {}{}",
                    if a { "" } else { "no accepting case " },
                    if r { "" } else { "no rejecting case" }
                )
            })
        })
        .collect();
    println!("{} conformance cases cover {} rules", paths.len(), RULES.len());
    assert!(failures.is_empty(), "{} cases failed:\n{}", failures.len(), failures.join("\n"));
    assert!(missing.is_empty(), "rules without cases:\n{}", missing.join("\n"));
}

/// A program without `frame` is an error (E0703) at the start of main.wrela, since no host can
/// run it. (The cases aren't told: see `errors`.)
#[test]
fn a_program_without_frame_is_rejected() {
    let dir = scratch("conformance-no-frame");
    std::fs::write(dir.join("main.wrela"), "pub fn f() -> f32 {\n    1.0\n}\n").expect("write");
    let built = wrela_driver::build(&dir);
    let errors: Vec<_> = built.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code.as_str(), "E0703");
    assert_eq!(errors[0].span().map(|s| (s.start, s.end)), Some((0, 0)));
}
