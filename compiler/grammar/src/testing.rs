//! Helpers for tests: this crate's, and the end-to-end tests of `wrela-tests`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The repository's root.
pub fn repo_root() -> PathBuf {
    // The test runner sets this for its checkout. A shared Cargo cache can retain a
    // different checkout's manifest path in the compiled test executable.
    let runtime = std::env::var_os("CARGO_MANIFEST_DIR");
    repo_root_from(runtime.as_deref().map(Path::new), Path::new(env!("CARGO_MANIFEST_DIR")))
        .expect("the test manifest must be inside the Wrela repository")
}

fn repo_root_from(runtime: Option<&Path>, compiled: &Path) -> Option<PathBuf> {
    runtime
        .unwrap_or(compiled)
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file() && dir.join("spec/grammar.ebnf").is_file())
        .map(Path::to_path_buf)
}

/// The stack of each thread [`par_map_with`] and [`with_big_stack`] start: the oracle's
/// derivation counting recurses once per nested rule and repetition, which can be deep for whole
/// files.
pub const BIG_STACK: usize = 256 << 20;

/// `f` of each item, in the items' order, spread over the machine's threads (each item once),
/// each with a [`BIG_STACK`] and its own state from `init`. A panic in `f` panics here, with the
/// same payload.
pub fn par_map_with<T: Sync, S, R: Send>(
    items: &[T],
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, &T) -> R + Sync,
) -> Vec<R> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    let budget = std::env::var("WRELA_CPU_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(cores);
    let threads = cores.min(budget).min(items.len());
    let mut done: Vec<(usize, R)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                std::thread::Builder::new()
                    .stack_size(BIG_STACK)
                    .spawn_scoped(s, || {
                        let (mut state, mut mine) = (init(), Vec::new());
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(item) = items.get(i) else { break };
                            mine.push((i, f(&mut state, item)));
                        }
                        mine
                    })
                    .expect("spawning a test thread")
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

/// [`par_map_with`], with no state: for a test that checks many independent cases.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    par_map_with(items, || (), |_, item| f(item))
}

/// Calls `f` on each item, as [`par_map_with`] does.
pub fn par_each<T: Sync, S>(
    items: &[T],
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, &T) + Sync,
) {
    par_map_with(items, init, f);
}

/// Runs `f` on a thread with a [`BIG_STACK`].
pub fn with_big_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(BIG_STACK)
            .spawn_scoped(s, f)
            .expect("spawning a test thread")
            .join()
            .unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

/// Build output directories, which [`files_under`] skips.
const BUILD_DIRS: [&str; 3] = ["build", "target", "node_modules"];

/// The files under `dir`, at any depth, whose extension is one of `exts` (every file if `exts`
/// is empty), sorted. Directories whose name starts with `.` are skipped, and so is build
/// output: `build`, `target` and `node_modules`.
pub fn files_under(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    fn walk(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let (p, name) = (entry.path(), entry.file_name());
            if p.is_dir() {
                let hidden = name.to_string_lossy().starts_with('.');
                if !hidden && !BUILD_DIRS.iter().any(|d| name == *d) {
                    walk(&p, exts, out);
                }
            } else if exts.is_empty() || exts.iter().any(|x| p.extension() == Some(OsStr::new(x))) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, exts, &mut out);
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_root_uses_the_running_checkout_and_rejects_an_unrelated_manifest() {
        let dir = std::env::temp_dir().join(format!("wrela-repo-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let compiled = dir.join("compiled");
        let runtime = dir.join("runtime");
        for root in [&compiled, &runtime] {
            std::fs::create_dir_all(root.join("spec")).expect("spec directory");
            std::fs::create_dir_all(root.join("compiler/tests")).expect("test package");
            std::fs::write(root.join("Cargo.toml"), "[workspace]").expect("workspace manifest");
            std::fs::write(root.join("compiler/tests/Cargo.toml"), "[package]")
                .expect("package manifest");
            std::fs::write(root.join("spec/grammar.ebnf"), "").expect("grammar marker");
        }
        let compiled_manifest = compiled.join("compiler/tests");
        assert_eq!(
            repo_root_from(Some(&runtime.join("compiler/tests")), &compiled_manifest),
            Some(runtime.clone())
        );
        assert_eq!(repo_root_from(None, &compiled_manifest), Some(compiled));
        // An invalid runtime path must not silently redirect a run to the old checkout.
        assert_eq!(repo_root_from(Some(&dir), &compiled_manifest), None);
        std::fs::remove_dir_all(dir).expect("remove fixture");
    }

    #[test]
    fn files_under_skips_hidden_directories_and_build_output() {
        let dir = std::env::temp_dir().join(format!("wrela-files-under-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for f in [
            "b.wrela",
            "a/c.wrela",
            "a/c.rs",
            "a/.hidden/d.wrela",
            "build/e.wrela",
            "x/target/f.wrela",
            "node_modules/g.wrela",
        ] {
            let path = dir.join(f);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
            std::fs::write(&path, "").expect("write");
        }
        let rel = |files: Vec<PathBuf>| -> Vec<String> {
            files
                .iter()
                .map(|f| f.strip_prefix(&dir).expect("inside").display().to_string())
                .collect()
        };
        assert_eq!(rel(files_under(&dir, &["wrela"])), ["a/c.wrela", "b.wrela"]);
        assert_eq!(rel(files_under(&dir, &[])), ["a/c.rs", "a/c.wrela", "b.wrela"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
