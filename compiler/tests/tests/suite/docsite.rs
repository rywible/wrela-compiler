//! The docs site (docsite): its readers of JSON and Markdown, its layout, search and routes, and
//! a frame test that drives its search and find with a script of keys. The site in Chrome, over
//! docs the toolchain exports, is the command's test (compiler/cli/tests/suite/docs_chrome.rs).
#[test]
fn the_docs_site_passes_its_tests() {
    let count = super::tests_pass(&wrela_tests::repo_root().join("docsite"));
    assert!(count >= 14, "{count} tests");
}
