//! Every diagnostic code the compiler declares is one some test makes it report: a conformance
//! case's `//~ CODE`, a curated mistake's `// expect:`, or an assertion in a Rust test. A code no
//! test reaches is either dead (retire it: `wrela_diag::codes::RETIRED`) or untested.

use std::path::Path;
use wrela_diag::codes;
use wrela_tests::root;

/// Codes no test can make the compiler report, and why.
const UNREACHABLE: &[(&str, &str)] =
    &[("I0001", "a bug in the compiler: a test that reaches it is a bug report")];

fn read_all(dir: &Path, exts: &[&str], out: &mut String) {
    read_where(dir, exts, &|_| true, out);
}

fn read_where(dir: &Path, exts: &[&str], keep: &dyn Fn(&Path) -> bool, out: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            read_where(&p, exts, keep, out);
        } else if p.extension().is_some_and(|x| exts.iter().any(|e| x == *e)) && keep(&p) {
            out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
            out.push('\n');
        }
    }
}

/// A unit-test module in its own file: `src/lexer/tests.rs`, or anything under `src/**/tests/`.
fn is_test_module(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n == "tests.rs")
        || p.components().any(|c| c.as_os_str() == "tests")
}

#[test]
fn every_code_is_tested() {
    let mut tests = String::new();
    read_all(&root().join("compiler/tests"), &["wrela", "rs", "json"], &mut tests);
    for krate in
        ["diag", "syntax", "sema", "lower", "ir", "wasm", "wgsl", "driver", "cli", "grammar"]
    {
        let dir = root().join("compiler").join(krate);
        read_all(&dir.join("tests"), &["rs", "wrela"], &mut tests);
        // Unit tests: modules in their own files, and what follows `#[cfg(test)]` in the
        // crate's other sources.
        let src_dir = dir.join("src");
        read_where(
            &src_dir,
            &["rs"],
            &|p| is_test_module(p.strip_prefix(&src_dir).unwrap_or(p)),
            &mut tests,
        );
        let mut src = String::new();
        read_where(
            &src_dir,
            &["rs"],
            &|p| !is_test_module(p.strip_prefix(&src_dir).unwrap_or(p)),
            &mut src,
        );
        for part in src.split("#[cfg(test)]").skip(1) {
            tests.push_str(part.split("\n}\n").next().unwrap_or(part));
        }
    }
    let untested: Vec<String> = codes::ALL
        .iter()
        .map(|c| c.as_str())
        .filter(|c| !UNREACHABLE.iter().any(|(u, _)| u == c))
        .filter(|c| !tests.contains(c))
        .map(|c| format!("{c} ({})", codes::Code::lookup(c).map_or("", |x| x.title())))
        .collect();
    assert!(untested.is_empty(), "no test reports:\n  {}", untested.join("\n  "));
}
