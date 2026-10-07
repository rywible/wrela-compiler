//! `wrela trace <package-dir> --watch <export>[,<export>...] [--frames n] [--csv]` runs a
//! program's frames and prints what its exports say after each: an agent watching a simulation.
//! `wrela bisect <package-dir> --until "<export> <op> <number>" [--frames n] [--json]` runs
//! frames until a condition holds, and says at which frame (`<op>` is `<`, `<=`, `>`, `>=`,
//! `==` or `!=`; `nan <export>` holds when the export says NaN; `<export>.x` picks a vector's
//! component, and `<export>.5` any value by its place, as a struct's are returned). Both take
//! `[--fps f] [--size WxH] [--input script.json] [--release]`.
//!
//! An export watched takes no arguments but the program's state (if it has one: the host passes
//! it), and returns numbers. The program is a debug build (language.md §11's checks, so a
//! simulation that indexes out of bounds stops at the frame it did), unless `--release`; it runs
//! on the GPU if it has pipelines, else on the CPU. Frame `i` runs at time `i / fps` (60 unless
//! said). Exit status: 0; for `bisect`, 1 if the condition never held; 2 a usage error or a
//! failure to build or run.

use std::path::PathBuf;
use std::process::ExitCode;
use wrela_host::{CpuHost, Host, Value};

struct Args {
    dir: PathBuf,
    watch: Vec<String>,
    until: Option<String>,
    frames: u32,
    fps: f64,
    size: (u32, u32),
    input: Option<PathBuf>,
    release: bool,
    csv: bool,
    json: bool,
}

fn parse(args: &[String], bisect: bool) -> Option<Args> {
    let mut a = Args {
        dir: PathBuf::new(),
        watch: Vec::new(),
        until: None,
        frames: if bisect { 3600 } else { 60 },
        fps: 60.0,
        size: (640, 480),
        input: None,
        release: false,
        csv: false,
        json: false,
    };
    let mut dir = None;
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--watch" if !bisect => {
                a.watch.extend(it.next()?.split(',').map(|s| s.trim().to_string()))
            }
            "--until" if bisect => a.until = Some(it.next()?.clone()),
            "--frames" => a.frames = it.next()?.parse().ok()?,
            "--fps" => a.fps = it.next()?.parse().ok()?,
            "--size" => a.size = crate::parse_size(it.next()?)?,
            "--input" => a.input = Some(PathBuf::from(it.next()?)),
            "--release" => a.release = true,
            "--csv" if !bisect => a.csv = true,
            "--json" if bisect => a.json = true,
            _ if dir.is_none() && !x.starts_with('-') => dir = Some(PathBuf::from(x)),
            _ => return None,
        }
    }
    a.dir = dir?;
    if (!bisect && a.watch.is_empty()) || (bisect && a.until.is_none()) {
        return None;
    }
    Some(a)
}

/// A program running, on the GPU or the CPU.
enum Runner {
    Gpu(Box<Host>),
    Cpu(Box<CpuHost>),
}

impl Runner {
    fn frame(&mut self, t: f32, size: (u32, u32)) -> Result<(), String> {
        match self {
            Runner::Gpu(h) => h.frame(t, size.0, size.1),
            Runner::Cpu(h) => h.frame(t, size.0, size.1),
        }
        .map_err(|e| e.to_string())
    }

    fn call(&mut self, name: &str) -> Result<Vec<f64>, String> {
        let out = match self {
            Runner::Gpu(h) => h.call_export(name, &[]),
            Runner::Cpu(h) => h.call_export(name, &[]),
        }
        .map_err(|e| format!("`{name}`: {e}"))?;
        Ok(out
            .iter()
            .map(|v| match *v {
                Value::I32(x) => f64::from(x),
                Value::I64(x) => x as f64,
                Value::F32(x) => f64::from(x),
                Value::F64(x) => x,
            })
            .collect())
    }

    fn push(&mut self, e: wrela_abi::input::Event) {
        match self {
            Runner::Gpu(h) => h.push_input(e),
            Runner::Cpu(h) => h.push_input(e),
        }
    }
}

/// Builds and loads the program, and reads its input script.
fn start(a: &Args) -> Result<(Runner, Vec<wrela_abi::input::Scripted>), String> {
    let out =
        if a.release { wrela_driver::build(&a.dir) } else { wrela_driver::build_debug(&a.dir) };
    let built = a.dir.join("build").join(if a.release { "trace" } else { "trace-debug" });
    crate::write_build(&out, &built)?;
    let manifest =
        std::fs::read_to_string(built.join("manifest.json")).map_err(|e| e.to_string())?;
    let gpu = serde_json::from_str::<serde_json::Value>(&manifest)
        .ok()
        .and_then(|m| m["pipelines"].as_array().map(|p| !p.is_empty()))
        .unwrap_or(false);
    let runner = if gpu {
        Runner::Gpu(Box::new(Host::load(&built).map_err(|e| e.to_string())?))
    } else {
        // Loading runs the program's `init`.
        Runner::Cpu(Box::new(CpuHost::load(&built).map_err(|e| e.to_string())?))
    };
    let script = match &a.input {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| format!("can't read {}: {e}", p.display()))?;
            wrela_abi::input::parse_script(&text)?
        }
        None => Vec::new(),
    };
    Ok((runner, script))
}

/// Runs frame `i` (its scripted events first): its time, in seconds.
fn step(
    r: &mut Runner,
    a: &Args,
    script: &[wrela_abi::input::Scripted],
    i: u32,
) -> Result<f64, String> {
    for e in wrela_abi::input::events_at(script, i) {
        r.push(e);
    }
    r.frame(wrela_host::frame_time(i, a.fps), a.size).map_err(|e| format!("frame {i}: {e}"))?;
    Ok(f64::from(i) / a.fps)
}

/// A vector's components, by name.
const COMPONENTS: [&str; 4] = ["x", "y", "z", "w"];

/// An export's values' column names: the export's for one value, with `.x`, `.y`, ... for up to
/// four (a vector's), and `.0`, `.1`, ... for more (a struct's, in the order it returns them).
fn columns(name: &str, n: usize) -> Vec<String> {
    match n {
        1 => vec![name.to_string()],
        2..=4 => COMPONENTS[..n].iter().map(|c| format!("{name}.{c}")).collect(),
        _ => (0..n).map(|k| format!("{name}.{k}")).collect(),
    }
}

/// Values as an export's are shown, each as an `f32`, with `sep` between.
fn shown(v: &[f64], sep: &str) -> String {
    v.iter().map(|x| format!("{}", *x as f32)).collect::<Vec<_>>().join(sep)
}

/// The exit status a run returns, or 2 after printing why it failed.
fn finish(result: Result<ExitCode, String>) -> ExitCode {
    result.unwrap_or_else(|why| {
        eprintln!("error: {why}");
        ExitCode::from(2)
    })
}

pub fn trace(args: &[String]) -> ExitCode {
    let Some(a) = parse(args, false) else { return crate::usage() };
    finish(run_trace(&a))
}

fn run_trace(a: &Args) -> Result<ExitCode, String> {
    let (mut r, script) = start(a)?;
    let mut header = false;
    for i in 0..a.frames {
        let t = step(&mut r, a, &script, i)?;
        let mut values = Vec::new();
        for w in &a.watch {
            let v = r.call(w).map_err(|why| format!("after frame {i}: {why}"))?;
            values.push((w.as_str(), v));
        }
        if a.csv {
            if !header {
                let cols: Vec<String> =
                    values.iter().flat_map(|(n, v)| columns(n, v.len())).collect();
                println!("frame,time,{}", cols.join(","));
                header = true;
            }
            let cells: Vec<f64> = values.iter().flat_map(|(_, v)| v.iter().copied()).collect();
            println!("{i},{},{}", t as f32, shown(&cells, ","));
        } else {
            let parts: Vec<String> =
                values.iter().map(|(n, v)| format!("{n} {}", shown(v, " "))).collect();
            println!("frame {i} ({:.3} s): {}", t, parts.join(", "));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// A condition on an export: its name, which of its values (0 unless `.x`/`.y`/... or a place,
/// `.5`, picks), and what must hold of it.
struct Until {
    export: String,
    component: usize,
    test: Test,
}

enum Test {
    /// A comparison with a number.
    Compare(Comparison, f64),
    Nan,
}

/// Whether a value compares with a number as an operator says.
type Comparison = fn(&f64, &f64) -> bool;

/// The operators `--until` takes.
const COMPARISONS: [(&str, Comparison); 6] = [
    ("<", f64::lt),
    ("<=", f64::le),
    (">", f64::gt),
    (">=", f64::ge),
    ("==", f64::eq),
    ("!=", f64::ne),
];

fn parse_until(text: &str) -> Option<Until> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let (target, test) = match words.as_slice() {
        ["nan", e] | [e, "is", "nan"] => (*e, Test::Nan),
        [e, op, n] => {
            let (_, holds) = COMPARISONS.iter().find(|(o, _)| o == op)?;
            (*e, Test::Compare(*holds, n.parse().ok()?))
        }
        _ => return None,
    };
    let (export, component) = match target.rsplit_once('.') {
        Some((e, c)) => (e, COMPONENTS.iter().position(|k| *k == c).or_else(|| c.parse().ok())?),
        None => (target, 0),
    };
    Some(Until { export: export.to_string(), component, test })
}

impl Until {
    fn holds(&self, v: f64) -> bool {
        match self.test {
            Test::Nan => v.is_nan(),
            Test::Compare(holds, n) => holds(&v, &n),
        }
    }
}

pub fn bisect(args: &[String]) -> ExitCode {
    let Some(a) = parse(args, true) else { return crate::usage() };
    let text = a.until.clone().unwrap_or_default();
    let Some(until) = parse_until(&text) else {
        eprintln!(
            "error: `--until` takes `<export> <op> <number>` or `nan <export>`, not `{text}`"
        );
        return ExitCode::from(2);
    };
    finish(run_bisect(&a, &until, &text))
}

/// Runs frames until `until` (written `text`) holds: success if it does, 1 if it never did.
fn run_bisect(a: &Args, until: &Until, text: &str) -> Result<ExitCode, String> {
    let (mut r, script) = start(a)?;
    for i in 0..a.frames {
        let t = step(&mut r, a, &script, i)?;
        let v = r.call(&until.export).map_err(|why| format!("after frame {i}: {why}"))?;
        let Some(&x) = v.get(until.component) else {
            return Err(format!("`{}` gives {} values", until.export, v.len()));
        };
        if until.holds(x) {
            if a.json {
                println!(
                    "{}",
                    serde_json::json!({ "held": true, "until": text, "frame": i, "time": t, "values": v })
                );
            } else {
                println!(
                    "`{text}` first holds at frame {i} ({t:.3} s): {} {}",
                    until.export,
                    shown(&v, " ")
                );
            }
            return Ok(ExitCode::SUCCESS);
        }
    }
    if a.json {
        println!("{}", serde_json::json!({ "held": false, "until": text, "frames": a.frames }));
    } else {
        println!("`{text}` never held in {} frames", a.frames);
    }
    Ok(ExitCode::from(1))
}
