//! The differential check between the oracle and the hand-written parser on one source text.
//!
//! **Agreement**, for a source text, means all of:
//!
//! 1. Both parse the token stream of `wrela_syntax::lex`, so they see the same tokens,
//!    NEWLINEs included.
//! 2. The oracle accepts exactly when the hand-written parser reports no error of its own.
//!    Lexical diagnostics are left out on both sides: the lexer is shared, and the grammar is
//!    about the tokens it produced.
//! 3. When the oracle accepts, its derivation is unique: an ambiguous grammar is a bug even if
//!    the parser happens to pick the intended reading.
//! 4. When both accept, the oracle's parse tree and the parser's AST have the same
//!    [shape](crate::canon): the same boundaries for every item, member, parameter, field,
//!    variant, use tree, block, statement, arm, expression, pattern and type, which includes
//!    the precedence and associativity of every operator.
//!
//! Anything else is a disagreement, and a bug in the grammar or the parser.

use crate::canon::{Roles, Shape, ast_shape};
use crate::earley::{Oracle, Outcome, Scratch};
use crate::ebnf::Grammar;
use std::fmt::Write as _;
use wrela_diag::FileId;
use wrela_syntax::{fmt, parse, parser};

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
    /// The lexer reported an error (the parsers were compared anyway).
    pub lexical_errors: bool,
    /// Why the two disagree, if they do.
    pub disagreement: Option<String>,
    /// The rules the oracle's parse tree uses, when it accepted.
    pub rules_used: Vec<usize>,
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
        let lexical_errors = lexed.diagnostics.iter().any(|d| d.is_error());
        let outcome = self.oracle.parse(&lexed.tokens, scratch);
        let parsed = parser::parse_tokens(FileId(0), src, lexed);
        let parser_errors: Vec<_> =
            parsed.diagnostics[lex_diags..].iter().filter(|d| d.is_error()).collect();
        let hand_accepts = parser_errors.is_empty();
        let mut check =
            Check { accepted: false, lexical_errors, disagreement: None, rules_used: Vec::new() };
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
                    let _ = write!(
                        msg,
                        "\n  {} at {:?}: {}",
                        d.code.as_str(),
                        snippet(
                            src,
                            d.primary.span.start,
                            d.primary.span.end.max(d.primary.span.start + 1)
                        ),
                        d.message
                    );
                }
                check.disagreement = Some(msg);
            }
            Outcome::Accepted(tree) => {
                check.accepted = true;
                tree.walk(&mut |n| check.rules_used.push(n.rule));
                let grammar_shape = self.roles.shape(&tree);
                let parser_shape = ast_shape(&parsed.file);
                if grammar_shape != parser_shape {
                    check.disagreement = Some(shape_diff(src, &grammar_shape, &parser_shape));
                }
            }
        }
        check
    }

    /// Only the hand-written parser's verdict, for runs that check something else (the
    /// formatter) on programs known to be in the grammar: `accepted` is "no error at all".
    pub fn check_parser_only(&self, src: &str) -> Check {
        let parsed = parse(FileId(0), src);
        Check {
            accepted: !parsed.has_errors(),
            lexical_errors: false,
            disagreement: None,
            rules_used: Vec::new(),
        }
    }
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

/// The AST's Debug output with spans removed: two sources with equal skeletons parsed to the
/// same tree.
pub fn ast_skeleton(file: &wrela_syntax::ast::File) -> String {
    let text = format!("{file:?}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(i) = rest.find("Span {") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let len = tail.find('}').map_or(tail.len(), |j| j + 1);
        out.push('_');
        rest = &tail[len..];
    }
    out.push_str(rest);
    out
}

/// Parse → format → parse gives the same AST, and formatting is idempotent. For sources the
/// parser accepts without any error.
pub fn round_trip(src: &str) -> Result<(), String> {
    let p1 = parse(FileId(0), src);
    if p1.has_errors() {
        return Err("round_trip on a source with errors".into());
    }
    let once = fmt::format(&p1, src);
    let p2 = parse(FileId(0), &once);
    if p2.has_errors() {
        let d = p2.diagnostics.iter().find(|d| d.is_error()).map(|d| d.message.clone());
        return Err(format!("the formatted source doesn't parse ({d:?}):\n{once}"));
    }
    if ast_skeleton(&p1.file) != ast_skeleton(&p2.file) {
        return Err(format!("formatting changed the AST; formatted:\n{once}"));
    }
    let twice = fmt::format(&p2, &once);
    if once != twice {
        return Err(format!("formatting isn't idempotent; once:\n{once}\ntwice:\n{twice}"));
    }
    Ok(())
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

    #[test]
    fn a_wrong_shape_is_reported() {
        let g: Shape = [(0, 5, crate::canon::Cat::Expr)].into_iter().collect();
        let p: Shape = [(0, 3, crate::canon::Cat::Expr)].into_iter().collect();
        let d = shape_diff("a - b", &g, &p);
        assert!(d.contains("only in the grammar's parse:\n    Expr 0..5 \"a - b\""), "{d}");
        assert!(d.contains("only in the parser's:\n    Expr 0..3 \"a -\""), "{d}");
    }

    #[test]
    fn skeletons_ignore_spans() {
        let a = parse(FileId(0), "fn f() { 1 + 2 }");
        let b = parse(FileId(0), "fn f() {\n    1 +\n        2\n}\n");
        assert_eq!(ast_skeleton(&a.file), ast_skeleton(&b.file));
        let c = parse(FileId(0), "fn f() { 1 - 2 }");
        assert_ne!(ast_skeleton(&a.file), ast_skeleton(&c.file));
    }
}
