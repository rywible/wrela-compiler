//! `wrela solve <package-dir> (--minimize <module>::<function> | --fit <module>::<function> |
//! --spec) --free <file>[:<lines or names>]... [--steps n] [--exact] [--write] [--json]`:
//! changes the literals named (those on the lines `12`, `12-20`; those of the value a `let`, a
//! `var` or a `const` of the file gives a name, `reach`; a whole file's, with neither) until the
//! function (a `pub fn` of no arguments returning `f32`, in a module of the package, not its
//! main) is as small as it gets; with `--fit`, until the sum of the squares of the numbers the
//! function gives (it returns `[f32; M]`: how far each of `M` things misses) is; or with
//! `--spec` until the package's spec holds (its checks' `std::lift::Loss`, from
//! `pub fn spec<C: Checks>(c: mut C)` in `spec.wrela`), reporting each check before and after
//! (language.md §23).
//!
//! It writes a program around the function (`<package>/build/solve/`), builds it with the
//! package's literals lifted, and runs it on the CPU, where `std::lift::Minimize` solves: BFGS
//! on the function's derivatives by the literals (`std::lift::gradient`, forward mode), each
//! step along a line searched until the value falls enough (Armijo's rule); a fit, by
//! Levenberg–Marquardt on every number's derivatives (`std::lift::jacobian`). The literals end
//! rounded to their own decimals, as a drag's are: the one that changed most first, the
//! rest solved again with it held, and the last given more decimals while rounding would cost
//! more than 5% of the value (`--exact` keeps them as solved). This finds the literals named
//! and their decimals, passes them in, and reports what came out, with a warning for each
//! literal the function never changed with (its derivative by it was 0 wherever the solve took
//! one). With `--write`, `wrela edit` writes them to the source, unless a spec's solve stopped
//! short: with a check that still misses, nothing is written, and the checks that miss are
//! said. Exit status: 0 solved (and written), 1 the build or the write failed or a spec that
//! doesn't hold wasn't written, 2 a usage error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wrela_driver::edit::LiteralEdit;
use wrela_host::{CpuHost, Value};
use wrela_syntax::ast;

pub fn run(args: &[String]) -> ExitCode {
    let (mut dir, mut target, mut free, mut steps) = (None, None, Vec::new(), 200usize);
    let (mut exact, mut write, mut json, mut spec, mut fit) = (false, false, false, false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--minimize" => match it.next() {
                Some(t) => target = Some(t.clone()),
                None => return crate::usage(),
            },
            "--fit" => match it.next() {
                Some(t) => {
                    target = Some(t.clone());
                    fit = true;
                }
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
            eprintln!(
                "error: give `--minimize <module>::<function>`, `--fit <module>::<function>` or `--spec`, one of them"
            );
            return ExitCode::from(2);
        }
    };
    let Some((module, function)) = target.rsplit_once("::") else {
        eprintln!("error: `--minimize` and `--fit` take `<module>::<function>`, not `{target}`");
        return ExitCode::from(2);
    };
    if free.is_empty() {
        eprintln!(
            "error: name the literals to change with `--free <file>:<lines>` or `--free <file>:<name>`"
        );
        return ExitCode::from(2);
    }
    match solve(&dir, module, function, spec, fit, &free, steps, exact, write) {
        Ok(report) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
            } else {
                print_report(&report);
            }
            // A solve that stopped short, asked to write: reported, and not written.
            if report["refused"].is_string() { ExitCode::from(1) } else { ExitCode::SUCCESS }
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

/// A file and the literals in it that may change: those on lines (`body.wrela:12`,
/// `body.wrela:12-20`), those of the value a name is bound to (`body.wrela:reach`: the file's
/// `let reach = 0.62`), or both (`body.wrela:3,reach,7-9`). A name stays put when the file's
/// lines move.
struct Free {
    file: PathBuf,
    lines: Vec<(u32, u32)>,
    names: Vec<String>,
}

fn parse_free(arg: &str) -> Option<Free> {
    // A file alone: every line of it.
    if !arg.contains(':') {
        return Some(Free { file: PathBuf::from(arg), lines: vec![(1, u32::MAX)], names: vec![] });
    }
    let (file, parts) = arg.rsplit_once(':')?;
    let (mut lines, mut names) = (Vec::new(), Vec::new());
    for part in parts.split(',') {
        let part = part.trim();
        if is_name(part) {
            names.push(part.to_string());
            continue;
        }
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim().parse().ok()?, b.trim().parse().ok()?),
            None => {
                let n = part.parse().ok()?;
                (n, n)
            }
        };
        lines.push((a, b));
    }
    Some(Free { file: PathBuf::from(file), lines, names })
}

/// Whether `s` is written as a name is: a letter or `_`, then letters, digits and `_`.
fn is_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// A value a name is bound to in a file: where its expression is (bytes), and its line.
struct Bound {
    start: u32,
    end: u32,
    line: u32,
}

/// The values `name` is bound to in the `.wrela` file `text`: by a `let` or a `var` of that
/// one name, in any function's body, or by a `const`.
fn bound(text: &str, name: &str) -> Vec<Bound> {
    let parsed = wrela_syntax::parser::parse(wrela_diag::FileId(0), text);
    let mut values: Vec<wrela_diag::Span> = Vec::new();
    let mut in_fn = |d: &ast::FnDecl| {
        let Some(body) = &d.body else { return };
        body.for_each_block(&mut |b| {
            for s in &b.stmts {
                match &s.kind {
                    ast::StmtKind::Let {
                        pat: ast::Pat { kind: ast::PatKind::Ident(id), .. },
                        init,
                        ..
                    }
                    | ast::StmtKind::Var { name: id, init, .. }
                        if id.name == name =>
                    {
                        values.push(init.span);
                    }
                    _ => {}
                }
            }
        });
    };
    for item in &parsed.file.items {
        match &item.kind {
            ast::ItemKind::Fn(d) => in_fn(d),
            ast::ItemKind::Trait(t) => {
                for m in &t.members {
                    if let ast::TraitMemberKind::Fn(d) = &m.kind {
                        in_fn(d);
                    }
                }
            }
            ast::ItemKind::Impl(i) => {
                for m in &i.members {
                    if let ast::ImplMemberKind::Fn(d) = &m.kind {
                        in_fn(d);
                    }
                }
            }
            _ => {}
        }
    }
    for item in &parsed.file.items {
        if let ast::ItemKind::Const(c) = &item.kind
            && c.name.name == name
        {
            values.push(c.value.span);
        }
    }
    let line = |at: u32| 1 + text.as_bytes()[..at as usize].iter().filter(|b| **b == b'\n').count();
    values.sort_by_key(|v| v.start);
    values
        .iter()
        .map(|v| Bound { start: v.start, end: v.end, line: line(v.start) as u32 })
        .collect()
}

/// Where the names of `free` are bound in its file, each to one value: an error for a name the
/// file doesn't bind, or binds more than once (its lines tell them apart).
fn named(dir: &Path, free: &Free) -> Result<Vec<(String, Bound)>, String> {
    if free.names.is_empty() {
        return Ok(Vec::new());
    }
    let file = free.file.display();
    let text = std::fs::read_to_string(dir.join(&free.file))
        .map_err(|e| format!("can't read `{file}`: {e}"))?;
    let mut out = Vec::new();
    for name in &free.names {
        let mut values = bound(&text, name);
        match values.len() {
            0 => {
                return Err(format!(
                    "`{file}` binds no `{name}`: `--free {file}:{name}` frees the literals of `let {name} = ...` (or a `var`'s, or a `const`'s)"
                ));
            }
            1 => out.push((name.clone(), values.remove(0))),
            n => {
                let lines: Vec<String> = values.iter().map(|v| v.line.to_string()).collect();
                return Err(format!(
                    "`{file}` binds `{name}` {n} times (lines {}): name the one meant by its line, `--free {file}:{}`",
                    lines.join(", "),
                    lines[0]
                ));
            }
        }
    }
    Ok(out)
}

/// The program around the function, as `wrela solve` writes it: around the spec's loss, and
/// with an export that prints its report, for `--spec`; around the sum of the squares of the
/// numbers the function gives, for `--fit`.
fn glue(package: &str, module: &str, function: &str, spec: bool, fit: bool) -> String {
    // The program up to its `loss` (and for `--spec`, its `report`), and the call it minimizes.
    let how = if fit { "fit" } else { "run" };
    let (head, call) = if fit {
        let head = format!(
            "// Written by `wrela solve --fit`: {function} of {package}::{module}, with its literals lifted.\n\n\
             use std::lift::{{Literal, Minimize}}\n\
             use {package}::{module}::{function}\n\n\
             pub fn frame(m: mut Minimize, time: f32, width: u32, height: u32) {{}}\n\n\
             pub fn loss() -> f32 {{\n    var sum = 0.0\n    for v in {function}() {{\n        sum += v * v\n    }}\n    sum\n}}\n\n"
        );
        (head, format!("{function}()"))
    } else if spec {
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
         pub fn solve(m: mut Minimize, steps: u32, exact: bool) {{\n    m.{how}(|| {call}, steps, exact)\n}}\n\n\
         pub fn losses(m: Minimize) -> vec3 {{\n    vec3(m.before(), m.solved(), m.after())\n}}\n\n\
         pub fn steps(m: Minimize) -> u32 {{\n    m.steps()\n}}\n\n\
         pub fn moved(m: Minimize, i: u32) -> vec2 {{\n    vec2(m.start(i), m.finish(i))\n}}\n\n\
         pub fn changes(m: Minimize, i: u32) -> bool {{\n    m.changes(i)\n}}\n"
    )
}

/// A literal that may change: its index, where it is, and its edit for `wrela edit` (its value
/// set when it's written).
struct Lit {
    index: u32,
    file: String,
    line: u32,
    /// The name it was freed by (`--free <file>:<name>`), if by one.
    name: Option<String>,
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
    fit: bool,
    free: &[Free],
    steps: usize,
    exact: bool,
    write: bool,
) -> Result<serde_json::Value, String> {
    // Where each name is bound, before the build: a name the file doesn't have is said at once.
    let names = free.iter().map(|f| named(dir, f)).collect::<Result<Vec<_>, _>>()?;
    let (glue_dir, package) =
        crate::glue_package(dir, "solve", |package| glue(package, module, function, spec, fit))?;
    let out = wrela_driver::build_lifted(
        &glue_dir,
        std::slice::from_ref(&package),
        Default::default(),
        &[],
    )?;
    let built = glue_dir.join("out");
    crate::write_build(&out, &built)?;
    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(built.join("lift.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    // The literals named: on a line named, or in the value a name named is bound to.
    let files = report["files"].as_array().map_or(&[][..], Vec::as_slice);
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    // Each `--free`: its file, and where its names are bound.
    let wanted: Vec<_> = free
        .iter()
        .zip(&names)
        .map(|(f, names)| (canonical(&dir.join(&f.file)), f, names.as_slice()))
        .collect();
    let mut lits = Vec::new();
    for l in report["literals"].as_array().into_iter().flatten() {
        let f = &files[l["file"].as_u64().unwrap_or(0) as usize];
        let path = canonical(&glue_dir.join(f["path"].as_str().unwrap_or("")));
        let line = l["line"].as_u64().unwrap_or(0) as u32;
        let at = l["start"].as_u64().unwrap_or(0) as u32;
        let by_name = |names: &[(String, Bound)]| {
            names.iter().find(|(_, b)| b.start <= at && at < b.end).map(|(n, _)| n.clone())
        };
        // The first `--free` that names it; by a name, if any of them names it by one.
        let mut freed: Option<(&Free, Option<String>)> = None;
        for (_, f, names) in wanted.iter().filter(|(p, ..)| *p == path) {
            let name = by_name(names);
            if name.is_none() && !f.lines.iter().any(|&(a, b)| a <= line && line <= b) {
                continue;
            }
            match &mut freed {
                None => freed = Some((f, name)),
                Some((_, by)) if by.is_none() => *by = name,
                Some(_) => {}
            }
        }
        let Some((fr, name)) = freed else { continue };
        let text = l["text"].as_str().unwrap_or("").to_string();
        lits.push(Lit {
            index: l["index"].as_u64().unwrap_or(0) as u32,
            file: fr.file.display().to_string(),
            line,
            name,
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
    // A name was given for its literals: one whose value has none is said, not passed over.
    for (f, names) in free.iter().zip(&names) {
        for (name, b) in names {
            if !lits.iter().any(|l| l.name.as_deref() == Some(name.as_str())) {
                return Err(format!(
                    "`{name}` in `{}` (line {}) is bound to a value with no lifted literal in it",
                    f.file.display(),
                    b.line
                ));
            }
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
    if !before.is_finite() {
        return Err("the function's loss is not finite at the literals' starting values".into());
    }
    if !solved_loss.is_finite() {
        return Err("the function's loss is not finite at the solved values".into());
    }
    if !after.is_finite() {
        return Err("the function's loss is not finite at the rounded solution".into());
    }
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
    // Whether the function changed with each literal, wherever the solve took its derivatives.
    let mut changes = Vec::with_capacity(lits.len());
    for k in 0..lits.len() {
        match call(&mut host, "changes", &[Value::I32(k as i32)])?.as_slice() {
            [Value::I32(b)] => changes.push(*b != 0),
            other => return Err(format!("`changes` returned {other:?}")),
        }
    }
    let what = if spec { "the spec's loss".to_string() } else { format!("{module}::{function}") };
    let warnings: Vec<String> = lits
        .iter()
        .zip(&changes)
        .filter(|(_, changed)| !**changed)
        .map(|(l, _)| {
            format!(
                "{}:{}: {what} never changed with the literal `{}`: its derivative by it was 0 at every step, so the solve left it",
                l.file, l.line, l.edit.text
            )
        })
        .collect();
    let checks_after = if spec { spec_report(&mut host)? } else { serde_json::Value::Null };
    // A spec's solve that stopped short isn't written: one with a check that misses. (A
    // function minimized has no checks to hold it to: it's written where it ended.)
    let missed = checks_after["missed"].as_u64().unwrap_or(0);
    let refused = (write && spec && missed > 0).then(|| {
        let all = checks_after["checks"].as_array().map_or(0, Vec::len);
        format!("{missed} of the spec's {all} checks miss{}", if missed == 1 { "es" } else { "" })
    });
    let mut answer = serde_json::json!({
        "solved": true,
        "minimize": format!("{module}::{function}"),
        "fit": fit,
        "loss_before": num(before),
        "loss_solved": num(solved_loss),
        "loss_after": num(after),
        "steps": taken,
        "rounded": !exact,
        "spec_before": checks_before,
        "spec_after": checks_after,
        "literals": lits.iter().zip(start.iter().zip(&finish)).zip(&changes).map(|((l, (b, a)), changed)| serde_json::json!({
            "literal": l.index, "file": l.file, "line": l.line, "name": l.name, "text": l.edit.text,
            "before": num(*b), "after": num(*a), "changes": changed,
        })).collect::<Vec<_>>(),
        "warnings": warnings,
        "written": false,
    });
    if let Some(why) = refused {
        answer["refused"] = why.into();
    } else if write {
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
    for w in r["warnings"].as_array().into_iter().flatten() {
        eprintln!("warning: {}", w.as_str().unwrap_or(""));
    }
    // A solve not written says why where an error is read, with the checks that miss.
    if let Some(why) = r["refused"].as_str() {
        eprintln!("error: nothing written: {why}");
        for c in r["spec_after"]["checks"].as_array().into_iter().flatten() {
            if c["holds"] != true {
                eprintln!(
                    "  misses {}: {} ({} {}), off by {}",
                    c["name"].as_str().unwrap_or(""),
                    c["value"],
                    c["check"].as_str().unwrap_or(""),
                    c["target"],
                    c["miss"]
                );
            }
        }
    }
}
