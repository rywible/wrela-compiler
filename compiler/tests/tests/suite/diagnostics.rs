//! AC6: the diagnostics suite (compiler/tests/diagnostics). Each case is one common mistake,
//! from language.md's rejection examples (§6.16) and the conformance suite's rejecting cases:
//!
//! ```text
//! // mistake: moving a value out of a named place without `take`
//! // expect: E0501
//! // fix: yes
//! ```
//!
//! The compiler must report exactly the expected codes, and its JSON output (versioned,
//! wrela_diag::json) must equal the checked-in golden `<case>.json`: the primary span, the
//! message, the labels, notes, help and fixes are all pinned there, and were reviewed when
//! blessed. A case marked `fix: yes` must get a suggested fix, and applying the fixes must
//! leave a program that compiles. `WRELA_BLESS=1` rewrites the goldens.

use crate::package;
use std::path::Path;
use wrela_tests::{apply_fixes, cases, header, repo_root};

struct Outcome {
    codes: Vec<String>,
    json: String,
    text: String,
    fixed: Option<String>,
}

/// Builds `text` as a one-file package; returns its errors, JSON, rendering, and the text with
/// every first fix applied.
fn compile(name: &str, text: &str) -> Outcome {
    let built = wrela_driver::build(&package(&format!("diagnostics/{name}"), text));
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
        let text = std::fs::read_to_string(path).expect("read");
        let first = |key| header(&text, key).next();
        let mistake = first("mistake").unwrap_or_else(|| panic!("{name}: no `mistake:`"));
        let expect: Vec<String> = first("expect")
            .unwrap_or_else(|| panic!("{name}: no `expect:`"))
            .split_whitespace()
            .map(String::from)
            .collect();
        let wants_fix = first("fix") == Some("yes");
        let out = compile(&name, &text);
        let mut problems = Vec::new();
        if out.codes != expect {
            problems.push(format!("codes {:?}, expected {expect:?}", out.codes));
        }
        let golden = path.with_extension("json");
        if bless {
            std::fs::write(&golden, &out.json).expect("bless");
        } else {
            match std::fs::read_to_string(&golden) {
                Ok(g) if g == out.json => {}
                Ok(_) => problems.push("JSON differs from the golden".into()),
                Err(_) => problems.push("no golden; bless with WRELA_BLESS=1".into()),
            }
        }
        if wants_fix {
            with_fixes += 1;
            match &out.fixed {
                None => problems.push("no fix suggested".into()),
                Some(fixed) => {
                    let again = compile(&format!("{name}-fixed"), fixed);
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

/// The goldens can only be what the compiler prints: they're its JSON, and parse as such.
#[test]
fn goldens_are_versioned_json() {
    let dir: &Path = &repo_root().join("compiler/tests/diagnostics");
    for e in std::fs::read_dir(dir).expect("dir") {
        let p = e.expect("entry").path();
        if p.extension().is_some_and(|x| x == "json") {
            let text = std::fs::read_to_string(&p).expect("read");
            // The top level's keys, two spaces in: `"version"` is one of them.
            assert!(
                text.lines()
                    .any(|l| l == format!("  \"version\": {}", wrela_diag::json::JSON_VERSION)),
                "{}: not version {} JSON",
                p.display(),
                wrela_diag::json::JSON_VERSION
            );
        }
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
