//! The program's threads (#43 §2–§3, AC6): tasks (`std::par::spawn`), parallel work started
//! inside parallel work, hand-offs (`std::handoff`) and per-thread allocation counts, natively
//! with 1, 2 and 8 threads (the program's own and its helpers). compiler/tests/jobs is the
//! program.

use crate::{a_flipped_record_fails_at_its_tick, built, replays_with_any_helpers};
use wrela_host::{CpuBuild, CpuHost, Value};
use wrela_tests::{one_u32, one_vec3, sized};

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
/// runs it, since no helper would ever start it (a page on a two-core machine has none; the map
/// lens, which polls its preview, waited forever in test mode's one thread).
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

/// A job that fills the heap panics at its join with its own message, every time, while the
/// joining thread makes and drops blocks of its own (M6, AC15). Before the allocator let its lock
/// go to panic, the job's thread ran out of memory holding the lock, and the joining thread spun
/// on it until its count of tries overflowed, 66 s later, with a trap that hid the job's panic
/// (spike 17's bake, 2 runs of 11). 100 runs, two threads each, four at a time.
#[test]
#[ignore = "long: fills a gigabyte of heap 100 times"]
fn a_job_that_fills_the_heap_panics_at_its_join() {
    let build = CpuBuild::load(built("jobs")).expect("load");
    let runs = sized(100, 1000);
    let next = std::sync::atomic::AtomicU32::new(0);
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                while next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < runs {
                    let t = std::time::Instant::now();
                    let mut host = build.start_with(2).expect("start");
                    let e = host
                        .call_export("fill_the_heap_while_allocating", &[])
                        .expect_err("the heap runs out");
                    let failure = host.last_failure().expect("a trap");
                    let took = t.elapsed().as_secs_f64();
                    assert!(
                        failure.panic.as_deref().is_some_and(|m| m.starts_with("out of memory")),
                        "the run failed otherwise, after {took:.1} s: {e}"
                    );
                    assert!(took < 20.0, "a run took {took:.1} s");
                }
            });
        }
    });
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
    let (values, reads) = (sized(20_000, 1_000_000), sized(100_000, 1_000_000));
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
    for round in 0..sized(20, 1000) {
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
#[ignore = "long: needs Chrome, python3 and a GPU"]
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

/// A job that traps does so at the tick that takes it (AC6): `engine::run`'s world asks at tick
/// 2 for a job due at tick 10, and at tick 5 for one due at tick 20 whose work panics; the ticks
/// before 20 run, and tick 20 traps with the job's message. With no helpers, one or seven, and
/// with jobs slowed (each result held 100 ms, so tick 10 waits for its job: it ends no sooner
/// than 100 ms after tick 2 asked for it, however long the ticks between took; a machine loaded
/// by other work slowed them, and a bound on tick 10's own wait failed). The ticks are 2 ms
/// apart, so a helper takes each job before its due tick does.
#[test]
fn a_trapping_job_traps_at_the_tick_that_takes_it() {
    let dir = super::engine_package("threads-due-trap", "due", DUE_TRAP);
    wrela_tests::must_build(&dir, &dir);
    let build = CpuBuild::load(&dir).expect("load");
    for (workers, hold) in [(1, 0), (2, 0), (8, 0), (2, 100_000), (8, 100_000)] {
        let mut host = build.instantiate_in(workers, None).expect("instantiate");
        host.hold_jobs(hold);
        host.init().expect("init");
        let mut asked = std::time::Instant::now();
        for k in 0..20 {
            if k == 2 {
                asked = std::time::Instant::now();
            }
            host.tick().unwrap_or_else(|e| panic!("{workers} threads, held {hold}: tick {k}: {e}"));
            if k == 10 && hold > 0 {
                let since = asked.elapsed().as_secs_f64() * 1000.0;
                assert!(
                    since >= 95.0,
                    "{workers} threads: tick 10 ended {since:.1} ms after the ask"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let e = host.tick().expect_err("tick 20 takes the trapping job");
        let msg = e.to_string();
        assert!(
            msg.contains("in tick 20") && msg.contains("a job's input was 13"),
            "{workers} threads, held {hold}: {msg}"
        );
    }
}

const DUE_TRAP: &str = "use engine::run::{Asks, Stamped, TickInput, run}
use engine::sim::{Sim, Tick}
use std::handoff::Latest

/// The ticks run and the answers' sum.
pub struct Count: Sim {
    pub ticks: u32,
    pub sum: u32,
}

impl Asks for Count {
    type Ask = u32
    type Answer = u32

    /// At tick 2, a job due at tick 10; at tick 5, one due at tick 20 that panics.
    @deterministic
    fn asks(self, tick: Tick) -> Vec<(u32, Tick)> {
        var out: Vec<(u32, Tick)> = Vec::new()
        if tick == 2 {
            out.push((4, 10))
        }
        if tick == 5 {
            out.push((13, 20))
        }
        out
    }

    @deterministic
    fn work(ask: u32) -> u32 {
        assert(ask != 13, f\"a job's input was {ask}\")
        ask * 2
    }
}

@deterministic
fn step(c: mut Count, input: TickInput<Count>) {
    c.ticks += 1
    for a in input.answers {
        c.sum += a.answer
    }
}

@deterministic
fn snapshot(c: Count) -> u32 {
    c.sum
}

pub struct Game {
    latest: Latest<Stamped<u32>>,
}

pub fn init() -> Game {
    Game { latest: run(Count { ticks: 0, sum: 0 }, hz: 60, step: step, present: snapshot) }
}

pub fn frame(game: mut Game, time: f32, width: u32, height: u32) {}
";

/// Memory one thread grew is every thread's to use (compiler/tests/grown), in Chrome with 8
/// threads and natively: `init` grows the heap in 64 steps while the host starts the helpers; a
/// task grows and frees 192 MiB while the others sleep, then wakes them to a parallel job whose
/// chunks grow vectors from that memory; the program's thread grows the heap each frame
/// meanwhile, then writes a block of 160 MiB. In Chrome a helper that started while the memory
/// grew saw an old size, and trapped on blocks beyond it (std's `alloc` line 127, as the map
/// lens did), until std asked for the memory's size wherever a thread takes memory another may
/// have grown (`std::alloc::reach_heap`).
#[test]
#[ignore = "long: needs Chrome, python3 and a GPU"]
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
        wrela_tests::ChromeRun { workers: 8, ..wrela_tests::ChromeRun::new(1200, 16, 16, 60.0) };
    let _ = wrela_tests::run_in_chrome_with(&rel, run);
    let log = std::fs::read_to_string(dir.join("results/log.txt")).expect("log.txt");
    let used = log.lines().find(|l| l.starts_with("used")).unwrap_or_default();
    assert_eq!(used, native, "in Chrome, the block carved from what a helper grew: {log}");
}
