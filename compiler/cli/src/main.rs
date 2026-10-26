//! `wrela`: the command-line compiler.
//!
//! ```text
//! wrela check <package-dir> [--json]      check a package; exit 1 if it has errors
//! wrela build <package-dir> [-o <dir>] [--debug] [--testing] [--optimize] [--lift <package>]... [--report data]
//!                                         check and build; writes <dir> (default
//!                                         <package>/build); --debug adds checks (§11);
//!                                         --testing keeps the `@testing` exports (§9);
//!                                         --optimize makes the WASM smaller with
//!                                         Binaryen, its trap locations in game.lines;
//!                                         --lift reads that package's literals from a
//!                                         table the program can change (§23)
//! wrela fmt <file-or-dir>... [--check]    format in place; --check only reports
//! wrela fix <package-dir> [--dry-run] [--json]
//!                                         make every fix a tool can make, and report each
//! wrela migrate <package-dir> [--dry-run] [--json]
//!                                         make every migration's fix (a form the language
//!                                         moved away from), and nothing else
//! wrela edit <package-dir> [--edits <file>] [--dry-run] [--json]
//!                                         write new values into literals (§23), from
//!                                         JSON edits; refuses a file that has changed
//! wrela explain <code>                    what a diagnostic code means, with a program that
//!                                         has it and the program fixed
//! wrela doc <item> [<package-dir>]        an item's signature, doc comment and examples
//! wrela pipelines <package-dir> [--json]  each GPU entry point's instantiations (§18.9)
//! wrela profile <package-dir> [--frames n] [--size WxH] [--input <script.json>] [--serial]
//!     [--json] [--baseline <report.json>] [--compare <build-dir>] where a frame's time goes on this machine: the GPU's
//!                                         by label, the CPU's and its phases (medians and
//!                                         95th percentiles), each pipeline's WGSL; with a
//!                                         baseline, how each time changed
//! wrela test <package-dir> [<filter>] [--json]
//!                                         run the package's `@test` functions (§13), or
//!                                         those whose names contain `filter`; exit 1 if one
//!                                         fails
//! wrela script <package-dir> <module>::<NAME> [-o <file>]
//!                                         a `std::input::Script` constant as the hosts'
//!                                         script format, for `wrela-host --input`
//! wrela primer [area]                     the language's rules, each with a program that
//!                                         keeps it and one that breaks it
//! wrela solve <package-dir> (--minimize <fn> | --spec) --free <file>:<lines>...
//!                                         choose literals that minimize a function, or that
//!                                         make the package's spec hold (§23)
//! wrela same <package-dir> --against <build-dir> [--frames n] [--within x] [--no-gpu] [--json]
//!                                         whether the package built now behaves as a saved
//!                                         build does: the same commands each frame, the
//!                                         same last frame within x, the same pipelines
//! wrela trace <package-dir> --watch <export>,...
//!                                         run frames and print what exports say after each
//! wrela bisect <package-dir> --until "<export> <op> <number>"
//!                                         the first frame at which a condition holds
//! wrela query <package-dir> [<query>...] [--json]
//!                                         types, callers, callees, impls, effects, borrows,
//!                                         instantiations and signatures, from one check;
//!                                         with errors, what's known, and what they make
//!                                         unsure
//! wrela context <item> [<package-dir>] [--budget n]
//!                                         an item's source and what's around it, in a budget,
//!                                         with errors too
//! wrela refactor <package-dir> <change> [--dry-run] [--json]
//!                                         rename, move, add-param, change-mode,
//!                                         extract-const, inline, extract-fn: checked
//!                                         before written, refused on a stale plan
//! wrela tool <package-dir> [--read NAME=DIR]... [--write NAME=DIR]... [--secret NAME]...
//!     [--timeout S] [--workers N] [--blob-budget B] [-- args...]
//!                                         build the package's tool and run it with those
//!                                         grants (§15.5); exit with the tool's status
//! wrela tool <name> [grants...] [-- args...]
//!                                         the tool the workspace declares as `name`
//!                                         (`[tools.<name>]`), with its grants, which the
//!                                         command line may only narrow
//! wrela suite [<check-name>] [--workspace <dir>]
//!                                         every check of the workspace: its members' format,
//!                                         its allowlist and its members' tests; or one
//!                                         (`fmt`, `allow`, `wrela::<package-path>::[<test>]`)
//! wrela toolchain                         which toolchain runs here, where it is and why
//! wrela --version                         this toolchain's release and source revision
//! ```
//!
//! In a workspace whose manifest pins another toolchain release (`toolchain = "X.Y.Z"`), or with
//! `WRELA_TOOLCHAIN` set, every command runs that toolchain instead (`pin`).
//!
//! Exit status: 0 success, 1 the program has errors (or `fmt --check` found unformatted
//! files, or a test failed), 2 a usage or I/O error.

// A faster allocator than macOS's: the compiler allocates a great deal.
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod build;
mod builtin_types;
mod doc;
mod edit;
mod explain;
mod export;
mod fix;
mod fmt;
mod migrate;
mod pin;
mod pipelines;
mod primer;
mod profile;
mod query;
mod refactor;
mod run;
mod same;
mod script;
mod solve;
mod suite;
mod test;
mod tool;
mod toolchain;
mod trace;

pub(crate) fn usage() -> ExitCode {
    let lines = [
        "wrela check <package-dir> [--json]",
        "wrela build <package-dir> [-o <out-dir>] [--debug] [--testing] [--optimize [size|speed]] [--relaxed-simd] [--lift <package>]... [--report data] [--json]",
        "wrela fmt <file-or-dir>... [--check]",
        "wrela fix <package-dir> [--dry-run] [--json]",
        "wrela migrate <package-dir> [--dry-run] [--json]",
        "wrela edit <package-dir> [--edits <file.json>] [--dry-run] [--json]",
        "wrela explain <code>",
        "wrela doc <item> [<package-dir>]",
        "wrela doc --export <dir> [--commit <sha>] [<package-dir>...]",
        "wrela pipelines <package-dir> [--json]",
        "wrela profile <package-dir> [--frames n] [--warmup n] [--workers n] [--size WxH] [--input <script.json>] [--scenario name] [--serial] [--worst n] [--chrome [--features none|<list>] [--checked]] [--optimize size|speed] [--relaxed-simd] [--json] [--baseline <report.json> | --compare <build-dir> [--pairs n]]",
        "wrela test <package-dir> [<filter>] [--exact] [--std] [--bless] [--json]",
        "wrela script <package-dir> <module>::<NAME> [-o <file>]",
        "wrela primer [area]",
        "wrela solve <package-dir> (--minimize <module>::<function> | --spec) --free <file>[:<lines>]... [--steps n] [--exact] [--write] [--json]",
        "wrela query <package-dir> [<query>...] [--json]",
        "wrela context <item> [<package-dir>] [--budget n]",
        "wrela refactor <package-dir> (rename <item> <new-name> | move <item> <module> | add-param <fn> \"<name>: <type>\" (--value <expr> | --default <expr>) | change-mode <fn> <param> <borrow|mut|take> | extract-const <file>:<line>:<col> <NAME> | inline <fn> | extract-fn <file>:<first>-<last> <name> | apply <plan.json>) [--dry-run] [--json]",
        "wrela trace <package-dir> --watch <export>[,<export>...] [--frames n] [--csv] [--fps f] [--size WxH] [--input <script.json>] [--release] [--no-gpu] [--workers N]",
        "wrela bisect <package-dir> --until \"<export> <op> <number>\" [--frames n] [--json] [--fps f] [--size WxH] [--input <script.json>] [--release] [--no-gpu] [--workers N]",
        "wrela run <package-dir> [--port N] [--lan] [--lift <package>]... [--debug]",
        "wrela same <package-dir> --against <build-dir> [--frames n] [--fps f] [--size WxH] [--input <script.json>] [--within x] [--no-gpu] [--json]",
        "wrela suite [fmt | allow | wrela::<package-path>::[<test-name>]] [--workspace <dir>]",
        "wrela tool (<package-dir> | <name>) [--read NAME=DIR]... [--write NAME=DIR]... [--secret NAME]... [--net HOST]... [--allow-run NAME[=PATH]]... [--browser] [--timeout S] [--workers N] [--blob-budget B] [-- args...]",
        "wrela toolchain",
        "wrela --version",
    ];
    eprintln!("usage:\n  {}\nqueries: {}", lines.join("\n  "), wrela_driver::query::KINDS);
    ExitCode::from(2)
}

fn main() -> ExitCode {
    // A driven build's process (`wrela tool --drive <parent>`), which the tool host starts: this
    // toolchain's, whatever a workspace pins.
    let mut args = std::env::args().skip(1);
    if let (Some(tool), Some(drive), Some(parent), None) =
        (args.next(), args.next(), args.next(), args.next())
        && tool == "tool"
        && drive == "--drive"
        && let Ok(parent) = parent.parse()
    {
        wrela_host::tool::drive_child(parent);
    }
    if let Some(status) = pin::redirect() {
        return status;
    }
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
        "migrate" => migrate::run(rest),
        "edit" => edit::run(rest),
        "explain" => explain::run(rest),
        "doc" if rest.iter().any(|a| a == "--export") => export::run(rest),
        "doc" => doc::run(rest),
        "pipelines" => pipelines::run(rest),
        "profile" => profile::run(rest),
        "test" => {
            // Each tool test runs in a process of this executable's (§13.2).
            if let Ok(me) = std::env::current_exe() {
                wrela_driver::tool_tests::set_runner(me);
            }
            // Ctrl-C ends the tool tests running, each with every process group it started,
            // then the process: a tool test's processes aren't in the terminal's group.
            let handled = ctrlc::set_handler(|| {
                wrela_driver::tool_tests::stop_all();
                std::process::exit(130);
            });
            if let Err(e) = handled {
                eprintln!(
                    "error: `wrela test` can't take Ctrl-C, so it couldn't end its tool tests: {e}"
                );
                return ExitCode::from(71);
            }
            test::run(rest)
        }
        "suite" => suite::run(rest),
        "run" => run::run(rest),
        "same" => same::run(rest),
        "tool" => tool::run(rest),
        "solve" => solve::run(rest),
        "script" => script::run(rest),
        "primer" => primer::run(rest),
        "query" => query::query(rest),
        "refactor" => refactor::run(rest),
        "context" => query::context(rest),
        "trace" => trace::trace(rest),
        "bisect" => trace::bisect(rest),
        "toolchain" => pin::run(rest),
        "--version" | "-V" if rest.is_empty() => {
            println!("{}", pin::version_line());
            ExitCode::SUCCESS
        }
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
    /// `--optimize [size|speed]`, which only `build` takes: the WASM through `wasm-opt`
    /// (`wrela_driver::optimize`), for size or speed (alone, `Level::DEFAULT`).
    pub optimize: Option<wrela_driver::optimize::Level>,
    /// `--report data`, which only `build` takes: what the build's constant data holds.
    pub report_data: bool,
    /// `--relaxed-simd`, which only `build` takes: a second module beside `game.wasm` whose
    /// presentation code uses relaxed SIMD (`wrela_driver::build_relaxed`).
    pub relaxed_simd: bool,
}

/// Reads the arguments of `check`, or of `build` (`takes_out`), and checks that the package is
/// a directory with a `main.wrela`. On an error, the message is printed and the exit status
/// returned.
pub(crate) fn package_args(args: &[String], takes_out: bool) -> Result<PackageArgs, ExitCode> {
    let (mut dir, mut out, mut json, mut debug, mut testing) = (None, None, false, false, false);
    let mut optimize = None;
    let mut report_data = false;
    let mut relaxed_simd = false;
    let mut lift = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--out" if takes_out => match it.next() {
                Some(o) => out = Some(PathBuf::from(o)),
                None => return Err(usage()),
            },
            "--json" => json = true,
            "--debug" if takes_out => debug = true,
            "--testing" if takes_out => testing = true,
            "--relaxed-simd" if takes_out => relaxed_simd = true,
            "--optimize" if takes_out => {
                use wrela_driver::optimize::Level;
                optimize = Some(match it.peek().and_then(|l| Level::parse(l)) {
                    Some(level) => {
                        it.next();
                        level
                    }
                    None => Level::DEFAULT,
                });
            }
            "--report" if takes_out => match it.next().map(String::as_str) {
                Some("data") => report_data = true,
                _ => return Err(usage()),
            },
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
    if relaxed_simd && (debug || testing || !lift.is_empty()) {
        eprintln!(
            "error: `--relaxed-simd` makes a release build: not with --debug, --testing or --lift"
        );
        return Err(ExitCode::from(2));
    }
    Ok(PackageArgs { dir, json, out, debug, testing, lift, optimize, report_data, relaxed_simd })
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

/// Checks a generated path below `<package>/build` without following a symbolic-link component.
pub(crate) fn generated_path(package: &Path, path: &Path) -> Result<(), String> {
    wrela_driver::ensure_no_symlink_path(&package.join("build"), path)
        .map_err(|e| format!("can't use `{}`: {e}", path.display()))
}

/// The package in `dir` built as `wrela build` builds it (its `game.wasm` through `wasm-opt`,
/// if `optimize` says so), into `<dir>/build/<name>`: where it is.
pub(crate) fn build_into(
    dir: &Path,
    name: &str,
    optimize: Option<wrela_driver::optimize::Level>,
    relaxed: bool,
) -> Result<PathBuf, String> {
    let built = dir.join("build").join(name);
    generated_path(dir, &built)?;
    let mut out = if relaxed { wrela_driver::build_relaxed(dir) } else { wrela_driver::build(dir) };
    if let Some(level) = optimize
        && !out.has_errors()
    {
        build::optimized(&mut out.files, level)?;
    }
    write_build(&out, &built)?;
    Ok(built)
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

/// The input script `--input` names, read and parsed; none without it.
pub(crate) fn read_script(input: Option<&Path>) -> Result<Vec<wrela_abi::input::Scripted>, String> {
    let Some(p) = input else { return Ok(Vec::new()) };
    let text =
        std::fs::read_to_string(p).map_err(|e| format!("can't read {}: {e}", p.display()))?;
    wrela_abi::input::parse_script(&text)
}

/// `bytes`' SHA-256, in lowercase hexadecimal.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
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

    #[cfg(unix)]
    #[test]
    fn generated_paths_reject_a_symlinked_build_parent() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!("wrela-cli-build-link-{}", std::process::id()));
        let outside =
            std::env::temp_dir().join(format!("wrela-cli-build-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&dir).expect("package");
        std::fs::create_dir_all(&outside).expect("outside");
        symlink(&outside, dir.join("build")).expect("symlink build");
        assert!(generated_path(&dir, &dir.join("build/trace")).is_err());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(outside);
    }
}
