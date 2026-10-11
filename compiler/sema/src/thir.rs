//! The typed tree: a function body after type checking. Every expression has a type (possibly
//! mentioning the function's generic parameters), every name is resolved, every method call is
//! a call with its receiver as the first argument, and named and defaulted arguments are in
//! parameter order. The memory checker and lowering read this, never the AST.

use crate::builtins::BuiltinFn;
use crate::defs::{Mode, RetMode};
use crate::ty::*;
use wrela_diag::Span;
use wrela_syntax::ast::{BinOp, UnOp};

id_type! {
    LocalId;
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
    /// The `let` or `var` that binds it alone (`let x = ...`), for fixes that change one to the
    /// other.
    pub keyword: Option<Span>,
    /// Bound by a struct pattern's field shorthand (`S { count }`): renaming it keeps the
    /// field's name (`count: _count`).
    pub shorthand: bool,
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
    /// Its value as an `f64` and as an `f32`, each rounded once from the literal.
    Float(f64, f32),
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
    /// A closure or function stored in a place (`(self.f)(p)`): a value of a type bounded by
    /// a function type (§6.7). The memory IR reads it into a local first ([`Callee::Local`]).
    Value(Box<Expr>),
    /// `.clone()`: structural.
    Clone,
    /// `f.start(...)`: a `Job<f>` holding the job's owned parameters (§6.18); `args` are the
    /// job's generics.
    JobStart {
        func: FnId,
        args: Vec<TyId>,
    },
    /// `job.resume(...)`: the job (`mut`), then the job's `borrow` and `mut` parameters.
    JobResume {
        func: FnId,
        args: Vec<TyId>,
    },
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
    /// A string literal: a `Text` holding this UTF-8, laid out in the build.
    Text(std::sync::Arc<str>),
    /// `embed("path")`: `Bytes` holding the file at this path in the package, which the build
    /// reads and lays out (§10).
    Embed(std::sync::Arc<str>),
    /// A `const N: u32` generic parameter's value (§4).
    ConstParam(ParamId),
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
    /// A `Packed` struct's field (§3): `width` bits of its word, from bit `shift`, a `u32`. A
    /// value, and an assignment's target (its word written again, the other bits kept): no
    /// place of its own to project.
    PackedField {
        base: Box<Expr>,
        shift: u32,
        width: u32,
    },
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
        /// Written as a struct literal, `S { .. }`, where a local named moves (§6.1).
        literal: bool,
    },
    /// A field of the enclosing struct literal that its `..base` supplies.
    FromBase,
    Tuple(Vec<Expr>),
    Array(Vec<Expr>),
    ArrayRepeat(Box<Expr>, u32),
    /// `[a, b, ..fill]`: the elements, then `fill`, evaluated once, to the length.
    ArrayFill(Vec<Expr>, Box<Expr>, u32),
    /// A vector or matrix built from components; the type says which.
    Construct(Vec<Expr>),
    /// A scalar conversion to this expression's type.
    Convert(Box<Expr>),
    /// The variant an enum place holds, by index: a `u32` (derived code only).
    Discriminant(Box<Expr>),
    Block(Block),
    If {
        cond: Box<Expr>,
        then: Block,
        else_: Option<Box<Expr>>,
    },
    /// `match`; `mutable` for `match mut`, whose bindings project the scrutinee mutably.
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<Arm>,
        mutable: bool,
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
    /// `yield`, in a job's body: its work stops here for this frame (§6.18).
    Yield,
    Dispatch(Box<Dispatch>),
    Draw(Box<Draw>),
    Error,
}

/// What a command records (§12): an entry point named where it's recorded, or a bound entry
/// point's value.
#[derive(Clone, Debug)]
pub enum Shader {
    /// `k` or `k.bind(...)`, with its generic arguments: its arguments are the command's.
    Named(FnId, Vec<TyId>),
    /// A value of a bound entry point's type (`let k = k.bind(...)`, or a parameter bounded by
    /// `Kernel`): the type names the entry point once it's known, and its fields hold the
    /// arguments.
    Value(Box<Expr>),
}

impl Shader {
    pub fn value(&self) -> Option<&Expr> {
        match self {
            Shader::Value(e) => Some(e),
            Shader::Named(..) => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Dispatch {
    pub kernel: Shader,
    /// The workgroup count: a `u32` (in x; y and z are 1), or a `(u32, u32, u32)`; or, when
    /// `indirect`, a `GpuBuffer<u32>` or `GpuSpan<u32>` whose first three elements hold it.
    pub groups: Box<Expr>,
    pub indirect: bool,
    /// `groups` is the domain (`over:`): a `u32`, a `(u32, u32)` or `(u32, u32, u32)`, or a
    /// texture, whose size it is. The workgroup counts cover it, and invocations past it do
    /// nothing (§12).
    pub over: bool,
    /// One per kernel parameter that isn't a builtin, as (parameter index, argument), in the
    /// order written; none for a bound kernel's value.
    pub args: Vec<(usize, Expr)>,
    /// How many of `args` are written before `groups`: it's evaluated after them.
    pub groups_at: usize,
}

/// One of `draw`'s own arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawCount {
    Vertices,
    Instances,
    Indirect,
    Indices,
}

/// A draw's render state, which its pipeline holds: build-time constants (§12).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RenderState {
    /// `std::gpu::Cull`'s variant.
    pub cull: wrela_abi::manifest::Cull,
    /// `std::gpu::DepthBias`: the constant, and the slope scale's and the clamp's f32 bits.
    pub bias_constant: i32,
    pub bias_slope: u32,
    pub bias_clamp: u32,
    /// `std::gpu::Depth`: how fragments' depths are tested, and whether they're written.
    pub depth: wrela_abi::manifest::DepthState,
}

#[derive(Clone, Debug)]
pub struct Draw {
    /// The open pass it draws into (§12): its type says the pass's targets.
    pub pass: Box<Expr>,
    pub vertex: Shader,
    pub fragment: Shader,
    pub vertices: Expr,
    pub instances: Expr,
    /// Instead of the counts: a `GpuBuffer<u32>` or `GpuSpan<u32>` holding the vertex count,
    /// the instance count, the first vertex and the first instance (or, with `indices`, the
    /// index count, the instance count, the first index, the base vertex and the first
    /// instance).
    pub indirect: Option<Box<Expr>>,
    /// An indexed draw's `u32` indices: a `GpuBuffer` or `GpuSpan` of `u32`s, or of structs of
    /// `u32` fields.
    pub indices: Option<Box<Expr>>,
    pub state: RenderState,
    /// Each shader's arguments, bound to it (§12), as (entry point: 0 the vertex shader, 1 the
    /// fragment shader; parameter index; argument), in the order written.
    pub args: Vec<(usize, usize, Expr)>,
    /// The counts in the order written, after the bound arguments; one not written (a
    /// default) isn't here.
    pub counts: Vec<DrawCount>,
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
    /// `let pat = init`; with `else_`, `let pat = init else { ... }`, whose block leaves the
    /// scope when the pattern doesn't match.
    Bind {
        pat: Pat,
        init: Expr,
        else_: Option<Block>,
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
    /// `for f in fields(self)` in a `@fieldwise` trait's method: `var` projects each field of
    /// `self` in turn, of the trait's field type (`trait.fieldwise-walk`). Only in the trait's
    /// own body: each type that declares the trait gets the loop unrolled over its fields.
    ForFields {
        var: LocalId,
        mutable: bool,
        /// `fields(self).rev()`: the last field first.
        reverse: bool,
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
    /// A string literal: matches a `str`, `Text` or `String` with the same text.
    Text(std::sync::Arc<str>),
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

/// A call that borrows each argument, evaluates them in order, and returns a value.
pub(crate) fn plain_call(
    callee: Callee,
    args: Vec<Expr>,
    receiver: bool,
    ty: TyId,
    span: Span,
) -> Expr {
    let n = args.len();
    let modes = vec![Mode::Borrow; n];
    let call =
        Call { callee, args, modes, order: (0..n).collect(), receiver, ret_mode: RetMode::Owned };
    Expr { ty, span, kind: ExprKind::Call(call) }
}

/// Code directly inside an expression: an expression, or a block.
pub enum Child<'a> {
    Expr(&'a Expr),
    Block(&'a Block),
}

impl Expr {
    /// Whether this expression names a place: a local, a field or element of one, or a
    /// projection returned from a call.
    pub fn is_place(&self) -> bool {
        match &self.place_root().kind {
            ExprKind::Local(_) | ExprKind::Const(_) => true,
            ExprKind::Call(c) => c.ret_mode != RetMode::Owned,
            _ => false,
        }
    }

    /// Whether this chooses between places (§6.3: `borrow x = if c { a } else { b }`): an `if`
    /// with an `else` each of whose branches ends in a place, or chooses again.
    pub fn chooses_places(&self) -> bool {
        fn ends_in_place(e: &Expr) -> bool {
            match &e.kind {
                ExprKind::Block(b) => b.tail.as_deref().is_some_and(ends_in_place),
                ExprKind::If { .. } => e.chooses_places(),
                ExprKind::MutArg(x) => x.is_place(),
                _ => e.is_place(),
            }
        }
        match &self.kind {
            ExprKind::If { then, else_: Some(else_), .. } => {
                then.tail.as_deref().is_some_and(ends_in_place) && ends_in_place(else_)
            }
            _ => false,
        }
    }

    /// The places this chooses between (`chooses_places`): each branch's.
    pub fn chosen_places(&self) -> Vec<&Expr> {
        let mut out = Vec::new();
        fn walk<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
            match &e.kind {
                ExprKind::Block(b) => b.tail.iter().for_each(|t| walk(t, out)),
                ExprKind::If { then, else_, .. } => {
                    then.tail.iter().for_each(|t| walk(t, out));
                    else_.iter().for_each(|x| walk(x, out));
                }
                ExprKind::MutArg(x) => out.push(x),
                _ => out.push(e),
            }
        }
        walk(self, &mut out);
        out
    }

    /// What a place expression is a part of: this expression without its fields, components
    /// and elements.
    pub fn place_root(&self) -> &Expr {
        let mut e = self;
        while let ExprKind::Field(b, _) | ExprKind::Swizzle(b, _) | ExprKind::Index(b, _) = &e.kind
        {
            e = b;
        }
        e
    }

    /// Calls `f` on each expression and block directly inside this one. (A closure's body is
    /// a body of its own, not inside the closure expression.)
    pub fn for_each_child<'a>(&'a self, f: &mut impl FnMut(Child<'a>)) {
        match &self.kind {
            ExprKind::Lit(_)
            | ExprKind::Text(_)
            | ExprKind::Embed(_)
            | ExprKind::ConstParam(_)
            | ExprKind::Local(_)
            | ExprKind::Const(_)
            | ExprKind::Closure(_)
            | ExprKind::FnRef(..)
            | ExprKind::Break
            | ExprKind::Continue
            | ExprKind::Yield
            | ExprKind::FromBase
            | ExprKind::Error => {}
            ExprKind::Unary(_, x)
            | ExprKind::Field(x, _)
            | ExprKind::Swizzle(x, _)
            | ExprKind::ArrayRepeat(x, _)
            | ExprKind::Convert(x)
            | ExprKind::Discriminant(x)
            | ExprKind::Take(x)
            | ExprKind::MutArg(x)
            | ExprKind::PackedField { base: x, .. } => f(Child::Expr(x)),
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => {
                f(Child::Expr(a));
                f(Child::Expr(b));
            }
            ExprKind::Call(c) => {
                if let Callee::Value(v) = &c.callee {
                    f(Child::Expr(v));
                }
                c.args.iter().for_each(|a| f(Child::Expr(a)))
            }
            ExprKind::Adt { fields, base, .. } => {
                fields.iter().for_each(|x| f(Child::Expr(x)));
                if let Some(b) = base {
                    f(Child::Expr(b));
                }
            }
            ExprKind::Tuple(xs) | ExprKind::Array(xs) | ExprKind::Construct(xs) => {
                xs.iter().for_each(|x| f(Child::Expr(x)))
            }
            ExprKind::ArrayFill(xs, fill, _) => {
                xs.iter().for_each(|x| f(Child::Expr(x)));
                f(Child::Expr(fill));
            }
            ExprKind::Block(b) => f(Child::Block(b)),
            ExprKind::If { cond, then, else_ } => {
                f(Child::Expr(cond));
                f(Child::Block(then));
                if let Some(e) = else_ {
                    f(Child::Expr(e));
                }
            }
            ExprKind::Match { scrutinee, arms, .. } => {
                f(Child::Expr(scrutinee));
                for a in arms {
                    if let Some(g) = &a.guard {
                        f(Child::Expr(g));
                    }
                    f(Child::Expr(&a.body));
                }
            }
            ExprKind::Return(v) => {
                if let Some(v) = v {
                    f(Child::Expr(v));
                }
            }
            ExprKind::Dispatch(d) => {
                d.kernel.value().into_iter().for_each(|v| f(Child::Expr(v)));
                f(Child::Expr(&d.groups));
                d.args.iter().for_each(|(_, a)| f(Child::Expr(a)));
            }
            ExprKind::Draw(d) => {
                f(Child::Expr(&d.pass));
                [&d.vertex, &d.fragment]
                    .into_iter()
                    .filter_map(Shader::value)
                    .for_each(|v| f(Child::Expr(v)));
                f(Child::Expr(&d.vertices));
                f(Child::Expr(&d.instances));
                d.indirect.iter().chain(&d.indices).for_each(|i| f(Child::Expr(i)));
                d.args.iter().for_each(|(_, _, a)| f(Child::Expr(a)));
            }
        }
    }
}
