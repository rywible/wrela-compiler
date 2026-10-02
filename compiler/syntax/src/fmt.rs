//! The formatter: one canonical layout for any parsed file (D-038: "the formatter normalizes").
//!
//! - Four-space indents; one statement per line; `;` separators become line breaks.
//! - Comments are kept. A comment is printed before the first item, member, statement, field,
//!   variant, arm or argument that starts after it, or after the previous one on the same line
//!   when it trailed code. A comment inside an expression moves to the end of its statement.
//! - The author's choices that carry meaning are kept: single blank lines between statements and
//!   between `const`s, leading-dot chains, and whether a call's arguments, a struct literal, a
//!   struct or an enum spans several lines.
//! - It never changes what a file means: parsing its output gives the same AST (tests check this
//!   and that formatting is idempotent).

use crate::ast::*;
use crate::parser::Parsed;
use crate::token::Comment;
use wrela_diag::Span;

/// Formats a file that parsed without errors.
pub fn format(parsed: &Parsed, text: &str) -> String {
    let mut p =
        Printer { out: String::new(), indent: 0, comments: &parsed.comments, next: 0, text };
    p.file(&parsed.file);
    p.out
}

struct Printer<'a> {
    out: String,
    indent: usize,
    comments: &'a [Comment],
    next: usize,
    text: &'a str,
}

/// The column a `use` list wraps at.
const MAX_WIDTH: usize = 100;
const INDENT: &str = "    ";

impl<'a> Printer<'a> {
    fn w(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn newline(&mut self) {
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        self.out.push('\n');
        for _ in 0..self.indent {
            self.out.push_str(INDENT);
        }
    }

    fn at_line_start(&self) -> bool {
        self.out.is_empty() || self.out.trim_end_matches(' ').ends_with('\n')
    }

    /// Prints, at the end of the current line, the comments that trailed code and start before
    /// `pos`.
    fn flush_trailing(&mut self, pos: u32) {
        while let Some(c) = self.comments.get(self.next) {
            if c.span.start >= pos || c.own_line || self.at_line_start() {
                break;
            }
            self.next += 1;
            self.w(" ");
            self.w(&c.text);
        }
    }

    /// At the start of a line: prints every pending comment that starts before `pos`, each on
    /// its own line.
    fn flush_comments(&mut self, pos: u32) {
        while let Some(c) = self.comments.get(self.next) {
            if c.span.start >= pos {
                break;
            }
            self.next += 1;
            self.w(&c.text);
            self.newline();
            // A blank line after a comment (a file's header, a section's divider) stays.
            let next = self.comments.get(self.next).map_or(pos, |n| n.span.start.min(pos));
            let next = next.min(self.text.len() as u32);
            let only_space = self.text[c.span.end as usize..next as usize].trim().is_empty();
            if only_space && self.blank_between(c.span.end, next) {
                self.newline();
            }
        }
    }

    /// Before an element of a multi-line sequence: trailing comments, a line break, and, when
    /// `blank`, an empty line, then the comments that come before the element.
    fn before_element(&mut self, start: u32, blank: bool) {
        self.flush_trailing(start);
        self.newline();
        if blank {
            self.newline();
        }
        self.flush_comments(start);
    }

    /// Before the `}` (or `)`, `]`) that closes a multi-line sequence at `end`: the remaining
    /// comments inside it, then the closer's line. Leaves the indent one level out.
    fn before_close(&mut self, end: u32) {
        self.flush_trailing(end);
        while let Some(c) = self.comments.get(self.next) {
            if c.span.start >= end {
                break;
            }
            self.next += 1;
            self.newline();
            self.w(&c.text);
        }
        self.indent -= 1;
        self.newline();
    }

    /// Whether the source has a blank line between two positions.
    fn blank_between(&self, a: u32, b: u32) -> bool {
        if a >= b {
            return false;
        }
        let parts: Vec<&str> = self.text[a as usize..b as usize].split('\n').collect();
        parts.len() > 2 && parts[1..parts.len() - 1].iter().any(|l| l.trim().is_empty())
    }

    // ---- items -----------------------------------------------------------------------------

    fn file(&mut self, f: &File) {
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
                self.before_element(item.span.start, blank);
            } else {
                self.flush_comments(item.span.start);
            }
            self.item(item);
            prev = Some(item);
        }
        self.flush_trailing(u32::MAX);
        if !self.at_line_start() {
            self.newline();
        }
        self.flush_comments(u32::MAX);
        while self.out.ends_with('\n') || self.out.ends_with(' ') {
            self.out.pop();
        }
        if !self.out.is_empty() {
            self.out.push('\n');
        }
    }

    fn attrs(&mut self, attrs: &[Attribute]) {
        for a in attrs {
            self.w("@");
            self.w(&a.name.name);
            if let Some(args) = &a.args {
                self.args(args, false, a.span.end);
            }
            // A comment after the attribute, on its line, stays there.
            let end = a.span.end as usize;
            let eol = self.text[end..].find('\n').map_or(self.text.len(), |i| end + i);
            self.flush_trailing(eol as u32);
            self.newline();
        }
    }

    fn item(&mut self, item: &Item) {
        self.attrs(&item.attrs);
        if item.vis.is_some() {
            self.w("pub ");
        }
        match &item.kind {
            ItemKind::Fn(f) => self.fn_decl(f),
            ItemKind::Struct(s) => {
                self.w("struct ");
                self.w(&s.name.name);
                self.generic_params(&s.generics);
                self.trait_list(&s.traits);
                self.w(" ");
                self.fields(&s.fields, s.multiline, item.span.end);
            }
            ItemKind::Enum(e) => {
                self.w("enum ");
                self.w(&e.name.name);
                self.generic_params(&e.generics);
                self.trait_list(&e.traits);
                if e.variants.is_empty() {
                    self.w(" {}");
                    return;
                }
                self.w(" {");
                if e.multiline {
                    self.indent += 1;
                    for v in &e.variants {
                        self.before_element(v.span.start, false);
                        self.variant(v);
                        self.w(",");
                    }
                    self.before_close(item.span.end);
                } else {
                    self.w(" ");
                    for (i, v) in e.variants.iter().enumerate() {
                        if i > 0 {
                            self.w(", ");
                        }
                        self.variant(v);
                    }
                    self.w(" ");
                }
                self.w("}");
            }
            ItemKind::Trait(t) => {
                self.w("trait ");
                self.w(&t.name.name);
                self.generic_params(&t.generics);
                self.trait_list(&t.supertraits);
                self.w(" {");
                self.indent += 1;
                let mut prev_end = None;
                for m in &t.members {
                    let blank = prev_end.is_some_and(|e| self.blank_between(e, m.span.start));
                    self.before_element(m.span.start, blank);
                    self.attrs(&m.attrs);
                    match &m.kind {
                        TraitMemberKind::Fn(f) => self.fn_decl(f),
                        TraitMemberKind::Type { name, bounds } => {
                            self.w("type ");
                            self.w(&name.name);
                            if !bounds.is_empty() {
                                self.w(": ");
                                self.bounds(bounds);
                            }
                        }
                    }
                    prev_end = Some(m.span.end);
                }
                self.close_braces(item.span.end, prev_end.is_some());
            }
            ItemKind::Impl(i) => {
                self.w("impl");
                self.generic_params(&i.generics);
                self.w(" ");
                if let Some(t) = &i.trait_ {
                    self.ty(t);
                    self.w(" for ");
                }
                self.ty(&i.self_ty);
                self.w(" {");
                self.indent += 1;
                let mut prev_end = None;
                for m in &i.members {
                    let blank = prev_end.is_some_and(|e| self.blank_between(e, m.span.start));
                    self.before_element(m.span.start, blank);
                    self.attrs(&m.attrs);
                    if m.vis.is_some() {
                        self.w("pub ");
                    }
                    match &m.kind {
                        ImplMemberKind::Fn(f) => self.fn_decl(f),
                        ImplMemberKind::Type { name, ty } => {
                            self.w("type ");
                            self.w(&name.name);
                            self.w(" = ");
                            self.ty(ty);
                        }
                    }
                    prev_end = Some(m.span.end);
                }
                self.close_braces(item.span.end, prev_end.is_some());
            }
            ItemKind::Const(c) => {
                self.w("const ");
                self.w(&c.name.name);
                if let Some(t) = &c.ty {
                    self.w(": ");
                    self.ty(t);
                }
                self.w(" = ");
                self.expr(&c.value);
            }
            ItemKind::Use(u) => {
                self.w("use ");
                self.use_tree(u);
            }
        }
    }

    fn close_braces(&mut self, end: u32, any: bool) {
        let pending = self.has_comment_before(end);
        if any || pending {
            self.before_close(end);
        } else {
            self.indent -= 1;
        }
        self.w("}");
    }

    fn trait_list(&mut self, traits: &[TypeExpr]) {
        if !traits.is_empty() {
            self.w(": ");
            self.bounds(traits);
        }
    }

    fn bounds(&mut self, bounds: &[TypeExpr]) {
        for (i, b) in bounds.iter().enumerate() {
            if i > 0 {
                self.w(" + ");
            }
            self.ty(b);
        }
    }

    fn generic_params(&mut self, g: &[GenericParam]) {
        if g.is_empty() {
            return;
        }
        self.w("<");
        for (i, p) in g.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            self.w(&p.name.name);
            if !p.bounds.is_empty() {
                self.w(": ");
                self.bounds(&p.bounds);
            }
        }
        self.w(">");
    }

    fn fields(&mut self, fields: &[FieldDecl], multiline: bool, end: u32) {
        if fields.is_empty() {
            self.w("{}");
            return;
        }
        self.w("{");
        if multiline {
            self.indent += 1;
            for f in fields {
                self.before_element(f.span.start, false);
                self.field_decl(f);
                self.w(",");
            }
            self.before_close(end);
        } else {
            self.w(" ");
            for (i, f) in fields.iter().enumerate() {
                if i > 0 {
                    self.w(", ");
                }
                self.field_decl(f);
            }
            self.w(" ");
        }
        self.w("}");
    }

    fn field_decl(&mut self, f: &FieldDecl) {
        if f.vis.is_some() {
            self.w("pub ");
        }
        self.w(&f.name.name);
        self.w(": ");
        self.ty(&f.ty);
        if let Some(d) = &f.default {
            self.w(" = ");
            self.expr(d);
        }
    }

    fn variant(&mut self, v: &Variant) {
        self.w(&v.name.name);
        match &v.kind {
            VariantKind::Unit => {}
            VariantKind::Tuple(tys) => {
                self.w("(");
                for (i, t) in tys.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    self.ty(t);
                }
                self.w(")");
            }
            VariantKind::Struct(fields) => {
                self.w(" ");
                self.fields(fields, false, v.span.end);
            }
        }
    }

    fn use_tree(&mut self, u: &UseTree) {
        for (i, seg) in u.path.iter().enumerate() {
            if i > 0 {
                self.w("::");
            }
            self.w(&seg.name);
        }
        match &u.kind {
            UseKind::Simple(None) => {}
            UseKind::Simple(Some(r)) => {
                self.w(" as ");
                self.w(&r.name);
            }
            UseKind::Group(trees) => {
                // On one line if it fits in 100 columns; otherwise one item per slot, packed
                // into lines of up to 100.
                let items: Vec<String> = trees
                    .iter()
                    .map(|t| {
                        let mut p = Printer {
                            out: String::new(),
                            indent: 0,
                            comments: &[],
                            next: 0,
                            text: self.text,
                        };
                        p.use_tree(t);
                        p.out
                    })
                    .collect();
                let column = self.out.len() - self.out.rfind('\n').map_or(0, |i| i + 1);
                let inline = items.join(", ");
                if column + 3 + inline.len() < MAX_WIDTH || items.iter().any(|i| i.contains('\n')) {
                    self.w("::{");
                    self.w(&inline);
                    self.w("}");
                    return;
                }
                self.w("::{");
                self.indent += 1;
                self.newline();
                let indent = INDENT.len() * self.indent;
                let mut line = indent;
                for (i, item) in items.iter().enumerate() {
                    let piece = item.len() + 1;
                    if i > 0 {
                        if line + 1 + piece > MAX_WIDTH {
                            self.newline();
                            line = indent;
                        } else {
                            self.w(" ");
                            line += 1;
                        }
                    }
                    self.w(item);
                    self.w(",");
                    line += piece;
                }
                self.indent -= 1;
                self.newline();
                self.w("}");
            }
        }
    }

    fn fn_decl(&mut self, f: &FnDecl) {
        self.w("fn ");
        self.w(&f.name.name);
        self.generic_params(&f.generics);
        self.w("(");
        for (i, p) in f.params.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            match p {
                Param::SelfParam { mode, .. } => {
                    if *mode != Mode::Borrow {
                        self.w(mode.keyword());
                        self.w(" ");
                    }
                    self.w("self");
                }
                Param::Named { name, mode, ty, default, .. } => {
                    self.w(&name.name);
                    self.w(": ");
                    if *mode != Mode::Borrow {
                        self.w(mode.keyword());
                        self.w(" ");
                    }
                    self.ty(ty);
                    if let Some(d) = default {
                        self.w(" = ");
                        self.expr(d);
                    }
                }
            }
        }
        self.w(")");
        if let Some(r) = &f.ret {
            self.w(" -> ");
            match r.mode {
                RetMode::Owned => {}
                RetMode::Borrow => self.w("borrow "),
                RetMode::Mut => self.w("mut "),
            }
            self.ty(&r.ty);
        }
        if let Some(b) = &f.body {
            self.w(" ");
            self.block(b, false);
        }
    }

    // ---- types -----------------------------------------------------------------------------

    fn ty(&mut self, t: &TypeExpr) {
        match &t.kind {
            TypeExprKind::Path(p) => self.path(p, false),
            TypeExprKind::Array(elem, len) => {
                self.w("[");
                self.ty(elem);
                if let Some(l) = len {
                    self.w("; ");
                    self.expr(l);
                }
                self.w("]");
            }
            TypeExprKind::Tuple(tys) => {
                self.w("(");
                for (i, t) in tys.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    self.ty(t);
                }
                if tys.len() == 1 {
                    self.w(",");
                }
                self.w(")");
            }
            TypeExprKind::Paren(inner) => {
                self.w("(");
                self.ty(inner);
                self.w(")");
            }
            TypeExprKind::Fn(params, ret) => {
                self.w("fn(");
                for (i, t) in params.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    self.ty(t);
                }
                self.w(")");
                if let Some(r) = ret {
                    self.w(" -> ");
                    self.ty(r);
                }
            }
        }
    }

    /// A path; in expressions, generic arguments are written with `::<`.
    fn path(&mut self, p: &Path, expr: bool) {
        for (i, seg) in p.segments.iter().enumerate() {
            if i > 0 {
                self.w("::");
            }
            self.w(&seg.ident.name);
            if let Some(g) = &seg.generics {
                if expr {
                    self.w("::");
                }
                self.generic_args(g);
            }
        }
    }

    fn generic_args(&mut self, g: &[TypeExpr]) {
        self.w("<");
        for (i, t) in g.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            self.ty(t);
        }
        self.w(">");
    }

    // ---- blocks and statements -------------------------------------------------------------

    /// Prints a block. A block that held one short statement on one line stays on one line,
    /// unless it's a function body.
    fn block(&mut self, b: &Block, allow_inline: bool) {
        if b.stmts.is_empty() && !self.has_comment_before(b.span.end) {
            self.w("{}");
            return;
        }
        if allow_inline
            && b.stmts.len() == 1
            && !self.spans_lines(b.span)
            && !self.has_comment_before(b.span.end)
        {
            let mut inner =
                Printer { out: String::new(), indent: 0, comments: &[], next: 0, text: self.text };
            inner.stmt(&b.stmts[0]);
            if !inner.out.contains('\n') {
                self.w("{ ");
                self.w(&inner.out);
                self.w(" }");
                return;
            }
        }
        self.w("{");
        self.indent += 1;
        let mut prev_end: Option<u32> = None;
        for s in &b.stmts {
            let blank = prev_end.is_some_and(|e| self.blank_between(e, s.span.start));
            self.before_element(s.span.start, blank);
            self.stmt(s);
            prev_end = Some(s.span.end);
        }
        self.before_close(b.span.end);
        self.w("}");
    }

    fn has_comment_before(&self, pos: u32) -> bool {
        self.comments.get(self.next).is_some_and(|c| c.span.start < pos)
    }

    fn spans_lines(&self, s: Span) -> bool {
        self.text[s.start as usize..s.end as usize].contains('\n')
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Bind { kind, pat, ty, init } => {
                self.w(kind.keyword());
                self.w(" ");
                self.pat(pat);
                if let Some(t) = ty {
                    self.w(": ");
                    self.ty(t);
                }
                self.w(" = ");
                self.expr(init);
            }
            StmtKind::Assign { target, op, value } => {
                self.expr(target);
                self.w(" ");
                self.w(op.text());
                self.w(" ");
                self.expr(value);
            }
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::While { cond, body } => {
                self.w("while ");
                self.expr(cond);
                self.w(" ");
                self.block(body, true);
            }
            StmtKind::Loop { body } => {
                self.w("loop ");
                self.block(body, true);
            }
            StmtKind::For { mutable, pat, iter, body } => {
                self.w("for ");
                if *mutable {
                    self.w("mut ");
                }
                self.pat(pat);
                self.w(" in ");
                match iter {
                    ForIter::Range { start, end, inclusive } => {
                        self.expr(start);
                        self.w(if *inclusive { "..=" } else { ".." });
                        self.expr(end);
                    }
                    ForIter::Expr(e) => self.expr(e),
                }
                self.w(" ");
                self.block(body, true);
            }
        }
    }

    // ---- expressions -----------------------------------------------------------------------

    /// A call's arguments; `end` is where the call ends (just past its `)`).
    fn args(&mut self, args: &[Arg], multiline: bool, end: u32) {
        if args.is_empty() {
            self.w("()");
            return;
        }
        self.w("(");
        if multiline {
            self.indent += 1;
            for a in args {
                self.before_element(a.span.start, false);
                self.arg(a);
                self.w(",");
            }
            // Comments up to the `)` belong inside the parentheses.
            self.before_close(end.saturating_sub(1));
        } else {
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    self.w(", ");
                }
                self.arg(a);
            }
        }
        self.w(")");
    }

    fn arg(&mut self, a: &Arg) {
        if let Some(n) = &a.name {
            self.w(&n.name);
            self.w(": ");
        }
        self.expr(&a.value);
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Lit(l) => self.w(&l.text),
            ExprKind::Path(p) => self.path(p, true),
            ExprKind::Unary(op, inner) => {
                self.w(match op {
                    UnOp::Neg => "-",
                    UnOp::Not => "!",
                });
                self.expr(inner);
            }
            ExprKind::Binary(op, a, b) => {
                self.expr(a);
                self.w(" ");
                self.w(op.text());
                self.w(" ");
                self.expr(b);
            }
            ExprKind::Take(inner) => {
                self.w("take ");
                self.expr(inner);
            }
            ExprKind::MutArg(inner) => {
                self.w("mut ");
                self.expr(inner);
            }
            ExprKind::Call { callee, args, multiline } => {
                self.expr(callee);
                self.args(args, *multiline, e.span.end);
            }
            ExprKind::MethodCall { receiver, name, generics, args, newline_before, multiline } => {
                self.expr(receiver);
                if *newline_before {
                    self.indent += 1;
                    self.before_element(name.span.start, false);
                }
                self.w(".");
                self.w(&name.name);
                if let Some(g) = generics {
                    self.w("::");
                    self.generic_args(g);
                }
                self.args(args, *multiline, e.span.end);
                if *newline_before {
                    self.indent -= 1;
                }
            }
            ExprKind::Field { base, name } => {
                self.expr(base);
                // `1 .0` isn't `1.0`.
                if matches!(name, FieldName::Index(..)) && matches!(base.kind, ExprKind::Lit(_)) {
                    self.w(" ");
                }
                self.w(".");
                self.w(&name.text());
            }
            ExprKind::Index { base, index } => {
                self.expr(base);
                self.w("[");
                self.expr(index);
                self.w("]");
            }
            ExprKind::StructLit { path, fields, base, multiline } => {
                self.path(path, true);
                if fields.is_empty() && base.is_none() {
                    self.w(" {}");
                    return;
                }
                self.w(" {");
                if *multiline {
                    self.indent += 1;
                    for f in fields {
                        self.before_element(f.span.start, false);
                        self.field_init(f);
                        self.w(",");
                    }
                    if let Some(b) = base {
                        self.before_element(b.span.start, false);
                        self.w("..");
                        self.expr(b);
                        self.w(",");
                    }
                    self.before_close(e.span.end);
                } else {
                    self.w(" ");
                    for (i, f) in fields.iter().enumerate() {
                        if i > 0 {
                            self.w(", ");
                        }
                        self.field_init(f);
                    }
                    if let Some(b) = base {
                        if !fields.is_empty() {
                            self.w(", ");
                        }
                        self.w("..");
                        self.expr(b);
                    }
                    self.w(" ");
                }
                self.w("}");
            }
            ExprKind::Tuple(items) => {
                self.w("(");
                for (i, x) in items.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    self.expr(x);
                }
                if items.len() == 1 {
                    self.w(",");
                }
                self.w(")");
            }
            ExprKind::Array(items) => {
                let multiline = items.first().is_some_and(|x| {
                    self.text[e.span.start as usize..x.span.start as usize].contains('\n')
                });
                self.w("[");
                if multiline {
                    self.indent += 1;
                    for x in items {
                        self.before_element(x.span.start, false);
                        self.expr(x);
                        self.w(",");
                    }
                    self.before_close(e.span.end);
                } else {
                    for (i, x) in items.iter().enumerate() {
                        if i > 0 {
                            self.w(", ");
                        }
                        self.expr(x);
                    }
                }
                self.w("]");
            }
            ExprKind::ArrayRepeat { value, count } => {
                self.w("[");
                self.expr(value);
                self.w("; ");
                self.expr(count);
                self.w("]");
            }
            ExprKind::Paren(inner) => {
                self.w("(");
                self.expr(inner);
                self.w(")");
            }
            ExprKind::Block(b) => self.block(b, true),
            ExprKind::If { cond, then, else_ } => {
                self.w("if ");
                self.expr(cond);
                self.w(" ");
                self.block(then, true);
                if let Some(e) = else_ {
                    self.w(" else ");
                    self.expr(e);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.w("match ");
                self.expr(scrutinee);
                self.w(" {");
                self.indent += 1;
                for arm in arms {
                    self.before_element(arm.span.start, false);
                    for (i, p) in arm.pats.iter().enumerate() {
                        if i > 0 {
                            self.w(" | ");
                        }
                        self.pat(p);
                    }
                    if let Some(g) = &arm.guard {
                        self.w(" if ");
                        self.expr(g);
                    }
                    self.w(" => ");
                    self.expr(&arm.body);
                    self.w(",");
                }
                self.before_close(e.span.end);
                self.w("}");
            }
            ExprKind::Closure { params, ret, body } => {
                if params.is_empty() {
                    self.w("||");
                } else {
                    self.w("|");
                    for (i, p) in params.iter().enumerate() {
                        if i > 0 {
                            self.w(", ");
                        }
                        self.w(&p.name.name);
                        if let Some(t) = &p.ty {
                            self.w(": ");
                            self.ty(t);
                        }
                    }
                    self.w("|");
                }
                self.w(" ");
                if let Some(r) = ret {
                    self.w("-> ");
                    self.ty(r);
                    self.w(" ");
                }
                self.expr(body);
            }
            ExprKind::Return(v) => {
                self.w("return");
                if let Some(v) = v {
                    self.w(" ");
                    self.expr(v);
                }
            }
            ExprKind::Break => self.w("break"),
            ExprKind::Continue => self.w("continue"),
            ExprKind::Error => self.w("<error>"),
        }
    }

    fn field_init(&mut self, f: &FieldInit) {
        self.w(&f.name.name);
        if let Some(v) = &f.value {
            self.w(": ");
            self.expr(v);
        }
    }

    fn pat(&mut self, p: &Pat) {
        match &p.kind {
            PatKind::Wild => self.w("_"),
            PatKind::Ident(i) => self.w(&i.name),
            PatKind::Lit { neg, lit } => {
                if *neg {
                    self.w("-");
                }
                self.w(&lit.text);
            }
            PatKind::Path(path) => self.path(path, true),
            PatKind::TupleStruct(path, pats) => {
                self.path(path, true);
                self.w("(");
                for (i, x) in pats.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    self.pat(x);
                }
                self.w(")");
            }
            PatKind::Struct { path, fields, rest } => {
                self.path(path, true);
                if fields.is_empty() && !rest {
                    self.w(" {}");
                    return;
                }
                self.w(" { ");
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    self.w(&f.name.name);
                    if let Some(x) = &f.pat {
                        self.w(": ");
                        self.pat(x);
                    }
                }
                if *rest {
                    if !fields.is_empty() {
                        self.w(", ");
                    }
                    self.w("..");
                }
                self.w(" }");
            }
            PatKind::Tuple(pats) => {
                self.w("(");
                for (i, x) in pats.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    self.pat(x);
                }
                if pats.len() == 1 {
                    self.w(",");
                }
                self.w(")");
            }
        }
    }
}
