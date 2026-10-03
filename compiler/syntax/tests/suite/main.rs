//! Parser recovery and formatter round trips, in one binary.

mod recovery;
mod roundtrip;

use std::path::{Path, PathBuf};

/// The `.wrela` files under `dir`, in its subdirectories too, in a fixed order.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|e| e == "wrela") {
            out.push(p);
        }
    }
}
