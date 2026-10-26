//! `wrela precision <package-dir> <function> --box <lo>..<hi>[,<lo>..<hi>]... [--box ...]
//! [--tol t] [--json]`: where an `f16` twin of a function would be safe, from its interval
//! (language.md §14). `<function>` is a function of one `f32` or float vector that returns one:
//! `<module>::<name>` in the package (not its main), or a path in std (`std::field::value_noise`).
//! Each `--box` is one box of its input, a range for each component; several are a sampled
//! domain.
//!
//! It writes a program around the function (`<package>/build/precision/`) that calls
//! `std::derive::precision` on each box and runs it on the CPU. That walks the function's
//! interval twice: widened as an `f32` rounds, and as a half float rounds (`2⁻¹¹` an ulp,
//! `2⁻¹⁴` near zero), noting each operation's range. Per operation, in source order: where it
//! is, the operation, its range's width in each walk, their ratio, and a verdict on the half
//! walk's extra width `e` beside the value's magnitude `|v|`: `safe` if `e ≤ tol × max(|v|, 1)`
//! (`--tol`, default `1e-3`) in every box, `watch` within ten times that, else `unsafe`. A value
//! past `f16`'s range anywhere (65504; an integer past 2048, the last one it holds exactly) is
//! `unsafe` by range. The summary gives the share that's `safe` and the first `unsafe` operation
//! the walk reached.
//!
//! It decides nothing: its verdicts are advice from a model of the rounding, not bounds (the
//! half walk doesn't derive in `f16`, which E0700 refuses). A twin written where it says `safe`
//! is proven by measuring it. Exit status: 0 reported, 1 the build or the run failed, 2 a usage
//! error.

use std::collections::HashMap;
use std::process::ExitCode;
use wrela_host::{CpuBuild, Value};

const USAGE: &str = "usage: wrela precision <package-dir> <module>::<function> --box <lo>..<hi>[,<lo>..<hi>]... [--box ...] [--tol t] [--json]";

/// A half float's largest finite value, and the largest integer up to which it holds every one.
const F16_MAX: f64 = 65504.0;
const F16_EXACT: f64 = 2048.0;

struct Args {
    dir: std::path::PathBuf,
    function: String,
    boxes: Vec<Vec<(f32, f32)>>,
    tol: f64,
    json: bool,
}

pub fn run(args: &[String]) -> ExitCode {
    let Some(a) = parse(args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match precision(&a) {
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
        dir: Default::default(),
        function: String::new(),
        boxes: Vec::new(),
        tol: 1e-3,
        json: false,
    };
    let mut plain = Vec::new();
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--box" => a.boxes.push(parse_box(it.next()?)?),
            "--tol" => {
                a.tol = it.next()?.parse().ok().filter(|&t: &f64| t > 0.0 && t.is_finite())?
            }
            "--json" => a.json = true,
            _ if !x.starts_with("--") => plain.push(x.clone()),
            _ => return None,
        }
    }
    let [dir, function] = <[String; 2]>::try_from(plain).ok()?;
    let dims = a.boxes.first()?.len();
    if !(1..=4).contains(&dims) || a.boxes.iter().any(|b| b.len() != dims) {
        return None;
    }
    if !function.contains("::") {
        return None;
    }
    a.dir = dir.into();
    a.function = function;
    Some(a)
}

/// `<lo>..<hi>[,<lo>..<hi>]...`: a range for each of the input's components.
fn parse_box(s: &str) -> Option<Vec<(f32, f32)>> {
    s.split(',')
        .map(|r| {
            let (lo, hi) = r.trim().split_once("..")?;
            let (lo, hi): (f32, f32) = (lo.trim().parse().ok()?, hi.trim().parse().ok()?);
            (lo.is_finite() && hi.is_finite() && lo <= hi).then_some((lo, hi))
        })
        .collect()
}

/// The program around the function: `check` takes a box's ends, each component in turn, and
/// runs `std::derive::precision` over it.
fn glue(package: &str, function: &str, dims: usize) -> String {
    let path = if function.starts_with("std::") {
        function.to_string()
    } else {
        format!("{package}::{function}")
    };
    let name = path.rsplit("::").next().unwrap_or(&path);
    let (bx, vec) = match dims {
        1 => ("Interval", ""),
        2 => ("Box2", "vec2"),
        3 => ("Box3", "vec3"),
        _ => ("Box4", "vec4"),
    };
    let ends = |end: &str| {
        let parts: Vec<String> = (0..dims).map(|i| format!("{end}{i}")).collect();
        if dims == 1 { parts[0].clone() } else { format!("{vec}({})", parts.join(", ")) }
    };
    let params: Vec<String> =
        ["lo", "hi"].iter().flat_map(|e| (0..dims).map(move |i| format!("{e}{i}: f32"))).collect();
    format!(
        "// Written by `wrela precision`.\n\n\
         use std::derive::{{{bx}, precision}}\n\
         use {path}\n\n\
         pub fn frame(time: f32, width: u32, height: u32) {{}}\n\n\
         pub fn check({}) {{\n    precision({name}, {bx} {{ lo: {}, hi: {} }})\n}}\n",
        params.join(", "),
        ends("lo"),
        ends("hi"),
    )
}

/// One operation's notes in one box: each walk's range, its hull over every time it ran.
#[derive(Clone, Copy, Debug)]
struct Seen {
    ranges: [Option<(f64, f64)>; 2],
}

/// An operation: where it is (file, byte), its name, and whether its value is an integer.
type Key = (u32, u32, String, bool);

/// The notes `precision_note` printed in one run: each operation's ranges, and the order the
/// half walk first reached each.
fn notes(lines: &[String], order: &mut Vec<Key>) -> Result<HashMap<Key, Seen>, String> {
    let mut seen: HashMap<Key, Seen> = HashMap::new();
    for line in lines {
        let Some(rest) = line.strip_prefix("precision ") else { continue };
        let w: Vec<u32> = rest
            .split(' ')
            .map(|x| x.parse().map_err(|_| format!("a note that doesn't read: {line}")))
            .collect::<Result<_, _>>()?;
        let [half, file, at, int, a, b, c, lo, hi] =
            <[u32; 9]>::try_from(w).map_err(|_| format!("a note of the wrong length: {line}"))?;
        let name: Vec<u8> =
            [a, b, c].iter().flat_map(|x| x.to_le_bytes()).take_while(|&b| b != 0).collect();
        let key = (file, at, String::from_utf8_lossy(&name).into_owned(), int != 0);
        let (lo, hi) = (f32::from_bits(lo) as f64, f32::from_bits(hi) as f64);
        if half == 1 && !order.contains(&key) {
            order.push(key.clone());
        }
        let r =
            &mut seen.entry(key).or_insert(Seen { ranges: [None; 2] }).ranges[half as usize & 1];
        *r = Some(match *r {
            Some((l, h)) => (l.min(lo), h.max(hi)),
            None => (lo, hi),
        });
    }
    Ok(seen)
}

/// What the report says of one operation, its worst box's.
#[derive(Clone, Debug, Default)]
struct Op {
    f32_width: f64,
    f16_width: f64,
    /// The half walk's extra width, and the value's magnitude, in the box where their ratio is
    /// worst.
    error: f64,
    magnitude: f64,
    /// `error / max(magnitude, 1)`, the worst box's; infinite past `f16`'s range.
    score: f64,
    range: bool,
}

impl Op {
    fn add(&mut self, s: &Seen, int: bool) {
        let (Some((l32, h32)), Some((l16, h16))) = (s.ranges[0], s.ranges[1]) else { return };
        self.f32_width = self.f32_width.max(h32 - l32);
        self.f16_width = self.f16_width.max(h16 - l16);
        let limit = if int { F16_EXACT } else { F16_MAX };
        let past = |x: f64| !x.is_finite() || x.abs() > limit;
        if past(l16) || past(h16) || past(l32) || past(h32) {
            self.range = true;
            self.score = f64::INFINITY;
            return;
        }
        let error = (l32 - l16).max(h16 - h32).max(0.0);
        let magnitude = l32.abs().max(h32.abs());
        let score = error / magnitude.max(1.0);
        if score >= self.score {
            (self.error, self.magnitude, self.score) = (error, magnitude, score);
        }
    }

    fn verdict(&self, tol: f64) -> &'static str {
        if self.range || self.score > 10.0 * tol {
            "unsafe"
        } else if self.score > tol {
            "watch"
        } else {
            "safe"
        }
    }
}

fn precision(a: &Args) -> Result<serde_json::Value, String> {
    let dims = a.boxes[0].len();
    let (glue_dir, _) =
        crate::glue_package(&a.dir, "precision", |package| glue(package, &a.function, dims))?;
    let out = wrela_driver::build(&glue_dir);
    let built = glue_dir.join("out");
    crate::write_build(&out, &built)?;
    let mut host = CpuBuild::load(&built)
        .and_then(|b| b.quiet().start())
        .map_err(|e| format!("can't run the program: {e}"))?;
    let mut ops: HashMap<Key, Op> = HashMap::new();
    let mut order = Vec::new();
    for b in &a.boxes {
        let args: Vec<Value> =
            b.iter().map(|r| Value::F32(r.0)).chain(b.iter().map(|r| Value::F32(r.1))).collect();
        host.call_export("check", &args).map_err(|e| format!("the interval failed: {e}"))?;
        for (key, s) in notes(&host.take_logs(), &mut order)? {
            let int = key.3;
            ops.entry(key).or_default().add(&s, int);
        }
    }
    let at = |k: &Key| {
        let file = out.sources.file(wrela_diag::FileId(k.0));
        file.location(k.1)
    };
    let row = |k: &Key, op: &Op| {
        let ratio = if op.f32_width > 0.0 { op.f16_width / op.f32_width } else { 1.0 };
        serde_json::json!({
            "at": at(k),
            "op": k.2,
            "integer": k.3,
            "f32_width": op.f32_width,
            "f16_width": op.f16_width,
            "ratio": ratio,
            "error": op.error,
            "magnitude": op.magnitude,
            "relative": if op.range { serde_json::Value::Null } else { op.score.into() },
            "verdict": op.verdict(a.tol),
            "why": if op.range { "range" } else if op.verdict(a.tol) == "unsafe" { "precision" } else { "" },
        })
    };
    let mut keys: Vec<&Key> = ops.keys().collect();
    keys.sort_by_key(|k| (k.0, k.1, k.2.clone()));
    let count = |v: &str| ops.values().filter(|o| o.verdict(a.tol) == v).count();
    let first = order.iter().find(|k| ops.get(*k).is_some_and(|o| o.verdict(a.tol) == "unsafe"));
    Ok(serde_json::json!({
        "function": a.function,
        "boxes": a.boxes.len(),
        "tol": a.tol,
        "ops": keys.iter().map(|k| row(k, &ops[*k])).collect::<Vec<_>>(),
        "summary": {
            "ops": ops.len(),
            "safe": count("safe"),
            "watch": count("watch"),
            "unsafe": count("unsafe"),
            "safe_share": if ops.is_empty() { 0.0 } else { count("safe") as f64 / ops.len() as f64 },
            "first_unsafe": first.map(|k| row(k, &ops[k])),
        },
    }))
}

/// The report as text: a line for the summary, the first unsafe operation, then a row per
/// operation: its widths in each walk and their ratio, the half walk's extra width, and that
/// beside the value's magnitude (`rel`, which the verdict reads).
fn text(r: &serde_json::Value) -> String {
    let s = &r["summary"];
    let mut out = format!(
        "{} over {}, tol {}: {} operations, {} safe ({:.0}%), {} watch, {} unsafe\n",
        r["function"].as_str().unwrap_or(""),
        crate::count(r["boxes"].as_u64().unwrap_or(0) as usize, "box", "boxes"),
        r["tol"],
        s["ops"],
        s["safe"],
        100.0 * s["safe_share"].as_f64().unwrap_or(0.0),
        s["watch"],
        s["unsafe"],
    );
    if let Some(f) = s["first_unsafe"].as_object() {
        out += &format!(
            "first unsafe: {} {} ({})\n",
            f["at"].as_str().unwrap_or(""),
            f["op"].as_str().unwrap_or(""),
            f["why"].as_str().unwrap_or(""),
        );
    }
    for o in r["ops"].as_array().into_iter().flatten() {
        let num = |k: &str| o[k].as_f64().unwrap_or(0.0);
        let why = o["why"].as_str().unwrap_or("");
        out += &format!(
            "  {:<32} {:<12} f32 {:<9.3e} f16 {:<9.3e} x{:<9.3e} extra {:<9.3e} rel {:<9} {}{}\n",
            o["at"].as_str().unwrap_or(""),
            o["op"].as_str().unwrap_or(""),
            num("f32_width"),
            num("f16_width"),
            num("ratio"),
            num("error"),
            o["relative"].as_f64().map_or("-".to_string(), |r| format!("{r:.2e}")),
            o["verdict"].as_str().unwrap_or(""),
            if why.is_empty() { String::new() } else { format!(" ({why})") },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPES: &str = "use std::field::lattice

pub fn sumsq(p: vec2) -> f32 {
    p.x * p.x + p.y * p.y
}

pub fn shell(p: vec3) -> f32 {
    length(p) - 1000.0
}

/// Two of value noise's corners, mixed by a weight from the input.
pub fn corners(t: f32) -> f32 {
    mix(lattice(1, 2, 3), lattice(4, 5, 6), t)
}
";

    /// A package of `SHAPES`, in a directory of its own for `test`.
    fn package(test: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wrela-precision-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("wrela.toml"), "[package]\nname = \"prec\"\n").expect("toml");
        std::fs::write(
            dir.join("main.wrela"),
            "pub fn frame(time: f32, width: u32, height: u32) {}\n",
        )
        .expect("main");
        std::fs::write(dir.join("shapes.wrela"), SHAPES).expect("shapes");
        dir
    }

    fn report(test: &str, function: &str, boxes: &[&str]) -> serde_json::Value {
        report_at(test, function, boxes, 1e-3)
    }

    fn report_at(test: &str, function: &str, boxes: &[&str], tol: f64) -> serde_json::Value {
        let dir = package(test);
        let a = Args {
            dir: dir.clone(),
            function: function.into(),
            boxes: boxes.iter().map(|b| parse_box(b).expect("a box")).collect(),
            tol,
            json: true,
        };
        let r = precision(&a).expect("a report");
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    /// Each operation's name and verdict, in source order.
    fn verdicts(r: &serde_json::Value) -> Vec<(String, String)> {
        let ops = r["ops"].as_array().expect("ops");
        ops.iter()
            .map(|o| (o["op"].as_str().unwrap().into(), o["verdict"].as_str().unwrap().into()))
            .collect()
    }

    #[test]
    fn a_sum_of_squares_is_safe_in_half_floats() {
        let r = report("sumsq", "shapes::sumsq", &["-1..1,-1..1"]);
        let v = verdicts(&r);
        assert_eq!(v.len(), 3, "two multiplies and an add: {v:?}");
        assert!(v.iter().all(|(_, s)| s == "safe"), "{v:?}");
        assert_eq!(r["summary"]["safe_share"], 1.0);
        assert!(r["summary"]["first_unsafe"].is_null());
    }

    #[test]
    fn a_shell_far_out_cancels_in_half_floats() {
        let r = report("shell", "shapes::shell", &["999..1001,-1..1,-1..1"]);
        let first = &r["summary"]["first_unsafe"];
        assert_eq!(first["op"], "sub", "the cancellation: {r:#}");
        assert_eq!(first["why"], "precision");
        assert!(first["at"].as_str().unwrap().ends_with("shapes.wrela:8:5"), "{first}");
    }

    #[test]
    fn value_noise_hashes_past_half_floats_range_and_a_mix_of_corners_is_safe() {
        let r = report("noise", "std::field::value_noise", &["0.1..0.9,0.1..0.9,0.1..0.9"]);
        let ops = r["ops"].as_array().expect("ops");
        let hash = ops.iter().find(|o| o["op"] == "wrappingmul").expect("the hash's multiply");
        assert_eq!(
            (&hash["verdict"], &hash["why"], &hash["integer"]),
            (&"unsafe".into(), &"range".into(), &true.into())
        );
        // `hash_u32` in std::field, and `pcg`, which it ends with, in std::hash.
        assert!(hash["at"].as_str().unwrap().starts_with("<std::"), "{hash}");
        // A mix rounds three times, each against |a| + |b| (up to 2 here): 4 ulps of that,
        // 3.9e-3 at a half float's, is `watch` at 1e-3 and `safe` at 5e-3.
        let r = report("corners", "shapes::corners", &["0..1"]);
        assert_eq!(verdicts(&r), [("mix".to_string(), "watch".to_string())]);
        let r = report_at("corners-5e-3", "shapes::corners", &["0..1"], 5e-3);
        assert_eq!(verdicts(&r), [("mix".to_string(), "safe".to_string())]);
    }

    #[test]
    fn the_report_names_each_operation_with_its_widths_and_verdict() {
        let r = report("json", "shapes::sumsq", &["-1..1,-1..1", "0..2,0..2"]);
        assert_eq!(
            (&r["function"], &r["boxes"], &r["tol"]),
            (&"shapes::sumsq".into(), &2.into(), &1e-3.into())
        );
        let op = &r["ops"][0];
        let keys: Vec<&str> = op.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "at",
                "error",
                "f16_width",
                "f32_width",
                "integer",
                "magnitude",
                "op",
                "ratio",
                "relative",
                "verdict",
                "why"
            ]
        );
        assert!(op["f16_width"].as_f64() > op["f32_width"].as_f64(), "{op}");
        let s = r["summary"].as_object().unwrap();
        let keys: Vec<&str> = s.keys().map(String::as_str).collect();
        assert_eq!(keys, ["first_unsafe", "ops", "safe", "safe_share", "unsafe", "watch"]);
        // The second box reaches 8: each width is the widest box's.
        let widest = r["ops"].as_array().unwrap().iter().filter_map(|o| o["f32_width"].as_f64());
        assert!(widest.fold(0.0, f64::max) >= 8.0, "{r:#}");
    }
}
