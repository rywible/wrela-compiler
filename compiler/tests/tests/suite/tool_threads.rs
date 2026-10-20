//! A tool's helpers (language.md §15.5, §16.1): a tool build's memory is shared, and the tool
//! host runs `std::par`'s work on helper threads beside the tool's own, as the native host does
//! a client build's. The tests check that a parallel job's result is the same with 0, 1, 2 and
//! 7 helpers (and that the helpers ran its chunks), that a helper's trap ends the call that
//! waits for it with the helper's message, that the timeout and an interrupt stop a parallel job
//! within the grace however its threads are busy, and that the run's end leaves no helper.

use crate::tool::{ENTRIES, printer, run, tool_build};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use wrela_host::tool::End;

/// A CPU-heavy job: each of 40,000 numbers stepped 20,000 times, folded with a `reduce` that
/// depends on the order it folds in, and summed as floats.
const HEAVY: &str = r#"
use std::io::print
use std::job::{Job, Step}
use std::tool::RUNNING

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

@job
fn main() -> u32 {
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
    let build = tool_build("threads-heavy", &format!("{HEAVY}{ENTRIES}"));
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
/// timer, so with no helpers the join runs the task.
const TASK_TRAPS: &str = r#"
use std::io::print
use std::job::{Job, Step}
use std::par::spawn
use std::tool::RUNNING
use std::tool::time::after

fn breaks(n: take u32) -> u32 {
    print("the task runs")
    if n > 0 {
        panic("a helper's task broke")
    }
    n
}

@job
fn main(until_done: take bool) -> u32 {
    let task = spawn(u32(3), breaks)
    if until_done {
        var polls = 0
        while !task.done() && polls < 1000 {
            var t = after(0.01)
            while t.poll().is_none() { yield }
            polls += 1
        }
    } else {
        var t = after(0.3)
        while t.poll().is_none() { yield }
    }
    print("the tool joins it")
    take task.join()
}

pub fn tool_init() -> Job<main> {
    main.start(std::tool::args().len() > 0)
}

pub fn tool(job: mut Job<main>) -> u32 {
    match job.resume() {
        Step::Yielded => RUNNING,
        Step::Done(status) => status,
    }
}
"#;

#[test]
fn a_helpers_trap_ends_the_call_that_waits_for_it_with_its_message() {
    let build = tool_build("threads-trap", TASK_TRAPS);
    let helper_first = ["the task runs", "the tool joins it"];
    let join_first = ["the tool joins it", "the task runs"];
    let slow = Duration::from_millis(500);
    for (workers, until_done, delay, order) in [
        // A helper ran the task while the tool waited for it: it printed first.
        (2, true, Duration::ZERO, helper_first),
        // So it does when the helpers start past the 0.3 s wait.
        (2, true, slow, helper_first),
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
use std::job::{Job, Step}
use std::tool::RUNNING

fn spin(i: u32) -> u32 {
    if i < 64 {
        return i
    }
    var n: u32 = 0
    loop {
        n = n.wrapping_add(1)
    }
}

@job
fn main() -> u32 {
    var xs: Vec<u32> = Vec::new()
    for i in 0..512 {
        xs.push(i)
    }
    xs.par_map_reduce(u32(0), |i| spin(i), |a, b| a + b)
}
"#;

#[test]
fn the_timeout_and_an_interrupt_stop_a_parallel_job_within_the_grace() {
    let build = tool_build("threads-spin", &format!("{SPINS}{ENTRIES}"));
    for workers in [0, 2, 7] {
        for interrupted in [false, true] {
            let (_, mut options) = printer(|_| {});
            options.workers = workers;
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
            let ended = run(&build, options);
            let took = started.elapsed();
            set.join().expect("the interrupting thread");
            let want = if interrupted { End::Interrupted } else { End::Timeout };
            assert_eq!(ended.end, want, "{workers} helpers");
            assert!(took < Duration::from_millis(300) + grace, "{workers} helpers: {took:?}");
        }
    }
}
