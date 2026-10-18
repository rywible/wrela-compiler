//! `wrela tool <package-dir> [--read NAME=DIR]... [--write NAME=DIR]... [--secret NAME]...
//! [--timeout S] [--blob-budget B] [-- args...]`: builds the package's tool build and runs it on
//! the native host's tool profile (language.md §15.5, `wrela_host::tool`), with what the command
//! line grants it and nothing else. The process's exit status is the tool's: its own, or 70 if
//! it trapped or waits on nothing, 71 if the host couldn't run it or end it cleanly (it can't
//! take Ctrl-C, or a worker still runs after the grace), 124 past `--timeout`, 130 on Ctrl-C. A
//! package that doesn't build exits 1 with its diagnostics, and a usage error (a timeout the
//! clock can't reach among them) 2. The process ends with the tool, so no worker outlives it.
//!
//! The build is made in memory and its WASM compiled for this run: nothing is written beside
//! the package but the constants' cache that every build keeps.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use wrela_host::tool::{BlobBudget, Caps, Grants, Options, ToolBuild};

pub fn run(args: &[String]) -> ExitCode {
    let mut grants = Grants::default();
    let mut timeout = None;
    let mut budget = BlobBudget::default();
    let mut dir = None;
    let mut tool_args = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let flag = a.as_str();
        if flag == "--" {
            tool_args = it.by_ref().cloned().collect();
            break;
        }
        if !flag.starts_with('-') && dir.is_none() {
            dir = Some(std::path::PathBuf::from(a));
            continue;
        }
        if !matches!(flag, "--read" | "--write" | "--secret" | "--timeout" | "--blob-budget") {
            return crate::unknown(a);
        }
        let Some(value) = it.next() else {
            eprintln!("error: {flag} takes a value");
            return ExitCode::from(2);
        };
        let parsed = match flag {
            // A time the clock can't reach from now is a usage error, as a word is.
            "--timeout" => value
                .parse::<f64>()
                .ok()
                .filter(|s| *s > 0.0)
                .and_then(|s| Duration::try_from_secs_f64(s).ok())
                .filter(|d| Instant::now().checked_add(*d).is_some())
                .map(|d| timeout = Some(d))
                .ok_or_else(|| {
                    format!("--timeout takes seconds the clock can reach, not `{value}`")
                }),
            "--blob-budget" => BlobBudget::parse(value).map(|b| budget = b),
            _ => grants.parse(flag, value).map(|_| ()),
        };
        if let Err(why) = parsed {
            eprintln!("error: {why}");
            return ExitCode::from(2);
        }
    }
    let Some(dir) = dir else { return crate::usage() };
    if let Err(status) = crate::package_dir(&dir) {
        return status;
    }
    let caps = match Caps::open(&grants, |name| std::env::var(name).ok()) {
        Ok(c) => c,
        Err(why) => {
            eprintln!("error: {why}");
            return ExitCode::from(2);
        }
    };
    let built = wrela_driver::build(&dir);
    if crate::report(&built, false) {
        return ExitCode::from(1);
    }
    let Some((_, wasm)) = built.files.iter().find(|(p, _)| p == wrela_driver::build::TOOL_WASM)
    else {
        eprintln!(
            "error: `{}` has no tool build: its main.wrela doesn't export `tool` (§15.5)",
            dir.display()
        );
        return ExitCode::from(2);
    };
    let interrupt = Arc::new(AtomicBool::new(false));
    let flag = interrupt.clone();
    // The first Ctrl-C stops the tool, and the host cleans up after it; a second ends the
    // process at once. A tool that couldn't be stopped isn't started.
    let handled = ctrlc::set_handler(move || {
        if flag.swap(true, Ordering::AcqRel) {
            std::process::exit(130);
        }
    });
    if let Err(e) = handled {
        eprintln!("error: the tool host can't take Ctrl-C, so it couldn't stop the tool: {e}");
        return ExitCode::from(71);
    }
    let mut options = Options::new(caps);
    options.args = tool_args;
    options.timeout = timeout;
    options.blob_budget = budget;
    options.interrupt = Some(interrupt);
    let ended = wrela_host::tool::run(&ToolBuild::from_wasm(wasm.clone()), options);
    if !matches!(ended.end, wrela_host::tool::End::Exit(_)) {
        say(format!("error: {}", ended.end));
    }
    // Ends the process here, so a worker still past its grace (the end says so) can't outlive it.
    std::process::exit(i32::from(ended.end.status()))
}

/// Writes `text` to standard error from a thread of its own, waiting for it at most a second:
/// a standard error that nobody reads can't keep the process from ending.
fn say(text: String) {
    let (done, wait) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().spawn(move || {
        eprintln!("{text}");
        let _ = done.send(());
    });
    if spawned.is_ok() {
        let _ = wait.recv_timeout(Duration::from_secs(1));
    }
}
