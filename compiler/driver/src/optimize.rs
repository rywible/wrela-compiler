//! An optimized build's WASM (`wrela build --optimize [size|speed]`): Binaryen's `wasm-opt`
//! makes the code smaller (`-Oz`) or faster (`-O3`), [`Level::DEFAULT`] unless the build says,
//! and the trap locations (`wrela_abi::lines`) go through it as a source map, so
//! a trap still says where it happened. The table leaves the module for a file of its own beside
//! it ([`wrela_abi::lines::SIDECAR`]), which a host reads only when a trap needs it.
//!
//! `wasm-opt` is the toolchain's own, beside the `wrela` executable in a release's `bin/`, which
//! must be exactly [`WASM_OPT_VERSION`]; else the one at `WRELA_WASM_OPT`, or on `PATH`, that
//! release or later. [`optimize_with`] runs it through a caller's runner: the tool host's
//! `toolchain` service runs it in a process group the host owns, and kills that group when the
//! request is cancelled, past its deadline, or when the tool ends (language.md §15.5). It may use
//! only the features the back end emits: plain SIMD (never relaxed,
//! [`wrela_wasm::check_no_relaxed_simd`]), threads, bulk memory, sign extension and multiple
//! results. It assumes traps happen, so it keeps every one, and isn't given fast math, so float
//! operations keep their IEEE results.

use std::path::PathBuf;
use std::process::Command;
use wrela_abi::lines::{self, Lines};

/// The Binaryen release a toolchain release ships: the oldest whose source maps keep segments
/// with no location.
pub const WASM_OPT_VERSION: u32 = 133;

/// What `wasm-opt` optimizes for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// `-Oz`: the smallest code.
    Size,
    /// `-O3`: the fastest, larger.
    Speed,
}

impl Level {
    /// What `--optimize` alone means, chosen by measurement (toolchain 0.6, §A15 of the
    /// world-class plan): `-O3` was 2.0% faster on the Last Green's CPU frame in Chrome (7
    /// pairs, inconclusive), under the 3% that would take it, at the same gzipped size; on the
    /// frozen herd and grazer it was within noise.
    pub const DEFAULT: Level = Level::Size;

    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "size" => Some(Level::Size),
            "speed" => Some(Level::Speed),
            _ => None,
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Level::Size => "-Oz",
            Level::Speed => "-O3",
        }
    }
}

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

/// `wasm`, optimized for `level`, without its lines table; and the table for its optimized
/// code, encoded.
pub fn optimize(wasm: &[u8], level: Level, relaxed: bool) -> Result<(Vec<u8>, Vec<u8>), String> {
    optimize_with(wasm, level, relaxed, &mut run_here)
}

/// [`optimize`], `wasm-opt` run by `run`.
pub fn optimize_with(
    wasm: &[u8],
    level: Level,
    relaxed: bool,
    run: &mut Runner,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let program = wasm_opt(run)?;
    let (bare, payload) = wrela_wasm::without_custom_section(wasm, lines::SECTION)?;
    let table = match payload {
        Some(p) => {
            Lines::decode(&p).map_err(|e| format!("its `{}` section: {e}", lines::SECTION))?
        }
        None => Lines::default(),
    };
    let scratch = crate::Scratch::new("wrela-optimize")?;
    let path = |name: &str| scratch.0.join(name);
    let write = |name: &str, bytes: &[u8]| {
        std::fs::write(path(name), bytes).map_err(|e| format!("can't write {name}: {e}"))
    };
    write("in.wasm", &bare)?;
    write("in.map", table.source_map()?.as_bytes())?;
    let mut args: Vec<std::ffi::OsString> = vec![path("in.wasm").into(), level.flag().into()];
    args.extend(FEATURES.iter().map(Into::into));
    // Only a relaxed module may hold relaxed SIMD: `wasm-opt` adds none to any other.
    if relaxed {
        args.push("--enable-relaxed-simd".into());
    }
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
    // A relaxed module keeps its relaxed SIMD where only `frame` reaches it, whatever
    // `wasm-opt` inlined or merged.
    if relaxed {
        wrela_wasm::check_relaxed_only_in(&optimized, wrela_abi::EXPORT_FRAME)?;
    } else {
        wrela_wasm::check_no_relaxed_simd(&optimized)?;
    }
    // Binaryen keeps a module's custom sections; the pair id must survive (`read_pair`).
    if wrela_abi::read_pair(&optimized) != wrela_abi::read_pair(wasm) {
        return Err("`wasm-opt` lost the module's pair id".into());
    }
    let map = String::from_utf8(read("out.map")?).map_err(|_| "the source map isn't UTF-8")?;
    let table = Lines::from_source_map(&map).map_err(|e| format!("`wasm-opt`'s {e}"))?;
    Ok((optimized, table.encode()))
}

/// The `wasm-opt` to run: the toolchain's own, beside this executable (a release's `bin/`),
/// which must be exactly [`WASM_OPT_VERSION`]; else `WRELA_WASM_OPT`'s, else the one on `PATH`,
/// either [`WASM_OPT_VERSION`] or later.
fn wasm_opt(run: &mut Runner) -> Result<PathBuf, String> {
    let bundled = std::env::current_exe()
        .and_then(|e| e.canonicalize())
        .ok()
        .and_then(|e| Some(e.parent()?.join("wasm-opt")))
        .filter(|p| p.is_file());
    let exact = bundled.is_some();
    let program = bundled.unwrap_or_else(|| {
        std::env::var_os("WRELA_WASM_OPT").map_or("wasm-opt".into(), PathBuf::from)
    });
    let missing = |why: String| {
        if exact {
            format!(
                "an optimized build needs the toolchain's own `wasm-opt`, Binaryen \
                 {WASM_OPT_VERSION}, at `{}`: {why}",
                program.display()
            )
        } else {
            format!(
                "an optimized build needs Binaryen's `wasm-opt` {WASM_OPT_VERSION} or later \
                 (on PATH, or at WRELA_WASM_OPT): {why}"
            )
        }
    };
    let out = run(&program, &["--version".into()]).map_err(missing)?;
    let text = String::from_utf8_lossy(&out.stdout);
    match version(&text) {
        Some(v) if accepts(v, exact) => Ok(program),
        _ => Err(missing(format!("it says `{}`", text.trim()))),
    }
}

/// Whether `wasm-opt` release `v` is used: exactly [`WASM_OPT_VERSION`] if it's the toolchain's
/// own (`exact`), and that or later if it's the machine's.
fn accepts(v: u32, exact: bool) -> bool {
    if exact { v == WASM_OPT_VERSION } else { v >= WASM_OPT_VERSION }
}

/// The release in `wasm-opt --version`'s `wasm-opt version 133 (version_133)`.
fn version(text: &str) -> Option<u32> {
    text.split_whitespace().skip_while(|w| *w != "version").nth(1)?.parse().ok()
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

    #[test]
    fn the_toolchain_s_own_wasm_opt_is_exactly_its_release_and_the_machine_s_is_that_or_later() {
        assert!(accepts(WASM_OPT_VERSION, true) && accepts(WASM_OPT_VERSION, false));
        assert!(!accepts(WASM_OPT_VERSION + 1, true) && accepts(WASM_OPT_VERSION + 1, false));
        assert!(!accepts(WASM_OPT_VERSION - 1, true) && !accepts(WASM_OPT_VERSION - 1, false));
    }
}
