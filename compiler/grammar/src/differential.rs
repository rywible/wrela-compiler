//! The differential run: many grammar-generated programs (and near-miss mutants of them),
//! each checked for [agreement](crate::agree) between the oracle and the hand-written parser,
//! and, when accepted, for a clean formatter round trip.
//!
//! Program `i` of a run with seed `s` is generated from `mix(s, i)` alone, so the results don't
//! depend on the thread count and a failure names the two numbers that reproduce it.

use crate::agree::{Checker, round_trip};
use crate::earley::Scratch;
use crate::generate::{Coverage, GenConfig, Generator};
use crate::rng::{Rng, mix};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub programs: u64,
    pub seed: u64,
    pub threads: usize,
    /// The chance that a program also gets a mutant checked.
    pub mutant_rate: f64,
    pub round_trip: bool,
    /// Compare with the oracle (off for a formatter-only run, which is much faster).
    pub oracle: bool,
    /// Stop recording failures after this many (they're still counted).
    pub max_failures: usize,
}

impl RunConfig {
    pub fn new(programs: u64, seed: u64) -> RunConfig {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        RunConfig {
            programs,
            seed,
            threads,
            mutant_rate: 0.25,
            round_trip: true,
            oracle: true,
            max_failures: 10,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Failure {
    pub seed: u64,
    pub index: u64,
    pub what: &'static str,
    pub source: String,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct RunStats {
    /// Whether the oracle was compared (otherwise "accepted" is the parser's verdict alone).
    pub oracle: bool,
    pub programs: u64,
    /// Generated programs both parsers accepted (and agreed on).
    pub accepted: u64,
    /// Generated programs both rejected: possible only when the lexer dropped a NEWLINE the
    /// derivation needed.
    pub rejected: u64,
    pub tokens: u64,
    pub programs_with_dropped_newlines: u64,
    pub mutants: u64,
    pub mutants_accepted: u64,
    pub round_trips: u64,
    pub disagreements: u64,
    pub round_trip_failures: u64,
    pub failures: Vec<Failure>,
    pub coverage: Coverage,
    /// Per rule: how many accepted programs' parse trees used it.
    pub oracle_rules: Vec<u64>,
    pub elapsed: Duration,
}

impl RunStats {
    fn merge(&mut self, o: RunStats, max_failures: usize) {
        self.programs += o.programs;
        self.accepted += o.accepted;
        self.rejected += o.rejected;
        self.tokens += o.tokens;
        self.programs_with_dropped_newlines += o.programs_with_dropped_newlines;
        self.mutants += o.mutants;
        self.mutants_accepted += o.mutants_accepted;
        self.round_trips += o.round_trips;
        self.disagreements += o.disagreements;
        self.round_trip_failures += o.round_trip_failures;
        for f in o.failures {
            if self.failures.len() < max_failures {
                self.failures.push(f);
            }
        }
        if self.coverage.rules.is_empty() {
            self.coverage = o.coverage;
        } else {
            self.coverage.merge(&o.coverage);
        }
        if self.oracle_rules.is_empty() {
            self.oracle_rules = o.oracle_rules;
        } else {
            self.oracle_rules.iter_mut().zip(&o.oracle_rules).for_each(|(a, b)| *a += b);
        }
    }

    /// A human-readable summary.
    pub fn report(&self, checker: &Checker) -> String {
        let g = &checker.grammar;
        let (hit, total) = self.coverage.branch_counts(g);
        let used = self.oracle_rules.iter().filter(|n| **n > 0).count();
        let pct = |a: u64, b: u64| if b == 0 { 0.0 } else { 100.0 * a as f64 / b as f64 };
        let both = if self.oracle { "by both parsers" } else { "by the parser (oracle not run)" };
        let mut s = format!(
            "{} grammar-generated programs in {:.1?} ({:.0} programs/s, {:.1} tokens on average)\n\
             accepted {both}: {} ({:.2}%), rejected {both}: {}\n\
             programs where the lexer dropped a grammar NEWLINE: {}\n\
             mutants checked: {} (accepted {both}: {})\n\
             formatter round trips: {}\n\
             disagreements: {}, round-trip failures: {}\n\
             generator coverage: {hit}/{total} alternatives, optional parts and repetitions; \
             rules in accepted parse trees: {used}/{}\n",
            self.programs,
            self.elapsed,
            self.programs as f64 / self.elapsed.as_secs_f64().max(1e-9),
            self.tokens as f64 / self.programs.max(1) as f64,
            self.accepted,
            pct(self.accepted, self.programs),
            self.rejected,
            self.programs_with_dropped_newlines,
            self.mutants,
            self.mutants_accepted,
            self.round_trips,
            self.disagreements,
            self.round_trip_failures,
            g.rules.len(),
        );
        for gap in self.coverage.gaps(g) {
            s.push_str(&format!("  not covered: {gap}\n"));
        }
        for f in &self.failures {
            s.push_str(&format!(
                "\nFAILURE ({}) seed {} program {}: {}\n--- source ---\n{}\n--------------\n",
                f.what, f.seed, f.index, f.message, f.source
            ));
        }
        s
    }
}

/// Runs the differential check on `cfg.programs` generated programs.
pub fn run(checker: &Checker, cfg: &RunConfig) -> RunStats {
    let start = Instant::now();
    let threads = cfg.threads.max(1) as u64;
    let generator = Generator::new(&checker.grammar, GenConfig::default());
    let generator = &generator;
    let mut total = RunStats { oracle: cfg.oracle, ..RunStats::default() };
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                std::thread::Builder::new()
                    .stack_size(64 << 20)
                    .spawn_scoped(scope, move || run_slice(checker, generator, cfg, t, threads))
                    .expect("spawning a worker thread")
            })
            .collect();
        for h in handles {
            match h.join() {
                Ok(stats) => total.merge(stats, cfg.max_failures),
                Err(p) => std::panic::resume_unwind(p),
            }
        }
    });
    total.elapsed = start.elapsed();
    total
}

fn run_slice(
    checker: &Checker,
    generator: &Generator,
    cfg: &RunConfig,
    first: u64,
    step: u64,
) -> RunStats {
    let g = &checker.grammar;
    let mut stats = RunStats {
        coverage: Coverage::new(g),
        oracle_rules: vec![0; g.rules.len()],
        ..RunStats::default()
    };
    let mut scratch = Scratch::default();
    let mut used = vec![false; g.rules.len()];
    let fail = |stats: &mut RunStats, i: u64, what, source: &str, message: String| {
        if what == "round trip" {
            stats.round_trip_failures += 1;
        } else {
            stats.disagreements += 1;
        }
        if stats.failures.len() < cfg.max_failures {
            stats.failures.push(Failure {
                seed: cfg.seed,
                index: i,
                what,
                source: source.to_string(),
                message,
            });
        }
    };
    let mut i = first;
    while i < cfg.programs {
        let mut rng = Rng::new(mix(cfg.seed, i));
        let terms = generator.generate(&mut rng, &mut stats.coverage);
        let rendered = generator.render(&terms, &mut rng);
        stats.programs += 1;
        stats.tokens += rendered.tokens.len() as u64;
        if rendered.dropped_newlines > 0 {
            stats.programs_with_dropped_newlines += 1;
        }
        let check = if cfg.oracle {
            checker.check(&rendered.text, &mut scratch)
        } else {
            checker.check_parser_only(&rendered.text)
        };
        if let Some(d) = check.disagreement {
            fail(&mut stats, i, "disagreement", &rendered.text, d);
        } else if check.accepted {
            stats.accepted += 1;
            used.iter_mut().for_each(|u| *u = false);
            for r in check.rules_used {
                used[r] = true;
            }
            for (n, u) in stats.oracle_rules.iter_mut().zip(&used) {
                *n += u64::from(*u);
            }
        } else {
            stats.rejected += 1;
        }
        if check.accepted && cfg.round_trip && !check.lexical_errors {
            stats.round_trips += 1;
            if let Err(e) = round_trip(&rendered.text) {
                fail(&mut stats, i, "round trip", &rendered.text, e);
            }
        }
        if cfg.oracle && rng.chance(cfg.mutant_rate) {
            let mutant = generator.mutate(&terms, &mut rng);
            let text = generator.render(&mutant, &mut rng).text;
            let check = checker.check(&text, &mut scratch);
            stats.mutants += 1;
            if let Some(d) = check.disagreement {
                fail(&mut stats, i, "disagreement on a mutant", &text, d);
            } else if check.accepted {
                stats.mutants_accepted += 1;
                if cfg.round_trip && !check.lexical_errors {
                    stats.round_trips += 1;
                    if let Err(e) = round_trip(&text) {
                        fail(&mut stats, i, "round trip", &text, e);
                    }
                }
            }
        }
        i += step;
    }
    stats
}
