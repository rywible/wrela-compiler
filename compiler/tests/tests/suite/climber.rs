//! The climber's own fight, movement, cape, figure and sound regressions on small CPU fixtures.

use wrela_tests::repo_root;
/// The package's own tests (`wrela test examples/climber`: the fight's, the garment's, the
/// figure's, the drawing's, the sounds', the look's), on the CPU: about 2 s, so the gate runs
/// them.
#[test]
fn the_fights_own_tests_pass() {
    assert_eq!(super::tests_pass(&repo_root().join("examples/climber")), 29);
}
