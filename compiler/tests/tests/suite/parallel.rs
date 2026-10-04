//! Parallel jobs (AC6, language.md §6.12): compiler/tests/parallel steps 10,000 particles with
//! `par_each_mut` and sums their energy with `par_map_reduce` each frame. The results, and the
//! state hash (which covers every particle each frame), are the same with 1, 2, 4 and 8
//! workers: on the CPU host here, in the native host and in headless Chrome
//! (`cargo test -p wrela-tests --test suite parallel:: -- --include-ignored`).

use wrela_host::{CpuBuild, Host, Options, Value, frame_time};
use wrela_tests::page;

const FRAMES: u32 = 20;
const WORKERS: [u32; 4] = [1, 2, 4, 8];

fn f32_of(v: &[Value]) -> f32 {
    match v {
        [Value::F32(x)] => *x,
        other => panic!("returned {other:?}"),
    }
}

#[test]
fn the_results_dont_depend_on_the_workers() {
    let (dir, _) = page("compiler/tests/parallel", "parallel-cpu");
    let build = CpuBuild::load(&dir).expect("load");
    let mut seen: Option<(u64, f32)> = None;
    for workers in WORKERS {
        let mut host = build.start_with(workers).expect("start");
        host.frame(frame_time(0, 60.0), 64, 64).expect("frame");
        let first = f32_of(&host.call_export("energy", &[]).expect("energy"));
        for i in 1..FRAMES {
            host.frame(frame_time(i, 60.0), 64, 64).expect("frame");
        }
        let energy = f32_of(&host.call_export("energy", &[]).expect("energy"));
        // The particles fall: `par_each_mut`'s closure changes each one (writes through a
        // closure's `mut` parameter were once lost, and every hash agreed anyway).
        assert!(
            energy > first * 1.01,
            "{workers} workers: the particles didn't move ({first} then {energy})"
        );
        let alone = f32_of(&host.call_export("energy_alone", &[]).expect("energy_alone"));
        assert_eq!(energy.to_bits(), alone.to_bits(), "{workers} workers: the reduction's order");
        let got = (host.hash(), energy);
        // The workers take part, once they've woken: on a busy machine the program's thread
        // can finish a whole job first, so a few more frames may pass before one helps.
        let mut more = 0;
        while workers > 1 && host.worker_chunks() == 0 && more < 2000 {
            host.frame(frame_time(FRAMES + more, 60.0), 64, 64).expect("frame");
            more += 1;
        }
        if workers > 1 {
            assert!(host.worker_chunks() > 0, "{workers} workers: the workers ran no chunks");
        } else {
            assert_eq!(host.worker_chunks(), 0, "no workers ran chunks");
        }
        match seen {
            None => seen = Some(got),
            Some(want) => assert_eq!(got, want, "{workers} workers"),
        }
    }
}

#[test]
fn a_panic_on_a_worker_is_the_programs() {
    let (dir, _) = page("compiler/tests/parallel", "parallel-panic");
    let build = CpuBuild::load(&dir).expect("load");
    // The first chunk and the last: the program's thread usually claims the first, a worker
    // often the last, but either way the result is the same.
    for at in [0, 5000, 9999i32] {
        for workers in WORKERS {
            let mut host = build.start_with(workers).expect("start");
            host.frame(0.0, 64, 64).expect("frame");
            let err = host.call_export("poison", &[Value::I32(at)]).expect_err("a panic");
            let msg = err.to_string();
            assert!(
                msg.contains("panic: a poisoned particle"),
                "{workers} workers, at {at}: {msg}"
            );
        }
    }
}

fn native(dir: &std::path::Path, workers: u32) -> String {
    let options = Options { workers, ..Options::default() };
    let mut host = Host::load_with(dir, &options).expect("load");
    let times: Vec<f32> = (0..FRAMES).map(|i| frame_time(i, 60.0)).collect();
    host.run_frames(&times, 16, 16).expect("run").hash_hex()
}

#[test]
#[ignore = "needs Chrome, python3 and a GPU"]
fn both_hosts_agree_with_any_workers() {
    let (dir, rel) = page("compiler/tests/parallel", "parallel-hosts");
    let want = native(&dir, 1);
    for workers in WORKERS {
        assert_eq!(native(&dir, workers), want, "the native host with {workers} workers");
        let run =
            wrela_tests::ChromeRun { workers, ..wrela_tests::ChromeRun::new(FRAMES, 16, 16, 60.0) };
        let chrome = wrela_tests::run_in_chrome_with(&rel, run);
        assert_eq!(chrome.hash, want, "Chrome with {workers} workers");
        // Chrome starts its workers while the program runs: they join in by the later frames.
        assert_eq!(chrome.worker_chunks > 0, workers > 1, "Chrome's workers ran chunks");
    }
}

/// Hosts started and dropped one after another, each with workers, then one more: a worker
/// that starts after its host shut down stops instead of waiting for good (it once waited on a
/// generation it read after the shutdown's bump).
#[test]
fn hosts_shut_down_their_workers_however_late_they_start() {
    let (dir, _) = page("compiler/tests/parallel", "parallel-shutdown");
    let build = CpuBuild::load(&dir).expect("load");
    for workers in [2, 3, 4, 8, 4, 2] {
        let mut host = build.start_with(workers).expect("start");
        host.frame(0.0, 4, 4).expect("frame");
    }
}
