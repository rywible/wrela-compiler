//! `wrela fmt`: formats files in place, or with `--check` reports which aren't formatted.
//! A file with syntax errors is left alone and reported.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wrela_diag::{FileId, SourceMap};

/// The `.wrela` files under `path`. A package (a directory with a `main.wrela`) contributes
/// exactly the files `wrela check` reads; other directories are searched for packages and
/// files, skipping hidden entries and symbolic links. Returns whether the layout was readable.
fn collect(path: &Path, out: &mut Vec<PathBuf>) -> bool {
    if !path.is_dir() {
        if path.extension().is_some_and(|x| x == "wrela") {
            out.push(path.to_path_buf());
        }
        return true;
    }
    if path.join("main.wrela").is_file() {
        return match wrela_driver::package::find_files(path) {
            Ok(files) => {
                out.extend(files.into_iter().map(|f| f.path));
                true
            }
            Err(errors) => {
                for e in errors {
                    eprintln!("error: {}: {}", path.join(&e.path).display(), e.message);
                }
                false
            }
        };
    }
    let Ok(entries) = std::fs::read_dir(path) else { return true };
    let mut entries: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    let mut ok = true;
    for e in entries {
        let hidden = e.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
        if hidden || std::fs::symlink_metadata(&e).is_ok_and(|m| m.file_type().is_symlink()) {
            continue;
        }
        ok &= collect(&e, out);
    }
    ok
}

/// The stack of the thread that formats: the parser and the formatter recurse over syntax
/// trees, and a file can hold the deepest tree the parser accepts (or rejects as too deep), as
/// on the compiler's own thread.
const STACK_SIZE: usize = 256 << 20;

pub fn run(args: &[String]) -> ExitCode {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("wrela fmt".into())
            .stack_size(STACK_SIZE)
            .spawn_scoped(s, || format_files(args))
            .expect("can't start the formatter's thread")
            .join()
            .unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
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
        let parsed = wrela_syntax::parse(FileId(0), &text);
        if parsed.has_errors() {
            let mut map = SourceMap::new();
            map.add(f.display().to_string(), text);
            eprint!("{}", wrela_diag::render::render_all(&map, &parsed.diagnostics));
            eprintln!("`{}` has syntax errors, so it wasn't formatted", f.display());
            failed = true;
            continue;
        }
        let formatted = wrela_syntax::fmt::format(&parsed, &text);
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
