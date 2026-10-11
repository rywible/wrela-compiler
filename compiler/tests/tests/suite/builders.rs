//! Small construction histories, including compile-time recipes and their real GPU fields.

#[test]
fn simulated_builders_keep_resources_access_and_field_agreement() {
    assert_eq!(super::tests_pass(&wrela_tests::repo_root().join("examples/builders")), 6);
}
