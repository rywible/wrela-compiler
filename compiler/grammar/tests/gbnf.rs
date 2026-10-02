//! The GBNF export: the checked-in spec/wrela.gbnf is what the exporter produces from
//! spec/grammar.ebnf, every program sampled from it parses, and it isn't so conservative that
//! ordinary code falls outside it.

mod common;

use wrela_grammar::ebnf::Grammar;
use wrela_grammar::gbnf::export;
use wrela_grammar::sampler::{Gbnf, check_samples};

fn checked_in() -> String {
    let path = common::repo_root().join("spec/wrela.gbnf");
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
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    // The oracle check is left to `gbnf-sample`: at opt-level 0 it would dominate the run.
    let stats = check_samples(&g, None, 10_000, 0x6b6e, threads);
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
        "@compute(64)\nfn f() {}\n",
    ];
    common::with_big_stack(|| {
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
        ];
        for src in invalid {
            assert!(!g.recognizes(g.root, src), "should not be in the GBNF language:\n{src}");
        }
    });
}
