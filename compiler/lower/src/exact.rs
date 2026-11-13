//! A `@deterministic` kernel's CPU form (language.md §18.14): `k.run(input, params, mut out)`
//! runs the kernel's body once for each of `out`'s places, on the CPU, with the bits the GPU
//! gives. It's made from the GPU module itself (`gpu::exact_module`: checked, its indexes
//! clamped, its calls inlined), so both run one code:
//! - the module's buffers become the function's parameters: the input's run, the output's
//!   address and length; its uniform block a local the caller's value fills; its private
//!   variables locals;
//! - its builtin inputs come from the loop's counter (its place `(i, 0, 0)`; the workgroups as
//!   many as cover the output);
//! - integer arithmetic is WGSL's: `+ - *` wrap; a division or remainder by zero, and `MIN /
//!   -1`, give WGSL's results (the dividend, and 0); a shift's amount is masked to the width;
//! - a `return` ends the place's pass, and the loop goes on to the next place.

use crate::body::Fl;
use crate::ir;
use std::collections::HashMap;
use wrela_diag::{Diagnostic, Span};
use wrela_sema::mir;
use wrela_sema::ty::{FnId, TyId};

/// `k.run(input, params, mut out)`: a call of the kernel's CPU form.
pub(crate) fn run(
    fl: &mut Fl,
    kernel: FnId,
    substs: &[TyId],
    args: &[mir::Arg],
    span: Span,
) -> Option<ir::ValueId> {
    let key = (kernel, substs.to_vec());
    let (func, run_t) = match fl.mb.exact.get(&key) {
        Some(&f) => (f, fl.mb.m.functions[f.index()].params[0].ty),
        None => {
            let (gm, wg) = crate::gpu::exact_module(fl.cx, kernel, substs)?;
            let name = fl.cx.checked.program.func(kernel).name.clone();
            let f = match Tr::translate(&mut fl.mb.m, &gm, wg, &name) {
                Ok(f) => f,
                Err(why) => {
                    fl.cx.err(Diagnostic::internal(format!("`{name}.run`: {why}")).with_note(
                        "a deterministic kernel's CPU form is made from its GPU module (§18.14)",
                    ));
                    return None;
                }
            };
            fl.mb.exact.insert(key, f);
            (f, fl.mb.m.functions[f.index()].params[0].ty)
        }
    };
    let input = fl.run_of(args.first()?.place()?, run_t)?;
    let params = fl.arg_value(args.get(1)?)?;
    // The uniform block holds the kernel's one value: the block's struct, built of it.
    let block_t = fl.mb.m.functions[func.index()].params[1].ty;
    let params = if fl.f.value_ty(params) == block_t {
        params
    } else {
        fl.value(block_t, ir::Expr::Construct(block_t, vec![params]))
    };
    let out = args.get(2)?.place()?;
    let data = fl.read(&out.with(mir::Proj::Field(0)))?;
    let len = fl.read(&out.with(mir::Proj::Field(1)))?;
    let _ = span;
    let call_args = [input, params, data, len].into_iter().map(ir::Arg::Value).collect();
    fl.emit(ir::Stmt::Eval(ir::Expr::Call(func, call_args)));
    None
}

/// The translation of one GPU module into a CPU function of the module `cpu`.
struct Tr<'g> {
    gpu: &'g ir::Module,
    f: ir::Function,
    types: HashMap<ir::TypeId, ir::TypeId>,
    vals: HashMap<ir::ValueId, ir::ValueId>,
    /// The input buffer's run, the output's address and length, the loop's counter.
    input: ir::ValueId,
    data: ir::ValueId,
    len: ir::ValueId,
    counter: ir::LocalId,
    /// The uniform block's local, and each private variable's.
    locals: HashMap<ir::ResourceId, ir::LocalId>,
    wg: [u32; 3],
    entry: usize,
}

type R<T> = Result<T, String>;

impl<'g> Tr<'g> {
    fn translate(
        cpu: &mut ir::Module,
        gpu: &'g ir::Module,
        wg: [u32; 3],
        name: &str,
    ) -> R<ir::FuncId> {
        let ep = gpu.entry_points.first().ok_or("no entry point")?;
        let entry = ep.function.index();
        let src = &gpu.functions[entry];
        let mut tr = Tr {
            gpu,
            f: ir::Function::default(),
            types: HashMap::new(),
            vals: HashMap::new(),
            input: ir::ValueId(0),
            data: ir::ValueId(0),
            len: ir::ValueId(0),
            counter: ir::LocalId(0),
            locals: HashMap::new(),
            wg,
            entry,
        };
        let u32t = cpu.types.u32();
        // The parameters: the input's run, the uniform block's value, the output.
        let mut input_elem = None;
        let mut uniform = None;
        for (i, r) in gpu.resources.iter().enumerate() {
            match r.kind {
                ir::ResourceKind::StorageRead => input_elem = Some(tr.ty(cpu, r.ty)?),
                ir::ResourceKind::Uniform { .. } => {
                    uniform = Some((ir::ResourceId(i as u32), tr.ty(cpu, r.ty)?))
                }
                _ => {}
            }
        }
        let input_elem = match input_elem {
            Some(t) => t,
            None => u32t,
        };
        let run_t = cpu.types.intern(ir::TypeDef::Run(input_elem));
        let uni_t = match uniform {
            Some((_, t)) => t,
            None => cpu
                .types
                .intern(ir::TypeDef::Struct { name: "NoParams".into(), fields: Vec::new() }),
        };
        tr.f = ir::Function::new(
            format!("{name}.run"),
            vec![
                ir::Param::value("input", run_t),
                ir::Param::value("params", uni_t),
                ir::Param::value("out", u32t),
                ir::Param::value("len", u32t),
            ],
            None,
        );
        let mut body = Vec::new();
        tr.input = tr.f.let_(&mut body, run_t, ir::Expr::Param(0));
        let params = tr.f.let_(&mut body, uni_t, ir::Expr::Param(1));
        tr.data = tr.f.let_(&mut body, u32t, ir::Expr::Param(2));
        tr.len = tr.f.let_(&mut body, u32t, ir::Expr::Param(3));
        // The kernel's locals, at their own indexes; then the loop's and the resources'.
        for l in &src.locals {
            let t = tr.ty(cpu, l.ty)?;
            tr.f.new_local(l.name.clone(), t);
        }
        tr.counter = tr.f.new_local("place", u32t);
        if let Some((r, t)) = uniform {
            let l = tr.f.new_local("params", t);
            body.push(ir::Stmt::Store(ir::Place::local(l), params));
            tr.locals.insert(r, l);
        }
        for (i, r) in gpu.resources.iter().enumerate() {
            if r.kind == ir::ResourceKind::Private {
                let t = tr.ty(cpu, r.ty)?;
                let l = tr.f.new_local(r.name.clone(), t);
                let z = tr.f.let_(&mut body, t, ir::Expr::Zero(t));
                body.push(ir::Stmt::Store(ir::Place::local(l), z));
                tr.locals.insert(ir::ResourceId(i as u32), l);
            }
        }
        let zero = tr.f.let_(&mut body, u32t, ir::Expr::Const(ir::Const::U32(0)));
        body.push(ir::Stmt::Store(ir::Place::local(tr.counter), zero));
        // The loop over the output's places.
        let mut each = Vec::new();
        let i = tr.f.let_(&mut each, u32t, ir::Expr::Load(ir::Place::local(tr.counter)));
        let bool_t = cpu.types.bool();
        let done = tr.f.let_(&mut each, bool_t, ir::Expr::Binary(ir::BinOp::Ge, i, tr.len));
        each.push(ir::Stmt::If { cond: done, then: vec![ir::Stmt::Break], else_: Vec::new() });
        let kernel_body = src.body.clone();
        tr.block(cpu, &kernel_body, &mut each)?;
        let mut continuing = Vec::new();
        let i2 = tr.f.let_(&mut continuing, u32t, ir::Expr::Load(ir::Place::local(tr.counter)));
        let one = tr.f.let_(&mut continuing, u32t, ir::Expr::Const(ir::Const::U32(1)));
        let next =
            tr.f.let_(&mut continuing, u32t, ir::Expr::Binary(ir::BinOp::WrappingAdd, i2, one));
        continuing.push(ir::Stmt::Store(ir::Place::local(tr.counter), next));
        body.push(ir::Stmt::Loop { body: each, continuing });
        body.push(ir::Stmt::Return(None));
        tr.f.body = body;
        cpu.functions.push(tr.f);
        Ok(ir::FuncId(cpu.functions.len() as u32 - 1))
    }

    /// A GPU type as the CPU module's.
    fn ty(&mut self, cpu: &mut ir::Module, t: ir::TypeId) -> R<ir::TypeId> {
        if let Some(&c) = self.types.get(&t) {
            return Ok(c);
        }
        let def = match self.gpu.types.get(t).clone() {
            d @ (ir::TypeDef::Scalar(_) | ir::TypeDef::Vector(..) | ir::TypeDef::Matrix(..)) => d,
            ir::TypeDef::Struct { name, fields } => {
                let mut fs = Vec::new();
                for (n, ft) in fields {
                    fs.push((n, self.ty(cpu, ft)?));
                }
                ir::TypeDef::Struct { name, fields: fs }
            }
            ir::TypeDef::Enum { name, variants, tags } => {
                let mut vs = Vec::new();
                for (n, p) in variants {
                    vs.push((n, p.map(|p| self.ty(cpu, p)).transpose()?));
                }
                ir::TypeDef::Enum { name, variants: vs, tags }
            }
            ir::TypeDef::Array(e, n) => ir::TypeDef::Array(self.ty(cpu, e)?, n),
            other => return Err(format!("a type the CPU form can't hold: {other:?}")),
        };
        let c = cpu.types.intern(def);
        self.types.insert(t, c);
        Ok(c)
    }

    fn val(&self, v: ir::ValueId) -> R<ir::ValueId> {
        self.vals.get(&v).copied().ok_or_else(|| format!("value {v:?} used before it's made"))
    }

    fn block(&mut self, cpu: &mut ir::Module, b: &[ir::Stmt], out: &mut ir::Block) -> R<()> {
        for s in b {
            self.stmt(cpu, s, out)?;
        }
        Ok(())
    }

    fn stmt(&mut self, cpu: &mut ir::Module, s: &ir::Stmt, out: &mut ir::Block) -> R<()> {
        match s {
            ir::Stmt::At(sp) => out.push(ir::Stmt::At(*sp)),
            ir::Stmt::Let(v, e) => {
                let t = self.ty(cpu, self.gpu.functions[self.entry].value_ty(*v))?;
                let e = self.expr(cpu, *v, e, t, out)?;
                let nv = self.f.let_(out, t, e);
                self.vals.insert(*v, nv);
            }
            ir::Stmt::Eval(e) => {
                let u32t = cpu.types.u32();
                let e = self.expr(cpu, ir::ValueId(u32::MAX), e, u32t, out)?;
                out.push(ir::Stmt::Eval(e));
            }
            ir::Stmt::Store(p, v) => {
                let p = self.place(cpu, p, out)?;
                out.push(ir::Stmt::Store(p, self.val(*v)?));
            }
            ir::Stmt::If { cond, then, else_ } => {
                let (mut t, mut e) = (Vec::new(), Vec::new());
                self.block(cpu, then, &mut t)?;
                self.block(cpu, else_, &mut e)?;
                out.push(ir::Stmt::If { cond: self.val(*cond)?, then: t, else_: e });
            }
            ir::Stmt::Loop { body, continuing } => {
                let (mut b, mut c) = (Vec::new(), Vec::new());
                self.block(cpu, body, &mut b)?;
                self.block(cpu, continuing, &mut c)?;
                out.push(ir::Stmt::Loop { body: b, continuing: c });
            }
            ir::Stmt::Break => out.push(ir::Stmt::Break),
            ir::Stmt::Continue => out.push(ir::Stmt::Continue),
            // The place's pass ends: the loop goes on to the next.
            ir::Stmt::Return(_) => out.push(ir::Stmt::Continue),
            ir::Stmt::Trap => out.push(ir::Stmt::Trap),
        }
        Ok(())
    }

    fn u32c(&mut self, cpu: &mut ir::Module, x: u32, out: &mut ir::Block) -> ir::ValueId {
        let t = cpu.types.u32();
        self.f.let_(out, t, ir::Expr::Const(ir::Const::U32(x)))
    }

    /// A place of the kernel's as the CPU function's: a buffer's element at its address.
    fn place(&mut self, cpu: &mut ir::Module, p: &ir::Place, out: &mut ir::Block) -> R<ir::Place> {
        let mut path = Vec::new();
        let rest = |tr: &mut Self, from: usize, path: &mut Vec<ir::Proj>| -> R<()> {
            for q in &p.path[from..] {
                path.push(match q {
                    ir::Proj::Index(v) => ir::Proj::Index(tr.val(*v)?),
                    other => other.clone(),
                });
            }
            Ok(())
        };
        let root = match &p.root {
            ir::PlaceRoot::Local(l) => {
                rest(self, 0, &mut path)?;
                ir::PlaceRoot::Local(*l)
            }
            ir::PlaceRoot::Resource(r) => {
                if let Some(&l) = self.locals.get(r) {
                    rest(self, 0, &mut path)?;
                    ir::PlaceRoot::Local(l)
                } else {
                    let res = &self.gpu.resources[r.index()];
                    let elem = self.ty(cpu, res.ty)?;
                    let Some(ir::Proj::Index(j)) = p.path.first() else {
                        return Err("a whole buffer read at once".into());
                    };
                    let j = self.val(*j)?;
                    let u32t = cpu.types.u32();
                    let base = match res.kind {
                        ir::ResourceKind::StorageRead => {
                            self.f.let_(out, u32t, ir::Expr::Extract(self.input, 0))
                        }
                        ir::ResourceKind::StorageReadWrite => self.data,
                        _ => {
                            return Err(format!(
                                "a resource the CPU form can't reach: {:?}",
                                res.kind
                            ));
                        }
                    };
                    let stride = ir::layout::array_stride(&cpu.types, elem);
                    let s = self.u32c(cpu, stride, out);
                    let off = self.f.let_(out, u32t, ir::Expr::Binary(ir::BinOp::Mul, j, s));
                    let addr = self.f.let_(out, u32t, ir::Expr::Binary(ir::BinOp::Add, base, off));
                    let pt = cpu.types.intern(ir::TypeDef::Ptr(elem));
                    let ptr = self.f.let_(out, pt, ir::Expr::Mem(ir::MemOp::Ptr, vec![addr]));
                    rest(self, 1, &mut path)?;
                    ir::PlaceRoot::Ptr(ptr)
                }
            }
            other => return Err(format!("a place the CPU form can't reach: {other:?}")),
        };
        Ok(ir::Place { root, path })
    }

    fn expr(
        &mut self,
        cpu: &mut ir::Module,
        v: ir::ValueId,
        e: &ir::Expr,
        t: ir::TypeId,
        out: &mut ir::Block,
    ) -> R<ir::Expr> {
        use crate::ir::Expr as E;
        let gf = &self.gpu.functions[self.entry];
        Ok(match e {
            E::Const(c) => E::Const(c.clone()),
            E::Zero(_) => E::Zero(t),
            E::Load(p) => E::Load(self.place(cpu, p, out)?),
            E::EntryInput(k) => {
                let ep = &self.gpu.entry_points[0];
                let which = ep.inputs.get(*k as usize).ok_or("an entry input past the entry's")?;
                let i = self.f.let_(out, cpu.types.u32(), E::Load(ir::Place::local(self.counter)));
                let (zero, one) = (self.u32c(cpu, 0, out), self.u32c(cpu, 1, out));
                let w = self.u32c(cpu, self.wg[0].max(1), out);
                let u32t = cpu.types.u32();
                let parts = match which {
                    ir::BuiltinInput::GlobalInvocationId => vec![i, zero, zero],
                    ir::BuiltinInput::LocalInvocationId => {
                        let l = self.f.let_(out, u32t, E::Binary(ir::BinOp::Rem, i, w));
                        vec![l, zero, zero]
                    }
                    ir::BuiltinInput::WorkgroupId => {
                        let g = self.f.let_(out, u32t, E::Binary(ir::BinOp::Div, i, w));
                        vec![g, zero, zero]
                    }
                    ir::BuiltinInput::NumWorkgroups => {
                        let wm1 = self.u32c(cpu, self.wg[0].max(1) - 1, out);
                        let up = self.f.let_(
                            out,
                            u32t,
                            E::Binary(ir::BinOp::WrappingAdd, self.len, wm1),
                        );
                        let g = self.f.let_(out, u32t, E::Binary(ir::BinOp::Div, up, w));
                        vec![g, one, one]
                    }
                    other => {
                        return Err(format!("a kernel input the CPU form doesn't give: {other:?}"));
                    }
                };
                match cpu.types.get(t) {
                    ir::TypeDef::Struct { .. } | ir::TypeDef::Vector(..) => E::Construct(t, parts),
                    _ => return Err("an entry input of an unexpected type".into()),
                }
            }
            E::ArrayLength(p) => match &p.root {
                ir::PlaceRoot::Resource(r) => match self.gpu.resources[r.index()].kind {
                    ir::ResourceKind::StorageRead => E::Extract(self.input, 1),
                    ir::ResourceKind::StorageReadWrite => {
                        let z = self.u32c(cpu, 0, out);
                        E::Binary(ir::BinOp::Add, self.len, z)
                    }
                    _ => return Err("the length of a resource that isn't a buffer".into()),
                },
                _ => return Err("the length of what isn't a buffer".into()),
            },
            E::Unary(ir::UnOp::Neg, a) if is_int(&self.gpu.types, gf.value_ty(*a)) => {
                // WGSL's `-` wraps.
                let a = self.val(*a)?;
                let z = self.f.let_(out, t, E::Zero(t));
                E::Binary(ir::BinOp::WrappingSub, z, a)
            }
            E::Unary(op, a) => E::Unary(*op, self.val(*a)?),
            E::Binary(op, a, b) => {
                let int = is_int(&self.gpu.types, gf.value_ty(*a));
                let signed = is_signed(&self.gpu.types, gf.value_ty(*a));
                let (a, b) = (self.val(*a)?, self.val(*b)?);
                if !int {
                    E::Binary(*op, a, b)
                } else {
                    use crate::ir::BinOp as B;
                    match op {
                        B::Add => E::Binary(B::WrappingAdd, a, b),
                        B::Sub => E::Binary(B::WrappingSub, a, b),
                        B::Mul => E::Binary(B::WrappingMul, a, b),
                        B::Div | B::Rem => self.division(cpu, *op, a, b, t, signed, out),
                        B::Shl | B::Shr => {
                            // WGSL masks a shift's amount to the width (32).
                            let bt = self.f.value_ty(b);
                            let lanes = match cpu.types.get(bt) {
                                ir::TypeDef::Vector(_, n) => Some(*n),
                                _ => None,
                            };
                            let m = self.u32c(cpu, 31, out);
                            let mask = match lanes {
                                Some(n) => self.f.let_(out, bt, E::Splat(m, n)),
                                None => m,
                            };
                            let masked = self.f.let_(out, bt, E::Binary(B::BitAnd, b, mask));
                            E::Binary(*op, a, masked)
                        }
                        other => E::Binary(*other, a, b),
                    }
                }
            }
            E::Builtin(b, xs) => {
                let xs = xs.iter().map(|x| self.val(*x)).collect::<R<Vec<_>>>()?;
                E::Builtin(*b, xs)
            }
            E::Construct(_, xs) => {
                let xs = xs.iter().map(|x| self.val(*x)).collect::<R<Vec<_>>>()?;
                E::Construct(t, xs)
            }
            E::Variant(_, k, x) => E::Variant(t, *k, x.map(|x| self.val(x)).transpose()?),
            E::Extract(a, k) => E::Extract(self.val(*a)?, *k),
            E::ExtractDyn(a, i) => E::ExtractDyn(self.val(*a)?, self.val(*i)?),
            E::Splat(a, n) => E::Splat(self.val(*a)?, *n),
            E::Swizzle(a, ls) => E::Swizzle(self.val(*a)?, ls.clone()),
            E::Convert(a, s) => E::Convert(self.val(*a)?, *s),
            E::Bitcast(a, s) => E::Bitcast(self.val(*a)?, *s),
            E::Select { cond, if_true, if_false } => E::Select {
                cond: self.val(*cond)?,
                if_true: self.val(*if_true)?,
                if_false: self.val(*if_false)?,
            },
            other => {
                let _ = v;
                return Err(format!("an operation the CPU form doesn't run: {other:?}"));
            }
        })
    }

    /// `a / b` or `a % b` as WGSL defines them: by zero, or `MIN / -1`, the dividend (`/`) and 0
    /// (`%`).
    #[allow(clippy::too_many_arguments)]
    fn division(
        &mut self,
        cpu: &mut ir::Module,
        op: ir::BinOp,
        a: ir::ValueId,
        b: ir::ValueId,
        t: ir::TypeId,
        signed: bool,
        out: &mut ir::Block,
    ) -> ir::Expr {
        use crate::ir::{BinOp as B, Const, Expr as E};
        let bt = match cpu.types.get(t) {
            ir::TypeDef::Vector(_, n) => {
                cpu.types.intern(ir::TypeDef::Vector(ir::Scalar::Bool, *n))
            }
            _ => cpu.types.bool(),
        };
        let at = self.f.value_ty(a);
        let lanes = match cpu.types.get(at).clone() {
            ir::TypeDef::Vector(sc, n) => Some((cpu.types.intern(ir::TypeDef::Scalar(sc)), n)),
            _ => None,
        };
        let splat = |tr: &mut Self, c: Const, out: &mut ir::Block| match lanes {
            Some((st, n)) => {
                let x = tr.f.let_(out, st, E::Const(c));
                tr.f.let_(out, at, E::Splat(x, n))
            }
            None => tr.f.let_(out, at, E::Const(c)),
        };
        let (zero, one) = if signed {
            (splat(self, Const::I32(0), out), splat(self, Const::I32(1), out))
        } else {
            (splat(self, Const::U32(0), out), splat(self, Const::U32(1), out))
        };
        let mut odd = self.f.let_(out, bt, E::Binary(B::Eq, b, zero));
        if signed {
            let min = splat(self, Const::I32(i32::MIN), out);
            let neg1 = splat(self, Const::I32(-1), out);
            let is_min = self.f.let_(out, bt, E::Binary(B::Eq, a, min));
            let is_neg1 = self.f.let_(out, bt, E::Binary(B::Eq, b, neg1));
            let both = self.f.let_(out, bt, E::Binary(B::And, is_min, is_neg1));
            odd = self.f.let_(out, bt, E::Binary(B::Or, odd, both));
        }
        let safe = self.f.let_(out, t, E::Select { cond: odd, if_true: one, if_false: b });
        let q = self.f.let_(out, t, E::Binary(op, a, safe));
        let defined = if op == B::Div { a } else { zero };
        E::Select { cond: odd, if_true: defined, if_false: q }
    }
}

fn is_int(types: &ir::Types, t: ir::TypeId) -> bool {
    matches!(types.get(t), ir::TypeDef::Scalar(s) | ir::TypeDef::Vector(s, _) if matches!(s, ir::Scalar::U32 | ir::Scalar::I32))
}

fn is_signed(types: &ir::Types, t: ir::TypeId) -> bool {
    matches!(
        types.get(t),
        ir::TypeDef::Scalar(ir::Scalar::I32) | ir::TypeDef::Vector(ir::Scalar::I32, _)
    )
}
