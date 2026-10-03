//! The differential run: many grammar-generated programs (and near-miss mutants of them),
//! each checked for [agreement](crate::agree) between the oracle and the hand-written parser,
//! and, when accepted, for a clean formatter round trip.
//!
//! Program `i` of a run with seed `s` is generated from `mix(s, i)` alone, so the results don't
//! depend on the thread count and a failure names the two numbers that reproduce it.

use crate::agree::{Check, Checker};
use crate::earley::Scratch;
use crate::ebnf::Grammar;
use crate::generate::{Coverage, GenConfig, Generator};
use crate::rng::{Rng, default_threads, par_seeded};
use std::time::{Duration, Instant};
use wrela_syntax::fmt::check_round_trip;

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
        RunConfig {
            programs,
            seed,
            threads: default_threads(),
            mutant_rate: 0.25,
            round_trip: true,
            oracle: true,
            max_failures: 10,
        }
    }
}

/// What went wrong with a program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The oracle and the parser disagree on the program.
    Disagreement,
    /// They disagree on a mutant of the program.
    MutantDisagreement,
    /// The formatter round trip failed, on the program or its mutant.
    RoundTrip,
    /// Both rejected a program the grammar derived, though the lexer kept all its NEWLINEs.
    Rejected,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureKind::Disagreement => "disagreement",
            FailureKind::MutantDisagreement => "disagreement on a mutant",
            FailureKind::RoundTrip => "round trip",
            FailureKind::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Failure {
    pub seed: u64,
    pub index: u64,
    pub what: FailureKind,
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
    /// Generated programs both rejected although the lexer dropped none of their NEWLINEs: a
    /// bug in the grammar, the lexer or the generator.
    pub wrongly_rejected: u64,
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
    fn new(g: &Grammar, oracle: bool) -> RunStats {
        RunStats {
            oracle,
            coverage: Coverage::new(g),
            oracle_rules: vec![0; g.rules.len()],
            ..RunStats::default()
        }
    }

    fn merge(&mut self, o: RunStats, max_failures: usize) {
        self.programs += o.programs;
        self.accepted += o.accepted;
        self.rejected += o.rejected;
        self.wrongly_rejected += o.wrongly_rejected;
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
        self.coverage.merge(&o.coverage);
        self.oracle_rules.iter_mut().zip(&o.oracle_rules).for_each(|(a, b)| *a += b);
    }

    /// A human-readable summary.
    pub fn report(&self, checker: &Checker) -> String {
        let g = &checker.grammar;
        let gaps = self.coverage.gaps(g);
        let total = Coverage::branches(g);
        let hit = self.coverage.hits(g);
        let used = self.oracle_rules.iter().filter(|n| **n > 0).count();
        let pct = |a: u64, b: u64| if b == 0 { 0.0 } else { 100.0 * a as f64 / b as f64 };
        let both = if self.oracle { "by both parsers" } else { "by the parser (oracle not run)" };
        let mut s = format!(
            "{} grammar-generated programs in {:.1?} ({:.0} programs/s, {:.1} tokens on average)\n\
             accepted {both}: {} ({:.2}%), rejected {both}: {} ({} with no NEWLINE dropped)\n\
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
            self.wrongly_rejected,
            self.programs_with_dropped_newlines,
            self.mutants,
            self.mutants_accepted,
            self.round_trips,
            self.disagreements,
            self.round_trip_failures,
            g.rules.len(),
        );
        for gap in gaps {
            s.push_str(&format!("  not covered: {gap}\n"));
        }
        for f in &self.failures {
            s.push_str(&format!(
                "\nFAILURE ({}) seed {} program {}: {}\n--- source ---\n{}\n--------------\n",
                f.what.as_str(),
                f.seed,
                f.index,
                f.message,
                f.source
            ));
        }
        s
    }
}

/// Runs the differential check on `cfg.programs` generated programs.
pub fn run(checker: &Checker, cfg: &RunConfig) -> RunStats {
    let start = Instant::now();
    let generator = Generator::new(&checker.grammar, GenConfig::default());
    let workers = par_seeded(
        cfg.programs,
        cfg.seed,
        cfg.threads,
        64 << 20,
        || Worker::new(checker, &generator, cfg),
        |w, i, rng| w.program(i, rng),
    );
    let mut total = RunStats::new(&checker.grammar, cfg.oracle);
    for w in workers {
        total.merge(w.stats, cfg.max_failures);
    }
    total.elapsed = start.elapsed();
    total
}

/// One thread's share of a run.
struct Worker<'a> {
    checker: &'a Checker,
    generator: &'a Generator<'a>,
    cfg: &'a RunConfig,
    stats: RunStats,
    scratch: Scratch,
    /// Per rule: whether the current program's parse tree uses it.
    used: Vec<bool>,
}

impl<'a> Worker<'a> {
    fn new(checker: &'a Checker, generator: &'a Generator<'a>, cfg: &'a RunConfig) -> Worker<'a> {
        let g = &checker.grammar;
        Worker {
            checker,
            generator,
            cfg,
            stats: RunStats::new(g, cfg.oracle),
            scratch: Scratch::default(),
            used: vec![false; g.rules.len()],
        }
    }

    /// Generates program `i` from `rng` and checks it, and sometimes a mutant of it.
    fn program(&mut self, i: u64, mut rng: Rng) {
        let generator = self.generator;
        let terms = generator.generate(&mut rng, &mut self.stats.coverage);
        let rendered = generator.render(&terms, &mut rng);
        self.stats.programs += 1;
        self.stats.tokens += rendered.tokens.len() as u64;
        if rendered.dropped_newlines > 0 {
            self.stats.programs_with_dropped_newlines += 1;
        }
        let check = self.check_text(i, &rendered.text, false);
        if check.disagreement.is_none() {
            if check.accepted {
                self.stats.accepted += 1;
                if let Some(tree) = &check.tree {
                    self.used.iter_mut().for_each(|u| *u = false);
                    tree.walk(&mut |n| self.used[n.rule] = true);
                    for (n, u) in self.stats.oracle_rules.iter_mut().zip(&self.used) {
                        *n += u64::from(*u);
                    }
                }
            } else {
                self.stats.rejected += 1;
                if rendered.dropped_newlines == 0 {
                    let why = "both parsers reject a program the grammar derives, and the lexer \
                               dropped none of its NEWLINEs";
                    self.fail(i, FailureKind::Rejected, &rendered.text, why.into());
                }
            }
        }
        if self.cfg.oracle && rng.chance(self.cfg.mutant_rate) {
            let mutant = generator.mutate(&terms, &mut rng);
            let text = generator.render(&mutant, &mut rng).text;
            let check = self.check_text(i, &text, true);
            self.stats.mutants += 1;
            if check.disagreement.is_none() && check.accepted {
                self.stats.mutants_accepted += 1;
            }
        }
    }

    /// Checks one text, program `i` or a mutant of it, for agreement and then, when the parser
    /// accepts it, for a clean formatter round trip. Records the failures.
    fn check_text(&mut self, i: u64, text: &str, mutant: bool) -> Check {
        let check = if self.cfg.oracle {
            self.checker.check(text, &mut self.scratch)
        } else {
            self.checker.check_parser_only(text)
        };
        if let Some(d) = &check.disagreement {
            let what =
                if mutant { FailureKind::MutantDisagreement } else { FailureKind::Disagreement };
            self.fail(i, what, text, d.clone());
        }
        if check.accepted && !check.limited && self.cfg.round_trip && !check.lexical_errors {
            self.stats.round_trips += 1;
            if let Err(e) = check_round_trip(&check.parsed, text) {
                self.fail(i, FailureKind::RoundTrip, text, e);
            }
        }
        check
    }

    fn fail(&mut self, i: u64, what: FailureKind, source: &str, message: String) {
        match what {
            FailureKind::RoundTrip => self.stats.round_trip_failures += 1,
            FailureKind::Rejected => self.stats.wrongly_rejected += 1,
            FailureKind::Disagreement | FailureKind::MutantDisagreement => {
                self.stats.disagreements += 1;
            }
        }
        if self.stats.failures.len() < self.cfg.max_failures {
            self.stats.failures.push(Failure {
                seed: self.cfg.seed,
                index: i,
                what,
                source: source.to_string(),
                message,
            });
        }
    }
}
