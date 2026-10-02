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
        next_id: 0,
        syntax_errors: 0,
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
    next_id: u32,
    syntax_errors: u32,
}

/// After this many syntax errors in one file, later ones are dropped: they're usually noise
/// from the first.
const MAX_SYNTAX_ERRORS: u32 = 20;

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

    fn id(&mut self) -> NodeId {
        self.next_id += 1;
        NodeId(self.next_id - 1)
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
            // A NEWLINE token is empty; point at the end of the line instead.
            d.primary.span = t.span;
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

    /// Skips to the next separator or closing `}` at this nesting depth (recovery).
    fn recover_to_sep(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.kind() {
                T::Eof => return,
                T::Newline | T::Semi if depth == 0 => return,
                T::RBrace if depth == 0 => return,
                T::LParen | T::LBracket | T::LBrace => depth += 1,
                T::RParen | T::RBracket | T::RBrace => depth -= 1,
                _ => {}
            }
            self.bump();
            if depth < 0 {
                depth = 0;
            }
        }
    }

    // ---- file and items --------------------------------------------------------------------

    /// file ::= sep* (item (sep+ item)* sep*)? EOF
    fn parse_file(&mut self) -> File {
        let start = self.span();
        let mut items = Vec::new();
        self.skip_seps();
        while !self.at(T::Eof) {
            let before = self.pos;
            match self.parse_item() {
                Ok(item) => {
                    items.push(item);
                    if !self.at_sep() && !self.at(T::Eof) {
                        self.missing_sep("an item");
                        self.recover_to_item();
                    }
                }
                Err(Failed) => self.recover_to_item(),
            }
            self.skip_seps();
            if self.pos == before && !self.at(T::Eof) {
                self.bump();
            }
        }
        File { items, span: start.to(self.span()), node_count: self.next_id }
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
        let mut params = Vec::new();
        while !self.at(T::RParen) {
            params.push(self.parse_param()?);
            if !self.eat(T::Comma) {
                break;
            }
        }
        self.expect(T::RParen, "`,` or `)`")?;
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
            return Err(self.expected("`self`"));
        }
        let name = self.ident("a parameter name")?;
        if !self.at(T::Colon) {
            let t = self.tok();
            self.error(
                Diagnostic::new(
                    codes::E0109,
                    name.span,
                    format!("the parameter `{}` needs a type", name.name),
                )
                .with_help(format!("write `{}: Type`", name.name)),
            );
            let _ = t;
            return Err(Failed);
        }
        self.bump();
        let mode = self.parse_mode();
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
        let mut fields = Vec::new();
        while !matches!(self.kind(), T::RBrace | T::Newline) {
            let start = self.span();
            let vis = if self.at(T::Pub) { Some(self.bump().span) } else { None };
            let name = self.ident("a field name")?;
            self.expect(T::Colon, "`:` and the field's type")?;
            let ty = self.parse_type()?;
            let default = if self.eat(T::Eq) { Some(self.parse_expr()?) } else { None };
            fields.push(FieldDecl { vis, name, ty, default, span: start.to(self.prev_span()) });
            if !self.eat(T::Comma) {
                break;
            }
        }
        self.close_list("`,` or `}`")?;
        Ok((fields, multiline))
    }

    /// NEWLINE? "}" at the end of a comma-separated list in braces.
    fn close_list(&mut self, what: &str) -> PResult<Span> {
        if self.at(T::Newline) && self.nth(1) == T::RBrace {
            self.bump();
        }
        if self.at(T::Newline) {
            // A line break between two entries: the comma is missing.
            let at = self.prev_span().shrink_to_end();
            let t = self.tok();
            self.error(
                Diagnostic::new(
                    codes::E0100,
                    t.span,
                    format!("expected {what}, found a line break"),
                )
                .with_help("separate the entries with commas")
                .with_fix("add a comma", at, ","),
            );
            return Err(Failed);
        }
        Ok(self.expect(T::RBrace, what)?.span)
    }

    /// enum_item ::= "enum" IDENT generic_params? (":" bounds)? "{" (variant ("," variant)* ","?)? NEWLINE? "}"
    fn parse_enum(&mut self) -> PResult<EnumDecl> {
        self.expect(T::Enum, "`enum`")?;
        let name = self.ident("an enum name")?;
        let generics = if self.at(T::Lt) { self.parse_generic_params()? } else { Vec::new() };
        let traits = if self.eat(T::Colon) { self.parse_bounds()? } else { Vec::new() };
        self.expect(T::LBrace, "`{`")?;
        let multiline = self.tok().line_break_before && !self.at(T::RBrace);
        let mut variants = Vec::new();
        while !matches!(self.kind(), T::RBrace | T::Newline) {
            let start = self.span();
            let vname = self.ident("a variant name")?;
            let kind = if self.at(T::LParen) {
                self.bump();
                let mut tys = Vec::new();
                while !self.at(T::RParen) {
                    tys.push(self.parse_type()?);
                    if !self.eat(T::Comma) {
                        break;
                    }
                }
                self.expect(T::RParen, "`,` or `)`")?;
                VariantKind::Tuple(tys)
            } else if self.at(T::LBrace) {
                VariantKind::Struct(self.parse_field_block()?.0)
            } else {
                VariantKind::Unit
            };
            variants.push(Variant { name: vname, kind, span: start.to(self.prev_span()) });
            if !self.eat(T::Comma) {
                break;
            }
        }
        self.close_list("`,` or `}`")?;
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
        let value = self.parse_expr()?;
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
                let mut trees = Vec::new();
                while !matches!(self.kind(), T::RBrace | T::Newline) {
                    trees.push(self.parse_use_tree()?);
                    if !self.eat(T::Comma) {
                        break;
                    }
                }
                self.close_list("`,` or `}`")?;
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
        let start = self.span();
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
