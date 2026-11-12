//! The typed programs' soak: one oracle over many programs, not in the gate.
//!
//! ```text
//! cargo run --release -p wrela-tests --bin typed-soak -- <oracle> <count> [--seed N] [--threads N] [--wild]
//! ```
//!
//! Each failure is shrunk and written to `target/typed/<oracle>-<seed>.wrela`; the run prints
//! each and a summary, and exits 1 if there was one. `--wild` runs O1 over wild programs only
//! (O1 otherwise sees both modes). `typed-soak --file <path>` runs one program file (its
//! header names its oracle and seed).

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
    let usage = "usage: typed-soak <O1..O6> <count> [--seed N] [--threads N] [--wild]";
    let (Some(oracle), Some(count)): (Option<Oracle>, Option<u64>) =
        (args.first().and_then(|a| Oracle::parse(a)), args.get(1).and_then(|a| a.parse().ok()))
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
    let started = std::time::Instant::now();
    // The GPU's oracle opens a device for each program: one at a time.
    let threads = if oracle == Oracle::O6 { 1 } else { threads };
    let total = count * modes.len() as u64;
    wrela_grammar::rng::par_seeded(
        total,
        base,
        threads,
        64 << 20,
        || (),
        |_, i, _| {
            let mode = modes[(i / count) as usize];
            let seed = oracles::program_seed(base, i % count);
            match oracles::check_seed(oracle, mode, oracle.features(), seed, &scratch, &out) {
                Ok(c) => {
                    points.fetch_add(u64::from(c.points), std::sync::atomic::Ordering::Relaxed);
                    counted.fetch_add(u64::from(c.counted), std::sync::atomic::Ordering::Relaxed);
                }
                Err(why) => {
                    eprintln!("{why}");
                    failures.lock().unwrap().push(why);
                }
            }
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if n.is_multiple_of(250) {
                eprintln!("{n} of {total} ({:.0} s)", started.elapsed().as_secs_f64());
            }
        },
    );
    let failures = failures.into_inner().unwrap();
    println!(
        "{}: {total} programs from seed {base:#x} ({:?}), {} failures, {} points, {} counted, {:.0} s",
        oracle.name(),
        modes,
        failures.len(),
        points.into_inner(),
        counted.into_inner(),
        started.elapsed().as_secs_f64()
    );
    if !failures.is_empty() {
        std::process::exit(1);
    }
}
