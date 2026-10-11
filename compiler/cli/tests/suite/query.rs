//! `wrela query` and `wrela context` (#39, the owner's additions): one check answers a batch of
//! queries about a program (types, callers, callees, impls, effects, borrows, instantiations,
//! signatures), and `context` gives an item's source and what's around it within a budget.

use crate::common;

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

/// The package in a scratch directory of its own for each test (`tag`).
fn package(tag: &str) -> PathBuf {
    common::package(
        &format!("query-shapes-{tag}"),
        &[("wrela.toml", &common::manifest("shapes")), ("main.wrela", common::SHAPES)],
    )
}

/// The answers to `queries`, given as arguments.
fn query(dir: &PathBuf, queries: &[&str]) -> Vec<serde_json::Value> {
    let a = common::json(common::wrela().arg("query").arg(dir).arg("--json").args(queries));
    a.as_array().cloned().unwrap_or_else(|| panic!("not a list: {a}"))
}

#[test]
fn one_check_answers_every_kind_of_query() {
    let dir = package("kinds");
    let a = query(
        &dir,
        &[
            "type main.wrela:50:13",
            "callers grow",
            "callees first",
            "callers Shape::area",
            "impls Shape",
            "impls Grid",
            "effects grow",
            "effects Square::area",
            "borrows main.wrela:49",
            "borrows main.wrela:53",
            "instantiations total",
            "instantiations Shape::area",
            "search (Grid, ..) -> f32",
            "callers nothing_is_called_this",
        ],
    );
    assert_eq!(a.len(), 14);
    assert_eq!(
        (a[0]["text"].as_str(), a[0]["type"].as_str()),
        (Some("first(g)"), Some("f32")),
        "{}",
        a[0]
    );
    let calls = a[1]["callers"].as_array().unwrap();
    assert_eq!(a[1]["function"], "main::grow", "the program's own `grow`, not std's private one");
    assert_eq!(
        (calls.len(), calls[0]["caller"].as_str(), calls[0]["at"].as_str()),
        (1, Some("main::frame"), Some("main.wrela:47:5"))
    );
    assert_eq!(a[2]["callees"][0]["callee"], "main::Grid::at", "{}", a[2]);
    assert_eq!(a[3]["callers"][0]["caller"], "main::total", "{}", a[3]);
    assert_eq!(a[4]["impls"][0]["type"], "Square", "{}", a[4]);
    let grid: Vec<&str> = a[5]["impls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["methods"][0].as_str().unwrap_or(""))
        .collect();
    assert_eq!(grid, ["at"], "{}", a[5]);
    // `grow` allocates, through `Vec::push`; an area does nothing but compute.
    let fx = &a[6]["effects"][0];
    assert_eq!(
        (fx["effect"].as_str(), fx["chain"][1].as_str()),
        (Some("alloc"), Some("Vec<T>::push")),
        "{}",
        a[6]
    );
    assert_eq!(a[7]["effects"].as_array().map(Vec::len), Some(0), "{}", a[7]);
    // Before line 49, `c` borrows `g.cells`; before line 53, `g` has been moved out of.
    let loan = &a[8]["loans"][0];
    assert_eq!(
        (loan["place"].as_str(), loan["holder"].as_str(), loan["mutable"].as_bool()),
        (Some("g.cells"), Some("`c`"), Some(false)),
        "{}",
        a[8]
    );
    assert_eq!(a[9]["moved"][0]["place"], "g", "{}", a[9]);
    assert_eq!(a[10]["instantiations"][0]["types"][0], "Square", "{}", a[10]);
    assert_eq!(a[11]["instantiations"][0]["function"], "main::Square::area", "{}", a[11]);
    let found: Vec<&str> = a[12]["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["function"].as_str().unwrap())
        .collect();
    assert_eq!(found, ["main::Grid::at", "main::first"]);
    assert!(a[13]["error"].as_str().is_some_and(|e| e.contains("nothing is named")), "{}", a[13]);
}

/// A program with an error is answered too: what doesn't depend on the function with the error
/// is answered as it would be, an answer it could change lists the error as `unsure`, and a
/// query of that function's code says why it can't be answered. The errors are on standard
/// error, and the exit status is 1.
#[test]
fn a_program_with_errors_is_answered_where_it_can_be() {
    let text = format!(
        "{}\nfn broken(g: Grid) -> f32 {{\n    let wrong: bool = 3\n    first(g)\n}}\n",
        common::SHAPES
    );
    let line = text.lines().position(|l| l.contains("let wrong")).expect("the error") + 1;
    let dir = common::package(
        "query-shapes-broken",
        &[("wrela.toml", &common::manifest("shapes")), ("main.wrela", &text)],
    );
    let inside = format!("type main.wrela:{line}:9");
    let out = common::wrela()
        .arg("query")
        .arg(&dir)
        .arg("--json")
        .args(["type main.wrela:50:13", "callers first", "effects frame", &inside])
        .args(["instantiations total"])
        .output()
        .expect("run wrela");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("E0300"));
    let a: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).expect("answers");
    assert_eq!(a[0]["type"], "f32", "{}", a[0]);
    assert!(a[0].get("unsure").is_none(), "`frame` has no error: {}", a[0]);
    // `broken` calls `first`, but its code isn't known.
    assert_eq!(a[1]["callers"].as_array().map(Vec::len), Some(1), "{}", a[1]);
    let unsure = &a[1]["unsure"][0];
    assert_eq!(
        (unsure["code"].as_str(), unsure["in"].as_str()),
        (Some("E0300"), Some("main::broken")),
        "{}",
        a[1]
    );
    assert!(a[2].get("unsure").is_none(), "`frame` doesn't reach `broken`: {}", a[2]);
    assert!(
        a[3]["error"].as_str().is_some_and(|e| e.contains("`main::broken` has errors")),
        "{}",
        a[3]
    );
    assert!(a[4]["error"].as_str().is_some_and(|e| e.contains("lowering")), "{}", a[4]);
    let context = common::wrela().args(["context", "broken"]).arg(&dir).output().unwrap();
    let text = String::from_utf8_lossy(&context.stdout);
    assert!(
        text.contains("fn broken") && text.contains("What `main::broken` calls isn't known"),
        "{text}"
    );
}

#[test]
fn queries_come_from_standard_input_too() {
    let dir = package("stdin");
    let mut child = common::wrela()
        .arg("query")
        .arg(&dir)
        .arg("--json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"callers first\n\nimpls Shape\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let a: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!((a.len(), a[0]["query"].as_str()), (2, Some("callers first")));
}

#[test]
fn context_fits_its_budget() {
    let dir = package("context");
    let context = |budget: &str| {
        common::stdout(
            common::wrela().args(["context", "first"]).arg(&dir).args(["--budget", budget]),
        )
    };
    let all = context("2000");
    for part in [
        "fn first(g: Grid) -> f32",
        "/// A row of heights.",
        "main::Grid::at: fn at(self, i: u32) -> f32",
        "main::frame (main.wrela:50:13)",
    ] {
        assert!(all.contains(part), "no `{part}` in:\n{all}");
    }
    let small = context("40");
    assert!(
        small.contains("fn first")
            && small.contains("Left out")
            && !small.contains("A row of heights"),
        "{small}"
    );
    assert!(small.len() <= 4 * 40 + 120, "{} characters", small.len());
}
