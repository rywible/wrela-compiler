//! Console policy is Wrela; these small fixtures exercise data, input and save validation.
#[test]
fn console_and_platform_policy_tests_pass() {
    for package in ["console", "platform", "examples/last-green/session", "ui"] {
        assert!(super::tests_pass(&wrela_tests::repo_root().join(package)) > 0, "{package}");
    }
}
