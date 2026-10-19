//! `wrela tool <package-dir> [--read NAME=DIR]... [--write NAME=DIR]... [--secret NAME]...
//! [--net HOST]... [--allow-run NAME[=PATH|=tool:DIR]]... [--browser] [--timeout S]
//! [--blob-budget B] [-- args...]`: builds the package's tool build and runs it on the native host's tool profile
//! (language.md §15.5, `wrela_host::tool`), with what the command line grants it and nothing
//! else. A `tool:` program is run by this same executable, as `wrela tool`.
//!
//! `wrela tool <name> [grants...] [-- args...]` runs the tool that the workspace at or above the
//! current directory declares as `[tools.<name>]` (language.md §3.6): its package's tool build,
//! with the grants the workspace declares, its paths from the workspace's directory. The
//! command line may only narrow them ([`narrowed`]). A path to a package keeps working as
//! before; a word that names a tool is the tool, else a directory.
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
//! the package. Its constants' cache, and the toolchain service's (`std::tool::toolchain`, which
//! this command gives the host: `crate::toolchain`), is the user's own
//! (`crate::toolchain::cache_dir`), never one in a root the tool is granted.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use wrela_driver::workspace::Workspace;
use wrela_host::tool::{BlobBudget, Caps, Grants, Opened, Options, ProgramGrant, ToolBuild};

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
        // Chrome for `std::tool::browser`: a grant with no value.
        if flag == "--browser" {
            grants.browser = true;
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
    let Some(mut dir) = dir else { return crate::usage() };
    match named(&dir) {
        Ok(None) => {}
        Ok(Some((ws, tool))) => {
            let declared = match declared(&ws, &tool) {
                Ok(d) => d,
                Err(why) => {
                    eprintln!("error: the tool `{}`: {why}", tool.name);
                    return ExitCode::from(2);
                }
            };
            let most = tool.timeout.map(Duration::from_secs);
            match narrowed(&declared, grants, timeout, most) {
                Ok((g, t)) => (grants, timeout) = (g, t),
                Err(why) => {
                    eprintln!("error: the tool `{}`: {why}", tool.name);
                    return ExitCode::from(2);
                }
            }
            dir = ws.root.join(&tool.package);
        }
        Err(why) => {
            eprintln!("error: {why}");
            return ExitCode::from(2);
        }
    }
    if let Err(status) = crate::package_dir(&dir) {
        return status;
    }
    if let Some(at) = std::env::var_os("WRELA_TESTING_GRANTS_HOLD")
        && let Err(why) = held(std::path::Path::new(&at))
    {
        eprintln!("error: {why}");
        return ExitCode::from(71);
    }
    let caps = match Caps::open(&grants, |name| std::env::var(name).ok()) {
        Ok(c) => c,
        Err(why) => {
            eprintln!("error: {why}");
            return ExitCode::from(2);
        }
    };
    let cache = crate::toolchain::cache_dir();
    let scope = wrela_driver::Scope {
        cache: Some(cache.iter().cloned().collect()),
        ..wrela_driver::Scope::default()
    };
    let built =
        match wrela_driver::build_in(&dir, wrela_driver::BuildKind::default(), true, &[], &scope) {
            Ok(b) => b,
            Err(why) => {
                eprintln!("error: {why}");
                return ExitCode::from(2);
            }
        };
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
    options.toolchain = Some(Arc::new(crate::toolchain::Compiler));
    options.cache = cache;
    options.testing = testing();
    let ended = wrela_host::tool::run(&ToolBuild::from_wasm(wasm.clone()), options);
    if !matches!(ended.end, wrela_host::tool::End::Exit(_)) {
        say(format!("error: {}", ended.end));
    }
    // Ends the process here, so a worker still past its grace (the end says so) can't outlive it.
    std::process::exit(i32::from(ended.end.status()))
}

/// The tool that `arg` names, if it's a name rather than a path: a word of letters, digits, `_`
/// and `-` that the workspace at or above the current directory declares as `[tools.<arg>]`.
/// Any other argument is a package's directory, as before workspaces named tools. `Err` if the
/// word names no directory and the workspace doesn't read, or declares no such tool.
fn named(
    arg: &std::path::Path,
) -> Result<Option<(Workspace, wrela_driver::manifest::ToolDecl)>, String> {
    let Some(word) = arg.to_str().filter(|w| wrela_driver::manifest::is_tool_name(w)) else {
        return Ok(None);
    };
    let here =
        std::env::current_dir().map_err(|e| format!("can't read the current directory: {e}"))?;
    let Some(manifest) = wrela_driver::workspace::find(&here) else { return Ok(None) };
    match Workspace::open(&manifest) {
        Ok(ws) => match ws.tool(word).cloned() {
            Some(tool) => Ok(Some((ws, tool))),
            None if arg.is_dir() => Ok(None),
            None => {
                let names: Vec<&str> = ws.decl.tools.iter().map(|t| t.name.as_str()).collect();
                Err(format!(
                    "`{word}` is neither a package's directory nor a tool of the workspace {} (its tools: {})",
                    manifest.display(),
                    if names.is_empty() { "none".to_string() } else { names.join(", ") }
                ))
            }
        },
        Err(_) if arg.is_dir() => Ok(None),
        Err(why) => Err(format!("the workspace isn't valid:\n{why}")),
    }
}

/// The grants the workspace declares for `tool`, its paths from the workspace's directory. Each
/// root is opened from the workspace's directory one part at a time, following no link, and a
/// missing `write` root is made the same way ([`Opened::within`]): so it is inside the
/// workspace, and the run gets that handle, not the path opened again.
fn declared(ws: &Workspace, tool: &wrela_driver::manifest::ToolDecl) -> Result<Grants, String> {
    let top = Opened::at(&ws.root)?;
    let mut g = Grants::default();
    for (roots, write) in [(&tool.read, false), (&tool.write, true)] {
        for (name, path) in roots {
            let opened = top.within(path, write).map_err(|e| {
                format!("the root `{name}` ({path}) isn't a directory inside the workspace: {e}")
            })?;
            let dir = if path == "." { ws.root.clone() } else { ws.root.join(path) };
            g = g.root(name, dir, write);
            g.opened.push((name.clone(), opened));
        }
    }
    for program in &tool.run {
        g.parse("--allow-run", program)?;
    }
    // A program's path, or a `tool:` package's, is from the workspace's directory.
    for (_, grant) in &mut g.programs {
        match grant {
            ProgramGrant::At(p) | ProgramGrant::Tool(p) if p.is_relative() => {
                *p = ws.root.join(&*p)
            }
            _ => {}
        }
    }
    g.net = tool.net.clone();
    g.secrets = tool.secrets.clone();
    g.browser = tool.browser;
    Ok(g)
}

/// The grants and timeout of a run of a declared tool: what the command line gives, if it gives
/// a grant, each within one that `declared` holds; else `declared`. The command line may only
/// narrow: a root at or below a declared root of its name (a write root's for writing), a
/// program as declared (`--allow-run NAME` alone keeps the declared one), and a host, a secret
/// or `--browser` that is declared. Its `--timeout` may only shorten the declared one (`most`).
fn narrowed(
    declared: &Grants,
    given: Grants,
    timeout: Option<Duration>,
    most: Option<Duration>,
) -> Result<(Grants, Option<Duration>), String> {
    let timeout = match (timeout, most) {
        (Some(t), Some(m)) if t > m => {
            return Err(format!(
                "`--timeout {}` widens its grants: the workspace gives it {} s at most",
                t.as_secs_f64(),
                m.as_secs()
            ));
        }
        (t, m) => t.or(m),
    };
    let grants_any = !given.roots.is_empty()
        || !given.secrets.is_empty()
        || !given.net.is_empty()
        || !given.programs.is_empty()
        || given.browser;
    if !grants_any {
        return Ok((Grants { tool_runner: given.tool_runner, ..declared.clone() }, timeout));
    }
    let wider =
        |what: String| format!("{what} widens its grants, which the command line may only narrow");
    let mut out = Grants { tool_runner: given.tool_runner.clone(), ..Grants::default() };
    for (name, dir, write) in &given.roots {
        let flag = if *write { "--write" } else { "--read" };
        let shown = format!("`{flag} {name}={}`", dir.display());
        let canonical = dir
            .canonicalize()
            .map_err(|e| format!("the root `{name}` ({}): {e}", dir.display()))?;
        let mine = declared.roots.iter().find(|(n, _, w)| n == name && (*w || !*write));
        let Some(rel) = mine.and_then(|(_, d, _)| canonical.strip_prefix(d).ok()) else {
            return Err(wider(shown));
        };
        // Opened from the declared root's handle, following no link: what replaces a part of
        // the path after this check can't lead the run anywhere else.
        let rel = rel.to_str().filter(|r| !r.is_empty()).unwrap_or(".");
        let (_, top) = (declared.opened.iter().find(|(n, _)| n == name))
            .ok_or_else(|| format!("the root `{name}` wasn't opened"))?;
        let opened = top.within(rel, false).map_err(|e| format!("{shown}: {e}"))?;
        out.roots.push((name.clone(), canonical, *write));
        out.opened.push((name.clone(), opened));
    }
    for (name, grant) in &given.programs {
        let Some((_, mine)) = declared.programs.iter().find(|(n, _)| n == name) else {
            return Err(wider(format!("`--allow-run {name}`")));
        };
        let same = |a: &std::path::Path, b: &std::path::Path| {
            a.canonicalize().ok().is_some_and(|a| b.canonicalize().ok() == Some(a))
        };
        let kept = match (grant, mine) {
            (ProgramGrant::OnPath, _) => true,
            (ProgramGrant::At(a), ProgramGrant::At(b))
            | (ProgramGrant::Tool(a), ProgramGrant::Tool(b)) => same(a, b),
            _ => false,
        };
        if !kept {
            return Err(wider(format!(
                "`--allow-run {name}=...`, not the program the workspace grants,"
            )));
        }
        out.programs.push((name.clone(), mine.clone()));
    }
    for host in &given.net {
        if !declared.net.contains(host) {
            return Err(wider(format!("`--net {host}`")));
        }
    }
    for secret in &given.secrets {
        if !declared.secrets.contains(secret) {
            return Err(wider(format!("`--secret {secret}`")));
        }
    }
    if given.browser && !declared.browser {
        return Err(wider("`--browser`".into()));
    }
    out.net = given.net;
    out.secrets = given.secrets;
    out.browser = given.browser;
    Ok((out, timeout))
}

/// Runs the tool test that `dir/test.json` describes, writes how it ended where the spec says,
/// and ends the process: status 0 if the test passed, 1 if it failed, 2 if the spec can't be
/// read. Its grants are a write root `scratch`, its fake sites and its `tool:` programs (and
/// Chrome, if it asks, on the real clock), on a fake clock, with its budget as the timeout.
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
        browser: spec.browser,
        ..Grants::default()
    }
    .root("scratch", &spec.scratch, true);
    for (name, package) in &spec.run {
        grants = grants.program(name, wrela_host::tool::ProgramGrant::Tool(package.clone()));
    }
    for (name, dir) in &spec.read {
        grants = grants.root(name, dir, false);
    }
    let ended = match (Caps::open(&grants, |_| None), std::fs::read(&spec.wasm)) {
        (Ok(caps), Ok(wasm)) => {
            let mut options = Options::new(caps);
            options.timeout = Some(Duration::from_secs_f64(spec.budget));
            if let Some(b) = &spec.blobs {
                match BlobBudget::parse(b) {
                    Ok(budget) => options.blob_budget = budget,
                    Err(why) => {
                        eprintln!("error: a tool test's spec: {why}");
                        return ExitCode::from(2);
                    }
                }
            }
            options.entries = (spec.init.clone(), spec.resume.clone());
            options.fake_clock = Some((spec.utc, 0));
            // A live browser keeps the system's time: the clock doesn't jump past its page.
            options.real_clock = spec.browser;
            options.groups = Some(spec.groups.clone());
            options.toolchain = Some(Arc::new(crate::toolchain::Compiler));
            options.cache = crate::toolchain::cache_dir();
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

/// For the command's own tests (`WRELA_TESTING_GRANTS_HOLD`, a directory): once the grants are
/// checked, before they are opened, says so (the file `checked`) and waits for the file `go`, so
/// a test can change the directories in between at a moment it knows.
fn held(at: &std::path::Path) -> Result<(), String> {
    std::fs::write(at.join("checked"), "").map_err(|e| format!("the testing hold: {e}"))?;
    let until = Instant::now() + Duration::from_secs(60);
    while !at.join("go").exists() {
        if Instant::now() > until {
            return Err("the testing hold waited a minute for `go`".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

/// The host's settings that only the command's own tests change, from the environment: a pause
/// between a program's start and its record (`WRELA_TESTING_SPAWN_PAUSE`), a name lookup that
/// stalls (`WRELA_TESTING_RESOLVE_STALL`) and a slow disk under each output written
/// (`WRELA_TESTING_IO_DELAY`), each in seconds; one output write held (`WRELA_TESTING_HOLD`,
/// `which:seconds`), and one that fails part way (`WRELA_TESTING_WRITE_FAIL`, `which`), each
/// chosen by its path's end or `#n`, the request's `n`th write.
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
        io_delay: seconds("WRELA_TESTING_IO_DELAY"),
        hold: std::env::var("WRELA_TESTING_HOLD").ok().and_then(|v| {
            let (which, s) = v.rsplit_once(':')?;
            Some((which.to_string(), Duration::try_from_secs_f64(s.parse().ok()?).ok()?))
        }),
        write_fail: std::env::var("WRELA_TESTING_WRITE_FAIL").ok().filter(|v| !v.is_empty()),
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
