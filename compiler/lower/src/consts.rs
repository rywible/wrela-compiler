//! Tables: constants too large to build at each use (`mir::Rvalue::Const`). On the CPU each is
//! one piece of constant data that code reads in place; GPU code builds its value.

use crate::body::{Fl, lit_const};
use crate::{Cx, ModuleBuilder};
use wrela_diag::Diagnostic;
use wrela_ir as ir;
use wrela_sema::thir::{ExprKind, Lit};
use wrela_sema::ty::{ConstId, TyKind};
use wrela_syntax::ast::UnOp;

impl<'a> Cx<'a> {
    /// The constant data holding constant `c` in a CPU module, made the first time.
    pub fn const_data(&mut self, mb: &mut ModuleBuilder, c: ConstId) -> Option<ir::DataId> {
        if let Some(&d) = mb.data.get(&c) {
            return d;
        }
        let d = self.const_value(mb, c).map(|(ty, value)| {
            let name = self.checked.program.const_(c).name.clone();
            mb.m.add_data(ir::Data { name, ty, value })
        });
        mb.data.insert(c, d);
        d
    }

    /// Constant `c`'s IR type and value.
    pub fn const_value(
        &mut self,
        mb: &mut ModuleBuilder,
        c: ConstId,
    ) -> Option<(ir::TypeId, ir::ConstValue)> {
        let checked = self.checked;
        let (t, e) = checked.consts.get(&c)?;
        let ty = self.lower_ty(mb, *t, e.span)?;
        if crate::eval::is_computed(checked, c) {
            // Computed by the build: its value, once it's computed.
            let Some(v) = self.data.values.get(&c).cloned() else {
                self.missing.insert(c);
                return None;
            };
            let placed = self.place_value(mb, *t, ty, &v, e.span);
            if placed.is_none() {
                let name = &checked.program.const_(c).name;
                self.err(Diagnostic::internal(format!(
                    "the computed value of `{name}` doesn't fit its type"
                )));
            }
            return placed.map(|v| (ty, v));
        }
        match self.fold(mb, e) {
            Some(v) => Some((ty, v)),
            None => {
                let name = &checked.program.const_(c).name;
                self.err(Diagnostic::internal(format!(
                    "the constant `{name}` isn't a literal value at lowering"
                )));
                None
            }
        }
    }

    /// The value of a literal value (language.md §3, constants), by its type's runtime parts.
    pub(crate) fn fold(
        &mut self,
        mb: &mut ModuleBuilder,
        e: &wrela_sema::thir::Expr,
    ) -> Option<ir::ConstValue> {
        walk(&mut Data { cx: self, mb }, e)
    }
}

/// A run's value in GPU code, which has no addresses: no address, and its length.
fn gpu_run(len: u32) -> ir::ConstValue {
    ir::ConstValue::Parts(vec![
        ir::ConstValue::Scalar(ir::Const::U32(0)),
        ir::ConstValue::Scalar(ir::Const::U32(len)),
    ])
}

/// How [`walk`] builds a literal value: as constant data ([`Data`]), or in code (an [`Fl`]),
/// with a lifted build's `f32` literals read from the table.
trait Build<'a> {
    type V: Clone;
    fn cx(&mut self) -> (&mut Cx<'a>, &mut ModuleBuilder);
    /// A literal, a negated literal, a constant, a text or an embed, of IR type `t`; `None` for
    /// anything else.
    fn leaf(&mut self, e: &wrela_sema::thir::Expr, t: ir::TypeId) -> Option<Self::V>;
    /// A value of IR type `t` from its parts: a struct's fields, a tuple's or an array's
    /// elements, a matrix's columns or a vector's components.
    fn parts(&mut self, t: ir::TypeId, parts: Vec<Self::V>) -> Self::V;
    /// Variant `v` of the enum `t`, its payload made of `parts`.
    fn variant(&mut self, t: ir::TypeId, v: u32, parts: Vec<Self::V>) -> Self::V;
    /// Adds the components of a vector `p` to `out`, or `p` itself, a scalar.
    fn components(&mut self, p: Self::V, out: &mut Vec<Self::V>);
}

/// A literal value, by its type's runtime parts, as `b` builds it.
fn walk<'a, B: Build<'a>>(b: &mut B, e: &wrela_sema::thir::Expr) -> Option<B::V> {
    let checked = b.cx().0.checked;
    let t = {
        let (cx, mb) = b.cx();
        cx.lower_ty(mb, e.ty, e.span)?
    };
    Some(match &e.kind {
        ExprKind::Adt { variant, fields, base: None, .. } => {
            let map = {
                let (cx, mb) = b.cx();
                cx.field_map(mb, e.ty, *variant, e.span)
            };
            let mut parts = Vec::new();
            for (f, (k, _)) in fields.iter().zip(map) {
                if k.is_some() {
                    parts.push(walk(b, f)?);
                }
            }
            match variant {
                None => b.parts(t, parts),
                Some(v) => b.variant(t, *v, parts),
            }
        }
        ExprKind::Tuple(xs) | ExprKind::Array(xs) => {
            let mut parts = Vec::new();
            for x in xs {
                let (cx, mb) = b.cx();
                if cx.lower_ty(mb, x.ty, x.span).is_some() {
                    parts.push(walk(b, x)?);
                }
            }
            b.parts(t, parts)
        }
        ExprKind::ArrayFill(xs, fill, n) => {
            let mut parts = Vec::new();
            let (cx, mb) = b.cx();
            if cx.lower_ty(mb, fill.ty, fill.span).is_none() {
                return Some(b.parts(t, parts));
            }
            for x in xs {
                parts.push(walk(b, x)?);
            }
            let f = walk(b, fill)?;
            parts.resize(*n as usize, f);
            b.parts(t, parts)
        }
        ExprKind::ArrayRepeat(x, n) => {
            let v = walk(b, x)?;
            b.parts(t, vec![v; *n as usize])
        }
        ExprKind::Construct(xs) => {
            let mut parts = Vec::new();
            for x in xs {
                parts.push(walk(b, x)?);
            }
            // A vector's components: scalars, and the components of vectors; one scalar fills
            // all. (A matrix's parts are its columns.)
            if let &TyKind::Vec(_, n) = checked.program.types.kind(e.ty) {
                let mut comps = Vec::new();
                for p in parts {
                    b.components(p, &mut comps);
                }
                if comps.len() == 1 {
                    comps = vec![comps[0].clone(); n as usize];
                }
                parts = comps;
            }
            b.parts(t, parts)
        }
        _ => b.leaf(e, t)?,
    })
}

/// Builds a literal value as constant data.
struct Data<'x, 'a> {
    cx: &'x mut Cx<'a>,
    mb: &'x mut ModuleBuilder,
}

impl<'a> Build<'a> for Data<'_, 'a> {
    type V = ir::ConstValue;

    fn cx(&mut self) -> (&mut Cx<'a>, &mut ModuleBuilder) {
        (self.cx, self.mb)
    }

    fn leaf(&mut self, e: &wrela_sema::thir::Expr, t: ir::TypeId) -> Option<ir::ConstValue> {
        let types = &self.cx.checked.program.types;
        let scalar =
            |mb: &ModuleBuilder, l: Lit| lit_const(&mb.m.types, l, t).map(ir::ConstValue::Scalar);
        let run = |(d, n): (ir::DataId, u32)| {
            let len = ir::ConstValue::Scalar(ir::Const::U32(n));
            ir::ConstValue::Parts(vec![ir::ConstValue::Addr(d), len])
        };
        Some(match &e.kind {
            ExprKind::Lit(l) => scalar(self.mb, *l)?,
            ExprKind::Unary(UnOp::Neg, x) => {
                let l = match x.kind {
                    ExprKind::Lit(Lit::Int(v)) if types.is_int(e.ty) => Lit::Int(-v),
                    ExprKind::Lit(Lit::Int(v)) => Lit::Float(-(v as f64), -(v as f32)),
                    ExprKind::Lit(Lit::Float(d, f)) => Lit::Float(-d, -f),
                    _ => return None,
                };
                scalar(self.mb, l)?
            }
            ExprKind::Const(c) => self.cx.const_value(self.mb, *c)?.1,
            // GPU code has no addresses: a text or an embed there is its length alone, so a
            // constant that holds one (a site's name beside its place) is built, and GPU code
            // reads its other fields (M6). Its bytes are the CPU's.
            ExprKind::Text(s) if self.mb.target() == ir::Target::Gpu => gpu_run(s.len() as u32),
            ExprKind::Embed(path) if self.mb.target() == ir::Target::Gpu => {
                let n = self.cx.embed_data(self.mb, path)?.1;
                gpu_run(n)
            }
            ExprKind::Text(s) => run(self.cx.text_data(self.mb, s)),
            ExprKind::Embed(path) => run(self.cx.embed_data(self.mb, path)?),
            _ => return None,
        })
    }

    fn parts(&mut self, _: ir::TypeId, parts: Vec<ir::ConstValue>) -> ir::ConstValue {
        ir::ConstValue::Parts(parts)
    }

    fn variant(&mut self, t: ir::TypeId, v: u32, parts: Vec<ir::ConstValue>) -> ir::ConstValue {
        let payload = self.mb.m.types.field(t, 1 + v).map(|_| ir::ConstValue::Parts(parts));
        ir::ConstValue::Variant(v, payload.map(Box::new))
    }

    fn components(&mut self, p: ir::ConstValue, out: &mut Vec<ir::ConstValue>) {
        match p {
            ir::ConstValue::Parts(cs) => out.extend(cs),
            s => out.push(s),
        }
    }
}

/// Builds a literal value in code: a lifted build's `f32` literals read from the table (§22).
impl<'a> Build<'a> for Fl<'_, 'a> {
    type V = ir::ValueId;

    fn cx(&mut self) -> (&mut Cx<'a>, &mut ModuleBuilder) {
        (self.cx, self.mb)
    }

    fn leaf(&mut self, e: &wrela_sema::thir::Expr, t: ir::TypeId) -> Option<ir::ValueId> {
        let f32_ty = self.cx.checked.program.types.f32;
        let lifted = (e.ty == f32_ty).then(|| self.cx.lift.and_then(|l| l.of(e.span))).flatten();
        match &e.kind {
            ExprKind::Lit(_) | ExprKind::Unary(UnOp::Neg, _) if lifted.is_some() => {
                self.lifted(lifted?)
            }
            ExprKind::Const(c) => match self.lifted_const(*c) {
                Some(v) => Some(v),
                None => {
                    let (ty, v) = self.cx.const_value(self.mb, *c)?;
                    self.build_const(ty, &v)
                }
            },
            _ => {
                let v = Data { cx: self.cx, mb: self.mb }.leaf(e, t)?;
                self.build_const(t, &v)
            }
        }
    }

    fn parts(&mut self, t: ir::TypeId, parts: Vec<ir::ValueId>) -> ir::ValueId {
        self.value(t, ir::Expr::Construct(t, parts))
    }

    fn variant(&mut self, t: ir::TypeId, v: u32, parts: Vec<ir::ValueId>) -> ir::ValueId {
        let payload = self
            .mb
            .m
            .types
            .field(t, 1 + v)
            .map(|pt| self.value(pt, ir::Expr::Construct(pt, parts)));
        self.value(t, ir::Expr::Variant(t, v, payload))
    }

    fn components(&mut self, p: ir::ValueId, out: &mut Vec<ir::ValueId>) {
        self.push_components(p, out)
    }
}

impl<'c, 'a> Fl<'c, 'a> {
    /// Pushes a vector's components to `out`, each its own value, or another value as it is.
    pub(crate) fn push_components(&mut self, p: ir::ValueId, out: &mut Vec<ir::ValueId>) {
        match *self.mb.m.types.get(self.f.value_ty(p)) {
            ir::TypeDef::Vector(s, k) => {
                let ct = self.mb.m.types.scalar(s);
                for i in 0..k {
                    out.push(self.value(ct, ir::Expr::Extract(p, u32::from(i))));
                }
            }
            _ => out.push(p),
        }
    }

    /// A lifted build's constant `c` of a lifted package, built here from its literals, each
    /// read from the table (§22); `None` for any other constant, which is data.
    pub(crate) fn lifted_const(&mut self, c: ConstId) -> Option<ir::ValueId> {
        self.cx.lift?;
        let checked = self.cx.checked;
        let program = &checked.program;
        if crate::eval::is_computed(checked, c) {
            return None;
        }
        if !program.package_of(program.const_(c).module).lifted {
            return None;
        }
        let (_, e) = checked.consts.get(&c)?;
        walk(self, e)
    }

    /// A constant's value built in code (GPU code has no constant data).
    pub fn build_const(&mut self, t: ir::TypeId, v: &ir::ConstValue) -> Option<ir::ValueId> {
        let types = &mut self.mb.m.types;
        match v {
            ir::ConstValue::Scalar(c) => Some(self.value(t, ir::Expr::Const(c.clone()))),
            ir::ConstValue::Parts(ps) => {
                let part_ty: Vec<ir::TypeId> = match *types.get(t) {
                    ir::TypeDef::Vector(s, _) => vec![types.scalar(s); ps.len()],
                    ir::TypeDef::Matrix(_, r) => vec![types.vector(r); ps.len()],
                    ir::TypeDef::Array(e, _) => vec![e; ps.len()],
                    ir::TypeDef::Struct { ref fields, .. } => fields.iter().map(|f| f.1).collect(),
                    _ => return None,
                };
                let mut vals = Vec::new();
                for (p, pt) in ps.iter().zip(part_ty) {
                    vals.push(self.build_const(pt, p)?);
                }
                Some(self.value(t, ir::Expr::Construct(t, vals)))
            }
            ir::ConstValue::Bytes(bs) => {
                let u8 = types.scalar(ir::Scalar::U8);
                let vals: Vec<ir::ValueId> = bs
                    .iter()
                    .map(|&b| {
                        self.value(
                            u8,
                            ir::Expr::Const(ir::Const::Small(ir::Scalar::U8, i64::from(b))),
                        )
                    })
                    .collect();
                Some(self.value(t, ir::Expr::Construct(t, vals)))
            }
            // GPU code has no addresses; nothing on the GPU holds one.
            ir::ConstValue::Addr(_) => None,
            ir::ConstValue::Variant(k, payload) => {
                let p = match (payload, types.field(t, 1 + k)) {
                    (Some(p), Some(pt)) => Some(self.build_const(pt, p)?),
                    _ => None,
                };
                Some(self.value(t, ir::Expr::Variant(t, *k, p)))
            }
        }
    }
}
