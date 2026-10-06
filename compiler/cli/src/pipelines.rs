//! `wrela pipelines <package-dir> [--json]`: the pipeline-count query (language.md §7). Each
//! GPU entry point, how many pipelines instantiate it, the type arguments of each, and where
//! CPU code dispatches or draws it: what to look at when a program makes more shaders than it
//! means to (D-044, D-070).

use std::process::ExitCode;
use wrela_driver::PipelineSummary;

pub fn run(args: &[String]) -> ExitCode {
    let args = match crate::package_args(args, false) {
        Ok(a) => a,
        Err(status) => return status,
    };
    let out = wrela_driver::check(&args.dir);
    if out.has_errors() {
        crate::report(&out, args.json);
        return ExitCode::from(1);
    }
    if args.json {
        print!("{}", json(&out.pipelines));
    } else {
        print!("{}", text(&out.pipelines));
    }
    ExitCode::SUCCESS
}

/// Each entry point's name, in the order pipelines first name them, with the pipelines that
/// instantiate it.
fn by_entry(pipelines: &[PipelineSummary]) -> Vec<(&str, Vec<&PipelineSummary>)> {
    let mut out: Vec<(&str, Vec<&PipelineSummary>)> = Vec::new();
    for p in pipelines {
        for name in &p.names {
            match out.iter_mut().find(|(n, _)| n == name) {
                Some((_, ps)) => ps.push(p),
                None => out.push((name, vec![p])),
            }
        }
    }
    out
}

/// The report for people.
pub fn text(pipelines: &[PipelineSummary]) -> String {
    let entries = by_entry(pipelines);
    let mut s = format!(
        "{} from {}\n",
        crate::count(pipelines.len(), "pipeline", "pipelines"),
        crate::count(entries.len(), "entry point", "entry points")
    );
    for (name, ps) in entries {
        s.push_str(&format!(
            "\n`{name}`: {}\n",
            crate::count(ps.len(), "instantiation", "instantiations")
        ));
        for p in ps {
            let verb = if p.kind == "compute" { "dispatched" } else { "drawn" };
            s.push_str(&format!(
                "  {} ({}), {verb} at {}\n",
                p.entries.join(" + "),
                p.kind,
                p.sites.join(", ")
            ));
        }
    }
    s
}

/// The report as JSON, for tools: `{"pipelines": [...], "entry_points": [...]}`.
pub fn json(pipelines: &[PipelineSummary]) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let list =
        |xs: &[String]| format!("[{}]", xs.iter().map(|x| q(x)).collect::<Vec<_>>().join(", "));
    let ps: Vec<String> = pipelines
        .iter()
        .map(|p| {
            format!(
                "{{\"kind\": {}, \"entries\": {}, \"sites\": {}}}",
                q(p.kind),
                list(&p.entries),
                list(&p.sites)
            )
        })
        .collect();
    let es: Vec<String> = by_entry(pipelines)
        .into_iter()
        .map(|(n, ps)| format!("{{\"name\": {}, \"instantiations\": {}}}", q(n), ps.len()))
        .collect();
    format!("{{\"pipelines\": [{}], \"entry_points\": [{}]}}\n", ps.join(", "), es.join(", "))
}
