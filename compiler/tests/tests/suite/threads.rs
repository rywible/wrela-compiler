//! The program's threads (#43 §2–§3, AC6): long jobs (`std::par::job`), parallel work started
//! inside parallel work, hand-offs (`std::handoff`) and per-thread allocation counts, natively
//! with 1, 2 and 8 threads (the program's own and its helpers). compiler/tests/jobs is the
//! program.

use crate::built;
use wrela_host::{CpuBuild, CpuHost, Value};
use wrela_tests::sized;

fn start(workers: u32) -> CpuHost {
    CpuBuild::load(built("jobs")).expect("load").start_with(workers).expect("start")
}

fn u32_of(v: &[Value]) -> u32 {
    match v {
        [Value::I32(x)] => *x as u32,
        other => panic!("expected a u32, got {other:?}"),
    }
}

fn call(host: &mut CpuHost, name: &str, args: &[Value]) -> u32 {
    u32_of(&host.call_export(name, args).unwrap_or_else(|e| panic!("{name}: {e}")))
}

/// A job's result is a function of its input alone, whichever thread runs it: twelve jobs at
/// once, and a job that starts and joins another, give the same with no helpers (every job runs
/// in its `join`), one, and seven. With one helper the inner job runs inside the outer's `join`,
/// so a job that joins another can't deadlock.
#[test]
fn jobs_give_the_same_results_with_any_helpers() {
    let results = |workers| {
        let mut host = start(workers);
        let many = call(&mut host, "many", &[Value::I32(12), Value::I32(200_000)]);
        let nested = call(&mut host, "nested_jobs", &[Value::I32(300_000)]);
        (many, nested, host.worker_chunks())
    };
    let (many, nested, _) = results(1);
    for workers in [2, 8] {
        let (m, n, _) = results(workers);
        assert_eq!((m, n), (many, nested), "with {workers} threads");
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
        assert_eq!(call(&mut host, "trap_at_join", &[Value::I32(4)]), 8);
        let e = host.call_export("trap_at_join", &[Value::I32(13)]).expect_err("13 traps");
        let msg = e.to_string();
        assert!(msg.contains("a job's input was 13"), "{workers} threads, held {hold}: {msg}");
        let failure = host.last_failure().expect("a trap");
        assert_eq!(failure.panic.as_deref(), Some("a job's input was 13"));
    }
}

/// Parallel work started inside a parallel closure runs on that thread alone, chunk after
/// chunk: the results are the same with any helpers, and nothing deadlocks.
#[test]
fn parallel_work_inside_parallel_work_runs_inline() {
    let run = |workers| call(&mut start(workers), "nested_parallel", &[Value::I32(64)]);
    let one = run(1);
    assert_eq!(run(2), one);
    assert_eq!(run(8), one);
}

/// Each thread counts its own allocations: what this thread counts making jobs is the same
/// whether helpers run them (and allocate) or it does.
#[test]
fn allocations_are_counted_per_thread() {
    let run = |workers| call(&mut start(workers), "allocations_while_helping", &[Value::I32(16)]);
    let alone = run(1);
    assert!(alone > 0);
    assert_eq!(run(8), alone);
}

/// A hand-off is never read torn (AC6): a helper publishes as fast as it can while this thread
/// reads, in a debug build, where each read checks its value's checksum and that it's no older
/// than the last; and the program checks each pair it reads. 0 mismatches in 10⁶ reads.
#[test]
fn a_hand_off_is_never_read_torn() {
    let dir = crate::scratch("threads-handoff");
    let built = wrela_driver::build_debug(&wrela_tests::repo_root().join("compiler/tests/jobs"));
    assert!(!built.has_errors(), "{:?}", built.diagnostics);
    built.write_to(&dir).expect("write");
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

fn vec3_of(v: &[Value]) -> [f32; 3] {
    match v {
        [Value::F32(a), Value::F32(b), Value::F32(c)] => [*a, *b, *c],
        other => panic!("expected a vec3, got {other:?}"),
    }
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
    let [tick, keys, _] = vec3_of(&host.call_export("newest", &[]).expect("newest"));
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
    let mut frames_alone = ticker(1);
    for i in 0..frames {
        frames_alone.frame(wrela_host::frame_time(i, 60.0), 64, 64).expect("a frame");
    }
    let work = u32_of(&frames_alone.call_export("work", &[]).expect("work"));
    for round in 0..sized(20, 1000) {
        let mut host = ticker(8);
        let hashes = host.ticks_beside_frames(ticks, frames, 60.0, 64, 64).expect("both");
        assert_eq!(hashes, alone, "round {round}: the ticks' hashes");
        assert_eq!(u32_of(&host.call_export("work", &[]).expect("work")), work, "round {round}");
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
    let bytes = log.encode();
    let read = wrela_host::TickLog::decode(&bytes).expect("reads back");
    for workers in [1, 2, 8] {
        assert_eq!(built.replay(&read, workers).expect("replays"), 60);
    }
    // A key going down at tick 10 becomes one going up: tick 10's hash differs.
    let mut changed = read.clone();
    changed.ticks[10].records[0][0] = wrela_abi::input::EventKind::KeyUp as u8;
    match built.replay(&changed, 2) {
        Err(wrela_host::ReplayError::Tick { tick, .. }) => assert_eq!(tick, 10),
        other => panic!("expected tick 10 to differ: {other:?}"),
    }
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
#[ignore = "needs Chrome, python3 and a GPU"]
fn the_ticker_in_chrome_agrees_with_the_native_host() {
    let (dir, rel) = wrela_tests::page("compiler/tests/ticker", "ticker-chrome");
    std::fs::copy(
        wrela_tests::repo_root().join("compiler/tests/ticker/keys.json"),
        dir.join("keys.json"),
    )
    .expect("copy the script");
    let text = std::fs::read_to_string(dir.join("keys.json")).expect("the script");
    let script = wrela_host::parse_script(&text).expect("a script");
    let built = CpuBuild::load(&dir).expect("load");
    let native = built.record_ticks(300, &script, 2).expect("native ticks");
    let hex = |h: u64| format!("{h:016x}");
    let frames = 120;
    for (fps, workers, paced) in
        [(60.0, 4, false), (30.0, 1, false), (144.0, 4, false), (60.0, 4, true)]
    {
        let run = wrela_tests::ChromeRun {
            workers,
            paced,
            input: "keys.json".into(),
            ..wrela_tests::ChromeRun::new(frames, 16, 16, fps)
        };
        let chrome = wrela_tests::run_in_chrome_with(&rel, run);
        let ticks = chrome.ticks.expect("the ticker's ticks");
        let what = format!("{fps} fps, {workers} threads{}", if paced { ", paced" } else { "" });
        assert_eq!(ticks.hz, 60, "{what}");
        if !paced {
            let n = wrela_host::lockstep_ticks(frames - 1, 60, fps) as usize;
            assert_eq!(ticks.hashes.len(), n, "{what}: the ticks before the last frame");
        }
        assert!(ticks.hashes.len() > 30, "{what}: {} ticks", ticks.hashes.len());
        for (k, h) in ticks.hashes.iter().enumerate() {
            assert_eq!(*h, hex(native.ticks[k].hash), "{what}: tick {k}");
        }
        let log = wrela_host::TickLog::decode(&ticks.log.expect("a tick log")).expect("reads");
        assert_eq!(log.first, native.first, "{what}: the first world");
        assert_eq!(built.replay(&log, 2).expect("replays") as usize, ticks.hashes.len(), "{what}");
    }
}

/// A job that traps does so at the tick that takes it (AC6): `engine::run`'s world asks at tick
/// 2 for a job due at tick 10, and at tick 5 for one due at tick 20 whose work panics; the ticks
/// before 20 run, and tick 20 traps with the job's message. With no helpers, one or seven, and
/// with jobs slowed (each result held 100 ms, so tick 10 waits for its job). The ticks are 2 ms
/// apart, so a helper takes each job before its due tick does.
#[test]
fn a_trapping_job_traps_at_the_tick_that_takes_it() {
    let dir = super::engine_package("threads-due-trap", "due", DUE_TRAP);
    let built = wrela_driver::build(&dir);
    assert!(!built.has_errors(), "{:?}", built.diagnostics);
    built.write_to(&dir).expect("write");
    let build = CpuBuild::load(&dir).expect("load");
    for (workers, hold) in [(1, 0), (2, 0), (8, 0), (2, 100_000), (8, 100_000)] {
        let mut host = build.instantiate_in(workers, None).expect("instantiate");
        host.hold_jobs(hold);
        host.init().expect("init");
        for k in 0..20 {
            let t = std::time::Instant::now();
            host.tick(false)
                .unwrap_or_else(|e| panic!("{workers} threads, held {hold}: tick {k}: {e}"));
            if k == 10 && hold > 0 {
                let waited = t.elapsed().as_secs_f64() * 1000.0;
                assert!(waited > 50.0, "{workers} threads: tick 10 took {waited:.1} ms");
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let e = host.tick(false).expect_err("tick 20 takes the trapping job");
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
