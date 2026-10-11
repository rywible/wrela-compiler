//! Exact native Wrela selection for the one-check interface.
use super::common::{answer, package, wrela};

#[test]
fn exact_selection_rejects_substrings_missing_names_and_ambiguous_names() {
    let dir = package(
        "exact-tests",
        &[(
            "main.wrela",
            r#"
@test
fn alpha() { std::io::print("selected") }
@test
fn alphabet() { panic("must not run") }
"#,
        )],
    );
    let report =
        answer(wrela().args(["test", dir.to_str().unwrap(), "alpha", "--exact", "--json"]));
    assert_eq!(report["tests"].as_array().unwrap().len(), 1);
    assert_eq!(report["tests"][0]["name"], "alpha");
    assert_eq!(report["tests"][0]["printed"][0], "selected");
    for name in ["alph", "missing"] {
        let out = wrela().args(["test", dir.to_str().unwrap(), name, "--exact"]).output().unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("0 matches"));
        assert!(out.stdout.is_empty());
    }
    std::fs::write(dir.join("other.wrela"), "@test\nfn alpha() { panic(\"must not run\") }\n")
        .unwrap();
    let out = wrela().args(["test", dir.to_str().unwrap(), "alpha", "--exact"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("2 matches"));
    assert!(out.stdout.is_empty());
}

#[test]
fn exact_std_selection_executes_the_standard_library_test() {
    let dir = package("exact-std", &[("main.wrela", "")]);
    let report = answer(wrela().args([
        "test",
        dir.to_str().unwrap(),
        "equal_signed_zeros_hash_alike",
        "--exact",
        "--std",
        "--json",
    ]));
    assert_eq!(report["tests"].as_array().unwrap().len(), 1);
    assert_eq!(report["tests"][0]["name"], "equal_signed_zeros_hash_alike");
    assert_eq!(report["tests"][0]["passed"], true);
}
