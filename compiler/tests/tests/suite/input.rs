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
/// platform/tests/text-input/typing.json), and each edit reaches the program once: typed text,
/// spaces too, as Text after its KeyDown; Enter and Backspace as KeyDowns the hidden field doesn't
/// also act on; a composition's changes whole, then its commit after a clear.
#[test]
#[ignore = "needs the runtime"]
fn the_browser_types_and_composes_each_edit_once() {
    let (_, rel) = wrela_tests::page("platform/tests/text-input", "text-input");
    let commands = std::fs::read_to_string(
        wrela_tests::repo_root().join("platform/tests/text-input/typing.json"),
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
