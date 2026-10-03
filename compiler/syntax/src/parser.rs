//! The hand-written recursive-descent parser. It accepts exactly the token sequences
//! spec/grammar.ebnf accepts (checked by the oracle tests in `wrela-grammar`); everything else
//! gets a diagnostic, and the parser recovers to report more than one error per file.
//!
//! Each `parse_*` function names the grammar rule it implements.

mod expr;

use crate::ast::*;
use crate::lexer::Lexed;
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
        self.diagnostics.iter().any(|d| d.is_error())
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
        tokens: lexed.tokens,
        pos: 0,
        diags: lexed.diagnostics,
        syntax_errors: 0,
        nesting: 0,
        recovered_at: None,
        closers: Vec::new(),
        lost_breaks: None,
    };
    let f = p.parse_file();
    Parsed { file: f, comments: lexed.comments, diagnostics: p.diags }
}

/// Raised when a rule can't continue; the caller recovers.
#[derive(Debug)]
pub(crate) struct Failed;

pub(crate) type PResult<T> = Result<T, Failed>;

pub(crate) struct Parser<'a> {
    file: FileId,
    text: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    diags: Vec<Diagnostic>,
    syntax_errors: u32,
    /// How deeply the rule being parsed is nested in others that nest (expressions, blocks,
    /// types, patterns).
    nesting: u32,
    /// The token position recovery last stopped at: an error there is already reported.
    pub(crate) recovered_at: Option<usize>,
    /// The closers of the lists and blocks being parsed, innermost last. Recovery stops at one
    /// of these; any other closer closes nothing (L19) and is skipped.
    pub(crate) closers: Vec<T>,
    /// After a mismatched closer (E0102): the depth of `closers` whose block it's in.
    pub(crate) lost_breaks: Option<usize>,
}

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
        }
        t
    }

    fn eat(&mut self, kind: T) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn text_of(&self, span: Span) -> &'a str {
        &self.text[span.start as usize..span.end as usize]
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
            Ok(Ident { name: self.text_of(t.span).to_string(), span: t.span })
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
        let mut depth = 0i32;
        loop {
            match self.kind() {
                T::Eof => break,
                T::Newline | T::Semi if depth == 0 => break,
                T::RBrace if depth == 0 => break,
                T::LParen | T::LBracket | T::LBrace => depth += 1,
                T::RParen | T::RBracket | T::RBrace => depth -= 1,
                _ => {}
            }
            self.bump();
            if depth < 0 {
                depth = 0;
            }
        }
        self.recovered_at = Some(self.pos);
    }

    /// An expression, or, if it fails to parse, an error node in its place after skipping to
    /// the end of the statement.
    pub(crate) fn expr_or_error(&mut self, f: impl FnOnce(&mut Self) -> PResult<Expr>) -> Expr {
        let start = self.span();
        f(self).unwrap_or_else(|Failed| {
            self.recover_to_sep();
            Expr { kind: ExprKind::Error, span: self.since(start) }
        })
    }

    /// Whether the token closes a list or block being parsed (`close`, or an enclosing one's,
    /// which closes this one with it (L19)).
    fn at_closer(&self, close: T) -> bool {
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
        // A closer of another kind that closes nothing open (L19): most likely a typo for
        // this one, so it's read as this one.
        let k = self.kind();
        if matches!(k, T::RParen | T::RBracket | T::RBrace)
            && close != T::Pipe
            && !self.closers.contains(&k)
        {
            let t = self.bump();
            let (want, found) = (close.fixed_text().unwrap_or("?"), k.fixed_text().unwrap_or("?"));
            let mut d = Diagnostic::new(
                codes::E0102,
                t.span,
                format!("`{found}` doesn't close anything here: expected `{want}`"),
            )
            .with_fix(format!("close with `{want}`"), t.span, want);
            if let Some(open) = self.opener_of(close) {
                d = d.with_secondary(open, "opened here");
            }
            self.error(d);
            // The block it's in: `closers` ends with this list's closer, after the block's.
            self.lost_breaks = self.closers.iter().rposition(|&c| c == T::RBrace).map(|i| i + 1);
            return t.span;
        }
        if self.recovered_at != Some(self.pos) {
            self.expected(what);
        }
        self.skip_until(close, |_| false);
        if self.at(close) { self.bump().span } else { self.prev_span() }
    }

    /// The bracket that `close` would close: the nearest unclosed one of its kind before the
    /// current token.
    fn opener_of(&self, close: T) -> Option<Span> {
        let open = match close {
            T::RParen => T::LParen,
            T::RBracket => T::LBracket,
            T::RBrace => T::LBrace,
            _ => return None,
        };
        let mut depth = 0u32;
        for t in self.tokens[..self.pos.min(self.tokens.len())].iter().rev() {
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
        (t.kind == T::Ident && colon.kind == T::Colon)
            .then(|| Ident { name: self.text_of(t.span).to_string(), span: t.span })
    }

    fn error_type(span: Span) -> TypeExpr {
        TypeExpr { kind: TypeExprKind::Error, span }
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
                        self.error(
                            Diagnostic::new(
                                codes::E0112,
                                span,
                                format!(
                                    "this expression is more than {MAX_EXPR_DEPTH} levels deep"
                                ),
                            )
                            .with_help("split it into `let` bindings"),
                        );
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
                T::Fn | T::Struct | T::Enum | T::Trait | T::Const => {
                    let n = self.tokens.get(i + 1)?;
                    return (n.kind == T::Ident)
                        .then(|| Ident { name: self.text_of(n.span).to_string(), span: n.span });
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
        self.error(
            Diagnostic::new(
                codes::E0104,
                t.span,
                format!("expected a line break or `;` after {what}, found {}", describe(t.kind)),
            )
            .with_fix("start a new line here", at, "\n"),
        );
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
        let vis = if self.at(T::Pub) { Some(self.bump().span) } else { None };
        let kind = match self.kind() {
            T::Fn => ItemKind::Fn(self.parse_fn(true)?),
            T::Struct => ItemKind::Struct(self.parse_struct()?),
            T::Enum => ItemKind::Enum(self.parse_enum()?),
            T::Trait => ItemKind::Trait(self.parse_trait()?),
            T::Const => ItemKind::Const(self.parse_const()?),
            T::Use => {
                self.bump();
                ItemKind::Use(self.parse_use_tree()?)
            }
            T::Impl if vis.is_none() => ItemKind::Impl(self.parse_impl()?),
            T::Impl => {
                let pub_span = vis.unwrap_or(start);
                self.error(
                    Diagnostic::new(codes::E0100, pub_span, "an `impl` can't be `pub`")
                        .with_help("its items carry their own visibility")
                        .with_fix("remove `pub`", pub_span.to(self.span().shrink_to_start()), ""),
                );
                return Err(Failed);
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
                    "an item (`fn`, `struct`, `enum`, `trait`, `impl`, `const` or `use`)",
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
        let generics = if self.at(T::Lt) { self.parse_generic_params()? } else { Vec::new() };
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
            let name = p.name_at(first).or_else(|| {
                (t.kind == T::Ident)
                    .then(|| Ident { name: p.text_of(t.span).to_string(), span: t.span })
            })?;
            let ty = Self::error_type(span);
            Some(Param::Named { name, mode, ty, default: None, span })
        });
        self.close_list(T::RParen, "`,` or `)`");
        let ret = if self.eat(T::Arrow) {
            let mode = if self.eat(T::Borrow) {
                RetMode::Borrow
            } else if self.eat(T::Mut) {
                RetMode::Mut
            } else {
                RetMode::Owned
            };
            Some(RetType { mode, ty: self.parse_type()? })
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
        Ok(FnDecl { name, generics, params, ret, body, sig_span })
    }

    /// generic_params ::= "<" (generic_param ("," generic_param)* ","?)? GT_CLOSE
    fn parse_generic_params(&mut self) -> PResult<Vec<GenericParam>> {
        self.expect(T::Lt, "`<`")?;
        let mut out = Vec::new();
        while !matches!(self.kind(), T::Gt | T::Shr | T::Ge | T::ShrEq) {
            let name = self.ident("a generic parameter name")?;
            let bounds = if self.eat(T::Colon) { self.parse_bounds()? } else { Vec::new() };
            out.push(GenericParam { name, bounds });
            if !self.eat(T::Comma) {
                break;
            }
        }
        self.expect_gt_close()?;
        Ok(out)
    }

    /// bounds ::= path_type ("+" path_type)*
    fn parse_bounds(&mut self) -> PResult<Vec<TypeExpr>> {
        let mut out = vec![self.parse_path_type()?];
        while self.eat(T::Plus) {
            out.push(self.parse_path_type()?);
        }
        Ok(out)
    }

    /// param ::= mode? "self" | IDENT ":" mode? type ("=" expr)?
    fn parse_param(&mut self) -> PResult<Param> {
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
        let ty = self.parse_type()?;
        let default = if self.eat(T::Eq) { Some(self.parse_expr()?) } else { None };
        Ok(Param::Named { name, mode, ty, default, span: start.to(self.prev_span()) })
    }

    /// `mut x: T`, which is `x: mut T`: reported with the fix, and parsed as meant.
    fn misplaced_mode(&mut self, start: Span, mode: Mode) -> PResult<Param> {
        let name = self.ident("a parameter name")?;
        self.bump();
        let ty_at = self.span().shrink_to_start();
        let written = self.parse_mode();
        let kw = if mode == Mode::Mut { "mut" } else { "take" };
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
        let default = if self.eat(T::Eq) { Some(self.parse_expr()?) } else { None };
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

    /// struct_item ::= "struct" IDENT generic_params? (":" bounds)? field_block
    fn parse_struct(&mut self) -> PResult<StructDecl> {
        self.expect(T::Struct, "`struct`")?;
        let name = self.ident("a struct name")?;
        let generics = if self.at(T::Lt) { self.parse_generic_params()? } else { Vec::new() };
        let traits = if self.eat(T::Colon) { self.parse_bounds()? } else { Vec::new() };
        let (fields, multiline) = self.parse_field_block()?;
        Ok(StructDecl { name, generics, traits, fields, multiline })
    }

    /// field_block ::= "{" (field_decl ("," field_decl)* ","?)? NEWLINE? "}"
    fn parse_field_block(&mut self) -> PResult<(Vec<FieldDecl>, bool)> {
        self.expect(T::LBrace, "`{`")?;
        let multiline = self.tok().line_break_before && !self.at(T::RBrace);
        let fields = self.list(
            T::RBrace,
            |p| {
                let start = p.span();
                let vis = if p.at(T::Pub) { Some(p.bump().span) } else { None };
                let name = p.ident("a field name")?;
                p.expect(T::Colon, "`:` and the field's type")?;
                let ty = p.parse_type()?;
                let default = if p.eat(T::Eq) { Some(p.parse_expr()?) } else { None };
                Ok(FieldDecl { vis, name, ty, default, span: start.to(p.prev_span()) })
            },
            |p, first, span| {
                let vis = (p.tokens[first].kind == T::Pub).then_some(p.tokens[first].span);
                let name = p.name_at(first)?;
                Some(FieldDecl { vis, name, ty: Self::error_type(span), default: None, span })
            },
        );
        self.close_list(T::RBrace, "`,` or `}`");
        Ok((fields, multiline))
    }

    /// enum_item ::= "enum" IDENT generic_params? (":" bounds)? "{" (variant ("," variant)* ","?)? NEWLINE? "}"
    fn parse_enum(&mut self) -> PResult<EnumDecl> {
        self.expect(T::Enum, "`enum`")?;
        let name = self.ident("an enum name")?;
        let generics = if self.at(T::Lt) { self.parse_generic_params()? } else { Vec::new() };
        let traits = if self.eat(T::Colon) { self.parse_bounds()? } else { Vec::new() };
        self.expect(T::LBrace, "`{`")?;
        let multiline = self.tok().line_break_before && !self.at(T::RBrace);
        let variants = self.list(
            T::RBrace,
            |p| {
                let start = p.span();
                let name = p.ident("a variant name")?;
                let kind = if p.eat(T::LParen) {
                    let tys = p.list(T::RParen, Self::parse_type, |_, _, span| {
                        Some(Self::error_type(span))
                    });
                    p.close_list(T::RParen, "`,` or `)`");
                    VariantKind::Tuple(tys)
                } else if p.at(T::LBrace) {
                    VariantKind::Struct(p.parse_field_block()?.0)
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
                let name = Ident { name: p.text_of(t.span).to_string(), span: t.span };
                let kind = match p.tokens.get(first + 1).map(|t| t.kind) {
                    Some(T::LParen) => VariantKind::Tuple(vec![Self::error_type(span)]),
                    Some(T::LBrace) => VariantKind::Struct(Vec::new()),
                    _ => VariantKind::Unit,
                };
                Some(Variant { name, kind, span })
            },
        );
        self.close_list(T::RBrace, "`,` or `}`");
        Ok(EnumDecl { name, generics, traits, variants, multiline })
    }

    /// trait_item ::= "trait" IDENT generic_params? (":" bounds)? "{" sep* (trait_member (sep+ trait_member)* sep*)? "}"
    fn parse_trait(&mut self) -> PResult<TraitDecl> {
        self.expect(T::Trait, "`trait`")?;
        let name = self.ident("a trait name")?;
        let generics = if self.at(T::Lt) { self.parse_generic_params()? } else { Vec::new() };
        let supertraits = if self.eat(T::Colon) { self.parse_bounds()? } else { Vec::new() };
        self.expect(T::LBrace, "`{`")?;
        let mut members = Vec::new();
        self.skip_seps();
        while !self.at(T::RBrace) && !self.at(T::Eof) {
            let start = self.span();
            let r = (|| -> PResult<TraitMember> {
                let attrs = self.parse_attrs()?;
                let kind = if self.eat(T::Type) {
                    let name = self.ident("an associated type name")?;
                    let bounds = if self.eat(T::Colon) { self.parse_bounds()? } else { Vec::new() };
                    TraitMemberKind::Type { name, bounds }
                } else if self.at(T::Fn) {
                    TraitMemberKind::Fn(self.parse_fn(false)?)
                } else {
                    return Err(self.expected("`fn` or `type`"));
                };
                Ok(TraitMember { attrs, kind, span: start.to(self.prev_span()) })
            })();
            match r {
                Ok(m) => {
                    members.push(m);
                    if !self.at_sep() && !self.at(T::RBrace) {
                        self.missing_sep("a trait member");
                        self.recover_to_sep();
                    }
                }
                Err(Failed) => self.recover_to_sep(),
            }
            self.skip_seps();
        }
        self.expect(T::RBrace, "`}`")?;
        Ok(TraitDecl { name, generics, supertraits, members })
    }

    /// impl_item ::= "impl" generic_params? type ("for" type)? "{" sep* (impl_member (sep+ impl_member)* sep*)? "}"
    fn parse_impl(&mut self) -> PResult<ImplDecl> {
        self.expect(T::Impl, "`impl`")?;
        let generics = if self.at(T::Lt) { self.parse_generic_params()? } else { Vec::new() };
        let first = self.parse_type()?;
        let (trait_, self_ty) =
            if self.eat(T::For) { (Some(first), self.parse_type()?) } else { (None, first) };
        self.expect(T::LBrace, "`{`")?;
        let mut members = Vec::new();
        self.skip_seps();
        while !self.at(T::RBrace) && !self.at(T::Eof) {
            let start = self.span();
            let r = (|| -> PResult<ImplMember> {
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
                let vis = if self.at(T::Pub) { Some(self.bump().span) } else { None };
                if !self.at(T::Fn) {
                    return Err(self.expected("`fn` or `type`"));
                }
                let f = self.parse_fn(true)?;
                Ok(ImplMember {
                    attrs,
                    vis,
                    kind: ImplMemberKind::Fn(f),
                    span: start.to(self.prev_span()),
                })
            })();
            match r {
                Ok(m) => {
                    members.push(m);
                    if !self.at_sep() && !self.at(T::RBrace) {
                        self.missing_sep("an impl member");
                        self.recover_to_sep();
                    }
                }
                Err(Failed) => self.recover_to_sep(),
            }
            self.skip_seps();
        }
        self.expect(T::RBrace, "`}`")?;
        Ok(ImplDecl { generics, trait_, self_ty, members })
    }

    /// const_item ::= "const" IDENT (":" type)? "=" expr
    fn parse_const(&mut self) -> PResult<ConstDecl> {
        self.expect(T::Const, "`const`")?;
        let name = self.ident("a constant name")?;
        let ty = if self.eat(T::Colon) { Some(self.parse_type()?) } else { None };
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
                let trees = self.list(T::RBrace, Self::parse_use_tree, |_, _, _| None);
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

    /// type ::= path_type | "[" type (";" expr)? "]" | "(" types ")" | "fn" "(" types ")" ("->" type)?
    pub(crate) fn parse_type(&mut self) -> PResult<TypeExpr> {
        self.nested(Self::parse_type_inner)
    }

    fn parse_type_inner(&mut self) -> PResult<TypeExpr> {
        let start = self.span();
        if self.at(T::Dyn) {
            // `dyn Trait` is tier 2: say so, and read the trait as the type.
            let d = self.bump();
            self.error(
                Diagnostic::new(codes::E0905, d.span, "`dyn` is tier 2")
                    .with_help("take the trait as a generic parameter: `x: Trait` (§7)"),
            );
            return self.parse_type();
        }
        match self.kind() {
            T::Ident | T::SelfType => {
                let p = self.parse_path_type()?;
                Ok(p)
            }
            T::LBracket => {
                self.bump();
                let elem = self.parse_type()?;
                let len = if self.eat(T::Semi) { Some(Box::new(self.parse_expr()?)) } else { None };
                self.expect(T::RBracket, "`]`")?;
                Ok(TypeExpr {
                    kind: TypeExprKind::Array(Box::new(elem), len),
                    span: start.to(self.prev_span()),
                })
            }
            T::LParen => {
                self.bump();
                let mut tys = Vec::new();
                let mut trailing = false;
                while !self.at(T::RParen) {
                    tys.push(self.parse_type()?);
                    trailing = self.eat(T::Comma);
                    if !trailing {
                        break;
                    }
                }
                self.expect(T::RParen, "`,` or `)`")?;
                let span = start.to(self.prev_span());
                let kind = if tys.len() == 1 && !trailing {
                    TypeExprKind::Paren(Box::new(tys.pop().ok_or(Failed)?))
                } else {
                    TypeExprKind::Tuple(tys)
                };
                Ok(TypeExpr { kind, span })
            }
            T::Fn => {
                self.bump();
                self.expect(T::LParen, "`(`")?;
                let mut tys = Vec::new();
                while !self.at(T::RParen) {
                    tys.push(self.parse_type()?);
                    if !self.eat(T::Comma) {
                        break;
                    }
                }
                self.expect(T::RParen, "`,` or `)`")?;
                let ret =
                    if self.eat(T::Arrow) { Some(Box::new(self.parse_type()?)) } else { None };
                Ok(TypeExpr { kind: TypeExprKind::Fn(tys, ret), span: start.to(self.prev_span()) })
            }
            T::Amp | T::AndAnd => {
                let amp = self.tok().span;
                self.error(
                    Diagnostic::new(codes::E0106, amp, "`&` isn't a type in wrela")
                        .with_note("wrela has no references; parameters have modes instead (language.md §6.2)")
                        .with_help("for a parameter, write `x: T` to borrow it or `x: mut T` to change it; to refer to another value long-term, store a `Handle<T>`")
                        .with_fix("remove the `&`", amp, ""),
                );
                Err(Failed)
            }
            _ => Err(self.expected("a type")),
        }
    }

    /// path_type ::= type_segment ("::" type_segment)*;  type_segment ::= (IDENT | "Self") generic_args?
    fn parse_path_type(&mut self) -> PResult<TypeExpr> {
        let start = self.span();
        let mut segments = Vec::new();
        loop {
            let ident = if self.at(T::SelfType) {
                let t = self.bump();
                Ident { name: "Self".into(), span: t.span }
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

    /// generic_args ::= "<" type ("," type)* ","? GT_CLOSE
    fn parse_generic_args(&mut self) -> PResult<Vec<TypeExpr>> {
        self.expect(T::Lt, "`<`")?;
        let mut out = vec![self.parse_type()?];
        while self.eat(T::Comma) {
            if matches!(self.kind(), T::Gt | T::Shr | T::Ge | T::ShrEq) {
                break;
            }
            out.push(self.parse_type()?);
        }
        self.expect_gt_close()?;
        Ok(out)
    }
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
