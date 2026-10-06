//! `wrela edit <package-dir> [--edits <file.json>] [--dry-run] [--json]`: writes new values into
//! literals of the program's source, each in its literal's style (language.md §22). The edits
//! are JSON, from the file or standard input: `{"edits": [{"file", "start", "end", "text",
//! "hash", "value"}]}`, each literal as a lifted build's `lift.json` or `std::lift` names it.
//! A file that has changed since its literals were read is refused, and so is an edit that would
//! change more than literals. Exit status: 0 written (or, with `--dry-run`, would be), 1 refused,
//! 2 a usage error.

use std::path::PathBuf;
use std::process::ExitCode;

pub fn run(args: &[String]) -> ExitCode {
    let (mut dir, mut from, mut dry, mut json) = (None, None, false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--edits" => match it.next() {
                Some(f) => from = Some(PathBuf::from(f)),
                None => return crate::usage(),
            },
            "--dry-run" => dry = true,
            "--json" => json = true,
            _ if dir.is_none() && !a.starts_with('-') => dir = Some(PathBuf::from(a)),
            _ => return crate::unknown(a),
        }
    }
    let Some(dir) = dir else { return crate::usage() };
    let text = match &from {
        Some(f) => std::fs::read_to_string(f),
        None => std::io::read_to_string(std::io::stdin()),
    };
    let text = match text {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: can't read the edits: {e}");
            return ExitCode::from(2);
        }
    };
    let edits = match wrela_driver::edit::parse_edits(&text) {
        Ok(e) => e,
        Err(why) => {
            eprintln!("error: {why}");
            return ExitCode::from(2);
        }
    };
    let planned = match wrela_driver::edit::plan(&dir, &edits) {
        Ok(p) => p,
        Err(r) => {
            if json {
                let v = serde_json::json!({ "version": 1, "written": false, "refused": { "file": r.file, "why": r.why } });
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                eprintln!("error: refused `{}`: {}", r.file, r.why);
            }
            return ExitCode::from(1);
        }
    };
    let written = !dry;
    if written && let Err(e) = wrela_driver::edit::write(&dir, &planned) {
        eprintln!("error: can't write: {e}");
        return ExitCode::from(2);
    }
    if json {
        print!("{}", wrela_driver::edit::to_json(&planned, written));
    } else {
        print!("{}", wrela_driver::edit::diff(&planned));
    }
    ExitCode::SUCCESS
}
