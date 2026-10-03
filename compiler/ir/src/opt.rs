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

use crate::derive::single_exit::single_exit;
use crate::*;
use std::collections::{HashMap, HashSet};

/// Flattens a GPU module (see the module docs).
pub fn flatten_gpu(m: &mut Module) -> Result<()> {
    inline_all(m)?;
    for i in 0..m.functions.len() {
        let mut f = std::mem::replace(&mut m.functions[i], Function::new("", Vec::new(), None));
        forward(m, &mut f);
        dce(&mut f);
        m.functions[i] = f;
    }
    drop_uncalled(m);
    Ok(())
}

// ---- generic walking ------------------------------------------------------------------------

fn map_expr(e: &mut Expr, v: &mut impl FnMut(ValueId) -> ValueId) {
    e.for_each_value_mut(&mut |x| *x = v(*x));
}

/// Calls `f` on every place in a block, recursively.
fn each_place(b: &Block, f: &mut impl FnMut(&Place)) {
    visit::walk(b, &mut |s| s.for_each_place(f));
}

/// Calls `f` on every value a block uses (not defines), recursively.
fn each_use(b: &Block, f: &mut impl FnMut(ValueId)) {
    visit::walk(b, &mut |s| s.for_each_value(f));
}

// ---- inlining -------------------------------------------------------------------------------

/// Inlines every call, callees first.
fn inline_all(m: &mut Module) -> Result<()> {
    let n = m.functions.len();
    let mut order = Vec::new();
    let mut state = vec![0u8; n]; // 0 new, 1 visiting, 2 done
    fn visit(m: &Module, f: usize, state: &mut [u8], order: &mut Vec<usize>) -> Result<()> {
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
            visit(m, g.index(), state, order)?;
        }
        state[f] = 2;
        order.push(f);
        Ok(())
    }
    for f in 0..n {
        visit(m, f, &mut state, &mut order)?;
    }
    for f in order {
        let mut func = std::mem::replace(&mut m.functions[f], Function::new("", Vec::new(), None));
        let body = std::mem::take(&mut func.body);
        let mut cx = Inliner { m, f: &mut func, rename: HashMap::new() };
        func_body(&mut cx, body)?;
        m.functions[f] = func;
    }
    Ok(())
}

fn func_body(cx: &mut Inliner<'_>, body: Block) -> Result<()> {
    let b = cx.block(body)?;
    cx.f.body = b;
    Ok(())
}

struct Inliner<'a> {
    m: &'a mut Module,
    f: &'a mut Function,
    /// Values of the function being flattened that now go by another name (a call's result).
    rename: HashMap<ValueId, ValueId>,
}

impl Inliner<'_> {
    fn block(&mut self, b: Block) -> Result<Block> {
        let mut out = Vec::new();
        for mut s in b {
            let rename = &self.rename;
            s.for_each_value_mut(&mut |x| *x = rename.get(x).copied().unwrap_or(*x));
            match s {
                Stmt::Let(v, Expr::Call(g, args)) => {
                    if let Some(res) = self.inline(g, &args, &mut out)? {
                        self.rename.insert(v, res);
                    }
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
        let orig = self.m.functions[g.index()].clone();
        let callee = match single_exit(self.m, &orig) {
            Some((nf, _)) => nf,
            None => orig,
        };
        let mut values: HashMap<ValueId, ValueId> = HashMap::new();
        let mut locals: HashMap<LocalId, LocalId> = HashMap::new();
        for (i, l) in callee.locals.iter().enumerate() {
            locals.insert(LocalId(i as u32), self.f.new_local(l.name.clone(), l.ty));
        }
        let mut result = None;
        let body =
            self.copy_block(&callee, &callee.body, args, &mut values, &locals, &mut result)?;
        out.extend(body);
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    fn copy_block(
        &mut self,
        g: &Function,
        b: &Block,
        args: &[Arg],
        values: &mut HashMap<ValueId, ValueId>,
        locals: &HashMap<LocalId, LocalId>,
        result: &mut Option<ValueId>,
    ) -> Result<Block> {
        let mut out = Vec::new();
        for s in b {
            match s {
                Stmt::Let(v, Expr::Param(i)) => match &args[*i as usize] {
                    // A by-value parameter is the argument itself.
                    Arg::Value(a) => {
                        values.insert(*v, *a);
                    }
                    Arg::Place(_) => {
                        return Err(Error::internal("a by-value parameter given a place"));
                    }
                },
                Stmt::Let(v, e) => {
                    let e = self.copy_expr(e, args, values, locals)?;
                    let nv = self.f.new_value(g.value_ty(*v));
                    values.insert(*v, nv);
                    out.push(Stmt::Let(nv, e));
                }
                Stmt::Eval(e) => {
                    let e = self.copy_expr(e, args, values, locals)?;
                    out.push(Stmt::Eval(e));
                }
                Stmt::Store(p, x) => {
                    let p = self.copy_place(p, args, values, locals)?;
                    out.push(Stmt::Store(p, val(values, *x)?));
                }
                Stmt::If { cond, then, else_ } => {
                    let cond = val(values, *cond)?;
                    let then = self.copy_block(g, then, args, values, locals, result)?;
                    let else_ = self.copy_block(g, else_, args, values, locals, result)?;
                    out.push(Stmt::If { cond, then, else_ });
                }
                Stmt::Loop { body, continuing } => {
                    let body = self.copy_block(g, body, args, values, locals, result)?;
                    let continuing =
                        self.copy_block(g, continuing, args, values, locals, result)?;
                    out.push(Stmt::Loop { body, continuing });
                }
                // Single exit: the one `return` is the body's last statement.
                Stmt::Return(Some(x)) => *result = Some(val(values, *x)?),
                Stmt::Return(None) => {}
                Stmt::Break | Stmt::Continue | Stmt::Trap | Stmt::At(_) => out.push(s.clone()),
            }
        }
        Ok(out)
    }

    fn copy_place(
        &mut self,
        p: &Place,
        args: &[Arg],
        values: &HashMap<ValueId, ValueId>,
        locals: &HashMap<LocalId, LocalId>,
    ) -> Result<Place> {
        let mut path = Vec::new();
        let root = match &p.root {
            PlaceRoot::Local(l) => PlaceRoot::Local(locals[l]),
            PlaceRoot::Resource(r) => PlaceRoot::Resource(*r),
            PlaceRoot::Ptr(v) => PlaceRoot::Ptr(val(values, *v)?),
            PlaceRoot::Param(i) => match &args[*i as usize] {
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
                Proj::Index(v) => Proj::Index(val(values, *v)?),
                other => other.clone(),
            });
        }
        Ok(Place { root, path })
    }

    fn copy_expr(
        &mut self,
        e: &Expr,
        args: &[Arg],
        values: &HashMap<ValueId, ValueId>,
        locals: &HashMap<LocalId, LocalId>,
    ) -> Result<Expr> {
        let mut e = e.clone();
        match &mut e {
            Expr::Load(p) | Expr::Run(p) | Expr::Addr(p) | Expr::ArrayLength(p) => {
                *p = self.copy_place(p, args, values, locals)?;
            }
            Expr::Call(..) => return Err(Error::internal("a call left in a flattened callee")),
            _ => {
                let mut err = None;
                map_expr(&mut e, &mut |x| {
                    val(values, x).unwrap_or_else(|m| {
                        err = Some(m);
                        x
                    })
                });
                if let Some(m) = err {
                    return Err(m);
                }
            }
        }
        Ok(e)
    }
}

fn val(values: &HashMap<ValueId, ValueId>, v: ValueId) -> Result<ValueId> {
    values
        .get(&v)
        .copied()
        .ok_or_else(|| Error::internal(format!("v{} used before it's defined", v.0)))
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
    // Whole stores, and writes or pointers that rule a local out.
    let mut whole_stores = vec![0u32; n];
    let mut excluded = vec![false; n];
    // Every mention of each local, the stores included.
    let mut mentions = vec![0u32; n];
    visit::walk(&f.body, &mut |s| {
        match s {
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
            Stmt::Let(_, Expr::Call(_, args)) | Stmt::Eval(Expr::Call(_, args)) => {
                for a in args {
                    if let Arg::Place(p) = a
                        && let Some(l) = p.root_local()
                    {
                        excluded[l.index()] = true;
                    }
                }
            }
            _ => {}
        }
        s.for_each_place(&mut |p| {
            if let Some(l) = p.root_local() {
                mentions[l.index()] += 1;
            }
        });
    });
    let candidate: Vec<bool> = (0..n).map(|l| whole_stores[l] == 1 && !excluded[l]).collect();
    // Dominance: in the block holding the store, every other mention comes after it. Each
    // block's statements are counted from the end, so this is one pass over the code.
    let mut ok = HashSet::new();
    fn mentions_in(s: &Stmt, candidate: &[bool], out: &mut HashMap<LocalId, u32>) {
        let mut count = |p: &Place| {
            if let Some(l) = p.root_local()
                && candidate[l.index()]
            {
                *out.entry(l).or_default() += 1;
            }
        };
        s.for_each_place(&mut count);
        for b in s.blocks() {
            visit::walk(b, &mut |inner| inner.for_each_place(&mut count));
        }
    }
    fn scan(b: &Block, candidate: &[bool], mentions: &[u32], ok: &mut HashSet<LocalId>) {
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
            mentions_in(s, candidate, &mut after);
            for inner in s.blocks() {
                scan(inner, candidate, mentions, ok);
            }
        }
    }
    scan(&f.body, &candidate, &mentions, &mut ok);
    ok
}

#[allow(clippy::too_many_arguments)]
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
                    Expr::Load(p) => match &p.root {
                        PlaceRoot::Local(l) if local_src.contains_key(l) => {
                            let mut q = local_src[l].clone();
                            q.path.extend(p.path.iter().cloned());
                            Some(q)
                        }
                        _ => None,
                    },
                    Expr::Extract(x, k) => src.get(x).and_then(|base| {
                        let proj = match m.types.get(f.value_ty(*x)) {
                            TypeDef::Struct { .. } | TypeDef::Enum { .. } => Some(Proj::Field(*k)),
                            TypeDef::Vector(_) => Some(Proj::Comp(*k as u8)),
                            _ => None,
                        };
                        proj.map(|p| base.with(p))
                    }),
                    Expr::ExtractDyn(x, i) => src.get(x).and_then(|base| {
                        let proj = match m.types.get(f.value_ty(*x)) {
                            TypeDef::Array(..) => Some(Proj::Index(*i)),
                            _ => None,
                        };
                        proj.map(|p| base.with(p))
                    }),
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
            Stmt::If { then, else_, .. } => {
                forward_block(m, f, then, fwd, src, local_src, changed);
                forward_block(m, f, else_, fwd, src, local_src, changed);
            }
            Stmt::Loop { body, continuing } => {
                forward_block(m, f, body, fwd, src, local_src, changed);
                forward_block(m, f, continuing, fwd, src, local_src, changed);
            }
            _ => {}
        }
    }
}

// ---- dead code ------------------------------------------------------------------------------

/// Removes unused pure values and stores to locals nothing reads, to a fixpoint.
fn dce(f: &mut Function) {
    loop {
        let mut used = vec![false; f.values.len()];
        each_use(&f.body, &mut |v| used[v.index()] = true);
        let mut read: HashSet<LocalId> = HashSet::new();
        each_read_local(&f.body, &mut read);
        let mut changed = false;
        let mut body = std::mem::take(&mut f.body);
        sweep(&mut body, &used, &read, &mut changed);
        f.body = body;
        if !changed {
            break;
        }
    }
    compact_locals(f);
}

/// Drops locals nothing mentions any more (a GPU compiler zero-initializes every variable it's
/// given, whether or not it's used).
fn compact_locals(f: &mut Function) {
    let mut seen = vec![false; f.locals.len()];
    each_place(&f.body, &mut |p| {
        if let PlaceRoot::Local(l) = p.root {
            seen[l.index()] = true;
        }
    });
    let mut remap = vec![None; f.locals.len()];
    let mut kept = Vec::new();
    for (i, l) in std::mem::take(&mut f.locals).into_iter().enumerate() {
        if seen[i] {
            remap[i] = Some(LocalId(kept.len() as u32));
            kept.push(l);
        }
    }
    f.locals = kept;
    visit::walk_mut(&mut f.body, &mut |s| {
        s.for_each_place_mut(&mut |p| {
            if let PlaceRoot::Local(l) = &mut p.root {
                *l = remap[l.index()].expect("a mentioned local is kept");
            }
        })
    });
}

/// The locals whose contents something reads (a load, a pointer, a call's argument).
fn each_read_local(b: &Block, read: &mut HashSet<LocalId>) {
    visit::walk(b, &mut |s| {
        if let Some(e) = s.expr() {
            e.for_each_place(&mut |p| {
                if let Some(l) = p.root_local() {
                    read.insert(l);
                }
            });
        }
    });
}

fn sweep(b: &mut Block, used: &[bool], read: &HashSet<LocalId>, changed: &mut bool) {
    let before = b.len();
    b.retain(|s| match s {
        Stmt::Let(v, e) => used[v.index()] || matches!(e, Expr::Call(..) | Expr::Host(..)),
        Stmt::Store(Place { root: PlaceRoot::Local(l), .. }, _) => read.contains(l),
        _ => true,
    });
    if b.len() != before {
        *changed = true;
    }
    for s in b.iter_mut() {
        match s {
            Stmt::If { then, else_, .. } => {
                sweep(then, used, read, changed);
                sweep(else_, used, read, changed);
            }
            Stmt::Loop { body, continuing } => {
                sweep(body, used, read, changed);
                sweep(continuing, used, read, changed);
            }
            _ => {}
        }
    }
}

/// Removes functions no entry point reaches (after inlining, all but the entry points).
fn drop_uncalled(m: &mut Module) {
    let mut keep = vec![false; m.functions.len()];
    let mut stack: Vec<FuncId> = m.entry_points.iter().map(|e| e.function).collect();
    stack.extend(m.exports.iter().map(|e| e.1));
    while let Some(f) = stack.pop() {
        if std::mem::replace(&mut keep[f.index()], true) {
            continue;
        }
        stack.extend(visit::calls(&m.functions[f.index()].body));
    }
    let mut remap = vec![None; m.functions.len()];
    let mut kept = Vec::new();
    for (i, f) in std::mem::take(&mut m.functions).into_iter().enumerate() {
        if keep[i] {
            remap[i] = Some(FuncId(kept.len() as u32));
            kept.push(f);
        }
    }
    m.functions = kept;
    let r = |f: FuncId| remap[f.index()].expect("kept");
    for e in &mut m.entry_points {
        e.function = r(e.function);
    }
    for e in &mut m.exports {
        e.1 = r(e.1);
    }
    for f in &mut m.functions {
        remap_calls(&mut f.body, &r);
    }
}

fn remap_calls(b: &mut Block, r: &impl Fn(FuncId) -> FuncId) {
    visit::walk_mut(b, &mut |s| {
        if let Some(Expr::Call(g, _)) = s.expr_mut() {
            *g = r(*g);
        }
    });
}
