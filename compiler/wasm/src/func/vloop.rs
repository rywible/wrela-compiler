//! Loops run four iterations at a time, in SIMD (language.md §11, AC12).
//!
//! A loop qualifies when it counts a `u32` up by one to a bound computed before it (a `for` over
//! a range, `0..xs.len()`), and its body, after the exit test, is straight-line code that
//!
//! - reads and writes `f32` elements at the counter's index, and only there, of arrays and
//!   runs that the loop doesn't replace (so iteration `i` touches element `i` alone);
//! - computes with `f32` arithmetic, comparisons and `select`, the componentwise built-ins
//!   (`min`, `sqrt`, `mix`, ...), the counter as an `f32`, and values from before the loop;
//! - keeps nothing from one iteration to the next: a local it writes, it writes before it reads
//!   it, and nothing outside the loop reads it.
//!
//! Then iterations `i..i + 4` run as one, with each `f32` in a lane, while the bound and every
//! array or run indexed have four more elements. The loop itself runs the rest, one at a time,
//! from where the four-wide loop stopped: so the last iterations, and one that would index out
//! of range (which traps), run as they would have. Each lane computes with the instruction the
//! scalar code uses, so the bits are the same. Nothing in a four-wide iteration traps, and it
//! writes only the elements the four iterations would: the memory after it is the same too.
//! Debug builds don't use it: their NaN checks panic at an iteration.

use super::{Fe, LocalRepr, R, mem, ops};
use std::collections::{HashMap, HashSet};
use wasm_encoder::{Instruction as I, ValType};
use wrela_ir as ir;

/// An array or run the body indexes: a place, or a value from before the loop.
#[derive(Clone, PartialEq)]
enum Root {
    Place(ir::Place),
    Value(ir::ValueId),
}

/// A loop that runs four iterations at a time.
pub(super) struct Plan<'a> {
    /// The counter's WASM local.
    counter: u32,
    /// The bound the counter stops at (a value from before the loop).
    bound: ir::ValueId,
    /// The counter's value in the body.
    index: ir::ValueId,
    /// The body after the exit test.
    body: Vec<&'a ir::Stmt>,
    /// The arrays and runs indexed, and whether each is a run (else an array of that length).
    roots: Vec<(Root, Option<u32>)>,
    /// The locals the body writes before it reads them.
    temps: HashSet<ir::LocalId>,
}

/// What a value of the body is, four iterations at a time.
#[derive(Clone, Copy)]
enum Lanes {
    /// The counter: lane `k` is `i + k`.
    Index,
    /// The same in every lane: an `f32` (or `bool`) in this WASM local.
    Same(u32),
    /// A `v128`, each lane its iteration's `f32`, or its comparison's mask.
    Each(u32),
}

fn skip_at(b: &[ir::Stmt]) -> Vec<&ir::Stmt> {
    b.iter().filter(|s| !matches!(s, ir::Stmt::At(_))).collect()
}

/// The plan for a loop with this body and continuing block, if it qualifies.
pub(super) fn plan<'a>(fe: &Fe, body: &'a [ir::Stmt], continuing: &[ir::Stmt]) -> Option<Plan<'a>> {
    let stmts = skip_at(body);
    let [head, test, exit, rest @ ..] = stmts.as_slice() else { return None };
    // The head: i = counter; if i >= bound { break }.
    let ir::Stmt::Let(index, ir::Expr::Load(cp)) = head else { return None };
    let ctr = cp.root_local().filter(|_| cp.path.is_empty())?;
    let LocalRepr::Scalar(counter) = fe.ir_locals[ctr.index()] else { return None };
    if counter == u32::MAX || fe.m.types.as_scalar(fe.f.locals[ctr.index()].ty)? != ir::Scalar::U32
    {
        return None;
    }
    let ir::Stmt::Let(t, ir::Expr::Binary(ir::BinOp::Ge, i2, bound)) = test else { return None };
    let ir::Stmt::If { cond, then, else_ } = exit else { return None };
    if i2 != index || cond != t || !skip_at(else_).is_empty() {
        return None;
    }
    if !matches!(skip_at(then).as_slice(), [ir::Stmt::Break]) {
        return None;
    }
    // The continuing block: counter = counter + 1.
    let cont = skip_at(continuing);
    let [
        ir::Stmt::Let(a, ir::Expr::Load(ap)),
        ir::Stmt::Let(one, ir::Expr::Const(ir::Const::U32(1))),
        ir::Stmt::Let(sum, ir::Expr::Binary(ir::BinOp::Add, x, y)),
        ir::Stmt::Store(sp, z),
    ] = cont.as_slice()
    else {
        return None;
    };
    if ap != cp || sp != cp || x != a || y != one || z != sum {
        return None;
    }
    let defined: HashSet<ir::ValueId> = rest
        .iter()
        .filter_map(|s| match s {
            ir::Stmt::Let(v, _) => Some(*v),
            _ => None,
        })
        .collect();
    if defined.contains(bound) || fe.values[bound.index()] == u32::MAX {
        return None;
    }
    let mut p = Planner {
        fe,
        index: *index,
        ctr,
        defined,
        roots: Vec::new(),
        temps: HashSet::new(),
        stored: HashSet::new(),
        kinds: HashMap::new(),
    };
    p.check(rest)?;
    Some(Plan {
        counter,
        bound: *bound,
        index: *index,
        body: rest.to_vec(),
        roots: p.roots,
        temps: p.temps,
    })
}

/// What a body value is, while planning.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Index,
    F32,
    Mask,
}

struct Planner<'f, 'm> {
    fe: &'f Fe<'m>,
    index: ir::ValueId,
    ctr: ir::LocalId,
    /// The values the body defines.
    defined: HashSet<ir::ValueId>,
    roots: Vec<(Root, Option<u32>)>,
    temps: HashSet<ir::LocalId>,
    /// The temporaries stored so far, in body order.
    stored: HashSet<ir::LocalId>,
    kinds: HashMap<ir::ValueId, Kind>,
}

impl Planner<'_, '_> {
    fn scalar(&self, t: ir::TypeId) -> Option<ir::Scalar> {
        self.fe.m.types.as_scalar(t)
    }

    /// An operand's kind: a body value's, the counter, or a value from before the loop (an
    /// `f32`, or a `bool` for a `select`).
    fn operand(&self, x: ir::ValueId) -> Option<Kind> {
        if x == self.index {
            return Some(Kind::Index);
        }
        if self.defined.contains(&x) {
            return self.kinds.get(&x).copied();
        }
        match self.scalar(self.fe.vty(x))? {
            ir::Scalar::F32 if self.fe.values[x.index()] != u32::MAX => Some(Kind::F32),
            ir::Scalar::Bool if self.fe.values[x.index()] != u32::MAX => Some(Kind::Mask),
            _ => None,
        }
    }

    fn f32_operand(&self, x: ir::ValueId) -> Option<()> {
        (self.operand(x)? == Kind::F32).then_some(())
    }

    /// The root an element access at the counter's index goes through, noted; `None` if it
    /// isn't one.
    fn element(&mut self, root: Root, root_ty: ir::TypeId) -> Option<()> {
        let (elem, len) = match self.fe.m.types.get(root_ty) {
            ir::TypeDef::Run(e) => (*e, None),
            ir::TypeDef::Array(e, n) => (*e, Some(*n)),
            _ => return None,
        };
        if self.scalar(elem)? != ir::Scalar::F32 {
            return None;
        }
        if !self.roots.iter().any(|(r, _)| *r == root) {
            self.roots.push((root, len));
        }
        Some(())
    }

    /// The type of an element place's root, if the place is `root[i]` with `root` a local the
    /// loop doesn't write whole, or a by-reference parameter.
    fn element_place(&mut self, p: &ir::Place) -> Option<()> {
        let [ir::Proj::Index(i)] = p.path.as_slice() else { return None };
        if *i != self.index {
            return None;
        }
        let root = ir::Place { root: p.root.clone(), path: Vec::new() };
        let ty = match &p.root {
            ir::PlaceRoot::Local(l) if *l != self.ctr && !self.temps.contains(l) => {
                self.fe.f.locals[l.index()].ty
            }
            ir::PlaceRoot::Param(k) if self.fe.f.params[*k as usize].by_ref => {
                self.fe.f.params[*k as usize].ty
            }
            _ => return None,
        };
        self.element(Root::Place(root), ty)
    }

    fn check(&mut self, body: &[&ir::Stmt]) -> Option<()> {
        // The locals the body stores whole: temporaries, which nothing outside it may mention.
        let mut inside = vec![0u32; self.fe.f.locals.len()];
        let mut whole = HashSet::new();
        for s in body {
            super::mentions(s, &mut |l| inside[l.index()] += 1);
            if let ir::Stmt::Store(p, _) = s
                && p.path.is_empty()
                && let Some(l) = p.root_local()
            {
                whole.insert(l);
            }
        }
        let total = &self.fe.local_mentions;
        for &l in &whole {
            let f32_ = self.scalar(self.fe.f.locals[l.index()].ty) == Some(ir::Scalar::F32);
            let scalar = matches!(self.fe.ir_locals[l.index()], LocalRepr::Scalar(_));
            if l == self.ctr || !f32_ || !scalar || inside[l.index()] != total[l.index()] {
                return None;
            }
            self.temps.insert(l);
        }
        for s in body {
            match s {
                ir::Stmt::Let(v, e) => {
                    let k = self.expr(*v, e)?;
                    self.kinds.insert(*v, k);
                }
                ir::Stmt::Store(p, x) => {
                    match p.root_local() {
                        Some(l) if p.path.is_empty() && self.temps.contains(&l) => {
                            self.stored.insert(l);
                        }
                        _ => self.element_place(p)?,
                    }
                    self.f32_operand(*x)?;
                }
                _ => return None,
            }
        }
        // No element is also reached another way: a root that's indexed is only indexed.
        let roots: Vec<Root> = self.roots.iter().map(|(r, _)| r.clone()).collect();
        for s in body {
            let mut other = false;
            s.for_each_place(&mut |p| {
                for r in &roots {
                    if let Root::Place(rp) = r
                        && p.root == rp.root
                        && p.path.as_slice() != [ir::Proj::Index(self.index)]
                    {
                        other = true;
                    }
                }
            });
            if other {
                return None;
            }
        }
        Some(())
    }

    fn expr(&mut self, v: ir::ValueId, e: &ir::Expr) -> Option<Kind> {
        use ir::BinOp as B;
        let ty = self.scalar(self.fe.vty(v))?;
        match e {
            ir::Expr::Const(ir::Const::F32(_)) => Some(Kind::F32),
            ir::Expr::Load(p) => {
                if ty != ir::Scalar::F32 {
                    return None;
                }
                match p.root_local() {
                    // A temporary, written already in this iteration.
                    Some(l) if p.path.is_empty() && self.temps.contains(&l) => {
                        self.stored.contains(&l).then_some(Kind::F32)
                    }
                    // A local the loop doesn't write: the same in every iteration.
                    Some(l)
                        if p.path.is_empty()
                            && l != self.ctr
                            && matches!(self.fe.ir_locals[l.index()], LocalRepr::Scalar(w) if w != u32::MAX) =>
                    {
                        Some(Kind::F32)
                    }
                    _ => {
                        self.element_place(p)?;
                        Some(Kind::F32)
                    }
                }
            }
            ir::Expr::ExtractDyn(x, i) if *i == self.index && !self.defined.contains(x) => {
                if ty != ir::Scalar::F32 || self.fe.values[x.index()] == u32::MAX {
                    return None;
                }
                self.element(Root::Value(*x), self.fe.vty(*x))?;
                Some(Kind::F32)
            }
            ir::Expr::Binary(op, a, b) => {
                self.f32_operand(*a)?;
                self.f32_operand(*b)?;
                match op {
                    B::Add | B::Sub | B::Mul | B::Div => Some(Kind::F32),
                    B::Eq | B::Ne | B::Lt | B::Le | B::Gt | B::Ge => Some(Kind::Mask),
                    _ => None,
                }
            }
            ir::Expr::Unary(ir::UnOp::Neg, a) => {
                self.f32_operand(*a)?;
                Some(Kind::F32)
            }
            ir::Expr::Builtin(b, args) if ops::lanes_builtin_ok(*b) && ty == ir::Scalar::F32 => {
                for a in args {
                    self.f32_operand(*a)?;
                }
                Some(Kind::F32)
            }
            ir::Expr::Convert(x, ir::Scalar::F32) if *x == self.index => Some(Kind::F32),
            ir::Expr::Select { cond, if_true, if_false } if ty == ir::Scalar::F32 => {
                (self.operand(*cond)? == Kind::Mask).then_some(())?;
                self.f32_operand(*if_true)?;
                self.f32_operand(*if_false)?;
                Some(Kind::F32)
            }
            _ => None,
        }
    }
}

/// Emits the four-wide loop, which leaves the counter where the loop itself goes on.
pub(super) fn emit(fe: &mut Fe, plan: &Plan) -> R<()> {
    let w = plan.counter;
    // Each root's first element's address and length, once: the loop doesn't change them.
    let mut bases = Vec::new();
    for (root, len) in &plan.roots {
        let base = fe.new_local(ValType::I32);
        let n = fe.new_local(ValType::I32);
        match root {
            Root::Place(p) => {
                fe.addr(p)?;
            }
            Root::Value(x) => fe.ins.push(I::LocalGet(fe.v(*x))),
        }
        match len {
            // A run is its elements' address, then their count.
            None => fe.ins.extend([
                I::LocalTee(base),
                I::I32Load(mem(4, 2)),
                I::LocalSet(n),
                I::LocalGet(base),
                I::I32Load(mem(0, 2)),
                I::LocalSet(base),
            ]),
            Some(len) => {
                fe.ins.extend([I::LocalSet(base), I::I32Const(*len as i32), I::LocalSet(n)])
            }
        }
        bases.push((root.clone(), base, n));
    }
    let temps: HashMap<ir::LocalId, u32> =
        plan.temps.iter().map(|&l| (l, fe.new_local(ValType::V128))).collect();
    fe.ins.push(I::Block(wasm_encoder::BlockType::Empty));
    fe.ins.push(I::Loop(wasm_encoder::BlockType::Empty));
    // On while i < bound and bound - i >= 4, and so for each root's length.
    let lens: Vec<I<'static>> = std::iter::once(I::LocalGet(fe.v(plan.bound)))
        .chain(bases.iter().map(|&(_, _, n)| I::LocalGet(n)))
        .collect();
    for len in lens {
        fe.ins.extend([
            len.clone(),
            I::LocalGet(w),
            I::I32LeU,
            I::BrIf(1),
            len,
            I::LocalGet(w),
            I::I32Sub,
            I::I32Const(4),
            I::I32LtU,
            I::BrIf(1),
        ]);
    }
    let mut vals: HashMap<ir::ValueId, Lanes> = HashMap::new();
    vals.insert(plan.index, Lanes::Index);
    for s in &plan.body {
        match s {
            ir::Stmt::Let(v, e) => {
                let l = expr(fe, plan, &bases, &temps, &vals, e)?;
                vals.insert(*v, l);
            }
            ir::Stmt::Store(p, x) => match p.root_local().and_then(|l| temps.get(&l)) {
                Some(&t) if p.path.is_empty() => {
                    push(fe, &vals, *x)?;
                    fe.ins.push(I::LocalSet(t));
                }
                _ => {
                    let root = Root::Place(ir::Place { root: p.root.clone(), path: Vec::new() });
                    element_addr(fe, plan, &bases, &root)?;
                    push(fe, &vals, *x)?;
                    fe.ins.push(I::V128Store(mem(0, 2)));
                }
            },
            _ => return Err("internal: a statement a four-wide loop can't have".into()),
        }
    }
    fe.ins.extend([
        I::LocalGet(w),
        I::I32Const(4),
        I::I32Add,
        I::LocalSet(w),
        I::Br(0),
        I::End,
        I::End,
    ]);
    Ok(())
}

/// Pushes the address of element `i` of `root`, for the four from it.
fn element_addr(fe: &mut Fe, plan: &Plan, bases: &[(Root, u32, u32)], root: &Root) -> R<()> {
    let &(_, base, _) =
        bases.iter().find(|(r, ..)| r == root).ok_or("internal: a root that wasn't planned")?;
    fe.ins.extend([
        I::LocalGet(base),
        I::LocalGet(plan.counter),
        I::I32Const(4),
        I::I32Mul,
        I::I32Add,
    ]);
    Ok(())
}

/// Pushes value `x` as a `v128`: its lanes, or itself in every lane.
fn push(fe: &mut Fe, vals: &HashMap<ir::ValueId, Lanes>, x: ir::ValueId) -> R<()> {
    match vals.get(&x) {
        Some(Lanes::Each(l)) => fe.ins.push(I::LocalGet(*l)),
        Some(Lanes::Same(l)) => fe.ins.extend([I::LocalGet(*l), I::F32x4Splat]),
        Some(Lanes::Index) => return Err("internal: the counter as an f32".into()),
        // A value from before the loop.
        None => match fe.m.types.as_scalar(fe.vty(x)) {
            Some(ir::Scalar::Bool) => {
                // A mask: all ones where true.
                fe.ins.extend([I::I32Const(0), I::LocalGet(fe.v(x)), I::I32Sub, I::I32x4Splat]);
            }
            _ => fe.ins.extend([I::LocalGet(fe.v(x)), I::F32x4Splat]),
        },
    }
    Ok(())
}

fn expr(
    fe: &mut Fe,
    plan: &Plan,
    bases: &[(Root, u32, u32)],
    temps: &HashMap<ir::LocalId, u32>,
    vals: &HashMap<ir::ValueId, Lanes>,
    e: &ir::Expr,
) -> R<Lanes> {
    use ir::BinOp as B;
    match e {
        ir::Expr::Const(ir::Const::F32(x)) => {
            let l = fe.new_local(ValType::F32);
            fe.ins.extend([I::F32Const((*x).into()), I::LocalSet(l)]);
            return Ok(Lanes::Same(l));
        }
        ir::Expr::Load(p) => match p.root_local() {
            Some(l) if p.path.is_empty() && temps.contains_key(&l) => {
                fe.ins.push(I::LocalGet(temps[&l]));
            }
            Some(l) if p.path.is_empty() => match fe.ir_locals[l.index()] {
                LocalRepr::Scalar(w) => return Ok(Lanes::Same(w)),
                LocalRepr::Slot(_) => return Err("internal: a planned load from memory".into()),
            },
            _ => {
                let root = Root::Place(ir::Place { root: p.root.clone(), path: Vec::new() });
                element_addr(fe, plan, bases, &root)?;
                fe.ins.push(I::V128Load(mem(0, 2)));
            }
        },
        ir::Expr::ExtractDyn(x, _) => {
            element_addr(fe, plan, bases, &Root::Value(*x))?;
            fe.ins.push(I::V128Load(mem(0, 2)));
        }
        ir::Expr::Binary(op, a, b) => {
            push(fe, vals, *a)?;
            push(fe, vals, *b)?;
            fe.ins.push(match op {
                B::Add => I::F32x4Add,
                B::Sub => I::F32x4Sub,
                B::Mul => I::F32x4Mul,
                B::Div => I::F32x4Div,
                B::Eq => I::F32x4Eq,
                B::Ne => I::F32x4Ne,
                B::Lt => I::F32x4Lt,
                B::Le => I::F32x4Le,
                B::Gt => I::F32x4Gt,
                B::Ge => I::F32x4Ge,
                _ => return Err(format!("internal: a planned {op:?}")),
            });
        }
        ir::Expr::Unary(_, a) => {
            push(fe, vals, *a)?;
            fe.ins.push(I::F32x4Neg);
        }
        ir::Expr::Builtin(b, args) => {
            let mut err = None;
            ops::lanes_builtin(fe, *b, &mut |fe, i| {
                if let Err(e) = push(fe, vals, args[i]) {
                    err = Some(e);
                }
            })?;
            if let Some(e) = err {
                return Err(e);
            }
        }
        ir::Expr::Convert(..) => {
            // Lanes i, i + 1, i + 2, i + 3, as f32s: each converted as the scalar is.
            let lanes = 1i128 << 32 | 2i128 << 64 | 3i128 << 96;
            fe.ins.extend([
                I::LocalGet(plan.counter),
                I::I32x4Splat,
                I::V128Const(lanes),
                I::I32x4Add,
                I::F32x4ConvertI32x4U,
            ]);
        }
        ir::Expr::Select { cond, if_true, if_false } => {
            push(fe, vals, *if_true)?;
            push(fe, vals, *if_false)?;
            push(fe, vals, *cond)?;
            fe.ins.push(I::V128Bitselect);
        }
        _ => return Err(format!("internal: a planned {e:?} in a four-wide loop")),
    }
    let out = fe.new_local(ValType::V128);
    fe.ins.push(I::LocalSet(out));
    Ok(Lanes::Each(out))
}
