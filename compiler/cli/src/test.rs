//! `wrela test <package-dir> [<filter>] [--json]`: runs the package's `@test` functions
//! (language.md §10), or those whose names contain `filter`. Each runs as a debug build's
//! constants are computed, on memory of its own; a test passes unless it panics. Exit status 1
//! if a test failed or the package has errors.

use std::process::ExitCode;
use wrela_diag::Diagnostic;
use wrela_diag::json::{JSON_VERSION, diagnostic_json};
use wrela_driver::TestOutput;

pub fn run(args: &[String]) -> ExitCode {
    // The package comes first; a second word is the filter.
    let (filter, rest): (Vec<&String>, Vec<&String>) = {
        let mut words = 0;
        args.iter().partition(|a| {
            if a.starts_with('-') {
                return false;
            }
            words += 1;
            words == 2
        })
    };
    let rest: Vec<String> = rest.into_iter().cloned().collect();
    let args = match crate::package_args(&rest, false) {
        Ok(a) => a,
        Err(status) => return status,
    };
    let filter = filter.first().map(|f| f.as_str());
    let out = wrela_driver::test(&args.dir, filter);
    // A filter that chose nothing is an error, so a misspelt name can't look like a pass.
    if let Some(f) = filter
        && out.results.is_empty()
        && out.filtered_out > 0
    {
        eprintln!("error: no test's name contains `{f}`");
        return ExitCode::from(1);
    }
    if args.json {
        print!("{}", json(&out));
    } else {
        eprint!("{}", wrela_diag::render::render_all(&out.sources, &out.diagnostics));
        print!("{}", text(&out));
    }
    if out.passed() { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

/// The report for people: a line for each test, why each failed one failed, and a count.
pub fn text(out: &TestOutput) -> String {
    let mut s = String::new();
    for r in &out.results {
        let verdict = if r.failure.is_some() { "FAILED" } else { "ok" };
        s.push_str(&format!("{verdict} {} ({})\n", r.name, r.at));
    }
    let failed: Vec<&Diagnostic> = out.results.iter().filter_map(|r| r.failure.as_ref()).collect();
    for d in &failed {
        s.push('\n');
        s.push_str(&wrela_diag::render::render(&out.sources, d));
    }
    if !failed.is_empty() {
        s.push('\n');
    }
    if wrela_diag::has_errors(&out.diagnostics) {
        s.push_str("no tests ran: the package has errors\n");
    } else {
        let passed = out.results.len() - failed.len();
        s.push_str(&format!("{passed} passed, {} failed", failed.len()));
        if out.filtered_out > 0 {
            s.push_str(&format!(", {} left out by the filter", out.filtered_out));
        }
        s.push('\n');
    }
    s
}

/// The report for tools: the diagnostics that kept the tests from running, and each test with
/// its result. A failed test's `failure` is a diagnostic in the format of `wrela check --json`.
pub fn json(out: &TestOutput) -> String {
    let diagnostics: Vec<_> =
        out.diagnostics.iter().map(|d| diagnostic_json(&out.sources, d)).collect();
    let tests: Vec<_> = out
        .results
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.name,
                "at": r.at,
                "passed": r.failure.is_none(),
                "failure": r.failure.as_ref().map(|d| diagnostic_json(&out.sources, d)),
            })
        })
        .collect();
    let all = serde_json::json!({
        "version": JSON_VERSION,
        "diagnostics": diagnostics,
        "tests": tests,
        "filtered_out": out.filtered_out,
    });
    format!("{all}\n")
}
