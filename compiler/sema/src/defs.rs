//! Definitions: modules, items and their signatures, as the checker sees them.

use crate::builtins::{BuiltinFn, BuiltinTy};
use crate::ty::*;
use std::collections::BTreeMap;
use std::rc::Rc;
use wrela_diag::{FileId, Span};
use wrela_syntax::ast;

/// What a name refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Res {
    Module(ModuleId),
    Adt(AdtId),
    Trait(TraitId),
    Fn(FnId),
    Const(ConstId),
    /// An enum variant: the enum and the variant's index.
    Variant(AdtId, u32),
    BuiltinTy(BuiltinTy),
    BuiltinFn(BuiltinFn),
}

/// A name in a module's scope.
#[derive(Clone, Debug)]
pub struct Binding {
    pub res: Res,
    /// `pub`: visible outside the module.
    pub public: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Module {
    /// `["shapes", "blob"]`; std modules start with `"std"`.
    pub path: Vec<String>,
    pub file: Option<FileId>,
    pub children: BTreeMap<String, ModuleId>,
    pub scope: BTreeMap<String, Binding>,
    pub is_std: bool,
    pub ast: Option<Rc<ast::File>>,
}

impl Module {
    pub fn name(&self) -> String {
        self.path.join("::")
    }
}

/// A bound on a generic parameter, or a supertrait: `Trait<Args>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TraitRef {
    pub trait_: TraitId,
    pub args: Vec<TyId>,
}

#[derive(Clone, Debug)]
pub struct ParamDef {
    pub name: String,
    pub bounds: Vec<TraitRef>,
    pub span: Span,
    /// A trait's implicit `Self` parameter.
    pub is_self: bool,
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: TyId,
    pub public: bool,
    pub default: Option<ast::Expr>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VariantShape {
    Unit,
    Tuple,
    Struct,
}

#[derive(Clone, Debug)]
pub struct VariantDef {
    pub name: String,
    pub shape: VariantShape,
    /// Tuple variants' fields are named "0", "1", …
    pub fields: Vec<FieldDef>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum AdtKind {
    Struct(Vec<FieldDef>),
    Enum(Vec<VariantDef>),
}

/// Compiler-known std items, found by path when the std library loads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    Option,
    Copy,
    Clone,
    GpuData,
    GpuBuffer,
    Slots,
    GlobalId,
    LocalId,
    WorkgroupId,
    VertexIndex,
    InstanceIndex,
    FragCoord,
    ClipPosition,
    Flat,
    Interval,
    Domain,
    Dispatch,
    Draw,
    Gradient,
    ValueAndGradient,
    IntervalOf,
    Buffer,
    Write,
    BeginScreenPass,
    Present,
    CpuSin,
    CpuCos,
    CpuTan,
    CpuAsin,
    CpuAcos,
    CpuAtan,
    CpuAtan2,
    CpuExp,
    CpuExp2,
    CpuLog,
    CpuLog2,
    CpuPow,
}

impl Lang {
    /// Where each lang item lives in std.
    pub const PATHS: &'static [(Lang, &'static str)] = &[
        (Lang::Option, "std::prelude::Option"),
        (Lang::Copy, "std::prelude::Copy"),
        (Lang::Clone, "std::prelude::Clone"),
        (Lang::GpuData, "std::prelude::GpuData"),
        (Lang::GpuBuffer, "std::gpu::GpuBuffer"),
        (Lang::Slots, "std::gpu::Slots"),
        (Lang::GlobalId, "std::gpu::GlobalId"),
        (Lang::LocalId, "std::gpu::LocalId"),
        (Lang::WorkgroupId, "std::gpu::WorkgroupId"),
        (Lang::VertexIndex, "std::gpu::VertexIndex"),
        (Lang::InstanceIndex, "std::gpu::InstanceIndex"),
        (Lang::FragCoord, "std::gpu::FragCoord"),
        (Lang::ClipPosition, "std::gpu::ClipPosition"),
        (Lang::Flat, "std::gpu::Flat"),
        (Lang::Dispatch, "std::gpu::dispatch"),
        (Lang::Draw, "std::gpu::draw"),
        (Lang::Buffer, "std::gpu::buffer"),
        (Lang::Write, "std::gpu::write"),
        (Lang::BeginScreenPass, "std::gpu::begin_screen_pass"),
        (Lang::Present, "std::gpu::present"),
        (Lang::Interval, "std::derive::Interval"),
        (Lang::Domain, "std::derive::Domain"),
        (Lang::Gradient, "std::derive::gradient"),
        (Lang::ValueAndGradient, "std::derive::value_and_gradient"),
        (Lang::IntervalOf, "std::derive::interval"),
        (Lang::CpuSin, "std::math::sin"),
        (Lang::CpuCos, "std::math::cos"),
        (Lang::CpuTan, "std::math::tan"),
        (Lang::CpuAsin, "std::math::asin"),
        (Lang::CpuAcos, "std::math::acos"),
        (Lang::CpuAtan, "std::math::atan"),
        (Lang::CpuAtan2, "std::math::atan2"),
        (Lang::CpuExp, "std::math::exp"),
        (Lang::CpuExp2, "std::math::exp2"),
        (Lang::CpuLog, "std::math::log"),
        (Lang::CpuLog2, "std::math::log2"),
        (Lang::CpuPow, "std::math::pow"),
    ];
}

#[derive(Clone, Debug)]
pub struct AdtDef {
    pub name: String,
    pub module: ModuleId,
    pub generics: Vec<ParamId>,
    pub kind: AdtKind,
    /// Traits the declaration opts in to (`struct S: Copy + GpuData`), with their spans.
    pub opt_in: Vec<(TraitRef, Span)>,
    pub public: bool,
    pub span: Span,
    pub name_span: Span,
    pub lang: Option<Lang>,
}

impl AdtDef {
    pub fn is_enum(&self) -> bool {
        matches!(self.kind, AdtKind::Enum(_))
    }
    pub fn fields(&self) -> &[FieldDef] {
        match &self.kind {
            AdtKind::Struct(f) => f,
            AdtKind::Enum(_) => &[],
        }
    }
    pub fn variants(&self) -> &[VariantDef] {
        match &self.kind {
            AdtKind::Struct(_) => &[],
            AdtKind::Enum(v) => v,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AssocTypeDef {
    pub name: String,
    pub bounds: Vec<TraitRef>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TraitDef {
    pub name: String,
    pub module: ModuleId,
    /// The trait's own parameters, not counting `Self`.
    pub generics: Vec<ParamId>,
    pub self_param: ParamId,
    pub supertraits: Vec<TraitRef>,
    pub assoc_types: Vec<AssocTypeDef>,
    pub methods: Vec<FnId>,
    pub public: bool,
    pub span: Span,
    pub lang: Option<Lang>,
}

#[derive(Clone, Debug)]
pub struct ImplDef {
    pub module: ModuleId,
    pub generics: Vec<ParamId>,
    pub trait_ref: Option<TraitRef>,
    pub self_ty: TyId,
    pub assoc_types: BTreeMap<String, TyId>,
    pub methods: Vec<FnId>,
    pub span: Span,
    /// An impl implied by opting in to a trait in a declaration, rather than written out.
    pub from_opt_in: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FnOwner {
    Free,
    Impl(ImplId),
    Trait(TraitId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Borrow,
    Mut,
    Take,
}

impl From<ast::Mode> for Mode {
    fn from(m: ast::Mode) -> Mode {
        match m {
            ast::Mode::Borrow => Mode::Borrow,
            ast::Mode::Mut => Mode::Mut,
            ast::Mode::Take => Mode::Take,
        }
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RetMode {
    Owned,
    Borrow,
    Mut,
}

#[derive(Clone, Debug)]
pub struct ParamSig {
    pub name: String,
    pub mode: Mode,
    pub ty: TyId,
    pub default: Option<ast::Expr>,
    pub span: Span,
    /// The receiver, `self`.
    pub is_self: bool,
}

/// A GPU entry point (§12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Entry {
    Compute([u32; 3]),
    Vertex,
    Fragment,
}

#[derive(Clone, Debug, Default)]
pub struct FnAttrs {
    pub entry: Option<(Entry, Span)>,
    /// `@gpu`: asserted GPU-safe.
    pub gpu: Option<Span>,
    /// `@intrinsic` (std only): implemented by the compiler; the body is ignored.
    pub intrinsic: bool,
}

#[derive(Clone, Debug)]
pub struct FnDef {
    pub name: String,
    pub module: ModuleId,
    pub owner: FnOwner,
    /// Parameters declared on the function itself.
    pub generics: Vec<ParamId>,
    pub params: Vec<ParamSig>,
    pub ret: TyId,
    pub ret_mode: RetMode,
    /// For a return type that names traits: the traits. `ret` is then `Opaque(self, ...)`.
    pub opaque: Option<Vec<TraitRef>>,
    pub attrs: FnAttrs,
    pub body: Option<Rc<ast::Block>>,
    pub public: bool,
    pub span: Span,
    pub name_span: Span,
    pub sig_span: Span,
    pub lang: Option<Lang>,
}

impl FnDef {
    pub fn has_self(&self) -> bool {
        self.params.first().is_some_and(|p| p.is_self)
    }
}

#[derive(Clone, Debug)]
pub struct ConstDef {
    pub name: String,
    pub module: ModuleId,
    pub ty: Option<TyId>,
    pub value: ast::Expr,
    pub public: bool,
    pub span: Span,
}
