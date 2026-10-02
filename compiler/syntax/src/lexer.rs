//! The lexer: spec/lexical.md, rule for rule. Rule numbers (L1–L21) are cited where they're
//! implemented.

use crate::token::{Comment, Token, TokenKind};
use wrela_diag::{Diagnostic, FileId, Span, codes};

/// The lexer's output: tokens (NEWLINEs inserted, EOF last), comments and diagnostics.
#[derive(Debug, Default)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub comments: Vec<Comment>,
    pub diagnostics: Vec<Diagnostic>,
}

const TYPE_SUFFIXES: &[&str] =
    &["i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64", "f32", "f64"];

pub fn lex(file: FileId, text: &str) -> Lexed {
    let mut lx = Lexer { file, src: text.as_bytes(), text, pos: 0, out: Lexed::default() };
    lx.run();
    let mut lexed = lx.out;
    lexed.tokens = insert_newlines(lexed.tokens, file, text.len() as u32);
    lexed
}

struct Lexer<'a> {
    file: FileId,
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    out: Lexed,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
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
                    self.pos += 1;
                    line_break = true;
                    line_has_token = false;
                }
                b'\r' => {
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
                    let had_break = self.block_comment(line_has_token);
                    if had_break {
                        line_break = true;
                    }
                }
                _ => {
                    let lb = std::mem::take(&mut line_break);
                    line_has_token = true;
                    self.token(lb);
                }
            }
        }
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
        let rest_of_line_blank = {
            let mut i = self.pos;
            let mut blank = true;
            while i < self.src.len() && self.src[i] != b'\n' && self.src[i] != b'\r' {
                if self.src[i] != b' ' && self.src[i] != b'\t' {
                    blank = false;
                }
                i += 1;
            }
            blank
        };
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

    fn token(&mut self, line_break_before: bool) {
        let start = self.pos;
        let b = self.src[self.pos];
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
        } else if let Some(kind) = self.punct() {
            self.pos += kind.fixed_text().map_or(1, str::len);
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
        }
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
                Some(b'\\') => {
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
                && !self.text[start..self.pos].contains(['e', 'E'])
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
            Err(NumberError::TypeSuffix { suffix_at }) => {
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
                if text[..suffix_at].contains(['.', 'e', 'E']) {
                    TokenKind::Float
                } else {
                    TokenKind::Int
                }
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
    TypeSuffix { suffix_at: usize },
    Malformed(&'static str),
}

/// L13: classifies a number token's text.
fn classify_number(text: &str) -> Result<TokenKind, NumberError> {
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
        return classify_suffix(suffix, 2 + end, TokenKind::Int);
    }
    // Decimal: digits, then an optional fraction and exponent.
    let mut i = 0;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
        i += 1;
    }
    let mut kind = TokenKind::Int;
    if i < b.len() && b[i] == b'.' {
        let f = i + 1;
        let mut j = f;
        while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'_') {
            j += 1;
        }
        if !has_digit(&text[f..j], 10) {
            return Err(NumberError::Malformed("a fraction needs at least one digit"));
        }
        i = j;
        kind = TokenKind::Float;
    }
    if i < b.len() && matches!(b[i], b'e' | b'E') {
        let mut j = i + 1;
        if j < b.len() && matches!(b[j], b'+' | b'-') {
            j += 1;
        }
        let ds = j;
        while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'_') {
            j += 1;
        }
        if has_digit(&text[ds..j], 10) {
            i = j;
            kind = TokenKind::Float;
        } else {
            return Err(NumberError::Malformed("an exponent needs at least one digit"));
        }
    }
    classify_suffix(&text[i..], i, kind)
}

fn classify_suffix(suffix: &str, at: usize, kind: TokenKind) -> Result<TokenKind, NumberError> {
    if suffix.is_empty() {
        Ok(kind)
    } else if TYPE_SUFFIXES.contains(&suffix) {
        Err(NumberError::TypeSuffix { suffix_at: at })
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

/// L17–L19: inserts NEWLINE tokens into the raw token stream.
fn insert_newlines(raw: Vec<Token>, file: FileId, _len: u32) -> Vec<Token> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 4);
    let mut stack: Vec<TokenKind> = Vec::new();
    for tok in raw {
        if tok.line_break_before
            && let Some(prev) = out.last().copied()
        {
            let prev: Token = prev;
            let in_brace = stack.last().is_none_or(|k| *k == TokenKind::LBrace);
            if in_brace
                && prev.kind != TokenKind::Newline
                && prev.kind.can_end_statement()
                && tok.kind != TokenKind::Dot
            {
                let at = prev.span.end;
                out.push(Token {
                    kind: TokenKind::Newline,
                    span: Span::new(file, at, at),
                    line_break_before: false,
                });
            }
        }
        match tok.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => stack.push(tok.kind),
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                let open = match tok.kind {
                    TokenKind::RParen => TokenKind::LParen,
                    TokenKind::RBracket => TokenKind::LBracket,
                    _ => TokenKind::LBrace,
                };
                if let Some(i) = stack.iter().rposition(|k| *k == open) {
                    stack.truncate(i);
                }
            }
            _ => {}
        }
        out.push(tok);
    }
    out
}

/// The value of an INT token's text (`_` ignored, radix prefixes read), or `None` if it
/// doesn't fit in a `u64`.
pub fn int_value(text: &str) -> Option<u64> {
    // A type suffix is already an error (E0003); read the digits before it. (Hex digits
    // include `f`, so `0x1f32` is a plain hex number.)
    let text = if text.starts_with("0x") {
        text
    } else {
        TYPE_SUFFIXES.iter().find_map(|s| text.strip_suffix(s)).unwrap_or(text)
    };
    let clean: String = text.chars().filter(|c| *c != '_').collect();
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
    let text = TYPE_SUFFIXES.iter().find_map(|s| text.strip_suffix(s)).unwrap_or(text);
    let clean: String = text.chars().filter(|c| *c != '_').collect();
    clean.parse().unwrap_or(f64::NAN)
}

#[cfg(test)]
mod tests;
