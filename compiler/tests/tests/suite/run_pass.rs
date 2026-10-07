//! The run-pass suite (compiler/tests/run): programs that must compile, and whose exports must
//! compute what their headers say when run under wasmtime. The conformance suite checks what's
//! accepted; this checks that what's accepted does what it means.
//!
//! ```text
//! // run: integer `**` with an `i32` base
//! // expect: sq(-3) = 9
//! // expect: overflow() traps
//! // expect: deep(300) panics "recursion deeper than 256 calls"
//! ```
//!
//! A case is a `.wrela` file, built as a one-file package, or a directory, built as a package
//! (its `main.wrela` holds the expectations). Arguments and results are numbers, read by the
//! export's WASM types (`u32` and `bool` are `i32`s); several results are written `(a, b, ...)`.
//! A program without a `frame` export gets an empty one. Each program is also built without
//! SIMD, which must give the same results (language.md §11).

use crate::{scratch, with_frame};
use std::path::Path;
use wrela_host::{CpuHost, Error, Value};
use wrela_tests::{build, cases, copy_dir, header, par_map, repo_root};

#[derive(Debug)]
enum Want {
    Returns(String),
    Traps,
    /// A trap after a panic with this message.
    Panics(String),
}

struct Expect {
    /// The expectation as written, for messages.
    text: String,
    call: String,
    args: Vec<String>,
    want: Want,
}

fn parse_expect(text: &str) -> Expect {
    let panics = text.split_once(" panics \"").and_then(|(c, m)| Some((c, m.strip_suffix('"')?)));
    let (call, want) = match (text.strip_suffix(" traps"), panics) {
        (Some(c), _) => (c, Want::Traps),
        (None, Some((c, m))) => (c, Want::Panics(m.to_string())),
        (None, None) => {
            let (c, v) = text
                .rsplit_once(" = ")
                .unwrap_or_else(|| panic!("`{text}` isn't `f(..) = v` or `f(..) traps`"));
            (c, Want::Returns(v.trim().to_string()))
        }
    };
    let (name, rest) = call.split_once('(').unwrap_or_else(|| panic!("`{text}`: no `(`"));
    let inner = rest.trim().strip_suffix(')').unwrap_or_else(|| panic!("`{text}`: no `)`"));
    let args =
        inner.split(',').map(str::trim).filter(|a| !a.is_empty()).map(String::from).collect();
    Expect { text: text.to_string(), call: name.trim().to_string(), args, want }
}

fn value(text: &str, ty: &str) -> Value {
    let int = |t: &str| -> i128 {
        match t {
            "true" => 1,
            "false" => 0,
            _ => t.parse().unwrap_or_else(|_| panic!("`{t}` isn't an integer")),
        }
    };
    match ty {
        "i32" => Value::I32(int(text) as i32),
        "i64" => Value::I64(int(text) as i64),
        "f32" => Value::F32(text.parse().unwrap_or_else(|_| panic!("`{text}` isn't an f32"))),
        "f64" => Value::F64(text.parse().unwrap_or_else(|_| panic!("`{text}` isn't an f64"))),
        other => panic!("can't pass a `{other}`"),
    }
}

/// The same value: a float's sign of zero counts (`-0.0` isn't `0.0`), and any NaN is NaN.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::F32(x), Value::F32(y)) => x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()),
        (Value::F64(x), Value::F64(y)) => x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()),
        _ => a == b,
    }
}

fn run_case(path: &Path) -> Result<usize, String> {
    let name = path.file_stem().expect("name").to_string_lossy().into_owned();
    let dir = scratch(&format!("run-pass/{name}"));
    let main = if path.is_dir() {
        copy_dir(path, &dir);
        dir.join("main.wrela")
    } else {
        path.to_path_buf()
    };
    let text = std::fs::read_to_string(&main).expect("read");
    let expects: Vec<Expect> = header(&text, "expect").map(parse_expect).collect();
    if expects.is_empty() {
        return Err("no `// expect:` lines".into());
    }
    std::fs::write(dir.join("main.wrela"), with_frame(&text)).expect("write");
    let out = dir.join("build");
    let built = build(&dir).map_err(|e| format!("doesn't build:\n{e}"))?;
    built.write_to(&out).expect("write the build");
    let mut host = CpuHost::load(&out).map_err(|e| format!("doesn't load: {e}"))?;
    // The same program without SIMD must give the same results (language.md §11; simd.rs).
    let scalar_out = dir.join("build-scalar");
    let scalar = wrela_driver::build_without_simd(&dir);
    if scalar.has_errors() {
        return Err("doesn't build without SIMD".into());
    }
    scalar.write_to(&scalar_out).expect("write the build");
    let mut scalar = CpuHost::load(&scalar_out).map_err(|e| format!("doesn't load: {e}"))?;
    let exports = host.exports();
    let mut problems = Vec::new();
    for e in &expects {
        let Some((_, params, results)) = exports.iter().find(|(n, ..)| *n == e.call) else {
            problems.push(format!("`{}`: no export `{}`", e.text, e.call));
            continue;
        };
        if params.len() != e.args.len() {
            problems.push(format!("`{}`: `{}` takes {} arguments", e.text, e.call, params.len()));
            continue;
        }
        let args: Vec<Value> = e.args.iter().zip(params).map(|(a, t)| value(a, t)).collect();
        let got = host.call_export(&e.call, &args);
        let without = scalar.call_export(&e.call, &args);
        let agree = match (&got, &without) {
            (Ok(x), Ok(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b)),
            (Err(Error::Trap(x)), Err(Error::Trap(y))) => x == y,
            _ => false,
        };
        if !agree {
            problems.push(format!("`{}`: {got:?} with SIMD, {without:?} without", e.text));
        }
        match (&e.want, got) {
            (Want::Traps, Err(Error::Trap(_))) => {}
            (Want::Panics(m), Err(Error::Trap(t))) if t.starts_with(&format!("panic: {m}: ")) => {}
            (Want::Returns(v), Ok(_)) if results.is_empty() && v != "()" => {
                problems.push(format!("`{}`: `{}` returns nothing: expect `()`", e.text, e.call));
            }
            (Want::Returns(v), Ok(vals)) => {
                // Several results: `(a, b, ...)`, each read by its result's type.
                let want: Vec<Value> = match v.strip_prefix('(').and_then(|v| v.strip_suffix(')')) {
                    Some(vs) if results.len() > 1 => vs
                        .split(',')
                        .map(str::trim)
                        .zip(results)
                        .map(|(v, t)| value(v, t))
                        .collect(),
                    _ => results.first().map(|t| value(v, t)).into_iter().collect(),
                };
                if want.len() != vals.len() || !want.iter().zip(&vals).all(|(a, b)| same(a, b)) {
                    problems.push(format!("`{}`: returned {vals:?}, expected {want:?}", e.text));
                }
            }
            (want, got) => problems.push(format!("`{}`: gave {got:?}, expected {want:?}", e.text)),
        }
    }
    if problems.is_empty() { Ok(expects.len()) } else { Err(problems.join("\n")) }
}

#[test]
fn run_pass() {
    let cases = cases(&repo_root().join("compiler/tests/run"));
    let only = std::env::var("WRELA_RUN_ONLY").ok();
    let name =
        |path: &std::path::PathBuf| path.file_stem().expect("name").to_string_lossy().into_owned();
    let chosen: Vec<_> = cases
        .iter()
        .filter(|p| only.as_ref().is_none_or(|o| name(p).contains(o.as_str())))
        .collect();
    let mut failures = Vec::new();
    let mut checked = 0;
    for (path, result) in chosen.iter().zip(par_map(&chosen, |p| run_case(p))) {
        match result {
            Ok(n) => checked += n,
            Err(e) => failures.push(format!("{}:\n{e}", name(path))),
        }
    }
    println!("{} programs, {checked} expectations", cases.len());
    assert!(failures.is_empty(), "{} cases failed:\n\n{}", failures.len(), failures.join("\n\n"));
    assert!(checked > 0, "no expectation was checked (WRELA_RUN_ONLY={only:?} names no case?)");
}
