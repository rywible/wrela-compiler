//! Warnings: W0001 (a local bound and never used) and W0002 (an arm that can't match). The
//! conformance suite checks errors only.

use crate::package;

fn warnings(name: &str, text: &str) -> Vec<(String, usize)> {
    let out = wrela_driver::check(&package(&format!("warnings/{name}"), text));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    out.diagnostics
        .iter()
        .filter(|d| !d.is_error())
        .map(|d| {
            let span = d.span().expect("warnings have spans");
            let f = out.sources.file(span.file);
            (d.code.to_string(), f.line_index(span.start) + 1)
        })
        .collect()
}

#[test]
fn unused_locals_are_reported_once_each() {
    let src = "fn f(unused_param: i32) -> i32 {
    let a = 1
    let b = 2
    var c = 0
    c = 5
    let _quiet = 3
    let (d, e) = (1, 2)
    for k in 0..3 {
    }
    for _ in 0..3 {
    }
    let g = 4
    let h = |x: i32| x + g
    b + e + h(1)
}";
    let w = warnings("unused", src);
    let lines: Vec<usize> = w.iter().filter(|(c, _)| c == "W0001").map(|(_, l)| *l).collect();
    // `a`, `c` (assigned, never read), `d`, `k`.
    assert_eq!(lines, [2, 4, 7, 8], "{w:?}");
}

#[test]
fn unreachable_arms_are_reported() {
    let src = "fn f(x: u32) -> u32 {
    match x {
        _ => 1,
        3 => 2,
    }
}";
    let w = warnings("unreachable", src);
    assert_eq!(w, [("W0002".to_string(), 4)]);
}

#[test]
fn a_pattern_with_an_error_hides_no_arm() {
    // The wrong-enum pattern is an error (E0320); the arms after it aren't unreachable.
    let src = "enum K {
    A,
    B(i32),
}

fn f(k: K) -> i32 {
    match k {
        Option::Some(x) => x,
        K::A => 1,
        K::B(x) => x,
    }
}";
    let out = wrela_driver::check(&package("warnings/wrong_pattern", src));
    let codes: Vec<String> = out.diagnostics.iter().map(|d| d.code.to_string()).collect();
    assert_eq!(codes, ["E0320"], "{:?}", out.diagnostics);
}
