//! `wrela doc` (AC13): every public item of std is found by its path, with its signature, in
//! under 100 ms (timed in a release build: a debug build's parser is slower).

use crate::common;

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

/// The built-in types have pages: each is its spec section, with the built-in functions that
/// take it as a method and std's impls of it; another name of one finds its page.
#[test]
fn builtin_types_are_documented() {
    let (ok, vec3, _) = doc(&["vec3"]);
    assert!(ok && vec3.starts_with("builtins::vec3 (type"), "{vec3}");
    for part in ["vec3(x, y, z)", "v.zyx", "x.normalize()", "language.md §4.2", "implements:"] {
        assert!(vec3.contains(part), "{part:?} isn't on vec3's page:\n{vec3}");
    }
    let (ok, run, _) = doc(&["[T]"]);
    assert!(ok && run.contains("x.len()") && run.contains("pub fn is_empty(self)"), "{run}");
    let (ok, again, _) = doc(&["run"]);
    assert!(ok && again == run, "`run` is `[T]`'s page:\n{again}");
    let (ok, array, _) = doc(&["[T; N]"]);
    assert!(ok && array.contains("[x; N]"), "{array}");
    let (ok, ints, _) = doc(&["u8"]);
    assert!(ok && ints.starts_with("builtins::u32") && ints.contains("x.count_ones()"), "{ints}");
    assert!(!ints.contains("x.sqrt()"), "an integer has no `sqrt`:\n{ints}");
    let (ok, f, _) = doc(&["f32"]);
    assert!(ok && f.contains("x.sqrt()"), "{f}");
    let (ok, text, _) = doc(&["str"]);
    assert!(ok && text.contains("parse_f32"), "std's `impl str` is on its page:\n{text}");
    let (ok, list, _) = doc(&["builtins"]);
    assert!(ok && list.contains("the built-in types") && list.contains("[T; N]"), "{list}");
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

/// A package for the export's tests: a struct with a method and a function, each documented.
fn shapes(name: &str) -> std::path::PathBuf {
    common::package(
        name,
        &[
            ("fixture/wrela.toml", &common::manifest("fixture")),
            (
                "fixture/shapes.wrela",
                "// Shapes: a fixture for the export.\n\n/// A disc.\npub struct Disc {\n    pub r: f32,\n}\n\nimpl Disc {\n    /// Its area.\n    pub fn area(self) -> f32 {\n        3.0 * self.r * self.r\n    }\n}\n\n/// A disc of radius one.\npub fn unit() -> Disc {\n    Disc { r: 1.0 }\n}\n",
            ),
        ],
    )
}

/// Exports `fixture` from `root` (so its files are named from there) into `root/out`.
fn export(root: &std::path::Path, out: &str, extra: &[&str]) -> std::process::Output {
    common::wrela()
        .current_dir(root)
        .args(["doc", "--export", out, "fixture"])
        .args(extra)
        .output()
        .expect("run wrela")
}

/// The export writes a page per module, an index of every item and every section of the spec,
/// and a map of the pages: each item with its page, its anchor there and its source, and the
/// spec's prose searchable by its words.
#[test]
fn the_export_writes_pages_an_index_and_a_map() {
    let root = shapes("doc-export");
    let out = export(&root, "out", &["--commit", "abcdef1"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let page =
        std::fs::read_to_string(root.join("out/fixture/shapes.md")).expect("the module's page");
    assert!(page.starts_with("# fixture::shapes\n\nShapes: a fixture for the export.\n"), "{page}");
    for part in
        ["## Disc\n", "### Disc::area\n", "Its area.", "`fixture/shapes.wrela:10`", "## unit\n"]
    {
        assert!(page.contains(part), "{part:?} isn't on the page:\n{page}");
    }
    let index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("out/index.json")).expect("index.json"))
            .expect("JSON");
    assert_eq!(index["commit"], "abcdef1");
    // A development build's std is the packages' repository's, at their commit.
    assert_eq!(index["toolchain"]["repository"], index["repository"]);
    assert_eq!(index["toolchain"]["commit"], "abcdef1");
    let area = index["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["path"] == "fixture::shapes::Disc::area");
    let area = area.expect("the method is indexed");
    assert_eq!(
        (&area["page"], &area["anchor"], &area["source"], &area["line"], &area["kind"]),
        (
            &"fixture/shapes.md".into(),
            &"disc-area".into(),
            &"fixture/shapes.wrela".into(),
            &10.into(),
            &"method".into()
        )
    );
    // std is there too, and the spec's prose is found by words only it has.
    assert!(index["items"].as_array().unwrap().iter().any(|i| i["path"] == "std::arena::Arena"));
    // So are the built-in types, on the built-ins' page with the built-in functions.
    let vec3 = index["items"].as_array().unwrap().iter().find(|i| i["path"] == "builtins::vec3");
    let vec3 = vec3.expect("the built-in types are indexed");
    assert_eq!(
        (&vec3["page"], &vec3["source"]),
        (&"std/builtins.md".into(), &"spec/language.md".into())
    );
    let builtins = std::fs::read_to_string(root.join("out/std/builtins.md")).expect("built-ins");
    assert!(builtins.starts_with("# Built-in types and functions\n"), "{builtins}");
    for part in [
        "## vec3\n",
        "## [T]\n",
        "### [T]::is_empty\n",
        "## Built-in functions",
        "`clamp(x, y, z)`",
    ] {
        assert!(builtins.contains(part), "{part:?} isn't on the built-ins' page");
    }
    let sections = index["sections"].as_array().expect("sections");
    let found = sections
        .iter()
        .find(|s| s["text"].as_str().is_some_and(|t| t.contains("garbage collector")));
    let found = found.expect("a section of the spec mentions the garbage collector");
    let page = found["page"].as_str().unwrap();
    assert!(page.starts_with("spec/") && root.join("out").join(page).is_file(), "{found}");
    let llms = std::fs::read_to_string(root.join("out/llms.txt")).expect("llms.txt");
    assert!(
        llms.contains(
            "- [fixture::shapes](fixture/shapes.md): Shapes: a fixture for the export.\n"
        ),
        "{llms}"
    );
    assert!(llms.contains("commit abcdef1"), "{llms}");
    let full = std::fs::read_to_string(root.join("out/llms-full.txt")).expect("llms-full.txt");
    assert!(full.contains("<!-- fixture/shapes.md -->") && full.contains("### Disc::area"));
    // Each group is in one file of its own too, listed with its size; the language's first.
    let group = std::fs::read_to_string(root.join("out/fixture/llms-full.txt")).expect("a group");
    assert!(group.starts_with("<!-- fixture/shapes.md -->") && !group.contains("<!-- std/"));
    let spec = std::fs::read_to_string(root.join("out/spec/llms-full.txt")).expect("the spec");
    assert!(spec.contains("<!-- spec/language/6-memory.md -->") && !spec.contains("<!-- std/"));
    let kb = group.len().div_ceil(1000);
    assert!(llms.contains(&format!("- [fixture](fixture/llms-full.txt): {kb} kB\n")), "{llms}");
    let (language, everything) =
        (llms.find("(spec/llms-full.txt)").unwrap(), llms.find("(llms-full.txt)").unwrap());
    assert!(language < llms.find("(std/llms-full.txt)").unwrap() && language < everything);
}

/// The same input gives the same bytes; an export never writes over another, and a commit is
/// a commit's name.
#[test]
fn the_export_is_the_same_each_time_and_writes_only_into_a_new_directory() {
    let root = shapes("doc-export-again");
    assert!(export(&root, "a", &[]).status.success());
    assert!(export(&root, "b", &[]).status.success());
    let (a, b) = (common::files(&root.join("a")), common::files(&root.join("b")));
    assert!(a.len() > 30, "{} files", a.len());
    assert!(a == b, "two exports of the same sources differ");
    let again = export(&root, "a", &[]);
    assert!(
        !again.status.success() && String::from_utf8_lossy(&again.stderr).contains("isn't empty")
    );
    let bad = export(&root, "c", &["--commit", "main; rm"]);
    assert!(!bad.status.success() && !root.join("c").exists());
}

#[test]
fn the_export_reads_commented_package_names() {
    let root = shapes("doc-export-commented-name");
    std::fs::write(
        root.join("fixture/wrela.toml"),
        "[package] # documentation fixture\nname = \"fixture\" # the package name\n",
    )
    .expect("write manifest");
    let out = export(&root, "out", &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(root.join("out/fixture/shapes.md").is_file());
}

#[test]
fn the_export_rejects_package_names_that_escape_its_directory() {
    let root = shapes("doc-export-escaping-name");
    std::fs::create_dir(root.join("escaped")).expect("outside directory");
    let outside = root.join("escaped/shapes.md");
    std::fs::write(&outside, "keep this file").expect("outside file");
    std::fs::write(root.join("fixture/wrela.toml"), common::manifest("../escaped"))
        .expect("write manifest");
    let out = export(&root, "out", &[]);
    assert_eq!(std::fs::read_to_string(outside).expect("outside file"), "keep this file");
    assert!(!out.status.success());
    assert!(!root.join("out").exists());
}

#[test]
fn the_export_reserves_spec_std_and_builtins_on_case_insensitive_filesystems() {
    let root = shapes("doc-export-spec-name");
    std::fs::write(
        root.join("fixture/lexical.wrela"),
        "/// A package page with the spec's path.\npub fn example() -> u32 { 1 }\n",
    )
    .expect("package page");
    for name in ["spec", "Spec", "std", "Std", "builtins", "Builtins"] {
        std::fs::write(root.join("fixture/wrela.toml"), common::manifest(name))
            .expect("package manifest");
        let out = export(&root, "out", &[]);
        assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(&format!("`{name}` is reserved")),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!root.join("out").exists());
    }
}

#[test]
fn the_export_rejects_package_names_that_only_differ_by_case() {
    let root = shapes("doc-export-case-collision");
    std::fs::write(root.join("fixture/wrela.toml"), common::manifest("Foo"))
        .expect("first manifest");
    std::fs::create_dir(root.join("other")).expect("second package");
    std::fs::write(root.join("other/wrela.toml"), common::manifest("foo"))
        .expect("second manifest");
    std::fs::write(root.join("other/shapes.wrela"), "pub fn sample() -> u32 { 1 }\n")
        .expect("second source");
    let out = export(&root, "out", &["other"]);
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("case-insensitive filesystem"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!root.join("out").exists());
}

/// A lookup reads the package as it is now, not an export: an edited doc comment shows at once.
#[test]
fn a_lookup_reads_the_package_as_it_is_now() {
    let root = shapes("doc-fresh");
    let dir = root.join("fixture");
    let d = dir.to_str().unwrap();
    let (ok, page, _) = doc(&["Disc::area", d]);
    assert!(ok && page.contains("Its area."), "{page}");
    let file = dir.join("shapes.wrela");
    let text =
        std::fs::read_to_string(&file).unwrap().replace("Its area.", "How much room it covers.");
    std::fs::write(&file, text).unwrap();
    let (ok, page, _) = doc(&["Disc::area", d]);
    assert!(ok && page.contains("How much room it covers."), "{page}");
}
