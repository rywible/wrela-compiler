//! The semantic tree in Chrome (compiler/tests/semantics): a person's clicks, typing and input
//! method reach the program through the host's real elements.

const FRAGMENT: &str = "#test&frames=600&width=128&height=128";

/// A click on the field's drawn rectangle gives it the keys; what's typed and composed there is
/// read back as the field's text (byte offsets), and the keys it doesn't use, such as Enter, reach
/// the program; a plain click on the link is the program's, once, and the page doesn't navigate.
#[test]
#[ignore = "needs the runtime"]
fn a_person_types_composes_and_clicks_through_the_semantic_tree() {
    let (_, rel) = wrela_tests::page("compiler/tests/semantics", "semantics");
    let script = wrela_tests::browser_script("compiler/tests/semantics", "PERSON");
    assert!(
        wrela_tests::run_page_with_script(&rel, FRAGMENT, &script, 300),
        "the browser run failed; see {rel}/results/console.log"
    );
    let log = std::fs::read_to_string(wrela_tests::repo_root().join(&rel).join("results/log.txt"))
        .expect("log.txt");
    let lines: Vec<&str> = log.lines().collect();
    let enter = format!("down {}", wrela_abi::input::key_code("Enter"));
    let want = [
        "assist 2 2",
        "text a 1",
        "text aé 3",
        "text aéかな 9 composing",
        "text aé仮名 9",
        enter.as_str(),
        "assist 1 0",
    ];
    // In order, with others between: the focus moving, say.
    let mut at = 0;
    for line in &lines {
        if at < want.len() && *line == want[at] {
            at += 1;
        }
    }
    assert_eq!(at, want.len(), "missing {:?} in {lines:?}", want.get(at));
    // The field's own keys stay its own, and the link is activated once.
    let a = format!("down {}", wrela_abi::input::key_code("KeyA"));
    assert!(!lines.contains(&a.as_str()), "{lines:?}");
    assert_eq!(lines.iter().filter(|l| **l == "assist 1 0").count(), 1, "{lines:?}");
}

/// The person's script waits for what the program printed: with one of those lines changed, the
/// run fails at that wait, saying what the program printed last.
#[test]
#[ignore = "needs the runtime"]
fn a_persons_script_waits_for_the_lines_it_names() {
    use wrela_host::chrome::script::{Step, Until};
    let (_, rel) = wrela_tests::page("compiler/tests/semantics", "semantics-wrong");
    let mut script = wrela_tests::browser_script("compiler/tests/semantics", "PERSON");
    let wrong = script.iter_mut().find_map(|s| match s {
        Step::Wait(Until::Printed(line), timeout) if line == "text aé 3" => {
            *timeout = std::time::Duration::from_secs(3);
            Some(line)
        }
        _ => None,
    });
    *wrong.expect("the script waits for the typed text") = "text aé 4".into();
    let failed =
        wrela_tests::run_page(&rel, FRAGMENT, 300, Some(&script)).expect_err("a wrong line");
    assert!(failed.contains("text aé 4") && failed.contains("text aé 3"), "{failed}");
}
