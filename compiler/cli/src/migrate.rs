//! `wrela migrate <package-dir> [--dry-run] [--json]`: makes the fix of every migration in the
//! package, and nothing else. A migration is a warning for a form the language moved away from,
//! whose one fix a tool always makes (`wrela_diag::codes::MIGRATIONS`), so moving a workspace's
//! pin to a new toolchain is: move it, run `wrela migrate` on each member, and commit. It runs
//! as `wrela fix` does (`fix.rs`: rounds in memory, each changed file written once), choosing
//! only the migrations' fixes; `--dry-run` shows the diff and writes nothing.
//!
//! Exit status: 0 when no migration is left and the package has no errors, 1 when one is left
//! or it has errors, 2 when a file can't be written or the arguments are wrong.

use crate::fix::{Fixes, fix_with};
use std::process::ExitCode;

const MIGRATE: Fixes = Fixes {
    keep: |d| d.code.is_migration(),
    verb: "migrated",
    nothing: "nothing to migrate",
    choices: false,
};

pub fn run(args: &[String]) -> ExitCode {
    wrela_driver::on_compiler_thread(|| fix_with(args, &MIGRATE))
}
