//! AC1: input reaches programs, through `std::input`, in both hosts. compiler/tests/input keeps
//! every event it reads and prints its log's hash each frame, so the command stream's hash
//! covers what it read.

use wrela_host::{CpuBuild, Value, parse_script};

const SCRIPT: &str = "compiler/tests/input/script.json";

fn script() -> Vec<wrela_host::Scripted> {
    let text = std::fs::read_to_string(wrela_tests::repo_root().join(SCRIPT)).expect("the script");
    parse_script(&text).expect("a valid script")
}

/// The package's frame test plays the script and finds each event in its frame.
#[test]
fn a_frame_test_plays_a_script() {
    assert_eq!(crate::tests_pass(&wrela_tests::repo_root().join("compiler/tests/input")), 1);
}

/// On the native host's CPU, the script's events reach the program: all 33, and the same
/// command-stream hash on two runs.
#[test]
fn the_native_host_plays_a_script() {
    let dir = crate::built("input");
    let build = CpuBuild::load(&dir).expect("load");
    let run = || {
        let mut host = build.start_with(1).expect("start");
        let script = script();
        for i in 0..12u32 {
            for e in wrela_abi::input::events_at(&script, i) {
                host.push_scripted(e);
            }
            host.frame(wrela_host::frame_time(i, 60.0), 640, 480).expect("frame");
        }
        let n = host.call_export("event_count", &[]).expect("event_count");
        assert_eq!(n, [Value::I32(33)]);
        host.hash()
    };
    assert_eq!(run(), run());
}

/// The same script gives the same state hash in Chrome's test mode and in the native host:
/// moves, clicks, a drag with Shift held, the wheel, keys, text with Latin-1 letters, and two pads.
#[test]
#[ignore = "needs the runtime"]
fn both_hosts_read_the_same_events() {
    let (dir, rel) = wrela_tests::page("compiler/tests/input", "input-hosts");
    let text = std::fs::read_to_string(wrela_tests::repo_root().join(SCRIPT)).expect("the script");
    let run = wrela_tests::ChromeRun {
        script: Some(text),
        ..wrela_tests::ChromeRun::new(12, 64, 64, 60.0)
    };
    let chrome = wrela_tests::run_in_chrome_with(&rel, run);
    let times: Vec<f32> = (0..12).map(|i| wrela_host::frame_time(i, 60.0)).collect();
    let native = wrela_host::Host::load(&dir)
        .expect("load")
        .run_frames_with(&times, 64, 64, &script())
        .expect("run");
    println!("input: Chrome {} native {}", chrome.hash, native.hash_hex());
    assert_eq!(chrome.hash, native.hash_hex(), "the hosts gave the program different events");
}

/// In Chrome, text mode takes trusted keys and an input method (DevTools' Input domain,
/// compiler/tests/text-input/typing.json), and each edit reaches the program once: typed text,
/// spaces too, as Text after its KeyDown; Enter and Backspace as KeyDowns the hidden field doesn't
/// also act on; a composition's changes whole, then its commit after a clear.
#[test]
#[ignore = "needs the runtime"]
fn the_browser_types_and_composes_each_edit_once() {
    let (_, rel) = wrela_tests::page("compiler/tests/text-input", "text-input");
    let commands = std::fs::read_to_string(
        wrela_tests::repo_root().join("compiler/tests/text-input/typing.json"),
    )
    .expect("the commands");
    assert!(
        wrela_tests::run_page_with_devtools(
            &rel,
            "#test&frames=240&width=64&height=64",
            &commands,
            300
        ),
        "the browser run failed; see {rel}/results/console.log"
    );
    let log = std::fs::read_to_string(wrela_tests::repo_root().join(&rel).join("results/log.txt"))
        .expect("log.txt");
    let down = |code: &str| format!("down {}", wrela_abi::input::key_code(code));
    let want: Vec<String> = [
        "text mode".to_string(),
        down("KeyA"),
        "text 97".into(),
        down("Space"),
        "text 32".into(),
        down("KeyB"),
        "text 98".into(),
        down("Enter"),
        down("Backspace"),
        "compose 12363 0 2".into(),
        "compose 12394 1 2".into(),
        "compose 20206 0 2".into(),
        "compose 21517 1 2".into(),
        "compose 0 0 0".into(),
        "text 20206".into(),
        "text 21517".into(),
    ]
    .to_vec();
    assert_eq!(log.lines().collect::<Vec<_>>(), want);
}

/// A `std::input::Script` constant holds the script its methods say, in the hosts' format: its
/// events read back as the events each method names, with each default.
#[test]
fn a_script_constant_is_the_events_its_methods_name() {
    use wrela_abi::input::{At, Event, EventKind, META, SHIFT, key_code};
    let dir = crate::package(
        "script-constant",
        "use std::input::{Assist, Button, Key, Mods, PadAxis, PadButton, Script}\n\n\
         pub const PEN: Script = Script::frames()\n    \
             .press(2, Key::A, Mods { meta: true })\n    \
             .down(5, vec2(150.0, 150.0))\n    \
             .up(5, vec2(150.5, 151.0), Button::Secondary, Mods { shift: true })\n    \
             .text(6, \"é\")\n    \
             .assist(7, node: 12, action: Assist::Focus)\n    \
             .message_text(8, conn: 3, text: \"hi\")\n\n\
         pub const PAD: Script = Script::ticks()\n    \
             .pad_axis(1, PadAxis::LeftY, -0.75)\n    \
             .pad_button(2, PadButton::East, pressed: true)\n",
    );
    let script = |name: &str| {
        let out = wrela_driver::script::script(&dir, name);
        let json = out.script.unwrap_or_else(|why| {
            panic!("{why}: {}", wrela_diag::render::render_all(&out.sources, &out.diagnostics))
        });
        parse_script(&json).expect("the hosts' format")
    };
    let pen = script("PEN");
    let at: Vec<At> = pen.iter().map(|s| s.at).collect();
    let frames = [2, 2, 5, 5, 6, 7, 8].map(At::Frame);
    assert_eq!(at, frames);
    let events: Vec<Event> = pen.iter().map(|s| s.event).collect();
    let a = key_code("KeyA");
    assert_eq!(
        events,
        [
            Event::key(true, a, false, META),
            Event::key(false, a, false, META),
            Event::pointer(EventKind::PointerDown, 150.0, 150.0, 0, 0),
            Event::pointer(EventKind::PointerUp, 150.5, 151.0, 2, SHIFT),
            Event::text('é'),
            Event::assist(12, 2),
            Event::message(3, 0, 2),
        ]
    );
    assert_eq!(pen[6].payload, b"hi");
    let pad = script("PAD");
    assert_eq!(pad.iter().map(|s| s.at).collect::<Vec<_>>(), [At::Tick(1), At::Tick(2)]);
    assert_eq!(pad[0].event, Event::pad_axis(0, 1, -0.75));
    assert_eq!(pad[1].event, Event::pad_button(0, 1, 1.0, true));
}

/// A `Script` constant that names another is that script, through a chain of names: `wrela
/// script` prints it and a frame test plays it.
#[test]
fn a_script_named_by_another_constant_is_the_same_script() {
    let dir = crate::package(
        "script-alias",
        "use std::input::{Input, Key, Script}\n\n\
         pub const BASE: Script = Script::frames().press(0, Key::A)\n\
         pub const SAME: Script = BASE\n\
         pub const REPLAY: Script = SAME\n\n\
         struct S {\n    input: Input,\n    pressed: bool,\n}\n\n\
         pub fn init() -> S {\n    S { input: Input::new(), pressed: false }\n}\n\n\
         pub fn frame(s: mut S, time: f32, width: u32, height: u32) {\n    \
             s.input.read()\n    \
             if s.input.pressed(Key::A) { s.pressed = true }\n}\n\n\
         @test(frames: 2, input: REPLAY)\n\
         fn the_alias_plays(s: S) {\n    assert(s.pressed)\n}\n",
    );
    let json = |name: &str| wrela_driver::script::script(&dir, name).script.expect(name);
    assert_eq!(json("REPLAY"), json("BASE"));
    assert_eq!(json("SAME"), json("BASE"));
    let out = wrela_driver::test(&dir, None);
    assert!(
        out.passed(),
        "{}\n{:#?}",
        wrela_diag::render::render_all(&out.sources, &out.diagnostics),
        out.results
    );
    assert_eq!(out.results.len(), 1);
}

/// A method given what the hosts would refuse panics, which fails the constant's build: frames
/// that decrease, a tick's message, `Key::Unknown`, an axis past its range.
#[test]
fn a_bad_script_fails_its_constants_build() {
    for (i, (value, why)) in [
        ("Script::frames().press(3, Key::A).press(2, Key::B)", "frame 2 comes after frame 3"),
        (
            "Script::ticks().pad_press(9, PadButton::South).pad_press(1, PadButton::South)",
            "tick 1 comes after tick 9",
        ),
        ("Script::ticks().message(0, conn: 1, bytes: [1])", "a tick has no network events"),
        ("Script::frames().press(0, Key::Unknown)", "not `Key::Unknown`"),
        ("Script::ticks().pad_axis(0, PadAxis::LeftX, 1.5)", "from -1 to 1"),
    ]
    .into_iter()
    .enumerate()
    {
        let dir = crate::package(
            &format!("script-bad-{i}"),
            &format!(
                "use std::input::{{Key, PadAxis, PadButton, Script}}\n\npub const BAD: Script = {value}\n"
            ),
        );
        let out = wrela_driver::script::script(&dir, "BAD");
        let rendered = wrela_diag::render::render_all(&out.sources, &out.diagnostics);
        assert!(out.script.is_err() && rendered.contains(why), "{value}: {rendered}");
    }
}

/// A frame test plays a `Script` constant; a tick test can't play one whose events are at frames.
#[test]
fn a_frame_test_plays_a_script_constant() {
    let dir = crate::package(
        "script-test",
        "use std::input::{Event, Key, Script, events}\n\n\
         pub struct Game {\n    keys: u32,\n}\n\n\
         pub fn init() -> Game {\n    Game { keys: 0 }\n}\n\n\
         pub fn frame(game: mut Game, time: f32, width: u32, height: u32) {\n    \
             for e in events() {\n        \
                 if let Event::KeyDown { key, .. } = e {\n            \
                     if key == Key::A {\n                game.keys += 1\n            }\n        \
                 }\n    \
             }\n}\n\n\
         const TWICE: Script = Script::frames().press(0, Key::A).press(2, Key::A)\n\n\
         @test(frames: 3, input: TWICE)\n\
         fn both_presses_arrive(game: Game) {\n    assert(game.keys == 2)\n}\n\n\
         @test(ticks: 1, input: TWICE)\n\
         fn ticks_get_no_frames() {\n}\n",
    );
    let out = wrela_driver::test(&dir, None);
    assert!(!wrela_diag::has_errors(&out.diagnostics), "{:?}", out.diagnostics);
    assert_eq!(out.results.len(), 2);
    assert!(out.results[0].failure.is_none(), "{:?}", out.results[0].failure);
    let failure = out.results[1].failure.as_ref().expect("a tick test of frames' events");
    assert!(failure.message.contains("a tick test runs no frames"), "{}", failure.message);
}
