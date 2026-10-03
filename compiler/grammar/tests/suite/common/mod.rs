//! Helpers shared by the integration tests (each test module uses some of them).
#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `*.wrela` file under `compiler/` (the conformance suite in compiler/tests, and the
/// standard library), sorted, skipping build output.
pub fn wrela_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            if p.is_dir() {
                if name != "target" && !name.starts_with('.') {
                    walk(&p, out);
                }
            } else if name.ends_with(".wrela") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(&repo_root().join("compiler"), &mut out);
    out.sort();
    out
}

/// The sample program of compiler/syntax/tests/roundtrip.rs.
pub fn roundtrip_sample() -> String {
    let path = repo_root().join("compiler/syntax/tests/sample.wrela");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// `small` normally, `full` when `WRELA_FULL` is set (as `tools/check.sh` does).
pub fn sized<T>(small: T, full: T) -> T {
    if std::env::var_os("WRELA_FULL").is_some() { full } else { small }
}

/// Calls `f` on each item, spread over the machine's threads, each with a big stack (see
/// [`with_big_stack`]) and its own state from `init`.
pub fn par_each<T: Sync, S>(
    items: &[T],
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, &T) + Sync,
) {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(items.len());
    std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                std::thread::Builder::new()
                    .stack_size(256 << 20)
                    .spawn_scoped(s, || {
                        let mut state = init();
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(item) = items.get(i) else { break };
                            f(&mut state, item);
                        }
                    })
                    .expect("spawning a test thread")
            })
            .collect();
        for w in workers {
            w.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
        }
    });
}

/// Runs `f` on a thread with a big stack: the oracle's derivation counting recurses once per
/// nested rule and repetition, which can be deep for whole files.
pub fn with_big_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn_scoped(s, f)
            .expect("spawning a test thread")
            .join()
            .unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}
