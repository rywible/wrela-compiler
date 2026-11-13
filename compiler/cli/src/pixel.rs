//! `wrela pixel <package-dir> --frame <n> --at <x>,<y> [--draw <k>] [--fragment <j>]
//! [--input <script.json>] [--size <w>x<h>] [--line <file>:<line>] [--full] [--json]`: why a
//! pixel of the screen has the colour it has (language.md §23.9). Every draw of frame `n` that
//! touched it, with the pixel before and after each; the draw that decided it (the last that
//! changed it, or `--draw k`); that draw's fragment there; and its fragment shader run again in
//! the interpreter of GPU code for that one pixel, with each source line's values: how often it
//! ran, its first and last values and its extremes, and the first value that wasn't finite.
//! Each operation WGSL leaves undefined is named.
//!
//! `wrela pixel <package-dir> --frame <n> --kernel <label> --id <i>` interprets one invocation
//! of the last dispatch of frame `n` under that label (or of that pipeline or entry point).
//!
//! The program runs from `init` with the same input each time, on the native host's GPU (under
//! `WRELA_ALLOW_SOFTWARE=1`, a software adapter's), as often as the frame has draws: the host
//! stops the frame's GPU work before each draw in turn (`wrela_host::Probe`), which draws the
//! same picture as a whole frame. Exit status: 0 answered (also when no draw touched the
//! pixel), 1 the build or the run failed, 2 a usage error.

use std::path::PathBuf;
use std::process::ExitCode;
use wrela_driver::pixel::{Query, Report, What, place};

const USAGE: &str = "usage: wrela pixel <package-dir> --frame <n> (--at <x>,<y> [--draw <k>] [--fragment <j>] | --kernel <label> --id <i>) [--input <script.json>] [--size <w>x<h>] [--line <file>:<line>] [--full] [--json]";

struct Args {
    dir: PathBuf,
    frame: u32,
    what: What,
    input: Option<PathBuf>,
    size: (u32, u32),
    line: Option<(String, u32)>,
    full: bool,
    json: bool,
}

pub fn run(args: &[String]) -> ExitCode {
    let Some(a) = parse(args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match answer(&a) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse(args: &[String]) -> Option<Args> {
    let (mut dir, mut frame, mut at, mut draw, mut fragment) = (None, None, None, None, None);
    let (mut kernel, mut id, mut input, mut size, mut line) = (None, None, None, (640, 360), None);
    let (mut full, mut json) = (false, false);
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--frame" => frame = Some(it.next()?.parse().ok()?),
            "--at" => {
                let (x, y) = it.next()?.split_once(',')?;
                at = Some([x.trim().parse().ok()?, y.trim().parse().ok()?]);
            }
            "--draw" => draw = Some(it.next()?.parse().ok().filter(|&k: &usize| k > 0)?),
            "--fragment" => fragment = Some(it.next()?.parse().ok()?),
            "--kernel" => kernel = Some(it.next()?.clone()),
            "--id" => id = Some(it.next()?.parse().ok()?),
            "--input" => input = Some(PathBuf::from(it.next()?)),
            "--size" => size = crate::parse_size(it.next()?)?,
            "--line" => {
                let (f, l) = it.next()?.rsplit_once(':')?;
                line = Some((f.to_string(), l.parse().ok()?));
            }
            "--full" => full = true,
            "--json" => json = true,
            _ if dir.is_none() && !x.starts_with('-') => dir = Some(PathBuf::from(x)),
            _ => return None,
        }
    }
    let what = match (at, kernel, id) {
        (Some(at), None, None) => What::Pixel { at, draw, fragment },
        (None, Some(label), Some(id)) if draw.is_none() && fragment.is_none() => {
            What::Kernel { label, id }
        }
        _ => return None,
    };
    Some(Args { dir: dir?, frame: frame?, what, input, size, line, full, json })
}

fn answer(a: &Args) -> Result<String, String> {
    let out = wrela_driver::gpu_modules(&a.dir, wrela_driver::BuildKind::default())
        .map_err(|out| wrela_diag::render::render_all(&out.sources, &out.diagnostics))?;
    let built = a.dir.join("build").join("pixel");
    crate::generated_path(&a.dir, &built)?;
    crate::write_build(&out, &built)?;
    if let What::Pixel { at, .. } = a.what
        && (at[0] >= a.size.0 || at[1] >= a.size.1)
    {
        return Err(format!(
            "({}, {}) isn't on the {}x{} screen",
            at[0], at[1], a.size.0, a.size.1
        ));
    }
    let q = Query {
        frame: a.frame,
        size: a.size,
        script: crate::read_script(a.input.as_deref())?,
        fps: 60.0,
        what: a.what.clone(),
    };
    let r = wrela_driver::pixel::query(&built, &out.gpu, &q)?;
    let json = report_json(a, &out.sources, &r);
    if a.full {
        // One value a line, in order.
        let mut s = String::new();
        for (span, xs) in &r.full {
            let at = span.map(|sp| place(&out.sources, sp));
            let v = serde_json::json!({
                "file": at.as_ref().map(|p| p.0.clone()),
                "line": at.as_ref().map(|p| p.1),
                "value": xs,
            });
            s += &format!("{v}\n");
        }
        return Ok(s);
    }
    if a.json {
        return Ok(serde_json::to_string_pretty(&json).expect("JSON") + "\n");
    }
    Ok(text(&json))
}

fn colour(c: [u8; 4]) -> Vec<f64> {
    c.iter().map(|&x| (f64::from(x) / 255.0 * 1e4).round() / 1e4).collect()
}

fn report_json(a: &Args, sources: &wrela_diag::SourceMap, r: &Report) -> serde_json::Value {
    use serde_json::json;
    let loc = |span: Option<wrela_diag::Span>| {
        span.map(|s| {
            let (file, line, col, text) = place(sources, s);
            json!({"file": file, "line": line, "col": col, "text": text})
        })
    };
    let marks = |ms: &[wrela_ir::interp::Mark]| -> Vec<serde_json::Value> {
        ms.iter()
            .map(|m| {
                let mut v = loc(m.at).unwrap_or(json!({}));
                v["what"] = json!(m.what);
                v
            })
            .collect()
    };
    let keep_line = |file: &str, line: u32| {
        a.line.as_ref().is_none_or(|(f, l)| *l == line && file.ends_with(f.as_str()))
    };
    let lines: Vec<serde_json::Value> = r
        .lines
        .iter()
        .filter_map(|l| {
            let (file, line, _, text) = place(sources, l.span);
            keep_line(&file, line).then(|| {
                json!({"file": file, "line": line, "text": text, "ran": l.ran,
                       "first": one(&l.first), "last": one(&l.last),
                       "least": one(&l.least), "most": one(&l.most)})
            })
        })
        .collect();
    let branches: Vec<serde_json::Value> = r
        .branches
        .iter()
        .map(|b| {
            let (file, line, _, _) = place(sources, b.span);
            json!({"file": file, "line": line, "taken": b.taken, "not_taken": b.not_taken})
        })
        .collect();
    let differs = match (&r.interpreted, &r.gpu) {
        (Some(i), Some(g)) => Some(
            i.iter()
                .zip(g)
                .map(|(a, b)| f64::from((a.clamp(0.0, 1.0) - b).abs()))
                .fold(0.0, f64::max),
        ),
        _ => None,
    };
    let mut out = match &a.what {
        What::Pixel { at, .. } => json!({
            "frame": a.frame,
            "at": at,
            "target": "screen",
            "history": r.history.iter().map(|h| json!({
                "draw": h.draw, "label": h.label, "pipeline": h.pipeline,
                "vertices": h.vertices, "instances": h.instances,
                "before": colour(h.before), "after": colour(h.after),
                "changed": h.before != h.after,
            })).collect::<Vec<_>>(),
            "traced": r.traced.map(|d| json!({
                "draw": d, "fragment": r.fragment, "fragments": r.fragments.len(),
                "vertex": r.vertex, "shader": r.shader,
                "varyings": r.varyings.as_ref().map(wrela_ir::interp::concrete::floats_of),
                "depth": r.depth,
                "passed": r.fragment.map(|j| r.fragments[j].passed),
            })),
        }),
        What::Kernel { label, id } => json!({"frame": a.frame, "kernel": label, "id": id,
                                               "shader": r.shader}),
    };
    out["result"] = json!({"interpreted": r.interpreted, "gpu": r.gpu, "differs_by": differs});
    out["first_not_finite"] = loc(r.first_not_finite).unwrap_or(serde_json::Value::Null);
    out["undefined_on_gpu"] = json!(marks(&r.undefined));
    out["approximate"] = json!(marks(&r.approximate));
    out["unsupported"] = json!(r.unsupported);
    out["lines"] = json!(lines);
    out["branches"] = json!(branches);
    out
}

/// A scalar's value as a number; a vector's as its components.
fn one(xs: &[f32]) -> serde_json::Value {
    if xs.len() == 1 { serde_json::json!(xs[0]) } else { serde_json::json!(xs) }
}

/// The report as text: the history, then each line beside its values.
fn text(j: &serde_json::Value) -> String {
    let mut s = String::new();
    if let Some(at) = j.get("at") {
        s += &format!("frame {}, pixel {}\n", j["frame"], at);
        let history = j["history"].as_array().cloned().unwrap_or_default();
        if history.is_empty() {
            s += "no draw touched it\n";
        }
        for h in history {
            s += &format!(
                "  draw {} {}{}: {} -> {}{}\n",
                h["draw"],
                h["pipeline"].as_str().unwrap_or("?"),
                h["label"].as_str().map(|l| format!(" ({l})")).unwrap_or_default(),
                h["before"],
                h["after"],
                if h["changed"] == true { "" } else { " (unchanged)" }
            );
        }
        if !j["traced"].is_null() {
            let t = &j["traced"];
            s += &format!(
                "traced draw {}, fragment {} of {}: {} then {}, depth {}\n",
                t["draw"], t["fragment"], t["fragments"], t["vertex"], t["shader"], t["depth"]
            );
        }
    } else {
        s += &format!("frame {}, kernel {}, invocation {}\n", j["frame"], j["kernel"], j["id"]);
    }
    s += &format!(
        "result: interpreted {}, GPU {}\n",
        j["result"]["interpreted"], j["result"]["gpu"]
    );
    if let Some(u) = j["unsupported"].as_str() {
        s += &format!("not interpreted: the interpreter doesn't run {u}\n");
    }
    if !j["first_not_finite"].is_null() {
        let f = &j["first_not_finite"];
        s += &format!(
            "first value not finite: {}:{}: {}\n",
            f["file"].as_str().unwrap_or("?"),
            f["line"],
            f["text"].as_str().unwrap_or("")
        );
    }
    for m in j["undefined_on_gpu"].as_array().into_iter().flatten() {
        s += &format!(
            "undefined on the GPU: {} at line {}\n",
            m["what"].as_str().unwrap_or(""),
            m["line"]
        );
    }
    for l in j["lines"].as_array().into_iter().flatten() {
        s += &format!(
            "{:>5} | {}\n      ran {}, first {}, last {}, least {}, most {}\n",
            l["line"],
            l["text"].as_str().unwrap_or(""),
            l["ran"],
            l["first"],
            l["last"],
            l["least"],
            l["most"]
        );
    }
    for b in j["branches"].as_array().into_iter().flatten() {
        s += &format!(
            "{:>5} | branch taken {}, not taken {}\n",
            b["line"], b["taken"], b["not_taken"]
        );
    }
    s
}
