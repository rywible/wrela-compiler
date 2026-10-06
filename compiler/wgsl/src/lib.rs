//! The WGSL back end (Q3 of #6: lower to naga IR, not WGSL text). A GPU module of the IR
//! becomes a naga module, which naga validates and writes as WGSL. Validation failures are
//! compiler bugs, reported as such.
//!
//! - Types keep the IR's layout (`wrela_ir::layout`), so struct member offsets in the WGSL are
//!   the offsets the CPU wrote uniforms with. An enum is the one exception: WGSL has no unions,
//!   so it's a struct of its tag and every payload, and types that hold one are laid out
//!   around that struct. An enum is never `GpuData`, so those types never cross to the CPU.
//! - Every IR value becomes a named WGSL `let`, so evaluation order is the IR's exactly.
//! - No operation is a WGSL const-expression: WGSL evaluates those when the shader is created,
//!   and rejects `1.0 / 0.0` or a shift by 40 there, where language.md §11 promises the values
//!   WGSL defines at run time. A constant operand that would make one goes through a variable.
//! - GPU modules arrive flattened (`wrela_ir::opt`): every call is inlined but those to the
//!   big functions that take only values, which are written as WGSL functions, callees first.

use naga::{
    AddressSpace, ArraySize, BinaryOperator, Binding, Block, BuiltIn, Expression, Function,
    FunctionArgument, FunctionResult, GlobalVariable, Handle, Interpolation, Literal,
    LocalVariable, MathFunction, ResourceBinding, Sampling, Scalar, ScalarKind, ShaderStage, Span,
    Statement, StorageAccess, StructMember, Type, TypeInner, UnaryOperator, VectorSize,
};
use std::collections::{HashMap, HashSet};
use wrela_ir as ir;
use wrela_ir::layout::WgslLayouts;

type R<T> = Result<T, String>;

/// A GPU module written as WGSL.
#[derive(Clone, Debug)]
pub struct Wgsl {
    pub text: String,
    /// Each entry point's name in `text`, in the module's order. The WGSL writer renames
    /// identifiers that are WGSL keywords or builtins, end in a digit, or are used twice.
    pub entry_points: Vec<String>,
}

/// Writes a GPU module as validated WGSL.
pub fn emit(m: &ir::Module) -> R<Wgsl> {
    let module = to_naga(m)?;
    let info = validate(&module)?;
    let text =
        naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
            .map_err(|e| format!("internal: writing WGSL failed: {e}"))?;
    let entry_points = entry_point_names(&module);
    if let Some(n) = entry_points.iter().find(|n| !text.contains(&format!("fn {n}("))) {
        return Err(format!("internal: the WGSL writer didn't name an entry point `{n}`"));
    }
    Ok(Wgsl { text, entry_points })
}

/// The names naga's WGSL writer gives a module's entry points: its namer, set up as the writer
/// sets it up (naga 30's `back::wgsl::Writer::reset`). `emit` checks them against the text.
fn entry_point_names(module: &naga::Module) -> Vec<String> {
    use naga::keywords::wgsl::{BUILTIN_IDENTIFIER_SET, RESERVED_SET};
    let mut names = naga::FastHashMap::default();
    naga::proc::Namer::default().reset(
        module,
        &RESERVED_SET,
        &BUILTIN_IDENTIFIER_SET,
        naga::proc::CaseInsensitiveKeywordSet::empty(),
        &["__", "_naga"],
        &mut names,
    );
    (0..module.entry_points.len())
        .map(|i| names.get(&naga::proc::NameKey::EntryPoint(i as _)).cloned().unwrap_or_default())
        .collect()
}

/// Validates a naga module as WebGPU will.
pub fn validate(module: &naga::Module) -> R<naga::valid::ModuleInfo> {
    let mut v = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    );
    v.validate(module)
        .map_err(|e| format!("internal: the generated shader is invalid: {}", e.emit_to_string("")))
}

/// The struct member that holds field `k` of a value of type `t`: for an enum, its tag, or the
/// payload of variant `k - 1` among the variants that have one.
/// How many elements an index into a value of type `t` can reach, when the type says.
fn static_len(types: &ir::Types, t: ir::TypeId) -> Option<u32> {
    match types.get(t) {
        ir::TypeDef::Array(_, n) => Some(*n),
        ir::TypeDef::Vector(n) | ir::TypeDef::Matrix(n) => Some(u32::from(*n)),
        _ => None,
    }
}

fn member(types: &ir::Types, t: ir::TypeId, k: u32) -> u32 {
    match types.get(t) {
        ir::TypeDef::Enum { variants, .. } if k > 0 => {
            1 + variants[..k as usize - 1].iter().filter(|(_, p)| p.is_some()).count() as u32
        }
        _ => k,
    }
}

fn vsize(n: u8) -> VectorSize {
    match n {
        2 => VectorSize::Bi,
        3 => VectorSize::Tri,
        _ => VectorSize::Quad,
    }
}

fn scalar(s: ir::Scalar) -> R<Scalar> {
    Ok(match s {
        ir::Scalar::Bool => Scalar::BOOL,
        ir::Scalar::I32 => Scalar::I32,
        ir::Scalar::U32 => Scalar::U32,
        ir::Scalar::F32 => Scalar::F32,
        other => return Err(format!("internal: `{}` in GPU code", other.name())),
    })
}

struct Cx<'m> {
    m: &'m ir::Module,
    out: naga::Module,
    types: HashMap<ir::TypeId, Handle<Type>>,
    layouts: WgslLayouts,
    globals: Vec<Handle<GlobalVariable>>,
    /// The functions that stay calls, written so far.
    fns: HashMap<ir::FuncId, Handle<Function>>,
    /// Module constants for long float literals, by their bits.
    float_consts: HashMap<u32, Handle<naga::Constant>>,
}

pub fn to_naga(m: &ir::Module) -> R<naga::Module> {
    let mut cx = Cx {
        m,
        out: naga::Module::default(),
        types: HashMap::new(),
        layouts: WgslLayouts::default(),
        globals: Vec::new(),
        fns: HashMap::new(),
        float_consts: HashMap::new(),
    };
    for r in &m.resources {
        let g = cx.global(r)?;
        cx.globals.push(g);
    }
    // The functions that stay calls, callees first.
    let entries: HashSet<ir::FuncId> = m.entry_points.iter().map(|e| e.function).collect();
    let mut done = HashSet::new();
    fn helpers_of(
        cx: &mut Cx<'_>,
        f: ir::FuncId,
        entries: &HashSet<ir::FuncId>,
        done: &mut HashSet<ir::FuncId>,
    ) -> R<()> {
        if !done.insert(f) {
            return Ok(());
        }
        for g in ir::visit::calls(&cx.m.functions[f.index()].body) {
            helpers_of(cx, g, entries, done)?;
        }
        if !entries.contains(&f) {
            let func = cx.helper(f)?;
            let h = cx.out.functions.append(func, Span::UNDEFINED);
            cx.fns.insert(f, h);
        }
        Ok(())
    }
    for e in &m.entry_points {
        helpers_of(&mut cx, e.function, &entries, &mut done)?;
    }
    for e in m.entry_points.iter() {
        let func = cx.function(e)?;
        let (stage, wg) = match &e.stage {
            ir::Stage::Compute { workgroup_size } => (ShaderStage::Compute, *workgroup_size),
            ir::Stage::Vertex { .. } => (ShaderStage::Vertex, [0; 3]),
            ir::Stage::Fragment { .. } => (ShaderStage::Fragment, [0; 3]),
        };
        cx.out.entry_points.push(naga::EntryPoint {
            name: ir::ident(&e.name),
            stage,
            early_depth_test: None,
            workgroup_size: wg,
            workgroup_size_overrides: None,
            function: func,
            mesh_info: None,
            task_payload: None,
            incoming_ray_payload: None,
        });
    }
    Ok(cx.out)
}

impl<'m> Cx<'m> {
    /// The module constant holding `x` (made once).
    fn float_const(&mut self, x: f32) -> Handle<naga::Constant> {
        if let Some(&k) = self.float_consts.get(&x.to_bits()) {
            return k;
        }
        let ty = self.ty_inner(TypeInner::Scalar(Scalar::F32));
        let init = self
            .out
            .global_expressions
            .append(Expression::Literal(Literal::F32(x)), Span::UNDEFINED);
        let name = format!("K{}", self.float_consts.len());
        let k = self
            .out
            .constants
            .append(naga::Constant { name: Some(name), ty, init }, Span::UNDEFINED);
        self.float_consts.insert(x.to_bits(), k);
        k
    }

    fn ty(&mut self, t: ir::TypeId) -> R<Handle<Type>> {
        if let Some(&h) = self.types.get(&t) {
            return Ok(h);
        }
        let m = self.m;
        let (name, inner) = match *m.types.get(t) {
            ir::TypeDef::Scalar(s) => (None, TypeInner::Scalar(scalar(s)?)),
            ir::TypeDef::Atomic(s) => (None, TypeInner::Atomic(scalar(s)?)),
            ir::TypeDef::Vector(n) => {
                (None, TypeInner::Vector { size: vsize(n), scalar: Scalar::F32 })
            }
            ir::TypeDef::Matrix(n) => {
                (None, TypeInner::Matrix { columns: vsize(n), rows: vsize(n), scalar: Scalar::F32 })
            }
            ir::TypeDef::Array(e, n) => {
                let n = std::num::NonZeroU32::new(n)
                    .ok_or("internal: a zero-length array in GPU code")?;
                (None, self.array(e, ArraySize::Constant(n))?)
            }
            ir::TypeDef::RuntimeArray(e) => (None, self.array(e, ArraySize::Dynamic)?),
            ir::TypeDef::Struct { ref name, .. } | ir::TypeDef::Enum { ref name, .. } => {
                let (l, offsets) = self.layouts.get(&self.m.types, t);
                let offsets = offsets.to_vec();
                let mut members = Vec::new();
                for ((mname, mt), offset) in self.members(t)?.into_iter().zip(offsets) {
                    members.push(StructMember {
                        name: Some(ir::ident(mname)),
                        ty: self.ty(mt)?,
                        binding: None,
                        offset,
                    });
                }
                (Some(ir::ident(name)), TypeInner::Struct { members, span: l.size })
            }
            ir::TypeDef::Run(_) | ir::TypeDef::Ptr(_) => {
                return Err("internal: a CPU-only type in GPU code".into());
            }
        };
        let h = self.out.types.insert(Type { name, inner }, Span::UNDEFINED);
        self.types.insert(t, h);
        Ok(h)
    }

    fn global(&mut self, r: &ir::Resource) -> R<Handle<GlobalVariable>> {
        let (space, ty) = match r.kind {
            ir::ResourceKind::Uniform { storage: false } => (AddressSpace::Uniform, self.ty(r.ty)?),
            ir::ResourceKind::Uniform { storage: true } => {
                (AddressSpace::Storage { access: StorageAccess::LOAD }, self.ty(r.ty)?)
            }
            ir::ResourceKind::StorageRead | ir::ResourceKind::StorageReadWrite => {
                let inner = self.array(r.ty, ArraySize::Dynamic)?;
                let arr = self.ty_inner(inner);
                let access = if r.kind == ir::ResourceKind::StorageRead {
                    StorageAccess::LOAD
                } else {
                    StorageAccess::LOAD | StorageAccess::STORE
                };
                (AddressSpace::Storage { access }, arr)
            }
            ir::ResourceKind::Private => (AddressSpace::Private, self.ty(r.ty)?),
            ir::ResourceKind::Workgroup => (AddressSpace::WorkGroup, self.ty(r.ty)?),
            ir::ResourceKind::Texture { depth } => {
                let class = if depth {
                    naga::ImageClass::Depth { multi: false }
                } else {
                    naga::ImageClass::Sampled { kind: ScalarKind::Float, multi: false }
                };
                let inner =
                    TypeInner::Image { dim: naga::ImageDimension::D2, arrayed: false, class };
                (AddressSpace::Handle, self.ty_inner(inner))
            }
            ir::ResourceKind::Sampler { comparison } => {
                (AddressSpace::Handle, self.ty_inner(TypeInner::Sampler { comparison }))
            }
        };
        let binding = !matches!(r.kind, ir::ResourceKind::Private | ir::ResourceKind::Workgroup);
        let binding = binding.then_some(ResourceBinding { group: 0, binding: r.binding });
        Ok(self.out.global_variables.append(
            GlobalVariable {
                name: Some(ir::ident(&r.name)),
                space,
                binding,
                ty,
                init: None,
                memory_decorations: naga::MemoryDecorations::empty(),
            },
            Span::UNDEFINED,
        ))
    }

    /// The vertex output (and fragment input) struct: the clip position, then the varyings.
    fn io_struct(
        &mut self,
        vt: ir::TypeId,
        position_field: u32,
        flat: &[bool],
    ) -> R<(Handle<Type>, IoMap)> {
        let m = self.m;
        let ir::TypeDef::Struct { fields, name } = m.types.get(vt) else {
            return Err("internal: vertex output isn't a struct".into());
        };
        let v4 = self.ty_vec4();
        let mut members = Vec::new();
        let mut offset = 0;
        let mut map = Vec::new();
        let mut location = 0;
        for (i, (fname, ft)) in fields.iter().enumerate() {
            let is_pos = i as u32 == position_field;
            let (ty, inner_flat) = if is_pos {
                (v4, false)
            } else {
                let is_flat = flat.get(i).copied().unwrap_or(false);
                // A `Flat<T>` is a struct with one field.
                let inner = if is_flat {
                    match m.types.get(*ft) {
                        ir::TypeDef::Struct { fields, .. } => fields[0].1,
                        _ => *ft,
                    }
                } else {
                    *ft
                };
                (self.ty(inner)?, is_flat)
            };
            let binding = if is_pos {
                Binding::BuiltIn(BuiltIn::Position { invariant: false })
            } else {
                let interp = if inner_flat
                    || matches!(
                        self.out.types[ty].inner,
                        TypeInner::Scalar(Scalar { kind: ScalarKind::Uint | ScalarKind::Sint, .. })
                    ) {
                    Interpolation::Flat
                } else {
                    Interpolation::Perspective
                };
                let b = Binding::Location {
                    location,
                    interpolation: Some(interp),
                    sampling: Some(if interp == Interpolation::Flat {
                        Sampling::First
                    } else {
                        Sampling::Center
                    }),
                    blend_src: None,
                    per_primitive: false,
                };
                location += 1;
                b
            };
            members.push(StructMember {
                name: Some(ir::ident(fname)),
                ty,
                binding: Some(binding),
                offset,
            });
            offset += 16;
            map.push((i as u32, inner_flat));
        }
        let h = self.out.types.insert(
            Type {
                name: Some(format!("{}_io", ir::ident(name))),
                inner: TypeInner::Struct { members, span: offset.max(16) },
            },
            Span::UNDEFINED,
        );
        Ok((h, map))
    }

    fn ty_vec4(&mut self) -> Handle<Type> {
        self.ty_inner(TypeInner::Vector { size: VectorSize::Quad, scalar: Scalar::F32 })
    }

    fn ty_inner(&mut self, inner: TypeInner) -> Handle<Type> {
        self.out.types.insert(Type { name: None, inner }, Span::UNDEFINED)
    }

    /// An array of `e`: of `size` elements, or a runtime-sized array (`ArraySize::Dynamic`).
    fn array(&mut self, e: ir::TypeId, size: ArraySize) -> R<TypeInner> {
        let base = self.ty(e)?;
        Ok(TypeInner::Array { base, size, stride: self.layouts.get(&self.m.types, e).0.stride() })
    }

    /// The members of a struct, or of an enum as WGSL holds it: its tag, then each payload.
    fn members(&self, t: ir::TypeId) -> R<Vec<(&'m str, ir::TypeId)>> {
        let types = &self.m.types;
        match types.get(t) {
            ir::TypeDef::Struct { fields, .. } => {
                Ok(fields.iter().map(|(n, f)| (n.as_str(), *f)).collect())
            }
            ir::TypeDef::Enum { variants, .. } => {
                let tag = types.field(t, 0).ok_or("internal: an enum's tag type")?;
                Ok(std::iter::once(("tag", tag))
                    .chain(variants.iter().filter_map(|(v, p)| Some((v.as_str(), (*p)?))))
                    .collect())
            }
            _ => Err("internal: members of a type that has none".into()),
        }
    }

    fn function(&mut self, entry: &'m ir::EntryPoint) -> R<Function> {
        self.build(entry.function, Some(entry))
    }

    /// A function that stays a call: its parameters, its result, and its body.
    fn helper(&mut self, f: ir::FuncId) -> R<Function> {
        self.build(f, None)
    }

    fn build(&mut self, id: ir::FuncId, entry: Option<&'m ir::EntryPoint>) -> R<Function> {
        let f = &self.m.functions[id.index()];
        let mut func = Function { name: Some(ir::ident(&f.name)), ..Function::default() };
        let mut fb = Fb {
            cx: self,
            f,
            func: &mut func,
            values: HashMap::new(),
            locals: Vec::new(),
            entry,
            io: None,
            param_base: 0,
            inputs: Vec::new(),
            consts: HashSet::new(),
            floats: HashMap::new(),
            ints: HashMap::new(),
        };
        fb.signature()?;
        let mut body = fb.block(&f.body)?;
        // A body that ends in a trap (a loop left only by `return`) still ends in a return, as
        // WGSL wants a function with a result to.
        if let Some(ir::Stmt::Trap) = f.body.last() {
            fb.return_zero(&mut body);
        }
        drop(fb);
        func.body = body;
        Ok(func)
    }
}

/// An expression that only computes from values: written where it's used.
fn pure(e: &ir::Expr) -> bool {
    match e {
        ir::Expr::Builtin(b, _) => {
            !matches!(b, ir::Builtin::Dpdx | ir::Builtin::Dpdy | ir::Builtin::Fwidth)
        }
        ir::Expr::Unary(..)
        | ir::Expr::Binary(..)
        | ir::Expr::Construct(..)
        | ir::Expr::Extract(..)
        | ir::Expr::Splat(..)
        | ir::Expr::Swizzle(..)
        | ir::Expr::Convert(..)
        | ir::Expr::Bitcast(..)
        | ir::Expr::Select { .. } => true,
        _ => false,
    }
}

/// Whether field `i` of the vertex output type `t` is a struct of one field (a `Flat<T>` or a
/// `ClipPosition`) that the WGSL passes as that field.
fn wrapped(types: &ir::Types, t: ir::TypeId, i: u32) -> R<bool> {
    let ft = types.field(t, i).ok_or("internal: a varying's type")?;
    match types.get(ft) {
        ir::TypeDef::Struct { fields, .. } if fields.len() == 1 => Ok(true),
        ir::TypeDef::Struct { .. } => Err("internal: a varying that's a struct".into()),
        _ => Ok(false),
    }
}

/// For each member of a vertex output (fragment input) struct: the IR struct's field it holds,
/// and whether that field is a `Flat<T>`.
type IoMap = Vec<(u32, bool)>;

/// Builds one naga function.
struct Fb<'a, 'm> {
    cx: &'a mut Cx<'m>,
    f: &'m ir::Function,
    func: &'a mut Function,
    values: HashMap<ir::ValueId, Handle<Expression>>,
    locals: Vec<Handle<LocalVariable>>,
    /// The entry point being written, or `None` for a function that stays a call.
    entry: Option<&'m ir::EntryPoint>,
    /// For a vertex entry: the output struct and how its members map to the IR struct's.
    io: Option<(Handle<Type>, IoMap, ir::TypeId)>,
    /// Where the IR's parameters start among the naga arguments: after the builtin inputs.
    param_base: u32,
    /// For each builtin input, its naga argument; `None` for a fragment's position when it
    /// takes the varyings, which hold it (WGSL takes each builtin once).
    inputs: Vec<Option<u32>>,
    /// Values that are WGSL const-expressions.
    consts: HashSet<ir::ValueId>,
    /// The scalar float constants among them, by value.
    floats: HashMap<ir::ValueId, f32>,
    /// The scalar integer constants among them, by value.
    ints: HashMap<ir::ValueId, i64>,
}

impl<'a, 'm> Fb<'a, 'm> {
    fn signature(&mut self) -> R<()> {
        for l in &self.f.locals {
            let ty = self.cx.ty(l.ty)?;
            let h = self.func.local_variables.append(
                LocalVariable { name: Some(ir::ident(&l.name)), ty, init: None },
                Span::UNDEFINED,
            );
            self.locals.push(h);
        }
        let Some(e) = self.entry else {
            // A function that stays a call: its parameters by value, and its result.
            for p in &self.f.params {
                let ty = self.cx.ty(p.ty)?;
                self.func.arguments.push(FunctionArgument {
                    name: Some(ir::ident(&p.name)),
                    ty,
                    binding: None,
                });
            }
            if let Some(r) = self.f.ret {
                let ty = self.cx.ty(r)?;
                self.func.result = Some(FunctionResult { ty, binding: None });
            }
            return Ok(());
        };
        let in_varyings = |b: &ir::BuiltinInput| {
            *b == ir::BuiltinInput::Position
                && matches!(e.stage, ir::Stage::Fragment { varyings: Some(_), .. })
        };
        // Builtin inputs first, in order.
        for (i, b) in e.inputs.iter().enumerate() {
            use ir::BuiltinInput as B;
            if in_varyings(b) {
                self.inputs.push(None);
                continue;
            }
            self.inputs.push(Some(self.func.arguments.len() as u32));
            let builtin = match b {
                B::GlobalInvocationId => BuiltIn::GlobalInvocationId,
                B::LocalInvocationId => BuiltIn::LocalInvocationId,
                B::WorkgroupId => BuiltIn::WorkGroupId,
                B::NumWorkgroups => BuiltIn::NumWorkGroups,
                B::VertexIndex => BuiltIn::VertexIndex,
                B::InstanceIndex => BuiltIn::InstanceIndex,
                B::Position => BuiltIn::Position { invariant: false },
            };
            let inner = match b {
                B::GlobalInvocationId
                | B::LocalInvocationId
                | B::WorkgroupId
                | B::NumWorkgroups => {
                    TypeInner::Vector { size: VectorSize::Tri, scalar: Scalar::U32 }
                }
                B::VertexIndex | B::InstanceIndex => TypeInner::Scalar(Scalar::U32),
                B::Position => TypeInner::Vector { size: VectorSize::Quad, scalar: Scalar::F32 },
            };
            let ty = self.cx.ty_inner(inner);
            self.func.arguments.push(FunctionArgument {
                name: Some(format!("input{i}")),
                ty,
                binding: Some(Binding::BuiltIn(builtin)),
            });
        }
        self.param_base = self.func.arguments.len() as u32;
        match &e.stage {
            ir::Stage::Compute { .. } => {}
            ir::Stage::Vertex { position_field, flat } => {
                let rt = self.f.ret.ok_or("internal: a vertex shader with no output")?;
                let (io, map) = self.cx.io_struct(rt, *position_field, flat)?;
                self.func.result = Some(FunctionResult { ty: io, binding: None });
                self.io = Some((io, map, rt));
            }
            ir::Stage::Fragment { varyings, position_field, flat } => {
                if let (Some(vt), Some(pf)) = (varyings, position_field) {
                    let (io, map) = self.cx.io_struct(*vt, *pf, flat)?;
                    self.func.arguments.push(FunctionArgument {
                        name: Some("varyings".into()),
                        ty: io,
                        binding: None,
                    });
                    self.io = Some((io, map, *vt));
                }
                let v4 = self.cx.ty_vec4();
                self.func.result = Some(FunctionResult {
                    ty: v4,
                    binding: Some(Binding::Location {
                        location: 0,
                        interpolation: None,
                        sampling: None,
                        blend_src: None,
                        per_primitive: false,
                    }),
                });
            }
        }
        Ok(())
    }

    /// Appends an expression; emits it if it must be.
    fn expr(&mut self, e: Expression, block: &mut Block) -> Handle<Expression> {
        let pre = e.needs_pre_emit();
        let h = self.func.expressions.append(e, Span::UNDEFINED);
        if !pre {
            block.push(Statement::Emit(naga::Range::new_from_bounds(h, h)), Span::UNDEFINED);
        }
        h
    }

    fn lit(&mut self, l: Literal, block: &mut Block) -> Handle<Expression> {
        self.expr(Expression::Literal(l), block)
    }

    /// `h`'s value, read back from a new variable: never a const-expression.
    fn through_variable(
        &mut self,
        h: Handle<Expression>,
        ty: Handle<Type>,
        out: &mut Block,
    ) -> Handle<Expression> {
        let var = self
            .func
            .local_variables
            .append(LocalVariable { name: None, ty, init: None }, Span::UNDEFINED);
        let pointer = self.expr(Expression::LocalVariable(var), out);
        out.push(Statement::Store { pointer, value: h }, Span::UNDEFINED);
        self.expr(Expression::Load { pointer }, out)
    }

    /// An index into a value of `len` elements (`None`: not known here). WGSL rejects a
    /// const-expression index it can see is out of range, so one not known to be in range goes
    /// through a variable: at run time, robust access gives a value from inside (§11).
    fn index(
        &mut self,
        i: ir::ValueId,
        len: Option<u32>,
        out: &mut Block,
    ) -> R<Handle<Expression>> {
        let h = self.val(i)?;
        if !self.consts.contains(&i) {
            return Ok(h);
        }
        let inside = match (self.ints.get(&i), len) {
            (Some(&k), Some(n)) => (0..i64::from(n)).contains(&k),
            _ => false,
        };
        if inside {
            return Ok(h);
        }
        let ty = self.cx.ty(self.f.value_ty(i))?;
        Ok(self.through_variable(h, ty, out))
    }

    /// The operands of an operation. When every one is a const-expression, the first goes
    /// through a variable, so the operation isn't one.
    fn operands<const N: usize>(
        &mut self,
        vs: [ir::ValueId; N],
        out: &mut Block,
    ) -> R<[Handle<Expression>; N]> {
        let hs = self.operands_of(&vs, out)?;
        hs.try_into().map_err(|_| "internal: operands".to_string())
    }

    fn operands_of(&mut self, vs: &[ir::ValueId], out: &mut Block) -> R<Vec<Handle<Expression>>> {
        let mut hs = vs.iter().map(|&v| self.val(v)).collect::<R<Vec<_>>>()?;
        if let Some(&first) = vs.first()
            && vs.iter().all(|v| self.consts.contains(v))
        {
            let ty = self.cx.ty(self.f.value_ty(first))?;
            hs[0] = self.through_variable(hs[0], ty, out);
        }
        Ok(hs)
    }

    /// Marks `v` a const-expression if all its `parts` are: it only gathers them.
    fn const_if_all(&mut self, v: ir::ValueId, parts: &[ir::ValueId]) {
        if parts.iter().all(|p| self.consts.contains(p)) {
            self.consts.insert(v);
        }
    }

    fn val(&self, v: ir::ValueId) -> R<Handle<Expression>> {
        self.values
            .get(&v)
            .copied()
            .ok_or_else(|| format!("internal: v{} has no naga expression", v.0))
    }

    /// A pointer to a place.
    fn place(&mut self, p: &ir::Place, block: &mut Block) -> R<Handle<Expression>> {
        let m = self.cx.m;
        let mut h = match &p.root {
            ir::PlaceRoot::Local(l) => {
                self.expr(Expression::LocalVariable(self.locals[l.index()]), block)
            }
            ir::PlaceRoot::Param(i) => {
                self.expr(Expression::FunctionArgument(self.param_base + *i), block)
            }
            ir::PlaceRoot::Resource(r) => {
                let g = *self.cx.globals.get(r.index()).ok_or("internal: a missing resource")?;
                self.expr(Expression::GlobalVariable(g), block)
            }
            ir::PlaceRoot::Ptr(_) => {
                return Err(
                    "projections returned from functions aren't supported on the GPU".into()
                );
            }
            ir::PlaceRoot::Data(_) => return Err("internal: constant data in GPU code".into()),
        };
        // The type each projection applies to, from where `place_ty` starts (past a storage
        // buffer's index): a field's member depends on it.
        let start = m.place_start(self.f, p);
        let skip = start.map_or(0, |(_, rest)| p.path.len() - rest.len());
        let mut t = start.map(|(t, _)| t);
        for (i, proj) in p.path.iter().enumerate() {
            h = match proj {
                ir::Proj::Field(k) => {
                    let t = t.ok_or("internal: a place's type")?;
                    let index = member(&m.types, t, *k);
                    self.expr(Expression::AccessIndex { base: h, index }, block)
                }
                ir::Proj::Comp(c) => {
                    self.expr(Expression::AccessIndex { base: h, index: *c as u32 }, block)
                }
                ir::Proj::Index(v) => {
                    let len = t.filter(|_| i >= skip).and_then(|t| static_len(&m.types, t));
                    let index = self.index(*v, len, block)?;
                    self.expr(Expression::Access { base: h, index }, block)
                }
            };
            if i >= skip {
                t = t.and_then(|t| m.proj_ty(t, proj));
            }
        }
        Ok(h)
    }

    fn block(&mut self, b: &ir::Block) -> R<Block> {
        let mut out = Block::new();
        for s in b {
            self.stmt(s, &mut out)?;
        }
        Ok(out)
    }

    fn stmt(&mut self, s: &ir::Stmt, out: &mut Block) -> R<()> {
        match s {
            ir::Stmt::Let(v, e) => {
                let h = self.value(*v, e, out)?;
                self.values.insert(*v, h);
                // Arithmetic is written where it's used (naga binds what's used twice to a
                // `let` of its own); what reads memory, calls or has an effect is bound where
                // it is, so it runs in the IR's order.
                if !self.func.expressions[h].needs_pre_emit() && !pure(e) {
                    self.func.named_expressions.insert(h, format!("v{}", v.0));
                }
            }
            ir::Stmt::Eval(e) => match e {
                ir::Expr::Call(g, args) => {
                    self.call(*g, args, out)?;
                }
                ir::Expr::Host(..) => return Err("GPU code can't record GPU work".into()),
                ir::Expr::Mem(..) => return Err("GPU code can't use raw memory".into()),
                ir::Expr::Barrier => {
                    out.push(Statement::ControlBarrier(naga::Barrier::WORK_GROUP), Span::UNDEFINED);
                }
                ir::Expr::Atomic(op, p, args) => {
                    self.atomic(*op, p, args, None, out)?;
                }
                _ => {}
            },
            ir::Stmt::Store(p, v) => {
                let pointer = self.place(p, out)?;
                let value = self.val(*v)?;
                out.push(Statement::Store { pointer, value }, Span::UNDEFINED);
            }
            ir::Stmt::If { cond, then, else_ } => {
                let condition = self.val(*cond)?;
                let accept = self.block(then)?;
                let reject = self.block(else_)?;
                out.push(Statement::If { condition, accept, reject }, Span::UNDEFINED);
            }
            ir::Stmt::Loop { body, continuing } => {
                let body = self.block(body)?;
                let continuing = self.block(continuing)?;
                out.push(Statement::Loop { body, continuing, break_if: None }, Span::UNDEFINED);
            }
            ir::Stmt::Break => out.push(Statement::Break, Span::UNDEFINED),
            ir::Stmt::Continue => out.push(Statement::Continue, Span::UNDEFINED),
            ir::Stmt::Return(v) => {
                let value = match v {
                    Some(v) => {
                        let h = self.val(*v)?;
                        Some(self.output(h, out)?)
                    }
                    None => None,
                };
                out.push(Statement::Return { value }, Span::UNDEFINED);
            }
            ir::Stmt::At(_) => {}
            // GPU code can't trap (its integers wrap, and its indexing is robust), so a trap is a
            // point valid code never reaches: an exhaustive `match`'s last test, the end of a
            // loop left only by `return`. Returning there would be a return under control flow
            // that varies between invocations, inlined into the entry point, after which WGSL
            // forbids derivatives (`fwidth` after a sphere-tracing loop).
            ir::Stmt::Trap => {}
        }
        Ok(())
    }

    /// A call to a function that stays one: its result, if it has one. (A call result is
    /// defined by the call statement, so it isn't emitted.)
    fn call(
        &mut self,
        g: ir::FuncId,
        args: &[ir::Arg],
        out: &mut Block,
    ) -> R<Option<Handle<Expression>>> {
        let function =
            *self.cx.fns.get(&g).ok_or("internal: a call to a function that was inlined")?;
        let mut arguments = Vec::new();
        for a in args {
            match a {
                ir::Arg::Value(v) => arguments.push(self.val(*v)?),
                ir::Arg::Place(_) => {
                    return Err("internal: a place passed to a called GPU function".into());
                }
            }
        }
        let result = self.cx.m.functions[g.index()].ret.map(|_| {
            self.func.expressions.append(Expression::CallResult(function), Span::UNDEFINED)
        });
        out.push(Statement::Call { function, arguments, result }, Span::UNDEFINED);
        Ok(result)
    }

    /// `return` with a zero of the function's result, or a bare `return`.
    fn return_zero(&mut self, out: &mut Block) {
        let value = self
            .func
            .result
            .as_ref()
            .map(|r| r.ty)
            .map(|t| self.expr(Expression::ZeroValue(t), out));
        out.push(Statement::Return { value }, Span::UNDEFINED);
    }

    /// A vertex shader's return value, rebuilt as the output struct.
    fn output(&mut self, h: Handle<Expression>, out: &mut Block) -> R<Handle<Expression>> {
        let Some((io, map, rt)) = self.io.clone() else { return Ok(h) };
        let Some(ir::EntryPoint { stage: ir::Stage::Vertex { .. }, .. }) = self.entry else {
            return Ok(h);
        };
        let mut comps = Vec::new();
        for (i, _) in map {
            let field = self.expr(Expression::AccessIndex { base: h, index: i }, out);
            // A `Flat<T>`'s value, or a `ClipPosition` field's `position`: the one field of a
            // struct. (A bare `ClipPosition` output's field is the position itself.)
            let v = if wrapped(&self.cx.m.types, rt, i)? {
                self.expr(Expression::AccessIndex { base: field, index: 0 }, out)
            } else {
                field
            };
            comps.push(v);
        }
        Ok(self.expr(Expression::Compose { ty: io, components: comps }, out))
    }

    fn value(&mut self, v: ir::ValueId, e: &ir::Expr, out: &mut Block) -> R<Handle<Expression>> {
        let t = self.f.value_ty(v);
        let types = &self.cx.m.types;
        Ok(match e {
            ir::Expr::Const(ir::Const::F32(x)) if !x.is_finite() || x.abs() == f32::MAX => {
                // Not a WGSL literal: its bits, made a float when the shader runs. So is
                // ±f32::MAX: the writer prints its shortest decimal in full, 3402823500…0, which
                // is above the largest f32, and Tint rejects it.
                let bits = self.lit(Literal::U32(x.to_bits()), out);
                let u = self.cx.ty_inner(TypeInner::Scalar(Scalar::U32));
                let bits = self.through_variable(bits, u, out);
                self.expr(
                    Expression::As { expr: bits, kind: ScalarKind::Float, convert: None },
                    out,
                )
            }
            ir::Expr::Const(c) => {
                let l = match c {
                    ir::Const::Bool(b) => Literal::Bool(*b),
                    ir::Const::I32(x) => {
                        self.ints.insert(v, (*x).into());
                        Literal::I32(*x)
                    }
                    ir::Const::U32(x) => {
                        self.ints.insert(v, (*x).into());
                        Literal::U32(*x)
                    }
                    ir::Const::F32(x) => {
                        self.floats.insert(v, *x);
                        // A float naga would write in many digits (it writes no exponents) is a
                        // module constant, written once: interval code widens by 2⁻²³ and by
                        // the smallest normal float at every step.
                        if format!("{x}").len() > 14 {
                            self.consts.insert(v);
                            let k = self.cx.float_const(*x);
                            return Ok(self.expr(Expression::Constant(k), out));
                        }
                        Literal::F32(*x)
                    }
                    other => return Err(format!("internal: the constant {other:?} in GPU code")),
                };
                self.consts.insert(v);
                self.lit(l, out)
            }
            ir::Expr::Zero(t) => {
                let ty = self.cx.ty(*t)?;
                self.consts.insert(v);
                self.expr(Expression::ZeroValue(ty), out)
            }
            ir::Expr::Param(i) => {
                // In a fragment shader, parameter 0 is the varyings: rebuild the IR struct.
                if let Some((_, map, vt)) = self.io.clone()
                    && let Some(ir::EntryPoint { stage: ir::Stage::Fragment { .. }, .. }) =
                        self.entry
                {
                    let arg = self.expr(Expression::FunctionArgument(self.param_base + *i), out);
                    let ir::TypeDef::Struct { fields, .. } = types.get(vt) else {
                        return Err("internal: varyings".into());
                    };
                    let mut comps = Vec::new();
                    for (k, (i, _)) in map.into_iter().enumerate() {
                        let member =
                            self.expr(Expression::AccessIndex { base: arg, index: k as u32 }, out);
                        let ft = fields[i as usize].1;
                        comps.push(if wrapped(types, vt, i)? {
                            let fty = self.cx.ty(ft)?;
                            self.expr(
                                Expression::Compose { ty: fty, components: vec![member] },
                                out,
                            )
                        } else {
                            member
                        });
                    }
                    let ty = self.cx.ty(vt)?;
                    self.expr(Expression::Compose { ty, components: comps }, out)
                } else {
                    self.expr(Expression::FunctionArgument(self.param_base + *i), out)
                }
            }
            ir::Expr::EntryInput(i) => {
                let arg = match self.inputs.get(*i as usize) {
                    Some(Some(k)) => self.expr(Expression::FunctionArgument(*k), out),
                    // The fragment's position, which the varyings hold.
                    Some(None) => {
                        let (
                            Some((_, map, _)),
                            Some(ir::EntryPoint {
                                stage: ir::Stage::Fragment { position_field: Some(pf), .. },
                                ..
                            }),
                        ) = (&self.io, self.entry)
                        else {
                            return Err("internal: an input with no argument".into());
                        };
                        let k = map.iter().position(|&(f, _)| f == *pf).unwrap_or(0) as u32;
                        let v = self.expr(Expression::FunctionArgument(self.param_base), out);
                        self.expr(Expression::AccessIndex { base: v, index: k }, out)
                    }
                    None => return Err("internal: a missing entry input".into()),
                };
                let ty = self.cx.ty(t)?;
                match types.get(t) {
                    ir::TypeDef::Struct { fields, .. } if fields.len() == 3 => {
                        let comps = (0..3)
                            .map(|k| {
                                self.expr(Expression::AccessIndex { base: arg, index: k }, out)
                            })
                            .collect();
                        self.expr(Expression::Compose { ty, components: comps }, out)
                    }
                    _ => self.expr(Expression::Compose { ty, components: vec![arg] }, out),
                }
            }
            ir::Expr::Load(p) => {
                let pointer = self.place(p, out)?;
                self.expr(Expression::Load { pointer }, out)
            }
            ir::Expr::ArrayLength(p) => {
                let pointer = self.place(p, out)?;
                self.expr(Expression::ArrayLength(pointer), out)
            }
            ir::Expr::Texture(op, tex, sampler, args) => {
                self.texture(*op, *tex, *sampler, args, out)?
            }
            ir::Expr::Atomic(op, p, args) => {
                let t = self.f.value_ty(v);
                self.atomic(*op, p, args, Some(t), out)?.ok_or("internal: an atomic's value")?
            }
            ir::Expr::Barrier => return Err("internal: a barrier's value".into()),
            ir::Expr::Call(g, args) => self
                .call(*g, args, out)?
                .ok_or("internal: a call's value from a function that returns nothing")?,
            ir::Expr::Construct(ty, parts) => {
                let components = parts.iter().map(|p| self.val(*p)).collect::<R<Vec<_>>>()?;
                let ty = self.cx.ty(*ty)?;
                self.const_if_all(v, parts);
                self.expr(Expression::Compose { ty, components }, out)
            }
            ir::Expr::Extract(x, i) => {
                let base = self.val(*x)?;
                let index = member(types, self.f.value_ty(*x), *i);
                self.const_if_all(v, &[*x]);
                self.expr(Expression::AccessIndex { base, index }, out)
            }
            ir::Expr::Variant(t, k, payload) => {
                let ir::TypeDef::Enum { variants, .. } = types.get(*t) else {
                    return Err("internal: a variant of a type that isn't an enum".into());
                };
                let ty = self.cx.ty(*t)?;
                let tag = self.lit(Literal::U32(*k), out);
                self.const_if_all(v, payload.as_slice());
                let mut components = vec![tag];
                for (v, (_, p)) in variants.iter().enumerate() {
                    let Some(pt) = p else { continue };
                    components.push(match payload {
                        Some(x) if v == *k as usize => self.val(*x)?,
                        _ => {
                            let pty = self.cx.ty(*pt)?;
                            self.expr(Expression::ZeroValue(pty), out)
                        }
                    });
                }
                self.expr(Expression::Compose { ty, components }, out)
            }
            ir::Expr::ExtractDyn(x, i) => {
                let [base, _] = self.operands([*x, *i], out)?;
                let index = self.index(*i, static_len(types, self.f.value_ty(*x)), out)?;
                self.expr(Expression::Access { base, index }, out)
            }
            ir::Expr::Splat(x, n) => {
                let value = self.val(*x)?;
                self.const_if_all(v, &[*x]);
                self.expr(Expression::Splat { size: vsize(*n), value }, out)
            }
            ir::Expr::Swizzle(x, comps) => {
                let vector = self.val(*x)?;
                self.const_if_all(v, &[*x]);
                let mut pattern = [naga::SwizzleComponent::X; 4];
                for (k, c) in comps.iter().enumerate() {
                    pattern[k] = match c {
                        0 => naga::SwizzleComponent::X,
                        1 => naga::SwizzleComponent::Y,
                        2 => naga::SwizzleComponent::Z,
                        _ => naga::SwizzleComponent::W,
                    };
                }
                self.expr(
                    Expression::Swizzle { size: vsize(comps.len() as u8), vector, pattern },
                    out,
                )
            }
            ir::Expr::Convert(x, s) => {
                let [expr] = self.operands([*x], out)?;
                let sc = scalar(*s)?;
                self.expr(Expression::As { expr, kind: sc.kind, convert: Some(sc.width) }, out)
            }
            ir::Expr::Bitcast(x, s) => {
                let [expr] = self.operands([*x], out)?;
                let sc = scalar(*s)?;
                self.expr(Expression::As { expr, kind: sc.kind, convert: None }, out)
            }
            ir::Expr::Select { cond, if_true, if_false } => {
                let [condition, accept, reject] =
                    self.operands([*cond, *if_true, *if_false], out)?;
                if matches!(
                    types.get(t),
                    ir::TypeDef::Struct { .. }
                        | ir::TypeDef::Enum { .. }
                        | ir::TypeDef::Array(..)
                        | ir::TypeDef::Matrix(_)
                ) {
                    // WGSL's select is for scalars and vectors: use a variable.
                    let ty = self.cx.ty(t)?;
                    let var = self
                        .func
                        .local_variables
                        .append(LocalVariable { name: None, ty, init: None }, Span::UNDEFINED);
                    let ptr = self.expr(Expression::LocalVariable(var), out);
                    let mut a = Block::new();
                    a.push(Statement::Store { pointer: ptr, value: accept }, Span::UNDEFINED);
                    let mut r = Block::new();
                    r.push(Statement::Store { pointer: ptr, value: reject }, Span::UNDEFINED);
                    out.push(Statement::If { condition, accept: a, reject: r }, Span::UNDEFINED);
                    self.expr(Expression::Load { pointer: ptr }, out)
                } else {
                    self.expr(Expression::Select { condition, accept, reject }, out)
                }
            }
            ir::Expr::Unary(op, x) => {
                let [expr] = self.operands([*x], out)?;
                let op = match (op, types.get(self.f.value_ty(*x))) {
                    // WGSL negates no matrix: scale it by -1, which flips every sign as `-`
                    // does (zeros included).
                    (ir::UnOp::Neg, ir::TypeDef::Matrix(_)) => {
                        let m1 = self.lit(Literal::F32(-1.0), out);
                        let e = Expression::Binary {
                            op: BinaryOperator::Multiply,
                            left: expr,
                            right: m1,
                        };
                        return Ok(self.expr(e, out));
                    }
                    (ir::UnOp::Neg, _) => UnaryOperator::Negate,
                    (ir::UnOp::Not, ir::TypeDef::Scalar(ir::Scalar::Bool)) => {
                        UnaryOperator::LogicalNot
                    }
                    (ir::UnOp::Not, _) => UnaryOperator::BitwiseNot,
                };
                self.expr(Expression::Unary { op, expr }, out)
            }
            ir::Expr::Binary(op, a, b) => {
                let ta = self.f.value_ty(*a);
                let [left, mut right] = self.operands([*a, *b], out)?;
                let is_bool = matches!(types.get(ta), ir::TypeDef::Scalar(ir::Scalar::Bool));
                let is_int =
                    matches!(types.get(ta), ir::TypeDef::Scalar(ir::Scalar::I32 | ir::Scalar::U32));
                // WGSL rejects a constant zero divisor or a constant shift past the width even
                // when the other operand isn't constant.
                if is_int
                    && matches!(
                        op,
                        ir::BinOp::Div | ir::BinOp::Rem | ir::BinOp::Shl | ir::BinOp::Shr
                    )
                    && self.consts.contains(b)
                {
                    let ty = self.cx.ty(self.f.value_ty(*b))?;
                    right = self.through_variable(right, ty, out);
                }
                let bop = match op {
                    ir::BinOp::Add | ir::BinOp::WrappingAdd => BinaryOperator::Add,
                    ir::BinOp::Sub | ir::BinOp::WrappingSub => BinaryOperator::Subtract,
                    ir::BinOp::Mul | ir::BinOp::WrappingMul => BinaryOperator::Multiply,
                    ir::BinOp::Div => BinaryOperator::Divide,
                    ir::BinOp::Rem => BinaryOperator::Modulo,
                    ir::BinOp::Shl => BinaryOperator::ShiftLeft,
                    ir::BinOp::Shr => BinaryOperator::ShiftRight,
                    ir::BinOp::BitAnd => BinaryOperator::And,
                    ir::BinOp::BitOr => BinaryOperator::InclusiveOr,
                    // WGSL's `^` is for integers; on bools it's `!=`.
                    ir::BinOp::BitXor if is_bool => BinaryOperator::NotEqual,
                    ir::BinOp::BitXor => BinaryOperator::ExclusiveOr,
                    ir::BinOp::And => {
                        if is_bool {
                            BinaryOperator::LogicalAnd
                        } else {
                            BinaryOperator::And
                        }
                    }
                    ir::BinOp::Or => {
                        if is_bool {
                            BinaryOperator::LogicalOr
                        } else {
                            BinaryOperator::InclusiveOr
                        }
                    }
                    ir::BinOp::Eq => BinaryOperator::Equal,
                    ir::BinOp::Ne => BinaryOperator::NotEqual,
                    ir::BinOp::Lt => BinaryOperator::Less,
                    ir::BinOp::Le => BinaryOperator::LessEqual,
                    ir::BinOp::Gt => BinaryOperator::Greater,
                    ir::BinOp::Ge => BinaryOperator::GreaterEqual,
                };
                self.expr(Expression::Binary { op: bop, left, right }, out)
            }
            ir::Expr::Builtin(b, args) => self.builtin(*b, args, out)?,
            ir::Expr::Host(..) => return Err("GPU code can't record GPU work".into()),
            ir::Expr::Mem(..) => return Err("GPU code can't use raw memory".into()),
            ir::Expr::Run(_) | ir::Expr::Addr(_) => {
                return Err("internal: a CPU-only expression in GPU code".into());
            }
        })
    }

    /// An atomic read-modify-write (`ir::Expr::Atomic`): the value that was there, if it's
    /// wanted, as type `want` (a compare-exchange's: a struct of it and whether it stored).
    fn atomic(
        &mut self,
        op: ir::AtomicOp,
        p: &ir::Place,
        args: &[ir::ValueId],
        want: Option<ir::TypeId>,
        out: &mut Block,
    ) -> R<Option<Handle<Expression>>> {
        use ir::AtomicOp as A;
        let m = self.cx.m;
        let pt = m.place_ty(self.f, p).ok_or("internal: an atomic place's type")?;
        let ir::TypeDef::Atomic(s) = *m.types.get(pt) else {
            return Err("internal: an atomic operation on something that isn't atomic".into());
        };
        let s = scalar(s)?;
        let pointer = self.place(p, out)?;
        // `atomicLoad` and `atomicStore` are a load and a store through an atomic's pointer.
        if op == A::Load {
            return Ok(Some(self.expr(Expression::Load { pointer }, out)));
        }
        let first = self.val(*args.first().ok_or("internal: an atomic without an operand")?)?;
        if op == A::Store {
            out.push(Statement::Store { pointer, value: first }, Span::UNDEFINED);
            return Ok(None);
        }
        let fun = match op {
            A::Add => naga::AtomicFunction::Add,
            A::Sub => naga::AtomicFunction::Subtract,
            A::Min => naga::AtomicFunction::Min,
            A::Max => naga::AtomicFunction::Max,
            A::And => naga::AtomicFunction::And,
            A::Or => naga::AtomicFunction::InclusiveOr,
            A::Xor => naga::AtomicFunction::ExclusiveOr,
            A::Exchange => naga::AtomicFunction::Exchange { compare: None },
            A::CompareExchange => naga::AtomicFunction::Exchange { compare: Some(first) },
            A::Load | A::Store => unreachable!("handled above"),
        };
        // A compare-exchange's new value is its second operand; the others' is their first.
        let comparison = op == A::CompareExchange;
        let value = if comparison {
            self.val(*args.get(1).ok_or("internal: a compare-exchange without its new value")?)?
        } else {
            first
        };
        let result = if want.is_some() || comparison {
            let ty = if comparison {
                let special = naga::PredeclaredType::AtomicCompareExchangeWeakResult(s);
                self.cx.out.generate_predeclared_type(special)
            } else {
                self.cx.ty_inner(TypeInner::Scalar(s))
            };
            let r = Expression::AtomicResult { ty, comparison };
            Some(self.func.expressions.append(r, Span::UNDEFINED))
        } else {
            None
        };
        out.push(Statement::Atomic { pointer, fun, value, result }, Span::UNDEFINED);
        Ok(match (result, comparison, want) {
            // The old value and whether it stored: the result struct's members, as the IR's
            // struct.
            (Some(r), true, Some(t)) => {
                let old = self.expr(Expression::AccessIndex { base: r, index: 0 }, out);
                let stored = self.expr(Expression::AccessIndex { base: r, index: 1 }, out);
                let ty = self.cx.ty(t)?;
                Some(self.expr(Expression::Compose { ty, components: vec![old, stored] }, out))
            }
            (r, _, _) => r,
        })
    }

    /// A texture read (`ir::Expr::Texture`). Images and samplers are their globals' handles.
    fn texture(
        &mut self,
        op: ir::TextureOp,
        tex: ir::ResourceId,
        sampler: Option<ir::ResourceId>,
        args: &[ir::ValueId],
        out: &mut Block,
    ) -> R<Handle<Expression>> {
        use ir::TextureOp as T;
        let global = |fb: &mut Self, r: ir::ResourceId, out: &mut Block| -> R<Handle<Expression>> {
            let g = *fb.cx.globals.get(r.index()).ok_or("internal: a missing resource")?;
            Ok(fb.expr(Expression::GlobalVariable(g), out))
        };
        let image = global(self, tex, out)?;
        let arg = |fb: &Self, i: usize| -> R<Handle<Expression>> {
            fb.val(*args.get(i).ok_or("internal: a texture read without its arguments")?)
        };
        Ok(match op {
            T::Sample | T::SampleLevel | T::SampleCompare | T::SampleCompareLevel => {
                let s = sampler.ok_or("internal: sampling without a sampler")?;
                let sampler = global(self, s, out)?;
                let coordinate = arg(self, 0)?;
                let (level, depth_ref) = match op {
                    T::Sample => (naga::SampleLevel::Auto, None),
                    T::SampleLevel => (naga::SampleLevel::Exact(arg(self, 1)?), None),
                    T::SampleCompare => (naga::SampleLevel::Auto, Some(arg(self, 1)?)),
                    _ => (naga::SampleLevel::Zero, Some(arg(self, 1)?)),
                };
                self.expr(
                    Expression::ImageSample {
                        image,
                        sampler,
                        gather: None,
                        coordinate,
                        array_index: None,
                        offset: None,
                        level,
                        depth_ref,
                        clamp_to_edge: false,
                    },
                    out,
                )
            }
            T::Load => {
                // A texel's coordinates are signed in naga's IR.
                let mut signed = Vec::new();
                for i in 0..2 {
                    let x = arg(self, i)?;
                    let kind = ScalarKind::Sint;
                    signed.push(self.expr(Expression::As { expr: x, kind, convert: Some(4) }, out));
                }
                let ty = self
                    .cx
                    .ty_inner(TypeInner::Vector { size: VectorSize::Bi, scalar: Scalar::I32 });
                let coordinate = self.expr(Expression::Compose { ty, components: signed }, out);
                let level = self.lit(Literal::I32(0), out);
                self.expr(
                    Expression::ImageLoad {
                        image,
                        coordinate,
                        array_index: None,
                        sample: None,
                        level: Some(level),
                    },
                    out,
                )
            }
            T::Width | T::Height => {
                let query = naga::ImageQuery::Size { level: None };
                let size = self.expr(Expression::ImageQuery { image, query }, out);
                let index = u32::from(op == T::Height);
                self.expr(Expression::AccessIndex { base: size, index }, out)
            }
        })
    }

    fn builtin(
        &mut self,
        b: ir::Builtin,
        args: &[ir::ValueId],
        out: &mut Block,
    ) -> R<Handle<Expression>> {
        use ir::Builtin as B;
        let mut hs = self.operands_of(args, out)?;
        // WGSL rejects constant bounds that are the wrong way round, even with a varying `x`
        // (`clamp(x, 1.0, 0.0)`, `smoothstep(0.5, 0.5, x)`): then the upper one goes through a
        // variable, and the call computes what the CPU's does.
        let bounds = match b {
            B::Clamp => Some((1, 2, false)),
            B::Smoothstep => Some((0, 1, true)),
            _ => None,
        };
        if let Some((l, h, strict)) = bounds
            && self.consts.contains(&args[l])
            && self.consts.contains(&args[h])
        {
            let wrong = match (self.floats.get(&args[l]), self.floats.get(&args[h])) {
                (Some(lo), Some(hi)) => lo > hi || (strict && lo == hi),
                // Constant vectors: their components aren't kept, so assume the worst.
                _ => true,
            };
            if wrong {
                let ty = self.cx.ty(self.f.value_ty(args[h]))?;
                hs[h] = self.through_variable(hs[h], ty, out);
            }
        }
        let deriv = |axis| Expression::Derivative {
            axis,
            ctrl: naga::DerivativeControl::None,
            expr: hs[0],
        };
        let e = match b {
            B::Dpdx => deriv(naga::DerivativeAxis::X),
            B::Dpdy => deriv(naga::DerivativeAxis::Y),
            B::Fwidth => deriv(naga::DerivativeAxis::Width),
            B::AllEqual => {
                let eq = self.expr(
                    Expression::Binary { op: BinaryOperator::Equal, left: hs[0], right: hs[1] },
                    out,
                );
                Expression::Relational { fun: naga::RelationalFunction::All, argument: eq }
            }
            // WGSL's are of the argument's type; the result is a `u32` (an `i32`'s count is
            // never negative, so it's the same bits).
            B::CountOnes | B::LeadingZeros | B::TrailingZeros => {
                let fun = match b {
                    B::CountOnes => MathFunction::CountOneBits,
                    B::LeadingZeros => MathFunction::CountLeadingZeros,
                    _ => MathFunction::CountTrailingZeros,
                };
                let count = self.expr(
                    Expression::Math { fun, arg: hs[0], arg1: None, arg2: None, arg3: None },
                    out,
                );
                if self.cx.m.types.as_scalar(self.f.value_ty(args[0])) == Some(ir::Scalar::U32) {
                    return Ok(count);
                }
                Expression::As { expr: count, kind: ScalarKind::Uint, convert: None }
            }
            _ => {
                let fun = match b {
                    B::Sqrt => MathFunction::Sqrt,
                    B::InverseSqrt => MathFunction::InverseSqrt,
                    B::Sin => MathFunction::Sin,
                    B::Cos => MathFunction::Cos,
                    B::Tan => MathFunction::Tan,
                    B::Asin => MathFunction::Asin,
                    B::Acos => MathFunction::Acos,
                    B::Atan => MathFunction::Atan,
                    B::Atan2 => MathFunction::Atan2,
                    B::Exp => MathFunction::Exp,
                    B::Exp2 => MathFunction::Exp2,
                    B::Log => MathFunction::Log,
                    B::Log2 => MathFunction::Log2,
                    B::Pow => MathFunction::Pow,
                    B::Floor => MathFunction::Floor,
                    B::Ceil => MathFunction::Ceil,
                    B::Round => MathFunction::Round,
                    B::Trunc => MathFunction::Trunc,
                    B::Fract => MathFunction::Fract,
                    B::Abs => MathFunction::Abs,
                    B::Sign => MathFunction::Sign,
                    B::Min => MathFunction::Min,
                    B::Max => MathFunction::Max,
                    B::Clamp => MathFunction::Clamp,
                    B::Saturate => MathFunction::Saturate,
                    B::Mix => MathFunction::Mix,
                    B::Step => MathFunction::Step,
                    B::Smoothstep => MathFunction::SmoothStep,
                    B::Length => MathFunction::Length,
                    B::Distance => MathFunction::Distance,
                    B::Dot => MathFunction::Dot,
                    B::Cross => MathFunction::Cross,
                    B::Normalize => MathFunction::Normalize,
                    B::Dpdx
                    | B::Dpdy
                    | B::Fwidth
                    | B::AllEqual
                    | B::CountOnes
                    | B::LeadingZeros
                    | B::TrailingZeros => unreachable!("handled above"),
                };
                Expression::Math {
                    fun,
                    arg: hs[0],
                    arg1: hs.get(1).copied(),
                    arg2: hs.get(2).copied(),
                    arg3: None,
                }
            }
        };
        Ok(self.expr(e, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny compute module named `name`, which writes each invocation's index as a float.
    fn kernel(name: &str) -> ir::Module {
        let mut m = ir::Module::default();
        let u = m.types.u32();
        let f32t = m.types.f32();
        let gid = m.types.intern(ir::TypeDef::Struct {
            name: "GlobalId".into(),
            fields: vec![("x".into(), u), ("y".into(), u), ("z".into(), u)],
        });
        m.resources.push(ir::Resource {
            name: "out".into(),
            binding: 1,
            kind: ir::ResourceKind::StorageReadWrite,
            ty: f32t,
        });
        let mut f = ir::Function::new(name, Vec::new(), None);
        let id = f.new_value(gid);
        let x = f.new_value(u);
        let xf = f.new_value(f32t);
        f.body = vec![
            ir::Stmt::Let(id, ir::Expr::EntryInput(0)),
            ir::Stmt::Let(x, ir::Expr::Extract(id, 0)),
            ir::Stmt::Let(xf, ir::Expr::Convert(x, ir::Scalar::F32)),
            ir::Stmt::Store(
                ir::Place {
                    root: ir::PlaceRoot::Resource(ir::ResourceId(0)),
                    path: vec![ir::Proj::Index(x)],
                },
                xf,
            ),
            ir::Stmt::Return(None),
        ];
        let fid = m.add_function(f);
        m.entry_points.push(ir::EntryPoint {
            name: name.into(),
            stage: ir::Stage::Compute { workgroup_size: [64, 1, 1] },
            function: fid,
            inputs: vec![ir::BuiltinInput::GlobalInvocationId],
        });
        m
    }

    /// A tiny compute module round-trips through naga's validator and back through its WGSL
    /// parser.
    #[test]
    fn a_minimal_kernel_is_valid_wgsl() {
        let wgsl = emit(&kernel("k")).expect("valid").text;
        assert!(wgsl.contains("@compute @workgroup_size(64, 1, 1)"), "{wgsl}");
        naga::front::wgsl::parse_str(&wgsl).expect("parses back");
    }

    /// Every float literal in the shader is a finite f32: ±f32::MAX (an interval's unbounded end
    /// on the GPU) goes through its bits, as infinities do, since its shortest decimal printed in
    /// full is above the largest f32, which Tint rejects.
    #[test]
    fn float_literals_are_in_range() {
        let mut m = kernel("k");
        let f = &mut m.functions[0];
        let f32t = f.value_ty(ir::ValueId(2));
        let mut stores = Vec::new();
        for c in [f32::MAX, -f32::MAX, f32::INFINITY, 1.5] {
            let v = f.new_value(f32t);
            stores.push(ir::Stmt::Let(v, ir::Expr::Const(ir::Const::F32(c))));
            stores.push(ir::Stmt::Store(
                ir::Place {
                    root: ir::PlaceRoot::Resource(ir::ResourceId(0)),
                    path: vec![ir::Proj::Index(ir::ValueId(1))],
                },
                v,
            ));
        }
        let at = f.body.len() - 1;
        f.body.splice(at..at, stores);
        let wgsl = emit(&m).expect("valid").text;
        let mut literals = 0;
        for word in wgsl.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.')) {
            let Some(n) = word.strip_suffix('f') else { continue };
            let Ok(n) = n.parse::<f64>() else { continue };
            literals += 1;
            assert!(n.abs() <= f32::MAX as f64, "{word} is out of range:\n{wgsl}");
        }
        assert!(literals > 0, "{wgsl}");
    }

    /// A trap is a point valid code never reaches: in a branch it's nothing (a return there would
    /// be one under control flow that may vary, after which WGSL forbids derivatives), and only
    /// a body that ends in one returns there.
    #[test]
    fn traps_return_only_at_the_end() {
        let mut m = kernel("k");
        let bt = m.types.bool();
        let f = &mut m.functions[0];
        let c = f.new_value(bt);
        let at = f.body.len() - 1;
        f.body.splice(
            at..at,
            [
                ir::Stmt::Let(c, ir::Expr::Binary(ir::BinOp::Lt, ir::ValueId(1), ir::ValueId(1))),
                ir::Stmt::If { cond: c, then: vec![ir::Stmt::Trap], else_: Vec::new() },
            ],
        );
        let wgsl = emit(&m).expect("valid").text;
        assert_eq!(wgsl.matches("return").count(), 1, "{wgsl}");
        // A body ending in a trap (its last statement) still ends in a return.
        let f = &mut m.functions[0];
        let last = f.body.len() - 1;
        f.body[last] = ir::Stmt::Trap;
        let wgsl = emit(&m).expect("valid").text;
        assert!(wgsl.trim_end().trim_end_matches('}').trim_end().ends_with("return;"), "{wgsl}");
    }

    /// WGSL rejects constant bounds the wrong way round even when `x` varies (Tint does; naga
    /// doesn't): `clamp(x, 1.0, 0.0)` and `smoothstep(0.5, 0.5, x)` pass one through a variable.
    #[test]
    fn wrong_way_constant_bounds_are_not_both_literals() {
        let mut m = kernel("k");
        let f = &mut m.functions[0];
        let f32t = f.value_ty(ir::ValueId(2));
        let mut stmts = Vec::new();
        let c = |f: &mut ir::Function, x: f32, stmts: &mut Vec<ir::Stmt>| {
            let v = f.new_value(f32t);
            stmts.push(ir::Stmt::Let(v, ir::Expr::Const(ir::Const::F32(x))));
            v
        };
        let (one, zero, half) =
            (c(f, 1.0, &mut stmts), c(f, 0.0, &mut stmts), c(f, 0.5, &mut stmts));
        let x = ir::ValueId(2);
        for (b, args) in [
            (ir::Builtin::Clamp, vec![x, one, zero]),
            (ir::Builtin::Smoothstep, vec![half, half, x]),
            (ir::Builtin::Clamp, vec![x, zero, one]),
        ] {
            let r = f.new_value(f32t);
            stmts.push(ir::Stmt::Let(r, ir::Expr::Builtin(b, args)));
            stmts.push(ir::Stmt::Store(
                ir::Place {
                    root: ir::PlaceRoot::Resource(ir::ResourceId(0)),
                    path: vec![ir::Proj::Index(ir::ValueId(1))],
                },
                r,
            ));
        }
        let at = f.body.len() - 1;
        f.body.splice(at..at, stmts);
        let wgsl = emit(&m).expect("valid").text;
        let calls = |name: &str| -> Vec<Vec<String>> {
            wgsl.match_indices(&format!("{name}("))
                .map(|(i, _)| {
                    let rest = &wgsl[i + name.len() + 1..];
                    rest[..rest.find(')').expect("a call")].split(", ").map(String::from).collect()
                })
                .collect()
        };
        let literal = |a: &str| a.ends_with('f') && a.starts_with(|c: char| c.is_ascii_digit());
        let clamps = calls("clamp");
        assert_eq!(clamps.len(), 2, "{wgsl}");
        // clamp(x, 1, 0) has a variable bound; clamp(x, 0, 1), the right way round, keeps both.
        assert!(clamps.iter().any(|a| !(literal(&a[1]) && literal(&a[2]))), "{wgsl}");
        assert!(clamps.iter().any(|a| literal(&a[1]) && literal(&a[2])), "{wgsl}");
        let steps = calls("smoothstep");
        assert!(steps.iter().all(|a| !(literal(&a[0]) && literal(&a[1]))), "{wgsl}");
    }

    /// The WGSL writer renames an entry point that's a WGSL builtin's name or ends in a digit;
    /// the names `emit` gives are the shader's.
    #[test]
    fn entry_point_names_are_the_shaders() {
        for (name, written) in [("fill", "fill"), ("step", "step_"), ("blur2", "blur2_")] {
            let w = emit(&kernel(name)).expect("valid");
            assert_eq!(w.entry_points, [written]);
            assert!(w.text.contains(&format!("fn {written}(")), "{}", w.text);
        }
        // A name a type already has (an entry point's local or another entry point's would do).
        assert_eq!(emit(&kernel("GlobalId")).expect("valid").entry_points, ["GlobalId_1"]);
    }
}
