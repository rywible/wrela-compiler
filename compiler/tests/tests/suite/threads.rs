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
