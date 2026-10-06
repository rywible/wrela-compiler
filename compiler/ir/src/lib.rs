//! wrela's mid-level IR.
//!
//! A [`Module`] holds types, functions and (for GPU code) resources. A function body is
//! structured control flow over **values** (each defined once, by a [`Stmt::Let`], usable where
//! its definition dominates) and **locals** (addressable variables, read with [`Expr::Load`] and
//! written with [`Stmt::Store`]). That's the shape both back ends want: naga's expressions and
//! local variables, and WASM's structured blocks. Merges go through locals, so there are no
//! phis.
//!
//! The IR knows nothing about the language's traits, generics or modes: lowering has
//! monomorphized and resolved all of it. It knows the two targets only where they differ:
//! integer arithmetic is checked (traps) on the CPU and wraps on the GPU, only GPU code has
//! resources and entry points, and only CPU code has constant data.

pub mod bounds;
pub mod derive;
mod error;
pub mod layout;
pub mod opt;
pub mod print;
mod single_exit;
mod types;
pub mod uniformity;
mod verify;
pub mod visit;
pub mod workgroup;

pub use error::{Error, Result};
pub use types::*;
pub use verify::verify;

use std::fmt;

macro_rules! ids {
    ($($name:ident),*) => {$(
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u32);
        impl $name {
            pub fn index(self) -> usize { self.0 as usize }
        }
    )*};
}

ids!(FuncId, ValueId, LocalId, ResourceId, DataId, TypeId);

/// Which processor a module's code runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Cpu,
    Gpu,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    Bool(bool),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    /// 8- and 16-bit integers, held widened.
    Small(Scalar, i64),
    F32(f32),
    F64(f64),
}

impl Const {
    pub fn scalar(&self) -> Scalar {
        match self {
            Const::Bool(_) => Scalar::Bool,
            Const::I32(_) => Scalar::I32,
            Const::U32(_) => Scalar::U32,
            Const::I64(_) => Scalar::I64,
            Const::U64(_) => Scalar::U64,
            Const::Small(s, _) => *s,
            Const::F32(_) => Scalar::F32,
            Const::F64(_) => Scalar::F64,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnOp {
    Neg,
    /// Logical not of a bool, bitwise not of an integer.
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    /// On integers: checked on the CPU (overflow traps), wrapping on the GPU.
    Add,
    Sub,
    Mul,
    /// Integer division by zero traps on the CPU.
    Div,
    /// On floats: `a - b * trunc(a / b)`, as WGSL defines it.
    Rem,
    /// Always wrapping integer arithmetic.
    WrappingAdd,
    WrappingSub,
    WrappingMul,
    /// Shift amounts are u32; on the CPU an amount of at least the bit width traps.
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    /// Logical, on bools; both operands are evaluated.
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub fn is_comparison(self) -> bool {
        matches!(self, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
    }
}

/// Built-in math. On the CPU, the transcendental ones are calls to std's wrela
/// implementations (lowering replaces them), so the WASM back end never sees them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Builtin {
    Sqrt,
    InverseSqrt,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Exp,
    Exp2,
    Log,
    Log2,
    Pow,
    Floor,
    Ceil,
    /// Round half to even.
    Round,
    Trunc,
    Fract,
    Abs,
    Sign,
    Min,
    Max,
    Clamp,
    Saturate,
    Mix,
    Step,
    Smoothstep,
    Length,
    Distance,
    Dot,
    Cross,
    Normalize,
    Dpdx,
    Dpdy,
    Fwidth,
    /// `all(a == b)` for vectors.
    AllEqual,
}

impl Builtin {
    /// Applies to each component of its arguments, which are all of one type: everything but
    /// the geometric functions and `AllEqual`.
    pub fn is_elementwise(self) -> bool {
        !matches!(
            self,
            Builtin::Length
                | Builtin::Distance
                | Builtin::Dot
                | Builtin::Cross
                | Builtin::Normalize
                | Builtin::AllEqual
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PlaceRoot {
    Local(LocalId),
    /// A pointer parameter (by index).
    Param(u32),
    /// A GPU resource: a uniform block or a storage buffer.
    Resource(ResourceId),
    /// A pointer value (a projection returned by a call; CPU only).
    Ptr(ValueId),
    /// Constant data: read, never written (CPU only).
    Data(DataId),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Proj {
    /// A struct field.
    Field(u32),
    /// A vector component.
    Comp(u8),
    /// An array element (or matrix column); traps out of range on the CPU.
    Index(ValueId),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Place {
    pub root: PlaceRoot,
    pub path: Vec<Proj>,
}

impl Place {
    /// All of `root`, with no projection.
    pub fn root(root: PlaceRoot) -> Place {
        Place { root, path: Vec::new() }
    }
    pub fn local(l: LocalId) -> Place {
        Place::root(PlaceRoot::Local(l))
    }
    pub fn with(&self, p: Proj) -> Place {
        let mut path = self.path.clone();
        path.push(p);
        Place { root: self.root.clone(), path }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Value(ValueId),
    /// A pointer to a place, for a by-reference parameter.
    Place(Place),
}

/// The most bytes a dispatch's or draw's uniform block can have: the block travels inside its
/// command, and a command fits in the CPU program's command buffer (1 MiB), with room to spare
/// for the rest of the command. Lowering rejects a larger one (E0702).
pub const MAX_UNIFORM_BYTES: u32 = (1 << 20) - 256;

/// Recording GPU work (CPU only); the WASM back end writes these as commands (wrela-abi).
#[derive(Clone, Debug, PartialEq)]
pub enum HostOp {
    /// `buffer(count)` of elements `elem_size` bytes: returns the new handle (u32).
    CreateBuffer {
        elem_size: u32,
    },
    /// Releases the buffer with this handle (u32).
    DestroyBuffer,
    /// Args: source, source offset, destination, destination offset, size: `u32`s, the offsets
    /// and size in bytes.
    CopyBuffer,
    /// `write(handle, at, values)`: args are the handle, the first element, and a run (a
    /// pointer and a count) of elements of type `elem`, `elem_size` bytes each.
    WriteBuffer {
        elem: TypeId,
        elem_size: u32,
    },
    /// Args: groups x, y, z (or, `indirect`, the handle and byte offset of a buffer holding
    /// them), each binding's handle, offset and size (`u32`s), then the uniform block value (if
    /// any).
    Dispatch {
        pipeline: u32,
        bindings: u32,
        uniform: Option<TypeId>,
        indirect: bool,
    },
    BeginScreenPass,
    /// A command of `words` 4-byte words and then `runs` runs of bytes. The args are the words
    /// (`u32`, `i32` or `f32`; with `handle`, all but the first, which is a new resource handle
    /// the op returns), then each run (a `[u8]`). The payload is the words, each run's length,
    /// then each run's bytes, padded with zeros to a multiple of 4 (stream v3's layout for
    /// textures, samplers, passes and requests).
    Command {
        opcode: u32,
        words: u32,
        runs: u32,
        handle: bool,
    },
    /// A new request number (a `u32`): `wrela.submit`'s requests are numbered by the program.
    NextRequest,
    /// `wrela.request_status(request)`: an `i32`, -1 while pending, -2 failed, else the answer's
    /// length in bytes.
    RequestStatus,
    /// `wrela.request_take(request, address)`: copies the answer to the address (a `u32`).
    RequestTake,
    /// `wrela.limit(index)`: one of the GPU's limits, a `u32`.
    Limit,
    /// `wrela.audio(task, context)`: starts the program's voice.
    Audio,
    /// `wrela.tick(task, context, hz)`: starts the program's ticker.
    Tick,
    /// `wrela.input(address, cap)`: copies up to `cap` queued input events to the address
    /// (`u32`s); how many, a `u32`.
    Input,
    /// Args: vertices, instances (or, `indirect`, the handle and byte offset of a buffer
    /// holding the counts, after the index buffer's handle, byte offset and size when
    /// `indexed`), each binding's handle, offset and size, then the uniform block value.
    Draw {
        pipeline: u32,
        bindings: u32,
        uniform: Option<TypeId>,
        indirect: bool,
        indexed: bool,
    },
    Present,
}

/// Raw memory (CPU only): what std's unsafe core is built on (language.md §6.14). Addresses are
/// `u32`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MemOp {
    /// The heap's first address, a `u32`: where the back end's memory layout puts it.
    HeapBase,
    /// The memory's size in 64 KiB pages, a `u32`.
    Pages,
    /// Grows the memory by this many pages: the old size in pages, or `u32::MAX` if it can't.
    Grow,
    /// The scalar of this kind at an address.
    Load(Scalar),
    /// Stores a scalar (the second argument) at an address (the first).
    Store(Scalar),
    /// Copies bytes: destination, source, count.
    Copy,
    /// Fills bytes: destination, the byte, count.
    Fill,
    /// An address as a pointer to a value of the type the result's pointer type names, so a
    /// place can be rooted at it ([`PlaceRoot::Ptr`]).
    Ptr,
    /// A pointer's address, a `u32`.
    Addr,
    /// Compare and swap of the `u32` at an address: (address, expected, replacement); the value
    /// that was there.
    Cas,
    /// A panic: writes the message, a `u32` address and a byte count, where hosts read it
    /// (the thread's block's `wrela_abi::memory::PANIC`), then traps.
    Panic,
    /// The address of the running thread's block (`wrela_abi::memory::thread_block`), a `u32`:
    /// the program's thread's, or the one a thread entry was given.
    ThreadBlock,
    /// Waits while the `u32` at an address is the second argument, for at most the third's
    /// microseconds: 0 woken, 1 it wasn't that value, 2 the time ran out.
    WaitFor,
    /// Atomically adds the second argument to the `u32` at an address: the old value.
    AtomicAdd,
    /// The `u32` at an address, read atomically.
    AtomicLoad,
    /// Stores the second argument at an address, atomically.
    AtomicStore,
    /// Waits while the `u32` at an address is the second argument (`memory.atomic.wait32`, no
    /// time limit): 0 woken, 1 it wasn't that value.
    Wait,
    /// Wakes up to the second argument's count of threads waiting at an address: how many.
    Notify,
    /// Calls task function number `args[0]` (an index into [`Module::tasks`]) with a context
    /// address and a chunk index.
    RunTask,
}

/// What [`Expr::Atomic`] does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AtomicOp {
    /// `atomicLoad`: no operand; the value.
    Load,
    /// `atomicStore`: the value to store; nothing (evaluated only for its effect).
    Store,
    Add,
    Sub,
    Min,
    Max,
    And,
    Or,
    Xor,
    Exchange,
    /// Stores the new value if the old one is the expected one. Its value is a struct of the
    /// old value and a `bool`, whether it stored: it may fail even when they're equal (WGSL's
    /// `atomicCompareExchangeWeak`).
    CompareExchange,
}

/// What [`Expr::Texture`] reads. Coordinates are `vec2`s in [0, 1] for sampling, and `u32`s
/// for a texel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureOp {
    /// Args: the coordinates. A `vec4`; fragment shaders only (it uses derivatives).
    Sample,
    /// Args: the coordinates and the mip level (an `f32`). A `vec4`.
    SampleLevel,
    /// Of a depth texture, with a comparison sampler. Args: the coordinates and the reference
    /// depth. An `f32`, the fraction of texels that pass; fragment shaders only.
    SampleCompare,
    /// As `SampleCompare`, at level 0: any stage.
    SampleCompareLevel,
    /// Args: x and y. The texel at level 0: a `vec4`, or an `f32` of a depth texture.
    Load,
    /// The width or height, a `u32`.
    Width,
    Height,
}

impl TextureOp {
    /// Whether it uses derivatives: fragment shaders only, in uniform control flow.
    pub fn uses_derivatives(self) -> bool {
        matches!(self, TextureOp::Sample | TextureOp::SampleCompare)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Const(Const),
    /// A zero (or false) of any type.
    Zero(TypeId),
    Load(Place),
    Unary(UnOp, ValueId),
    /// Operands have the same type, except: matrix op scalar (each element), matrix × vector
    /// and vector × matrix. (A scalar with a vector is splatted first.)
    Binary(BinOp, ValueId, ValueId),
    Call(FuncId, Vec<Arg>),
    Builtin(Builtin, Vec<ValueId>),
    /// Builds a struct, vector, matrix (from columns) or array from its parts.
    Construct(TypeId, Vec<ValueId>),
    /// Builds a value of an enum: its variant, and the payload if the variant has one.
    Variant(TypeId, u32, Option<ValueId>),
    /// A field of a struct value, a component of a vector, or a column of a matrix.
    Extract(ValueId, u32),
    /// An element of an array value (or a vector's component) at a dynamic index.
    ExtractDyn(ValueId, ValueId),
    Splat(ValueId, u8),
    Swizzle(ValueId, Vec<u8>),
    /// Numeric conversion: float to int truncates (and traps out of range on the CPU); int to
    /// int keeps the low bits; bool to int is 0 or 1.
    Convert(ValueId, Scalar),
    Bitcast(ValueId, Scalar),
    /// `cond ? if_true : if_false`, evaluating both.
    Select {
        cond: ValueId,
        if_true: ValueId,
        if_false: ValueId,
    },
    /// The run (pointer and length) of a fixed array place, for a `[T]` parameter.
    Run(Place),
    /// A pointer to a place, as a value (CPU only: a projection to return).
    Addr(Place),
    Host(HostOp, Vec<ValueId>),
    /// Raw memory (CPU only).
    Mem(MemOp, Vec<ValueId>),
    /// The element count of a storage buffer's runtime array (GPU only), a `u32`.
    ArrayLength(Place),
    /// A read of a texture resource (GPU only), with a sampler resource for the sampling ops.
    Texture(TextureOp, ResourceId, Option<ResourceId>, Vec<ValueId>),
    /// An atomic read-modify-write of the atomic at the place (GPU only): the value that was
    /// there. Args: the operand (for `CompareExchange`, the expected value then the new one).
    Atomic(AtomicOp, Place, Vec<ValueId>),
    /// `workgroupBarrier()` (GPU only): no invocation of the workgroup goes on until all have
    /// reached it, and their workgroup memory writes are visible. In uniform control flow only.
    Barrier,
    /// A GPU builtin input of the entry point (`@builtin(...)`), by index into the entry's
    /// inputs, as the std struct that holds it (`GlobalId`, `FragCoord`, ...).
    EntryInput(u32),
    /// A by-value parameter.
    Param(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Let(ValueId, Expr),
    /// Evaluates for its effect (a call returning nothing, or a host op).
    Eval(Expr),
    Store(Place, ValueId),
    If {
        cond: ValueId,
        then: Block,
        else_: Block,
    },
    /// Runs `body`, then `continuing`, and repeats, until a `Break` (or `Return`) leaves it.
    /// `Continue` in `body` jumps to `continuing`. `continuing` can't break, continue or
    /// return (WGSL's rule).
    Loop {
        body: Block,
        continuing: Block,
    },
    Break,
    Continue,
    Return(Option<ValueId>),
    /// Unreachable: traps on the CPU.
    Trap,
    /// Where in the source the statements after it come from, up to the next `At`: it does
    /// nothing, and lets a trap say where it happened (and an error in a derived function
    /// point at the code that caused it).
    At(wrela_diag::Span),
}

pub type Block = Vec<Stmt>;

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: TypeId,
    /// Passed as a pointer to the caller's place.
    pub by_ref: bool,
    pub mutable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalDecl {
    pub name: String,
    pub ty: TypeId,
}

/// How many counted calls (recursion that `@deterministic` code reaches) may be in progress
/// at once: the next panics, the same way on every engine (language.md §8).
pub const RECURSION_LIMIT: u32 = 256;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    /// `None` returns nothing.
    pub ret: Option<TypeId>,
    /// The return is a pointer to a place (a projection; CPU only).
    pub ret_ref: bool,
    pub locals: Vec<LocalDecl>,
    /// Each value's type, by `ValueId`.
    pub values: Vec<TypeId>,
    pub body: Block,
    /// A recursive function whose depth is counted (CPU only): a call made while
    /// [`RECURSION_LIMIT`] counted calls are in progress panics with [`Module::depth_message`]
    /// (language.md §8).
    pub counted: bool,
    /// An interval derivation's (`derive::interval`): GPU flattening may keep a call to it
    /// rather than inline it (`opt::outlined`), since nested derivations call the same ones
    /// from many places.
    pub interval: bool,
}

impl Function {
    pub fn new(name: impl Into<String>, params: Vec<Param>, ret: Option<TypeId>) -> Function {
        Function { name: name.into(), params, ret, ..Function::default() }
    }

    pub fn new_value(&mut self, ty: TypeId) -> ValueId {
        self.values.push(ty);
        ValueId(self.values.len() as u32 - 1)
    }

    /// A new value of type `ty`, defined as `e` at the end of `body`.
    pub fn let_(&mut self, body: &mut Block, ty: TypeId, e: Expr) -> ValueId {
        let v = self.new_value(ty);
        body.push(Stmt::Let(v, e));
        v
    }

    pub fn new_local(&mut self, name: impl Into<String>, ty: TypeId) -> LocalId {
        self.locals.push(LocalDecl { name: name.into(), ty });
        LocalId(self.locals.len() as u32 - 1)
    }

    pub fn value_ty(&self, v: ValueId) -> TypeId {
        self.values[v.index()]
    }

    /// A copy of all but the body, for a transform to build a new body in.
    pub fn clone_signature(&self) -> Function {
        Function {
            name: self.name.clone(),
            params: self.params.clone(),
            ret: self.ret,
            ret_ref: self.ret_ref,
            locals: self.locals.clone(),
            values: self.values.clone(),
            body: Vec::new(),
            counted: self.counted,
            interval: false,
        }
    }

    /// Parameter `i` passed on as a call's argument: its place if it's by reference, else its
    /// value, read in `body`.
    pub fn param_arg(&mut self, i: u32, body: &mut Block) -> Arg {
        let p = &self.params[i as usize];
        if p.by_ref {
            return Arg::Place(Place::root(PlaceRoot::Param(i)));
        }
        let v = self.new_value(p.ty);
        body.push(Stmt::Let(v, Expr::Param(i)));
        Arg::Value(v)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// A uniform block (`var<uniform>`), or a read-only storage block with the same bytes when
    /// its layout doesn't meet the uniform rules.
    Uniform { storage: bool },
    /// `var<storage, read>` holding `array<T>`.
    StorageRead,
    /// `var<storage, read_write>` holding `array<T>`.
    StorageReadWrite,
    /// `var<private>`: one per invocation, no binding (the entry point fills it in).
    Private,
    /// `var<workgroup>` holding `ty` (an array): one per workgroup, shared by its invocations.
    Workgroup,
    /// `texture_2d<f32>`, or `texture_depth_2d`.
    Texture { depth: bool },
    /// `sampler`, or `sampler_comparison`.
    Sampler { comparison: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Resource {
    pub name: String,
    pub binding: u32,
    pub kind: ResourceKind,
    /// The block's type (a struct), or the element type of a storage array.
    pub ty: TypeId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltinInput {
    GlobalInvocationId,
    LocalInvocationId,
    WorkgroupId,
    NumWorkgroups,
    VertexIndex,
    InstanceIndex,
    /// The fragment's position.
    Position,
}

/// How a GPU entry point's inputs and outputs map onto WGSL's.
#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    Compute {
        workgroup_size: [u32; 3],
    },
    /// Returns a struct whose field `position` is the clip position; the rest are varyings.
    Vertex {
        position_field: u32,
        flat: Vec<bool>,
    },
    /// Takes the vertex's varyings struct (if any) as its last argument; returns a vec4 color.
    Fragment {
        varyings: Option<TypeId>,
        position_field: Option<u32>,
        flat: Vec<bool>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct EntryPoint {
    pub name: String,
    pub stage: Stage,
    pub function: FuncId,
    /// The builtins the function reads with `Expr::EntryInput`.
    pub inputs: Vec<BuiltinInput>,
}

/// A constant's value, part by part as its type has them.
#[derive(Clone, Debug, PartialEq)]
pub enum ConstValue {
    Scalar(Const),
    /// A struct's fields, a vector's components, a matrix's columns or an array's elements.
    Parts(Vec<ConstValue>),
    /// An enum's value: its variant, and the payload if the variant has one.
    Variant(u32, Option<Box<ConstValue>>),
    /// An array of bytes (`u8`s): text, embedded files.
    Bytes(Vec<u8>),
    /// A `u32`: the address of other constant data (CPU only). A constant the build computed
    /// points at its parts this way: a `Vec`'s elements, a `Box`'s value, a `Text`'s bytes.
    Addr(DataId),
}

/// A constant in memory, which code reads through [`PlaceRoot::Data`] (CPU only): one copy
/// however many functions read it.
#[derive(Clone, Debug, PartialEq)]
pub struct Data {
    pub name: String,
    pub ty: TypeId,
    pub value: ConstValue,
}

/// A compiled unit for one target: the CPU program, or one GPU pipeline's shader module.
#[derive(Clone, Debug, Default)]
pub struct Module {
    pub types: Types,
    pub functions: Vec<Function>,
    pub resources: Vec<Resource>,
    pub entry_points: Vec<EntryPoint>,
    /// CPU only: functions exported from the WASM, by name.
    pub exports: Vec<(String, FuncId)>,
    /// CPU only: constant data.
    pub data: Vec<Data>,
    /// CPU only: the text a counted call past the limit panics with: the data holding it, and
    /// its length (see [`Function::counted`]).
    pub depth_message: Option<(DataId, u32)>,
    /// CPU only: the text a call whose frame doesn't fit on its thread's stack panics with.
    pub stack_message: Option<(DataId, u32)>,
    /// CPU only, in a debug build: the text a float operation that creates a NaN panics with
    /// (language.md §11). A release build has none, and no such checks.
    pub nan_message: Option<(DataId, u32)>,
    /// CPU only: the program's state, which lasts across calls (language.md §12): data the
    /// program changes, unlike the rest, which is constant.
    pub state: Option<DataId>,
    /// CPU only: other data the program changes: a lifted build's table of literals, and what
    /// keeps its GPU copy (language.md §22).
    pub writable: Vec<DataId>,
    /// CPU only: functions called by index ([`MemOp::RunTask`]): a parallel job's chunks, a
    /// voice's quanta, a ticker's ticks. Each takes a context address and a chunk index.
    pub tasks: Vec<FuncId>,
    /// CPU only: std's thread entries the program uses (`@thread_entry`), each exported as
    /// `__` and its name: a host calls it on a thread of its own, with the thread's number
    /// first (wrela_abi `memory`'s threads), and it runs on that thread's stack.
    pub thread_entries: Vec<(String, FuncId)>,
    /// CPU only: whether the program starts a voice (`std::audio`), so the module imports
    /// `wrela.audio`.
    pub audio: bool,
    /// CPU only: whether the program starts a ticker (`std::tick`), so the module imports
    /// `wrela.tick`.
    pub tick: bool,
    /// CPU only: whether the program reads input (`std::input`), so the module imports
    /// `wrela.input`.
    pub input: bool,
}

impl Module {
    /// The type of a place in `f`: its root's, through each projection. `None` when a type it
    /// passes through was never interned (a storage buffer's whole array, or the component
    /// type of a vector in a module that names no `f32`), which a well-formed place never
    /// needs.
    pub fn place_ty(&self, f: &Function, p: &Place) -> Option<TypeId> {
        let (t, path) = self.place_start(f, p)?;
        path.iter().try_fold(t, |t, proj| self.proj_ty(t, proj))
    }

    /// Where [`Module::place_ty`] starts: the type of a place's root, and the projections to
    /// apply to it. A storage buffer's place is its array of elements, whose type may never
    /// have been interned: a place that indexes the array starts at the element, past the index.
    pub fn place_start<'p>(&self, f: &Function, p: &'p Place) -> Option<(TypeId, &'p [Proj])> {
        let t = match &p.root {
            PlaceRoot::Local(l) => f.locals.get(l.index())?.ty,
            PlaceRoot::Param(i) => f.params.get(*i as usize)?.ty,
            PlaceRoot::Ptr(v) => match self.types.get(f.value_ty(*v)) {
                TypeDef::Ptr(t) => *t,
                _ => return None,
            },
            PlaceRoot::Data(d) => self.data.get(d.index())?.ty,
            PlaceRoot::Resource(r) => {
                let res = self.resources.get(r.index())?;
                match res.kind {
                    ResourceKind::Uniform { .. }
                    | ResourceKind::Private
                    | ResourceKind::Workgroup
                    | ResourceKind::Texture { .. }
                    | ResourceKind::Sampler { .. } => res.ty,
                    ResourceKind::StorageRead | ResourceKind::StorageReadWrite => {
                        match p.path.as_slice() {
                            [Proj::Index(_), rest @ ..] => return Some((res.ty, rest)),
                            _ => self.types.lookup(&TypeDef::RuntimeArray(res.ty))?,
                        }
                    }
                }
            }
        };
        Some((t, &p.path))
    }

    /// The type of one projection of a value of type `t`: `None` when the projection doesn't
    /// apply to `t`, or its type was never interned.
    pub fn proj_ty(&self, t: TypeId, proj: &Proj) -> Option<TypeId> {
        match (self.types.get(t), proj) {
            (TypeDef::Struct { .. } | TypeDef::Enum { .. }, Proj::Field(i)) => {
                self.types.field(t, *i)
            }
            (TypeDef::Vector(_), Proj::Comp(_) | Proj::Index(_)) => {
                self.types.lookup(&TypeDef::Scalar(Scalar::F32))
            }
            (TypeDef::Matrix(n), Proj::Index(_)) => self.types.lookup(&TypeDef::Vector(*n)),
            (TypeDef::Array(e, _) | TypeDef::RuntimeArray(e) | TypeDef::Run(e), Proj::Index(_)) => {
                Some(*e)
            }
            _ => None,
        }
    }

    /// The local whose own storage place `p` (of `f`) is in: its root is a local, and its
    /// path doesn't go through a run or a pointer the local holds (whose data is elsewhere).
    pub fn local_storage(&self, f: &Function, p: &Place) -> Option<LocalId> {
        let l = p.root_local()?;
        let mut t = f.locals.get(l.index())?.ty;
        for proj in &p.path {
            if matches!(self.types.get(t), TypeDef::Run(_) | TypeDef::Ptr(_)) {
                return None;
            }
            t = self.proj_ty(t, proj)?;
        }
        Some(l)
    }

    pub fn add_function(&mut self, f: Function) -> FuncId {
        self.functions.push(f);
        FuncId(self.functions.len() as u32 - 1)
    }

    pub fn add_resource(&mut self, r: Resource) -> ResourceId {
        self.resources.push(r);
        ResourceId(self.resources.len() as u32 - 1)
    }

    pub fn add_data(&mut self, d: Data) -> DataId {
        self.data.push(d);
        DataId(self.data.len() as u32 - 1)
    }
}

/// A name as an identifier both back ends accept: letters, digits and single underscores, not
/// starting with a digit or an underscore (WGSL reserves names starting with `__`). The WGSL
/// writer may still rename one (a WGSL keyword, a name ending in a digit, a name used twice),
/// so the manifest takes entry points' names from the written shader (`wrela_wgsl::emit`).
pub fn ident(name: &str) -> String {
    let mut s = String::with_capacity(name.len());
    for c in name.chars() {
        let c = if c.is_ascii_alphanumeric() { c } else { '_' };
        if !(c == '_' && (s.is_empty() || s.ends_with('_'))) {
            s.push(c);
        }
    }
    let s = s.trim_end_matches('_');
    if s.is_empty() || s.starts_with(|c: char| c.is_ascii_digit()) {
        format!("t_{s}")
    } else {
        s.to_string()
    }
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&print::print(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A matrix's column and a vector's component are types of the module as soon as the
    /// matrix is: `m[c][r]` projects through both, in a module (a GPU pipeline's) that may
    /// make no `vecN` of its own.
    #[test]
    fn a_matrix_brings_its_parts() {
        let mut m = Module::default();
        let mat = m.types.intern(TypeDef::Matrix(3));
        assert!(m.types.lookup(&TypeDef::Vector(3)).is_some());
        assert!(m.types.lookup(&TypeDef::Scalar(Scalar::F32)).is_some());
        assert_eq!(m.types.lookup(&TypeDef::Matrix(3)), Some(mat));
    }

    #[test]
    fn identifiers() {
        assert_eq!(ident("Sphere::distance"), "Sphere_distance");
        assert_eq!(ident("a__b"), "a_b");
        assert_eq!(ident("__k"), "k");
        assert_eq!(ident("2d"), "t_2d");
        assert_eq!(ident("_"), "t_");
    }
}
