//! The abstract syntax tree. It keeps enough of the source's shape (parentheses, multi-line
//! lists, leading-dot chains) for the formatter to reproduce it.

use wrela_diag::Span;

#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct File {
    pub items: Vec<Item>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    pub name: Ident,
    pub args: Option<Vec<Arg>>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub attrs: Vec<Attribute>,
    /// The span of `pub`, if the item has it.
    pub vis: Option<Span>,
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Trait(TraitDecl),
    Impl(ImplDecl),
    Const(ConstDecl),
    Use(UseTree),
}

impl ItemKind {
    pub fn name(&self) -> Option<&Ident> {
        match self {
            ItemKind::Fn(f) => Some(&f.name),
            ItemKind::Struct(s) => Some(&s.name),
            ItemKind::Enum(e) => Some(&e.name),
            ItemKind::Trait(t) => Some(&t.name),
            ItemKind::Const(c) => Some(&c.name),
            ItemKind::Impl(_) | ItemKind::Use(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Borrow,
    Mut,
    Take,
}

impl Mode {
    pub fn keyword(self) -> &'static str {
        match self {
            Mode::Borrow => "borrow",
            Mode::Mut => "mut",
            Mode::Take => "take",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenericParam {
    pub name: Ident,
    pub bounds: Vec<TypeExpr>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Param {
    SelfParam { mode: Mode, span: Span },
    Named { name: Ident, mode: Mode, ty: TypeExpr, default: Option<Expr>, span: Span },
}

impl Param {
    pub fn span(&self) -> Span {
        match self {
            Param::SelfParam { span, .. } | Param::Named { span, .. } => *span,
        }
    }
    pub fn mode(&self) -> Mode {
        match self {
            Param::SelfParam { mode, .. } | Param::Named { mode, .. } => *mode,
        }
    }
}

/// `-> T`, `-> borrow T` or `-> mut T`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RetMode {
    Owned,
    Borrow,
    Mut,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RetType {
    pub mode: RetMode,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub ret: Option<RetType>,
    pub body: Option<Block>,
    /// The signature, from `fn` to the end of the return type.
    pub sig_span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeExprKind {
    Path(Path),
    /// `[T; N]`, or `[T]` when the length is `None`.
    Array(Box<TypeExpr>, Option<Box<Expr>>),
    /// `()` is the empty tuple.
    Tuple(Vec<TypeExpr>),
    Paren(Box<TypeExpr>),
    Fn(Vec<TypeExpr>, Option<Box<TypeExpr>>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PathSegment {
    /// An identifier, `Self` or `self`.
    pub ident: Ident,
    /// `Option<T>` in a type, `::<T>` in an expression.
    pub generics: Option<Vec<TypeExpr>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub segments: Vec<PathSegment>,
    pub span: Span,
}

impl Path {
    pub fn is_single(&self) -> bool {
        self.segments.len() == 1 && self.segments[0].generics.is_none()
    }
    pub fn last(&self) -> &Ident {
        &self.segments[self.segments.len() - 1].ident
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldDecl {
    pub vis: Option<Span>,
    pub name: Ident,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StructDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub traits: Vec<TypeExpr>,
    pub fields: Vec<FieldDecl>,
    pub multiline: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VariantKind {
    Unit,
    Tuple(Vec<TypeExpr>),
    Struct(Vec<FieldDecl>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub name: Ident,
    pub kind: VariantKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub traits: Vec<TypeExpr>,
    pub variants: Vec<Variant>,
    pub multiline: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TraitMemberKind {
    Fn(FnDecl),
    Type { name: Ident, bounds: Vec<TypeExpr> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TraitMember {
    pub attrs: Vec<Attribute>,
    pub kind: TraitMemberKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TraitDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub supertraits: Vec<TypeExpr>,
    pub members: Vec<TraitMember>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ImplMemberKind {
    Fn(FnDecl),
    Type { name: Ident, ty: TypeExpr },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImplMember {
    pub attrs: Vec<Attribute>,
    pub vis: Option<Span>,
    pub kind: ImplMemberKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImplDecl {
    pub generics: Vec<GenericParam>,
    /// `impl Trait for Type` has `trait_ = Some(Trait)`; `impl Type` has `None`.
    pub trait_: Option<TypeExpr>,
    pub self_ty: TypeExpr,
    pub members: Vec<ImplMember>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConstDecl {
    pub name: Ident,
    pub ty: Option<TypeExpr>,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UseKind {
    /// `use a::b` or `use a::b as c`.
    Simple(Option<Ident>),
    /// `use a::{b, c}`.
    Group(Vec<UseTree>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct UseTree {
    pub path: Vec<Ident>,
    pub kind: UseKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

impl Block {
    /// The block's value: its last statement, when that's an expression.
    pub fn tail(&self) -> Option<&Expr> {
        match self.stmts.last().map(|s| &s.kind) {
            Some(StmtKind::Expr(e)) => Some(e),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BindKind {
    Let,
    Var,
    Mut,
}

impl BindKind {
    pub fn keyword(self) -> &'static str {
        match self {
            BindKind::Let => "let",
            BindKind::Var => "var",
            BindKind::Mut => "mut",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl AssignOp {
    pub fn text(self) -> &'static str {
        match self {
            AssignOp::Assign => "=",
            AssignOp::Add => "+=",
            AssignOp::Sub => "-=",
            AssignOp::Mul => "*=",
            AssignOp::Div => "/=",
            AssignOp::Rem => "%=",
            AssignOp::Pow => "**=",
            AssignOp::BitAnd => "&=",
            AssignOp::BitOr => "|=",
            AssignOp::BitXor => "^=",
            AssignOp::Shl => "<<=",
            AssignOp::Shr => ">>=",
        }
    }

    /// The binary operator a compound assignment applies.
    pub fn binop(self) -> Option<BinOp> {
        Some(match self {
            AssignOp::Assign => return None,
            AssignOp::Add => BinOp::Add,
            AssignOp::Sub => BinOp::Sub,
            AssignOp::Mul => BinOp::Mul,
            AssignOp::Div => BinOp::Div,
            AssignOp::Rem => BinOp::Rem,
            AssignOp::Pow => BinOp::Pow,
            AssignOp::BitAnd => BinOp::BitAnd,
            AssignOp::BitOr => BinOp::BitOr,
            AssignOp::BitXor => BinOp::BitXor,
            AssignOp::Shl => BinOp::Shl,
            AssignOp::Shr => BinOp::Shr,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ForIter {
    Range { start: Expr, end: Expr, inclusive: bool },
    Expr(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    /// `let p = e`, `var x = e`, `mut x = e`. Only `let` takes a pattern other than a name.
    Bind {
        kind: BindKind,
        pat: Pat,
        ty: Option<TypeExpr>,
        init: Expr,
    },
    Assign {
        target: Expr,
        op: AssignOp,
        value: Expr,
    },
    Expr(Expr),
    While {
        cond: Expr,
        body: Block,
    },
    Loop {
        body: Block,
    },
    For {
        mutable: bool,
        pat: Pat,
        iter: ForIter,
        body: Block,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    BitOr,
    BitXor,
    BitAnd,
    Shl,
    Shr,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
}

impl BinOp {
    pub fn text(self) -> &'static str {
        match self {
            BinOp::Or => "||",
            BinOp::And => "&&",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::BitAnd => "&",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Pow => "**",
        }
    }

    pub fn is_comparison(self) -> bool {
        matches!(self, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LitKind {
    Int,
    Float,
    Bool(bool),
    Str,
    /// A number with a unit suffix (tier 1).
    Suffixed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lit {
    pub kind: LitKind,
    /// The source text.
    pub text: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub name: Option<Ident>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldInit {
    pub name: Ident,
    /// `None` for the shorthand `S { time }`.
    pub value: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arm {
    pub pats: Vec<Pat>,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClosureParam {
    pub name: Ident,
    pub ty: Option<TypeExpr>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldName {
    Ident(Ident),
    /// `.0`
    Index(u32, Span),
}

impl FieldName {
    pub fn span(&self) -> Span {
        match self {
            FieldName::Ident(i) => i.span,
            FieldName::Index(_, s) => *s,
        }
    }
    pub fn text(&self) -> String {
        match self {
            FieldName::Ident(i) => i.name.clone(),
            FieldName::Index(n, _) => n.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Lit(Lit),
    Path(Path),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `take place`
    Take(Box<Expr>),
    /// `mut place`, at a call site or in a projection return.
    MutArg(Box<Expr>),
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
        multiline: bool,
    },
    MethodCall {
        receiver: Box<Expr>,
        name: Ident,
        generics: Option<Vec<TypeExpr>>,
        args: Vec<Arg>,
        /// The `.` starts a line: a leading-dot chain the formatter keeps.
        newline_before: bool,
        multiline: bool,
    },
    Field {
        base: Box<Expr>,
        name: FieldName,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    StructLit {
        path: Path,
        fields: Vec<FieldInit>,
        base: Option<Box<Expr>>,
        multiline: bool,
    },
    Tuple(Vec<Expr>),
    Array(Vec<Expr>),
    ArrayRepeat {
        value: Box<Expr>,
        count: Box<Expr>,
    },
    Paren(Box<Expr>),
    Block(Block),
    If {
        cond: Box<Expr>,
        then: Block,
        else_: Option<Box<Expr>>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<Arm>,
    },
    Closure {
        params: Vec<ClosureParam>,
        ret: Option<TypeExpr>,
        body: Box<Expr>,
    },
    Return(Option<Box<Expr>>),
    Break,
    Continue,
    /// A placeholder after a syntax error.
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldPat {
    pub name: Ident,
    /// `None` for the shorthand `S { a }`.
    pub pat: Option<Pat>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pat {
    pub kind: PatKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatKind {
    Wild,
    /// A name: a new binding, unless it names a unit variant or constant in scope.
    Ident(Ident),
    /// A literal, optionally negated.
    Lit {
        neg: bool,
        lit: Lit,
    },
    Path(Path),
    TupleStruct(Path, Vec<Pat>),
    Struct {
        path: Path,
        fields: Vec<FieldPat>,
        rest: bool,
    },
    Tuple(Vec<Pat>),
}

impl Expr {
    /// Calls `f` on each expression directly inside this one: operands, arguments, fields, the
    /// expressions of a block's statements, arms (guards and bodies) and a closure's body.
    pub fn for_each_child<'a>(&'a self, f: &mut impl FnMut(&'a Expr)) {
        match &self.kind {
            ExprKind::Lit(_)
            | ExprKind::Path(_)
            | ExprKind::Break
            | ExprKind::Continue
            | ExprKind::Error => {}
            ExprKind::Unary(_, x)
            | ExprKind::Take(x)
            | ExprKind::MutArg(x)
            | ExprKind::Paren(x)
            | ExprKind::Field { base: x, .. } => f(x),
            ExprKind::Binary(_, a, b)
            | ExprKind::Index { base: a, index: b }
            | ExprKind::ArrayRepeat { value: a, count: b } => {
                f(a);
                f(b);
            }
            ExprKind::Call { callee, args, .. } => {
                f(callee);
                args.iter().for_each(|a| f(&a.value));
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                f(receiver);
                args.iter().for_each(|a| f(&a.value));
            }
            ExprKind::StructLit { fields, base, .. } => {
                fields.iter().filter_map(|x| x.value.as_ref()).for_each(&mut *f);
                if let Some(b) = base {
                    f(b);
                }
            }
            ExprKind::Tuple(xs) | ExprKind::Array(xs) => xs.iter().for_each(f),
            ExprKind::Block(b) => b.for_each_expr(f),
            ExprKind::If { cond, then, else_ } => {
                f(cond);
                then.for_each_expr(f);
                if let Some(e) = else_ {
                    f(e);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                f(scrutinee);
                for a in arms {
                    if let Some(g) = &a.guard {
                        f(g);
                    }
                    f(&a.body);
                }
            }
            ExprKind::Closure { body, .. } => f(body),
            ExprKind::Return(v) => {
                if let Some(v) = v {
                    f(v);
                }
            }
        }
    }

    /// Calls `f` on this expression and every expression inside it, outermost first.
    pub fn walk(&self, f: &mut impl FnMut(&Expr)) {
        f(self);
        self.for_each_child(&mut |c| c.walk(f));
    }
}

impl Block {
    /// Calls `f` on each expression directly in the block's statements.
    pub fn for_each_expr<'a>(&'a self, f: &mut impl FnMut(&'a Expr)) {
        for s in &self.stmts {
            match &s.kind {
                StmtKind::Bind { init, .. } => f(init),
                StmtKind::Assign { target, value, .. } => {
                    f(target);
                    f(value);
                }
                StmtKind::Expr(e) => f(e),
                StmtKind::While { cond, body } => {
                    f(cond);
                    body.for_each_expr(f);
                }
                StmtKind::Loop { body } => body.for_each_expr(f),
                StmtKind::For { iter, body, .. } => {
                    match iter {
                        ForIter::Range { start, end, .. } => {
                            f(start);
                            f(end);
                        }
                        ForIter::Expr(e) => f(e),
                    }
                    body.for_each_expr(f);
                }
            }
        }
    }
}

impl Item {
    /// Calls `f` on the item's top-level expressions: function bodies (as blocks' statements),
    /// parameter and field defaults, and a constant's value.
    pub fn for_each_body_expr<'a>(&'a self, f: &mut impl FnMut(&'a Expr)) {
        fn fn_decl<'a>(d: &'a FnDecl, f: &mut impl FnMut(&'a Expr)) {
            for p in &d.params {
                if let Param::Named { default: Some(e), .. } = p {
                    f(e);
                }
            }
            if let Some(b) = &d.body {
                b.for_each_expr(f);
            }
        }
        fn fields<'a>(fs: &'a [FieldDecl], f: &mut impl FnMut(&'a Expr)) {
            fs.iter().filter_map(|x| x.default.as_ref()).for_each(f);
        }
        match &self.kind {
            ItemKind::Fn(d) => fn_decl(d, f),
            ItemKind::Struct(s) => fields(&s.fields, f),
            ItemKind::Enum(e) => {
                for v in &e.variants {
                    if let VariantKind::Struct(fs) = &v.kind {
                        fields(fs, f);
                    }
                }
            }
            ItemKind::Trait(t) => {
                for m in &t.members {
                    if let TraitMemberKind::Fn(d) = &m.kind {
                        fn_decl(d, f);
                    }
                }
            }
            ItemKind::Impl(i) => {
                for m in &i.members {
                    if let ImplMemberKind::Fn(d) = &m.kind {
                        fn_decl(d, f);
                    }
                }
            }
            ItemKind::Const(c) => f(&c.value),
            ItemKind::Use(_) => {}
        }
    }
}
