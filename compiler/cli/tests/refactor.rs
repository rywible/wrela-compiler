//! `wrela refactor` (#39, the owner's additions): renames, moves, new parameters and modes,
//! each planned, checked with the new texts before anything is written, and refused (with the
//! errors) when the program wouldn't check, when a call would quietly change its target, or
//! when a plan is applied to a file that changed since.

mod common;

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
