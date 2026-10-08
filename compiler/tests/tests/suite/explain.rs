//! AC13: `wrela explain <code>` covers every code. Each explanation's wrong program gives its
//! code, and its fixed program compiles with no diagnostic at all. A code no program
//! can show says why (`// ---- untested:`); the list of those is pinned here.

use crate::scratch;
use wrela_diag::codes;

/// The codes whose explanations have no program, and why that's right.
const UNTESTED: &[&str] = &[
    // A symbolic link to a directory: a link can't be written as a file's text.
    "E0208", // A bug in the compiler: no correct program should show it.
    "I0001",
];

/// The codes only `wrela test` reports: their programs are run as tests, not only checked.
const FROM_TESTS: &[&str] = &["E0706"];

/// The codes only a shipped build reports: their programs are built, not only checked.
const FROM_BUILDS: &[&str] = &["E0707"];

/// A program's diagnostics: `wrela check`'s, or for a code only tests report, `wrela test`'s
/// with each failed test's.
fn diagnose(
    code: &str,
    dir: &std::path::Path,
) -> (wrela_diag::SourceMap, Vec<wrela_diag::Diagnostic>) {
    if FROM_TESTS.contains(&code) {
        let out = wrela_driver::test(dir, None);
        let mut all = out.diagnostics;
        all.extend(out.results.into_iter().filter_map(|r| r.failure));
        (out.sources, all)
    } else if FROM_BUILDS.contains(&code) {
        let out = wrela_driver::build(dir);
        (out.sources, out.diagnostics)
    } else {
        let out = wrela_driver::check(dir);
        (out.sources, out.diagnostics)
    }
}

#[test]
fn every_code_is_explained() {
    let all = wrela_driver::explain::all();
    let mut missing = Vec::new();
    for c in codes::ALL {
        if !all.iter().any(|e| e.code == c.as_str()) {
            missing.push(c.as_str());
        }
    }
    assert!(missing.is_empty(), "codes with no explanation: {missing:?}");
    for e in &all {
        assert!(codes::Code::lookup(e.code).is_some(), "{}: not a code", e.code);
        assert!(!e.meaning.is_empty(), "{}: no meaning", e.code);
        let untested = UNTESTED.contains(&e.code);
        assert_eq!(e.untested.is_some(), untested, "{}: untested is pinned here", e.code);
        if !untested {
            assert!(!e.wrong.is_empty() && !e.fixed.is_empty(), "{}: no programs", e.code);
        }
    }
}

#[test]
fn explanations_programs_do_what_they_say() {
    let all: Vec<_> =
        wrela_driver::explain::all().into_iter().filter(|e| e.untested.is_none()).collect();
    let failures = std::sync::Mutex::new(Vec::new());
    wrela_tests::par_each(
        &all,
        || (),
        |(), e| {
            let write = |side: &str, files: &[(String, String)]| {
                let dir = scratch(&format!("explain/{}-{side}", e.code));
                // An empty file isn't written: W0003's program has no `main.wrela`.
                for (path, text) in files.iter().filter(|(_, t)| !t.is_empty()) {
                    let p = dir.join(path);
                    std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
                    std::fs::write(p, text).expect("write");
                }
                dir
            };
            let wrong = diagnose(e.code, &write("wrong", &e.wrong));
            let found: Vec<&str> = wrong.1.iter().map(|d| d.code.as_str()).collect();
            let mut problems = Vec::new();
            if !found.contains(&e.code) {
                problems.push(format!(
                    "the wrong program gives {found:?}, not {}:\n{}",
                    e.code,
                    wrela_diag::render::render_all(&wrong.0, &wrong.1)
                ));
            }
            let mut fixed = diagnose(e.code, &write("fixed", &e.fixed));
            // No diagnostic at all: a warning in a fixed program would teach the wrong thing.
            // But W0008: an explanation's program shows a function, which nothing need call
            // (W0008's own program shows one that something does).
            if e.code != "W0008" {
                fixed.1.retain(|d| d.code != wrela_diag::codes::W0008);
            }
            if !fixed.1.is_empty() {
                problems.push(format!(
                    "the fixed program doesn't compile:\n{}",
                    wrela_diag::render::render_all(&fixed.0, &fixed.1)
                ));
            }
            if !problems.is_empty() {
                failures.lock().expect("lock").push(format!("{}: {}", e.code, problems.join("\n")));
            }
        },
    );
    let failures = failures.into_inner().expect("lock");
    assert!(
        failures.is_empty(),
        "{} explanations fail:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
