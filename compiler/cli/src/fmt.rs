//! `wrela fmt`: formats files in place, or with `--check` reports which aren't formatted.
//! A file with syntax errors is left alone and reported.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wrela_diag::{FileId, SourceMap};

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else { return };
        let mut entries: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            let name = e.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if name.starts_with('.') || name == "build" || name == "results" {
                continue;
            }
            if std::fs::symlink_metadata(&e).is_ok_and(|m| m.file_type().is_symlink()) {
                continue;
            }
            collect(&e, out);
        }
    } else if path.extension().is_some_and(|x| x == "wrela") {
        out.push(path.to_path_buf());
    }
}

pub fn run(args: &[String]) -> ExitCode {
    let check_only = args.iter().any(|a| a == "--check");
    let mut files = Vec::new();
    for a in args.iter().filter(|a| !a.starts_with("--")) {
        let p = Path::new(a);
        if !p.exists() {
            eprintln!("error: `{a}` doesn't exist");
            return ExitCode::from(2);
        }
        collect(p, &mut files);
    }
    if files.is_empty() {
        eprintln!("usage: wrela fmt <file-or-dir>... [--check]");
        return ExitCode::from(2);
    }
    let mut unformatted = 0;
    let mut failed = false;
    for f in files {
        let text = match std::fs::read_to_string(&f) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("error: can't read `{}`: {e}", f.display());
                failed = true;
                continue;
            }
        };
        let parsed = wrela_syntax::parse(FileId(0), &text);
        if parsed.has_errors() {
            let mut map = SourceMap::new();
            map.add(f.display().to_string(), text.clone());
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
            failed = true;
        }
    }
    if failed {
        ExitCode::from(1)
    } else if check_only && unformatted > 0 {
        eprintln!(
            "{unformatted} file{} would be reformatted",
            if unformatted == 1 { "" } else { "s" }
        );
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
