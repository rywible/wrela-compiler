//! The climber's fight with the wolf (#30; examples/climber, drawn by examples/climber-arena): the
//! package's own tests (its frame data, hits, the blink, the sword's sweep, the cape, the
//! darkness); and a scripted fight (examples/climber/fight.json, the package's test bot's presses
//! on a pad), recorded, replays to the same state hash at every tick; played in Chrome, its tick
//! log replays on the native host, with no GPU, the same.
//!
//! `tools/check.sh --long climber::`.

use wrela_host::CpuBuild;
use wrela_tests::{page, repo_root};

/// The scripted fight's ticks (25 s at 60 a second).
const TICKS: u32 = 1500;

fn script() -> String {
    std::fs::read_to_string(repo_root().join("examples/climber/fight.json")).expect("fight.json")
}

/// The fight's own tests (`wrela test examples/climber`, tests.wrela), on the CPU.
#[test]
#[ignore = "long: builds the fight and plays two minutes of it"]
fn the_fights_own_tests_pass() {
    assert_eq!(super::tests_pass(&repo_root().join("examples/climber")), 12);
}

/// The fight recorded natively replays to the same hashes: the same presses, the same fight.
#[test]
#[ignore = "long: builds the fight and records 1,500 ticks"]
fn the_scripted_fight_replays_natively() {
    let (dir, _) = page("examples/climber-arena", "climber-native");
    let built = CpuBuild::load(&dir).expect("load");
    let script = wrela_host::parse_script(&script()).expect("a script");
    let log = built.record_ticks(TICKS, &script, 2).expect("record");
    assert_eq!(log.ticks.len(), TICKS as usize);
    assert_eq!(built.replay(&log, 1).expect("replays"), TICKS);
}

/// The fight played in Chrome (WebAssembly, lockstep at 60 frames a second) replays on the
/// native host to the same state hash at every tick.
#[test]
#[ignore = "long: needs Chrome"]
fn a_fight_in_chrome_replays_natively() {
    let (dir, rel) = page("examples/climber-arena", "climber-chrome");
    let run = wrela_tests::ChromeRun {
        workers: 4,
        saturate: true,
        script: Some(script()),
        ..wrela_tests::ChromeRun::new(TICKS, 64, 36, 60.0)
    };
    let chrome = wrela_tests::run_in_chrome_with(&rel, run);
    let log = chrome.ticks.and_then(|t| t.log).expect("the ticker's tick log");
    assert_eq!(log.ticks.len(), TICKS as usize);
    let built = CpuBuild::load(&dir).expect("load");
    assert_eq!(built.replay(&log, 2).expect("the native host replays Chrome's log"), TICKS);
}
