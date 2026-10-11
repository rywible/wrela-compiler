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

/// On the native host's CPU, the script's events reach the program: all 28, and the same
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
                host.push_input(e);
            }
            host.frame(wrela_host::frame_time(i, 60.0), 640, 480).expect("frame");
        }
        let n = host.call_export("event_count", &[]).expect("event_count");
        assert_eq!(n, [Value::I32(28)]);
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
