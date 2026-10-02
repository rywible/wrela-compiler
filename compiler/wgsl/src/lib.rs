//! The WGSL back end (Q3 of #6: lower to naga IR, not WGSL text). A GPU module of the IR
//! becomes a naga module, which naga validates and writes as WGSL. Validation failures are
//! compiler bugs, reported as such.
//!
//! - Types keep the IR's layout (`wrela_ir::layout`), so struct member offsets in the WGSL are
//!   the offsets the CPU wrote uniforms with.
//! - Every IR value becomes a named WGSL `let`, so evaluation order is the IR's exactly.
//! - Only entry points are written: GPU modules arrive flattened (`wrela_ir::opt` inlines
//!   every call), so a call here is a compiler bug.

use naga::{
    AddressSpace, ArraySize, BinaryOperator, Binding, Block, BuiltIn, Expression, Function,
    FunctionArgument, FunctionResult, GlobalVariable, Handle, Interpolation, Literal,
    LocalVariable, MathFunction, ResourceBinding, Sampling, Scalar, ScalarKind, ShaderStage, Span,
    Statement, StorageAccess, StructMember, Type, TypeInner, UnaryOperator, VectorSize,
};
use std::collections::HashMap;
use wrela_ir as ir;
use wrela_ir::layout::{array_stride, field_offsets, layout};

type R<T> = Result<T, String>;

const CALL: &str = "internal: a call in a GPU module, which should have been flattened";

/// Writes a GPU module as validated WGSL.
pub fn emit(m: &ir::Module) -> R<String> {
    let module = to_naga(m)?;
    let info = validate(&module)?;
    naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
        .map_err(|e| format!("internal: writing WGSL failed: {e}"))
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

fn sanitize(name: &str) -> String {
    ir::ident(name)
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
    globals: Vec<Option<Handle<GlobalVariable>>>,
}

pub fn to_naga(m: &ir::Module) -> R<naga::Module> {
    let mut cx = Cx { m, out: naga::Module::default(), types: HashMap::new(), globals: Vec::new() };
    for r in &m.resources {
        let g = cx.global(r)?;
        cx.globals.push(Some(g));
    }
    for e in m.entry_points.iter() {
        let func = cx.function(e)?;
        let (stage, wg) = match &e.stage {
            ir::Stage::Compute { workgroup_size } => (ShaderStage::Compute, *workgroup_size),
            ir::Stage::Vertex { .. } => (ShaderStage::Vertex, [0; 3]),
            ir::Stage::Fragment { .. } => (ShaderStage::Fragment, [0; 3]),
        };
        cx.out.entry_points.push(naga::EntryPoint {
            name: sanitize(&e.name),
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
                let base = self.ty(e)?;
                let size =
                    std::num::NonZeroU32::new(n).map_or(ArraySize::Dynamic, ArraySize::Constant);
                (None, TypeInner::Array { base, size, stride: array_stride(&self.m.types, e) })
            }
            ir::TypeDef::RuntimeArray(e) => {
                let base = self.ty(e)?;
                (
                    None,
                    TypeInner::Array {
                        base,
                        size: ArraySize::Dynamic,
                        stride: array_stride(&self.m.types, e),
                    },
                )
            }
            ir::TypeDef::Struct { name, fields } => {
                let offsets = field_offsets(&self.m.types, t);
                let mut members = Vec::new();
                for (i, (fname, ft)) in fields.iter().enumerate() {
                    members.push(StructMember {
                        name: Some(sanitize(fname)),
                        ty: self.ty(*ft)?,
                        binding: None,
                        offset: offsets[i],
                    });
                }
                (
                    Some(sanitize(&name)),
                    TypeInner::Struct { members, span: layout(&self.m.types, t).size },
                )
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
                let base = self.ty(r.ty)?;
                let arr = self.out.types.insert(
                    Type {
                        name: None,
                        inner: TypeInner::Array {
                            base,
                            size: ArraySize::Dynamic,
                            stride: array_stride(&self.m.types, r.ty),
                        },
                    },
                    Span::UNDEFINED,
                );
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
                name: Some(sanitize(&r.name)),
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
                name: Some(sanitize(fname)),
                ty,
                binding: Some(binding),
                offset,
            });
            offset += 16;
            map.push((i as u32, inner_flat));
        }
        let h = self.out.types.insert(
            Type {
                name: Some(format!("{}_io", sanitize(&name))),
                inner: TypeInner::Struct { members, span: offset.max(16) },
            },
            Span::UNDEFINED,
        );
        Ok((h, map))
    }

    fn ty_vec4(&mut self) -> Handle<Type> {
        self.out.types.insert(
            Type {
                name: None,
                inner: TypeInner::Vector { size: VectorSize::Quad, scalar: Scalar::F32 },
            },
            Span::UNDEFINED,
        )
    }

    fn ty_inner(&mut self, inner: TypeInner) -> Handle<Type> {
        self.out.types.insert(Type { name: None, inner }, Span::UNDEFINED)
    }

    fn function(&mut self, entry: &'m ir::EntryPoint) -> R<Function> {
        let f = &self.m.functions[entry.function.index()];
        let mut func = Function { name: Some(sanitize(&f.name)), ..Function::default() };
        let mut fb = Fb {
            cx: self,
            f,
            func: &mut func,
            values: HashMap::new(),
            locals: Vec::new(),
            entry,
            io: None,
            inputs: Vec::new(),
            param_base: 0,
        };
        fb.signature()?;
        let body = fb.block(&f.body)?;
        drop(fb);
        func.body = body;
        Ok(func)
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
    /// The entry's builtin inputs, as argument indices.
    inputs: Vec<u32>,
    /// Where the IR's parameters start among the naga arguments.
    param_base: u32,
}

impl<'a, 'm> Fb<'a, 'm> {
    fn signature(&mut self) -> R<()> {
        for l in &self.f.locals {
            let ty = self.cx.ty(l.ty)?;
            let h = self.func.local_variables.append(
                LocalVariable { name: Some(sanitize(&l.name)), ty, init: None },
                Span::UNDEFINED,
            );
            self.locals.push(h);
        }
        let e = self.entry;
        // Builtin inputs first.
        for (i, b) in e.inputs.iter().enumerate() {
            let (builtin, inner) = match b {
                ir::BuiltinInput::GlobalInvocationId => (
                    BuiltIn::GlobalInvocationId,
                    TypeInner::Vector { size: VectorSize::Tri, scalar: Scalar::U32 },
                ),
                ir::BuiltinInput::LocalInvocationId => (
                    BuiltIn::LocalInvocationId,
                    TypeInner::Vector { size: VectorSize::Tri, scalar: Scalar::U32 },
                ),
                ir::BuiltinInput::WorkgroupId => (
                    BuiltIn::WorkGroupId,
                    TypeInner::Vector { size: VectorSize::Tri, scalar: Scalar::U32 },
                ),
                ir::BuiltinInput::NumWorkgroups => (
                    BuiltIn::NumWorkGroups,
                    TypeInner::Vector { size: VectorSize::Tri, scalar: Scalar::U32 },
                ),
                ir::BuiltinInput::VertexIndex => {
                    (BuiltIn::VertexIndex, TypeInner::Scalar(Scalar::U32))
                }
                ir::BuiltinInput::InstanceIndex => {
                    (BuiltIn::InstanceIndex, TypeInner::Scalar(Scalar::U32))
                }
                ir::BuiltinInput::Position => (
                    BuiltIn::Position { invariant: false },
                    TypeInner::Vector { size: VectorSize::Quad, scalar: Scalar::F32 },
                ),
            };
            let ty = self.cx.ty_inner(inner);
            self.func.arguments.push(FunctionArgument {
                name: Some(format!("input{i}")),
                ty,
                binding: Some(Binding::BuiltIn(builtin)),
            });
            self.inputs.push(i as u32);
        }
        self.param_base = e.inputs.len() as u32;
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

    fn val(&self, v: ir::ValueId) -> R<Handle<Expression>> {
        self.values
            .get(&v)
            .copied()
            .ok_or_else(|| format!("internal: v{} has no naga expression", v.0))
    }

    /// A pointer to a place.
    fn place(&mut self, p: &ir::Place, block: &mut Block) -> R<Handle<Expression>> {
        let mut h = match &p.root {
            ir::PlaceRoot::Local(l) => {
                self.expr(Expression::LocalVariable(self.locals[l.index()]), block)
            }
            ir::PlaceRoot::Param(i) => {
                self.expr(Expression::FunctionArgument(self.param_base + *i), block)
            }
            ir::PlaceRoot::Resource(r) => {
                let g = self.cx.globals[r.index()].ok_or("internal: a missing resource")?;
                self.expr(Expression::GlobalVariable(g), block)
            }
            ir::PlaceRoot::Ptr(_) => {
                return Err(
                    "projections returned from functions aren't supported on the GPU".into()
                );
            }
        };
        for proj in &p.path {
            h = match proj {
                ir::Proj::Field(k) => {
                    self.expr(Expression::AccessIndex { base: h, index: *k }, block)
                }
                ir::Proj::Comp(c) => {
                    self.expr(Expression::AccessIndex { base: h, index: *c as u32 }, block)
                }
                ir::Proj::Index(v) => {
                    let index = self.val(*v)?;
                    self.expr(Expression::Access { base: h, index }, block)
                }
            };
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
        let Some((io, map, _)) = self.io.clone() else { return Ok(h) };
        let ir::Stage::Vertex { position_field: position, .. } = self.entry.stage else {
            return Ok(h);
        };
        let mut comps = Vec::new();
        let single = map.len() == 1
            && !self.f.ret.is_some_and(|r| matches!(self.cx.m.types.get(r), ir::TypeDef::Struct { fields, .. } if fields.len() > 1));
        for (i, is_flat) in map {
            let field = self.expr(Expression::AccessIndex { base: h, index: i }, out);
            let v = if i == position && !single {
                // The field is a `ClipPosition` struct: its `position`.
                self.expr(Expression::AccessIndex { base: field, index: 0 }, out)
            } else if is_flat {
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
            ir::Expr::Const(c) => {
                let l = match c {
                    ir::Const::Bool(b) => Literal::Bool(*b),
                    ir::Const::I32(x) => Literal::I32(*x),
                    ir::Const::U32(x) => Literal::U32(*x),
                    ir::Const::F32(x) => Literal::F32(*x),
                    other => return Err(format!("internal: the constant {other:?} in GPU code")),
                };
                self.lit(l, out)
            }
            ir::Expr::Zero(t) => {
                let ty = self.cx.ty(*t)?;
                self.expr(Expression::ZeroValue(ty), out)
            }
            ir::Expr::Param(i) => {
                // In a fragment shader, parameter 0 is the varyings: rebuild the IR struct.
                if let Some((_, map, vt)) = self.io.clone()
                    && let ir::Stage::Fragment { position_field, .. } = &self.entry.stage
                {
                    let arg = self.expr(Expression::FunctionArgument(self.param_base + *i), out);
                    let ir::TypeDef::Struct { fields, .. } = types.get(vt).clone() else {
                        return Err("internal: varyings".into());
                    };
                    let position = position_field.unwrap_or(u32::MAX);
                    let mut comps = Vec::new();
                    for (k, (i, is_flat)) in map.into_iter().enumerate() {
                        let member =
                            self.expr(Expression::AccessIndex { base: arg, index: k as u32 }, out);
                        let ft = fields[i as usize].1;
                        let fty = self.cx.ty(ft)?;
                        comps.push(if i == position || is_flat {
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
                let arg = self.expr(Expression::FunctionArgument(self.inputs[*i as usize]), out);
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
                self.expr(Expression::Compose { ty, components }, out)
            }
            ir::Expr::Extract(x, i) => {
                let base = self.val(*x)?;
                self.expr(Expression::AccessIndex { base, index: *i }, out)
            }
            ir::Expr::ExtractDyn(x, i) => {
                let base = self.val(*x)?;
                let index = self.val(*i)?;
                self.expr(Expression::Access { base, index }, out)
            }
            ir::Expr::Splat(x, n) => {
                let value = self.val(*x)?;
                self.expr(Expression::Splat { size: vsize(*n), value }, out)
            }
            ir::Expr::Swizzle(x, comps) => {
                let vector = self.val(*x)?;
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
                let expr = self.val(*x)?;
                let sc = scalar(*s)?;
                self.expr(Expression::As { expr, kind: sc.kind, convert: Some(sc.width) }, out)
            }
            ir::Expr::Bitcast(x, s) => {
                let expr = self.val(*x)?;
                let sc = scalar(*s)?;
                self.expr(Expression::As { expr, kind: sc.kind, convert: None }, out)
            }
            ir::Expr::Select { cond, if_true, if_false } => {
                let condition = self.val(*cond)?;
                let accept = self.val(*if_true)?;
                let reject = self.val(*if_false)?;
                if matches!(
                    types.get(t),
                    ir::TypeDef::Struct { .. } | ir::TypeDef::Array(..) | ir::TypeDef::Matrix(_)
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
                let expr = self.val(*x)?;
                let op = match (op, types.get(self.f.value_ty(*x))) {
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
                let left = self.val(*a)?;
                let right = self.val(*b)?;
                let is_bool = matches!(types.get(ta), ir::TypeDef::Scalar(ir::Scalar::Bool));
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
        let hs = args.iter().map(|a| self.val(*a)).collect::<R<Vec<_>>>()?;
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

    /// A tiny compute module round-trips through naga's validator and back through its WGSL
    /// parser.
    #[test]
    fn a_minimal_kernel_is_valid_wgsl() {
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
        let mut f = ir::Function::new("k", Vec::new(), None);
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
            name: "k".into(),
            stage: ir::Stage::Compute { workgroup_size: [64, 1, 1] },
            function: fid,
            inputs: vec![ir::BuiltinInput::GlobalInvocationId],
        });
        let wgsl = emit(&m).expect("valid");
        assert!(wgsl.contains("@compute @workgroup_size(64, 1, 1)"), "{wgsl}");
        naga::front::wgsl::parse_str(&wgsl).expect("parses back");
    }
}
