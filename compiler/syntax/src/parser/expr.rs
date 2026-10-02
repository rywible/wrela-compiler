//! Blocks, statements, expressions and patterns.

use super::{Failed, PResult, Parser, describe};
use crate::ast::*;
use crate::token::TokenKind as T;
use wrela_diag::{Diagnostic, Span, codes};

/// Whether a struct literal may appear here (the `_ns` rules of the grammar exclude it, and
/// closures, from the heads of `if`, `while`, `for` and `match`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ctx {
    Full,
    NoStruct,
}

impl<'a> Parser<'a> {
    fn expr_node(&mut self, kind: ExprKind, span: Span) -> Expr {
        Expr { id: self.id(), kind, span }
    }

    // ---- blocks and statements -------------------------------------------------------------

    /// block ::= "{" sep* (stmt (sep+ stmt)* sep*)? "}"
    pub(crate) fn parse_block(&mut self) -> PResult<Block> {
        let open = self.expect(T::LBrace, "`{`")?.span;
        let mut stmts = Vec::new();
        self.skip_seps();
        while !self.at(T::RBrace) && !self.at(T::Eof) {
            let before = self.pos;
            match self.parse_stmt() {
                Ok(s) => {
                    stmts.push(s);
                    if !self.at_sep() && !self.at(T::RBrace) {
                        self.stmt_sep_error();
                        self.recover_to_sep();
                    }
                }
                Err(Failed) => self.recover_to_sep(),
            }
            self.skip_seps();
            if self.pos == before && !self.at(T::RBrace) && !self.at(T::Eof) {
                self.bump();
            }
        }
        if self.at(T::Eof) {
            self.error(
                Diagnostic::new(codes::E0101, open, "this `{` is never closed")
                    .with_secondary(self.span(), "the file ends here"),
            );
            return Err(Failed);
        }
        let close = self.bump().span;
        Ok(Block { stmts, span: open.to(close) })
    }

    /// Two statements on one line without `;`, or a statement followed by junk.
    fn stmt_sep_error(&mut self) {
        let t = self.tok();
        if t.kind == T::Else {
            // `}` then a line break then `else` is caught in parse_if; here `else` follows an
            // expression on the same line that isn't an `if`.
            self.error(Diagnostic::new(codes::E0100, t.span, "`else` without an `if`"));
            return;
        }
        if t.kind.is_binary_operator() || t.kind == T::Eq {
            self.error(Diagnostic::new(
                codes::E0100,
                t.span,
                format!("expected a line break or `;`, found {}", describe(t.kind)),
            ));
            return;
        }
        self.missing_sep("a statement");
    }

    /// stmt ::= bind_stmt | assign_stmt | while_stmt | loop_stmt | for_stmt | expr
    fn parse_stmt(&mut self) -> PResult<Stmt> {
        let start = self.span();
        let kind = match self.kind() {
            T::Let => {
                self.bump();
                let pat = self.parse_pattern()?;
                let ty = if self.eat(T::Colon) { Some(self.parse_type()?) } else { None };
                self.expect_bind_eq()?;
                let init = self.parse_expr()?;
                StmtKind::Bind { kind: BindKind::Let, pat, ty, init }
            }
            T::Var => self.parse_named_bind(BindKind::Var)?,
            T::Mut if self.nth(1) == T::Ident && matches!(self.nth(2), T::Eq | T::Colon) => {
                self.parse_named_bind(BindKind::Mut)?
            }
            T::While => {
                self.bump();
                let cond = self.parse_expr_ctx(Ctx::NoStruct)?;
                let body = self.parse_block()?;
                StmtKind::While { cond, body }
            }
            T::Loop => {
                self.bump();
                StmtKind::Loop { body: self.parse_block()? }
            }
            T::For => {
                self.bump();
                let mutable = self.eat(T::Mut);
                let pat = self.parse_pattern()?;
                self.expect(T::In, "`in`")?;
                let first = self.parse_expr_ctx(Ctx::NoStruct)?;
                let iter = if self.at(T::DotDot) || self.at(T::DotDotEq) {
                    let inclusive = self.bump().kind == T::DotDotEq;
                    let end = self.parse_expr_ctx(Ctx::NoStruct)?;
                    ForIter::Range { start: first, end, inclusive }
                } else {
                    ForIter::Expr(first)
                };
                let body = self.parse_block()?;
                StmtKind::For { mutable, pat, iter, body }
            }
            k if k.is_binary_operator()
                && !matches!(k, T::Minus | T::Pipe | T::OrOr | T::Amp | T::AndAnd) =>
            {
                let t = self.tok();
                let mut d = Diagnostic::new(
                    codes::E0103,
                    t.span,
                    format!("a line can't start with the binary operator {}", describe(k)),
                )
                .with_note("a line break ends a statement; an operator that continues an expression goes at the end of the line before (D-079)");
                if t.line_break_before && self.pos >= 2 {
                    // Move the operator to the end of the previous line.
                    let prev = self.tokens[self.pos - 2].span; // the token before the NEWLINE
                    let op = self.text_of(t.span).to_string();
                    let ws_end = self.tokens.get(self.pos + 1).map_or(t.span.end, |n| n.span.start);
                    d = d.with_fix_edits(
                        format!("put `{op}` at the end of the previous line"),
                        vec![
                            wrela_diag::Edit {
                                span: prev.shrink_to_end(),
                                replacement: format!(" {op}"),
                            },
                            wrela_diag::Edit {
                                span: Span::new(self.file, t.span.start, ws_end),
                                replacement: String::new(),
                            },
                        ],
                    );
                }
                self.error(d);
                return Err(Failed);
            }
            _ => {
                // An expression, or an assignment whose target is a postfix expression.
                let e = self.parse_expr()?;
                if let Some(op) = self.assign_op() {
                    if !is_postfix_shaped(&e) {
                        let t = self.tok();
                        self.error(Diagnostic::new(
                            codes::E0100,
                            t.span,
                            "only a name, a field, an element or a call result can be assigned to",
                        ));
                        return Err(Failed);
                    }
                    self.bump();
                    let value = self.parse_expr()?;
                    StmtKind::Assign { target: e, op, value }
                } else {
                    StmtKind::Expr(e)
                }
            }
        };
        Ok(Stmt { kind, span: start.to(self.prev_span()) })
    }

    fn expect_bind_eq(&mut self) -> PResult<()> {
        if self.eat(T::Eq) {
            return Ok(());
        }
        if self.at(T::Newline) || self.at(T::Semi) || self.at(T::RBrace) {
            let t = self.prev_span();
            self.error(
                Diagnostic::new(codes::E0100, t, "a binding needs a value")
                    .with_help("wrela has no uninitialized variables; write `= value`"),
            );
            return Err(Failed);
        }
        Err(self.expected("`=`"))
    }

    /// ("var" | "mut") IDENT (":" type)? "=" expr
    fn parse_named_bind(&mut self, kind: BindKind) -> PResult<StmtKind> {
        self.bump();
        let name = self.ident("a name")?;
        let pat = Pat { id: self.id(), span: name.span, kind: PatKind::Ident(name) };
        let ty = if self.eat(T::Colon) { Some(self.parse_type()?) } else { None };
        self.expect_bind_eq()?;
        let init = self.parse_expr()?;
        Ok(StmtKind::Bind { kind, pat, ty, init })
    }

    fn assign_op(&self) -> Option<AssignOp> {
        Some(match self.kind() {
            T::Eq => AssignOp::Assign,
            T::PlusEq => AssignOp::Add,
            T::MinusEq => AssignOp::Sub,
            T::StarEq => AssignOp::Mul,
            T::SlashEq => AssignOp::Div,
            T::PercentEq => AssignOp::Rem,
            T::StarStarEq => AssignOp::Pow,
            T::AmpEq => AssignOp::BitAnd,
            T::PipeEq => AssignOp::BitOr,
            T::CaretEq => AssignOp::BitXor,
            T::ShlEq => AssignOp::Shl,
            T::ShrEq => AssignOp::Shr,
            _ => return None,
        })
    }

    // ---- expressions -----------------------------------------------------------------------

    /// expr ::= closure | jump | or_expr
    pub(crate) fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_expr_ctx(Ctx::Full)
    }

    pub(crate) fn parse_expr_ctx(&mut self, ctx: Ctx) -> PResult<Expr> {
        if ctx == Ctx::Full {
            match self.kind() {
                T::Pipe | T::OrOr => return self.parse_closure(),
                T::Return => {
                    let start = self.bump().span;
                    let value =
                        if self.starts_expr() { Some(Box::new(self.parse_expr()?)) } else { None };
                    return Ok(self.expr_node(ExprKind::Return(value), start.to(self.prev_span())));
                }
                T::Break => {
                    let s = self.bump().span;
                    return Ok(self.expr_node(ExprKind::Break, s));
                }
                T::Continue => {
                    let s = self.bump().span;
                    return Ok(self.expr_node(ExprKind::Continue, s));
                }
                _ => {}
            }
        }
        self.parse_binary(0, ctx)
    }

    /// Can the current token start an expression (`expr`)?
    fn starts_expr(&self) -> bool {
        matches!(
            self.kind(),
            T::Ident
                | T::Int
                | T::Float
                | T::Suffixed
                | T::Str
                | T::True
                | T::False
                | T::SelfValue
                | T::SelfType
                | T::LParen
                | T::LBracket
                | T::LBrace
                | T::If
                | T::Match
                | T::Minus
                | T::Bang
                | T::Take
                | T::Mut
                | T::Pipe
                | T::OrOr
                | T::Return
                | T::Break
                | T::Continue
        )
    }

    /// closure ::= ("|" (closure_param ("," closure_param)* ","?)? "|" | "||") ("->" type block | expr)
    fn parse_closure(&mut self) -> PResult<Expr> {
        let start = self.span();
        let mut params = Vec::new();
        if !self.eat(T::OrOr) {
            self.expect(T::Pipe, "`|`")?;
            while !self.at(T::Pipe) {
                let name = self.ident("a closure parameter")?;
                let ty = if self.eat(T::Colon) { Some(self.parse_type()?) } else { None };
                params.push(ClosureParam { name, ty });
                if !self.eat(T::Comma) {
                    break;
                }
            }
            self.expect(T::Pipe, "`,` or `|`")?;
        }
        let (ret, body) = if self.eat(T::Arrow) {
            let ty = self.parse_type()?;
            let b = self.parse_block()?;
            let span = b.span;
            (Some(ty), self.expr_node(ExprKind::Block(b), span))
        } else {
            (None, self.parse_expr()?)
        };
        Ok(self.expr_node(
            ExprKind::Closure { params, ret, body: Box::new(body) },
            start.to(self.prev_span()),
        ))
    }

    /// Binary levels, loosest first: `||`, `&&`, comparisons (non-chaining), `|`, `^`, `&`,
    /// shifts, `+ -`, `* / %`.
    fn parse_binary(&mut self, level: usize, ctx: Ctx) -> PResult<Expr> {
        const LEVELS: usize = 9;
        if level == LEVELS {
            return self.parse_unary(ctx);
        }
        let mut lhs = self.parse_binary(level + 1, ctx)?;
        loop {
            let op = match (level, self.kind()) {
                (0, T::OrOr) => BinOp::Or,
                (1, T::AndAnd) => BinOp::And,
                (2, T::EqEq) => BinOp::Eq,
                (2, T::Ne) => BinOp::Ne,
                (2, T::Lt) => BinOp::Lt,
                (2, T::Le) => BinOp::Le,
                (2, T::Gt) => BinOp::Gt,
                (2, T::Ge) => BinOp::Ge,
                (3, T::Pipe) => BinOp::BitOr,
                (4, T::Caret) => BinOp::BitXor,
                (5, T::Amp) => BinOp::BitAnd,
                (6, T::Shl) => BinOp::Shl,
                (6, T::Shr) => BinOp::Shr,
                (7, T::Plus) => BinOp::Add,
                (7, T::Minus) => BinOp::Sub,
                (8, T::Star) => BinOp::Mul,
                (8, T::Slash) => BinOp::Div,
                (8, T::Percent) => BinOp::Rem,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_binary(level + 1, ctx)?;
            let span = lhs.span.to(rhs.span);
            lhs = self.expr_node(ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), span);
            if level == 2 {
                // cmp_expr ::= bitor_expr (cmp_op bitor_expr)?
                if matches!(self.kind(), T::EqEq | T::Ne | T::Lt | T::Le | T::Gt | T::Ge) {
                    let t = self.tok();
                    self.error(
                        Diagnostic::new(codes::E0107, t.span, "comparisons don't chain")
                            .with_help("combine them with `&&`, as in `a < b && b < c`"),
                    );
                    return Err(Failed);
                }
                break;
            }
        }
        if level == 0 && matches!(self.kind(), T::Eq) && ctx == Ctx::NoStruct {
            let t = self.tok();
            self.error(
                Diagnostic::new(
                    codes::E0110,
                    t.span,
                    "`=` assigns; a condition compares with `==`",
                )
                .with_fix("compare with `==`", t.span, "=="),
            );
            return Err(Failed);
        }
        Ok(lhs)
    }

    /// unary_expr ::= ("-" | "!" | "take" | "mut") unary_expr | power_expr
    fn parse_unary(&mut self, ctx: Ctx) -> PResult<Expr> {
        let start = self.span();
        let kind = self.kind();
        if matches!(kind, T::Minus | T::Bang | T::Take | T::Mut) {
            self.bump();
            let inner = Box::new(self.parse_unary(ctx)?);
            let span = start.to(inner.span);
            let k = match kind {
                T::Minus => ExprKind::Unary(UnOp::Neg, inner),
                T::Bang => ExprKind::Unary(UnOp::Not, inner),
                T::Take => ExprKind::Take(inner),
                _ => ExprKind::MutArg(inner),
            };
            return Ok(self.expr_node(k, span));
        }
        if matches!(kind, T::Amp | T::AndAnd) {
            self.error(
                Diagnostic::new(codes::E0106, start, "`&` doesn't take a reference in wrela")
                    .with_note("there are no references; a parameter borrows its argument by default")
                    .with_help("pass the value itself: `f(x)` borrows, `f(mut x)` lends it mutably, `f(take x)` moves it")
                    .with_fix("remove the `&`", start, ""),
            );
            return Err(Failed);
        }
        self.parse_power(ctx)
    }

    /// power_expr ::= postfix_expr ("**" unary_expr)?
    fn parse_power(&mut self, ctx: Ctx) -> PResult<Expr> {
        let base = self.parse_postfix(ctx)?;
        if self.eat(T::StarStar) {
            let exp = self.parse_unary(ctx)?;
            let span = base.span.to(exp.span);
            return Ok(
                self.expr_node(ExprKind::Binary(BinOp::Pow, Box::new(base), Box::new(exp)), span)
            );
        }
        Ok(base)
    }

    /// postfix_expr ::= primary_expr postfixes, where a field access is never directly
    /// followed by call arguments.
    fn parse_postfix(&mut self, ctx: Ctx) -> PResult<Expr> {
        let mut e = self.parse_primary(ctx)?;
        let mut after_field = false;
        loop {
            match self.kind() {
                T::LParen => {
                    if after_field {
                        let t = self.tok();
                        let mut d = Diagnostic::new(
                            codes::E0100,
                            t.span,
                            "a field can't be called directly",
                        )
                        .with_help("to call a function stored in a field, wrap it in parentheses: `(a.f)(x)`");
                        if let ExprKind::Field { name: FieldName::Index(..), .. } = e.kind {
                            d = d.with_help("`.0(` isn't a method call");
                        }
                        self.error(d);
                        return Err(Failed);
                    }
                    let (args, multiline) = self.parse_call_args()?;
                    let span = e.span.to(self.prev_span());
                    e = self
                        .expr_node(ExprKind::Call { callee: Box::new(e), args, multiline }, span);
                    after_field = false;
                }
                T::LBracket => {
                    self.bump();
                    let index = self.parse_expr()?;
                    self.expect(T::RBracket, "`]`")?;
                    let span = e.span.to(self.prev_span());
                    e = self.expr_node(
                        ExprKind::Index { base: Box::new(e), index: Box::new(index) },
                        span,
                    );
                    after_field = false;
                }
                T::Dot => {
                    let dot = self.bump();
                    match self.kind() {
                        T::Ident => {
                            let name = self.ident("a field or method name")?;
                            if self.at(T::LParen)
                                || (self.at(T::ColonColon) && self.nth(1) == T::Lt)
                            {
                                let generics = if self.eat(T::ColonColon) {
                                    Some(self.parse_generic_args()?)
                                } else {
                                    None
                                };
                                let (args, multiline) = self.parse_call_args()?;
                                let span = e.span.to(self.prev_span());
                                e = self.expr_node(
                                    ExprKind::MethodCall {
                                        receiver: Box::new(e),
                                        name,
                                        generics,
                                        args,
                                        newline_before: dot.line_break_before,
                                        multiline,
                                    },
                                    span,
                                );
                                after_field = false;
                            } else {
                                let span = e.span.to(name.span);
                                e = self.expr_node(
                                    ExprKind::Field {
                                        base: Box::new(e),
                                        name: FieldName::Ident(name),
                                    },
                                    span,
                                );
                                after_field = true;
                            }
                        }
                        T::Int => {
                            let t = self.bump();
                            let text = self.text_of(t.span);
                            let Some(n) = text.parse::<u32>().ok() else {
                                self.error(Diagnostic::new(
                                    codes::E0100,
                                    t.span,
                                    format!("`{text}` isn't a tuple field"),
                                ));
                                return Err(Failed);
                            };
                            let span = e.span.to(t.span);
                            e = self.expr_node(
                                ExprKind::Field {
                                    base: Box::new(e),
                                    name: FieldName::Index(n, t.span),
                                },
                                span,
                            );
                            after_field = true;
                        }
                        _ => return Err(self.expected("a field or method name after `.`")),
                    }
                }
                _ => break,
            }
        }
        Ok(e)
    }

    /// call_args ::= "(" args? ")"; positional arguments come before named ones.
    pub(crate) fn parse_call_args(&mut self) -> PResult<(Vec<Arg>, bool)> {
        self.expect(T::LParen, "`(`")?;
        let multiline = self.tok().line_break_before && !self.at(T::RParen);
        let mut args: Vec<Arg> = Vec::new();
        while !self.at(T::RParen) {
            let start = self.span();
            let name = if self.at(T::Ident) && self.nth(1) == T::Colon {
                let n = self.ident("a name")?;
                self.bump();
                Some(n)
            } else {
                None
            };
            let value = self.parse_expr()?;
            let span = start.to(value.span);
            if name.is_none()
                && let Some(prev) = args.iter().rev().find(|a| a.name.is_some())
            {
                let prev_name = prev.name.as_ref().map(|n| n.name.clone()).unwrap_or_default();
                self.error(
                    Diagnostic::new(
                        codes::E0105,
                        span,
                        "a positional argument can't follow a named one",
                    )
                    .with_secondary(prev.span, format!("`{prev_name}` is named here"))
                    .with_help("put positional arguments first, or name this one too (D-039)"),
                );
                return Err(Failed);
            }
            args.push(Arg { name, value, span });
            if !self.eat(T::Comma) {
                break;
            }
        }
        self.expect(T::RParen, "`,` or `)`")?;
        Ok((args, multiline))
    }

    /// primary_expr ::= literal | path_expr | struct_literal | paren_expr | array_expr | block | if_expr | match_expr
    fn parse_primary(&mut self, ctx: Ctx) -> PResult<Expr> {
        let t = self.tok();
        match t.kind {
            T::Int | T::Float | T::Suffixed | T::Str | T::True | T::False => {
                self.bump();
                let kind = match t.kind {
                    T::Int => LitKind::Int,
                    T::Float => LitKind::Float,
                    T::Suffixed => LitKind::Suffixed,
                    T::Str => LitKind::Str,
                    T::True => LitKind::Bool(true),
                    _ => LitKind::Bool(false),
                };
                let lit = Lit { kind, text: self.text_of(t.span).to_string(), span: t.span };
                Ok(self.expr_node(ExprKind::Lit(lit), t.span))
            }
            T::Ident | T::SelfType | T::SelfValue => {
                let path = self.parse_path_expr()?;
                if ctx == Ctx::Full && self.at(T::LBrace) {
                    return self.parse_struct_literal(path);
                }
                let span = path.span;
                Ok(self.expr_node(ExprKind::Path(path), span))
            }
            T::LParen => {
                self.bump();
                let mut items = Vec::new();
                let mut trailing = false;
                while !self.at(T::RParen) {
                    items.push(self.parse_expr()?);
                    trailing = self.eat(T::Comma);
                    if !trailing {
                        break;
                    }
                }
                self.expect(T::RParen, "`,` or `)`")?;
                let span = t.span.to(self.prev_span());
                if items.len() == 1 && !trailing {
                    let inner = items.pop().ok_or(Failed)?;
                    Ok(self.expr_node(ExprKind::Paren(Box::new(inner)), span))
                } else {
                    Ok(self.expr_node(ExprKind::Tuple(items), span))
                }
            }
            T::LBracket => {
                self.bump();
                let mut items = Vec::new();
                if !self.at(T::RBracket) {
                    let first = self.parse_expr()?;
                    if self.eat(T::Semi) {
                        let count = self.parse_expr()?;
                        self.expect(T::RBracket, "`]`")?;
                        let span = t.span.to(self.prev_span());
                        return Ok(self.expr_node(
                            ExprKind::ArrayRepeat {
                                value: Box::new(first),
                                count: Box::new(count),
                            },
                            span,
                        ));
                    }
                    items.push(first);
                    while self.eat(T::Comma) {
                        if self.at(T::RBracket) {
                            break;
                        }
                        items.push(self.parse_expr()?);
                    }
                }
                self.expect(T::RBracket, "`,` or `]`")?;
                let span = t.span.to(self.prev_span());
                Ok(self.expr_node(ExprKind::Array(items), span))
            }
            T::LBrace => {
                let b = self.parse_block()?;
                let span = b.span;
                Ok(self.expr_node(ExprKind::Block(b), span))
            }
            T::If => self.parse_if(),
            T::Match => self.parse_match(),
            T::Return | T::Break | T::Continue | T::Pipe | T::OrOr if ctx == Ctx::Full => {
                let what = describe(t.kind);
                self.error(
                    Diagnostic::new(codes::E0100, t.span, format!("{what} can't be an operand"))
                        .with_help("wrap it in parentheses"),
                );
                Err(Failed)
            }
            _ => Err(self.expected("an expression")),
        }
    }

    /// path_expr ::= expr_segment ("::" expr_segment)*;
    /// expr_segment ::= IDENT ("::" generic_args)? | "Self" | "self"
    fn parse_path_expr(&mut self) -> PResult<Path> {
        let start = self.span();
        let mut segments = Vec::new();
        loop {
            let t = self.tok();
            let ident = match t.kind {
                T::Ident => self.ident("a name")?,
                T::SelfType | T::SelfValue => {
                    self.bump();
                    Ident { name: self.text_of(t.span).to_string(), span: t.span }
                }
                _ => return Err(self.expected("a name")),
            };
            let mut generics = None;
            if t.kind == T::Ident && self.at(T::ColonColon) && self.nth(1) == T::Lt {
                self.bump();
                generics = Some(self.parse_generic_args()?);
            }
            segments.push(PathSegment { ident, generics });
            if self.at(T::ColonColon)
                && matches!(self.nth(1), T::Ident | T::SelfType | T::SelfValue)
            {
                self.bump();
            } else if self.at(T::ColonColon) {
                self.bump();
                return Err(self.expected("a name after `::`"));
            } else {
                break;
            }
        }
        Ok(Path { segments, span: start.to(self.prev_span()) })
    }

    /// struct_literal ::= path_expr "{" (field_init ("," field_init)* ("," ".." expr)? ","? | ".." expr ","?)? NEWLINE? "}"
    fn parse_struct_literal(&mut self, path: Path) -> PResult<Expr> {
        self.expect(T::LBrace, "`{`")?;
        let multiline = self.tok().line_break_before && !self.at(T::RBrace);
        let mut fields = Vec::new();
        let mut base = None;
        while !matches!(self.kind(), T::RBrace | T::Newline) {
            if self.eat(T::DotDot) {
                base = Some(Box::new(self.parse_expr()?));
                self.eat(T::Comma);
                break;
            }
            let start = self.span();
            let name = self.ident("a field name")?;
            let value = if self.eat(T::Colon) { Some(self.parse_expr()?) } else { None };
            fields.push(FieldInit { name, value, span: start.to(self.prev_span()) });
            if !self.eat(T::Comma) {
                break;
            }
        }
        let close = self.close_list("`,` or `}`")?;
        let span = path.span.to(close);
        Ok(self.expr_node(ExprKind::StructLit { path, fields, base, multiline }, span))
    }

    /// if_expr ::= "if" expr_ns block ("else" (if_expr | block))?
    fn parse_if(&mut self) -> PResult<Expr> {
        let start = self.expect(T::If, "`if`")?.span;
        let cond = self.parse_expr_ctx(Ctx::NoStruct)?;
        if !self.at(T::LBrace) {
            return Err(self.expected("`{` to start the `if` body"));
        }
        let then = self.parse_block()?;
        let else_ = if self.eat(T::Else) {
            if self.at(T::If) {
                Some(Box::new(self.parse_if()?))
            } else {
                let b = self.parse_block()?;
                let span = b.span;
                Some(Box::new(self.expr_node(ExprKind::Block(b), span)))
            }
        } else if self.at(T::Newline) && self.nth(1) == T::Else {
            let nl = self.tok().span;
            let els = self.tokens[self.pos + 1].span;
            self.error(
                Diagnostic::new(
                    codes::E0111,
                    els,
                    "`else` must be on the same line as the `}` before it",
                )
                .with_note("a line break after `}` ends the `if` statement (L20)")
                .with_fix(
                    "join the lines",
                    Span::new(self.file, nl.start, els.start),
                    " ",
                ),
            );
            return Err(Failed);
        } else {
            None
        };
        Ok(self.expr_node(
            ExprKind::If { cond: Box::new(cond), then, else_ },
            start.to(self.prev_span()),
        ))
    }

    /// match_expr ::= "match" expr_ns "{" (arm (arm_sep arm)* arm_sep?)? "}"
    fn parse_match(&mut self) -> PResult<Expr> {
        let start = self.expect(T::Match, "`match`")?.span;
        let scrutinee = self.parse_expr_ctx(Ctx::NoStruct)?;
        self.expect(T::LBrace, "`{`")?;
        let mut arms = Vec::new();
        while !self.at(T::RBrace) {
            let arm_start = self.span();
            let mut pats = vec![self.parse_pattern()?];
            while self.eat(T::Pipe) {
                pats.push(self.parse_pattern()?);
            }
            let guard =
                if self.eat(T::If) { Some(self.parse_expr_ctx(Ctx::NoStruct)?) } else { None };
            self.expect(T::FatArrow, "`=>`")?;
            let body = self.parse_expr()?;
            arms.push(Arm { pats, guard, body, span: arm_start.to(self.prev_span()) });
            if !(self.eat(T::Comma) || self.eat(T::Newline)) {
                break;
            }
        }
        self.expect(T::RBrace, "`,`, a line break or `}`")?;
        Ok(self.expr_node(
            ExprKind::Match { scrutinee: Box::new(scrutinee), arms },
            start.to(self.prev_span()),
        ))
    }

    // ---- patterns --------------------------------------------------------------------------

    /// pattern ::= "_" | "-"? (INT | FLOAT) | "true" | "false"
    ///           | path_expr ("(" patterns ")" | pattern_fields)? | "(" patterns ")"
    pub(crate) fn parse_pattern(&mut self) -> PResult<Pat> {
        let t = self.tok();
        let kind = match t.kind {
            T::Underscore => {
                self.bump();
                PatKind::Wild
            }
            T::Minus | T::Int | T::Float => {
                let neg = self.eat(T::Minus);
                let lt = self.tok();
                let kind = match lt.kind {
                    T::Int => LitKind::Int,
                    T::Float => LitKind::Float,
                    _ => return Err(self.expected("a number")),
                };
                self.bump();
                PatKind::Lit {
                    neg,
                    lit: Lit { kind, text: self.text_of(lt.span).to_string(), span: lt.span },
                }
            }
            T::True | T::False => {
                self.bump();
                let kind = LitKind::Bool(t.kind == T::True);
                PatKind::Lit {
                    neg: false,
                    lit: Lit { kind, text: self.text_of(t.span).to_string(), span: t.span },
                }
            }
            T::Ident | T::SelfType | T::SelfValue => {
                let path = self.parse_path_expr()?;
                if self.at(T::LParen) {
                    self.bump();
                    let mut pats = Vec::new();
                    while !self.at(T::RParen) {
                        pats.push(self.parse_pattern()?);
                        if !self.eat(T::Comma) {
                            break;
                        }
                    }
                    self.expect(T::RParen, "`,` or `)`")?;
                    PatKind::TupleStruct(path, pats)
                } else if self.at(T::LBrace) {
                    self.bump();
                    let mut fields = Vec::new();
                    let mut rest = false;
                    while !matches!(self.kind(), T::RBrace | T::Newline) {
                        if self.eat(T::DotDot) {
                            rest = true;
                            self.eat(T::Comma);
                            break;
                        }
                        let name = self.ident("a field name")?;
                        let pat =
                            if self.eat(T::Colon) { Some(self.parse_pattern()?) } else { None };
                        fields.push(FieldPat { name, pat });
                        if !self.eat(T::Comma) {
                            break;
                        }
                    }
                    self.close_list("`,` or `}`")?;
                    PatKind::Struct { path, fields, rest }
                } else if path.is_single() && t.kind == T::Ident {
                    PatKind::Ident(path.segments.into_iter().next().ok_or(Failed)?.ident)
                } else {
                    PatKind::Path(path)
                }
            }
            T::LParen => {
                self.bump();
                let mut pats = Vec::new();
                let mut trailing = false;
                while !self.at(T::RParen) {
                    pats.push(self.parse_pattern()?);
                    trailing = self.eat(T::Comma);
                    if !trailing {
                        break;
                    }
                }
                self.expect(T::RParen, "`,` or `)`")?;
                if pats.len() == 1 && !trailing {
                    // `(p)` is just `p`, parenthesized.
                    let mut inner = pats.pop().ok_or(Failed)?;
                    inner.span = t.span.to(self.prev_span());
                    return Ok(inner);
                }
                PatKind::Tuple(pats)
            }
            _ => return Err(self.expected("a pattern")),
        };
        Ok(Pat { id: self.id(), kind, span: t.span.to(self.prev_span()) })
    }
}

/// The grammar's assignment target is a postfix_expr: a primary followed by postfixes.
fn is_postfix_shaped(e: &Expr) -> bool {
    !matches!(
        e.kind,
        ExprKind::Unary(..)
            | ExprKind::Binary(..)
            | ExprKind::Take(_)
            | ExprKind::MutArg(_)
            | ExprKind::Closure { .. }
            | ExprKind::Return(_)
            | ExprKind::Break
            | ExprKind::Continue
    )
}
