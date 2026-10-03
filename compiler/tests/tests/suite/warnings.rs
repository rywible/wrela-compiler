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
    // Literals compare by value: `-0.0` is `0.0`.
    let src = "fn g(x: f32) -> u32 {
    match x {
        0.0 => 1,
        -0.0 => 2,
        _ => 3,
    }
}";
    let w = warnings("unreachable_zero", src);
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

#[test]
fn unused_local_fixes_compile() {
    // The rename keeps a shorthand field's name; for a local assigned later, only its binding
    // is known, so there's help but no fix.
    let src = "struct Pair: Copy + Clone {
    count: u32,
    other: u32,
}

fn second(p: Pair) -> u32 {
    match p {
        Pair { count, other } => other,
    }
}

fn pick(c: bool) -> f32 {
    var x = 1.0
    if c {
        x = 2.0
    }
    3.0
}

fn plain() -> u32 {
    let y = 4
    5
}";
    let out = wrela_driver::check(&package("warnings/fixes", src));
    let fixes: Vec<(String, Vec<String>)> = out
        .diagnostics
        .iter()
        .filter(|d| d.code.as_str() == "W0001")
        .map(|d| {
            let edits = d.fixes.iter().flat_map(|f| &f.edits).map(|e| e.replacement.clone());
            (d.message.clone(), edits.collect())
        })
        .collect();
    assert_eq!(
        fixes,
        [
            ("`count` is never used".to_string(), vec!["count: _count".to_string()]),
            ("`x` is never used".to_string(), vec![]),
            ("`y` is never used".to_string(), vec!["_".to_string()]),
        ]
    );
    let main = out.sources.files().find(|(_, f)| f.name.ends_with("main.wrela")).map(|(id, _)| id);
    let fixed = wrela_tests::apply_fixes(src, main.expect("main.wrela"), &out.diagnostics);
    let again = wrela_driver::check(&package("warnings/fixed", &fixed));
    assert!(!again.has_errors(), "{fixed}\n{:?}", again.diagnostics);
    assert!(again.diagnostics.iter().filter(|d| d.code.as_str() == "W0001").count() == 1);
}
