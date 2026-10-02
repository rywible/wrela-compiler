//! Checks the oracle and the hand-written parser against each other on grammar-generated
//! programs.
//!
//!     cargo run -p wrela-grammar --release --bin differential -- [PROGRAMS] [--seed N] [--threads N]
//!
//! PROGRAMS defaults to 1000000 (the AC5 run). Exits nonzero on any disagreement or round-trip
//! failure.

use wrela_grammar::agree::Checker;
use wrela_grammar::differential::{RunConfig, run};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut cfg = RunConfig::new(1_000_000, 0x5eed);
    while let Some(a) = args.next() {
        let mut value = |name: &str| -> u64 {
            args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
                eprintln!("{name} needs a number");
                std::process::exit(2)
            })
        };
        match a.as_str() {
            "--seed" => cfg.seed = value("--seed"),
            "--threads" => cfg.threads = value("--threads") as usize,
            n => {
                cfg.programs = n.parse().unwrap_or_else(|_| {
                    eprintln!("usage: differential [PROGRAMS] [--seed N] [--threads N]");
                    std::process::exit(2)
                })
            }
        }
    }
    let checker = Checker::default();
    eprintln!("checking {} programs, seed {}, {} threads…", cfg.programs, cfg.seed, cfg.threads);
    let stats = run(&checker, &cfg);
    println!("{}", stats.report(&checker));
    if stats.disagreements + stats.round_trip_failures > 0 {
        std::process::exit(1);
    }
}
