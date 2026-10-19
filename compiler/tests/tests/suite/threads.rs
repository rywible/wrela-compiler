//! The program's threads (#43 §2–§3, AC6): tasks (`std::par::spawn`), parallel work started
//! inside parallel work, hand-offs (`std::handoff`) and per-thread allocation counts, natively
//! with 1, 2 and 8 threads (the program's own and its helpers). compiler/tests/jobs is the
//! program.

use crate::{a_flipped_record_fails_at_its_tick, built, replays_with_any_helpers};
use wrela_host::{CpuBuild, CpuHost, Value};
use wrela_tests::{one_u32, one_vec3};

fn start(workers: u32) -> CpuHost {
    CpuBuild::load(built("jobs")).expect("load").start_with(workers).expect("start")
}

/// A job's result is a function of its input alone, whichever thread runs it: twelve jobs at
/// once, and a job that starts and joins another, give the same with no helpers (every job runs
/// in its `join`), one, and seven. With one helper the inner job runs inside the outer's `join`,
/// so a job that joins another can't deadlock.
#[test]
fn jobs_give_the_same_results_with_any_helpers() {
    let results = |workers| {
        let mut host = start(workers);
        let many = one_u32(&mut host, "many", &[Value::I32(12), Value::I32(200_000)]);
        let nested = one_u32(&mut host, "nested_jobs", &[Value::I32(300_000)]);
        (many, nested)
    };
    let one = results(1);
    for workers in [2, 8] {
        assert_eq!(results(workers), one, "with {workers} threads");
    }
}

/// A job a program polls (`done`) is done before long, with no helpers too: then the first poll
/// runs it, since no helper would ever start it (a page on a two-core machine has none; a program
/// that polled its preview waited forever in test mode's one thread).
#[test]
fn a_polled_job_is_done_with_any_helpers() {
    const MOST: u32 = 2_000_000_000;
    let mut host = start(1);
    assert_eq!(one_u32(&mut host, "polled", &[Value::I32(200_000), Value::I32(MOST as i32)]), 0);
    for workers in [2, 8] {
        let mut host = start(workers);
        let polls = one_u32(&mut host, "polled", &[Value::I32(200_000), Value::I32(MOST as i32)]);
        assert!(polls < MOST, "{workers} threads: the job was never done");
    }
}

/// A trap in a job is the program's panic at its `join`, with the job's message, whichever
/// thread ran it: with no helpers, one or seven, and with each job's result held back (slowed)
/// so the joining thread waits for it.
#[test]
fn a_trapping_job_traps_at_its_join() {
    for (workers, hold) in [(1, 0), (2, 0), (8, 0), (2, 50_000), (8, 50_000)] {
        let mut host = start(workers);
        host.hold_jobs(hold);
        assert_eq!(one_u32(&mut host, "trap_at_join", &[Value::I32(4)]), 8);
        let e = host.call_export("trap_at_join", &[Value::I32(13)]).expect_err("13 traps");
        let msg = e.to_string();
        assert!(msg.contains("a job's input was 13"), "{workers} threads, held {hold}: {msg}");
        let failure = host.last_failure().expect("a trap");
        assert_eq!(failure.panic.as_deref(), Some("a job's input was 13"));
    }
}

#[test]
fn a_trapping_job_traps_when_dropped() {
    for (workers, hold) in [(1, 0), (2, 0), (8, 0), (2, 50_000), (8, 50_000)] {
        let mut host = start(workers);
        host.hold_jobs(hold);
        assert_eq!(one_u32(&mut host, "trap_at_drop", &[Value::I32(4)]), 8);
        let error = host.call_export("trap_at_drop", &[Value::I32(13)]).expect_err("13 traps");
        assert!(error.to_string().contains("a job's input was 13"));
        assert_eq!(
            host.last_failure().expect("a trap").panic.as_deref(),
            Some("a job's input was 13")
        );
    }
}

/// Shrink only the fixture's imported memory maximum, so memory.grow fails under the real
/// allocator lock after a few blocks. No production allocator or scheduling path is replaced.
fn small_heap() -> std::path::PathBuf {
    use wasm_encoder::reencode::Reencode;
    struct Heap;
    impl Reencode for Heap {
        type Error = std::convert::Infallible;
        fn memory_type(
            &mut self,
            ty: wasmparser::MemoryType,
        ) -> Result<wasm_encoder::MemoryType, wasm_encoder::reencode::Error<Self::Error>> {
            let mut ty = wasm_encoder::reencode::utils::memory_type(self, ty);
            ty.maximum = Some(ty.minimum + 64);
            Ok(ty)
        }
    }
    let dir = crate::scratch("jobs-small-heap");
    wrela_tests::copy_dir(&built("jobs"), &dir);
    let manifest = wrela_tests::manifest(&dir);
    let wasm = dir.join(&manifest.wasm);
    let bytes = std::fs::read(&wasm).unwrap();
    let mut module = wasm_encoder::Module::new();
    Heap.parse_core_module(&mut module, wasmparser::Parser::new(0), &bytes).unwrap();
    std::fs::write(wasm, module.finish()).unwrap();
    dir
}

/// OOM is reported at join, after releasing the allocator lock, and another thread can
/// continue allocating. One forced failure with 1, 2 and 8 workers replaces repeated GiB fills.
#[test]
fn an_allocation_failure_releases_the_lock_and_panics_at_join() {
    let build = CpuBuild::load(small_heap()).expect("load");
    for workers in [1, 2, 8] {
        let mut host = build.start_with(workers).expect("start");
        let error = host.call_export("fill_the_heap_while_allocating", &[]).expect_err("OOM");
        let failure = host.last_failure().expect("a trap");
        assert!(
            failure.panic.as_deref().is_some_and(|m| m.starts_with("out of memory")),
            "{error}"
        );
        // If the failing thread retained the lock, this allocation would deadlock.
        assert!(one_u32(&mut host, "allocations_while_helping", &[Value::I32(1)]) > 0);
    }
}

/// Parallel work started inside a parallel closure runs on that thread alone, chunk after
/// chunk: the results are the same with any helpers, and nothing deadlocks.
#[test]
fn parallel_work_inside_parallel_work_runs_inline() {
    let run = |workers| one_u32(&mut start(workers), "nested_parallel", &[Value::I32(64)]);
    let one = run(1);
    assert_eq!(run(2), one);
    assert_eq!(run(8), one);
}

/// Each thread counts its own allocations: what this thread counts making jobs is the same
/// whether helpers run them (and allocate) or it does.
#[test]
fn allocations_are_counted_per_thread() {
    let run =
        |workers| one_u32(&mut start(workers), "allocations_while_helping", &[Value::I32(16)]);
    let alone = run(1);
    assert!(alone > 0);
    assert_eq!(run(8), alone);
}

/// A hand-off is never read torn (AC6): a helper publishes as fast as it can while this thread
/// reads, in a debug build, where each read checks its value's checksum and that it's no older
/// than the last; and the program checks each pair it reads, and that it's the latest complete
/// (no older than one the helper had finished publishing before the read). 0 mismatches in 10⁶
/// reads.
#[test]
fn a_hand_off_is_never_read_torn() {
    let dir = crate::built_debug("jobs");
    let mut host = CpuBuild::load(&dir).expect("load").start_with(2).expect("start");
    let (values, reads) = (20_000, 100_000);
    let r = host
        .call_export("stress", &[Value::I32(values), Value::I32(reads)])
        .unwrap_or_else(|e| panic!("stress: {e}"));
    let [Value::F32(count), Value::F32(published)] = r[..] else { panic!("{r:?}") };
    println!("{count} reads while {values} values were published; the last read saw {published}");
    assert!(count >= reads as f32);
    assert_eq!(published, values as f32);
}

fn ticker(workers: u32) -> CpuHost {
    CpuBuild::load(built("ticker")).expect("load").start_with(workers).expect("start")
}

/// A ticker (`std::tick`) in lockstep with the frames (#43 §2.3): before frame i, a 60 Hz
/// ticker has run i + 1 ticks at 60 frames a second, so frame i reads tick i's snapshot through
/// the hand-off; a script's tick-keyed events reach their ticks as records, and its
/// frame-keyed ones reach both the frames and the next tick.
#[test]
fn a_ticker_takes_its_records_and_publishes_to_the_frames() {
    let script = wrela_host::parse_script(
        r#"[
            {"tick": 3, "type": "key", "key": "Space"},
            {"frame": 5, "type": "keydown", "key": "KeyA"},
            {"tick": 20, "type": "keydown", "key": "KeyB"}
        ]"#,
    )
    .expect("a script");
    let mut host = ticker(2);
    assert_eq!(host.ticker_hz(), Some(60));
    for i in 0..30 {
        host.lockstep_frame(i, 60.0, 64, 64, &script).expect("a frame");
    }
    let [tick, keys, _] = one_vec3(&mut host, "newest", &[]);
    assert_eq!((tick, keys), (29.0, 3.0));
    let e = host.call_export("start_again", &[]).expect_err("a second ticker");
    assert!(e.to_string().contains("a program starts one ticker"), "{e}");
}

/// Parallel work started from the ticker's thread and from the frames' at the same time gives
/// the same results as each alone (AC6): ticks on an OS thread of their own beside frames, with
/// seven helpers, give the same state hashes as ticks alone, and the frames the same work.
#[test]
fn parallel_work_from_ticks_and_frames_doesnt_collide() {
    let (ticks, frames) = (60, 60);
    let built = CpuBuild::load(built("ticker")).expect("load");
    let alone = built.record_ticks(ticks, &[], 1).expect("ticks alone");
    let alone: Vec<u64> = alone.ticks.iter().map(|t| t.hash).collect();
    let mut frames_alone = built.start_with(1).expect("start");
    for i in 0..frames {
        frames_alone.frame(wrela_host::frame_time(i, 60.0), 64, 64).expect("a frame");
    }
    let work = one_u32(&mut frames_alone, "work", &[]);
    for round in 0..2 {
        let mut host = built.start_with(8).expect("start");
        let ticked = host.ticks_beside_frames(ticks, frames, 60.0, 64, 64).expect("both");
        let hashes: Vec<u64> = ticked.iter().map(|(t, _)| t.hash.expect("asked for")).collect();
        assert_eq!(hashes, alone, "round {round}: the ticks' hashes");
        assert_eq!(one_u32(&mut host, "work", &[]), work, "round {round}");
    }
}

/// A tick log (runtime/abi `ticks`) replays to the same hashes (`wrela-host --replay`), with
/// any helpers, and a log with one record changed fails at that tick and names it (AC6).
#[test]
fn a_tick_log_replays_and_a_changed_record_fails_at_its_tick() {
    let script = wrela_host::parse_script(
        r#"[
            {"tick": 3, "type": "key", "key": "Space"},
            {"tick": 10, "type": "keydown", "key": "KeyA"},
            {"tick": 40, "type": "keyup", "key": "KeyA"}
        ]"#,
    )
    .expect("a script");
    let built = CpuBuild::load(built("ticker")).expect("load");
    let log = built.record_ticks(60, &script, 1).expect("record");
    let read = replays_with_any_helpers(&built, &log);
    // A key going down at tick 10 becomes one going up: tick 10's hash differs.
    a_flipped_record_fails_at_its_tick(&built, &read, 10);
    let mut other = read.clone();
    other.wasm_hash ^= 1;
    assert!(matches!(built.replay(&other, 1), Err(wrela_host::ReplayError::OtherBuild { .. })));
}

/// The ticker in Chrome (runtime/browser's ticker.ts, its own worker): on test mode's lockstep
/// schedule at 30, 60 and 144 frames a second, with 1 and 4 threads, its ticks' state hashes
/// are the native host's, the frames see the same snapshots, and Chrome's tick log replays
/// natively (#43 §2.3, AC5, AC6). On the paced schedule (ticks on the ticker's own clock, the
/// frames not waiting) the hashes are the same for every tick it ran.
#[test]
#[ignore = "needs the runtime"]
fn the_ticker_in_chrome_agrees_with_the_native_host() {
    let (dir, rel) = wrela_tests::page("compiler/tests/ticker", "ticker-chrome");
    let text =
        std::fs::read_to_string(wrela_tests::repo_root().join("compiler/tests/ticker/keys.json"))
            .expect("the script");
    let script = wrela_host::parse_script(&text).expect("a script");
    let built = CpuBuild::load(&dir).expect("load");
    let native = built.record_ticks(300, &script, 2).expect("native ticks");
    let frames = 120;
    for (fps, workers, paced) in
        [(60.0, 4, false), (30.0, 1, false), (144.0, 4, false), (60.0, 4, true)]
    {
        // In lockstep, the frames run back to back (a frame's ticks follow its number, not the
        // clock); paced, the ticker keeps its own clock, so they run at their times.
        let run = wrela_tests::ChromeRun {
            workers,
            paced,
            saturate: !paced,
            script: Some(text.clone()),
            ..wrela_tests::ChromeRun::new(frames, 16, 16, fps)
        };
        let chrome = wrela_tests::run_in_chrome_with(&rel, run);
        let ticks = chrome.ticks.expect("the ticker's ticks");
        let what = format!("{fps} fps, {workers} threads{}", if paced { ", paced" } else { "" });
        assert_eq!(ticks.hz, 60, "{what}");
        let log = ticks.log.expect("a tick log");
        if !paced {
            let n = wrela_host::lockstep_ticks(frames - 1, 60, fps) as usize;
            assert_eq!(log.ticks.len(), n, "{what}: the ticks before the last frame");
        }
        assert!(log.ticks.len() > 30, "{what}: {} ticks", log.ticks.len());
        for (k, t) in log.ticks.iter().enumerate() {
            assert_eq!(t.hash, native.ticks[k].hash, "{what}: tick {k}");
        }
        assert_eq!(log.first, native.first, "{what}: the first world");
        assert_eq!(built.replay(&log, 2).expect("replays") as usize, log.ticks.len(), "{what}");
    }
}

/// Memory grown by one thread is usable by all helpers. A 4 MiB initialization overlaps helper
/// startup; one helper holds 12 MiB while parallel rows write beyond it, then the main thread
/// writes every page of 16 MiB. Both hosts must report the same contents.
#[test]
#[ignore = "needs the runtime"]
fn memory_a_helper_grew_is_every_threads() {
    let (dir, rel) = wrela_tests::page("compiler/tests/grown", "grown-chrome");
    let native = {
        let mut host = wrela_host::Host::load(&dir).expect("load");
        let (mut lines, began) = (Vec::new(), std::time::Instant::now());
        for i in 0.. {
            assert!(began.elapsed().as_secs() < 60, "natively, the helper never finished");
            host.frame(i as f32 / 60.0, 16, 16).expect("a frame");
            lines.extend(host.take_logs());
            if lines.iter().any(|l| l.starts_with("used")) {
                break;
            }
        }
        lines.into_iter().find(|l| l.starts_with("used")).expect("natively, the block's used")
    };
    let run =
        wrela_tests::ChromeRun { workers: 8, ..wrela_tests::ChromeRun::new(120, 16, 16, 60.0) };
    let _ = wrela_tests::run_in_chrome_with(&rel, run);
    let log = std::fs::read_to_string(dir.join("results/log.txt")).expect("log.txt");
    let used = log.lines().find(|l| l.starts_with("used")).unwrap_or_default();
    assert_eq!(used, native, "in Chrome, the block carved from what a helper grew: {log}");
}
