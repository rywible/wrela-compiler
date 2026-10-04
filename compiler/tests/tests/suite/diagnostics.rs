//! AC6: the diagnostics suite (compiler/tests/diagnostics). Each case is one common mistake,
//! from language.md's rejection examples (§6.16) and the conformance suite's rejecting cases:
//!
//! ```text
//! // mistake: moving a value out of a named place without `take`
//! // expect: E0501
//! // fix: yes
//! ```
//!
//! A case is a file, or a directory that's a package (with dependencies, for a library's
//! diagnostic) whose `main.wrela` has the headers and whose golden is `expected.json`.
//!
//! The compiler must report exactly the expected codes, and its JSON output (versioned,
//! wrela_diag::json) must equal the checked-in golden `<case>.json`: the primary span, the
//! message, the labels, notes, help and fixes are all pinned there, and were reviewed when
//! blessed. A case marked `fix: yes` must get a suggested fix, and applying the fixes must
//! leave a program that compiles. `WRELA_BLESS=1` rewrites the goldens. Where in std a label
//! or note points (its line, column and offsets) isn't compared: std's lines move whenever std
//! is edited, and which std file it names still is.

use crate::package;
use wrela_tests::{apply_fixes, cases, files_under, header, repo_root};

struct Outcome {
    codes: Vec<String>,
    json: String,
    text: String,
    fixed: Option<String>,
}

/// Builds `text` as a one-file package, or as the `main.wrela` of a copy of the package in
/// `dir`; returns its errors, JSON, rendering, and the text with every first fix applied.
fn compile(name: &str, text: &str, dir: Option<&std::path::Path>) -> Outcome {
    let pkg = package(&format!("diagnostics/{name}"), text);
    if let Some(dir) = dir {
        wrela_tests::copy_dir(dir, &pkg);
        std::fs::write(pkg.join("main.wrela"), text).expect("write main.wrela");
    }
    let built = wrela_driver::build(&pkg);
    let errors: Vec<_> = built.diagnostics.iter().filter(|d| d.is_error()).cloned().collect();
    let main =
        built.sources.files().find(|(_, f)| f.name.ends_with("main.wrela")).map(|(id, _)| id);
    let fixed = main.map(|id| apply_fixes(text, id, &errors));
    Outcome {
        codes: errors.iter().map(|d| d.code.as_str().to_string()).collect(),
        json: wrela_diag::json::to_json_string(&built.sources, &errors),
        text: wrela_diag::render::render_all(&built.sources, &errors),
        fixed: fixed.filter(|f| f != text),
    }
}

#[test]
fn diagnostics_meet_the_bar() {
    let bless = std::env::var_os("WRELA_BLESS").is_some();
    let cases = cases(&repo_root().join("compiler/tests/diagnostics"));
    let mut failures = Vec::new();
    let mut with_fixes = 0;
    for path in &cases {
        let name = path.file_stem().expect("name").to_string_lossy().into_owned();
        // A directory is a package (with its dependencies): its `main.wrela` has the headers.
        let dir = path.is_dir().then_some(path.as_path());
        let main = if path.is_dir() { path.join("main.wrela") } else { path.clone() };
        let text = std::fs::read_to_string(&main).expect("read");
        let first = |key| header(&text, key).next();
        let mistake = first("mistake").unwrap_or_else(|| panic!("{name}: no `mistake:`"));
        let expect: Vec<String> = first("expect")
            .unwrap_or_else(|| panic!("{name}: no `expect:`"))
            .split_whitespace()
            .map(String::from)
            .collect();
        let wants_fix = match first("fix") {
            Some("yes") => true,
            Some("no") => false,
            other => panic!("{name}: `fix:` is `yes` or `no`, not {other:?}"),
        };
        let out = compile(&name, &text, dir);
        let mut problems = Vec::new();
        if out.codes != expect {
            problems.push(format!("codes {:?}, expected {expect:?}", out.codes));
        }
        let golden =
            if path.is_dir() { path.join("expected.json") } else { path.with_extension("json") };
        if bless {
            std::fs::write(&golden, &out.json).expect("bless");
        } else {
            match std::fs::read_to_string(&golden) {
                Ok(g) if g == out.json || std_lines_aside(&g) == std_lines_aside(&out.json) => {}
                Ok(_) => problems.push("JSON differs from the golden".into()),
                Err(_) => problems.push("no golden; bless with WRELA_BLESS=1".into()),
            }
        }
        if !wants_fix && out.fixed.is_some() {
            problems.push("a fix is suggested, but the header says `fix: no`".into());
        }
        if wants_fix {
            with_fixes += 1;
            match &out.fixed {
                None => problems.push("no fix suggested".into()),
                Some(fixed) => {
                    let again = compile(&format!("{name}-fixed"), fixed, dir);
                    if !again.codes.is_empty() {
                        problems.push(format!(
                            "after the fix, still {:?}:\n{fixed}\n{}",
                            again.codes, again.text
                        ));
                    }
                }
            }
        }
        if !problems.is_empty() {
            failures.push(format!(
                "{name} ({mistake}):\n  {}\n{}",
                problems.join("\n  "),
                out.text
            ));
        }
    }
    println!("{} curated mistakes, {with_fixes} with fixes that compile", cases.len());
    assert!(cases.len() >= 50, "only {} cases", cases.len());
    assert!(failures.is_empty(), "{} cases failed:\n{}", failures.len(), failures.join("\n"));
}

/// A golden's JSON with where it points in std left out: each span of a label in a `<std::…>`
/// file, and the `:line:column` after `<std::…>` in its text.
fn std_lines_aside(json: &str) -> serde_json::Value {
    fn walk(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(o) => {
                let in_std =
                    o.get("file").and_then(|f| f.as_str()).is_some_and(|f| f.starts_with("<std::"));
                if in_std {
                    o.remove("span");
                }
                o.values_mut().for_each(walk);
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(walk),
            serde_json::Value::String(s) => *s = strip_std_lines(s),
            _ => {}
        }
    }
    let mut v: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    walk(&mut v);
    v
}

/// `<std::field>:28:5: declared here` as `<std::field>: declared here`.
fn strip_std_lines(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find("<std::") {
        let Some(close) = rest[at..].find('>') else { break };
        let end = at + close + 1;
        out.push_str(&rest[..end]);
        rest = &rest[end..];
        let skip =
            rest.len() - rest.trim_start_matches(|c: char| c == ':' || c.is_ascii_digit()).len();
        // Keep the `:` that ends a location before its text.
        if skip > 0 {
            rest = &rest[skip..];
            out.push(':');
        }
    }
    out.push_str(rest);
    out
}

#[test]
fn std_locations_in_goldens_dont_count() {
    assert_eq!(strip_std_lines("<std::field>:28:5: declared here"), "<std::field>: declared here");
    assert_eq!(strip_std_lines("no std here: 3:4"), "no std here: 3:4");
    let a = r#"{"labels":[{"file":"<std::math>","span":{"line":360}},{"file":"main.wrela","span":{"line":4}}]}"#;
    let b = r#"{"labels":[{"file":"<std::math>","span":{"line":362}},{"file":"main.wrela","span":{"line":4}}]}"#;
    let c = r#"{"labels":[{"file":"<std::math>","span":{"line":360}},{"file":"main.wrela","span":{"line":5}}]}"#;
    assert_eq!(std_lines_aside(a), std_lines_aside(b));
    assert_ne!(std_lines_aside(a), std_lines_aside(c));
}

/// The goldens can only be what the compiler prints: they're its JSON, and parse as such.
#[test]
fn goldens_are_versioned_json() {
    for p in files_under(&repo_root().join("compiler/tests/diagnostics"), &["json"]) {
        let text = std::fs::read_to_string(&p).expect("read");
        // The top level's keys, two spaces in: `"version"` is one of them.
        assert!(
            text.lines().any(|l| l == format!("  \"version\": {}", wrela_diag::json::JSON_VERSION)),
            "{}: not version {} JSON",
            p.display(),
            wrela_diag::json::JSON_VERSION
        );
    }
}

/// A symbolic link to a directory isn't followed (E0208). No curated case can hold one: the
/// package is made here.
#[cfg(unix)]
#[test]
fn a_symlinked_directory_is_e0208() {
    let dir = package("diagnostics-symlink/game", "pub fn f() -> i32 {\n    1\n}\n");
    let target = dir.with_file_name("shapes");
    std::fs::create_dir_all(&target).expect("make the target");
    std::fs::write(target.join("blob.wrela"), "pub fn g() -> i32 {\n    2\n}\n").expect("write");
    std::os::unix::fs::symlink(&target, dir.join("shapes")).expect("make the link");
    let built = wrela_driver::build(&dir);
    let codes: Vec<&str> = built.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes, ["E0208"]);
}
