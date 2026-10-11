//! `wrela query <package-dir> [<query>...] [--json]`: what the compiler knows of a program,
//! for tools (see `wrela_driver::query`): one check answers every query given, each an argument
//! (`"callers main::step"`), or, with none, each line of standard input. The answers are a JSON
//! array, in the queries' order: indented for people, one line with `--json`. A query that can't
//! be answered has an `error`. A program with errors is answered too: an answer its errors could
//! change lists them as `unsure`, and they're printed on standard error. A package that doesn't
//! load gets an `error` for each query.
//!
//! `wrela context <item> [<package-dir>] [--budget n]`: what an agent needs to change `item`
//! (its source, the types it names, what it calls, where it's called, its impls) in at most
//! about `n` tokens (2,000 unless said). A program with errors gets it too, and what they keep
//! from being known is said at its end.
//!
//! Exit status: 0; 1 if the package has errors (they're printed, after the answers) or a query
//! couldn't be answered; 2 a usage error.

use serde_json::json;
use std::path::PathBuf;
use std::process::ExitCode;

pub fn query(args: &[String]) -> ExitCode {
    let (mut dir, mut json, mut texts) = (None, false, Vec::new());
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            _ if dir.is_none() && !a.starts_with('-') => dir = Some(PathBuf::from(a)),
            _ if !a.starts_with('-') => texts.push(a.clone()),
            _ => return crate::usage(),
        }
    }
    let Some(dir) = dir else { return crate::usage() };
    if texts.is_empty() {
        let Ok(input) = std::io::read_to_string(std::io::stdin()) else {
            eprintln!("error: can't read the queries from standard input");
            return ExitCode::from(2);
        };
        texts = input.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    }
    let mut queries = Vec::new();
    for t in &texts {
        match wrela_driver::query::parse(t) {
            Ok(q) => queries.push((t.clone(), q)),
            Err(why) => {
                eprintln!("error: {why}");
                return ExitCode::from(2);
            }
        }
    }
    let (answers, errors) = match wrela_driver::query::answer(&dir, &queries) {
        Ok(a) => (a.answers, a.errors),
        // Each query is still answered, so standard output is always the answers.
        Err(errors) => {
            let why = "the package didn't load: its errors are on standard error";
            let answers = texts.iter().map(|t| json!({ "query": t, "error": why })).collect();
            (answers, Some(errors))
        }
    };
    let failed = answers.iter().any(|a| a.get("error").is_some());
    let all = serde_json::Value::Array(answers);
    if json {
        println!("{all}");
    } else {
        println!("{}", serde_json::to_string_pretty(&all).unwrap_or_default());
    }
    if let Some(errors) = &errors {
        eprint!("{errors}");
    }
    if failed || errors.is_some() { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

pub fn context(args: &[String]) -> ExitCode {
    let (mut item, mut dir, mut budget) = (None, None, 2000);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--budget" => match it.next().and_then(|n| n.parse().ok()) {
                Some(n) => budget = n,
                None => return crate::usage(),
            },
            _ if item.is_none() && !a.starts_with('-') => item = Some(a.clone()),
            _ if dir.is_none() && !a.starts_with('-') => dir = Some(PathBuf::from(a)),
            _ => return crate::usage(),
        }
    }
    let Some(item) = item else { return crate::usage() };
    let dir = dir.unwrap_or_else(|| PathBuf::from("."));
    match wrela_driver::query::context(&dir, &item, budget) {
        Ok((text, None)) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Ok((text, Some(errors))) => {
            print!("{text}");
            eprint!("{errors}");
            ExitCode::from(1)
        }
        Err(why) => {
            eprintln!("error: {}", why.trim_end());
            ExitCode::from(1)
        }
    }
}
