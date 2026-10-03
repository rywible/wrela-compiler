//! The run-pass suite (compiler/tests/run): programs that must compile, and whose exports must
//! compute what their headers say when run under wasmtime. The conformance suite checks what's
//! accepted; this checks that what's accepted does what it means.
//!
//! ```text
//! // run: integer `**` with an `i32` base
//! // expect: sq(-3) = 9
//! // expect: overflow() traps
//! ```
//!
//! A case is a `.wrela` file, built as a one-file package, or a directory, built as a package
//! (its `main.wrela` holds the expectations). Arguments and results are numbers, read by the
//! export's WASM types (`u32` and `bool` are `i32`s). A program without a `frame` export gets
//! an empty one.

use std::path::PathBuf;
use wrela_host::{CpuHost, Error, Value};
use wrela_tests::{build, root};

#[derive(Debug)]
enum Want {
    Returns(String),
    Traps,
}

struct Expect {
    line: usize,
    call: String,
    args: Vec<String>,
    want: Want,
}

fn parse_expect(line: usize, text: &str) -> Expect {
    let (call, want) = match text.strip_suffix(" traps") {
        Some(c) => (c, Want::Traps),
        None => {
            let (c, v) = text.rsplit_once(" = ").unwrap_or_else(|| {
                panic!("line {line}: `{text}` isn't `f(..) = v` or `f(..) traps`")
            });
            (c, Want::Returns(v.trim().to_string()))
        }
    };
    let (name, rest) = call.split_once('(').unwrap_or_else(|| panic!("line {line}: no `(`"));
    let inner = rest.trim().strip_suffix(')').unwrap_or_else(|| panic!("line {line}: no `)`"));
    let args =
        inner.split(',').map(str::trim).filter(|a| !a.is_empty()).map(String::from).collect();
    Expect { line, call: name.trim().to_string(), args, want }
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

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::F32(x), Value::F32(y)) => x == y || (x.is_nan() && y.is_nan()),
        (Value::F64(x), Value::F64(y)) => x == y || (x.is_nan() && y.is_nan()),
        _ => a == b,
    }
}

fn run_case(path: &std::path::Path) -> Result<usize, String> {
    let name = path.file_stem().expect("name").to_string_lossy().into_owned();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("run-pass").join(&name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let main = if path.is_dir() {
        copy_dir(path, &dir);
        dir.join("main.wrela")
    } else {
        path.to_path_buf()
    };
    let mut text = std::fs::read_to_string(&main).expect("read");
    let expects: Vec<Expect> = text
        .lines()
        .enumerate()
        .filter_map(|(i, l)| l.strip_prefix("// expect:").map(|e| parse_expect(i + 1, e.trim())))
        .collect();
    if expects.is_empty() {
        return Err("no `// expect:` lines".into());
    }
    if !text.contains("fn frame(") {
        text.push_str("\npub fn frame(time: f32, width: u32, height: u32) {}\n");
    }
    std::fs::write(dir.join("main.wrela"), &text).expect("write");
    let out = dir.join("build");
    build(&dir, &out).map_err(|e| format!("doesn't build:\n{e}"))?;
    let mut host = CpuHost::load(&out).map_err(|e| format!("doesn't load: {e}"))?;
    let exports = host.exports();
    let mut problems = Vec::new();
    for e in &expects {
        let Some((_, params, results)) = exports.iter().find(|(n, ..)| *n == e.call) else {
            problems.push(format!("line {}: no export `{}`", e.line, e.call));
            continue;
        };
        if params.len() != e.args.len() {
            problems.push(format!(
                "line {}: `{}` takes {} arguments",
                e.line,
                e.call,
                params.len()
            ));
            continue;
        }
        let args: Vec<Value> = e.args.iter().zip(params).map(|(a, t)| value(a, t)).collect();
        let got = host.call_export(&e.call, &args);
        match (&e.want, got) {
            (Want::Traps, Err(Error::Trap(_))) => {}
            (Want::Returns(v), Ok(vals)) => {
                let want: Vec<Value> = match results.first() {
                    Some(t) => vec![value(v, t)],
                    None => Vec::new(),
                };
                if want.len() != vals.len() || !want.iter().zip(&vals).all(|(a, b)| same(a, b)) {
                    problems.push(format!(
                        "line {}: {}({}) returned {vals:?}, expected {want:?}",
                        e.line,
                        e.call,
                        e.args.join(", ")
                    ));
                }
            }
            (want, got) => problems.push(format!(
                "line {}: {}({}) gave {got:?}, expected {want:?}",
                e.line,
                e.call,
                e.args.join(", ")
            )),
        }
    }
    if problems.is_empty() { Ok(expects.len()) } else { Err(problems.join("\n")) }
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    for e in std::fs::read_dir(from).expect("read case dir") {
        let p = e.expect("entry").path();
        let dest = to.join(p.file_name().expect("name"));
        if p.is_dir() {
            std::fs::create_dir_all(&dest).expect("dir");
            copy_dir(&p, &dest);
        } else {
            std::fs::copy(&p, &dest).expect("copy");
        }
    }
}

#[test]
fn run_pass() {
    let dir = root().join("compiler/tests/run");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("run dir")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.is_dir() || p.extension().is_some_and(|e| e == "wrela"))
        .collect();
    cases.sort();
    let only = std::env::var("WRELA_RUN_ONLY").ok();
    let mut failures = Vec::new();
    let mut checked = 0;
    for path in &cases {
        let name = path.file_stem().expect("name").to_string_lossy().into_owned();
        if only.as_ref().is_some_and(|o| !name.contains(o.as_str())) {
            continue;
        }
        match run_case(path) {
            Ok(n) => checked += n,
            Err(e) => failures.push(format!("{name}:\n{e}")),
        }
    }
    println!("{} programs, {checked} expectations", cases.len());
    assert!(failures.is_empty(), "{} cases failed:\n\n{}", failures.len(), failures.join("\n\n"));
}
