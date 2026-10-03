//! Every diagnostic code the compiler declares is one some test expects it to report: a
//! conformance case's `//~ CODE`, a curated mistake's `// expect: CODE` (and its golden
//! `"code": "CODE"`), or a Rust test that compares against the code (`"CODE"` as a whole string
//! literal, or `codes::CODE`). A code named only in passing (a comment, a message) doesn't
//! count. A code no test expects is either dead (retire it: `wrela_diag::codes::RETIRED`) or
//! untested.

use std::collections::BTreeSet;
use std::path::Path;
use wrela_diag::codes;
use wrela_tests::{files_under, repo_root};

/// Codes no test can make the compiler report, and why.
const UNREACHABLE: &[(&str, &str)] =
    &[("I0001", "a bug in the compiler: a test that reaches it is a bug report")];

/// The files under `dir` with one of the extensions `exts` that `keep` accepts, with their text.
fn read_where(dir: &Path, exts: &[&str], keep: impl Fn(&Path) -> bool) -> Vec<(String, String)> {
    files_under(dir, exts)
        .into_iter()
        .filter(|p| keep(p) && !p.ends_with("tests/tests/suite/codes.rs"))
        .map(|p| {
            let ext = p.extension().map_or(String::new(), |e| e.to_string_lossy().into_owned());
            (ext, std::fs::read_to_string(&p).unwrap_or_default())
        })
        .collect()
}

/// A unit-test module in its own file: `src/lexer/tests.rs`, or anything under `src/**/tests/`.
fn is_test_module(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n == "tests.rs")
        || p.components().any(|c| c.as_os_str() == "tests")
}

/// The words of `s` that have a code's shape (`E0208`), with what precedes each.
fn code_words(s: &str) -> impl Iterator<Item = (&str, &str)> {
    s.match_indices(['E', 'W', 'I']).filter_map(|(i, _)| {
        let word = s.get(i..i + 5)?;
        let shaped = word[1..].bytes().all(|b| b.is_ascii_digit());
        let after = s[i + 5..].chars().next();
        let bounded = !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
            && !s[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_');
        (shaped && bounded).then(|| (&s[..i], word))
    })
}

/// The codes a test file expects. In WRELA files: those after `//~` and `// expect:`. In JSON
/// goldens: `"code": "CODE"`. In Rust: `"CODE"` as a whole string literal, or `codes::CODE`,
/// outside `//` comments.
fn expected(ext: &str, text: &str, out: &mut BTreeSet<String>) {
    for line in text.lines() {
        match ext {
            "wrela" => {
                let tail = line.split_once("//~").or_else(|| line.split_once("// expect:"));
                if let Some((_, tail)) = tail {
                    out.extend(code_words(tail).map(|(_, c)| c.to_string()));
                }
            }
            "json" => out.extend(
                code_words(line)
                    .filter(|(before, _)| before.trim_end().ends_with("\"code\": \""))
                    .map(|(_, c)| c.to_string()),
            ),
            _ => {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for (before, code) in code_words(line) {
                    let rest = &line[before.len() + 5..];
                    if (before.ends_with('"') && rest.starts_with('"'))
                        || before.ends_with("codes::")
                    {
                        out.insert(code.to_string());
                    }
                }
            }
        }
    }
}

#[test]
fn every_code_is_tested() {
    let mut files =
        read_where(&repo_root().join("compiler/tests"), &["wrela", "rs", "json"], |_| true);
    for krate in
        ["diag", "syntax", "sema", "lower", "ir", "wasm", "wgsl", "driver", "cli", "grammar"]
    {
        let dir = repo_root().join("compiler").join(krate);
        files.extend(read_where(&dir.join("tests"), &["rs", "wrela"], |_| true));
        // Unit tests: modules in their own files, and `#[cfg(test)] mod` blocks in the crate's
        // other sources.
        let src_dir = dir.join("src");
        let in_tests = |p: &Path| is_test_module(p.strip_prefix(&src_dir).unwrap_or(p));
        files.extend(read_where(&src_dir, &["rs"], in_tests));
        for (ext, src) in read_where(&src_dir, &["rs"], |p| !in_tests(p)) {
            for part in src.split("#[cfg(test)]").skip(1) {
                if part.trim_start().starts_with("mod ") {
                    files.push((ext.clone(), part.split("\n}\n").next().unwrap_or(part).into()));
                }
            }
        }
    }
    let mut tested = BTreeSet::new();
    for (ext, text) in &files {
        expected(ext, text, &mut tested);
    }
    let untested: Vec<String> = codes::ALL
        .iter()
        .map(|c| c.as_str())
        .filter(|c| !UNREACHABLE.iter().any(|(u, _)| u == c))
        .filter(|c| !tested.contains(*c))
        .map(|c| format!("{c} ({})", codes::Code::lookup(c).map_or("", |x| x.title())))
        .collect();
    assert!(untested.is_empty(), "no test expects:\n  {}", untested.join("\n  "));
}

/// Mentions that aren't expectations don't count.
#[test]
fn only_expectations_count() {
    let found = |ext: &str, text: &str| {
        let mut out = BTreeSet::new();
        expected(ext, text, &mut out);
        out.into_iter().collect::<Vec<_>>()
    };
    assert_eq!(found("wrela", "let x = 1 //~ E0001 W0002\n// see E0003"), ["E0001", "W0002"]);
    assert_eq!(found("wrela", "// expect: E0004\nfn f() {}"), ["E0004"]);
    assert_eq!(found("json", "\"code\": \"E0005\",\n\"message\": \"not E0006\""), ["E0005"]);
    let rust = r#"
        assert_eq!(codes_of("x"), ["E0007"]);
        assert_eq!(d.code, codes::E0008);
        assert!(errs[0].symlink, "reported as E0009");
        // E0010 "E0011"
        let e = "E00123";
    "#;
    assert_eq!(found("rs", rust), ["E0007", "E0008"]);
}
