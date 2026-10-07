//! Vectors whose components aren't `f32`s, on the CPU (language.md §4): their arithmetic a
//! component at a time, as scalar code, so an integer component's is checked as a scalar's is
//! (WASM's integer SIMD wraps). The WASM back end keeps these vectors in memory and does only
//! `f32` vectors' arithmetic in SIMD; the GPU does every vector's itself.

use crate::*;

/// Expands the arithmetic on vectors of `i32`s, `u32`s and `f64`s in every function of a CPU
/// module into scalar arithmetic on their components.
pub fn scalarize_vectors(m: &mut Module) {
    for i in 0..m.functions.len() {
        let mut f = std::mem::take(&mut m.functions[i]);
        let body = std::mem::take(&mut f.body);
        f.body = block(m, &mut f, body);
        m.functions[i] = f;
    }
}

/// A vector type whose components aren't `f32`s: its component and size.
fn other_vector(m: &Module, t: TypeId) -> Option<(Scalar, u8)> {
    match *m.types.get(t) {
        TypeDef::Vector(s, n) if s != Scalar::F32 => Some((s, n)),
        _ => None,
    }
}

fn block(m: &mut Module, f: &mut Function, b: Block) -> Block {
    let mut out = Vec::with_capacity(b.len());
    for mut s in b {
        for inner in s.blocks_mut() {
            let taken = std::mem::take(inner);
            *inner = block(m, f, taken);
        }
        match s {
            Stmt::Let(v, e) => {
                if !expand(m, f, v, &e, &mut out) {
                    out.push(Stmt::Let(v, e));
                }
            }
            s => out.push(s),
        }
    }
    out
}

struct Emit<'a> {
    m: &'a mut Module,
    f: &'a mut Function,
    out: &'a mut Block,
}

impl Emit<'_> {
    fn let_(&mut self, t: TypeId, e: Expr) -> ValueId {
        let v = self.f.new_value(t);
        self.out.push(Stmt::Let(v, e));
        v
    }

    /// Component `i` of `x`, or `x` itself if it's a scalar.
    fn comp(&mut self, x: ValueId, i: u32) -> ValueId {
        let t = self.f.value_ty(x);
        match *self.m.types.get(t) {
            TypeDef::Vector(s, _) => {
                let ct = self.m.types.scalar(s);
                self.let_(ct, Expr::Extract(x, i))
            }
            _ => x,
        }
    }

    /// `f(i)` for each component of an `n`-vector of `s`.
    fn each(
        &mut self,
        s: Scalar,
        n: u8,
        mut f: impl FnMut(&mut Self, TypeId, u32) -> Expr,
    ) -> Vec<ValueId> {
        let ct = self.m.types.scalar(s);
        (0..u32::from(n))
            .map(|i| {
                let e = f(self, ct, i);
                self.let_(ct, e)
            })
            .collect()
    }

    /// The sum of the components' products of `a` and `b`, in order: WGSL's `dot`.
    fn dot(&mut self, a: ValueId, b: ValueId, s: Scalar, n: u8) -> ValueId {
        let ct = self.m.types.scalar(s);
        let mut sum = None;
        for i in 0..u32::from(n) {
            let (x, y) = (self.comp(a, i), self.comp(b, i));
            let p = self.let_(ct, Expr::Binary(BinOp::Mul, x, y));
            sum = Some(match sum {
                None => p,
                Some(acc) => self.let_(ct, Expr::Binary(BinOp::Add, acc, p)),
            });
        }
        sum.expect("a vector has components")
    }
}

/// Expands `let v = e` if it computes with a vector whose components aren't `f32`s; whether it
/// did.
fn expand(m: &mut Module, f: &mut Function, v: ValueId, e: &Expr, out: &mut Block) -> bool {
    let vt = f.value_ty(v);
    let result = other_vector(m, vt);
    let operand = |x: ValueId| other_vector(m, f.value_ty(x));
    let (s, n) = match e {
        Expr::Binary(_, a, b) => match result.or_else(|| operand(*a)).or_else(|| operand(*b)) {
            Some(d) => d,
            None => return false,
        },
        Expr::Unary(_, a) => match result.or_else(|| operand(*a)) {
            Some(d) => d,
            None => return false,
        },
        Expr::Convert(x, _) | Expr::Swizzle(x, _) => match result.or_else(|| operand(*x)) {
            Some(d) => d,
            None => return false,
        },
        Expr::Splat(..) => match result {
            Some(d) => d,
            None => return false,
        },
        Expr::Builtin(_, args) => match args.iter().find_map(|&a| operand(a)).or(result) {
            Some(d) => d,
            None => return false,
        },
        _ => return false,
    };
    let mut em = Emit { m, f, out };
    let e = match e {
        // A comparison of vectors is `AllEqual`'s: only arithmetic here.
        Expr::Binary(op, a, b) => {
            let parts = em.each(s, n, |em, _, i| Expr::Binary(*op, em.comp(*a, i), em.comp(*b, i)));
            Expr::Construct(vt, parts)
        }
        Expr::Unary(op, a) => {
            let parts = em.each(s, n, |em, _, i| Expr::Unary(*op, em.comp(*a, i)));
            Expr::Construct(vt, parts)
        }
        // To or from another vector: each component converted.
        Expr::Convert(x, to) => {
            let parts = (0..u32::from(n))
                .map(|i| {
                    let c = em.comp(*x, i);
                    let t = em.m.types.scalar(*to);
                    em.let_(t, Expr::Convert(c, *to))
                })
                .collect();
            Expr::Construct(vt, parts)
        }
        Expr::Splat(x, k) => Expr::Construct(vt, vec![*x; usize::from(*k)]),
        Expr::Swizzle(x, cs) => {
            let parts = cs.iter().map(|&c| em.comp(*x, u32::from(c))).collect();
            Expr::Construct(vt, parts)
        }
        Expr::Builtin(b, args) if b.is_elementwise() => {
            let parts = em.each(s, n, |em, _, i| {
                Expr::Builtin(*b, args.iter().map(|&a| em.comp(a, i)).collect())
            });
            Expr::Construct(vt, parts)
        }
        // A scalar: the sum's last statement defines `v` itself.
        Expr::Builtin(Builtin::Dot, args) => {
            let d = em.dot(args[0], args[1], s, n);
            rename_last(em.out, d, v);
            return true;
        }
        Expr::Builtin(Builtin::AllEqual, args) => {
            let bt = em.m.types.bool();
            let mut all = None;
            for i in 0..u32::from(n) {
                let (x, y) = (em.comp(args[0], i), em.comp(args[1], i));
                let eq = em.let_(bt, Expr::Binary(BinOp::Eq, x, y));
                all = Some(match all {
                    None => eq,
                    Some(acc) => em.let_(bt, Expr::Binary(BinOp::And, acc, eq)),
                });
            }
            rename_last(em.out, all.expect("a vector has components"), v);
            return true;
        }
        // f64 vectors' geometry.
        Expr::Builtin(Builtin::Length, args) => {
            let d = em.dot(args[0], args[0], s, n);
            Expr::Builtin(Builtin::Sqrt, vec![d])
        }
        Expr::Builtin(Builtin::Distance, args) => {
            let parts = em.each(s, n, |em, _, i| {
                Expr::Binary(BinOp::Sub, em.comp(args[0], i), em.comp(args[1], i))
            });
            let diff_t = em.f.value_ty(args[0]);
            let diff = em.let_(diff_t, Expr::Construct(diff_t, parts));
            let d = em.dot(diff, diff, s, n);
            Expr::Builtin(Builtin::Sqrt, vec![d])
        }
        Expr::Builtin(Builtin::Normalize, args) => {
            let d = em.dot(args[0], args[0], s, n);
            let ct = em.m.types.scalar(s);
            let len = em.let_(ct, Expr::Builtin(Builtin::Sqrt, vec![d]));
            let parts =
                em.each(s, n, |em, _, i| Expr::Binary(BinOp::Div, em.comp(args[0], i), len));
            Expr::Construct(vt, parts)
        }
        Expr::Builtin(Builtin::Cross, args) => {
            let ct = em.m.types.scalar(s);
            let (a, b) = (args[0], args[1]);
            let c: Vec<ValueId> = (0..3).map(|i| em.comp(a, i)).collect();
            let d: Vec<ValueId> = (0..3).map(|i| em.comp(b, i)).collect();
            let part = |em: &mut Emit, i: usize, j: usize| {
                let p = em.let_(ct, Expr::Binary(BinOp::Mul, c[i], d[j]));
                let q = em.let_(ct, Expr::Binary(BinOp::Mul, c[j], d[i]));
                em.let_(ct, Expr::Binary(BinOp::Sub, p, q))
            };
            let parts = vec![part(&mut em, 1, 2), part(&mut em, 2, 0), part(&mut em, 0, 1)];
            Expr::Construct(vt, parts)
        }
        _ => return false,
    };
    em.out.push(Stmt::Let(v, e));
    true
}

/// Makes the last statement, which defines `from`, define `to` instead.
fn rename_last(out: &mut Block, from: ValueId, to: ValueId) {
    if let Some(Stmt::Let(v, _)) = out.last_mut()
        && *v == from
    {
        *v = to;
    }
}
