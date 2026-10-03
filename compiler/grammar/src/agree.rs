//! The differential check between the oracle and the hand-written parser on one source text.
//!
//! **Agreement**, for a source text, means all of:
//!
//! 1. Both parse the token stream of `wrela_syntax::lex`, so they see the same tokens,
//!    NEWLINEs included.
//! 2. The oracle accepts exactly when the hand-written parser reports no error of its own.
//!    Lexical diagnostics are left out on both sides: the lexer is shared, and the grammar is
//!    about the tokens it produced. So are the parser's limits on what the grammar allows,
//!    which aren't syntax: nesting and depth (E0112), and a tuple index too large for a `u32`
//!    (E0006). A text that passes only because of these is [`Check::limited`].
//! 3. When the oracle accepts, its derivation is unique: an ambiguous grammar is a bug even if
//!    the parser happens to pick the intended reading.
//! 4. When both accept, the oracle's parse tree and the parser's AST have the same
//!    [shape](crate::canon): the same boundaries for every item, member, parameter, field,
//!    variant, use tree, block, statement, arm, expression, pattern and type, which includes
//!    the precedence and associativity of every operator.
//!
//! Anything else is a disagreement, and a bug in the grammar or the parser.

use crate::canon::{Roles, Shape, ast_shape};
use crate::earley::{Node, Oracle, Outcome, Scratch};
use crate::ebnf::Grammar;
use std::fmt::Write as _;
use wrela_diag::{Diagnostic, FileId, codes};
use wrela_syntax::{Parsed, parse, parser};

pub struct Checker {
    pub grammar: Grammar,
    pub oracle: Oracle,
    roles: Roles,
}

/// The result of checking one source text.
#[derive(Debug)]
pub struct Check {
    /// Both accepted it (and agree), or both rejected it.
    pub accepted: bool,
    /// The parser reported only errors for its limits (E0112, E0006), which agreement leaves
    /// out: the grammar accepts the text, but the compiler doesn't.
    pub limited: bool,
    /// The lexer reported an error (the parsers were compared anyway).
    pub lexical_errors: bool,
    /// Why the two disagree, if they do.
    pub disagreement: Option<String>,
    /// The oracle's parse tree, when both parsers accepted.
    pub tree: Option<Node>,
    /// The hand-written parser's result, for further checks on the same text (the formatter's
    /// round trip: `wrela_syntax::fmt::check_round_trip`).
    pub parsed: Parsed,
}

impl Default for Checker {
    fn default() -> Checker {
        Checker::new(Grammar::spec())
    }
}

impl Checker {
    pub fn new(grammar: Grammar) -> Checker {
        let oracle = Oracle::new(&grammar);
        let roles = Roles::new(&grammar);
        Checker { grammar, oracle, roles }
    }

    pub fn check(&self, src: &str, scratch: &mut Scratch) -> Check {
        let lexed = wrela_syntax::lex(FileId(0), src);
        let lex_diags = lexed.diagnostics.len();
        let lexical_errors = wrela_diag::has_errors(&lexed.diagnostics);
        let outcome = self.oracle.parse(&lexed.tokens, scratch);
        let parsed = parser::parse_tokens(FileId(0), src, lexed);
        let mut check = Check {
            accepted: false,
            limited: false,
            lexical_errors,
            disagreement: None,
            tree: None,
            parsed,
        };
        let (limits, parser_errors): (Vec<_>, Vec<_>) = check.parsed.diagnostics[lex_diags..]
            .iter()
            .filter(|d| d.is_error())
            .partition(|d| is_limit(d));
        let hand_accepts = parser_errors.is_empty();
        check.limited = !limits.is_empty();
        match outcome {
            Outcome::Ambiguous(a) => {
                check.disagreement = Some(format!(
                    "the grammar is ambiguous: `{}` over {:?} ({}): {}",
                    a.rule,
                    snippet(src, a.span.0, a.span.1),
                    a.how,
                    if hand_accepts { "the parser accepts" } else { "the parser rejects" }
                ));
            }
            Outcome::Rejected { at, expected } if hand_accepts => {
                let names: Vec<String> = expected.iter().map(ToString::to_string).collect();
                check.disagreement = Some(format!(
                    "the parser accepts, the grammar rejects at byte {at} ({:?}), expecting one of: {}",
                    snippet(src, at, (at + 12).min(src.len() as u32)),
                    names.join(" ")
                ));
            }
            Outcome::Rejected { .. } => {}
            Outcome::Accepted(_) if !hand_accepts => {
                let mut msg = String::from("the grammar accepts, the parser rejects:");
                for d in parser_errors.iter().take(3) {
                    let at = d.span().map_or_else(String::new, |s| {
                        format!("{:?}", snippet(src, s.start, s.end.max(s.start + 1)))
                    });
                    let _ = write!(msg, "\n  {} at {at}: {}", d.code.as_str(), d.message);
                }
                check.disagreement = Some(msg);
            }
            Outcome::Accepted(tree) => {
                check.accepted = true;
                let grammar_shape = self.roles.shape(&tree);
                let parser_shape = ast_shape(&check.parsed.file);
                // Where the parser stopped at a limit, its tree holds an error node instead.
                if !check.limited && grammar_shape != parser_shape {
                    check.disagreement = Some(shape_diff(src, &grammar_shape, &parser_shape));
                }
                check.tree = Some(tree);
            }
        }
        check
    }

    /// Only the hand-written parser's verdict, for runs that check something else (the
    /// formatter) on programs known to be in the grammar: `accepted` is "no error at all".
    pub fn check_parser_only(&self, src: &str) -> Check {
        let parsed = parse(FileId(0), src);
        let errors = || parsed.diagnostics.iter().filter(|d| d.is_error());
        Check {
            accepted: errors().all(is_limit),
            limited: errors().any(is_limit),
            lexical_errors: false,
            disagreement: None,
            tree: None,
            parsed,
        }
    }
}

/// Whether a parser error is for one of its limits rather than for the syntax.
fn is_limit(d: &Diagnostic) -> bool {
    d.code == codes::E0112 || d.code == codes::E0006
}

fn snippet(src: &str, start: u32, end: u32) -> &str {
    let (s, e) = (start as usize, (end as usize).min(src.len()));
    src.get(s..e.max(s)).unwrap_or("<not on a char boundary>")
}

fn shape_diff(src: &str, grammar: &Shape, parser: &Shape) -> String {
    let mut msg = String::from("the grammar and the parser give different structures:");
    for (label, a, b) in [
        ("only in the grammar's parse", grammar, parser),
        ("only in the parser's", parser, grammar),
    ] {
        let only: Vec<_> = a.difference(b).collect();
        if !only.is_empty() {
            let _ = write!(msg, "\n  {label}:");
            for (s, e, cat) in only.iter().take(8) {
                let _ = write!(msg, "\n    {cat} {s}..{e} {:?}", snippet(src, *s, *e));
            }
            if only.len() > 8 {
                let _ = write!(msg, "\n    … and {} more", only.len() - 8);
            }
        }
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agrees(c: &Checker, src: &str) -> bool {
        let r = c.check(src, &mut Scratch::default());
        if let Some(d) = &r.disagreement {
            panic!("{src:?}: {d}");
        }
        r.accepted
    }

    #[test]
    fn small_programs_agree() {
        let c = Checker::default();
        assert!(agrees(&c, "fn f() { a - b - c * d ** -e ** f }"));
        assert!(agrees(&c, "fn f() -> Option<Option<T>> { let x: A<B<C>>= y; x.0.1 }"));
        assert!(agrees(&c, "fn f() { match x { (A(_)) | ((b)) => 1, _ => 2 } }"));
        assert!(!agrees(&c, "fn f() { a < b < c }"));
        assert!(!agrees(&c, "fn f() { a.b(c }"));
    }

    /// The parser's limits aren't the grammar's: the oracle accepts, and that's agreement.
    #[test]
    fn limits_are_left_out() {
        let c = Checker::default();
        let deep = format!("fn f() {{ {}1{} }}", "(".repeat(200), ")".repeat(200));
        for src in ["fn f() { t.4294967296 }", deep.as_str()] {
            let r = crate::testing::with_big_stack(|| c.check(src, &mut Scratch::default()));
            assert_eq!(r.disagreement, None, "{src}");
            assert!(r.accepted && r.limited, "{src}");
        }
    }

    #[test]
    fn a_wrong_shape_is_reported() {
        let g: Shape = [(0, 5, crate::canon::Cat::Expr)].into_iter().collect();
        let p: Shape = [(0, 3, crate::canon::Cat::Expr)].into_iter().collect();
        let d = shape_diff("a - b", &g, &p);
        assert!(d.contains("only in the grammar's parse:\n    Expr 0..5 \"a - b\""), "{d}");
        assert!(d.contains("only in the parser's:\n    Expr 0..3 \"a -\""), "{d}");
    }

    /// The formatter round trip compares skeletons: they must ignore layout, not structure.
    #[test]
    fn skeletons_ignore_spans() {
        use wrela_syntax::fmt::skeleton;
        let a = parse(FileId(0), "fn f() { 1 + 2 }");
        let b = parse(FileId(0), "fn f() {\n    1 +\n        2\n}\n");
        assert_eq!(skeleton(&a.file), skeleton(&b.file));
        let c = parse(FileId(0), "fn f() { 1 - 2 }");
        assert_ne!(skeleton(&a.file), skeleton(&c.file));
    }
}
