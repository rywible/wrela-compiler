//! The hand-written recursive-descent parser. It accepts exactly the token sequences
//! spec/grammar.ebnf accepts (checked by the oracle tests in `wrela-grammar`); everything else
//! gets a diagnostic, and the parser recovers to report more than one error per file.
//!
//! Each `parse_*` function names the grammar rule it implements.

mod expr;

use crate::ast::*;
use crate::lexer::{Brackets, Lexed, line_break_is_newline};
use crate::token::{Comment, Token, TokenKind as T};
use wrela_diag::{Diagnostic, FileId, Span, codes};

/// A parsed file, with its comments (for the formatter) and every lexical and syntax error.
#[derive(Debug)]
pub struct Parsed {
    pub file: File,
    pub comments: Vec<Comment>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Parsed {
    pub fn has_errors(&self) -> bool {
        wrela_diag::has_errors(&self.diagnostics)
    }
}

pub fn parse(file: FileId, text: &str) -> Parsed {
    let lexed = crate::lexer::lex(file, text);
    parse_tokens(file, text, lexed)
}

pub fn parse_tokens(file: FileId, text: &str, lexed: Lexed) -> Parsed {
    let mut p = Parser {
        file,
        text,
        lifetimes: lexed.lifetimes,
        field_of: None,
        tokens: Tokens::new(file, lexed.tokens),
        brackets: (0, Brackets::default()),
        pos: 0,
        diags: lexed.diagnostics,
        syntax_errors: 0,
        nesting: 0,
        recovered_at: None,
        trait_type_here: false,
        closers: Vec::new(),
        print_fixes: Vec::new(),
    };
    p.tokens.reach(AHEAD);
    let f = p.parse_file();
    p.settle_print_fixes(&f);
    Parsed { file: f, comments: lexed.comments, diagnostics: p.diags }
}

/// Raised when a rule can't continue; the caller recovers.
#[derive(Debug)]
pub(crate) struct Failed;

pub(crate) type PResult<T> = Result<T, Failed>;

pub(crate) struct Parser<'a> {
    file: FileId,
    text: &'a str,
    tokens: Tokens,
    /// The brackets open (L19) after the first `.0` tokens, kept for [`Parser::read_as`].
    brackets: (usize, Brackets),
    pos: usize,
    diags: Vec<Diagnostic>,
    syntax_errors: u32,
    /// How deeply the rule being parsed is nested in others that nest (expressions, blocks,
    /// types, patterns, `use` groups).
    nesting: u32,
    /// The token position recovery last stopped at: an error there is already reported.
    pub(crate) recovered_at: Option<usize>,
    /// The closers of the brackets open around the rule being parsed, innermost last: the
    /// brackets L19 counts as open. Recovery stops at one of these; any other closer closes
    /// nothing (L19) and is skipped.
    pub(crate) closers: Vec<T>,
    /// Whether the type about to be parsed is a whole parameter or return type, where a trait
    /// names a type: there `dyn Trait` reads as the trait, and removing `dyn` fixes it.
    trait_type_here: bool,
    /// Where in `diags` the fixes of `println!` and its kin are: each needs `std::io::print`,
    /// which only the whole module shows how to name (`settle_print_fixes`).
    print_fixes: Vec<usize>,
    /// Where the lexer found lifetimes (E0114).
    lifetimes: Vec<Span>,
    /// While a struct's fields are parsed: whether it had a lifetime, which makes it a
    /// `borrow struct` once fixed. A `&` in a field's type is fixed to fit.
    field_of: Option<bool>,
}

/// The tokens. The parser changes them as it reads: it splits a `>>` in two (L16), and reads
/// a stray closer as another (L19), which changes what's open after it and so the NEWLINEs
/// there (L17). So a NEWLINE is decided only once the parser reaches it: then reading a closer
/// as another redoes only the few tokens read ahead, not the rest of the file.
struct Tokens {
    file: FileId,
    /// The tokens reached, NEWLINEs decided.
    head: Vec<Token>,
    /// The rest, without NEWLINEs, last first.
    tail: Vec<Token>,
    /// The brackets open after `head`.
    open: Brackets,
}

impl Tokens {
    fn new(file: FileId, tokens: Vec<Token>) -> Tokens {
        let mut tail: Vec<Token> = tokens.into_iter().filter(|t| t.kind != T::Newline).collect();
        tail.reverse();
        let head = Vec::with_capacity(tail.len() + tail.len() / 4);
        Tokens { file, head, tail, open: Brackets::default() }
    }

    /// Whether a NEWLINE goes between `prev` and `next`, with `open` open (L17).
    fn newline(&self, open: &Brackets, prev: Option<&Token>, next: &Token) -> Option<Token> {
        let p = prev?;
        (next.line_break_before && line_break_is_newline(open.in_brace(), p.kind, next.kind)).then(
            || Token {
                kind: T::Newline,
                span: Span::new(self.file, p.span.end, p.span.end),
                line_break_before: false,
            },
        )
    }

    /// Reaches token `i` (or the end).
    fn reach(&mut self, i: usize) {
        while self.head.len() <= i
            && let Some(t) = self.tail.pop()
        {
            if let Some(n) = self.newline(&self.open, self.head.last(), &t) {
                self.head.push(n);
            }
            self.open.track(t.kind);
            self.head.push(t);
        }
    }

    /// Token `i`, deciding NEWLINEs on the way to it without keeping them if it hasn't been
    /// reached (the parser seldom looks that far ahead).
    fn get(&self, i: usize) -> Option<Token> {
        if let Some(t) = self.head.get(i) {
            return Some(*t);
        }
        let mut open = self.open.clone();
        let mut prev = self.head.last().copied();
        let mut at = self.head.len();
        for t in self.tail.iter().rev() {
            if let Some(n) = self.newline(&open, prev.as_ref(), t) {
                if at == i {
                    return Some(n);
                }
                at += 1;
            }
            if at == i {
                return Some(*t);
            }
            at += 1;
            open.track(t.kind);
            prev = Some(*t);
        }
        None
    }

    fn insert(&mut self, i: usize, t: Token) {
        self.reach(i.saturating_sub(1));
        self.head.insert(i, t);
    }

    /// The token at `at` has become another kind, with `open` open after it: the tokens after
    /// it are reached again.
    fn redo_after(&mut self, at: usize, open: Brackets) {
        while self.head.len() > at + 1
            && let Some(t) = self.head.pop()
        {
            if t.kind != T::Newline {
                self.tail.push(t);
            }
        }
        self.open = open;
    }
}

impl std::ops::Index<usize> for Tokens {
    type Output = Token;

    fn index(&self, i: usize) -> &Token {
        self.head.get(i).expect("a token the parser has reached")
    }
}

impl std::ops::IndexMut<usize> for Tokens {
    fn index_mut(&mut self, i: usize) -> &mut Token {
        self.head.get_mut(i).expect("a token the parser has reached")
    }
}

/// How many tokens past the current one the parser reads ahead (`nth`).
const AHEAD: usize = 3;

/// After this many syntax errors in one file, later ones are dropped: they're usually noise
/// from the first.
const MAX_SYNTAX_ERRORS: u32 = 20;

/// How deeply expressions, blocks, types and patterns may nest in one another (L19). The
/// parser and every pass after it recurse over the tree, so the limit keeps their stacks
/// bounded; no reasonable program comes near it.
pub const MAX_NESTING: u32 = 128;

/// How deep an expression's tree may be, counting each operand of a chain of binary operators
/// (`a + b + c` is two deep) (L19).
pub const MAX_EXPR_DEPTH: u32 = 512;

fn describe(kind: T) -> String {
    match kind {
        T::Ident => "a name".into(),
        T::Int | T::Float => "a number".into(),
        T::Suffixed => "a number with a unit".into(),
        T::Str => "a string".into(),
        T::Newline => "a line break".into(),
        T::Eof => "the end of the file".into(),
        k => format!("`{}`", k.fixed_text().unwrap_or("?")),
    }
}

impl<'a> Parser<'a> {
    // ---- token access ----------------------------------------------------------------------

    fn tok(&self) -> Token {
        self.tokens[self.pos]
    }

    fn kind(&self) -> T {
        self.tokens[self.pos].kind
    }

    fn nth(&self, k: usize) -> T {
        self.tokens.get(self.pos + k).map_or(T::Eof, |t| t.kind)
    }

    /// The token `k` ahead (the last one, EOF, past the end).
    pub(crate) fn token_at(&self, k: usize) -> Token {
        self.tokens.get(self.pos + k).unwrap_or_else(|| self.tokens[self.pos])
    }

    fn at(&self, kind: T) -> bool {
        self.kind() == kind
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn prev_span(&self) -> Span {
        if self.pos == 0 { self.span().shrink_to_start() } else { self.tokens[self.pos - 1].span }
    }

    fn bump(&mut self) -> Token {
        let t = self.tok();
        if t.kind != T::Eof {
            self.pos += 1;
            self.tokens.reach(self.pos + AHEAD);
        }
        t
    }

    fn eat(&mut self, kind: T) -> bool {
        self.eat_span(kind).is_some()
    }

    /// `pub` or `pub(package)`, if it's next. `package` isn't a keyword: it's a name here.
    fn vis(&mut self) -> Option<Vis> {
        let span = self.eat_span(T::Pub)?;
        let package = self.at(T::LParen) && self.nth(1) == T::Package && self.nth(2) == T::RParen;
        if !package {
            return Some(Vis { span, package: false });
        }
        for _ in 0..3 {
            self.bump();
        }
        Some(Vis { span: span.to(self.prev_span()), package: true })
    }

    /// Consumes a token of this kind if it's next, and returns its span.
    fn eat_span(&mut self, kind: T) -> Option<Span> {
        self.at(kind).then(|| self.bump().span)
    }

    fn text_of(&self, span: Span) -> &'a str {
        &self.text[span.range()]
    }

    /// The name a token spells.
    fn ident_of(&self, t: Token) -> Ident {
        Ident { name: self.text_of(t.span).to_string(), span: t.span }
    }

    /// Runs a rule that nests, unless the nesting is already at [`MAX_NESTING`].
    pub(crate) fn nested<R>(&mut self, f: impl FnOnce(&mut Self) -> PResult<R>) -> PResult<R> {
        if self.nesting >= MAX_NESTING {
            let span = self.span();
            self.error(
                Diagnostic::new(
                    codes::E0112,
                    span,
                    format!("this is nested more than {MAX_NESTING} levels deep"),
                )
                .with_help("move the inner parts into `let` bindings or functions"),
            );
            return Err(Failed);
        }
        self.nesting += 1;
        let r = f(self);
        self.nesting -= 1;
        r
    }

    /// E0112 for an expression tree deeper than [`MAX_EXPR_DEPTH`], starting at `span`.
    pub(crate) fn too_deep_error(&mut self, span: Span) {
        self.error(
            Diagnostic::new(
                codes::E0112,
                span,
                format!("this expression is more than {MAX_EXPR_DEPTH} levels deep"),
            )
            .with_help("split it into `let` bindings"),
        );
    }

    /// Runs `f` inside a bracket that `close` ends, which L19 counts as open: recovery in it
    /// stops at `close`.
    pub(crate) fn within<R>(&mut self, close: T, f: impl FnOnce(&mut Self) -> R) -> R {
        self.closers.push(close);
        let r = f(self);
        self.closers.pop();
        r
    }

    fn error(&mut self, d: Diagnostic) {
        self.syntax_errors += 1;
        if self.syntax_errors <= MAX_SYNTAX_ERRORS {
            self.diags.push(d);
        }
    }

    /// E0100 at the current token: "expected X, found Y".
    fn expected(&mut self, what: &str) -> Failed {
        let t = self.tok();
        let found = describe(t.kind);
        let mut d =
            Diagnostic::new(codes::E0100, t.span, format!("expected {what}, found {found}"));
        if t.kind == T::Newline {
            // A NEWLINE token is the line's end, which is where the error points.
            d = d.with_note("a line break ends a statement unless the line ends with an operator or the next line starts with `.` (L17)");
        }
        self.error(d);
        Failed
    }

    fn expect(&mut self, kind: T, what: &str) -> PResult<Token> {
        if self.at(kind) { Ok(self.bump()) } else { Err(self.expected(what)) }
    }

    fn ident(&mut self, what: &str) -> PResult<Ident> {
        if self.at(T::Ident) {
            let t = self.bump();
            Ok(self.ident_of(t))
        } else if self.kind().is_keyword() && !matches!(self.kind(), T::SelfValue | T::SelfType) {
            let t = self.tok();
            let kw = t.kind.fixed_text().unwrap_or("");
            self.error(
                Diagnostic::new(
                    codes::E0100,
                    t.span,
                    format!("expected {what}, found the keyword `{kw}`"),
                )
                .with_help(format!("`{kw}` is reserved; choose another name, such as `{kw}_`")),
            );
            Err(Failed)
        } else {
            Err(self.expected(what))
        }
    }

    /// Whether the next token starts with GT_CLOSE (L16).
    fn at_gt_close(&self) -> bool {
        matches!(self.kind(), T::Gt | T::Shr | T::Ge | T::ShrEq)
    }

    /// GT_CLOSE (L16): a `>`, or the first `>` of `>>`, `>=` or `>>=`.
    fn expect_gt_close(&mut self) -> PResult<Span> {
        let t = self.tok();
        let rest = match t.kind {
            T::Gt => {
                self.bump();
                return Ok(t.span);
            }
            T::Shr => T::Gt,
            T::Ge => T::Eq,
            T::ShrEq => T::Ge,
            _ => return Err(self.expected("`>`")),
        };
        // Split the token in two: the `>` that closes the list, consumed here, then the rest of
        // its text as the next token. (Consuming a real token keeps `prev_span` right, so the
        // spans of the types and paths that end here include their `>`.)
        let split = t.span.start + 1;
        let close = Span::new(self.file, t.span.start, split);
        self.tokens[self.pos] = Token { kind: T::Gt, span: close, ..t };
        let rest = Token {
            kind: rest,
            span: Span::new(self.file, split, t.span.end),
            line_break_before: false,
        };
        self.tokens.insert(self.pos + 1, rest);
        if self.brackets.0 > self.pos + 1 {
            // Not a bracket: what's open is the same.
            self.brackets.0 += 1;
        }
        self.bump();
        Ok(close)
    }

    fn at_sep(&self) -> bool {
        matches!(self.kind(), T::Newline | T::Semi)
    }

    fn skip_seps(&mut self) {
        while self.at_sep() {
            self.bump();
        }
    }

    /// The span from `start` to the last token consumed; empty at `start` if none was.
    fn since(&self, start: Span) -> Span {
        let prev = self.prev_span();
        if prev.end <= start.start { start.shrink_to_start() } else { start.to(prev) }
    }

    /// Skips to the next separator or closing `}` at this nesting depth (recovery).
    fn recover_to_sep(&mut self) {
        let mut depth = 0u32;
        loop {
            match self.kind() {
                T::Eof => break,
                T::Newline | T::Semi | T::RBrace if depth == 0 => break,
                // A closer of a bracket opened before the statement (L19).
                k @ (T::RParen | T::RBracket) if depth == 0 && self.closers.contains(&k) => break,
                T::LParen | T::LBracket | T::LBrace => depth += 1,
                T::RParen | T::RBracket | T::RBrace => depth = depth.saturating_sub(1),
                _ => {}
            }
            self.bump();
        }
        self.recovered_at = Some(self.pos);
    }

    /// An expression, or, if it fails to parse, an error node in its place after skipping to
    /// the end of the statement.
    pub(crate) fn expr_or_error(&mut self, f: impl FnOnce(&mut Self) -> PResult<Expr>) -> Expr {
        let start = self.span();
        f(self).unwrap_or_else(|Failed| {
            self.recover_to_sep();
            Expr::error(self.since(start))
        })
    }

    /// Whether the token closes a list or block being parsed (`close`, or an enclosing one's,
    /// which closes this one with it (L19)).
    pub(crate) fn at_closer(&self, close: T) -> bool {
        let k = self.kind();
        k == close
            || (matches!(k, T::RParen | T::RBracket | T::RBrace) && self.closers.contains(&k))
    }

    /// Whether the list that `close` ends ends here: at a closer (after a line break, in
    /// braces), or at the file's end.
    fn at_list_end(&self, close: T) -> bool {
        match self.kind() {
            T::Eof => true,
            T::Newline => close == T::RBrace && self.nth(1) == T::RBrace,
            _ => self.at_closer(close),
        }
    }

    /// Skips tokens, bracket groups whole, until `stop` holds at this depth, or a closer of
    /// the lists and blocks being parsed, or the file's end.
    fn skip_until(&mut self, close: T, stop: impl Fn(T) -> bool) {
        let mut depth = 0u32;
        loop {
            let k = self.kind();
            if k == T::Eof || (depth == 0 && (stop(k) || self.at_closer(close))) {
                break;
            }
            match k {
                T::LParen | T::LBracket | T::LBrace => depth += 1,
                // A closer with no opener here closes nothing (L19).
                T::RParen | T::RBracket | T::RBrace => depth = depth.saturating_sub(1),
                _ => {}
            }
            self.bump();
        }
        self.recovered_at = Some(self.pos);
    }

    /// Recovery inside a list that `close` ends: skips to the next `,` or the end of the list
    /// (neither consumed).
    fn recover_in_list(&mut self, close: T) {
        self.skip_until(close, |k| k == T::Comma || (k == T::Newline && close == T::RBrace));
    }

    /// The elements of a comma-separated list, up to the token that ends it (not consumed). An
    /// element that fails to parse is skipped up to the next `,` or the list's end, and
    /// replaced by what `broken` makes of it (given its first token's index and its span), if
    /// anything. In braces, a line break between two elements is a missing comma: reported,
    /// and read as one.
    pub(crate) fn list<E>(
        &mut self,
        close: T,
        mut elem: impl FnMut(&mut Self) -> PResult<E>,
        broken: impl Fn(&Self, usize, Span) -> Option<E>,
    ) -> Vec<E> {
        self.closers.push(close);
        let mut out = Vec::new();
        while !self.at_list_end(close) {
            let (first, start) = (self.pos, self.span());
            match elem(self) {
                Ok(e) => out.push(e),
                Err(Failed) => {
                    self.recover_in_list(close);
                    out.extend(broken(self, first, self.since(start)));
                }
            }
            if self.eat(T::Comma) {
                continue;
            }
            if close == T::RBrace && self.at(T::Newline) && self.nth(1) != T::RBrace {
                if self.recovered_at == Some(self.pos) {
                    self.bump();
                    continue;
                }
                let at = self.prev_span().shrink_to_end();
                let t = self.tok();
                self.error(
                    Diagnostic::new(
                        codes::E0100,
                        t.span,
                        "expected `,` or `}`, found a line break",
                    )
                    .with_help("separate the entries with commas")
                    .with_fix("add a comma", at, ","),
                );
                self.bump();
                continue;
            }
            break;
        }
        self.closers.pop();
        out
    }

    /// The token that closes a list (after a line break, in braces): consumed, or reported
    /// (unless recovery just stopped here) and skipped to. Returns where the list ends.
    pub(crate) fn close_list(&mut self, close: T, what: &str) -> Span {
        if close == T::RBrace && self.at(T::Newline) && self.nth(1) == T::RBrace {
            self.bump();
        }
        if self.at(close) {
            return self.bump().span;
        }
        let t = self.tok();
        let (want, found) = (close.fixed_text().unwrap_or("?"), t.kind.fixed_text().unwrap_or("?"));
        let at_closer = matches!(t.kind, T::RParen | T::RBracket | T::RBrace) && close != T::Pipe;
        // A closer of another kind that closes nothing open (L19): most likely a typo for
        // this one, so it's read as this one.
        if at_closer && !self.closers.contains(&t.kind) {
            let mut d = Diagnostic::new(
                codes::E0102,
                t.span,
                format!("`{found}` doesn't close anything here: expected `{want}`"),
            )
            .with_fix(format!("close with `{want}`"), t.span, want);
            // (Only for an error that's kept: finding it reads back through the file.)
            if self.syntax_errors < MAX_SYNTAX_ERRORS
                && let Some(open) = self.opener_of(close)
            {
                d = d.with_secondary(open, "opened here");
            }
            self.error(d);
            self.read_as(close);
            self.bump();
            return t.span;
        }
        if self.recovered_at != Some(self.pos) {
            if at_closer {
                // It closes a bracket opened before this list, and this list with it (L19).
                self.error(
                    Diagnostic::new(
                        codes::E0100,
                        t.span,
                        format!("expected {what}, found `{found}`"),
                    )
                    .with_fix(
                        format!("close with `{want}` first"),
                        t.span.shrink_to_start(),
                        want,
                    ),
                );
            } else {
                self.expected(what);
            }
        }
        self.skip_until(close, |_| false);
        if self.at(close) { self.bump().span } else { self.prev_span() }
    }

    /// The edits that join the line of the token at `a` to that of the token at `b` with one
    /// space. The comments between them can't stay there, so they move to lines of their own
    /// before `a`'s line.
    fn join_lines(&self, a: Span, b: Span) -> Vec<wrela_diag::Edit> {
        let between = &self.text[a.end as usize..b.start as usize];
        let line_start = self.text[..a.start as usize].rfind('\n').map_or(0, |i| i + 1);
        let line = &self.text[line_start..a.start as usize];
        let indent = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
        // Only space and comments lie between two tokens, so each `//` starts a comment.
        let moved: String = between
            .lines()
            .filter_map(|l| l.find("//").map(|i| format!("{indent}{}\n", l[i..].trim_end())))
            .collect();
        let mut edits = Vec::new();
        if !moved.is_empty() {
            let at = Span::new(self.file, line_start as u32, line_start as u32);
            edits.push(wrela_diag::Edit { span: at, replacement: moved });
        }
        let gap = Span::new(self.file, a.end, b.start);
        edits.push(wrela_diag::Edit { span: gap, replacement: " ".into() });
        edits
    }

    /// Reads the current token, a closer that closes nothing (L19), as `close`. What's open
    /// after it changes, and so the NEWLINEs after it (L17): the tokens read ahead are redone.
    fn read_as(&mut self, close: T) {
        let at = self.pos;
        self.tokens[at].kind = close;
        // What's open before `at`, from where the last call left off.
        let (k, mut open) = std::mem::take(&mut self.brackets);
        if k > at {
            open = Brackets::default();
        }
        for i in k.min(at)..at {
            open.track(self.tokens[i].kind);
        }
        open.track(close);
        self.brackets = (at + 1, open.clone());
        self.tokens.redo_after(at, open);
        self.tokens.reach(at + AHEAD);
    }

    /// The bracket that `close` would close: the nearest unclosed one of its kind before the
    /// current token.
    fn opener_of(&self, close: T) -> Option<Span> {
        let open = close.opener()?;
        let mut depth = 0u32;
        for i in (0..self.pos.min(self.tokens.head.len())).rev() {
            let t = &self.tokens[i];
            if t.kind == close {
                depth += 1;
            } else if t.kind == open {
                if depth == 0 {
                    return Some(t.span);
                }
                depth -= 1;
            }
        }
        None
    }

    /// The name of a broken list element that starts `pub? IDENT :`, for keeping the element.
    pub(crate) fn name_at(&self, first: usize) -> Option<Ident> {
        let i = if self.tokens.get(first).is_some_and(|t| t.kind == T::Pub) {
            first + 1
        } else {
            first
        };
        let (t, colon) = (self.tokens.get(i)?, self.tokens.get(i + 1)?);
        (t.kind == T::Ident && colon.kind == T::Colon).then(|| self.ident_of(t))
    }

    // ---- file and items --------------------------------------------------------------------

    /// file ::= sep* (item (sep+ item)* sep*)? EOF
    fn parse_file(&mut self) -> File {
        let start = self.span();
        let mut items = Vec::new();
        self.skip_seps();
        while !self.at(T::Eof) {
            let (before, start) = (self.pos, self.span());
            let broken = |p: &Self| Item {
                attrs: Vec::new(),
                vis: None,
                kind: ItemKind::Error(p.item_name_at(before)),
                span: p.since(start),
            };
            match self.parse_item() {
                Ok(item) => {
                    if let Some(span) = too_deep(&item) {
                        self.too_deep_error(span);
                        items.push(Item {
                            kind: ItemKind::Error(item.kind.name().cloned()),
                            ..item
                        });
                    } else {
                        items.push(item);
                    }
                    if !self.at_sep() && !self.at(T::Eof) {
                        self.missing_sep("an item");
                        let rest = self.span();
                        self.recover_to_item();
                        let span = self.since(rest);
                        items.push(Item {
                            attrs: Vec::new(),
                            vis: None,
                            kind: ItemKind::Error(None),
                            span,
                        });
                    }
                }
                Err(Failed) => {
                    self.recover_to_item();
                    items.push(broken(self));
                }
            }
            self.skip_seps();
            if self.pos == before && !self.at(T::Eof) {
                self.bump();
            }
        }
        File { items, span: start.to(self.span()) }
    }

    /// Completes each fix of `println!` and its kin, which calls `print`, from the module's
    /// names. A module that imports `std::io::print` already needs nothing more. In one that
    /// declares or imports another `print`, the call names `std::io::print` in full. Any other
    /// module gets the import.
    fn settle_print_fixes(&mut self, file: &File) {
        if self.print_fixes.is_empty() {
            return;
        }
        let (mut imported, mut taken) = (false, false);
        let mut name = |path: &[&str], name: &str| {
            if name == "print" {
                if path == ["std", "io", "print"] { imported = true } else { taken = true }
            }
        };
        for item in &file.items {
            match &item.kind {
                ItemKind::Use(u) => use_bindings(u, &mut Vec::new(), &mut name),
                k => k.name().into_iter().for_each(|n| name(&[], &n.name)),
            }
        }
        let import = wrela_diag::Edit {
            span: Span::new(self.file, 0, 0),
            replacement: "use std::io::print\n\n".into(),
        };
        for &i in &self.print_fixes {
            let fix = &mut self.diags[i].fixes[0];
            if taken {
                fix.edits[0].replacement.insert_str(0, "std::io::");
            } else if !imported {
                fix.edits.insert(0, import.clone());
            }
        }
    }

    /// The name of the item whose first token is at `first`, if it got that far: after its
    /// attributes and `pub`, a keyword and an identifier.
    fn item_name_at(&self, first: usize) -> Option<Ident> {
        let mut i = first;
        let mut depth = 0u32;
        while let Some(t) = self.tokens.get(i) {
            match t.kind {
                // Attributes and their arguments.
                T::LParen => depth += 1,
                T::RParen => depth = depth.saturating_sub(1),
                _ if depth > 0 => {}
                T::At | T::Pub | T::Newline => {}
                T::Ident if self.tokens.get(i.wrapping_sub(1)).is_some_and(|p| p.kind == T::At) => {
                }
                T::Borrow => {}
                T::Fn | T::Struct | T::Enum | T::Trait | T::Const | T::Type => {
                    let n = self.tokens.get(i + 1)?;
                    return (n.kind == T::Ident).then(|| self.ident_of(n));
                }
                _ => return None,
            }
            i += 1;
        }
        None
    }

    fn missing_sep(&mut self, what: &str) {
        let t = self.tok();
        let at = self.prev_span().shrink_to_end();
        let d = Diagnostic::new(
            codes::E0104,
            t.span,
            format!("expected a line break or `;` after {what}, found {}", describe(t.kind)),
        );
        // A new line helps only before something that can start a statement or an item.
        let starts = self.starts_expr()
            || matches!(
                t.kind,
                T::Let
                    | T::Var
                    | T::Borrow
                    | T::For
                    | T::While
                    | T::Loop
                    | T::Fn
                    | T::Struct
                    | T::Enum
                    | T::Trait
                    | T::Impl
                    | T::Const
                    | T::Use
                    | T::Type
                    | T::Pub
                    | T::At
            );
        self.error(match t.kind {
            T::As => d.with_help("a conversion is a call of the type: `f32(x)`, not `x as f32`"),
            T::DotDot | T::DotDotEq => {
                d.with_help("a range is written only in a `for` loop: `for i in 0..10`")
            }
            _ if starts => d.with_fix("start a new line here", at, "\n"),
            _ => d,
        });
    }

    /// Skips to a token that can start an item, at the top level, after a separator.
    fn recover_to_item(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.kind() {
                T::Eof => return,
                T::LParen | T::LBracket | T::LBrace => depth += 1,
                T::RParen | T::RBracket | T::RBrace => depth = (depth - 1).max(0),
                T::Newline | T::Semi if depth == 0 => {
                    self.bump();
                    if matches!(
                        self.kind(),
                        T::Fn
                            | T::Struct
                            | T::Enum
                            | T::Trait
                            | T::Impl
                            | T::Const
                            | T::Use
                            | T::Type
                            | T::Borrow
                            | T::Pub
                            | T::At
                    ) {
                        return;
                    }
                    continue;
                }
                _ => {}
            }
            self.bump();
        }
    }

    fn parse_attrs(&mut self) -> PResult<Vec<Attribute>> {
        let mut attrs = Vec::new();
        while self.at(T::At) {
            attrs.push(self.parse_attribute()?);
        }
        Ok(attrs)
    }

    /// attribute ::= "@" IDENT call_args? NEWLINE?
    fn parse_attribute(&mut self) -> PResult<Attribute> {
        let at = self.bump().span;
        let name = self.ident("an attribute name")?;
        let args = if self.at(T::LParen) { Some(self.parse_call_args()?.0) } else { None };
        let span = at.to(self.prev_span());
        self.eat(T::Newline);
        Ok(Attribute { name, args, span })
    }

    /// item ::= attribute* (pub_item | impl_item)
    fn parse_item(&mut self) -> PResult<Item> {
        let start = self.span();
        let attrs = self.parse_attrs()?;
        let mut vis = self.vis();
        let kind = match self.kind() {
            T::Fn => ItemKind::Fn(self.parse_fn(true)?),
            T::Struct => ItemKind::Struct(self.parse_struct(false)?),
            T::Borrow if self.nth(1) == T::Struct => {
                self.bump();
                ItemKind::Struct(self.parse_struct(true)?)
            }
            T::Type => ItemKind::TypeAlias(self.parse_type_alias()?),
            T::Enum => ItemKind::Enum(self.parse_enum()?),
            T::Trait => self.parse_trait()?,
            T::Const => ItemKind::Const(self.parse_const()?),
            T::Use => {
                self.bump();
                ItemKind::Use(self.parse_use_tree()?)
            }
            T::Impl => {
                if let Some(Vis { span: pub_span, .. }) = vis.take() {
                    // Reported, and read without the `pub`, so its members are still there.
                    self.error(
                        Diagnostic::new(codes::E0100, pub_span, "an `impl` can't be `pub`")
                            .with_help("its items carry their own visibility")
                            .with_fix(
                                "remove `pub`",
                                pub_span.to(self.span().shrink_to_start()),
                                "",
                            ),
                    );
                }
                ItemKind::Impl(self.parse_impl()?)
            }
            T::Let | T::Var => {
                let t = self.tok();
                self.error(
                    Diagnostic::new(codes::E0213, t.span, "a file can't hold `let` or `var` bindings")
                        .with_help("wrela has no globals; use `const NAME = value` for a constant, or pass state in as a parameter"),
                );
                return Err(Failed);
            }
            _ => {
                return Err(self.expected(
                    "an item (`fn`, `struct`, `enum`, `trait`, `impl`, `const`, `type` or `use`)",
                ));
            }
        };
        Ok(Item { attrs, vis, kind, span: start.to(self.prev_span()) })
    }

    /// fn_item ::= fn_sig block; with `body_required` false, the block is optional (trait
    /// members).
    fn parse_fn(&mut self, body_required: bool) -> PResult<FnDecl> {
        let start = self.expect(T::Fn, "`fn`")?.span;
        let name = self.ident("a function name")?;
        let generics = self.opt_generic_params()?;
        self.expect(T::LParen, "`(`")?;
        let params = self.list(T::RParen, Self::parse_param, |p, first, span| {
            // `x: <broken type>`, or `x` with its type missing; `mut` or `take` may come first.
            let mode_at = |i: usize| match p.tokens.get(i).map(|t| t.kind) {
                Some(T::Mut) => Mode::Mut,
                Some(T::Take) => Mode::Take,
                _ => Mode::Borrow,
            };
            let mut mode = mode_at(first);
            let first = first + usize::from(mode != Mode::Borrow);
            if mode == Mode::Borrow && p.tokens.get(first + 1).is_some_and(|t| t.kind == T::Colon) {
                mode = mode_at(first + 2);
            }
            let t = p.tokens[first];
            let name = p.name_at(first).or_else(|| (t.kind == T::Ident).then(|| p.ident_of(t)))?;
            let ty = TypeExpr::error(span);
            Some(Param::Named { name, mode, ty, default: None, span })
        });
        let params_close = self.close_list(T::RParen, "`,` or `)`");
        let ret = if self.eat(T::Arrow) {
            let mode = if self.eat(T::Borrow) {
                RetMode::Borrow
            } else if self.eat(T::Mut) {
                RetMode::Mut
            } else {
                RetMode::Owned
            };
            Some(RetType { mode, ty: self.parse_trait_position_type()? })
        } else {
            None
        };
        let sig_span = start.to(self.prev_span());
        let body = if self.at(T::LBrace) {
            Some(self.parse_block()?)
        } else if body_required {
            return Err(self.expected("`{` to start the function's body"));
        } else {
            None
        };
        Ok(FnDecl { name, generics, params, params_close, ret, body, sig_span })
    }

    /// generic_params?
    fn opt_generic_params(&mut self) -> PResult<Vec<GenericParam>> {
        if self.at(T::Lt) { self.parse_generic_params() } else { Ok(Vec::new()) }
    }

    /// generic_params ::= "<" (generic_param ("," generic_param)* ","?)? GT_CLOSE;
    /// generic_param ::= IDENT (":" bounds)? | "const" IDENT ":" type
    fn parse_generic_params(&mut self) -> PResult<Vec<GenericParam>> {
        self.expect(T::Lt, "`<`")?;
        let mut out = Vec::new();
        while !self.at_gt_close() {
            if self.eat(T::Const) {
                let name = self.ident("a constant parameter name")?;
                self.expect(T::Colon, "`:` and the constant's type")?;
                let ty = self.parse_type()?;
                out.push(GenericParam { name, bounds: Vec::new(), const_ty: Some(ty) });
                if !self.eat(T::Comma) {
                    break;
                }
                continue;
            }
            let name = self.ident("a generic parameter name")?;
            let bounds = self.opt_bounds()?;
            out.push(GenericParam { name, bounds, const_ty: None });
            if !self.eat(T::Comma) {
                break;
            }
        }
        self.expect_gt_close()?;
        Ok(out)
    }

    /// (":" bounds)?
    fn opt_bounds(&mut self) -> PResult<Vec<TypeExpr>> {
        if self.eat(T::Colon) { self.parse_bounds() } else { Ok(Vec::new()) }
    }

    /// bounds ::= bound ("+" bound)*
    fn parse_bounds(&mut self) -> PResult<Vec<TypeExpr>> {
        let mut out = vec![self.parse_bound()?];
        while self.eat(T::Plus) {
            out.push(self.parse_bound()?);
        }
        Ok(out)
    }

    /// bound ::= path_type | fn_type: a trait, or a function type a parameter's values are
    /// called as (`F: fn(vec3) -> f32`).
    fn parse_bound(&mut self) -> PResult<TypeExpr> {
        if self.at(T::Fn) || self.at(T::At) { self.parse_fn_type() } else { self.parse_path_type() }
    }

    /// param ::= mode? "self" | IDENT ":" mode? type ("=" expr)?
    fn parse_param(&mut self) -> PResult<Param> {
        // `&self`, `&mut self`: reported, then read without the `&`.
        if self.at(T::Amp)
            && (self.nth(1) == T::SelfValue
                || (self.nth(1) == T::Mut && self.nth(2) == T::SelfValue))
        {
            let amp = self.bump().span;
            self.error(
                Diagnostic::new(codes::E0106, amp, "`&` isn't a type in wrela")
                    .with_note(
                        "a method borrows `self` by default, and takes `mut self` to change it",
                    )
                    .with_fix("remove the `&`", amp, ""),
            );
        }
        let start = self.span();
        let mode = self.parse_mode();
        if self.at(T::SelfValue) {
            let end = self.bump().span;
            return Ok(Param::SelfParam { mode, span: start.to(end) });
        }
        if mode != Mode::Borrow {
            if self.at(T::Ident) && self.nth(1) == T::Colon {
                return self.misplaced_mode(start, mode);
            }
            return Err(self.expected("`self`"));
        }
        let name = self.ident("a parameter name")?;
        if !self.at(T::Colon) {
            self.error(
                Diagnostic::new(
                    codes::E0109,
                    name.span,
                    format!("the parameter `{}` needs a type", name.name),
                )
                .with_help(format!("write `{}: Type`", name.name)),
            );
            return Err(Failed);
        }
        self.bump();
        let mode = self.parse_mode();
        let ty = self.parse_trait_position_type()?;
        let default = self.opt_default()?;
        Ok(Param::Named { name, mode, ty, default, span: start.to(self.prev_span()) })
    }

    /// ("=" expr)?: a parameter's or a field's default.
    fn opt_default(&mut self) -> PResult<Option<Expr>> {
        if self.eat(T::Eq) { self.parse_expr().map(Some) } else { Ok(None) }
    }

    /// (":" type)?
    fn opt_colon_type(&mut self) -> PResult<Option<TypeExpr>> {
        if self.eat(T::Colon) { self.parse_type().map(Some) } else { Ok(None) }
    }

    /// `mut x: T`, which is `x: mut T`: reported with the fix, and parsed as meant.
    fn misplaced_mode(&mut self, start: Span, mode: Mode) -> PResult<Param> {
        let name = self.ident("a parameter name")?;
        self.bump();
        let ty_at = self.span().shrink_to_start();
        let written = self.parse_mode();
        let kw = mode.keyword();
        let mut d = Diagnostic::new(
            codes::E0100,
            start,
            format!("a parameter's mode is written with its type: `{}: {kw} T`", name.name),
        );
        if written == Mode::Borrow {
            let remove = Span { end: name.span.start, ..start };
            d = d.with_fix_edits(
                format!("write `{}: {kw} ..`", name.name),
                vec![
                    wrela_diag::Edit { span: remove, replacement: String::new() },
                    wrela_diag::Edit { span: ty_at, replacement: format!("{kw} ") },
                ],
            );
        }
        self.error(d);
        let ty = self.parse_type()?;
        let default = self.opt_default()?;
        Ok(Param::Named { name, mode, ty, default, span: start.to(self.prev_span()) })
    }

    /// mode ::= "mut" | "take" (borrow is the default and isn't written)
    fn parse_mode(&mut self) -> Mode {
        if self.eat(T::Mut) {
            Mode::Mut
        } else if self.eat(T::Take) {
            Mode::Take
        } else {
            Mode::Borrow
        }
    }

    /// struct_item ::= "struct" IDENT generic_params? (":" bounds)? field_block;
    /// borrow_struct_item ::= "borrow" "struct" IDENT generic_params? borrow_fields (`borrow`
    /// already read)
    fn parse_struct(&mut self, borrow: bool) -> PResult<StructDecl> {
        self.expect(T::Struct, "`struct`")?;
        let name = self.ident("a struct name")?;
        let generics = self.opt_generic_params()?;
        // `struct P(f32, f32)`, a Rust tuple struct.
        if self.at(T::LParen) {
            let open = self.span();
            self.error(
                Diagnostic::new(codes::E0100, open, "wrela has no tuple structs: a struct names its fields")
                    .with_help(format!("name them, as in `struct {} {{ x: f32, y: f32 }}`; or, for a plain pair, use a tuple type: `type {} = (f32, f32)`", name.name, name.name)),
            );
            self.recovered_at = Some(self.pos);
            return Err(Failed);
        }
        let traits = if borrow && self.at(T::Colon) {
            // A borrow struct is a projection: it has no traits (it's never copied or kept).
            let colon = self.span();
            let bounds = self.opt_bounds()?;
            let end = self.span().start;
            let at =
                Span::new(colon.file, colon.start, bounds.last().map_or(colon.end, |b| b.span.end));
            self.error(
                Diagnostic::new(codes::E0519, at, format!("the borrow struct `{}` can't declare traits: it's a projection", name.name))
                    .with_note("a borrow struct lives only as long as the call that made it, so it's never copied, cloned or kept (§6.6)")
                    .with_fix("remove them", Span::new(colon.file, colon.start, end), " "),
            );
            Vec::new()
        } else if borrow {
            Vec::new()
        } else {
            self.opt_bounds()?
        };
        // A lifetime between the name and the fields (the lexer took it out): Rust's way of
        // holding references, which is a `borrow struct`'s projections.
        let (from, to) = (name.span.end, self.span().start);
        let had_lifetime = self.lifetimes.iter().any(|l| l.start >= from && l.end <= to);
        let outer = self.field_of.replace(had_lifetime);
        let block = self.parse_field_block(borrow);
        self.field_of = outer;
        let (fields, multiline) = block?;
        Ok(StructDecl { borrow, name, generics, traits, fields, multiline })
    }

    /// type_item ::= "type" IDENT generic_params? "=" trait_type
    fn parse_type_alias(&mut self) -> PResult<TypeAliasDecl> {
        self.expect(T::Type, "`type`")?;
        let name = self.ident("a type name")?;
        let generics = self.opt_generic_params()?;
        self.expect(T::Eq, "`=` and the type")?;
        // Traits name the type a function returns (§4): `type Blob = Surface + Copy`.
        let ty = self.parse_trait_position_type()?;
        Ok(TypeAliasDecl { name, generics, ty })
    }

    /// field_block ::= "{" (field_decl ("," field_decl)* ","?)? NEWLINE? "}"; a borrow struct's
    /// fields (`projections`) may be `borrow T` or `mut T`:
    /// borrow_fields ::= "{" (borrow_field ("," borrow_field)* ","?)? NEWLINE? "}";
    /// borrow_field ::= "pub"? IDENT ":" ("borrow" | "mut")? type
    fn parse_field_block(&mut self, projections: bool) -> PResult<(Vec<FieldDecl>, bool)> {
        self.expect(T::LBrace, "`{`")?;
        let multiline = self.tok().line_break_before && !self.at(T::RBrace);
        let fields = self.list(
            T::RBrace,
            |p| {
                let start = p.span();
                let vis = p.vis();
                let name = p.ident("a field name")?;
                p.expect(T::Colon, "`:` and the field's type")?;
                let mode = if !projections {
                    // A projection in an ordinary struct, as a reference would be in Rust: said
                    // so, and parsed on as the type after it.
                    if p.at(T::Borrow) || p.at(T::Mut) {
                        let kw = p.span();
                        let mode = if p.at(T::Mut) { "mut" } else { "borrow" };
                        p.error(
                            Diagnostic::new(
                                codes::E0519,
                                kw,
                                format!("a `{mode}` field is a projection, so it can't be a field of an ordinary struct"),
                            )
                            .with_note("a projection lives only as long as the call that made it (§6.6)")
                            .with_help("store a handle (`Handle<T>`) or an owned copy, make the type a `borrow struct`, or pass a closure that does the work where the place is"),
                        );
                        p.bump();
                    }
                    RetMode::Owned
                } else if p.eat(T::Borrow) {
                    RetMode::Borrow
                } else if p.eat(T::Mut) {
                    RetMode::Mut
                } else {
                    RetMode::Owned
                };
                let ty = p.parse_type()?;
                let default = if projections { None } else { p.opt_default()? };
                Ok(FieldDecl { vis, name, mode, ty, default, span: start.to(p.prev_span()) })
            },
            |p, first, span| {
                let vis = (p.tokens[first].kind == T::Pub)
                    .then_some(Vis { span: p.tokens[first].span, package: false });
                let name = p.name_at(first)?;
                let ty = TypeExpr::error(span);
                Some(FieldDecl { vis, name, mode: RetMode::Owned, ty, default: None, span })
            },
        );
        self.close_list(T::RBrace, "`,` or `}`");
        Ok((fields, multiline))
    }

    /// enum_item ::= "enum" IDENT generic_params? (":" bounds)? "{" (variant ("," variant)* ","?)? NEWLINE? "}"
    fn parse_enum(&mut self) -> PResult<EnumDecl> {
        self.expect(T::Enum, "`enum`")?;
        let name = self.ident("an enum name")?;
        let generics = self.opt_generic_params()?;
        let traits = self.opt_bounds()?;
        self.expect(T::LBrace, "`{`")?;
        let multiline = self.tok().line_break_before && !self.at(T::RBrace);
        let variants = self.list(
            T::RBrace,
            |p| {
                let start = p.span();
                let name = p.ident("a variant name")?;
                let kind = if p.eat(T::LParen) {
                    let tys = p.list(T::RParen, Self::parse_type, |_, _, span| {
                        Some(TypeExpr::error(span))
                    });
                    p.close_list(T::RParen, "`,` or `)`");
                    VariantKind::Tuple(tys)
                } else if p.at(T::LBrace) {
                    VariantKind::Struct(p.parse_field_block(false)?.0)
                } else {
                    VariantKind::Unit
                };
                Ok(Variant { name, kind, span: start.to(p.prev_span()) })
            },
            // A variant whose name parsed is kept, with fields of unknown types.
            |p, first, span| {
                let t = p.tokens[first];
                if t.kind != T::Ident {
                    return None;
                }
                let name = p.ident_of(t);
                let kind = match p.tokens.get(first + 1).map(|t| t.kind) {
                    Some(T::LParen) => VariantKind::Tuple(vec![TypeExpr::error(span)]),
                    Some(T::LBrace) => VariantKind::Struct(Vec::new()),
                    _ => VariantKind::Unit,
                };
                Some(Variant { name, kind, span })
            },
        );
        self.close_list(T::RBrace, "`,` or `}`");
        Ok(EnumDecl { name, generics, traits, variants, multiline })
    }

    /// trait_item ::= "trait" IDENT generic_params? ((":" bounds)? "{" sep* (trait_member (sep+
    /// trait_member)* sep*)? "}" | "=" bounds): a trait, or a trait set
    fn parse_trait(&mut self) -> PResult<ItemKind> {
        self.expect(T::Trait, "`trait`")?;
        let name = self.ident("a trait name")?;
        let generics = self.opt_generic_params()?;
        if self.eat(T::Eq) {
            let traits = self.parse_bounds()?;
            return Ok(ItemKind::TraitSet(TraitSetDecl { name, generics, traits }));
        }
        let supertraits = self.opt_bounds()?;
        let members = self.member_block("a trait member", Self::parse_trait_member)?;
        Ok(ItemKind::Trait(TraitDecl { name, generics, supertraits, members }))
    }

    /// trait_member ::= attribute* (fn_sig block? | "type" IDENT (":" bounds)?)
    fn parse_trait_member(&mut self) -> PResult<TraitMember> {
        let start = self.span();
        let attrs = self.parse_attrs()?;
        let kind = if self.eat(T::Type) {
            let name = self.ident("an associated type name")?;
            let bounds = self.opt_bounds()?;
            TraitMemberKind::Type { name, bounds }
        } else if self.at(T::Fn) {
            TraitMemberKind::Fn(self.parse_fn(false)?)
        } else {
            return Err(self.expected("`fn` or `type`"));
        };
        Ok(TraitMember { attrs, kind, span: start.to(self.prev_span()) })
    }

    /// impl_item ::= "impl" generic_params? type ("for" type)? "{" sep* (impl_member (sep+ impl_member)* sep*)? "}"
    fn parse_impl(&mut self) -> PResult<ImplDecl> {
        self.expect(T::Impl, "`impl`")?;
        let generics = self.opt_generic_params()?;
        let first = self.parse_type()?;
        let (trait_, self_ty) =
            if self.eat(T::For) { (Some(first), self.parse_type()?) } else { (None, first) };
        let members = self.member_block("an impl member", Self::parse_impl_member)?;
        Ok(ImplDecl { generics, trait_, self_ty, members })
    }

    /// impl_member ::= attribute* "pub"? fn_item | "type" IDENT "=" type
    fn parse_impl_member(&mut self) -> PResult<ImplMember> {
        let start = self.span();
        if self.eat(T::Type) {
            let name = self.ident("an associated type name")?;
            self.expect(T::Eq, "`=`")?;
            let ty = self.parse_type()?;
            return Ok(ImplMember {
                attrs: Vec::new(),
                vis: None,
                kind: ImplMemberKind::Type { name, ty },
                span: start.to(self.prev_span()),
            });
        }
        let attrs = self.parse_attrs()?;
        let vis = self.vis();
        if self.at(T::Type) {
            // impl_member allows neither before `type`: reported, then read without them.
            for a in &attrs {
                self.error(
                    Diagnostic::new(
                        codes::E0108,
                        a.span,
                        format!("`@{}` can't be on an associated type", a.name.name),
                    )
                    .with_help("attributes apply to functions")
                    .with_fix("remove it", a.span, ""),
                );
            }
            if let Some(Vis { span: p, .. }) = vis {
                self.error(
                    Diagnostic::new(codes::E0100, p, "an associated type can't be `pub`")
                        .with_help("an impl's associated types have no visibility of their own")
                        .with_fix("remove `pub`", p.to(self.span().shrink_to_start()), ""),
                );
            }
            return self.parse_impl_member();
        }
        if !self.at(T::Fn) {
            return Err(self.expected("`fn` or `type`"));
        }
        let f = self.parse_fn(true)?;
        Ok(ImplMember { attrs, vis, kind: ImplMemberKind::Fn(f), span: start.to(self.prev_span()) })
    }

    /// The members of a trait or an impl: "{" sep* (member (sep+ member)* sep*)? "}". A member
    /// that fails to parse is skipped up to the next separator. `what` names a member in errors.
    fn member_block<M>(
        &mut self,
        what: &str,
        mut member: impl FnMut(&mut Self) -> PResult<M>,
    ) -> PResult<Vec<M>> {
        self.expect(T::LBrace, "`{`")?;
        let mut members = Vec::new();
        self.closers.push(T::RBrace);
        self.skip_seps();
        while !self.at(T::RBrace) && !self.at(T::Eof) {
            match member(self) {
                Ok(m) => {
                    members.push(m);
                    if !self.at_sep() && !self.at(T::RBrace) {
                        self.missing_sep(what);
                        self.recover_to_sep();
                    }
                }
                Err(Failed) => self.recover_to_sep(),
            }
            self.skip_seps();
        }
        self.closers.pop();
        self.expect(T::RBrace, "`}`")?;
        Ok(members)
    }

    /// const_item ::= "const" IDENT (":" type)? "=" expr
    fn parse_const(&mut self) -> PResult<ConstDecl> {
        self.expect(T::Const, "`const`")?;
        let name = self.ident("a constant name")?;
        let ty = self.opt_colon_type()?;
        self.expect(T::Eq, "`=` and the constant's value")?;
        let value = self.expr_or_error(Self::parse_expr);
        Ok(ConstDecl { name, ty, value })
    }

    /// use_tree ::= IDENT ("::" IDENT)* ("::" "{" (use_tree ("," use_tree)* ","?)? NEWLINE? "}" | "as" IDENT)?
    fn parse_use_tree(&mut self) -> PResult<UseTree> {
        let start = self.span();
        let mut path = vec![self.ident("a module or item name")?];
        loop {
            if self.at(T::ColonColon) && self.nth(1) == T::Ident {
                self.bump();
                path.push(self.ident("a name")?);
            } else if self.at(T::ColonColon) && self.nth(1) == T::LBrace {
                self.bump();
                self.bump();
                let trees =
                    self.list(T::RBrace, |p| p.nested(Self::parse_use_tree), |_, _, _| None);
                self.close_list(T::RBrace, "`,` or `}`");
                return Ok(UseTree {
                    path,
                    kind: UseKind::Group(trees),
                    span: start.to(self.prev_span()),
                });
            } else if self.at(T::ColonColon) {
                self.bump();
                return Err(self.expected("a name or `{`"));
            } else {
                break;
            }
        }
        let rename = if self.eat(T::As) { Some(self.ident("a name")?) } else { None };
        Ok(UseTree { path, kind: UseKind::Simple(rename), span: start.to(self.prev_span()) })
    }

    // ---- types -----------------------------------------------------------------------------

    /// type ::= path_type | "[" type (";" expr)? "]" | "(" types ")" | fn_type
    pub(crate) fn parse_type(&mut self) -> PResult<TypeExpr> {
        self.nested(Self::parse_type_inner)
    }

    /// A parameter's type, a function's return type or a type alias's type, where a trait names
    /// a type.
    fn parse_trait_position_type(&mut self) -> PResult<TypeExpr> {
        self.trait_type_here = true;
        let ty = self.parse_type();
        self.trait_type_here = false;
        let ty = ty?;
        if !self.at(T::Plus) {
            return Ok(ty);
        }
        // Traits joined with `+`: any type with them all, or the one type that has them.
        let start = ty.span;
        let mut tys = vec![ty];
        while self.eat(T::Plus) {
            tys.push(self.parse_path_type()?);
        }
        Ok(TypeExpr { kind: TypeExprKind::Traits(tys), span: start.to(self.prev_span()) })
    }

    fn parse_type_inner(&mut self) -> PResult<TypeExpr> {
        let start = self.span();
        let trait_type_here = std::mem::take(&mut self.trait_type_here);
        if self.at(T::Dyn) {
            // wrela has no `dyn` (language.md §18): say what to use. Where a trait names a type,
            // read the trait as the type, which removing `dyn` makes valid; elsewhere the type
            // is an error, so nothing else is reported about it.
            let d = self.bump();
            let mut e = Diagnostic::new(codes::E0113, d.span, "wrela has no `dyn`")
                .with_note("`dyn` is reserved so this error can say what to use instead (§18)");
            if trait_type_here {
                e = e
                    .with_help("for any type with the trait, take it as a parameter of the trait's type, `x: Trait`, which makes the function generic; for one of a closed set of types, use an enum")
                    .with_fix("remove `dyn`", d.span.to(self.span().shrink_to_start()), "");
                self.error(e);
                return self.parse_type();
            }
            e = e.with_help("for one of a closed set of types, use an enum of them; for any type with the trait, add a generic parameter, as in `struct S<T: Trait> { x: T }`");
            self.error(e);
            let inner = self.parse_type()?;
            return Ok(TypeExpr::error(start.to(inner.span)));
        }
        match self.kind() {
            T::Ident | T::SelfType => self.parse_path_type(),
            T::LBracket => {
                self.bump();
                let (elem, len) = self.within(T::RBracket, |p| -> PResult<_> {
                    let elem = p.parse_type()?;
                    let len = if p.eat(T::Semi) { Some(Box::new(p.parse_expr()?)) } else { None };
                    Ok((elem, len))
                })?;
                self.expect(T::RBracket, "`]`")?;
                Ok(TypeExpr {
                    kind: TypeExprKind::Array(Box::new(elem), len),
                    span: start.to(self.prev_span()),
                })
            }
            T::LParen => {
                self.bump();
                let (mut tys, trailing) = self.parse_paren_types()?;
                let span = start.to(self.prev_span());
                let kind = if tys.len() == 1 && !trailing {
                    TypeExprKind::Paren(Box::new(tys.pop().ok_or(Failed)?))
                } else {
                    TypeExprKind::Tuple(tys)
                };
                Ok(TypeExpr { kind, span })
            }
            T::Fn | T::At => self.parse_fn_type(),
            T::Amp | T::AndAnd => {
                let amp = self.tok().span;
                let then_mut = self.nth(1) == T::Mut;
                let d = Diagnostic::new(codes::E0106, amp, "`&` isn't a type in wrela");
                let d = match self.field_of {
                    // A lifetime struct's field, about to be a `borrow struct`'s: a projection.
                    Some(true) => {
                        let d = d.with_note("a `borrow struct`'s fields are projections: `borrow T` reads a place, `mut T` changes it (§6.6)");
                        if then_mut {
                            d.with_fix("make it a `mut` projection", amp, "")
                        } else {
                            d.with_fix("make it a `borrow` projection", amp, "borrow ")
                        }
                    }
                    // An ordinary struct's field: a reference stored in a struct.
                    Some(false) => {
                        // Up to the type: `&mut Log` and `&Log` are both `Log`.
                        let k = if then_mut { 2 } else { 1 };
                        let end = self.tokens.get(self.pos + k).map_or(amp.end, |t| t.span.start);
                        let whole = Span::new(amp.file, amp.start, end);
                        d.with_note("a struct can't hold a reference: wrela has none, and a projection lives only as long as the call that made it (§6.6)")
                            .with_help("hold an owned copy (the fix), a handle (`Handle<T>`) into an arena, or make the struct a `borrow struct`")
                            .with_fix("hold an owned value", whole, "")
                    }
                    None => d
                        .with_note("wrela has no references; parameters have modes instead (language.md §6.2)")
                        .with_help("for a parameter, write `x: T` to borrow it or `x: mut T` to change it; to refer to another value long-term, store a `Handle<T>`")
                        .with_fix("remove the `&`", amp, ""),
                };
                self.error(d);
                Err(Failed)
            }
            _ => Err(self.expected("a type")),
        }
    }

    /// fn_type ::= ("@" IDENT)* "fn" "(" (fn_type_param ("," fn_type_param)* ","?)? ")" ("->" type)?;
    /// fn_type_param ::= mode? type
    fn parse_fn_type(&mut self) -> PResult<TypeExpr> {
        let start = self.span();
        let mut attrs = Vec::new();
        while self.eat(T::At) {
            attrs.push(self.ident("an attribute name")?);
        }
        self.expect(T::Fn, "`fn`")?;
        self.expect(T::LParen, "`(`")?;
        let params = self.within(T::RParen, |p| -> PResult<_> {
            let mut params = Vec::new();
            while !p.at(T::RParen) {
                let s = p.span();
                let mode = p.parse_mode();
                let ty = p.parse_type()?;
                params.push(FnTypeParam { mode, ty, span: s.to(p.prev_span()) });
                if !p.eat(T::Comma) {
                    break;
                }
            }
            Ok(params)
        })?;
        self.expect(T::RParen, "`,` or `)`")?;
        let ret = if self.eat(T::Arrow) { Some(Box::new(self.parse_type()?)) } else { None };
        Ok(TypeExpr {
            kind: TypeExprKind::Fn(FnType { attrs, params, ret }),
            span: start.to(self.prev_span()),
        })
    }

    /// The types of a tuple type, after its `(`: (type ("," type)* ","?)? ")".
    /// Returns them, and whether a `,` came last.
    fn parse_paren_types(&mut self) -> PResult<(Vec<TypeExpr>, bool)> {
        let types = self.within(T::RParen, |p| -> PResult<_> {
            let mut tys = Vec::new();
            let mut trailing = false;
            while !p.at(T::RParen) {
                tys.push(p.parse_type()?);
                trailing = p.eat(T::Comma);
                if !trailing {
                    break;
                }
            }
            Ok((tys, trailing))
        })?;
        self.expect(T::RParen, "`,` or `)`")?;
        Ok(types)
    }

    /// path_type ::= type_segment ("::" type_segment)*;  type_segment ::= (IDENT | "Self") generic_args?
    fn parse_path_type(&mut self) -> PResult<TypeExpr> {
        let start = self.span();
        let mut segments = Vec::new();
        loop {
            let ident = if self.at(T::SelfType) {
                let t = self.bump();
                self.ident_of(t)
            } else {
                self.ident("a type name")?
            };
            let generics = if self.at(T::Lt) { Some(self.parse_generic_args()?) } else { None };
            segments.push(PathSegment { ident, generics });
            if !self.eat(T::ColonColon) {
                break;
            }
        }
        let span = start.to(self.prev_span());
        Ok(TypeExpr { kind: TypeExprKind::Path(Path { segments, span }), span })
    }

    /// generic_args ::= "<" generic_arg ("," generic_arg)* ","? GT_CLOSE
    fn parse_generic_args(&mut self) -> PResult<Vec<TypeExpr>> {
        self.expect(T::Lt, "`<`")?;
        let mut out = vec![self.parse_generic_arg()?];
        while self.eat(T::Comma) {
            if self.at_gt_close() {
                break;
            }
            out.push(self.parse_generic_arg()?);
        }
        self.expect_gt_close()?;
        Ok(out)
    }

    /// generic_arg ::= type | INT
    fn parse_generic_arg(&mut self) -> PResult<TypeExpr> {
        if self.at(T::Int) {
            let t = self.bump();
            let lit = self.lit_of(t);
            return Ok(TypeExpr { kind: TypeExprKind::Int(lit), span: t.span });
        }
        self.parse_type()
    }
}

/// Calls `f` with each name a `use` binds and the path it binds the name to (`prefix` is the
/// path of the groups around `u`).
fn use_bindings<'a>(u: &'a UseTree, prefix: &mut Vec<&'a str>, f: &mut impl FnMut(&[&str], &str)) {
    let depth = prefix.len();
    prefix.extend(u.path.iter().map(|i| i.name.as_str()));
    match &u.kind {
        UseKind::Simple(alias) => {
            let last = prefix.last().copied().unwrap_or_default();
            f(prefix, alias.as_ref().map_or(last, |a| a.name.as_str()));
        }
        UseKind::Group(trees) => trees.iter().for_each(|t| use_bindings(t, prefix, f)),
    }
    prefix.truncate(depth);
}

/// Where an item's expressions are deeper than [`MAX_EXPR_DEPTH`], if anywhere. Iterative, so
/// the check itself doesn't recurse as deep as the tree.
fn too_deep(item: &Item) -> Option<Span> {
    let mut roots: Vec<&Expr> = Vec::new();
    item.for_each_body_expr(&mut |e| roots.push(e));
    for root in roots {
        let mut stack: Vec<(&Expr, u32)> = vec![(root, 1)];
        while let Some((e, d)) = stack.pop() {
            if d > MAX_EXPR_DEPTH {
                return Some(root.span.shrink_to_start());
            }
            e.for_each_child(&mut |c| stack.push((c, d + 1)));
        }
    }
    None
}
