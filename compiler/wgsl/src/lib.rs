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
//! - Only entry points are written: GPU modules arrive flattened (`wrela_ir::opt` inlines
//!   every call), so a call here is a compiler bug.

use naga::{
    AddressSpace, ArraySize, BinaryOperator, Binding, Block, BuiltIn, Expression, Function,
    FunctionArgument, FunctionResult, GlobalVariable, Handle, Interpolation, Literal,
    LocalVariable, MathFunction, ResourceBinding, Sampling, Scalar, ScalarKind, ShaderStage, Span,
    Statement, StorageAccess, StructMember, Type, TypeInner, UnaryOperator, VectorSize,
};
use std::collections::{HashMap, HashSet};
use wrela_ir as ir;
use wrela_ir::layout::{Layout, pack_layouts, round_up};

type R<T> = Result<T, String>;

const CALL: &str = "internal: a call in a GPU module, which should have been flattened";

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
    /// Each type's layout in the WGSL, and its members' offsets.
    layouts: HashMap<ir::TypeId, (Layout, Vec<u32>)>,
    globals: Vec<Handle<GlobalVariable>>,
}

pub fn to_naga(m: &ir::Module) -> R<naga::Module> {
    let mut cx = Cx {
        m,
        out: naga::Module::default(),
        types: HashMap::new(),
        layouts: HashMap::new(),
        globals: Vec::new(),
    };
    for r in &m.resources {
        let g = cx.global(r)?;
        cx.globals.push(g);
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
    fn ty(&mut self, t: ir::TypeId) -> R<Handle<Type>> {
        if let Some(&h) = self.types.get(&t) {
            return Ok(h);
        }
        let (name, inner) = match self.m.types.get(t).clone() {
            ir::TypeDef::Scalar(s) => (None, TypeInner::Scalar(scalar(s)?)),
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
            ir::TypeDef::Struct { name, .. } | ir::TypeDef::Enum { name, .. } => {
                let (l, offsets) = self.layout(t);
                let mut members = Vec::new();
                for ((mname, mt), offset) in self.members(t)?.into_iter().zip(offsets) {
                    members.push(StructMember {
                        name: Some(ir::ident(&mname)),
                        ty: self.ty(mt)?,
                        binding: None,
                        offset,
                    });
                }
                (Some(ir::ident(&name)), TypeInner::Struct { members, span: l.size })
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
        };
        let binding = (r.kind != ir::ResourceKind::Private)
            .then_some(ResourceBinding { group: 0, binding: r.binding });
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
        let ir::TypeDef::Struct { fields, name } = self.m.types.get(vt).clone() else {
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
                    match self.m.types.get(*ft) {
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
                name: Some(format!("{}_io", ir::ident(&name))),
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
        let l = self.layout(e).0;
        Ok(TypeInner::Array { base, size, stride: round_up(l.align, l.size) })
    }

    /// The members of a struct, or of an enum as WGSL holds it: its tag, then each payload.
    fn members(&self, t: ir::TypeId) -> R<Vec<(String, ir::TypeId)>> {
        match self.m.types.get(t) {
            ir::TypeDef::Struct { fields, .. } => Ok(fields.clone()),
            ir::TypeDef::Enum { variants, .. } => {
                let tag = self.m.types.field(t, 0).ok_or("internal: an enum's tag type")?;
                Ok(std::iter::once(("tag".to_string(), tag))
                    .chain(variants.iter().filter_map(|(v, p)| Some((v.clone(), (*p)?))))
                    .collect())
            }
            _ => Err("internal: members of a type that has none".into()),
        }
    }

    /// A type's layout in the WGSL, and its members' offsets: the IR's (WGSL's rules), but
    /// with each enum laid out as the struct WGSL holds it in.
    fn layout(&mut self, t: ir::TypeId) -> (Layout, Vec<u32>) {
        if let Some(l) = self.layouts.get(&t) {
            return l.clone();
        }
        let m = self.m;
        let out = match m.types.get(t).clone() {
            ir::TypeDef::Array(e, n) => {
                let el = self.layout(e).0;
                let size = n.saturating_mul(round_up(el.align, el.size));
                (Layout { size, align: el.align }, Vec::new())
            }
            ir::TypeDef::Struct { .. } | ir::TypeDef::Enum { .. } => {
                let members = self.members(t).unwrap_or_default();
                let ls: Vec<Layout> = members.iter().map(|&(_, mt)| self.layout(mt).0).collect();
                let (offsets, l) = pack_layouts(ls);
                (l, offsets)
            }
            _ => (ir::layout::layout(&m.types, t), Vec::new()),
        };
        self.layouts.insert(t, out.clone());
        out
    }

    fn function(&mut self, entry: &'m ir::EntryPoint) -> R<Function> {
        let f = &self.m.functions[entry.function.index()];
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
        };
        fb.signature()?;
        let body = fb.block(&f.body)?;
        drop(fb);
        func.body = body;
        Ok(func)
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
    entry: &'m ir::EntryPoint,
    /// For a vertex entry: the output struct and how its members map to the IR struct's.
    io: Option<(Handle<Type>, IoMap, ir::TypeId)>,
    /// Where the IR's parameters start among the naga arguments: after the builtin inputs.
    param_base: u32,
    /// For each builtin input, its naga argument; `None` for a fragment's position when it
    /// takes the varyings, which hold it (WGSL takes each builtin once).
    inputs: Vec<Option<u32>>,
    /// Values that are WGSL const-expressions.
    consts: HashSet<ir::ValueId>,
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
        let e = self.entry;
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
                    let index = self.val(*v)?;
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
                if !self.func.expressions[h].needs_pre_emit() {
                    self.func.named_expressions.insert(h, format!("v{}", v.0));
                }
            }
            ir::Stmt::Eval(e) => match e {
                ir::Expr::Call(..) => return Err(CALL.into()),
                ir::Expr::Host(..) => return Err("GPU code can't record GPU work".into()),
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
            ir::Stmt::Trap => {
                // GPU code can't trap; an unreachable point returns a zero.
                let value = self
                    .func
                    .result
                    .as_ref()
                    .map(|r| r.ty)
                    .map(|t| self.expr(Expression::ZeroValue(t), out));
                out.push(Statement::Return { value }, Span::UNDEFINED);
            }
        }
        Ok(())
    }

    /// A vertex shader's return value, rebuilt as the output struct.
    fn output(&mut self, h: Handle<Expression>, out: &mut Block) -> R<Handle<Expression>> {
        let Some((io, map, rt)) = self.io.clone() else { return Ok(h) };
        let ir::Stage::Vertex { .. } = self.entry.stage else { return Ok(h) };
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
            ir::Expr::Const(ir::Const::F32(x)) if !x.is_finite() => {
                // Not a WGSL literal: its bits, made a float when the shader runs.
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
                    ir::Const::I32(x) => Literal::I32(*x),
                    ir::Const::U32(x) => Literal::U32(*x),
                    ir::Const::F32(x) => Literal::F32(*x),
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
                    && let ir::Stage::Fragment { .. } = &self.entry.stage
                {
                    let arg = self.expr(Expression::FunctionArgument(self.param_base + *i), out);
                    let ir::TypeDef::Struct { fields, .. } = types.get(vt).clone() else {
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
                        let Some((_, map, _)) = self.io.clone() else {
                            return Err("internal: an input with no argument".into());
                        };
                        let ir::Stage::Fragment { position_field: Some(pf), .. } = self.entry.stage
                        else {
                            return Err("internal: an input with no argument".into());
                        };
                        let k = map.iter().position(|&(f, _)| f == pf).unwrap_or(0) as u32;
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
            ir::Expr::Call(..) => return Err(CALL.into()),
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
                let ir::TypeDef::Enum { variants, .. } = types.get(*t).clone() else {
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
                let [base, index] = self.operands([*x, *i], out)?;
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
            ir::Expr::Run(_) | ir::Expr::Addr(_) => {
                return Err("internal: a CPU-only expression in GPU code".into());
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
        let hs = self.operands_of(args, out)?;
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
                    B::Dpdx | B::Dpdy | B::Fwidth | B::AllEqual => unreachable!("handled above"),
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
