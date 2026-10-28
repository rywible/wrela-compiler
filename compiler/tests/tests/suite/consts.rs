//! Build-time constants between builds (language.md §13, M6): cached by their inputs, with the
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
    // A first build can read what it wrote a moment before: constants with the same code (two
    // fields' `String::new()` defaults in std, say) share one entry.
    let first = stats(&dir);
    let (computed, cached) = first;
    assert!(computed >= 3, "the first build computes them: {computed}, {cached}");
    let all = computed + cached;
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
    let cache = dir.join("build/consts").join(wrela_driver::const_cache::VERSION);
    for e in std::fs::read_dir(&cache).expect("the cache is in build/consts/<version>") {
        std::fs::write(e.expect("entry").path(), b"not a value").expect("write");
    }
    assert_eq!(stats(&dir), first, "corrupt entries are computed again");
}

/// A constant whose code reads a function in another module, with a test of its value.
const READS: &str = r#"
use sizes::size

const SIZE: u32 = size(3)

pub fn frame(time: f32, width: u32, height: u32) {
    let _ = SIZE
}

@test
fn the_size_is_what_the_sources_say() {
    assert(SIZE == WANT)
}
"#;

/// The cache holds what a value depends on (language.md §13): an edit to a function a constant
/// calls, in another module, gives the new value; the toolchain's version names the cache's
/// directory, so another version's entries aren't read; and `--report consts` says which
/// constants were read from the cache.
#[test]
fn a_constants_key_is_its_code_and_the_toolchains_version() {
    let dir = scratch("consts/keyed");
    let with = |n: u32, want: u32| {
        std::fs::write(dir.join("main.wrela"), READS.replace("WANT", &want.to_string()))
            .expect("write");
        let sizes = format!("pub fn size(x: u32) -> u32 {{\n    x * {n}\n}}\n");
        std::fs::write(dir.join("sizes.wrela"), sizes).expect("write");
    };
    let passes = |dir: &Path| {
        let out = wrela_driver::test(dir, None);
        let failed: Vec<_> = out.results.iter().filter_map(|r| r.failure.clone()).collect();
        assert!(
            out.passed(),
            "{}{}",
            wrela_diag::render::render_all(&out.sources, &out.diagnostics),
            wrela_diag::render::render_all(&out.sources, &failed)
        );
    };
    let runs = |dir: &Path| wrela_driver::build(dir).consts.runs;
    with(2, 6);
    passes(&dir);
    let first = runs(&dir);
    let size = first.iter().find(|r| r.name == "SIZE").expect("SIZE was computed");
    assert!(size.bytes > 0 && size.fuel > 0, "{size:?}");
    assert!(runs(&dir).iter().all(|r| r.cached), "a second build reads them all");
    // The function `SIZE` calls changed: its new value, not the cached one.
    with(5, 15);
    passes(&dir);
    let after = runs(&dir);
    assert!(after.iter().any(|r| r.name == "SIZE" && !r.cached), "computed again: {after:?}");

    // Another version's directory isn't read: this version's entries, moved under another
    // version's name, are computed again.
    let consts = dir.join("build/consts");
    let this = consts.join(wrela_driver::const_cache::VERSION);
    let other = consts.join("0.0.1");
    let _ = std::fs::remove_dir_all(&other);
    std::fs::rename(&this, &other).expect("move the cache");
    // (Constants with the same code share an entry within the build, so only those first of
    // their code are computed.)
    let moved = runs(&dir);
    assert!(moved.iter().any(|r| r.name == "SIZE" && !r.cached), "{moved:?}");
    assert!(moved.iter().filter(|r| !r.cached).count() > moved.len() / 2, "{moved:?}");
    assert!(other.read_dir().expect("kept").next().is_some(), "a recent version's are kept");
}

/// Waiting constants keep their round order, including a fan-in whose inputs become ready
/// in different rounds. A cached build produces exactly the same files without tracing again.
#[test]
fn waiting_constants_keep_values_order_and_cache_hits() {
    let dir = crate::package(
        "consts/waiting",
        r#"
use std::io::print

const TOTAL: u32 = step(C0 + FAST)
const C0: u32 = step(C1)
const C1: u32 = step(C2)
const C2: u32 = step(C3)
const FAST: u32 = step(10)
const C3: u32 = step(0)

fn step(x: u32) -> u32 {
    print(f"{x}")
    x + 1
}

pub fn frame(time: f32, width: u32, height: u32) {
    assert(TOTAL == 16)
}
"#,
    );
    let first = wrela_driver::build(&dir);
    assert!(!first.has_errors(), "{:?}", first.diagnostics);
    let traces: Vec<_> = first
        .consts
        .traced
        .iter()
        .map(|(name, trace)| (name.as_str(), trace.lines[0].as_str()))
        .collect();
    assert_eq!(
        traces,
        [("FAST", "10"), ("C3", "0"), ("C2", "1"), ("C1", "2"), ("C0", "3"), ("TOTAL", "15")]
    );
    let second = wrela_driver::build(&dir);
    assert!(!second.has_errors(), "{:?}", second.diagnostics);
    assert_eq!(second.consts.computed, 0);
    assert_eq!(second.consts.cached, first.consts.computed + first.consts.cached);
    assert!(second.consts.traced.is_empty());
    assert_eq!(first.files, second.files);
}

#[test]
fn waiting_constants_preserve_failures_and_cycles() {
    let prefix = "fn step(x: u32) -> u32 { x + 1 }\n";
    let failing = crate::package(
        "consts/waiting-failure",
        &format!(
            "{prefix}\nconst GRAND: u32 = step(CHILD)\nconst CHILD: u32 = step(FAILED)\n\
             const FAILED: u32 = fail()\nfn fail() -> u32 {{ panic(\"failed root\") }}\n\
             const OK0: u32 = step(OK1)\nconst OK1: u32 = step(0)"
        ),
    );
    let out = wrela_driver::build(&failing);
    let errors: Vec<_> = out.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code.as_str(), "E0704");
    assert!(errors[0].message.contains("failed root"), "{errors:?}");

    let cyclic = crate::package(
        "consts/waiting-cycle",
        &format!(
            "{prefix}\nconst A: u32 = step(b())\nconst B: u32 = step(a())\n\
             fn a() -> u32 {{ A }}\nfn b() -> u32 {{ B }}\n\
             const OK0: u32 = step(OK1)\nconst OK1: u32 = step(OK2)\n\
             const OK2: u32 = step(0)"
        ),
    );
    let out = wrela_driver::build(&cyclic);
    let errors: Vec<_> = out.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code.as_str(), "E0328");
    assert!(errors[0].message.contains("`A` → `B` → `A`"), "{errors:?}");
}
/// Fuel declarations are enforced on a tiny computation, without exhausting the default
/// build budget. A changed fuel value must invalidate the constant cache.
#[test]
fn a_constants_declared_fuel_is_enforced_and_invalidates_its_cache() {
    let dir = scratch("consts/fuel");
    let program = r#"
@fuel(1000000)
const SUM: u32 = sum()
fn sum() -> u32 {
    var s: u32 = 0
    for i in 0..1000 { s += i }
    s
}
pub fn frame(time: f32, width: u32, height: u32) { assert(SUM == 499500) }
"#;
    let main = dir.join("main.wrela");
    std::fs::write(&main, program).unwrap();
    let out = wrela_driver::build(&dir);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(out.consts.computed > 0);
    std::fs::write(&main, program.replace("@fuel(1000000)", "@fuel(10)")).unwrap();
    let out = wrela_driver::build(&dir);
    assert!(out.diagnostics.iter().any(|d| d.code.as_str() == "E0705"), "{:?}", out.diagnostics);
}

/// Two constants that ship files into one directory of the build (`std::io::Shipped`) are E0704;
/// a file's name that isn't one fails the build that computes the constant, and two files of one
/// constant can't have the same name (one would overwrite the other).
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

    let dir = scratch("consts/shipped-name-twice");
    let twice = program
        .replace("const B: Shipped = ship(\"data\", one(\"b.bin\"))", "")
        .replace("A.len() + B.len()", "A.len()")
        .replace(
            "Vec::from([(String::from(name), Vec::from([1, 2, 3]))])",
            "Vec::from([(String::from(name), Vec::from([1])), (String::from(name), Vec::from([2, 3]))])",
        );
    std::fs::write(dir.join("main.wrela"), twice).expect("write");
    let out = wrela_driver::build(&dir);
    let text = wrela_diag::render::render_all(&out.sources, &out.diagnostics);
    assert!(text.contains("ships more than one file named `a.bin`"), "{text}");
}

/// A program whose root module's constants a build may set (language.md §13).
const SETTABLE: &str = r#"
pub enum Tier {
    Low,
    High,
}

pub const N: u32 = 4
pub const TIER: Tier = Tier::Low
pub const SCALE: f32 = 1.5
const HIDDEN: u32 = 1
pub const AT: vec2 = vec2(1.0, 2.0)

pub fn value() -> u32 {
    N
}

pub fn length() -> u32 {
    let a = [0.0; N]
    a.len()
}

pub fn tier() -> u32 {
    match TIER {
        Tier::Low => 1,
        Tier::High => 2,
    }
}

pub fn frame(time: f32, width: u32, height: u32) {
    let _ = HIDDEN + u32(SCALE) + u32(AT.x)
}
"#;

/// `wrela build --set N=8`: the program reads 8, an array of length `N` has 8 elements, an
/// enum's constant takes a variant by name, and the manifest records the sets; a `pub const`
/// that isn't there, one that isn't `pub`, and one of a type a build can't set are E0224.
#[test]
fn a_build_sets_a_programs_pub_constants() {
    let dir = scratch("consts/set");
    std::fs::write(dir.join("main.wrela"), SETTABLE).expect("write");
    let set = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs.iter().map(|&(n, v)| (n.to_string(), v.to_string())).collect()
    };
    let kind = wrela_driver::BuildKind::default();
    let sets = set(&[("N", "8"), ("TIER", "High"), ("SCALE", "-2.5")]);
    let out = wrela_driver::build_set(&dir, kind, false, &sets);
    assert!(
        !out.has_errors(),
        "{}",
        wrela_diag::render::render_all(&out.sources, &out.diagnostics)
    );
    let built = dir.join("build-set");
    let _ = std::fs::remove_dir_all(&built);
    out.write_to(&built).expect("write the build");
    let mut host = crate::cpu_host(&built);
    assert_eq!(wrela_tests::one_u32(&mut host, "value", &[]), 8);
    assert_eq!(wrela_tests::one_u32(&mut host, "length", &[]), 8, "N folds where it's a length");
    assert_eq!(wrela_tests::one_u32(&mut host, "tier", &[]), 2);
    let manifest = wrela_tests::manifest(&built);
    assert_eq!(manifest.set.get("N").map(String::as_str), Some("8"));
    assert_eq!(manifest.set.get("TIER").map(String::as_str), Some("High"));
    // Without sets, the manifest says none and the program reads what it's written with.
    let plain = wrela_driver::build(&dir);
    let json = plain.files.iter().find(|(p, _)| p == "manifest.json").expect("manifest");
    assert!(!String::from_utf8_lossy(&json.1).contains("\"set\""));
    for (pairs, says) in [
        (set(&[("MISSING", "1")]), "has no constant `MISSING`"),
        (set(&[("HIDDEN", "2")]), "isn't `pub`"),
        (set(&[("AT", "3")]), "is a `vec2`"),
    ] {
        let out = wrela_driver::build_set(&dir, kind, false, &pairs);
        let e = out.diagnostics.iter().find(|d| d.code.as_str() == "E0224");
        assert!(e.is_some_and(|d| d.message.contains(says)), "{pairs:?}: {:?}", out.diagnostics);
    }
}
