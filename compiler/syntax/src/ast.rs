//! The abstract syntax tree. It keeps enough of the source's shape (parentheses, multi-line
//! lists, leading-dot chains) for the formatter to reproduce it.

use crate::token::TokenKind;
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

/// `pub`, or `pub(package)`: visible outside the module, and with `package`, only inside the
/// package (§3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vis {
    pub span: Span,
    pub package: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub attrs: Vec<Attribute>,
    /// `pub` or `pub(package)`, if the item has it.
    pub vis: Option<Vis>,
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Trait(TraitDecl),
    /// `trait Sim = Clone + StateHash`: a name for a set of traits (language.md §7).
    TraitSet(TraitSetDecl),
    Impl(ImplDecl),
    Const(ConstDecl),
    Use(UseTree),
    /// `type Name<T> = Type`.
    TypeAlias(TypeAliasDecl),
    /// An item that failed to parse (the error is reported), with its name if that parsed.
    Error(Option<Ident>),
}

impl ItemKind {
    pub fn name(&self) -> Option<&Ident> {
        match self {
            ItemKind::Fn(f) => Some(&f.name),
            ItemKind::Struct(s) => Some(&s.name),
            ItemKind::Enum(e) => Some(&e.name),
            ItemKind::Trait(t) => Some(&t.name),
            ItemKind::TraitSet(t) => Some(&t.name),
            ItemKind::Const(c) => Some(&c.name),
            ItemKind::TypeAlias(t) => Some(&t.name),
            ItemKind::Error(name) => name.as_ref(),
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
    /// `const N: u32`: a constant parameter, of this type (its bounds are empty).
    pub const_ty: Option<TypeExpr>,
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
    /// The `)` that closes the parameters.
    pub params_close: Span,
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

impl TypeExpr {
    /// Where a type failed to parse.
    pub fn error(span: Span) -> TypeExpr {
        TypeExpr { kind: TypeExprKind::Error, span }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeExprKind {
    Path(Path),
    /// `[T; N]`, or `[T]` when the length is `None`.
    Array(Box<TypeExpr>, Option<Box<Expr>>),
    /// `()` is the empty tuple.
    Tuple(Vec<TypeExpr>),
    Paren(Box<TypeExpr>),
    /// `@deterministic fn(mut W, T) -> R`: the attributes' names, then the parameters.
    Fn(FnType),
    /// A constant generic argument: `Ring<f32, 8>`.
    Int(Lit),
    /// Traits joined with `+` where a trait names a type: a parameter's type (any type with
    /// them all) or a return type (the one type the body returns): `-> Field<Tissue> +
    /// Lipschitz`.
    Traits(Vec<TypeExpr>),
    /// Where a type failed to parse (the error is reported).
    Error,
}

/// A function type.
#[derive(Clone, Debug, PartialEq)]
pub struct FnType {
    pub attrs: Vec<Ident>,
    pub params: Vec<FnTypeParam>,
    pub ret: Option<Box<TypeExpr>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnTypeParam {
    pub mode: Mode,
    pub ty: TypeExpr,
    pub span: Span,
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
    pub vis: Option<Vis>,
    pub name: Ident,
    /// A borrow struct's field may be a projection: `borrow T` or `mut T`.
    pub mode: RetMode,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StructDecl {
    /// `borrow struct`: a named group of projections (language.md §6.6).
    pub borrow: bool,
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
    /// `Hidden = 99`: a fieldless variant's discriminant, its tag and its `u32` (§3).
    pub discriminant: Option<Lit>,
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

/// `trait Name<T> = A + B<T>`: the traits it names, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct TraitSetDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub traits: Vec<TypeExpr>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ImplMemberKind {
    Fn(FnDecl),
    Type { name: Ident, ty: TypeExpr },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImplMember {
    pub attrs: Vec<Attribute>,
    pub vis: Option<Vis>,
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
pub struct TypeAliasDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub ty: TypeExpr,
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

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

/// `var x = e` declares a variable; `mut x = e` names a place to change through, and
/// `borrow x = e` a place to read in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VarKind {
    Var,
    Mut,
    Borrow,
}

impl VarKind {
    pub fn keyword(self) -> &'static str {
        match self {
            VarKind::Var => "var",
            VarKind::Mut => "mut",
            VarKind::Borrow => "borrow",
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
    /// The assignment a token is, if it's one.
    pub fn from_token(k: TokenKind) -> Option<AssignOp> {
        Some(match k {
            TokenKind::Eq => AssignOp::Assign,
            TokenKind::PlusEq => AssignOp::Add,
            TokenKind::MinusEq => AssignOp::Sub,
            TokenKind::StarEq => AssignOp::Mul,
            TokenKind::SlashEq => AssignOp::Div,
            TokenKind::PercentEq => AssignOp::Rem,
            TokenKind::StarStarEq => AssignOp::Pow,
            TokenKind::AmpEq => AssignOp::BitAnd,
            TokenKind::PipeEq => AssignOp::BitOr,
            TokenKind::CaretEq => AssignOp::BitXor,
            TokenKind::ShlEq => AssignOp::Shl,
            TokenKind::ShrEq => AssignOp::Shr,
            _ => return None,
        })
    }

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
    /// `let p = e`, or `let p = e else { ... }`, whose block must leave the scope.
    Let {
        pat: Pat,
        ty: Option<TypeExpr>,
        init: Expr,
        else_: Option<Block>,
    },
    /// `var x = e`, `mut x = e` or `borrow x = e`: one name, not a pattern.
    Var {
        kind: VarKind,
        name: Ident,
        ty: Option<TypeExpr>,
        init: Expr,
    },
    Assign {
        target: Expr,
        op: AssignOp,
        value: Expr,
    },
    Expr(Expr),
    /// `while cond { body }`, or `while let pat = cond { body }`: runs `body` while `cond` is
    /// true, or matches `pat`.
    While {
        pat: Option<Pat>,
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
    /// The binary operator a token is, if it's one.
    pub fn from_token(k: TokenKind) -> Option<BinOp> {
        Some(match k {
            TokenKind::OrOr => BinOp::Or,
            TokenKind::AndAnd => BinOp::And,
            TokenKind::EqEq => BinOp::Eq,
            TokenKind::Ne => BinOp::Ne,
            TokenKind::Lt => BinOp::Lt,
            TokenKind::Le => BinOp::Le,
            TokenKind::Gt => BinOp::Gt,
            TokenKind::Ge => BinOp::Ge,
            TokenKind::Pipe => BinOp::BitOr,
            TokenKind::Caret => BinOp::BitXor,
            TokenKind::Amp => BinOp::BitAnd,
            TokenKind::Shl => BinOp::Shl,
            TokenKind::Shr => BinOp::Shr,
            TokenKind::Plus => BinOp::Add,
            TokenKind::Minus => BinOp::Sub,
            TokenKind::Star => BinOp::Mul,
            TokenKind::Slash => BinOp::Div,
            TokenKind::Percent => BinOp::Rem,
            TokenKind::StarStar => BinOp::Pow,
            _ => return None,
        })
    }

    /// How tightly the operator binds, from 0 (`||`) to 9 (`**`). Each level is one level of
    /// the grammar's binary expressions; `**` has its own rule (power_expr).
    pub fn precedence(self) -> u8 {
        match self {
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
    Int(IntValue),
    Float(f64),
    Bool(bool),
    Str,
    /// A number with a unit suffix: `15cm`.
    Suffixed,
}

/// The value of an integer literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntValue {
    Ok(u64),
    /// It doesn't fit in a `u64`.
    TooLarge,
    /// It isn't a number (`0x`, `1e`): the lexer reported it (E0004).
    Malformed,
}

impl IntValue {
    /// The value, if it's a number that fits in a `u64`.
    pub fn ok(self) -> Option<u64> {
        match self {
            IntValue::Ok(v) => Some(v),
            IntValue::TooLarge | IntValue::Malformed => None,
        }
    }
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
    /// An index that isn't a `u32` (`t.0x`, `t.99999999999`): its error is reported.
    BadIndex(Span),
}

impl FieldName {
    pub fn span(&self) -> Span {
        match self {
            FieldName::Ident(i) => i.span,
            FieldName::Index(_, s) | FieldName::BadIndex(s) => *s,
        }
    }
    pub fn text(&self) -> String {
        match self {
            FieldName::Ident(i) => i.name.clone(),
            FieldName::Index(n, _) => n.to_string(),
            FieldName::BadIndex(_) => "<error>".into(),
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
    /// `if cond { }`, or `if let pat = cond { }` when `pat` is there.
    If {
        pat: Option<Box<Pat>>,
        cond: Box<Expr>,
        then: Block,
        else_: Option<Box<Expr>>,
    },
    /// `match x { }`; `match mut x { }` binds mutable projections.
    Match {
        mutable: bool,
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
    /// `x?`: the value of an `Ok` or `Some`, or a return of the error or `None`.
    Try(Box<Expr>),
    /// An assignment as a match arm's body: `Some(s) => s.count += 1`.
    Assign {
        target: Box<Expr>,
        op: AssignOp,
        value: Box<Expr>,
    },
    /// `unsafe { ... }` (the stdlib's core, §6.14).
    Unsafe(Block),
    /// `f"Weight: {w:.1} kg"`: text and holes, in order.
    FString(Vec<FPart>),
    /// A placeholder after a syntax error.
    Error,
}

/// A part of an f-string.
#[derive(Clone, Debug, PartialEq)]
pub enum FPart {
    /// Text, with escapes and `{{ }}` decoded.
    Text(String),
    /// `{expr}` or `{expr:spec}`.
    Hole { expr: Expr, spec: Option<String>, span: Span },
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

impl Pat {
    /// Where a pattern failed to parse.
    pub fn error(span: Span) -> Pat {
        Pat { kind: PatKind::Error, span }
    }
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
    /// Where a pattern failed to parse (the error is reported).
    Error,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Expr {
        Expr { kind, span }
    }

    /// A placeholder after a syntax error.
    pub fn error(span: Span) -> Expr {
        Expr { kind: ExprKind::Error, span }
    }

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
            | ExprKind::Try(x)
            | ExprKind::Field { base: x, .. } => f(x),
            ExprKind::Binary(_, a, b)
            | ExprKind::Index { base: a, index: b }
            | ExprKind::Assign { target: a, value: b, .. }
            | ExprKind::ArrayRepeat { value: a, count: b } => {
                f(a);
                f(b);
            }
            ExprKind::Call { callee: x, args, .. }
            | ExprKind::MethodCall { receiver: x, args, .. } => {
                f(x);
                args.iter().for_each(|a| f(&a.value));
            }
            ExprKind::StructLit { fields, base, .. } => {
                fields.iter().filter_map(|x| x.value.as_ref()).for_each(&mut *f);
                if let Some(b) = base {
                    f(b);
                }
            }
            ExprKind::Tuple(xs) | ExprKind::Array(xs) => xs.iter().for_each(f),
            ExprKind::Block(b) | ExprKind::Unsafe(b) => b.for_each_expr(f),
            ExprKind::FString(parts) => {
                for p in parts {
                    if let FPart::Hole { expr, .. } = p {
                        f(expr);
                    }
                }
            }
            ExprKind::If { cond, then, else_, .. } => {
                f(cond);
                then.for_each_expr(f);
                if let Some(e) = else_ {
                    f(e);
                }
            }
            ExprKind::Match { scrutinee, arms, .. } => {
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
                StmtKind::Let { init, else_, .. } => {
                    f(init);
                    if let Some(b) = else_ {
                        b.for_each_expr(f);
                    }
                }
                StmtKind::Var { init, .. } => f(init),
                StmtKind::Assign { target, value, .. } => {
                    f(target);
                    f(value);
                }
                StmtKind::Expr(e) => f(e),
                StmtKind::While { cond, body, .. } => {
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
            ItemKind::Use(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::TraitSet(_)
            | ItemKind::Error(_) => {}
        }
    }
}
