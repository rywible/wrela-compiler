//! AC3: the conformance suite (compiler/tests/conformance). Every rule of docs/language.md,
//! the memory model (§6) included, has at least one program that's accepted and one that's
//! rejected with the rule's diagnostic code, and the table of rules is the spec's.
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
use wrela_tests::{cases, files_under, header, par_map, repo_root};

/// The rules, by section of docs/language.md, which names each one where it's written
/// (`the_rule_table_is_the_specs`): the driver keeps the table, which `wrela primer` reads too.
use wrela_driver::primer::RULES;

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
    // An accepting case goes all the way: with a `frame` added if it has none (at the end, so
    // no line moves), and every function lowered and emitted, called or not.
    let accepting = case.rejects.is_empty();
    for (file, text) in &case.files {
        let p = dir.join(file);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("dir");
        let text =
            if accepting && file == "main.wrela" { crate::with_frame(text) } else { text.clone() };
        std::fs::write(p, text).expect("write");
    }
    let built =
        if accepting { wrela_driver::build_every_fn(&dir) } else { wrela_driver::build(&dir) };
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
        // By its path in the package (`shapes/blob.wrela`), as the markers are; std's files by
        // their names (`<std::field>`).
        out.entry((file.name.clone(), line)).or_default().push(d.code.as_str().to_string());
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
    // The cases compiled at once; then their rules gathered, in order.
    let checked = par_map(&paths, |path| {
        let case = load_case(path);
        let got = errors(&case);
        (case, got)
    });
    let mut failures = Vec::new();
    for (case, got) in checked {
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

/// The rule table is language.md's, in both directions: every rule the spec names (as
/// `` `area.rule` ``, with one of the table's areas) has a row here, and every row is named in the
/// spec, where the rule is written.
#[test]
fn the_rule_table_is_the_specs() {
    let spec = std::fs::read_to_string(repo_root().join("docs/language.md")).expect("language.md");
    let areas: BTreeSet<&str> = RULES.iter().filter_map(|(id, _)| id.split('.').next()).collect();
    let mut named = BTreeSet::new();
    for piece in spec.split('`').skip(1).step_by(2) {
        if let Some((area, rule)) = piece.split_once('.')
            && areas.contains(area)
            && !rule.is_empty()
            && rule.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
        {
            named.insert(piece.to_string());
        }
    }
    let table: BTreeSet<String> = RULES.iter().map(|(id, _)| id.to_string()).collect();
    let unwritten: Vec<&String> = table.difference(&named).collect();
    let untabled: Vec<&String> = named.difference(&table).collect();
    assert!(unwritten.is_empty(), "rules language.md doesn't name: {unwritten:?}");
    assert!(untabled.is_empty(), "rules language.md names that the table lacks: {untabled:?}");
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
