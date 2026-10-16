//! The whiteboard (examples/board): its model's rules, and frame tests that drive it with
//! scripts of input as a person would.
#[test]
fn the_board_passes_its_tests() {
    let count = super::tests_pass(&wrela_tests::repo_root().join("examples/board"));
    assert!(count >= 17, "{count} tests");
}
