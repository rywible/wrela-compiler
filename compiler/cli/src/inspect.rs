//! `wrela inspect <package-dir> <module>::<fn> [--depth <n>] [--region <lo>..<hi>,<lo>..<hi>,<lo>..<hi>]
//! [--thin <metres>] [--lift] [--views <dir>] [--json]`: what a surface is made of (language.md
//! §23.10), as findings with a point each. `<fn>` takes nothing and returns a `Surface`, as a
//! spec's subject does.
//!
//! It writes a program around the function (`<package>/build/inspect/`) that runs std's own
//! analysis (`std::field::pieces`, `dead_parts`, `thin_parts`, `preview`) on the CPU and prints
//! what it finds as lines this command reads. The region is the surface's `bound()` grown by a
//! tenth; an unbounded surface needs `--region`. `--depth` is 7 unless said (cells a 128th of the
//! region), at most 9. `--lift` builds the subject's package lifted and finds the literals that
//! move the surface nowhere (`dead-literal`) or almost nowhere (`faint-literal`), from
//! `std::lift::jacobian` of its distance at up to 512 edge cells' centres. `--views <dir>` writes
//! `side.png`, `front.png` and `top.png`, 128 pixels across.
//!
//! Findings are advice: exit status 0 with or without them, 1 if the package doesn't build, 2 a
//! usage error.

use std::path::PathBuf;
use std::process::ExitCode;
use wrela_host::{CpuHost, Value};

const USAGE: &str = "usage: wrela inspect <package-dir> <module>::<fn> [--depth <n>] [--region <lo>..<hi>,<lo>..<hi>,<lo>..<hi>] [--thin <metres>] [--lift] [--views <dir>] [--json]";

struct Args {
    dir: PathBuf,
    function: String,
    depth: u32,
    region: Option<[(f32, f32); 3]>,
    thin: f32,
    lift: bool,
    views: Option<PathBuf>,
    json: bool,
}

pub fn run(args: &[String]) -> ExitCode {
    let Some(a) = parse(args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match inspect(&a) {
        Ok(Ok(r)) => {
            if a.json {
                println!("{}", serde_json::to_string_pretty(&r).expect("JSON"));
            } else {
                print!("{}", text(&r));
            }
            ExitCode::SUCCESS
        }
        Ok(Err(usage)) => {
            eprintln!("error: {usage}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse(args: &[String]) -> Option<Args> {
    let mut a = Args {
        dir: PathBuf::new(),
        function: String::new(),
        depth: 7,
        region: None,
        thin: 0.005,
        lift: false,
        views: None,
        json: false,
    };
    let mut plain = Vec::new();
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--depth" => a.depth = it.next()?.parse().ok().filter(|d| (1..=9).contains(d))?,
            "--region" => {
                let r: Vec<(f32, f32)> = it
                    .next()?
                    .split(',')
                    .map(|r| {
                        let (lo, hi) = r.split_once("..")?;
                        Some((lo.trim().parse().ok()?, hi.trim().parse().ok()?))
                    })
                    .collect::<Option<_>>()?;
                a.region = Some(<[(f32, f32); 3]>::try_from(r).ok()?);
            }
            "--thin" => a.thin = it.next()?.parse().ok().filter(|t: &f32| *t > 0.0)?,
            "--lift" => a.lift = true,
            "--views" => a.views = Some(PathBuf::from(it.next()?)),
            "--json" => a.json = true,
            _ if !x.starts_with("--") => plain.push(x.clone()),
            _ => return None,
        }
    }
    let [dir, function] = <[String; 2]>::try_from(plain).ok()?;
    function.contains("::").then_some(())?;
    a.dir = dir.into();
    a.function = function;
    Some(a)
}

/// The program around the subject.
fn glue(package: &str, function: &str) -> String {
    let path = format!("{package}::{function}");
    let name = path.rsplit("::").next().unwrap_or(&path).to_string();
    format!(
        "// Written by `wrela inspect`.

use std::derive::Box3
use std::field::{{Surface, View, dead_parts, pieces, preview, thin_parts}}
use std::io::print
use std::lift::{{Literal, count, jacobian}}
use {path}

pub struct Points {{
    at: [vec3; 64],
}}

pub fn init() -> Points {{
    Points {{ at: [vec3(0.0); 64] }}
}}

pub fn frame(s: mut Points, time: f32, width: u32, height: u32) {{}}

fn bits(x: f32) -> u32 {{
    bitcast_u32(x)
}}

pub fn bound() -> vec4 {{
    let b = {name}().bound()
    vec4(b.centre, b.radius)
}}

pub fn report(lx: f32, ly: f32, lz: f32, hx: f32, hy: f32, hz: f32, depth: u32, thin: f32) {{
    let f = {name}()
    let r = Box3 {{ lo: vec3(lx, ly, lz), hi: vec3(hx, hy, hz) }}
    let p = pieces(f, r, depth)
    print(f\"cell {{bits(p.cell)}}\")
    for q in p.pieces {{
        let (_, part) = f.part_at(q.at)
        print(f\"piece {{bits(q.lo.x)}} {{bits(q.lo.y)}} {{bits(q.lo.z)}} {{bits(q.hi.x)}} {{bits(q.hi.y)}} {{bits(q.hi.z)}} {{bits(q.at.x)}} {{bits(q.at.y)}} {{bits(q.at.z)}} {{bits(q.volume_lo)}} {{bits(q.volume_hi)}} {{part}} {{f.part_name(part).name}}\")
    }}
    for n in p.necks {{
        print(f\"neck {{bits(n.x)}} {{bits(n.y)}} {{bits(n.z)}}\")
    }}
    for s in p.specks {{
        print(f\"speck {{bits(s.x)}} {{bits(s.y)}} {{bits(s.z)}}\")
    }}
    for d in dead_parts(f, p) {{
        print(f\"dead {{d}} {{f.part_name(d).name}}\")
    }}
    for t in thin_parts(f, r, depth, thin) {{
        print(f\"thin {{t.0}} {{bits(t.1)}} {{f.part_name(t.0).name}}\")
    }}
    let n = p.edges.len()
    let stride = max(1, (n + 511) / 512)
    var i = 0
    while i < n {{
        let e = p.edges[i]
        print(f\"edge {{bits(e.x)}} {{bits(e.y)}} {{bits(e.z)}}\")
        i += stride
    }}
}}

pub fn row(view: u32, lx: f32, ly: f32, lz: f32, hx: f32, hy: f32, hz: f32, size: u32, r: u32) {{
    let v = if view == 0 {{ View::Side }} else if view == 1 {{ View::Front }} else {{ View::Top }}
    let px = preview({name}(), v, Box3 {{ lo: vec3(lx, ly, lz), hi: vec3(hx, hy, hz) }}, size, r)
    for i in 0..size {{
        let c = px[i]
        print(f\"px {{u32(saturate(c.x) * 255.0 + 0.5)}} {{u32(saturate(c.y) * 255.0 + 0.5)}} {{u32(saturate(c.z) * 255.0 + 0.5)}} {{u32(saturate(c.w) * 255.0 + 0.5)}}\")
    }}
}}

pub fn set_point(s: mut Points, i: u32, x: f32, y: f32, z: f32) {{
    s.at[i] = vec3(x, y, z)
}}

/// The distances' derivatives by literals, through a parameter (§14.2).
fn by(f: fn() -> [f32; 64], which: [Literal; 16]) -> [[f32; 64]; 16] {{
    let (_, d) = jacobian(f, which)
    d
}}

/// The largest size of the derivative by each of the 16 literals from `k`, over the 64 points.
pub fn slopes(s: Points, k: u32) -> [f32; 16] {{
    let at = s.at
    // Past the last literal, the last again: its derivative is read once.
    let last = max(count(), 1) - 1
    var which = [Literal {{ index: min(k, last) }}; 16]
    for j in 0..16 {{
        which[j] = Literal {{ index: min(k + j, last) }}
    }}
    let d = by(|| {{
        var out = [0.0; 64]
        for i in 0..64 {{
            out[i] = {name}().distance(at[i])
        }}
        out
    }}, which)
    var most = [0.0; 16]
    for j in 0..16 {{
        for i in 0..64 {{
            most[j] = max(most[j], abs(d[j][i]))
        }}
    }}
    most
}}
"
    )
}

fn f(w: &str) -> Result<f32, String> {
    w.parse::<u32>().map(f32::from_bits).map_err(|_| format!("a line that doesn't read: {w}"))
}

fn call(host: &mut CpuHost, name: &str, args: &[Value]) -> Result<Vec<Value>, String> {
    host.call_export(name, args).map_err(|e| format!("`{name}` failed: {e}"))
}

fn floats(v: &[Value]) -> Vec<f32> {
    v.iter()
        .filter_map(|x| match x {
            Value::F32(x) => Some(*x),
            _ => None,
        })
        .collect()
}

/// The report, or a usage error (an unbounded surface with no `--region`).
fn inspect(a: &Args) -> Result<Result<serde_json::Value, String>, String> {
    use serde_json::json;
    let (glue_dir, package) =
        crate::glue_package(&a.dir, "inspect", |package| glue(package, &a.function))?;
    let out = wrela_driver::build(&glue_dir);
    let built = glue_dir.join("out");
    crate::write_build(&out, &built)?;
    let mut host = wrela_host::CpuBuild::load(&built)
        .map(wrela_host::CpuBuild::quiet)
        .and_then(|b| b.start())
        .map_err(|e| format!("can't run the program: {e}"))?;
    let region = match a.region {
        Some(r) => r,
        None => {
            let b = floats(&call(&mut host, "bound", &[])?);
            if b[3] >= 1.0e29 {
                return Ok(Err(format!(
                    "`{}` is unbounded (its `bound()` is `UNBOUNDED`): give `--region`",
                    a.function
                )));
            }
            let r = b[3] * 1.1;
            [(b[0] - r, b[0] + r), (b[1] - r, b[1] + r), (b[2] - r, b[2] + r)]
        }
    };
    let corners: Vec<Value> = region
        .iter()
        .map(|r| Value::F32(r.0))
        .chain(region.iter().map(|r| Value::F32(r.1)))
        .collect();
    let mut args = corners.clone();
    args.extend([Value::I32(a.depth as i32), Value::F32(a.thin)]);
    host.take_logs();
    call(&mut host, "report", &args)?;
    let lines = host.take_logs();
    let mut findings = Vec::new();
    let (mut cell, mut pieces, mut edges) = (0.0f32, Vec::new(), Vec::new());
    let (mut necks, mut specks) = (0, 0);
    for line in &lines {
        let w: Vec<&str> = line.split(' ').collect();
        match w.first().copied() {
            Some("cell") => cell = f(w[1])?,
            Some("piece") => {
                let v: Vec<f32> = w[1..12].iter().map(|x| f(x)).collect::<Result<_, _>>()?;
                let name = w[13..].join(" ");
                pieces.push((v, w[12].to_string(), name));
            }
            Some("neck") | Some("speck") => {
                let p: Vec<f32> = w[1..4].iter().map(|x| f(x)).collect::<Result<_, _>>()?;
                let kind = w[0];
                if kind == "neck" {
                    necks += 1;
                } else {
                    specks += 1;
                }
                findings.push(json!({
                    "kind": kind, "proof": "certified", "at": p,
                    "what": if kind == "neck" {
                        format!("joined, if at all, by something thinner than a cell: {cell:.4} m")
                    } else {
                        format!("something smaller than a cell ({cell:.4} m) may be here")
                    },
                }));
            }
            Some("dead") => findings.push(json!({
                "kind": "dead-part", "proof": "sampled", "part": w[1].parse::<u32>().unwrap_or(0),
                "name": w[2..].join(" "),
            })),
            Some("thin") => findings.push(json!({
                "kind": "thin", "proof": "certified", "part": w[1].parse::<u32>().unwrap_or(0),
                "thinner_than": f(w[2])?, "name": w[3..].join(" "),
            })),
            Some("edge") => {
                edges.push([f(w[1])?, f(w[2])?, f(w[3])?]);
            }
            _ => {}
        }
    }
    // Each piece apart from the largest floats: certified, apart by at least a cell.
    for (k, (v, part, name)) in pieces.iter().enumerate().skip(1) {
        findings.push(json!({
            "kind": "floating", "proof": "certified", "parts": [if name.is_empty() { format!("part {part}") } else { name.clone() }],
            "gap_at_least": cell, "at": [v[6], v[7], v[8]],
            "box": {"lo": [v[0], v[1], v[2]], "hi": [v[3], v[4], v[5]]},
            "piece": k,
        }));
    }
    if a.lift {
        lifted(a, &package, &edges, &mut findings)?;
    }
    let mut views = serde_json::Map::new();
    if let Some(dir) = &a.views {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        for (k, name) in ["side", "front", "top"].iter().enumerate() {
            let size = 128u32;
            let mut rgba = Vec::with_capacity((size * size * 4) as usize);
            for r in 0..size {
                let mut args = vec![Value::I32(k as i32)];
                args.extend(corners.iter().copied());
                args.extend([Value::I32(size as i32), Value::I32(r as i32)]);
                host.take_logs();
                call(&mut host, "row", &args)?;
                for line in host.take_logs() {
                    if let Some(px) = line.strip_prefix("px ") {
                        rgba.extend(px.split(' ').map(|c| c.parse::<u8>().unwrap_or(0)));
                    }
                }
            }
            let path = dir.join(format!("{name}.png"));
            wrela_host::image::write_png(&path, size, size, &rgba).map_err(|e| e.to_string())?;
            views.insert(name.to_string(), json!(path.display().to_string()));
        }
    }
    let certain = pieces.len();
    Ok(Ok(json!({
        "subject": a.function,
        "region": {"lo": region.map(|r| r.0), "hi": region.map(|r| r.1)},
        "cell": cell,
        "pieces": {"certain": certain, "one": certain == 1 && necks == 0 && specks == 0},
        "findings": findings,
        "views": views,
    })))
}

/// `--lift`: the subject's package built lifted, and each of its literals' largest derivative
/// of the distance over the edge points: 0 at every one is `dead-literal`, under 1e-6 of the
/// largest literal's `faint-literal`. A package whose code is E0700 lifted (it keeps what depends
/// on a literal in heap memory) is a finding of its own, `not-derivable`.
fn lifted(
    a: &Args,
    package: &str,
    edges: &[[f32; 3]],
    findings: &mut Vec<serde_json::Value>,
) -> Result<(), String> {
    use serde_json::json;
    let glue_dir = a.dir.join("build").join("inspect");
    let out =
        wrela_driver::build_lifted(&glue_dir, &[package.to_string()], Default::default(), &[])?;
    if out.has_errors() {
        if let Some(d) = out.diagnostics.iter().find(|d| d.code.as_str() == "E0700")
            && let Some(span) = d.span()
        {
            let file = out.sources.file(span.file);
            let lc = file.line_col(span.start);
            findings.push(json!({
                "kind": "not-derivable", "proof": "certified", "file": file.name, "line": lc.line,
                "col": lc.column, "what": d.message,
            }));
            return Ok(());
        }
        return Err(wrela_diag::render::render_all(&out.sources, &out.diagnostics));
    }
    let built = glue_dir.join("out-lifted");
    crate::write_build(&out, &built)?;
    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(built.join("lift.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let files = report["files"].as_array().cloned().unwrap_or_default();
    let lits: Vec<serde_json::Value> = report["literals"].as_array().cloned().unwrap_or_default();
    let count = lits.iter().filter_map(|l| l["index"].as_u64()).max().map_or(0, |m| m as u32 + 1);
    let mut host = wrela_host::CpuBuild::load(&built)
        .map(wrela_host::CpuBuild::quiet)
        .and_then(|b| b.start())
        .map_err(|e| format!("can't run the program: {e}"))?;
    let mut most = vec![0.0f32; count as usize];
    for chunk in edges.chunks(64) {
        for i in 0..64 {
            let p = chunk[i % chunk.len()];
            let args = [Value::I32(i as i32), Value::F32(p[0]), Value::F32(p[1]), Value::F32(p[2])];
            call(&mut host, "set_point", &args)?;
        }
        let mut k = 0;
        while k < count {
            let d = floats(&call(&mut host, "slopes", &[Value::I32(k as i32)])?);
            for (j, x) in d.iter().enumerate() {
                if let Some(m) = most.get_mut(k as usize + j) {
                    *m = m.max(*x);
                }
            }
            k += 16;
        }
    }
    let largest = most.iter().copied().fold(0.0f32, f32::max);
    for l in &lits {
        let i = l["index"].as_u64().unwrap_or(0) as usize;
        let m = most.get(i).copied().unwrap_or(0.0);
        let kind = if m == 0.0 {
            "dead-literal"
        } else if m < 1e-6 * largest {
            "faint-literal"
        } else {
            continue;
        };
        let file = &files[l["file"].as_u64().unwrap_or(0) as usize];
        findings.push(json!({
            "kind": kind, "proof": "sampled",
            "file": file["path"].as_str().unwrap_or("").trim_start_matches("../../"), "line": l["line"],
            "col": l["column"], "text": l["text"], "samples": edges.len(), "largest": m,
        }));
    }
    Ok(())
}

/// The report as text: a line a finding.
fn text(r: &serde_json::Value) -> String {
    let mut s = format!(
        "{}: {} certain piece(s){}, cells {:.4} m\n",
        r["subject"].as_str().unwrap_or(""),
        r["pieces"]["certain"],
        if r["pieces"]["one"] == true { ", one body" } else { "" },
        r["cell"].as_f64().unwrap_or(0.0)
    );
    for f in r["findings"].as_array().into_iter().flatten() {
        let mut parts = vec![format!(
            "{} ({})",
            f["kind"].as_str().unwrap_or(""),
            f["proof"].as_str().unwrap_or("")
        )];
        for key in
            ["parts", "name", "what", "at", "file", "line", "text", "thinner_than", "gap_at_least"]
        {
            if !f[key].is_null() {
                parts.push(format!("{key} {}", f[key]));
            }
        }
        s += &format!("  {}\n", parts.join(", "));
    }
    for (name, path) in r["views"].as_object().into_iter().flatten() {
        s += &format!("  view {name}: {}\n", path.as_str().unwrap_or(""));
    }
    s
}
