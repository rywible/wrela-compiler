//! `wrela sliders <package-dir> <module>::<fn> --free <file>[:<lines or names>]... [--top <k>]
//! [--depth <n>] [--region <lo>..<hi>,...] [--move <i>=<amount>]... [--write] [--sheet
//! <file.png>] [--json]`: the independent ways a surface's literals change it (language.md
//! §23.10). `<fn>` takes nothing and returns a `Surface`.
//!
//! The matrix: at up to 512 points on the surface (stage E's edge cells' centres, each moved onto
//! the surface by two Newton steps), how far the surface moves along its normal for a unit change
//! of each freed literal, `J[i][j] = -(∂f/∂θ_j)(p_i) / |∇f(p_i)|`, from `std::lift::jacobian` of
//! the distances, 64 points and 16 literals a call. Scaled by each literal's own size,
//! `s_j = max(|θ_j|, q_j)` (`q_j` ten of its last written decimal), the sliders are `J diag(s)`'s
//! leading right singular vectors, by orthogonal iteration in `f64` (Golub and Van Loan): the
//! directions in literal space that move the surface most, each at right angles to those before.
//!
//! Each slider is named by rule from what it does (the two parts that move most, whether it
//! grows, shrinks or shifts the surface, and whether its motion is mirrored across x = 0), with
//! the root mean square motion of a unit, the literals that make it, and the parts. Dead literals
//! (a zero column) and redundant pairs (two columns at a cosine over 0.999) are listed too.
//! `--move 2=1.5` adds 1.5 units of slider 2 (each literal `1.5 s_j v_j`), rounded to each
//! literal's decimals; with `--write` the edits are written as `wrela solve --write` writes them.
//! `--sheet` draws each slider at -1, 0 and +1 units from the side, 96 pixels a picture.
//! Exit status: 0 answered, 1 the build or the run failed, 2 a usage error.

use crate::solve::{Free, Lit, freed, named, parse_free};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wrela_driver::edit::LiteralEdit;
use wrela_host::{CpuBuild, CpuHost, Value};

const USAGE: &str = "usage: wrela sliders <package-dir> <module>::<fn> --free <file>[:<lines or names>]... [--top <k>] [--depth <n>] [--region <lo>..<hi>,<lo>..<hi>,<lo>..<hi>] [--move <i>=<amount>]... [--write] [--sheet <file.png>] [--json]";

struct Args {
    dir: PathBuf,
    function: String,
    free: Vec<Free>,
    top: usize,
    depth: u32,
    region: Option<[(f32, f32); 3]>,
    moves: Vec<(usize, f64)>,
    write: bool,
    sheet: Option<PathBuf>,
    json: bool,
}

pub fn run(args: &[String]) -> ExitCode {
    let Some(a) = parse(args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match sliders(&a) {
        Ok(r) => {
            if a.json {
                println!("{}", serde_json::to_string_pretty(&r).expect("JSON"));
            } else {
                print!("{}", text(&r));
            }
            ExitCode::SUCCESS
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
        free: Vec::new(),
        top: 8,
        depth: 6,
        region: None,
        moves: Vec::new(),
        write: false,
        sheet: None,
        json: false,
    };
    let mut plain = Vec::new();
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--free" => a.free.push(parse_free(it.next()?)?),
            "--top" => a.top = it.next()?.parse().ok().filter(|&k| k > 0)?,
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
            "--move" => {
                let (i, amount) = it.next()?.split_once('=')?;
                a.moves.push((i.parse().ok().filter(|&i| i > 0)?, amount.parse().ok()?));
            }
            "--write" => a.write = true,
            "--sheet" => a.sheet = Some(PathBuf::from(it.next()?)),
            "--json" => a.json = true,
            _ if !x.starts_with("--") => plain.push(x.clone()),
            _ => return None,
        }
    }
    let [dir, function] = <[String; 2]>::try_from(plain).ok()?;
    (function.contains("::") && !a.free.is_empty()).then_some(())?;
    a.dir = dir.into();
    a.function = function;
    Some(a)
}

/// The program around the subject.
fn glue(package: &str, function: &str) -> String {
    let path = format!("{package}::{function}");
    let name = path.rsplit("::").next().unwrap_or(&path).to_string();
    format!(
        "// Written by `wrela sliders`.

use std::derive::Box3
use std::field::{{Surface, View, part_share, pieces, preview}}
use std::io::print
use std::lift::{{Literal, count, jacobian, literal}}
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

/// Up to 512 points on the surface: edge cells' centres, two Newton steps onto it; each with
/// the size of the gradient there and the part that decides it.
pub fn points(lx: f32, ly: f32, lz: f32, hx: f32, hy: f32, hz: f32, depth: u32) {{
    let f = {name}()
    let p = pieces(f, Box3 {{ lo: vec3(lx, ly, lz), hi: vec3(hx, hy, hz) }}, depth)
    let n = p.edges.len()
    let stride = max(1, (n + 511) / 512)
    var i = 0
    while i < n {{
        var q = p.edges[i]
        for _ in 0..2 {{
            let (d, g) = f.sample(q)
            q = q - g * (d / max(dot(g, g), 0.000001))
        }}
        let (d, g) = f.sample(q)
        let (_, part) = f.part_at(q)
        if abs(d) < 0.01 && length(g) > 0.000001 {{
            print(f\"point {{bits(q.x)}} {{bits(q.y)}} {{bits(q.z)}} {{bits(length(g))}} {{f.part_name(part).name}}\")
        }}
        i += stride
    }}
}}

pub fn set_point(s: mut Points, i: u32, x: f32, y: f32, z: f32) {{
    s.at[i] = vec3(x, y, z)
}}

pub fn value(i: u32) -> f32 {{
    if let Some(l) = literal(i) {{ l.value() }} else {{ 0.0 }}
}}

pub fn set(i: u32, v: f32) {{
    if let Some(l) = literal(i) {{
        l.set(v)
    }}
}}

/// The distances' derivatives by literals, through a parameter (§14.2).
fn by(f: fn() -> [f32; 64], which: [Literal; 16]) -> [[f32; 64]; 16] {{
    let (_, d) = jacobian(f, which)
    d
}}

/// The derivatives of the 64 points' distances by literals `k0` to `k15`, a line a literal.
pub fn slopes(s: Points, k0: u32, k1: u32, k2: u32, k3: u32, k4: u32, k5: u32, k6: u32, k7: u32, k8: u32, k9: u32, k10: u32, k11: u32, k12: u32, k13: u32, k14: u32, k15: u32) {{
    let at = s.at
    let ks = [k0, k1, k2, k3, k4, k5, k6, k7, k8, k9, k10, k11, k12, k13, k14, k15]
    let last = max(count(), 1) - 1
    var which = [Literal {{ index: 0 }}; 16]
    for j in 0..16 {{
        which[j] = Literal {{ index: min(ks[j], last) }}
    }}
    let d = by(|| {{
        var out = [0.0; 64]
        for i in 0..64 {{
            out[i] = {name}().distance(at[i])
        }}
        out
    }}, which)
    for j in 0..16 {{
        var line = f\"slope {{j}}\"
        for i in 0..64 {{
            line = f\"{{line}} {{bits(d[j][i])}}\"
        }}
        print(line)
    }}
}}

pub fn row(lx: f32, ly: f32, lz: f32, hx: f32, hy: f32, hz: f32, size: u32, r: u32) {{
    let px = preview({name}(), View::Side, Box3 {{ lo: vec3(lx, ly, lz), hi: vec3(hx, hy, hz) }}, size, r)
    for i in 0..size {{
        let c = px[i]
        print(f\"px {{u32(saturate(c.x) * 255.0 + 0.5)}} {{u32(saturate(c.y) * 255.0 + 0.5)}} {{u32(saturate(c.z) * 255.0 + 0.5)}} {{u32(saturate(c.w) * 255.0 + 0.5)}}\")
    }}
}}
"
    )
}

fn call(host: &mut CpuHost, name: &str, args: &[Value]) -> Result<Vec<Value>, String> {
    host.call_export(name, args).map_err(|e| format!("`{name}` failed: {e}"))
}

fn bits(w: &str) -> Result<f32, String> {
    w.parse::<u32>().map(f32::from_bits).map_err(|_| format!("a line that doesn't read: {w}"))
}

/// The top `k` right singular vectors of `a` (`rows` × `cols`, row by row) and their singular
/// values, by orthogonal iteration on `aᵀa` applied as two products (Golub and Van Loan): `k`
/// start vectors from a fixed seed, 60 rounds of multiply and Gram–Schmidt, then each value from
/// its vector's image. Largest first. A direction with no motion has a value of 0.
pub(crate) fn top_singular(a: &[f64], rows: usize, cols: usize, k: usize) -> Vec<(f64, Vec<f64>)> {
    let k = k.min(cols);
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    };
    let mut v: Vec<Vec<f64>> = (0..k).map(|_| (0..cols).map(|_| next()).collect()).collect();
    let apply = |x: &[f64]| -> Vec<f64> {
        // aᵀ (a x).
        let ax: Vec<f64> =
            (0..rows).map(|i| (0..cols).map(|j| a[i * cols + j] * x[j]).sum()).collect();
        (0..cols).map(|j| (0..rows).map(|i| a[i * cols + j] * ax[i]).sum()).collect()
    };
    // Gram–Schmidt twice over (once loses orthogonality where a residual is at rounding
    // level); a residual under 1e-10 of the vector is no direction at all, not noise blown up.
    let orthonormal = |v: &mut Vec<Vec<f64>>| {
        for i in 0..v.len() {
            let before = v[i].iter().map(|x| x * x).sum::<f64>().sqrt();
            for _ in 0..2 {
                for p in 0..i {
                    let d: f64 = v[i].iter().zip(&v[p]).map(|(x, y)| x * y).sum();
                    let prev = v[p].clone();
                    for (x, y) in v[i].iter_mut().zip(prev) {
                        *x -= d * y;
                    }
                }
            }
            let n = v[i].iter().map(|x| x * x).sum::<f64>().sqrt();
            if n > 1e-10 * before && n > 1e-300 {
                v[i].iter_mut().for_each(|x| *x /= n);
            } else {
                v[i].iter_mut().for_each(|x| *x = 0.0);
            }
        }
    };
    orthonormal(&mut v);
    for _ in 0..60 {
        v = v.iter().map(|x| apply(x)).collect();
        orthonormal(&mut v);
    }
    let mut out: Vec<(f64, Vec<f64>)> = v
        .into_iter()
        .map(|x| {
            let ax: Vec<f64> =
                (0..rows).map(|i| (0..cols).map(|j| a[i * cols + j] * x[j]).sum()).collect();
            (ax.iter().map(|y| y * y).sum::<f64>().sqrt(), x)
        })
        .collect();
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    out
}

/// A literal's unit step: its size, or ten of its last written decimal where that's larger.
fn step_of(value: f32, decimals: u32) -> f64 {
    let q = 10f64.powi(1 - decimals as i32);
    f64::from(value.abs()).max(q)
}

/// A point on the surface: where, how steep the distance is, and the part.
struct Point {
    at: [f32; 3],
    slope: f32,
    part: String,
}

fn sliders(a: &Args) -> Result<serde_json::Value, String> {
    use serde_json::json;
    let names = a.free.iter().map(|f| named(&a.dir, f)).collect::<Result<Vec<_>, _>>()?;
    let (glue_dir, package) =
        crate::glue_package(&a.dir, "sliders", |package| glue(package, &a.function))?;
    let mut lifted: Vec<String> = a
        .free
        .iter()
        .map(|f| format!("{package}/{}", f.file.to_string_lossy().replace('\\', "/")))
        .collect();
    lifted.sort();
    lifted.dedup();
    let out = wrela_driver::build_lifted(&glue_dir, &lifted, Default::default(), &[])?;
    let built = glue_dir.join("out");
    crate::write_build(&out, &built)?;
    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(built.join("lift.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let lits = freed(&a.dir, &glue_dir, &a.free, &names, &report)?;
    if lits.is_empty() {
        return Err("no lifted literal is on the lines named".into());
    }
    let mut host = CpuBuild::load(&built)
        .map(CpuBuild::quiet)
        .and_then(|b| b.start())
        .map_err(|e| format!("can't run the program: {e}"))?;
    let region = match a.region {
        Some(r) => r,
        None => {
            let b: Vec<f32> = call(&mut host, "bound", &[])?
                .iter()
                .filter_map(|v| if let Value::F32(x) = v { Some(*x) } else { None })
                .collect();
            if b[3] >= 1.0e29 {
                return Err(format!("`{}` is unbounded: give `--region`", a.function));
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
    args.push(Value::I32(a.depth as i32));
    host.take_logs();
    call(&mut host, "points", &args)?;
    let mut points = Vec::new();
    for line in host.take_logs() {
        let w: Vec<&str> = line.split(' ').collect();
        if w.first() == Some(&"point") && w.len() >= 5 {
            points.push(Point {
                at: [bits(w[1])?, bits(w[2])?, bits(w[3])?],
                slope: bits(w[4])?,
                part: w[5..].join(" "),
            });
        }
    }
    if points.is_empty() {
        return Err("no point of the surface was found in the region".into());
    }
    let values: Vec<f32> = lits
        .iter()
        .map(|l| match call(&mut host, "value", &[Value::I32(l.index as i32)]) {
            Ok(v) => match v.as_slice() {
                [Value::F32(x)] => Ok(*x),
                other => Err(format!("`value` returned {other:?}")),
            },
            Err(e) => Err(e),
        })
        .collect::<Result<_, _>>()?;
    let steps: Vec<f64> = lits.iter().zip(&values).map(|(l, v)| step_of(*v, l.decimals)).collect();
    // The matrix, scaled: rows the points, columns the literals.
    let (rows, cols) = (points.len(), lits.len());
    let mut j = vec![0.0f64; rows * cols];
    for (c0, chunk) in points.chunks(64).enumerate() {
        for i in 0..64 {
            let p = &chunk[i % chunk.len()];
            let args = [
                Value::I32(i as i32),
                Value::F32(p.at[0]),
                Value::F32(p.at[1]),
                Value::F32(p.at[2]),
            ];
            call(&mut host, "set_point", &args)?;
        }
        for (l0, group) in lits.chunks(16).enumerate() {
            let ks: Vec<Value> =
                (0..16).map(|t| Value::I32(group[t % group.len()].index as i32)).collect();
            host.take_logs();
            call(&mut host, "slopes", &ks)?;
            for line in host.take_logs() {
                let w: Vec<&str> = line.split(' ').collect();
                if w.first() != Some(&"slope") {
                    continue;
                }
                let t: usize = w[1].parse().map_err(|_| "a slope line that doesn't read")?;
                if t >= group.len() {
                    continue;
                }
                let col = l0 * 16 + t;
                for (i, x) in w[2..].iter().enumerate().take(chunk.len()) {
                    let row = c0 * 64 + i;
                    let d = f64::from(bits(x)?);
                    j[row * cols + col] = -d / f64::from(points[row].slope) * steps[col];
                }
            }
        }
    }
    let top = top_singular(&j, rows, cols, a.top);
    let column = |c: usize| (0..rows).map(|r| j[r * cols + c]).collect::<Vec<f64>>();
    let norm = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
    let largest = (0..cols).map(|c| norm(&column(c))).fold(0.0, f64::max);
    let dead: Vec<usize> =
        (0..cols).filter(|&c| norm(&column(c)) <= 1e-12 * largest.max(1e-30)).collect();
    let mut redundant = Vec::new();
    for x in 0..cols {
        for y in x + 1..cols {
            let (cx, cy) = (column(x), column(y));
            let (nx, ny) = (norm(&cx), norm(&cy));
            if nx == 0.0 || ny == 0.0 {
                continue;
            }
            let cos = cx.iter().zip(&cy).map(|(p, q)| p * q).sum::<f64>() / (nx * ny);
            if cos.abs() > 0.999 {
                redundant.push(json!({
                    "literals": [describe(&lits[x]), describe(&lits[y])],
                    "cosine": cos, "ratio": nx / ny * (steps[y] / steps[x]),
                }));
            }
        }
    }
    // Each point's mirror image across x = 0: the nearest point to it.
    let mirror: Vec<usize> = points
        .iter()
        .map(|p| {
            let m = [-p.at[0], p.at[1], p.at[2]];
            (0..rows)
                .min_by(|&u, &v| {
                    let d =
                        |k: usize| (0..3).map(|c| (points[k].at[c] - m[c]).powi(2)).sum::<f32>();
                    d(u).total_cmp(&d(v))
                })
                .unwrap_or(0)
        })
        .collect();
    let mut sliders = Vec::new();
    for (n, (sigma, mut v)) in top.into_iter().enumerate() {
        let mut motion: Vec<f64> =
            (0..rows).map(|r| (0..cols).map(|c| j[r * cols + c] * v[c]).sum()).collect();
        // A direction's sign is free: the one that grows the surface on the whole.
        if motion.iter().sum::<f64>() < 0.0 {
            v.iter_mut().for_each(|x| *x = -*x);
            motion.iter_mut().for_each(|x| *x = -*x);
        }
        let rms = sigma / (rows as f64).sqrt();
        let squared: f64 = motion.iter().map(|m| m * m).sum();
        let mut by_part: std::collections::BTreeMap<String, f64> = Default::default();
        for (p, m) in points.iter().zip(&motion) {
            let name = if p.part.is_empty() { "unnamed".to_string() } else { p.part.clone() };
            *by_part.entry(name).or_default() += m * m;
        }
        let mut parts: Vec<(String, f64)> =
            by_part.into_iter().map(|(k, x)| (k, x / squared.max(1e-300))).collect();
        parts.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mean = motion.iter().sum::<f64>() / rows as f64;
        let sense = if mean.abs() < 0.2 * rms {
            "shifts"
        } else if mean > 0.0 {
            "grows"
        } else {
            "shrinks"
        };
        let same: f64 =
            (0..rows).map(|r| motion[r] * motion[mirror[r]]).sum::<f64>() / squared.max(1e-300);
        let side = if same > 0.8 { "(mirrored)" } else { "(one side)" };
        let top_parts: Vec<&str> = parts.iter().take(2).map(|(n, _)| n.as_str()).collect();
        let name = format!("{}: {sense} {side}", top_parts.join(", "));
        let mut lit_rank: Vec<(usize, f64)> =
            v.iter().enumerate().map(|(c, x)| (c, x * x)).collect();
        lit_rank.sort_by(|a, b| b.1.total_cmp(&a.1));
        sliders.push(json!({
            "slider": n + 1,
            "name": name,
            "singular_value": sigma,
            "rms_motion": rms,
            "literals": lit_rank.iter().take(12).filter(|(_, s)| *s > 1e-6).map(|&(c, share)| {
                let mut d = describe(&lits[c]);
                d["share"] = json!(share);
                d["direction"] = json!(v[c]);
                d
            }).collect::<Vec<_>>(),
            "parts": parts.iter().take(6).map(|(n, s)| json!({"part": n, "share": s})).collect::<Vec<_>>(),
            "mirrored": same,
            "vector": v,
        }));
    }
    // `--move`: the literals' new values, each rounded to its decimals.
    let mut moved: Vec<f64> = values.iter().map(|&x| f64::from(x)).collect();
    for &(i, amount) in &a.moves {
        let s = sliders.get(i - 1).ok_or_else(|| format!("there is no slider {i}"))?;
        let v: Vec<f64> = serde_json::from_value(s["vector"].clone()).map_err(|e| e.to_string())?;
        for c in 0..cols {
            moved[c] += amount * steps[c] * v[c];
        }
    }
    let mut edits = Vec::new();
    for (c, l) in lits.iter().enumerate() {
        let scale = 10f64.powi(l.decimals as i32);
        let rounded = ((moved[c] * scale).round() / scale) as f32;
        if !a.moves.is_empty() && rounded.to_bits() != values[c].to_bits() {
            edits.push((c, rounded));
        }
    }
    let mut answer = json!({
        "subject": a.function,
        "points": rows,
        "literals": cols,
        "sliders": sliders,
        "dead": dead.iter().map(|&c| describe(&lits[c])).collect::<Vec<_>>(),
        "redundant": redundant,
        "moves": edits.iter().map(|&(c, v)| {
            let mut d = describe(&lits[c]);
            d["before"] = json!(values[c]);
            d["after"] = json!(v);
            d
        }).collect::<Vec<_>>(),
        "written": false,
    });
    if a.write && !edits.is_empty() {
        let planned_edits: Vec<LiteralEdit> =
            edits.iter().map(|&(c, value)| LiteralEdit { value, ..lits[c].edit.clone() }).collect();
        let planned = wrela_driver::edit::plan(&glue_dir, &planned_edits)
            .map_err(|r| format!("`wrela edit` refused `{}`: {}", r.file, r.why))?;
        wrela_driver::edit::write(&glue_dir, &planned).map_err(|e| e.to_string())?;
        answer["written"] = true.into();
    }
    if let Some(sheet) = &a.sheet {
        draw_sheet(&mut host, sheet, &corners, &lits, &values, &steps, &answer["sliders"])?;
        answer["sheet"] = json!(sheet.display().to_string());
    }
    Ok(answer)
}

fn describe(l: &Lit) -> serde_json::Value {
    serde_json::json!({"literal": l.index, "file": l.file, "line": l.line, "name": l.name, "text": l.edit.text})
}

/// A PNG of a row for each slider, three pictures across: the subject from the side at -1, 0
/// and +1 units of it, 96 pixels square each. Nothing is written to the source.
fn draw_sheet(
    host: &mut CpuHost,
    path: &Path,
    corners: &[Value],
    lits: &[Lit],
    values: &[f32],
    steps: &[f64],
    sliders: &serde_json::Value,
) -> Result<(), String> {
    const SIZE: usize = 96;
    let list = sliders.as_array().cloned().unwrap_or_default();
    let (w, h) = (3 * SIZE, list.len().max(1) * SIZE);
    let mut rgba = vec![0u8; w * h * 4];
    for (n, s) in list.iter().enumerate() {
        let v: Vec<f64> = serde_json::from_value(s["vector"].clone()).map_err(|e| e.to_string())?;
        for (k, amount) in [-1.0f64, 0.0, 1.0].iter().enumerate() {
            for (c, l) in lits.iter().enumerate() {
                let x = f64::from(values[c]) + amount * steps[c] * v[c];
                call(host, "set", &[Value::I32(l.index as i32), Value::F32(x as f32)])?;
            }
            for r in 0..SIZE {
                let mut args = corners.to_vec();
                args.extend([Value::I32(SIZE as i32), Value::I32(r as i32)]);
                host.take_logs();
                call(host, "row", &args)?;
                let px: Vec<u8> = host
                    .take_logs()
                    .iter()
                    .filter_map(|line| line.strip_prefix("px "))
                    .flat_map(|p| {
                        p.split(' ').map(|c| c.parse::<u8>().unwrap_or(0)).collect::<Vec<_>>()
                    })
                    .collect();
                let at = ((n * SIZE + r) * w + k * SIZE) * 4;
                rgba[at..at + SIZE * 4].copy_from_slice(&px[..SIZE * 4]);
            }
        }
    }
    for (c, l) in lits.iter().enumerate() {
        call(host, "set", &[Value::I32(l.index as i32), Value::F32(values[c])])?;
    }
    wrela_host::image::write_png(path, w as u32, h as u32, &rgba).map_err(|e| e.to_string())
}

/// The report as text: a paragraph a slider.
fn text(r: &serde_json::Value) -> String {
    let mut s = format!(
        "{}: {} points, {} literals\n",
        r["subject"].as_str().unwrap_or(""),
        r["points"],
        r["literals"]
    );
    for sl in r["sliders"].as_array().into_iter().flatten() {
        s += &format!(
            "slider {}: {} ({:.4} m a unit)\n",
            sl["slider"],
            sl["name"].as_str().unwrap_or(""),
            sl["rms_motion"].as_f64().unwrap_or(0.0)
        );
        for l in sl["literals"].as_array().into_iter().flatten().take(5) {
            s += &format!(
                "    {}:{} {} ({:.0}%)\n",
                l["file"].as_str().unwrap_or(""),
                l["line"],
                l["text"].as_str().unwrap_or(""),
                100.0 * l["share"].as_f64().unwrap_or(0.0)
            );
        }
    }
    for d in r["dead"].as_array().into_iter().flatten() {
        s += &format!(
            "dead: {}:{} {}\n",
            d["file"].as_str().unwrap_or(""),
            d["line"],
            d["text"].as_str().unwrap_or("")
        );
    }
    for d in r["redundant"].as_array().into_iter().flatten() {
        s += &format!("redundant: {}\n", d["literals"]);
    }
    for m in r["moves"].as_array().into_iter().flatten() {
        s += &format!(
            "move {}:{} {} -> {}\n",
            m["file"].as_str().unwrap_or(""),
            m["line"],
            m["before"],
            m["after"]
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::top_singular;

    /// A 6 × 4 matrix with known singular vectors: two orthonormal directions in literal space
    /// scaled by 3 and 1, the rest none.
    #[test]
    fn a_matrix_with_known_singular_vectors() {
        // A = 3 u1 v1ᵀ + 1 u2 v2ᵀ.
        let s = 0.5f64;
        let v1 = [s, s, s, s];
        let v2 = [s, -s, s, -s];
        let u1 = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let u2 = [0.0, 0.6, 0.8, 0.0, 0.0, 0.0];
        let mut a = vec![0.0; 24];
        for i in 0..6 {
            for j in 0..4 {
                a[i * 4 + j] = 3.0 * u1[i] * v1[j] + u2[i] * v2[j];
            }
        }
        let top = top_singular(&a, 6, 4, 4);
        assert!((top[0].0 - 3.0).abs() < 1e-9 && (top[1].0 - 1.0).abs() < 1e-9, "{top:?}");
        let dot = |x: &[f64], y: &[f64]| x.iter().zip(y).map(|(p, q)| p * q).sum::<f64>();
        assert!((dot(&top[0].1, &v1).abs() - 1.0).abs() < 1e-9);
        assert!((dot(&top[1].1, &v2).abs() - 1.0).abs() < 1e-9);
        assert!(top[2].0 < 1e-9 && top[3].0 < 1e-9);
    }

    /// A matrix of rank 2 gives two sliders and zeros.
    #[test]
    fn rank_two_gives_two_sliders() {
        let rows = 10;
        let cols = 5;
        let mut a = vec![0.0; rows * cols];
        for i in 0..rows {
            let (x, y) = (i as f64, (i * i) as f64 / 10.0);
            a[i * cols] = x;
            a[i * cols + 1] = y;
            a[i * cols + 2] = x + y;
            a[i * cols + 3] = 2.0 * x;
            a[i * cols + 4] = y - x;
        }
        let top = top_singular(&a, rows, cols, 5);
        assert!(top[0].0 > 1.0 && top[1].0 > 1e-3);
        assert!(top[2].0 < 1e-6 * top[0].0, "{top:?}");
    }
}
