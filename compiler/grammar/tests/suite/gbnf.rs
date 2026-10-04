//! The GBNF export: the checked-in spec/wrela.gbnf is what the exporter produces from
//! spec/grammar.ebnf, every program sampled from it parses, and it isn't so conservative that
//! ordinary code falls outside it.

use crate::common;

use wrela_grammar::ebnf::Grammar;
use wrela_grammar::gbnf::{export, spec_path};
use wrela_grammar::rng::default_threads;
use wrela_grammar::sampler::{Gbnf, check_samples};
use wrela_grammar::testing::{sized, with_big_stack};

fn checked_in() -> String {
    let path = spec_path();
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

#[test]
fn the_checked_in_gbnf_is_current() {
    let fresh = export(&Grammar::spec()).expect("exporting the grammar");
    assert!(
        fresh == checked_in(),
        "spec/wrela.gbnf is out of date; run `cargo run -p wrela-grammar --bin export-gbnf`"
    );
}

#[test]
fn every_sampled_program_parses() {
    let g = Gbnf::parse(&checked_in()).expect("reading spec/wrela.gbnf");
    // The oracle check is left to `gbnf-sample`: at opt-level 0 it would dominate the run.
    let stats = check_samples(&g, None, 10_000, 0x6b6e, default_threads());
    let report = stats.report();
    println!("{report}");
    assert!(stats.rejected.is_empty() && stats.disagreements.is_empty(), "{report}");
    assert!(stats.multi_line > 1000 && stats.with_comments > 1000, "{report}");
    assert!(
        stats.rules_used * 10 >= stats.rules_total * 9,
        "samples cover too little of the GBNF:\n{report}"
    );
}

#[test]
fn identifiers_exclude_keywords_and_the_wildcard() {
    let g = Gbnf::parse(&checked_in()).unwrap();
    let ident = g.rule_id("ident").unwrap();
    for kw in wrela_syntax::TokenKind::KEYWORDS {
        let text = kw.fixed_text().unwrap();
        assert!(!g.recognizes(ident, text), "{text} is a keyword");
        assert!(g.recognizes(ident, &format!("{text}_")), "{text}_ is a name");
        assert!(g.recognizes(ident, &format!("{text}1")), "{text}1 is a name");
    }
    for name in
        ["a", "Z", "_a", "__", "_0", "selfish", "Selfie", "s", "S", "f32", "vec3", "in_", "i"]
    {
        assert!(g.recognizes(ident, name), "{name}");
    }
    for not in ["_", "1a", "", "a-b"] {
        assert!(!g.recognizes(ident, not), "{not:?}");
    }
}

/// The export is conservative, but formatted code and the usual hand layouts are in it.
#[test]
fn ordinary_layouts_are_in_the_language() {
    let g = Gbnf::parse(&checked_in()).unwrap();
    let sample = common::roundtrip_sample();
    let formatted = {
        let p = wrela_syntax::parse(wrela_diag::FileId(0), &sample);
        wrela_syntax::fmt::format(&p, &sample)
    };
    let layouts = [
        formatted.as_str(),
        sample.as_str(),
        "fn f() {}",
        "fn f(a: f32, b: f32) -> f32 {\n    let c = a +\n        b // trailing\n    // own line\n\n    c * 2.0\n}\n",
        "fn f() {\n    let r = sphere(radius: 1.0)\n        .smooth_union(cone(), k: 0.1)\n        .x\n}\n",
        "fn f() {\n    g(\n        1,\n        2,\n    )\n    if a { b } else { c }\n}\n",
        "struct S {\n    a: f32\n}\n\nenum E { A, B }\n",
        "fn f() { match x {\n    A => 1\n    B => 2,\n} }\n",
        "fn f() { let x: Option<Option<u32>> = y; t.0 .1 }",
        "fn f() { t.0.1 + pair.0.x + 2.0.sqrt() + 1.max(2) + 1e5.abs() + 1 .0 }",
        "@compute(64)\nfn f() {}\n",
        "fn f() { let s = \"a \\\"q\\\" b\"\n    let d = 15cm + 1.5e-3mm\n    let t = f\"{w:.1} kg, {{x}}, {f\"{a}\"}\" }",
        "fn f() { match w { \"weapon\" => 1, _ => 2 } }",
    ];
    with_big_stack(|| {
        for src in layouts {
            assert!(g.recognizes(g.root, src), "not in the GBNF language:\n{src}");
        }
        // And what it must not allow: line breaks the lexer turns into NEWLINEs where the grammar
        // has none, and missing separators.
        let invalid = [
            "fn f() { a\n + b }",
            "fn f()\n{}",
            "fn f() { if a {}\n else {} }",
            "fn f() { a b }",
            "fn f() { match x { a => 1 b => 2 } }",
            // A number takes a `.` and a digit (L12): this is one malformed number.
            "fn f() { 1e5.5 }",
        ];
        for src in invalid {
            assert!(!g.recognizes(g.root, src), "should not be in the GBNF language:\n{src}");
        }
    });
}

/// The formatter's layout is in the GBNF: grammar-generated programs (tier 0, with tuple
/// indexes the GBNF can write: up to four decimal digits), formatted.
#[test]
fn formatted_programs_are_in_the_language() {
    use wrela_grammar::generate::{Coverage, GenConfig, Generator};
    use wrela_grammar::rng::Rng;
    use wrela_syntax::TokenKind as T;
    let g = Gbnf::parse(&checked_in()).unwrap();
    let grammar = Grammar::spec();
    let generator = Generator::new(&grammar, GenConfig::default());
    let mut cov = Coverage::new(&grammar);
    with_big_stack(|| {
        let mut formatted = 0;
        for seed in 0..sized(2000, 5000) {
            let mut rng = Rng::new(seed);
            let terms = generator.generate(&mut rng, &mut cov);
            let src = generator.render(&terms, &mut rng).text;
            let p = wrela_syntax::parse(wrela_diag::FileId(0), &src);
            let tokens = wrela_syntax::lex(wrela_diag::FileId(0), &src).tokens;
            // The GBNF writes a tuple index touching its `.`: not after a comment between them.
            let odd_index = |w: &[wrela_syntax::Token]| {
                let index = &src[w[1].span.range()];
                w[0].kind == T::Dot
                    && w[1].kind == T::Int
                    && (index.len() > 4
                        || !index.bytes().all(|b| b.is_ascii_digit())
                        || w[0].span.end != w[1].span.start)
            };
            // A unit suffix the GBNF leaves out: one starting with `x`, `b` or `o`, which
            // after a `0` reads as a radix prefix.
            let odd_unit = |t: &wrela_syntax::Token| {
                t.kind == T::Suffixed
                    && wrela_syntax::lexer::split_suffix(&src[t.span.range()])
                        .1
                        .starts_with(['x', 'X', 'b', 'B', 'o', 'O'])
            };
            if p.has_errors() || tokens.iter().any(odd_unit) || tokens.windows(2).any(odd_index) {
                continue;
            }
            let out = wrela_syntax::fmt::format(&p, &src);
            assert!(g.recognizes(g.root, &out), "not in the GBNF language:\n{out}");
            formatted += 1;
        }
        assert!(formatted > 1000, "only {formatted} programs");
    });
}
