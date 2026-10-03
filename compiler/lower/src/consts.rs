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
    fn fold(
        &mut self,
        mb: &mut ModuleBuilder,
        e: &wrela_sema::thir::Expr,
    ) -> Option<ir::ConstValue> {
        let checked = self.checked;
        let types = &checked.program.types;
        let scalar = |cx: &mut Self, mb: &mut ModuleBuilder, l: Lit| {
            let t = cx.lower_ty(mb, e.ty, e.span)?;
            lit_const(&mb.m.types, l, t).map(ir::ConstValue::Scalar)
        };
        Some(match &e.kind {
            ExprKind::Lit(l) => scalar(self, mb, *l)?,
            ExprKind::Unary(UnOp::Neg, x) => {
                let l = match x.kind {
                    ExprKind::Lit(Lit::Int(v)) if types.is_int(e.ty) => Lit::Int(-v),
                    ExprKind::Lit(Lit::Int(v)) => Lit::Float(-(v as f64)),
                    ExprKind::Lit(Lit::Float(f)) => Lit::Float(-f),
                    _ => return None,
                };
                scalar(self, mb, l)?
            }
            ExprKind::Const(c) => {
                let (_, v) = checked.consts.get(c)?;
                self.fold(mb, v)?
            }
            ExprKind::Adt { variant, fields, base: None, .. } => {
                let map = self.field_map(mb, e.ty, *variant, e.span);
                let mut parts = Vec::new();
                for (f, (k, _)) in fields.iter().zip(map) {
                    if k.is_some() {
                        parts.push(self.fold(mb, f)?);
                    }
                }
                match variant {
                    None => ir::ConstValue::Parts(parts),
                    Some(v) => {
                        let t = self.lower_ty(mb, e.ty, e.span)?;
                        let payload =
                            mb.m.types.field(t, 1 + v).map(|_| ir::ConstValue::Parts(parts));
                        ir::ConstValue::Variant(*v, payload.map(Box::new))
                    }
                }
            }
            ExprKind::Tuple(xs) | ExprKind::Array(xs) => {
                let mut parts = Vec::new();
                for x in xs {
                    if self.lower_ty(mb, x.ty, x.span).is_some() {
                        parts.push(self.fold(mb, x)?);
                    }
                }
                ir::ConstValue::Parts(parts)
            }
            ExprKind::ArrayRepeat(x, n) => {
                let v = self.fold(mb, x)?;
                ir::ConstValue::Parts(vec![v; *n as usize])
            }
            ExprKind::Construct(xs) => {
                let mut parts = Vec::new();
                for x in xs {
                    parts.push(self.fold(mb, x)?);
                }
                match types.kind(e.ty) {
                    // Components: scalars, and the components of vectors; one scalar fills all.
                    &TyKind::Vec(n) => {
                        let mut comps = Vec::new();
                        for p in parts {
                            match p {
                                ir::ConstValue::Parts(cs) => comps.extend(cs),
                                s => comps.push(s),
                            }
                        }
                        if comps.len() == 1 {
                            comps = vec![comps[0].clone(); n as usize];
                        }
                        ir::ConstValue::Parts(comps)
                    }
                    // Columns.
                    _ => ir::ConstValue::Parts(parts),
                }
            }
            _ => return None,
        })
    }
}

impl<'c, 'a> Fl<'c, 'a> {
    /// A constant's value built in code (GPU code has no constant data).
    pub fn build_const(&mut self, t: ir::TypeId, v: &ir::ConstValue) -> Option<ir::ValueId> {
        let types = &self.mb.m.types;
        match v {
            ir::ConstValue::Scalar(c) => Some(self.value(t, ir::Expr::Const(c.clone()))),
            ir::ConstValue::Parts(ps) => {
                let part_ty: Vec<ir::TypeId> = match types.get(t).clone() {
                    ir::TypeDef::Vector(_) => vec![self.mb.m.types.f32(); ps.len()],
                    ir::TypeDef::Matrix(n) => vec![self.mb.m.types.vector(n); ps.len()],
                    ir::TypeDef::Array(e, _) => vec![e; ps.len()],
                    ir::TypeDef::Struct { fields, .. } => fields.iter().map(|f| f.1).collect(),
                    _ => return None,
                };
                let mut vals = Vec::new();
                for (p, pt) in ps.iter().zip(part_ty) {
                    vals.push(self.build_const(pt, p)?);
                }
                Some(self.value(t, ir::Expr::Construct(t, vals)))
            }
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
