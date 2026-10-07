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

/// On the native host's CPU, the script's events reach the program: all 23, and the same
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
        assert_eq!(n, [Value::I32(23)]);
        host.hash()
    };
    assert_eq!(run(), run());
}

/// The same script gives the same state hash in Chrome's test mode and in the native host:
/// moves, clicks, a drag with Shift held, the wheel, keys, and text with Latin-1 letters.
#[test]
#[ignore = "long: needs Chrome, python3 and a GPU"]
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

/// In Chrome, an event reaches the program no later than the next frame: pointer moves sent
/// through the DOM at random times, while frames run at 60 a second, each arrive in the first
/// frame that starts after them.
#[test]
#[ignore = "long: alone: needs Chrome, python3 and a GPU"]
fn events_reach_the_program_by_the_next_frame() {
    let (dir, rel) = wrela_tests::page("compiler/tests/input", "input-latency");
    let run =
        wrela_tests::ChromeRun { latency: 200, ..wrela_tests::ChromeRun::new(180, 64, 64, 60.0) };
    wrela_tests::run_in_chrome_with(&rel, run);
    let text = std::fs::read_to_string(dir.join("results/latency.json")).expect("latency.json");
    let v: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    let starts: Vec<f64> = v["frames"]
        .as_array()
        .expect("frames")
        .iter()
        .map(|t| t.as_f64().expect("a time"))
        .collect();
    let delivered = v["delivered"].as_array().expect("delivered");
    assert!(delivered.len() >= 190, "only {} of 200 events arrived", delivered.len());
    let mut worst: f64 = 0.0;
    for d in delivered {
        let sent = d["sent"].as_f64().expect("sent");
        let frame = d["frame"].as_u64().expect("frame") as usize;
        // It arrived in a frame that started after it was sent, and the frame before started
        // before it was sent: the first frame it could reach.
        assert!(
            starts[frame] >= sent,
            "event sent at {sent} arrived in frame {frame}, which started first"
        );
        if frame > 0 {
            // A frame that started at the same time as the event (the clocks' resolution can't
            // order them) was already reading its input. The same time is within Chrome's
            // clock resolution in an isolated page (5 µs): each context adds its own time origin
            // to its clock, and at epoch milliseconds the sums of equal instants can differ by
            // a rounding (a run failed by under a microsecond).
            assert!(
                starts[frame - 1] <= sent + 0.005,
                "event sent at {sent} waited a frame (frame {frame}, whose predecessor started {:.3} ms after it)",
                starts[frame - 1] - sent
            );
        }
        worst = worst.max(starts[frame] - sent);
    }
    let intervals: Vec<f64> = starts.windows(2).map(|w| w[1] - w[0]).collect();
    let longest = intervals.iter().fold(0.0f64, |m, x| m.max(*x));
    println!(
        "input latency: {} events, each in the next frame; longest wait {worst:.1} ms (longest frame interval {longest:.1} ms)",
        delivered.len()
    );
}
