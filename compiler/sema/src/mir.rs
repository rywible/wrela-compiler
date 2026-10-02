//! The mid-level IR: a function body as a control-flow graph of basic blocks over places, with
//! every move, loan and evaluation order explicit. It's built from the typed tree once per
//! function ([`build`]); the memory checker ([`crate::borrowck`]) runs dataflow over it, and
//! lowering turns it into IR per instance.
//!
//! - **Places** are a local and a path: fields, enum payloads, vector components, elements.
//!   An element's index is a local, evaluated before the place is used.
//! - **Operands** read a place (a copy), move out of one, or are a constant.
//! - **Projections** are locals that alias a place ([`StatementKind::Bind`]), or hold the place a
//!   projection-returning call returned. Lowering substitutes the place; the checker gives the
//!   local a loan on what it points into for as long as the local is live.
//! - **Control flow** is structured, as WGSL needs: an `If` names the block its branches join
//!   at, a `Loop` its continuing block and exit, a `Match` its arms and exit (SPIR-V's merge
//!   blocks). Dataflow follows the edges; lowering follows the structure.
//! - **Closures** are bodies of their own in the same [`Body`], sharing its locals: an outer
//!   local a closure body uses is a capture.

pub mod build;
pub mod print;

use crate::builtins::BuiltinFn;
use crate::defs::{Mode, RetMode};
use crate::thir::{self, Lit};
use crate::ty::*;
use wrela_diag::Span;
use wrela_syntax::ast::{BinOp, UnOp};

pub use crate::thir::LocalId as Local;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub u32);

impl BlockId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A function's MIR: its locals, and the code of the function and of each of its closures.
#[derive(Clone, Debug)]
pub struct Body {
    /// The typed tree's locals, at their own indices, then temporaries.
    pub locals: Vec<LocalDecl>,
    /// `fns[0]` is the function; `fns[1 + i]` is closure `i`.
    pub fns: Vec<FnBody>,
    /// Per closure: its captures (outer locals, and whether it writes through them).
    pub closures: Vec<ClosureInfo>,
    /// For a return type that names traits: the concrete type the body returns.
    pub hidden_ret: Option<TyId>,
}

impl Body {
    pub fn local(&self, l: Local) -> &LocalDecl {
        &self.locals[l.index()]
    }

    pub fn closure_fn(&self, c: ClosureId) -> &FnBody {
        &self.fns[1 + c.0 as usize]
    }
}

#[derive(Clone, Debug)]
pub struct ClosureInfo {
    pub params: Vec<Local>,
    pub ret: TyId,
    pub captures: Vec<(Local, bool)>,
    pub span: Span,
}

/// One function's (or closure's) code.
#[derive(Clone, Debug)]
pub struct FnBody {
    pub params: Vec<Local>,
    pub blocks: Vec<BlockData>,
    /// How the function returns its value (a closure's is always owned).
    pub ret_mode: RetMode,
    pub span: Span,
}

impl FnBody {
    pub const ENTRY: BlockId = BlockId(0);

    pub fn block(&self, b: BlockId) -> &BlockData {
        &self.blocks[b.index()]
    }
}

/// How a local holds its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalKind {
    /// A source local, as the checker declared it.
    User(thir::LocalKind),
    /// An expression's value, assigned once before any use, in a block that encloses them.
    Temp,
    /// A value assigned on several paths: the result of an `if`, a `match` or `&&`.
    TempVar,
    /// The place a projection-returning call returned (writable if it returned `mut`), or the
    /// array a `for` loop walks.
    TempProjection { mutable: bool },
    /// A call's `borrow` or `mut` argument, bound where it's evaluated and used by the call:
    /// so nothing evaluated in between (a later argument) can change it.
    Arg { mutable: bool },
}

impl LocalKind {
    /// Whether the local aliases a place rather than holding a value.
    pub fn is_projection(self) -> bool {
        matches!(
            self,
            LocalKind::User(thir::LocalKind::Projection { .. })
                | LocalKind::TempProjection { .. }
                | LocalKind::Arg { .. }
        )
    }

    /// Whether a place rooted at the local may be written (or lent `mut`). A parameter's mode
    /// decides; a capture is the closure's business.
    pub fn writable(self) -> bool {
        match self {
            LocalKind::User(k) => match k {
                thir::LocalKind::Owned { mutable } | thir::LocalKind::Projection { mutable } => {
                    mutable
                }
                thir::LocalKind::Param(m) => m == Mode::Mut,
                thir::LocalKind::ClosureParam => false,
            },
            LocalKind::Temp | LocalKind::TempVar => true,
            LocalKind::TempProjection { mutable } | LocalKind::Arg { mutable } => mutable,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LocalDecl {
    /// The source name, or a description of the temporary for diagnostics.
    pub name: String,
    pub ty: TyId,
    pub kind: LocalKind,
    pub span: Span,
    /// The `let` that binds it alone, for fixes that change it to `var`.
    pub keyword: Option<Span>,
    /// The closure whose body declares it.
    pub closure: Option<ClosureId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Proj {
    /// A struct or tuple field, by index.
    Field(u32),
    /// An enum value's payload for this variant.
    Downcast(u32),
    /// One component of a vector.
    Comp(u8),
    /// Several components of a vector, in order: the last projection of a place.
    Swizzle(Vec<u8>),
    /// An element of an array, run, matrix, vector or `Slots`, by the index in this local.
    Index(Local),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Place {
    pub local: Local,
    pub proj: Vec<Proj>,
}

impl Place {
    pub fn local(l: Local) -> Place {
        Place { local: l, proj: Vec::new() }
    }

    pub fn with(&self, p: Proj) -> Place {
        let mut proj = self.proj.clone();
        proj.push(p);
        Place { local: self.local, proj }
    }
}

/// How a move came to be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveKind {
    /// `take place`.
    Take,
    /// A function returning one of its owned locals whole: implicitly moved.
    Return,
    /// A use of a non-`Copy` place that needs `take` (or `.clone()`): an error, checked as a
    /// read.
    Unmarked,
}

#[derive(Clone, Debug)]
pub enum OperandKind {
    Copy(Place),
    Move(Place, MoveKind),
    Const(Lit),
}

#[derive(Clone, Debug)]
pub struct Operand {
    pub kind: OperandKind,
    pub ty: TyId,
    pub span: Span,
}

impl Operand {
    pub fn place(&self) -> Option<&Place> {
        match &self.kind {
            OperandKind::Copy(p) | OperandKind::Move(p, _) => Some(p),
            OperandKind::Const(_) => None,
        }
    }
}

/// A call's argument, by its parameter's mode.
#[derive(Clone, Debug)]
pub enum Arg {
    /// `borrow`: the callee reads the place for the call.
    Borrow(Place, Span),
    /// `mut`: the callee may write the place.
    Mut(Place, Span),
    /// `take`: the value is moved (or copied) in.
    Take(Operand),
}

impl Arg {
    pub fn span(&self) -> Span {
        match self {
            Arg::Borrow(_, s) | Arg::Mut(_, s) => *s,
            Arg::Take(o) => o.span,
        }
    }

    pub fn place(&self) -> Option<&Place> {
        match self {
            Arg::Borrow(p, _) | Arg::Mut(p, _) => Some(p),
            Arg::Take(o) => o.place(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Callee {
    Fn {
        func: FnId,
        args: Vec<TyId>,
    },
    TraitMethod {
        method: FnId,
        self_ty: TyId,
        trait_args: Vec<TyId>,
        method_args: Vec<TyId>,
    },
    Builtin(BuiltinFn),
    /// A closure or function held by a local (a closure binding or a `fn(..)` parameter).
    Local(Local),
    /// `.clone()`: a structural copy of the argument.
    Clone,
}

#[derive(Clone, Debug)]
pub struct Call {
    pub callee: Callee,
    /// In parameter order.
    pub args: Vec<Arg>,
    /// Whether the first argument is a method receiver.
    pub receiver: bool,
    /// A call returning `borrow T` or `mut T` returns a place.
    pub ret_mode: RetMode,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Dispatch {
    pub kernel: FnId,
    pub kernel_args: Vec<TyId>,
    pub groups: [Operand; 3],
    /// One per kernel parameter that isn't a builtin, in parameter order: borrowed.
    pub args: Vec<(usize, Place, Span)>,
}

#[derive(Clone, Debug)]
pub struct Draw {
    pub vertex: (FnId, Vec<TyId>),
    pub fragment: (FnId, Vec<TyId>),
    pub vertices: Operand,
    pub instances: Operand,
    pub args: Vec<(String, Place, Span)>,
}

#[derive(Clone, Debug)]
pub enum Rvalue {
    Use(Operand),
    Unary(UnOp, Operand),
    /// Operands may be a vector and a scalar, or a matrix and a vector. `&&` and `||` here
    /// evaluate both operands (the builder uses them only to combine a pattern's tests); in
    /// source they're control flow.
    Binary(BinOp, Operand, Operand),
    /// A struct (`variant: None`) or variant; fields complete, in declaration order.
    Adt {
        adt: AdtId,
        args: Vec<TyId>,
        variant: Option<u32>,
        fields: Vec<Operand>,
    },
    Tuple(Vec<Operand>),
    Array(Vec<Operand>),
    ArrayRepeat(Operand, u32),
    /// A vector or matrix from components; the destination's type says which.
    Construct(Vec<Operand>),
    /// A scalar conversion to the destination's type.
    Convert(Operand),
    /// An enum value's variant number.
    Discriminant(Place),
    /// A run's or array's length.
    Len(Place),
    Call(Call),
    /// A closure value: its captures are borrowed while the local holding it is live.
    Closure(ClosureId),
    /// A named function as a value.
    FnRef(FnId, Vec<TyId>),
    Dispatch(Box<Dispatch>),
    Draw(Box<Draw>),
}

#[derive(Clone, Debug)]
pub enum StatementKind {
    /// `place = value`. A projection local assigned a projection-returning call holds the place
    /// it returned.
    Assign(Place, Rvalue),
    /// `value` for its effects.
    Eval(Rvalue),
    /// A projection local starts aliasing `place` (`let x = w.a`, `mut x = w.a`, a pattern's
    /// binding into a place, a `for` loop's element).
    Bind { local: Local, place: Place, mutable: bool },
    /// A local's scope begins: a new value each time (a loop's next pass rebinds it).
    Live(Local),
    /// A local's scope ends.
    Dead(Local),
}

#[derive(Clone, Debug)]
pub struct Statement {
    pub kind: StatementKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum TerminatorKind {
    Goto(BlockId),
    /// Both branches end at `merge` (or leave: `return`, `break`, ...). A merge block nothing
    /// reaches is never entered (both branches left).
    If {
        cond: Operand,
        then: BlockId,
        else_: BlockId,
        merge: BlockId,
    },
    /// The loop's body starts at `body`; falling off its end or `continue` goes to
    /// `continuing`, which ends with [`TerminatorKind::LoopBack`]; `break` goes to `merge`.
    Loop {
        body: BlockId,
        continuing: BlockId,
        merge: BlockId,
    },
    /// The end of a loop's continuing block: back to its body. Names the `Loop` block.
    LoopBack(BlockId),
    /// To the innermost loop's exit (or its continuing block); names the `Loop` block.
    Break(BlockId),
    Continue(BlockId),
    /// The arms are tried in order: each starts at its block and ends in
    /// [`TerminatorKind::ArmMatched`] (its body ran; on to `merge`) or
    /// [`TerminatorKind::ArmFailed`] (on to the next arm).
    Match {
        arms: Vec<BlockId>,
        merge: BlockId,
    },
    /// Names the `Match` block.
    ArmMatched(BlockId),
    /// Names the `Match` block, and where control goes: the next arm's block, or the merge
    /// block after the last arm (unreachable when the match is exhaustive).
    ArmFailed {
        match_: BlockId,
        next: BlockId,
    },
    Return(Option<Operand>),
    /// A projection-returning function returns this place.
    ReturnPlace(Place),
    /// Unreachable: after an expression of type `!`, or no arm matched.
    Unreachable,
}

#[derive(Clone, Debug)]
pub struct Terminator {
    pub kind: TerminatorKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct BlockData {
    pub stmts: Vec<Statement>,
    pub term: Terminator,
}

impl FnBody {
    /// The blocks control can go to from `b`'s end.
    pub fn successors(&self, b: BlockId) -> Vec<BlockId> {
        match &self.block(b).term.kind {
            TerminatorKind::Goto(t) => vec![*t],
            TerminatorKind::If { then, else_, .. } => vec![*then, *else_],
            TerminatorKind::Loop { body, .. } => vec![*body],
            TerminatorKind::LoopBack(l) => match &self.block(*l).term.kind {
                TerminatorKind::Loop { body, .. } => vec![*body],
                _ => Vec::new(),
            },
            TerminatorKind::Break(l) => match &self.block(*l).term.kind {
                TerminatorKind::Loop { merge, .. } => vec![*merge],
                _ => Vec::new(),
            },
            TerminatorKind::Continue(l) => match &self.block(*l).term.kind {
                TerminatorKind::Loop { continuing, .. } => vec![*continuing],
                _ => Vec::new(),
            },
            TerminatorKind::Match { arms, merge } => vec![*arms.first().unwrap_or(merge)],
            TerminatorKind::ArmMatched(m) => match &self.block(*m).term.kind {
                TerminatorKind::Match { merge, .. } => vec![*merge],
                _ => Vec::new(),
            },
            TerminatorKind::ArmFailed { next, .. } => vec![*next],
            TerminatorKind::Return(_)
            | TerminatorKind::ReturnPlace(_)
            | TerminatorKind::Unreachable => Vec::new(),
        }
    }

    /// Whether `b` is the back edge's source: a `LoopBack`.
    pub fn is_back_edge(&self, b: BlockId) -> bool {
        matches!(self.block(b).term.kind, TerminatorKind::LoopBack(_))
    }
}

/// The type of a place, from its local's type through each projection. Types may mention the
/// function's generic parameters.
pub fn place_ty(p: &crate::program::Program, locals: &[LocalDecl], place: &Place) -> TyId {
    let mut t = locals[place.local.index()].ty;
    let mut variant: Option<u32> = None;
    for proj in &place.proj {
        let k = p.types.kind(t).clone();
        t = match (proj, k) {
            (Proj::Downcast(v), _) => {
                variant = Some(*v);
                continue;
            }
            (Proj::Field(i), TyKind::Adt(a, args)) => {
                let fields = match variant.take() {
                    Some(v) => p.variant_fields(a, &args, v as usize),
                    None => p.struct_fields(a, &args),
                };
                fields.get(*i as usize).map_or(p.types.error, |f| f.1)
            }
            (Proj::Field(i), TyKind::Tuple(ts)) => {
                ts.get(*i as usize).copied().unwrap_or(p.types.error)
            }
            (Proj::Comp(_), _) => p.types.f32,
            (Proj::Swizzle(cs), _) => p.types.vec(cs.len() as u8),
            (Proj::Index(_), TyKind::Array(e, _) | TyKind::Slice(e)) => e,
            (Proj::Index(_), TyKind::Vec(_)) => p.types.f32,
            (Proj::Index(_), TyKind::Mat(n)) => p.types.vec(n),
            (Proj::Index(_), TyKind::Adt(_, args)) if !args.is_empty() => args[0], // `Slots<T>`
            _ => p.types.error,
        };
    }
    t
}

/// A place as the source would write it, for diagnostics: `w.log.count`, `xs[_]`, `v.x`.
pub fn describe(p: &crate::program::Program, locals: &[LocalDecl], place: &Place) -> String {
    let mut s = locals[place.local.index()].name.clone();
    let mut sub = Place::local(place.local);
    for proj in &place.proj {
        match proj {
            Proj::Field(i) => {
                let base = place_ty(p, locals, &sub);
                let name = match p.types.kind(base).clone() {
                    TyKind::Adt(a, args) => {
                        let variant = sub.proj.iter().rev().find_map(|x| match x {
                            Proj::Downcast(v) => Some(*v),
                            _ => None,
                        });
                        let fields = match variant {
                            Some(v) if matches!(sub.proj.last(), Some(Proj::Downcast(_))) => {
                                p.variant_fields(a, &args, v as usize)
                            }
                            _ => p.struct_fields(a, &args),
                        };
                        fields.get(*i as usize).map_or_else(|| i.to_string(), |f| f.0.clone())
                    }
                    _ => i.to_string(),
                };
                s.push('.');
                s.push_str(&name);
            }
            Proj::Downcast(_) => {}
            Proj::Comp(c) => {
                s.push('.');
                s.push(['x', 'y', 'z', 'w'][*c as usize % 4]);
            }
            Proj::Swizzle(cs) => {
                s.push('.');
                for c in cs {
                    s.push(['x', 'y', 'z', 'w'][*c as usize % 4]);
                }
            }
            Proj::Index(_) => s.push_str("[_]"),
        }
        sub = sub.with(proj.clone());
    }
    s
}
