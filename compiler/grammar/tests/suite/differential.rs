//! AC5, "the grammar is the spec": the oracle parser for spec/grammar.ebnf and the hand-written
//! parser agree (see `wrela_grammar::agree` for what that means) on grammar-generated programs,
//! near-miss mutants of them, the conformance suite and the round-trip sample; and every
//! accepted program survives parse → format → parse unchanged.
//!
//! These runs are sized for `cargo test` (larger with `WRELA_FULL`). The full AC run is
//! `cargo run -p wrela-grammar --release --bin differential -- 1000000`.

use crate::common;

use std::sync::Mutex;
use wrela_grammar::agree::{Checker, round_trip};
use wrela_grammar::differential::{RunConfig, run};
use wrela_grammar::earley::Scratch;

const SEED: u64 = 0xac5;

#[test]
fn generated_programs_agree() {
    let checker = Checker::default();
    let mut cfg = RunConfig::new(common::sized(2000, 3000), SEED);
    cfg.round_trip = false;
    let stats = run(&checker, &cfg);
    let report = stats.report(&checker);
    println!("{report}");
    assert_eq!(stats.disagreements, 0, "the oracle and the parser disagree:\n{report}");
    // Generated programs are rejected only when the lexer drops a NEWLINE the derivation
    // needed; if that became common, the test would be checking little.
    assert!(
        stats.accepted * 100 >= stats.programs * 95,
        "too few generated programs are valid:\n{report}"
    );
    // About one mutant per four programs, a tenth of them valid.
    assert!(
        stats.mutants * 6 > stats.programs && stats.mutants_accepted * 100 > stats.programs,
        "too few mutants:\n{report}"
    );
    let gaps = stats.coverage.gaps(&checker.grammar);
    assert!(gaps.is_empty(), "parts of the grammar were never generated:\n{report}");
    assert!(stats.oracle_rules.iter().all(|n| *n > 0), "a rule is in no accepted parse:\n{report}");
}

#[test]
fn generated_programs_round_trip_through_the_formatter() {
    let checker = Checker::default();
    // The oracle is checked above; this run is about the formatter, so it can be bigger.
    let programs = common::sized(3000, 10_000);
    let mut cfg = RunConfig::new(programs, SEED + 1);
    cfg.oracle = false;
    let stats = run(&checker, &cfg);
    let report = stats.report(&checker);
    println!("{report}");
    assert!(stats.round_trips > programs * 9 / 10);
    assert_eq!(stats.round_trip_failures, 0, "formatting changed or broke programs:\n{report}");
}

#[test]
fn the_conformance_suite_and_the_standard_library_agree() {
    let files = common::wrela_files();
    println!("{} .wrela files under compiler/", files.len());
    let checker = Checker::default();
    let failures = Mutex::new(Vec::new());
    common::par_each(&files, Scratch::default, |scratch, path| {
        let src =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let check = checker.check(&src, scratch);
        let failure = if let Some(d) = check.disagreement {
            Some(format!("{}: {d}", path.display()))
        } else if check.accepted && !check.lexical_errors {
            round_trip(&src).err().map(|e| format!("{}: {e}", path.display()))
        } else {
            None
        };
        failures.lock().expect("failures").extend(failure);
    });
    let mut failures = failures.into_inner().expect("failures");
    failures.sort();
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn the_round_trip_sample_agrees() {
    let src = common::roundtrip_sample();
    common::with_big_stack(|| {
        let check = Checker::default().check(&src, &mut Scratch::default());
        assert_eq!(check.disagreement, None);
        assert!(check.accepted && !check.lexical_errors);
    });
}

/// Hand-picked cases at the edges of the grammar, each checked for agreement (and, for the
/// valid ones, that both accept).
#[test]
fn edge_cases_agree() {
    let valid = [
        "",
        "\n\n// only a comment\n",
        "fn f() { a - b - c * d ** -e ** f }",
        "fn f() -> Option<Option<T>> { let x: A<B<C>>= y; x.0.1 }",
        "fn f() { let x: A<B<C<D>>>= y }",
        "fn f() { x >>= 1; y = a::<T>>b }",
        "fn f() { match x { (A(_)) | ((b)) => 1, S { a, b: (c), .. } => 2, _ => 3 } }",
        "fn f() { t.0x1 + t.1_0 }",
        "fn f() {\n    a\n        .b()\n        .c\n}",
        "fn f() { if a { b } else if c { d } else { e } }",
        "fn f() { for mut i in 0..=n { }; for p in xs { } }",
        "fn f() { |x: f32| -> f32 { x }; || 1; | | 2 }",
        "fn f() { S { ..b }; S { a, b: 1, ..c, }; if (S {}) == s {} }",
        "@compute(64)\n@other\nfn f(x: mut T = 1, take self) {}",
        "use a::{b, c::{d as e}, }\n",
        "trait T: A + B {\n    type Out: C\n    fn f(self) -> f32\n    fn g() {}\n}",
        "impl<T: A> X<T> for Y { type Z = W\n pub fn f() {} }",
        "enum E<T> { A, B(T, [T; 3]), C { x: fn(T) -> T }, }",
        "struct S {\n    a: f32 = 1.0\n}",
        "const N: (u32, [f32]) = (1, [2.0; 3])",
        "fn f() { return return }",
    ];
    let invalid = [
        "fn f() { a < b < c }",
        "fn f() { a.b(c }",
        "fn f() { a.0(1) }",
        "fn f() { f(a: 1, 2) }",
        "fn f() { x = |y| y = 1 }",
        "fn f() { if a {}\n else {} }",
        "fn f() { a\n + b }",
        "let x = 1",
        "pub impl X {}",
        "fn f() { A<B> }",
        "fn f() -> A<B>>> {}",
        "fn f() { 1 + return }",
        "struct S { a: f32\n b: f32 }",
    ];
    let checker = Checker::default();
    let mut scratch = Scratch::default();
    for (src, ok) in valid.iter().map(|s| (s, true)).chain(invalid.iter().map(|s| (s, false))) {
        let check = checker.check(src, &mut scratch);
        assert_eq!(check.disagreement, None, "{src:?}");
        assert_eq!(check.accepted, ok, "{src:?}");
    }
}
