//! Sketch 03's world on the ticker's thread, its player driven by keys through per-tick records
//! (#42 AC6; compiler/tests/sketches/03-keys). A scripted run of 2,000 ticks replays from its
//! tick log to the same hashes with any number of helpers, and a log with one record changed
//! fails at that tick. In Chrome (`--include-ignored`): the run is recorded as a tick log, which
//! replays on the native host with no GPU, on arm64 and on x86-64 under Rosetta; and key presses
//! sent through the DOM reach the screen, their latency measured.

use crate::built;
use wrela_host::{CpuBuild, ReplayError, TickLog};

const TICKS: u32 = 2000;

/// A script of key presses over `ticks` ticks: the arrows held in turn, Space now and then.
fn script(ticks: u32) -> String {
    let arrows = ["ArrowRight", "ArrowUp", "ArrowLeft", "ArrowDown"];
    let mut events = Vec::new();
    let mut t = 10;
    let mut i = 0;
    while t + 60 < ticks {
        let key = arrows[i % 4];
        events.push(format!(r#"{{"tick": {t}, "type": "keydown", "key": "{key}"}}"#));
        if i % 3 == 1 {
            events.push(format!(r#"{{"tick": {}, "type": "key", "key": "Space"}}"#, t + 20));
        }
        events.push(format!(r#"{{"tick": {}, "type": "keyup", "key": "{key}"}}"#, t + 45));
        t += 60 + (i as u32 * 7) % 23;
        i += 1;
    }
    format!("[\n  {}\n]\n", events.join(",\n  "))
}

/// The first tick at which `log` has a record, and that record's index.
fn first_record(log: &TickLog) -> (usize, usize) {
    let k = log.ticks.iter().position(|t| !t.records.is_empty()).expect("a tick with records");
    (k, 0)
}

#[test]
fn a_keyed_run_replays_and_a_changed_record_fails_at_its_tick() {
    let built = CpuBuild::load(built("sketches/03-keys")).expect("load");
    let script = wrela_host::parse_script(&script(TICKS)).expect("a script");
    let log = built.record_ticks(TICKS, &script, 2).expect("record");
    assert_eq!(log.ticks.len(), TICKS as usize);
    let bytes = log.encode();
    let read = TickLog::decode(&bytes).expect("reads back");
    for workers in [1, 2, 8] {
        assert_eq!(built.replay(&read, workers).expect("replays"), TICKS, "{workers} threads");
    }
    // A key going down becomes one going up.
    let (k, r) = first_record(&read);
    let mut changed = read.clone();
    changed.ticks[k].records[r][0] = wrela_abi::input::EventKind::KeyUp as u8;
    match built.replay(&changed, 2) {
        Err(ReplayError::Tick { tick, .. }) => assert_eq!(tick as usize, k),
        other => panic!("expected tick {k} to differ: {other:?}"),
    }
}

/// `wrela-host`, built for x86-64 without the GPU, to run under Rosetta: its path.
fn x86_host() -> std::path::PathBuf {
    let root = wrela_tests::repo_root();
    let status = std::process::Command::new("cargo")
        .args(["build", "--release", "-p", "wrela-host", "--no-default-features"])
        .args(["--target", "x86_64-apple-darwin"])
        .current_dir(&root)
        .status()
        .expect("cargo runs");
    assert!(status.success(), "building wrela-host for x86-64 failed");
    root.join("target/x86_64-apple-darwin/release/wrela-host")
}

/// `wrela-host --replay <log> --no-gpu <build>` for x86-64, under Rosetta: its output, and
/// whether it passed.
fn replay_on_x86(
    host: &std::path::Path,
    log: &std::path::Path,
    build: &std::path::Path,
) -> (bool, String) {
    let out = std::process::Command::new("arch")
        .arg("-x86_64")
        .arg(host)
        .arg("--replay")
        .arg(log)
        .arg("--no-gpu")
        .arg(build)
        .output()
        .expect("arch -x86_64 runs (Rosetta 2)");
    let text =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

#[test]
#[ignore = "needs Chrome, python3, a GPU and Rosetta"]
fn a_run_recorded_in_chrome_replays_on_arm64_and_x86_64() {
    let (dir, rel) = wrela_tests::page("compiler/tests/sketches/03-keys", "keys-chrome");
    std::fs::write(dir.join("keys.json"), script(TICKS)).expect("write the script");
    // Lockstep at 60 frames a second: frame i follows i + 1 ticks, so 2,000 frames run 2,000.
    let run = wrela_tests::ChromeRun {
        workers: 4,
        input: "keys.json".into(),
        ..wrela_tests::ChromeRun::new(TICKS, 64, 64, 60.0)
    };
    let chrome = wrela_tests::run_in_chrome_with(&rel, run);
    let ticks = chrome.ticks.expect("the ticker's ticks");
    assert_eq!(ticks.hashes.len(), TICKS as usize);
    let bytes = ticks.log.expect("a tick log");
    let log = TickLog::decode(&bytes).expect("Chrome's tick log reads");
    let built = CpuBuild::load(&dir).expect("load");
    assert_eq!(built.replay(&log, 2).expect("arm64 replays Chrome's log"), TICKS);
    // The same ticks the native host records from the script.
    let script = wrela_host::parse_script(&script(TICKS)).expect("a script");
    let native = built.record_ticks(TICKS, &script, 1).expect("record");
    for (k, (a, b)) in log.ticks.iter().zip(&native.ticks).enumerate() {
        assert_eq!(a.records, b.records, "tick {k}'s records");
        assert_eq!(a.hash, b.hash, "tick {k}'s hash");
    }
    // x86-64, under Rosetta: the log replays, and with one record changed it fails, naming the
    // tick.
    let host = x86_host();
    let path = dir.join("results/ticks.log");
    let (ok, text) = replay_on_x86(&host, &path, &dir);
    assert!(ok, "x86-64 didn't replay Chrome's log: {text}");
    assert!(text.contains(&format!("{TICKS} ticks replayed")), "{text}");
    let (k, r) = first_record(&log);
    let mut changed = log.clone();
    changed.ticks[k].records[r][0] = wrela_abi::input::EventKind::KeyUp as u8;
    let bad = dir.join("results/changed.ticks");
    std::fs::write(&bad, changed.encode()).expect("write the changed log");
    let (ok, text) = replay_on_x86(&host, &bad, &dir);
    assert!(!ok, "a changed log replayed: {text}");
    assert!(text.contains(&format!("tick {k} differs")), "{text}");
}

/// The input latency through the sim (#42 AC6, measured, not gated): from a key's DOM event to
/// the first frame that draws its effect, in Chrome at 60 frames a second, paced. Each press
/// is matched with the first frame after it that printed `moved`.
#[test]
#[ignore = "needs Chrome, python3 and a GPU"]
fn key_presses_reach_the_screen_through_the_sim() {
    let (dir, rel) = wrela_tests::page("compiler/tests/sketches/03-keys", "keys-latency");
    let presses = 40;
    let run = wrela_tests::ChromeRun {
        workers: 4,
        paced: true,
        keylatency: presses,
        nohash: true,
        ..wrela_tests::ChromeRun::new(900, 64, 64, 60.0)
    };
    let chrome = wrela_tests::run_in_chrome_with(&rel, run);
    let latency = wrela_tests::result_json(&dir.join("results"), "latency.json");
    // The key-downs, by when they were sent (every other event the page saw).
    let sent: Vec<f64> = latency["delivered"]
        .as_array()
        .expect("delivered")
        .iter()
        .map(|d| d["sent"].as_f64().expect("a time"))
        .collect::<Vec<_>>()
        .chunks(2)
        .map(|pair| pair[0])
        .collect();
    let moved: Vec<f64> = chrome
        .printed
        .iter()
        .filter(|(_, line)| line == "moved")
        .map(|(frame, _)| chrome.began_ms[*frame])
        .collect();
    let mut ms: Vec<f64> =
        sent.iter().filter_map(|&t| moved.iter().find(|&&f| f >= t).map(|f| f - t)).collect();
    assert!(
        ms.len() as u32 >= presses * 3 / 4,
        "only {} of {presses} presses moved the player",
        ms.len()
    );
    ms.sort_by(f64::total_cmp);
    let median = ms[ms.len() / 2];
    let p99 = ms[(ms.len() * 99 / 100).min(ms.len() - 1)];
    eprintln!(
        "key to screen through the sim, {} presses: median {median:.1} ms, 99th percentile {p99:.1} ms (#43 §6's estimate: about 37 ms more than the frame's own latency)",
        ms.len()
    );
}
