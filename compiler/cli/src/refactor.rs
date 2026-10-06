//! `wrela refactor <package-dir> <change> [--dry-run] [--json]`: changes that keep what the
//! program means (see `wrela_driver::refactor`), checked before they're written:
//!
//! ```text
//! rename <item> <new-name>
//! move <item> <module>
//! add-param <fn> "<name>: <type>" (--value <expr> | --default <expr>)
//! change-mode <fn> <param> <borrow|mut|take>
//! apply <plan.json>                       a plan `--dry-run --json` printed, if no file it
//!                                         changes has changed since
//! ```
//!
//! It prints the diff (`--json`: the plan, with each file's hashes, hunks and new text).
//! `--dry-run` writes nothing. Exit status: 0 planned (and written, without `--dry-run`); 1
//! refused, with why (and the errors the program would have had); 2 a usage error.

use std::path::PathBuf;
use std::process::ExitCode;
use wrela_driver::refactor;

pub fn run(args: &[String]) -> ExitCode {
    let (mut dry, mut json, mut value, mut default) = (false, false, None, None);
    let mut words: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" => dry = true,
            "--json" => json = true,
            "--value" => match it.next() {
                Some(v) => value = Some(v.clone()),
                None => return crate::usage(),
            },
            "--default" => match it.next() {
                Some(v) => default = Some(v.clone()),
                None => return crate::usage(),
            },
            _ if a.starts_with("--") => return crate::usage(),
            _ => words.push(a.clone()),
        }
    }
    let [dir, change, rest @ ..] = words.as_slice() else { return crate::usage() };
    let dir = PathBuf::from(dir);
    let planned = match (change.as_str(), rest) {
        ("rename", [item, new]) => refactor::rename(&dir, item, new),
        ("move", [item, module]) => refactor::move_item(&dir, item, module),
        ("add-param", [func, param]) => {
            refactor::add_param(&dir, func, param, value.as_deref(), default.as_deref())
        }
        ("change-mode", [func, param, mode]) => refactor::change_mode(&dir, func, param, mode),
        ("apply", [plan]) => match std::fs::read_to_string(plan) {
            Ok(text) => refactor::read_plan(&dir, &text),
            Err(e) => {
                eprintln!("error: can't read {plan}: {e}");
                return ExitCode::from(2);
            }
        },
        _ => return crate::usage(),
    };
    let plan = match planned {
        Ok(p) => p,
        Err(r) => return refused(&r, json),
    };
    let written = !dry;
    if written && let Err(r) = refactor::write(&dir, &plan) {
        return refused(&r, json);
    }
    if json {
        print!("{}", refactor::to_json(&plan, written));
    } else {
        print!("{}", refactor::diff(&plan));
        for n in &plan.notes {
            println!("note: {n}");
        }
        println!(
            "{}: {} ({})",
            plan.op,
            if written { "written" } else { "planned, not written" },
            crate::count(plan.files.len(), "file", "files")
        );
    }
    ExitCode::SUCCESS
}

fn refused(r: &refactor::Refusal, json: bool) -> ExitCode {
    if json {
        print!("{}", refactor::refusal_json(r));
    } else {
        eprintln!("refused: {}", r.why);
        if let Some(e) = &r.errors {
            eprint!("{e}");
        }
    }
    ExitCode::from(1)
}
