//! An optimized build's WASM (`wrela build --optimize`): Binaryen's `wasm-opt -Oz` makes the
//! code smaller, and the trap locations (`wrela_abi::lines`) go through it as a source map, so
//! a trap still says where it happened. The table leaves the module for a file of its own beside
//! it ([`wrela_abi::lines::SIDECAR`]), which a host reads only when a trap needs it.
//!
//! `wasm-opt` is found on `PATH`, or at `WRELA_WASM_OPT`. [`optimize_with`] runs it through a
//! caller's runner: the tool host's `toolchain` service runs it in a process group the host owns,
//! and kills that group when the request is cancelled, past its deadline, or when the tool ends
//! (language.md §15.5). It may use only the features the
//! back end emits: plain SIMD (never relaxed, [`wrela_wasm::check_no_relaxed_simd`]), threads,
//! bulk memory, sign extension and multiple results. It assumes traps happen, so it keeps every
//! one, and isn't given fast math, so float operations keep their IEEE results.

use std::path::PathBuf;
use std::process::Command;
use wrela_abi::lines::{self, Lines};

/// The oldest Binaryen release whose source maps keep segments with no location.
pub const WASM_OPT_VERSION: u32 = 133;

/// The features `wasm-opt` may use: exactly those the back end emits.
const FEATURES: [&str; 6] = [
    "--mvp-features",
    "--enable-simd",
    "--enable-threads",
    "--enable-bulk-memory",
    "--enable-sign-ext",
    "--enable-multivalue",
];

/// How a program the optimizer ran ended: whether it succeeded, its status as text, and what it
/// wrote to its standard output and standard error.
pub struct Ran {
    pub success: bool,
    pub status: String,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// What runs `wasm-opt`: given the program and its arguments, how it ended, or why it couldn't
/// run (or was stopped).
pub type Runner<'a> =
    dyn FnMut(&std::path::Path, &[std::ffi::OsString]) -> Result<Ran, String> + 'a;

/// Runs `program` with `args` as a child of this process, waiting for it.
fn run_here(program: &std::path::Path, args: &[std::ffi::OsString]) -> Result<Ran, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("can't run `{}`: {e}", program.display()))?;
    Ok(Ran {
        success: out.status.success(),
        status: out.status.to_string(),
        stdout: out.stdout,
        stderr: out.stderr,
    })
}

/// `wasm`, optimized, without its lines table; and the table for its optimized code, encoded.
pub fn optimize(wasm: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    optimize_with(wasm, &mut run_here)
}

/// [`optimize`], `wasm-opt` run by `run`.
pub fn optimize_with(wasm: &[u8], run: &mut Runner) -> Result<(Vec<u8>, Vec<u8>), String> {
    let program = wasm_opt(run)?;
    let (bare, payload) = wrela_wasm::without_custom_section(wasm, lines::SECTION)?;
    let table = match payload {
        Some(p) => {
            Lines::decode(&p).map_err(|e| format!("its `{}` section: {e}", lines::SECTION))?
        }
        None => Lines::default(),
    };
    let scratch = Scratch::new()?;
    let path = |name: &str| scratch.0.join(name);
    let write = |name: &str, bytes: &[u8]| {
        std::fs::write(path(name), bytes).map_err(|e| format!("can't write {name}: {e}"))
    };
    write("in.wasm", &bare)?;
    write("in.map", table.source_map()?.as_bytes())?;
    let mut args: Vec<std::ffi::OsString> = vec![path("in.wasm").into(), "-Oz".into()];
    args.extend(FEATURES.iter().map(Into::into));
    for (flag, name) in
        [("--input-source-map", "in.map"), ("--output-source-map", "out.map"), ("-o", "out.wasm")]
    {
        args.push(flag.into());
        args.push(path(name).into());
    }
    let out = run(&program, &args)?;
    if !out.success {
        return Err(format!(
            "`wasm-opt` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let read =
        |name: &str| std::fs::read(path(name)).map_err(|e| format!("can't read {name}: {e}"));
    let optimized = read("out.wasm")?;
    wrela_wasm::check_no_relaxed_simd(&optimized)?;
    // Binaryen keeps a module's custom sections; the pair id must survive (`read_pair`).
    if wrela_abi::read_pair(&optimized) != wrela_abi::read_pair(wasm) {
        return Err("`wasm-opt` lost the module's pair id".into());
    }
    let map = String::from_utf8(read("out.map")?).map_err(|_| "the source map isn't UTF-8")?;
    let table = Lines::from_source_map(&map).map_err(|e| format!("`wasm-opt`'s {e}"))?;
    Ok((optimized, table.encode()))
}

/// The `wasm-opt` to run, checked to be [`WASM_OPT_VERSION`] or later.
fn wasm_opt(run: &mut Runner) -> Result<PathBuf, String> {
    let program = std::env::var_os("WRELA_WASM_OPT").map_or("wasm-opt".into(), PathBuf::from);
    let missing = |why: String| {
        format!(
            "an optimized build needs Binaryen's `wasm-opt` {WASM_OPT_VERSION} or later \
             (on PATH, or at WRELA_WASM_OPT): {why}"
        )
    };
    let out = run(&program, &["--version".into()]).map_err(missing)?;
    let text = String::from_utf8_lossy(&out.stdout);
    match version(&text) {
        Some(v) if v >= WASM_OPT_VERSION => Ok(program),
        _ => Err(missing(format!("it says `{}`", text.trim()))),
    }
}

/// The release in `wasm-opt --version`'s `wasm-opt version 133 (version_133)`.
fn version(text: &str) -> Option<u32> {
    text.split_whitespace().skip_while(|w| *w != "version").nth(1)?.parse().ok()
}

/// A directory of the optimizer's files, removed when done.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Scratch, String> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("wrela-optimize-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("can't make {}: {e}", dir.display()))?;
        Ok(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_is_read_from_its_version() {
        assert_eq!(version("wasm-opt version 133 (version_133)\n"), Some(133));
        assert_eq!(version("wasm-opt version 99\n"), Some(99));
        assert_eq!(version("something else"), None);
    }
}
