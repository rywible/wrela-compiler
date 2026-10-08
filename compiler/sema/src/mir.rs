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

use crate::builtins::BuiltinFn;
use crate::defs::{Mode, RetMode};
use crate::program::Program;
use crate::thir::{self, Lit};
use crate::ty::*;
use wrela_diag::Span;
use wrela_syntax::ast::{BinOp, UnOp};

pub use crate::thir::Callee;
pub use crate::thir::LocalId as Local;

id_type! {
    BlockId;
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
    /// Calls a statement makes only to throw their value away: the callee, whether any
    /// argument is lent `mut`, the value's type, and where (the effects pass reports those
    /// that change nothing, E0333).
    pub discarded: Vec<(Callee, bool, TyId, Span)>,
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
    /// The place a projection-returning call returned (writable if it returned `mut`), a
    /// `match`'s scrutinee, or the array a `for` loop walks.
    TempProjection { mutable: bool, role: TempRole },
    /// A call's `borrow` or `mut` argument, bound where it's evaluated and used by the call:
    /// so nothing evaluated in between (a later argument) can change it.
    Arg { mutable: bool },
    /// A constant, a place that lives as long as the program (language.md §10): read and
    /// projected, never written or moved out of.
    Const(ConstId),
}

/// What a [`LocalKind::TempProjection`] holds, as diagnostics name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempRole {
    /// The place a named function's call returned: the local is named `f(..)`.
    CallResult,
    /// A `match`'s scrutinee, borrowed while the arms test it.
    Match,
    /// Anything else: another call's result, or the array a `for` loop walks.
    Other,
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

    /// Whether the local owns its value outright, so a move out of it is allowed (unless it's a
    /// closure's capture).
    pub fn owns_value(self) -> bool {
        matches!(
            self,
            LocalKind::User(thir::LocalKind::Owned { .. } | thir::LocalKind::Param(Mode::Take))
                | LocalKind::Temp
                | LocalKind::TempVar
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
            LocalKind::TempProjection { mutable, .. } | LocalKind::Arg { mutable } => mutable,
            LocalKind::Const(_) => false,
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
    /// The `let` or `var` that binds it alone, for fixes that change one to the other.
    pub keyword: Option<Span>,
    /// Bound by a struct pattern's field shorthand (see `thir::LocalDecl`).
    pub shorthand: bool,
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
    /// A temporary's value (or part of it) passed on to what consumes it: its new owner.
    Temp,
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

/// What a command records (§12): an entry point named where it's recorded, its arguments the
/// command's, or a bound entry point's value, borrowed, whose type names it once it's known
/// (`gpu::bound_fields` says which parameter each field binds).
#[derive(Clone, Debug)]
pub enum Shader {
    Named(FnId, Vec<TyId>),
    Value(Place, Span),
}

impl Shader {
    pub fn value(&self) -> Option<&Place> {
        match self {
            Shader::Value(p, _) => Some(p),
            Shader::Named(..) => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Dispatch {
    pub kernel: Shader,
    /// The workgroup counts; or, `over` a domain, its size (lowering covers it with groups).
    pub groups: [Operand; 3],
    pub over: bool,
    /// One per kernel parameter that isn't a builtin, in parameter order: borrowed. None for
    /// a bound kernel's value.
    pub args: Vec<(usize, Place, Span)>,
    /// The buffer (or span) holding the group counts, in place of `groups`: borrowed.
    pub indirect: Option<(Place, Span)>,
}

#[derive(Clone, Debug)]
pub struct Draw {
    pub vertex: Shader,
    pub fragment: Shader,
    pub vertices: Operand,
    pub instances: Operand,
    /// (entry point: 0 the vertex shader, 1 the fragment shader; parameter index; argument).
    pub args: Vec<(usize, usize, Place, Span)>,
    /// The buffer (or span) holding the counts, in place of `vertices` and `instances`.
    pub indirect: Option<(Place, Span)>,
    /// An indexed draw's indices (with `indirect`).
    pub indices: Option<(Place, Span)>,
    pub state: crate::thir::RenderState,
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
    /// A large constant's value, kept in one place rather than built at each use (a table):
    /// a local assigned it is a read-only view of it.
    Const(ConstId),
    /// A string literal: a `Text` of this UTF-8, laid out in the build.
    Text(std::sync::Arc<str>),
    /// `embed("path")`: `Bytes` of the file at this path in the package (§10).
    Embed(std::sync::Arc<str>),
    /// A `const N: u32` generic parameter's value: known once the function is instantiated.
    ConstParam(ParamId),
    /// A borrow struct (§6.6): each field in declaration order, a place it projects (`Borrow`
    /// or `Mut`; a run or another borrow struct is `Borrow`) or a `Copy` value (`Take`). The
    /// local it's assigned to holds a loan on each field's place.
    BorrowStruct {
        adt: AdtId,
        args: Vec<TyId>,
        fields: Vec<Arg>,
    },
    Dispatch(Box<Dispatch>),
    Draw(Box<Draw>),
}

impl Rvalue {
    /// Calls `f` on each operand, in order: a call's and a borrow struct's `take` arguments
    /// among them, not the places they lend.
    pub fn for_each_operand(&self, f: &mut impl FnMut(&Operand)) {
        match self {
            Rvalue::Use(o)
            | Rvalue::Unary(_, o)
            | Rvalue::ArrayRepeat(o, _)
            | Rvalue::Convert(o) => f(o),
            Rvalue::Binary(_, a, b) => {
                f(a);
                f(b);
            }
            Rvalue::Adt { fields: xs, .. }
            | Rvalue::Tuple(xs)
            | Rvalue::Array(xs)
            | Rvalue::Construct(xs) => xs.iter().for_each(f),
            Rvalue::Call(Call { args, .. }) | Rvalue::BorrowStruct { fields: args, .. } => {
                for a in args {
                    if let Arg::Take(o) = a {
                        f(o);
                    }
                }
            }
            Rvalue::Dispatch(d) => d.groups.iter().for_each(f),
            Rvalue::Draw(d) => {
                f(&d.vertices);
                f(&d.instances);
            }
            Rvalue::Discriminant(_)
            | Rvalue::Len(_)
            | Rvalue::Closure(_)
            | Rvalue::FnRef(..)
            | Rvalue::Const(_)
            | Rvalue::Text(_)
            | Rvalue::Embed(_)
            | Rvalue::ConstParam(_) => {}
        }
    }
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
    /// `place` is evaluated but maybe never accessed (a place on its own, a `_` pattern, a
    /// projection not used yet): its indices are checked here, as an index out of range traps
    /// wherever it's written (§11). A read, for the memory model.
    Check(Place),
    /// A local's scope begins: a new value each time (a loop's next pass rebinds it).
    Live(Local),
    /// A local's scope ends.
    Dead(Local),
    /// Drops the value at `place`, which its owner is done with: runs its destructors, then
    /// leaves zeros there. A value moved out is zeros already, so dropping it does nothing
    /// (language.md §6.1).
    Drop(Place),
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
    /// The blocks control can go to from `b`'s end (at most two).
    pub fn successors(&self, b: BlockId) -> impl Iterator<Item = BlockId> {
        let one = |t: BlockId| [Some(t), None];
        let succ = match &self.block(b).term.kind {
            TerminatorKind::Goto(t) => one(*t),
            TerminatorKind::If { then, else_, .. } => [Some(*then), Some(*else_)],
            TerminatorKind::Loop { body, .. } => one(*body),
            TerminatorKind::LoopBack(l) => match &self.block(*l).term.kind {
                TerminatorKind::Loop { body, .. } => one(*body),
                _ => [None, None],
            },
            TerminatorKind::Break(l) => match &self.block(*l).term.kind {
                TerminatorKind::Loop { merge, .. } => one(*merge),
                _ => [None, None],
            },
            TerminatorKind::Continue(l) => match &self.block(*l).term.kind {
                TerminatorKind::Loop { continuing, .. } => one(*continuing),
                _ => [None, None],
            },
            TerminatorKind::Match { arms, merge } => one(*arms.first().unwrap_or(merge)),
            TerminatorKind::ArmMatched(m) => match &self.block(*m).term.kind {
                TerminatorKind::Match { merge, .. } => one(*merge),
                _ => [None, None],
            },
            TerminatorKind::ArmFailed { next, .. } => one(*next),
            TerminatorKind::Return(_)
            | TerminatorKind::ReturnPlace(_)
            | TerminatorKind::Unreachable => [None, None],
        };
        succ.into_iter().flatten()
    }

    /// Whether `b` is the back edge's source: a `LoopBack`.
    pub fn is_back_edge(&self, b: BlockId) -> bool {
        matches!(self.block(b).term.kind, TerminatorKind::LoopBack(_))
    }
}

/// Whether `l` is outside the body of `closure` (a capture). Nothing is outside the function's
/// own body (`closure` is `None`).
pub(crate) fn is_capture(locals: &[LocalDecl], closure: Option<ClosureId>, l: Local) -> bool {
    // A closure's parameters and the locals its body declares are its own.
    closure.is_some_and(|c| locals[l.index()].closure != Some(c))
}

/// Whether a value of type `t` can be called: a closure, or a function.
pub fn is_callable(types: &Types, t: TyId) -> bool {
    matches!(types.kind(t), TyKind::Closure(..) | TyKind::FnPtr(..) | TyKind::FnDef(..))
}

/// The type of a place, from its local's type through each projection. Types may mention the
/// function's generic parameters.
pub fn place_ty(p: &Program, locals: &[LocalDecl], place: &Place) -> TyId {
    let mut t = locals[place.local.index()].ty;
    let mut variant: Option<u32> = None;
    for proj in &place.proj {
        t = proj_ty(p, t, &mut variant, proj);
    }
    t
}

/// The type of projection `proj` of a place of type `t`. `variant` is the variant a `Downcast`
/// chose, for the `Field` after it.
pub fn proj_ty(p: &Program, t: TyId, variant: &mut Option<u32>, proj: &Proj) -> TyId {
    match (proj, p.types.kind(t)) {
        (Proj::Downcast(v), _) => {
            *variant = Some(*v);
            t
        }
        (Proj::Field(i), TyKind::Adt(a, args)) => {
            p.field_ty(*a, args, variant.take(), *i as usize).unwrap_or(p.types.error)
        }
        (Proj::Field(i), TyKind::Tuple(ts)) => {
            ts.get(*i as usize).copied().unwrap_or(p.types.error)
        }
        (Proj::Comp(_) | Proj::Index(_), &TyKind::Vec(e, _)) => p.types.elem(e),
        (Proj::Comp(_), _) => p.types.f32,
        (Proj::Swizzle(cs), &TyKind::Vec(e, _)) => p.types.vec_of(e, cs.len() as u8),
        (Proj::Swizzle(cs), _) => p.types.vec(cs.len() as u8),
        (Proj::Index(_), TyKind::Array(e, _) | TyKind::ArrayN(e, _) | TyKind::Slice(e)) => *e,
        (Proj::Index(_), TyKind::Mat(n)) => p.types.vec(*n),
        // A lang container's element (`Slots`, `Arena`, `Bounded`, `Vec`): its first type
        // argument.
        (Proj::Index(_), TyKind::Adt(_, args)) if !args.is_empty() => args[0],
        _ => p.types.error,
    }
}

/// A place as the source would write it, for diagnostics: `w.log.count`, `xs[_]`, `v.x`.
pub fn describe(p: &Program, locals: &[LocalDecl], place: &Place) -> String {
    let mut s = locals[place.local.index()].name.clone();
    let mut t = locals[place.local.index()].ty;
    let mut variant: Option<u32> = None;
    for proj in &place.proj {
        match proj {
            Proj::Field(i) => {
                s.push('.');
                s.push_str(&field_name(p, t, variant, *i));
            }
            Proj::Downcast(_) => {}
            Proj::Comp(c) => {
                s.push('.');
                s.push(comp_char(*c));
            }
            Proj::Swizzle(cs) => {
                s.push('.');
                s.extend(cs.iter().map(|&c| comp_char(c)));
            }
            Proj::Index(_) => s.push_str("[_]"),
        }
        t = proj_ty(p, t, &mut variant, proj);
    }
    s
}

/// The name of field `i` of a value of type `t` (of `variant`, for an enum): its index when it
/// has no name (a tuple's).
pub(crate) fn field_name(p: &Program, t: TyId, variant: Option<u32>, i: u32) -> String {
    match p.types.kind(t) {
        TyKind::Adt(a, _) => p
            .adt_fields(*a, variant)
            .get(i as usize)
            .map_or_else(|| i.to_string(), |f| f.name.clone()),
        _ => i.to_string(),
    }
}

/// A vector component's name: `x`, `y`, `z` or `w`.
pub(crate) fn comp_char(c: u8) -> char {
    ['x', 'y', 'z', 'w'][c as usize % 4]
}
