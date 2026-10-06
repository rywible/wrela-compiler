//! The compiler's driver: loads a package, then checks it (names, types, the memory model, the
//! GPU rules, and the per-instantiation effects found while lowering) or builds it.
//!
//! Each call compiles the package from its files; nothing is cached between calls. Checking the
//! examples takes tens of milliseconds; an LSP (in the backlog, #31) would decide what
//! incremental reuse has to look like.
//!
//! The compiler recurses over syntax trees, so it runs on its own thread with a stack sized for
//! the deepest tree the parser accepts (`wrela_syntax::MAX_NESTING`, `MAX_EXPR_DEPTH`).

pub mod build;
pub mod consts;
pub mod edit;
pub mod explain;
mod frames;
pub mod lift;
pub mod manifest;
pub mod package;
pub mod primer;
pub mod query;
pub mod refactor;
pub mod serve;
pub mod studio;

/// std's modules: each one's path and source.
pub use wrela_sema::STD_SOURCES;

use std::path::Path;
use wrela_diag::{Diagnostic, SourceMap, Span, codes, has_errors, sort_and_dedup};
use wrela_sema::SourceUnit;
use wrela_sema::defs::PackageKind;

/// What compiling a package produced.
#[derive(Debug)]
pub struct Output {
    /// Every source file read: std's, then the package's.
    pub sources: SourceMap,
    /// Every diagnostic, sorted, but warnings in std's own code.
    pub diagnostics: Vec<Diagnostic>,
    /// Warnings in std's own code: std's authors' to fix, not the program's, so they aren't
    /// among `diagnostics`. Kept for std's tests, which expect none.
    pub std_warnings: Vec<Diagnostic>,
    /// The build's files by path relative to the output directory, in a fixed order; empty
    /// unless building found no errors.
    pub files: Vec<(String, Vec<u8>)>,
    /// Each GPU pipeline the program records, for the pipeline-count query (§7); empty when
    /// checking found errors before lowering.
    pub pipelines: Vec<PipelineSummary>,
    /// The program package's own files (not std's or a dependency's), and where each is: what
    /// `wrela fix` may edit.
    pub program_files: ProgramFiles,
}

/// One pipeline: an instantiation of its entry points, and why it exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineSummary {
    /// `compute` or `render`.
    pub kind: &'static str,
    /// Its entry points, each with the type arguments it's instantiated with: `shade::<f32>`.
    pub entries: Vec<String>,
    /// The entry points' names alone, to group instantiations by.
    pub names: Vec<String>,
    /// Where CPU code dispatches or draws it, as `file:line:column`.
    pub sites: Vec<String>,
}

impl Output {
    pub fn has_errors(&self) -> bool {
        has_errors(&self.diagnostics)
    }

    /// Writes the build's files into `dir`, and makes the directories they need.
    pub fn write_to(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        for (path, bytes) in &self.files {
            let p = dir.join(path);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(p, bytes)?;
        }
        Ok(())
    }
}

/// The compiler thread's stack, and the formatter's. Generous: it's reserved, not committed, and
/// the deepest trees the parser accepts need far less.
pub const STACK_SIZE: usize = 256 << 20;

/// Checks the package at `root`: every diagnostic, including those lowering finds.
pub fn check(root: &Path) -> Output {
    on_compiler_thread(|| compile(root, Mode::Check, wrela_lower::Roots::of, &[]))
}

/// Files' texts that stand in for what's on disk, by their canonical paths: for checking a
/// change before it's written (`wrela refactor`).
pub type Overlay = std::collections::HashMap<std::path::PathBuf, String>;

/// [`check`], with `overlay`'s texts in place of those files' on disk.
pub fn check_with(root: &Path, overlay: &Overlay) -> Output {
    on_compiler_thread(|| compile_with(root, Mode::Check, wrela_lower::Roots::of, &[], overlay))
}

/// Builds the package at `root`: its diagnostics, and its files when there are no errors.
pub fn build(root: &Path) -> Output {
    on_compiler_thread(|| compile(root, Mode::Release, wrela_lower::Roots::of, &[]))
}

/// [`build`] in debug mode: the build pays for checks a release build doesn't make, such as a
/// trap where a float operation creates a NaN (language.md §11).
pub fn build_debug(root: &Path) -> Output {
    on_compiler_thread(|| compile(root, Mode::Debug, wrela_lower::Roots::of, &[]))
}

/// [`build`], lowering every function of the package that can be lowered on its own, exported
/// or not ([`wrela_lower::Roots::every_fn`]): for tests of code that nothing calls.
pub fn build_every_fn(root: &Path) -> Output {
    on_compiler_thread(|| compile(root, Mode::Release, wrela_lower::Roots::every_fn, &[]))
}

/// [`build`], with no SIMD in the WASM (language.md §11): for the test that a build with SIMD
/// gives the same results, bit for bit.
pub fn build_without_simd(root: &Path) -> Output {
    on_compiler_thread(|| compile(root, Mode::NoSimd, wrela_lower::Roots::of, &[]))
}

/// A lifted build (language.md §22): [`build`], with each `f32` literal of the packages named
/// `lift` (the program's own, or its dependencies', by the names their manifests give) read
/// from a table the program can change while it runs. The build's files include `lift.json`,
/// which says where each literal is and why any float literal of those packages isn't lifted.
/// `debug` makes it a debug build. An error if a name isn't one of the program's packages.
pub fn build_lifted(root: &Path, lift: &[String], debug: bool) -> Result<Output, String> {
    on_compiler_thread(|| {
        let loaded = load(root);
        if let Some(l) = &loaded.2 {
            let names: Vec<&str> = l.packages.iter().skip(1).map(|p| p.name.as_str()).collect();
            if let Some(bad) = lift.iter().find(|n| !names.contains(&n.as_str())) {
                return Err(format!(
                    "--lift: the program has no package named `{bad}` (it has {})",
                    names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")
                ));
            }
        }
        let mode = if debug { Mode::Debug } else { Mode::Release };
        Ok(compile_loaded(root, mode, wrela_lower::Roots::of, lift, loaded))
    })
}

/// Writes `bytes` to `path` atomically: through a hidden temporary file beside it, renamed over
/// it, so a reader sees the old file or the whole new one.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // A temporary file of this write's own: the studio server's threads may write one file at
    // once.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.tmp-{}-{n}", std::process::id()));
    std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// `v` as the JSON files and answers the tools write: pretty, with a newline at the end.
pub(crate) fn pretty(v: &serde_json::Value) -> String {
    let mut s = serde_json::to_string_pretty(v).expect("JSON values serialize");
    s.push('\n');
    s
}

/// Loads and checks the package at `root`, and runs `f` on what checking found, with the
/// sources: for queries over a program (`wrela studio`'s subject). The rendered errors if it
/// doesn't load or check.
pub fn with_checked<T: Send>(
    root: &Path,
    f: impl FnOnce(&wrela_sema::Checked, &SourceMap) -> T + Send,
) -> Result<T, String> {
    with_checked_in(root, &Overlay::new(), f)
}

/// [`with_checked`], and if `lower`, with what lowering every function of the package that
/// can be lowered on its own instantiates (`wrela query`'s instantiations): lowering's own
/// errors (a library package has no `frame`) don't stop it.
pub fn with_lowered<T: Send>(
    root: &Path,
    lower: bool,
    f: impl FnOnce(&wrela_sema::Checked, &SourceMap, Option<&wrela_lower::Lowered>) -> T + Send,
) -> Result<T, String> {
    with_lowered_in(root, &Overlay::new(), lower, f)
}

/// [`with_checked`], with `overlay`'s texts in place of those files' on disk.
pub fn with_checked_in<T: Send>(
    root: &Path,
    overlay: &Overlay,
    f: impl FnOnce(&wrela_sema::Checked, &SourceMap) -> T + Send,
) -> Result<T, String> {
    with_lowered_in(root, overlay, false, |c, s, _| f(c, s))
}

fn with_lowered_in<T: Send>(
    root: &Path,
    overlay: &Overlay,
    lower: bool,
    f: impl FnOnce(&wrela_sema::Checked, &SourceMap, Option<&wrela_lower::Lowered>) -> T + Send,
) -> Result<T, String> {
    on_compiler_thread(|| {
        let (sources, mut diagnostics, loaded) = load_with(root, overlay);
        let Some(Loaded { units, packages, dirs, .. }) = loaded else {
            return Err(wrela_diag::render::render_all(&sources, &diagnostics));
        };
        let checked = wrela_sema::check_packages(units, &packages, &mut diagnostics);
        if has_errors(&diagnostics) {
            return Err(wrela_diag::render::render_all(&sources, &diagnostics));
        }
        if !lower {
            return Ok(f(&checked, &sources, None));
        }
        let mut data = wrela_lower::BuildData::default();
        prepare(&checked, &sources, &dirs, &mut data, &mut diagnostics);
        if has_errors(&diagnostics) {
            return Err(wrela_diag::render::render_all(&sources, &diagnostics));
        }
        let roots = wrela_lower::Roots::every_fn(&checked);
        let (lowered, _) = wrela_lower::lower(&checked, &roots, &data, false);
        Ok(f(&checked, &sources, Some(&lowered)))
    })
}

/// The built-in functions (language.md §4): each one's name, how many arguments it takes, and
/// whether it's called as a method only (`x.len()`), for `wrela doc`.
pub fn builtins() -> Vec<(&'static str, usize, bool)> {
    wrela_sema::builtins::BuiltinFn::ALL
        .iter()
        .map(|b| (b.name(), b.arity(), wrela_sema::builtins::BuiltinFn::lookup(b.name()).is_none()))
        .collect()
}

/// What running a package's tests found (`wrela test`, §10).
#[derive(Debug)]
pub struct TestOutput {
    pub sources: SourceMap,
    /// Errors that kept the tests from running (checking, computing constants, lowering).
    pub diagnostics: Vec<Diagnostic>,
    /// Each test of the program package that the filter chose, in the order written: its name,
    /// where it is (`file:line:column`), and why it failed, if it did.
    pub results: Vec<TestResult>,
    /// How many tests the filter left out.
    pub filtered_out: usize,
}

#[derive(Debug)]
pub struct TestResult {
    pub name: String,
    pub at: String,
    pub failure: Option<Diagnostic>,
}

impl TestOutput {
    pub fn passed(&self) -> bool {
        !has_errors(&self.diagnostics) && self.results.iter().all(|r| r.failure.is_none())
    }
}

/// Runs the package's `@test` functions as the build runs constants (§10): checked as a debug
/// build, its constants computed, then each test whose name contains `filter` called, with
/// nothing of them in a build.
pub fn test(root: &Path, filter: Option<&str>) -> TestOutput {
    on_compiler_thread(|| run_tests(root, filter, PackageKind::Program))
}

/// [`test`] for std's own tests, with the package at `root` as the program: how std's
/// tests run (the suite's `std_tests_pass`), since only a program package's tests run.
pub fn test_std(root: &Path, filter: Option<&str>) -> TestOutput {
    on_compiler_thread(|| run_tests(root, filter, PackageKind::Std))
}

fn run_tests(root: &Path, filter: Option<&str>, kind: PackageKind) -> TestOutput {
    let (sources, mut diagnostics, loaded) = load(root);
    let Some(Loaded { units, packages, dirs, .. }) = loaded else {
        split_std_warnings(&sources, &mut diagnostics);
        return TestOutput { sources, diagnostics, results: Vec::new(), filtered_out: 0 };
    };
    let checked = wrela_sema::check_packages(units, &packages, &mut diagnostics);
    // A debug build's checks, as `wrela build --debug` makes them (§11).
    let mut data = wrela_lower::BuildData { debug: true, ..Default::default() };
    let mut filtered_out = 0;
    prepare(&checked, &sources, &dirs, &mut data, &mut diagnostics);
    let mut results = Vec::new();
    if !has_errors(&diagnostics) {
        // The package's tests, in the order written.
        let p = &checked.program;
        let mut tests: Vec<wrela_sema::ty::FnId> = (0..p.fns.len() as u32)
            .map(wrela_sema::ty::FnId)
            .filter(|&f| {
                let def = p.func(f);
                def.attrs.test.is_some() && p.package_of(def.module).kind == kind
            })
            .collect();
        tests.sort_by_key(|&f| (p.func(f).span.file, p.func(f).span.start));
        let all = tests.len();
        tests.retain(|&f| filter.is_none_or(|w| p.func(f).name.contains(w)));
        filtered_out = all - tests.len();
        // Tests of code run as constants are computed; frame tests run the program first.
        let (framed, plain): (Vec<_>, Vec<_>) =
            tests.iter().partition(|&&f| p.func(f).attrs.runs_program());
        let mut ran = Vec::new();
        if !plain.is_empty() {
            let (r, d) = consts::run_tests(&checked, &sources, &data, &plain);
            ran.extend(r);
            diagnostics.extend(d);
        }
        if !framed.is_empty() && !has_errors(&diagnostics) {
            let (r, d) = frames::run(&checked, &sources, &data, &framed, root);
            ran.extend(r);
            diagnostics.extend(d);
        }
        // In the order written.
        ran.sort_by_key(|&(f, _)| tests.iter().position(|&t| t == f));
        for (f, failure) in ran {
            let def = p.func(f);
            let at = sources.file(def.name_span.file).location(def.name_span.start);
            results.push(TestResult { name: def.name.clone(), at, failure });
        }
    }
    sort_and_dedup(&mut diagnostics);
    split_std_warnings(&sources, &mut diagnostics);
    TestOutput { sources, diagnostics, results, filtered_out }
}

/// What a compile makes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Diagnostics only.
    Check,
    Release,
    Debug,
    /// A release build whose WASM has no SIMD.
    NoSimd,
}

/// Runs `f` on a thread with the compiler's stack ([`STACK_SIZE`]), and gives back what it
/// returns; a panic in it goes on in the caller. For anything that parses or walks syntax trees.
pub fn on_compiler_thread<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        let worker = std::thread::Builder::new()
            .name("wrela".into())
            .stack_size(STACK_SIZE)
            .spawn_scoped(s, f)
            .expect("can't start the compiler's thread");
        worker.join().unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

fn compile(
    root: &Path,
    mode: Mode,
    roots: fn(&wrela_sema::Checked) -> wrela_lower::Roots,
    lift: &[String],
) -> Output {
    compile_with(root, mode, roots, lift, &Overlay::new())
}

fn compile_with(
    root: &Path,
    mode: Mode,
    roots: fn(&wrela_sema::Checked) -> wrela_lower::Roots,
    lift: &[String],
    overlay: &Overlay,
) -> Output {
    compile_loaded(root, mode, roots, lift, load_with(root, overlay))
}

/// [`compile_with`] of the program [`load_with`] read.
fn compile_loaded(
    root: &Path,
    mode: Mode,
    roots: fn(&wrela_sema::Checked) -> wrela_lower::Roots,
    lift: &[String],
    (sources, mut diagnostics, loaded): (SourceMap, Vec<Diagnostic>, Option<Loaded>),
) -> Output {
    let emit = mode != Mode::Check;
    let Some(Loaded { units, mut packages, dirs, files: all_files }) = loaded else {
        let std_warnings = split_std_warnings(&sources, &mut diagnostics);
        return Output {
            sources,
            diagnostics,
            std_warnings,
            files: Vec::new(),
            pipelines: Vec::new(),
            program_files: Vec::new(),
        };
    };
    // The program package's own files: package 1's (std is 0).
    let program_files: ProgramFiles = all_files
        .iter()
        .filter(|(_, pkg, _)| *pkg == 1)
        .map(|(f, _, path)| (*f, path.clone()))
        .collect();
    for p in packages.iter_mut().skip(1) {
        p.lifted = lift.contains(&p.name);
    }
    // The lifted packages' files, each with its path from the program's package.
    let lifted_files: Vec<(wrela_diag::FileId, String)> = all_files
        .iter()
        .filter(|(_, pkg, _)| packages[*pkg].lifted)
        .map(|(f, _, path)| (*f, lift::relative(path, root)))
        .collect();
    let checked = wrela_sema::check_packages(units, &packages, &mut diagnostics);
    let mut files = Vec::new();
    let mut pipelines = Vec::new();
    let mut data = wrela_lower::BuildData {
        debug: mode == Mode::Debug,
        no_simd: mode == Mode::NoSimd,
        ..Default::default()
    };
    prepare(&checked, &sources, &dirs, &mut data, &mut diagnostics);
    if !lift.is_empty() && !has_errors(&diagnostics) {
        data.lift = Some(lift::table(&checked, &sources, &lifted_files));
    }
    if !has_errors(&diagnostics) {
        let roots = roots(&checked);
        let (lowered, d) = wrela_lower::lower(&checked, &roots, &data, emit);
        let ok = !has_errors(&d);
        diagnostics.extend(d);
        pipelines = summarize(&checked, &sources, &lowered);
        if ok && emit {
            let out = build::emit(&lowered, &sources, !data.no_simd);
            diagnostics.extend(out.diagnostics);
            files = out.files;
            if let Some(t) = &data.lift {
                files.push(("lift.json".into(), lift::report(&checked, &sources, t).into_bytes()));
            }
        }
    }
    sort_and_dedup(&mut diagnostics);
    if has_errors(&diagnostics) {
        files.clear();
    }
    let std_warnings = split_std_warnings(&sources, &mut diagnostics);
    Output { sources, diagnostics, std_warnings, files, pipelines, program_files }
}

/// Reads the files `embed` reads, then computes the constants the build computes, into `data`,
/// before what reads them is lowered (§10): each step only if no errors came before it.
fn prepare(
    checked: &wrela_sema::Checked,
    sources: &SourceMap,
    dirs: &[std::path::PathBuf],
    data: &mut wrela_lower::BuildData,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if has_errors(diagnostics) {
        return;
    }
    let (embeds, d) = consts::read_embeds(checked, dirs);
    data.embeds = embeds;
    diagnostics.extend(d);
    if !has_errors(diagnostics) {
        diagnostics.extend(consts::compute(checked, sources, data));
    }
}

/// Takes the warnings in std's own code out of `diags`, and returns them.
fn split_std_warnings(sources: &SourceMap, diags: &mut Vec<Diagnostic>) -> Vec<Diagnostic> {
    let in_std = |d: &Diagnostic| {
        !d.is_error() && d.span().is_some_and(|s| sources.file(s.file).name.starts_with("<std::"))
    };
    let (std, rest): (Vec<Diagnostic>, Vec<Diagnostic>) =
        std::mem::take(diags).into_iter().partition(in_std);
    *diags = rest;
    std
}

/// What the pipeline-count query reports of each pipeline (§7).
fn summarize(
    checked: &wrela_sema::Checked,
    sources: &SourceMap,
    lowered: &wrela_lower::Lowered,
) -> Vec<PipelineSummary> {
    let p = &checked.program;
    let entry = |f: wrela_sema::ty::FnId, substs: &[wrela_sema::ty::TyId]| {
        let name = p.fn_display_name(f);
        if substs.is_empty() {
            (name.clone(), name)
        } else {
            let args: Vec<String> = substs.iter().map(|&t| p.display_ty(t)).collect();
            (format!("{name}::<{}>", args.join(", ")), name)
        }
    };
    lowered
        .pipelines
        .iter()
        .map(|out| {
            let (kind, parts) = match &out.key {
                wrela_lower::PipelineKey::Compute { kernel, substs } => {
                    ("compute", vec![entry(*kernel, substs)])
                }
                wrela_lower::PipelineKey::Render { vertex, fragment, .. } => {
                    ("render", vec![entry(vertex.0, &vertex.1), entry(fragment.0, &fragment.1)])
                }
            };
            let sites = out.sites.iter().map(|s| sources.file(s.file).location(s.start)).collect();
            PipelineSummary {
                kind,
                entries: parts.iter().map(|(e, _)| e.clone()).collect(),
                names: parts.into_iter().map(|(_, n)| n).collect(),
                sites,
            }
        })
        .collect()
}

/// The program package's own files, and where each is.
type ProgramFiles = Vec<(wrela_diag::FileId, std::path::PathBuf)>;

/// Reads and parses std, the package and its dependencies. The units are `None` when the
/// package can't be loaded. Files with syntax errors are still checked: the parser keeps what it
/// could read, with error nodes where it couldn't, and checking stays quiet about those.
fn load(root: &Path) -> (SourceMap, Vec<Diagnostic>, Option<Loaded>) {
    load_with(root, &Overlay::new())
}

/// [`load`], with `overlay`'s texts in place of those files' on disk.
fn load_with(root: &Path, overlay: &Overlay) -> (SourceMap, Vec<Diagnostic>, Option<Loaded>) {
    let mut l = Loader {
        overlay: overlay.clone(),
        sources: SourceMap::new(),
        diags: Vec::new(),
        units: Vec::new(),
        packages: vec![wrela_sema::collect::PackageInfo {
            name: "std".into(),
            kind: PackageKind::Std,
            deps: Vec::new(),
            unsafe_ok: true,
            lifted: false,
        }],
        dirs: Vec::new(),
        visiting: Vec::new(),
        layout_failed: false,
        files: Vec::new(),
    };
    for (name, text) in STD_SOURCES {
        let path = name.split("::").map(String::from).collect();
        let name = format!("<{name}>");
        let u = add_unit(&mut l.sources, &mut l.diags, name, text.to_string(), path, 0);
        l.units.push(u);
    }
    l.package(root, None, PackageKind::Program, None);
    if l.layout_failed {
        sort_and_dedup(&mut l.diags);
        return (l.sources, l.diags, None);
    }
    // Without `main.wrela` the package is a library, with no exports: a `Main.wrela` on a file
    // system that ignores case is more likely a mistake than a module named `Main`.
    let package = || l.units.iter().filter(|u| u.package == 1);
    if !package().any(|u| u.path == ["main"])
        && let Some(u) =
            package().find(|u| u.path.len() == 1 && u.path[0].eq_ignore_ascii_case("main"))
    {
        let file = u.ast.span.file;
        l.diags.push(
            Diagnostic::new(
                codes::W0003,
                Span::new(file, 0, 0),
                format!(
                    "`{}.wrela` isn't the entry module, which is `main.wrela` in lower case: the package is a library, with no exports",
                    u.path[0]
                ),
            )
            .with_help("rename it `main.wrela`, or the module something else"),
        );
    }
    // Each package's directory, by index (std's has none).
    let mut dirs = vec![std::path::PathBuf::new(); l.packages.len()];
    for (dir, i) in &l.dirs {
        dirs[*i] = dir.clone();
    }
    let loaded = Loaded { units: l.units, packages: l.packages, dirs, files: l.files };
    (l.sources, l.diags, Some(loaded))
}

/// A program's parsed files, and the packages they're in.
struct Loaded {
    units: Vec<SourceUnit>,
    packages: Vec<wrela_sema::collect::PackageInfo>,
    /// Each package's directory, by index: where its `embed`s read.
    dirs: Vec<std::path::PathBuf>,
    /// Every file of every package but std: its package (an index; the program's is 1) and its
    /// path on disk.
    files: Vec<(wrela_diag::FileId, usize, std::path::PathBuf)>,
}

/// Loads a package and, through their manifests, the packages it depends on (language.md §3).
struct Loader {
    /// Texts that stand in for files on disk ([`Overlay`]).
    overlay: Overlay,
    sources: SourceMap,
    diags: Vec<Diagnostic>,
    units: Vec<SourceUnit>,
    packages: Vec<wrela_sema::collect::PackageInfo>,
    /// Each package's directory (canonical), by index: a package two others depend on is
    /// loaded once.
    dirs: Vec<(std::path::PathBuf, usize)>,
    /// The packages being loaded, outermost first, with where each was declared: a dependency
    /// on one of them is a cycle.
    visiting: Vec<(std::path::PathBuf, String, Option<Span>)>,
    /// The program's own files couldn't be found: nothing more is checked.
    layout_failed: bool,
    /// Every package's files but std's: each one's package and path.
    files: Vec<(wrela_diag::FileId, usize, std::path::PathBuf)>,
}

impl Loader {
    /// Loads the package in `dir` (a dependency is named `name` by the package declaring it,
    /// at `at`), and its dependencies: its index, or `None` if it can't be.
    fn package(
        &mut self,
        dir: &Path,
        name: Option<&str>,
        kind: PackageKind,
        at: Option<Span>,
    ) -> Option<usize> {
        let canon = match dir.canonicalize() {
            Ok(c) if c.is_dir() => c,
            _ => {
                let what = name.unwrap_or("the package");
                let msg = format!(
                    "the dependency `{what}` isn't there: `{}` isn't a directory",
                    dir.display()
                );
                let d = Diagnostic::new(codes::E0220, at?, msg);
                self.diags.push(d.with_help(
                    "fix its `path`, which is relative to the `wrela.toml` that declares it",
                ));
                return None;
            }
        };
        if let Some(i) = self.visiting.iter().position(|v| v.0 == canon) {
            let chain: Vec<String> =
                self.visiting[i..].iter().map(|v| format!("`{}`", v.1)).collect();
            let here = name.unwrap_or("?");
            // Reported where the cycle starts: the first dependency in it, as the outermost
            // package in it declares it.
            if let Some(span) = self.visiting.get(i + 1).and_then(|v| v.2).or(at) {
                self.diags.push(
                    Diagnostic::new(
                        codes::E0221,
                        span,
                        format!("packages depend on each other: {} → `{here}`", chain.join(" → ")),
                    )
                    .with_note("a package's dependencies are built before it, so they can't lead back to it")
                    .with_help("move what both need into a package of its own"),
                );
            }
            return None;
        }
        if let Some(&(_, i)) = self.dirs.iter().find(|d| d.0 == canon) {
            return Some(i);
        }
        let program = kind == PackageKind::Program;
        // The manifest, if there is one: shown in diagnostics by its path in the package.
        let prefix = if program { String::new() } else { format!("[{}] ", name.unwrap_or("?")) };
        let manifest_path = dir.join("wrela.toml");
        let manifest = if manifest_path.is_file() {
            let text = std::fs::read_to_string(&manifest_path).unwrap_or_default();
            let f = self.sources.add(format!("{prefix}wrela.toml"), text);
            match manifest::parse(f, &self.sources.file(f).text.clone()) {
                Ok(m) => m,
                Err(errors) => {
                    self.diags.extend(errors);
                    manifest::Manifest::default()
                }
            }
        } else {
            manifest::Manifest::default()
        };
        let pkg_name = match (&manifest.name, name) {
            (Some((n, _)), _) => n.clone(),
            (None, Some(n)) => n.to_string(),
            (None, None) => "main".to_string(),
        };
        let prefix = if program { String::new() } else { format!("[{pkg_name}] ") };
        let index = self.packages.len();
        self.packages.push(wrela_sema::collect::PackageInfo {
            name: pkg_name.clone(),
            kind,
            deps: Vec::new(),
            unsafe_ok: manifest.unsafe_ok,
            lifted: false,
        });
        self.dirs.push((canon.clone(), index));
        self.visiting.push((canon, pkg_name, at));
        let files = match package::find_files(dir) {
            Ok(files) => files,
            Err(errors) => {
                for e in errors {
                    let f = self.sources.add(format!("{prefix}{}", e.path), "");
                    let code = if e.symlink { codes::E0208 } else { codes::E0205 };
                    let mut d = Diagnostic::new(code, Span::new(f, 0, 0), e.message);
                    if let Some(h) = e.help {
                        d = d.with_help(h);
                    }
                    self.diags.push(d);
                }
                if program {
                    self.layout_failed = true;
                }
                self.visiting.pop();
                return Some(index);
            }
        };
        let mut tops = std::collections::BTreeSet::new();
        for pf in &files {
            tops.insert(pf.module[0].clone());
            let stand_in = std::fs::canonicalize(&pf.path).ok().and_then(|c| self.overlay.get(&c));
            let text = match stand_in {
                Some(t) => Ok(t.clone()),
                None => std::fs::read(&pf.path).map_err(|e| e.to_string()).and_then(|b| {
                    String::from_utf8(b).map_err(|_| "the file isn't valid UTF-8 (L1)".to_string())
                }),
            };
            let display = format!("{prefix}{}", pf.display);
            let text = match text {
                Ok(t) => t,
                Err(why) => {
                    let f = self.sources.add(display.clone(), "");
                    self.diags.push(Diagnostic::new(
                        codes::E0205,
                        Span::new(f, 0, 0),
                        format!("can't read `{display}`: {why}"),
                    ));
                    continue;
                }
            };
            let u = add_unit(
                &mut self.sources,
                &mut self.diags,
                display,
                text,
                pf.module.clone(),
                index,
            );
            self.files.push((u.ast.span.file, index, pf.path.clone()));
            self.units.push(u);
        }
        for dep in &manifest.deps {
            if tops.contains(&dep.name) || dep.name == "std" {
                self.diags.push(
                    Diagnostic::new(
                        codes::E0201,
                        dep.span,
                        format!("the dependency `{}` has the name of a module of this package", dep.name),
                    )
                    .with_note("a path's first name is a module of the package or a dependency, so one name can't be both (§3)")
                    .with_help("rename the dependency here, or the module"),
                );
                continue;
            }
            let dep_dir = dir.join(&dep.path);
            if let Some(j) =
                self.package(&dep_dir, Some(&dep.name), PackageKind::Dependency, Some(dep.span))
            {
                self.packages[index].deps.push((dep.name.clone(), j));
            }
        }
        self.visiting.pop();
        Some(index)
    }
}

/// Adds a file to `sources` and parses it as the module `path`. Its lexical and syntax
/// diagnostics go to `diags`.
fn add_unit(
    sources: &mut SourceMap,
    diags: &mut Vec<Diagnostic>,
    name: String,
    text: String,
    path: Vec<String>,
    package: usize,
) -> SourceUnit {
    let file = sources.add(name, text);
    let parsed = wrela_syntax::parse(file, &sources.file(file).text);
    let syntax_errors =
        parsed.diagnostics.iter().filter(|d| d.is_error()).filter_map(Diagnostic::span).collect();
    diags.extend(parsed.diagnostics);
    let text = sources.file(file).text.as_str().into();
    SourceUnit { path, ast: parsed.file, package, syntax_errors, text }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A module file that can't be read, or a file whose name can't be a module, is E0205.
    #[test]
    fn unreadable_module_files_are_e0205() {
        let d = std::env::temp_dir().join(format!("wrela-driver-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        std::fs::write(d.join("main.wrela"), "fn f() -> i32 {\n    1\n}\n").expect("write");
        std::fs::write(d.join("bad.wrela"), [0x66, 0x6e, 0xff, 0xfe]).expect("write");
        let out = check(&d);
        let codes: Vec<&str> = out.diagnostics.iter().map(|x| x.code.as_str()).collect();
        assert_eq!(codes, ["E0205"], "{:?}", out.diagnostics);
        assert!(out.diagnostics[0].message.contains("isn't valid UTF-8"));

        std::fs::remove_file(d.join("bad.wrela")).expect("remove");
        std::fs::write(d.join("my-module.wrela"), "").expect("write");
        let out = check(&d);
        let codes: Vec<&str> = out.diagnostics.iter().map(|x| x.code.as_str()).collect();
        assert_eq!(codes, ["E0205"], "{:?}", out.diagnostics);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The codes `check` reports for a package of these files.
    fn codes_of(name: &str, files: &[(&str, &str)]) -> Vec<&'static str> {
        let d = std::env::temp_dir().join(format!("wrela-driver-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        for (file, text) in files {
            std::fs::write(d.join(file), text).expect("write");
        }
        let out = check(&d);
        let _ = std::fs::remove_dir_all(&d);
        out.diagnostics.iter().map(|x| x.code.as_str()).collect()
    }

    /// A pipe named like a module would block a read forever: it's refused (E0205).
    #[cfg(unix)]
    #[test]
    fn a_pipe_is_no_module() {
        let d = std::env::temp_dir().join(format!("wrela-driver-pipe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        std::fs::write(
            d.join("main.wrela"),
            "pub fn frame(time: f32, width: u32, height: u32) {}\n",
        )
        .expect("write");
        let made = std::process::Command::new("mkfifo").arg(d.join("pipe.wrela")).status();
        if made.is_ok_and(|s| s.success()) {
            let out = check(&d);
            let codes: Vec<&str> = out.diagnostics.iter().map(|x| x.code.as_str()).collect();
            assert_eq!(codes, ["E0205"], "{:?}", out.diagnostics);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A main.wrela with no items has no `frame` either (E0703); `Main.wrela` instead of it
    /// makes a library, which is likely a mistake (W0003).
    #[test]
    fn a_program_without_frame_is_e0703() {
        assert_eq!(codes_of("empty", &[("main.wrela", "")]), ["E0703"]);
        assert_eq!(codes_of("uses", &[("main.wrela", "use std::gpu::buffer\n")]), ["E0703"]);
        let upper = codes_of("upper", &[("Main.wrela", "pub fn f() -> i32 {\n    1\n}\n")]);
        assert_eq!(upper, ["W0003"]);
    }
}
