//! An optimized build (`wrela build --optimize`, `wrela_driver::optimize`): Binaryen makes its
//! WASM smaller, and its trap locations go through it as a source map, into `game.lines` beside
//! it. A trap says where it happened just as the unoptimized build's does: in the native host,
//! which reads the file with the build, and in Chrome, which fetches it only when a trap needs
//! it. Without the file, a trap gives its code offset. compiler/tests/traps traps in functions
//! the optimizer inlines; compiler/tests/numerics in each way a CPU operation traps.

use crate::{built, scratch};
use std::path::{Path, PathBuf};
use wrela_host::{CpuHost, Error, Value};
use wrela_tests::{ChromeRun, chrome_failure, page};

/// The build in `dir`, its WASM optimized and its trap locations beside it.
fn optimize_in_place(dir: &Path) {
    let wasm = std::fs::read(dir.join("game.wasm")).expect("game.wasm");
    let (code, lines) =
        wrela_driver::optimize::optimize(&wasm, wrela_driver::optimize::Level::Size)
            .expect("optimizes");
    assert!(code.len() < wasm.len(), "optimized {} bytes into {}", wasm.len(), code.len());
    // As `wrela build --optimize` does: the manifest states the rewritten module's interface.
    let manifest = std::fs::read(dir.join("manifest.json")).expect("manifest.json");
    let mut files = vec![("game.wasm".to_string(), code), ("manifest.json".to_string(), manifest)];
    wrela_driver::build::restate_abi(&mut files).expect("restates the interface");
    for (name, bytes) in files {
        std::fs::write(dir.join(&name), bytes).expect(&name);
    }
    std::fs::write(dir.join("game.lines"), lines).expect("write game.lines");
}

/// A copy of the test package `pkg`'s build, optimized: its directory.
fn optimized(pkg: &str, name: &str) -> PathBuf {
    let dir = scratch(name);
    wrela_tests::copy_dir(&built(pkg), &dir);
    optimize_in_place(&dir);
    dir
}

/// Why `name(args)` trapped in the build in `dir`.
fn trap(dir: &Path, name: &str, args: &[Value]) -> String {
    match CpuHost::load(dir).expect("load").call_export(name, args) {
        Err(Error::Trap(why)) => why,
        other => panic!("{name}{args:?} should trap, got {other:?}"),
    }
}

/// Each way compiler/tests/numerics traps: overflow, division by zero, a long shift, a
/// float-to-int conversion out of range, an index out of range.
const NUMERICS_TRAPS: [(&str, [i32; 2]); 10] = [
    ("add_i32", [i32::MAX, 1]),
    ("sub_u32", [3, 5]),
    ("mul_i32", [65536, 65536]),
    ("neg_i32", [i32::MIN, 0]),
    ("div_i32", [1, 0]),
    ("rem_u32", [7, 0]),
    ("shl_u32", [1, 32]),
    ("add_u8", [200, 56]),
    ("to_i32", [f32::NAN.to_bits() as i32, 0]),
    ("index", [3, 0]),
];

#[test]
fn an_optimized_build_traps_where_it_did() {
    let (plain, small) = (built("numerics"), optimized("numerics", "optimize-numerics"));
    for (name, [a, b]) in NUMERICS_TRAPS {
        let args: Vec<Value> = match name {
            "neg_i32" | "index" => vec![Value::I32(a)],
            "to_i32" => vec![Value::F32(f32::from_bits(a as u32))],
            _ => vec![Value::I32(a), Value::I32(b)],
        };
        let want = trap(&plain, name, &args);
        assert!(want.contains(" at main.wrela:"), "{name}: {want}");
        assert_eq!(trap(&small, name, &args), want, "{name}");
    }
}

#[test]
fn an_inlined_trap_says_the_callee_s_line() {
    let (plain, small) = (built("traps"), optimized("traps", "optimize-traps"));
    let divide = [Value::I32(1), Value::I32(0)];
    for dir in [&plain, &small] {
        // `a / b` is line 12 of compiler/tests/traps, `a + 1` line 20.
        let zero = trap(dir, "divide", &divide);
        assert!(zero.ends_with(" at main.wrela:12:5"), "{zero}");
        let overflow = trap(dir, "grow_twice", &[Value::I32(i32::MAX)]);
        assert!(overflow.ends_with(" at main.wrela:20:5"), "{overflow}");
    }
}

#[test]
fn without_its_lines_a_trap_gives_its_code_offset() {
    let dir = optimized("traps", "optimize-traps-unlocated");
    std::fs::remove_file(dir.join("game.lines")).expect("remove game.lines");
    let why = trap(&dir, "divide", &[Value::I32(1), Value::I32(0)]);
    let offset = why.rsplit_once(" at code offset 0x").map(|(_, o)| o);
    assert!(offset.is_some_and(|o| u32::from_str_radix(o, 16).is_ok()), "{why}");
}

/// The traps fixture's page, optimized; without its `game.lines` unless `lines`.
fn optimized_page(name: &str, lines: bool) -> String {
    let (dir, rel) = page("compiler/tests/traps", name);
    optimize_in_place(&dir);
    if !lines {
        std::fs::remove_file(dir.join("game.lines")).expect("remove game.lines");
    }
    rel
}

#[test]
#[ignore = "needs the runtime"]
fn chrome_fetches_the_location_of_a_trap() {
    let rel = optimized_page("optimize-traps-chrome", true);
    let failure = chrome_failure(&rel, ChromeRun::new(2, 4, 4, 60.0));
    assert!(failure.contains(" at main.wrela:12:5"), "{failure}");
}

#[test]
#[ignore = "needs the runtime"]
fn chrome_gives_the_code_offset_when_the_lines_do_not_arrive() {
    let rel = optimized_page("optimize-traps-chrome-unlocated", false);
    let failure = chrome_failure(&rel, ChromeRun::new(2, 4, 4, 60.0));
    let offset = failure.trim_end().rsplit_once(" at code offset 0x").map(|(_, o)| o);
    assert!(offset.is_some_and(|o| u32::from_str_radix(o, 16).is_ok()), "{failure}");
}

/// Every frozen program's manifest states its WASM's interface (`wasm_abi`) as its bytes give
/// it, every import the host's with the host's type (what the browser once read from the bytes),
/// and an optimized build's manifest states the optimized module's.
#[test]
fn each_frozen_programs_manifest_states_its_wasm_interface() {
    let host: std::collections::HashMap<&str, String> =
        wrela_abi::HOST_FUNCTIONS.iter().map(|f| (f.name, f.signature())).collect();
    for name in ["hello-field", "grazer", "herd", "wolf"] {
        let dir = built(&format!("frozen/{name}"));
        let read = |dir: &Path| {
            let wasm = std::fs::read(dir.join("game.wasm")).expect("game.wasm");
            let json = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest");
            let stated = wrela_abi::Manifest::parse(&json).expect("valid").wasm_abi;
            (stated, wrela_wasm::interface(&wasm).expect("reads"))
        };
        let (stated, abi) = read(&dir);
        assert_eq!(stated.as_ref(), Some(&abi), "{name}");
        assert!(abi.memory.is_some_and(|m| m.shared), "{name} imports its memory, shared");
        for [module, import, ty] in &abi.imports {
            assert_eq!(module, wrela_abi::IMPORT_MODULE, "{name}");
            if import != wrela_abi::IMPORT_MEMORY {
                assert_eq!(host.get(import.as_str()), Some(ty), "{name}'s `{import}`");
            }
        }
        assert!(abi.exports.contains(&["frame".into(), "(f32, i32, i32) -> ()".into()]), "{name}");
        // The optimized build's manifest is restated from its module.
        let small = scratch(&format!("abi-{name}"));
        wrela_tests::copy_dir(&dir, &small);
        let mut files: Vec<(String, Vec<u8>)> = ["game.wasm", "manifest.json"]
            .iter()
            .map(|f| (f.to_string(), std::fs::read(small.join(f)).expect("file")))
            .collect();
        let (code, _) =
            wrela_driver::optimize::optimize(&files[0].1, wrela_driver::optimize::Level::Size)
                .expect("optimizes");
        files[0].1 = code;
        wrela_driver::build::restate_abi(&mut files).expect("restated");
        for (f, bytes) in &files {
            std::fs::write(small.join(f), bytes).expect("write");
        }
        let (stated, abi) = read(&small);
        assert_eq!(stated, Some(abi), "{name}, optimized");
    }
}
