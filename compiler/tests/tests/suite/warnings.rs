//! Warnings: W0001 (a local bound and never used), W0002 (an arm that can't match), W0004
//! (`Clone` declared beside `Copy`) and W0005 (a bare literal by position where a swap would
//! compile, or a `select` by position). The conformance suite checks errors only.

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
    let src = "struct Pair: Copy {
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

/// `Clone` beside `Copy`, in a type's traits or a trait set's, before or after it: W0004, whose
/// fix removes it; a set that brings `Copy` counts too.
#[test]
fn clone_beside_copy_is_reported_and_fixed() {
    let src = "trait Pod = Copy + Eq

trait Plainly = Clone + Copy

struct A: Copy + Clone {
    x: f32,
}

struct B: Clone + Copy + Eq {
    x: f32,
}

struct C: Pod + Clone {
    x: f32,
}

struct D: Copy {
    x: f32,
}
";
    let w = warnings("clone-copy", src);
    let lines: Vec<usize> = w.iter().filter(|(c, _)| c == "W0004").map(|(_, l)| *l).collect();
    assert_eq!(lines, [3, 5, 9, 13]);
    let out = wrela_driver::check(&package("warnings/clone-copy-fix", src));
    let main = out.sources.files().find(|(_, f)| f.name.ends_with("main.wrela")).map(|(id, _)| id);
    let fixed = wrela_tests::apply_fixes(src, main.expect("main.wrela"), &out.diagnostics);
    assert!(fixed.contains("trait Plainly = Copy\n"), "{fixed}");
    assert!(fixed.contains("struct A: Copy {"), "{fixed}");
    assert!(fixed.contains("struct B: Copy + Eq {"), "{fixed}");
    assert!(fixed.contains("struct C: Pod {"), "{fixed}");
    let again = warnings("clone-copy-fixed", &fixed);
    assert!(again.iter().all(|(c, _)| c != "W0004"), "{again:?}");
}

/// W0005: two neighbouring parameters of one type both given bare numbers, or a bare `bool` to
/// a function of several arguments; one per call, and its fix names every positional argument
/// from that one on. One literal beside a variable, a named argument, a single parameter or
/// too many arguments isn't reported.
#[test]
fn bare_literals_that_could_swap_are_reported_and_named() {
    let src = "fn area(width: f32, height: f32) -> f32 {
    width * height
}

fn show(id: u32, visible: bool) -> u32 {
    id
}

fn one(on: bool) -> bool {
    on
}

fn f(w: f32) -> f32 {
    let a = area(3.0, 4.0)
    let b = area(w, 4.0)
    let c = area(3.0, height: 4.0)
    let d = show(1, true)
    let e = one(true)
    a + b + c + f32(d) + if e { 1.0 } else { 0.0 }
}
";
    let w = warnings("bare-literals", src);
    let lines: Vec<usize> = w.iter().filter(|(c, _)| c == "W0005").map(|(_, l)| *l).collect();
    assert_eq!(lines, [14, 17]);
    let out = wrela_driver::check(&package("warnings/bare-literals-fix", src));
    let main = out.sources.files().find(|(_, f)| f.name.ends_with("main.wrela")).map(|(id, _)| id);
    let fixed = wrela_tests::apply_fixes(src, main.expect("main.wrela"), &out.diagnostics);
    assert!(fixed.contains("area(width: 3.0, height: 4.0)"), "{fixed}");
    assert!(fixed.contains("show(1, visible: true)"), "{fixed}");
    let again = warnings("bare-literals-fixed", &fixed);
    assert!(again.iter().all(|(c, _)| c != "W0005"), "{again:?}");
}

/// W0005 for `select`: its two values by position, whatever they are, since the value for
/// `false` comes first. Its fix names every positional argument; named ones, in any order,
/// aren't reported.
#[test]
fn a_positional_select_is_reported_and_named() {
    let src = "fn f(c: f32, a: f32, b: f32) -> f32 {
    let x = select(a, b, c > 0.0)
    let y = select(a, if_true: b, cond: c > 0.0)
    let z = select(cond: c > 0.0, if_true: b, if_false: a)
    x + y + z
}
";
    let w = warnings("select", src);
    let lines: Vec<usize> = w.iter().filter(|(c, _)| c == "W0005").map(|(_, l)| *l).collect();
    assert_eq!(lines, [2]);
    let out = wrela_driver::check(&package("warnings/select-fix", src));
    let main = out.sources.files().find(|(_, f)| f.name.ends_with("main.wrela")).map(|(id, _)| id);
    let fixed = wrela_tests::apply_fixes(src, main.expect("main.wrela"), &out.diagnostics);
    assert!(fixed.contains("select(if_false: a, if_true: b, cond: c > 0.0)"), "{fixed}");
    let again = warnings("select-fixed", &fixed);
    assert!(again.iter().all(|(c, _)| c != "W0005"), "{again:?}");
}

/// std's own code has no warnings. A warning there would be std's authors' to fix, so a
/// program's diagnostics leave it out (`Output::std_warnings`); this keeps std clean.
#[test]
fn std_has_no_warnings() {
    let out = wrela_driver::check(&package("warnings/std", ""));
    let shown = wrela_diag::render::render_all(&out.sources, &out.std_warnings);
    assert!(out.std_warnings.is_empty(), "std's warnings:\n{shown}");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
}
