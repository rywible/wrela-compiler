//! Parser recovery and formatter round trips, in one binary.

mod recovery;
mod roundtrip;

use std::path::{Path, PathBuf};

/// The `.wrela` files under `dir`, in its subdirectories too, in a fixed order. A `build`
/// directory is skipped: what a build writes there (a lens's copies of the studio's sources)
/// isn't the repository's, and the gate writes it while these tests read.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() && p.file_name().is_some_and(|n| n == "build") {
            continue;
        }
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|e| e == "wrela") {
            out.push(p);
        }
    }
}
