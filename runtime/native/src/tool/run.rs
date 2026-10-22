//! `std::tool::run` (language.md §15.5): a program the grants name (`--allow-run`), run as a
//! process of its own, with its arguments, its standard input, and what it writes to its
//! standard output and error given back. No shell runs: the program is the executable found
//! when the grants were opened.
//!
//! **What it gets.** An empty environment, then a fixed baseline: `PATH` as it was when the
//! grants were opened, `HOME` a directory the host owns for this run, `LANG=C.UTF-8`; then the
//! request's variables, and the granted secrets it names, by name. It works in a write root the
//! request names. A program runs with the user's authority, natively: the working directory is
//! where it starts, not a confinement.
//!
//! **Its process group is the host's.** The program leads a process group of its own, and the
//! host kills that group (`SIGKILL`) when the program ends, so nothing it started outlives it;
//! past the request's deadline; past [`MOST_OUTPUT`] bytes of output; and when the tool ends.
//! The worker that runs it looks for the tool's end every few milliseconds, so the host's grace
//! is never spent waiting for a program. Its input and output are files in the host's own
//! directory for the run, so no pipe a program holds open can hold up the host; they're
//! removed however the request ends, and the output goes into the answer a chunk at a time,
//! charged to the blob budget as it's read. The deadline counts from when the host took the
//! request: one still queued at it isn't started. Each group is written to the supervisor's
//! file (`Options::groups`), if there is one, as the program starts.
//!
//! A `tool:` program is a tool package, which `wrela tool` runs on the tool host in its process,
//! as any program, with the request's working directory as its one write root (under the same
//! name), its arguments, the secrets the request names, what's left of the request's deadline
//! as its `--timeout`, and the supervisor's file of groups ([`GROUPS_ENV`]).

use super::caps::Program;
use super::services::{Body, Data, Services};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// A program's deadline unless the request asks for another.
const DEADLINE: Duration = Duration::from_secs(60);

/// The most bytes a program may write to its standard output, and to its standard error.
pub(crate) const MOST_OUTPUT: u64 = 256 << 20;

/// How often the worker looks at the program.
const POLL: Duration = Duration::from_millis(10);

/// The environment variable that names a tool child's file of process groups
/// (`wrela tool` reads it), so a supervisor finds its programs' groups too.
pub const GROUPS_ENV: &str = "WRELA_TOOL_GROUPS";

/// How long the worker waits for a killed program's exit to be reported.
const REAP: Duration = Duration::from_secs(1);

/// A request to run a program, as std's `run::run` writes it.
pub(crate) struct Run {
    program: String,
    args: Vec<String>,
    stdin: Data,
    env: Vec<(String, String)>,
    secrets: Vec<String>,
    cwd: String,
    /// How long it may take, and the moment it must be done by: from when the host took it.
    deadline: Duration,
    due: Instant,
}

/// Reads a request, which the host took at `entered`: the program, its arguments, its standard
/// input, its variables, the secrets it's given, the write root it works in, and its deadline.
pub(crate) fn parse(b: &mut Body, entered: Instant) -> Result<Run, String> {
    let program = b.str()?;
    let mut args = Vec::new();
    for _ in 0..b.u32()? {
        args.push(b.str()?);
    }
    let stdin = b.data()?;
    let mut env = Vec::new();
    for _ in 0..b.u32()? {
        env.push((b.str()?, b.str()?));
    }
    let mut secrets = Vec::new();
    for _ in 0..b.u32()? {
        secrets.push(b.str()?);
    }
    let cwd = b.str()?;
    let deadline = super::http::deadline(f64::from_bits(b.u64()?), DEADLINE)?;
    b.end()?;
    for (name, _) in &env {
        if name.is_empty() || name.contains(['=', '\0']) {
            return Err(format!("`{name}` can't name an environment variable"));
        }
    }
    if args.iter().chain(env.iter().map(|(_, v)| v)).any(|a| a.contains('\0')) {
        return Err("an argument or a variable holds a NUL byte".into());
    }
    Ok(Run { program, args, stdin, env, secrets, cwd, deadline, due: entered + deadline })
}

/// What `wrela tool --launch <go> <program> [args...]` does, in the launched process: waits
/// until the file `go` is there (the host has recorded this process's group), at most
/// [`LAUNCH_WAIT`], then takes a process group of its own and becomes `program`. Only an
/// error returns.
pub fn launch(go: &Path, program: &std::ffi::OsStr, args: &[std::ffi::OsString]) -> String {
    let since = Instant::now();
    while !go.exists() {
        if since.elapsed() > LAUNCH_WAIT {
            return "the host never let the program go".into();
        }
        std::thread::sleep(POLL);
    }
    if let Err(e) = rustix::process::setpgid(None, None) {
        return format!("can't take a process group: {e}");
    }
    #[cfg(unix)]
    {
        let e = std::os::unix::process::CommandExt::exec(Command::new(program).args(args));
        format!("can't run {}: {e}", program.to_string_lossy())
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        "launching needs a Unix".into()
    }
}

/// How long a launched program waits for its host to let it go.
const LAUNCH_WAIT: Duration = Duration::from_secs(30);

/// Kills process group `group`, whatever is left of it: a group already gone is no error. On
/// Linux, where the host adopts its descendants once it has started a browser
/// (`PR_SET_CHILD_SUBREAPER`), it then reaps what of the group it adopted, waiting at most
/// [`REAP`], so no zombie of a program's accumulates.
pub(crate) fn kill_group(group: i32) {
    if let Some(pid) = rustix::process::Pid::from_raw(group) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        #[cfg(target_os = "linux")]
        {
            let since = Instant::now();
            while !wrela_chrome::reap_group(pid) && since.elapsed() < REAP {
                std::thread::sleep(POLL);
            }
        }
    }
}

/// Kills `child`'s process group, and waits up to [`REAP`] for its exit to be reported.
fn stop(child: &mut std::process::Child, group: i32) {
    kill_group(group);
    // One still in the host's group, started but not let go, has no group of its own yet.
    let _ = child.kill();
    let since = Instant::now();
    while since.elapsed() < REAP {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(POLL);
    }
}

/// A request's files in the run's directory, removed however the request ends.
pub(crate) struct Files(pub(crate) Vec<std::path::PathBuf>);

impl Drop for Files {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Writes `group` to the supervisor's file of groups, if there is one: before the program can
/// start anything, a supervisor that has to can kill it.
pub(crate) fn record(s: &Services, group: i32) -> Result<(), String> {
    let Some(path) = &s.groups else { return Ok(()) };
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("the file of process groups: {e}"))?;
    std::io::Write::write_all(&mut f, format!("{group}\n").as_bytes())
        .map_err(|e| format!("the file of process groups: {e}"))
}

/// The size of the file at `path`, 0 if it can't be read.
fn size(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

/// A command that runs `exe` with `argv` in a process group the host owns: in a group of its
/// own, or, under a supervisor, in this host's group through `wrela tool --launch`, which waits
/// until the file `go` is there (its group is recorded), then takes a group of its own and
/// becomes the program. So there's no moment when it has a group nobody knows of.
pub(crate) fn owned_command(
    s: &Services,
    exe: &Path,
    argv: Vec<std::ffi::OsString>,
    go: &Path,
) -> Result<Command, String> {
    match (&s.groups, s.caps.runner()) {
        (None, _) => {
            let mut c = Command::new(exe);
            c.args(&argv);
            #[cfg(unix)]
            std::os::unix::process::CommandExt::process_group(&mut c, 0);
            Ok(c)
        }
        (Some(_), Some(runner)) => {
            let mut c = Command::new(runner);
            c.arg("tool").arg("--launch").arg(go).arg(exe).args(argv);
            Ok(c)
        }
        (Some(_), None) => {
            Err("a supervised program needs `wrela tool --launch`, and nothing here runs it".into())
        }
    }
}

/// Starts `command` (an [`owned_command`]), records its process group for a supervisor, and lets
/// it go: the child and its group. `program` names it in errors.
pub(crate) fn start_owned(
    s: &Services,
    command: &mut Command,
    go: &Path,
    program: &str,
) -> Result<(std::process::Child, i32), String> {
    // While no pipe is being made: a pipe's ends kept by this program could hold up its owner.
    let spawned = {
        let _spawning = wrela_chrome::spawning();
        command.spawn()
    };
    let mut child = spawned.map_err(|e| format!("can't run `{program}`: {e}"))?;
    let group = let_go(s, &mut child, go)?;
    Ok((child, group))
}

/// `child`, an [`owned_command`] just started, recorded for a supervisor and let go: the process
/// group it leads. If it can't be, it's killed.
pub(crate) fn let_go(
    s: &Services,
    child: &mut std::process::Child,
    go: &Path,
) -> Result<i32, String> {
    let group = child.id() as i32;
    // A test's moment between the start and the record, when only this host's group holds it.
    std::thread::sleep(s.spawn_pause);
    if let Err(why) = record(s, group) {
        stop(child, group);
        return Err(why);
    }
    if s.groups.is_some()
        && let Err(e) = std::fs::write(go, b"")
    {
        stop(child, group);
        return Err(format!("letting the program go: {e}"));
    }
    Ok(group)
}

/// Waits for `child`, which leads process group `group`, looking every few milliseconds for the
/// tool's end and for `stop`'s reason to end it: its exit status, its group killed as it ends
/// (so nothing it started outlives it), or why the host killed the group first.
pub(crate) fn wait_owned(
    s: &Services,
    child: &mut std::process::Child,
    group: i32,
    program: &str,
    mut stop_now: impl FnMut() -> Option<String>,
) -> Result<std::process::ExitStatus, String> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // What it started and left running goes with it.
                kill_group(group);
                return Ok(status);
            }
            Ok(None) => {}
            Err(e) => {
                stop(child, group);
                return Err(format!("waiting for `{program}`: {e}"));
            }
        }
        if s.closing.load(Ordering::Acquire) {
            stop(child, group);
            return Err(format!("the tool ended before `{program}` did"));
        }
        if let Some(why) = stop_now() {
            stop(child, group);
            return Err(why);
        }
        std::thread::sleep(POLL);
    }
}

/// Carries out `r` for `request`: the program's status (its exit code, or minus the signal that
/// ended it), then its standard output and standard error, each bytes or a blob.
pub(crate) fn run(s: &Services, request: u32, r: Run) -> Result<Vec<u8>, String> {
    let program = s.caps.program(&r.program)?.clone();
    let cwd = s.caps.write_root_dir(&r.cwd)?.to_path_buf();
    let mut secrets = Vec::new();
    for name in &r.secrets {
        let Some(value) = s.caps.secret(name) else {
            return Err(format!("the secret `{name}` isn't granted: `--secret {name}` grants it"));
        };
        secrets.push((name.clone(), value.to_string()));
    }
    // Queued past its deadline (the workers were busy): it isn't started.
    if Instant::now() >= r.due {
        return Err(format!(
            "`{}` didn't start within its deadline of {} s",
            r.program,
            r.deadline.as_secs_f64()
        ));
    }
    let dir = s.work_dir()?;
    let (input, output, errors) = (
        dir.join(format!("{request}.in")),
        dir.join(format!("{request}.out")),
        dir.join(format!("{request}.err")),
    );
    let go = dir.join(format!("{request}.go"));
    let _files = Files(vec![input.clone(), output.clone(), errors.clone(), go.clone()]);
    let stdin = match &r.stdin {
        Data::Bytes(b) => std::fs::write(&input, b),
        Data::Blob(id) => {
            let bytes = s.blobs().get(*id)?;
            std::fs::write(&input, bytes.as_slice())
        }
    };
    stdin.map_err(|e| format!("the program's input: {e}"))?;
    let opened = |path: &Path, create: bool| {
        let f = if create { std::fs::File::create(path) } else { std::fs::File::open(path) };
        f.map_err(|e| format!("the program's input or output: {e}"))
    };
    // What runs: the executable and its arguments.
    let (exe, argv): (std::path::PathBuf, Vec<std::ffi::OsString>) = match &program {
        Program::Native(path) => (path.clone(), r.args.iter().map(Into::into).collect()),
        Program::Tool { runner, package } => {
            let left = r.due.saturating_duration_since(Instant::now());
            let mut a: Vec<std::ffi::OsString> = vec!["tool".into(), package.into()];
            a.push("--write".into());
            a.push(format!("{}={}", r.cwd, cwd.display()).into());
            a.push("--timeout".into());
            a.push(format!("{}", left.as_secs_f64().max(0.001)).into());
            for (name, _) in &secrets {
                a.push("--secret".into());
                a.push(name.into());
            }
            a.push("--".into());
            a.extend(r.args.iter().map(Into::into));
            (runner.clone(), a)
        }
    };
    let mut command = owned_command(s, &exe, argv, &go)?;
    command.env_clear();
    command.env("PATH", s.caps.path_baseline());
    command.env("HOME", dir.join("home"));
    command.env("LANG", "C.UTF-8");
    for (name, value) in r.env.iter().chain(&secrets) {
        command.env(name, value);
    }
    // A tool child writes its own programs' groups where this host writes its.
    if let (Program::Tool { .. }, Some(groups)) = (&program, &s.groups) {
        command.env(GROUPS_ENV, groups);
    }
    command
        .current_dir(&cwd)
        .stdin(opened(&input, false)?)
        .stdout(opened(&output, true)?)
        .stderr(opened(&errors, true)?);
    let (mut child, group) = start_owned(s, &mut command, &go, &r.program)?;
    let status = wait_owned(s, &mut child, group, &r.program, || {
        if Instant::now() >= r.due {
            return Some(format!(
                "`{}` ran past its deadline of {} s, and the host killed its process group",
                r.program,
                r.deadline.as_secs_f64()
            ));
        }
        if size(&output) > MOST_OUTPUT || size(&errors) > MOST_OUTPUT {
            return Some(format!(
                "`{}` wrote more than {MOST_OUTPUT} bytes of output, and the host killed its process group",
                r.program
            ));
        }
        None
    })?;
    let code = match status.code() {
        Some(c) => c,
        None => {
            #[cfg(unix)]
            {
                -std::os::unix::process::ExitStatusExt::signal(&status).unwrap_or(0)
            }
            #[cfg(not(unix))]
            {
                -1
            }
        }
    };
    let mut out = (code as u32).to_le_bytes().to_vec();
    // Into the answer a chunk at a time, charged to the blob budget as they're read.
    for (path, what) in [(&output, "its standard output"), (&errors, "its standard error")] {
        let mut f = std::fs::File::open(path).map_err(|e| format!("the program's output: {e}"))?;
        s.put_read(request, &mut out, &mut f, MOST_OUTPUT, &format!("`{}`'s {what}", r.program))?;
    }
    Ok(out)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A program starts only while no pipe is being made (`wrela_chrome::spawning`): one started
    /// in the moment a pipe's ends aren't yet close-on-exec (macOS) would keep them open.
    #[test]
    fn a_program_waits_to_start_while_a_pipe_is_being_made() {
        let s = super::super::services::for_test(super::super::BlobBudget::default());
        let making = wrela_chrome::spawning();
        let (started, seen) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut c = Command::new("true");
                let go = std::env::temp_dir().join("wrela-spawning-go");
                let (mut child, _) = start_owned(&s, &mut c, &go, "true").expect("started");
                let _ = child.wait();
                let _ = started.send(());
            });
            assert!(seen.recv_timeout(Duration::from_millis(300)).is_err(), "it waited");
            drop(making);
            assert!(seen.recv_timeout(Duration::from_secs(10)).is_ok(), "then it started");
        });
    }
}
