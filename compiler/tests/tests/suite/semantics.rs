//! The semantic tree in Chrome (compiler/tests/semantics): a person's clicks, typing and input
//! method reach the program through the host's real elements.

/// A click on the field's drawn rectangle gives it the keys; what's typed and composed there is
/// read back as the field's text (byte offsets), and the keys it doesn't use, such as Enter, reach
/// the program; a plain click on the link is the program's, once, and the page doesn't navigate.
#[test]
#[ignore = "needs the runtime"]
fn a_person_types_composes_and_clicks_through_the_semantic_tree() {
    let (_, rel) = wrela_tests::page("compiler/tests/semantics", "semantics");
    let commands = std::fs::read_to_string(
        wrela_tests::repo_root().join("compiler/tests/semantics/person.json"),
    )
    .expect("the commands");
    assert!(
        wrela_tests::run_page_with_devtools(
            &rel,
            "#test&frames=600&width=128&height=128",
            &commands,
            300
        ),
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
