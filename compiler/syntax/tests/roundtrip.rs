//! Parse → format → parse gives the same AST, and formatting is idempotent, on hand-written
//! samples. (The grammar-generated round trips live in wrela-grammar's tests.)

use wrela_diag::FileId;
use wrela_syntax::{fmt, parse};

/// The AST's Debug output without spans and node ids.
fn shape(src: &str) -> String {
    let p = parse(FileId(0), src);
    assert!(!p.has_errors(), "parse errors in:\n{src}\n{:#?}", p.diagnostics);
    strip(&format!("{:#?}", p.file))
}

fn strip(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("Span {") {
        out.push_str(&rest[..i]);
        let j = rest[i..].find('}').map_or(rest.len(), |j| i + j + 1);
        out.push('S');
        rest = &rest[j..];
    }
    out.push_str(rest);
    out
}

fn round_trip(src: &str) -> String {
    let p = parse(FileId(0), src);
    assert!(!p.has_errors(), "parse errors:\n{:#?}", p.diagnostics);
    let once = fmt::format(&p, src);
    assert_eq!(shape(src), shape(&once), "format changed the AST:\n{once}");
    let p2 = parse(FileId(0), &once);
    let twice = fmt::format(&p2, &once);
    assert_eq!(once, twice, "formatting isn't idempotent");
    once
}

/// A file using most of the syntax (compiler/grammar's tests read it too).
const SAMPLE: &str = include_str!("sample.wrela");

#[test]
fn sample_round_trips() {
    round_trip(SAMPLE);
}

#[test]
fn formatting_normalizes() {
    let out = round_trip("fn f(){let a=1;let b=a+2\n\n\n  let c = b}\n");
    assert_eq!(out, "fn f() {\n    let a = 1\n    let b = a + 2\n\n    let c = b\n}\n");
}

#[test]
fn comments_survive() {
    let src = "// top\n\n/// doc\nfn f() { // after brace\n    let a = 1 // trailing\n    // own line\n    let b = 2\n    // before close\n}\n// end\n";
    let out = round_trip(src);
    for c in [
        "// top",
        "/// doc",
        "// after brace",
        "// trailing",
        "// own line",
        "// before close",
        "// end",
    ] {
        assert!(out.contains(c), "lost {c}:\n{out}");
    }
}

#[test]
fn leading_dot_chains_are_kept() {
    let src = "fn f() {\n    let r = a\n        .b(1)\n        .c(2)\n}\n";
    assert_eq!(round_trip(src), src);
}

#[test]
fn multiline_arguments_are_kept() {
    let src = "fn f() {\n    g(\n        1,\n        2,\n    )\n}\n";
    assert_eq!(round_trip(src), src);
}

#[test]
#[ignore]
fn show_sample() {
    println!("{}", round_trip(SAMPLE));
}
