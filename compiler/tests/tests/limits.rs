//! The compiler's limits: programs at the parser's nesting and depth limits build and run;
//! one level past them is an error (E0112), never a crash. Polymorphic recursion that would
//! make instances without end is an error too.

use std::path::PathBuf;
use wrela_host::{CpuHost, Value};
use wrela_syntax::{MAX_EXPR_DEPTH, MAX_NESTING};

fn compile(name: &str, body: &str) -> (Vec<String>, PathBuf) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("limits").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let text = format!("{body}\npub fn frame(time: f32, width: u32, height: u32) {{}}\n");
    std::fs::write(dir.join("main.wrela"), text).expect("write");
    let out = wrela_driver::build(&dir);
    let codes =
        out.diagnostics.iter().filter(|d| d.is_error()).map(|d| d.code.to_string()).collect();
    let build = dir.join("build");
    if !out.has_errors() {
        std::fs::create_dir_all(&build).expect("dir");
        for (p, bytes) in &out.files {
            std::fs::write(build.join(p), bytes).expect("write");
        }
    }
    (codes, build)
}

fn returns(build: &PathBuf, name: &str, want: i32) {
    let mut host = CpuHost::load(build).expect("load");
    assert_eq!(host.call_export(name, &[]).expect("call"), [Value::I32(want)]);
}

/// `pub fn f() -> i32 { ((...(1)...)) }` with the body's expression nested `n` deep.
fn parens(n: u32) -> String {
    // The body's statement is one level; each parenthesis is another.
    let k = n as usize - 1;
    format!("pub fn f() -> i32 {{ {}1{} }}", "(".repeat(k), ")".repeat(k))
}

#[test]
fn nesting_at_the_limit_builds_and_past_it_is_an_error() {
    let (codes, build) = compile("nest-ok", &parens(MAX_NESTING - 1));
    assert!(codes.is_empty(), "{codes:?}");
    returns(&build, "f", 1);
    let (codes, _) = compile("nest-over", &parens(MAX_NESTING + 1));
    assert_eq!(codes, ["E0112"]);
    let (codes, _) = compile("nest-far", &parens(5000));
    assert_eq!(codes, ["E0112"]);
    let unary = format!("pub fn f() -> i32 {{ {}1 }}", "-".repeat(4000));
    let (codes, _) = compile("unary-far", &unary);
    assert_eq!(codes, ["E0112"]);
}

#[test]
fn depth_at_the_limit_builds_and_past_it_is_an_error() {
    // A chain of `n` additions is `n` deep, under the body's one level.
    let chain = |n: u32| format!("pub fn f() -> i32 {{ 0{} }}", " + 1".repeat(n as usize));
    let (codes, build) = compile("depth-ok", &chain(MAX_EXPR_DEPTH - 2));
    assert!(codes.is_empty(), "{codes:?}");
    returns(&build, "f", (MAX_EXPR_DEPTH - 2) as i32);
    let (codes, _) = compile("depth-over", &chain(MAX_EXPR_DEPTH));
    assert_eq!(codes, ["E0112"]);
    let (codes, _) = compile("depth-far", &chain(20_000));
    assert_eq!(codes, ["E0112"]);
}

#[test]
fn polymorphic_recursion_is_an_error() {
    let src = "struct Pair<T>: Copy + Clone {
    a: T,
    b: T,
}

fn grow<T: Copy>(x: T, n: u32) -> u32 {
    if n == 0 {
        return 1
    }
    grow(Pair { a: x, b: x }, n - 1)
}

pub fn poly() -> u32 {
    grow(1, 3)
}";
    let (codes, _) = compile("poly", src);
    assert_eq!(codes, ["E0412"]);
}
