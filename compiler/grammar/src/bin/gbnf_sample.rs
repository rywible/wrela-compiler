//! Samples programs from spec/wrela.gbnf and checks that the hand-written parser accepts every
//! one without a diagnostic (and that the oracle agrees).
//!
//!     cargo run -p wrela-grammar --release --bin gbnf-sample -- [PROGRAMS] [--seed N]

use wrela_grammar::agree::Checker;
use wrela_grammar::gbnf::spec_path;
use wrela_grammar::rng::default_threads;
use wrela_grammar::sampler::{Gbnf, check_samples};

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut n, mut seed) = (100_000u64, 0x6b6eu64);
    while let Some(a) = args.next() {
        let parsed = if a == "--seed" {
            args.next().and_then(|v| v.parse().ok()).map(|v| seed = v)
        } else {
            a.parse().ok().map(|v| n = v)
        };
        if parsed.is_none() {
            eprintln!("usage: gbnf-sample [PROGRAMS] [--seed N]");
            std::process::exit(2);
        }
    }
    let path = spec_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("reading {}: {e}", path.display());
        std::process::exit(1)
    });
    let g = Gbnf::parse(&text).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    let start = std::time::Instant::now();
    let stats = check_samples(&g, Some(&Checker::default()), n, seed, default_threads());
    println!("{}in {:.1?}", stats.report(), start.elapsed());
    if !stats.rejected.is_empty() || !stats.disagreements.is_empty() {
        std::process::exit(1);
    }
}
