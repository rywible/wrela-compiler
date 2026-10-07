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
    /// A type alias.
    Alias(AliasId),
    /// A trait set: a name for several traits.
    TraitSet(TraitSetId),
    BuiltinTy(BuiltinTy),
    BuiltinFn(BuiltinFn),
}

/// A name in a module's scope.
#[derive(Clone, Debug)]
pub struct Binding {
    pub res: Res,
    /// `pub`: visible outside the module.
    pub public: bool,
    /// `pub(package)`: visible only inside the module's package.
    pub package_only: bool,
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
    pub package: PackageId,
}

/// What a package is to the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageKind {
    /// The standard library: the one package with std's privileges (its core's `unsafe`,
    /// intrinsics, destructors, lang items).
    Std,
    /// The package being built: its `main.wrela` is the program's interface to the host.
    Program,
    /// A package the program depends on, by path.
    Dependency,
}

/// A package: a directory of modules, with a `wrela.toml` if it has dependencies or uses
/// `unsafe` (language.md §3).
#[derive(Clone, Debug)]
pub struct PackageDef {
    pub name: String,
    pub root: ModuleId,
    pub kind: PackageKind,
    /// Its dependencies, by the name its code uses for each.
    pub deps: BTreeMap<String, PackageId>,
    /// Its manifest declares that it uses `unsafe` (std's core does).
    pub unsafe_ok: bool,
    /// A lifted build lifts its literals (language.md §22).
    pub lifted: bool,
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
    /// `const N: u32`: a constant parameter, a `u32` (§4). Its argument is a
    /// [`TyKind::ConstU32`].
    pub is_const: bool,
    /// `F: fn(A) -> R`: its values are closures or functions called as that type, which can
    /// be stored (§6.7). A `TyKind::FnPtr`.
    pub fn_bound: Option<TyId>,
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: TyId,
    pub public: bool,
    /// `pub(package)`: visible only inside its type's package.
    pub package_only: bool,
    pub default: Option<ast::Expr>,
    pub span: Span,
    /// A borrow struct's projection field: `borrow T` or `mut T` (§6.6). `Owned` otherwise.
    pub mode: RetMode,
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
            /// Whether calling it records GPU work: the `host` effect (language.md §8).
            pub fn records_gpu_work(self) -> bool {
                matches!(
                    self,
                    Lang::Buffer
                        | Lang::Write
                        | Lang::DestroyBuffer
                        | Lang::CopyBuffer
                        | Lang::BeginScreenPass
                        | Lang::Present
                        | Lang::CreateTexture
                        | Lang::WriteTexture
                        | Lang::DestroyTexture
                        | Lang::CreateSampler
                        | Lang::DestroySampler
                        | Lang::BeginPass
                        | Lang::EndPass
                        | Lang::ReadBufferCommand
                )
            }

            /// Whether it's a request to the host for IO, or a line for its console (the `io`
            /// effect, §6.15).
            pub fn requests_io(self) -> bool {
                matches!(
                    self,
                    Lang::StorageReadCommand
                        | Lang::StorageWriteCommand
                        | Lang::FetchCommand
                        | Lang::PrintCommand
                        | Lang::PostCommand
                )
            }

            /// Whether what it gives depends on when the host answers (the `nondet` effect):
            /// polling a request, and reading the GPU back.
            pub fn is_nondet(self) -> bool {
                matches!(
                    self,
                    Lang::RequestStatus
                        | Lang::RequestTake
                        | Lang::ReadBufferCommand
                        | Lang::Limit
                        | Lang::InputTake
                )
            }

            /// Whether it's a kernel's `mut` parameter that every invocation may hold at once
            /// (§6.13): slots, workgroup memory, atomics, an append and a map.
            pub fn is_invocation_safe(self) -> bool {
                matches!(
                    self,
                    Lang::Slots | Lang::Shared | Lang::Atomics | Lang::Append | Lang::AtomicMap
                )
            }

            /// Whether it's a GPU resource other than a buffer: bound by its handle.
            pub fn is_texture_or_sampler(self) -> bool {
                matches!(
                    self,
                    Lang::Texture | Lang::DepthTexture | Lang::Sampler | Lang::ComparisonSampler
                )
            }

            /// Where each lang item lives in std.
            pub const PATHS: &'static [(Lang, &'static str)] = &[$( (Lang::$v, $path), )*];
        }
    };
}

lang_items! {
    Option = "std::prelude::Option",
    Result = "std::prelude::Result",
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
    Over = "std::gpu::Over",
    Cull = "std::gpu::Cull",
    DepthBias = "std::gpu::DepthBias",
    Dispatch = "std::gpu::dispatch",
    Draw = "std::gpu::draw",
    Buffer = "std::gpu::buffer",
    Write = "std::gpu::write_buffer",
    DestroyBuffer = "std::gpu::destroy_buffer",
    CopyBuffer = "std::gpu::copy_buffer",
    GpuSpan = "std::gpu::GpuSpan",
    GpuSpanMut = "std::gpu::GpuSpanMut",
    BeginScreenPass = "std::gpu::begin_screen_pass",
    Present = "std::gpu::present",
    Texture = "std::gpu::Texture",
    DepthTexture = "std::gpu::DepthTexture",
    Sampler = "std::gpu::Sampler",
    ComparisonSampler = "std::gpu::ComparisonSampler",
    CreateTexture = "std::gpu::create_texture",
    WriteTexture = "std::gpu::write_texture_rows",
    DestroyTexture = "std::gpu::destroy_texture",
    CreateSampler = "std::gpu::create_sampler",
    DestroySampler = "std::gpu::destroy_sampler",
    BeginPass = "std::gpu::begin_pass_command",
    EndPass = "std::gpu::end_pass_command",
    TextureSample = "std::gpu::texture_sample",
    TextureSampleLevel = "std::gpu::texture_sample_level",
    TextureSampleCompare = "std::gpu::texture_sample_compare",
    TextureSampleCompareLevel = "std::gpu::texture_sample_compare_level",
    TextureLoad = "std::gpu::texture_load",
    DepthLoad = "std::gpu::depth_load",
    TextureWidth = "std::gpu::texture_width",
    TextureHeight = "std::gpu::texture_height",
    ReadBufferCommand = "std::gpu::read_buffer_command",
    NextRequest = "std::io::next_request",
    RequestStatus = "std::io::request_status",
    RequestTake = "std::mem::take_answer",
    InputTake = "std::mem::take_input",
    StorageReadCommand = "std::io::storage_read_command",
    StorageWriteCommand = "std::io::storage_write_command",
    FetchCommand = "std::io::fetch_command",
    PrintCommand = "std::io::print_command",
    PostCommand = "std::io::post_command",
    Limit = "std::gpu::limit",
    Shared = "std::gpu::Shared",
    Atomics = "std::gpu::Atomics",
    Append = "std::gpu::Append",
    AtomicMap = "std::gpu::AtomicMap",
    AppendBuffer = "std::gpu::AppendBuffer",
    AtomicMapBuffer = "std::gpu::AtomicMapBuffer",
    LocalIndex = "std::gpu::local_index",
    WorkgroupInvocations = "std::gpu::workgroup_invocations",
    SharedGet = "std::gpu::shared_get",
    SharedSet = "std::gpu::shared_set",
    WorkgroupBarrier = "std::gpu::workgroup_barrier",
    AtomicLen = "std::gpu::atomic_len",
    AtomicLoad = "std::gpu::atomic_load",
    AtomicStore = "std::gpu::atomic_store",
    AtomicAdd = "std::gpu::atomic_add",
    AtomicSub = "std::gpu::atomic_sub",
    AtomicMin = "std::gpu::atomic_min",
    AtomicMax = "std::gpu::atomic_max",
    AtomicAnd = "std::gpu::atomic_and",
    AtomicOr = "std::gpu::atomic_or",
    AtomicXor = "std::gpu::atomic_xor",
    AtomicExchange = "std::gpu::atomic_exchange",
    AtomicCompareExchange = "std::gpu::atomic_compare_exchange",
    AppendPush = "std::gpu::append_push",
    Interval = "std::derive::Interval",
    Domain = "std::derive::Domain",
    Gradient = "std::derive::gradient",
    ValueAndGradient = "std::derive::value_and_gradient",
    ValueGradientWith = "std::derive::value_gradient_with",
    IntervalOf = "std::derive::interval",
    LiftGradient = "std::lift::gradient",
    LiftReads = "std::lift::reads",
    LiftCount = "std::lift::literal_count",
    LiftValue = "std::lift::literal_value",
    LiftBuiltValue = "std::lift::literal_built_value",
    LiftSet = "std::lift::set_literal",
    LiftSource = "std::lift::literal_source",
    LiftFiles = "std::lift::lifted_files",
    LiftFile = "std::lift::lifted_file",
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
    CpuSinh = "std::math::sinh",
    CpuCosh = "std::math::cosh",
    CpuTanh = "std::math::tanh",
    CpuPow = "std::math::pow",
    Drop = "std::mem::Drop",
    MemSizeOf = "std::mem::size_of",
    MemAlignOf = "std::mem::align_of",
    MemNeedsDrop = "std::mem::needs_drop",
    MemRead = "std::mem::read",
    MemZeroed = "std::mem::zeroed",
    MemWrite = "std::mem::write",
    MemDropAt = "std::mem::drop_at",
    MemAt = "std::mem::at",
    MemAtMut = "std::mem::at_mut",
    MemAtMutPair = "std::mem::at_mut_pair",
    MemHeapBase = "std::mem::heap_base",
    MemPages = "std::mem::memory_pages",
    MemGrow = "std::mem::memory_grow",
    MemLoadU32 = "std::mem::load_u32",
    MemStoreU32 = "std::mem::store_u32",
    MemLoadU8 = "std::mem::load_u8",
    MemStoreU8 = "std::mem::store_u8",
    MemCopy = "std::mem::copy",
    MemFill = "std::mem::fill",
    MemCas = "std::mem::compare_swap",
    MemAtomicAdd = "std::mem::atomic_add",
    MemAtomicLoad = "std::mem::atomic_load",
    MemAtomicStore = "std::mem::atomic_store",
    MemWait = "std::mem::wait",
    MemNotify = "std::mem::notify",
    MemRunTask = "std::mem::run_task",
    MemTask = "std::mem::task",
    MemThreadBlock = "std::mem::thread_block",
    MemWaitFor = "std::mem::wait_for",
    MemAbort = "std::mem::abort",
    DebugBuild = "std::mem::debug_build",
    ParEach = "std::par::par_each",
    ParEachChunk = "std::par::par_each_chunk",
    ParMapReduce = "std::par::par_map_reduce",
    ParMapReduceChunk = "std::par::par_map_reduce_chunk",
    RunChunks = "std::par::run_chunks",
    StartVoice = "std::audio::start_voice",
    StartTicker = "std::tick::start_ticker",
    Vec = "std::collections::Vec",
    Bounded = "std::collections::Bounded",
    Box = "std::collections::Box",
    Swap = "std::collections::swap",
    Replace = "std::collections::replace",
    Text = "std::string::Text",
    Bytes = "std::string::Bytes",
    String = "std::string::String",
    StrAddr = "std::string::str_addr",
    StrLen = "std::string::str_len",
    StrPart = "std::string::str_part",
    Eq = "std::cmp::Eq",
    Ord = "std::cmp::Ord",
    Ordering = "std::cmp::Ordering",
    Format = "std::fmt::Format",
    AssertFailed1 = "std::fmt::assert_failed1",
    AssertFailed2 = "std::fmt::assert_failed2",
    Arena = "std::arena::Arena",
    Handle = "std::arena::Handle",
    FormatSpec = "std::fmt::Spec",
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
    /// `@diagnostic("...")`: the message when it lacks a trait a use needs (§7).
    pub diagnostic: Option<String>,
    /// `borrow struct`: a named group of projections (§6.6).
    pub borrow: bool,
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
    /// `@fieldwise`: derived field by field for every type that declares it (§3).
    pub fieldwise: bool,
    /// `@diagnostic("...")`: the message when a type doesn't have it (§7).
    pub diagnostic: Option<String>,
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
    /// The function the build runs to compute a constant (language.md §10): no parameters,
    /// and the constant's type. Nothing names it.
    Const(ConstId),
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

/// The most frames a frame test runs (§10): ten minutes at 60 frames a second.
pub const MAX_TEST_FRAMES: u32 = 36_000;

#[derive(Clone, Debug, Default)]
pub struct FnAttrs {
    pub entry: Option<(Entry, Span)>,
    /// `@gpu`: asserted GPU-safe.
    pub gpu: Option<Span>,
    /// `@intrinsic` (std only): implemented by the compiler; the body is ignored.
    pub intrinsic: bool,
    /// Part of std's unsafe core (`std::mem`, `std::alloc`): callable only inside `unsafe`.
    pub unsafe_call: bool,
    /// `@deterministic`: checked to have no nondeterministic effects (§14).
    pub deterministic: Option<Span>,
    /// `@audio`: an audio-worklet entry point (§8).
    pub audio: Option<Span>,
    /// `@test`: a test `wrela test` runs, as the build runs constants (§10).
    pub test: Option<Span>,
    /// `@test(frames: n)` or `@test(ticks: n)`: what of the program the test runs first (§10).
    pub test_run: Option<TestRun>,
    /// `@test(frames: n, input: "script.json")`: a script of input events the frames get, by
    /// its path in the package, and where the path is written.
    pub test_input: Option<(String, Span)>,
    /// `@thread_entry` (std's unsafe core): a host calls it on a thread of its own, with that
    /// thread's number first (wrela_abi `memory`'s threads); exported as `__` and its name.
    pub thread_entry: Option<Span>,
    /// `@effects(...)` (std's unsafe core): effects it has that inference can't see, because
    /// another thread or the host decides them through memory (§8).
    pub effects: Vec<crate::effects::Effect>,
}

/// What of the program a test runs before it (§10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestRun {
    /// `@test(frames: n)`: `n` frames, with the program's ticks in lockstep.
    Frames(u32),
    /// `@test(ticks: n)`: `n` ticks, with no frames.
    Ticks(u32),
}

/// std's unsafe core (language.md §17, §6.14): the only modules that use `unsafe`, and the
/// only ones whose functions may state `@effects` or be a `@thread_entry`.
pub const UNSAFE_CORE: &[&str] =
    &["mem", "alloc", "collections", "string", "par", "audio", "tick", "handoff"];

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
    /// `pub(package)`: callable only inside its package.
    pub package_only: bool,
    pub span: Span,
    pub name_span: Span,
    pub sig_span: Span,
    pub lang: Option<Lang>,
    /// A method derived field by field for a type that declares a `@fieldwise` trait (§3): its
    /// body is built by `crate::fieldwise`, not written.
    pub derived: Option<DerivedFn>,
}

/// What a derived method derives: method `method` of the trait `trait_ref` names, for its
/// impl's type.
#[derive(Clone, Debug)]
pub struct DerivedFn {
    pub method: FnId,
    pub trait_ref: TraitRef,
}

impl FnDef {
    pub fn has_self(&self) -> bool {
        self.params.first().is_some_and(|p| p.is_self)
    }
}

/// `type Name<T> = Type`: another name for a type.
#[derive(Clone, Debug)]
pub struct AliasDef {
    pub name: String,
    pub module: ModuleId,
    pub generics: Vec<ParamId>,
    /// The type it names; the error type until it's resolved.
    pub ty: TyId,
    /// For an alias that names traits (`type GrazerField = Parts<Tissue>`, §4): the function
    /// whose result is the type it names, hidden behind those traits.
    pub defined_by: Option<FnId>,
    pub public: bool,
    pub span: Span,
}

/// `trait Sim<T> = A + B<T>`: wherever traits are listed, it stands for its traits (§7).
#[derive(Clone, Debug)]
pub struct TraitSetDef {
    pub name: String,
    pub module: ModuleId,
    pub generics: Vec<ParamId>,
    /// Its traits, in terms of `generics`, with any sets it names expanded; empty until
    /// they're resolved.
    pub traits: Vec<TraitRef>,
    pub public: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ConstDef {
    pub name: String,
    pub module: ModuleId,
    pub ty: Option<TyId>,
    pub value: ast::Expr,
    pub public: bool,
    pub span: Span,
    /// The function that computes it ([`FnOwner::Const`]).
    pub eval: FnId,
    /// Whether it's a parameter's or field's default (named by `name`), not a `const` item.
    pub default: bool,
}

impl ConstDef {
    /// How a diagnostic names it: "the constant `X`", or "the default of `x`".
    pub fn shown(&self) -> String {
        if self.default {
            format!("the default of `{}`", self.name)
        } else {
            format!("the constant `{}`", self.name)
        }
    }

    /// The same, shorter: "`X`", or "the default of `x`".
    pub fn short(&self) -> String {
        if self.default { self.shown() } else { format!("`{}`", self.name) }
    }
}
