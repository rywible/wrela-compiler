//! Tool tests (language.md §13.2): `@test(tool, ...) @job fn name() { ... }`, a job run on the
//! package's tool build, on the tool host (§15.5). The package is lowered once as a tool build
//! whose entries are the tests' (each test's job started and resumed by
//! `tool_test_init.<i>` and `tool_test.<i>`), and each test runs in a `wrela` process of its
//! own (`wrela tool --test <dir>`), in a process group of its own, so a test that doesn't end
//! is killed with everything in its group.
//!
//! Each test's run gets: a new, empty write root `scratch`; its budget (`budget: s`, 10 s
//! unless it says) as the run's timeout; a fake clock; its `net` hosts as fake sites, served
//! from `scratch/sites/<host>/`; and its `run` programs, each a tool package of the test's
//! package. Nothing else: no other file, no network, no secret, no other program. It passes if
//! the job ends, and fails if it panics or traps, if its budget runs out, or if the host fails.
//! If the process itself is still running [`MARGIN`] past the budget, it's killed, and the test
//! fails.
//!
//! **The supervisor owns every process group a test starts.** A test's programs (its fakes,
//! and theirs) each lead a group of their own, which the test's host writes to a file of the
//! test's (`Spec::groups`) as it starts each. When the host can't end them itself (its budget
//! and the margin passed, it was stopped, it ended without saying how), the supervisor stops
//! the host's group, then stops and kills every group in the file, then the host's. So does
//! [`stop_all`], which `wrela test` calls on Ctrl-C.

use crate::consts::Ran;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use wrela_diag::{Diagnostic, SourceMap, codes, has_errors};
use wrela_lower::BuildData;
use wrela_sema::Checked;
use wrela_sema::defs::Role;
use wrela_sema::ty::FnId;

/// How long past its budget a test's process may run, to start and to end, before it's killed.
pub const MARGIN: Duration = Duration::from_secs(10);

/// The fake clock's date when a tool test starts: 2026-01-01T00:00:00Z.
pub const START_UTC: u64 = 1_767_225_600;

/// The `wrela` executable that runs each tool test.
static RUNNER: OnceLock<PathBuf> = OnceLock::new();

/// Sets the `wrela` executable that runs each tool test (`wrela tool --test`): `wrela test`
/// sets its own. A process that hasn't set one can't run tool tests, and each fails, saying so.
pub fn set_runner(wrela: PathBuf) {
    let _ = RUNNER.set(wrela);
}

/// What a tool test's process runs: the file `test.json` in its directory.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Spec {
    /// The tool build's module.
    pub wasm: PathBuf,
    /// The exports that start and resume the test's job.
    pub init: String,
    pub resume: String,
    /// The run's budget, in seconds.
    pub budget: f64,
    /// The new write root `scratch`.
    pub scratch: PathBuf,
    /// The hosts the test's fake sites stand in for.
    pub net: Vec<String>,
    /// Each program the test may run, and the tool package that stands in for it.
    pub run: Vec<(String, PathBuf)>,
    /// The fake clock's date at the start: seconds since 1970.
    pub utc: u64,
    /// Where the process writes how the run ended ([`Ended`]).
    pub ended: PathBuf,
    /// Where the test's host, and each tool program's, writes each program's process group.
    pub groups: PathBuf,
}

/// How a tool test's run ended, as its process writes it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ended {
    /// Whether the job ended: the test passed.
    pub passed: bool,
    /// How the run ended, in the host's words.
    pub end: String,
}

/// A directory of the test run's own, removed when it's dropped.
struct Dir(PathBuf);

impl Dir {
    fn new() -> std::io::Result<Dir> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir()
            .join(format!("wrela-tool-tests-{}-{n}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(Dir(dir))
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs tool tests `tests` (§13.2) of the package at `root`, its constants computed into
/// `data`: each test's failure, if it failed, and what it printed, with the errors building
/// them found.
pub(crate) fn run(
    checked: &Checked,
    sources: &SourceMap,
    data: &mut BuildData,
    tests: &[FnId],
    root: &Path,
) -> (Vec<Ran>, Vec<Diagnostic>) {
    let mut roots = wrela_lower::Roots::of(checked);
    roots.tool_tests = tests.to_vec();
    let role = std::mem::replace(&mut data.role, Role::Tool);
    let (lowered, mut diags) = wrela_lower::lower(checked, &roots, data, true);
    data.role = role;
    if has_errors(&diags) {
        return (Vec::new(), diags);
    }
    let out = crate::build::emit_headless(&lowered, sources, !data.no_simd, Role::Tool);
    diags.extend(out.diagnostics);
    let Some((_, wasm)) = out.files.into_iter().find(|(p, _)| p == crate::build::TOOL_WASM) else {
        return (Vec::new(), diags);
    };
    let dir = match Dir::new() {
        Ok(d) => d,
        Err(e) => {
            diags.push(Diagnostic::internal(format!("a tool test's directory: {e}")));
            return (Vec::new(), diags);
        }
    };
    let module = dir.0.join("tool.wasm");
    if let Err(e) = std::fs::write(&module, wasm) {
        diags.push(Diagnostic::internal(format!("writing the tool tests' build: {e}")));
        return (Vec::new(), diags);
    }
    // What each test's run needs, apart from the program (which threads don't share).
    let p = &checked.program;
    let each: Vec<(usize, String, Option<wrela_sema::defs::ToolTest>)> = tests
        .iter()
        .enumerate()
        .map(|(i, &f)| (i, p.func(f).name.clone(), p.func(f).attrs.test_tool.clone()))
        .collect();
    let ran = crate::consts::each_at_once(&each, |(i, _, test)| {
        let Some(test) = test else { return Err("it isn't a tool test".to_string()) };
        let Some(runner) = RUNNER.get() else {
            return Err("this process can't run tool tests: `wrela test` runs each in a `wrela` process of its own".into());
        };
        one(runner, &dir.0, &module, *i, test, root)
    });
    let results = tests
        .iter()
        .zip(each.iter().zip(ran))
        .map(|(&f, ((_, name, _), ran))| {
            let fail = |why: String| {
                Diagnostic::new(
                    codes::E0706,
                    p.func(f).name_span,
                    format!("the tool test `{name}` failed: {why}"),
                )
                .with_note("a tool test passes if its job ends; a panic, a trap, its budget running out or the host's failure fail it (§13.2)")
            };
            let (failed, lines) = match ran {
                Ok((ended, lines)) => ((!ended.passed).then(|| fail(ended.end)), lines),
                Err(why) => (Some(fail(why)), Vec::new()),
            };
            (f, failed, wrela_host::Traced { lines, phases: Vec::new() })
        })
        .collect();
    (results, diags)
}

/// Runs tool test `i`, `test`, of the package at `root` in a process of its own, with what its
/// attribute grants, in a directory of its own under `dir`: how it ended, and what it printed.
fn one(
    runner: &Path,
    dir: &Path,
    module: &Path,
    i: usize,
    test: &wrela_sema::defs::ToolTest,
    root: &Path,
) -> Result<(Ended, Vec<String>), String> {
    let own = dir.join(i.to_string());
    let scratch = own.join("scratch");
    std::fs::create_dir_all(&scratch).map_err(|e| format!("its scratch directory: {e}"))?;
    let mut run = Vec::new();
    for (name, package) in &test.run {
        let at = root.join(package);
        if !at.join("wrela.toml").is_file() {
            return Err(format!(
                "its program `{name}` is the tool package `{package}`, and there's no package there"
            ));
        }
        run.push((name.clone(), at));
    }
    let spec = Spec {
        wasm: module.to_path_buf(),
        init: format!("{}.{i}", wrela_lower::TOOL_TEST_INIT),
        resume: format!("{}.{i}", wrela_lower::TOOL_TEST),
        budget: test.budget,
        scratch,
        net: test.net.clone(),
        run,
        utc: START_UTC,
        ended: own.join("ended.json"),
        groups: own.join("groups.txt"),
    };
    let json = serde_json::to_vec(&spec).map_err(|e| format!("its spec: {e}"))?;
    std::fs::write(own.join("test.json"), json).map_err(|e| format!("its spec: {e}"))?;
    // Files, not pipes: nothing the test's process leaves open can hold this one.
    let printed = own.join("printed.txt");
    let stdout = std::fs::File::create(&printed).map_err(|e| format!("its output: {e}"))?;
    let stderr = std::fs::File::create(own.join("errors.txt")).map_err(|e| format!("{e}"))?;
    let mut command = Command::new(runner);
    command.arg("tool").arg("--test").arg(&own);
    command.stdin(Stdio::null()).stdout(stdout).stderr(stderr);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = {
        let mut running = RUNNING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if STOPPING.load(Ordering::Acquire) {
            return Err("`wrela test` was interrupted".into());
        }
        let child = command.spawn().map_err(|e| format!("can't start its process: {e}"))?;
        running.push((child.id() as i32, spec.groups.clone()));
        child
    };
    let group = child.id() as i32;
    let bound = Duration::from_secs_f64(test.budget) + MARGIN;
    let started = Instant::now();
    let killed = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) if started.elapsed() > bound => {
                kill_tree(group, &spec.groups);
                break true;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                kill_tree(group, &spec.groups);
                forget(group);
                return Err(format!("waiting for its process: {e}"));
            }
        }
    };
    let _ = child.wait();
    let lines: Vec<String> =
        std::fs::read_to_string(&printed).unwrap_or_default().lines().map(str::to_string).collect();
    if killed {
        forget(group);
        return Ok((
            Ended {
                passed: false,
                end: format!(
                    "its process was still running {:?} past its budget of {} s, and was killed",
                    MARGIN, test.budget
                ),
            },
            lines,
        ));
    }
    let ended = std::fs::read(&spec.ended).ok().and_then(|b| serde_json::from_slice(&b).ok());
    // A host that ended without saying how didn't end its programs either.
    if killed || ended.is_none() {
        kill_tree(group, &spec.groups);
    }
    forget(group);
    let Some(ended) = ended else {
        let errors = std::fs::read_to_string(own.join("errors.txt")).unwrap_or_default();
        return Err(format!("its process ended without saying how the run did: {}", errors.trim()));
    };
    Ok((ended, lines))
}

/// The tool tests running: each host's process group, and its file of programs' groups.
static RUNNING: Mutex<Vec<(i32, PathBuf)>> = Mutex::new(Vec::new());

/// Set once [`stop_all`] has run: no test starts after it.
static STOPPING: AtomicBool = AtomicBool::new(false);

/// The test whose host leads `group` has ended.
fn forget(group: i32) {
    RUNNING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|(g, _)| *g != group);
}

/// Ends every tool test running, with every process group each started, and starts no more:
/// for `wrela test` on Ctrl-C.
pub fn stop_all() {
    STOPPING.store(true, Ordering::Release);
    let running = RUNNING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for (group, groups) in running.iter() {
        kill_tree(*group, groups);
    }
}

/// Sends `signal` to process group `group`; one that's gone is no error.
fn signal_group(group: i32, signal: rustix::process::Signal) {
    if let Some(pid) = rustix::process::Pid::from_raw(group) {
        let _ = rustix::process::kill_process_group(pid, signal);
    }
}

/// Kills the test host's group `group` and every group in the file `groups`: the host is
/// stopped first, so it starts nothing more, then each group in the file, read again until
/// it's read no new one, and then all are killed.
fn kill_tree(group: i32, groups: &Path) {
    use rustix::process::Signal;
    signal_group(group, Signal::STOP);
    let mut stopped: Vec<i32> = Vec::new();
    for _ in 0..8 {
        let listed: Vec<i32> = std::fs::read_to_string(groups)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .filter(|&g| g > 1)
            .collect();
        let new: Vec<i32> = listed.into_iter().filter(|g| !stopped.contains(g)).collect();
        if new.is_empty() {
            break;
        }
        for &g in &new {
            signal_group(g, Signal::STOP);
        }
        stopped.extend(new);
    }
    for &g in &stopped {
        signal_group(g, Signal::KILL);
    }
    signal_group(group, Signal::KILL);
}
