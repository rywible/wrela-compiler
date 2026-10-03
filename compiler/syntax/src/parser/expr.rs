//! Blocks, statements, expressions and patterns.

use super::{Failed, PResult, Parser, describe};
use crate::ast::*;
use crate::lexer::{float_value, int_value};
use crate::token::{Token, TokenKind as T};
use wrela_diag::{Diagnostic, Span, codes};

/// Whether a struct literal may appear here (the `_ns` rules of the grammar exclude it, and
/// closures, from the heads of `if`, `while`, `for` and `match`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ctx {
    Full,
    NoStruct,
}

impl<'a> Parser<'a> {
    /// The literal a token is: a number, a string, `true` or `false`.
    fn lit_of(&self, t: Token) -> Lit {
        let text = self.text_of(t.span);
        let kind = match t.kind {
            T::Int => LitKind::Int(int_value(text)),
            T::Float => LitKind::Float(float_value(text)),
            T::Suffixed => LitKind::Suffixed,
            T::Str => LitKind::Str,
            T::True => LitKind::Bool(true),
            _ => LitKind::Bool(false),
        };
        Lit { kind, text: text.to_string(), span: t.span }
    }

    // ---- blocks and statements -------------------------------------------------------------

    /// block ::= "{" sep* (stmt (sep+ stmt)* sep*)? "}"
    pub(crate) fn parse_block(&mut self) -> PResult<Block> {
        self.nested(Self::parse_block_inner)
    }

    fn parse_block_inner(&mut self) -> PResult<Block> {
        let open = self.expect(T::LBrace, "`{`")?.span;
        let stmts = self.within(T::RBrace, Self::block_stmts);
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

    /// The statements of a block, up to its `}` or the file's end.
    fn block_stmts(&mut self) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        self.skip_seps();
        while !self.at(T::RBrace) && !self.at(T::Eof) {
            let (before, start) = (self.pos, self.span());
            match self.parse_stmt() {
                Ok(s) => {
                    stmts.push(s);
                    if !self.at_sep() && !self.at(T::RBrace) {
                        self.stmt_sep_error();
                        let rest = self.span();
                        self.recover_to_sep();
                        let span = self.since(rest);
                        stmts.push(Stmt { kind: StmtKind::Expr(Expr::error(span)), span });
                    }
                }
                Err(Failed) => {
                    // The statement stays, as an error node: later passes know something was
                    // there.
                    self.recover_to_sep();
                    let span = self.since(start);
                    stmts.push(Stmt { kind: StmtKind::Expr(Expr::error(span)), span });
                }
            }
            self.skip_seps();
            if self.pos == before && !self.at(T::RBrace) && !self.at(T::Eof) {
                self.bump();
            }
        }
        stmts
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
        if BinOp::from_token(t.kind).is_some() || t.kind == T::Eq {
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
                let ty = self.opt_colon_type()?;
                self.expect_bind_eq()?;
                // A value that fails to parse leaves the binding, so its uses still resolve.
                let init = self.expr_or_error(Self::parse_expr);
                StmtKind::Let { pat, ty, init }
            }
            T::Var => self.parse_named_bind(VarKind::Var)?,
            T::Mut if self.nth(1) == T::Ident && matches!(self.nth(2), T::Eq | T::Colon) => {
                self.parse_named_bind(VarKind::Mut)?
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
            k if BinOp::from_token(k).is_some()
                && !matches!(k, T::Minus | T::Pipe | T::OrOr | T::Amp | T::AndAnd) =>
            {
                let t = self.tok();
                let mut d = Diagnostic::new(
                    codes::E0103,
                    t.span,
                    format!("a line can't start with the binary operator {}", describe(k)),
                )
                .with_note("a line break ends a statement; an operator that continues an expression goes at the end of the line before (D-079)");
                // The line before must end in an operand: a NEWLINE comes only after a token that
                // can end a statement (L17), and those that aren't operands can't take one.
                let prev = self.pos.checked_sub(2).map(|i| self.tokens[i]).filter(|b| {
                    self.tokens[self.pos - 1].kind == T::Newline
                        && b.kind.can_end_statement()
                        && !matches!(b.kind, T::Return | T::Break | T::Continue | T::Gt | T::Shr)
                });
                if let Some(prev) = prev {
                    let op = self.text_of(t.span).to_string();
                    if matches!(k, T::Gt | T::Shr) {
                        // `>` and `>>` can't end a line (L20): the two lines are joined.
                        d = d.with_fix_edits("join the lines", self.join_lines(prev.span, t.span));
                    } else {
                        // Move the operator to the end of the previous line. A comment after it
                        // stays where it is.
                        let rest = &self.text[t.span.end as usize..];
                        let spaces = rest.len() - rest.trim_start_matches([' ', '\t']).len();
                        d = d.with_fix_edits(
                            format!("put `{op}` at the end of the previous line"),
                            vec![
                                wrela_diag::Edit {
                                    span: prev.span.shrink_to_end(),
                                    replacement: format!(" {op}"),
                                },
                                wrela_diag::Edit {
                                    span: Span::new(
                                        self.file,
                                        t.span.start,
                                        t.span.end + spaces as u32,
                                    ),
                                    replacement: String::new(),
                                },
                            ],
                        );
                    }
                }
                self.error(d);
                return Err(Failed);
            }
            _ => {
                // An expression, or an assignment whose target is a postfix expression.
                let e = self.parse_expr()?;
                if let Some(op) = AssignOp::from_token(self.kind()) {
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
    fn parse_named_bind(&mut self, kind: VarKind) -> PResult<StmtKind> {
        self.bump();
        let name = self.ident("a name")?;
        let ty = self.opt_colon_type()?;
        self.expect_bind_eq()?;
        let init = self.expr_or_error(Self::parse_expr);
        Ok(StmtKind::Var { kind, name, ty, init })
    }

    // ---- expressions -----------------------------------------------------------------------

    /// expr ::= closure | jump | or_expr
    pub(crate) fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_expr_ctx(Ctx::Full)
    }

    pub(crate) fn parse_expr_ctx(&mut self, ctx: Ctx) -> PResult<Expr> {
        self.nested(|p| p.parse_expr_inner(ctx))
    }

    fn parse_expr_inner(&mut self, ctx: Ctx) -> PResult<Expr> {
        if ctx == Ctx::Full {
            match self.kind() {
                T::Pipe | T::OrOr => return self.parse_closure(),
                T::Return => {
                    let start = self.bump().span;
                    let value =
                        if self.starts_expr() { Some(Box::new(self.parse_expr()?)) } else { None };
                    return Ok(Expr::new(ExprKind::Return(value), start.to(self.prev_span())));
                }
                T::Break => {
                    let s = self.bump().span;
                    return Ok(Expr::new(ExprKind::Break, s));
                }
                T::Continue => {
                    let s = self.bump().span;
                    return Ok(Expr::new(ExprKind::Continue, s));
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
            params = self.list(
                T::Pipe,
                |p| {
                    let name = p.ident("a closure parameter")?;
                    let ty = p.opt_colon_type()?;
                    Ok(ClosureParam { name, ty })
                },
                |p, first, span| {
                    let name = p.name_at(first)?;
                    Some(ClosureParam { name, ty: Some(TypeExpr::error(span)) })
                },
            );
            self.close_list(T::Pipe, "`,` or `|`");
        }
        let (ret, body) = if self.eat(T::Arrow) {
            let ty = self.parse_type()?;
            let b = self.parse_block()?;
            let span = b.span;
            (Some(ty), Expr::new(ExprKind::Block(b), span))
        } else {
            (None, self.parse_expr()?)
        };
        Ok(Expr::new(
            ExprKind::Closure { params, ret, body: Box::new(body) },
            start.to(self.prev_span()),
        ))
    }

    /// Binary levels ([`BinOp::precedence`]), loosest first: `||`, `&&`, comparisons
    /// (non-chaining), `|`, `^`, `&`, shifts, `+ -`, `* / %`.
    fn parse_binary(&mut self, level: usize, ctx: Ctx) -> PResult<Expr> {
        const LEVELS: usize = 9;
        if level == LEVELS {
            return self.parse_unary(ctx);
        }
        let mut lhs = self.parse_binary(level + 1, ctx)?;
        let mut links = 0;
        while let Some(op) =
            BinOp::from_token(self.kind()).filter(|op| usize::from(op.precedence()) == level)
        {
            // Each operator makes the tree a level deeper; a chain is built in a loop, not by
            // recursion, so the nesting limit doesn't see it.
            links += 1;
            if links > super::MAX_EXPR_DEPTH {
                self.too_deep_error(lhs.span.shrink_to_start());
                return Err(Failed);
            }
            self.bump();
            let rhs = self.parse_binary(level + 1, ctx)?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr::new(ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), span);
            if level == 2 {
                // cmp_expr ::= bitor_expr (cmp_op bitor_expr)?
                if BinOp::from_token(self.kind()).is_some_and(BinOp::is_comparison) {
                    let t = self.tok();
                    // `a < x < b` means `a < x && x < b`: repeat the middle operand.
                    let mid = match &lhs.kind {
                        ExprKind::Binary(_, _, m) => self.text_of(m.span).to_string(),
                        _ => String::new(),
                    };
                    let mut d = Diagnostic::new(codes::E0107, t.span, "comparisons don't chain")
                        .with_help("combine them with `&&`, as in `a < b && b < c`");
                    if !mid.is_empty() && !mid.contains('\n') {
                        d = d.with_fix(
                            format!("compare `{mid}` twice, joined with `&&`"),
                            Span::new(self.file, t.span.start, t.span.start),
                            format!("&& {mid} "),
                        );
                    }
                    self.error(d);
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
            let inner = Box::new(self.nested(|p| p.parse_unary(ctx))?);
            let span = start.to(inner.span);
            let k = match kind {
                T::Minus => ExprKind::Unary(UnOp::Neg, inner),
                T::Bang => ExprKind::Unary(UnOp::Not, inner),
                T::Take => ExprKind::Take(inner),
                _ => ExprKind::MutArg(inner),
            };
            return Ok(Expr::new(k, span));
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
            let exp = self.nested(|p| p.parse_unary(ctx))?;
            let span = base.span.to(exp.span);
            return Ok(Expr::new(
                ExprKind::Binary(BinOp::Pow, Box::new(base), Box::new(exp)),
                span,
            ));
        }
        Ok(base)
    }

    /// postfix_expr ::= primary_expr postfixes, where a field access is never directly
    /// followed by call arguments.
    fn parse_postfix(&mut self, ctx: Ctx) -> PResult<Expr> {
        let mut e = self.parse_primary(ctx)?;
        let mut after_field = false;
        let mut links = 0;
        loop {
            // As in a chain of binary operators, each link makes the tree a level deeper.
            if matches!(self.kind(), T::LParen | T::LBracket | T::Dot) {
                links += 1;
                if links > super::MAX_EXPR_DEPTH {
                    self.too_deep_error(e.span.shrink_to_start());
                    return Err(Failed);
                }
            }
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
                    e = Expr::new(ExprKind::Call { callee: Box::new(e), args, multiline }, span);
                    after_field = false;
                }
                T::Question => {
                    // `?` is tier 1: say so, and read on as if it weren't there.
                    let q = self.bump();
                    self.error(
                        Diagnostic::new(
                            codes::E0902,
                            q.span,
                            "`?` and `Result` are tier 1, so errors can't be propagated yet",
                        )
                        .with_help("match on the value instead"),
                    );
                }
                T::LBracket => {
                    self.bump();
                    let index = self.within(T::RBracket, Self::parse_expr)?;
                    self.expect(T::RBracket, "`]`")?;
                    let span = e.span.to(self.prev_span());
                    e = Expr::new(
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
                                e = Expr::new(
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
                                e = Expr::new(
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
                            // field_access ::= "." (IDENT | INT): any INT is a tuple index
                            // (`t.0x1` is `t.1`, `t.1_0` is `t.10`); only its value is checked.
                            let value = int_value(text);
                            let name = match value.ok().and_then(|v| u32::try_from(v).ok()) {
                                Some(n) => FieldName::Index(n, t.span),
                                // A malformed number (`0x`) has its error from the lexer.
                                None if value == IntValue::Malformed => FieldName::BadIndex(t.span),
                                None => {
                                    self.error(Diagnostic::new(
                                        codes::E0006,
                                        t.span,
                                        format!("`{text}` is too large for a tuple index"),
                                    ));
                                    FieldName::BadIndex(t.span)
                                }
                            };
                            let span = e.span.to(t.span);
                            e = Expr::new(ExprKind::Field { base: Box::new(e), name }, span);
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
        let args = self.list(
            T::RParen,
            |p| {
                let start = p.span();
                let name = if p.at(T::Ident) && p.nth(1) == T::Colon {
                    let n = p.ident("a name")?;
                    p.bump();
                    Some(n)
                } else {
                    None
                };
                let value = p.parse_expr()?;
                Ok(Arg { name, value, span: start.to(p.prev_span()) })
            },
            |p, first, span| Some(Arg { name: p.name_at(first), value: Expr::error(span), span }),
        );
        self.close_list(T::RParen, "`,` or `)`");
        // Positional arguments come first (D-039); the call is an error otherwise.
        let mut named: Option<&Arg> = None;
        for a in &args {
            match (named, &a.name) {
                (_, Some(_)) => named = Some(a),
                (Some(prev), None) => {
                    let prev_name = prev.name.as_ref().map(|n| n.name.clone()).unwrap_or_default();
                    self.error(
                        Diagnostic::new(
                            codes::E0105,
                            a.span,
                            "a positional argument can't follow a named one",
                        )
                        .with_secondary(prev.span, format!("`{prev_name}` is named here"))
                        .with_help("put positional arguments first, or name this one too (D-039)"),
                    );
                    self.recovered_at = Some(self.pos);
                    return Err(Failed);
                }
                (None, None) => {}
            }
        }
        Ok((args, multiline))
    }

    /// primary_expr ::= literal | path_expr | struct_literal | paren_expr | array_expr | block | if_expr | match_expr
    fn parse_primary(&mut self, ctx: Ctx) -> PResult<Expr> {
        let t = self.tok();
        match t.kind {
            T::Int | T::Float | T::Suffixed | T::Str | T::True | T::False => {
                self.bump();
                Ok(Expr::new(ExprKind::Lit(self.lit_of(t)), t.span))
            }
            T::Unsafe if self.nth(1) == T::LBrace => {
                // `unsafe` is for the stdlib's core, in tier 1: say so, and read the block.
                let u = self.bump();
                self.error(
                    Diagnostic::new(codes::E0904, u.span, "`unsafe` is tier 1")
                        .with_note("only the stdlib's core will use it (language.md §6.14)"),
                );
                let b = self.parse_block()?;
                let span = u.span.to(b.span);
                Ok(Expr::new(ExprKind::Block(b), span))
            }
            T::Ident | T::SelfType | T::SelfValue => {
                let path = self.parse_path_expr()?;
                if ctx == Ctx::Full && self.at(T::LBrace) {
                    return self.parse_struct_literal(path);
                }
                let span = path.span;
                Ok(Expr::new(ExprKind::Path(path), span))
            }
            T::LParen => {
                self.bump();
                let mut items =
                    self.list(T::RParen, Self::parse_expr, |_, _, span| Some(Expr::error(span)));
                let trailing = self.tokens[self.pos - 1].kind == T::Comma;
                self.close_list(T::RParen, "`,` or `)`");
                let span = t.span.to(self.prev_span());
                if items.len() == 1 && !trailing {
                    let inner = items.pop().ok_or(Failed)?;
                    Ok(Expr::new(ExprKind::Paren(Box::new(inner)), span))
                } else {
                    Ok(Expr::new(ExprKind::Tuple(items), span))
                }
            }
            T::LBracket => {
                self.bump();
                let mut items = Vec::new();
                if !self.at(T::RBracket) {
                    let first = self.within(T::RBracket, Self::parse_expr)?;
                    if self.eat(T::Semi) {
                        let count = self.within(T::RBracket, Self::parse_expr)?;
                        self.expect(T::RBracket, "`]`")?;
                        let span = t.span.to(self.prev_span());
                        return Ok(Expr::new(
                            ExprKind::ArrayRepeat {
                                value: Box::new(first),
                                count: Box::new(count),
                            },
                            span,
                        ));
                    }
                    items.push(first);
                    if self.eat(T::Comma) {
                        items.extend(self.list(T::RBracket, Self::parse_expr, |_, _, span| {
                            Some(Expr::error(span))
                        }));
                    }
                }
                self.close_list(T::RBracket, "`,` or `]`");
                let span = t.span.to(self.prev_span());
                Ok(Expr::new(ExprKind::Array(items), span))
            }
            T::LBrace => {
                let b = self.parse_block()?;
                let span = b.span;
                Ok(Expr::new(ExprKind::Block(b), span))
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
                    self.ident_of(t)
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
        let mut base = None;
        let fields = self.list(
            T::RBrace,
            |p| {
                if base.is_some() {
                    // `..base` comes last.
                    return Err(p.expected("`}`"));
                }
                let start = p.span();
                if p.eat(T::DotDot) {
                    base = Some(Box::new(p.parse_expr()?));
                    return Ok(None);
                }
                let name = p.ident("a field name")?;
                let value = if p.eat(T::Colon) { Some(p.parse_expr()?) } else { None };
                Ok(Some(FieldInit { name, value, span: start.to(p.prev_span()) }))
            },
            |p, first, span| {
                let name = p.name_at(first)?;
                Some(Some(FieldInit { name, value: Some(Expr::error(span)), span }))
            },
        );
        let fields: Vec<FieldInit> = fields.into_iter().flatten().collect();
        let close = self.close_list(T::RBrace, "`,` or `}`");
        let span = path.span.to(close);
        Ok(Expr::new(ExprKind::StructLit { path, fields, base, multiline }, span))
    }

    /// if_expr ::= "if" expr_ns block ("else" (if_expr | block))?
    fn parse_if(&mut self) -> PResult<Expr> {
        let start = self.expect(T::If, "`if`")?.span;
        let cond = self.parse_expr_ctx(Ctx::NoStruct)?;
        if !self.at(T::LBrace) {
            return Err(self.expected("`{` to start the `if` body"));
        }
        let then = self.parse_block()?;
        let misplaced = self.at(T::Newline) && self.nth(1) == T::Else;
        if misplaced {
            let close = self.prev_span();
            let els = self.tokens[self.pos + 1].span;
            self.error(
                Diagnostic::new(
                    codes::E0111,
                    els,
                    "`else` must be on the same line as the `}` before it",
                )
                .with_note("a line break after `}` ends the `if` statement (L20)")
                .with_fix_edits("join the lines", self.join_lines(close, els)),
            );
            // Read on as if the lines were joined, so nothing else is reported for it.
            self.bump();
        }
        let else_ = if self.eat(T::Else) {
            if self.at(T::If) {
                Some(Box::new(self.nested(Self::parse_if)?))
            } else {
                let b = self.parse_block()?;
                let span = b.span;
                Some(Box::new(Expr::new(ExprKind::Block(b), span)))
            }
        } else {
            None
        };
        Ok(Expr::new(
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
        self.closers.push(T::RBrace);
        while !self.at(T::RBrace) && !self.at(T::Eof) {
            let arm_start = self.span();
            let arm = (|| -> PResult<Arm> {
                let mut pats = vec![self.parse_pattern()?];
                while self.eat(T::Pipe) {
                    pats.push(self.parse_pattern()?);
                }
                let guard =
                    if self.eat(T::If) { Some(self.parse_expr_ctx(Ctx::NoStruct)?) } else { None };
                self.expect(T::FatArrow, "`=>`")?;
                let body = self.parse_expr()?;
                Ok(Arm { pats, guard, body, span: arm_start.to(self.prev_span()) })
            })();
            match arm {
                Ok(a) => arms.push(a),
                Err(Failed) => {
                    // The arm stays, matching anything, so the match isn't reported as missing
                    // the cases it covered.
                    self.recover_in_list(T::RBrace);
                    let span = self.since(arm_start);
                    let body = Expr::error(span);
                    arms.push(Arm { pats: vec![Pat::error(span)], guard: None, body, span });
                }
            }
            if !(self.eat(T::Comma) || self.eat(T::Newline)) {
                break;
            }
        }
        self.closers.pop();
        self.close_list(T::RBrace, "`,`, a line break or `}`");
        Ok(Expr::new(
            ExprKind::Match { scrutinee: Box::new(scrutinee), arms },
            start.to(self.prev_span()),
        ))
    }

    // ---- patterns --------------------------------------------------------------------------

    /// pattern ::= "_" | "-"? (INT | FLOAT) | "true" | "false"
    ///           | path_expr ("(" patterns ")" | pattern_fields)? | "(" patterns ")"
    pub(crate) fn parse_pattern(&mut self) -> PResult<Pat> {
        self.nested(Self::parse_pattern_inner)
    }

    fn parse_pattern_inner(&mut self) -> PResult<Pat> {
        let t = self.tok();
        let kind = match t.kind {
            T::Underscore => {
                self.bump();
                PatKind::Wild
            }
            T::Minus | T::Int | T::Float => {
                let neg = self.eat(T::Minus);
                let lt = self.tok();
                if !matches!(lt.kind, T::Int | T::Float) {
                    return Err(self.expected("a number"));
                }
                self.bump();
                PatKind::Lit { neg, lit: self.lit_of(lt) }
            }
            T::True | T::False => {
                self.bump();
                PatKind::Lit { neg: false, lit: self.lit_of(t) }
            }
            T::Ident | T::SelfType | T::SelfValue => {
                let path = self.parse_path_expr()?;
                if self.eat(T::LParen) {
                    let pats = self
                        .list(T::RParen, Self::parse_pattern, |_, _, span| Some(Pat::error(span)));
                    self.close_list(T::RParen, "`,` or `)`");
                    PatKind::TupleStruct(path, pats)
                } else if self.at(T::LBrace) {
                    self.bump();
                    let mut rest = false;
                    let fields = self.list(
                        T::RBrace,
                        |p| {
                            if rest {
                                // `..` comes last.
                                return Err(p.expected("`}`"));
                            }
                            if p.eat(T::DotDot) {
                                rest = true;
                                return Ok(None);
                            }
                            let name = p.ident("a field name")?;
                            let pat = if p.eat(T::Colon) { Some(p.parse_pattern()?) } else { None };
                            Ok(Some(FieldPat { name, pat }))
                        },
                        |p, first, span| {
                            let name = p.name_at(first)?;
                            Some(Some(FieldPat { name, pat: Some(Pat::error(span)) }))
                        },
                    );
                    let fields = fields.into_iter().flatten().collect();
                    self.close_list(T::RBrace, "`,` or `}`");
                    PatKind::Struct { path, fields, rest }
                } else if path.is_single() && t.kind == T::Ident {
                    PatKind::Ident(path.segments.into_iter().next().ok_or(Failed)?.ident)
                } else {
                    PatKind::Path(path)
                }
            }
            T::LParen => {
                self.bump();
                let mut pats =
                    self.list(T::RParen, Self::parse_pattern, |_, _, span| Some(Pat::error(span)));
                let trailing = self.tokens[self.pos - 1].kind == T::Comma;
                self.close_list(T::RParen, "`,` or `)`");
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
        Ok(Pat { kind, span: t.span.to(self.prev_span()) })
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
