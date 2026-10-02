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

use std::path::Path;
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
        "check" => {
            let mut json = false;
            let mut dir = None;
            for a in rest {
                match a.as_str() {
                    "--json" => json = true,
                    _ if dir.is_none() && !a.starts_with('-') => dir = Some(a),
                    _ => return unknown(a),
                }
            }
            let Some(dir) = dir else { return usage() };
            check(Path::new(dir), json)
        }
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
    if !dir.is_dir() {
        eprintln!(
            "error: `{}` isn't a directory (a package is a directory of .wrela files)",
            dir.display()
        );
        return ExitCode::from(2);
    }
    let out = wrela_driver::check(dir);
    if report(&out, json) { ExitCode::from(1) } else { ExitCode::SUCCESS }
}
