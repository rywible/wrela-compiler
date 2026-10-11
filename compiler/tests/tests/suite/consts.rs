//! Build-time constants between builds (language.md §10, M6): cached by their inputs, with the
//! fuel a declaration sets, and their parallel jobs on the build's threads.

use crate::scratch;
use std::path::Path;

/// A package whose constants each call functions of their own, and one that reads another.
const PROGRAM: &str = r#"
const SQUARES: Vec<u32> = squares(64)
const TOTAL: u32 = total(SQUARES)
const HALF: f32 = half(9.0)

fn squares(n: u32) -> Vec<u32> {
    var out: Vec<u32> = Vec::new()
    for i in 0..n {
        out.push(i * i)
    }
    out
}

fn total(xs: [u32]) -> u32 {
    var s: u32 = 0
    for x in xs {
        s += x
    }
    s
}

fn half(x: f32) -> f32 {
    x / 2.0
}

fn unrelated(x: u32) -> u32 {
    x + 1
}

pub fn frame(time: f32, width: u32, height: u32) {
    let _ = TOTAL + unrelated(1)
    let _ = HALF
}
"#;

fn stats(dir: &Path) -> (u32, u32) {
    let out = wrela_driver::build(dir);
    assert!(
        !wrela_diag::has_errors(&out.diagnostics),
        "{}",
        wrela_diag::render::render_all(&out.sources, &out.diagnostics)
    );
    (out.consts.computed, out.consts.cached)
}

/// The package's constants: computed by the first build, read from the cache by the second; an
/// edit to a function one of them calls computes it again (and what reads it, if its value
/// changed), and an edit elsewhere none.
#[test]
fn the_second_build_computes_no_constant() {
    let dir = scratch("consts/cached");
    let main = dir.join("main.wrela");
    std::fs::write(&main, PROGRAM).expect("write");
    let (computed, cached) = stats(&dir);
    assert!(computed >= 3 && cached == 0, "the first build computes them: {computed}, {cached}");
    let all = computed;
    assert_eq!(stats(&dir), (0, all), "the second build computes none");

    // A function `HALF` calls: it alone is computed again.
    std::fs::write(&main, PROGRAM.replace("x / 2.0", "x * 0.5")).expect("write");
    assert_eq!(stats(&dir), (1, all - 1), "an edit to `half` computes `HALF` again");

    // A function `SQUARES` calls, which changes its value: `TOTAL` reads it, so both.
    std::fs::write(&main, PROGRAM.replace("i * i", "i * i + 1")).expect("write");
    assert_eq!(stats(&dir), (2, all - 2), "an edit to `squares` computes it and `TOTAL`");

    // A function no constant calls, and the lines above every constant's code moved: none.
    let elsewhere = PROGRAM.replace("i * i", "i * i + 1").replace("x + 1", "x + 2");
    let moved = format!("// a comment that moves every line\n\n{elsewhere}");
    std::fs::write(&main, moved).expect("write");
    assert_eq!(stats(&dir), (0, all), "an edit elsewhere computes none");

    // The cache's files are the package's build's, and a corrupt one is a miss, not an error.
    let cache = dir.join("build/consts");
    for e in std::fs::read_dir(&cache).expect("the cache is in build/consts") {
        std::fs::write(e.expect("entry").path(), b"not a value").expect("write");
    }
    assert_eq!(stats(&dir), (all, 0), "corrupt entries are computed again");
}

/// A constant that does more work than the build's own limit (2^34 units) is built when its
/// declaration gives it the fuel, and fails without; its parallel job runs on the build's
/// threads, and its value doesn't depend on how many.
#[test]
#[ignore = "long: computes a constant past the build's fuel limit (seconds)"]
fn a_constant_with_fuel_of_its_own_does_more_than_the_builds_limit() {
    let program = r#"
@fuel(2 ** 37)
const SUM: u64 = sum(2048)

fn sum(rows: u32) -> u64 {
    var parts: Vec<u64> = Vec::filled(rows, 0)
    parts.par_each_mut(|p| work(mut p))
    var s: u64 = 0
    for p in parts {
        s = s.wrapping_add(p)
    }
    s
}

fn work(p: mut u64) {
    var x: u64 = 1
    for i in 0..1000000 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(u64(i))
    }
    p = x
}

pub fn frame(time: f32, width: u32, height: u32) {
    let _ = SUM
}
"#;
    let dir = scratch("consts/fuel");
    std::fs::write(dir.join("main.wrela"), program).expect("write");
    let t = std::time::Instant::now();
    let out = wrela_driver::build(&dir);
    assert!(!wrela_diag::has_errors(&out.diagnostics), "{:?}", out.diagnostics);
    eprintln!(
        "a constant of about 2^35 units computed in {:.1} s, the helpers running {} of its chunks",
        t.elapsed().as_secs_f64(),
        out.consts.helped
    );
    assert!(out.consts.computed >= 1);
    if std::thread::available_parallelism().map_or(1, |n| n.get()) > 1 {
        assert!(out.consts.helped > 0, "the build's helpers ran none of its parallel job");
    }
    std::fs::remove_dir_all(dir.join("build")).expect("clear the cache");
    std::fs::write(dir.join("main.wrela"), program.replace("@fuel(2 ** 37)\n", "")).expect("write");
    let out = wrela_driver::build(&dir);
    let codes: Vec<&str> = out.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes, ["E0705"], "without its fuel it stops at the build's limit");
}

/// Two constants that ship files into one directory of the build (`std::io::Shipped`) are E0704;
/// a file's name that isn't one fails the build that computes the constant.
#[test]
fn shipped_files_need_a_directory_each_and_plain_names() {
    let program = r#"
use std::io::{Shipped, ship}

const A: Shipped = ship("data", one("a.bin"))
const B: Shipped = ship("data", one("b.bin"))

fn one(name: str) -> Vec<(String, Vec<u8>)> {
    Vec::from([(String::from(name), Vec::from([1, 2, 3]))])
}

pub fn frame(time: f32, width: u32, height: u32) {
    let _ = A.len() + B.len()
}
"#;
    let dir = scratch("consts/shipped-twice");
    std::fs::write(dir.join("main.wrela"), program).expect("write");
    let out = wrela_driver::build(&dir);
    let codes: Vec<&str> = out.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes, ["E0704"], "two constants ship into `data`");

    let dir = scratch("consts/shipped-name");
    let bad = program
        .replace("\"b.bin\"", "\"../b.bin\"")
        .replace("ship(\"data\", one(\"a", "ship(\"other\", one(\"a");
    std::fs::write(dir.join("main.wrela"), bad).expect("write");
    let out = wrela_driver::build(&dir);
    let text = wrela_diag::render::render_all(&out.sources, &out.diagnostics);
    assert!(text.contains("isn't a file's name") || text.contains("has a character"), "{text}");
}
