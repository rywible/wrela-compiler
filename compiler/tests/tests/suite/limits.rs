//! The compiler's limits: programs at the parser's nesting and depth limits build and run;
//! one level past them is an error (E0112), never a crash. Polymorphic recursion that would
//! make instances without end is an error too, and so is a type that grows past
//! `MAX_TYPE_SIZE`. Types that share parts are worked on once per part, so a short program
//! can't make the compiler take exponential time; a value too large for memory traps when the
//! function holding it is entered, as a stack overflow does. A large constant is stored once,
//! however many functions read it, and a function may have more `let`s than an engine has
//! locals.

use crate::package;
use std::path::{Path, PathBuf};
use wrela_host::{CpuHost, Value};
use wrela_syntax::{MAX_EXPR_DEPTH, MAX_NESTING};

fn compile(name: &str, body: &str) -> (Vec<String>, PathBuf) {
    let dir = package(&format!("limits/{name}"), body);
    let out = wrela_driver::build(&dir);
    let codes =
        out.diagnostics.iter().filter(|d| d.is_error()).map(|d| d.code.to_string()).collect();
    let build = dir.join("build");
    if !out.has_errors() {
        out.write_to(&build).expect("write the build");
    }
    (codes, build)
}

fn returns(build: &Path, name: &str, want: i32) {
    let mut host = CpuHost::load(build).expect("load");
    assert_eq!(host.call_export(name, &[]).expect("call"), [Value::I32(want)]);
}

/// `pub fn f() -> i32 { ((...(1)...)) }`, nested `n` deep in all.
fn parens(n: u32) -> String {
    // The body's block is one level and its statement another; each parenthesis is one more.
    let k = n as usize - 2;
    format!("pub fn f() -> i32 {{ {}1{} }}", "(".repeat(k), ")".repeat(k))
}

#[test]
fn nesting_at_the_limit_builds_and_past_it_is_an_error() {
    let (codes, build) = compile("nest-ok", &parens(MAX_NESTING));
    assert!(codes.is_empty(), "{codes:?}");
    returns(&build, "f", 1);
    let (codes, _) = compile("nest-over", &parens(MAX_NESTING + 1));
    assert_eq!(codes, ["E0112"]);
    let (codes, _) = compile("nest-far", &parens(5000));
    assert_eq!(codes, ["E0112"]);
    let unary = format!("pub fn f() -> i32 {{ {}1 }}", "-".repeat(4000));
    let (codes, _) = compile("unary-far", &unary);
    assert_eq!(codes, ["E0112"]);
    // Each `**` nests its right side, each `else if` its `if`, each `use` group its trees.
    let pow = format!("pub fn f() -> i32 {{ 1{} }}", " ** 1".repeat(5000));
    let (codes, _) = compile("pow-far", &pow);
    assert_eq!(codes, ["E0112"]);
    let elif = format!("pub fn f(x: i32) -> i32 {{ {}1 }}", "if x == 0 { 0 } else ".repeat(5000));
    let (codes, _) = compile("elif-far", &elif);
    assert!(!codes.is_empty() && codes.iter().all(|c| c == "E0112"), "{codes:?}");
    let group = format!("use std::{{{}math{}", "std::{".repeat(5000), "}".repeat(5001));
    let (codes, _) = compile("use-far", &group);
    assert_eq!(codes, ["E0112"]);
}

#[test]
fn depth_at_the_limit_builds_and_past_it_is_an_error() {
    // A chain of `n` additions is `n + 1` deep: its first operand is under all of them.
    let chain = |n: u32| format!("pub fn f() -> i32 {{ 0{} }}", " + 1".repeat(n as usize));
    let (codes, build) = compile("depth-ok", &chain(MAX_EXPR_DEPTH - 1));
    assert!(codes.is_empty(), "{codes:?}");
    returns(&build, "f", (MAX_EXPR_DEPTH - 1) as i32);
    let (codes, _) = compile("depth-over", &chain(MAX_EXPR_DEPTH));
    assert_eq!(codes, ["E0112"]);
    let (codes, _) = compile("depth-far", &chain(20_000));
    assert_eq!(codes, ["E0112"]);
    // Chains of postfixes are as deep as they are long, and a chain that ends in an error is
    // as deep as the rest: none is built past the limit.
    let fields = format!("pub fn f(x: i32) -> i32 {{ x{} }}", ".a".repeat(20_000));
    let (codes, _) = compile("fields-far", &fields);
    assert_eq!(codes, ["E0112"]);
    let broken = format!("pub fn f() -> i32 {{ 0{} + }}", " + 1".repeat(20_000));
    let (codes, _) = compile("broken-far", &broken);
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
fn a_long_chain_of_plain_calls_builds() {
    // Only instances with type arguments or callables count toward the 256 levels: 300 plain
    // functions, each calling the next, are no recursion.
    let mut src = String::new();
    for i in 0..300 {
        src.push_str(&format!("fn step{i}() -> i32 {{\n    step{}() + 1\n}}\n", i + 1));
    }
    src.push_str("fn step300() -> i32 {\n    0\n}\npub fn chain() -> i32 {\n    step0()\n}");
    let (codes, build) = compile("plain_chain", &src);
    assert!(codes.is_empty(), "{codes:?}");
    returns(&build, "chain", 300);
}

#[test]
fn more_lets_than_an_engine_has_locals_load() {
    // Engines take 50,000 locals in a function; each `let` lives only until its last use, and
    // one in a loop until the loop ends, so their locals are used again.
    // (Floats: wasmtime takes time quadratic in a function's branches, and each checked
    // integer `+` is one.)
    let mut src = String::from("pub fn lets() -> i32 {\n    var s = 0.0\n");
    for i in 0..50_100 {
        src.push_str(&format!("    let a{i} = s + 1.0\n    s = a{i}\n"));
    }
    src.push_str("    for i in 0..3 {\n        var k = 0\n        k = k + 1\n        s = s + f32(k)\n    }\n");
    src.push_str("    i32(s)\n}\n");
    let (codes, build) = compile("many_lets", &src);
    assert!(codes.is_empty(), "{codes:?}");
    returns(&build, "lets", 50_103);
}

#[test]
fn a_table_is_stored_once() {
    // 100 functions read one 1024-entry table: one copy of its 4 KiB, not one per reader.
    let values: Vec<String> = (0..1024).map(|i| format!("{i}.5")).collect();
    let mut src = format!("const TABLE: [f32; 1024] = [{}]\n", values.join(", "));
    for k in 0..100 {
        src.push_str(&format!("pub fn read{k}(i: u32) -> f32 {{\n    TABLE[i % 1024]\n}}\n"));
    }
    let (codes, build) = compile("table", &src);
    assert!(codes.is_empty(), "{codes:?}");
    let size = std::fs::metadata(build.join("game.wasm")).expect("the wasm").len();
    assert!(size < 64 * 1024, "the module is {size} bytes");
    let mut host = CpuHost::load(&build).expect("load");
    let got = host.call_export("read99", &[Value::I32(1027)]).expect("call");
    assert_eq!(got, [Value::F32(3.5)]);
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
fn moves_on_many_paths_check_quickly() {
    // 1,000 values, each moved on its own branch: every join meets a long list of values that
    // may have moved (once quadratic in it, and the blocks visited again and again).
    let n = 1000;
    let mut src = String::from(
        "struct Log {\n    count: i32,\n}\n\nfn eat(l: Log) -> i32 {\n    l.count\n}\n\npub fn moves() -> i32 {\n    let c = 500\n    var t = 0\n",
    );
    for i in 0..n {
        src.push_str(&format!("    let a{i} = Log {{ count: {i} }}\n"));
    }
    for i in 0..n {
        src.push_str(&format!("    if c == {i} {{\n        t += eat(take a{i})\n    }}\n"));
    }
    src.push_str("    t\n}");
    let started = std::time::Instant::now();
    let (codes, build) = compile("many_moves", &src);
    assert!(codes.is_empty(), "{codes:?}");
    assert!(started.elapsed().as_secs() < 10, "took {:?}", started.elapsed());
    returns(&build, "moves", 500);
}

#[test]
fn many_temporaries_given_places_lower_quickly() {
    // Each call borrows a temporary vector, which so gets a local, stored where the vector is
    // made: that once searched the block from its start each time.
    let n = 60_000;
    let mut src = String::from(
        "fn g(v: vec3) -> f32 {\n    v.x + v.y\n}\n\npub fn f() -> i32 {\n    var s = 0.0\n",
    );
    for i in 0..n {
        src.push_str(&format!("    s += g(vec3(1.0, {}.0, 0.0))\n", i % 2));
    }
    src.push_str("    i32(s)\n}");
    let started = std::time::Instant::now();
    let (codes, _) = compile("many_spills", &src);
    assert!(codes.is_empty(), "{codes:?}");
    assert!(started.elapsed().as_secs() < 10, "took {:?}", started.elapsed());
}

#[test]
fn matches_hard_to_check_are_checked_quickly() {
    // `n` bools, an arm with each one `true` and one with it `false`: the first two cover
    // everything, and the checks of the rest once took time exponential in `n`.
    let n = 40;
    let mut arms = String::new();
    for i in 0..n {
        for v in ["true", "false"] {
            let mut pat = vec!["_"; n];
            pat[i] = v;
            arms.push_str(&format!("        ({}) => 1,\n", pat.join(", ")));
        }
    }
    let params: Vec<String> = (0..n).map(|i| format!("b{i}: bool")).collect();
    let args: Vec<String> = (0..n).map(|i| format!("b{i}")).collect();
    let wide = format!(
        "pub fn f({}) -> u32 {{\n    match ({}) {{\n{arms}    }}\n}}",
        params.join(", "),
        args.join(", ")
    );
    let started = std::time::Instant::now();
    let (codes, _) = compile("wide_match", &wide);
    assert!(codes.is_empty(), "{codes:?}");
    // Deciding coverage is NP-complete: no two of 8 pigeons share one of 7 holes, and none is
    // in no hole, can't be checked in bounded steps. It's an error until a `_` arm ends it.
    let (pigeons, holes) = (8, 7);
    let at = |p: usize, h: usize| p * holes + h;
    let mut arms = String::new();
    for p in 0..pigeons {
        let mut pat = vec!["_"; pigeons * holes];
        for h in 0..holes {
            pat[at(p, h)] = "false";
        }
        arms.push_str(&format!("        ({}) => 1,\n", pat.join(", ")));
    }
    for h in 0..holes {
        for a in 0..pigeons {
            for b in a + 1..pigeons {
                let mut pat = vec!["_"; pigeons * holes];
                pat[at(a, h)] = "true";
                pat[at(b, h)] = "true";
                arms.push_str(&format!("        ({}) => 2,\n", pat.join(", ")));
            }
        }
    }
    let params: Vec<String> = (0..pigeons * holes).map(|i| format!("b{i}: bool")).collect();
    let args: Vec<String> = (0..pigeons * holes).map(|i| format!("b{i}")).collect();
    let hard = |last: &str| {
        format!(
            "pub fn f({}) -> u32 {{\n    match ({}) {{\n{arms}{last}    }}\n}}",
            params.join(", "),
            args.join(", ")
        )
    };
    let (codes, _) = compile("hard_match", &hard(""));
    assert_eq!(codes, ["E0309"]);
    let (codes, _) = compile("hard_match_wild", &hard("        _ => 0,\n"));
    assert!(codes.is_empty(), "{codes:?}");
    assert!(started.elapsed().as_secs() < 10, "took {:?}", started.elapsed());
}

#[test]
fn a_type_that_doubles_through_later_bindings_is_an_error_quickly() {
    // The same doubling, with each variable's type bound after the value that holds it was
    // checked (assigned in reverse): the bound is checked once the body's types are known.
    let n = 40;
    let mut src = String::from("pub fn f() -> i32 {\n");
    for i in 0..=n {
        src.push_str(&format!("    var a{i} = None\n"));
    }
    for i in (1..=n).rev() {
        src.push_str(&format!("    a{i} = Some((a{}, a{}))\n", i - 1, i - 1));
    }
    src.push_str("    a0 = Some(1)\n    1\n}");
    let started = std::time::Instant::now();
    let (codes, _) = compile("doubling_later", &src);
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
