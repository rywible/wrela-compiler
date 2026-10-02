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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use wrela_tests::root;

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

fn parse_header(text: &str, key: &str) -> Vec<String> {
    text.lines()
        .take_while(|l| l.starts_with("//"))
        .filter_map(|l| l.strip_prefix(&format!("// {key}:")))
        .flat_map(|rest| rest.split_whitespace().map(String::from))
        .collect()
}

fn load_case(path: &Path) -> Case {
    let name = path.file_name().expect("name").to_string_lossy().into_owned();
    let mut files = Vec::new();
    if path.is_dir() {
        let mut stack = vec![path.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).expect("case dir") {
                let p = e.expect("entry").path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                let rel = p.strip_prefix(path).expect("inside").to_string_lossy().into_owned();
                files.push((rel, std::fs::read_to_string(&p).expect("read")));
            }
        }
        files.sort();
    } else {
        files.push(("main.wrela".into(), std::fs::read_to_string(path).expect("read")));
    }
    let main = &files.iter().find(|f| f.0 == "main.wrela").expect("a main.wrela").1;
    let (accepts, rejects) = (parse_header(main, "accepts"), parse_header(main, "rejects"));
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
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("conformance").join(&case.name);
    let _ = std::fs::remove_dir_all(&dir);
    for (file, text) in &case.files {
        let p = dir.join(file);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("dir");
        std::fs::write(p, text).expect("write");
    }
    let built = wrela_driver::build(&dir);
    let mut out: BTreeMap<(String, usize), Vec<String>> = BTreeMap::new();
    for d in built.diagnostics.iter().filter(|d| d.is_error()) {
        let span = d.primary.span;
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
    let dir = root().join("compiler/tests/conformance");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("conformance dir")
        .map(|e| e.expect("entry").path())
        .collect();
    paths.sort();
    let known: BTreeSet<&str> = RULES.iter().map(|r| r.0).collect();
    let mut accepted: BTreeSet<String> = BTreeSet::new();
    let mut rejected: BTreeSet<String> = BTreeSet::new();
    let mut failures = Vec::new();
    let mut count = 0;
    for path in paths {
        if !(path.is_dir() || path.extension().is_some_and(|e| e == "wrela")) {
            continue;
        }
        count += 1;
        let case = load_case(&path);
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
    println!("{count} conformance cases cover {} rules", RULES.len());
    assert!(failures.is_empty(), "{} cases failed:\n{}", failures.len(), failures.join("\n"));
    assert!(missing.is_empty(), "rules without cases:\n{}", missing.join("\n"));
}
