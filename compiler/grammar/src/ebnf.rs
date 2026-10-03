//! Reads spec/grammar.ebnf: the dialect its header documents, into a [`Grammar`].
//!
//! The reader is strict, because a grammar that quietly means something other than what it says
//! makes every test built on it meaningless. It rejects, with the line:
//!
//! - syntax errors in the dialect, and a rule name that isn't `lowercase_with_underscores`;
//! - a rule defined twice, a reference to an undefined rule, an unknown token class, and a quoted
//!   terminal that isn't a keyword or punctuation token of spec/lexical.md;
//! - a grammar without the start rule `file`, a rule `file` can't reach, and a rule that can't
//!   derive any finite token sequence;
//! - an optional or repeated part that can already be empty (`(a?)*`, `(a*)?`), and a rule that
//!   can derive itself without consuming a token: both make the grammar infinitely ambiguous.

use std::collections::HashMap;
use std::fmt;
use wrela_syntax::TokenKind;

/// Index of a rule in [`Grammar::rules`].
pub type RuleId = usize;

/// Index of an expression node; numbered densely across the whole grammar, so tools can keep
/// per-node data (coverage counters, costs) in a flat vector.
pub type ExprId = usize;

/// A terminal of the grammar: a token, or GT_CLOSE (L16).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Terminal {
    Token(TokenKind),
    /// A `>` that closes a generic list; also the first `>` of `>>`, `>=` or `>>=` (L16).
    GtClose,
    /// An INT right after `.`: a tuple index. grammar.ebnf has no such terminal; the GBNF export
    /// tells these INTs apart, because nothing joins one to a `.` after it (L12).
    TupleIndex,
}

impl Terminal {
    /// The name the grammar uses: a class such as `IDENT`, or a keyword's or punctuation
    /// token's text.
    pub fn name(self) -> &'static str {
        match self {
            Terminal::Token(k) => k.grammar_name(),
            Terminal::GtClose => "GT_CLOSE",
            Terminal::TupleIndex => "INT",
        }
    }

    /// The fixed source text, for keywords, punctuation and GT_CLOSE (`>`).
    pub fn fixed_text(self) -> Option<&'static str> {
        match self {
            Terminal::Token(k) => k.fixed_text(),
            Terminal::GtClose => Some(">"),
            Terminal::TupleIndex => None,
        }
    }

    /// The lexer's kind for this terminal (GT_CLOSE is a `>`).
    pub fn lexer_kind(self) -> TokenKind {
        match self {
            Terminal::Token(k) => k,
            Terminal::GtClose => TokenKind::Gt,
            Terminal::TupleIndex => TokenKind::Int,
        }
    }
}

impl fmt::Display for Terminal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Terminal::Token(k) if k.fixed_text().is_some() => write!(f, "\"{}\"", k.grammar_name()),
            t => f.write_str(t.name()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub id: ExprId,
    pub kind: ExprKind,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Seq(Vec<Expr>),
    Alt(Vec<Expr>),
    Opt(Box<Expr>),
    Star(Box<Expr>),
    Plus(Box<Expr>),
    Term(Terminal),
    Rule(RuleId),
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub name: String,
    pub body: Expr,
    /// The 1-based line of the rule's head.
    pub line: u32,
}

#[derive(Clone, Debug)]
pub struct Grammar {
    pub rules: Vec<Rule>,
    pub start: RuleId,
    expr_count: usize,
}

/// A malformed grammar: where, and what's wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrammarError {
    pub line: u32,
    pub message: String,
}

impl fmt::Display for GrammarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "grammar line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for GrammarError {}

/// The text of spec/grammar.ebnf, compiled in (so a change to it rebuilds this crate).
pub const SPEC_SOURCE: &str = include_str!("../../../spec/grammar.ebnf");

/// The terminal a class name stands for: a token class of L21, or GT_CLOSE.
fn class_terminal(name: &str) -> Option<Terminal> {
    let classes = TokenKind::CLASSES.iter().map(|k| Terminal::Token(*k));
    classes.chain([Terminal::GtClose]).find(|t| t.name() == name)
}

impl Grammar {
    /// spec/grammar.ebnf. It's validated by this crate's tests, so a failure here is a bug in
    /// the checked-in grammar.
    pub fn spec() -> Grammar {
        match Grammar::parse(SPEC_SOURCE) {
            Ok(g) => g,
            Err(e) => panic!("spec/grammar.ebnf is malformed: {e}"),
        }
    }

    pub fn parse(src: &str) -> Result<Grammar, GrammarError> {
        let tokens = tokenize(src)?;
        let g = Reader::read(&tokens)?;
        g.validate()?;
        Ok(g)
    }

    pub fn rule_id(&self, name: &str) -> Option<RuleId> {
        self.rules.iter().position(|r| r.name == name)
    }

    /// How many expression nodes the grammar has; [`ExprId`]s are `0..expr_count()`.
    pub fn expr_count(&self) -> usize {
        self.expr_count
    }

    /// Every terminal the grammar mentions, in a stable order.
    pub fn terminals(&self) -> Vec<Terminal> {
        let mut out = Vec::new();
        for r in &self.rules {
            visit(&r.body, &mut |e| {
                if let ExprKind::Term(t) = e.kind {
                    out.push(t);
                }
            });
        }
        out.sort();
        out.dedup();
        out
    }

    /// Whether each rule can derive the empty sequence.
    pub fn nullable_rules(&self) -> Vec<bool> {
        let mut nullable = vec![false; self.rules.len()];
        loop {
            let mut changed = false;
            for (i, r) in self.rules.iter().enumerate() {
                if !nullable[i] && expr_nullable(&r.body, &nullable) {
                    nullable[i] = true;
                    changed = true;
                }
            }
            if !changed {
                return nullable;
            }
        }
    }

    /// The fewest terminals each rule can derive (`u32::MAX` for an unproductive rule).
    pub fn min_lengths(&self) -> Vec<u32> {
        let mut len = vec![u32::MAX; self.rules.len()];
        loop {
            let mut changed = false;
            for (i, r) in self.rules.iter().enumerate() {
                let l = expr_min_len(&r.body, &len);
                if l < len[i] {
                    len[i] = l;
                    changed = true;
                }
            }
            if !changed {
                return len;
            }
        }
    }

    fn validate(&self) -> Result<(), GrammarError> {
        let lengths = self.min_lengths();
        if let Some(r) = self.rules.iter().zip(&lengths).find(|(_, l)| **l == u32::MAX) {
            return Err(GrammarError {
                line: r.0.line,
                message: format!("the rule `{}` can't derive any finite token sequence", r.0.name),
            });
        }
        let mut reached = vec![false; self.rules.len()];
        let mut stack = vec![self.start];
        reached[self.start] = true;
        while let Some(r) = stack.pop() {
            visit(&self.rules[r].body, &mut |e| {
                if let ExprKind::Rule(id) = e.kind
                    && !reached[id]
                {
                    reached[id] = true;
                    stack.push(id);
                }
            });
        }
        if let Some((i, _)) = reached.iter().enumerate().find(|(_, r)| !**r) {
            return Err(GrammarError {
                line: self.rules[i].line,
                message: format!("the rule `{}` is never used", self.rules[i].name),
            });
        }
        let nullable = self.nullable_rules();
        for r in &self.rules {
            let mut bad = false;
            visit(&r.body, &mut |e| {
                if let ExprKind::Opt(x) | ExprKind::Star(x) | ExprKind::Plus(x) = &e.kind
                    && !bad
                    && expr_nullable(x, &nullable)
                {
                    bad = true;
                }
            });
            if bad {
                return Err(GrammarError {
                    line: r.line,
                    message: format!(
                        "in `{}`, an optional or repeated part can already be empty, which makes the grammar ambiguous",
                        r.name
                    ),
                });
            }
        }
        // A rule that derives itself without consuming anything: `a ::= b`, `b ::= a | ...`.
        let units: Vec<Vec<RuleId>> = self
            .rules
            .iter()
            .map(|r| {
                let mut out = Vec::new();
                unit_refs(&r.body, &nullable, &mut out);
                out
            })
            .collect();
        for start in 0..self.rules.len() {
            let mut seen = vec![false; self.rules.len()];
            let mut stack = units[start].clone();
            while let Some(r) = stack.pop() {
                if r == start {
                    let rule = &self.rules[start];
                    return Err(GrammarError {
                        line: rule.line,
                        message: format!(
                            "the rule `{}` can derive itself without consuming a token, which makes the grammar ambiguous",
                            rule.name
                        ),
                    });
                }
                if !seen[r] {
                    seen[r] = true;
                    stack.extend(&units[r]);
                }
            }
        }
        Ok(())
    }
}

/// Calls `f` on every node of `e`, parents first.
pub fn visit<'a>(e: &'a Expr, f: &mut impl FnMut(&'a Expr)) {
    f(e);
    match &e.kind {
        ExprKind::Seq(xs) | ExprKind::Alt(xs) => xs.iter().for_each(|x| visit(x, f)),
        ExprKind::Opt(x) | ExprKind::Star(x) | ExprKind::Plus(x) => visit(x, f),
        ExprKind::Term(_) | ExprKind::Rule(_) => {}
    }
}

pub fn expr_nullable(e: &Expr, nullable_rules: &[bool]) -> bool {
    match &e.kind {
        ExprKind::Seq(xs) => xs.iter().all(|x| expr_nullable(x, nullable_rules)),
        ExprKind::Alt(xs) => xs.iter().any(|x| expr_nullable(x, nullable_rules)),
        ExprKind::Opt(_) | ExprKind::Star(_) => true,
        ExprKind::Plus(x) => expr_nullable(x, nullable_rules),
        ExprKind::Term(_) => false,
        ExprKind::Rule(r) => nullable_rules[*r],
    }
}

/// The fewest terminals `e` derives, given the rules' minimums.
pub fn expr_min_len(e: &Expr, rule_len: &[u32]) -> u32 {
    match &e.kind {
        ExprKind::Seq(xs) => {
            xs.iter().fold(0u32, |a, x| a.saturating_add(expr_min_len(x, rule_len)))
        }
        ExprKind::Alt(xs) => xs.iter().map(|x| expr_min_len(x, rule_len)).min().unwrap_or(u32::MAX),
        ExprKind::Opt(_) | ExprKind::Star(_) => 0,
        ExprKind::Plus(x) => expr_min_len(x, rule_len),
        ExprKind::Term(_) => 1,
        ExprKind::Rule(r) => rule_len[*r],
    }
}

/// The rules that `e` can derive alone, everything around them deriving nothing.
fn unit_refs(e: &Expr, nullable: &[bool], out: &mut Vec<RuleId>) {
    match &e.kind {
        ExprKind::Seq(xs) => {
            for (i, x) in xs.iter().enumerate() {
                let rest_nullable =
                    xs.iter().enumerate().all(|(j, y)| j == i || expr_nullable(y, nullable));
                if rest_nullable {
                    unit_refs(x, nullable, out);
                }
            }
        }
        ExprKind::Alt(xs) => xs.iter().for_each(|x| unit_refs(x, nullable, out)),
        ExprKind::Opt(x) | ExprKind::Star(x) | ExprKind::Plus(x) => unit_refs(x, nullable, out),
        ExprKind::Term(_) => {}
        ExprKind::Rule(r) => out.push(*r),
    }
}

// ---- reading ---------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Name(String),
    Quoted(String),
    Define,
    Bar,
    LParen,
    RParen,
    Question,
    Star,
    Plus,
}

fn tokenize(src: &str) -> Result<Vec<(Tok, u32)>, GrammarError> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let line_no = i as u32 + 1;
        let err = |message: String| GrammarError { line: line_no, message };
        let b = line.as_bytes();
        let mut p = 0;
        while p < b.len() {
            let c = b[p];
            match c {
                b' ' | b'\t' | b'\r' => p += 1,
                b'#' => break,
                b'"' => {
                    let end = line[p + 1..]
                        .find('"')
                        .ok_or_else(|| err("a quoted terminal isn't closed".into()))?;
                    let text = &line[p + 1..p + 1 + end];
                    if text.is_empty() {
                        return Err(err("an empty quoted terminal".into()));
                    }
                    out.push((Tok::Quoted(text.to_string()), line_no));
                    p += end + 2;
                }
                b':' if line[p..].starts_with("::=") => {
                    out.push((Tok::Define, line_no));
                    p += 3;
                }
                b'|' | b'(' | b')' | b'?' | b'*' | b'+' => {
                    let tok = match c {
                        b'|' => Tok::Bar,
                        b'(' => Tok::LParen,
                        b')' => Tok::RParen,
                        b'?' => Tok::Question,
                        b'*' => Tok::Star,
                        _ => Tok::Plus,
                    };
                    out.push((tok, line_no));
                    p += 1;
                }
                c if c.is_ascii_alphabetic() || c == b'_' => {
                    let start = p;
                    while p < b.len() && (b[p].is_ascii_alphanumeric() || b[p] == b'_') {
                        p += 1;
                    }
                    out.push((Tok::Name(line[start..p].to_string()), line_no));
                }
                _ => {
                    let ch = line[p..].chars().next().unwrap_or('?');
                    return Err(err(format!("unexpected character `{ch}`")));
                }
            }
        }
    }
    Ok(out)
}

fn is_rule_name(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn is_class_name(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_uppercase())
        && s.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
}

/// Keywords and punctuation, by their text.
fn fixed_tokens() -> HashMap<&'static str, TokenKind> {
    TokenKind::KEYWORDS
        .iter()
        .chain(TokenKind::PUNCT)
        .filter_map(|k| k.fixed_text().map(|t| (t, *k)))
        .collect()
}

struct Reader<'a> {
    toks: &'a [(Tok, u32)],
    pos: usize,
    end: usize,
    names: HashMap<String, RuleId>,
    fixed: HashMap<&'static str, TokenKind>,
    next_id: ExprId,
}

impl<'a> Reader<'a> {
    fn read(toks: &'a [(Tok, u32)]) -> Result<Grammar, GrammarError> {
        // Pass 1: the rule heads (`name ::=`), so bodies can refer to rules defined later.
        let mut heads = Vec::new();
        let mut names = HashMap::new();
        for i in 0..toks.len() {
            if let (Tok::Name(n), line) = &toks[i]
                && matches!(toks.get(i + 1), Some((Tok::Define, _)))
            {
                if !is_rule_name(n) {
                    return Err(GrammarError {
                        line: *line,
                        message: format!("the rule name `{n}` isn't lowercase_with_underscores"),
                    });
                }
                if names.insert(n.clone(), heads.len()).is_some() {
                    return Err(GrammarError {
                        line: *line,
                        message: format!("the rule `{n}` is defined twice"),
                    });
                }
                heads.push((n.clone(), *line, i));
            }
        }
        if let Some((t, line)) = toks.first()
            && heads.first().is_none_or(|h| h.2 != 0)
        {
            return Err(GrammarError {
                line: *line,
                message: format!("expected a rule (`name ::= ...`), found {t:?}"),
            });
        }
        let mut reader = Reader { toks, pos: 0, end: 0, names, fixed: fixed_tokens(), next_id: 0 };
        let mut rules = Vec::new();
        for (k, (name, line, at)) in heads.iter().enumerate() {
            reader.pos = at + 2;
            reader.end = heads.get(k + 1).map_or(toks.len(), |h| h.2);
            if reader.pos == reader.end {
                return Err(GrammarError {
                    line: *line,
                    message: format!("the rule `{name}` has an empty body"),
                });
            }
            let body = reader.alt()?;
            if reader.pos != reader.end {
                return Err(reader.error("expected `|`, a name, a terminal or `(`"));
            }
            rules.push(Rule { name: name.clone(), body, line: *line });
        }
        let start = reader.names.get("file").copied().ok_or(GrammarError {
            line: 1,
            message: "the grammar has no start rule `file`".into(),
        })?;
        Ok(Grammar { rules, start, expr_count: reader.next_id })
    }

    fn line(&self) -> u32 {
        self.toks
            .get(self.pos)
            .or_else(|| self.toks.get(self.pos.saturating_sub(1)))
            .map_or(1, |t| t.1)
    }

    fn error(&self, what: &str) -> GrammarError {
        let found = match self.toks.get(self.pos) {
            Some((t, _)) if self.pos < self.end => format!("{t:?}"),
            _ => "the end of the rule".into(),
        };
        GrammarError { line: self.line(), message: format!("{what}, found {found}") }
    }

    fn peek(&self) -> Option<&Tok> {
        if self.pos < self.end { Some(&self.toks[self.pos].0) } else { None }
    }

    fn node(&mut self, kind: ExprKind) -> Expr {
        self.next_id += 1;
        Expr { id: self.next_id - 1, kind }
    }

    fn alt(&mut self) -> Result<Expr, GrammarError> {
        let mut alts = vec![self.seq()?];
        while self.peek() == Some(&Tok::Bar) {
            self.pos += 1;
            alts.push(self.seq()?);
        }
        Ok(if alts.len() == 1 { alts.remove(0) } else { self.node(ExprKind::Alt(alts)) })
    }

    fn seq(&mut self) -> Result<Expr, GrammarError> {
        let mut items = Vec::new();
        while matches!(self.peek(), Some(Tok::Name(_) | Tok::Quoted(_) | Tok::LParen)) {
            items.push(self.postfix()?);
        }
        if items.is_empty() {
            return Err(
                self.error("expected a name, a terminal or `(` (an alternative can't be empty)")
            );
        }
        Ok(if items.len() == 1 { items.remove(0) } else { self.node(ExprKind::Seq(items)) })
    }

    fn postfix(&mut self) -> Result<Expr, GrammarError> {
        let atom = self.atom()?;
        let wrap = match self.peek() {
            Some(Tok::Question) => ExprKind::Opt as fn(Box<Expr>) -> ExprKind,
            Some(Tok::Star) => ExprKind::Star,
            Some(Tok::Plus) => ExprKind::Plus,
            _ => return Ok(atom),
        };
        self.pos += 1;
        if matches!(self.peek(), Some(Tok::Question | Tok::Star | Tok::Plus)) {
            return Err(
                self.error("a second repetition operator in a row is ambiguous; use parentheses")
            );
        }
        Ok(self.node(wrap(Box::new(atom))))
    }

    fn atom(&mut self) -> Result<Expr, GrammarError> {
        let line = self.line();
        match self.peek().cloned() {
            Some(Tok::LParen) => {
                self.pos += 1;
                let e = self.alt()?;
                if self.peek() != Some(&Tok::RParen) {
                    return Err(self.error("expected `)`"));
                }
                self.pos += 1;
                Ok(e)
            }
            Some(Tok::Quoted(text)) => {
                self.pos += 1;
                let kind = self.fixed.get(text.as_str()).copied().ok_or_else(|| GrammarError {
                    line,
                    message: format!(
                        "\"{text}\" isn't a keyword or punctuation token (spec/lexical.md L11, L15)"
                    ),
                })?;
                Ok(self.node(ExprKind::Term(Terminal::Token(kind))))
            }
            Some(Tok::Name(n)) => {
                self.pos += 1;
                if is_class_name(&n) {
                    let t = class_terminal(&n).ok_or_else(|| GrammarError {
                        line,
                        message: format!("`{n}` isn't a token class (spec/lexical.md L21)"),
                    })?;
                    Ok(self.node(ExprKind::Term(t)))
                } else if is_rule_name(&n) {
                    let id = self.names.get(&n).copied().ok_or_else(|| GrammarError {
                        line,
                        message: format!("the rule `{n}` isn't defined"),
                    })?;
                    Ok(self.node(ExprKind::Rule(id)))
                } else {
                    Err(GrammarError {
                        line,
                        message: format!(
                            "`{n}` is neither a rule name (lowercase) nor a token class (UPPERCASE)"
                        ),
                    })
                }
            }
            _ => Err(self.error("expected a name, a terminal or `(`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(src: &str) -> String {
        match Grammar::parse(src) {
            Ok(_) => panic!("accepted a malformed grammar:\n{src}"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn the_spec_grammar_is_well_formed() {
        let g = Grammar::spec();
        assert_eq!(g.rules[g.start].name, "file");
        assert!(g.rules.len() > 50);
    }

    #[test]
    fn reads_the_dialect() {
        let g = Grammar::parse(
            "# comment\nfile ::= item* EOF   # trailing\nitem ::= \"fn\" IDENT (\"(\" | \"[\")? GT_CLOSE\n       | \"+=\"+\n",
        )
        .unwrap();
        assert_eq!(g.rules.len(), 2);
        assert!(matches!(g.rules[1].body.kind, ExprKind::Alt(ref a) if a.len() == 2));
        assert!(g.terminals().contains(&Terminal::GtClose));
    }

    #[test]
    fn rejects_malformed_grammars() {
        assert!(err("file ::= a EOF\na ::= IDENT\na ::= INT\n").contains("defined twice"));
        assert!(err("file ::= b EOF\n").contains("`b` isn't defined"));
        assert!(err("file ::= WORD EOF\n").contains("isn't a token class"));
        assert!(err("file ::= \"fnord\" EOF\n").contains("isn't a keyword or punctuation"));
        assert!(err("item ::= IDENT\n").contains("no start rule"));
        assert!(err("file ::= EOF\nunused ::= IDENT\n").contains("never used"));
        assert!(err("file ::= a EOF\na ::= \"(\" a \")\"\n").contains("finite"));
        assert!(err("file ::= (IDENT?)* EOF\n").contains("can already be empty"));
        assert!(err("file ::= a EOF\na ::= b | IDENT\nb ::= a\n").contains("derive itself"));
        assert!(err("file ::= IDENT** EOF\n").contains("second repetition"));
        assert!(err("file ::= ( IDENT EOF\n").contains("expected `)`"));
        assert!(err("file ::= IDENT | EOF |\n").contains("can't be empty"));
        assert!(err("File ::= EOF\n").contains("lowercase_with_underscores"));
        assert!(err("file ::= \"fn EOF\n").contains("isn't closed"));
        assert!(err("IDENT\nfile ::= EOF\n").contains("expected a rule"));
        assert!(err("file ::= EOF $\n").contains("unexpected character"));
    }
}
