//! The docs site in Chrome, over docs that the toolchain exports: `wrela doc --export` of
//! compiler/tests/docs-guide, a small package written for this (two modules, their items, code
//! and links), beside the language's spec and std, as the site serves them. The site's browser
//! script (docsite `scripts::READER`) searches for an item, opens its page from the results,
//! reads its outline, goes to the next page, copies its code and the page, and finds an item's
//! link to itself.

use crate::common::{package, wrela};
use std::path::{Path, PathBuf};

const FRAGMENT: &str = "#test&frames=900&width=1400&height=900";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A directory holding `page/`: the docs site's test build, with the guide's export beside it.
fn docs_page(name: &str) -> PathBuf {
    let root = package(name, &[]);
    let page = root.join("page");
    let built = wrela_driver::build_for_tests(&repo().join("docsite"), false);
    assert!(
        !built.has_errors(),
        "{}",
        wrela_diag::render::render_all(&built.sources, &built.diagnostics)
    );
    built.write_to(&page).expect("write the site");
    let export = root.join("export");
    let out = wrela()
        .args(["doc", "--export"])
        .arg(&export)
        .arg(repo().join("compiler/tests/docs-guide"))
        .output()
        .expect("run wrela doc");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    copy_into(&export, &page);
    root
}

/// Copies every file under `from` to the same path under `to`, none already there.
fn copy_into(from: &Path, to: &Path) {
    for e in std::fs::read_dir(from).expect("read").flatten() {
        let (src, dest) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            std::fs::create_dir_all(&dest).expect("mkdir");
            copy_into(&src, &dest);
        } else {
            assert!(!dest.exists(), "the export's {} would replace the site's", dest.display());
            std::fs::copy(&src, &dest).expect("copy");
        }
    }
}

fn reader() -> Vec<wrela_host::chrome::script::Step> {
    let out = wrela_driver::script::browser_script(&repo().join("docsite"), "READER");
    let json = out.script.unwrap_or_else(|why| panic!("{why}"));
    wrela_host::chrome::script::parse(&json).expect("the script")
}

fn run(root: &Path, script: &[wrela_host::chrome::script::Step]) -> Result<(), String> {
    wrela_host::pages::run(wrela_host::pages::Page {
        root,
        page: "page",
        fragment: FRAGMENT,
        timeout: std::time::Duration::from_secs(300),
        script: Some(script),
    })
}

/// In Chrome, the exported docs are real links: search finds an item, whose result opens its
/// page, which names the tab, has its items as an outline's links and its next page as a link,
/// which opens that page;
/// a copy button and the page's copy reach the clipboard; an item links to itself. No tree is
/// refused.
#[test]
#[ignore = "needs the runtime"]
fn the_exported_docs_in_chrome_are_links_an_outline_and_titles() {
    let root = docs_page("docs-chrome");
    run(&root, &reader()).unwrap_or_else(|why| {
        panic!("the browser run failed ({why}); see {}/page/results/console.log", root.display())
    });
    let log = std::fs::read_to_string(root.join("page/results/log.txt")).unwrap_or_default();
    assert!(!log.contains("isn't sent"), "{log}");
}

/// The reader script's expectations are checked: with the outline it waits for changed, the run
/// fails, saying what the page had.
#[test]
#[ignore = "needs the runtime"]
fn the_docs_scripts_outline_is_an_expectation_that_fails_when_wrong() {
    use wrela_host::chrome::script::{Step, Until};
    let root = docs_page("docs-chrome-wrong");
    let mut script = reader();
    let wrong = script.iter_mut().find_map(|s| match s {
        Step::Wait(Until::Texts(_, are), timeout) => {
            *timeout = std::time::Duration::from_secs(3);
            Some(are)
        }
        _ => None,
    });
    let are = wrong.expect("the script waits for the outline");
    are[1] = "wander".into();
    let failed = run(&root, &script).expect_err("a wrong outline");
    assert!(failed.contains("wander") && failed.contains("\"walk\""), "{failed}");
}
