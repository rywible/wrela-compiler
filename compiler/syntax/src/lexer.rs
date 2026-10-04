//! The lexer: spec/lexical.md, rule for rule. Rule numbers (L1–L21) are cited where they're
//! implemented.

use crate::ast::IntValue;
use crate::token::{Comment, Token, TokenKind};
use wrela_diag::{Diagnostic, Edit, FileId, Span, codes};

/// The lexer's output: tokens (NEWLINEs inserted, EOF last), comments and diagnostics.
#[derive(Debug, Default)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub comments: Vec<Comment>,
    pub diagnostics: Vec<Diagnostic>,
    /// Where each lifetime (`'a`, E0114) was, which makes no token: the parser offers a
    /// `borrow struct` for a struct that had one.
    pub lifetimes: Vec<Span>,
}

const TYPE_SUFFIXES: &[&str] =
    &["i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64", "f32", "f64"];

pub fn lex(file: FileId, text: &str) -> Lexed {
    let mut lx = Lexer {
        file,
        src: text.as_bytes(),
        text,
        pos: 0,
        out: Lexed::default(),
        holes: Vec::new(),
    };
    lx.run();
    let mut lexed = lx.out;
    lexed.tokens = insert_newlines(lexed.tokens, file);
    lexed
}

struct Lexer<'a> {
    file: FileId,
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    out: Lexed,
    /// The f-string holes open (L22), innermost last: each with where it opened and how many
    /// brackets are open inside it.
    holes: Vec<(usize, u32)>,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether `s` is a name: an IDENT token (L10), so not `_` and not a keyword.
pub fn is_name(s: &str) -> bool {
    let b = s.as_bytes();
    b.first().is_some_and(|&c| is_ident_start(c))
        && b[1..].iter().all(|&c| is_ident_continue(c))
        && s != "_"
        && TokenKind::keyword(s).is_none()
}

impl<'a> Lexer<'a> {
    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.file, start as u32, end as u32)
    }

    fn peek(&self, k: usize) -> Option<u8> {
        self.src.get(self.pos + k).copied()
    }

    fn char_at(&self, pos: usize) -> Option<char> {
        self.text.get(pos..).and_then(|s| s.chars().next())
    }

    fn error(&mut self, d: Diagnostic) {
        self.out.diagnostics.push(d);
    }

    fn push(&mut self, kind: TokenKind, start: usize, line_break_before: bool) {
        let span = self.span(start, self.pos);
        self.out.tokens.push(Token { kind, span, line_break_before });
    }

    fn run(&mut self) {
        // L2: a byte-order mark at the very start is ignored.
        if self.text.starts_with('\u{feff}') {
            self.pos = 3;
        }
        let mut line_break = false;
        let mut line_has_token = false; // anything but whitespace on the current line so far
        while self.pos < self.src.len() {
            let start = self.pos;
            let b = self.src[self.pos];
            match b {
                b' ' | b'\t' => self.pos += 1,
                b'\n' => {
                    self.unclosed_holes();
                    self.pos += 1;
                    line_break = true;
                    line_has_token = false;
                }
                b'\r' => {
                    self.unclosed_holes();
                    self.pos += 1;
                    if self.peek(0) == Some(b'\n') {
                        self.pos += 1;
                    } else {
                        // L4: a lone `\r` is an error, read as a line break.
                        let sp = self.span(start, self.pos);
                        self.error(
                            Diagnostic::new(
                                codes::E0001,
                                sp,
                                "a carriage return without a line feed",
                            )
                            .with_note("wrela line breaks are `\\n` or `\\r\\n` (L4)")
                            .with_fix(
                                "replace it with `\\n`",
                                sp,
                                "\n",
                            ),
                        );
                    }
                    line_break = true;
                    line_has_token = false;
                }
                b'/' if self.peek(1) == Some(b'/') => {
                    self.line_comment(line_has_token);
                    line_has_token = true;
                }
                b'/' if self.peek(1) == Some(b'*') => {
                    line_break |= self.block_comment(line_has_token);
                }
                _ => {
                    // A character that L3 skips makes no token, so the line break before it
                    // stays for the next one.
                    if self.token(line_break) {
                        line_break = false;
                        line_has_token = true;
                    }
                }
            }
        }
        self.unclosed_holes();
        let end = self.src.len();
        self.out.tokens.push(Token {
            kind: TokenKind::Eof,
            span: self.span(end, end),
            line_break_before: line_break,
        });
    }

    fn line_comment(&mut self, after_code: bool) {
        let start = self.pos;
        while let Some(b) = self.peek(0) {
            if b == b'\n' || b == b'\r' {
                break;
            }
            self.pos += 1;
        }
        let text = &self.text[start..self.pos];
        let doc = text.starts_with("///") && !text.starts_with("////");
        self.out.comments.push(Comment {
            span: self.span(start, self.pos),
            text: text.trim_end().to_string(),
            doc,
            own_line: !after_code,
        });
    }

    /// L9. Returns whether the comment held a line break.
    fn block_comment(&mut self, after_code: bool) -> bool {
        let start = self.pos;
        self.pos += 2;
        let mut depth = 1;
        let mut had_break = false;
        while self.pos < self.src.len() && depth > 0 {
            match (self.src[self.pos], self.peek(1)) {
                (b'/', Some(b'*')) => {
                    depth += 1;
                    self.pos += 2;
                }
                (b'*', Some(b'/')) => {
                    depth -= 1;
                    self.pos += 2;
                }
                (b'\n' | b'\r', _) => {
                    had_break = true;
                    self.pos += 1;
                }
                _ => self.pos += 1,
            }
        }
        let sp = self.span(start, self.pos);
        let body = &self.text[start..self.pos];
        let mut d = Diagnostic::new(
            codes::E0002,
            self.span(start, start + 2),
            "wrela has no block comments",
        )
        .with_help("use `//` comments, one per line");
        let rest_of_line_blank = self.src[self.pos..]
            .iter()
            .take_while(|&&b| b != b'\n' && b != b'\r')
            .all(|&b| b == b' ' || b == b'\t');
        if !had_break && rest_of_line_blank && depth == 0 {
            let inner = body[2..body.len() - 2].trim().trim_start_matches('*').trim();
            d = d.with_fix("rewrite it as a `//` comment", sp, format!("// {inner}"));
        }
        self.error(d);
        self.out.comments.push(Comment {
            span: sp,
            text: body.to_string(),
            doc: false,
            own_line: !after_code,
        });
        had_break
    }

    /// Whether the `'` here starts what Rust calls a character literal, `'a'`: a quote closes
    /// it on the same line.
    fn char_literal(&self) -> bool {
        let mut i = self.pos + 1;
        while self.src.get(i).is_some_and(|&b| is_ident_continue(b)) {
            i += 1;
        }
        self.src.get(i) == Some(&b'\'')
    }

    /// A lifetime, as Rust writes one (`'a`), or a loop label (`'outer`): E0114, and no token.
    /// A lifetime's fix removes it, with the brackets or the comma it leaves behind (those
    /// make no token either, so the parser sees what the fix would leave); a struct whose
    /// only parameters are lifetimes holds projections, so its fix makes it a `borrow struct`.
    fn lifetime(&mut self) {
        let start = self.pos;
        self.pos += 1;
        while self.peek(0).is_some_and(is_ident_continue) {
            self.pos += 1;
        }
        let end = self.pos;
        let name = &self.text[start..end];
        let sp = self.span(start, end);
        self.out.lifetimes.push(sp);
        let spaces = |from: usize| {
            let mut i = from;
            while matches!(self.src.get(i), Some(b' ' | b'\t')) {
                i += 1;
            }
            i
        };
        let after = spaces(end);
        let prev = self.out.tokens.last().map(|t| (t.kind, t.span));
        let label = matches!(prev, Some((TokenKind::Break | TokenKind::Continue, _)))
            || (self.src.get(after) == Some(&b':')
                && self.text[spaces(after + 1)..].starts_with(['l', 'f', 'w']));
        if label {
            // `'outer: loop`: the `:` goes with it.
            if self.src.get(after) == Some(&b':') {
                self.pos = after + 1;
            }
            self.error(
                Diagnostic::new(codes::E0114, sp, format!("`{name}` is a loop label, which wrela doesn't have"))
                    .with_note("`break` and `continue` leave the innermost loop")
                    .with_help("move the inner loop into a function that returns, or set a flag the outer loop checks"),
            );
            return;
        }
        let d = Diagnostic::new(codes::E0114, sp, format!("`{name}` is a lifetime, which wrela doesn't have"))
            .with_note("wrela has no references, so nothing needs a lifetime: a parameter reads its argument in place, and a projection lives as long as the call that made it (§6.4)");
        let d = match (prev, self.src.get(after)) {
            // `<'a>`: the brackets go too. On a struct, it holds projections.
            (Some((TokenKind::Lt, lt)), Some(b'>')) => {
                self.out.tokens.pop();
                self.pos = after + 1;
                let whole = self.span(lt.start as usize, self.pos);
                let n = self.out.tokens.len();
                let is_struct = n >= 2
                    && self.out.tokens[n - 2].kind == TokenKind::Struct
                    && self.out.tokens[n - 1].kind == TokenKind::Ident;
                if is_struct {
                    let kw = self.out.tokens[n - 2].span;
                    d.with_help("a struct that holds projections is a `borrow struct` (§6.6)")
                        .with_fix_edits(
                            "make it a `borrow struct`",
                            vec![
                                Edit { span: kw, replacement: "borrow struct".into() },
                                Edit { span: whole, replacement: String::new() },
                            ],
                        )
                } else {
                    d.with_fix("remove it", whole, "")
                }
            }
            // `<'a, T>`: the comma after it goes too.
            (Some((TokenKind::Lt, _)), Some(b',')) => {
                self.pos = spaces(after + 1);
                d.with_fix("remove it", self.span(start, self.pos), "")
            }
            // `<T, 'a>`: the comma before it goes too.
            (Some((TokenKind::Comma, comma)), _) => {
                self.out.tokens.pop();
                d.with_fix("remove it", self.span(comma.start as usize, end), "")
            }
            // `&'a T`: it, and the space after it.
            _ => d.with_fix("remove it", self.span(start, after), ""),
        };
        self.error(d);
    }

    /// Lexes the token at the current position. Returns whether it made one: a run of
    /// characters that can't appear here (L3) is reported and skipped.
    fn token(&mut self, line_break_before: bool) -> bool {
        let start = self.pos;
        let b = self.src[self.pos];
        // L22: in an f-string's hole, outside brackets, `}` ends the hole and `:` starts its
        // format spec.
        if let Some(&(_, 0)) = self.holes.last()
            && (b == b'}' || (b == b':' && self.peek(1) != Some(b':')))
        {
            self.hole_end(line_break_before);
            return true;
        }
        if b == b'f' && self.peek(1) == Some(b'"') {
            self.pos += 2;
            let kind = self.fstring_text(start, true);
            self.push(kind, start, line_break_before);
            return true;
        }
        if b == b'_' && !self.peek(1).is_some_and(is_ident_continue) {
            self.pos += 1;
            self.push(TokenKind::Underscore, start, line_break_before);
        } else if is_ident_start(b)
            || b >= 0x80 && self.char_at(start).is_some_and(|c| c.is_alphabetic())
        {
            self.ident(line_break_before);
        } else if b.is_ascii_digit() {
            self.number(line_break_before);
        } else if b == b'"' {
            self.string(line_break_before);
        } else if b == b'\'' && self.peek(1).is_some_and(is_ident_start) && !self.char_literal() {
            self.lifetime();
            return false;
        } else if let Some(kind) = self.punct() {
            self.pos += kind.fixed_text().map_or(1, str::len);
            if let Some((_, depth)) = self.holes.last_mut() {
                match kind {
                    TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => *depth += 1,
                    TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                        *depth = depth.saturating_sub(1)
                    }
                    _ => {}
                }
            }
            self.push(kind, start, line_break_before);
        } else {
            // L3: a run of characters that can't appear here, reported once.
            while self.pos < self.src.len() {
                let c = self.char_at(self.pos).unwrap_or('\u{fffd}');
                let b = self.src[self.pos];
                let ok = b.is_ascii_alphanumeric()
                    || b == b'_'
                    || matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'"')
                    || (b < 0x80 && self.punct().is_some())
                    || (b == b'/' && matches!(self.peek(1), Some(b'/' | b'*')));
                if ok && self.pos > start {
                    break;
                }
                self.pos += c.len_utf8();
                if ok {
                    break;
                }
            }
            let sp = self.span(start, self.pos);
            let shown = &self.text[start..self.pos];
            let mut d = Diagnostic::new(
                codes::E0001,
                sp,
                format!("`{}` can't appear outside a comment", shown.escape_debug()),
            );
            if shown == "'" {
                d = d.with_help("wrela has no character literals");
            } else if shown.chars().all(|c| c.is_whitespace()) {
                d = d.with_help("only spaces and tabs separate tokens (L5)").with_fix(
                    "replace it with a space",
                    sp,
                    " ",
                );
            }
            self.error(d);
            return false;
        }
        true
    }

    /// L15: the longest punctuation token at the current position.
    fn punct(&self) -> Option<TokenKind> {
        let rest = &self.src[self.pos..];
        TokenKind::PUNCT
            .iter()
            .copied()
            .find(|k| k.fixed_text().is_some_and(|t| rest.starts_with(t.as_bytes())))
            .filter(|k| {
                // `_` followed by an identifier character is an identifier (L10).
                *k != TokenKind::Underscore || !rest.get(1).copied().is_some_and(is_ident_continue)
            })
    }

    fn ident(&mut self, line_break_before: bool) {
        let start = self.pos;
        let mut bad: Option<(usize, usize)> = None;
        while self.pos < self.src.len() {
            let b = self.src[self.pos];
            if is_ident_continue(b) {
                self.pos += 1;
            } else if b >= 0x80 {
                let c = self.char_at(self.pos).unwrap_or('\u{fffd}');
                if !c.is_alphanumeric() {
                    break;
                }
                let s = self.pos;
                self.pos += c.len_utf8();
                bad = Some(bad.map_or((s, self.pos), |(a, _)| (a, self.pos)));
            } else {
                break;
            }
        }
        if let Some((a, b)) = bad {
            let sp = self.span(a, b);
            self.error(
                Diagnostic::new(
                    codes::E0001,
                    sp,
                    format!("`{}` isn't an ASCII letter or digit", &self.text[a..b]),
                )
                .with_help("names are ASCII: letters, digits and `_` (L10)"),
            );
        }
        let text = &self.text[start..self.pos];
        let kind = TokenKind::keyword(text).unwrap_or(TokenKind::Ident);
        self.push(kind, start, line_break_before);
    }

    /// L22: the end of an f-string's hole, at `}` or a format spec's `:`, and the text after it,
    /// up to the next hole (FSTRING_MID) or the closing quote (FSTRING_TAIL).
    fn hole_end(&mut self, line_break_before: bool) {
        let start = self.pos;
        if self.src[self.pos] == b':' {
            // The format spec runs to the `}` that ends the hole.
            loop {
                match self.peek(0) {
                    Some(b'}') => break,
                    None | Some(b'\n' | b'\r' | b'"') => {
                        let sp = self.span(start, self.pos);
                        self.error(
                            Diagnostic::new(codes::E0005, sp, "this format spec isn't closed")
                                .with_help("end it with `}`, as in `{x:.2}` (L22)"),
                        );
                        self.holes.pop();
                        self.push(TokenKind::FStringTail, start, line_break_before);
                        return;
                    }
                    Some(_) => self.pos += self.char_at(self.pos).map_or(1, char::len_utf8),
                }
            }
        }
        self.pos += 1;
        self.holes.pop();
        let kind = self.fstring_text(start, false);
        self.push(kind, start, line_break_before);
    }

    /// L22: an f-string's text from the current position, up to a hole's `{` or the closing
    /// quote. Returns the token it ends: after `f"` (`first`) an FSTRING or FSTRING_HEAD, after a
    /// hole an FSTRING_TAIL or FSTRING_MID.
    fn fstring_text(&mut self, start: usize, first: bool) -> TokenKind {
        let (closed, opened) = if first {
            (TokenKind::FString, TokenKind::FStringHead)
        } else {
            (TokenKind::FStringTail, TokenKind::FStringMid)
        };
        loop {
            match self.peek(0) {
                None | Some(b'\n' | b'\r') => {
                    let sp = self.span(start, self.pos);
                    self.error(
                        Diagnostic::new(codes::E0005, sp, "this f-string isn't closed")
                            .with_help("close it with `\"` on the same line (L22)"),
                    );
                    return closed;
                }
                Some(b'"') => {
                    self.pos += 1;
                    return closed;
                }
                Some(b'{') if self.peek(1) == Some(b'{') => self.pos += 2,
                Some(b'}') if self.peek(1) == Some(b'}') => self.pos += 2,
                Some(b'{') => {
                    self.pos += 1;
                    self.holes.push((self.pos, 0));
                    return opened;
                }
                Some(b'}') => {
                    let sp = self.span(self.pos, self.pos + 1);
                    self.error(
                        Diagnostic::new(
                            codes::E0005,
                            sp,
                            "a `}` in an f-string's text is written `}}`",
                        )
                        .with_fix("double it", sp, "}}"),
                    );
                    self.pos += 1;
                }
                Some(b'\\') => self.escape(),
                Some(_) => self.pos += self.char_at(self.pos).map_or(1, char::len_utf8),
            }
        }
    }

    /// A line or the file ends inside f-string holes: each is reported, and closed.
    fn unclosed_holes(&mut self) {
        while let Some((at, _)) = self.holes.pop() {
            let sp = self.span(at - 1, at);
            self.error(
                Diagnostic::new(codes::E0005, sp, "this f-string hole isn't closed").with_help(
                    "close it with `}`, and the string with `\"`, on the same line (L22)",
                ),
            );
        }
    }

    /// An escape at `\` in a string's text (L14): reported if unknown.
    fn escape(&mut self) {
        let esc = self.pos;
        self.pos += 1;
        match self.peek(0) {
            Some(b'\\' | b'"' | b'n' | b'r' | b't' | b'0') => self.pos += 1,
            _ => {
                let c = self.char_at(self.pos).map_or(0, char::len_utf8);
                self.pos += c;
                let sp = self.span(esc, self.pos);
                self.error(
                    Diagnostic::new(codes::E0005, sp, "an unknown escape")
                        .with_help("the escapes are `\\\\ \\\" \\n \\r \\t \\0` (L14)"),
                );
            }
        }
    }

    fn string(&mut self, line_break_before: bool) {
        let start = self.pos;
        self.pos += 1;
        loop {
            match self.peek(0) {
                None | Some(b'\n' | b'\r') => {
                    let sp = self.span(start, self.pos);
                    self.error(
                        Diagnostic::new(codes::E0005, sp, "this string literal isn't closed")
                            .with_help("close it with `\"` on the same line (L14)"),
                    );
                    break;
                }
                Some(b'"') => {
                    self.pos += 1;
                    break;
                }
                Some(b'\\') => self.escape(),
                Some(_) => {
                    let c = self.char_at(self.pos).map_or(1, char::len_utf8);
                    self.pos += c;
                }
            }
        }
        self.push(TokenKind::Str, start, line_break_before);
    }

    /// L12–L13.
    fn number(&mut self, line_break_before: bool) {
        let start = self.pos;
        let after_dot = self
            .out
            .tokens
            .last()
            .is_some_and(|t| t.kind == TokenKind::Dot && t.span.end as usize == start);
        let prefixed = self.src[start] == b'0'
            && matches!(self.src.get(start + 1), Some(b'x' | b'b' | b'o' | b'X' | b'B' | b'O'));
        let mut seen_dot = false;
        loop {
            while self.peek(0).is_some_and(is_ident_continue) {
                self.pos += 1;
            }
            if prefixed || after_dot {
                break;
            }
            if !seen_dot
                && self.peek(0) == Some(b'.')
                && self.peek(1).is_some_and(|b| b.is_ascii_digit())
            {
                seen_dot = true;
                self.pos += 1;
                continue;
            }
            let last = self.src[self.pos - 1];
            if matches!(last, b'e' | b'E')
                && matches!(self.peek(0), Some(b'+' | b'-'))
                && self.peek(1).is_some_and(|b| b.is_ascii_digit())
            {
                self.pos += 1;
                continue;
            }
            break;
        }
        let sp = self.span(start, self.pos);
        let text = &self.text[start..self.pos];
        let kind = match classify_number(text) {
            Ok(k) => k,
            Err(NumberError::TypeSuffix { suffix_at, kind }) => {
                let suffix_span = self.span(start + suffix_at, self.pos);
                let digits = text[..suffix_at].trim_end_matches('_');
                self.error(
                    Diagnostic::new(
                        codes::E0003,
                        suffix_span,
                        format!("number literals have no type suffixes, so `{text}` isn't valid"),
                    )
                    .with_help(format!(
                        "a literal takes its type from context; write `{digits}`, or convert with `{}({digits})`",
                        &text[suffix_at..]
                    ))
                    .with_fix("remove the suffix", sp, digits),
                );
                kind
            }
            Err(NumberError::Malformed(why)) => {
                self.error(Diagnostic::new(
                    codes::E0004,
                    sp,
                    format!("`{text}` isn't a number: {why}"),
                ));
                TokenKind::Int
            }
        };
        self.push(kind, start, line_break_before);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum NumberError {
    /// The number before the suffix is fine: an INT or a FLOAT.
    TypeSuffix {
        suffix_at: usize,
        kind: TokenKind,
    },
    Malformed(&'static str),
}

/// L13: classifies a number token's text.
fn classify_number(text: &str) -> Result<TokenKind, NumberError> {
    let (end, kind) = number_body(text)?;
    classify_suffix(&text[end..], end, kind)
}

/// Where a suffixed number's digits end and its suffix starts: `1e-3m` is `1e-3` and `m`.
pub fn split_suffix(text: &str) -> (&str, &str) {
    let end = number_body(text).map_or(text.len(), |(end, _)| end);
    text.split_at(end)
}

/// L13: where a number token's digits end (its suffix, if any, starts there), and whether
/// they're an INT or a FLOAT.
fn number_body(text: &str) -> Result<(usize, TokenKind), NumberError> {
    let b = text.as_bytes();
    let has_digit = |s: &str, radix: u32| s.chars().any(|c| c.is_digit(radix));
    if b.len() >= 2 && b[0] == b'0' && matches!(b[1], b'X' | b'B' | b'O') {
        return Err(NumberError::Malformed("radix prefixes are lowercase: `0x`, `0b`, `0o`"));
    }
    if b.len() >= 2 && b[0] == b'0' && matches!(b[1], b'x' | b'b' | b'o') {
        let radix = match b[1] {
            b'x' => 16,
            b'b' => 2,
            _ => 8,
        };
        let body = &text[2..];
        let end = body.find(|c: char| !(c.is_digit(radix) || c == '_')).unwrap_or(body.len());
        if !has_digit(&body[..end], radix) {
            return Err(NumberError::Malformed("a radix prefix needs at least one digit"));
        }
        let suffix = &body[end..];
        if suffix.starts_with(|c: char| c.is_ascii_digit()) {
            return Err(NumberError::Malformed("a digit out of range for the radix"));
        }
        return Ok((2 + end, TokenKind::Int));
    }
    // Decimal: digits, then an optional fraction and exponent.
    let mut i = digits_end(b, 0);
    let mut kind = TokenKind::Int;
    if i < b.len() && b[i] == b'.' {
        let f = i + 1;
        let j = digits_end(b, f);
        if !has_digit(&text[f..j], 10) {
            return Err(NumberError::Malformed("a fraction needs at least one digit"));
        }
        i = j;
        kind = TokenKind::Float;
    }
    if i < b.len() && matches!(b[i], b'e' | b'E') {
        let mut ds = i + 1;
        if ds < b.len() && matches!(b[ds], b'+' | b'-') {
            ds += 1;
        }
        let j = digits_end(b, ds);
        if has_digit(&text[ds..j], 10) {
            i = j;
            kind = TokenKind::Float;
        } else {
            return Err(NumberError::Malformed("an exponent needs at least one digit"));
        }
    }
    Ok((i, kind))
}

/// Where the run of decimal digits and `_` that starts at `from` ends.
fn digits_end(b: &[u8], from: usize) -> usize {
    from + b[from..].iter().take_while(|&&c| c.is_ascii_digit() || c == b'_').count()
}

fn classify_suffix(suffix: &str, at: usize, kind: TokenKind) -> Result<TokenKind, NumberError> {
    if suffix.is_empty() {
        Ok(kind)
    } else if TYPE_SUFFIXES.contains(&suffix) {
        Err(NumberError::TypeSuffix { suffix_at: at, kind })
    } else if suffix.starts_with(['e', 'E']) {
        Err(NumberError::Malformed("an exponent needs at least one digit"))
    } else if suffix.starts_with(|c: char| c.is_ascii_alphabetic())
        && suffix.bytes().all(is_ident_continue)
    {
        Ok(TokenKind::Suffixed)
    } else {
        Err(NumberError::Malformed("unexpected characters after the digits"))
    }
}

/// L17: whether a line break between a token of kind `prev` and one of kind `next` makes a
/// NEWLINE. `in_brace`: the innermost open bracket is `{`, or none is open ([`Brackets`]). A
/// NEWLINE can't end a statement, so no line break after one makes another (L18).
pub fn line_break_is_newline(in_brace: bool, prev: TokenKind, next: TokenKind) -> bool {
    in_brace && prev.can_end_statement() && next != TokenKind::Dot
}

/// The open brackets, as L19 tracks them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Brackets {
    open: Vec<TokenKind>,
    /// How many of each kind are open (`(`, `[`, `{`): a closer that closes nothing is seen
    /// at once, however many brackets are open.
    count: [u32; 3],
}

/// Where [`Brackets::count`] counts an opener of kind `k`.
fn opener_index(k: TokenKind) -> Option<usize> {
    match k {
        TokenKind::LParen => Some(0),
        TokenKind::LBracket => Some(1),
        TokenKind::LBrace => Some(2),
        _ => None,
    }
}

impl Brackets {
    /// Takes the next token into account: an opening bracket opens; a closing bracket closes
    /// the innermost open bracket of its kind and every bracket opened after it, or nothing if
    /// none of its kind is open.
    pub fn track(&mut self, k: TokenKind) {
        if let Some(c) = opener_index(k) {
            self.open.push(k);
            self.count[c] += 1;
        } else if let Some(open) = k.opener()
            && opener_index(open).is_some_and(|c| self.count[c] > 0)
            && let Some(i) = self.open.iter().rposition(|o| *o == open)
        {
            for o in self.open.drain(i..) {
                if let Some(c) = opener_index(o) {
                    self.count[c] -= 1;
                }
            }
        }
    }

    /// Whether the innermost open bracket is `{`, or none is open (L17 condition 1).
    pub fn in_brace(&self) -> bool {
        self.open.last().is_none_or(|k| *k == TokenKind::LBrace)
    }

    /// How many brackets are open.
    pub fn depth(&self) -> usize {
        self.open.len()
    }
}

/// L17–L19: inserts NEWLINE tokens into the raw token stream.
fn insert_newlines(raw: Vec<Token>, file: FileId) -> Vec<Token> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 4);
    newlines_after(None, Brackets::default(), raw, file, &mut out);
    out
}

/// [`insert_newlines`] for the raw tokens that follow `prev`, with `brackets` open before them:
/// appends them to `out`, NEWLINEs inserted.
pub(crate) fn newlines_after(
    mut prev: Option<Token>,
    mut brackets: Brackets,
    raw: Vec<Token>,
    file: FileId,
    out: &mut Vec<Token>,
) {
    for tok in raw {
        if tok.line_break_before
            && let Some(p) = prev
            && line_break_is_newline(brackets.in_brace(), p.kind, tok.kind)
        {
            let at = p.span.end;
            out.push(Token {
                kind: TokenKind::Newline,
                span: Span::new(file, at, at),
                line_break_before: false,
            });
        }
        brackets.track(tok.kind);
        out.push(tok);
        prev = Some(tok);
    }
}

/// The text a STRING token's source spells: quotes removed, escapes decoded (L14).
pub fn string_value(text: &str) -> String {
    let inner = text.strip_prefix('"').unwrap_or(text);
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    decode(inner, false)
}

/// The parts of an f-string token's text (L22): its format spec (the hole before it ends with
/// one, as in `:.1}`), and its literal text, decoded.
pub fn fstring_segment(text: &str) -> (Option<String>, String) {
    let mut rest = text;
    let mut spec = None;
    if let Some(r) = rest.strip_prefix("f\"") {
        rest = r;
    } else if let Some(r) = rest.strip_prefix(':') {
        let end = r.find('}').unwrap_or(r.len());
        spec = Some(r[..end].to_string());
        rest = r.get(end + 1..).unwrap_or("");
    } else if let Some(r) = rest.strip_prefix('}') {
        rest = r;
    }
    // The text ends before the `{` that opens the next hole, or the closing quote.
    // (A token that ends in `{` ends at a hole: the lexer reads `{{` as text first.)
    let body = match rest.as_bytes().last() {
        Some(b'"' | b'{') => &rest[..rest.len() - 1],
        _ => rest,
    };
    (spec, decode(body, true))
}

/// A format spec's parts (`{x:spec}`, §4).
pub struct SpecFields {
    pub width: u32,
    pub precision: Option<u32>,
    /// 0 for none, then `<`, `>` and `^`.
    pub align: u32,
    pub zero: bool,
    pub hex: bool,
}

/// Reads a format spec: `[<^>][0][width][.precision][x]`.
pub fn parse_spec(s: &str) -> Result<SpecFields, String> {
    let mut f = SpecFields { width: 0, precision: None, align: 0, zero: false, hex: false };
    let b = s.as_bytes();
    let mut i = 0;
    let number = |i: &mut usize| -> Result<Option<u32>, String> {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i == start {
            return Ok(None);
        }
        s[start..*i].parse::<u32>().map(Some).map_err(|_| "a number in it is too large".into())
    };
    if let Some(&c) = b.first() {
        f.align = match c {
            b'<' => 1,
            b'>' => 2,
            b'^' => 3,
            _ => 0,
        };
        if f.align != 0 {
            i += 1;
        }
    }
    if b.get(i) == Some(&b'0') {
        f.zero = true;
        i += 1;
    }
    f.width = number(&mut i)?.unwrap_or(0);
    if b.get(i) == Some(&b'.') {
        i += 1;
        match number(&mut i)? {
            Some(p) => f.precision = Some(p),
            None => return Err("`.` needs the number of digits after it".into()),
        }
    }
    if b.get(i) == Some(&b'x') {
        f.hex = true;
        i += 1;
    }
    if i != b.len() {
        return Err(format!("`{}` isn't part of a spec", &s[i..]));
    }
    if f.width > 1 << 16 || f.precision.is_some_and(|p| p > 1 << 10) {
        return Err("its width or precision is too large".into());
    }
    Ok(f)
}

/// A string's text with its escapes decoded; in an f-string (`braces`), `{{` and `}}` too.
fn decode(s: &str, braces: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('0') => out.push('\0'),
                Some(other) => out.push(other),
                None => {}
            },
            '{' | '}' if braces && chars.peek() == Some(&c) => {
                chars.next();
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// The value of an INT token's text: `_` ignored, a radix prefix read.
pub fn int_value(text: &str) -> IntValue {
    match u64_value(text) {
        Some(v) => IntValue::Ok(v),
        None if matches!(classify_number(text), Err(NumberError::Malformed(_))) => {
            IntValue::Malformed
        }
        None => IntValue::TooLarge,
    }
}

/// A number token's text without its type suffix: the suffix is already an error (E0003), and
/// the number before it has a value. (Hex digits include `f`, so `0x1f32` has no suffix.)
fn without_type_suffix(text: &str) -> &str {
    match classify_number(text) {
        Err(NumberError::TypeSuffix { suffix_at, .. }) => &text[..suffix_at],
        _ => text,
    }
}

/// The value of an INT token's text, or `None` if it isn't a number that fits in a `u64`.
fn u64_value(text: &str) -> Option<u64> {
    let clean: String = without_type_suffix(text).chars().filter(|c| *c != '_').collect();
    let (digits, radix) = match clean.get(..2) {
        Some("0x") => (&clean[2..], 16),
        Some("0b") => (&clean[2..], 2),
        Some("0o") => (&clean[2..], 8),
        _ => (&clean[..], 10),
    };
    u64::from_str_radix(digits, radix).ok()
}

/// The value of a FLOAT token's text, `_` ignored.
pub fn float_value(text: &str) -> f64 {
    let clean: String = without_type_suffix(text).chars().filter(|c| *c != '_').collect();
    clean.parse().unwrap_or(f64::NAN)
}

/// [`float_value`] as an `f32`, rounded once from the decimal: rounding the `f64` again could
/// land on the other side of a tie.
pub fn float_value_f32(text: &str) -> f32 {
    let clean: String = without_type_suffix(text).chars().filter(|c| *c != '_').collect();
    clean.parse().unwrap_or(f32::NAN)
}

#[cfg(test)]
mod tests;
