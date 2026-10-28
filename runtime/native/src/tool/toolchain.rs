//! `std::tool::toolchain` (language.md §15.5): builds, tests, checks, formatting and the
//! reference docs of packages in a tool's roots, by the compiler's own code in this process.
//! The host has no compiler: whoever runs the tool host gives it one ([`Toolchain`], which
//! `wrela tool` makes from the compiler's driver). This module is the service's mechanism: it
//! reads requests as std writes them, finds their packages and outputs through the grants, runs
//! the compiler's work beside the tool, writes its files through the grants, and answers.
//!
//! **One capability context for every read.** A package and its output are root paths
//! (`NAME/rel`). The compiler is given the same roots the grants opened, as directory handles
//! ([`Job::roots`]), and reads every file through them: the package's sources and manifest, its
//! dependencies' (a dependency outside the roots is an error), the workspace's manifest and its
//! members', and what `embed` reads. Its constants' cache is a directory the host owns
//! ([`Job::cache`]), never one beside a read-only package. Its outputs are written through the
//! grants, into a write root.
//!
//! **Every request can be stopped.** The compiler's work runs on a thread of its own; the worker
//! that waits for it looks for the request's deadline (from when the host took it) and the
//! tool's end every few milliseconds. Then it cancels the work, which the compiler sees between
//! its phases and, in a constant it computes, within about 10 ms; a program the work runs
//! (`wasm-opt`) has its process group killed at once ([`Job::run`]). If the work hasn't stopped
//! within the grace after that, the run is the host's failure (status 71), as an HTTP client
//! that won't return is, once the tool has had its grace to say why its request failed. Files a
//! failed build wrote are removed. A request may ask for a deadline as long as the run's own
//! budget.

use super::caps::MOST_BYTES;
use super::services::{Body, Services, Task, Work, err};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A request's deadline unless it asks for another: builds of large packages take minutes.
const DEADLINE: Duration = Duration::from_secs(300);

/// The longest deadline a request may ask for: 600 s, or the run's whole budget if that is
/// longer, so a tool with an hour can give one build most of it.
fn longest(s: &Services) -> f64 {
    s.budget.map_or(super::http::LONGEST, |b| b.as_secs_f64().max(super::http::LONGEST))
}

/// The deadline a request asks for, in seconds, as the host takes it: in a run with a budget,
/// one longer than [`longest`] is cut to it, since the run ends by then anyway (so a tool that
/// fits its builds to its declared budget still runs with `--timeout` narrowed). Without one,
/// as asked, and past 600 s an error.
fn asked(s: &Services, seconds: f64) -> f64 {
    match s.budget {
        Some(_) if seconds > longest(s) => longest(s),
        _ => seconds,
    }
}

/// How often the worker looks at the work it waits for.
const POLL: Duration = Duration::from_millis(10);

/// The most bytes a source file to format, or a build's output file read back, may have.
const MOST_SOURCE: u64 = 64 << 20;

/// The most diagnostics an answer carries, and the most bytes of each one's text: so an answer
/// stays a bounded size, whatever the program.
const MOST_DIAGNOSTICS: usize = 256;
const MOST_TEXT: usize = 16 << 10;

/// The most lines of a test's output an answer carries.
const MOST_LINES: usize = 256;

/// The list of files a build wrote into its output directory, which `wrela build` keeps too.
pub const LIST: &str = ".wrela-build";

/// The compiler, as the tool host runs it: each method does one request's work, reading only
/// through the job's roots and stopping when the job is cancelled. None writes a file: the host
/// writes what they give, through the grants.
pub trait Toolchain: Send + Sync {
    /// The build of the package in `package`: its files by their paths in the build, or its
    /// diagnostics. `wasm-opt` runs through [`Job::run`].
    fn build(&self, job: &Job, package: &Path, options: &BuildOptions) -> Result<Built, String>;
    /// The package's tests whose names hold `filter` (all of them for `None`), run as
    /// `wrela test` runs them.
    fn test(
        &self,
        job: &Job,
        package: &Path,
        filter: Option<&str>,
        overrides: &[(String, PathBuf)],
    ) -> Result<Tested, String>;
    /// The package's diagnostics, as `wrela check` finds them, and the pipelines its build
    /// makes, as `wrela pipelines` counts them.
    fn check(
        &self,
        job: &Job,
        package: &Path,
        overrides: &[(String, PathBuf)],
    ) -> Result<Checked, String>;
    /// Whether a build writes a file named `name` into its output: what the list of the files
    /// the last build wrote ([`LIST`]) may name (`wrela build`'s rule).
    fn owns(&self, name: &str) -> bool;
    /// The `.wrela` files under `dir` that `wrela fmt` formats, by their real paths.
    fn sources(&self, job: &Job, dir: &Path) -> Result<Vec<PathBuf>, String>;
    /// `text` formatted, or its syntax errors, rendered.
    fn format(&self, text: &str) -> Result<String, String>;
    /// The reference docs of std and `packages` (`wrela doc --export`): their files by their
    /// paths in the export, with the source in `repository` at `revision`, each package's files
    /// named by their paths from `base`.
    fn doc_export(
        &self,
        job: &Job,
        packages: &[PathBuf],
        repository: &str,
        revision: Option<&str>,
        base: &Path,
    ) -> Result<Vec<(String, Vec<u8>)>, String>;
    /// The answers to `queries` on the package in `package` (`wrela query --json`'s JSON), its
    /// dependencies read from other directories as `overrides` says.
    fn query(
        &self,
        job: &Job,
        package: &Path,
        queries: &[String],
        overrides: &[(String, PathBuf)],
    ) -> Result<String, String>;
    /// Each file a lifted build's report (`lift.json`'s text) lifted, by its path in the report
    /// (relative to the program's package), and each literal's value as the build has it.
    fn lifted(&self, report: &str) -> Result<(Vec<String>, Vec<f32>), String>;
    /// The values of a lifted build's literals in its lifted files as they are now, each read
    /// by `read` by its path in the report: `None` if a file changed past its literals.
    fn literal_values(
        &self,
        report: &str,
        read: &dyn Fn(&str) -> Result<String, String>,
    ) -> Result<Option<Vec<f32>>, String>;
    /// Edits of a lifted build's literals, each by its index in the build and its new value,
    /// planned by `wrela edit`'s code against the files as they are now (each read by `read`):
    /// what to write, or why they're refused.
    fn plan_literal_edits(
        &self,
        report: &str,
        read: &dyn Fn(&str) -> Result<String, String>,
        edits: &[(u32, f32)],
    ) -> Result<LiteralEdits, String>;
}

/// Planned edits of a lifted build's literals: each file's path in the report, the hash of its
/// text they were planned against (`std::lift::file_hash`'s) and its new text, and the answer
/// once they're written (`wrela edit --json`'s, on one line); or, if they're refused, the
/// refusal's answer, and no files.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiteralEdits {
    pub files: Vec<(String, String, String)>,
    pub answer: String,
    pub refused: bool,
}

/// How to build: through `wasm-opt`, lifting these packages, with these dependencies read from
/// other directories (by name, wherever declared: a program written around another package), with SIMD or not, as a
/// debug build or not, as a test build (with the program's `@testing` exports) or not.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BuildOptions {
    pub optimize: bool,
    pub lift: Vec<String>,
    pub overrides: Vec<(String, PathBuf)>,
    pub simd: bool,
    pub debug: bool,
    pub testing: bool,
    /// The program's constants set (`wrela build --set NAME=VALUE`).
    pub set: Vec<(String, String)>,
}

/// A diagnostic, as the compiler gives it: whether it's an error, its code, its message, where
/// (`file:line:column`, or empty), and the whole of it as `wrela check` shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diagnostic {
    pub error: bool,
    pub code: String,
    pub message: String,
    pub at: String,
    pub rendered: String,
}

/// A build: its files (empty if it has errors), its diagnostics, and how many constants it
/// computed and read from the cache.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Built {
    pub files: Vec<(String, Vec<u8>)>,
    pub diagnostics: Vec<Diagnostic>,
    pub consts: (u32, u32),
}

/// A check: its diagnostics, and (with no errors) the pipelines the build makes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Checked {
    pub diagnostics: Vec<Diagnostic>,
    pub pipelines: Vec<Pipeline>,
}

/// A pipeline of a build, as `wrela pipelines --json` gives it: `compute` or `render`, its
/// entry points with their type arguments (`shade::<f32>`), and where CPU code dispatches or
/// draws it (`file:line:column`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pipeline {
    pub kind: String,
    pub entries: Vec<String>,
    pub sites: Vec<String>,
}

/// A test run: each test's result, the errors that kept the tests from running, and how many
/// tests the filter left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tested {
    pub results: Vec<TestResult>,
    pub diagnostics: Vec<Diagnostic>,
    pub filtered_out: u32,
}

/// One test's result: its name, where it is, whether it passed, why it failed (rendered), and
/// what it printed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestResult {
    pub name: String,
    pub at: String,
    pub passed: bool,
    pub message: String,
    pub printed: Vec<String>,
}

/// One request's work, as the compiler gets it: the roots it reads, the cache it keeps, the
/// flag that cancels it, and the programs it runs.
pub struct Job {
    services: Arc<Services>,
    request: u32,
    cancel: Arc<AtomicBool>,
    cache: PathBuf,
    /// When the request must be done by.
    due: Instant,
}

impl Job {
    /// The roots the tool was granted, each its real path and a handle of its own to its
    /// directory: every read of the compiler's goes through them.
    pub fn roots(&self) -> Result<Vec<(PathBuf, cap_std::fs::Dir)>, String> {
        self.services.caps.readable()
    }

    /// The directory the host owns for the compiler's constants' cache.
    pub fn cache(&self) -> &Path {
        &self.cache
    }

    /// The flag set when the work is to stop: past its deadline, or the tool ended.
    pub fn cancel(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// Whether the work is to stop.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    /// Runs `program` with `args` in a process group the host owns, recorded for a supervisor,
    /// and waits for it: whether it succeeded, its status, its standard output and error. The
    /// group is killed when the program ends, when the job is cancelled, past the request's
    /// deadline, and when the tool ends.
    pub fn run(
        &self,
        program: &Path,
        args: &[std::ffi::OsString],
    ) -> Result<(bool, String, Vec<u8>, Vec<u8>), String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::AcqRel);
        let s = &self.services;
        let name: String =
            program.file_name().map_or("a program".into(), |n| n.to_string_lossy().into());
        let dir = s.work_dir()?;
        let stem = format!("toolchain-{}-{n}", self.request);
        let (out, err, go) = (
            dir.join(format!("{stem}.out")),
            dir.join(format!("{stem}.err")),
            dir.join(format!("{stem}.go")),
        );
        let _files = super::run::Files(vec![out.clone(), err.clone(), go.clone()]);
        let file =
            |p: &Path| std::fs::File::create(p).map_err(|e| format!("`{name}`'s output: {e}"));
        let mut command = super::run::owned_command(s, program, args.to_vec(), &go)?;
        command.stdin(std::process::Stdio::null()).stdout(file(&out)?).stderr(file(&err)?);
        let (mut child, group) = super::run::start_owned(s, &mut command, &go, &name)?;
        let status = super::run::wait_owned(s, &mut child, group, &name, || {
            if self.cancelled() || Instant::now() >= self.due {
                return Some(format!(
                    "`{name}` was stopped with the request, and the host killed its process group"
                ));
            }
            let big = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len()) > MOST_SOURCE;
            if big(&out) || big(&err) {
                return Some(format!("`{name}` wrote more than {MOST_SOURCE} bytes of output"));
            }
            None
        })?;
        let read = |p: &Path| std::fs::read(p).map_err(|e| format!("`{name}`'s output: {e}"));
        Ok((status.success(), status.to_string(), read(&out)?, read(&err)?))
    }
}

/// What the service shares between requests: the compiler, and the cache's directory.
pub(crate) struct Service {
    pub toolchain: Arc<dyn Toolchain>,
    pub cache: Option<PathBuf>,
}

/// Bytes of an answer, written as std's readers read them.
#[derive(Default)]
pub(crate) struct Out(pub Vec<u8>);

impl Out {
    pub(crate) fn u8(&mut self, v: u8) -> &mut Out {
        self.0.push(v);
        self
    }

    pub(crate) fn u32(&mut self, v: u32) -> &mut Out {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub(crate) fn u64(&mut self, v: u64) -> &mut Out {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub(crate) fn f64(&mut self, v: f64) -> &mut Out {
        self.u64(v.to_bits())
    }

    pub(crate) fn str(&mut self, v: &str) -> &mut Out {
        self.u32(v.len() as u32);
        self.0.extend_from_slice(v.as_bytes());
        self
    }

    /// A count, then each of `items`.
    pub(crate) fn strs(&mut self, items: &[impl AsRef<str>]) -> &mut Out {
        self.u32(items.len() as u32);
        for item in items {
            self.str(item.as_ref());
        }
        self
    }
}

/// `text`, at most [`MOST_TEXT`] bytes of it (cut at a character), with a note if it was cut.
pub(crate) fn bounded(text: &str) -> String {
    bounded_to(text, MOST_TEXT)
}

/// `text`, at most `most` bytes of it (cut at a character), with a note if it was cut.
pub(crate) fn bounded_to(text: &str, most: usize) -> String {
    if text.len() <= most {
        return text.to_string();
    }
    let end = text.floor_char_boundary(most);
    format!("{}… ({} more bytes)", &text[..end], text.len() - end)
}

/// Writes `diagnostics`, at most [`MOST_DIAGNOSTICS`] of them, errors first: a count, then each
/// as `toolchain::read_diagnostic` reads it.
fn write_diagnostics(out: &mut Out, diagnostics: &[Diagnostic]) {
    let mut ordered: Vec<&Diagnostic> = diagnostics.iter().filter(|d| d.error).collect();
    ordered.extend(diagnostics.iter().filter(|d| !d.error));
    ordered.truncate(MOST_DIAGNOSTICS);
    out.u32(ordered.len() as u32);
    for d in ordered {
        out.u8(u8::from(d.error))
            .str(&bounded(&d.code))
            .str(&bounded(&d.message))
            .str(&bounded(&d.at))
            .str(&bounded(&d.rendered));
    }
}

/// A request's text list: a count, then each.
fn strs(b: &mut Body) -> Result<Vec<String>, String> {
    let n = b.u32()?;
    if n as usize > MOST_BYTES {
        return Err("a list longer than the request".into());
    }
    (0..n).map(|_| b.str()).collect()
}

/// How request `name` of the toolchain, with `b`, is carried out.
pub(crate) fn route(s: &Services, name: &str, b: &mut Body) -> Work {
    let Some(service) = &s.toolchain else {
        return Work::Now(err("this tool host has no toolchain: `wrela tool` gives one"));
    };
    let toolchain = service.toolchain.clone();
    let entered = Instant::now();
    let parsed: Result<Op, String> = (|| {
        let op = match name {
            "build" => {
                let package = b.str()?;
                let out = b.str()?;
                let (optimize, simd, debug) = (b.u8()? != 0, b.u8()? != 0, b.u8()? != 0);
                let testing = b.u8()? != 0;
                let lift = strs(b)?;
                let overrides = overrides_of(b)?;
                // The constants set: name and value, as `overrides` holds its pairs.
                let set = overrides_of(b)?;
                Op::Build { package, out, optimize, simd, debug, testing, lift, overrides, set }
            }
            "test" => {
                let package = b.str()?;
                let filter = b.str()?;
                let overrides = overrides_of(b)?;
                Op::Test { package, filter: (!filter.is_empty()).then_some(filter), overrides }
            }
            "check" => {
                let package = b.str()?;
                Op::Check { package, overrides: overrides_of(b)? }
            }
            "fmt" => {
                let paths = strs(b)?;
                let check = b.u8()? != 0;
                Op::Fmt { paths, check }
            }
            "doc_export" => {
                let packages = strs(b)?;
                let out = b.str()?;
                let repository = b.str()?;
                let revision = b.str()?;
                Op::Doc { packages, out, repository, revision }
            }
            "query" => {
                let package = b.str()?;
                let queries = strs(b)?;
                Op::Query { package, queries, overrides: overrides_of(b)? }
            }
            "literals" => Op::Literals { program: b.str()?, build: b.str()? },
            "edit" => {
                let program = b.str()?;
                let build = b.str()?;
                let n = b.u32()?;
                if n as usize > MOST_BYTES / 8 {
                    return Err("more edits than the request holds".into());
                }
                let mut edits = Vec::new();
                for _ in 0..n {
                    edits.push((b.u32()?, f32::from_bits(b.u32()?)));
                }
                Op::Edit { program, build, edits }
            }
            _ => return Err(format!("the toolchain has no `{name}`")),
        };
        Ok(op)
    })();
    let deadline = parsed.and_then(|op| {
        let deadline =
            super::http::deadline_within(asked(s, f64::from_bits(b.u64()?)), DEADLINE, longest(s))?;
        b.end()?;
        Ok((op, deadline))
    });
    let (op, deadline) = match deadline {
        Ok(v) => v,
        Err(why) => return Work::Now(err(&why)),
    };
    let task: Task = Box::new(move |s, request| {
        let due = entered + deadline;
        super::services::answer(carry_out(s, request, toolchain, op, due, deadline))
    });
    Work::Heavy(task)
}

/// A toolchain request, read.
enum Op {
    Build {
        package: String,
        out: String,
        optimize: bool,
        simd: bool,
        debug: bool,
        testing: bool,
        lift: Vec<String>,
        overrides: Vec<(String, String)>,
        set: Vec<(String, String)>,
    },
    Test {
        package: String,
        filter: Option<String>,
        overrides: Vec<(String, String)>,
    },
    Check {
        package: String,
        overrides: Vec<(String, String)>,
    },
    Fmt {
        paths: Vec<String>,
        check: bool,
    },
    Doc {
        packages: Vec<String>,
        out: String,
        repository: String,
        revision: String,
    },
    Query {
        package: String,
        queries: Vec<String>,
        overrides: Vec<(String, String)>,
    },
    Literals {
        program: String,
        build: String,
    },
    Edit {
        program: String,
        build: String,
        edits: Vec<(u32, f32)>,
    },
}

/// Carries out `op` for `request` by `deadline` (`due`): the answer's payload, or why it failed.
fn carry_out(
    s: &Arc<Services>,
    request: u32,
    toolchain: Arc<dyn Toolchain>,
    op: Op,
    due: Instant,
    deadline: Duration,
) -> Result<Vec<u8>, String> {
    if Instant::now() >= due {
        return Err(format!(
            "the request didn't start within its deadline of {} s",
            deadline.as_secs_f64()
        ));
    }
    let cache = match s.toolchain.as_ref().and_then(|t| t.cache.clone()) {
        Some(c) => c,
        None => s.work_dir()?.join("consts"),
    };
    let job =
        Job { services: s.clone(), request, cancel: Arc::new(AtomicBool::new(false)), cache, due };
    let cancel = job.cancel.clone();
    let (sent, got) = std::sync::mpsc::channel();
    let services = s.clone();
    let spawned =
        std::thread::Builder::new().name(format!("wrela-toolchain-{request}")).spawn(move || {
            let run = std::panic::AssertUnwindSafe(|| work(&services, &job, &*toolchain, op));
            let result = std::panic::catch_unwind(run)
                .unwrap_or_else(|_| Err("the compiler failed (it panicked)".into()));
            let _ = sent.send(result);
        });
    let thread = spawned.map_err(|e| format!("can't start the toolchain's thread: {e}"))?;
    let stopped = loop {
        match got.recv_timeout(POLL) {
            Ok(result) => {
                let _ = thread.join();
                return result;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                return Err("the toolchain's thread ended without an answer".into());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if s.closing.load(Ordering::Acquire) {
            // Half the grace: the pool waits for this worker the whole grace.
            break ("the tool ended before the toolchain's work did".to_string(), s.grace / 2);
        }
        if Instant::now() >= due {
            let why = format!(
                "the toolchain's work ran past its deadline of {} s, and the host stopped it",
                deadline.as_secs_f64()
            );
            break (why, s.grace);
        }
    };
    let (why, grace) = stopped;
    cancel.store(true, Ordering::Release);
    if got.recv_timeout(grace).is_ok() {
        let _ = thread.join();
        return Err(why);
    }
    // The compiler is in a phase that doesn't stop: the run can't end cleanly.
    let _ = s.stuck.set(format!(
        "the toolchain's work for request {request} didn't stop within {grace:?} of being cancelled"
    ));
    crate::locked(&s.strays).push(thread);
    Err(why)
}

/// The work of `op`, on the toolchain's thread.
fn work(s: &Services, job: &Job, toolchain: &dyn Toolchain, op: Op) -> Result<Vec<u8>, String> {
    let caps = &s.caps;
    let mut out = Out::default();
    match op {
        Op::Build { package, out: into, optimize, simd, debug, testing, lift, overrides, set } => {
            let dir = caps.dir_path(&package, false)?;
            // The output's root is a write root, checked before any work.
            caps.writable(&into)?;
            let overrides = override_dirs(caps, overrides)?;
            let options = BuildOptions { optimize, lift, simd, debug, testing, overrides, set };
            let built = toolchain.build(job, &dir, &options)?;
            if job.cancelled() {
                return Err(CANCELLED.into());
            }
            let ok = !built.diagnostics.iter().any(|d| d.error);
            let written = write_build(s, job, toolchain, &into, &built.files, ok)?;
            out.u8(u8::from(ok)).strs(&written);
            write_diagnostics(&mut out, &built.diagnostics);
            out.u32(built.consts.0).u32(built.consts.1);
        }
        Op::Test { package, filter, overrides } => {
            let dir = caps.dir_path(&package, false)?;
            let dirs = override_dirs(caps, overrides)?;
            let tested = toolchain.test(job, &dir, filter.as_deref(), &dirs)?;
            let passed = !tested.diagnostics.iter().any(|d| d.error)
                && tested.results.iter().all(|r| r.passed);
            out.u8(u8::from(passed)).u32(tested.results.len() as u32);
            for r in &tested.results {
                out.str(&r.name).str(&r.at).u8(u8::from(r.passed)).str(&bounded(&r.message));
                let lines = &r.printed[..r.printed.len().min(MOST_LINES)];
                out.u32(lines.len() as u32);
                for l in lines {
                    out.str(&bounded(l));
                }
            }
            write_diagnostics(&mut out, &tested.diagnostics);
            out.u32(tested.filtered_out);
        }
        Op::Check { package, overrides } => {
            let dir = caps.dir_path(&package, false)?;
            let dirs = override_dirs(caps, overrides)?;
            let checked = toolchain.check(job, &dir, &dirs)?;
            out.u8(u8::from(!checked.diagnostics.iter().any(|d| d.error)));
            write_diagnostics(&mut out, &checked.diagnostics);
            out.u32(checked.pipelines.len() as u32);
            for p in &checked.pipelines {
                out.str(&p.kind).strs(&p.entries).strs(&p.sites);
            }
        }
        Op::Fmt { paths, check } => {
            let (changed, failed) = format(s, job, toolchain, &paths, check)?;
            out.strs(&changed).u32(failed.len() as u32);
            for (p, why) in &failed {
                out.str(p).str(&bounded(why));
            }
        }
        Op::Doc { packages, out: into, repository, revision } => {
            let mut dirs = Vec::new();
            for p in &packages {
                dirs.push(caps.dir_path(p, false)?);
            }
            // Files are named by their paths in their roots.
            let base = match (packages.first(), dirs.first()) {
                (Some(p), Some(d)) => {
                    let depth = p.split('/').count() - 1;
                    d.ancestors().nth(depth).map_or_else(|| d.clone(), Path::to_path_buf)
                }
                _ => PathBuf::new(),
            };
            let revision = (!revision.is_empty()).then_some(revision.as_str());
            if caps.list(&into).is_ok_and(|e| !e.is_empty()) {
                return Err(format!("`{into}` isn't empty: export into a new directory"));
            }
            caps.writable(&into)?;
            let files = toolchain.doc_export(job, &dirs, &repository, revision, &base)?;
            let mut written = Written::new(s);
            written.make_dirs(&into)?;
            for (path, bytes) in &files {
                written.write(job, format!("{into}/{path}"), bytes)?;
            }
            written.finish(job)?;
            out.u32(files.len() as u32);
        }
        Op::Query { package, queries, overrides } => {
            let dir = caps.dir_path(&package, false)?;
            let dirs = override_dirs(caps, overrides)?;
            out.str(&toolchain.query(job, &dir, &queries, &dirs)?);
        }
        Op::Literals { program, build } => {
            let lifted = Lifted::open(s, &program, &build, false)?;
            let (paths, built) = toolchain.lifted(&lifted.report)?;
            let mut roots = Vec::with_capacity(paths.len());
            for p in &paths {
                roots.push(lifted.root_path(p)?);
            }
            let values = toolchain.literal_values(&lifted.report, &|p| lifted.read(p))?;
            out.strs(&roots).u32(built.len() as u32);
            for x in built {
                out.u32(x.to_bits());
            }
            match values {
                Some(v) => {
                    out.u8(1).u32(v.len() as u32);
                    for x in v {
                        out.u32(x.to_bits());
                    }
                }
                None => {
                    out.u8(0);
                }
            }
        }
        Op::Edit { program, build, edits } => {
            let lifted = Lifted::open(s, &program, &build, true)?;
            let planned =
                toolchain.plan_literal_edits(&lifted.report, &|p| lifted.read(p), &edits)?;
            if job.cancelled() {
                return Err(CANCELLED.into());
            }
            out.str(&lifted.write(job, &planned)?);
        }
    }
    Ok(out.0)
}

/// A lifted build and its program, for literal edits and values: the build's report
/// (`lift.json`), and the program's package's real directory, which the report's paths are
/// relative to. Each file is reached through its root (a write root, to edit), one part at a
/// time following no link.
struct Lifted<'a> {
    s: &'a Services,
    report: String,
    program: PathBuf,
    write: bool,
}

impl<'a> Lifted<'a> {
    fn open(
        s: &'a Services,
        program: &str,
        build: &str,
        write: bool,
    ) -> Result<Lifted<'a>, String> {
        let program = s.caps.dir_path(program, false)?;
        let bytes = s.caps.read_confined(&format!("{build}/lift.json"), MOST_SOURCE)?;
        let report =
            String::from_utf8(bytes).map_err(|_| format!("`{build}/lift.json` isn't UTF-8"))?;
        Ok(Lifted { s, report, program, write })
    }

    /// The root path of the file at `rel` from the program's package.
    fn root_path(&self, rel: &str) -> Result<String, String> {
        let mut real = self.program.clone();
        for part in rel.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if !real.pop() {
                        return Err(format!("`{rel}` leads above the file system's root"));
                    }
                }
                p => real.push(p),
            }
        }
        self.s.caps.root_path_of(&real, self.write)
    }

    /// The text of the file at `rel` from the program's package.
    fn read(&self, rel: &str) -> Result<String, String> {
        let path = self.root_path(rel)?;
        let bytes = self.s.caps.read_confined(&path, MOST_SOURCE)?;
        String::from_utf8(bytes).map_err(|_| format!("`{path}` isn't UTF-8"))
    }

    /// Writes `planned`, unless it's a refusal, or a file has changed since it was planned
    /// (then nothing is written): the answer the edits give. The request's cancellation is
    /// looked at before each file is read, and right before each file takes its old one's place:
    /// a cancelled edit replaces no more files, and fails.
    fn write(&self, job: &Job, planned: &LiteralEdits) -> Result<String, String> {
        if planned.refused {
            return Ok(planned.answer.clone());
        }
        let failed = |why: String| Ok(format!("{{\"why\":{},\"written\":false}}", json_text(&why)));
        let mut paths = Vec::with_capacity(planned.files.len());
        for (rel, hash, _) in &planned.files {
            if job.cancelled() {
                return Err(CANCELLED.into());
            }
            let checked = self.root_path(rel).and_then(|path| {
                let now = self.s.caps.read_confined(&path, MOST_SOURCE)?;
                let mut h = wrela_abi::hash::StateHash::new();
                h.update(&now);
                if h.hex() != *hash {
                    return Err(format!(
                        "{rel} changed since the edits were planned: nothing is written"
                    ));
                }
                Ok(path)
            });
            match checked {
                Ok(path) => paths.push(path),
                Err(why) => return failed(why),
            }
        }
        for (path, (_, _, text)) in paths.iter().zip(&planned.files) {
            let before = || {
                // A test's held replacement: it waits for the request's end, or the hold's.
                if let Some((which, held)) = &self.s.hold
                    && path.ends_with(which.as_str())
                {
                    let until = Instant::now() + *held;
                    while !job.cancelled() && Instant::now() < until {
                        std::thread::sleep(POLL);
                    }
                }
                if job.cancelled() { Err(CANCELLED.to_string()) } else { Ok(()) }
            };
            match self.s.caps.replace_nofollow(path, text.as_bytes(), &before) {
                Ok(()) => {}
                Err(why) if why == CANCELLED => return Err(why),
                Err(why) => return failed(why),
            }
        }
        Ok(planned.answer.clone())
    }
}

/// `text` as a JSON string, quoted.
fn json_text(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// What a request the toolchain's work stopped for gives, if it stops between phases.
pub(crate) const CANCELLED: &str = "the toolchain's work was cancelled";

/// What a request wrote, removed if the request doesn't finish (it failed, or was cancelled,
/// also during its last write): each file once it's opened, before it's emptied or written, and
/// each directory it made, emptied of those files, in the reverse order. So no partial output,
/// and nothing a retry would find in its way, is left. A file whose open failed isn't touched.
struct Written<'a> {
    s: &'a Services,
    files: Vec<String>,
    dirs: Vec<String>,
    writes: usize,
    kept: bool,
}

impl<'a> Written<'a> {
    fn new(s: &'a Services) -> Written<'a> {
        Written { s, files: Vec::new(), dirs: Vec::new(), writes: 0, kept: false }
    }

    /// Makes the directory `path` and those above it, each one made counted as this request's.
    fn make_dirs(&mut self, path: &str) -> Result<(), String> {
        self.s.caps.make_dirs_tracked(path, true, &mut self.dirs).map(|_| ())
    }

    /// Writes `bytes` as the whole file at `path`, following no link, unless `job` is cancelled
    /// first.
    fn write(&mut self, job: &Job, path: String, bytes: &[u8]) -> Result<(), String> {
        let s = self.s;
        self.writes += 1;
        let chosen = |which: &str| match which.strip_prefix('#') {
            Some(n) => n.parse() == Ok(self.writes),
            None => path.ends_with(which),
        };
        // A test's slow disk.
        std::thread::sleep(s.io_delay);
        if job.cancelled() {
            return Err(CANCELLED.into());
        }
        let mut file = s.caps.open_output(&path, &mut self.dirs)?;
        // The request's from here on, before it changes.
        self.files.push(path.clone());
        let fail = |e: std::io::Error| format!("`{path}`: {e}");
        file.set_len(0).map_err(fail)?;
        if let Some((which, held)) = &s.hold
            && chosen(which)
        {
            std::thread::sleep(*held);
        }
        if s.write_fail.as_deref().is_some_and(chosen) {
            std::io::Write::write_all(&mut file, &bytes[..bytes.len() / 2]).map_err(fail)?;
            return Err(format!("`{path}`: a test's disk failed part way"));
        }
        std::io::Write::write_all(&mut file, bytes).map_err(fail)
    }

    /// The request finished: what it wrote stays, unless it was cancelled meanwhile, also
    /// during its last write, and then it all goes.
    fn finish(mut self, job: &Job) -> Result<(), String> {
        if job.cancelled() {
            return Err(CANCELLED.into());
        }
        self.kept = true;
        Ok(())
    }
}

impl Drop for Written<'_> {
    fn drop(&mut self) {
        if self.kept {
            return;
        }
        for p in self.files.iter().rev() {
            let _ = self.s.caps.remove_file_nofollow(p);
        }
        for d in self.dirs.iter().rev() {
            let _ = self.s.caps.remove_dir_nofollow(d);
        }
    }
}

/// Whether `name` is a path inside a directory: parts with `/` between, none empty, `.` or `..`.
fn simple(name: &str) -> bool {
    !name.is_empty() && name.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

/// Writes a build's `files` into the write root path `into` by `wrela build`'s rules
/// (compiler/cli/src/build.rs), each followed by no link: a link at any output's path, or at
/// the list of what the last build wrote ([`LIST`]), stops it before anything changes; the
/// files the list names that a build writes (`Toolchain::owns`) and this one doesn't are
/// removed (all of them, for a build with errors, `ok` false); a file this build would write that
/// no build wrote stops it; and the list names what this one wrote, last. If anything fails, or
/// `job` is cancelled, the files this request wrote are removed. Its files' paths.
fn write_build(
    s: &Services,
    job: &Job,
    toolchain: &dyn Toolchain,
    into: &str,
    files: &[(String, Vec<u8>)],
    ok: bool,
) -> Result<Vec<String>, String> {
    use super::caps::Kind;
    let caps = &s.caps;
    let at = |name: &str| format!("{into}/{name}");
    let list = at(LIST);
    // Before anything is removed or written.
    let names = files.iter().map(|(f, _)| f.as_str()).chain([LIST]);
    for name in names {
        if !simple(name) {
            return Err(format!("the build has a file named `{name}`"));
        }
        if caps.kind_nofollow(&at(name))? == Some(Kind::Link) {
            return Err(format!("`{}` is a symbolic link, so it won't be used", at(name)));
        }
    }
    let owned: Vec<String> = match caps.kind_nofollow(&list)? {
        Some(Kind::File) => {
            let bytes = caps.read_nofollow(&list, MOST_SOURCE)?.unwrap_or_default();
            String::from_utf8_lossy(&bytes)
                .lines()
                .filter(|l| simple(l) && toolchain.owns(l))
                .map(String::from)
                .collect()
        }
        None => {
            // A directory a build wrote before there was a list: every file a build writes.
            let present: Vec<String> = match caps.list(into) {
                Ok(entries) => entries.into_iter().map(|e| e.name).collect(),
                Err(_) if caps.kind_nofollow(into).ok().flatten().is_none() => Vec::new(),
                Err(why) => return Err(why),
            };
            let old = ["game.wasm", "manifest.json"].iter().all(|f| present.iter().any(|p| p == f));
            if old {
                present.into_iter().filter(|n| toolchain.owns(n)).collect()
            } else {
                Vec::new()
            }
        }
        Some(_) => return Err(format!("can't read `{list}`: it isn't a regular file")),
    };
    let stale: Vec<&String> =
        owned.iter().filter(|n| !files.iter().any(|(f, _)| f == *n)).collect();
    for name in &stale {
        if caps.kind_nofollow(&at(name))? == Some(Kind::Link) {
            return Err(format!("`{}` is a symbolic link, so it won't be used", at(name)));
        }
    }
    for name in &stale {
        caps.remove_file_nofollow(&at(name))?;
    }
    if !ok {
        return Ok(Vec::new());
    }
    for (name, _) in files {
        if !owned.contains(name) && caps.kind_nofollow(&at(name))?.is_some() {
            return Err(format!(
                "`{}` is in the way: an earlier build didn't write it, so it isn't replaced",
                at(name)
            ));
        }
    }
    let mut written = Written::new(s);
    written.make_dirs(into)?;
    let mut text = String::new();
    for (name, bytes) in files {
        written.write(job, at(name), bytes)?;
        text.push_str(name);
        text.push('\n');
    }
    written.write(job, list, text.as_bytes())?;
    written.finish(job)?;
    Ok(files.iter().map(|(f, _)| f.clone()).collect())
}

/// Formats the files at `paths` (root paths of files or directories), or with `check` only
/// says which aren't formatted: those changed (or that would be), and those with syntax errors,
/// with their errors.
#[allow(clippy::type_complexity)]
fn format(
    s: &Services,
    job: &Job,
    toolchain: &dyn Toolchain,
    paths: &[String],
    check: bool,
) -> Result<(Vec<String>, Vec<(String, String)>), String> {
    let caps = &s.caps;
    let mut files = Vec::new();
    for p in paths {
        match caps.stat(p)?.kind {
            super::caps::Kind::Dir => {
                let dir = caps.dir_path(p, false)?;
                let found = toolchain.sources(job, &dir)?;
                if found.is_empty() {
                    return Err(format!("`{p}` holds no `.wrela` file to format"));
                }
                for f in found {
                    let Ok(rel) = f.strip_prefix(&dir) else { continue };
                    let parts: Vec<String> =
                        rel.components().map(|c| c.as_os_str().to_string_lossy().into()).collect();
                    files.push(format!("{p}/{}", parts.join("/")));
                }
            }
            super::caps::Kind::File if p.ends_with(".wrela") => files.push(p.clone()),
            _ => return Err(format!("`{p}` is neither a directory nor a `.wrela` file")),
        }
    }
    let (mut changed, mut failed) = (Vec::new(), Vec::new());
    for f in files {
        if job.cancelled() {
            return Err(CANCELLED.into());
        }
        let bytes = caps.read_most(&f, MOST_SOURCE)?;
        let Ok(text) = String::from_utf8(bytes) else {
            failed.push((f, "it isn't UTF-8".into()));
            continue;
        };
        match toolchain.format(&text) {
            Ok(formatted) if formatted == text => {}
            Ok(formatted) => {
                if !check {
                    caps.write(&f, formatted.as_bytes())?;
                }
                changed.push(f);
            }
            Err(why) => failed.push((f, why)),
        }
    }
    Ok((changed, failed))
}

/// A request's overrides: a count, then each dependency's name and the root path read for it.
fn overrides_of(b: &mut Body) -> Result<Vec<(String, String)>, String> {
    let n = b.u32()?;
    let mut overrides = Vec::new();
    for _ in 0..n.min(MOST_BYTES as u32) {
        overrides.push((b.str()?, b.str()?));
    }
    Ok(overrides)
}

/// Each override's directory, through the grants.
fn override_dirs(
    caps: &super::caps::Caps,
    overrides: Vec<(String, String)>,
) -> Result<Vec<(String, PathBuf)>, String> {
    let mut dirs = Vec::new();
    for (name, path) in overrides {
        dirs.push((name, caps.dir_path(&path, false)?));
    }
    Ok(dirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request may ask for as long as the run's whole budget, so a tool that grants itself an
    /// hour can give one build most of it: never less than 600 s, as without a budget.
    #[test]
    fn a_requests_deadline_may_be_as_long_as_the_runs_budget() {
        let mut s = super::super::services::for_test(super::super::BlobBudget::default());
        assert_eq!(longest(&s), 600.0);
        s.budget = Some(Duration::from_secs(5400));
        assert_eq!(longest(&s), 5400.0);
        let given = super::super::http::deadline_within(5000.0, DEADLINE, longest(&s));
        assert_eq!(given, Ok(Duration::from_secs(5000)));
        assert!(super::super::http::deadline_within(6000.0, DEADLINE, longest(&s)).is_err());
        // Past the budget, cut to it; without one, an error.
        assert_eq!(asked(&s, 6000.0), 5400.0);
        s.budget = Some(Duration::from_secs(30));
        assert_eq!(longest(&s), 600.0);
        assert_eq!(asked(&s, 5100.0), 600.0);
        s.budget = None;
        assert_eq!(asked(&s, 5100.0), 5100.0);
        assert!(super::super::http::deadline_within(5100.0, DEADLINE, longest(&s)).is_err());
    }
}
