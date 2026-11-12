//! `wrela fuzz` (language.md §16.2): a step that adds key codes into a `u8` traps once they pass
//! 255, and the fuzzer finds that line; a step that divides by `pointer x - 300` traps only at
//! one x, which mutating the inputs that reach the division finds; a step with no bug gives no
//! finding and exit status 0.

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
