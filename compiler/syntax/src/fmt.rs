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
//! - Comments stay where they are: on their own lines before the item, statement, element, arm,
//!   operand or closing bracket they come before, or at the end of the line they trailed. A list
//!   or chain with a comment inside breaks, so the comment can end its line. A comment where the
//!   layout has no line break (between a keyword and a name, say) moves to the end of its line.
//! - It never changes what a file means: parsing its output gives the same AST (tests check this
//!   and that formatting is idempotent).

mod doc;

use crate::ast::*;
use crate::parser::Parsed;
use crate::token::Comment;
use doc::{Doc, alone, block, broken, concat, group, if_break, indent, nil, text};
use wrela_diag::Span;

/// The width lines are laid out in.
const MAX_WIDTH: usize = 100;

/// Formats a file that parsed without errors.
pub fn format(parsed: &Parsed, text: &str) -> String {
    let mut out = format_once(parsed, text);
    // A comment the layout has no place for moves to the end of a line, and there the next
    // pass may lay its surroundings out differently. That settles after a move or two; the
    // result is what formatting keeps.
    for _ in 0..4 {
        let again = crate::parser::parse(wrela_diag::FileId(0), &out);
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

fn format_once(parsed: &Parsed, text: &str) -> String {
    let mut b = Builder { comments: &parsed.comments, next: 0, text };
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
}

/// How a list is written.
#[derive(Clone, Copy)]
struct List {
    open: &'static str,
    close: &'static str,
    /// Spaces inside the brackets when it's on one line (`{ x: 1 }`).
    padded: bool,
    /// One element per line whatever the width: the author wrote it so.
    multiline: bool,
    /// A comma after a lone element even on one line (`(x,)`).
    lone_comma: bool,
}

const PARENS: List =
    List { open: "(", close: ")", padded: false, multiline: false, lone_comma: false };
const BRACES: List =
    List { open: "{", close: "}", padded: true, multiline: false, lone_comma: false };

impl<'a> Builder<'a> {
    // ---- comments ----------------------------------------------------------------------------

    fn pending(&self) -> Option<&'a Comment> {
        self.comments.get(self.next)
    }

    fn has_comment_before(&self, pos: u32) -> bool {
        self.pending().is_some_and(|c| c.span.start < pos)
    }

    /// The comments before `pos` that trailed code on their lines, for the end of the line.
    fn trailing(&mut self, pos: u32) -> Vec<Doc> {
        let mut out = Vec::new();
        while let Some(c) = self.pending() {
            if c.span.start >= pos || c.own_line {
                break;
            }
            self.next += 1;
            out.push(Doc::LineSuffix(format!(" {}", c.text)));
        }
        out
    }

    /// The comments before `pos`, each with a line break before it. The line breaks belong to
    /// the caller's indentation.
    fn own_lines(&mut self, pos: u32) -> Vec<Doc> {
        let mut out = Vec::new();
        while let Some(c) = self.pending() {
            if c.span.start >= pos {
                break;
            }
            self.next += 1;
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
        let parts: Vec<&str> = self.text[a as usize..b as usize].split('\n').collect();
        parts.len() > 2 && parts[1..parts.len() - 1].iter().any(|l| l.trim().is_empty())
    }

    /// The comments before `pos`, at the start of a line: each on its own line, and a blank
    /// line after one where the source has one (a file's header, a section's divider).
    fn leading(&mut self, pos: u32) -> Vec<Doc> {
        let mut out = Vec::new();
        while let Some(c) = self.pending() {
            if c.span.start >= pos {
                break;
            }
            self.next += 1;
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
    /// before the node: trailing ones end the line before, the rest go on lines of their own.
    /// Either kind breaks the group.
    fn soft_break(&mut self, pos: u32, line: Doc) -> Doc {
        let mut out = self.trailing(pos);
        let mut any = !out.is_empty();
        out.push(line);
        while let Some(c) = self.pending() {
            if c.span.start >= pos {
                break;
            }
            self.next += 1;
            any = true;
            out.push(text(&c.text));
            out.push(Doc::HardLine);
        }
        if any {
            out.push(Doc::BreakParent);
        }
        concat(out)
    }

    /// Whether only space and comments come between `from` and `pos` in the source: a comment
    /// that ends at `from` is directly before what's at `pos`, so the line break after it was
    /// there too.
    fn adjacent(&self, from: u32, pos: u32) -> bool {
        let mut i = from;
        while i < pos {
            let b = self.text.as_bytes()[i as usize];
            if b.is_ascii_whitespace() {
                i += 1;
            } else if let Some(c) = self.comments.iter().find(|c| c.span.start == i) {
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
        while let Some(c) = self.pending() {
            if c.span.start >= pos {
                break;
            }
            self.next += 1;
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
        let mut lines = Vec::new();
        for (i, c) in here.into_iter().enumerate() {
            if i == 0 && !c.own_line {
                out.push(Doc::LineSuffix(format!(" {}", c.text)));
            } else {
                lines.push(Doc::HardLine);
                lines.push(text(&c.text));
            }
        }
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
        let mut lines = Vec::new();
        for (i, c) in here.into_iter().enumerate() {
            if i == 0 && !c.own_line {
                out.push(Doc::LineSuffix(format!(" {}", c.text)));
            } else {
                lines.push(Doc::HardLine);
                lines.push(text(&c.text));
            }
        }
        out.push(Doc::BreakParent);
        out.push(broken(concat([indent(concat(lines)), Doc::HardLine])));
        concat(out)
    }

    /// The comments before the `close` that ends a list whose last element ends at `from`:
    /// placed before it, so none moves past a closer, where a line break could end a statement.
    fn close_after(&mut self, from: u32, close: char) -> Doc {
        // Between the last element and the closer: only space, comments and a trailing comma.
        let mut i = from as usize;
        let bytes = self.text.as_bytes();
        loop {
            match bytes.get(i) {
                Some(b) if b.is_ascii_whitespace() || *b == b',' => i += 1,
                Some(b'/') => match self.comments.iter().find(|c| c.span.start as usize == i) {
                    Some(c) => i = c.span.end as usize,
                    None => break,
                },
                _ => break,
            }
        }
        if bytes.get(i) == Some(&(close as u8)) {
            self.close_comments(i as u32, close == '>')
        } else {
            nil()
        }
    }

    // ---- lists -------------------------------------------------------------------------------

    /// A comma-separated list in brackets, ending at `end` (just past its closer).
    fn list<T>(
        &mut self,
        items: &[T],
        start: impl Fn(&T) -> u32,
        mut item: impl FnMut(&mut Self, &T) -> Doc,
        end: u32,
        l: List,
    ) -> Doc {
        // Comments up to the closer belong inside the brackets.
        let inner_end = end.saturating_sub(1);
        if items.is_empty() && !self.has_comment_before(inner_end) {
            return text(format!("{}{}", l.open, l.close));
        }
        let line = || if l.padded { Doc::Line } else { Doc::SoftLine };
        let mut inner = Vec::new();
        for (i, x) in items.iter().enumerate() {
            if i > 0 {
                inner.push(text(","));
            }
            inner.push(self.soft_break(start(x), if i == 0 { line() } else { Doc::Line }));
            inner.push(item(self, x));
        }
        if !items.is_empty() {
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
        let d = concat([text(l.open), indent(concat(inner)), line(), text(l.close)]);
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
                let both_use =
                    matches!(p.kind, ItemKind::Use(_)) && matches!(item.kind, ItemKind::Use(_));
                let both_const =
                    matches!(p.kind, ItemKind::Const(_)) && matches!(item.kind, ItemKind::Const(_));
                let blank = if both_use {
                    false
                } else if both_const {
                    self.blank_between(p.span.end, item.span.start)
                } else {
                    true
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
        while let Some(c) = self.pending() {
            self.next += 1;
            if !out.is_empty() {
                out.push(Doc::HardLine);
            }
            out.push(text(&c.text));
        }
        concat(out)
    }

    fn attrs(&mut self, attrs: &[Attribute]) -> Doc {
        let mut out = Vec::new();
        for a in attrs {
            out.push(text(format!("@{}", a.name.name)));
            if let Some(args) = &a.args {
                out.push(self.args(args, false, a.span.end));
            }
            // A comment after the attribute, on its line, stays there.
            let end = a.span.end as usize;
            let eol = self.text[end..].find('\n').map_or(self.text.len(), |i| end + i);
            out.extend(self.trailing(eol as u32));
            out.push(Doc::HardLine);
        }
        concat(out)
    }

    fn item(&mut self, item: &Item) -> Doc {
        let attrs = self.attrs(&item.attrs);
        let vis = if item.vis.is_some() { text("pub ") } else { nil() };
        let kind = match &item.kind {
            ItemKind::Fn(f) => self.fn_decl(f),
            ItemKind::Struct(s) => concat([
                text(format!("struct {}", s.name.name)),
                self.generic_params(&s.generics),
                self.trait_list(&s.traits),
                text(" "),
                self.fields(&s.fields, s.multiline, item.span.end),
            ]),
            ItemKind::Enum(e) => {
                let head = concat([
                    text(format!("enum {}", e.name.name)),
                    self.generic_params(&e.generics),
                    self.trait_list(&e.traits),
                    text(" "),
                ]);
                let l = List { multiline: e.multiline, ..BRACES };
                let body =
                    self.list(&e.variants, |v| v.span.start, |b, v| b.variant(v), item.span.end, l);
                concat([head, body])
            }
            ItemKind::Trait(t) => {
                let head = concat([
                    text(format!("trait {}", t.name.name)),
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
                            TraitMemberKind::Type { name, bounds } => {
                                concat([text(format!("type {}", name.name)), b.trait_list(bounds)])
                            }
                        };
                        concat([attrs, kind])
                    },
                );
                concat([head, body])
            }
            ItemKind::Impl(i) => {
                let mut head = vec![text("impl"), self.generic_params(&i.generics), text(" ")];
                if let Some(t) = &i.trait_ {
                    head.push(self.ty(t));
                    head.push(text(" for "));
                }
                head.push(self.ty(&i.self_ty));
                head.push(text(" "));
                let body = self.members(
                    &i.members,
                    |m| m.span,
                    item.span.end,
                    |b, m| {
                        let attrs = b.attrs(&m.attrs);
                        let vis = if m.vis.is_some() { text("pub ") } else { nil() };
                        let kind = match &m.kind {
                            ImplMemberKind::Fn(f) => b.fn_decl(f),
                            ImplMemberKind::Type { name, ty } => {
                                concat([text(format!("type {} = ", name.name)), b.ty(ty)])
                            }
                        };
                        concat([attrs, vis, kind])
                    },
                );
                concat([concat(head), body])
            }
            ItemKind::Const(c) => {
                let ty = match &c.ty {
                    Some(t) => concat([text(": "), self.ty(t)]),
                    None => nil(),
                };
                concat([
                    text(format!("const {}", c.name.name)),
                    ty,
                    text(" = "),
                    self.expr(&c.value),
                ])
            }
            ItemKind::Use(u) => {
                let column = if item.vis.is_some() { "pub use ".len() } else { "use ".len() };
                concat([text("use "), use_tree(u, column)])
            }
            ItemKind::Error(_) => text("<error>"),
        };
        concat([attrs, vis, kind])
    }

    /// A trait's or impl's members in braces, one per line, keeping single blank lines.
    fn members<T>(
        &mut self,
        members: &[T],
        span: impl Fn(&T) -> Span,
        end: u32,
        mut member: impl FnMut(&mut Self, &T) -> Doc,
    ) -> Doc {
        if members.is_empty() && !self.has_comment_before(end) {
            return text("{}");
        }
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
        block(concat([text("{"), indent(concat(inner)), Doc::HardLine, text("}")]))
    }

    fn trait_list(&mut self, traits: &[TypeExpr]) -> Doc {
        if traits.is_empty() { nil() } else { concat([text(": "), self.bounds(traits)]) }
    }

    fn bounds(&mut self, bounds: &[TypeExpr]) -> Doc {
        let mut out = Vec::new();
        for (i, b) in bounds.iter().enumerate() {
            if i > 0 {
                out.push(text(" + "));
            }
            out.push(self.ty(b));
        }
        concat(out)
    }

    fn generic_params(&mut self, g: &[GenericParam]) -> Doc {
        if g.is_empty() {
            return nil();
        }
        let params =
            self.flat_list(g, |b, p| concat([text(&p.name.name), b.trait_list(&p.bounds)]));
        let last = g.last().map_or(0, |p| p.bounds.last().map_or(p.name.span.end, |b| b.span.end));
        concat([text("<"), params, self.close_after(last, '>'), text(">")])
    }

    fn fields(&mut self, fields: &[FieldDecl], multiline: bool, end: u32) -> Doc {
        let l = List { multiline, ..BRACES };
        self.list(fields, |f| f.span.start, |b, f| b.field_decl(f), end, l)
    }

    fn field_decl(&mut self, f: &FieldDecl) -> Doc {
        let vis = if f.vis.is_some() { text("pub ") } else { nil() };
        let ty = self.ty(&f.ty);
        let default = match &f.default {
            Some(d) => concat([text(" = "), self.expr(d)]),
            None => nil(),
        };
        concat([vis, text(format!("{}: ", f.name.name)), ty, default])
    }

    fn variant(&mut self, v: &Variant) -> Doc {
        let kind = match &v.kind {
            VariantKind::Unit => nil(),
            VariantKind::Tuple(tys) => {
                let list = self.flat_list(tys, |b, t| b.ty(t));
                let close = tys.last().map_or(nil(), |t| self.close_after(t.span.end, ')'));
                concat([text("("), list, close, text(")")])
            }
            VariantKind::Struct(fields) => {
                concat([text(" "), self.fields(fields, false, v.span.end)])
            }
        };
        concat([text(&v.name.name), kind])
    }

    fn fn_decl(&mut self, f: &FnDecl) -> Doc {
        let mut out = vec![text(format!("fn {}", f.name.name)), self.generic_params(&f.generics)];
        // Where the parameters' `)` is: the first after the last parameter, or the name.
        let from = f.params.last().map_or(f.name.span.end, |p| p.span().end) as usize;
        let end = self.text[from..].find(')').map_or(from, |i| from + i) as u32 + 1;
        out.push(self.list(&f.params, |p| p.span().start, |b, p| b.param(p), end, PARENS));
        if let Some(r) = &f.ret {
            out.push(text(match r.mode {
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
        let mode =
            |m: Mode| if m == Mode::Borrow { nil() } else { text(format!("{} ", m.keyword())) };
        match p {
            Param::SelfParam { mode: m, .. } => concat([mode(*m), text("self")]),
            Param::Named { name, mode: m, ty, default, .. } => {
                let ty = self.ty(ty);
                let default = match default {
                    Some(d) => concat([text(" = "), self.expr(d)]),
                    None => nil(),
                };
                concat([text(format!("{}: ", name.name)), mode(*m), ty, default])
            }
        }
    }

    // ---- types -------------------------------------------------------------------------------

    fn ty(&mut self, t: &TypeExpr) -> Doc {
        let pre = self.inline_comments(t.span.start);
        let d = match &t.kind {
            TypeExprKind::Path(p) => self.path(p, false),
            TypeExprKind::Array(elem, len) => {
                // Built in source order: the comments are taken in order.
                let elem = self.ty(elem);
                let len = match len {
                    Some(l) => concat([text("; "), self.expr(l)]),
                    None => nil(),
                };
                let close = self.inline_close(t.span.end - 1);
                concat([text("["), elem, len, close, text("]")])
            }
            TypeExprKind::Tuple(tys) => {
                let lone = if tys.len() == 1 { text(",") } else { nil() };
                let list = self.flat_list(tys, |b, t| b.ty(t));
                let close = self.inline_close(t.span.end - 1);
                concat([text("("), list, lone, close, text(")")])
            }
            TypeExprKind::Paren(inner) => {
                let inner = self.ty(inner);
                concat([text("("), inner, self.inline_close(t.span.end - 1), text(")")])
            }
            TypeExprKind::Fn(params, ret) => {
                let list = self.flat_list(params, |b, t| b.ty(t));
                let close = params.last().map_or(nil(), |p| self.close_after(p.span.end, ')'));
                let ret = match ret {
                    Some(r) => concat([text(" -> "), self.ty(r)]),
                    None => nil(),
                };
                concat([text("fn("), list, close, text(")"), ret])
            }
            TypeExprKind::Error => text("<error>"),
        };
        concat([pre, d])
    }

    /// A path; in expressions, generic arguments are written with `::<`.
    fn path(&mut self, p: &Path, expr: bool) -> Doc {
        let mut out = Vec::new();
        for (i, seg) in p.segments.iter().enumerate() {
            if i > 0 {
                out.push(text("::"));
            }
            out.push(text(&seg.ident.name));
            if let Some(g) = &seg.generics {
                if expr {
                    out.push(text("::"));
                }
                out.push(self.generic_args(g));
            }
        }
        concat(out)
    }

    fn generic_args(&mut self, g: &[TypeExpr]) -> Doc {
        let list = self.flat_list(g, |b, t| b.ty(t));
        let close = g.last().map_or(nil(), |t| self.close_after(t.span.end, '>'));
        concat([text("<"), list, close, text(">")])
    }

    // ---- blocks and statements ---------------------------------------------------------------

    /// A block. One that held one statement on one line stays on one line if it fits, unless
    /// it's a function body.
    fn block(&mut self, b: &Block, allow_inline: bool) -> Doc {
        if b.stmts.is_empty() && !self.has_comment_before(b.span.end) {
            return text("{}");
        }
        if allow_inline
            && b.stmts.len() == 1
            && !self.spans_lines(b.span)
            && !self.has_comment_before(b.span.end)
        {
            let saved = self.next;
            let s = self.stmt(&b.stmts[0]);
            if !s.has_hard_line() {
                return alone(concat([
                    text("{"),
                    indent(concat([Doc::Line, s])),
                    Doc::Line,
                    text("}"),
                ]));
            }
            self.next = saved;
        }
        let mut inner = Vec::new();
        let mut prev_end: Option<u32> = None;
        for s in &b.stmts {
            let blank = prev_end.is_some_and(|e| self.blank_between(e, s.span.start));
            inner.push(self.hard_break(s.span.start, blank));
            inner.push(self.stmt(s));
            prev_end = Some(s.span.end);
        }
        inner.push(self.hard_close(b.span.end));
        block(concat([text("{"), indent(concat(inner)), Doc::HardLine, text("}")]))
    }

    fn spans_lines(&self, s: Span) -> bool {
        self.text[s.start as usize..s.end as usize].contains('\n')
    }

    fn stmt(&mut self, s: &Stmt) -> Doc {
        match &s.kind {
            StmtKind::Bind { kind, pat, ty, init } => {
                let pat = self.pat(pat);
                let ty = match ty {
                    Some(t) => concat([text(": "), self.ty(t)]),
                    None => nil(),
                };
                concat([
                    text(format!("{} ", kind.keyword())),
                    pat,
                    ty,
                    text(" = "),
                    self.expr(init),
                ])
            }
            StmtKind::Assign { target, op, value } => {
                concat([self.expr(target), text(format!(" {} ", op.text())), self.expr(value)])
            }
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::While { cond, body } => {
                concat([text("while "), self.expr(cond), text(" "), self.block(body, true)])
            }
            StmtKind::Loop { body } => concat([text("loop "), self.block(body, true)]),
            StmtKind::For { mutable, pat, iter, body } => {
                let mut out = vec![text(if *mutable { "for mut " } else { "for " })];
                out.push(self.pat(pat));
                out.push(text(" in "));
                match iter {
                    ForIter::Range { start, end, inclusive } => {
                        out.push(self.expr(start));
                        out.push(text(if *inclusive { "..=" } else { ".." }));
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
        self.list(args, |a| a.span.start, |b, a| b.arg(a), end, l)
    }

    fn arg(&mut self, a: &Arg) -> Doc {
        let name = match &a.name {
            Some(n) => text(format!("{}: ", n.name)),
            None => nil(),
        };
        concat([name, self.expr(&a.value)])
    }

    fn expr(&mut self, e: &Expr) -> Doc {
        // A line break before a block's `{` could end the statement before it: comments there
        // go inside the block, before its first statement.
        let pre = if matches!(e.kind, ExprKind::Block(_)) {
            nil()
        } else {
            self.inline_comments(e.span.start)
        };
        let d = self.expr_kind(e);
        concat([pre, d])
    }

    fn expr_kind(&mut self, e: &Expr) -> Doc {
        match &e.kind {
            ExprKind::Lit(l) => text(&l.text),
            ExprKind::Path(p) => self.path(p, true),
            ExprKind::Unary(op, inner) => {
                let op = match op {
                    UnOp::Neg => "-",
                    UnOp::Not => "!",
                };
                concat([text(op), self.expr(inner)])
            }
            ExprKind::Binary(..) => self.binary(e),
            ExprKind::Take(inner) => concat([text("take "), self.expr(inner)]),
            ExprKind::MutArg(inner) => concat([text("mut "), self.expr(inner)]),
            ExprKind::Call { callee, args, multiline } => {
                concat([self.expr(callee), self.args(args, *multiline, e.span.end)])
            }
            ExprKind::MethodCall { .. } | ExprKind::Field { .. } => self.chain(e),
            ExprKind::Index { base, index } => concat([
                self.expr(base),
                text("["),
                self.expr(index),
                self.inline_close(e.span.end - 1),
                text("]"),
            ]),
            ExprKind::StructLit { path, fields, base, multiline } => {
                let path = self.path(path, true);
                if fields.is_empty() && base.is_none() {
                    return concat([path, text(" {}")]);
                }
                let mut parts: Vec<Part> = fields.iter().map(Part::Field).collect();
                parts.extend(base.as_deref().map(Part::Base));
                let start = |p: &Part| match p {
                    Part::Field(f) => f.span.start,
                    Part::Base(b) => b.span.start,
                };
                let item = |b: &mut Self, p: &Part| match p {
                    Part::Field(f) => b.field_init(f),
                    Part::Base(x) => concat([text(".."), b.expr(x)]),
                };
                let l = List { multiline: *multiline, ..BRACES };
                let body = self.list(&parts, start, item, e.span.end, l);
                concat([path, text(" "), body])
            }
            ExprKind::Tuple(items) => {
                let l = List { lone_comma: true, ..PARENS };
                self.list(items, |x| x.span.start, |b, x| b.expr(x), e.span.end, l)
            }
            ExprKind::Array(items) => {
                let multiline = items.first().is_some_and(|x| {
                    self.text[e.span.start as usize..x.span.start as usize].contains('\n')
                });
                let l = List { open: "[", close: "]", multiline, ..PARENS };
                self.list(items, |x| x.span.start, |b, x| b.expr(x), e.span.end, l)
            }
            ExprKind::ArrayRepeat { value, count } => concat([
                text("["),
                self.expr(value),
                text("; "),
                self.expr(count),
                self.inline_close(e.span.end - 1),
                text("]"),
            ]),
            ExprKind::Paren(inner) => {
                concat([text("("), self.expr(inner), self.inline_close(e.span.end - 1), text(")")])
            }
            ExprKind::Block(b) => self.block(b, true),
            ExprKind::If { cond, then, else_ } => {
                let mut out = vec![text("if "), self.expr(cond), text(" "), self.block(then, true)];
                if let Some(x) = else_ {
                    out.push(text(" else "));
                    out.push(self.expr(x));
                }
                concat(out)
            }
            ExprKind::Match { scrutinee, arms } => {
                let head = concat([text("match "), self.expr(scrutinee), text(" ")]);
                let mut inner = Vec::new();
                for arm in arms {
                    inner.push(self.hard_break(arm.span.start, false));
                    let mut a = Vec::new();
                    for (i, p) in arm.pats.iter().enumerate() {
                        if i > 0 {
                            a.push(text(" | "));
                        }
                        a.push(self.pat(p));
                    }
                    if let Some(g) = &arm.guard {
                        a.push(text(" if "));
                        a.push(self.expr(g));
                    }
                    a.push(text(" => "));
                    a.push(self.expr(&arm.body));
                    a.push(text(","));
                    inner.push(concat(a));
                }
                inner.push(self.hard_close(e.span.end));
                let body =
                    block(concat([text("{"), indent(concat(inner)), Doc::HardLine, text("}")]));
                concat([head, body])
            }
            ExprKind::Closure { params, ret, body } => {
                let params = if params.is_empty() {
                    text("||")
                } else {
                    let ps = self.flat_list(params, |b, p| {
                        let ty = match &p.ty {
                            Some(t) => concat([text(": "), b.ty(t)]),
                            None => nil(),
                        };
                        concat([text(&p.name.name), ty])
                    });
                    concat([text("|"), ps, text("|")])
                };
                let ret = match ret {
                    Some(r) => concat([text("-> "), self.ty(r), text(" ")]),
                    None => nil(),
                };
                concat([params, text(" "), ret, self.expr(body)])
            }
            ExprKind::Return(v) => match v {
                Some(v) => concat([text("return "), self.expr(v)]),
                None => text("return"),
            },
            ExprKind::Break => text("break"),
            ExprKind::Continue => text("continue"),
            ExprKind::Error => text("<error>"),
        }
    }

    /// A chain of operators of one precedence (`a + b - c`): on one line if it fits, and
    /// otherwise broken after each operator, the operands after the first indented.
    fn binary(&mut self, e: &Expr) -> Doc {
        let ExprKind::Binary(top, ..) = &e.kind else { return self.expr_kind(e) };
        // `a ** b ** c` is `a ** (b ** c)`, and comparisons don't chain: neither is flattened.
        let chains = |op: BinOp| {
            op != BinOp::Pow && precedence(op) != 2 && precedence(op) == precedence(*top)
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
            tail.push(text(format!(" {}", op.text())));
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
        for link in links {
            let into = if fit { &mut tail } else { &mut out };
            match &link.kind {
                ExprKind::Field { base: b, name } => {
                    // `1 .0` isn't `1.0`.
                    if matches!(name, FieldName::Index(..)) && matches!(b.kind, ExprKind::Lit(_)) {
                        into.push(text(" "));
                    }
                    into.push(text(format!(".{}", name.text())));
                }
                ExprKind::MethodCall {
                    name, generics, args, newline_before, multiline, ..
                } => {
                    // The break before the `.` first: the comments before it are its.
                    let brk = (*newline_before || fit)
                        .then(|| self.soft_break(name.span.start, Doc::SoftLine));
                    let mut d = vec![text(format!(".{}", name.name))];
                    if let Some(g) = generics {
                        d.push(text("::"));
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
            Some(v) => concat([text(format!("{}: ", f.name.name)), self.expr(v)]),
            None => text(&f.name.name),
        }
    }

    fn pat(&mut self, p: &Pat) -> Doc {
        let pre = self.inline_comments(p.span.start);
        let d = match &p.kind {
            PatKind::Wild => text("_"),
            PatKind::Ident(i) => text(&i.name),
            PatKind::Lit { neg, lit } => {
                text(if *neg { format!("-{}", lit.text) } else { lit.text.clone() })
            }
            PatKind::Path(path) => self.path(path, true),
            PatKind::TupleStruct(path, pats) => {
                let path = self.path(path, true);
                let list = self.flat_list(pats, |b, x| b.pat(x));
                let close = self.inline_close(p.span.end - 1);
                concat([path, text("("), list, close, text(")")])
            }
            PatKind::Struct { path, fields, rest } => {
                let path = self.path(path, true);
                if fields.is_empty() && !rest {
                    return concat([pre, path, text(" {}")]);
                }
                let mut parts = vec![self.flat_list(fields, |b, f| match &f.pat {
                    Some(x) => concat([text(format!("{}: ", f.name.name)), b.pat(x)]),
                    None => text(&f.name.name),
                })];
                if *rest {
                    parts.push(text(if fields.is_empty() { ".." } else { ", .." }));
                }
                let close = self.inline_close(p.span.end - 1);
                concat([path, text(" { "), concat(parts), close, text(" }")])
            }
            PatKind::Tuple(pats) => {
                let lone = if pats.len() == 1 { text(",") } else { nil() };
                let list = self.flat_list(pats, |b, x| b.pat(x));
                let close = self.inline_close(p.span.end - 1);
                concat([text("("), list, lone, close, text(")")])
            }
            PatKind::Error => text("<error>"),
        };
        concat([pre, d])
    }
}

/// A part of a struct literal.
enum Part<'e> {
    Field(&'e FieldInit),
    Base(&'e Expr),
}

/// A `use` tree starting at `column`. A group goes on one line if it fits in the width;
/// otherwise its items are packed into lines of up to the width.
fn use_tree(u: &UseTree, column: usize) -> Doc {
    let path: Vec<&str> = u.path.iter().map(|s| s.name.as_str()).collect();
    let path = path.join("::");
    let UseKind::Group(trees) = &u.kind else { return text(use_text(u)) };
    let items: Vec<String> = trees.iter().map(use_text).collect();
    let inline = items.join(", ");
    if column + path.len() + 4 + inline.len() <= MAX_WIDTH {
        return text(format!("{path}::{{{inline}}}"));
    }
    let start = INDENT_COLS;
    let mut lines = vec![Doc::HardLine];
    let mut line = start;
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
        lines.push(text(format!("{item},")));
        line += piece;
    }
    broken(concat([text(format!("{path}::{{")), indent(concat(lines)), Doc::HardLine, text("}")]))
}

const INDENT_COLS: usize = 4;

/// A `use` tree on one line.
fn use_text(u: &UseTree) -> String {
    let path: Vec<&str> = u.path.iter().map(|s| s.name.as_str()).collect();
    let path = path.join("::");
    match &u.kind {
        UseKind::Simple(None) => path,
        UseKind::Simple(Some(r)) => format!("{path} as {}", r.name),
        UseKind::Group(trees) => {
            let items: Vec<String> = trees.iter().map(use_text).collect();
            format!("{path}::{{{}}}", items.join(", "))
        }
    }
}

/// How tightly an operator binds: a chain of one strength is laid out as one.
fn precedence(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 0,
        BinOp::And => 1,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 2,
        BinOp::BitOr => 3,
        BinOp::BitXor => 4,
        BinOp::BitAnd => 5,
        BinOp::Shl | BinOp::Shr => 6,
        BinOp::Add | BinOp::Sub => 7,
        BinOp::Mul | BinOp::Div | BinOp::Rem => 8,
        BinOp::Pow => 9,
    }
}
