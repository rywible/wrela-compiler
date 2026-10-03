//! The compiler's limits: programs at the parser's nesting and depth limits build and run;
//! one level past them is an error (E0112), never a crash. Polymorphic recursion that would
//! make instances without end is an error too, and so is a type that grows past
//! `MAX_TYPE_SIZE`. Types that share parts are worked on once per part, so a short program
//! can't make the compiler take exponential time; a value too large for memory traps when the
//! function holding it is entered, as a stack overflow does.

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

#[test]
fn tuple_polymorphic_recursion_is_an_error_quickly() {
    // Each instance's type doubles: the size bound stops it long before the depth bound.
    let src = "fn grow<T: Copy>(x: T, n: u32) -> u32 {
    if n == 0 {
        return 1
    }
    grow((x, x), n - 1)
}

pub fn poly() -> u32 {
    grow(1, 3)
}";
    let started = std::time::Instant::now();
    let (codes, _) = compile("poly_tuple", src);
    assert_eq!(codes, ["E0412"]);
    assert!(started.elapsed().as_secs() < 10, "took {:?}", started.elapsed());
}

#[test]
fn a_type_that_doubles_is_an_error_quickly() {
    let mut src = String::from("pub fn f() -> i32 {\n    let a0 = (1, 1)\n");
    for i in 1..60 {
        src.push_str(&format!("    let a{i} = (a{}, a{})\n", i - 1, i - 1));
    }
    src.push_str("    1\n}");
    let started = std::time::Instant::now();
    let (codes, _) = compile("doubling", &src);
    assert_eq!(codes, ["E0329"]);
    assert!(started.elapsed().as_secs() < 10, "took {:?}", started.elapsed());
}

#[test]
fn values_too_large_for_memory_trap_on_entry() {
    // Each struct holds two of the one before: 2^41 floats, laid out once per struct.
    let mut src = String::from("struct P0: Copy + Clone { a: f32, b: f32 }\n");
    src.push_str("fn mk0() -> P0 {\n    P0 { a: 1.0, b: 2.0 }\n}\n");
    for i in 1..40 {
        let j = i - 1;
        src.push_str(&format!("struct P{i}: Copy + Clone {{ a: P{j}, b: P{j} }}\n"));
        src.push_str(&format!(
            "fn mk{i}() -> P{i} {{\n    let x = mk{j}()\n    P{i} {{ a: x, b: x }}\n}}\n"
        ));
    }
    src.push_str(&format!("pub fn nested() -> f32 {{\n    mk39(){}.a\n}}\n", ".a".repeat(39)));
    src.push_str("pub fn array() -> f32 {\n    let a = [[0.0; 100000]; 100000]\n    a[7][7]\n}\n");
    src.push_str("pub fn small() -> i32 {\n    7\n}");
    let started = std::time::Instant::now();
    let (codes, build) = compile("too_large", &src);
    assert!(codes.is_empty(), "{codes:?}");
    assert!(started.elapsed().as_secs() < 10, "took {:?}", started.elapsed());
    let mut host = CpuHost::load(&build).expect("load");
    for name in ["nested", "array"] {
        assert!(host.call_export(name, &[]).is_err(), "`{name}` didn't trap");
    }
    // The stack is unharmed: calls go on working.
    assert_eq!(host.call_export("small", &[]).expect("call"), [Value::I32(7)]);
}
