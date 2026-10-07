//! `wrela doc` (AC13): every public item of std is found by its path, with its signature, in
//! under 100 ms (timed in a release build: a debug build's parser is slower).

mod common;

use std::time::Instant;

fn doc(args: &[&str]) -> (bool, String, f64) {
    let t = Instant::now();
    let out = common::wrela().arg("doc").args(args).output().expect("run wrela");
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

/// Every public item of std, by its path (`std::arena::Arena`).
fn std_items() -> Vec<String> {
    let (ok, modules, _) = doc(&["std"]);
    assert!(ok, "{modules}");
    let modules: Vec<String> = modules
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|m| m.starts_with("std::"))
        .map(String::from)
        .collect();
    assert!(modules.len() >= 20, "{modules:?}");
    let mut paths = Vec::new();
    for m in &modules {
        let (ok, listing, _) = doc(&[m]);
        assert!(ok, "{m}: {listing}");
        paths.extend(items(&listing).into_iter().map(|item| format!("{m}::{item}")));
    }
    assert!(paths.len() > 150, "only {} items", paths.len());
    paths
}

#[test]
fn every_public_std_item_has_a_page() {
    let paths = std_items();
    // A process per page, several at once.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for chunk in paths.chunks(paths.len().div_ceil(threads)) {
            s.spawn(move || {
                for path in chunk {
                    let (ok, page, _) = doc(&[path]);
                    assert!(ok && page.starts_with(path.as_str()), "{path}:\n{page}");
                }
            });
        }
    });
}

/// Each page in under 100 ms, one at a time.
#[test]
#[ignore = "long: a timing run"]
fn every_page_takes_under_a_tenth_of_a_second() {
    let mut slowest = 0.0f64;
    for path in std_items() {
        let (ok, page, secs) = doc(&[&path]);
        assert!(ok, "{path}:\n{page}");
        slowest = slowest.max(secs);
    }
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

/// The built-in functions are listed, and one is found by its name.
#[test]
fn builtins_are_listed() {
    let (ok, list, _) = doc(&["builtins"]);
    assert!(ok, "{list}");
    for f in ["length(x)", "clamp(x, y, z)", "smoothstep(x, y, z)", "normalize(x)"] {
        assert!(list.contains(f), "{f} isn't listed:\n{list}");
    }
    let (ok, page, _) = doc(&["mix"]);
    assert!(ok && page.starts_with("mix: a built-in function of 3 arguments"), "{page}");
}

/// The packages a package depends on are read: their items are found under the dependency's
/// name, and a struct's page shows its fields with their doc comments.
#[test]
fn a_dependency_is_read() {
    let dir = common::package(
        "doc-deps",
        &[
            (
                "wrela.toml",
                "[package]\nname = \"app\"\n\n[dependencies]\nkit = { path = \"kit\" }\n",
            ),
            ("main.wrela", common::FRAME),
            ("kit/wrela.toml", &common::manifest("kit")),
            (
                "kit/shapes.wrela",
                "// Shapes for the kit.\n\n/// A box's size.\npub struct Size {\n    /// Across, in metres.\n    pub width: f32,\n    pub height: f32,\n}\n",
            ),
        ],
    );
    let d = dir.to_str().unwrap();
    let (ok, page, _) = doc(&["kit::shapes::Size", d]);
    assert!(ok && page.starts_with("kit::shapes::Size"), "{page}");
    assert!(page.contains("/// Across, in metres.") && page.contains("pub width: f32"), "{page}");
    let (ok, listing, _) = doc(&["kit::shapes", d]);
    assert!(ok && items(&listing) == ["Size"], "{listing}");
}
