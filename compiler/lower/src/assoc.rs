//! Associated constants as `const` arguments (§3.4, §18.1): `blur.bind(taps: S::K)` records a
//! pipeline per instantiation, so the value of `S::K` is folded here, where `S` is known, from
//! the constant's initializer as it's written: literals, other constants, other associated
//! constants, `const` generic parameters, arithmetic, comparisons and `if`.

use crate::Cx;
use wrela_diag::{Diagnostic, Span, codes};
use wrela_sema::thir::{Block, Callee, ConstArg, ConstSrc, Expr, ExprKind, Lit};
use wrela_sema::ty::{FloatTy, FnId, IntTy, Subst, TyId, TyKind};
use wrela_syntax::ast::{BinOp, UnOp};

/// A folded value, before it takes its parameter's type.
#[derive(Clone, Copy, Debug)]
enum Folded {
    Bool(bool),
    Int(i128),
    Float(f32),
}

/// How deep a constant may name constants that name constants: past it, a cycle.
const MAX_DEPTH: u32 = 64;

impl<'a> Cx<'a> {
    /// `src`'s value as a `const` argument of type `ty`, its types already concrete; an error
    /// (E0603) naming `param` when it doesn't fold.
    pub(crate) fn const_src(
        &mut self,
        src: &ConstSrc,
        ty: TyId,
        param: &str,
        span: Span,
    ) -> Option<ConstArg> {
        let folded = match src {
            &ConstSrc::Value(c) => return Some(c),
            ConstSrc::Inherent { func, args } => self.fold_assoc(*func, args.clone(), None, 0),
            ConstSrc::Trait { method, self_ty, trait_args } => {
                self.fold_assoc(*method, Vec::new(), Some((*self_ty, trait_args.clone())), 0)
            }
        };
        let arg = folded.and_then(|v| self.const_arg_of(v, ty));
        if arg.is_none() {
            let shown = self.checked.program.display_ty(ty);
            self.err(
                Diagnostic::new(
                    codes::E0603,
                    span,
                    format!("`{param}`'s associated constant doesn't fold to a `{shown}` here"),
                )
                .with_note("a `const` argument's associated constant is folded from what it's written as: literals, constants, other associated constants, arithmetic, comparisons and `if`")
                .with_help("give the constant a value the build can fold, or compute it in a `const` item and name that"),
            );
        }
        arg
    }

    /// The value of the associated constant `func` (an inherent one, `args` its impl's), or
    /// the trait's `func` of `of`'s type (and the trait's arguments).
    fn fold_assoc(
        &mut self,
        func: FnId,
        args: Vec<TyId>,
        of: Option<(TyId, Vec<TyId>)>,
        depth: u32,
    ) -> Option<Folded> {
        if depth > MAX_DEPTH {
            return None;
        }
        let p = &self.checked.program;
        let (func, subst) = match of {
            Some((self_ty, trait_args)) => {
                wrela_sema::traits::resolve_trait_method(p, func, self_ty, &trait_args, &[])?
            }
            None => (func, Subst::from_pairs(&p.fn_all_generics(func), &args)),
        };
        let checked = self.checked;
        let e = checked.assoc_consts.get(&func)?;
        self.fold_expr(e, &subst, depth + 1)
    }

    fn fold_expr(&mut self, e: &Expr, subst: &Subst, depth: u32) -> Option<Folded> {
        let checked = self.checked;
        let ty = self.concrete(e.ty, subst);
        Some(match &e.kind {
            ExprKind::Lit(Lit::Bool(b)) => Folded::Bool(*b),
            ExprKind::Lit(Lit::Int(n)) => match checked.program.types.kind(ty) {
                TyKind::Float(_) => Folded::Float(*n as f32),
                _ => Folded::Int(*n),
            },
            ExprKind::Lit(Lit::Float(_, v)) => Folded::Float(*v),
            &ExprKind::ConstParam(g) => {
                let t = self.concrete(checked.program.types.param(g), subst);
                match checked.program.types.kind(t) {
                    &TyKind::ConstU32(n) => Folded::Int(i128::from(n)),
                    _ => return None,
                }
            }
            &ExprKind::Const(c) => {
                if crate::eval::is_computed(checked, c) {
                    let v = self.data.values.get(&c)?.clone();
                    match &*v {
                        crate::eval::Value::Scalar(k) => scalar(k)?,
                        _ => return None,
                    }
                } else {
                    let (_, init) = checked.consts.get(&c)?;
                    self.fold_expr(init, &Subst::default(), depth + 1)?
                }
            }
            ExprKind::Call(call) => match &call.callee {
                Callee::Fn { func, args } if checked.program.func(*func).attrs.assoc_const => {
                    let args = args.iter().map(|&a| self.concrete(a, subst)).collect();
                    self.fold_assoc(*func, args, None, depth)?
                }
                Callee::TraitMethod { method, self_ty, trait_args, .. }
                    if checked.program.func(*method).attrs.assoc_const =>
                {
                    let self_ty = self.concrete(*self_ty, subst);
                    let trait_args = trait_args.iter().map(|&a| self.concrete(a, subst)).collect();
                    self.fold_assoc(*method, Vec::new(), Some((self_ty, trait_args)), depth)?
                }
                _ => return None,
            },
            ExprKind::Block(b) => self.fold_block(b, subst, depth)?,
            ExprKind::Unary(op, x) => match (op, self.fold_expr(x, subst, depth)?) {
                (UnOp::Neg, Folded::Int(n)) => Folded::Int(-n),
                (UnOp::Neg, Folded::Float(v)) => Folded::Float(-v),
                (UnOp::Not, Folded::Bool(b)) => Folded::Bool(!b),
                _ => return None,
            },
            ExprKind::Binary(op, a, b) => {
                let (a, b) = (self.fold_expr(a, subst, depth)?, self.fold_expr(b, subst, depth)?);
                binary(*op, a, b)?
            }
            ExprKind::If { cond, then, else_ } => match self.fold_expr(cond, subst, depth)? {
                Folded::Bool(true) => self.fold_block(then, subst, depth)?,
                Folded::Bool(false) => self.fold_expr(else_.as_deref()?, subst, depth)?,
                _ => return None,
            },
            _ => return None,
        })
    }

    /// A block that's only its value.
    fn fold_block(&mut self, b: &Block, subst: &Subst, depth: u32) -> Option<Folded> {
        if !b.stmts.is_empty() {
            return None;
        }
        self.fold_expr(b.tail.as_deref()?, subst, depth)
    }

    /// `v` as a `const` argument of type `ty`, if it is one.
    fn const_arg_of(&self, v: Folded, ty: TyId) -> Option<ConstArg> {
        Some(match (self.checked.program.types.kind(ty), v) {
            (TyKind::Bool, Folded::Bool(b)) => ConstArg::Bool(b),
            (TyKind::Int(IntTy::U32), Folded::Int(n)) => ConstArg::U32(u32::try_from(n).ok()?),
            (TyKind::Int(IntTy::I32), Folded::Int(n)) => ConstArg::I32(i32::try_from(n).ok()?),
            (TyKind::Float(FloatTy::F32), Folded::Float(x)) => ConstArg::F32(x.to_bits()),
            (TyKind::Float(FloatTy::F32), Folded::Int(n)) => {
                ConstArg::F32((n as f32).to_bits())
            }
            _ => return None,
        })
    }
}

/// A computed constant's scalar.
fn scalar(k: &wrela_ir::Const) -> Option<Folded> {
    Some(match *k {
        wrela_ir::Const::Bool(b) => Folded::Bool(b),
        wrela_ir::Const::U32(n) => Folded::Int(i128::from(n)),
        wrela_ir::Const::I32(n) => Folded::Int(i128::from(n)),
        wrela_ir::Const::F32(x) => Folded::Float(x),
        _ => return None,
    })
}

/// `a op b`, for the operators a constant's initializer folds. Integer arithmetic is exact
/// here, and the result must fit the parameter's type ([`Cx::const_arg_of`]); a float's is
/// `f32`'s, as the CPU computes it (§11.1).
fn binary(op: BinOp, a: Folded, b: Folded) -> Option<Folded> {
    use Folded::*;
    Some(match (a, b) {
        (Int(x), Int(y)) => match op {
            BinOp::Add => Int(x + y),
            BinOp::Sub => Int(x - y),
            BinOp::Mul => Int(x.checked_mul(y)?),
            BinOp::Div => Int(x.checked_div(y)?),
            BinOp::Rem => Int(x.checked_rem(y)?),
            BinOp::Eq => Bool(x == y),
            BinOp::Ne => Bool(x != y),
            BinOp::Lt => Bool(x < y),
            BinOp::Le => Bool(x <= y),
            BinOp::Gt => Bool(x > y),
            BinOp::Ge => Bool(x >= y),
            _ => return None,
        },
        (Float(x), Float(y)) => match op {
            BinOp::Add => Float(x + y),
            BinOp::Sub => Float(x - y),
            BinOp::Mul => Float(x * y),
            BinOp::Div => Float(x / y),
            BinOp::Eq => Bool(x == y),
            BinOp::Ne => Bool(x != y),
            BinOp::Lt => Bool(x < y),
            BinOp::Le => Bool(x <= y),
            BinOp::Gt => Bool(x > y),
            BinOp::Ge => Bool(x >= y),
            _ => return None,
        },
        (Bool(x), Bool(y)) => match op {
            BinOp::And => Bool(x && y),
            BinOp::Or => Bool(x || y),
            BinOp::Eq => Bool(x == y),
            BinOp::Ne => Bool(x != y),
            _ => return None,
        },
        _ => return None,
    })
}
