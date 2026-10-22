//! `wrela fmt`: formats files in place, or with `--check` reports which aren't formatted.
//! A file with syntax errors is left alone and reported.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wrela_diag::{FileId, SourceMap};

/// The `.wrela` files under `path`. A package (a directory with a `main.wrela`) contributes
/// exactly the files `wrela check` reads; other directories are searched for packages and
/// files, skipping hidden entries and symbolic links. Returns whether the layout was readable.
fn collect(path: &Path, out: &mut Vec<PathBuf>) -> bool {
    let mut errors = Vec::new();
    collect_in(&wrela_driver::reach::Reach::anywhere(), path, out, &mut errors);
    for e in &errors {
        eprintln!("error: {e}");
    }
    errors.is_empty()
}

/// [`collect`], reading only what `reach` may, with what's wrong put in `errors`.
pub(crate) fn collect_in(
    reach: &wrela_driver::reach::Reach,
    path: &Path,
    out: &mut Vec<PathBuf>,
    errors: &mut Vec<String>,
) {
    use wrela_driver::reach::Kind;
    // A path that can't be read is an error, never a path with nothing to format.
    let kind = match reach.kind(path, true) {
        Ok(Some(kind)) => kind,
        Ok(None) => {
            errors.push(format!("`{}` isn't there", path.display()));
            return;
        }
        Err(e) => {
            errors.push(format!("can't read `{}`: {e}", path.display()));
            return;
        }
    };
    if kind != Kind::Dir {
        if path.extension().is_some_and(|x| x == "wrela") {
            // A pipe or a device would block or never end when read.
            if kind != Kind::File {
                errors.push(format!("`{}` isn't a regular file", path.display()));
                return;
            }
            out.push(path.to_path_buf());
        }
        return;
    }
    if reach.is_file(&path.join("main.wrela")) {
        match wrela_driver::package::find_files_in(reach, path) {
            Ok(files) => out.extend(files.into_iter().map(|f| f.path)),
            Err(errs) => {
                for e in errs {
                    errors.push(format!("{}: {}", path.join(&e.path).display(), e.message));
                }
            }
        }
        return;
    }
    let entries = match reach.entries(path) {
        Ok(e) => e,
        Err(e) => {
            // Its files can't be checked: that's no pass.
            errors.push(format!("can't read `{}`: {e}", path.display()));
            return;
        }
    };
    let mut entries: Vec<(PathBuf, Kind)> =
        entries.into_iter().map(|(name, kind)| (path.join(name), kind)).collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (e, kind) in entries {
        let hidden = e.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
        if hidden || kind == Kind::Link {
            continue;
        }
        collect_in(reach, &e, out, errors);
    }
}

/// Formats on a thread with the compiler's stack: the parser and the formatter recurse over
/// syntax trees, and a file can hold the deepest tree the parser accepts (or rejects as too deep).
pub fn run(args: &[String]) -> ExitCode {
    wrela_driver::on_compiler_thread(|| format_files(args))
}

/// `text` formatted, or `None` when it has syntax errors. Call it on a thread with the
/// compiler's stack, as [`run`] does.
pub(crate) fn format_text(text: &str) -> Option<String> {
    parse_and_format(text).ok()
}

/// `text` formatted, or its syntax errors rendered as in the file `name`. Call it on a thread
/// with the compiler's stack, as [`run`] does.
pub(crate) fn formatted(name: &str, text: &str) -> Result<String, String> {
    parse_and_format(text).map_err(|diagnostics| {
        let mut map = SourceMap::new();
        map.add(name, text);
        wrela_diag::render::render_all(&map, &diagnostics)
    })
}

/// `text` formatted, or its diagnostics when it has syntax errors.
fn parse_and_format(text: &str) -> Result<String, Vec<wrela_diag::Diagnostic>> {
    let parsed = wrela_syntax::parse(FileId(0), text);
    if parsed.has_errors() {
        return Err(parsed.diagnostics);
    }
    Ok(wrela_syntax::fmt::format(&parsed, text))
}

fn format_files(args: &[String]) -> ExitCode {
    let mut check_only = false;
    let mut files = Vec::new();
    let mut layout_ok = true;
    for a in args {
        if a == "--check" {
            check_only = true;
            continue;
        }
        if a.starts_with('-') {
            return crate::unknown(a);
        }
        let p = Path::new(a);
        if !p.exists() {
            eprintln!("error: `{a}` doesn't exist");
            return ExitCode::from(2);
        }
        layout_ok &= collect(p, &mut files);
    }
    if !layout_ok {
        return ExitCode::from(2);
    }
    if files.is_empty() {
        eprintln!("usage: wrela fmt <file-or-dir>... [--check]");
        return ExitCode::from(2);
    }
    let mut unformatted = 0;
    // A file with syntax errors (exit 1), and a file that can't be read or written (exit 2).
    let (mut failed, mut io_failed) = (false, false);
    for f in files {
        let text = match std::fs::read_to_string(&f) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("error: can't read `{}`: {e}", f.display());
                io_failed = true;
                continue;
            }
        };
        let formatted = match formatted(&f.display().to_string(), &text) {
            Ok(formatted) => formatted,
            Err(rendered) => {
                eprint!("{rendered}");
                eprintln!("`{}` has syntax errors, so it wasn't formatted", f.display());
                failed = true;
                continue;
            }
        };
        if formatted == text {
            continue;
        }
        unformatted += 1;
        if check_only {
            println!("{}", f.display());
        } else if let Err(e) = std::fs::write(&f, formatted) {
            eprintln!("error: can't write `{}`: {e}", f.display());
            io_failed = true;
        }
    }
    if io_failed {
        ExitCode::from(2)
    } else if failed {
        ExitCode::from(1)
    } else if check_only && unformatted > 0 {
        eprintln!("{unformatted} file{} would be reformatted", wrela_diag::plural(unformatted));
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
