//! The compiler, as the tool host's `toolchain` service runs it (language.md §15.5,
//! `wrela_host::tool::Toolchain`): `wrela tool` gives the host this, so a tool builds, tests,
//! checks, formats and documents packages by the same code as `wrela build`, `test`, `check`,
//! `fmt` and `doc --export`, in this process.
//!
//! Each request's work reads only through the roots the tool was granted (`wrela_driver::reach`),
//! keeps its constants in the host's cache, stops between its phases when its request is
//! cancelled, and runs `wasm-opt` through the host, which owns its process group.

use std::path::{Path, PathBuf};
use wrela_driver::Scope;
use wrela_driver::reach::Reach;
use wrela_host::tool::toolchain::{
    BuildOptions, Built, Checked, Diagnostic, Job, LiteralEdits, Pipeline, TestResult, Tested,
};

/// The compiler.
pub struct Compiler;

/// What a request's work may reach: the job's roots, its cache, `overrides`, its cancellation.
fn scope_of(job: &Job, overrides: &[(String, PathBuf)]) -> Result<Scope, String> {
    Ok(Scope {
        reach: Reach::of_dirs(job.roots()?),
        cache: Some(vec![job.cache().to_path_buf()]),
        overrides: overrides.to_vec(),
        cancel: Some(job.cancel()),
    })
}

/// `diagnostics`, each with where it is and as `wrela check` shows it.
fn diagnostics_of(
    sources: &wrela_diag::SourceMap,
    diagnostics: &[wrela_diag::Diagnostic],
) -> Vec<Diagnostic> {
    diagnostics
        .iter()
        .map(|d| Diagnostic {
            error: d.is_error(),
            code: d.code.as_str().to_string(),
            message: d.message.clone(),
            at: d.span().map(|s| sources.file(s.file).location(s.start)).unwrap_or_default(),
            rendered: wrela_diag::render::render(sources, d),
        })
        .collect()
}

impl wrela_host::tool::Toolchain for Compiler {
    fn build(&self, job: &Job, package: &Path, options: &BuildOptions) -> Result<Built, String> {
        let scope = scope_of(job, &options.overrides)?;
        let kind = wrela_driver::BuildKind { debug: options.debug, testing: options.testing };
        let mut out = wrela_driver::build_in(package, kind, options.simd, &options.lift, &scope)?;
        if options.optimize && !out.has_errors() {
            crate::build::optimized_with(&mut out.files, |wasm| {
                let level = wrela_driver::optimize::Level::DEFAULT;
                wrela_driver::optimize::optimize_with(wasm, level, &mut |program, args| {
                    let (success, status, stdout, stderr) = job.run(program, args)?;
                    Ok(wrela_driver::optimize::Ran { success, status, stdout, stderr })
                })
            })?;
        }
        let diagnostics = diagnostics_of(&out.sources, &out.diagnostics);
        Ok(Built {
            files: out.files,
            diagnostics,
            consts: (out.consts.computed, out.consts.cached),
        })
    }

    fn test(
        &self,
        job: &Job,
        package: &Path,
        filter: Option<&str>,
        overrides: &[(String, PathBuf)],
    ) -> Result<Tested, String> {
        let scope = scope_of(job, overrides)?;
        let out = wrela_driver::test_in(package, filter, &scope)?;
        let results = out
            .results
            .iter()
            .map(|r| TestResult {
                name: r.name.clone(),
                at: r.at.clone(),
                passed: r.failure.is_none(),
                message: r
                    .failure
                    .as_ref()
                    .map(|d| wrela_diag::render::render(&out.sources, d))
                    .unwrap_or_default(),
                printed: r.traced.lines.clone(),
            })
            .collect();
        Ok(Tested {
            results,
            diagnostics: diagnostics_of(&out.sources, &out.diagnostics),
            filtered_out: out.filtered_out as u32,
        })
    }

    fn check(
        &self,
        job: &Job,
        package: &Path,
        overrides: &[(String, PathBuf)],
    ) -> Result<Checked, String> {
        let scope = scope_of(job, overrides)?;
        let out = wrela_driver::check_in(package, &scope)?;
        let pipelines = out
            .pipelines
            .iter()
            .map(|p| Pipeline {
                kind: p.kind.to_string(),
                entries: p.entries.clone(),
                sites: p.sites.clone(),
            })
            .collect();
        Ok(Checked { diagnostics: diagnostics_of(&out.sources, &out.diagnostics), pipelines })
    }

    fn owns(&self, name: &str) -> bool {
        crate::build::written_by_build(name)
    }

    fn sources(&self, job: &Job, dir: &Path) -> Result<Vec<PathBuf>, String> {
        let reach = Reach::of_dirs(job.roots()?);
        let (mut files, mut errors) = (Vec::new(), Vec::new());
        crate::fmt::collect_in(&reach, dir, &mut files, &mut errors);
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        Ok(files)
    }

    fn format(&self, text: &str) -> Result<String, String> {
        wrela_driver::on_compiler_thread(|| crate::fmt::formatted("the file", text))
    }

    fn doc_export(
        &self,
        job: &Job,
        packages: &[PathBuf],
        repository: &str,
        revision: Option<&str>,
        base: &Path,
    ) -> Result<Vec<(String, Vec<u8>)>, String> {
        let reach = Reach::of_dirs(job.roots()?);
        crate::export::files_of(revision, repository, packages, &reach, base).map(|(_, f)| f)
    }

    fn query(
        &self,
        job: &Job,
        package: &Path,
        queries: &[String],
        overrides: &[(String, PathBuf)],
    ) -> Result<String, String> {
        let scope = scope_of(job, overrides)?;
        let parsed = queries
            .iter()
            .map(|q| Ok((q.clone(), wrela_driver::query::parse(q)?)))
            .collect::<Result<Vec<_>, String>>()?;
        let answered = wrela_driver::query::answer_in(package, &parsed, &scope)?;
        Ok(serde_json::Value::Array(answered.answers).to_string())
    }

    fn lifted(&self, report: &str) -> Result<(Vec<String>, Vec<f32>), String> {
        let r = wrela_driver::lift::parse_report(report)?;
        let built = r.literals.iter().map(|l| l.value).collect();
        Ok((r.files.into_iter().map(|f| f.path).collect(), built))
    }

    fn literal_values(
        &self,
        report: &str,
        read: &dyn Fn(&str) -> Result<String, String>,
    ) -> Result<Option<Vec<f32>>, String> {
        let r = wrela_driver::lift::parse_report(report)?;
        wrela_driver::lift::values_now(&r, read)
    }

    fn plan_literal_edits(
        &self,
        report: &str,
        read: &dyn Fn(&str) -> Result<String, String>,
        edits: &[(u32, f32)],
    ) -> Result<LiteralEdits, String> {
        let r = wrela_driver::lift::parse_report(report)?;
        Ok(match wrela_driver::lift::plan_by_index(&r, read, edits) {
            Ok(files) => LiteralEdits {
                answer: wrela_driver::edit::to_value(&files, true).to_string(),
                files: files.into_iter().map(|f| (f.file, f.old_hash, f.text)).collect(),
                refused: false,
            },
            Err(refused) => LiteralEdits {
                files: Vec::new(),
                answer: serde_json::json!({
                    "written": false, "file": refused.file, "why": refused.why,
                })
                .to_string(),
                refused: true,
            },
        })
    }
}

/// Where the toolchain keeps its constants: the directory `WRELA_CONST_CACHE` names, else
/// `~/.cache/wrela/consts`. Either is the user's, never a root a tool is granted. `None` if
/// neither can be found, and the run keeps a cache of its own.
pub fn cache_dir() -> Option<PathBuf> {
    let named = std::env::var_os("WRELA_CONST_CACHE").filter(|d| !d.is_empty());
    let dir = named.map(PathBuf::from).or_else(|| {
        let home = std::env::var_os("HOME").filter(|d| !d.is_empty())?;
        Some(PathBuf::from(home).join(".cache/wrela/consts"))
    })?;
    std::fs::create_dir_all(&dir).ok()?;
    dir.canonicalize().ok()
}
