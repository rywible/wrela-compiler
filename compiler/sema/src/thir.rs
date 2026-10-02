//! The typed tree: a function body after type checking. Every expression has a type (possibly
//! mentioning the function's generic parameters), every name is resolved, every method call is
//! a call with its receiver as the first argument, and named and defaulted arguments are in
//! parameter order. The memory checker and lowering read this, never the AST.

use crate::builtins::BuiltinFn;
use crate::defs::{Mode, RetMode};
use crate::ty::*;
use wrela_diag::Span;
use wrela_syntax::ast::{BinOp, UnOp};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalId(pub u32);

impl LocalId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// How a local holds its value (§6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalKind {
    /// A function parameter, with its mode.
    Param(Mode),
    /// Owns its value: `let x = temporary`, `var x = ...`, a `Copy` binding, a range loop's index.
    Owned { mutable: bool },
    /// A projection of an existing place: `let x = place`, `mut x = place`, `for x in array`, or
    /// a pattern binding into a place.
    Projection { mutable: bool },
    /// A closure's parameter: owned and immutable.
    ClosureParam,
}

#[derive(Clone, Debug)]
pub struct LocalDecl {
    pub name: String,
    pub ty: TyId,
    pub kind: LocalKind,
    pub span: Span,
    /// The `let` that binds it alone (`let x = ...`), for fixes that change it to `var`.
    pub keyword: Option<Span>,
    /// The closure this local belongs to, if it's declared inside one.
    pub closure: Option<ClosureId>,
}

#[derive(Clone, Debug)]
pub struct ClosureDef {
    pub params: Vec<LocalId>,
    pub ret: TyId,
    pub body: Expr,
    /// Locals of the enclosing function the body uses, in first-use order, and whether the
    /// body writes through them.
    pub captures: Vec<(LocalId, bool)>,
    pub span: Span,
}

/// A checked function body.
#[derive(Clone, Debug)]
pub struct Body {
    pub params: Vec<LocalId>,
    pub locals: Vec<LocalDecl>,
    pub closures: Vec<ClosureDef>,
    pub value: Expr,
    /// For a return type that names traits: the concrete type the body returns.
    pub hidden_ret: Option<TyId>,
}

impl Body {
    pub fn local(&self, l: LocalId) -> &LocalDecl {
        &self.locals[l.index()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lit {
    /// Any integer or float type's literal: its exact value (negative only for a negated
    /// literal in a pattern; a negation in an expression is a `Unary`).
    Int(i128),
    Float(f64),
    Bool(bool),
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub ty: TyId,
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Callee {
    /// A free function or inherent method; `args` are all its generics (owner's, then own).
    Fn {
        func: FnId,
        args: Vec<TyId>,
    },
    /// A trait method, resolved to an impl (or the default) when monomorphized.
    TraitMethod {
        method: FnId,
        self_ty: TyId,
        trait_args: Vec<TyId>,
        method_args: Vec<TyId>,
    },
    Builtin(BuiltinFn),
    /// A closure in a local, or a function-typed parameter.
    Local(LocalId),
    /// `.clone()`: structural.
    Clone,
}

#[derive(Clone, Debug)]
pub struct Call {
    pub callee: Callee,
    /// In parameter order, defaults filled in.
    pub args: Vec<Expr>,
    /// Each argument's parameter mode.
    pub modes: Vec<Mode>,
    /// Whether the first argument is a method receiver (unmarked at the call site, §6.2).
    pub receiver: bool,
    /// The order the arguments are evaluated in: as written (the receiver first), then the
    /// defaults, by parameter index into `args`.
    pub order: Vec<usize>,
    /// A call to a function returning `borrow T` or `mut T` is a place.
    pub ret_mode: RetMode,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Lit(Lit),
    Local(LocalId),
    Const(ConstId),
    Unary(UnOp, Box<Expr>),
    /// Operands may be a vector and a scalar (the scalar applies to every component), or a
    /// matrix and a vector.
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call(Call),
    /// A struct field or tuple element, by index.
    Field(Box<Expr>, u32),
    /// Vector components: one index is a component, more make a vector.
    Swizzle(Box<Expr>, Vec<u8>),
    Index(Box<Expr>, Box<Expr>),
    /// A struct (`variant: None`) or an enum variant. `fields` is complete and in declaration
    /// order; a field `..base` supplies is [`ExprKind::FromBase`]. The fields given are evaluated
    /// in the order written (`order`, by index into `fields`), then the defaults, then `base`.
    Adt {
        adt: AdtId,
        args: Vec<TyId>,
        variant: Option<u32>,
        fields: Vec<Expr>,
        order: Vec<u32>,
        base: Option<Box<Expr>>,
    },
    /// A field of the enclosing struct literal that its `..base` supplies.
    FromBase,
    Tuple(Vec<Expr>),
    Array(Vec<Expr>),
    ArrayRepeat(Box<Expr>, u32),
    /// A vector or matrix built from components; the type says which.
    Construct(Vec<Expr>),
    /// A scalar conversion to this expression's type.
    Convert(Box<Expr>),
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
    Closure(ClosureId),
    /// A named function as a value.
    FnRef(FnId, Vec<TyId>),
    /// `take place`, or `take recv.method()` when `inner` is a call whose receiver it moves.
    Take(Box<Expr>),
    /// `mut place` at a call site or in a projection return.
    MutArg(Box<Expr>),
    Return(Option<Box<Expr>>),
    Break,
    Continue,
    Dispatch(Box<Dispatch>),
    Draw(Box<Draw>),
    Error,
}

#[derive(Clone, Debug)]
pub struct Dispatch {
    pub kernel: FnId,
    pub kernel_args: Vec<TyId>,
    /// The workgroup count: a `u32` (in x; y and z are 1), or a `(u32, u32, u32)`.
    pub groups: Box<Expr>,
    /// One per kernel parameter that isn't a builtin, as (parameter index, argument), in the
    /// order written.
    pub args: Vec<(usize, Expr)>,
}

#[derive(Clone, Debug)]
pub struct Draw {
    pub vertex: (FnId, Vec<TyId>),
    pub fragment: (FnId, Vec<TyId>),
    pub vertices: Expr,
    pub instances: Expr,
    /// The shaders' arguments by name (shared by both when they have the same name).
    pub args: Vec<(String, Expr)>,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    pub ty: TyId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Bind {
        pat: Pat,
        init: Expr,
    },
    Assign {
        place: Expr,
        op: Option<BinOp>,
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
    ForRange {
        var: LocalId,
        start: Expr,
        end: Expr,
        inclusive: bool,
        body: Block,
    },
    /// `for x in array` and `for mut x in array`: `var` projects each element in turn.
    ForEach {
        var: LocalId,
        array: Expr,
        mutable: bool,
        body: Block,
    },
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub pat: Pat,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Pat {
    pub ty: TyId,
    pub kind: PatKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum PatKind {
    Wild,
    Bind(LocalId),
    Lit(Lit),
    /// A struct (`variant: None`) or variant, with sub-patterns by field index.
    Adt {
        adt: AdtId,
        args: Vec<TyId>,
        variant: Option<u32>,
        fields: Vec<(u32, Pat)>,
    },
    Tuple(Vec<Pat>),
    Or(Vec<Pat>),
}

impl Expr {
    /// Whether this expression names a place: a local, a field or element of one, or a
    /// projection returned from a call.
    pub fn is_place(&self) -> bool {
        match &self.kind {
            ExprKind::Local(_) | ExprKind::Const(_) => true,
            ExprKind::Field(b, _) | ExprKind::Swizzle(b, _) | ExprKind::Index(b, _) => b.is_place(),
            ExprKind::Call(c) => c.ret_mode != RetMode::Owned,
            _ => false,
        }
    }
}
