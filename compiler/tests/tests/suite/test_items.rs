//! `@test` functions and `wrela test` (language.md §10): each test of the program package runs
//! as the build runs constants, in the order written, and passes unless it panics. A failed one
//! is E0706 at the innermost frame in the program's own code, with the call chain; one that
//! runs away is E0705. Tests aren't part of a build, and a dependency's tests don't run.

use crate::{package, scratch};
use wrela_diag::codes;
use wrela_host::CpuHost;

/// A test's fuel here: enough for every test but `spins`, which runs past it at once rather
/// than after `wrela test`'s seconds.
const FUEL: u64 = 1 << 26;

const TESTS: &str = "const SCALE: f32 = 2.0

fn double(x: f32) -> f32 {
    x * SCALE
}

fn third(x: f32) -> f32 {
    x / 4.0
}

fn checked_third(x: f32) -> f32 {
    let t = third(x)
    assert(t * 3.0 == x)
    t
}

@test
fn doubles() {
    assert(double(2.0) == 4.0)
}

@test
fn thirds() {
    let t = checked_third(6.0)
}

@test
fn indexes() {
    let a = [1.0, 2.0]
    var i = 0
    while i < 3 {
        i = i + 1
    }
    let _x = a[i]
}

@test
fn spins() {
    var n: u32 = 0
    while true {
        n = n.wrapping_add(1)
    }
}

@test
fn reads_a_vec() {
    var v: Vec<u32> = Vec::new()
    for i in 0..100 {
        v.push(i)
    }
    assert(v.len() == 100 && v[99] == 99)
}
";

#[test]
fn tests_run_in_order_and_fail_with_a_call_chain() {
    let dir = package("test_items/run", TESTS);
    let out = wrela_driver::test_with_fuel(&dir, None, FUEL);
    let errors: Vec<_> = out.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let ran: Vec<(&str, Option<&str>)> = out
        .results
        .iter()
        .map(|r| (r.name.as_str(), r.failure.as_ref().map(|d| d.code.as_str())))
        .collect();
    assert_eq!(
        ran,
        [
            ("doubles", None),
            ("thirds", Some("E0706")),
            ("indexes", Some("E0706")),
            ("spins", Some("E0705")),
            ("reads_a_vec", None),
        ]
    );
    assert!(!out.passed());
    let failure = |name: &str| {
        let r = out.results.iter().find(|r| r.name == name).expect("ran");
        r.failure.clone().expect("failed")
    };
    // The failed `assert` is the primary span, in `checked_third`; the test's call is a label.
    let thirds = failure("thirds");
    assert_eq!(thirds.code, codes::E0706);
    assert_eq!(
        thirds.message,
        "the test `thirds` panicked: assertion failed: `t * 3.0` is 4.5, `x` is 6"
    );
    let at = |span: wrela_diag::Span| {
        let text = &out.sources.file(span.file).text;
        text[span.start as usize..span.end as usize].to_string()
    };
    assert_eq!(at(thirds.primary.as_ref().expect("a span").span), "assert(t * 3.0 == x)");
    let labels: Vec<(String, Option<&str>)> =
        thirds.secondary.iter().map(|l| (at(l.span), l.message.as_deref())).collect();
    assert_eq!(labels, [("checked_third(6.0)".to_string(), Some("in the test `thirds`"))]);
    assert!(thirds.notes.iter().any(|n| n == "the call chain: thirds → checked_third"));
    // A trap names what traps on the CPU (§11), at the index.
    let indexes = failure("indexes");
    assert!(indexes.message.starts_with("the test `indexes` trapped: an index out of range"));
    assert_eq!(at(indexes.primary.as_ref().expect("a span").span), "a[i]");
    assert_eq!(failure("spins").code, codes::E0705);
}

/// Tests are built only for `wrela test`: a build has no export for one, and `wrela check` runs
/// none (so it reports no failure).
#[test]
fn tests_are_not_part_of_a_build() {
    let dir = package("test_items/build", TESTS);
    let checked = wrela_driver::check(&dir);
    assert!(!checked.has_errors());
    assert!(checked.diagnostics.iter().all(|d| d.code != codes::E0706));
    let built = wrela_tests::build(&dir).expect("builds");
    let out = dir.join("build");
    built.write_to(&out).expect("write the build");
    let host = CpuHost::load(&out).expect("load");
    let exports = host.exports();
    for name in ["doubles", "thirds", "indexes", "spins", "reads_a_vec"] {
        assert!(!exports.iter().any(|(n, ..)| n == name), "the build exports the test `{name}`");
    }
}

/// Only the program package's tests run: a dependency's failing test doesn't.
#[test]
fn a_dependencys_tests_dont_run() {
    let dir = scratch("test_items/deps");
    let write = |path: &str, text: &str| {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
        std::fs::write(p, text).expect("write");
    };
    write("wrela.toml", "[package]\nname = \"game\"\n\n[dependencies]\nlib = { path = \"lib\" }\n");
    write("lib/wrela.toml", "[package]\nname = \"lib\"\n");
    write(
        "lib/count.wrela",
        "pub fn one() -> u32 {\n    1\n}\n\n@test\nfn fails() {\n    assert(one() == 2)\n}\n",
    );
    write(
        "main.wrela",
        "use lib::count::one\n\n@test\nfn one_is_one() {\n    assert(one() == 1)\n}\n\npub fn frame(time: f32, width: u32, height: u32) {}\n",
    );
    let out = wrela_driver::test(&dir, None);
    let ran: Vec<_> = out.results.iter().map(|r| (r.name.as_str(), r.failure.is_none())).collect();
    assert_eq!(ran, [("one_is_one", true)]);
    assert!(out.passed());
}

/// A package with errors runs no test.
#[test]
fn errors_keep_tests_from_running() {
    let dir = package(
        "test_items/errors",
        "@test\nfn good() {\n    assert(true)\n}\n\n@test\nfn bad(x: f32) {\n}\n",
    );
    let out = wrela_driver::test(&dir, None);
    assert!(out.results.is_empty());
    assert!(out.diagnostics.iter().any(|d| d.code == codes::E0222));
    assert!(!out.passed());
}

/// A failed `assert` of a comparison shows the operands that aren't literals, each evaluated
/// once, with their source: numbers, vectors and text (quoted); a type without `Format` isn't
/// shown. A message given comes first.
#[test]
fn a_failed_assert_shows_what_it_compared() {
    let src = "struct Id: Copy + Eq {
    n: u32,
}

fn half(x: f32) -> f32 {
    x / 3.0
}

fn counted(log: mut u32) -> u32 {
    log += 1
    log
}

@test
fn one_operand() {
    let got = half(1.0)
    assert(got == 0.5)
}

@test
fn with_a_message() {
    assert(half(3.0) > half(6.0), \"the first is larger\")
}

@test
fn text() {
    let s = f\"{1.5}\"
    assert(s == \"1.25\")
}

@test
fn not_formatted() {
    assert(Id { n: 1 } == Id { n: 2 })
}

@test
fn evaluated_once() {
    var log = 0
    assert(counted(mut log) == 2)
}

@test
fn messages_are_made_only_on_failure() {
    var log = 0
    assert(1 < 2, f\"{counted(mut log)}\")
    assert(log == 0)
}
";
    let out = wrela_driver::test(&package("test_items/values", src), None);
    let messages: Vec<(&str, String)> = out
        .results
        .iter()
        .map(|r| (r.name.as_str(), r.failure.as_ref().map_or(String::new(), |d| d.message.clone())))
        .collect();
    let want = [
        ("one_operand", "the test `one_operand` panicked: assertion failed: `got` is 0.33333334"),
        (
            "with_a_message",
            "the test `with_a_message` panicked: the first is larger: `half(3.0)` is 1, `half(6.0)` is 2",
        ),
        ("text", "the test `text` panicked: assertion failed: `s` is \"1.5\""),
        ("not_formatted", "the test `not_formatted` panicked: assertion failed"),
        (
            "evaluated_once",
            "the test `evaluated_once` panicked: assertion failed: `counted(mut log)` is 1",
        ),
        ("messages_are_made_only_on_failure", ""),
    ];
    let want: Vec<(&str, String)> = want.iter().map(|(n, m)| (*n, m.to_string())).collect();
    assert_eq!(messages, want);
}

/// Where else a failure is explained: a constant the build computes, and a debug build. A
/// release build's failed `assert` says only its message, and has no trace of the formatting.
#[test]
fn constants_and_debug_builds_explain_and_release_builds_dont() {
    let constant = "const N: u32 = third(9)

fn third(n: u32) -> u32 {
    let t = n / 3
    assert(t == 4)
    t
}
";
    let out = wrela_driver::check(&package("test_items/explained-const", constant));
    let e = out.diagnostics.iter().find(|d| d.code.as_str() == "E0704").expect("E0704");
    assert!(e.message.ends_with("panicked: assertion failed: `t` is 3"), "{}", e.message);
    let export = "pub fn check(x: f32) -> f32 {
    let y = x * 2.0
    assert(y > 1.0)
    y
}
";
    let message = |debug: bool| {
        let dir = package(&format!("test_items/explained-{debug}"), export);
        let built = if debug { wrela_driver::build_debug(&dir) } else { wrela_driver::build(&dir) };
        assert!(!built.has_errors());
        let out = dir.join("build");
        built.write_to(&out).expect("write the build");
        let wasm = built.files.iter().find(|(p, _)| p.ends_with(".wasm")).expect("wasm").1.len();
        let mut host = CpuHost::load(&out).expect("load");
        let err = host.call_export("check", &[wrela_host::Value::F32(0.25)]).expect_err("fails");
        (err.to_string(), wasm)
    };
    let (debug, debug_size) = message(true);
    let (release, release_size) = message(false);
    assert!(debug.contains("assertion failed: `y` is 0.5"), "{debug}");
    assert!(release.contains("assertion failed") && !release.contains("`y`"), "{release}");
    // Formatting a float is tens of kilobytes; a release build has none of it.
    assert!(release_size + 20_000 < debug_size, "release {release_size} B, debug {debug_size} B");
}

/// `wrela test <package> <filter>` runs the tests whose names contain the filter, and says how
/// many it left out.
#[test]
fn a_filter_chooses_tests_by_name() {
    let dir = package("test_items/filter", TESTS);
    let out = wrela_driver::test_with_fuel(&dir, Some("pin"), FUEL);
    let ran: Vec<&str> = out.results.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(ran, ["spins"]);
    assert_eq!(out.filtered_out, 4);
}

/// Tests run as a debug build's code does: `debug_build()` is true, and a float operation that
/// makes a NaN from operands that hold none fails the test (§11).
#[test]
fn tests_run_with_a_debug_builds_checks() {
    let src = "use std::mem::debug_build

@test
fn is_debug() {
    assert(debug_build())
}

@test
fn makes_a_nan() {
    var z = 0.0
    let _x = z / z
}
";
    let out = wrela_driver::test(&package("test_items/debug", src), None);
    let ran: Vec<(&str, Option<String>)> = out
        .results
        .iter()
        .map(|r| (r.name.as_str(), r.failure.as_ref().map(|d| d.message.clone())))
        .collect();
    assert_eq!(
        ran,
        [
            ("is_debug", None),
            (
                "makes_a_nan",
                Some(
                    "the test `makes_a_nan` panicked: debug build: a float operation created a NaN"
                        .to_string()
                )
            ),
        ]
    );
}

/// std's own `@test`s pass (only a program package's tests run, so the suite runs std's).
#[test]
fn std_tests_pass() {
    let out = wrela_driver::test_std(&package("test_items/std", ""), None);
    let errors: Vec<_> = out.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert!(
        errors.is_empty(),
        "{}",
        wrela_diag::render::render_all(&out.sources, &out.diagnostics)
    );
    let failed: Vec<String> = out
        .results
        .iter()
        .filter_map(|r| r.failure.as_ref())
        .map(|d| wrela_diag::render::render(&out.sources, d))
        .collect();
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    assert!(out.results.len() >= 5, "{} of std's tests ran", out.results.len());
}

/// Frame tests (§10): `init`, then the frames at 60 a second on a 640 × 480 screen, then the
/// test with the program's state, on the native host's CPU, which answers storage requests.
/// A failure says in which call it happened, at the program's own code; tests of code run
/// beside them, all in the order written.
#[test]
fn frame_tests_run_the_programs_frames_first() {
    let src = "use std::io::{Pending, load, store}

pub struct Game {
    ticks: u32,
    time: f32,
    width: u32,
    saved: Option<Pending<u8>>,
    loaded: Option<Pending<u8>>,
    length: u32,
}

pub fn init() -> Game {
    Game { ticks: 0, time: 0.0, width: 0, saved: None, loaded: None, length: 0 }
}

pub fn frame(state: mut Game, time: f32, width: u32, height: u32) {
    state.ticks += 1
    state.time = time
    state.width = width
    if state.ticks == 1 {
        state.saved = Some(store(\"slot\", [1, 2, 3]))
    }
    if state.ticks == 5 {
        state.loaded = Some(load(\"slot\"))
    }
    match mut state.loaded {
        Some(p) => {
            if let Some(Ok(bytes)) = p.poll() {
                state.length = bytes.len()
            }
        },
        None => {},
    }
    assert(state.ticks < 500, \"too many frames\")
}

@test(frames: 120)
fn counts_its_frames(game: Game) {
    assert(game.ticks == 120 && game.width == 640)
    assert(game.time == 119.0 / 60.0)
    assert(game.length == 3, \"the stored bytes didn't load back\")
}

@test
fn a_test_of_code() {
    assert(init().ticks == 0)
}

@test(frames: 60)
fn after_the_frames(game: Game) {
    assert(game.ticks == 61)
}

@test(frames: 600)
fn in_a_frame() {
}
";
    let out = wrela_driver::test(&package("test_items/frames", src), None);
    let errors: Vec<_> = out.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let ran: Vec<(&str, String)> = out
        .results
        .iter()
        .map(|r| (r.name.as_str(), r.failure.as_ref().map_or(String::new(), |d| d.message.clone())))
        .collect();
    let want = [
        ("counts_its_frames", ""),
        ("a_test_of_code", ""),
        (
            "after_the_frames",
            "the test `after_the_frames` panicked after 60 frames: assertion failed: `game.ticks` is 60",
        ),
        (
            "in_a_frame",
            "the test `in_a_frame` panicked in frame 499 of 600: too many frames: `state.ticks` is 500",
        ),
    ];
    let want: Vec<(&str, String)> = want.iter().map(|(n, m)| (*n, m.to_string())).collect();
    assert_eq!(ran, want);
    // The failure in a frame is at the program's own `assert`.
    let d = out.results[3].failure.as_ref().expect("failed");
    let span = d.primary.as_ref().expect("a span").span;
    let text = &out.sources.file(span.file).text[span.start as usize..span.end as usize];
    assert_eq!(text, "assert(state.ticks < 500, \"too many frames\")");
}

/// A trap in `init` fails each frame test there, and a frame that never ends runs out of a
/// test's fuel rather than hanging `wrela test`.
#[test]
fn frame_tests_fail_in_init_and_past_their_fuel() {
    let broken_init = "pub struct Game {
    n: u32,
}

pub fn init() -> Game {
    let v: [u32; 2] = [1, 2]
    var i = 0
    while i < 3 {
        i += 1
    }
    Game { n: v[i] }
}

pub fn frame(state: mut Game, time: f32, width: u32, height: u32) {}

@test(frames: 1)
fn starts(game: Game) {
}
";
    let out = wrela_driver::test(&package("test_items/frames-init", broken_init), None);
    let d = out.results[0].failure.as_ref().expect("fails");
    assert!(
        d.message.starts_with("the test `starts` trapped in `init`: an index out of range"),
        "{}",
        d.message
    );
    let spins = "pub fn frame(time: f32, width: u32, height: u32) {
    var n: u32 = 0
    while true {
        n = n.wrapping_add(1)
    }
}

@test(frames: 1)
fn spins() {
}
";
    let out = wrela_driver::test_with_fuel(&package("test_items/frames-fuel", spins), None, FUEL);
    let d = out.results[0].failure.as_ref().expect("fails");
    assert_eq!(d.code, wrela_diag::codes::E0705);
    assert_eq!(d.message, "the test `spins` ran past a test's fuel limit in frame 0 of 1");
}

/// `@testing` (§9): a test build has the program's `@testing` export, the kernel only it
/// dispatches, and the path only `test_build()` takes; a shipped build has none of them.
#[test]
fn a_shipped_build_has_no_testing_exports() {
    let src = r#"
use std::gpu::{GlobalId, GpuBuffer, Slots, buffer, dispatch}
use std::mem::test_build

struct State {
    out: GpuBuffer<f32>,
    frames: u32,
}

@compute(64)
fn probe(out: mut Slots<f32>, id: GlobalId) {
    out[id] = f32(id.x)
}

pub fn init() -> State {
    State { out: buffer(64), frames: 0 }
}

@testing
pub fn test_frames(s: mut State) -> u32 {
    dispatch(probe.bind(mut s.out), over: 64)
    s.frames
}

pub fn frame(s: mut State, time: f32, width: u32, height: u32) {
    if test_build() {
        s.frames += 1
    }
}
"#;
    let dir = scratch("test_items/testing");
    std::fs::write(dir.join("main.wrela"), src).expect("write main.wrela");
    let shipped = wrela_driver::build(&dir);
    let tested = wrela_driver::build_for_tests(&dir, false);
    for out in [&shipped, &tested] {
        assert!(!out.has_errors(), "{:?}", out.diagnostics);
    }
    assert_eq!(shipped.pipelines.len(), 0, "a shipped build records the test's kernel");
    assert_eq!(tested.pipelines.len(), 1, "a test build doesn't record the test's kernel");
    let load = |out: &wrela_driver::Output, name: &str| {
        let at = dir.join(name);
        out.write_to(&at).expect("write the build");
        CpuHost::load(&at).expect("load")
    };
    let mut host = load(&tested, "tested");
    host.init().expect("init");
    host.frame(0.0, 4, 4).expect("a frame");
    let frames = wrela_tests::one_u32(&mut host, "test_frames", &[]);
    assert_eq!(frames, 1, "test_build() was false in a test build");
    let shipped = load(&shipped, "shipped");
    let exports = shipped.exports();
    assert!(
        !exports.iter().any(|(n, ..)| n == "test_frames"),
        "a shipped build exports the test's"
    );
}

/// Frame tests on the GPU (§10, `@test(frames: n, gpu: true)`): compiler/tests/gpu-frames has a
/// kernel square numbers and reads them back between frames; one test finds the squares, and
/// one that expects a wrong square fails with the values it compared. Its frames draw a ramp:
/// the screen is within its golden, and far from a black one.
#[test]
#[ignore = "needs a GPU"]
fn gpu_frame_tests_read_back_what_the_gpu_wrote() {
    let dir = wrela_tests::repo_root().join("compiler/tests/gpu-frames");
    let out = wrela_driver::test(&dir, None);
    assert!(out.diagnostics.iter().all(|d| !d.is_error()), "{:?}", out.diagnostics);
    let ran: Vec<_> = out.results.iter().map(|r| (r.name.as_str(), r.failure.is_none())).collect();
    assert_eq!(
        ran,
        [
            ("squares_come_back", true),
            ("a_wrong_square_fails", false),
            ("the_ramp_is_its_golden", true),
            ("a_ramp_is_not_black", false),
        ]
    );
    let why =
        |i: usize| out.results[i].failure.as_ref().map(|d| d.message.clone()).unwrap_or_default();
    assert!(why(1).contains("9"), "{}", why(1));
    assert!(why(3).contains("from its golden `goldens/black.png`"), "{}", why(3));
}

/// A golden is written by `--bless` (`wrela test --bless`), from this machine's screen, and a
/// test with none fails saying so (§10).
#[test]
#[ignore = "needs a GPU"]
fn a_golden_is_blessed_from_the_screen() {
    let from = wrela_tests::repo_root().join("compiler/tests/gpu-frames");
    let dir = crate::scratch("golden-bless");
    wrela_tests::copy_dir(&from, &dir);
    let golden = dir.join("goldens/ramp.png");
    std::fs::remove_file(&golden).expect("remove the golden");
    let out = wrela_driver::test(&dir, Some("the_ramp"));
    let why = out.results[0].failure.as_ref().map(|d| d.message.clone()).unwrap_or_default();
    assert!(why.contains("has no golden"), "{why}");
    let out = wrela_driver::test_blessing(&dir, Some("the_ramp"));
    assert!(out.passed(), "{:?}", out.results);
    assert!(golden.is_file(), "--bless wrote the golden");
    assert!(wrela_driver::test(&dir, Some("the_ramp")).passed(), "and it passes after");
}

/// Constants that hold `Text`, read by GPU code (M6, AC15): compiler/tests/gpu-constants has a
/// kernel read a table of sites, each a place, a height and a name, written as a literal and
/// computed by the build; it reads back what the CPU computes from the same tables. Spike 17
/// found this an internal error (I0001).
#[test]
#[ignore = "needs a GPU"]
fn the_gpu_reads_constants_that_hold_text() {
    let dir = wrela_tests::repo_root().join("compiler/tests/gpu-constants");
    let out = wrela_driver::test(&dir, None);
    assert!(out.diagnostics.iter().all(|d| !d.is_error()), "{:?}", out.diagnostics);
    let ran: Vec<_> = out.results.iter().map(|r| (r.name.as_str(), r.failure.is_none())).collect();
    assert_eq!(ran, [("the_gpu_reads_constants_that_hold_text", true)], "{:?}", out.results);
}
