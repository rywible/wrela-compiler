//! `wrela`: the command-line compiler.
//!
//! ```text
//! wrela check <package-dir> [--json]      check a package; exit 1 if it has errors
//! wrela build <package-dir> [-o <dir>] [--debug] [--testing] [--lift <package>]...
//!                                         check and build; writes <dir> (default
//!                                         <package>/build); --debug adds checks (§11);
//!                                         --testing keeps the `@testing` exports (§9);
//!                                         --lift reads that package's literals from a
//!                                         table the program can change (§22)
//! wrela fmt <file-or-dir>... [--check]    format in place; --check only reports
//! wrela fix <package-dir> [--json]        make every fix a tool can make, and report each
//! wrela edit <package-dir> [--edits <file>] [--dry-run] [--json]
//!                                         write new values into literals (§22), from
//!                                         JSON edits; refuses a file that has changed
//! wrela explain <code>                    what a diagnostic code means, with a program that
//!                                         has it and the program fixed
//! wrela doc <item> [<package-dir>]        an item's signature, doc comment and examples
//! wrela pipelines <package-dir> [--json]  each GPU entry point's instantiations (§7)
//! wrela test <package-dir> [<filter>] [--json]
//!                                         run the package's `@test` functions (§10), or
//!                                         those whose names contain `filter`; exit 1 if one
//!                                         fails
//! wrela primer [area]                     the language's rules, each with a program that
//!                                         keeps it and one that breaks it
//! wrela solve <package-dir> (--minimize <fn> | --spec) --free <file>:<lines>...
//!                                         choose literals that minimize a function, or that
//!                                         make the package's spec hold (§22)
//! wrela trace <package-dir> --watch <export>,...
//!                                         run frames and print what exports say after each
//! wrela bisect <package-dir> --until "<export> <op> <number>"
//!                                         the first frame at which a condition holds
//! wrela query <package-dir> [<query>...] [--json]
//!                                         types, callers, callees, impls, effects, borrows,
//!                                         instantiations and signatures, from one check
//! wrela context <item> [<package-dir>] [--budget n]
//!                                         an item's source and what's around it, in a budget
//! wrela refactor <package-dir> <change> [--dry-run] [--json]
//!                                         rename, move, add-param, change-mode: checked
//!                                         before written, refused on a stale plan
//! wrela studio <package-dir> [...]        the lens (§22)
//! wrela reference replica <parts> -o <dir> [--engine <dir>] [--harmonics k]
//!     [--sections-every m] [--seam m]     a reference mesh as a package of lofts, maybe
//!                                         compressed
//! wrela reference deviation <parts> <package-dir> [--points n] [--json]
//!                                         how far a package's surface is from the mesh's
//! ```
//!
//! Exit status: 0 success, 1 the program has errors (or `fmt --check` found unformatted
//! files, or a test failed), 2 a usage or I/O error.

// A faster allocator than macOS's: the compiler allocates a great deal.
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod build;
mod compare;
mod doc;
mod edit;
mod explain;
mod fix;
mod fmt;
mod mesh;
mod pipelines;
mod primer;
mod query;
mod refactor;
mod reference;
mod run;
mod solve;
mod studio;
mod sweeps;
mod test;
mod trace;

pub(crate) fn usage() -> ExitCode {
    let lines = [
        "wrela check <package-dir> [--json]",
        "wrela build <package-dir> [-o <out-dir>] [--debug] [--testing] [--lift <package>]... [--json]",
        "wrela fmt <file-or-dir>... [--check]",
        "wrela fix <package-dir> [--json]",
        "wrela edit <package-dir> [--edits <file.json>] [--dry-run] [--json]",
        "wrela explain <code>",
        "wrela doc <item> [<package-dir>]",
        "wrela pipelines <package-dir> [--json]",
        "wrela test <package-dir> [<filter>] [--json]",
        "wrela primer [area]",
        "wrela solve <package-dir> (--minimize <module>::<function> | --spec) --free <file>[:<lines>]... [--steps n] [--exact] [--write] [--json]",
        "wrela query <package-dir> [<query>...] [--json]",
        "wrela context <item> [<package-dir>] [--budget n]",
        "wrela refactor <package-dir> (rename <item> <new-name> | move <item> <module> | add-param <fn> \"<name>: <type>\" (--value <expr> | --default <expr>) | change-mode <fn> <param> <borrow|mut|take> | apply <plan.json>) [--dry-run] [--json]",
        "wrela trace <package-dir> --watch <export>[,<export>...] [--frames n] [--csv] [--fps f] [--size WxH] [--input <script.json>] [--release]",
        "wrela bisect <package-dir> --until \"<export> <op> <number>\" [--frames n] [--json] [--fps f] [--size WxH] [--input <script.json>] [--release]",
        "wrela reference replica <parts.toml> -o <out-dir> [--engine <dir>] [--harmonics k] [--sections-every m] [--seam m]",
        "wrela reference deviation <parts.toml> <package-dir> [--points n] [--json]",
        "wrela run <package-dir> [--port N] [--lift <package>]... [--debug]",
        "wrela studio <package-dir> [serve [--port N] | build | run <script> | look | beside <manifest> | variants <package-dir>... | sweep <literal> <value>... | <action> [args...]] [--png FILE] [--view F] [--size WxH] [--reference PNG] [--debug]",
    ];
    eprintln!("usage:\n  {}\nqueries: {}", lines.join("\n  "), wrela_driver::query::KINDS);
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
        "fix" => fix::run(rest),
        "edit" => edit::run(rest),
        "explain" => explain::run(rest),
        "doc" => doc::run(rest),
        "pipelines" => pipelines::run(rest),
        "test" => test::run(rest),
        "studio" => studio::run(rest),
        "run" => run::run(rest),
        "reference" => reference::run(rest),
        "solve" => solve::run(rest),
        "primer" => primer::run(rest),
        "query" => query::query(rest),
        "refactor" => refactor::run(rest),
        "context" => query::context(rest),
        "trace" => trace::trace(rest),
        "bisect" => trace::bisect(rest),
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
    /// `--debug`, which only `build` takes.
    pub debug: bool,
    /// `--testing`, which only `build` takes: a test build, with the program's `@testing`
    /// exports (§9).
    pub testing: bool,
    /// `--lift <package>`, which only `build` takes: the packages whose literals a lifted
    /// build lifts.
    pub lift: Vec<String>,
}

/// Reads the arguments of `check`, or of `build` (`takes_out`), and checks that the package is
/// a directory with a `main.wrela`. On an error, the message is printed and the exit status
/// returned.
pub(crate) fn package_args(args: &[String], takes_out: bool) -> Result<PackageArgs, ExitCode> {
    let (mut dir, mut out, mut json, mut debug, mut testing) = (None, None, false, false, false);
    let mut lift = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--out" if takes_out => match it.next() {
                Some(o) => out = Some(PathBuf::from(o)),
                None => return Err(usage()),
            },
            "--json" => json = true,
            "--debug" if takes_out => debug = true,
            "--testing" if takes_out => testing = true,
            "--lift" if takes_out => match it.next() {
                Some(p) => lift.extend(p.split(',').map(String::from)),
                None => return Err(usage()),
            },
            _ if dir.is_none() && !a.starts_with('-') => dir = Some(PathBuf::from(a)),
            _ => return Err(unknown(a)),
        }
    }
    let Some(dir) = dir else { return Err(usage()) };
    package_dir(&dir)?;
    // A program has `main.wrela`; a library (a dependency, like `engine`) has a `wrela.toml`
    // or modules of its own, and can be checked and tested but not built: it exports nothing.
    let is_library = dir.join("wrela.toml").is_file() || has_module(&dir);
    if !dir.join("main.wrela").is_file() && (takes_out || !is_library) {
        eprintln!(
            "error: `{}` has no main.wrela, so it isn't {}",
            dir.display(),
            if is_library {
                "a program (a library can be checked and tested, not built)"
            } else {
                "a package"
            }
        );
        return Err(ExitCode::from(2));
    }
    Ok(PackageArgs { dir, json, out, debug, testing, lift })
}

/// Checks that the package `dir` is a directory. If it isn't, the message is printed and the
/// exit status returned.
pub(crate) fn package_dir(dir: &Path) -> Result<(), ExitCode> {
    if dir.is_dir() {
        return Ok(());
    }
    eprintln!(
        "error: `{}` isn't a directory (a package is a directory of .wrela files)",
        dir.display()
    );
    Err(ExitCode::from(2))
}

/// Whether `dir` holds a `.wrela` file.
fn has_module(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|e| e.path().extension().is_some_and(|x| x == "wrela"))
    })
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

/// Writes a build to `to`; if it has errors, fails with them, rendered.
pub(crate) fn write_build(out: &wrela_driver::Output, to: &Path) -> Result<(), String> {
    if out.has_errors() {
        return Err(wrela_diag::render::render_all(&out.sources, &out.diagnostics));
    }
    out.write_to(to).map_err(|e| e.to_string())
}

/// The last line of `logs` that starts `{`: a program's JSON answer.
pub(crate) fn last_json(logs: &[String]) -> Option<&str> {
    logs.iter().rev().find(|l| l.starts_with('{')).map(String::as_str)
}

/// `n` things: `1 <one>`, or `<n> <many>`.
pub(crate) fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// `WxH`, as `--size` takes it.
pub(crate) fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn check(dir: &Path, json: bool) -> ExitCode {
    let out = wrela_driver::check(dir);
    if report(&out, json) { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory with no `.wrela` file isn't a package; one with modules but no `main.wrela`
    /// is a library, which `check` takes and `build` refuses.
    #[test]
    fn a_package_needs_modules_and_a_program_needs_a_main() {
        let dir = std::env::temp_dir().join(format!("wrela-cli-nomain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let args = [dir.display().to_string()];
        assert!(package_args(&args, false).is_err());
        std::fs::write(dir.join("shapes.wrela"), "").expect("write");
        assert!(package_args(&args, false).is_ok(), "a library is checked");
        assert!(package_args(&args, true).is_err(), "a library isn't built");
        std::fs::write(dir.join("main.wrela"), "").expect("write");
        assert!(package_args(&args, true).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
