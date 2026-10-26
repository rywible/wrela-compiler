//! `@test` functions and `wrela test` (language.md §13): each test of the program package runs
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
/// shown. A message given comes first. An operand that projects into a temporary
/// (`made(..).as_str()`) keeps the temporary through the comparison, and is shown as it is.
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

fn made(s: str) -> String {
    var out = String::new()
    out.push_str(s)
    out.push_str(\"!\")
    out
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
fn a_temporary_lives_through_the_comparison() {
    assert(String::from(\"ab\").as_str() == \"ab\")
    assert(made(\"ab\").as_str() == \"ab!\", \"differs\")
}

@test
fn a_temporary_is_shown() {
    assert(made(\"ab\").as_str() == \"ab\", \"made\")
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
        ("a_temporary_lives_through_the_comparison", ""),
        (
            "a_temporary_is_shown",
            "the test `a_temporary_is_shown` panicked: made: `made(\"ab\").as_str()` is \"ab!\"",
        ),
        ("messages_are_made_only_on_failure", ""),
    ];
    let want: Vec<(&str, String)> = want.iter().map(|(n, m)| (*n, m.to_string())).collect();
    assert_eq!(messages, want);
}

/// An `assert`'s operands run once each, in the order written, through the calls they make:
/// an index is read before a later argument changes it, and an earlier call that returns a
/// projection runs (and here panics) before a later argument's call.
#[test]
fn an_asserts_operands_run_in_order() {
    let src = "fn made(s: str) -> String {
    var out = String::new()
    out.push_str(s)
    out
}

fn changed(i: mut u32) -> String {
    i += 1
    made(\"ab\")
}

fn pick(n: u32, s: str) -> borrow str {
    if n == 0 { s } else { s.slice(from: s.len() - 1, to: s.len()) }
}

fn first(s: str) -> borrow str {
    if s.len() > 0 {
        panic(\"first\")
    }
    s
}

fn late(s: str) -> String {
    if s.len() > 0 {
        panic(\"late\")
    }
    made(s)
}

fn either(a: str, b: str) -> borrow str {
    if a.len() > 0 { a } else { b }
}

@test
fn an_index_is_read_first() {
    let values = [0, 1]
    var i = 0
    assert(pick(values[i], changed(mut i).as_str()) == \"ab\")
    i = 0
    assert(pick(s: changed(mut i).as_str(), n: values[i]) == \"b\")
}

@test
fn an_earlier_call_runs_first() {
    assert(either(first(\"ab\"), late(\"cd\").as_str()) == \"ab\")
}
";
    let out = wrela_driver::test(&package("test_items/order", src), None);
    let messages: Vec<(&str, String)> = out
        .results
        .iter()
        .map(|r| (r.name.as_str(), r.failure.as_ref().map_or(String::new(), |d| d.message.clone())))
        .collect();
    assert_eq!(
        messages,
        [
            ("an_index_is_read_first", String::new()),
            (
                "an_earlier_call_runs_first",
                "the test `an_earlier_call_runs_first` panicked: first".to_string()
            ),
        ]
    );
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

/// Frame tests (§13): `init`, then the frames at 60 a second on a 640 × 480 screen, then the
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

/// Frame tests on the GPU (§13, `@test(frames: n, gpu: true)`): compiler/tests/gpu-frames has a
/// kernel square numbers and reads them back between frames; one test finds the squares, and
/// one that expects a wrong square fails with the values it compared. Its frames draw a ramp:
/// the screen is within its golden, and far from a black one (the failure writes what it saw
/// and how far beside the package's build); the ramp's float target, named by its pass's
/// label, is within a golden of its own; and Chrome, running the same frames, ends on the same
/// state hash and frame.
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
            ("the_ramp_target_is_its_golden", true),
            ("both_hosts_draw_the_same_frame", true),
        ]
    );
    let why =
        |i: usize| out.results[i].failure.as_ref().map(|d| d.message.clone()).unwrap_or_default();
    assert!(why(1).contains("9"), "{}", why(1));
    assert!(why(3).contains("from its golden `goldens/black.png`"), "{}", why(3));
    assert!(why(3).contains("`build/goldens/goldens/black.diff.png`"), "{}", why(3));
    let diff = dir.join("build/goldens/goldens/black.diff.png");
    let (w, h, _) =
        wrela_host::image::png_of(&std::fs::read(&diff).expect("the diff"), &diff).expect("a PNG");
    assert_eq!((w, h), (640, 480));
}

/// Held requests (§13.2, `hold: [...]`): compiler/tests/held's fetch and storage read arrive on
/// the frames their holds name, a hold past the last frame never arrives, unheld ones arrive on
/// the first frame, and a file the build ships is in the test's build.
#[test]
fn frame_tests_hold_requests_until_their_frames() {
    let dir = wrela_tests::repo_root().join("compiler/tests/held");
    let out = wrela_driver::test(&dir, None);
    assert!(out.diagnostics.iter().all(|d| !d.is_error()), "{:?}", out.diagnostics);
    let ran: Vec<_> = out.results.iter().map(|r| (r.name.as_str(), r.failure.is_none())).collect();
    assert_eq!(
        ran,
        [
            ("requests_arrive_on_the_first_frame", true),
            ("held_requests_arrive_on_their_frames", true),
            ("a_hold_past_the_last_frame_never_arrives", true),
            ("a_shipped_file_arrives_in_a_test", true),
        ],
        "{:?}",
        out.results
    );
}

/// A program's GPU time (§17.1, `std::gpu::timed`): compiler/tests/timed's labelled kernel is
/// timed on the GPU after the frame it's asked in, and the CPU host's request fails.
#[test]
#[ignore = "needs a GPU"]
fn a_labelled_kernels_gpu_time_comes_back() {
    let dir = wrela_tests::repo_root().join("compiler/tests/timed");
    let out = wrela_driver::test(&dir, None);
    assert!(out.diagnostics.iter().all(|d| !d.is_error()), "{:?}", out.diagnostics);
    let ran: Vec<_> = out.results.iter().map(|r| (r.name.as_str(), r.failure.is_none())).collect();
    assert_eq!(
        ran,
        [("a_labelled_kernels_time_comes_back", true), ("a_cpu_host_times_nothing", true)],
        "{:?}",
        out.results
    );
}

/// A golden is written by `--bless` (`wrela test --bless`), from this machine's screen, and a
/// test with none fails saying so (§13).
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

/// Toolchain 0.6's GPU capabilities (compiler/tests/capabilities): texture copies (one from an
/// offset, at its default size) and readback, the new formats, a generated mip chain sampled
/// trilinear, blend states, a pass of three targets, a scissor, texture arrays, a cube, a depth
/// array, the depth targets of passes into several targets, a layer and a face, a direct indexed
/// draw and a multi-draw, each read back and checked; and Chrome records the same frames.
#[test]
#[ignore = "needs a GPU"]
fn every_gpu_capability_reads_back_what_it_drew() {
    let dir = wrela_tests::repo_root().join("compiler/tests/capabilities");
    assert_eq!(crate::tests_pass(&dir), 21);
}

/// Toolchain 0.6's kernel capabilities (compiler/tests/kernels): a histogram in workgroup
/// atomics, a compaction by `workgroup_prefix_sum`, and the optional features (subgroups, half
/// floats, BC1), each passing trivially on a device without it; Chrome agrees.
#[test]
#[ignore = "needs a GPU"]
fn every_kernel_capability_computes_what_the_cpu_does() {
    let dir = wrela_tests::repo_root().join("compiler/tests/kernels");
    assert_eq!(crate::tests_pass(&dir), 10);
}

/// `std::gpu::memory()` (compiler/tests/memory): std counts the buffers and textures a
/// program makes and drops, by their levels and layers.
#[test]
#[ignore = "needs a GPU"]
fn std_counts_the_gpu_memory_a_program_holds() {
    let dir = wrela_tests::repo_root().join("compiler/tests/memory");
    assert_eq!(crate::tests_pass(&dir), 1);
}

/// A constant that is mostly zeros (compiler/tests/zero_runs, language.md §13): its 64 KiB
/// table costs the WASM's data segments under a quarter of that, and both hosts read it back
/// whole (the test runs `hosts: both`).
#[test]
#[ignore = "needs a GPU"]
fn a_mostly_zero_constant_costs_its_bytes_only() {
    let dir = wrela_tests::repo_root().join("compiler/tests/zero_runs");
    assert_eq!(crate::tests_pass(&dir), 1);
    let wasm = std::fs::read(crate::built("zero_runs").join("game.wasm")).expect("game.wasm");
    let (segments, bytes) = wrela_wasm::data_segments(&wasm).expect("reads");
    eprintln!("the 65,536-byte table: {segments} data segments of {bytes} bytes in all");
    assert!(bytes < 65_536 / 4, "{bytes} bytes of data");
}

/// `const` parameters (compiler/tests/overrides, language.md §18.1): a kernel dispatched with
/// two sets of constants is two pipelines, each made with its own, and both hosts compute what
/// each set says; the microbench (compiler/tests/override_bench) times the `const` form and a
/// uniform-branch twin. And pipeline tiers (compiler/tests/tiers): a tier-1 pipeline a frame
/// needs first is made at once, on both hosts.
#[test]
#[ignore = "needs a GPU"]
fn const_parameters_and_tiers_run_on_both_hosts() {
    for (pkg, n) in [("overrides", 1), ("override_bench", 1), ("tiers", 1)] {
        let dir = wrela_tests::repo_root().join("compiler/tests").join(pkg);
        assert_eq!(crate::tests_pass(&dir), n, "{pkg}");
    }
}

/// Tapes that can hold a creature (language.md §14.4): each new operation against its std
/// twin at 10⁶ points on the CPU and the GPU (compiler/tests/tape_ops), and a full 4,096-op
/// tape bound as storage, run on both hosts and timed (compiler/tests/tape_long).
#[test]
#[ignore = "needs a GPU"]
fn tape_operations_match_their_twins_and_a_full_tape_runs() {
    for (pkg, n) in [("tape_ops", 2), ("tape_long", 1)] {
        let dir = wrela_tests::repo_root().join("compiler/tests").join(pkg);
        assert_eq!(crate::tests_pass(&dir), n, "{pkg}");
    }
}

/// What a tape must take as compiled code does (compiler/tests/tape_cases): a branch's writes
/// through a call, a value only part of which depends on the point, an integer table past 2²⁴,
/// `lattice` and `lattice_coord` past `i32`'s range, one function taped at two sizes, and a
/// pruned tape of `tape_with`.
#[test]
fn tapes_take_what_compiled_code_does() {
    let dir = wrela_tests::repo_root().join("compiler/tests/tape_cases");
    assert_eq!(crate::tests_pass(&dir), 6);
}

/// `gather` (language.md §18.8): a 2 × 2 colour texture's texels, one component of each, and a
/// 2 × 2 depth texture's depths and their comparisons, each in one fetch and in WGSL's order, on
/// both hosts (compiler/tests/gather).
#[test]
#[ignore = "needs a GPU"]
fn gathers_read_four_texels_in_wgsls_order() {
    let dir = wrela_tests::repo_root().join("compiler/tests/gather");
    assert_eq!(crate::tests_pass(&dir), 1);
}

/// A gather is one WGSL fetch: `textureGather`, and `textureGatherCompare` of a depth texture.
#[test]
fn a_gather_is_one_fetch_in_the_wgsl() {
    let dir = wrela_tests::repo_root().join("compiler/tests/gather");
    let built = wrela_tests::build(&dir).unwrap_or_else(|e| panic!("doesn't build:\n{e}"));
    let wgsl: String = built
        .files
        .iter()
        .filter(|(n, _)| n.ends_with(".wgsl"))
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
        .collect();
    assert_eq!(wgsl.matches("textureGather(").count(), 3, "two of colour, one of depth");
    assert_eq!(wgsl.matches("textureGatherCompare(").count(), 1);
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

/// A frame test in another module than `main` takes the program's state too, whatever order
/// the modules' files come in: `checks.wrela` is collected before `main.wrela`.
#[test]
fn a_frame_test_in_another_module_takes_the_state() {
    let dir = package(
        "test-items/frame-test-elsewhere",
        "pub struct Game {
    pub ticks: u32,
}

pub fn init() -> Game {
    Game { ticks: 0 }
}

pub fn frame(state: mut Game, time: f32, width: u32, height: u32) {
    state.ticks += 1
}
",
    );
    std::fs::write(
        dir.join("checks.wrela"),
        "use main::Game\n\n@test(frames: 30)\nfn counts(game: Game) {\n    assert(game.ticks == 30)\n}\n",
    )
    .expect("write checks.wrela");
    assert_eq!(crate::tests_pass(&dir), 1);
}
