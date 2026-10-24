//! `wrela refactor` (#39, the owner's additions): renames, moves, new parameters and modes,
//! each planned, checked with the new texts before anything is written, and refused (with the
//! errors) when the program wouldn't check, when a call would quietly change its target, or
//! when a plan is applied to a file that changed since.

use crate::common;

use std::path::{Path, PathBuf};

fn package(tag: &str) -> PathBuf {
    common::package(
        &format!("refactor-{tag}"),
        &[
            ("wrela.toml", &common::manifest("shapes")),
            ("main.wrela", common::SHAPES),
            ("util.wrela", "// Helpers.\n"),
        ],
    )
}

fn plan(dir: &Path, args: &[&str]) -> serde_json::Value {
    common::json(common::wrela().arg("refactor").arg(dir).args(args))
}

fn checks(dir: &Path) -> bool {
    common::wrela().arg("check").arg(dir).output().unwrap().status.success()
}

fn main_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("main.wrela")).unwrap()
}

#[test]
fn a_rename_reaches_every_reference_and_nothing_else() {
    let dir = package("rename");
    let a = plan(&dir, &["rename", "Grid::at", "get", "--json"]);
    assert_eq!(a["written"], true, "{a}");
    let text = main_text(&dir);
    assert_eq!(text.matches(".get(").count(), 2, "{text}");
    assert!(!text.contains(".at(") && text.contains("pub fn get(self"), "{text}");
    assert!(checks(&dir));
    // The only lines changed are those three.
    let changed: usize = a["files"][0]["hunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["new"].as_array().unwrap().len())
        .sum();
    assert_eq!(changed, 3, "{a}");
    // A trait's method, with its impl's; a type, through signatures, impls and literals.
    plan(&dir, &["rename", "Shape::area", "size", "--json"]);
    plan(&dir, &["rename", "Grid", "Row", "--json"]);
    let text = main_text(&dir);
    assert!(
        text.contains("fn size(self) -> f32 {")
            && text.contains("s.size()")
            && !text.contains("Grid"),
        "{text}"
    );
    assert!(checks(&dir));
}

#[test]
fn a_rename_is_refused_where_it_would_change_what_code_means() {
    let dir = package("rename-refused");
    // A name the module has already.
    let a = plan(&dir, &["rename", "first", "total", "--json"]);
    assert!(a["refused"].as_str().unwrap().contains("already a name"), "{a}");
    // A function named like a built-in: without it, its call would go to the built-in.
    std::fs::write(
        dir.join("main.wrela"),
        "/// The smaller, by a margin.\nfn min(a: f32, b: f32) -> f32 {\n    if a < b + 0.5 { a } else { b }\n}\n\npub fn frame(time: f32, width: u32, height: u32) {\n    let _ = min(time, 2.0)\n}\n",
    )
    .unwrap();
    let before = main_text(&dir);
    let a = plan(&dir, &["rename", "min", "smallest", "--json"]);
    assert!(a["refused"].as_str().unwrap().contains("another function"), "{a}");
    assert_eq!(main_text(&dir), before, "nothing written");
}

#[test]
fn a_move_imports_what_each_side_needs() {
    let dir = package("move");
    let a = plan(&dir, &["move", "first", "util", "--json"]);
    assert_eq!(a["written"], true, "{a}");
    let util = std::fs::read_to_string(dir.join("util.wrela")).unwrap();
    let text = main_text(&dir);
    assert!(
        util.contains("use main::Grid") && util.contains("pub(package) fn first(g: Grid)"),
        "{util}"
    );
    assert!(text.contains("use util::first") && !text.contains("fn first"), "{text}");
    assert!(checks(&dir));
    // A type moves with its impls.
    plan(&dir, &["move", "Grid", "util", "--json"]);
    let util = std::fs::read_to_string(dir.join("util.wrela")).unwrap();
    assert!(util.contains("pub struct Grid") && util.contains("impl Grid {"), "{util}");
    assert!(checks(&dir));
}

#[test]
fn parameters_are_added_and_their_modes_changed_at_every_call() {
    let dir = package("params");
    plan(&dir, &["add-param", "first", "scale: f32", "--value", "2.0", "--json"]);
    let text = main_text(&dir);
    assert!(
        text.contains("fn first(g: Grid, scale: f32) -> f32")
            && text.contains("first(g, scale: 2.0)"),
        "{text}"
    );
    plan(&dir, &["add-param", "Shape::area", "k: f32", "--default", "1.0", "--json"]);
    assert!(
        main_text(&dir).contains("fn area(self, k: f32 = 1.0) -> f32\n"),
        "{}",
        main_text(&dir)
    );
    plan(&dir, &["change-mode", "first", "g", "mut", "--json"]);
    let text = main_text(&dir);
    assert!(
        text.contains("fn first(g: mut Grid, scale: f32)")
            && text.contains("first(mut g, scale: 2.0)"),
        "{text}"
    );
    assert!(checks(&dir));
    // `take` would move `g` before its last use: refused, with the error, and nothing written.
    let before = main_text(&dir);
    let a = plan(&dir, &["change-mode", "first", "g", "take", "--json"]);
    assert!(a["errors"].as_str().unwrap().contains("E0500"), "{a}");
    assert_eq!(main_text(&dir), before);
}

#[test]
fn new_arguments_and_parameters_stay_before_trailing_comments() {
    let dir = common::package(
        "refactor-parameter-comments",
        &[(
            "main.wrela",
            "fn f(a: i32 // keep the parameter\n) -> i32 { a }\n\
             pub fn g() -> i32 {\n    f(1 // keep the argument\n    )\n}\n\
             pub fn frame(time: f32, width: u32, height: u32) {}\n",
        )],
    );
    let result = plan(&dir, &["add-param", "f", "b: i32", "--value", "0", "--json"]);
    assert_eq!(result["written"], true, "{result}");
    let text = main_text(&dir);
    assert!(text.contains("a: i32, b: i32 // keep the parameter"), "{text}");
    assert!(text.contains("f(1, b: 0 // keep the argument"), "{text}");
    assert!(checks(&dir));
}

#[test]
fn a_plan_applies_only_to_the_files_it_was_made_from() {
    let dir = package("apply");
    let a = plan(&dir, &["rename", "first", "head", "--dry-run", "--json"]);
    assert_eq!(a["written"], false);
    let saved = dir.join("plan.json");
    std::fs::write(&saved, serde_json::to_string(&a).unwrap()).unwrap();
    // Another tool changes the file: the plan is stale.
    let edited =
        main_text(&dir).replace("/// A row of heights.", "/// A row of heights, in metres.");
    std::fs::write(dir.join("main.wrela"), &edited).unwrap();
    let r = plan(&dir, &["apply", saved.to_str().unwrap(), "--json"]);
    assert!(r["refused"].as_str().unwrap().contains("has changed since the plan was made"), "{r}");
    assert_eq!(main_text(&dir), edited);
    // Made again, it applies.
    let a = plan(&dir, &["rename", "first", "head", "--dry-run", "--json"]);
    std::fs::write(&saved, serde_json::to_string(&a).unwrap()).unwrap();
    let r = plan(&dir, &["apply", saved.to_str().unwrap(), "--json"]);
    assert_eq!(r["written"], true, "{r}");
    assert!(main_text(&dir).contains("fn head(g: Grid)") && checks(&dir));
}

/// A package of `main` alone, for the extract and inline refactors.
fn program(tag: &str, main: &str) -> PathBuf {
    common::package(
        &format!("refactor-{tag}"),
        &[("wrela.toml", &common::manifest("shapes")), ("main.wrela", main)],
    )
}

/// Whether the package's tests pass: a refactor keeps what the code computes, not only that it
/// checks, so each case's package asserts its values.
fn tests_pass(dir: &Path) -> bool {
    let out = common::wrela().arg("test").arg(dir).output().unwrap();
    out.status.success()
}

const EXTRACTS: &str = "pub fn frame(time: f32, width: u32, height: u32) {}

fn area(w: f32, h: f32) -> f32 {
    var total = 0.0
    total += w * h * 0.5
    total += 1.0
    total
}

@test
fn area_is_right() {
    assert(area(2.0, 3.0) == 4.0)
}
";

#[test]
fn an_expression_of_literals_becomes_a_constant() {
    let dir = program("extract-const", EXTRACTS);
    let a = plan(&dir, &["extract-const", "main.wrela:5:22", "HALF", "--json"]);
    assert_eq!(a["written"], true, "{a}");
    let text = main_text(&dir);
    assert!(text.contains("const HALF: f32 = 0.5") && text.contains("w * h * HALF"), "{text}");
    assert!(checks(&dir) && tests_pass(&dir));
    // (The constant and a blank line now come first.) An expression with a local in it can't
    // be a constant.
    let r = plan(&dir, &["extract-const", "main.wrela:7:14", "ONE", "--json"]);
    assert!(r["refused"].as_str().is_some_and(|w| w.contains("a constant is")), "{r}");
    // Nor can it take a name the module has.
    let r = plan(&dir, &["extract-const", "main.wrela:8:14", "HALF", "--json"]);
    assert!(r["refused"].as_str().is_some_and(|w| w.contains("already")), "{r}");
}

#[test]
fn lines_become_a_function_with_what_flows_in_and_out() {
    let dir = program("extract-fn", EXTRACTS);
    let a = plan(&dir, &["extract-fn", "main.wrela:4-6", "accumulate", "--json"]);
    assert_eq!(a["written"], true, "{a}");
    let text = main_text(&dir);
    assert!(text.contains("let total = accumulate(w, h)"), "{text}");
    assert!(text.contains("fn accumulate(w: f32, h: f32) -> f32 {"), "{text}");
    assert!(checks(&dir) && tests_pass(&dir));
    // A parameter the lines change is lent `mut`.
    let dir = program(
        "extract-fn-mut",
        "pub fn frame(time: f32, width: u32, height: u32) {}

fn fill(n: u32) -> u32 {
    var v: Vec<u32> = Vec::new()
    v.push(n)
    v.push(n + 1)
    v.len()
}

@test
fn fills_two() {
    assert(fill(3) == 2)
}
",
    );
    plan(&dir, &["extract-fn", "main.wrela:5-6", "push_two", "--json"]);
    let text = main_text(&dir);
    assert!(text.contains("push_two(mut v, n)") && text.contains("v: mut Vec<u32>"), "{text}");
    assert!(checks(&dir) && tests_pass(&dir));
    // Two values that flow out are refused, and so are lines that leave early.
    let dir = program(
        "extract-fn-refused",
        "pub fn frame(time: f32, width: u32, height: u32) {}

fn two(x: f32) -> f32 {
    let a = x + 1.0
    let b = x + 2.0
    if a > b {
        return a
    }
    a + b
}
",
    );
    let r = plan(&dir, &["extract-fn", "main.wrela:4-5", "both", "--json"]);
    assert!(r["refused"].as_str().is_some_and(|w| w.contains("more than one value")), "{r}");
    let r = plan(&dir, &["extract-fn", "main.wrela:6-8", "early", "--json"]);
    assert!(r["refused"].as_str().is_some_and(|w| w.contains("leave early")), "{r}");
    // The call would be a local's (`b`) or a built-in's (`sqrt`), not the new function's.
    for name in ["b", "sqrt"] {
        let r = plan(&dir, &["extract-fn", "main.wrela:4-4", name, "--json"]);
        assert!(r["refused"].as_str().is_some_and(|w| w.contains("has already")), "{name}: {r}");
    }
}

const INLINES: &str = "pub fn frame(time: f32, width: u32, height: u32) {}

fn half(x: f32) -> f32 {
    x / 2
}

fn widen(x: f32) -> f64 {
    f64(x)
}

fn minus(a: f32, b: f32) -> f32 {
    a - b
}

fn twice(x: f32) -> f32 {
    x + x
}

fn swapped(a: f32, b: f32) -> f32 {
    b - a
}

fn unused(a: f32, b: f32) -> f32 {
    a
}

fn uses() -> f32 {
    let three: f32 = 3.0
    let big: f32 = 16777217.0
    let one: f32 = 1.0
    let untyped = 4.0
    half(three) + f32(widen(big) - 16777216.0) + minus(b: one, a: three) + twice(one) + swapped(one, three) + unused(one, three) + half(untyped) + half(2.0) + half(minus(one, one))
}

@test
fn values_hold() {
    assert(uses() == 1.5 + 0.0 + 2.0 + 2.0 + 2.0 + 1.0 + 2.0 + 1.0 + 0.0)
}
";

#[test]
fn an_inlined_body_names_what_it_named_in_its_function() {
    let dir = common::package(
        "refactor-inline-names",
        &[
            ("wrela.toml", &common::manifest("shapes")),
            (
                "main.wrela",
                "use util::shift

const K: f32 = 2.0

pub fn frame(time: f32, width: u32, height: u32) {}

fn shifted() -> f32 {
    let one: f32 = 1.0
    shift(one) + K
}

fn unit() -> f32 {
    3.0
}

fn grow(x: f32) -> f32 {
    x * unit()
}

fn grown() -> f32 {
    let unit = || f32(10.0)
    let one: f32 = 1.0
    grow(one) + unit()
}

@test
fn values_hold() {
    assert(shifted() == 4.0)
    assert(grown() == 13.0)
}
",
            ),
            (
                "util.wrela",
                "pub const K: f32 = 1.0\n\npub(package) fn shift(x: f32) -> f32 {\n    x + K\n}\n",
            ),
        ],
    );
    assert!(tests_pass(&dir), "the fixture's own test passes");
    let before = main_text(&dir);
    // `K` in `shift` is util's; where it's called, `K` is main's.
    let r = plan(&dir, &["inline", "shift", "--json"]);
    assert!(r["refused"].as_str().is_some_and(|w| w.contains("names something else")), "{r}");
    // `unit` in `grow` is the function; where it's called, `unit` is a local closure.
    let r = plan(&dir, &["inline", "grow", "--json"]);
    assert!(r["refused"].as_str().is_some_and(|w| w.contains("would name the local")), "{r}");
    assert_eq!(main_text(&dir), before, "nothing written");
}

#[test]
fn a_call_becomes_the_body_where_nothing_about_it_can_change() {
    let dir = program("inline", INLINES);
    assert!(tests_pass(&dir), "the fixture's own test passes");
    // Arguments that aren't typed locals: a literal, an untyped local, a call.
    let r = plan(&dir, &["inline", "half", "--json"]);
    assert!(r["refused"].as_str().is_some_and(|w| w.contains("passes")), "{r}");
    // A parameter used twice, or not at all, or out of order, is refused.
    for (f, why) in
        [("twice", "used 2 times"), ("unused", "used 0 times"), ("swapped", "another order")]
    {
        let r = plan(&dir, &["inline", f, "--json"]);
        assert!(r["refused"].as_str().is_some_and(|w| w.contains(why)), "{f}: {r}");
    }
    // A conversion of a typed local keeps its value; a named argument maps to its parameter.
    let a = plan(&dir, &["inline", "widen", "--json"]);
    assert_eq!(a["written"], true, "{a}");
    assert!(main_text(&dir).contains("f64(big)") && !main_text(&dir).contains("fn widen"));
    assert!(checks(&dir) && tests_pass(&dir));
    // A named argument goes to its parameter: `minus(b: one, a: three)` is `three - one`.
    let a = plan(&dir, &["inline", "minus", "--json"]);
    assert_eq!(a["written"], true, "{a}");
    assert!(main_text(&dir).contains("(three - one)") && !main_text(&dir).contains("fn minus"));
    assert!(checks(&dir) && tests_pass(&dir));
}
