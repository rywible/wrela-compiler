//! The docs site (docsite): its readers of JSON and Markdown, its layout, search and routes, and
//! a frame test that drives its search and find with a script of keys; and the site in Chrome,
//! read through its semantic tree.
#[test]
fn the_docs_site_passes_its_tests() {
    let count = super::tests_pass(&wrela_tests::repo_root().join("docsite"));
    assert!(count >= 14, "{count} tests");
}

/// In Chrome, over a small corpus (docsite/scripts/corpus), the docs are real links: the list of
/// pages opens a page, which names the tab, has its sections as an outline's links and its next
/// page as a link, which opens that page. No tree is refused.
#[test]
#[ignore = "needs the runtime"]
fn the_docs_in_chrome_are_links_an_outline_and_titles() {
    let (dir, rel) = wrela_tests::page("docsite", "docsite-chrome");
    let corpus = wrela_tests::repo_root().join("docsite/scripts/corpus");
    for file in wrela_tests::files_under(&corpus, &["json", "md"]) {
        let to = dir.join(file.strip_prefix(&corpus).expect("in the corpus"));
        std::fs::create_dir_all(to.parent().expect("a directory")).expect("mkdir");
        std::fs::copy(&file, &to).expect("copy");
    }
    let commands =
        std::fs::read_to_string(wrela_tests::repo_root().join("docsite/scripts/reader.json"))
            .expect("the commands");
    assert!(
        wrela_tests::run_page_with_devtools(
            &rel,
            "#test&frames=900&width=1400&height=900",
            &commands,
            300
        ),
        "the browser run failed; see {rel}/results/console.log"
    );
    let log = std::fs::read_to_string(dir.join("results/log.txt")).unwrap_or_default();
    assert!(!log.contains("isn't sent"), "{log}");
}
