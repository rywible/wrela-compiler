//! The typed programs' soak: one oracle over many programs, not in the gate.
//!
//! ```text
//! cargo run --release -p wrela-tests --bin typed-soak -- <oracle> <count> [--seed N] [--threads N] [--wild]
//! ```
//!
//! Each failure is shrunk and written to `target/typed/<oracle>-<seed>.wrela`; the run prints
//! each and a summary, and exits 1 if there was one. `--wild` runs O1 over wild programs only
//! (O1 otherwise sees both modes). `typed-soak --file <path>` runs one program file (its
//! header names its oracle and seed). `--only <seed>,<seed>...` runs those programs' seeds
//! (as the files' headers give them) instead of a count. `--no-shrink` reports each failure as it
//! is; else each is shrunk, keeping its kind (`oracles::class`). `--stop-after N` (3 unless said)
//! stops the run after N failures: a soak with failures has done its job. Progress every 25.

use std::sync::Mutex;
use wrela_tests::typed::Mode;
use wrela_tests::typed::oracles::{self, Oracle};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--file <path>`: one program, its oracle and seed from its header (a regression file).
    if let [flag, path] = args.as_slice()
        && flag == "--file"
    {
        let root = wrela_tests::repo_root();
        let scratch = wrela_tests::scratch_root(root.join("target/tmp")).join("typed-file");
        match oracles::run_file(std::path::Path::new(path), &scratch) {
            Ok(c) => println!("passes: {} points, {} counted", c.points, c.counted),
            Err(why) => {
                println!("{why}");
                std::process::exit(1);
            }
        }
        return;
    }
    let usage = "usage: typed-soak <O1..O9> (<count> | --only <seed>,...) [--seed N] [--threads N] [--wild] [--no-shrink] [--stop-after N]";
    let only: Option<Vec<u64>> = args
        .iter()
        .position(|a| a == "--only")
        .and_then(|i| args.get(i + 1))
        .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect());
    let count_arg =
        only.as_ref().map(|o| o.len() as u64).or(args.get(1).and_then(|a| a.parse().ok()));
    let (Some(oracle), Some(count)): (Option<Oracle>, Option<u64>) =
        (args.first().and_then(|a| Oracle::parse(a)), count_arg)
    else {
        eprintln!("{usage}");
        std::process::exit(2);
    };
    let flag = |name: &str| -> Option<u64> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
    };
    let base = flag("--seed").unwrap_or(0x50a6);
    let threads =
        flag("--threads").map_or_else(wrela_grammar::rng::default_threads, |t| t as usize);
    let modes: Vec<Mode> =
        if args.iter().any(|a| a == "--wild") { vec![Mode::Wild] } else { oracle.modes().to_vec() };
    let root = wrela_tests::repo_root();
    let scratch = wrela_tests::scratch_root(root.join("target/tmp")).join("typed-soak");
    let out = root.join("target/typed");
    let failures = Mutex::new(Vec::new());
    let done = std::sync::atomic::AtomicU64::new(0);
    let (points, counted) =
        (std::sync::atomic::AtomicU64::new(0), std::sync::atomic::AtomicU64::new(0));
    // Programs any of whose points counted: the rest (O9: refused by the walk) are apart.
    let proven = std::sync::atomic::AtomicU64::new(0);
    let started = std::time::Instant::now();
    // The GPU's oracle opens a device for each program: one at a time.
    let threads = if matches!(oracle, Oracle::O6 | Oracle::O7) { 1 } else { threads };
    let total = count * modes.len() as u64;
    let shrink = !args.iter().any(|a| a == "--no-shrink");
    let stop_after = flag("--stop-after").unwrap_or(3) as usize;
    let stopped = std::sync::atomic::AtomicBool::new(false);
    wrela_grammar::rng::par_seeded(
        total,
        base,
        threads,
        64 << 20,
        || (),
        |_, i, _| {
            if stopped.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let mode = modes[(i / count) as usize];
            let seed = match &only {
                Some(seeds) => seeds[(i % count) as usize],
                None => oracles::program_seed(base, i % count),
            };
            let checked = oracles::check_seed_shrinking(
                oracle,
                mode,
                oracle.features(),
                seed,
                &scratch,
                &out,
                shrink,
            );
            match checked {
                Ok(c) => {
                    points.fetch_add(u64::from(c.points), std::sync::atomic::Ordering::Relaxed);
                    counted.fetch_add(u64::from(c.counted), std::sync::atomic::Ordering::Relaxed);
                    if c.counted > 0 {
                        proven.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                Err(why) => {
                    eprintln!("{why}");
                    let mut f = failures.lock().unwrap();
                    f.push(why);
                    if only.is_none() && f.len() >= stop_after {
                        stopped.store(true, std::sync::atomic::Ordering::Relaxed);
                        eprintln!("stopping: {} failures", f.len());
                    }
                }
            }
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if n.is_multiple_of(25) {
                eprintln!("{n} of {total} ({:.0} s)", started.elapsed().as_secs_f64());
            }
        },
    );
    let failures = failures.into_inner().unwrap();
    println!(
        "{}: {} of {total} programs run from seed {base:#x} ({:?}), {} failures, {} points, {} counted (in {} programs), {:.0} s",
        oracle.name(),
        done.load(std::sync::atomic::Ordering::Relaxed),
        modes,
        failures.len(),
        points.into_inner(),
        counted.into_inner(),
        proven.into_inner(),
        started.elapsed().as_secs_f64()
    );
    let mut refusals = oracles::REFUSALS.lock().unwrap().clone();
    if !refusals.is_empty() {
        refusals.sort_by_key(|r| std::cmp::Reverse(r.1));
        println!("refused, by reason:");
        for (why, n) in refusals.iter().take(10) {
            println!("  {n:>6}  {why}");
        }
    }
    if !failures.is_empty() {
        std::process::exit(1);
    }
}
