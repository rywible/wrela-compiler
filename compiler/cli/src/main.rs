//! `wrela`: the command-line compiler.
//!
//! ```text
//! wrela check <package-dir> [--json]      check a package; exit 1 if it has errors
//! wrela build <package-dir> [-o <dir>]    check and build; writes <dir> (default <package>/build)
//! wrela fmt <file-or-dir>... [--check]    format in place; --check only reports
//! ```
//!
//! Exit status: 0 success, 1 the program has errors (or `fmt --check` found unformatted
//! files), 2 a usage or I/O error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod build;
mod fmt;

pub(crate) fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  wrela check <package-dir> [--json]\n  wrela build <package-dir> [-o <out-dir>] [--json]\n  wrela fmt <file-or-dir>... [--check]"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else { return usage() };
    let rest = &args[1..];
    match cmd.as_str() {
        "check" => match package_args(rest, false) {
            Ok(a) => check(&a.dir, a.json),
            Err(status) => status,
        },
        "build" => build::run(rest),
        "fmt" => fmt::run(rest),
        "--help" | "-h" | "help" => {
            usage();
            ExitCode::SUCCESS
        }
        _ => usage(),
    }
}

/// An argument the command doesn't take.
pub(crate) fn unknown(arg: &str) -> ExitCode {
    if arg.starts_with('-') {
        eprintln!("error: unknown option `{arg}`");
    } else {
        eprintln!("error: unexpected argument `{arg}`");
    }
    usage()
}

/// The arguments of `check` and `build`.
pub(crate) struct PackageArgs {
    /// The package: a directory.
    pub dir: PathBuf,
    pub json: bool,
    /// `-o <dir>`, which only `build` takes.
    pub out: Option<PathBuf>,
}

/// Reads the arguments of `check`, or of `build` (`takes_out`), and checks that the package is
/// a directory with a `main.wrela`. On an error, the message is printed and the exit status
/// returned.
pub(crate) fn package_args(args: &[String], takes_out: bool) -> Result<PackageArgs, ExitCode> {
    let (mut dir, mut out, mut json) = (None, None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--out" if takes_out => match it.next() {
                Some(o) => out = Some(PathBuf::from(o)),
                None => return Err(usage()),
            },
            "--json" => json = true,
            _ if dir.is_none() && !a.starts_with('-') => dir = Some(PathBuf::from(a)),
            _ => return Err(unknown(a)),
        }
    }
    let Some(dir) = dir else { return Err(usage()) };
    if !dir.is_dir() {
        eprintln!(
            "error: `{}` isn't a directory (a package is a directory of .wrela files)",
            dir.display()
        );
        return Err(ExitCode::from(2));
    }
    if !dir.join("main.wrela").is_file() {
        eprintln!("error: `{}` has no main.wrela, so it isn't a package", dir.display());
        return Err(ExitCode::from(2));
    }
    Ok(PackageArgs { dir, json, out })
}

/// Prints diagnostics, as JSON or for people. Returns whether there were errors.
pub(crate) fn report(out: &wrela_driver::Output, json: bool) -> bool {
    if json {
        print!("{}", wrela_diag::json::to_json_string(&out.sources, &out.diagnostics));
    } else {
        eprint!("{}", wrela_diag::render::render_all(&out.sources, &out.diagnostics));
    }
    out.has_errors()
}

fn check(dir: &Path, json: bool) -> ExitCode {
    let out = wrela_driver::check(dir);
    if report(&out, json) { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory without a `main.wrela` isn't a package: `check` refuses it as `build` does.
    #[test]
    fn a_package_needs_a_main() {
        let dir = std::env::temp_dir().join(format!("wrela-cli-nomain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let args = [dir.display().to_string()];
        assert!(package_args(&args, false).is_err());
        std::fs::write(dir.join("main.wrela"), "").expect("write");
        assert!(package_args(&args, false).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
