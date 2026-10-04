//! Helpers shared by the integration tests, beyond those of `wrela_grammar::testing`.

use std::path::PathBuf;
use wrela_grammar::testing::{files_under, repo_root};

/// Every `*.wrela` file in the repository (std, the test suites, the sketches, the examples,
/// the explanations and the agent tests' attempts), sorted, skipping build output.
pub fn wrela_files() -> Vec<PathBuf> {
    files_under(&repo_root(), &["wrela"])
}

/// The sample program of compiler/syntax/tests/suite/roundtrip.rs.
pub fn roundtrip_sample() -> String {
    let path = repo_root().join("compiler/syntax/tests/sample.wrela");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}
