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

fn usage() -> ExitCode {
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
            let json = rest.iter().any(|a| a == "--json");
            let dirs: Vec<&String> = rest.iter().filter(|a| !a.starts_with("--")).collect();
            let [dir] = dirs.as_slice() else { return usage() };
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

/// Prints diagnostics, as JSON or for people. Returns whether there were errors.
pub(crate) fn report(out: &wrela_driver::CheckOutput, json: bool) -> bool {
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
    let compiler = wrela_driver::Compiler::new(dir);
    let out = compiler.check();
    if report(&out, json) { ExitCode::from(1) } else { ExitCode::SUCCESS }
}
