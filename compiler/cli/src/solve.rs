//! `wrela solve <package-dir> (--minimize <module>::<function> | --spec) --free <file>[:<lines>]...
//! [--steps n] [--exact] [--write] [--json]`: changes the literals on the lines named (a whole
//! file, with no lines) until the function (a `pub fn` of no arguments returning `f32`, in a
//! module of the package, not its main) is as small as it gets, or with `--spec` until the
//! package's spec holds (its checks' `std::lift::Loss`, from `pub fn spec<C: Checks>(c: mut C)`
//! in `spec.wrela`), reporting each check before and after (language.md §22).
//!
//! It writes a program around the function (`<package>/build/solve/`), builds it with the
//! package's literals lifted, and runs it on the CPU: the function's value at the literals'
//! values, and its derivative by each (`std::lift::gradient`, forward mode). BFGS takes the
//! steps, each along a line searched until the value falls enough (Armijo's rule). The literals
//! end rounded to their own decimals, as the lens's drags do: the one that changed most first,
//! the rest solved again with it held, and the last given more decimals while rounding would
//! cost more than 5% of the value (`--exact` keeps them as solved). With `--write`, `wrela edit`
//! writes them to the source. Exit status: 0 solved (and written),
//! 1 the build or the write failed, 2 a usage error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wrela_driver::edit::LiteralEdit;
use wrela_host::{CpuHost, Value};

pub fn run(args: &[String]) -> ExitCode {
    let (mut dir, mut target, mut free, mut steps) = (None, None, Vec::new(), 200usize);
    let (mut exact, mut write, mut json, mut spec) = (false, false, false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--minimize" => match it.next() {
                Some(t) => target = Some(t.clone()),
                None => return crate::usage(),
            },
            "--free" => match it.next().and_then(|f| parse_free(f)) {
                Some(f) => free.push(f),
                None => return crate::usage(),
            },
            "--steps" => match it.next().and_then(|s| s.parse().ok()) {
                Some(n) => steps = n,
                None => return crate::usage(),
            },
            "--exact" => exact = true,
            "--spec" => spec = true,
            "--write" => write = true,
            "--json" => json = true,
            _ if dir.is_none() && !a.starts_with('-') => dir = Some(PathBuf::from(a)),
            _ => return crate::unknown(a),
        }
    }
    let Some(dir) = dir else { return crate::usage() };
    let target = match (target, spec) {
        (Some(t), false) => t,
        (None, true) => "spec::spec".to_string(),
        _ => {
            eprintln!("error: give `--minimize <module>::<function>` or `--spec`, one of them");
            return ExitCode::from(2);
        }
    };
    let Some((module, function)) = target.rsplit_once("::") else {
        eprintln!("error: `--minimize` takes `<module>::<function>`, not `{target}`");
        return ExitCode::from(2);
    };
    if free.is_empty() {
        eprintln!("error: name the literals to change with `--free <file>:<lines>`");
        return ExitCode::from(2);
    }
    match solve(&dir, module, function, spec, &free, steps, exact, write) {
        Ok(report) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
            } else {
                print_report(&report);
            }
            ExitCode::SUCCESS
        }
        Err(why) => {
            if json {
                println!("{}", serde_json::json!({ "solved": false, "why": why }));
            } else {
                eprintln!("error: {why}");
            }
            ExitCode::from(1)
        }
    }
}

/// A file and the lines in it whose literals may change: `body.wrela:12`, `body.wrela:12-20`,
/// `body.wrela:3,7-9`.
struct Free {
    file: PathBuf,
    lines: Vec<(u32, u32)>,
}

fn parse_free(arg: &str) -> Option<Free> {
    // A file alone: every line of it.
    if !arg.contains(':') {
        return Some(Free { file: PathBuf::from(arg), lines: vec![(1, u32::MAX)] });
    }
    let (file, lines) = arg.rsplit_once(':')?;
    let mut ranges = Vec::new();
    for part in lines.split(',') {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim().parse().ok()?, b.trim().parse().ok()?),
            None => {
                let n = part.trim().parse().ok()?;
                (n, n)
            }
        };
        ranges.push((a, b));
    }
    Some(Free { file: PathBuf::from(file), lines: ranges })
}

/// The program around the function, as `wrela solve` writes it: around the spec's loss, and
/// with an export that prints its report, for `--spec`.
fn glue(package: &str, module: &str, function: &str, spec: bool) -> String {
    // The program up to its `loss` (and for `--spec`, its `report`), and the call it minimizes.
    let (head, call) = if spec {
        let head = format!(
            "// Written by `wrela solve --spec`: the spec of {package}, with its literals lifted.\n\n\
             use std::io::print\n\
             use std::lift::{{Literal, Loss, Report, count, gradient, literal}}\n\n\
             pub fn frame(time: f32, width: u32, height: u32) {{}}\n\n\
             fn spec_loss() -> f32 {{\n    var l = Loss::new()\n    {package}::spec::spec(mut l)\n    l.total\n}}\n\n\
             pub fn loss() -> f32 {{\n    spec_loss()\n}}\n\n\
             pub fn report() {{\n    var r = Report::new()\n    {package}::spec::spec(mut r)\n    print(r.done().as_str())\n}}\n\n"
        );
        (head, "spec_loss()".to_string())
    } else {
        let head = format!(
            "// Written by `wrela solve`: {function} of {package}::{module}, with its literals lifted.\n\n\
             use std::lift::{{Literal, count, gradient, literal}}\n\
             use {package}::{module}::{function}\n\n\
             pub fn frame(time: f32, width: u32, height: u32) {{}}\n\n\
             pub fn loss() -> f32 {{\n    {function}()\n}}\n\n"
        );
        (head, format!("{function}()"))
    };
    format!(
        "{head}pub fn literals() -> u32 {{\n    count()\n}}\n\n\
         pub fn value(i: u32) -> f32 {{\n    if let Some(l) = literal(i) {{ l.value() }} else {{ 0.0 }}\n}}\n\n\
         pub fn set(i: u32, v: f32) {{\n    if let Some(l) = literal(i) {{\n        l.set(v)\n    }}\n}}\n\n\
         pub fn derivatives4(a: u32, b: u32, c: u32, d: u32) -> vec4 {{\n    \
         let which = [Literal {{ index: a }}, Literal {{ index: b }}, Literal {{ index: c }}, Literal {{ index: d }}]\n    \
         let (_, g) = gradient(|| {call}, which)\n    vec4(g[0], g[1], g[2], g[3])\n}}\n"
    )
}

/// A literal that may change: its index, where it is, and its edit for `wrela edit` (its value
/// set when it's written).
struct Lit {
    index: u32,
    file: String,
    line: u32,
    /// Its text's decimals, and the unit they count in (`15cm`: 0.01; 1 with no unit suffix).
    decimals: u32,
    unit: f64,
    edit: LiteralEdit,
}

#[allow(clippy::too_many_arguments)]
fn solve(
    dir: &Path,
    module: &str,
    function: &str,
    spec: bool,
    free: &[Free],
    steps: usize,
    exact: bool,
    write: bool,
) -> Result<serde_json::Value, String> {
    let package = wrela_driver::manifest::name_in(dir)
        .ok_or_else(|| format!("`{}` has no wrela.toml with a package name", dir.display()))?;
    let glue_dir = dir.join("build").join("solve");
    std::fs::create_dir_all(&glue_dir).map_err(|e| e.to_string())?;
    std::fs::write(
        glue_dir.join("wrela.toml"),
        format!(
            "[package]\nname = \"solve\"\n\n[dependencies]\n{package} = {{ path = \"../..\" }}\n"
        ),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(glue_dir.join("main.wrela"), glue(&package, module, function, spec))
        .map_err(|e| e.to_string())?;
    let out = wrela_driver::build_lifted(&glue_dir, std::slice::from_ref(&package), false)?;
    let built = glue_dir.join("out");
    crate::write_build(&out, &built)?;
    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(built.join("lift.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    // The literals on the lines named.
    let files = report["files"].as_array().cloned().unwrap_or_default();
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let wanted: Vec<(PathBuf, &Free)> =
        free.iter().map(|f| (canonical(&dir.join(&f.file)), f)).collect();
    let mut lits = Vec::new();
    for l in report["literals"].as_array().cloned().unwrap_or_default() {
        let f = &files[l["file"].as_u64().unwrap_or(0) as usize];
        let path = canonical(&glue_dir.join(f["path"].as_str().unwrap_or("")));
        let line = l["line"].as_u64().unwrap_or(0) as u32;
        let Some((_, fr)) = wanted.iter().find(|(p, _)| *p == path) else { continue };
        if fr.lines.iter().any(|&(a, b)| a <= line && line <= b) {
            let text = l["text"].as_str().unwrap_or("").to_string();
            lits.push(Lit {
                index: l["index"].as_u64().unwrap_or(0) as u32,
                file: fr.file.display().to_string(),
                line,
                decimals: decimals(&text),
                unit: wrela_driver::edit::unit_of(&text),
                edit: LiteralEdit {
                    file: f["path"].as_str().unwrap_or("").into(),
                    start: l["start"].as_u64().unwrap_or(0) as u32,
                    end: l["end"].as_u64().unwrap_or(0) as u32,
                    text,
                    hash: f["hash"].as_str().unwrap_or("").into(),
                    value: 0.0,
                },
            });
        }
    }
    if lits.is_empty() {
        return Err("no lifted literal is on the lines named".into());
    }
    let mut host = CpuHost::load(&built).map_err(|e| format!("can't run the program: {e}"))?;
    let mut f = Objective { host: &mut host, lits: &lits };
    let start: Vec<f32> = lits.iter().map(|l| f.value(l.index)).collect();
    let before = f.loss(&start)?;
    let checks_before = if spec { spec_report(f.host)? } else { serde_json::Value::Null };
    let all: Vec<bool> = vec![true; lits.len()];
    let (solved, taken) = bfgs(&mut f, &start, &all, steps)?;
    let solved_loss = f.loss(&solved)?;
    let finish: Vec<f32> =
        if exact { solved.clone() } else { round(&mut f, &start, &solved, solved_loss, steps)? };
    let after = f.loss(&finish)?;
    let checks_after = if spec { spec_report(f.host)? } else { serde_json::Value::Null };
    let mut answer = serde_json::json!({
        "solved": true,
        "minimize": format!("{module}::{function}"),
        "loss_before": num(before as f32),
        "loss_solved": num(solved_loss as f32),
        "loss_after": num(after as f32),
        "steps": taken,
        "rounded": !exact,
        "spec_before": checks_before,
        "spec_after": checks_after,
        "literals": lits.iter().zip(start.iter().zip(&finish)).map(|(l, (b, a))| serde_json::json!({
            "literal": l.index, "file": l.file, "line": l.line, "text": l.edit.text, "before": num(*b), "after": num(*a),
        })).collect::<Vec<_>>(),
        "written": false,
    });
    if write {
        let edits: Vec<LiteralEdit> = lits
            .iter()
            .zip(&finish)
            .zip(&start)
            .filter(|((_, a), b)| a.to_bits() != b.to_bits())
            .map(|((l, &value), _)| LiteralEdit { value, ..l.edit.clone() })
            .collect();
        let planned = wrela_driver::edit::plan(&glue_dir, &edits)
            .map_err(|r| format!("`wrela edit` refused `{}`: {}", r.file, r.why))?;
        wrela_driver::edit::write(&glue_dir, &planned).map_err(|e| e.to_string())?;
        answer["written"] = true.into();
        answer["edit"] = wrela_driver::edit::to_value(&planned, true);
    }
    Ok(answer)
}

/// The spec's report at the literals' values now (the glue's `report` prints it).
fn spec_report(host: &mut CpuHost) -> Result<serde_json::Value, String> {
    let _ = host.take_logs();
    host.call_export("report", &[]).map_err(|e| format!("the spec's report failed: {e}"))?;
    let logs = host.take_logs();
    let line = crate::last_json(&logs).ok_or("the spec printed no report")?;
    serde_json::from_str(line).map_err(|e| format!("the spec's report isn't JSON: {e}"))
}

/// How many decimals a literal's text has (its mantissa's; 0 for an integer).
fn decimals(text: &str) -> u32 {
    let t = text.trim_start_matches('-');
    let mantissa = t.split(['e', 'E']).next().unwrap_or(t);
    let digits = mantissa.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    digits.split_once('.').map_or(0, |(_, d)| d.len() as u32)
}

/// An `f32` as JSON, written as its shortest text (`0.69`, not `0.6899999976158142`): a reader
/// parsing it as an `f32` gets the same value.
fn num(v: f32) -> serde_json::Value {
    v.to_string().parse::<f64>().map(serde_json::Value::from).unwrap_or(serde_json::Value::Null)
}

/// The solution `x` rounded, a literal at a time: the one that changed most from `start` first,
/// to its source's decimals, the rest solved again with it held. A literal gets more decimals
/// while rounding would make the value over 5% worse: the last (to 7) than `solved`, each
/// before it (to three more) than before rounding it.
fn round(
    f: &mut Objective,
    start: &[f32],
    x: &[f32],
    solved: f64,
    steps: usize,
) -> Result<Vec<f32>, String> {
    let n = x.len();
    let mut x = x.to_vec();
    // A literal the solve didn't move keeps its value.
    let mut held: Vec<bool> =
        (0..n).map(|i| (x[i] - start[i]).abs() <= 1e-7 * start[i].abs().max(1.0)).collect();
    for i in 0..n {
        if held[i] {
            x[i] = start[i];
        }
    }
    let moved = held.iter().filter(|h| !**h).count();
    for left in (0..moved).rev() {
        let Some(k) = (0..n)
            .filter(|&i| !held[i])
            .max_by(|&a, &b| (x[a] - start[a]).abs().total_cmp(&(x[b] - start[b]).abs()))
        else {
            break;
        };
        let mut d = f.lits[k].decimals;
        let unit = f.lits[k].unit;
        let mut v = round_to(x[k], d, unit);
        // More decimals where its own would cost too much: the last literal against the solved
        // value; each one before it, up to three more, against the value before rounding it
        // (the rest are solved again after).
        let (enough, most) = if left == 0 {
            (solved * 1.05 + 1e-9, 7)
        } else {
            (f.loss(&x)? * 1.05 + 1e-9, (f.lits[k].decimals + 3).min(7))
        };
        loop {
            let mut trial = x.clone();
            trial[k] = v;
            if d >= most || f.loss(&trial)? <= enough {
                break;
            }
            d += 1;
            v = round_to(x[k], d, unit);
        }
        x[k] = v;
        held[k] = true;
        if left > 0 {
            let free: Vec<bool> = held.iter().map(|h| !h).collect();
            x = bfgs(f, &x, &free, steps).map(|r| r.0)?;
        }
    }
    Ok(x)
}

/// `v` rounded to `decimals` decimals of `unit` (the literal's, `wrela_driver::edit::unit_of`:
/// `15cm`'s 0 decimals are whole centimetres).
fn round_to(v: f32, decimals: u32, unit: f64) -> f32 {
    let k = 10f64.powi(decimals as i32);
    let r = ((f64::from(v) / unit * k).round() / k * unit) as f32;
    if r == 0.0 { 0.0 } else { r }
}

/// The function at literal values, and its gradient, through the program's exports.
struct Objective<'a> {
    host: &'a mut CpuHost,
    lits: &'a [Lit],
}

impl Objective<'_> {
    fn value(&mut self, i: u32) -> f32 {
        match self.host.call_export("value", &[Value::I32(i as i32)]).as_deref() {
            Ok([Value::F32(v)]) => *v,
            _ => 0.0,
        }
    }

    fn set(&mut self, x: &[f32]) -> Result<(), String> {
        for (l, &v) in self.lits.iter().zip(x) {
            self.host
                .call_export("set", &[Value::I32(l.index as i32), Value::F32(v)])
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn loss(&mut self, x: &[f32]) -> Result<f64, String> {
        self.set(x)?;
        match self.host.call_export("loss", &[]).map_err(|e| e.to_string())?.as_slice() {
            [Value::F32(v)] => Ok(f64::from(*v)),
            other => Err(format!("the function returned {other:?}, not an f32")),
        }
    }

    fn gradient(&mut self, x: &[f32]) -> Result<Vec<f64>, String> {
        self.set(x)?;
        let mut g = Vec::with_capacity(x.len());
        for chunk in self.lits.chunks(4) {
            let idx: Vec<Value> = (0..4)
                .map(|k| Value::I32(chunk.get(k).unwrap_or(&chunk[0]).index as i32))
                .collect();
            match self.host.call_export("derivatives4", &idx).map_err(|e| e.to_string())?.as_slice()
            {
                [Value::F32(a), Value::F32(b), Value::F32(c), Value::F32(d)] => {
                    g.extend([*a, *b, *c, *d].iter().take(chunk.len()).map(|&v| f64::from(v)));
                }
                other => return Err(format!("derivatives4 returned {other:?}")),
            }
        }
        Ok(g)
    }
}

/// BFGS from `x0` over the literals `free` says may change, `steps` at most: the literals where
/// it stopped, and the steps taken.
fn bfgs(
    f: &mut Objective,
    x0: &[f32],
    free: &[bool],
    steps: usize,
) -> Result<(Vec<f32>, usize), String> {
    let n = x0.len();
    let mut x: Vec<f64> = x0.iter().map(|&v| f64::from(v)).collect();
    let as32 = |x: &[f64]| -> Vec<f32> { x.iter().map(|&v| v as f32).collect() };
    // A held literal's gradient is 0, so its step is too.
    let masked = |g: Vec<f64>| -> Vec<f64> {
        g.into_iter().zip(free).map(|(v, &m)| if m { v } else { 0.0 }).collect()
    };
    let mut fx = f.loss(&as32(&x))?;
    let mut g = masked(f.gradient(&as32(&x))?);
    // The inverse Hessian's estimate, row-major; the identity to begin with.
    let mut h = identity(n);
    let mut taken = 0;
    for _ in 0..steps {
        let gnorm = dot(&g, &g).sqrt();
        if gnorm < 1e-12 || !gnorm.is_finite() {
            break;
        }
        let mut p: Vec<f64> = matvec(&h, &g).iter().map(|v| -v).collect();
        let mut slope = dot(&p, &g);
        if slope >= 0.0 {
            // Not a descent direction: steepest descent, and the estimate starts again.
            p = g.iter().map(|v| -v).collect();
            slope = -gnorm * gnorm;
            h = identity(n);
        }
        // Armijo's rule, halving the step from 1.
        let mut t = 1.0;
        let mut accepted = None;
        for _ in 0..40 {
            let trial: Vec<f64> = x.iter().zip(&p).map(|(a, b)| a + t * b).collect();
            let ft = f.loss(&as32(&trial))?;
            if ft.is_finite() && ft <= fx + 1e-4 * t * slope {
                accepted = Some((trial, ft));
                break;
            }
            t *= 0.5;
        }
        let Some((next, fnext)) = accepted else { break };
        let gnext = masked(f.gradient(&as32(&next))?);
        let s: Vec<f64> = next.iter().zip(&x).map(|(a, b)| a - b).collect();
        let y: Vec<f64> = gnext.iter().zip(&g).map(|(a, b)| a - b).collect();
        let sy = dot(&s, &y);
        taken += 1;
        let improved = fx - fnext;
        (x, fx, g) = (next, fnext, gnext);
        if sy > 1e-20 {
            // H ← (I − ρ s yᵀ) H (I − ρ y sᵀ) + ρ s sᵀ
            let rho = 1.0 / sy;
            let hy = matvec(&h, &y);
            let yhy = dot(&y, &hy);
            for i in 0..n {
                for j in 0..n {
                    h[i * n + j] +=
                        rho * ((1.0 + rho * yhy) * s[i] * s[j] - hy[i] * s[j] - s[i] * hy[j]);
                }
            }
        }
        if improved.abs() <= 1e-12 * fx.abs().max(1e-30) {
            break;
        }
    }
    Ok((as32(&x), taken))
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The square matrix `m` (row-major) times `v`.
fn matvec(m: &[f64], v: &[f64]) -> Vec<f64> {
    let n = v.len();
    (0..n).map(|i| dot(&m[i * n..(i + 1) * n], v)).collect()
}

/// The `n` by `n` identity matrix, row-major.
fn identity(n: usize) -> Vec<f64> {
    (0..n * n).map(|k| if k % (n + 1) == 0 { 1.0 } else { 0.0 }).collect()
}

fn print_report(r: &serde_json::Value) {
    println!(
        "{}: {} -> {} ({} steps{})",
        r["minimize"].as_str().unwrap_or(""),
        r["loss_before"],
        r["loss_after"],
        r["steps"],
        if r["rounded"] == true { ", rounded to the literals' decimals" } else { "" }
    );
    for l in r["literals"].as_array().into_iter().flatten() {
        println!(
            "  {}:{}  {} -> {}",
            l["file"].as_str().unwrap_or(""),
            l["line"],
            l["before"],
            l["after"]
        );
    }
    if let Some(after) = r["spec_after"]["checks"].as_array() {
        let before = r["spec_before"]["checks"].as_array().cloned().unwrap_or_default();
        println!("the spec: {} of {} hold", r["spec_after"]["held"], after.len());
        for (i, c) in after.iter().enumerate() {
            let was = before.get(i).map(|b| b["value"].clone()).unwrap_or_default();
            println!(
                "  {} {}: {} -> {} ({} {}){}",
                if c["holds"] == true { "holds " } else { "misses" },
                c["name"].as_str().unwrap_or(""),
                was,
                c["value"],
                c["check"].as_str().unwrap_or(""),
                c["target"],
                if c["holds"] == true { String::new() } else { format!(", off by {}", c["miss"]) }
            );
        }
    }
    if r["written"] == true {
        println!("written");
    }
}
