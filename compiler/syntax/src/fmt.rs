//! The formatter (D-038: "the formatter normalizes"). It lays a parsed file out in 100 columns:
//!
//! - Four-space indents; one statement per line; `;` separators become line breaks.
//! - A list (arguments, parameters, fields, elements, struct literals) goes on one line if it
//!   fits, and otherwise one element per line, with a trailing comma. A chain of two or more
//!   method calls breaks before each call, and a chain of binary operators after each operator
//!   (but not after `>` or `>>`, L20). Lines break only where L17 makes the break whitespace.
//! - The author's choices that carry meaning are kept: single blank lines between statements and
//!   between `const`s, leading-dot chains, and whether a call's arguments, a struct literal, an
//!   array, a struct or an enum spans several lines. So the same code can be formatted in more
//!   than one way; formatting any of them again changes nothing.
//! - Comments stay where they are: between the same two tokens (`,` and `;` aside), at the end
//!   of the line they trailed or on lines of their own. Before an item, statement, element, arm,
//!   operand or closing bracket they go on the lines before it; a list or chain with a comment
//!   inside breaks, so the comment can end its line. Where the layout has no line break (between
//!   a keyword and a name, say), the line breaks after the comment and goes on indented.
//! - It never changes what a file means: parsing its output gives the same AST and the same
//!   comments in the same places (tests check this, and that formatting is idempotent).

mod doc;

use crate::ast::*;
use crate::parser::{Parsed, parse};
use crate::token::{Comment, TokenKind};
use doc::{Doc, INDENT, alone, block, broken, concat, group, if_break, indent, nil, text};
use wrela_diag::{FileId, Span};

/// The width lines are laid out in.
const MAX_WIDTH: usize = 100;

/// Formats a file that parsed without errors.
pub fn format(parsed: &Parsed, text: &str) -> String {
    let mut out = format_once(parsed, text);
    if out == text {
        // Formatted already: another pass would parse the same text and give it again.
        return out;
    }
    // A comment the layout has no place for moves to the end of a line, and there the next
    // pass may lay its surroundings out differently. That settles after a move or two; the
    // result is what formatting keeps.
    for _ in 0..4 {
        let again = parse(FileId(0), &out);
        if again.has_errors() {
            break;
        }
        let next = format_once(&again, &out);
        if next == out {
            break;
        }
        out = next;
    }
    out
}

/// Moves each of `offsets`, a place in `before`, to the same place in `after`, which is
/// `before` formatted. Formatting keeps the tokens and their order, but `,`, `;` and line
/// breaks, so a place is found by the token it's in or before: the n-th of the others.
pub fn move_offsets(before: &str, after: &str, offsets: &mut [u32]) {
    let kept = |text: &str| -> Vec<Span> {
        crate::lexer::lex(FileId(0), text)
            .tokens
            .into_iter()
            .filter(|t| {
                !matches!(
                    t.kind,
                    TokenKind::Comma | TokenKind::Semi | TokenKind::Newline | TokenKind::Eof
                )
            })
            .map(|t| t.span)
            .collect()
    };
    let (a, b) = (kept(before), kept(after));
    for at in offsets {
        let i = a.partition_point(|s| s.end <= *at);
        *at = match (a.get(i), b.get(i)) {
            (Some(x), Some(y)) => y.start + at.saturating_sub(x.start).min(y.end - y.start),
            _ => after.len() as u32,
        };
    }
}

/// The AST's Debug output without spans, and without the flags that record layout (whether a
/// list or a chain link was written on several lines), which formatting may change: two
/// sources with equal skeletons parse to the same tree. For tests.
#[doc(hidden)]
pub fn skeleton(file: &File) -> String {
    // The compact Debug form: the pretty form (`{:#?}`) is several times slower to write, and
    // this runs for every program of a differential run. The layout flags are never a
    // struct's first field, so each one follows ", ".
    let mut text = format!("{file:?}");
    for flag in ["newline_before", "multiline"] {
        for value in ["true", "false"] {
            text = text.replace(&format!(", {flag}: {value}"), "");
        }
    }
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

/// Checks that formatting `src`, parsed without errors as `parsed`, gives a source that parses
/// to the same tree ([`skeleton`]), and that formatting it again changes nothing. Returns the
/// formatted source, or why the check failed. For tests.
#[doc(hidden)]
pub fn check_round_trip(parsed: &Parsed, src: &str) -> Result<String, String> {
    if parsed.has_errors() {
        return Err("the source has errors, so it can't be formatted".into());
    }
    let once = format(parsed, src);
    let again = parse(FileId(0), &once);
    if again.has_errors() {
        let d = again.diagnostics.iter().find(|d| d.is_error()).map(|d| d.message.clone());
        return Err(format!("the formatted source doesn't parse ({d:?}):\n{once}"));
    }
    if skeleton(&parsed.file) != skeleton(&again.file) {
        return Err(format!("formatting changed the AST; formatted:\n{once}"));
    }
    let (before, after) = (comment_places(src), comment_places(&once));
    if before != after {
        let i = before.iter().zip(&after).take_while(|(a, b)| a == b).count();
        return Err(format!(
            "formatting moved or changed a comment: {:?} became {:?}; formatted:\n{once}",
            before.get(i),
            after.get(i)
        ));
    }
    let twice = format(&again, &once);
    if once != twice {
        return Err(format!("formatting isn't idempotent; once:\n{once}\ntwice:\n{twice}"));
    }
    Ok(once)
}

/// Each comment of `src`, with where it is: its text, how many bytes of code come before it, and
/// whether it's on a line of its own. Code is every token but `,` and `;`, which formatting adds
/// and removes. Formatting keeps these. For tests.
#[doc(hidden)]
pub fn comment_places(src: &str) -> Vec<(String, usize, bool)> {
    let lexed = crate::lexer::lex(FileId(0), src);
    let mut code = lexed.tokens.iter().filter(|t| is_code(t.kind)).peekable();
    let mut before = 0;
    lexed
        .comments
        .into_iter()
        .map(|c| {
            while let Some(t) = code.next_if(|t| t.span.end <= c.span.start) {
                before += t.span.range().len();
            }
            let own_line = on_own_line(src, &c);
            (c.text, before, own_line)
        })
        .collect()
}

/// Whether a comment is on a line of its own: nothing but space, `,` and `;` comes before it
/// on its line (the formatter adds, moves and removes those).
fn on_own_line(src: &str, c: &Comment) -> bool {
    let start = c.span.start as usize;
    let line = src[..start].rfind('\n').map_or(0, |i| i + 1);
    src[line..start].chars().all(|ch| ch.is_whitespace() || ch == ',' || ch == ';')
}

/// Whether the formatter writes a token of this kind as the source has it: everything but
/// NEWLINE, EOF, and the `,` and `;` it adds and removes.
fn is_code(k: TokenKind) -> bool {
    !matches!(k, TokenKind::Newline | TokenKind::Eof | TokenKind::Comma | TokenKind::Semi)
}

fn format_once(parsed: &Parsed, text: &str) -> String {
    let code = crate::lexer::lex(FileId(0), text)
        .tokens
        .into_iter()
        .filter(|t| is_code(t.kind))
        .map(|t| t.span)
        .collect();
    let mut b = Builder { comments: &parsed.comments, next: 0, text, code, at: (0, 0) };
    let d = b.file(&parsed.file);
    let mut out = doc::print(&d, MAX_WIDTH);
    while out.ends_with('\n') || out.ends_with(' ') {
        out.pop();
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Builds the document, placing the comments in source order as it goes.
struct Builder<'a> {
    comments: &'a [Comment],
    /// The first comment not yet placed.
    next: usize,
    text: &'a str,
    /// The source's code tokens ([`is_code`]), which the output spells in the same order.
    code: Vec<Span>,
    /// How far the output has got: the code token it writes next, and how many of its bytes
    /// are written (a `>>` is written as two `>`).
    at: (usize, usize),
}

/// How a list is written.
#[derive(Clone, Copy)]
struct List {
    open: &'static str,
    close: &'static str,
    /// Spaces inside the brackets when it's on one line (`{ x: 1 }`).
    padded: bool,
    /// No comma after the last element when it breaks: it's a pattern's `..`.
    rest_last: bool,
    /// One element per line whatever the width: the author wrote it so.
    multiline: bool,
    /// A comma after a lone element even on one line (`(x,)`).
    lone_comma: bool,
}

const PARENS: List = List {
    open: "(",
    close: ")",
    padded: false,
    rest_last: false,
    multiline: false,
    lone_comma: false,
};
const BRACES: List = List { open: "{", close: "}", padded: true, ..PARENS };

impl<'a> Builder<'a> {
    // ---- code --------------------------------------------------------------------------------

    /// Source text that spells code, with the comments before each of its tokens that nothing
    /// placed yet: those are directly before it, and keep their line break.
    fn tok(&mut self, s: &str) -> Doc {
        let mut out = Vec::new();
        // Where the text not yet in `out` starts.
        let mut from = 0;
        for (i, c) in s.char_indices() {
            if let Some(t) = self.code.get(self.at.0).copied() {
                let expected = self.text[t.start as usize + self.at.1..].chars().next();
                if expected == Some(c) {
                    if self.at.1 == 0 && self.has_comment_before(t.start) {
                        let before = s[from..i].trim_end();
                        if !before.is_empty() {
                            out.push(text(before));
                        }
                        from = i;
                        out.push(self.inline_comments(t.start));
                    }
                    self.at.1 += c.len_utf8();
                    if self.at.1 >= t.range().len() {
                        self.at = (self.at.0 + 1, 0);
                    }
                } else {
                    debug_assert!(
                        c.is_whitespace() || c == ',' || c == ';',
                        "the formatter writes {s:?} where the source has {:?}",
                        &self.text[t.range()]
                    );
                }
            }
        }
        let rest = &s[from..];
        if out.is_empty() {
            return text(rest);
        }
        if !rest.is_empty() {
            out.push(text(rest));
        }
        concat(out)
    }

    /// The next code token, as the source spells it.
    fn code_token(&mut self) -> Doc {
        match self.code.get(self.at.0) {
            Some(t) => {
                let s = self.text[t.start as usize + self.at.1..t.end as usize].to_string();
                self.tok(&s)
            }
            None => nil(),
        }
    }

    /// An f-string hole's expression laid out on one line: formatted, or as written if it
    /// can't be (a block of several statements). One that starts with `{` gets a space before
    /// it, which keeps the hole's `{` from reading as `{{` (L22).
    fn flat_expr(&mut self, e: &Expr) -> Doc {
        let space = if self.text[e.span.range()].starts_with('{') { text(" ") } else { nil() };
        let saved = (self.at, self.next);
        let d = self.expr(e);
        let flat = doc::print(&d, 1 << 30);
        if !flat.contains('\n') {
            return concat([space, text(flat.trim_end())]);
        }
        (self.at, self.next) = saved;
        let written = self.text[e.span.range()].to_string();
        concat([space, self.tok(&written)])
    }

    /// Where the next code token to write starts.
    fn next_pos(&self) -> u32 {
        self.code.get(self.at.0).map_or(self.text.len() as u32, |t| t.start + self.at.1 as u32)
    }

    // ---- comments ----------------------------------------------------------------------------

    fn pending(&self) -> Option<&'a Comment> {
        self.comments.get(self.next)
    }

    fn has_comment_before(&self, pos: u32) -> bool {
        self.pending().is_some_and(|c| c.span.start < pos)
    }

    /// Takes the next comment to place, if it starts before `pos`.
    fn next_before(&mut self, pos: u32) -> Option<&'a Comment> {
        let c = self.pending().filter(|c| c.span.start < pos)?;
        self.next += 1;
        Some(c)
    }

    /// The comment that starts at `pos`, if one does.
    fn comment_at(&self, pos: u32) -> Option<&'a Comment> {
        let i = self.comments.binary_search_by_key(&pos, |c| c.span.start).ok()?;
        Some(&self.comments[i])
    }

    /// The comments before `pos` that trailed code on their lines, for the end of the line.
    fn trailing(&mut self, pos: u32) -> Vec<Doc> {
        let mut out = Vec::new();
        while self.pending().is_some_and(|c| !on_own_line(self.text, c))
            && let Some(c) = self.next_before(pos)
        {
            out.push(Doc::LineSuffix(format!(" {}", c.text)));
        }
        out
    }

    /// The comments before `pos`, each with a line break before it. The line breaks belong to
    /// the caller's indentation.
    fn own_lines(&mut self, pos: u32) -> Vec<Doc> {
        let mut out = Vec::new();
        while let Some(c) = self.next_before(pos) {
            out.push(Doc::HardLine);
            out.push(text(&c.text));
        }
        out
    }

    /// Whether the source has a blank line between two positions.
    fn blank_between(&self, a: u32, b: u32) -> bool {
        if a >= b {
            return false;
        }
        // The lines wholly between them: after the first line break, up to the last.
        self.text[a as usize..b as usize]
            .split_once('\n')
            .and_then(|(_, rest)| rest.rsplit_once('\n'))
            .is_some_and(|(lines, _)| lines.split('\n').any(|l| l.trim().is_empty()))
    }

    /// The comments before `pos`, at the start of a line: each on its own line, and a blank
    /// line after one where the source has one (a file's header, a section's divider).
    fn leading(&mut self, pos: u32) -> Vec<Doc> {
        let mut out = Vec::new();
        while let Some(c) = self.next_before(pos) {
            out.push(text(&c.text));
            out.push(Doc::HardLine);
            let next = self.pending().map_or(pos, |n| n.span.start.min(pos));
            let next = next.min(self.text.len() as u32);
            let only_space = self.text[c.span.end as usize..next as usize].trim().is_empty();
            if only_space && self.blank_between(c.span.end, next) {
                out.push(Doc::HardLine);
            }
        }
        out
    }

    /// The line break before an element of a sequence that is always one per line (items,
    /// statements, members, arms), with a blank line if `blank`, and the comments before it.
    fn hard_break(&mut self, pos: u32, blank: bool) -> Doc {
        let mut out = self.trailing(pos);
        out.push(Doc::HardLine);
        if blank {
            out.push(Doc::HardLine);
        }
        out.extend(self.leading(pos));
        concat(out)
    }

    /// The comments before the closer of a sequence that is one per line, at `end`: trailing
    /// ones at the end of the last line, the rest on lines of their own.
    fn hard_close(&mut self, end: u32) -> Doc {
        let mut out = self.trailing(end);
        out.extend(self.own_lines(end));
        concat(out)
    }

    /// A line break the group decides on (`line`), before the node at `pos`, with the comments
    /// before the node: trailing ones end the line before, the rest go on lines of their own,
    /// with a blank line after one where the source has one (a section's divider). Either kind
    /// breaks the group.
    fn soft_break(&mut self, pos: u32, line: Doc) -> Doc {
        let mut out = self.trailing(pos);
        let mut any = !out.is_empty();
        out.push(line);
        if any {
            out.extend(self.blank_after(self.next - 1, pos));
        }
        while let Some(c) = self.next_before(pos) {
            any = true;
            out.push(text(&c.text));
            out.push(Doc::HardLine);
            out.extend(self.blank_after(self.next - 1, pos));
        }
        if any {
            out.push(Doc::BreakParent);
        }
        concat(out)
    }

    /// A line break if the source has a blank line after comment `i`, before the next comment
    /// or `pos`.
    fn blank_after(&self, i: usize, pos: u32) -> Option<Doc> {
        let end = self.comments[i].span.end;
        let next = self.pending().map_or(pos, |n| n.span.start.min(pos));
        (self.blank_between(end, next) && self.adjacent(end, next)).then_some(Doc::HardLine)
    }

    /// Whether only space, comments, `,` and `;` come between `from` and `pos` in the source:
    /// a comment that ends at `from` is directly before what's at `pos`, so the line break
    /// after it was there too.
    fn adjacent(&self, from: u32, pos: u32) -> bool {
        let mut i = from;
        while i < pos {
            let b = self.text.as_bytes()[i as usize];
            if b.is_ascii_whitespace() || b == b',' || b == b';' {
                i += 1;
            } else if let Some(c) = self.comment_at(i) {
                i = c.span.end;
            } else {
                return false;
            }
        }
        true
    }

    /// The comments before `pos` that are directly before it in the source (see
    /// [`Self::adjacent`]); the others, which a layout without a place for them passed over,
    /// go at the end of the current line, which adds no line break.
    fn split_stale(&mut self, pos: u32) -> (Vec<Doc>, Vec<&'a Comment>) {
        let mut stale = Vec::new();
        let mut here = Vec::new();
        while let Some(c) = self.next_before(pos) {
            if self.adjacent(c.span.end, pos) {
                here.push(c);
            } else {
                stale.push(Doc::LineSuffix(format!(" {}", c.text)));
            }
        }
        (stale, here)
    }

    /// The comments before a node at `pos` that no list, chain or sequence placed. Those
    /// directly before it keep their line break (it was whitespace there in the source) and
    /// the node goes on, indented, after them.
    fn inline_comments(&mut self, pos: u32) -> Doc {
        if !self.has_comment_before(pos) {
            return nil();
        }
        let (mut out, here) = self.split_stale(pos);
        if here.is_empty() {
            return concat(out);
        }
        let mut lines = place_here(self.text, here, &mut out);
        lines.push(Doc::HardLine);
        out.push(Doc::BreakParent);
        out.push(broken(indent(concat(lines))));
        concat(out)
    }

    /// The comments before a closer at `end` that ends a bracket written on one line (`)`, `]`).
    /// Those directly before it keep their line break, before the closer.
    fn inline_close(&mut self, end: u32) -> Doc {
        self.close_comments(end, false)
    }

    /// [`Self::inline_close`]; with `comma`, a comma goes before a line break (the line before
    /// a `>` that closes a generic list must not end on an operand, L17).
    fn close_comments(&mut self, end: u32, comma: bool) -> Doc {
        if !self.has_comment_before(end) {
            return nil();
        }
        let (mut out, here) = self.split_stale(end);
        if here.is_empty() {
            return concat(out);
        }
        if comma {
            out.insert(0, text(","));
        }
        let lines = place_here(self.text, here, &mut out);
        out.push(Doc::BreakParent);
        out.push(broken(concat([indent(concat(lines)), Doc::HardLine])));
        concat(out)
    }

    /// The comments before the `close` that ends a list, the next code to write: placed before
    /// it, so none moves past a closer, where a line break could end a statement.
    fn close_before(&mut self, close: char) -> Doc {
        let at = self.next_pos();
        if self.text[at as usize..].starts_with(close) {
            // After an operand, a line break before `>` or `|` would end the statement (L17).
            self.close_comments(at, matches!(close, '>' | '|'))
        } else {
            nil()
        }
    }

    // ---- lists -------------------------------------------------------------------------------

    /// A comma-separated list in brackets, ending at `end` (just past its closer). `start` gives
    /// where an element starts in the source.
    fn list<T>(
        &mut self,
        items: &[T],
        start: impl Fn(&Self, &T) -> u32,
        mut item: impl FnMut(&mut Self, &T) -> Doc,
        end: u32,
        l: List,
    ) -> Doc {
        // Comments up to the closer belong inside the brackets.
        let inner_end = end.saturating_sub(1);
        if items.is_empty() && !self.has_comment_before(inner_end) {
            return self.tok(&format!("{}{}", l.open, l.close));
        }
        let line = || if l.padded { Doc::Line } else { Doc::SoftLine };
        let open = self.tok(l.open);
        let mut inner = Vec::new();
        for (i, x) in items.iter().enumerate() {
            if i > 0 {
                inner.push(text(","));
            }
            let at = start(self, x);
            inner.push(self.soft_break(at, if i == 0 { line() } else { Doc::Line }));
            inner.push(item(self, x));
        }
        if !items.is_empty() && !l.rest_last {
            inner.push(if l.lone_comma && items.len() == 1 {
                text(",")
            } else {
                if_break(text(","), nil())
            });
        }
        // Comments before the closer, on the elements' lines.
        let close = self.hard_close(inner_end);
        if close.has_hard_line() {
            inner.push(close);
            inner.push(Doc::BreakParent);
        }
        let d = concat([open, indent(concat(inner)), line(), self.tok(l.close)]);
        if l.multiline { broken(d) } else { group(d) }
    }

    /// A list that never breaks: where a line break could end a statement (generic parameters
    /// and arguments, closure parameters) or where lists are always short.
    fn flat_list<T>(&mut self, items: &[T], mut item: impl FnMut(&mut Self, &T) -> Doc) -> Doc {
        let mut out = Vec::new();
        for (i, x) in items.iter().enumerate() {
            if i > 0 {
                out.push(text(", "));
            }
            out.push(item(self, x));
        }
        concat(out)
    }

    // ---- items -------------------------------------------------------------------------------

    fn file(&mut self, f: &File) -> Doc {
        let mut out = Vec::new();
        let mut prev: Option<&Item> = None;
        for item in &f.items {
            if let Some(p) = prev {
                let blank = match (&p.kind, &item.kind) {
                    (ItemKind::Use(_), ItemKind::Use(_)) => false,
                    (ItemKind::Const(_), ItemKind::Const(_)) => {
                        self.blank_between(p.span.end, item.span.start)
                    }
                    _ => true,
                };
                out.push(self.hard_break(item.span.start, blank));
            } else {
                out.extend(self.leading(item.span.start));
            }
            out.push(self.item(item));
            prev = Some(item);
        }
        if !f.items.is_empty() {
            out.extend(self.trailing(u32::MAX));
        }
        while let Some(c) = self.next_before(u32::MAX) {
            if !out.is_empty() {
                out.push(Doc::HardLine);
            }
            out.push(text(&c.text));
        }
        concat(out)
    }

    /// Attributes, each on its line, with the comments between it and what follows it.
    fn attrs(&mut self, attrs: &[Attribute]) -> Doc {
        let mut out = Vec::new();
        for a in attrs {
            out.push(self.tok(&format!("@{}", a.name.name)));
            if let Some(args) = &a.args {
                out.push(self.args(args, false, a.span.end));
            }
            let next = self.next_pos();
            out.extend(self.trailing(next));
            out.push(Doc::HardLine);
            out.extend(self.leading(next));
        }
        concat(out)
    }

    /// `pub ` or `pub(package) `, if the item or member has it.
    fn vis(&mut self, vis: Option<Vis>) -> Doc {
        match vis {
            Some(Vis { package: true, .. }) => self.tok("pub(package) "),
            Some(_) => self.tok("pub "),
            None => nil(),
        }
    }

    fn item(&mut self, item: &Item) -> Doc {
        let attrs = self.attrs(&item.attrs);
        let vis = self.vis(item.vis);
        let kind = match &item.kind {
            ItemKind::Fn(f) => self.fn_decl(f),
            ItemKind::Struct(s) => concat([
                self.tok(&format!(
                    "{}struct {}",
                    if s.borrow { "borrow " } else { "" },
                    s.name.name
                )),
                self.generic_params(&s.generics),
                self.trait_list(&s.traits),
                text(" "),
                self.fields(&s.fields, s.multiline, item.span.end),
            ]),
            ItemKind::Enum(e) => {
                let head = concat([
                    self.tok(&format!("enum {}", e.name.name)),
                    self.generic_params(&e.generics),
                    self.trait_list(&e.traits),
                    text(" "),
                ]);
                let l = List { multiline: e.multiline, ..BRACES };
                let body = self.list(
                    &e.variants,
                    |_, v| v.span.start,
                    |b, v| b.variant(v),
                    item.span.end,
                    l,
                );
                concat([head, body])
            }
            ItemKind::Trait(t) => {
                let head = concat([
                    self.tok(&format!("trait {}", t.name.name)),
                    self.generic_params(&t.generics),
                    self.trait_list(&t.supertraits),
                    text(" "),
                ]);
                let body = self.members(
                    &t.members,
                    |m| m.span,
                    item.span.end,
                    |b, m| {
                        let attrs = b.attrs(&m.attrs);
                        let kind = match &m.kind {
                            TraitMemberKind::Fn(f) => b.fn_decl(f),
                            TraitMemberKind::Type { name, bounds } => concat([
                                b.tok(&format!("type {}", name.name)),
                                b.trait_list(bounds),
                            ]),
                        };
                        concat([attrs, kind])
                    },
                );
                concat([head, body])
            }
            ItemKind::TraitSet(t) => concat([
                self.tok(&format!("trait {}", t.name.name)),
                self.generic_params(&t.generics),
                self.tok(" = "),
                self.bounds(&t.traits),
            ]),
            ItemKind::Impl(i) => {
                let mut head = vec![self.tok("impl"), self.generic_params(&i.generics), text(" ")];
                if let Some(t) = &i.trait_ {
                    head.push(self.ty(t));
                    head.push(self.tok(" for "));
                }
                head.push(self.ty(&i.self_ty));
                head.push(text(" "));
                let body = self.members(
                    &i.members,
                    |m| m.span,
                    item.span.end,
                    |b, m| {
                        let attrs = b.attrs(&m.attrs);
                        let vis = b.vis(m.vis);
                        let kind = match &m.kind {
                            ImplMemberKind::Fn(f) => b.fn_decl(f),
                            ImplMemberKind::Type { name, ty } => {
                                concat([b.tok(&format!("type {} = ", name.name)), b.ty(ty)])
                            }
                        };
                        concat([attrs, vis, kind])
                    },
                );
                concat([concat(head), body])
            }
            ItemKind::Const(c) => concat([
                self.tok(&format!("const {}", c.name.name)),
                self.opt_ty(c.ty.as_ref()),
                self.tok(" = "),
                self.expr(&c.value),
            ]),
            ItemKind::Use(u) => {
                let column = match item.vis {
                    Some(Vis { package: true, .. }) => "pub(package) use ".len(),
                    Some(_) => "pub use ".len(),
                    None => "use ".len(),
                };
                concat([self.tok("use "), self.use_tree(u, column)])
            }
            ItemKind::TypeAlias(t) => concat([
                self.tok(&format!("type {}", t.name.name)),
                self.generic_params(&t.generics),
                self.tok(" = "),
                self.ty(&t.ty),
            ]),
            ItemKind::Error(_) => text("<error>"),
        };
        concat([attrs, vis, kind])
    }

    /// A sequence in braces, one per line, keeping single blank lines: a block's statements, or
    /// a trait's or an impl's members.
    fn members<T>(
        &mut self,
        members: &[T],
        span: impl Fn(&T) -> Span,
        end: u32,
        mut member: impl FnMut(&mut Self, &T) -> Doc,
    ) -> Doc {
        if members.is_empty() && !self.has_comment_before(end) {
            return self.tok("{}");
        }
        let open = self.tok("{");
        let mut inner = Vec::new();
        let mut prev_end = None;
        for m in members {
            let s = span(m);
            let blank = prev_end.is_some_and(|e| self.blank_between(e, s.start));
            inner.push(self.hard_break(s.start, blank));
            inner.push(member(self, m));
            prev_end = Some(s.end);
        }
        inner.push(self.hard_close(end));
        block(concat([open, indent(concat(inner)), Doc::HardLine, self.tok("}")]))
    }

    fn trait_list(&mut self, traits: &[TypeExpr]) -> Doc {
        if traits.is_empty() { nil() } else { concat([self.tok(": "), self.bounds(traits)]) }
    }

    fn bounds(&mut self, bounds: &[TypeExpr]) -> Doc {
        let mut out = Vec::new();
        for (i, b) in bounds.iter().enumerate() {
            if i > 0 {
                out.push(self.tok(" + "));
            }
            out.push(self.ty(b));
        }
        concat(out)
    }

    fn generic_params(&mut self, g: &[GenericParam]) -> Doc {
        if g.is_empty() {
            // `<>` stays if it was written.
            let written = self.text[self.next_pos() as usize..].starts_with('<');
            return if written { self.tok("<>") } else { nil() };
        }
        let open = self.tok("<");
        let params = self.flat_list(g, |b, p| match &p.const_ty {
            Some(t) => concat([b.tok(&format!("const {}: ", p.name.name)), b.ty(t)]),
            None => concat([b.tok(&p.name.name), b.trait_list(&p.bounds)]),
        });
        concat([open, params, self.close_before('>'), self.tok(">")])
    }

    fn fields(&mut self, fields: &[FieldDecl], multiline: bool, end: u32) -> Doc {
        let l = List { multiline, ..BRACES };
        self.list(fields, |_, f| f.span.start, |b, f| b.field_decl(f), end, l)
    }

    fn field_decl(&mut self, f: &FieldDecl) -> Doc {
        let vis = self.vis(f.vis);
        let name = self.tok(&format!("{}: ", f.name.name));
        let mode = match f.mode {
            RetMode::Owned => nil(),
            RetMode::Borrow => self.tok("borrow "),
            RetMode::Mut => self.tok("mut "),
        };
        let ty = concat([mode, self.ty(&f.ty)]);
        let default = match &f.default {
            Some(d) => concat([self.tok(" = "), self.expr(d)]),
            None => nil(),
        };
        concat([vis, name, ty, default])
    }

    fn variant(&mut self, v: &Variant) -> Doc {
        let name = self.tok(&v.name.name);
        let kind = match &v.kind {
            VariantKind::Unit => nil(),
            VariantKind::Tuple(tys) => {
                let open = self.tok("(");
                let list = self.flat_list(tys, |b, t| b.ty(t));
                let close = self.close_before(')');
                concat([open, list, close, self.tok(")")])
            }
            VariantKind::Struct(fields) => {
                concat([text(" "), self.fields(fields, false, v.span.end)])
            }
        };
        concat([name, kind])
    }

    fn fn_decl(&mut self, f: &FnDecl) -> Doc {
        let mut out =
            vec![self.tok(&format!("fn {}", f.name.name)), self.generic_params(&f.generics)];
        let end = f.params_close.end;
        out.push(self.list(&f.params, |_, p| p.span().start, |b, p| b.param(p), end, PARENS));
        if let Some(r) = &f.ret {
            out.push(self.tok(match r.mode {
                RetMode::Owned => " -> ",
                RetMode::Borrow => " -> borrow ",
                RetMode::Mut => " -> mut ",
            }));
            out.push(self.ty(&r.ty));
        }
        if let Some(b) = &f.body {
            out.push(text(" "));
            out.push(self.block(b, false));
        }
        concat(out)
    }

    fn param(&mut self, p: &Param) -> Doc {
        let mode = |b: &mut Self, m: Mode| {
            if m == Mode::Borrow { nil() } else { b.tok(&format!("{} ", m.keyword())) }
        };
        match p {
            Param::SelfParam { mode: m, .. } => concat([mode(self, *m), self.tok("self")]),
            Param::Named { name, mode: m, ty, default, .. } => {
                let name = self.tok(&format!("{}: ", name.name));
                let mode = mode(self, *m);
                let ty = self.ty(ty);
                let default = match default {
                    Some(d) => concat([self.tok(" = "), self.expr(d)]),
                    None => nil(),
                };
                concat([name, mode, ty, default])
            }
        }
    }

    // ---- types -------------------------------------------------------------------------------

    /// `: T`, if there's a type.
    fn opt_ty(&mut self, t: Option<&TypeExpr>) -> Doc {
        match t {
            Some(t) => concat([self.tok(": "), self.ty(t)]),
            None => nil(),
        }
    }

    fn ty(&mut self, t: &TypeExpr) -> Doc {
        let pre = self.inline_comments(t.span.start);
        let d = match &t.kind {
            TypeExprKind::Path(p) => self.path(p, false),
            TypeExprKind::Array(elem, len) => {
                let open = self.tok("[");
                let elem = self.ty(elem);
                let len = match len {
                    Some(l) => concat([self.tok("; "), self.expr(l)]),
                    None => nil(),
                };
                let close = self.inline_close(t.span.end - 1);
                concat([open, elem, len, close, self.tok("]")])
            }
            TypeExprKind::Tuple(tys) => {
                let open = self.tok("(");
                let list = self.flat_list(tys, |b, t| b.ty(t));
                let lone = if tys.len() == 1 { text(",") } else { nil() };
                let close = self.inline_close(t.span.end - 1);
                concat([open, list, lone, close, self.tok(")")])
            }
            TypeExprKind::Traits(tys) => {
                let mut parts = Vec::new();
                for (i, t) in tys.iter().enumerate() {
                    if i > 0 {
                        parts.push(self.tok(" + "));
                    }
                    parts.push(self.ty(t));
                }
                concat(parts)
            }
            TypeExprKind::Paren(inner) => {
                let open = self.tok("(");
                let inner = self.ty(inner);
                concat([open, inner, self.inline_close(t.span.end - 1), self.tok(")")])
            }
            TypeExprKind::Fn(FnType { attrs, params, ret }) => {
                let attrs: Vec<Doc> =
                    attrs.iter().map(|a| self.tok(&format!("@{} ", a.name))).collect();
                let open = concat([concat(attrs), self.tok("fn(")]);
                let list = self.flat_list(params, |b, p| {
                    let mode = match p.mode {
                        Mode::Borrow => nil(),
                        m => b.tok(&format!("{} ", m.keyword())),
                    };
                    concat([mode, b.ty(&p.ty)])
                });
                let close = concat([self.close_before(')'), self.tok(")")]);
                let ret = match ret {
                    Some(r) => concat([self.tok(" -> "), self.ty(r)]),
                    None => nil(),
                };
                concat([open, list, close, ret])
            }
            TypeExprKind::Int(l) => self.tok(&l.text),
            TypeExprKind::Error => text("<error>"),
        };
        concat([pre, d])
    }

    /// A path; in expressions, generic arguments are written with `::<`.
    fn path(&mut self, p: &Path, expr: bool) -> Doc {
        let mut out = Vec::new();
        for (i, seg) in p.segments.iter().enumerate() {
            if i > 0 {
                out.push(self.tok("::"));
            }
            out.push(self.tok(&seg.ident.name));
            if let Some(g) = &seg.generics {
                if expr {
                    out.push(self.tok("::"));
                }
                out.push(self.generic_args(g));
            }
        }
        concat(out)
    }

    fn generic_args(&mut self, g: &[TypeExpr]) -> Doc {
        let open = self.tok("<");
        let list = self.flat_list(g, |b, t| b.ty(t));
        let close = self.close_before('>');
        concat([open, list, close, self.tok(">")])
    }

    // ---- blocks and statements ---------------------------------------------------------------

    /// A block. One that held one statement on one line stays on one line if it fits, unless
    /// it's a function body.
    fn block(&mut self, b: &Block, allow_inline: bool) -> Doc {
        if allow_inline
            && b.stmts.len() == 1
            && !self.spans_lines(b.span)
            && !self.has_comment_before(b.span.end)
        {
            let open = self.tok("{");
            let s = self.stmt(&b.stmts[0]);
            let close = self.tok("}");
            if !s.has_hard_line() {
                return alone(concat([open, indent(concat([Doc::Line, s])), Doc::Line, close]));
            }
            // What `members` makes of it, with no comments to place, without formatting the
            // statement again (for blocks nested on one line, that doubles at each level).
            return block(concat([open, indent(concat([Doc::HardLine, s])), Doc::HardLine, close]));
        }
        self.members(&b.stmts, |s| s.span, b.span.end, Self::stmt)
    }

    fn spans_lines(&self, s: Span) -> bool {
        self.text[s.range()].contains('\n')
    }

    fn stmt(&mut self, s: &Stmt) -> Doc {
        match &s.kind {
            StmtKind::Let { pat, ty, init, else_ } => {
                let mut out = vec![
                    self.tok("let "),
                    self.pat(pat),
                    self.opt_ty(ty.as_ref()),
                    self.tok(" = "),
                    self.expr(init),
                ];
                if let Some(b) = else_ {
                    out.push(self.tok(" else "));
                    out.push(self.block(b, true));
                }
                concat(out)
            }
            StmtKind::Var { kind, name, ty, init } => concat([
                self.tok(&format!("{} {}", kind.keyword(), name.name)),
                self.opt_ty(ty.as_ref()),
                self.tok(" = "),
                self.expr(init),
            ]),
            StmtKind::Assign { target, op, value } => {
                concat([self.expr(target), self.tok(&format!(" {} ", op.text())), self.expr(value)])
            }
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::While { cond, body } => {
                concat([self.tok("while "), self.expr(cond), text(" "), self.block(body, true)])
            }
            StmtKind::Loop { body } => concat([self.tok("loop "), self.block(body, true)]),
            StmtKind::For { mutable, pat, iter, body } => {
                let mut out = vec![self.tok(if *mutable { "for mut " } else { "for " })];
                out.push(self.pat(pat));
                out.push(self.tok(" in "));
                match iter {
                    ForIter::Range { start, end, inclusive } => {
                        out.push(self.expr(start));
                        out.push(self.tok(if *inclusive { "..=" } else { ".." }));
                        out.push(self.expr(end));
                    }
                    ForIter::Expr(e) => out.push(self.expr(e)),
                }
                out.push(text(" "));
                out.push(self.block(body, true));
                concat(out)
            }
        }
    }

    // ---- expressions -------------------------------------------------------------------------

    /// A call's arguments; `end` is where the call ends (just past its `)`).
    fn args(&mut self, args: &[Arg], multiline: bool, end: u32) -> Doc {
        let l = List { multiline, ..PARENS };
        self.list(args, |_, a| a.span.start, |b, a| b.arg(a), end, l)
    }

    fn arg(&mut self, a: &Arg) -> Doc {
        let name = match &a.name {
            Some(n) => self.tok(&format!("{}: ", n.name)),
            None => nil(),
        };
        concat([name, self.expr(&a.value)])
    }

    fn expr(&mut self, e: &Expr) -> Doc {
        let pre = self.inline_comments(e.span.start);
        let d = self.expr_kind(e);
        concat([pre, d])
    }

    fn expr_kind(&mut self, e: &Expr) -> Doc {
        match &e.kind {
            ExprKind::Lit(l) => self.tok(&l.text),
            ExprKind::Path(p) => self.path(p, true),
            ExprKind::Unary(op, inner) => {
                let op = match op {
                    UnOp::Neg => "-",
                    UnOp::Not => "!",
                };
                concat([self.tok(op), self.expr(inner)])
            }
            ExprKind::Binary(..) => self.binary(e),
            ExprKind::Take(inner) => concat([self.tok("take "), self.expr(inner)]),
            ExprKind::MutArg(inner) => concat([self.tok("mut "), self.expr(inner)]),
            ExprKind::Call { callee, args, multiline } => {
                concat([self.expr(callee), self.args(args, *multiline, e.span.end)])
            }
            ExprKind::MethodCall { .. } | ExprKind::Field { .. } => self.chain(e),
            ExprKind::Index { base, index } => concat([
                self.expr(base),
                self.tok("["),
                self.expr(index),
                self.inline_close(e.span.end - 1),
                self.tok("]"),
            ]),
            ExprKind::StructLit { path, fields, base, multiline } => {
                let path = self.path(path, true);
                if fields.is_empty() && base.is_none() && !self.has_comment_before(e.span.end) {
                    return concat([path, self.tok(" {}")]);
                }
                let mut parts: Vec<Part> = fields.iter().map(Part::Field).collect();
                parts.extend(base.as_deref().map(Part::Base));
                let start = |b: &Self, p: &Part| match p {
                    Part::Field(f) => f.span.start,
                    Part::Base(_) => b.next_pos(),
                };
                let item = |b: &mut Self, p: &Part| match p {
                    Part::Field(f) => b.field_init(f),
                    Part::Base(x) => concat([b.tok(".."), b.expr(x)]),
                };
                let l = List { multiline: *multiline, ..BRACES };
                let body = self.list(&parts, start, item, e.span.end, l);
                concat([path, text(" "), body])
            }
            ExprKind::Tuple(items) => {
                let l = List { lone_comma: true, ..PARENS };
                self.list(items, |_, x| x.span.start, |b, x| b.expr(x), e.span.end, l)
            }
            ExprKind::Array(items) => {
                let multiline = items.first().is_some_and(|x| {
                    self.text[e.span.start as usize..x.span.start as usize].contains('\n')
                });
                let l = List { open: "[", close: "]", multiline, ..PARENS };
                self.list(items, |_, x| x.span.start, |b, x| b.expr(x), e.span.end, l)
            }
            ExprKind::ArrayRepeat { value, count } => concat([
                self.tok("["),
                self.expr(value),
                self.tok("; "),
                self.expr(count),
                self.inline_close(e.span.end - 1),
                self.tok("]"),
            ]),
            ExprKind::Paren(inner) => concat([
                self.tok("("),
                self.expr(inner),
                self.inline_close(e.span.end - 1),
                self.tok(")"),
            ]),
            ExprKind::Block(b) => self.block(b, true),
            ExprKind::If { pat, cond, then, else_ } => {
                let mut out = vec![self.tok("if ")];
                if let Some(p) = pat {
                    out.push(self.tok("let "));
                    out.push(self.pat(p));
                    out.push(self.tok(" = "));
                }
                out.extend([self.expr(cond), text(" "), self.block(then, true)]);
                if let Some(x) = else_ {
                    out.push(self.tok(" else "));
                    out.push(self.expr(x));
                }
                concat(out)
            }
            ExprKind::Match { mutable, scrutinee, arms } => {
                let kw = self.tok(if *mutable { "match mut " } else { "match " });
                let head = concat([kw, self.expr(scrutinee), text(" ")]);
                let open = self.tok("{");
                let mut inner = Vec::new();
                for arm in arms {
                    inner.push(self.hard_break(arm.span.start, false));
                    let mut a = Vec::new();
                    for (i, p) in arm.pats.iter().enumerate() {
                        if i > 0 {
                            a.push(self.tok(" | "));
                        }
                        a.push(self.pat(p));
                    }
                    if let Some(g) = &arm.guard {
                        a.push(self.tok(" if "));
                        a.push(self.expr(g));
                    }
                    a.push(self.tok(" => "));
                    a.push(self.expr(&arm.body));
                    a.push(text(","));
                    inner.push(concat(a));
                }
                inner.push(self.hard_close(e.span.end));
                let body =
                    block(concat([open, indent(concat(inner)), Doc::HardLine, self.tok("}")]));
                concat([head, body])
            }
            ExprKind::Closure { params, ret, body } => {
                let params = if params.is_empty() {
                    self.tok("||")
                } else {
                    let open = self.tok("|");
                    let ps = self.flat_list(params, |b, p| {
                        concat([b.tok(&p.name.name), b.opt_ty(p.ty.as_ref())])
                    });
                    concat([open, ps, self.close_before('|'), self.tok("|")])
                };
                let ret = match ret {
                    Some(r) => concat([self.tok("-> "), self.ty(r), text(" ")]),
                    None => nil(),
                };
                concat([params, text(" "), ret, self.expr(body)])
            }
            ExprKind::Return(v) => match v {
                Some(v) => concat([self.tok("return "), self.expr(v)]),
                None => self.tok("return"),
            },
            ExprKind::Break => self.tok("break"),
            ExprKind::Continue => self.tok("continue"),
            ExprKind::Try(inner) => concat([self.expr(inner), self.tok("?")]),
            ExprKind::Assign { target, op, value } => {
                concat([self.expr(target), self.tok(&format!(" {} ", op.text())), self.expr(value)])
            }
            ExprKind::Unsafe(b) => concat([self.tok("unsafe "), self.block(b, true)]),
            // Its pieces as written, and each hole's expression formatted on one line: an
            // f-string is on one line (L22), and its holes can't hold comments.
            ExprKind::FString(parts) => {
                let mut out = vec![self.code_token()];
                for p in parts {
                    if let FPart::Hole { expr, .. } = p {
                        out.push(self.flat_expr(expr));
                        out.push(self.code_token());
                    }
                }
                concat(out)
            }
            ExprKind::Error => text("<error>"),
        }
    }

    /// A chain of operators of one precedence (`a + b - c`): on one line if it fits, and
    /// otherwise broken after each operator, the operands after the first indented.
    fn binary(&mut self, e: &Expr) -> Doc {
        let ExprKind::Binary(top, ..) = &e.kind else { return self.expr_kind(e) };
        // `a ** b ** c` is `a ** (b ** c)`, and comparisons don't chain: neither is flattened.
        let chains = |op: BinOp| {
            op != BinOp::Pow && !op.is_comparison() && op.precedence() == top.precedence()
        };
        let mut rest: Vec<(BinOp, &Expr)> = Vec::new();
        let mut first = e;
        while let ExprKind::Binary(op, a, b) = &first.kind {
            rest.push((*op, b));
            first = a;
            if !chains(*op) || !matches!(&first.kind, ExprKind::Binary(next, ..) if chains(*next)) {
                break;
            }
        }
        rest.reverse();
        let head = self.expr(first);
        let mut tail = Vec::new();
        for (op, operand) in rest {
            tail.push(self.tok(&format!(" {}", op.text())));
            // A line break after `>` or `>>` could end the statement (L20).
            if matches!(op, BinOp::Gt | BinOp::Shr) {
                tail.push(text(" "));
            } else {
                tail.push(self.soft_break(operand.span.start, Doc::Line));
            }
            tail.push(self.expr(operand));
        }
        group(concat([head, indent(concat(tail))]))
    }

    /// Field accesses and method calls. A call written on its own line (a leading `.`) stays
    /// there; a chain of two or more calls with none written so goes on one line if it fits,
    /// and otherwise puts each call on its own line.
    fn chain(&mut self, e: &Expr) -> Doc {
        let mut links: Vec<&Expr> = Vec::new();
        let mut base = e;
        while let ExprKind::MethodCall { receiver: b, .. } | ExprKind::Field { base: b, .. } =
            &base.kind
        {
            links.push(base);
            base = b;
        }
        links.reverse();
        let calls = links.iter().filter(|l| matches!(l.kind, ExprKind::MethodCall { .. })).count();
        let leading_dots = links
            .iter()
            .any(|l| matches!(l.kind, ExprKind::MethodCall { newline_before: true, .. }));
        let fit = calls >= 2 && !leading_dots;
        let mut out = vec![self.expr(base)];
        let mut tail = Vec::new();
        // The last thing written is a tuple index on the line after its `.`: a number that, like
        // a literal, takes a `.` and a digit after it (L12).
        let mut loose = false;
        for link in links {
            match &link.kind {
                ExprKind::Field { base: b, name } => {
                    let index = matches!(name, FieldName::Index(..));
                    let mut d = Vec::new();
                    // `1 .0` isn't `1.0`.
                    if index && (loose || matches!(b.kind, ExprKind::Lit(_))) {
                        d.push(text(" "));
                    }
                    loose = index && self.has_comment_before(name.span().start);
                    // A tuple index as written: `t.0x1` is `t.1`, but it's spelled `0x1`.
                    let name = &self.text[name.span().range()];
                    d.push(self.tok(&format!(".{name}")));
                    if fit { &mut tail } else { &mut out }.extend(d);
                }
                ExprKind::MethodCall {
                    name, generics, args, newline_before, multiline, ..
                } => {
                    loose = false;
                    // The break before the `.` first: the comments before it are its.
                    let dot = self.next_pos();
                    let brk = (*newline_before || fit).then(|| self.soft_break(dot, Doc::SoftLine));
                    let mut d = vec![self.tok(&format!(".{}", name.name))];
                    if let Some(g) = generics {
                        d.push(self.tok("::"));
                        d.push(self.generic_args(g));
                    }
                    d.push(self.args(args, *multiline, link.span.end));
                    match brk {
                        Some(brk) if *newline_before => {
                            out.push(broken(indent(concat([brk, concat(d)]))));
                        }
                        Some(brk) => {
                            tail.push(brk);
                            tail.extend(d);
                        }
                        None => out.extend(d),
                    }
                }
                _ => {}
            }
        }
        if fit {
            out.push(indent(concat(tail)));
        }
        // A group either way, so a chain is the same to the groups around it however its
        // calls were laid out (a comment in it breaks it, not them).
        group(concat(out))
    }

    fn field_init(&mut self, f: &FieldInit) -> Doc {
        match &f.value {
            Some(v) => concat([self.tok(&format!("{}: ", f.name.name)), self.expr(v)]),
            None => self.tok(&f.name.name),
        }
    }

    /// How many parentheses around pattern `p` the parser dropped: `(p)` parses as `p`, with
    /// the parentheses' span. The cursor is at the pattern.
    fn pattern_parens(&self, p: &Pat) -> usize {
        // A tuple's own `(` is the last before its first element, or, if it's empty, before
        // its `)`.
        let (own, first) = match &p.kind {
            PatKind::Tuple(pats) => (1, pats.first().map(|x| x.span.start)),
            _ => (0, None),
        };
        let opens = self.code[self.at.0..]
            .iter()
            .take_while(|t| &self.text[t.range()] == "(" && first.is_none_or(|f| t.start < f))
            .count();
        opens.saturating_sub(own)
    }

    fn pat(&mut self, p: &Pat) -> Doc {
        let mut out = vec![self.inline_comments(p.span.start)];
        let parens = self.pattern_parens(p);
        // Where the pattern ends inside them: before the last `parens` code tokens.
        let last = self.code.partition_point(|t| t.end <= p.span.end);
        let end = self.code.get(last.wrapping_sub(parens + 1)).map_or(p.span.end, |t| t.end);
        for _ in 0..parens {
            out.push(self.tok("("));
        }
        out.push(self.pat_kind(p, end));
        for _ in 0..parens {
            let close = self.next_pos();
            out.push(self.inline_close(close));
            out.push(self.tok(")"));
        }
        concat(out)
    }

    /// A pattern's own syntax, which ends at `end`.
    fn pat_kind(&mut self, p: &Pat, end: u32) -> Doc {
        match &p.kind {
            PatKind::Wild => self.tok("_"),
            PatKind::Ident(i) => self.tok(&i.name),
            PatKind::Lit { neg, lit } => {
                self.tok(&if *neg { format!("-{}", lit.text) } else { lit.text.clone() })
            }
            PatKind::Path(path) => self.path(path, true),
            PatKind::TupleStruct(path, pats) => {
                let path = self.path(path, true);
                let list = self.list(pats, |_, x| x.span.start, |b, x| b.pat(x), end, PARENS);
                concat([path, list])
            }
            PatKind::Struct { path, fields, rest } => {
                let path = self.path(path, true);
                if fields.is_empty() && !rest && !self.has_comment_before(end) {
                    return concat([path, self.tok(" {}")]);
                }
                let mut parts: Vec<Option<&FieldPat>> = fields.iter().map(Some).collect();
                if *rest {
                    parts.push(None);
                }
                let start = |b: &Self, f: &Option<&FieldPat>| match f {
                    Some(f) => f.name.span.start,
                    None => b.next_pos(),
                };
                let item = |b: &mut Self, f: &Option<&FieldPat>| match f {
                    Some(FieldPat { name, pat: Some(x) }) => {
                        concat([b.tok(&format!("{}: ", name.name)), b.pat(x)])
                    }
                    Some(FieldPat { name, pat: None }) => b.tok(&name.name),
                    None => b.tok(".."),
                };
                let l = List { rest_last: *rest, ..BRACES };
                let body = self.list(&parts, start, item, end, l);
                concat([path, text(" "), body])
            }
            PatKind::Tuple(pats) => {
                let l = List { lone_comma: true, ..PARENS };
                self.list(pats, |_, x| x.span.start, |b, x| b.pat(x), end, l)
            }
            PatKind::Error => text("<error>"),
        }
    }

    /// A `use` tree starting at `column`. A group goes on one line if it fits in the width;
    /// otherwise its items are packed into lines of up to the width. A group with a comment in
    /// it is a list: one item per line, so each comment has its place.
    fn use_tree(&mut self, u: &UseTree, column: usize) -> Doc {
        let UseKind::Group(trees) = &u.kind else { return self.tok(&use_text(u)) };
        let path = use_path(u);
        if self.has_comment_before(u.span.end) {
            let head = self.tok(&format!("{path}::"));
            let l = List { padded: false, ..BRACES };
            let column = INDENT;
            let body =
                self.list(trees, |_, t| t.span.start, |b, t| b.use_tree(t, column), u.span.end, l);
            return concat([head, body]);
        }
        let items: Vec<String> = trees.iter().map(use_text).collect();
        let inline = items.join(", ");
        if column + path.len() + 4 + inline.len() <= MAX_WIDTH {
            return self.tok(&format!("{path}::{{{inline}}}"));
        }
        let start = INDENT;
        let mut lines = vec![Doc::HardLine];
        let mut line = start;
        let head = self.tok(&format!("{path}::{{"));
        for (i, item) in items.iter().enumerate() {
            let piece = item.len() + 1;
            if i > 0 {
                if line + 1 + piece > MAX_WIDTH {
                    lines.push(Doc::HardLine);
                    line = start;
                } else {
                    lines.push(text(" "));
                    line += 1;
                }
            }
            lines.push(self.tok(&format!("{item},")));
            line += piece;
        }
        broken(concat([head, indent(concat(lines)), Doc::HardLine, self.tok("}")]))
    }
}

/// A part of a struct literal.
enum Part<'e> {
    Field(&'e FieldInit),
    Base(&'e Expr),
}

/// The comments directly before a node that no layout placed: the first at the end of the
/// current line (in `out`) if it trailed code there, and the others on lines of their own,
/// returned.
fn place_here(src: &str, here: Vec<&Comment>, out: &mut Vec<Doc>) -> Vec<Doc> {
    let mut lines = Vec::new();
    for (i, c) in here.into_iter().enumerate() {
        if i == 0 && !on_own_line(src, c) {
            out.push(Doc::LineSuffix(format!(" {}", c.text)));
        } else {
            lines.push(Doc::HardLine);
            lines.push(text(&c.text));
        }
    }
    lines
}

/// A `use` tree's path: `a::b`.
fn use_path(u: &UseTree) -> String {
    let mut path = String::new();
    for s in &u.path {
        if !path.is_empty() {
            path.push_str("::");
        }
        path.push_str(&s.name);
    }
    path
}

/// A `use` tree on one line.
fn use_text(u: &UseTree) -> String {
    let path = use_path(u);
    match &u.kind {
        UseKind::Simple(None) => path,
        UseKind::Simple(Some(r)) => format!("{path} as {}", r.name),
        UseKind::Group(trees) => {
            let items: Vec<String> = trees.iter().map(use_text).collect();
            format!("{path}::{{{}}}", items.join(", "))
        }
    }
}
