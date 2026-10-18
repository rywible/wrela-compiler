//! `wrela tool <package-dir> [--read NAME=DIR]... [--write NAME=DIR]... [--secret NAME]...
//! [--net HOST]... [--allow-run NAME[=PATH|=tool:DIR]]... [--timeout S] [--blob-budget B]
//! [-- args...]`: builds the package's tool build and runs it on the native host's tool profile
//! (language.md §15.5, `wrela_host::tool`), with what the command line grants it and nothing
//! else. A `tool:` program is run by this same executable, as `wrela tool`.
//!
//! `wrela tool --test <dir>` runs one tool test (§13.2), as `wrela test` asks: the test's
//! build, entries and grants are in `<dir>/test.json` (`wrela_driver::tool_tests::Spec`), and
//! how its run ended goes to the file the spec names. The process's exit status is the tool's: its own, or 70 if
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
    if let [flag, dir] = args
        && flag == "--test"
    {
        return test(std::path::Path::new(dir));
    }
    if let [flag, go, program, rest @ ..] = args
        && flag == "--launch"
    {
        let rest: Vec<std::ffi::OsString> = rest.iter().map(Into::into).collect();
        let why = wrela_host::tool::launch(std::path::Path::new(go), program.as_ref(), &rest);
        eprintln!("error: {why}");
        return ExitCode::from(127);
    }
    let mut grants = Grants { tool_runner: std::env::current_exe().ok(), ..Grants::default() };
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
        if !matches!(
            flag,
            "--read"
                | "--write"
                | "--secret"
                | "--net"
                | "--allow-run"
                | "--timeout"
                | "--blob-budget"
        ) {
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
    // A supervisor (a tool test's) that must be able to kill this tool's programs names a file
    // for their process groups.
    options.groups = std::env::var_os(wrela_host::tool::GROUPS_ENV).map(std::path::PathBuf::from);
    options.testing = testing();
    let ended = wrela_host::tool::run(&ToolBuild::from_wasm(wasm.clone()), options);
    if !matches!(ended.end, wrela_host::tool::End::Exit(_)) {
        say(format!("error: {}", ended.end));
    }
    // Ends the process here, so a worker still past its grace (the end says so) can't outlive it.
    std::process::exit(i32::from(ended.end.status()))
}

/// Runs the tool test that `dir/test.json` describes, writes how it ended where the spec says,
/// and ends the process: status 0 if the test passed, 1 if it failed, 2 if the spec can't be
/// read. Its grants are a write root `scratch`, its fake sites and its `tool:` programs, on a
/// fake clock, with its budget as the timeout.
fn test(dir: &std::path::Path) -> ExitCode {
    use wrela_driver::tool_tests::{Ended, Spec};
    let read = std::fs::read(dir.join("test.json"))
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice::<Spec>(&b).map_err(|e| e.to_string()));
    let spec = match read {
        Ok(s) => s,
        Err(why) => {
            eprintln!("error: a tool test's spec in {}: {why}", dir.display());
            return ExitCode::from(2);
        }
    };
    let mut grants = Grants {
        fake_net: spec.net.clone(),
        tool_runner: std::env::current_exe().ok(),
        ..Grants::default()
    }
    .root("scratch", &spec.scratch, true);
    for (name, package) in &spec.run {
        grants = grants.program(name, wrela_host::tool::ProgramGrant::Tool(package.clone()));
    }
    let ended = match (Caps::open(&grants, |_| None), std::fs::read(&spec.wasm)) {
        (Ok(caps), Ok(wasm)) => {
            let mut options = Options::new(caps);
            options.timeout = Some(Duration::from_secs_f64(spec.budget));
            options.entries = (spec.init.clone(), spec.resume.clone());
            options.fake_clock = Some((spec.utc, 0));
            options.groups = Some(spec.groups.clone());
            options.testing = testing();
            let ended = wrela_host::tool::run(&ToolBuild::from_wasm(wasm), options);
            let end = match &ended.end {
                wrela_host::tool::End::Timeout => {
                    format!("it ran past its budget of {} s", spec.budget)
                }
                wrela_host::tool::End::Trap(why) => format!("it trapped: {why}"),
                other => other.to_string(),
            };
            Ended { passed: ended.end == wrela_host::tool::End::Exit(0), end }
        }
        (Err(why), _) => Ended { passed: false, end: format!("its grants: {why}") },
        (_, Err(e)) => Ended { passed: false, end: format!("its build: {e}") },
    };
    let written = serde_json::to_vec(&ended)
        .map_err(|e| e.to_string())
        .and_then(|b| std::fs::write(&spec.ended, b).map_err(|e| e.to_string()));
    if let Err(why) = written {
        say(format!("error: writing how the tool test ended: {why}"));
        std::process::exit(2);
    }
    // Ends the process here, so a worker still past its grace can't outlive it.
    std::process::exit(if ended.passed { 0 } else { 1 })
}

/// The host's settings that only the command's own tests change, from the environment: a pause
/// between a program's start and its record (`WRELA_TESTING_SPAWN_PAUSE`) and a name lookup
/// that stalls (`WRELA_TESTING_RESOLVE_STALL`), each in seconds.
fn testing() -> wrela_host::tool::Testing {
    let seconds = |name: &str| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .and_then(|s| Duration::try_from_secs_f64(s).ok())
            .unwrap_or(Duration::ZERO)
    };
    wrela_host::tool::Testing {
        spawn_pause: seconds("WRELA_TESTING_SPAWN_PAUSE"),
        resolve_stall: seconds("WRELA_TESTING_RESOLVE_STALL"),
        ..wrela_host::tool::Testing::default()
    }
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
