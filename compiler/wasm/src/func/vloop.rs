//! Loops run four iterations at a time, in SIMD (language.md §11, AC12).
//!
//! A loop qualifies when it counts a `u32` up by one (an `Add`, or the `WrappingAdd` a range proof
//! makes it, `ir::ranges`) to a bound computed before it (a `for` over
//! a range, `0..xs.len()`), and its body, after the exit test, is straight-line code that
//!
//! - reads and writes `f32` elements at the counter's index, and only there, of arrays and
//!   runs that the loop doesn't replace (so iteration `i` touches element `i` alone), or of a
//!   `Vec`: through the pointer `data + i * 4` its indexing makes (`vec_elem`), from a `data`
//!   the loop can't change;
//! - checks the counter only against values the loop can't change (`if i >= n { panic }`, a
//!   `Vec`'s bounds check, where a range proof hasn't removed it);
//! - computes with `f32` arithmetic, comparisons and `select`, the componentwise built-ins
//!   (`min`, `sqrt`, `mix`, ...), the counter as an `f32`, and values from before the loop;
//! - keeps nothing from one iteration to the next: a local it writes, it writes before it reads
//!   it, and nothing outside the loop reads it.
//!
//! Then iterations `i..i + 4` run as one, with each `f32` in a lane, while the bound, every
//! array or run indexed, and every value a check holds `i` below have four more elements. The loop itself runs the rest, one at a time,
//! from where the four-wide loop stopped: so the last iterations, and one that would index out
//! of range (which traps), run as they would have. Each lane computes with the instruction the
//! scalar code uses, so the bits are the same. Nothing in a four-wide iteration traps, and it
//! writes only the elements the four iterations would: the memory after it is the same too.
//! Debug builds don't use it: their NaN checks panic at an iteration.

use super::{Fe, LocalRepr, R, mem, ops};
use std::collections::{HashMap, HashSet};
use wasm_encoder::{Instruction as I, ValType};
use wrela_ir as ir;

/// An array or run the body indexes: a place, or a value from before the loop; or elements
/// reached through a pointer, `base + i * 4`, from one of the loop's words (a `Vec`'s, §6.10).
#[derive(Clone, PartialEq)]
enum Root {
    Place(ir::Place),
    Value(ir::ValueId),
    Base(usize),
}

/// How many elements a root has, which the four-wide loop stays within.
#[derive(Clone, Copy, PartialEq)]
enum Len {
    /// A run's own count.
    Run,
    /// An array's length.
    Array(u32),
    /// Whatever the loop's checks say: a `Vec`'s elements, each read behind a check that `i` is
    /// below a word (one of `bounds`), or whose check a range proof removed (`ir::ranges`).
    Free,
}

/// A `u32` the loop doesn't change: a place's (loaded once, before the four-wide loop), or a
/// value from before the loop.
#[derive(Clone, PartialEq)]
enum Word {
    Place(ir::Place),
    Value(ir::ValueId),
}

impl Root {
    /// The root of element place `p`: the place without its index.
    fn of(p: &ir::Place) -> Root {
        Root::Place(ir::Place::root(p.root.clone()))
    }
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
    /// The arrays, runs and pointers indexed, and how many elements each has.
    roots: Vec<(Root, Len)>,
    /// The locals the body writes before it reads them.
    temps: HashSet<ir::LocalId>,
    /// The loop's words, and those its checks hold `i` below.
    words: Vec<Word>,
    bounds: Vec<usize>,
    /// Each element pointer's word (its base), and the body's values that emit nothing in the
    /// four-wide loop (words, offsets, addresses, pointers, checks' conditions).
    ptrs: HashMap<ir::ValueId, usize>,
    quiet: HashSet<ir::ValueId>,
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
        ir::Stmt::Let(sum, ir::Expr::Binary(ir::BinOp::Add | ir::BinOp::WrappingAdd, x, y)),
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
        words: Vec::new(),
        bounds: Vec::new(),
        consts: HashMap::new(),
    };
    ir::visit::walk(&fe.f.body, &mut |s| {
        if let ir::Stmt::Let(v, ir::Expr::Const(ir::Const::U32(n))) = s {
            p.consts.insert(*v, *n);
        }
    });
    p.check(rest)?;
    let mut ptrs = HashMap::new();
    let mut quiet = HashSet::new();
    for (v, k) in &p.kinds {
        match k {
            Kind::Ptr(w) => {
                ptrs.insert(*v, *w);
                quiet.insert(*v);
            }
            Kind::Word(_) | Kind::Num(_) | Kind::Offset | Kind::Addr(_) | Kind::Check(_) => {
                quiet.insert(*v);
            }
            _ => {}
        }
    }
    Some(Plan {
        counter,
        bound: *bound,
        index: *index,
        body: rest.to_vec(),
        roots: p.roots,
        temps: p.temps,
        words: p.words,
        bounds: p.bounds,
        ptrs,
        quiet,
    })
}

/// What a body value is, while planning.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Index,
    F32,
    Mask,
    /// One of the loop's words.
    Word(usize),
    /// A `u32` constant.
    Num(u32),
    /// `i * 4`.
    Offset,
    /// `base + i * 4`, and as a pointer to an `f32`.
    Addr(usize),
    Ptr(usize),
    /// `i >= word`.
    Check(usize),
}

struct Planner<'f, 'm> {
    fe: &'f Fe<'m>,
    index: ir::ValueId,
    ctr: ir::LocalId,
    /// The values the body defines.
    defined: HashSet<ir::ValueId>,
    roots: Vec<(Root, Len)>,
    temps: HashSet<ir::LocalId>,
    words: Vec<Word>,
    bounds: Vec<usize>,
    /// The function's `u32` constants.
    consts: HashMap<ir::ValueId, u32>,
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

    /// The loop word `w` is, noted.
    fn word(&mut self, w: Word) -> usize {
        match self.words.iter().position(|x| *x == w) {
            Some(k) => k,
            None => {
                self.words.push(w);
                self.words.len() - 1
            }
        }
    }

    /// A `u32` operand's kind: a body value's, the counter, or a value from before the loop
    /// (a word).
    fn u32_operand(&mut self, x: ir::ValueId) -> Option<Kind> {
        if x == self.index {
            return Some(Kind::Index);
        }
        if self.defined.contains(&x) {
            return self.kinds.get(&x).copied();
        }
        if let Some(&n) = self.consts.get(&x) {
            return Some(Kind::Num(n));
        }
        if self.scalar(self.fe.vty(x))? != ir::Scalar::U32 || self.fe.values[x.index()] == u32::MAX
        {
            return None;
        }
        Some(Kind::Word(self.word(Word::Value(x))))
    }

    /// Whether a `u32` place is one the loop can't change and the four-wide loop can load
    /// before it starts: fields of a constant, of a by-reference parameter, of a local in the
    /// frame, or through a pointer from before the loop, or a scalar local the body doesn't
    /// store. The body stores only `f32`s, to elements and temporaries, so no `u32` changes.
    fn steady(&self, p: &ir::Place) -> bool {
        if !p.path.iter().all(|x| matches!(x, ir::Proj::Field(_))) {
            return false;
        }
        match &p.root {
            ir::PlaceRoot::Data(_) => true,
            ir::PlaceRoot::Param(k) => self.fe.f.params[*k as usize].by_ref,
            ir::PlaceRoot::Local(l) => {
                *l != self.ctr
                    && match self.fe.ir_locals[l.index()] {
                        LocalRepr::Slot(_) => true,
                        LocalRepr::Scalar(w) => p.path.is_empty() && w != u32::MAX,
                    }
            }
            ir::PlaceRoot::Ptr(v) => {
                !self.defined.contains(v) && self.fe.values[v.index()] != u32::MAX
            }
            ir::PlaceRoot::Resource(_) => false,
        }
    }

    /// Whether `then` is only a panic (what a bounds check runs): its message's address and
    /// length, then `Panic`.
    fn only_panics(then: &[ir::Stmt]) -> bool {
        let s = skip_at(then);
        let Some((last, before)) = s.split_last() else { return false };
        matches!(last, ir::Stmt::Eval(ir::Expr::Mem(ir::MemOp::Panic, _)))
            && before.iter().all(|s| {
                matches!(
                    s,
                    ir::Stmt::Let(
                        _,
                        ir::Expr::Addr(_) | ir::Expr::Const(_) | ir::Expr::Mem(ir::MemOp::Addr, _)
                    )
                )
            })
    }

    /// The root an element access at the counter's index goes through, noted; `None` if it
    /// isn't one.
    fn element(&mut self, root: Root, root_ty: ir::TypeId) -> Option<()> {
        let (elem, len) = match self.fe.m.types.get(root_ty) {
            ir::TypeDef::Run(e) => (*e, Len::Run),
            ir::TypeDef::Array(e, n) => (*e, Len::Array(*n)),
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
        // `*(base + i * 4)`, an `f32`.
        if let ir::PlaceRoot::Ptr(v) = &p.root
            && p.path.is_empty()
        {
            let Some(Kind::Ptr(w)) = self.kinds.get(v) else { return None };
            if self.scalar(self.fe.m.place_ty(self.fe.f, p)?)? != ir::Scalar::F32 {
                return None;
            }
            let root = Root::Base(*w);
            if !self.roots.iter().any(|(r, _)| *r == root) {
                self.roots.push((root, Len::Free));
            }
            return Some(());
        }
        let [ir::Proj::Index(i)] = p.path.as_slice() else { return None };
        if *i != self.index {
            return None;
        }
        let ty = match &p.root {
            ir::PlaceRoot::Local(l) if *l != self.ctr && !self.temps.contains(l) => {
                self.fe.f.locals[l.index()].ty
            }
            ir::PlaceRoot::Param(k) if self.fe.f.params[*k as usize].by_ref => {
                self.fe.f.params[*k as usize].ty
            }
            _ => return None,
        };
        self.element(Root::of(p), ty)
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
                // A bounds check, `if i >= n { panic }`: the four-wide loop stays below `n`, so
                // it never fails there, and the loop itself, one at a time, reaches it as it
                // would have.
                ir::Stmt::If { cond, then, else_ }
                    if skip_at(else_).is_empty() && Self::only_panics(then) =>
                {
                    let Some(Kind::Check(w)) = self.kinds.get(cond) else { return None };
                    if !self.bounds.contains(w) {
                        self.bounds.push(*w);
                    }
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
        let vt = self.fe.vty(v);
        // An element pointer: `mem Ptr(base + i * 4)`, to an `f32`.
        if let ir::Expr::Mem(ir::MemOp::Ptr, args) = e {
            let ir::TypeDef::Ptr(t) = self.fe.m.types.get(vt) else { return None };
            if self.scalar(*t)? != ir::Scalar::F32 {
                return None;
            }
            let [a] = args.as_slice() else { return None };
            let Some(Kind::Addr(w)) = self.kinds.get(a) else { return None };
            return Some(Kind::Ptr(*w));
        }
        let ty = self.scalar(vt)?;
        if ty == ir::Scalar::U32 {
            return match e {
                ir::Expr::Const(ir::Const::U32(n)) => Some(Kind::Num(*n)),
                ir::Expr::Load(p) if self.steady(p) => {
                    Some(Kind::Word(self.word(Word::Place(p.clone()))))
                }
                ir::Expr::Binary(B::Mul, a, b) => {
                    match (self.u32_operand(*a)?, self.u32_operand(*b)?) {
                        (Kind::Index, Kind::Num(4)) | (Kind::Num(4), Kind::Index) => {
                            Some(Kind::Offset)
                        }
                        _ => None,
                    }
                }
                ir::Expr::Binary(B::Add | B::WrappingAdd, a, b) => {
                    match (self.u32_operand(*a)?, self.u32_operand(*b)?) {
                        (Kind::Word(w), Kind::Offset) | (Kind::Offset, Kind::Word(w)) => {
                            Some(Kind::Addr(w))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
        }
        if let ir::Expr::Binary(B::Ge, a, b) = e
            && *a == self.index
            && let Some(Kind::Word(w)) = self.u32_operand(*b)
        {
            return Some(Kind::Check(w));
        }
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
    // The loop's words, once.
    let mut words = Vec::new();
    for word in &plan.words {
        words.push(match word {
            Word::Value(x) => fe.v(*x),
            Word::Place(p) => match p.root_local().map(|l| fe.ir_locals[l.index()]) {
                Some(LocalRepr::Scalar(w)) if p.path.is_empty() => w,
                _ => {
                    let l = fe.new_local(ValType::I32);
                    fe.addr(p)?;
                    fe.ins.extend([I::I32Load(mem(0, 2)), I::LocalSet(l)]);
                    l
                }
            },
        });
    }
    // Each root's first element's address and length, once: the loop doesn't change them.
    let mut bases = Vec::new();
    for (root, len) in &plan.roots {
        if let Root::Base(k) = root {
            bases.push((root.clone(), words[*k], u32::MAX));
            continue;
        }
        let base = fe.new_local(ValType::I32);
        let n = fe.new_local(ValType::I32);
        match root {
            Root::Place(p) => {
                fe.addr(p)?;
            }
            Root::Value(x) => fe.ins.push(I::LocalGet(fe.v(*x))),
            Root::Base(_) => unreachable!(),
        }
        match len {
            // A run is its elements' address, then their count.
            Len::Run => fe.ins.extend([
                I::LocalTee(base),
                I::I32Load(mem(4, 2)),
                I::LocalSet(n),
                I::LocalGet(base),
                I::I32Load(mem(0, 2)),
                I::LocalSet(base),
            ]),
            Len::Array(len) => {
                fe.ins.extend([I::LocalSet(base), I::I32Const(*len as i32), I::LocalSet(n)])
            }
            Len::Free => return Err("internal: a free root that isn't a pointer's".into()),
        }
        bases.push((root.clone(), base, n));
    }
    let temps: HashMap<ir::LocalId, u32> =
        plan.temps.iter().map(|&l| (l, fe.new_local(ValType::V128))).collect();
    fe.ins.push(I::Block(wasm_encoder::BlockType::Empty));
    fe.ins.push(I::Loop(wasm_encoder::BlockType::Empty));
    // On while i < bound and bound - i >= 4, and so for each root's length and each word a
    // check holds `i` below.
    let lens: Vec<I<'static>> = std::iter::once(I::LocalGet(fe.v(plan.bound)))
        .chain(bases.iter().filter(|&&(_, _, n)| n != u32::MAX).map(|&(_, _, n)| I::LocalGet(n)))
        .chain(plan.bounds.iter().map(|&k| I::LocalGet(words[k])))
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
            ir::Stmt::Let(v, _) if plan.quiet.contains(v) => {}
            // A bounds check, which the four-wide loop's test settles.
            ir::Stmt::If { .. } => {}
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
                    element_addr(fe, plan, &bases, &root_of(plan, p))?;
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

/// The root an element place in the plan's body goes through.
fn root_of(plan: &Plan, p: &ir::Place) -> Root {
    match &p.root {
        ir::PlaceRoot::Ptr(v) if p.path.is_empty() && plan.ptrs.contains_key(v) => {
            Root::Base(plan.ptrs[v])
        }
        _ => Root::of(p),
    }
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
                element_addr(fe, plan, bases, &root_of(plan, p))?;
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
