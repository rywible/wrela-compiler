//! `wrela solve <package-dir> (--minimize <module>::<function> | --spec) --free <file>[:<lines>]...
//! [--steps n] [--exact] [--write] [--json]`: changes the literals on the lines named (a whole
//! file, with no lines) until the function (a `pub fn` of no arguments returning `f32`, in a
//! module of the package, not its main) is as small as it gets, or with `--spec` until the
//! package's spec holds (its checks' `std::lift::Loss`, from `pub fn spec<C: Checks>(c: mut C)`
//! in `spec.wrela`), reporting each check before and after (language.md §22).
//!
//! It writes a program around the function (`<package>/build/solve/`), builds it with the
//! package's literals lifted, and runs it on the CPU, where `std::lift::Minimize` solves: BFGS
//! on the function's derivatives by the literals (`std::lift::gradient`, forward mode), each
//! step along a line searched until the value falls enough (Armijo's rule). The literals end
//! rounded to their own decimals, as the lens's drags do: the one that changed most first, the
//! rest solved again with it held, and the last given more decimals while rounding would cost
//! more than 5% of the value (`--exact` keeps them as solved). This finds the literals on the
//! lines named and their decimals, passes them in, and reports what came out; with `--write`,
//! `wrela edit` writes them to the source. Exit status: 0 solved (and written), 1 the build or
//! the write failed, 2 a usage error.

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
             use std::lift::{{Literal, Loss, Minimize, Report}}\n\n\
             pub fn frame(m: mut Minimize, time: f32, width: u32, height: u32) {{}}\n\n\
             fn spec_loss() -> f32 {{\n    var l = Loss::new()\n    {package}::spec::spec(mut l)\n    l.total\n}}\n\n\
             pub fn loss() -> f32 {{\n    spec_loss()\n}}\n\n\
             pub fn report() {{\n    var r = Report::new()\n    {package}::spec::spec(mut r)\n    print(r.done().as_str())\n}}\n\n"
        );
        (head, "spec_loss()".to_string())
    } else {
        let head = format!(
            "// Written by `wrela solve`: {function} of {package}::{module}, with its literals lifted.\n\n\
             use std::lift::{{Literal, Minimize}}\n\
             use {package}::{module}::{function}\n\n\
             pub fn frame(m: mut Minimize, time: f32, width: u32, height: u32) {{}}\n\n\
             pub fn loss() -> f32 {{\n    {function}()\n}}\n\n"
        );
        (head, format!("{function}()"))
    };
    format!(
        "{head}pub fn init() -> Minimize {{\n    Minimize::new()\n}}\n\n\
         pub fn free(m: mut Minimize, literal: u32, decimals: u32, unit: f64) {{\n    \
         m.free(Literal {{ index: literal }}, decimals, unit)\n}}\n\n\
         pub fn solve(m: mut Minimize, steps: u32, exact: bool) {{\n    m.run(|| {call}, steps, exact)\n}}\n\n\
         pub fn losses(m: Minimize) -> vec3 {{\n    vec3(m.before(), m.solved(), m.after())\n}}\n\n\
         pub fn steps(m: Minimize) -> u32 {{\n    m.steps()\n}}\n\n\
         pub fn moved(m: Minimize, i: u32) -> vec2 {{\n    vec2(m.start(i), m.finish(i))\n}}\n"
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
    let out =
        wrela_driver::build_lifted(&glue_dir, std::slice::from_ref(&package), Default::default())?;
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
    let call = |host: &mut CpuHost, name: &str, args: &[Value]| {
        host.call_export(name, args).map_err(|e| format!("`{name}` failed: {e}"))
    };
    for l in &lits {
        let args = [Value::I32(l.index as i32), Value::I32(l.decimals as i32), Value::F64(l.unit)];
        call(&mut host, "free", &args)?;
    }
    let checks_before = if spec { spec_report(&mut host)? } else { serde_json::Value::Null };
    call(&mut host, "solve", &[Value::I32(steps as i32), Value::I32(i32::from(exact))])?;
    let (before, solved_loss, after) = match call(&mut host, "losses", &[])?.as_slice() {
        [Value::F32(a), Value::F32(b), Value::F32(c)] => (*a, *b, *c),
        other => return Err(format!("`losses` returned {other:?}")),
    };
    let taken = match call(&mut host, "steps", &[])?.as_slice() {
        [Value::I32(n)] => *n as u32,
        other => return Err(format!("`steps` returned {other:?}")),
    };
    let mut start = Vec::with_capacity(lits.len());
    let mut finish = Vec::with_capacity(lits.len());
    for k in 0..lits.len() {
        match call(&mut host, "moved", &[Value::I32(k as i32)])?.as_slice() {
            [Value::F32(a), Value::F32(b)] => {
                start.push(*a);
                finish.push(*b);
            }
            other => return Err(format!("`moved` returned {other:?}")),
        }
    }
    let checks_after = if spec { spec_report(&mut host)? } else { serde_json::Value::Null };
    let mut answer = serde_json::json!({
        "solved": true,
        "minimize": format!("{module}::{function}"),
        "loss_before": num(before),
        "loss_solved": num(solved_loss),
        "loss_after": num(after),
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
