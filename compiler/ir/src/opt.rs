//! Flattening for GPU modules: every call inlined, reads of uniform data forwarded to the
//! uniform itself, and dead code removed.
//!
//! Why: a field's numbers arrive as one uniform block (D-070), and fields are built of small
//! methods that take the field by value. Emitted as is, each call copies its part of the
//! uniform into a function-local variable, and a loop over an array of parts (the grazer's
//! legs) indexes a private copy of the array. GPU compilers keep such copies in slow memory:
//! the grazer ran 19× slower than spike 01's hand-written kernel, which reads `G.parts[i]`
//! straight from the uniform. After flattening, the compiled kernel reads the uniform the same
//! way. WGSL can't pass pointers into uniform memory to functions (naga doesn't implement
//! `unrestricted_pointer_parameters`), which is why this inlines rather than passing places.
//!
//! GPU code has no recursion (E0600), so inlining terminates. Only GPU modules are flattened;
//! the CPU's WASM keeps its calls.

use crate::single_exit::single_exit;
use crate::*;
use std::collections::{HashMap, HashSet};

/// Flattens a GPU module (see the module docs).
pub fn flatten_gpu(m: &mut Module) -> Result<()> {
    inline_all(m)?;
    // No calls are left: only a store writes a local, and nothing calls a function that isn't
    // an entry point, so only the entry points are kept (and optimized).
    debug_assert!(m.functions.iter().all(|f| visit::calls(&f.body).is_empty()));
    keep_entry_points(m);
    for i in 0..m.functions.len() {
        let mut f = std::mem::take(&mut m.functions[i]);
        forward(m, &mut f);
        dce(&mut f);
        m.functions[i] = f;
    }
    Ok(())
}

// ---- inlining -------------------------------------------------------------------------------

/// Inlines every call, callees first.
fn inline_all(m: &mut Module) -> Result<()> {
    let n = m.functions.len();
    let mut order = Vec::new();
    let mut state = vec![0u8; n]; // 0 new, 1 visiting, 2 done
    let mut called = vec![false; n];
    fn visit(
        m: &Module,
        f: usize,
        state: &mut [u8],
        called: &mut [bool],
        order: &mut Vec<usize>,
    ) -> Result<()> {
        match state[f] {
            2 => return Ok(()),
            1 => {
                return Err(Error::internal(format!(
                    "`{}` is recursive on the GPU",
                    m.functions[f].name
                )));
            }
            _ => {}
        }
        state[f] = 1;
        for g in visit::calls(&m.functions[f].body) {
            called[g.index()] = true;
            visit(m, g.index(), state, called, order)?;
        }
        state[f] = 2;
        order.push(f);
        Ok(())
    }
    for f in 0..n {
        visit(m, f, &mut state, &mut called, &mut order)?;
    }
    // Each called function's single-exit form, made once it's flattened (`None`: its body has
    // one exit already).
    let mut exits: Vec<Option<Function>> = vec![None; n];
    for f in order {
        let mut func = std::mem::take(&mut m.functions[f]);
        let body = std::mem::take(&mut func.body);
        let rename = vec![None; func.values.len()];
        let mut cx = Inliner { m, exits: &exits, f: &mut func, rename };
        let body = cx.block(body)?;
        func.body = body;
        if called[f] {
            exits[f] = single_exit(m, &func).map(|(nf, _)| nf);
        }
        m.functions[f] = func;
    }
    Ok(())
}

struct Inliner<'a> {
    m: &'a Module,
    exits: &'a [Option<Function>],
    f: &'a mut Function,
    /// Values of the function being flattened that now go by another name (a call's result),
    /// by `ValueId`.
    rename: Vec<Option<ValueId>>,
}

impl Inliner<'_> {
    fn block(&mut self, b: Block) -> Result<Block> {
        let mut out = Vec::new();
        for mut s in b {
            let rename = &self.rename;
            s.for_each_value_mut(&mut |x| *x = rename[x.index()].unwrap_or(*x));
            match s {
                Stmt::Let(v, Expr::Call(g, args)) => {
                    self.rename[v.index()] = self.inline(g, &args, &mut out)?;
                }
                Stmt::Eval(Expr::Call(g, args)) => {
                    self.inline(g, &args, &mut out)?;
                }
                Stmt::If { cond, then, else_ } => {
                    let then = self.block(then)?;
                    let else_ = self.block(else_)?;
                    out.push(Stmt::If { cond, then, else_ });
                }
                Stmt::Loop { body, continuing } => {
                    let body = self.block(body)?;
                    let continuing = self.block(continuing)?;
                    out.push(Stmt::Loop { body, continuing });
                }
                other => out.push(other),
            }
        }
        Ok(out)
    }

    /// Copies `g`'s body (already flattened) into `out`, returning the value its result now is.
    fn inline(&mut self, g: FuncId, args: &[Arg], out: &mut Block) -> Result<Option<ValueId>> {
        let callee = self.exits[g.index()].as_ref().unwrap_or(&self.m.functions[g.index()]);
        let locals = callee.locals.iter().map(|l| self.f.new_local(l.name.clone(), l.ty)).collect();
        let values = vec![None; callee.values.len()];
        let mut cx = Copier { callee, caller: self.f, args, values, locals, result: None };
        out.extend(cx.block(&callee.body)?);
        // A body that never ends (it loops forever, or traps) has no `return` to take the
        // result from: what follows the call never runs, but needs a value.
        let result = match (cx.result, callee.ret) {
            (None, Some(t)) => {
                let v = self.f.new_value(t);
                out.push(Stmt::Let(v, Expr::Zero(t)));
                Some(v)
            }
            (r, _) => r,
        };
        Ok(result)
    }
}

/// A callee's body, copied into its caller with the call's arguments for its parameters.
struct Copier<'a> {
    callee: &'a Function,
    caller: &'a mut Function,
    args: &'a [Arg],
    /// The caller's value for each of the callee's, by `ValueId`, once it's defined.
    values: Vec<Option<ValueId>>,
    /// The caller's local for each of the callee's, by `LocalId`.
    locals: Vec<LocalId>,
    /// The value the callee returns.
    result: Option<ValueId>,
}

impl Copier<'_> {
    fn block(&mut self, b: &Block) -> Result<Block> {
        let mut out = Vec::new();
        for s in b {
            match s {
                Stmt::Let(v, Expr::Param(i)) => match &self.args[*i as usize] {
                    // A by-value parameter is the argument itself.
                    Arg::Value(a) => self.values[v.index()] = Some(*a),
                    Arg::Place(_) => {
                        return Err(Error::internal("a by-value parameter given a place"));
                    }
                },
                Stmt::Let(v, e) => {
                    let e = self.expr(e)?;
                    let nv = self.caller.new_value(self.callee.value_ty(*v));
                    self.values[v.index()] = Some(nv);
                    out.push(Stmt::Let(nv, e));
                }
                Stmt::Eval(e) => out.push(Stmt::Eval(self.expr(e)?)),
                Stmt::Store(p, x) => out.push(Stmt::Store(self.place(p)?, self.val(*x)?)),
                Stmt::If { cond, then, else_ } => {
                    let cond = self.val(*cond)?;
                    let then = self.block(then)?;
                    let else_ = self.block(else_)?;
                    out.push(Stmt::If { cond, then, else_ });
                }
                Stmt::Loop { body, continuing } => {
                    let body = self.block(body)?;
                    let continuing = self.block(continuing)?;
                    out.push(Stmt::Loop { body, continuing });
                }
                // Single exit: the one `return` is the body's last statement.
                Stmt::Return(Some(x)) => self.result = Some(self.val(*x)?),
                Stmt::Return(None) => {}
                Stmt::Break | Stmt::Continue | Stmt::Trap | Stmt::At(_) => out.push(s.clone()),
            }
        }
        Ok(out)
    }

    fn place(&self, p: &Place) -> Result<Place> {
        let mut path = Vec::new();
        let root = match &p.root {
            PlaceRoot::Local(l) => PlaceRoot::Local(self.locals[l.index()]),
            PlaceRoot::Resource(r) => PlaceRoot::Resource(*r),
            PlaceRoot::Data(d) => PlaceRoot::Data(*d),
            PlaceRoot::Ptr(v) => PlaceRoot::Ptr(self.val(*v)?),
            PlaceRoot::Param(i) => match &self.args[*i as usize] {
                Arg::Place(a) => {
                    path.extend(a.path.iter().cloned());
                    a.root.clone()
                }
                Arg::Value(_) => {
                    return Err(Error::internal("a by-reference parameter given a value"));
                }
            },
        };
        for proj in &p.path {
            path.push(match proj {
                Proj::Index(v) => Proj::Index(self.val(*v)?),
                other => other.clone(),
            });
        }
        Ok(Place { root, path })
    }

    fn expr(&self, e: &Expr) -> Result<Expr> {
        let mut e = e.clone();
        match &mut e {
            Expr::Load(p) | Expr::Run(p) | Expr::Addr(p) | Expr::ArrayLength(p) => {
                *p = self.place(p)?;
            }
            Expr::Call(..) => return Err(Error::internal("a call left in a flattened callee")),
            _ => {
                let mut err = None;
                e.for_each_value_mut(&mut |x| match self.val(*x) {
                    Ok(v) => *x = v,
                    Err(m) => err = Some(m),
                });
                if let Some(m) = err {
                    return Err(m);
                }
            }
        }
        Ok(e)
    }

    fn val(&self, v: ValueId) -> Result<ValueId> {
        self.values[v.index()]
            .ok_or_else(|| Error::internal(format!("v{} used before it's defined", v.0)))
    }
}

// ---- forwarding reads of uniform data -------------------------------------------------------

/// Whether a place's data can't change while an invocation runs: a uniform or a read-only
/// storage buffer.
fn read_only(m: &Module, p: &Place) -> bool {
    matches!(&p.root, PlaceRoot::Resource(r)
        if matches!(m.resources[r.index()].kind, ResourceKind::Uniform { .. } | ResourceKind::StorageRead))
}

/// Rewrites reads of values that are (parts of) read-only resources into loads of the resource
/// itself, through locals that are written once.
fn forward(m: &Module, f: &mut Function) {
    loop {
        let fwd = forwardable_locals(f);
        let mut src: HashMap<ValueId, Place> = HashMap::new();
        let mut local_src: HashMap<LocalId, Place> = HashMap::new();
        let mut changed = false;
        let mut body = std::mem::take(&mut f.body);
        forward_block(m, f, &mut body, &fwd, &mut src, &mut local_src, &mut changed);
        f.body = body;
        if !changed {
            break;
        }
    }
}

/// Locals written exactly once, as a whole, by a store that comes before every read of them in
/// the same block (so it dominates them), and never written through or pointed to.
fn forwardable_locals(f: &Function) -> HashSet<LocalId> {
    let n = f.locals.len();
    // Whole stores, and writes or pointers that rule a local out. (No calls are left to write
    // one through a by-reference argument.)
    let mut whole_stores = vec![0u32; n];
    let mut excluded = vec![false; n];
    visit::walk(&f.body, &mut |s| match s {
        Stmt::Store(p, _) => {
            if let Some(l) = p.root_local() {
                if p.path.is_empty() {
                    whole_stores[l.index()] += 1;
                } else {
                    excluded[l.index()] = true;
                }
            }
        }
        Stmt::Let(_, Expr::Addr(p) | Expr::Run(p)) => {
            if let Some(l) = p.root_local() {
                excluded[l.index()] = true;
            }
        }
        _ => {}
    });
    // Every mention of each local, the stores included.
    let mut mentions = vec![0u32; n];
    visit::count_local_mentions(&f.body, &mut mentions);
    let candidate: Vec<bool> = (0..n).map(|l| whole_stores[l] == 1 && !excluded[l]).collect();
    // Dominance: in the block holding the store, every other mention comes after it. Each
    // block's statements are counted from the end, so this is one pass over the code.
    let mut ok = HashSet::new();
    /// Adds `b`'s forwardable stores to `ok`, and returns how often `b` mentions each candidate.
    fn scan(
        b: &Block,
        candidate: &[bool],
        mentions: &[u32],
        ok: &mut HashSet<LocalId>,
    ) -> HashMap<LocalId, u32> {
        // The mentions in the statements after the one at hand.
        let mut after: HashMap<LocalId, u32> = HashMap::new();
        for s in b.iter().rev() {
            if let Stmt::Store(p, _) = s
                && p.path.is_empty()
                && let Some(l) = p.root_local()
                && candidate[l.index()]
                && after.get(&l).copied().unwrap_or(0) + 1 == mentions[l.index()]
            {
                ok.insert(l);
            }
            s.for_each_place(&mut |p| {
                if let Some(l) = p.root_local()
                    && candidate[l.index()]
                {
                    *after.entry(l).or_default() += 1;
                }
            });
            for inner in s.blocks() {
                for (l, k) in scan(inner, candidate, mentions, ok) {
                    *after.entry(l).or_default() += k;
                }
            }
        }
        after
    }
    scan(&f.body, &candidate, &mentions, &mut ok);
    ok
}

fn forward_block(
    m: &Module,
    f: &Function,
    b: &mut Block,
    fwd: &HashSet<LocalId>,
    src: &mut HashMap<ValueId, Place>,
    local_src: &mut HashMap<LocalId, Place>,
    changed: &mut bool,
) {
    for s in b.iter_mut() {
        match s {
            Stmt::Let(v, e) => {
                let from: Option<Place> = match e {
                    Expr::Load(p) if read_only(m, p) => {
                        src.insert(*v, p.clone());
                        None
                    }
                    Expr::Load(p) => p.root_local().and_then(|l| local_src.get(&l)).map(|base| {
                        let mut q = base.clone();
                        q.path.extend(p.path.iter().cloned());
                        q
                    }),
                    Expr::Extract(x, k) => src.get(x).and_then(|base| {
                        let proj = match m.types.get(f.value_ty(*x)) {
                            TypeDef::Struct { .. } | TypeDef::Enum { .. } => Proj::Field(*k),
                            TypeDef::Vector(_) => Proj::Comp(*k as u8),
                            _ => return None,
                        };
                        Some(base.with(proj))
                    }),
                    Expr::ExtractDyn(x, i) => src
                        .get(x)
                        .filter(|_| matches!(m.types.get(f.value_ty(*x)), TypeDef::Array(..)))
                        .map(|base| base.with(Proj::Index(*i))),
                    _ => None,
                };
                if let Some(q) = from {
                    *e = Expr::Load(q.clone());
                    src.insert(*v, q);
                    *changed = true;
                }
            }
            Stmt::Store(Place { root: PlaceRoot::Local(l), path }, x)
                if path.is_empty() && fwd.contains(l) && !local_src.contains_key(l) =>
            {
                if let Some(q) = src.get(x) {
                    local_src.insert(*l, q.clone());
                }
            }
            _ => {
                for inner in s.blocks_mut() {
                    forward_block(m, f, inner, fwd, src, local_src, changed);
                }
            }
        }
    }
}

// ---- dead code ------------------------------------------------------------------------------

/// Removes the pure values nothing needs and the stores to locals nothing needs. What's
/// needed starts from what must stay (calls, host operations, control flow, returns, stores
/// that aren't to a local) and goes back through the values and locals they read: a value
/// needs its operands and the locals it loads, and a local needs what's stored to it. One walk
/// collects that, a worklist follows it, and one sweep removes the rest, however long a dead
/// chain is (through locals, too).
fn dce(f: &mut Function) {
    #[derive(Clone, Copy)]
    enum Need {
        Value(ValueId),
        Local(LocalId),
    }
    let reads = |e: &Expr, into: &mut Vec<Need>| {
        e.for_each_value(&mut |v| into.push(Need::Value(v)));
        e.for_each_place(&mut |p| {
            if let Some(l) = p.root_local() {
                into.push(Need::Local(l));
            }
        });
    };
    // What a pure value, or a store to a local, needs in turn; and what must stay needs.
    let mut by_value = vec![Vec::new(); f.values.len()];
    let mut by_local = vec![Vec::new(); f.locals.len()];
    let mut work = Vec::new();
    visit::walk(&f.body, &mut |s| match s {
        Stmt::Let(v, e) if !matches!(e, Expr::Call(..) | Expr::Host(..)) => {
            reads(e, &mut by_value[v.index()]);
        }
        Stmt::Store(p @ Place { root: PlaceRoot::Local(l), .. }, v) => {
            let into = &mut by_local[l.index()];
            into.push(Need::Value(*v));
            p.for_each_value(&mut |x| into.push(Need::Value(x)));
        }
        s => {
            s.for_each_value(&mut |v| work.push(Need::Value(v)));
            if let Some(e) = s.expr() {
                reads(e, &mut work);
            }
        }
    });
    let mut used = vec![false; f.values.len()];
    let mut read = vec![false; f.locals.len()];
    while let Some(n) = work.pop() {
        let (seen, more) = match n {
            Need::Value(v) => (&mut used[v.index()], &mut by_value[v.index()]),
            Need::Local(l) => (&mut read[l.index()], &mut by_local[l.index()]),
        };
        if !std::mem::replace(seen, true) {
            work.append(more);
        }
    }
    sweep(&mut f.body, &used, &read);
    compact_locals(f);
}

/// Removes the pure values not `used` and the stores to locals not `read`.
fn sweep(b: &mut Block, used: &[bool], read: &[bool]) {
    b.retain_mut(|s| {
        for inner in s.blocks_mut() {
            sweep(inner, used, read);
        }
        match s {
            Stmt::Let(v, e) => used[v.index()] || matches!(e, Expr::Call(..) | Expr::Host(..)),
            Stmt::Store(p, _) => p.root_local().is_none_or(|l| read[l.index()]),
            _ => true,
        }
    });
}

/// Drops locals nothing mentions any more (a GPU compiler zero-initializes every variable it's
/// given, whether or not it's used).
fn compact_locals(f: &mut Function) {
    let mut mentions = vec![0u32; f.locals.len()];
    visit::count_local_mentions(&f.body, &mut mentions);
    let remap = retain_by_index(&mut f.locals, |i| mentions[i] > 0);
    visit::walk_mut(&mut f.body, &mut |s| {
        s.for_each_place_mut(&mut |p| {
            if let PlaceRoot::Local(l) = &mut p.root {
                *l = LocalId(remap[l.index()].expect("a mentioned local is kept"));
            }
        })
    });
}

/// Removes every function but the entry points: after inlining, nothing calls the others.
fn keep_entry_points(m: &mut Module) {
    debug_assert!(m.exports.is_empty(), "a GPU module exports nothing");
    let mut keep = vec![false; m.functions.len()];
    for e in &m.entry_points {
        keep[e.function.index()] = true;
    }
    let remap = retain_by_index(&mut m.functions, |i| keep[i]);
    for e in &mut m.entry_points {
        e.function = FuncId(remap[e.function.index()].expect("an entry point is kept"));
    }
}

/// Keeps the items whose index `keep` holds for, in order: each one's new index, by its old one.
fn retain_by_index<T>(items: &mut Vec<T>, keep: impl Fn(usize) -> bool) -> Vec<Option<u32>> {
    let mut remap = vec![None; items.len()];
    let mut kept = Vec::new();
    for (i, x) in std::mem::take(items).into_iter().enumerate() {
        if keep(i) {
            remap[i] = Some(kept.len() as u32);
            kept.push(x);
        }
    }
    *items = kept;
    remap
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dead chain through locals, as a run of `let`s makes it, goes in one pass; what the
    /// result needs stays.
    #[test]
    fn a_dead_chain_through_locals_goes_at_once() {
        let mut m = Module::default();
        let f32t = m.types.f32();
        let x_param = Param { name: "x".into(), ty: f32t, by_ref: false, mutable: false };
        let mut f = Function::new("f", vec![x_param], Some(f32t));
        let x = f.new_value(f32t);
        let mut body = vec![Stmt::Let(x, Expr::Param(0))];
        let mut prev = x;
        for i in 0..100 {
            let l = f.new_local(format!("v{i}"), f32t);
            body.push(Stmt::Store(Place::local(l), prev));
            let v = f.new_value(f32t);
            body.push(Stmt::Let(v, Expr::Load(Place::local(l))));
            let w = f.new_value(f32t);
            body.push(Stmt::Let(w, Expr::Binary(BinOp::Add, v, x)));
            prev = w;
        }
        body.push(Stmt::Return(Some(x)));
        f.body = body;
        dce(&mut f);
        assert_eq!(f.body, vec![Stmt::Let(x, Expr::Param(0)), Stmt::Return(Some(x))]);
        assert!(f.locals.is_empty());
    }
}
