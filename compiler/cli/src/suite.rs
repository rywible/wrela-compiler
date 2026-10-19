//! `wrela suite [<check-name>] [--workspace <dir>]`: every check of a workspace in one command
//! (language.md §13.2), or exactly one of them: the workspace at or above the current
//! directory, or the one `--workspace` names.
//!
//! - `fmt`: the members' `.wrela` files are formatted (`wrela fmt --check` over each member).
//! - `allow`: every file of the workspace is one its `[workspace] allow` declares
//!   ([`wrela_driver::suite`]).
//! - `wrela::<package-path>::`: every test of the member at that path, run as `wrela test` runs
//!   them, in a `wrela test` process of its own: plain, frame, GPU frame (with goldens, on the
//!   hardware GPU) and tool tests, each tool test supervised as `wrela test` supervises it.
//! - `wrela::<package-path>::<test-name>`: exactly that test (`wrela test --exact`).
//!
//! No name runs `fmt`, `allow` and every member's tests, several members at a time, but those of
//! the members that `[workspace] untested` lists, which it names instead. Every check
//! runs even when another fails; each failure's output is shown at the end, then a summary.
//! Exit status 0 when every check passes, 1 when one fails (or a member has no test to run when
//! its tests were named), 2 for a usage error: a name that's malformed, unknown or ambiguous, a
//! second name, or no workspace.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Mutex;
use std::time::Instant;
use wrela_driver::suite::{self, Selection};
use wrela_driver::workspace::Workspace;

pub fn run(args: &[String]) -> ExitCode {
    let mut names = Vec::new();
    let mut at = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--workspace" => match it.next() {
                Some(dir) => at = Some(PathBuf::from(dir)),
                None => return crate::usage(),
            },
            a if a.starts_with('-') => return crate::unknown(a),
            _ => names.push(a.clone()),
        }
    }
    let dir = match at {
        Some(dir) => dir,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let Some(manifest) = wrela_driver::workspace::find(&dir) else {
        eprintln!("error: no `wrela.toml` at or above `{}` has `[workspace]`", dir.display());
        return ExitCode::from(2);
    };
    let ws = match Workspace::open(&manifest) {
        Ok(ws) => ws,
        Err(why) => {
            eprintln!("error: the workspace isn't valid:\n{why}");
            return ExitCode::from(1);
        }
    };
    let selection = match suite::select(&ws, &names) {
        Ok(s) => s,
        Err(why) => {
            eprintln!("error: {why}");
            return ExitCode::from(2);
        }
    };
    let started = Instant::now();
    let status = match &selection {
        Selection::Test(package, test) => exact(&ws, package, test),
        _ => checks(&ws, &selection),
    };
    let what = names.first().map_or("all checks", String::as_str);
    let verdict = if status == ExitCode::SUCCESS { "passed" } else { "failed" };
    println!("{what} {verdict} in {:.1}s", started.elapsed().as_secs_f64());
    status
}

/// A check's end: what failed (`None` if nothing), what it showed, and the tests it ran.
struct Ended {
    failure: Option<String>,
    log: String,
    tests: usize,
}

/// Runs `selection`'s checks (all, `fmt`, `allow` or one member's tests), each to its end, and
/// summarizes them.
fn checks(ws: &Workspace, selection: &Selection) -> ExitCode {
    let paths: Vec<String> = ws.members.iter().map(|m| ws.path_of(&m.dir)).collect();
    let mut work: Vec<(String, Job)> = Vec::new();
    if matches!(selection, Selection::All | Selection::Fmt) {
        work.push(("fmt".into(), Job::Fmt));
    }
    if matches!(selection, Selection::All | Selection::Allow) {
        work.push(("allow".into(), Job::Allow));
    }
    let untested: Vec<&str> = ws.decl.untested.iter().map(|(p, _)| p.as_str()).collect();
    for (member, path) in ws.members.iter().zip(&paths) {
        let named = matches!(selection, Selection::Group(p) if p == path);
        if *selection == Selection::All && untested.contains(&path.as_str()) {
            println!("not run: wrela::{path}:: (`untested` in {})", ws.path_of(&ws.manifest));
        } else if named || *selection == Selection::All {
            work.push((format!("wrela::{path}::"), Job::Tests(member.dir.clone(), named)));
        }
    }
    let threads = jobs();
    if work.len() > 2 {
        println!("members' tests run {threads} packages at a time");
    }
    let print = Mutex::new(());
    let next = Mutex::new(work.iter().enumerate());
    let results: Mutex<Vec<(usize, Ended)>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..threads.min(work.len()) {
            s.spawn(|| {
                loop {
                    let Some((i, (name, job))) = next.lock().expect("the queue").next() else {
                        return;
                    };
                    let started = Instant::now();
                    let (ended, lines) = job.run(ws, name);
                    let _held = print.lock().expect("the terminal");
                    for line in lines {
                        println!("{line}");
                    }
                    let verdict = if ended.failure.is_none() { "ok" } else { "FAILED" };
                    println!("{verdict} {name} in {:.1}s", started.elapsed().as_secs_f64());
                    results.lock().expect("the results").push((i, ended));
                }
            });
        }
    });
    let mut results = results.into_inner().expect("the results");
    results.sort_by_key(|(i, _)| *i);
    let failed: Vec<(&str, &Ended)> = (results.iter())
        .filter(|(_, e)| e.failure.is_some())
        .map(|(i, e)| (work[*i].0.as_str(), e))
        .collect();
    for (name, e) in &failed {
        print!("\n== {name}: {}\n{}", e.failure.as_deref().unwrap_or_default(), e.log);
    }
    let tests: usize = results.iter().map(|(_, e)| e.tests).sum();
    if failed.is_empty() {
        println!("\n{} checks passed, with {tests} tests", results.len());
        ExitCode::SUCCESS
    } else {
        let names: Vec<&str> = failed.iter().map(|(n, _)| *n).collect();
        println!("\n{} of {} checks failed: {}", failed.len(), results.len(), names.join(", "));
        ExitCode::from(1)
    }
}

/// How many members' tests run at a time: half the cores, from 2 to 6, as `tools/check.sh`
/// runs a program's tests. Tests start their own worker pools, so more contend for the cores.
fn jobs() -> usize {
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    (cores / 2).clamp(2, 6)
}

/// One check of [`checks`].
enum Job {
    Fmt,
    Allow,
    /// A member's tests, in its directory; `true` if they were named, so none is a failure.
    Tests(PathBuf, bool),
}

impl Job {
    /// Runs the check named `name`: how it ended, and the lines to show as it ends.
    fn run(&self, ws: &Workspace, name: &str) -> (Ended, Vec<String>) {
        match self {
            Job::Fmt => (fmt(ws), Vec::new()),
            Job::Allow => allow(ws),
            Job::Tests(dir, named) => {
                let (mut ended, lines) = tests(ws, dir);
                if *named && ended.failure.is_none() && ended.tests == 0 {
                    ended.failure = Some(format!("{name} selects no test: the member has none"));
                }
                (ended, lines)
            }
        }
    }
}

/// `fmt`: each member's `.wrela` files (as `wrela fmt` finds them in its directory), formatted.
fn fmt(ws: &Workspace) -> Ended {
    wrela_driver::on_compiler_thread(|| {
        let reach = wrela_driver::reach::Reach::anywhere();
        let mut files = Vec::new();
        let mut errors = Vec::new();
        for m in &ws.members {
            crate::fmt::collect_in(&reach, &m.dir, &mut files, &mut errors);
        }
        files.sort();
        files.dedup();
        let mut log = String::new();
        for e in &errors {
            log.push_str(&format!("error: {e}\n"));
        }
        let mut wrong = 0;
        for f in &files {
            let shown = ws.path_of(f);
            match std::fs::read_to_string(f) {
                Ok(text) => match crate::fmt::format_text(&text) {
                    Some(formatted) if formatted == text => {}
                    Some(_) => {
                        wrong += 1;
                        log.push_str(&format!("not formatted: {shown}\n"));
                    }
                    None => {
                        wrong += 1;
                        log.push_str(&format!("has syntax errors: {shown}\n"));
                    }
                },
                Err(e) => {
                    wrong += 1;
                    log.push_str(&format!("can't read {shown}: {e}\n"));
                }
            }
        }
        let failure = if !errors.is_empty() {
            Some(format!("{} of the members' layouts can't be read", errors.len()))
        } else if wrong > 0 {
            Some(format!("{wrong} of {} files aren't formatted (`wrela fmt` them)", files.len()))
        } else if files.is_empty() {
            Some("the members have no .wrela files".into())
        } else {
            None
        };
        Ended { failure, log, tests: 0 }
    })
}

/// `allow`: the allowlist check. Files waiting for removal are shown as it ends.
fn allow(ws: &Workspace) -> (Ended, Vec<String>) {
    let files = match suite::files(&ws.root) {
        Ok(f) => f,
        Err(why) => {
            return (Ended { failure: Some(why), log: String::new(), tests: 0 }, Vec::new());
        }
    };
    let found = suite::allowlist(&ws.decl, &files);
    let lines: Vec<String> =
        found.pending.iter().map(|f| format!("pending removal: {f}")).collect();
    let mut log = String::new();
    for f in &found.refused {
        log.push_str(&format!("not allowed: {f}\n"));
    }
    for g in &found.stale {
        log.push_str(&format!("pending, but no file waits: {g}\n"));
    }
    let failure = (!found.passed()).then(|| {
        format!(
            "{} of {} files aren't allowed, and {} pending entries match nothing (`[workspace] allow` and `pending` in {})",
            found.refused.len(),
            found.checked,
            found.stale.len(),
            ws.path_of(&ws.manifest)
        )
    });
    (Ended { failure, log, tests: 0 }, lines)
}

/// The tests of the member in `dir`, in a `wrela test` process: how they ended, and a line for
/// each test.
fn tests(ws: &Workspace, dir: &Path) -> (Ended, Vec<String>) {
    match report(dir, None) {
        Ok(r) => r.ended(&ws.path_of(dir), false),
        Err(why) => (Ended { failure: Some(why), log: String::new(), tests: 0 }, Vec::new()),
    }
}

/// What a `wrela test --json` process reported, and what it printed to standard error.
struct Report {
    json: serde_json::Value,
    status: Option<i32>,
    stderr: String,
}

/// Runs `wrela test` on the member in `dir` (exactly the test `exact`, if given). `Err` if it
/// gave no report: its standard error.
fn report(dir: &Path, exact: Option<&str>) -> Result<Report, String> {
    let me = std::env::current_exe().map_err(|e| format!("can't find this `wrela`: {e}"))?;
    let mut command = Command::new(me);
    command.arg("test").arg(dir);
    if let Some(name) = exact {
        command.args([name, "--exact"]);
    }
    let out = command.arg("--json").output().map_err(|e| format!("can't run `wrela test`: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    match serde_json::from_slice(&out.stdout) {
        Ok(json) => Ok(Report { json, status: out.status.code(), stderr }),
        Err(_) => Err(format!("`wrela test` gave no report ({}):\n{stderr}", out.status)),
    }
}

impl Report {
    /// How the run ended, for the member at `path`, and a line for each test. The log has what
    /// each failed test printed (each test's, with `all`) that `wrela test` didn't show as it was
    /// made: a tool test's lines, which its own process captured.
    fn ended(&self, path: &str, all: bool) -> (Ended, Vec<String>) {
        let mut log = self.stderr.clone();
        let mut lines = Vec::new();
        let mut failed = Vec::new();
        let tests = self.json["tests"].as_array().cloned().unwrap_or_default();
        for t in &tests {
            let name = t["name"].as_str().unwrap_or("?");
            let at = t["at"].as_str().unwrap_or("?");
            let passed = t["passed"].as_bool() == Some(true);
            if all || !passed {
                for line in t["printed"].as_array().into_iter().flatten() {
                    let line = line.as_str().unwrap_or_default();
                    if !self.stderr.contains(&format!("{name}: {line}")) {
                        log.push_str(&format!("{name}: {line}\n"));
                    }
                }
            }
            if passed {
                lines.push(format!("ok wrela::{path}::{name} ({at})"));
            } else {
                lines.push(format!("FAILED wrela::{path}::{name} ({at})"));
                log.push_str(&format!(
                    "FAILED wrela::{path}::{name} ({at}): {}\n",
                    said(&t["failure"])
                ));
                failed.push(name.to_string());
            }
        }
        let errors: Vec<&serde_json::Value> = (self.json["diagnostics"].as_array())
            .into_iter()
            .flatten()
            .filter(|d| d["severity"] == "error")
            .collect();
        for d in &errors {
            log.push_str(&format!("{}\n", said(d)));
        }
        let failure = if !errors.is_empty() {
            Some(format!("the package has {} errors, so no tests ran", errors.len()))
        } else if !failed.is_empty() {
            Some(format!("{} of {} tests failed: {}", failed.len(), tests.len(), failed.join(", ")))
        } else if self.status != Some(0) {
            Some(format!("`wrela test` exited with status {:?}", self.status))
        } else {
            None
        };
        (Ended { failure, log, tests: tests.len() }, lines)
    }
}

/// A diagnostic of `--json`, in a line: its code, where it is, and what it says.
fn said(d: &serde_json::Value) -> String {
    let file = d["file"].as_str().unwrap_or("?");
    let line = &d["span"]["line"];
    let column = &d["span"]["column"];
    format!(
        "{} {file}:{line}:{column}: {}",
        d["code"].as_str().unwrap_or("?"),
        d["message"].as_str().unwrap_or("?")
    )
}

/// `wrela::<package>::<test>`: exactly that test, which must be one and pass.
fn exact(ws: &Workspace, package: &str, test: &str) -> ExitCode {
    let dir = ws.members.iter().find(|m| ws.path_of(&m.dir) == package).map(|m| m.dir.clone());
    let dir = dir.expect("a selected member");
    let name = format!("wrela::{package}::{test}");
    let started = Instant::now();
    let report = match report(&dir, Some(test)) {
        Ok(r) => r,
        // The runner refused the name before it ran anything: none or several tests have it.
        Err(why) => {
            eprintln!("error: `{name}` must name exactly one test: {why}");
            return ExitCode::from(2);
        }
    };
    let (mut ended, lines) = report.ended(package, true);
    let ran = report.json["tests"].as_array().map_or(0, Vec::len);
    if ended.failure.is_none() && (ran != 1 || report.json["tests"][0]["name"] != test) {
        ended.failure = Some(format!("the runner ran {ran} tests, not exactly `{test}`"));
    }
    for line in lines {
        println!("{line}");
    }
    // One test's output is shown whether it passed or not, as `tools/check.sh` shows it.
    print!("{}", ended.log);
    let verdict = if ended.failure.is_none() { "ok" } else { "FAILED" };
    println!("{verdict} {name} in {:.1}s", started.elapsed().as_secs_f64());
    match ended.failure {
        None => ExitCode::SUCCESS,
        Some(why) => {
            println!("{why}");
            ExitCode::from(1)
        }
    }
}
