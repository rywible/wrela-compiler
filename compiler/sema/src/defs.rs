//! Definitions: modules, items and their signatures, as the checker sees them.

use crate::builtins::{BuiltinFn, BuiltinTy};
use crate::ty::*;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use wrela_diag::Span;
use wrela_syntax::ast;

pub use wrela_syntax::ast::{Mode, RetMode};

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
    pub children: BTreeMap<String, ModuleId>,
    pub scope: BTreeMap<String, Binding>,
    /// The names of items that failed to parse (and of imports of them). Their errors are
    /// reported, so uses of them aren't.
    pub broken: BTreeSet<String>,
    pub is_std: bool,
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

impl TraitRef {
    /// The same trait, with `s` applied to its arguments.
    pub fn subst(&self, types: &Types, s: &Subst) -> TraitRef {
        TraitRef {
            trait_: self.trait_,
            args: self.args.iter().map(|&a| types.subst(a, s)).collect(),
        }
    }
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

/// The compiler-known std items, one row each: the variant and where it lives in std.
macro_rules! lang_items {
    ($( $v:ident = $path:literal, )*) => {
        /// Compiler-known std items, found by path when the std library loads.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Lang {
            $( $v, )*
        }

        impl Lang {
            /// Where each lang item lives in std.
            pub const PATHS: &'static [(Lang, &'static str)] = &[$( (Lang::$v, $path), )*];
        }
    };
}

lang_items! {
    Option = "std::prelude::Option",
    Copy = "std::prelude::Copy",
    Clone = "std::prelude::Clone",
    GpuData = "std::prelude::GpuData",
    GpuBuffer = "std::gpu::GpuBuffer",
    Slots = "std::gpu::Slots",
    GlobalId = "std::gpu::GlobalId",
    LocalId = "std::gpu::LocalId",
    WorkgroupId = "std::gpu::WorkgroupId",
    VertexIndex = "std::gpu::VertexIndex",
    InstanceIndex = "std::gpu::InstanceIndex",
    FragCoord = "std::gpu::FragCoord",
    ClipPosition = "std::gpu::ClipPosition",
    Flat = "std::gpu::Flat",
    Dispatch = "std::gpu::dispatch",
    Draw = "std::gpu::draw",
    Buffer = "std::gpu::buffer",
    Write = "std::gpu::write",
    BeginScreenPass = "std::gpu::begin_screen_pass",
    Present = "std::gpu::present",
    Interval = "std::derive::Interval",
    Domain = "std::derive::Domain",
    Gradient = "std::derive::gradient",
    ValueAndGradient = "std::derive::value_and_gradient",
    IntervalOf = "std::derive::interval",
    CpuSin = "std::math::sin",
    CpuCos = "std::math::cos",
    CpuTan = "std::math::tan",
    CpuAsin = "std::math::asin",
    CpuAcos = "std::math::acos",
    CpuAtan = "std::math::atan",
    CpuAtan2 = "std::math::atan2",
    CpuExp = "std::math::exp",
    CpuExp2 = "std::math::exp2",
    CpuLog = "std::math::log",
    CpuLog2 = "std::math::log2",
    CpuPow = "std::math::pow",
}

impl Lang {
    /// `Copy`, `Clone` and `GpuData`: traits a type has through its fields, by opting in.
    pub fn is_structural(self) -> bool {
        matches!(self, Lang::Copy | Lang::Clone | Lang::GpuData)
    }
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
    /// Every field of a struct, or of every variant of an enum, in order.
    pub fn all_fields(&self) -> impl Iterator<Item = &FieldDef> {
        let variants = self.variants().iter().flat_map(|v| &v.fields);
        self.fields().iter().chain(variants)
    }
    /// [`AdtDef::all_fields`], to change.
    pub fn all_fields_mut(&mut self) -> impl Iterator<Item = &mut FieldDef> {
        let (fields, variants): (&mut [FieldDef], &mut [VariantDef]) = match &mut self.kind {
            AdtKind::Struct(f) => (f, &mut []),
            AdtKind::Enum(v) => (&mut [], v),
        };
        fields.iter_mut().chain(variants.iter_mut().flat_map(|v| &mut v.fields))
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

impl Entry {
    /// The stage, as diagnostics name it: "a `@vertex` shader".
    pub fn describe(self) -> &'static str {
        match self {
            Entry::Compute(_) => "a `@compute` kernel",
            Entry::Vertex => "a `@vertex` shader",
            Entry::Fragment => "a `@fragment` shader",
        }
    }
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
