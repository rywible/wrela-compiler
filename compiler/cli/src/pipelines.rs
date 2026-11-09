//! `wrela pipelines <package-dir> [--set NAME=VALUE]... [--json]`: the pipeline-count query (language.md §18.9). Each
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
    let out = if args.set.is_empty() {
        wrela_driver::check_pipelines(&args.dir)
    } else {
        wrela_driver::check_pipelines_set(&args.dir, &args.set)
    };
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

/// The report for people. A site with no [`PipelineSummary::conditional`] (a compile other than
/// `check_pipelines`) is shown without one.
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
            let tier = if p.tier() == 1 { ", tier 1" } else { "" };
            let work = if p.work.is_empty() {
                String::new()
            } else {
                let each: Vec<String> = p
                    .work
                    .iter()
                    .enumerate()
                    .map(|(i, w)| match (w, p.unbounded.get(i).filter(|s| !s.is_empty())) {
                        (Some(w), _) => w.to_string(),
                        (None, Some(at)) => format!("unbounded (the loop at {at})"),
                        (None, None) => "unbounded".to_string(),
                    })
                    .collect();
                format!(", work {}, weight {}", each.join(" + "), p.weight)
            };
            if p.stands_in && p.sites.is_empty() {
                s.push_str(&format!(
                    "  {} ({}, a stand-in{tier}{work}), run in another's place until it's built\n",
                    p.entries.join(" + "),
                    p.kind
                ));
                continue;
            }
            let standin = match p.standin.and_then(|j| pipelines.get(j)) {
                Some(c) => format!(", stand-in {}", c.names.last().map_or("", |n| n.as_str())),
                None => String::new(),
            };
            s.push_str(&format!(
                "  {} ({}{tier}{work}{standin}), {verb} at {}\n",
                p.entries.join(" + "),
                p.kind,
                (p.sites.iter().enumerate())
                    .map(|(i, s)| match p.conditional.get(i) {
                        Some(true) => format!("{s} (under a condition)"),
                        _ => s.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            s.push_str(&few_text(p));
        }
    }
    s
}

/// Each entry point's calls with few numbers that vary (§18.13), one line each, under the
/// pipeline: what a table over those numbers could replace.
fn few_text(p: &PipelineSummary) -> String {
    let mut s = String::new();
    for (i, calls) in p.few.iter().enumerate() {
        for c in calls.iter().take(FEW_SHOWN) {
            let work = c.work.map_or_else(|| "unbounded".to_string(), |w| w.to_string());
            let times =
                if c.calls == 1 { "once".to_string() } else { format!("{} times", c.calls) };
            let of = if p.few.len() > 1 {
                format!(" ({})", p.names.get(i).map_or("", |n| n.as_str()))
            } else {
                String::new()
            };
            let at = if c.at.is_empty() { String::new() } else { format!(", first at {}", c.at) };
            s.push_str(&format!(
                "    `{}`{of}: {times} an invocation, {} varying, work {work}{at}\n",
                c.function,
                crate::count(c.varying as usize, "number", "numbers"),
            ));
        }
        if calls.len() > FEW_SHOWN {
            s.push_str(&format!(
                "    and {} more (`--json` lists them)\n",
                calls.len() - FEW_SHOWN
            ));
        }
    }
    s
}

/// The most of an entry point's calls with few varying numbers the report shows.
const FEW_SHOWN: usize = 6;

/// The report as JSON, for tools: `{"pipelines": [...], "entry_points": [...]}`.
pub fn json(pipelines: &[PipelineSummary]) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let list =
        |xs: &[String]| format!("[{}]", xs.iter().map(|x| q(x)).collect::<Vec<_>>().join(", "));
    let ps: Vec<String> = pipelines
        .iter()
        .map(|p| {
            format!(
                "{{\"kind\": {}, \"entries\": {}, \"sites\": {}, \"conditional\": [{}], \"tier\": {}{}, \"work\": [{}], \"weight\": {}{}{}}}",
                q(p.kind),
                list(&p.entries),
                list(&p.sites),
                p.conditional.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", "),
                p.tier(),
                p.attachment_bytes.map_or_else(String::new, |b| format!(", \"attachment_bytes\": {b}")),
                p.work
                    .iter()
                    .map(|w| w.map_or_else(|| "null".to_string(), |w| w.to_string()))
                    .collect::<Vec<_>>()
                    .join(", "),
                p.weight,
                match (p.standin, p.stands_in) {
                    (Some(j), _) => format!(", \"standin\": {j}"),
                    (None, true) => ", \"stands_in\": true".to_string(),
                    _ => String::new(),
                },
                if p.few.iter().all(|c| c.is_empty()) {
                    String::new()
                } else {
                    format!(
                        ", \"few\": [{}]",
                        p.few
                            .iter()
                            .map(|calls| format!(
                                "[{}]",
                                calls
                                    .iter()
                                    .map(|c| format!(
                                        "{{\"function\": {}, \"varying\": {}, \"calls\": {}, \"work\": {}, \"at\": {}}}",
                                        q(&c.function),
                                        c.varying,
                                        c.calls,
                                        c.work.map_or_else(|| "null".to_string(), |w| w.to_string()),
                                        q(&c.at)
                                    ))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            )
        })
        .collect();
    let es: Vec<String> = by_entry(pipelines)
        .into_iter()
        .map(|(n, ps)| format!("{{\"name\": {}, \"instantiations\": {}}}", q(n), ps.len()))
        .collect();
    format!("{{\"pipelines\": [{}], \"entry_points\": [{}]}}\n", ps.join(", "), es.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites_show_without_their_conditions() {
        let p = PipelineSummary {
            kind: "compute",
            entries: vec!["fill".into()],
            names: vec!["fill".into()],
            sites: vec!["main.wrela:3:5".into(), "main.wrela:9:5".into()],
            conditional: Vec::new(),
            attachment_bytes: None,
            work: Vec::new(),
            weight: 0,
            unbounded: Vec::new(),
            standin: None,
            stands_in: false,
            few: Vec::new(),
        };
        assert!(text(std::slice::from_ref(&p)).contains("at main.wrela:3:5, main.wrela:9:5\n"));
        let p = PipelineSummary { conditional: vec![false, true], ..p };
        assert!(
            text(std::slice::from_ref(&p))
                .contains("at main.wrela:3:5, main.wrela:9:5 (under a condition)\n")
        );
        assert!(json(std::slice::from_ref(&p)).contains("\"tier\": 0"));
        // Every site under a condition: tier 1.
        let p = PipelineSummary { conditional: vec![true, true], ..p };
        assert!(text(std::slice::from_ref(&p)).contains("  fill (compute, tier 1), dispatched at"));
        assert!(json(std::slice::from_ref(&p)).contains("\"tier\": 1"));
        assert!(!json(std::slice::from_ref(&p)).contains("\"few\""));
        // Its calls with few varying numbers, under it.
        let sky = wrela_driver::FewCall {
            function: "sky".into(),
            varying: 3,
            calls: 4,
            work: Some(1600),
            at: "main.wrela:7:9".into(),
        };
        let p = PipelineSummary { few: vec![vec![sky; 8]], ..p };
        let t = text(std::slice::from_ref(&p));
        assert!(
            t.contains(
                "    `sky`: 4 times an invocation, 3 numbers varying, work 1600, first at main.wrela:7:9\n"
            ),
            "{t}"
        );
        assert!(t.contains("    and 2 more (`--json` lists them)\n"), "{t}");
        assert!(json(&[p]).contains(
            "\"few\": [[{\"function\": \"sky\", \"varying\": 3, \"calls\": 4, \"work\": 1600, \"at\": \"main.wrela:7:9\"}"
        ));
    }
}
