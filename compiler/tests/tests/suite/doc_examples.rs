//! The crates with `doctest = false` have no examples in their docs for rustdoc to run: a code
//! block in a doc comment there is `text` (or another language). A Rust example would go
//! unchecked, so this fails if one appears: turn the crate's doctests back on.

use std::path::Path;
use wrela_tests::root;

fn rust_examples(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_examples(&p, out);
            continue;
        }
        if p.extension().is_none_or(|x| x != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        let mut open = false;
        for (i, line) in text.lines().enumerate() {
            let t = line.trim_start();
            let Some(doc) = t.strip_prefix("///").or_else(|| t.strip_prefix("//!")) else {
                continue;
            };
            let Some(info) = doc.trim().strip_prefix("```") else { continue };
            if open {
                open = false;
                continue;
            }
            open = true;
            // rustdoc runs a block with no language, `rust`, or only rustdoc's attributes.
            let rust = info.split(',').map(str::trim).all(|a| {
                matches!(a, "" | "rust" | "no_run" | "should_panic" | "compile_fail")
                    || a.starts_with("edition")
            });
            if rust {
                out.push(format!("{}:{}", p.display(), i + 1));
            }
        }
    }
}

#[test]
fn doc_examples_are_tested() {
    let mut found = Vec::new();
    for group in ["compiler", "runtime"] {
        for e in std::fs::read_dir(root().join(group)).expect("read").flatten() {
            let manifest = std::fs::read_to_string(e.path().join("Cargo.toml")).unwrap_or_default();
            if manifest.contains("doctest = false") {
                rust_examples(&e.path().join("src"), &mut found);
            }
        }
    }
    assert!(
        found.is_empty(),
        "Rust examples in crates whose doctests are off (turn them on, or mark the block `text`):\n  {}",
        found.join("\n  ")
    );
}
