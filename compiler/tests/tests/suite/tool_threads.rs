//! A tool's helpers (language.md §15.5, §16.1): a tool build's memory is shared, and the tool
//! host runs `std::par`'s work on helper threads beside the tool's own, as the native host does
//! a client build's. The tests check that a parallel job's result is the same with 0, 1, 2 and
//! 7 helpers (and that the helpers ran its chunks), that a helper's trap ends the run with the
//! helper's message, whatever the tool's thread does then, that the timeout and an interrupt stop
//! a parallel job within the grace however its threads are busy, and that the run's end leaves
//! no helper. Tool tasks (`std::tool::spawn`) wait on calls on the helpers: the tests check that
//! calls made before a wait run at once, that `any` gives the first answered and leaves the
//! others, and that the timeout and an interrupt stop a thread blocked in a wait, in `any`, in a
//! task's join or in compute, each with its status and nothing left running.

use crate::tool::{printer, run, tool_build};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use wrela_host::tool::{End, Ended, Options, ToolBuild};

/// A CPU-heavy job: each of 40,000 numbers stepped 20,000 times, folded with a `reduce` that
/// depends on the order it folds in, and summed as floats.
const HEAVY: &str = r#"
use std::io::print

/// `i` stepped `n` times: about `n` multiplies and adds.
fn work(i: u32, n: u32) -> u32 {
    var x = i
    for _ in 0..n {
        x = x.wrapping_mul(1664525).wrapping_add(1013904223)
    }
    x
}

fn real(i: u32) -> f64 {
    f64(work(i, 50) % 1000) * 0.001
}

pub fn tool() -> u32 {
    var xs: Vec<u32> = Vec::new()
    for i in 0..40000 {
        xs.push(i)
    }
    let folded = xs.par_map_reduce(u32(7), |i| work(i, 20000), |a, b| a.wrapping_mul(31).wrapping_add(b))
    let summed = xs.par_map_reduce(0.0, |i| real(i), |a, b| a + b)
    print(f"folded {folded}, summed {summed}")
    0
}
"#;

#[test]
fn a_parallel_jobs_result_is_the_same_with_any_number_of_helpers_and_they_help() {
    let build = tool_build("threads-heavy", HEAVY);
    let mut seen: Option<Vec<String>> = None;
    for workers in [0, 1, 2, 7] {
        let (lines, mut options) = printer(|_| {});
        options.workers = workers;
        let started = Instant::now();
        let ended = run(&build, options);
        let took = started.elapsed();
        assert_eq!(ended.end, End::Exit(0), "{workers} helpers");
        // Told, not asserted: the speed with more helpers is reported, not tested.
        eprintln!("{workers} helpers: {took:?}, {} chunks helped", ended.helped);
        if workers == 0 {
            assert_eq!(ended.helped, 0, "with no helpers, the tool's thread runs every chunk");
        } else {
            assert!(ended.helped > 0, "{workers} helpers ran none of the chunks");
        }
        let printed = lines.lock().expect("the lines").clone();
        match &seen {
            None => seen = Some(printed),
            Some(first) => assert_eq!(&printed, first, "{workers} helpers"),
        }
    }
}

/// A task that prints and panics, which a helper runs while the tool waits; then the tool joins
/// it. With an argument, the tool waits until the task is done (polling `done`, which runs the
/// task itself only when there are no helpers), at most 10 s; without, it waits 0.3 s on a
/// timer, so with no helpers (or none started yet) the join runs the task.
const TASK_TRAPS: &str = r#"
use std::io::print
use std::par::spawn
use std::tool::time::after

fn breaks(n: take u32) -> u32 {
    print("the task runs")
    if n > 0 {
        panic("a helper's task broke")
    }
    n
}

pub fn tool() -> u32 {
    let task = spawn(u32(3), breaks)
    if std::tool::args().len() > 0 {
        var polls = 0
        while !task.done() && polls < 1000 {
            let _ = after(0.01).wait()
            polls += 1
        }
    } else {
        let _ = after(0.3).wait()
    }
    print("the tool joins it")
    take task.join()
}
"#;

#[test]
fn a_helpers_trap_ends_the_run_with_its_message() {
    let build = tool_build("threads-trap", TASK_TRAPS);
    let helper: &[&str] = &["the task runs"];
    let join_first: &[&str] = &["the tool joins it", "the task runs"];
    let slow = Duration::from_millis(500);
    for (workers, until_done, delay, order) in [
        // A helper ran the task while the tool waited on a timer: its trap ended the run there.
        (2, true, Duration::ZERO, helper),
        // So it does when the helpers start past the 0.3 s wait.
        (2, true, slow, helper),
        // Where a fixed wait would have joined first, and run the task itself.
        (2, false, slow, join_first),
        // With no helpers, the join runs it on the tool's thread.
        (0, false, Duration::ZERO, join_first),
    ] {
        let (lines, mut options) = printer(|_| {});
        options.workers = workers;
        options.testing.helper_delay = delay;
        if until_done {
            options.args = vec!["until-done".into()];
        }
        let ended = run(&build, options);
        let case = format!("{workers} helpers, until done: {until_done}, {delay:?} late");
        assert_eq!(ended.end, End::Trap("a helper's task broke".into()), "{case}");
        assert_eq!(ended.end.status(), 70);
        assert_eq!(*lines.lock().expect("the lines"), order, "{case}");
    }
}

/// A parallel job whose chunks never end, but the first's: the tool's thread spins in one, or
/// waits for the helpers that do.
const SPINS: &str = r#"
fn spin(i: u32) -> u32 {
    if i < 64 {
        return i
    }
    var n: u32 = 0
    loop {
        n = n.wrapping_add(1)
    }
}

pub fn tool() -> u32 {
    var xs: Vec<u32> = Vec::new()
    for i in 0..512 {
        xs.push(i)
    }
    xs.par_map_reduce(u32(0), |i| spin(i), |a, b| a + b)
}
"#;

/// Runs `build` with `options`, stopped after 300 ms by its timeout, or by an interrupt if
/// `interrupted` (`run` checks no worker or helper is left): how it ended, how long it took, and
/// the grace it had.
fn stopped(
    build: &ToolBuild,
    mut options: Options,
    interrupted: bool,
) -> (Ended, Duration, Duration) {
    let flag = Arc::new(AtomicBool::new(false));
    if interrupted {
        options.interrupt = Some(flag.clone());
    } else {
        options.timeout = Some(Duration::from_millis(300));
    }
    let grace = options.testing.grace;
    let set = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        flag.store(true, Ordering::Release);
    });
    let started = Instant::now();
    let ended = run(build, options);
    let took = started.elapsed();
    set.join().expect("the interrupting thread");
    (ended, took, grace)
}

#[test]
fn the_timeout_and_an_interrupt_stop_a_parallel_job_within_the_grace() {
    let build = tool_build("threads-spin", SPINS);
    for workers in [0, 2, 7] {
        for interrupted in [false, true] {
            let (_, mut options) = printer(|_| {});
            options.workers = workers;
            let (ended, took, grace) = stopped(&build, options, interrupted);
            let want = if interrupted { End::Interrupted } else { End::Timeout };
            assert_eq!(ended.end, want, "{workers} helpers");
            assert!(took < Duration::from_millis(300) + grace, "{workers} helpers: {took:?}");
        }
    }
}

/// Calls made before a wait: two timers of 1 s, then a read; `any` of a slow and a fast timer;
/// each prints how long it took on the system's clock.
const CALLS: &str = r#"
use std::io::print
use std::tool::any
use std::tool::files
use std::tool::time::after

pub fn tool() -> u32 {
    let started = std::time::now()
    let a = after(1.0)
    let b = after(1.0)
    let _ = take a.wait()
    let _ = take b.wait()
    let both = std::time::since(started)
    print(f"both timers {both < 1.8}")
    let Ok(bytes) = files::read("in/a.txt").wait() else { return 1 }
    print(f"read {bytes.len()} bytes")
    let slow = after(0.5)
    let fast = after(0.05)
    let at = std::time::now()
    let first = any([slow.request(), fast.request()])
    print(f"first {first}, before the slow one {std::time::since(at) < 0.5}")
    // The fast one's answer is still there, and the slow one still runs.
    let quick = take fast.wait()
    print(f"fast {quick.is_ok()}")
    let late = take slow.wait()
    print(f"slow {late.is_ok()} after {std::time::since(at) >= 0.5}")
    0
}
"#;

#[test]
fn calls_made_before_a_wait_run_at_once_and_any_gives_the_first_answered() {
    let build = tool_build("threads-calls", CALLS);
    let root = crate::tool::scratch("threads-calls");
    std::fs::write(root.join("a.txt"), "abc").expect("write");
    let (lines, mut options) = printer(|_| {});
    options.caps = wrela_host::tool::Caps::open(
        &wrela_host::tool::Grants::default().root("in", &root, false),
        |_| None,
    )
    .expect("granted");
    let ended = run(&build, options);
    let lines = lines.lock().expect("the lines").clone();
    assert_eq!(ended.end, End::Exit(0), "{lines:?}");
    assert_eq!(
        lines,
        [
            "both timers true",
            "read 3 bytes",
            "first 1, before the slow one true",
            "fast true",
            "slow true after true",
        ]
    );
}

/// A tool whose thread blocks, as its argument says: in a wait (`wait`), in `any`, in a task's
/// join while the task waits (`join`), or in compute (`spin`); or whose task, on a helper,
/// computes while the tool's thread waits for it (`task-spin`).
const BLOCKS: &str = r#"
use std::tool::{any, args, spawn}
use std::tool::time::after

fn sleeps(s: take f64) -> u32 {
    let _ = after(s).wait()
    0
}

fn spins(n: take u32) -> u32 {
    var x = n
    loop {
        x = x.wrapping_add(1)
    }
}

pub fn tool() -> u32 {
    let given = args()
    let how = given[0].as_str()
    if how == "wait" {
        let _ = after(3600.0).wait()
    } else if how == "any" {
        let a = after(3600.0)
        let b = after(7200.0)
        let _ = any([a.request(), b.request()])
    } else if how == "join" {
        let _ = spawn(3600.0, sleeps).join()
    } else if how == "task-spin" {
        let _ = spawn(u32(1), spins).join()
    } else {
        let _ = spins(0)
    }
    0
}
"#;

#[test]
fn the_timeout_and_an_interrupt_stop_a_thread_blocked_anywhere() {
    let build = tool_build("threads-blocks", BLOCKS);
    for how in ["wait", "any", "join", "spin", "task-spin"] {
        for workers in [0, 2] {
            for interrupted in [false, true] {
                let (_, mut options) = printer(|_| {});
                options.workers = workers;
                options.args = vec![how.into()];
                let (ended, took, grace) = stopped(&build, options, interrupted);
                let case = format!("{how}, {workers} helpers, interrupted: {interrupted}");
                let (want, status) =
                    if interrupted { (End::Interrupted, 130) } else { (End::Timeout, 124) };
                assert_eq!(ended.end, want, "{case}");
                assert_eq!(ended.end.status(), status, "{case}");
                assert!(took < Duration::from_millis(300) + grace, "{case}: {took:?}");
            }
        }
    }
}

/// A tool task that waits a moment, then panics, on a helper, while the tool's thread waits an
/// hour.
const TASK_PANICS: &str = r#"
use std::tool::spawn
use std::tool::time::after

fn breaks(s: take f64) -> u32 {
    let _ = after(s).wait()
    panic("a tool task broke")
}

pub fn tool() -> u32 {
    let task = spawn(0.05, breaks)
    let _ = after(3600.0).wait()
    take task.join()
}
"#;

#[test]
fn a_tool_tasks_trap_on_a_helper_ends_the_run_with_its_message() {
    let build = tool_build("threads-task-panics", TASK_PANICS);
    let (_, mut options) = printer(|_| {});
    options.workers = 2;
    let started = Instant::now();
    let ended = run(&build, options);
    assert_eq!(ended.end, End::Trap("a tool task broke".into()));
    assert_eq!(ended.end.status(), 70);
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn a_dropped_call_releases_what_it_made_by_the_end_of_the_run() {
    let main = r#"
use std::tool::blob
use std::tool::time::after

pub fn tool() -> u32 {
    {
        // Made, and never waited on.
        let _ = blob::new()
    }
    let _ = after(0.1).wait()
    0
}
"#;
    let build = tool_build("threads-dropped-call", main);
    let (_, options) = printer(|_| {});
    let ended = run(&build, options);
    assert_eq!(ended.end, End::Exit(0));
    assert_eq!(ended.blobs_released, (1, 0), "the dropped call's blob, released as it ended");
}

#[test]
fn a_reason_to_stop_that_came_first_wins_over_a_tool_that_returns() {
    let build = tool_build("threads-returns", "pub fn tool() -> u32 {\n    0\n}\n");
    // An interrupt given before the tool runs.
    let (_, mut options) = printer(|_| {});
    options.interrupt = Some(Arc::new(AtomicBool::new(true)));
    let ended = run(&build, options);
    assert_eq!((ended.end.clone(), ended.end.status()), (End::Interrupted, 130));
    // A budget spent before it runs (while it compiled).
    let (_, mut options) = printer(|_| {});
    options.timeout = Some(Duration::from_nanos(1));
    let ended = run(&build, options);
    assert_eq!((ended.end.clone(), ended.end.status()), (End::Timeout, 124));
    // And without either, its status.
    let (_, options) = printer(|_| {});
    assert_eq!(run(&build, options).end, End::Exit(0));
}

/// A task that waits for the first of one request, a copy of the tool's own call's, which the
/// tool's thread takes the answer of meanwhile.
const TAKEN: &str = r#"
use std::tool::{Request, any, spawn}
use std::tool::time::after

fn observe(r: take Request) -> u32 {
    any([r])
}

pub fn tool() -> u32 {
    let c = after(0.05)
    let t = spawn(c.request(), observe)
    let _ = take c.wait()
    take t.join()
}
"#;

#[test]
fn a_wait_on_a_request_whose_answer_another_thread_took_waits_on_nothing() {
    let build = tool_build("threads-taken", TAKEN);
    let (_, mut options) = printer(|_| {});
    options.workers = 1;
    // The task checks its request, then waits 0.5 s before it blocks: the tool's thread takes
    // the answer in between.
    options.testing.any_pause = Duration::from_millis(500);
    options.timeout = Some(Duration::from_secs(10));
    let started = Instant::now();
    let ended = run(&build, options);
    assert_eq!(ended.end, End::WaitsOnNothing);
    assert_eq!(ended.end.status(), 70);
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

#[test]
fn helpers_started_before_one_that_cant_start_are_shut_down_with_the_grace() {
    let main = r#"
fn twice(n: take u32) -> u32 {
    n * 2
}

pub fn tool() -> u32 {
    std::tool::spawn(u32(1), twice).join()
}
"#;
    let build = tool_build("threads-partial", main);
    // The second helper can't start, while the first is still waiting to: past a short grace,
    // the first is left, and counted.
    let (_, mut options) = printer(|_| {});
    options.workers = 2;
    options.testing.helper_delay = Duration::from_secs(1);
    options.testing.fail_helper = Some(1);
    options.testing.grace = Duration::from_millis(100);
    let ended = wrela_host::tool::run(&build, options);
    assert_eq!(ended.workers_left, 1, "{ended:?}");
    let End::Host(why) = &ended.end else { panic!("{:?}", ended.end) };
    assert!(why.contains("still running") && why.contains("can't start helper 1"), "{why}");
    // With a grace long enough for it, it returns, and the run says why it couldn't start.
    let (_, mut options) = printer(|_| {});
    options.workers = 2;
    options.testing.helper_delay = Duration::from_millis(300);
    options.testing.fail_helper = Some(1);
    options.testing.grace = Duration::from_secs(3);
    let ended = run(&build, options);
    let End::Host(why) = &ended.end else { panic!("{:?}", ended.end) };
    assert!(why.contains("can't start helper 1"), "{why}");
}
