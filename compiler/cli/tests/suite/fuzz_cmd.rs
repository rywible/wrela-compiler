//! `wrela fuzz` (language.md §16.2): a step that adds key codes into a `u8` traps once they pass
//! 255, and the fuzzer finds that line; a step that divides by `pointer x - 300` traps only at
//! one x, which mutating the inputs that reach the division finds; a step with no bug gives no
//! finding and exit status 0. A finding is minimized, saved as a tick log and a script, and
//! replayed until its bug is fixed.

use crate::common;

const TICKED: &str = "use std::hash::{StateHash, state_hash}
use std::input::Event
use std::tick::{Ticked, start}

struct Sim: Clone + StateHash {
    total: TOTAL,
}

@deterministic
fn step(sim: mut Sim, t: Ticked) {
    for e in t.records {
STEP
    }
}

@deterministic
fn hash(sim: Sim) -> u64 {
    state_hash(sim)
}

pub fn init() -> u32 {
    start(Sim { total: 0 }, hz: 60, step: step, hash: hash)
    0
}

pub fn frame(state: mut u32, time: f32, width: u32, height: u32) {}
";

/// The lines of [`TICKED`] before its step's body: where a finding's line counts from.
const BEFORE_STEP: u32 = 11;

const OVERFLOW: &str = "        if let Event::KeyDown { key, .. } = e {
            sim.total += u8(u32(key))
        }";

const DIVIDE: &str = "        if let Event::PointerDown { at, .. } = e {
            if at.x >= 0.0 && at.x < 1000.0 {
                let d = i32(at.x) - 300
                sim.total = sim.total.wrapping_add(1000 / d)
            }
        }";

const CLEAN: &str = "        if let Event::KeyDown { key, .. } = e {
            sim.total = sim.total.wrapping_add(u8(u32(key)))
        }";

pub(crate) fn ticked(name: &str, total: &str, step: &str) -> std::path::PathBuf {
    let main = TICKED.replace("TOTAL", total).replace("STEP", step);
    let package = name.replace('-', "_");
    common::package(name, &[("wrela.toml", &common::manifest(&package)), ("main.wrela", &main)])
}

fn fuzz(dir: &std::path::Path, extra: &[&str]) -> (Option<i32>, serde_json::Value) {
    let out = common::wrela().arg("fuzz").arg(dir).args(extra).arg("--json").output().expect("run");
    let json = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.code(), json)
}

#[test]
fn a_u8_counter_that_overflows_is_found_at_its_line() {
    let dir = ticked("fuzz-overflow", "u8", OVERFLOW);
    let (code, r) = fuzz(&dir, &["--runs", "20000", "--seed", "1"]);
    assert_eq!(code, Some(1), "{r}");
    let f = &r["findings"].as_array().unwrap()[0];
    assert_eq!(f["kind"], "trap", "{r}");
    assert_eq!(f["file"], "main.wrela", "{r}");
    assert_eq!(f["line"], BEFORE_STEP + 2, "{r}");
    assert!(r["sites"].as_u64().unwrap() > 0, "{r}");
}

#[test]
fn a_division_by_pointer_x_minus_300_is_found() {
    let dir = ticked("fuzz-divide", "i32", DIVIDE);
    let (code, r) = fuzz(&dir, &["--runs", "200000", "--seed", "1"]);
    assert_eq!(code, Some(1), "{r}");
    let f = &r["findings"].as_array().unwrap()[0];
    assert_eq!(f["kind"], "trap", "{r}");
    assert_eq!(f["line"], BEFORE_STEP + 4, "{r}");
    assert!(f["message"].as_str().unwrap().contains("divide by zero"), "{r}");
}

#[test]
fn a_step_with_no_bug_has_no_finding() {
    let dir = ticked("fuzz-clean", "u8", CLEAN);
    let (code, r) = fuzz(&dir, &["--runs", "5000", "--seed", "1"]);
    assert_eq!(code, Some(0), "{r}");
    assert_eq!(r["findings"].as_array().unwrap().len(), 0, "{r}");
    assert_eq!(r["executions"].as_u64().unwrap(), 5000, "{r}");
}

/// The overflow's finding, minimized, is the key presses that pass 255 and nothing else; its
/// tick log fails on replay at the same line (as `wrela-host --replay` runs it); and `--replay`
/// over the saved findings exits 1 before the bug is fixed and 0 after.
#[test]
fn a_finding_is_minimized_and_replays_until_its_bug_is_fixed() {
    let dir = ticked("fuzz-replay", "u8", OVERFLOW);
    let (code, r) = fuzz(&dir, &["--runs", "3000", "--seed", "1"]);
    assert_eq!(code, Some(1), "{r}");
    let f = &r["findings"].as_array().unwrap()[0];
    let files: Vec<&str> =
        f["files"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
    let (log, script) = (files[0], files[1]);
    assert!(log.ends_with(".ticks") && script.ends_with(".json"), "{files:?}");

    // Only key presses, whose codes pass 255 together and don't without the last.
    let events: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(script).unwrap()).unwrap();
    let events = events.as_array().unwrap();
    assert!(events.iter().all(|e| e["type"] == "keydown"), "{events:?}");
    let codes: Vec<u64> = events.iter().map(|e| e["code"].as_u64().unwrap()).collect();
    let sum: u64 = codes.iter().sum();
    assert!(sum > 255 && sum - codes.iter().min().unwrap() <= 255, "{codes:?}");

    let bytes = std::fs::read(log).unwrap();
    let log = wrela_abi::ticks::TickLog::decode(&bytes).unwrap();
    let build = wrela_host::CpuBuild::load_cpu(dir.join("build/fuzz-build")).unwrap();
    match build.replay(&log, 1) {
        Err(wrela_host::ReplayError::Failed { tick: Some(k), error }) => {
            assert_eq!(u64::from(k), f["tick"].as_u64().unwrap());
            let line = format!("main.wrela:{}:", BEFORE_STEP + 2);
            assert!(error.to_string().contains(&line), "{error}");
        }
        other => panic!("the log replays as {other:?}"),
    }

    let saved = dir.join("build/fuzz");
    let (code, r) = fuzz(&dir, &["--replay", saved.to_str().unwrap()]);
    assert_eq!(code, Some(1), "{r}");
    assert!(r["replayed"].as_array().unwrap().iter().all(|x| x["fails"] == true), "{r}");
    let main = std::fs::read_to_string(dir.join("main.wrela")).unwrap();
    std::fs::write(dir.join("main.wrela"), main.replace(OVERFLOW, CLEAN)).unwrap();
    let (code, r) = fuzz(&dir, &["--replay", saved.to_str().unwrap()]);
    assert_eq!(code, Some(0), "{r}");
    assert_eq!(r["replayed"].as_array().unwrap().len(), 2, "{r}");
}
