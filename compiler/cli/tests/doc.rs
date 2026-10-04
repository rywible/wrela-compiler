//! `wrela doc` (AC13): every public item of std is found by its path, with its signature, in
//! under 100 ms (timed in a release build: a debug build's parser is slower).

use std::process::Command;
use std::time::Instant;

fn doc(args: &[&str]) -> (bool, String, f64) {
    let t = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_wrela"))
        .arg("doc")
        .args(args)
        .output()
        .expect("run wrela");
    let secs = t.elapsed().as_secs_f64();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), secs)
}

/// The items a module listing names: `  kind  name  summary` lines.
fn items(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| l.strip_prefix("  "))
        .filter_map(|l| l.split_whitespace().nth(1).map(String::from))
        .collect()
}

#[test]
fn every_public_std_item_has_a_page() {
    let (ok, modules, _) = doc(&["std"]);
    assert!(ok, "{modules}");
    let modules: Vec<String> = modules
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|m| m.starts_with("std::"))
        .map(String::from)
        .collect();
    assert!(modules.len() >= 20, "{modules:?}");
    let mut slowest = 0.0f64;
    let mut checked = 0;
    for m in &modules {
        let (ok, listing, _) = doc(&[m]);
        assert!(ok, "{m}: {listing}");
        for item in items(&listing) {
            let path = format!("{m}::{item}");
            let (ok, page, secs) = doc(&[&path]);
            assert!(ok && page.starts_with(&path), "{path}:\n{page}");
            slowest = slowest.max(secs);
            checked += 1;
        }
    }
    assert!(checked > 150, "only {checked} items");
    if !cfg!(debug_assertions) {
        assert!(slowest < 0.1, "the slowest page took {slowest:.3} s");
    }
}

/// A name alone finds the item; one that several items have lists them.
#[test]
fn a_bare_name_is_found() {
    let (ok, page, _) = doc(&["Arena"]);
    assert!(ok && page.starts_with("std::arena::Arena (struct"), "{page}");
    assert!(page.contains("pub fn insert("), "the methods are listed:\n{page}");
    let (ok, list, _) = doc(&["par_map_reduce"]);
    assert!(ok && list.contains("items are named `par_map_reduce`"), "{list}");
    let (ok, _, _) = doc(&["no_such_thing_anywhere"]);
    assert!(!ok);
}
